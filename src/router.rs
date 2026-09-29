// Port of Router.h / Router.c

use rusqlite::Connection;

use crate::execute::execute;
use crate::http_response::HttpResponse;
use crate::query::query;
use crate::request::Request;

// k&r style shit...
// Sums the chars (signed, as `char` is in C on x86) up to the first NUL, or
// the end of the slice. The NUL itself adds 0, so it is simply not counted.
const fn hash(str: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    let mut i = 0;
    while i < str.len() && str[i] != 0 {
        hash = hash.wrapping_add(str[i] as i8 as i32 as u32);
        i += 1;
    }
    hash
}

// C computed these at startup in RouterInit(); here they are compile-time.
const EXECUTE_ROUTE: u32 = hash(b"/execute");
const QUERY_ROUTE: u32 = hash(b"/query");
const HEALTH_ROUTE: u32 = hash(b"/health");

pub fn router_route(req: &Request, db: &Connection, res: &mut HttpResponse) {
    // C hashes Parser.Url as a C string: the whole array up to its NUL.
    let route = hash(&req.url);

    if route == EXECUTE_ROUTE {
        execute(req, db, res);
        return;
    }
    if route == QUERY_ROUTE {
        query(req, db, res);
        return;
    }
    if route == HEALTH_ROUTE {
        res.status_code(200);
        res.body(b"health");
        return;
    }

    res.status_code(404);
    res.body(b"not found");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_c_semantics() {
        assert_eq!(hash(b"/query"), b"/query".iter().map(|&c| c as u32).sum::<u32>());
        // stops at NUL, like the C loop over a C string
        assert_eq!(hash(b"/query\0garbage"), QUERY_ROUTE);
        // signed char: bytes >= 0x80 subtract
        assert_eq!(hash(&[0xff]), (-1i32) as u32);
        // known collision in the original scheme: any permutation matches
        assert_eq!(hash(b"/yreuq"), QUERY_ROUTE);
        assert_ne!(EXECUTE_ROUTE, QUERY_ROUTE);
        assert_ne!(QUERY_ROUTE, HEALTH_ROUTE);
    }
}
