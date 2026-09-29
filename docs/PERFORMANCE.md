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
# network thread (parser + socket actions are inlined into it)
python tools/asmfn.py target/release/deps/riffdb.s 'TcpServer$LT$D$GT$3run' --stats
# worker thread (router, service, protocol are inlined into std's thread entry)
python tools/asmfn.py target/release/deps/riffdb.s __rust_begin_short_backtrace --stats
```

The output is post-LTO, so most helpers are inlined into their callers. `--stats`
lists every call a function makes. Heap calls (`HeapAlloc`/`HeapFree`,
`process_heap_alloc`), `memset`, `core::fmt::write` or `from_utf8_lossy` on a
hot path are the things to look for.

For code that's hard to see in a whole-server benchmark, time it in isolation with an
ignored unit test. For example, the JSON writer:
`cargo test --release --bin riffdb json_micro -- --ignored --nocapture`.

### Against the C original

`compare/` builds the C original and this port in one Linux container and benchmarks
them alternately. The results and how to run it are in [compare/README.md](../compare/README.md).

## The safe migration

Here "unsafe" is the last raw-pointer build (commit `1b179b5`) and "safe" is the
`#![forbid(unsafe_code)]` build. Numbers are from alternating runs in the same session.

| scenario    | unsafe      | safe, first cut | safe, final |
|-------------|-------------|-----------------|-------------|
| health      | 253–263k    | 223k (−15%)     | 248k (−2%)  |
| query_point | 167–181k    | 165k            | 169k (−3%, noise) |
| query_1000  | 9.9–12.5k   | 9.2k (−7%)      | 10.5k (−4…−7%, p50 noisy) |
| execute     | ~600        | ~590            | ~590        |

What closed the gap:

1. **Windows event loop.** `mio` on Windows is IOCP/AFD: edge-triggered, with each socket's
   interest re-armed only after a read returns `WouldBlock`. That meant one extra `recv`
   per request, plus a per-socket mutex and a re-arm. On Windows, a short read now calls
   `Registry::reregister` instead; mio re-submits the AFD poll inside the next `poll()`,
   so no extra `recv` is needed. `health` +10%. Linux keeps C's read-until-`EAGAIN`,
   because there `reregister` is itself an `epoll_ctl` syscall.
