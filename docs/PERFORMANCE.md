# Performance notes

## How to measure

```sh
# terminal 1: a release server with 4 workers
cargo run --release -- -p 19990 -t 4 -d ./benchdb

# terminal 2: keep-alive load (8 connections, 4 s per scenario)
cargo run --release --example bench -- 19990 8 4
```

Scenarios: `health` (no SQLite), `query_point` (1 row by primary key), `query_50`
(about 4 KB of JSON), `query_1000` (1000 rows, 93 KB of JSON), and `execute` (a
single-row `UPDATE`). An optional fourth argument filters them, e.g.
`health,execute`.

The client and server share one machine, so small-request numbers are noisy by
±10%. To compare two builds, run them alternately a few times rather than once each.

### Reading the assembly

```sh
cargo rustc --release --bin riffdb -- --emit asm -C "llvm-args=-x86-asm-syntax=intel"
# network thread (the parser and read handling are inlined into it)
python tools/asmfn.py target/release/deps/riffdb.s 'TcpServer$LT$D$GT$3run' --stats
# worker thread (router, service and protocol are inlined into std's thread entry)
python tools/asmfn.py target/release/deps/riffdb.s __rust_begin_short_backtrace --stats
```

The output is post-LTO, so most helpers are inlined into their callers. `--stats`
lists every call a function makes. On a hot path, look for heap calls
(`HeapAlloc`/`HeapFree`, `malloc`), `memset`, `core::fmt::write` and
`from_utf8_lossy`.

Code that is hard to see in a whole-server benchmark can be timed on its own with an
ignored unit test. The JSON writer, for example:
`cargo test --release --bin riffdb json_micro -- --ignored --nocapture`.

## Where the time goes, and what was done about it

### SQLite

- **Statement cache.** `prepare()` uses rusqlite's `prepare_cached`, a
  per-connection LRU of 16 statements. `Rows` resets the statement on drop and the
  cache clears bindings before reuse, so unbound parameters are still `NULL`. After
  DDL, SQLite re-prepares the statement by itself. The cache is keyed on
  `sql.trim()`, which strips Unicode whitespace (U+00A0, for example) that SQLite's
  tokenizer rejects, so any query that trimming would change bypasses the cache.
  `query_point`: +17%.
- **Busy handler.** `sqlite3_busy_timeout` sleeps 1, 2, 5, 10… ms between retries,
  and on Windows `Sleep(1)` is a whole 15.6 ms timer tick. A WAL write lock is held
  for microseconds, so concurrent writers spent almost all of their time asleep.
  `database::busy_wait` yields 8 times, then sleeps from 20 µs up to 1 ms, within
  the same 5 s budget (measured as wall-clock time). On timeout the error is still
  `database is locked` after about 5 s. All 20,000 concurrent `n = n + 1` updates
  from 16 clients land. Contended `execute`: from about 600 req/s (p99 62 ms) to
  about 100k req/s (p99 0.6 ms) on Windows.
- **Contention remains.** Each worker has its own connection, so writers still
  compete for SQLite's single write lock. Sending all writes to one worker would
  remove that, at the cost of write parallelism in the network layer.

### JSON output

- Rows are streamed straight into the response buffer; no JSON tree is built. The
  body starts after 64 bytes of reserved space, and the status line and
  `Content-Length` are written right-aligned into that space once the length is
  known, so the result is never copied.
- Column names are escaped once per statement, not once per cell.
- Integers are formatted on the stack, not through `core::fmt`.
- Strings are scanned 8 bytes at a time with an exact SWAR test (a byte is `< 0x20`,
  `"` or `\`), and clean runs are copied in bulk. The NUL cut-off is folded into the
  same pass. Microbenchmark: 1000 benchmark rows went from 50 to 39 µs, and 4 KiB
  clean strings from 245 to 57 µs. The first SWAR version was *slower* on short
  strings, because after a failed word it retried a word at every following byte;
  scanning byte-wise to the escape first fixed that.
- `query_1000`: 6.1k → 10.7k req/s, p99 3.7 → 2.1 ms.

### Network and dispatch

- **One `recv` fewer per request on Windows.** `mio` on Windows is IOCP/AFD:
  edge-triggered, and a socket's interest is re-armed only after a read returns
  `WouldBlock`. On a short read the server now calls `Registry::reregister` instead,
  and mio re-submits the AFD poll inside the next `poll()`. `health`: +10%. Linux
  keeps reading until `EAGAIN`, because there `reregister` is itself an `epoll_ctl`
  syscall.
- **One `Arc` per connection** (socket and cancel flag together) halves the
  refcount traffic between the network thread and the workers.
- **The condvar is notified after unlocking**, so the woken worker doesn't
  immediately block on the mutex.
- **No 8 KiB `memset` per read.** The read buffer is allocated once and reused.
- **Header values grow on demand**, so a new connection costs about 4 KiB instead of
  a fixed allocation for every possible header.

### Logging

- The log macros check the level before building their arguments. Trace logging
  used to format the parser state, the body and the whole response into temporary
  `String`s about 20 times per request, only to drop them. The parser dump is now a
  separate `#[cold]` function.
- `log_flog` itself is `#[cold]` and never inlined, so each log call site costs a
  level check and a call.

### Known leftovers

- rusqlite's `Row::get_ref` bounds-checks the column index with one extra
  `sqlite3_column_count` call per cell. It's a field read and doesn't show in the
  benchmarks.
- The parser works byte by byte, with a few bounds checks in its inner loop.
  Scanning for `\r\n` and `:` with `memchr` would be faster.
- Round-robin dispatch ignores load: a slow query holds up everything queued behind
  it on that worker while others sit idle.
- On Windows, a short-read ioctl per request is inherent to AFD polling.

## Against the C implementation

[compare/](../compare/README.md) builds the original C server and this one in the
same Linux container and benchmarks them alternately. In short, networking is at
parity, queries are 13–24% faster and writes 2.8–3.5× faster. The C server can't
return results over about 8 KB, and with more than one worker it corrupts
keep-alive responses.
