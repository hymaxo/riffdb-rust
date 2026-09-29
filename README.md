# riffdb-rust

**SQLite over HTTP, in safe Rust.** riffdb is a small, fast database server: it keeps a
single SQLite database on disk and lets any client that speaks HTTP and JSON run SQL
against it. There's no driver to install and no binary protocol to implement. `curl`
is enough.

```sh
cargo run --release -- -p 9889 -d ./data

curl -X POST localhost:9889/execute -d '{"q":"CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"}'
# ok
curl -X POST localhost:9889/execute -d '{"q":"INSERT INTO users (name) VALUES (?)","args":["ada"]}'
# ok
curl -X POST localhost:9889/query   -d '{"q":"SELECT * FROM users WHERE id = ?","args":[1]}'
# [{"id":1,"name":"ada"}]
```

## What problem it solves

SQLite is a great database, but it's a library: only the process that links it can use
it. As soon as a second service, a script in another language, or a runtime without
native bindings (edge functions, Deno, a browser tool) needs the data, you have two
options. You can move to a client/server database, or you can put something in front
of SQLite.

riffdb is that something, kept deliberately small:

- **One file, one process.** The data is a plain `riff.db` SQLite file, which you can
  open, back up or inspect with any SQLite tool.
- **Zero client dependencies.** Any HTTP client works. Queries are parameterized
  (`args`), so there's no string-building SQL on the client side.
- **Fast by design.** One network thread multiplexes every connection over
  epoll, kqueue or IOCP. A pool of workers, each with its own SQLite connection in WAL
  mode, runs the queries. Results are streamed straight into the response buffer as
  JSON.
- **Safe.** The crate is `#![forbid(unsafe_code)]`. The only unsafe code is inside
  SQLite itself and the standard, widely used crates underneath.

It is not a replacement for a networked RDBMS. There's no auth or TLS, and it's meant
to sit on localhost or inside a private network, next to the services that use it.

## Based on

This is a port of **[ssleert/riffdb](https://github.com/ssleert/riffdb)**, written in C.
The module layout follows the C sources one to one, and the wire protocol, error
messages and even the quirks are the same, so existing clients such as the original
[`riffdb.js`](https://github.com/ssleert/riffdb/tree/master/packages/riffdb.js) work
unchanged. The original's JS test suite is ported and runs against this server.

It went through three stages: a direct raw-pointer port, then an incremental migration
to safe Rust, then assembly-guided optimization. Along the way it fixes the C version's
memory-safety bugs: a heap overflow on responses over 8 KB, a keep-alive race with more
than one worker, a use-after-free on disconnect, truncated large responses, and leaks.
See [docs/PORTING.md](docs/PORTING.md) for the full list.

## Usage

```
riffdb [OPTIONS]
  -p, --port PORT        Listen port (default: 9889)
  -d, --directory DIR    Where riff.db lives (default: current directory)
  -t, --threads N        Worker threads (default: logical CPU count)
  -h, --help / -v, --version
```

The database is created on first start (`<DIR>/riff.db`, WAL mode).

### API

| endpoint | body | success |
|---|---|---|
| `POST /query` | `{"q": "<sql>", "args": [...]}` | `200`, a JSON array of row objects |
| `POST /execute` | `{"q": "<sql>", "args": [...]}` | `200`, `ok` |
| `GET /health` | — | `200`, `health` |

- `args` is optional. Its values (numbers, strings, booleans, `null`) bind to the `?`
  placeholders in order.
- Only the first statement in `q` runs.
- Errors return `500` with the SQLite error message as a plain-text body, for example
  `no such table: users`. Unknown routes return `404`.

## Benchmarks

These compare the C original with this port. Both were built and run in the same Linux
container (Docker Desktop, 16 threads). C used its own release flags
(`-Ofast -march=native`, LTO, mimalloc); Rust used a plain `cargo build --release`. The
client used keep-alive connections, one request in flight per connection. Full method,
raw output and a one-command rerun are in [compare/](compare/README.md).

4 workers, 32 connections:

| scenario | C (req/s) | Rust (req/s) | Rust vs C |
|---|---:|---:|---:|
| `/health` (no SQLite) | 155,553 ⚠ | 154,150 | on par |
| point query (1 row) | 90,343 ⚠ | 102,104 | **1.13×** |
| 50-row query (~4 KB JSON) | 59,394 ⚠ | 71,441 | **1.20×** |
| 1000-row query (93 KB JSON) | crashes | 11,234 | — |
| single-row `UPDATE` | 25,427 | 70,512 | **2.77×** |

⚠ = C sent corrupted responses during the run. Rust had zero errors in every run.

What the numbers show:

- **Networking is on par.** A mio event loop matches the hand-written C `poll()` loop.
- **Queries are 13–24% faster** (13–20% here; up to 24% in the 1-worker runs in
  [compare/](compare/README.md)). The gain comes from a per-worker prepared-statement
  cache and from streaming rows straight into the response instead of building a JSON
  tree and copying it.
- **Writes are 2.8–3.5× faster.** When a write waits for SQLite's lock, it retries on a
  microsecond scale instead of sleeping 1–100 ms.
- **The C original can't return large results**: any response over 8 KB overflows its
  buffer and kills the process. With more than one worker it also corrupts keep-alive
  responses.

## Development

```sh
cargo test --release        # unit tests, the ported riffdb.js suite, and wire tests
cargo run --release --example bench -- 9889 8 5   # load a running server
```

- [docs/PORTING.md](docs/PORTING.md): architecture, the C-to-Rust file mapping,
  library substitutions, every deliberate deviation, and the C quirks that were kept.
- [docs/MIGRATION.md](docs/MIGRATION.md): how the raw-pointer port became safe Rust.
- [docs/PERFORMANCE.md](docs/PERFORMANCE.md): how to benchmark and read the assembly,
  what was optimized, and the inefficiencies found in the C code.
- [compare/](compare/README.md): the benchmark against the C original.

Tests:

- **Unit tests**: parser, response bytes, JSON output, protocol, queue, channel, pool,
  router, options.
- **`tests/mod_test.rs`**: a port of the original `riffdb.js` test suite.
- **`tests/wire.rs`**: raw keep-alive sockets against 4 workers, checking response
  integrity.

All three pass on Windows and Linux. `RIFFDB_BIN=<exe>` points the integration suites
at another build.

## License

The original riffdb is licensed under GPL-3.0. This port is a derivative work of it.
