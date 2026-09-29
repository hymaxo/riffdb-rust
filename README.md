# riffdb (Rust port, phase 1: direct unsafe translation)

A line-by-line port of [ssleert/riffdb](https://github.com/ssleert/riffdb): SQLite over HTTP.
This phase keeps the C structure intact: raw pointers, `malloc`/`free`, and
`static mut` globals. That keeps it easy to diff against the original. Making it
safe comes in a later phase.

```
cargo run -- -p 9889 -t 4 -d ./data
curl -X POST localhost:9889/query -d '{"q":"SELECT ? AS x","args":[1]}'
```

Endpoints: `POST /execute`, `POST /query` (JSON body `{"q": "...", "args": [...]}`), `GET /health`.

## Docs

- [docs/PERFORMANCE.md](docs/PERFORMANCE.md): how to benchmark and read the assembly, the Rust
  optimizations so far, and the inefficiencies found in the C code (tracked only).
- [docs/MIGRATION.md](docs/MIGRATION.md): the plan for the remaining unsafe code, the safety
  net, and the decisions still open.

## Safe-Rust progress

Phase 2 converted the modules that sit outside the shared `Request` lifecycle
and the sqlite/socket FFI:

| Module        | Now                                                                          |
|---------------|------------------------------------------------------------------------------|
| log           | atomics instead of `static mut` globals; `log_flog` is safe                  |
| queue         | safe methods; one `unsafe impl Send` (it stores pointers, never dereferences them) |
| channel       | `Mutex<Queue>` + `Condvar`, all safe                                         |
| thread_pool   | `Vec`s instead of calloc'd arrays; `process` is safe; `start`/`stop` still unsafe (workers hold a raw pointer to the pool) |
| options       | owned `String`s, returns `Result<Options, ()>`, published once in a `OnceLock` |
| router        | route hash is a safe `const fn`; `router_init` is gone                       |
| utils         | `mkdir_if_not_exists(&str)`; `xstrdup` stays unsafe for its FFI callers      |
| main          | safe up to `run_server`, which keeps the unsafe server/pool setup            |

There is no `static mut` left in the crate.

## Tests

`cargo test` runs `tests/mod_test.rs`, a port of `packages/riffdb.js/mod_test.ts`.
Each test starts the real binary on a free port, with its own database and a
single worker (`-t 1`, as in `.vscode/launch.json`). Each test talks to the
server the way the JS client does. Two tests are `#[ignore]`d because they fail
against the C server too:
`blob_round_trip` (BLOBs aren't supported by the wire protocol) and
`large_batch_insert` (it binds the `VALUES` list as a parameter).
`large_batch_insert_inlined` is the working version of the second one.

## File mapping

| C                              | Rust                  |
|--------------------------------|-----------------------|
| main.c / main.h                | main.rs               |
| Options.c                      | options.rs (hand-rolled `getopt_long`) |
| Log.c / Log.h                  | log.rs (`log_trace!` … `log_fatal!` macros) |
| XMalloc.c                      | xmalloc.rs (libc malloc) |
| Utils.c                        | utils.rs              |
| Greeting.c                     | greeting.rs           |
| Any.h                          | any.rs (unused, as in C) |
| Queue.c                        | queue.rs              |
| Channel.c                      | channel.rs (std Mutex/Condvar instead of C11 mtx/cnd) |
| ThreadPool.c                   | thread_pool.rs (std::thread instead of thrd_create) |
| TcpServer.c                    | tcp_server.rs         |
| SocketActions.c                | socket_actions.rs     |
| HttpParser.c                   | http_parser.rs        |
| HttpResponse.c / HttpUtils.c   | http_response.rs / http_utils.rs |
| Request.h                      | request.rs            |
| Router.c                       | router.rs             |
| Execute.c / Query.c            | execute.rs / query.rs |
| Service.c                      | service.rs            |
| Protocol.c                     | protocol.rs           |
| DataBase.c                     | database.rs           |
| (new)                          | sys.rs: socket/poll shim, libc on Unix, WinSock `WSAPoll` on Windows |

## Library substitutions

- **sqlite**: `libsqlite3-sys` (bundled), raw `sqlite3_*` FFI calls as before.
  The compile flags from `CMakeLists.txt` are passed through `.cargo/config.toml`.
- **yyjson**: `serde_json` for parsing. The output side is a small writer in
  `protocol.rs` that copies yyjson's minified format.
- **cwpack**: dropped. `ProtocolBindMsgpackArgsToStmt` was never called.
- **wolfssl**: dropped. It was only initialised and printed in `--version`.
- **mimalloc**: not used. Release builds use the system allocator.

## Deliberate deviations

These places are marked `PORT FIX` in the code. Each one fixed memory corruption or a
request-breaking bug in the C version:

1. `HttpResponseAppend` doubled the capacity only once, so any response over
   about 8 KB overflowed the heap. Now it keeps doubling until the data fits.
2. `HttpParserParseBody` copied bytes beyond Content-Length into the body buffer
   (heap overflow on pipelined requests). The copy is now clamped.
3. `HttpParserParseBody` re-applied `BodyStart` on the next `read()` when the
   body arrived separately from the headers. Now it is applied only once.
4. `SocketActionsOnReadable` sent requests to a worker before the body was
   complete. Now it waits for `Complete`.
5. Leaks: sqlite error strings (the `XFree` sat after a `return`) and the
   `/query` JSON buffer are now freed.

## Known issues kept from C (for the safe rewrite)

- A disconnect frees the parser and response buffers while a worker may still
  be using the `Request` (use-after-free). The `Request` itself is never freed.
- The main thread and the workers share `Request` with no synchronisation beyond the
  `cancel` flag.
- More than 24 headers index out of bounds in `Headers[]`. In Rust this is a
  bounds-check panic instead of silent corruption.
- `ThreadPoolStop` never returns, because workers block in `channel_recv`.
  `Enqueue` on a full queue silently drops the request.
- Routes are matched by a sum-of-chars hash, so e.g. `/yreuq` routes to `/query`.
- Invalid JSON returns the error message `query len < 3`.
