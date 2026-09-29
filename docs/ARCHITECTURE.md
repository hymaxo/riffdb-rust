# Architecture

## How a request flows

```
client ──► network thread ──────────────► worker N ──► client
           (mio poll, parse)   mailbox    (SQLite, JSON, send)
```

1. **One network thread** (`tcp_server.rs`) polls the listening socket and every
   client through `mio` (epoll, kqueue or IOCP). It accepts up to 1024 clients and
   reads each readable socket until it's drained.
2. **Parsing** (`socket_actions.rs`, `http_parser.rs`). Each connection owns an
   incremental parser that is fed whatever each `read()` returns. When a request is
   complete, its URL and body move into a `Request`, together with an `Arc` handle
   to the socket and a cancel flag.
3. **Dispatch** (`thread_pool.rs`, `channel.rs`, `queue.rs`). Requests go to the
   workers round-robin, one bounded mailbox (1024 entries) per worker.
4. **Workers** (`worker.rs`). Each worker has its own SQLite connection and response
   buffer. It routes the request (`router.rs`), runs it (`service.rs`), writes the
   response, and sends it straight from the worker thread.
5. **Disconnects.** The network thread drops its side of the connection and sets
   the cancel flag. A worker holding a request for that client skips it or stops
   sending, and the socket closes once the last `Arc` is gone.

Everything is owned; nothing is shared through raw pointers. The crate is
`#![forbid(unsafe_code)]`.

## Modules

| module | role |
|---|---|
| `main.rs` | startup: options, database init, server and pool |
| `options.rs` | command line (a small `getopt_long`-compatible parser) |
| `log.rs` | leveled logging macros, `log_trace!` … `log_fatal!` |
| `tcp_server.rs` | the event loop; callbacks through the `TcpServerCallbacks` trait |
| `socket_actions.rs` | the network thread's per-connection state and read handling |
| `http_parser.rs` | incremental HTTP/1.1 request parser |
| `request.rs` | what a worker receives |
| `thread_pool.rs`, `channel.rs`, `queue.rs` | worker pool, blocking mailbox, ring buffer |
| `worker.rs` | the worker loop and `send_all` |
| `router.rs` | route table |
| `query.rs`, `execute.rs` | the two SQL endpoints |
| `service.rs` | payload → prepared statement → result |
| `protocol.rs` | JSON payload parsing and row serialization |
| `http_response.rs` | response bytes |
| `database.rs` | opening connections, the busy handler |
| `greeting.rs`, `utils.rs` | banner, small helpers |

## Dependencies

- **`rusqlite`** with bundled SQLite. The compile options are set through
  `LIBSQLITE3_FLAGS` in `.cargo/config.toml` (WAL-friendly defaults, no shared
  cache, no deprecated APIs, `SQLITE_DQS=0`, …).
- **`mio`** for the event loop and **`socket2`** for listen options:
  `SO_REUSEADDR` (not on Windows), `SO_REUSEPORT` on Linux, backlog 128.
- **`serde` / `serde_json`** to read payloads. A borrowing visitor avoids copying
  the SQL text and string arguments when they contain no escapes.
- **`chrono`** for log timestamps.

## Protocol details

These behaviours are part of the wire protocol that existing clients see, so they
are kept on purpose:

- **Routing** compares a sum of the URL's bytes, so any permutation of a route's
  characters matches it (`/yreuq` is `/query`). Query strings are not stripped.
- **Error messages** are fixed strings that clients may match on: invalid JSON gives
  `query len < 3`, a missing `q` gives `query is empty`, and a `q` shorter than 3
  bytes gives `query len < 3`. SQLite errors pass through as `sqlite3_errmsg` text.
- **Payloads** are strict JSON. When a key appears twice, the first one wins. A root
  that isn't an object has no `q`. A non-string `q` counts as empty. A non-array
  `args` binds nothing. Objects and arrays inside `args` are skipped but still use
  up a placeholder index. Bind errors (more args than `?`) are ignored.
- **Results.** Text is cut at the first NUL. BLOB columns come back as `null`.
  NaN/Inf or invalid UTF-8 in a result fails the whole request with `cant create json`.
- **Only the first statement** in `q` runs; the rest is ignored.
- **HTTP.** Only `Content-Length` and `content-length` are recognised, and values are
  parsed leniently (like `strtoul`). The HTTP version isn't checked. The URL is
  truncated at 63 bytes and header keys at 63; header values are capped at 8191
  bytes, and only the first 24 headers are kept.
- **Ordering.** Pipelined requests on one connection can go to different workers,
  so their responses may arrive out of order. A client that waits for each response
  before sending the next request (every normal HTTP client) is unaffected. A full
  mailbox drops the request.

## Limits and timeouts

| what | value |
|---|---|
| concurrent clients | 1024 (more are accepted and closed) |
| queued requests per worker | 1024 |
| worker threads | 1–255 |
| waiting on a locked database | 5 s, then `database is locked` |
| a client that stops reading | 10 s without progress, then the response is dropped |

## Differences from the C implementation

The server started as a Rust rewrite of [ssleert/riffdb](https://github.com/ssleert/riffdb)
and speaks the same protocol. Beyond the language, a few things behave differently,
all on purpose:

- Responses of any size work. The C response buffer grows only once, so larger
  results overflow it.
- Each worker has its own response buffer, so keep-alive responses can't be mixed
  up when there is more than one worker.
- Requests are dispatched only once their body is complete. Bytes past
  `Content-Length` are left for the next request.
- A client disconnecting while its request runs is safe, and nothing leaks.
- Partial `send()`s are retried until the whole response is written.
- `SO_REUSEADDR` isn't set on Windows, where it would let two servers share a port.
- Lock waits are 20 µs–1 ms instead of 1–100 ms (same 5 s limit), and prepared
  statements are cached. See [PERFORMANCE.md](PERFORMANCE.md).
- The worker pool can shut down cleanly.
