// Port of DataBase.h / DataBase.c

use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

use crate::log_err;
use crate::options::options;

/// The text C gets from sqlite3_errmsg(db) / the sqlite3_exec error string.
pub fn sqlite_errmsg(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        rusqlite::Error::SqliteFailure(err, None) => err.to_string(),
        // prepare() errors, with the offending token's offset attached; `msg`
        // is the plain sqlite3_errmsg text.
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

/// sqlite's primary result code (C logs the `Rc` of sqlite3_open_v2).
fn sqlite_rc(e: &rusqlite::Error) -> i32 {
    match e {
        rusqlite::Error::SqliteFailure(err, _) | rusqlite::Error::SqlInputError { error: err, .. } => {
            err.extended_code & 0xff
        }
        _ => -1,
    }
}

pub fn database_create_if_not_exists() -> i32 {
    let db = match Connection::open_with_flags(
        "./riff.db",
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    ) {
        Ok(db) => db,
        Err(_) => {
            log_err!("Cant open sqlite3 file on {}/riff.db", options().directory);
            return -1;
        }
    };

    if let Err(e) = db.execute_batch("PRAGMA journal_mode = WAL;PRAGMA synchronous = normal;PRAGMA temp_store = memory;")
    {
        log_err!("Cant execute pragmas: {}", sqlite_errmsg(&e));
        return -1;
    }

    if let Err(e) = db.execute_batch("CREATE TABLE IF NOT EXISTS _riff (    id INTEGER PRIMARY KEY,    version TEXT NOT NULL);")
    {
        log_err!("Cant execute meta table creation: {}", sqlite_errmsg(&e));
        return -1;
    }

    0
}

pub fn database_open(read_only: bool) -> Option<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_CREATE;
    if read_only {
        flags |= OpenFlags::SQLITE_OPEN_READ_ONLY;
    } else {
        flags |= OpenFlags::SQLITE_OPEN_READ_WRITE;
    }

    match Connection::open_with_flags("./riff.db", flags) {
        Ok(db) => {
            let _ = db.busy_timeout(Duration::from_millis(5000));
            Some(db)
        }
        Err(e) => {
            log_err!("cant open sqlite connection for worker: Rc = {}", sqlite_rc(&e));
            None
        }
    }
}
