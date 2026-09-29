# riffdb

**SQLite over HTTP.** riffdb is a small, fast database server. It keeps one SQLite
database on disk and lets any client that can send HTTP and JSON run SQL against it.
You don't need a driver or a binary protocol; `curl` works.

```sh
cargo run --release -- -p 9889 -d ./data

curl -X POST localhost:9889/execute -d '{"q":"CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"}'
# ok
curl -X POST localhost:9889/execute -d '{"q":"INSERT INTO users (name) VALUES (?)","args":["ada"]}'
# ok
curl -X POST localhost:9889/query   -d '{"q":"SELECT * FROM users WHERE id = ?","args":[1]}'
# [{"id":1,"name":"ada"}]
```

## Why

SQLite is a great database, but it's a library, so only the process that links it
can use it. Once a second service, a script in another language, or a runtime
without native bindings (edge functions, Deno, a browser tool) needs the same data,
you either move to a client/server database or put something in front of SQLite.

riffdb is a small thing to put in front of it:

- **One file, one process.** The data lives in a plain `riff.db` SQLite file, which
  you can open, back up or inspect with any SQLite tool.
- **No client library needed.** Any HTTP client works. Queries take parameters
  (`args`), so clients don't build SQL strings.
- **Fast.** One network thread handles every connection with epoll, kqueue or IOCP.
  A pool of workers, each with its own SQLite connection in WAL mode, runs the
  queries, and rows are written straight into the response as JSON.
- **Memory-safe.** The crate is `#![forbid(unsafe_code)]`. The only unsafe code is
  in SQLite itself and in widely used crates underneath.

It doesn't replace a networked RDBMS. There is no auth and no TLS, so run it on
localhost or inside a private network, next to the services that use it.

## Usage

```
riffdb [OPTIONS]
  -p, --port PORT        Listen port (default: 9889)
  -d, --directory DIR    Where riff.db lives (default: current directory)
  -t, --threads N        Worker threads (default: logical CPU count)
  -h, --help / -v, --version
```

The database (`<DIR>/riff.db`, WAL mode) is created on first start. Set `NO_COLOR`
to turn off colored logs.

### API

| endpoint | body | success |
|---|---|---|
| `POST /query` | `{"q": "<sql>", "args": [...]}` | `200`, a JSON array of row objects |
| `POST /execute` | `{"q": "<sql>", "args": [...]}` | `200`, `ok` |
| `GET /health` | none | `200`, `health` |

- `args` is optional. Its values (numbers, strings, booleans, `null`) are bound to the
  `?` placeholders in order.
- Only the first statement in `q` runs.
- Errors return `500` with the SQLite error message as a plain-text body, for example
  `no such table: users`. Unknown routes return `404`.
- Each worker has its own connection, so per-connection settings such as
  `PRAGMA foreign_keys` only apply to requests that land on that worker. Use `-t 1`
  if you rely on them.

## Performance

With 4 workers and 32 keep-alive connections, riffdb serves about 100k point
queries/s and 70k single-row `UPDATE`s/s, with p99 latency around 1 ms. With 8
connections, a 1000-row result (93 KB of JSON) has a median latency of about 0.5 ms.
These numbers come from a Linux container on a 16-thread desktop, with the load
generator running on the same machine.

Most of the speed comes from three things:

- prepared statements are cached per worker;
- rows are written straight into the response buffer, with no intermediate JSON
  tree;
- a writer that hits SQLite's lock retries within microseconds instead of sleeping
  for whole milliseconds.

[docs/PERFORMANCE.md](docs/PERFORMANCE.md) covers how to measure and what was
optimized. [compare/](compare/README.md) has a reproducible benchmark against the
original C implementation.

## Development

```sh
cargo test --release                              # unit, client-suite and wire tests
cargo run --release --example bench -- 9889 8 5   # load a running server
```

- **Unit tests** cover the HTTP parser, response bytes, JSON output, payload parsing,
  the queue, channel and pool, routing and options.
- **`tests/mod_test.rs`** runs the riffdb.js client test suite against a live server.
- **`tests/wire.rs`** checks exact response bytes on raw keep-alive sockets against
  several workers.

Set `RIFFDB_BIN=<exe>` to point the integration tests at another build. Everything
passes on Linux and Windows.

More in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) (how a request flows through
the server, the modules, and protocol details) and
[docs/PERFORMANCE.md](docs/PERFORMANCE.md).

## Credits

riffdb was created by [ssleert](https://github.com/ssleert/riffdb) in C. This is a
Rust implementation of the same server. It keeps the wire protocol, so existing
clients such as
[`riffdb.js`](https://github.com/ssleert/riffdb/tree/master/packages/riffdb.js) work
unchanged.

## License

GPL-3.0, the same as the original riffdb.
