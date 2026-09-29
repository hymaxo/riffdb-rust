use rusqlite::{CachedStatement, Connection, Statement};
use std::borrow::Cow;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::database::sqlite_errmsg;
use crate::protocol::{bind_args, parse_payload, write_rows, JsonWriter};

#[derive(Debug)]
pub enum ServiceError {
    Cancelled,
    Failed(Cow<'static, str>),
}

impl ServiceError {
    fn sqlite(e: &rusqlite::Error) -> ServiceError {
        ServiceError::Failed(Cow::Owned(sqlite_errmsg(e)))
    }
}

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
    // yes, "query len < 3" for bad json. clients match on it
    let Ok(doc) = parse_payload(payload) else {
        return Err(ServiceError::Failed(Cow::Borrowed("query len < 3")));
    };
    let Some(q) = doc.q else {
        return Err(ServiceError::Failed(Cow::Borrowed("query is empty")));
    };
    let sql: &str = q.as_deref().unwrap_or("");
    if sql.len() < 3 {
        return Err(ServiceError::Failed(Cow::Borrowed("query len < 3")));
    }

    // prepare_cached keys on trim(), skip weird whitespace
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

pub fn execute(cancel: &AtomicBool, db: &Connection, payload: &[u8]) -> Result<(), ServiceError> {
    let mut stmt = prepare(cancel, db, payload)?;
    stmt.raw_query().next().map_err(|e| ServiceError::sqlite(&e))?;
    Ok(())
}

pub fn query(cancel: &AtomicBool, db: &Connection, payload: &[u8], out: &mut Vec<u8>) -> Result<(), ServiceError> {
    let mut stmt = prepare(cancel, db, payload)?;

    let mut json = JsonWriter::new(out);
    write_rows(&mut json, &mut stmt).map_err(|e| ServiceError::sqlite(&e))?;
    if json.failed {
        return Err(ServiceError::Failed(Cow::Borrowed("cant create json")));
    }
    Ok(())
}
