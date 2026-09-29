# Plan: migrating the remaining unsafe code

## Where the unsafe code is

`unsafe` occurrences outside test modules:

| module            | count | why it's unsafe                                                   |
|-------------------|------:|-------------------------------------------------------------------|
| sys               | 23 | libc / WinSock FFI                                                   |
| tcp_server        |  9 | calloc'd `pollfd` / client-data arrays, raw callbacks                |
| http_response     |  7 | malloc'd buffer behind a raw pointer                                 |
| http_parser       |  6 | malloc'd header and body buffers, raw input pointer                  |
| thread_pool       |  5 | workers hold `*const ThreadPool`, `SendPtr`                          |
| main              |  5 | raw server/pool pointers                                             |
| xmalloc           |  4 | the allocator itself                                                 |
| worker, socket_actions | 3 + 3 | the shared `*mut Request`                                      |
| service, protocol, database | 3 + 3 + 2 | sqlite FFI                                              |
| queue             |  2 | `unsafe impl Send` (it stores pointers without dereferencing them)   |
| others            |  1 each | FFI and C-string helpers                                       |

Almost all of it comes from one design choice in the C code: **a single heap `Request`
is shared by the network thread and a worker through a raw pointer.** It is never
freed, and on disconnect its buffers are freed even though a worker may still be using
them. Fixing that ownership (step 2) is what makes most of the other modules easy.

## Safety net (in place)

Run everything with `cargo test --release`. `RIFFDB_BIN=<exe>` runs the integration
suites against another build.

| suite | what it pins |
|---|---|
| `src/http_parser.rs` tests | parsing at every split point, byte-by-byte input, large bodies, keep-alive reuse, truncation, content-length quirks, the >24 headers panic (`should_panic`: flip it when that's fixed) |
| `src/http_response.rs` tests | exact wire bytes, growth past 8 KB |
| `src/protocol.rs` tests | exact JSON bytes for every column type, escapes, NaN/Inf and invalid UTF-8 failures |
| queue / channel / router / options tests | behaviour of the already-safe modules |
| `tests/mod_test.rs` | the ported riffdb.js suite, 1 worker |
| `tests/wire.rs` | raw keep-alive sockets against 4 workers: response integrity (catches C-1), split writes, 200 KB bodies and responses, error messages |
| `examples/bench.rs` + `tools/asmfn.py` | performance; see [PERFORMANCE.md](PERFORMANCE.md) |

Rule for every step: all suites green, and `bench` is no slower than the numbers
in PERFORMANCE.md (`query_1000` about 10.7k req/s).

## Steps

Each step is small enough to review and benchmark on its own.

### 1. Leaf buffers become owned types (no threading change)

- `HttpResponse` becomes `struct HttpResponse { buf: Vec<u8> }` with safe
  `status_code` / `body` / `clear` methods. The wire-format tests carry over unchanged.
- `HttpParser` keeps the same state machine but owns its buffers
  (`Vec<u8>` body, header values allocated on demand instead of 24 × 8 KiB up front,
  see C-5), with `feed(&mut self, &[u8])`. The parser tests switch from
  `Parser::feed` to calling the method directly.
- `Request` is allocated with `Box` instead of `xmalloc` + `write_bytes`, so its
  fields run `Drop`.

This step alone doesn't fix the use-after-free on disconnect, because the `Request`
is still shared; that's step 2.

### 2. Request ownership handoff (the core change)

- The network thread owns a `Connection { parser, socket, cancel: Arc<AtomicBool> }`
  for each client.
- When a request completes, it sends a **`Job`** (method, url, body `Vec<u8>`, socket
  handle, cancel flag) to a worker through a typed `Channel<Job>`. Nothing is
  shared mutably any more.
- The worker builds the response in its own buffer and sends it.
- Disconnect just drops the `Connection`; a job in flight keeps its own socket
  handle and sees `cancel`.
- `Queue<T>` / `Channel<T>` become generic, which removes `unsafe impl Send`, `SendPtr`
  and `*mut c_void`.

This removes the use-after-free and the leaked `Request`, and makes C-1 impossible by construction.

Open question: **ordering of keep-alive / pipelined requests.** Today two requests from
one connection can run on two workers at once. Options:
- (a) stop reading a connection while its job is in flight; this needs a worker → network-thread wakeup.
- (b) route by connection to a fixed worker.
- (c) keep it as it is.

### 3. SQLite layer

- `Db` / `Stmt` wrappers with `Drop` (finalize/close), either
  hand-written over `libsqlite3-sys` or through `rusqlite`.
- `service_*` returns `Result<Response, ServiceError>` instead of filling
  `ServiceState` through out-pointers.
- The request payload is parsed with a borrowing serde visitor instead of `Value`.
  It keeps yyjson's semantics: the first `q`/`args` key wins, a non-object root means
  "query is empty", nested values in `args` are skipped. That removes about 5 allocations per
  request and lets arguments bind with `SQLITE_STATIC`, as in C.
- Optional (behaviour-neutral) statement cache per worker (C-7).

### 4. Networking

- Replace `sys` + `tcp_server` with `std::net::{TcpListener, TcpStream}` plus a
  readiness crate (`mio` or `polling`), or keep a minimal safe `poll` wrapper.
- A worker send loop that handles partial writes (C-2).
- Decide the Linux `SO_REUSEPORT` behaviour (C-3).
- `tcp_server_create` failures report `os error 0` on Windows, because the cleanup
  calls overwrite the WinSock error before `main` prints it. Return the error instead.

### 5. Cleanup

- Delete `xmalloc`, `xstrdup`, `any.rs` and the remaining `*const c_char` plumbing.
- `main` becomes fully safe; `run_server` owns the pool (or an `Arc`) instead of
  handing out raw pointers.
- Goal: no `unsafe` outside the sqlite FFI wrapper, or none at all with `rusqlite`.

## Decisions needed before starting

1. **SQLite binding**: `rusqlite` (safe API, one more dependency, and we can't
   pass `SQLITE_OMIT_*` flags the same way) or a thin wrapper of our own over `libsqlite3-sys`
   (keeps the current build flags and exact control).
2. **Event loop**: `mio`, `polling`, or our own `poll` wrapper. `tokio` is also an option,
   but it's a much bigger change of design.
3. **Keep-alive ordering**: option (a), (b) or (c) from step 2.
4. **C quirks**: keep bit-exact during the migration, or fix as we go? These are the route-hash
   collisions, `"query len < 3"` for invalid JSON, only two `Content-Length` spellings, and the
   >24 header limit. Keeping them bit-exact means the tests stay the oracle, and fixes land as
   separate commits later.
5. **C-2 (partial send)**: fix now, or as part of step 4?
