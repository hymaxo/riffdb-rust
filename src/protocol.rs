// Port of Protocol.h / Protocol.c
//
// yyjson is replaced by serde_json for reading, and by a small hand-written
// writer (`JsonWriter`) that reproduces yyjson_mut_write's minified output
// (insertion order, duplicate keys allowed, fails on NaN/Inf and invalid
// UTF-8 like yyjson does without flags).
//
// ProtocolBindMsgpackArgsToStmt (cwpack) is not ported: it was never called.

use libc::c_char;
use libsqlite3_sys::*;
use serde_json::Value;
use std::ffi::CStr;
use std::io::Write;

use crate::http_response::fmt_u64;

use crate::log_warn;

pub unsafe fn protocol_bind_json_args_to_stmt(args: &Value, stmt: *mut sqlite3_stmt) -> i32 {
    // TODO: ssleert - add normal error handling
    let Some(arr) = args.as_array() else {
        return 0;
    };

    for (i, element) in arr.iter().enumerate() {
        let idx = (i + 1) as i32;

        match element {
            Value::String(s) => {
                // PORT NOTE: C binds with a NULL (static) destructor pointing
                // into the (leaked) yyjson doc. Use SQLITE_TRANSIENT so sqlite
                // copies the text instead.
                sqlite3_bind_text(stmt, idx, s.as_ptr() as *const c_char, s.len() as i32, SQLITE_TRANSIENT());
            }
            Value::Number(n) if n.is_i64() || n.is_u64() => {
                // yyjson_get_sint on a uint just reinterprets the bits.
                let int = n.as_i64().unwrap_or_else(|| n.as_u64().unwrap() as i64);
                sqlite3_bind_int64(stmt, idx, int);
            }
            Value::Number(n) => {
                let double = n.as_f64().unwrap_or(0.0);
                sqlite3_bind_double(stmt, idx, double);
            }
            Value::Bool(b) => {
                sqlite3_bind_int(stmt, idx, *b as i32);
            }
            Value::Null => {
                sqlite3_bind_null(stmt, idx);
            }
            // objects / arrays are silently skipped
            _ => {}
        }
    }

    0
}

/// Minimal stand-in for yyjson_mut_doc + yyjson_mut_write.
///
/// Writes straight into a byte buffer. Output matches the previous
/// char-by-char version byte for byte (see the tests at the bottom).
pub struct JsonWriter {
    pub out: Vec<u8>,
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

impl JsonWriter {
    pub fn new() -> JsonWriter {
        JsonWriter {
            out: Vec::with_capacity(1024),
            failed: false,
        }
    }

    /// `s` must be valid UTF-8 (checked by the caller). Copies unescaped runs
    /// in bulk instead of pushing one char at a time.
    fn write_str(&mut self, s: &[u8]) {
        self.out.reserve(s.len() + 2);
        self.out.push(b'"');
        let mut run_start = 0;
        for (i, &b) in s.iter().enumerate() {
            if !NEEDS_ESCAPE[b as usize] {
                continue;
            }
            self.out.extend_from_slice(&s[run_start..i]);
            run_start = i + 1;
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
        self.out.extend_from_slice(&s[run_start..]);
        self.out.push(b'"');
    }

    /// NUL-terminated text, as yyjson_mut_obj_add_strcpy sees it (strlen).
    /// Fails like yyjson does on invalid UTF-8.
    unsafe fn write_cstr(&mut self, p: *const c_char) {
        let bytes = CStr::from_ptr(p).to_bytes();
        if std::str::from_utf8(bytes).is_err() {
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

pub unsafe fn protocol_json_from_stmt(doc: *mut JsonWriter, stmt: *mut sqlite3_stmt) -> i32 {
    let doc = &mut *doc;
    let mut rc;

    let column_count = sqlite3_column_count(stmt);

    // `"name":` for every column, escaped once per statement instead of once
    // per cell (C re-adds the key for every row).
    // A bad (non UTF-8) name only fails the write once a row uses it, as before.
    let mut keys: Vec<Vec<u8>> = Vec::with_capacity(column_count.max(0) as usize);
    let mut keys_failed = false;
    for i in 0..column_count {
        let mut key = JsonWriter { out: Vec::new(), failed: false };
        key.write_cstr(sqlite3_column_name(stmt, i));
        key.out.push(b':');
        keys_failed |= key.failed;
        keys.push(key.out);
    }

    doc.out.push(b'[');
    let mut first_row = true;

    loop {
        rc = sqlite3_step(stmt);
        if rc != SQLITE_ROW {
            break;
        }

        if !first_row {
            doc.out.push(b',');
        }
        first_row = false;
        doc.failed |= keys_failed;
        doc.out.push(b'{');

        for (i, key) in keys.iter().enumerate() {
            let i = i as i32;
            if i != 0 {
                doc.out.push(b',');
            }
            doc.out.extend_from_slice(key);

            match sqlite3_column_type(stmt, i) {
                SQLITE_INTEGER => {
                    doc.write_int(sqlite3_column_int64(stmt, i));
                }
                SQLITE_FLOAT => {
                    doc.write_real(sqlite3_column_double(stmt, i));
                }
                SQLITE_TEXT => {
                    let text = sqlite3_column_text(stmt, i) as *const c_char;
                    if !text.is_null() {
                        doc.write_cstr(text);
                    } else {
                        doc.out.extend_from_slice(b"null");
                    }
                }
                SQLITE_BLOB => {
                    log_warn!("blobs unsupported");
                    doc.out.extend_from_slice(b"null");
                }
                _ => {
                    doc.out.extend_from_slice(b"null");
                }
            }
        }

        doc.out.push(b'}');
    }

    doc.out.push(b']');

    rc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    /// Runs `sql` on a fresh in-memory db and returns (rc, json, failed).
    fn run(sql: &str) -> (i32, String, bool) {
        unsafe {
            // built with SQLITE_OMIT_AUTOINIT, like the C version
            assert_eq!(sqlite3_initialize(), SQLITE_OK);
            let mut db = ptr::null_mut();
            assert_eq!(sqlite3_open(c":memory:".as_ptr(), &mut db), SQLITE_OK);
            let mut stmt = ptr::null_mut();
            let rc = sqlite3_prepare_v2(db, sql.as_ptr() as *const c_char, sql.len() as i32, &mut stmt, ptr::null_mut());
            assert_eq!(rc, SQLITE_OK, "{sql}");
            let mut doc = JsonWriter::new();
            let rc = protocol_json_from_stmt(&mut doc, stmt);
            sqlite3_finalize(stmt);
            sqlite3_close(db);
            (rc, String::from_utf8(doc.out).unwrap(), doc.failed)
        }
    }

    #[test]
    fn empty_result() {
        assert_eq!(run("SELECT 1 WHERE 0"), (SQLITE_DONE, "[]".into(), false));
    }

    #[test]
    fn column_types() {
        let (rc, json, failed) = run(
            "SELECT 1 AS i, -9223372036854775808 AS min, 9223372036854775807 AS max, \
             1.5 AS r, 100.0 AS whole, 1e300 AS big, 1e-7 AS small, NULL AS n, x'00ff' AS b, 'txt' AS t \
             UNION ALL SELECT 0, 0, 0, -0.25, 0.1, 0, 0, NULL, NULL, ''",
        );
        assert_eq!(rc, SQLITE_DONE);
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
    fn non_finite_and_invalid_utf8_fail_like_yyjson() {
        assert!(run("SELECT 1e999 AS inf").2);
        assert!(run("SELECT CAST(x'ff' AS TEXT) AS bad").2);
    }
}
