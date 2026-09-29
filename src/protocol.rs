// The JSON side of the API: reading `{"q": ..., "args": [...]}` payloads and
// writing result rows.
//
// Payloads are read with a borrowing serde visitor, so the SQL text and
// string arguments are usually not copied. Rows are written by `JsonWriter`
// straight into the response buffer: compact, columns in order, duplicate
// column names kept, and NaN/Inf or invalid UTF-8 fail the whole result.

use rusqlite::types::ValueRef;
use rusqlite::Statement;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::borrow::Cow;
use std::fmt;
use std::io::Write;

use crate::http_response::fmt_u64;
use crate::log_warn;

// ---------------------------------------------------------------------------
// Request payload: {"q": "...", "args": [...]}
// ---------------------------------------------------------------------------

/// One element of `args`.
#[derive(Debug, PartialEq)]
pub enum Arg<'a> {
    Str(Cow<'a, str>),
    /// Any JSON integer. Values above i64::MAX wrap around.
    Int(i64),
    Real(f64),
    Bool(bool),
    Null,
    /// Objects and arrays: not bound, but they still take an index.
    Skip,
}

/// The parts of a payload the service uses. For duplicate keys the first one
/// wins, and a root that isn't an object has neither key.
#[derive(Debug, Default, PartialEq)]
pub struct Payload<'a> {
    /// None: no "q" key (or root isn't an object).
    /// Some(None): "q" isn't a string.
    pub q: Option<Option<Cow<'a, str>>>,
    /// None: no "args" key. A non-array "args" is Some(empty): it binds
    /// nothing.
    pub args: Option<Vec<Arg<'a>>>,
}

/// Parses a request body. Strict JSON: the whole buffer must be one document.
pub fn parse_payload(payload: &[u8]) -> Result<Payload<'_>, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_slice(payload);
    let doc = de.deserialize_any(PayloadVisitor)?;
    de.end()?;
    Ok(doc)
}

struct PayloadVisitor;

impl<'de> Visitor<'de> for PayloadVisitor {
    type Value = Payload<'de>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON document")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Payload<'de>, A::Error> {
        let mut doc = Payload::default();
        while let Some(key) = map.next_key::<Key>()? {
            match key {
                Key::Q if doc.q.is_none() => doc.q = Some(map.next_value::<StrValue>()?.0),
                Key::Args if doc.args.is_none() => {
                    doc.args = Some(map.next_value::<ArgsValue>()?.0.unwrap_or_default());
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(doc)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Payload<'de>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Payload::default())
    }

    // Any scalar root: valid JSON, but no "q" in it.
    fn visit_bool<E>(self, _: bool) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
    fn visit_i64<E>(self, _: i64) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
    fn visit_u64<E>(self, _: u64) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
    fn visit_f64<E>(self, _: f64) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
    fn visit_str<E>(self, _: &str) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
    fn visit_unit<E>(self) -> Result<Payload<'de>, E> {
        Ok(Payload::default())
    }
}

/// Object keys, matched without allocating.
enum Key {
    Q,
    Args,
    Other,
}

impl<'de> de::Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object key")
            }
            fn visit_str<E>(self, v: &str) -> Result<Key, E> {
                Ok(match v {
                    "q" => Key::Q,
                    "args" => Key::Args,
                    _ => Key::Other,
                })
            }
        }
        d.deserialize_str(KeyVisitor)
    }
}

/// "q": borrowed when the string has no escapes, None if it isn't a string.
struct StrValue<'a>(Option<Cow<'a, str>>);

impl<'de> de::Deserialize<'de> for StrValue<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<StrValue<'de>, D::Error> {
        d.deserialize_any(ArgSeed).map(|arg| match arg {
            Arg::Str(s) => StrValue(Some(s)),
            _ => StrValue(None),
        })
    }
}

/// "args": Some(elements) for an array, None for anything else.
struct ArgsValue<'a>(Option<Vec<Arg<'a>>>);

impl<'de> de::Deserialize<'de> for ArgsValue<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ArgsValue<'de>, D::Error> {
        struct ArgsVisitor;
        impl<'de> Visitor<'de> for ArgsVisitor {
            type Value = ArgsValue<'de>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ArgsValue<'de>, A::Error> {
                let mut args = Vec::with_capacity(seq.size_hint().unwrap_or(4));
                while let Some(arg) = seq.next_element_seed(ArgSeed)? {
                    args.push(arg);
                }
                Ok(ArgsValue(Some(args)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ArgsValue<'de>, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(ArgsValue(None))
            }
            fn visit_bool<E>(self, _: bool) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
            fn visit_unit<E>(self) -> Result<ArgsValue<'de>, E> {
                Ok(ArgsValue(None))
            }
        }
        d.deserialize_any(ArgsVisitor)
    }
}

