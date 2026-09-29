// Port of DataBase.h / DataBase.c

use rusqlite::{Connection, OpenFlags};
use std::cell::Cell;
use std::time::{Duration, Instant};

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

/// Busy handler with the same 5 s budget as `sqlite3_busy_timeout(db, 5000)`.
///
/// sqlite's default callback sleeps 1, 2, 5, 10... ms between retries, and on
/// Windows `Sleep(1)` rounds up to the ~15.6 ms timer tick. WAL write locks
/// are held for microseconds, so most of that time is wasted (C-8). This
/// yields first, then backs off from 20 us to 1 ms.
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
        let us = 20u64 << ((count - 8).min(6) as u32); // 20 us .. 1280 us
        std::thread::sleep(Duration::from_micros(us.min(1000)));
    }
    true
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
            // C: sqlite3_busy_timeout(db, 5000). Same limit, finer waits (C-8).
            let _ = db.busy_handler(Some(busy_wait));
            Some(db)
        }
        Err(e) => {
            log_err!("cant open sqlite connection for worker: Rc = {}", sqlite_rc(&e));
            None
        }
    }
}
