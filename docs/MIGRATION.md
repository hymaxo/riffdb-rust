# Migration: unsafe port → safe Rust (done)

The crate is `#![forbid(unsafe_code)]`. This file records how each unsafe construct
from the direct port was replaced, and the decisions behind it. The last raw-pointer
version is commit `1b179b5`.

## Decisions

| question | choice | why |
|---|---|---|
| SQLite binding | `rusqlite` on the bundled `libsqlite3-sys` | Zero `unsafe` in our code. The C build's `LIBSQLITE3_FLAGS` still apply, except `SQLITE_OMIT_AUTOINIT` (no safe `sqlite3_initialize`). `rusqlite`'s `extra_check` feature is off, so like C only the first statement runs. |
| Event loop | `mio` + `socket2` | Keeps the C shape exactly: one poll thread, a read-until-`EAGAIN` loop, a worker pool. `socket2` sets the same listen options through a safe API. The `polling` crate's `add` is `unsafe`. |
| Keep-alive ordering | unchanged (round-robin) | Same as C. Requests are now owned, so concurrency can't corrupt anything, but pipelined responses may be out of order, as in C. |
| C quirks | kept bit-exact | Route-hash collisions, "query len < 3" for invalid JSON, the two `Content-Length` spellings, first-key-wins JSON. The tests are the oracle. |
| Partial `send()` (C-2) | fixed | A safe writer has to handle short writes anyway. |

## What replaced what

| unsafe construct (direct port) | safe replacement |
|---|---|
| heap `Request` shared through `*mut` by the network thread and a worker | `Connection` (the network thread owns the parser) plus a `Request` **moved** to the worker (URL, body, `Arc<ConnShared>`) |
| `Request` never freed; buffers freed on disconnect (use-after-free) | the network thread drops its `Connection`; queued or running requests keep their `Arc` and see `cancel` |
| per-connection response buffer shared with workers (C-1 race) | one `HttpResponse` per worker, reused |
| `xmalloc`'d header and body buffers | `Vec<u8>`; header values grow on demand (same 8191-byte limit) |
| `strtoul` on a NUL-terminated header value | `strtoul_u32`, which emulates whitespace, sign, overflow → `ULONG_MAX`, and C `unsigned long` width |
| `HttpHeader` array indexed out of bounds after 24 headers | extra headers are ignored |
| `Queue` of `*mut c_void` + `unsafe impl Send` | generic `Queue<T>` / `Channel<T>` |
| `ThreadPool` workers holding `*const ThreadPool` | an `Arc<Shared>` with the mailboxes and the `working` flag; `stop()` closes the channels, so it can actually join |
| `sys.rs` (libc / WinSock FFI, `WSAPoll`) | `mio` + `socket2` |
| `TcpServer` callbacks as fn pointers + `void*` | the `TcpServerCallbacks` trait with an associated `ClientData`; tokens carry a slot generation so stale events are ignored |
| raw `sqlite3_*` calls, `sqlite3_errmsg` | `rusqlite` `Connection` / `Statement` / `Rows`; `sqlite_errmsg()` extracts the same text |
| `serde_json::Value` payload | a borrowing visitor (`Payload` / `Arg`) with yyjson semantics |
| `xstrdup`'d error strings, malloc'd JSON | `Cow<'static, str>` messages; JSON written into the response buffer |
| `Any` union | an enum |

## Behaviour changes

Everything else is kept. The complete list with reasons is in the README
("Deliberate deviations from C"):

- the ten PORT FIXes (memory safety, races, leaks, partial sends, more than 24 headers);
- Windows doesn't set `SO_REUSEADDR`;
- the trace-level parser dump now runs on the network thread, just before dispatch, instead
  of in the worker;
- `ThreadPoolStop` returns, where in C it blocked forever.

## Verification

- 31 unit tests, the 28 ported JS tests, and 5 wire tests against 4 workers. Checked with
  30+ back-to-back full runs.
- Byte-exact checks: HTTP responses, JSON output (golden and exhaustive escaping),
  and payload semantics.
- Performance: within a few percent of the unsafe build on every scenario. See
  [PERFORMANCE.md](PERFORMANCE.md#the-safe-migration).
