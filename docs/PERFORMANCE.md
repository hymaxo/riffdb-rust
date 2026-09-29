# Performance notes

## How to measure

```sh
# terminal 1: a release server with 4 workers
cargo run --release -- -p 19990 -t 4 -d ./benchdb

# terminal 2: keep-alive load (8 connections, 4 s per scenario)
cargo run --release --example bench -- 19990 8 4
```

Scenarios: `health` (no sqlite), `query_point` (1 row by primary key),
`query_1000` (1000 rows, 93 KB of JSON), `execute` (single-row UPDATE).
Client and server share one machine, so small-request numbers are noisy by
±10%. Compare builds by running them alternately a few times, not once each.

### Reading the assembly

```sh
cargo rustc --release --bin riffdb -- --emit asm -C "llvm-args=-x86-asm-syntax=intel"
python tools/asmfn.py target/release/deps/riffdb.s socket_actions_on_readable --stats  # calls per function
python tools/asmfn.py target/release/deps/riffdb.s worker_handler                      # full body, demangled
```

The output is post-LTO, so most helpers are inlined into their callers. `--stats`
lists every call a function makes. Heap calls (`HeapAlloc`/`HeapFree`,
`process_heap_alloc`), `memset`, `core::fmt::write` or `from_utf8_lossy` on a
hot path are the things to look for.

## Rust-side optimizations so far

Machine: 16 threads, Windows 11. The server runs with `-t 4`. Numbers are averages of 3 alternating runs.

| scenario    | before        | after          |
|-------------|---------------|----------------|
| health      | 230k req/s    | 244k req/s (+6%) |
| query_point | 154k req/s    | 164k req/s (+7%) |
| query_1000  | 6.1k req/s, p99 3.7 ms | 10.7k req/s (+75%), p99 2.1 ms |
| execute     | ~610 req/s    | ~600 req/s (lock-bound, see C-8) |

What the assembly showed, and what changed:

1. **Trace logging built its arguments even when disabled.** The worker made about 20 `log_flog`
   calls per request. Before each one, `dump()` copied the parser state, the whole body and the
   whole response into temporary `String`s, only for `log_flog` to return early. The log macros now
   check the level first, `dump` borrows (`Cow`), and the parser dump is a separate `#[cold]`
   function. C doesn't have this cost: its `%.*s` arguments are just pointers.
2. **8 KiB `memset` per `read()`.** `[0; 8192]` zeroed the receive buffer on every read. It is
   now `MaybeUninit`, like the C stack buffer.
3. **Two heap allocations per response.** `status.to_string()` and `len.to_string()` are
   replaced by `fmt_u64` into a stack buffer.
4. **JSON output:**
   - The writer pushed one `char` at a time with a capacity check on each push; it now copies
     unescaped runs in bulk using a 256-entry lookup table.
   - Integers went through `core::fmt`; they are now formatted on the stack.
   - Column names were escaped for every cell; they are now escaped once per statement.
   - The finished JSON was copied into a fresh `malloc` block before being copied into the
     response. `ServiceState` now owns the `Vec`, which saves one copy of the whole result.

   The output is byte-identical; the golden tests in `protocol.rs` check this.
5. **One wasted `recv` per request.** The read loop called `recv` until `EWOULDBLOCK`. A short read
   now counts as "drained" (`TCP_SERVER_READ_DRAINED`). `poll` is level-triggered, so no data can
   be missed.
6. **Condvar notify under the lock.** `Channel::send` now notifies after unlocking, so the woken
   worker doesn't immediately block on the mutex.
7. The out-of-memory paths are `#[cold]`, so the inlined `malloc` wrappers stay small.

Still visible in the assembly, left for the rewrite: 9 bounds checks in the per-byte parser loop,
and `serde_json::Value` building a tree (about 5 allocations) for every request payload.

## Found in the C code (tracked only; the C code is not modified)

Correctness:

- **C-1: send-then-zero race.** `Worker.c` calls `send()` and then `HttpResponseZero()`. With more
  than one worker, the client's next keep-alive request goes to another worker, which appends to the
  same response buffer while this worker resets it, so responses get corrupted. It never shows with
  `-t 1`, which the test suite and `launch.json` use. Fixed in the port (`worker.rs`, PORT FIX), with
  a regression test in `tests/wire.rs`.
- **C-2: partial `send()` is ignored.** Client sockets are non-blocking, and the worker calls
  `send()` once, ignoring a short write. A large response can be truncated when the socket buffer
  is full. Windows loopback buffers enough that the wire test with a 400 KB response passes here, but
  Linux is more likely to hit it. **Not fixed yet** (it is also in the port).
- **C-3: two servers can share a port.** Linux gets `SO_REUSEPORT` (option `15`), so a second riffdb
  on the same port starts "successfully" and the kernel load-balances connections between two
  processes with different databases. (The Windows port had the same effect through WinSock's
  `SO_REUSEADDR`; it now uses `SO_EXCLUSIVEADDRUSE`.)
- **C-4: memory leaks.** The yyjson request document from `Prepare()` is leaked for every
  `/query` and `/execute`. The `yyjson_mut_write` output buffer is leaked for every `/query`.
  sqlite error strings are leaked, because the `XFree` sits after a `return`.
- Also listed in the README: heap overflow on responses over about 8 KB, body overflow on pipelined
  bytes, re-applied `BodyStart`, dispatch before the body is complete, more than 24 headers, the
  use-after-free on disconnect.

Performance:

- **C-5: about 194 KiB allocated per connection up front.** `HttpParserInit` allocates 24 × 8 KiB
  header-value buffers plus a 2 KiB body, which is about 200 MB at the 1024-client limit.
- **C-6: JSON is built as a tree and then serialized.** Every value in the `yyjson_mut_doc` is an
  allocation, the column name is re-added on every row, and the result is copied again into the
  response. Streaming rows straight into the response buffer would avoid all of that.
- **C-7: no prepared-statement cache.** `sqlite3_prepare_v3` runs on every request.
- **C-8: writers contend across connections.** Each worker has its own connection, so concurrent
  writes fight over the SQLite write lock. The busy handler sleeps in whole milliseconds, so
  `execute` peaks at about 600 req/s with p99 about 61 ms. A single writer (or routing writes to one
  worker) would remove the contention.
- **C-9: round-robin dispatch ignores load.** A slow query blocks everything queued behind it on
  that worker while other workers sit idle.
- **C-10: `poll()` scans every fd on each wakeup.** With 1024 connections that is O(n) per wakeup;
  epoll, kqueue or IOCP would scale better.
- **C-11: one extra `read()` per request to get `EAGAIN`.** Avoided in the port, see (5) above.
- **C-12: condvar signalled while holding the mutex.** Avoided in the port, see (6) above.
- **C-13: byte-at-a-time parser.** It runs a `switch` per byte and `strncmp`s every header twice for
  `Content-Length`. Scanning for `\r\n` and `:` with `memchr` would be much faster.
- **C-14: about 20 `LogFlog` calls per request in the worker, even in release.** Each returns early,
  but it is still about 20 calls plus argument setup.
