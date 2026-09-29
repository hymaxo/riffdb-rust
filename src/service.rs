// Port of Service.h / Service.c

use libc::c_char;
use libsqlite3_sys::*;
use serde_json::Value;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::protocol::{protocol_bind_json_args_to_stmt, protocol_json_from_stmt, JsonWriter};
use crate::utils::xstrdup;

pub type ServiceError = i32;
pub const SERVICE_OK: ServiceError = 0;
pub const SERVICE_ERROR_INCORRECT_JSON: ServiceError = -1;
pub const SERVICE_ERROR_QUERY_EMPTY: ServiceError = -2;
pub const SERVICE_ERROR_QUERY_LEN: ServiceError = -3;
pub const SERVICE_ERROR_PROTOCOL: ServiceError = -4;
pub const SERVICE_ERROR_SQLITE: ServiceError = -5;
pub const SERVICE_ERROR_CANT_CREATE_JSON: ServiceError = -6;
pub const SERVICE_ERROR_CANCEL: ServiceError = -7;

pub struct ServiceState {
    pub cancel: *const AtomicBool,
    pub db: *mut sqlite3,
    pub payload: *const c_char,
    pub payload_len: u32,

    pub res_size: u32,
    pub res: *const c_char,
    pub status: u16,

    /// Owns the /query JSON that `res` points into (C: the yyjson output
    /// buffer, which it leaked). Dropped with the state.
    pub res_buf: Vec<u8>,
}

#[inline]
unsafe fn prepare(
    cancel: *const AtomicBool,
    err: *mut *const c_char,
    payload_len: u32,
    payload: *const c_char,
    db: *mut sqlite3,
    stmt: *mut *mut sqlite3_stmt,
) -> ServiceError {
    if (*cancel).load(Ordering::SeqCst) {
        return SERVICE_ERROR_CANCEL;
    }

    let payload_slice: &[u8] = if payload_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(payload as *const u8, payload_len as usize)
    };

    let doc: Value = match serde_json::from_slice(payload_slice) {
        Ok(v) => v,
        Err(_) => {
            // (sic) message kept from the original
            *err = c"query len < 3".as_ptr();
            return SERVICE_ERROR_INCORRECT_JSON;
        }
    };
    let root = &doc;

    let Some(query_obj) = root.get("q") else {
        *err = c"query is empty".as_ptr();
        return SERVICE_ERROR_QUERY_EMPTY;
    };

    // yyjson_get_str / yyjson_get_len on a non-string yield NULL / 0
    let (query, query_len) = match query_obj.as_str() {
        Some(s) => (s.as_ptr() as *const c_char, s.len()),
        None => (ptr::null(), 0),
    };
    if query_len < 3 {
        *err = c"query len < 3".as_ptr();
        return SERVICE_ERROR_QUERY_LEN;
    }

    let args = root.get("args");

    let mut rc = sqlite3_prepare_v3(db, query, query_len as i32, 0, stmt, ptr::null_mut());
    if rc != SQLITE_OK {
        *err = xstrdup(sqlite3_errmsg(db));
        return SERVICE_ERROR_SQLITE;
    }

    if let Some(args) = args {
        if args.as_array().is_some_and(|a| !a.is_empty()) {
            rc = protocol_bind_json_args_to_stmt(args, *stmt);
            if rc != 0 {
                return SERVICE_ERROR_PROTOCOL;
            }
        }
    }

    SERVICE_OK
}

pub unsafe fn service_execute(self_: *mut ServiceState) -> ServiceError {
    let mut ret;
    let mut stmt: *mut sqlite3_stmt = ptr::null_mut();

    'body: {
        ret = prepare(
            (*self_).cancel,
            ptr::addr_of_mut!((*self_).res),
            (*self_).payload_len,
            (*self_).payload,
            (*self_).db,
            &mut stmt,
        );
        if ret != SERVICE_OK {
            break 'body;
        }

        let rc = sqlite3_step(stmt);
        if rc != SQLITE_ROW && rc != SQLITE_DONE {
            (*self_).res = xstrdup(sqlite3_errmsg((*self_).db));
            ret = SERVICE_ERROR_SQLITE;
            break 'body;
        }

        (*self_).status = 200;
        (*self_).res_size = 2;
        (*self_).res = c"ok".as_ptr();

        sqlite3_finalize(stmt);
        return SERVICE_OK;
    }

    // cleanup:
    (*self_).status = 500;
    sqlite3_finalize(stmt);
    ret
}

/// On success `res` points into `res_buf`.
pub unsafe fn service_query(self_: *mut ServiceState) -> ServiceError {
    let mut ret;
    let mut stmt: *mut sqlite3_stmt = ptr::null_mut();

    let mut res_doc = JsonWriter::new();
    'body: {
        ret = prepare(
            (*self_).cancel,
            ptr::addr_of_mut!((*self_).res),
            (*self_).payload_len,
            (*self_).payload,
            (*self_).db,
            &mut stmt,
        );
        if ret != SERVICE_OK {
            break 'body;
        }

        let rc = protocol_json_from_stmt(&mut res_doc, stmt);
        if rc != SQLITE_DONE {
            (*self_).res = xstrdup(sqlite3_errmsg((*self_).db));
            ret = SERVICE_ERROR_SQLITE;
            break 'body;
        }

        if res_doc.failed {
            (*self_).res = c"cant create json".as_ptr();
            ret = SERVICE_ERROR_CANT_CREATE_JSON;
            break 'body;
        }

        // Hand the buffer over instead of copying it into a malloc'd block.
        (*self_).res_buf = std::mem::take(&mut res_doc.out);

        (*self_).status = 200;
        (*self_).res_size = (*self_).res_buf.len() as u32;
        (*self_).res = (*self_).res_buf.as_ptr() as *const c_char;

        sqlite3_finalize(stmt);
        return SERVICE_OK;
    }

    // cleanup:
    (*self_).status = 500;
    sqlite3_finalize(stmt);
    ret
}
