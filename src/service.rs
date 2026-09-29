// The /query and /execute logic: parse the payload, prepare and bind the
// statement, run it.

use rusqlite::{CachedStatement, Connection, Statement};
use std::borrow::Cow;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::database::sqlite_errmsg;
use crate::protocol::{bind_args, parse_payload, write_rows, JsonWriter};

#[derive(Debug)]
pub enum ServiceError {
    /// The client went away; nothing should be sent.
    Cancelled,
    /// Answered with a 500 and this message as the body.
    Failed(Cow<'static, str>),
}

impl ServiceError {
    fn sqlite(e: &rusqlite::Error) -> ServiceError {
        ServiceError::Failed(Cow::Owned(sqlite_errmsg(e)))
    }
}

/// A statement from the connection's cache, or a one-off one.
enum Prepared<'db> {
    Cached(CachedStatement<'db>),
    Fresh(Statement<'db>),
}

impl<'db> Deref for Prepared<'db> {
    type Target = Statement<'db>;
    #[inline]
    fn deref(&self) -> &Statement<'db> {
        match self {
            Prepared::Cached(s) => s,
            Prepared::Fresh(s) => s,
        }
    }
}

impl<'db> DerefMut for Prepared<'db> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Statement<'db> {
        match self {
            Prepared::Cached(s) => s,
            Prepared::Fresh(s) => s,
        }
    }
}

fn prepare<'db>(cancel: &AtomicBool, db: &'db Connection, payload: &[u8]) -> Result<Prepared<'db>, ServiceError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(ServiceError::Cancelled);
    }
    // Clients match on these messages, so they stay as they are, including
    // the odd one for invalid JSON.
    let Ok(doc) = parse_payload(payload) else {
        return Err(ServiceError::Failed(Cow::Borrowed("query len < 3")));
    };
    let Some(q) = doc.q else {
        return Err(ServiceError::Failed(Cow::Borrowed("query is empty")));
    };
    // A non-string "q" counts as an empty string.
    let sql: &str = q.as_deref().unwrap_or("");
    if sql.len() < 3 {
        return Err(ServiceError::Failed(Cow::Borrowed("query len < 3")));
    }

    // rusqlite's statement cache resets statements and clears their bindings
    // before reuse, so it doesn't change results. It keys on `sql.trim()`,
    // which also strips Unicode whitespace that sqlite itself would reject,
    // so only text that trimming leaves unchanged goes through it.
    let prepared = if sql.trim().len() == sql.len() {
        db.prepare_cached(sql).map(Prepared::Cached)
    } else {
        db.prepare(sql).map(Prepared::Fresh)
    };
    let mut stmt = prepared.map_err(|e| ServiceError::sqlite(&e))?;

    if let Some(args) = &doc.args {
        bind_args(args, &mut stmt);
    }

    Ok(stmt)
}

/// Runs the statement once. Rows it returns are ignored.
pub fn execute(cancel: &AtomicBool, db: &Connection, payload: &[u8]) -> Result<(), ServiceError> {
    let mut stmt = prepare(cancel, db, payload)?;
    stmt.raw_query().next().map_err(|e| ServiceError::sqlite(&e))?;
    Ok(())
}

/// Runs the statement and appends every row to `out` as a JSON array of
/// objects.
pub fn query(cancel: &AtomicBool, db: &Connection, payload: &[u8], out: &mut Vec<u8>) -> Result<(), ServiceError> {
    let mut stmt = prepare(cancel, db, payload)?;

    let mut json = JsonWriter::new(out);
    write_rows(&mut json, &mut stmt).map_err(|e| ServiceError::sqlite(&e))?;
    if json.failed {
        return Err(ServiceError::Failed(Cow::Borrowed("cant create json")));
    }
    Ok(())
}
