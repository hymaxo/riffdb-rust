# riffdb (C) vs riffdb-rust

This benchmarks the C original ([ssleert/riffdb](https://github.com/ssleert/riffdb) @ `cb5b374`)
against this port. Both are built and run in the same Linux container, so they share the
kernel, the CPU and the loopback.

```sh
docker build -f compare/Dockerfile -t riffdb-compare .
docker run --rm riffdb-compare                                   # defaults: t=1,4; 8 conns; 3 rounds x 3 s
docker run --rm -e THREADS=4 -e CONNS=32 -e SECS=4 riffdb-compare  # knobs: see run.sh
```

- **C** uses its own release config: `-Ofast -march=native -mtune=native`, LTO and
  mimalloc (mimalloc `31d034d`, wolfssl `09bc4fe`, as cloned by its `libs.sh`).
- **Rust** uses `cargo build --release` (LTO, 1 codegen unit), with no `target-cpu=native`
  and the system allocator.
- For every cell, each round starts a fresh server on an empty database, and C and Rust
  alternate. The client is `examples/bench.rs`: keep-alive connections, one request in flight
  per connection, and failures counted rather than fatal. A response with extra bytes after
  it ("trailing bytes") or a dropped connection counts as an error and is excluded from req/s.

Scenarios:

| name | request | response |
|---|---|---|
| health | `GET /health` | 6 B |
| query_point | `SELECT id, name, score FROM bench WHERE id = ?` | 1 row, 44 B |
| query_50 | `SELECT * FROM bench LIMIT 50` | 50 rows, ~4 KB (the largest C can return) |
| query_1000 | `SELECT * FROM bench` | 1000 rows, 93 KB |
| execute | `UPDATE bench SET score = score WHERE id = ?` | `ok` |

## Results

Docker Desktop (WSL2) on Windows 11, 16 threads. Throughput is the mean over rounds; p50
and p99 latencies (µs) are medians over rounds.

**8 connections, 3 rounds × 5 s** ([raw](results-raw.txt)):

| threads | scenario | C req/s | C p50/p99 | Rust req/s | Rust p50/p99 | Rust/C |
|---|---|---:|---:|---:|---:|---:|
| 1 | health      | 44,710 | 171/295 | 45,902 | 166/302 | 1.03× |
| 1 | query_point | 26,914 | 281/747 | 32,334 | 236/551 | **1.20×** |
| 1 | query_50    | 17,924 | 396/1315 | 22,302 | 344/591 | **1.24×** |
| 1 | query_1000  | crashed (3/3) | — | 3,350 | 2290/3099 | C dies |
| 1 | execute     | 26,188 | 290/702 | 30,700 | 248/612 | **1.17×** |
| 4 | health      | 55,927 ⚠ | 138/278 | 58,940 | 131/252 | 1.05× |
| 4 | query_point | 56,938 ⚠ | 132/394 | 60,685 | 126/282 | 1.07× |
| 4 | query_50    | 46,693 ⚠ | 137/991 | 57,508 | 131/360 | **1.23×** |
| 4 | query_1000  | crashed (3/3) | — | 11,234 | 457/2562 | C dies |
| 4 | execute     | 16,781 ⚠ | 142/2881 | 58,698 | 126/379 | **3.50×** |

**4 threads, 32 connections, 2 rounds × 4 s** ([raw](results-raw-32conns.txt)). With 8
connections the client caps at about 60k req/s, so this run is the fairer throughput
comparison:

| scenario | C req/s | C p50/p99 | Rust req/s | Rust p50/p99 | Rust/C |
|---|---:|---:|---:|---:|---:|
| health      | 155,553 ⚠ | 157/658 | 154,150 | 155/694 | 0.99× |
| query_point |  90,343 ⚠ | 260/1164 | 102,104 | 244/912 | **1.13×** |
| query_50    |  59,394 ⚠ | 376/1874 | 71,441 | 319/1428 | **1.20×** |
| execute     |  25,427 | 835/6459 | 70,512 | 355/1377 | **2.77×** |

⚠ = the C server returned corrupted responses during the run. Rust had **0 errors in
every run**.

## What the numbers say

- **Networking is at parity.** `health` doesn't touch SQLite, and at 155k req/s the two
  servers are within noise. mio's epoll loop costs the same as C's `poll()` loop, even
  though the Rust build doesn't use `-march=native` or mimalloc.
- **Queries: +13–24%.** Most of this is the prepared-statement cache (C-7; C calls
  `sqlite3_prepare_v3` on every request) and streaming rows straight into the response
  (C-6; C builds a yyjson tree and then copies it). `query_50` gains the most, because it
  serializes the most JSON.
- **Writes: 2.8–3.5×.** C uses sqlite's default busy handler, which sleeps 1, 2, 5… ms
  between retries (sqlite 3.53 delay table, `nanosleep` on Linux), while a WAL write lock is held for microseconds. The port's
  `busy_wait` yields, then sleeps 20 µs–1 ms under the same 5 s budget (C-8). p99 falls
  from 2.9–6.5 ms to 0.4–1.4 ms.
- **C isn't correct under load:**
  - *Crash on large results.* Any response over 8 KiB overflows `HttpResponse`'s buffer,
    which grows only once, from 4 to 8 KiB. `query_1000` killed the C server in all 6 runs.
  - *Corrupted keep-alive responses with `-t > 1` (C-1).* A worker `send()`s and then
    zeroes the connection's shared response buffer while another worker is already
    appending the next response. The client sees extra bytes after a response. This
    happened in every `-t 4` scenario, `health` included.

## Caveats

- The client runs in the same container as the server and competes with it for CPU, so
  absolute numbers are well below native. On native Windows, `health` does about 250k
  req/s with 8 connections; see [../docs/PERFORMANCE.md](../docs/PERFORMANCE.md).
  Compare ratios, not absolute numbers.
- Expect ±5–10% noise between runs. Ratios within about 1.05× mean parity.
- C's `-t 4` throughput includes the requests that succeeded around the corrupted ones.
  Each corrupted response also forces a reconnect, which costs C a little throughput.