2. **JSON strings.** Clean 8-byte words are skipped with an exact SWAR test (a byte is
   `< 0x20`, `"` or `\`). The C `strlen` cut-off is folded into the same pass.
   Microbenchmark: 1000 benchmark rows 50 → 39 µs; 4 KiB clean strings 245 → 57 µs.
   The first SWAR version was *slower* on short strings: after a failed word it retried
   a word at every following byte. Fixed by byte-scanning to the escape first.
3. **One `Arc` per connection instead of two** (socket and cancel flag). That halves
   the refcount traffic between the network thread and the workers on every request.
4. **In-place `/query` body.** The JSON is written straight into the response buffer after
   64 bytes of reserved room; the header is then written right-aligned into that room.
   This removes one copy of every result (C-6) and the scratch buffer.
5. **A short-read ioctl remains on Windows.** It's the rest of the `health` gap and is
   inherent to AFD polling. It doesn't exist on Linux/epoll.

`rusqlite` adds one `sqlite3_column_count` call per cell: `Row::get_ref` bounds-checks
the index, and there's no safe way to skip that. It's a field read, so it doesn't
show in the benchmarks.

## Statement cache and busy handler

These come after the safe migration. Numbers are from 3 alternating runs.

| scenario    | before       | after                          |
|-------------|--------------|--------------------------------|
| health      | 250k         | 250k (unchanged)               |
| query_point | 166k         | 194k (+17%)                    |
| query_1000  | 9.8k         | 9.8k (unchanged; JSON-bound)   |
| execute     | ~590, p99 62 ms | **~101k, p99 0.6 ms** (≈170×) |

1. **Prepared-statement cache (C-7).** `prepare()` uses `Connection::prepare_cached`, which is
   rusqlite's per-connection LRU of 16 statements. Behaviour doesn't change: `Rows` resets the
   statement on drop, and the cache clears its bindings before reuse, so unbound parameters are
   still `NULL`. After DDL, sqlite re-prepares the statement on its own, and a statement it
   can't re-prepare fails with the same `sqlite3_errmsg`. The cache is keyed on
   `sql.trim()`. That trim uses Unicode whitespace (U+00A0, for example), which sqlite's
   tokenizer rejects, so any query whose text trimming would change skips the cache.
2. **Busy handler (C-8).** `sqlite3_busy_timeout`'s callback sleeps 1, 2, 5, 10… ms, and on
   Windows `Sleep(1)` becomes a whole 15.6 ms timer tick. A WAL write lock is held for
   microseconds, so writers spent almost all of their time asleep. `database::busy_wait` yields
   8 times, then sleeps 20 µs, doubling up to 1 ms, under the same 5 s budget (measured as
   wall-clock time instead of summed nominal sleeps). On timeout the error is still
   `database is locked` / 500, after 5.03 s (checked with an external `BEGIN IMMEDIATE`). All
   20,000 concurrent `n = n + 1` updates from 16 clients land.

## Rust-side optimizations so far

Machine: 16 threads, Windows 11. The server runs with `-t 4`. Numbers are averages of 3 alternating runs.

| scenario    | before        | after          |
|-------------|---------------|----------------|
| health      | 230k req/s    | 244k req/s (+6%) |
| query_point | 154k req/s    | 164k req/s (+7%) |
| query_1000  | 6.1k req/s, p99 3.7 ms | 10.7k req/s (+75%), p99 2.1 ms |
| execute     | ~610 req/s    | ~600 req/s (lock-bound, see C-8; since fixed) |

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
  Linux is more likely to hit it. *Port:* fixed in the safe version (`send_all` in `worker.rs`).
- **C-3: two servers can share a port.** Linux gets `SO_REUSEPORT` (option `15`), so a second riffdb
  on the same port starts "successfully" and the kernel load-balances connections between two
  processes with different databases. *Port:* kept on Linux as in C. On Windows `SO_REUSEADDR`
  is no longer set, because there it allows the same thing.
- **C-4: memory leaks.** The yyjson request document from `Prepare()` is leaked for every
  `/query` and `/execute`. The `yyjson_mut_write` output buffer is leaked for every `/query`.
  sqlite error strings are leaked, because the `XFree` sits after a `return`.
  *Port:* nothing leaks (owned buffers).
- Also listed in [PORTING.md](PORTING.md#deliberate-deviations-from-c): heap overflow on responses over about 8 KB, body overflow on pipelined
  bytes, re-applied `BodyStart`, dispatch before the body is complete, more than 24 headers, the
  use-after-free on disconnect.

Performance:

- **C-5: about 194 KiB allocated per connection up front.** `HttpParserInit` allocates 24 × 8 KiB
  header-value buffers plus a 2 KiB body, which is about 200 MB at the 1024-client limit.
  *Port:* header values grow on demand; a connection starts at about 4 KiB.
- **C-6: JSON is built as a tree and then serialized.** Every value in the `yyjson_mut_doc` is an
  allocation, the column name is re-added on every row, and the result is copied again into the
  response. Streaming rows straight into the response buffer would avoid all of that.
  *Port:* done. Rows are streamed into the response buffer, and keys are escaped once per statement.
- **C-7: no prepared-statement cache.** `sqlite3_prepare_v3` runs on every request.
  *Port:* fixed; see "Statement cache and busy handler".
- **C-8: writers contend across connections.** Each worker has its own connection, so concurrent
  writes fight over the SQLite write lock. The busy handler sleeps in whole milliseconds, so
  `execute` peaks at about 600 req/s with p99 about 61 ms. A single writer (or routing writes to one
  worker) would remove the contention. *Port:* the waits are fixed (about 100k req/s); see
  "Statement cache and busy handler".
- **C-9: round-robin dispatch ignores load.** A slow query blocks everything queued behind it on
  that worker while other workers sit idle.
- **C-10: `poll()` scans every fd on each wakeup.** With 1024 connections that is O(n) per wakeup;
  epoll, kqueue or IOCP would scale better. *Port:* `mio` uses epoll, kqueue or IOCP.
- **C-11: one extra `read()` per request to get `EAGAIN`.** *Port:* kept on Linux, where edge-triggered
  epoll requires it. Avoided on Windows via `reregister` (see "The safe migration").
- **C-12: condvar signalled while holding the mutex.** *Port:* avoided; see (6) above.
- **C-13: byte-at-a-time parser.** It runs a `switch` per byte and `strncmp`s every header twice for
  `Content-Length`. Scanning for `\r\n` and `:` with `memchr` would be much faster.
- **C-14: about 20 `LogFlog` calls per request in the worker, even in release.** Each returns early,
  but it is still about 20 calls plus argument setup. *Port:* the trace dump is behind one level check.
