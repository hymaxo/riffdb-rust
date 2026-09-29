# riffdb (Rust port)

A port of [ssleert/riffdb](https://github.com/ssleert/riffdb): SQLite over HTTP.
The module layout follows the C sources one to one, and the behaviour on the
wire is kept the same, including the C code's quirks. The crate is entirely
safe Rust (`#![forbid(unsafe_code)]`).

```
cargo run -- -p 9889 -t 4 -d ./data
curl -X POST localhost:9889/query -d '{"q":"SELECT ? AS x","args":[1]}'
```

Endpoints: `POST /execute`, `POST /query` (JSON body `{"q": "...", "args": [...]}`), `GET /health`.

## Docs

- [docs/PERFORMANCE.md](docs/PERFORMANCE.md): how to benchmark and read the assembly,
  what was optimized and by how much, and the inefficiencies found in the C code.
- [docs/MIGRATION.md](docs/MIGRATION.md): how the unsafe port became safe, and what
  replaced each unsafe construct.

## Architecture (same as C)

One network thread polls the listening socket and all clients. It parses requests
byte by byte and hands each complete request to a pool of worker threads,
round-robin, through one mailbox per worker. Each worker has its own SQLite
connection. It routes the request, builds the response and writes it to the
client socket.

The difference is ownership. In C, one heap `Request` per connection is shared
by the network thread and the workers through a raw pointer. Here:

- the network thread owns each connection's parser (`Connection`);
- a finished request is **moved** to a worker as a `Request` (URL, body, and an
  `Arc` to the socket and cancel flag);
- each worker owns its own response buffer.

## File mapping

| C                              | Rust                  |
|--------------------------------|-----------------------|
| main.c / main.h                | main.rs               |
| Options.c                      | options.rs (hand-rolled `getopt_long`) |
| Log.c / Log.h                  | log.rs (`log_trace!` … `log_fatal!` macros) |
| XMalloc.c                      | (gone: Rust allocations)   |
| Utils.c                        | utils.rs              |
| Greeting.c                     | greeting.rs           |
| Any.h                          | any.rs (an enum; unused, as in C) |
| Queue.c                        | queue.rs (generic `Queue<T>`) |
| Channel.c                      | channel.rs (`Mutex` + `Condvar`, generic, closable) |
| ThreadPool.c                   | thread_pool.rs (`std::thread`, shared state in an `Arc`) |
| TcpServer.c                    | tcp_server.rs (`mio` + `socket2`, callbacks as a trait) |
| SocketActions.c                | socket_actions.rs (+ `Connection`, the network thread's half of `Request`) |
| HttpParser.c                   | http_parser.rs (same state machine, owned buffers) |
| HttpResponse.c / HttpUtils.c   | http_response.rs / http_utils.rs |
| Request.h                      | request.rs (what moves to a worker) |
| Router.c                       | router.rs             |
| Execute.c / Query.c            | execute.rs / query.rs |
| Service.c                      | service.rs            |
| Protocol.c                     | protocol.rs           |
| DataBase.c                     | database.rs           |

## Library substitutions

- **sqlite**: `rusqlite` on top of the bundled `libsqlite3-sys`. The compile flags
  from `CMakeLists.txt` are passed through `.cargo/config.toml`. The one exception is
  `SQLITE_OMIT_AUTOINIT`: sqlite now initialises itself on first use, because
  `sqlite3_initialize()` has no safe binding. There is no visible difference.
- **yyjson**: requests are read with a borrowing serde visitor that keeps
  `yyjson_obj_get` semantics (first key wins, a non-object root means "no q").
  Responses are written by a writer in `protocol.rs` that produces yyjson's exact bytes.
- **poll / sockets**: `mio` (epoll / kqueue / IOCP) and `socket2`, which sets the
  same listen options as C: `SO_REUSEADDR`, `SO_REUSEPORT` on Linux, backlog 128.
- **cwpack**: dropped. `ProtocolBindMsgpackArgsToStmt` was never called.
- **wolfssl**: dropped. It was only initialised and printed in `--version`.
- **mimalloc**: not used. Release builds use the system allocator.

## Deliberate deviations from C

Each of these fixes memory corruption, a race, or a request-breaking bug in the C
code. The code marks them `PORT FIX`. Everything else, quirks included, behaves
like C.

1. **Response buffer overflow.** `HttpResponseAppend` doubled the capacity only once, so
   responses over about 8 KB overflowed the heap. The buffer is now a `Vec`.
2. **Body overflow.** `HttpParserParseBody` copied bytes beyond Content-Length into the
   body buffer (heap overflow on pipelined requests). The copy is clamped.
3. **`BodyStart` re-applied.** When the body arrived in a later `read()` than the headers,
   the offset was applied again. It is now applied once.
4. **Incomplete bodies dispatched.** `SocketActionsOnReadable` sent requests to a worker
   before the body was complete. It now waits for `Complete`.
5. **Keep-alive race.** With more than one worker, back-to-back keep-alive requests got
   corrupted responses (send-then-zero race). Each worker now has its own buffer.
6. **Use-after-free on disconnect.** A disconnect freed buffers a worker might still be
   using. Requests are now owned, and a disconnect only sets `cancel`.
7. **Truncated responses.** Large responses could be cut off by a partial `send()`. The
   worker now writes until done: it retries on `WouldBlock`, gives up once the client
   has disconnected, and times out after 10 s with no progress.
8. **Leaks.** Error strings, the yyjson request and response buffers, and every `Request`
   were leaked; they no longer are.
9. **More than 24 headers.** C writes past the end of `Headers[]`; the extra headers
   are now ignored.
10. **Windows port sharing.** `SO_REUSEADDR` is not set on Windows, because there it lets
    a second server share the port.

## Kept as in C

- Routes are matched by a sum-of-chars hash, so e.g. `/yreuq` routes to `/query`.
- Invalid JSON returns the error message `query len < 3`.
- Only `Content-Length` and `content-length` are recognised.
- Requests are dispatched round-robin. Pipelined requests on one connection may be
  answered out of order, and a full mailbox (1024 requests) silently drops the request.
- Only the first SQL statement of `q` runs; the rest is ignored.
- Per-connection state such as `PRAGMA foreign_keys` applies only to the worker
  connection that ran it.

## Tests

`cargo test --release` runs three suites:

- **Unit tests:** the parser at every split point, the exact response bytes, golden
  JSON, payload semantics, word-at-a-time escaping against a reference, the queue,
  channel, thread pool, router and options.
- **`tests/mod_test.rs`:** a port of `packages/riffdb.js/mod_test.ts` against the real
  binary, with 1 worker as in `.vscode/launch.json`. Two tests are `#[ignore]`d
  because they fail against the C server too: `blob_round_trip` (BLOBs aren't
  supported by the wire protocol) and `large_batch_insert` (it binds the `VALUES` list
  as a parameter). `large_batch_insert_inlined` is the working version of the second.
- **`tests/wire.rs`:** raw keep-alive sockets against 4 workers. It checks response
  integrity, split writes, 200 KB bodies and responses, and the error messages.

`RIFFDB_BIN=<exe>` runs the integration suites against another build.
