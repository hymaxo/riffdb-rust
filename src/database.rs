// Port of DataBase.h / DataBase.c

use libc::c_char;
use libsqlite3_sys::*;
use std::ffi::CStr;
use std::ptr;

use crate::log_err;
use crate::options::options;

pub unsafe fn database_create_if_not_exists() -> i32 {
    let mut db: *mut sqlite3 = ptr::null_mut();
    let mut err: *mut c_char = ptr::null_mut();

    let mut rc = sqlite3_open_v2(
        c"./riff.db".as_ptr(),
        &mut db,
        SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
        ptr::null(),
    );

    if rc != SQLITE_OK {
        log_err!("Cant open sqlite3 file on {}/riff.db", options().directory);
        sqlite3_close(db);
        return -1;
    }

    rc = sqlite3_exec(
        db,
        c"PRAGMA journal_mode = WAL;PRAGMA synchronous = normal;PRAGMA temp_store = memory;".as_ptr(),
        None,
        ptr::null_mut(),
        &mut err,
    );
    if rc != SQLITE_OK {
        log_err!("Cant execute pragmas: {}", CStr::from_ptr(err).to_string_lossy());
        sqlite3_free(err as *mut _);
        sqlite3_close(db);
        return -1;
    }

    rc = sqlite3_exec(
        db,
        c"CREATE TABLE IF NOT EXISTS _riff (    id INTEGER PRIMARY KEY,    version TEXT NOT NULL);".as_ptr(),
        None,
        ptr::null_mut(),
        &mut err,
    );
    if rc != SQLITE_OK {
        log_err!(
            "Cant execute meta table creation: {}",
            CStr::from_ptr(err).to_string_lossy()
        );
        sqlite3_free(err as *mut _);
        sqlite3_close(db);
        return -1;
    }

    sqlite3_close(db);
    0
}

pub unsafe fn database_open(read_only: bool) -> *mut sqlite3 {
    let mut db: *mut sqlite3 = ptr::null_mut();

    let mut flags = SQLITE_OPEN_CREATE;
    if read_only {
        flags |= SQLITE_OPEN_READONLY;
    } else {
        flags |= SQLITE_OPEN_READWRITE;
    }

    let rc = sqlite3_open_v2(c"./riff.db".as_ptr(), &mut db, flags, ptr::null());

    sqlite3_busy_timeout(db, 5000);

    if rc != SQLITE_OK {
        log_err!("cant open sqlite connection for worker: Rc = {}", rc);
        return ptr::null_mut();
    }

    db
}