/// Any JSON value -> Arg.
struct ArgSeed;

impl<'de> DeserializeSeed<'de> for ArgSeed {
    type Value = Arg<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Arg<'de>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ArgSeed {
    type Value = Arg<'de>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Arg<'de>, E> {
        Ok(Arg::Str(Cow::Borrowed(v)))
    }
    fn visit_str<E>(self, v: &str) -> Result<Arg<'de>, E> {
        Ok(Arg::Str(Cow::Owned(v.to_owned())))
    }
    fn visit_string<E>(self, v: String) -> Result<Arg<'de>, E> {
        Ok(Arg::Str(Cow::Owned(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Arg<'de>, E> {
        Ok(Arg::Int(v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Arg<'de>, E> {
        Ok(Arg::Int(v as i64))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Arg<'de>, E> {
        Ok(Arg::Real(v))
    }
    fn visit_bool<E>(self, v: bool) -> Result<Arg<'de>, E> {
        Ok(Arg::Bool(v))
    }
    fn visit_unit<E>(self) -> Result<Arg<'de>, E> {
        Ok(Arg::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Arg<'de>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Arg::Skip)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Arg<'de>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Arg::Skip)
    }
}

/// Binds `args` to the statement's `?` placeholders in order. Bind errors
/// (e.g. more args than placeholders) are ignored; unbound placeholders are
/// NULL.
pub fn bind_args(args: &[Arg], stmt: &mut Statement) {
    for (i, arg) in args.iter().enumerate() {
        let idx = i + 1;
        let _ = match arg {
            Arg::Str(s) => stmt.raw_bind_parameter(idx, s.as_ref()),
            Arg::Int(v) => stmt.raw_bind_parameter(idx, v),
            Arg::Real(v) => stmt.raw_bind_parameter(idx, v),
            Arg::Bool(v) => stmt.raw_bind_parameter(idx, *v as i32),
            Arg::Null => stmt.raw_bind_parameter(idx, rusqlite::types::Null),
            Arg::Skip => Ok(()),
        };
    }
}

// ---------------------------------------------------------------------------
// Result rows -> JSON
// ---------------------------------------------------------------------------

/// Writes JSON straight into a byte buffer. `failed` is set when a value
/// can't be represented; the output is then unusable.
pub struct JsonWriter<'a> {
    pub out: &'a mut Vec<u8>,
    pub failed: bool,
}

/// Bytes that must be escaped inside a JSON string: control chars, `"`, `\`.
const NEEDS_ESCAPE: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = true;
        i += 1;
    }
    t[b'"' as usize] = true;
    t[b'\\' as usize] = true;
    t
};

const HEX: &[u8; 16] = b"0123456789ABCDEF";

const fn splat(b: u8) -> u64 {
    u64::from_ne_bytes([b; 8])
}

/// Whether any byte of the word is < 0x20, `"` or `\` (i.e. NEEDS_ESCAPE
/// for any of its 8 bytes), with the classic exact SWAR tests:
/// `hasless(x, n) = (x - n*0x01..) & !x & 0x80..` for n <= 128, and a byte
/// equals `c` exactly when `x ^ c*0x01..` has a zero byte there.
#[inline(always)]
fn word_needs_escape(w: u64) -> bool {
    const LO: u64 = splat(0x01);
    const HI: u64 = splat(0x80);
    let has_zero = |x: u64| x.wrapping_sub(LO) & !x & HI;
    let below_space = w.wrapping_sub(splat(0x20)) & !w & HI;
    (below_space | has_zero(w ^ splat(b'"')) | has_zero(w ^ splat(b'\\'))) != 0
}

