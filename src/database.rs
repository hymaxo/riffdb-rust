use rusqlite::{Connection, OpenFlags};
use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::log_err;
use crate::options::options;

const DB_PATH: &str = "./riff.db";

pub fn sqlite_errmsg(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        rusqlite::Error::SqliteFailure(err, None) => err.to_string(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

fn sqlite_rc(e: &rusqlite::Error) -> i32 {
    match e {
        rusqlite::Error::SqliteFailure(err, _) | rusqlite::Error::SqlInputError { error: err, .. } => {
            err.extended_code & 0xff
        }
        _ => -1,
    }
}

pub fn init() -> Result<(), ()> {
    let db = match Connection::open_with_flags(DB_PATH, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)
    {
        Ok(db) => db,
        Err(_) => {
            log_err!("Cant open sqlite3 file on {}/riff.db", options().directory);
            return Err(());
        }
    };

    if let Err(e) = db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = normal;
         PRAGMA temp_store = memory;",
    ) {
        log_err!("Cant execute pragmas: {}", sqlite_errmsg(&e));
        return Err(());
    }

    if let Err(e) = db.execute_batch(
        "CREATE TABLE IF NOT EXISTS _riff (
             id INTEGER PRIMARY KEY,
             version TEXT NOT NULL
         );",
    ) {
        log_err!("Cant execute meta table creation: {}", sqlite_errmsg(&e));
        return Err(());
    }

    Ok(())
}

// default busy handler sleeps whole ms, way too slow for wal
fn busy_wait(count: i32) -> bool {
    thread_local!(static START: Cell<Option<Instant>> = const { Cell::new(None) });
    const TIMEOUT: Duration = Duration::from_millis(5000);

    let now = Instant::now();
    let start = START.with(|s| {
        if count == 0 || s.get().is_none() {
            s.set(Some(now));
        }
        s.get().unwrap_or(now)
    });
    if now.duration_since(start) >= TIMEOUT {
        return false;
    }
    if count < 8 {
        std::thread::yield_now();
    } else {
        let us = 20u64 << ((count - 8).min(6) as u32);
        std::thread::sleep(Duration::from_micros(us.min(1000)));
    }
    true
}

pub fn open() -> Option<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE;
    match Connection::open_with_flags(DB_PATH, flags) {
        Ok(db) => {
            let _ = db.busy_handler(Some(busy_wait));
            Some(db)
        }
        Err(e) => {
            log_err!("cant open sqlite connection for worker: Rc = {}", sqlite_rc(&e));
            None
        }
    }
}
