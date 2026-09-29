// Port of Service.h / Service.c

use rusqlite::{Connection, Statement};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::database::sqlite_errmsg;
use crate::protocol::{protocol_bind_json_args_to_stmt, protocol_json_from_stmt, protocol_parse_payload, JsonWriter};

pub type ServiceError = i32;
pub const SERVICE_OK: ServiceError = 0;
pub const SERVICE_ERROR_INCORRECT_JSON: ServiceError = -1;
pub const SERVICE_ERROR_QUERY_EMPTY: ServiceError = -2;
pub const SERVICE_ERROR_QUERY_LEN: ServiceError = -3;
pub const SERVICE_ERROR_PROTOCOL: ServiceError = -4;
pub const SERVICE_ERROR_SQLITE: ServiceError = -5;
pub const SERVICE_ERROR_CANT_CREATE_JSON: ServiceError = -6;
pub const SERVICE_ERROR_CANCEL: ServiceError = -7;

/// What `Res`/`ResSize` point at in C.
pub enum ServiceRes {
    /// A fixed or error message (C: string literal or strdup'd errmsg).
    Msg(Cow<'static, str>),
    /// The /query JSON, in `ServiceState::json_buf`.
    Json,
}

pub struct ServiceState<'a> {
    pub cancel: &'a AtomicBool,
    pub db: &'a Connection,
    pub payload: &'a [u8],

    pub res: ServiceRes,
    pub status: u16,

    /// Where /query appends its JSON: the worker's response buffer (C: a
    /// fresh yyjson doc + output buffer per query, leaked).
    pub json_buf: &'a mut Vec<u8>,
}

impl<'a> ServiceState<'a> {
    pub fn new(cancel: &'a AtomicBool, db: &'a Connection, payload: &'a [u8], json_buf: &'a mut Vec<u8>) -> Self {
        ServiceState {
            cancel,
            db,
            payload,
            res: ServiceRes::Msg(Cow::Borrowed("")),
            status: 0,
            json_buf,
        }
    }

    /// Res[0..ResSize]
    pub fn res(&self) -> &[u8] {
        match &self.res {
            ServiceRes::Msg(m) => m.as_bytes(),
            ServiceRes::Json => self.json_buf,
        }
    }
}

#[inline]
fn prepare<'db>(
    cancel: &AtomicBool,
    err: &mut ServiceRes,
    payload: &[u8],
    db: &'db Connection,
) -> Result<Statement<'db>, ServiceError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(SERVICE_ERROR_CANCEL);
    }
    let Ok(doc) = protocol_parse_payload(payload) else {
        // (sic) message kept from the original
        *err = ServiceRes::Msg(Cow::Borrowed("query len < 3"));
        return Err(SERVICE_ERROR_INCORRECT_JSON);
    };

    let Some(query_obj) = doc.q else {
        *err = ServiceRes::Msg(Cow::Borrowed("query is empty"));
        return Err(SERVICE_ERROR_QUERY_EMPTY);
    };

    // yyjson_get_str / yyjson_get_len on a non-string yield NULL / 0
    let query: &str = query_obj.as_deref().unwrap_or("");
    if query.len() < 3 {
        *err = ServiceRes::Msg(Cow::Borrowed("query len < 3"));
        return Err(SERVICE_ERROR_QUERY_LEN);
    }

    let mut stmt = match db.prepare(query) {
        Ok(stmt) => stmt,
        Err(e) => {
            *err = ServiceRes::Msg(Cow::Owned(sqlite_errmsg(&e)));
            return Err(SERVICE_ERROR_SQLITE);
        }
    };

    if let Some(args) = &doc.args {
        if !args.is_empty() && protocol_bind_json_args_to_stmt(args, &mut stmt) != 0 {
            return Err(SERVICE_ERROR_PROTOCOL);
        }
    }

    Ok(stmt)
}

pub fn service_execute(self_: &mut ServiceState) -> ServiceError {
    let ret = 'body: {
        let mut stmt = match prepare(self_.cancel, &mut self_.res, self_.payload, self_.db) {
            Ok(stmt) => stmt,
            Err(ret) => break 'body ret,
        };

        // One sqlite3_step: SQLITE_ROW or SQLITE_DONE are both fine.
        if let Err(e) = stmt.raw_query().next() {
            self_.res = ServiceRes::Msg(Cow::Owned(sqlite_errmsg(&e)));
            break 'body SERVICE_ERROR_SQLITE;
        }

        self_.status = 200;
        self_.res = ServiceRes::Msg(Cow::Borrowed("ok"));
        return SERVICE_OK;
    };

    // cleanup:
    self_.status = 500;
    ret
}

pub fn service_query(self_: &mut ServiceState) -> ServiceError {
    let ret = 'body: {
        let mut stmt = match prepare(self_.cancel, &mut self_.res, self_.payload, self_.db) {
            Ok(stmt) => stmt,
            Err(ret) => break 'body ret,
        };

        let mut res_doc = JsonWriter::new(self_.json_buf);
        if let Err(e) = protocol_json_from_stmt(&mut res_doc, &mut stmt) {
            self_.res = ServiceRes::Msg(Cow::Owned(sqlite_errmsg(&e)));
            break 'body SERVICE_ERROR_SQLITE;
        }

        if res_doc.failed {
            self_.res = ServiceRes::Msg(Cow::Borrowed("cant create json"));
            break 'body SERVICE_ERROR_CANT_CREATE_JSON;
        }

        self_.status = 200;
        self_.res = ServiceRes::Json;
        return SERVICE_OK;
    };

    // cleanup:
    self_.status = 500;
    ret
}