impl<'a> JsonWriter<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> JsonWriter<'a> {
        JsonWriter { out, failed: false }
    }

    /// Writes `s` as a JSON string, up to its first NUL. `s` must be valid
    /// UTF-8 up to there. Clean runs are found 8 bytes at a time and copied
    /// in bulk.
    fn write_str(&mut self, s: &[u8]) {
        self.out.reserve(s.len() + 2);
        self.out.push(b'"');
        let mut run_start = 0;
        let mut i = 0;
        while i < s.len() {
            // Skip clean 8-byte words...
            while let Some(word) = s.get(i..i + 8) {
                if word_needs_escape(u64::from_ne_bytes(word.try_into().unwrap())) {
                    break;
                }
                i += 8;
            }
            // ...then walk byte-wise to the byte that stopped it (at most 7
            // bytes away), or to the end of the tail.
            while i < s.len() && !NEEDS_ESCAPE[s[i] as usize] {
                i += 1;
            }
            let Some(&b) = s.get(i) else {
                break;
            };
            if b == 0 {
                break;
            }
            self.out.extend_from_slice(&s[run_start..i]);
            i += 1;
            run_start = i;
            match b {
                b'"' => self.out.extend_from_slice(b"\\\""),
                b'\\' => self.out.extend_from_slice(b"\\\\"),
                b'\n' => self.out.extend_from_slice(b"\\n"),
                b'\r' => self.out.extend_from_slice(b"\\r"),
                b'\t' => self.out.extend_from_slice(b"\\t"),
                0x08 => self.out.extend_from_slice(b"\\b"),
                0x0c => self.out.extend_from_slice(b"\\f"),
                _ => self.out.extend_from_slice(&[b'\\', b'u', b'0', b'0', HEX[(b >> 4) as usize], HEX[(b & 0xf) as usize]]),
            }
        }
        self.out.extend_from_slice(&s[run_start..i.min(s.len())]);
        self.out.push(b'"');
    }

    /// Writes sqlite text. Text is cut at the first NUL, and invalid UTF-8
    /// before that point fails the write.
    fn write_text(&mut self, bytes: &[u8]) {
        let valid = match std::str::from_utf8(bytes) {
            Ok(_) => true,
            // Fine if the string ends (at a NUL) before the bad bytes.
            Err(e) => bytes[..e.valid_up_to()].contains(&0),
        };
        if !valid {
            self.failed = true;
            return;
        }
        self.write_str(bytes);
    }

    fn write_int(&mut self, v: i64) {
        let mut buf = [0u8; 20];
        if v < 0 {
            self.out.push(b'-');
        }
        self.out.extend_from_slice(fmt_u64(v.unsigned_abs(), &mut buf));
    }

    fn write_real(&mut self, d: f64) {
        if !d.is_finite() {
            self.failed = true;
            return;
        }
        let _ = write!(self.out, "{:?}", d);
    }
}

