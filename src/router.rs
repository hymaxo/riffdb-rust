use rusqlite::Connection;

use crate::execute::execute;
use crate::http_response::HttpResponse;
use crate::query::query;
use crate::request::Request;

// k&r style shit...
const fn hash(str: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    let mut i = 0;
    while i < str.len() && str[i] != 0 {
        hash = hash.wrapping_add(str[i] as i8 as i32 as u32);
        i += 1;
    }
    hash
}

const EXECUTE_ROUTE: u32 = hash(b"/execute");
const QUERY_ROUTE: u32 = hash(b"/query");
const HEALTH_ROUTE: u32 = hash(b"/health");

pub fn route(req: &Request, db: &Connection, res: &mut HttpResponse) {
    match hash(&req.url) {
        EXECUTE_ROUTE => execute(req, db, res),
        QUERY_ROUTE => query(req, db, res),
        HEALTH_ROUTE => res.status_and_body(200, b"health"),
        _ => res.status_and_body(404, b"not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_sums_signed_bytes_up_to_nul() {
        assert_eq!(hash(b"/query"), b"/query".iter().map(|&c| c as u32).sum::<u32>());
        assert_eq!(hash(b"/query\0garbage"), QUERY_ROUTE);
        assert_eq!(hash(&[0xff]), (-1i32) as u32);
        assert_eq!(hash(b"/yreuq"), QUERY_ROUTE);
        assert_ne!(EXECUTE_ROUTE, QUERY_ROUTE);
        assert_ne!(QUERY_ROUTE, HEALTH_ROUTE);
    }
}