/// Steps `stmt` to the end, writing `[{...},...]` with one object per row.
/// Err is the sqlite error that stopped the stepping.
pub fn write_rows(doc: &mut JsonWriter, stmt: &mut Statement) -> rusqlite::Result<()> {
    let column_count = stmt.column_count();

    // `"name":` for every column, escaped once per statement instead of once
    // per cell. A bad name only fails the write once a row uses it.
    let mut keys: Vec<Vec<u8>> = Vec::with_capacity(column_count);
    let mut keys_failed = false;
    for i in 0..column_count {
        let mut key_buf = Vec::new();
        let mut key = JsonWriter::new(&mut key_buf);
        match stmt.column_name(i) {
            Ok(name) => key.write_text(name.as_bytes()),
            Err(_) => key.failed = true,
        }
        keys_failed |= key.failed;
        key_buf.push(b':');
        keys.push(key_buf);
    }

    doc.out.push(b'[');
    let mut first_row = true;

    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        if !first_row {
            doc.out.push(b',');
        }
        first_row = false;
        doc.failed |= keys_failed;
        doc.out.push(b'{');

        for (i, key) in keys.iter().enumerate() {
            if i != 0 {
                doc.out.push(b',');
            }
            doc.out.extend_from_slice(key);

            match row.get_ref(i)? {
                ValueRef::Integer(v) => doc.write_int(v),
                ValueRef::Real(v) => doc.write_real(v),
                ValueRef::Text(text) => doc.write_text(text),
                ValueRef::Blob(_) => {
                    log_warn!("blobs unsupported");
                    doc.out.extend_from_slice(b"null");
                }
                ValueRef::Null => doc.out.extend_from_slice(b"null"),
            }
        }

        doc.out.push(b'}');
    }

    doc.out.push(b']');

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Runs `sql` on a fresh in-memory db and returns (ok, json, failed).
    fn run(sql: &str) -> (bool, String, bool) {
        let db = Connection::open_in_memory().unwrap();
        let mut stmt = db.prepare(sql).unwrap();
        let mut out = Vec::new();
        let mut doc = JsonWriter::new(&mut out);
        let ok = write_rows(&mut doc, &mut stmt).is_ok();
        let failed = doc.failed;
        (ok, String::from_utf8(out).unwrap(), failed)
    }

    #[test]
    fn empty_result() {
        assert_eq!(run("SELECT 1 WHERE 0"), (true, "[]".into(), false));
    }

    #[test]
    fn column_types() {
        let (ok, json, failed) = run(
            "SELECT 1 AS i, -9223372036854775808 AS min, 9223372036854775807 AS max, \
             1.5 AS r, 100.0 AS whole, 1e300 AS big, 1e-7 AS small, NULL AS n, x'00ff' AS b, 'txt' AS t \
             UNION ALL SELECT 0, 0, 0, -0.25, 0.1, 0, 0, NULL, NULL, ''",
        );
        assert!(ok);
        assert!(!failed);
        assert_eq!(
            json,
            r#"[{"i":1,"min":-9223372036854775808,"max":9223372036854775807,"r":1.5,"whole":100.0,"big":1e300,"small":1e-7,"n":null,"b":null,"t":"txt"},{"i":0,"min":0,"max":0,"r":-0.25,"whole":0.1,"big":0,"small":0,"n":null,"b":null,"t":""}]"#
        );
    }

    #[test]
    fn string_escapes() {
        // value: q"b\s/ LF CR TAB BS FF 0x01 0x1f DEL é€😀   column name: k"ey
        let (_, json, failed) = run(concat!(
            r#"SELECT 'q"b\s/' || char(10) || char(13) || char(9) || char(8) || char(12)"#,
            r#" || char(1) || char(31) || char(127) || 'é€😀' AS "k""ey""#,
        ));
        assert!(!failed);
        assert_eq!(json, concat!(r#"[{"k\"ey":"q\"b\\s/\n\r\t\b\f\u0001\u001F"#, "\u{7f}", r#"é€😀"}]"#));
    }

    #[test]
    fn text_stops_at_nul_like_strlen() {
        let (_, json, _) = run("SELECT 'ab' || char(0) || 'cd' AS t");
        assert_eq!(json, r#"[{"t":"ab"}]"#);
    }

    #[test]
    fn non_finite_and_invalid_utf8_fail() {
        assert!(run("SELECT 1e999 AS inf").2);
        assert!(run("SELECT CAST(x'ff' AS TEXT) AS bad").2);
        // bytes after a NUL are never looked at, so they can't fail the write
        let (_, json, failed) = run("SELECT 'ok' || char(0) || CAST(x'ff' AS TEXT) AS t");
        assert!(!failed);
        assert_eq!(json, r#"[{"t":"ok"}]"#);
    }

    /// The obvious byte-by-byte version, to check the word-at-a-time one.
    fn reference_escape(s: &[u8]) -> Vec<u8> {
        let s = &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())];
        let mut out = vec![b'"'];
        for &b in s {
            match b {
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                0x08 => out.extend_from_slice(b"\\b"),
                0x0c => out.extend_from_slice(b"\\f"),
                b if b < 0x20 => out.extend_from_slice(format!("\\u{:04X}", b).as_bytes()),
                b => out.push(b),
            }
        }
        out.push(b'"');
        out
    }

    /// The previous byte-table writer (no NUL handling), for timing.
    fn write_str_table(out: &mut Vec<u8>, s: &[u8]) {
        out.reserve(s.len() + 2);
        out.push(b'"');
        let mut run_start = 0;
        for (i, &b) in s.iter().enumerate() {
            if !NEEDS_ESCAPE[b as usize] {
                continue;
            }
            out.extend_from_slice(&s[run_start..i]);
            run_start = i + 1;
            match b {
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'\n' => out.extend_from_slice(b"\\n"),
                _ => out.extend_from_slice(b"\\u0000"),
            }
        }
        out.extend_from_slice(&s[run_start..]);
        out.push(b'"');
    }

    /// cargo test --release --bin riffdb json_micro -- --ignored --nocapture
    #[test]
    #[ignore]
    fn json_micro() {
        // the strings of the bench's query_1000 rows
        let rows: Vec<(Vec<u8>, Vec<u8>)> = (1..=1000)
            .map(|x| {
                (
                    format!("user {x}").into_bytes(),
                    format!("line1\nline2 \"quoted\" {:016X}", x * 2654435761u64).into_bytes(),
                )
            })
            .collect();
        let long = vec![b'x'; 4096];
        let time = |name: &str, f: &dyn Fn(&mut Vec<u8>)| {
            let mut out = Vec::with_capacity(1 << 20);
            let t = std::time::Instant::now();
            for _ in 0..2000 {
                out.clear();
                f(&mut out);
            }
            println!("{name:<28} {:>8.1} us/iter", t.elapsed().as_secs_f64() * 1e6 / 2000.0);
        };
        time("table   bench rows", &|out| {
            for (a, b) in &rows {
                write_str_table(out, a);
                write_str_table(out, b);
            }
        });
        let prev_write_text = |out: &mut Vec<u8>, bytes: &[u8]| {
            let bytes = match bytes.iter().position(|&b| b == 0) {
                Some(nul) => &bytes[..nul],
                None => bytes,
            };
            if std::str::from_utf8(bytes).is_ok() {
                write_str_table(out, bytes);
            }
        };
        time("previous write_text rows", &|out| {
            for (a, b) in &rows {
                prev_write_text(out, a);
                prev_write_text(out, b);
            }
        });
        time("previous 4 KiB clean x100", &|out| {
            for _ in 0..100 {
                prev_write_text(out, &long);
            }
        });
        time("current bench rows", &|out| {
            let mut w = JsonWriter::new(out);
            for (a, b) in &rows {
                w.write_text(a);
                w.write_text(b);
            }
        });
        time("table   4 KiB clean x100", &|out| {
            for _ in 0..100 {
                write_str_table(out, &long);
            }
        });
        time("current 4 KiB clean x100", &|out| {
            let mut w = JsonWriter::new(out);
            for _ in 0..100 {
                w.write_text(&long);
            }
        });
    }

    #[test]
    fn word_at_a_time_escaping_matches_reference() {
        let specials: &[u8] = &[b'"', b'\\', b'\n', 0x01, 0x1f, 0x20, 0x21, 0x5b, 0x5d, 0x7f, 0x80, 0xff, 0];
        // every special byte at every position of strings up to 3 words long
        for len in 0..24 {
            for pos in 0..len {
                for &sp in specials {
                    let mut s = vec![b'a'; len];
                    s[pos] = sp;
                    let mut out = Vec::new();
                    JsonWriter::new(&mut out).write_str(&s);
                    assert_eq!(out, reference_escape(&s), "len {len} pos {pos} byte {sp:#x}");
                }
            }
        }
        for b in 0..=255u8 {
            let w = u64::from_ne_bytes([b'a', b'a', b'a', b, b'a', b'a', b'a', b'a']);
            assert_eq!(word_needs_escape(w), NEEDS_ESCAPE[b as usize], "byte {b:#x}");
        }
    }

    fn parse(s: &str) -> Option<Payload<'_>> {
        parse_payload(s.as_bytes()).ok()
    }

    #[test]
    fn payload_parsing() {
        let p = parse(r#"{"q":"SELECT ?","args":["s",1,-2,1.5,true,null,[1],{"a":1},18446744073709551615]}"#).unwrap();
        assert_eq!(p.q, Some(Some(Cow::Borrowed("SELECT ?"))));
        assert_eq!(
            p.args.unwrap(),
            vec![
                Arg::Str("s".into()),
                Arg::Int(1),
                Arg::Int(-2),
                Arg::Real(1.5),
                Arg::Bool(true),
                Arg::Null,
                Arg::Skip,
                Arg::Skip,
                Arg::Int(-1), // u64::MAX wraps
            ]
        );

        // first key wins
        assert_eq!(parse(r#"{"q":"first","q":"second"}"#).unwrap().q, Some(Some("first".into())));
        // non-string q, missing q, non-object root
        assert_eq!(parse(r#"{"q":5}"#).unwrap().q, Some(None));
        assert_eq!(parse(r#"{"x":1}"#).unwrap().q, None);
        assert_eq!(parse(r#"[{"q":"x"}]"#).unwrap(), Payload::default());
        assert_eq!(parse("42").unwrap(), Payload::default());
        // non-array args bind nothing
        assert_eq!(parse(r#"{"q":"x","args":{"a":1}}"#).unwrap().args, Some(vec![]));
        // escaped strings are unescaped (owned)
        assert_eq!(parse(r#"{"q":"a\"bé"}"#).unwrap().q, Some(Some(Cow::Owned("a\"bé".into()))));
        // invalid JSON / trailing content / empty
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"q":"x"} x"#).is_none());
        assert!(parse("").is_none());
    }
}
