// Port of packages/riffdb.js/mod_test.ts
//
// Differences from the Deno suite:
// - Every test gets its own server + database (Deno ran them sequentially
//   against one shared server), so the `withDb` DROP-everything cleanup is
//   unnecessary and tests can run in parallel.
// - Tagged templates become `?` placeholders + a JSON args array, which is
//   exactly what the JS client sends over the wire.
// - Tests commented out in the original (AFTER INSERT trigger, transaction
//   commit/rollback) stay commented out.

// 3.14159 is the literal value from the original JS test, not a stand-in for PI.
#![allow(clippy::approx_constant)]

mod common;

use common::{int, num, str_, Server};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 1. Basic DDL / DML
// ---------------------------------------------------------------------------

#[test]
fn create_table_insert_select_drop() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    let id = 42;
    let name = "alice";

    let rows = sql
        .sql("INSERT INTO users (id, name) VALUES (?, ?) RETURNING id, name", json!([id, name]))
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(int(&rows[0], "id"), id);
    assert_eq!(str_(&rows[0], "name"), name);

    let selected = sql.sql("SELECT id, name FROM users WHERE id = ?", json!([id])).unwrap();
    assert_eq!(str_(&selected[0], "name"), "alice");

    sql.x("DROP TABLE users");
}

#[test]
fn insert_or_replace_upsert() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
    sql.x("INSERT INTO users (id, name) VALUES (1, 'first')");

    let rows = sql.q("INSERT OR REPLACE INTO users (id, name) VALUES (1, 'second') RETURNING id, name");
    assert_eq!(str_(&rows[0], "name"), "second");

    sql.x("DROP TABLE users");
}

#[test]
fn update_delete_row_count() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)");
    sql.x("INSERT INTO users (id, name, age) VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)");

    sql.x("UPDATE users SET age = age + 1 WHERE id = 2");

    let updated = sql.q("SELECT age FROM users WHERE id = 2");
    assert_eq!(int(&updated[0], "age"), 21);

    sql.x("DELETE FROM users WHERE age < 25");

    let remaining = sql.q("SELECT id FROM users ORDER BY id");
    assert_eq!(remaining.iter().map(|r| int(r, "id")).collect::<Vec<_>>(), vec![3]);

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 2. Constraints
// ---------------------------------------------------------------------------

#[test]
fn primary_key_uniqueness_violation() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
    sql.x("INSERT INTO users (id, name) VALUES (1, 'a')");

    assert!(sql.exec("INSERT INTO users (id, name) VALUES (1, 'b')", json!([])).is_err());

    sql.x("DROP TABLE users");
}

#[test]
fn not_null_violation() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    assert!(sql.exec("INSERT INTO users (id, name) VALUES (1, NULL)", json!([])).is_err());

    sql.x("DROP TABLE users");
}

#[test]
fn unique_constraint() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE)");
    sql.x("INSERT INTO users (id, email) VALUES (1, 'a@x.com')");

    assert!(sql.exec("INSERT INTO users (id, email) VALUES (2, 'a@x.com')", json!([])).is_err());

    sql.x("DROP TABLE users");
}

#[test]
fn check_constraint() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE products (id INTEGER PRIMARY KEY, price REAL CHECK (price > 0))");

    assert!(sql.exec("INSERT INTO products (id, price) VALUES (1, -5)", json!([])).is_err());

    sql.x("INSERT INTO products (id, price) VALUES (1, 9.99)");
    sql.x("DROP TABLE products");
}

#[test]
fn foreign_key_constraint() {
    // PRAGMA foreign_keys is per connection: needs the single-worker server.
    let s = Server::start();
    let sql = s.db();

    sql.x("PRAGMA foreign_keys = ON");

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
    sql.x(
        "CREATE TABLE posts (
            id      INTEGER PRIMARY KEY,
            user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
            title   TEXT
        )",
    );

    sql.x("INSERT INTO users (id, name) VALUES (1, 'alice')");
    sql.x("INSERT INTO posts (id, user_id, title) VALUES (10, 1, 'hello')");

    assert!(sql
        .exec("INSERT INTO posts (id, user_id, title) VALUES (11, 999, 'orphan')", json!([]))
        .is_err());

    sql.x("DELETE FROM users WHERE id = 1");
    let posts = sql.q("SELECT id FROM posts");
    assert_eq!(posts.len(), 0);

    sql.x("DROP TABLE posts");
    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 3. Indexes & EXPLAIN
// ---------------------------------------------------------------------------

#[test]
fn create_index_and_query_plan() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE logs (id INTEGER PRIMARY KEY, ts INTEGER, msg TEXT)");
    sql.x("CREATE INDEX idx_logs_ts ON logs(ts)");

    for i in 0..100 {
        sql.exec(
            "INSERT INTO logs (id, ts, msg) VALUES (?, ?, ?)",
            json!([i, 1000 + i, format!("msg{i}")]),
        )
        .unwrap();
    }

    let plan = sql.q("EXPLAIN QUERY PLAN SELECT * FROM logs WHERE ts = 1050");
    // Just ensure it returns something; exact plan text can vary
    assert!(!plan.is_empty());

    sql.x("DROP TABLE logs");
}

// ---------------------------------------------------------------------------
// 4. Joins, aggregates, GROUP BY, HAVING
// ---------------------------------------------------------------------------

#[test]
fn joins_aggregates() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
    sql.x("CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER, title TEXT)");

    sql.x("INSERT INTO users VALUES (1, 'alice'), (2, 'bob')");
    sql.x("INSERT INTO posts VALUES (10, 1, 'post1'), (11, 1, 'post2'), (12, 2, 'post3')");

    let rows = sql.q(
        "SELECT u.name, COUNT(p.id) AS cnt
         FROM users u
         LEFT JOIN posts p ON p.user_id = u.id
         GROUP BY u.id
         HAVING cnt >= 1
         ORDER BY cnt DESC",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(str_(&rows[0], "name"), "alice");
    assert_eq!(int(&rows[0], "cnt"), 2);
    assert_eq!(str_(&rows[1], "name"), "bob");
    assert_eq!(int(&rows[1], "cnt"), 1);

    sql.x("DROP TABLE posts");
    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 5. CTEs (WITH)
// ---------------------------------------------------------------------------

#[test]
fn common_table_expression() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE numbers (n INTEGER)");
    sql.x("INSERT INTO numbers VALUES (1),(2),(3),(4),(5)");

    let rows = sql.q(
        "WITH doubled AS (
            SELECT n, n * 2 AS doubled FROM numbers WHERE n > 2
         )
         SELECT * FROM doubled ORDER BY n",
    );

    assert_eq!(rows.iter().map(|r| int(r, "doubled")).collect::<Vec<_>>(), vec![6, 8, 10]);

    sql.x("DROP TABLE numbers");
}

// ---------------------------------------------------------------------------
// 6. Window functions
// ---------------------------------------------------------------------------

#[test]
fn window_functions() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE sales (id INTEGER PRIMARY KEY, region TEXT, amount REAL)");
    sql.x("INSERT INTO sales VALUES (1, 'east', 100), (2, 'east', 200), (3, 'west', 150), (4, 'west', 50)");

    let rows = sql.q(
        "SELECT
            region,
            amount,
            RANK() OVER (PARTITION BY region ORDER BY amount DESC) AS rank,
            SUM(amount) OVER (PARTITION BY region) AS total
         FROM sales
         ORDER BY region, rank",
    );

    assert_eq!(str_(&rows[0], "region"), "east");
    assert_eq!(int(&rows[0], "rank"), 1);
    assert_eq!(num(&rows[0], "amount"), 200.0);
    assert_eq!(num(&rows[0], "total"), 300.0);

    sql.x("DROP TABLE sales");
}

// ---------------------------------------------------------------------------
// 7. JSON support
// ---------------------------------------------------------------------------

#[test]
fn json_functions() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE json_data (id INTEGER PRIMARY KEY, data TEXT)");

    let payload = json!({ "name": "widget", "tags": ["a", "b"], "meta": { "v": 1 } }).to_string();
    sql.exec("INSERT INTO json_data (id, data) VALUES (1, ?)", json!([payload])).unwrap();

    let rows = sql.q(
        "SELECT
            json_extract(data, '$.name') AS name,
            json_extract(data, '$.tags[0]') AS tag0,
            json_extract(data, '$.meta.v') AS v
         FROM json_data",
    );

    assert_eq!(str_(&rows[0], "name"), "widget");
    assert_eq!(str_(&rows[0], "tag0"), "a");
    assert_eq!(int(&rows[0], "v"), 1);

    sql.x("DROP TABLE json_data");
}

// ---------------------------------------------------------------------------
// 8. Full-text search (FTS5)
// ---------------------------------------------------------------------------

#[test]
fn fts5_full_text_search() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE VIRTUAL TABLE fts_docs USING fts5(title, body)");

    sql.x(
        "INSERT INTO fts_docs (title, body) VALUES
            ('Hello World', 'This is a test document about SQLite'),
            ('Another Doc', 'Full text search is powerful'),
            ('Third', 'Nothing relevant here')",
    );

    let rows = sql.q("SELECT title FROM fts_docs WHERE fts_docs MATCH 'SQLite OR powerful' ORDER BY rank");

    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| str_(r, "title") == "Hello World"));
    assert!(rows.iter().any(|r| str_(r, "title") == "Another Doc"));

    sql.x("DROP TABLE fts_docs");
}

// ---------------------------------------------------------------------------
// 9. Triggers — commented out in the original suite.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 10. Views
// ---------------------------------------------------------------------------

#[test]
fn create_and_query_view() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, active INTEGER)");
    sql.x("INSERT INTO users VALUES (1, 'alice', 1), (2, 'bob', 0), (3, 'carol', 1)");

    sql.x("CREATE VIEW active_users AS SELECT id, name FROM users WHERE active = 1");

    let rows = sql.q("SELECT * FROM active_users ORDER BY id");
    assert_eq!(rows.iter().map(|r| str_(r, "name")).collect::<Vec<_>>(), vec!["alice", "carol"]);

    sql.x("DROP VIEW active_users");
    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 11. Transactions — commented out in the original suite.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 12. NULL handling & COALESCE
// ---------------------------------------------------------------------------

#[test]
fn null_handling() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)");
    sql.x("INSERT INTO users (id, name, age) VALUES (1, 'alice', NULL)");

    let rows = sql.q("SELECT age, COALESCE(age, 0) AS coalesced FROM users");
    assert_eq!(rows[0]["age"], Value::Null);
    assert_eq!(int(&rows[0], "coalesced"), 0);

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 13. BLOB / binary data
// ---------------------------------------------------------------------------

#[test]
#[ignore = "fails against the C server too: JSON.stringify(Uint8Array) sends an object, \
            which riffdb skips when binding, and BLOB columns are returned as null"]
fn blob_round_trip() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB)");

    let bytes: [u8; 5] = [0x00, 0x01, 0x02, 0xff, 0xfe];
    // What JSON.stringify(new Uint8Array([...])) actually puts on the wire.
    let as_js_sends: Value = bytes
        .iter()
        .enumerate()
        .map(|(i, b)| (i.to_string(), json!(b)))
        .collect::<serde_json::Map<_, _>>()
        .into();
    sql.exec("INSERT INTO blobs (id, data) VALUES (1, ?)", json!([as_js_sends])).unwrap();

    let rows = sql.q("SELECT data FROM blobs WHERE id = 1");
    assert!(!rows[0]["data"].is_null());
    let got: Vec<u8> = rows[0]["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect();
    assert_eq!(got, bytes);

    sql.x("DROP TABLE blobs");
}

// ---------------------------------------------------------------------------
// 14. Date / time functions
// ---------------------------------------------------------------------------

#[test]
fn date_and_time_functions() {
    let s = Server::start();
    let sql = s.db();

    let rows = sql.q("SELECT datetime('now') AS now, date('now') AS date, unixepoch('now') AS unix");

    assert!(!rows[0]["now"].is_null());
    assert!(!rows[0]["date"].is_null());
    assert!(rows[0]["unix"].is_number());
}

// ---------------------------------------------------------------------------
// 15. Parameter edge cases
// ---------------------------------------------------------------------------

#[test]
fn many_parameters_different_types() {
    let s = Server::start();
    let sql = s.db();

    sql.x(
        "CREATE TABLE events (
            id     INTEGER PRIMARY KEY,
            name   TEXT,
            score  REAL,
            active INTEGER,
            note   TEXT
        )",
    );

    let id = 7;
    let name = "test event";
    let score = 3.14159;
    let active = 1;
    let note = Value::Null;

    let rows = sql
        .sql(
            "INSERT INTO events (id, name, score, active, note) VALUES (?, ?, ?, ?, ?) RETURNING *",
            json!([id, name, score, active, note]),
        )
        .unwrap();

    assert_eq!(int(&rows[0], "id"), 7);
    assert_eq!(str_(&rows[0], "name"), "test event");
    assert_eq!(num(&rows[0], "score"), 3.14159);
    assert_eq!(int(&rows[0], "active"), 1);
    assert_eq!(rows[0]["note"], Value::Null);

    sql.x("DROP TABLE events");
}

// ---------------------------------------------------------------------------
// 16. Empty result sets
// ---------------------------------------------------------------------------

#[test]
fn empty_result_set() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
    let rows = sql.q("SELECT * FROM users WHERE id = 999");
    assert_eq!(rows.len(), 0);
    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 17. Recursive CTE (tree / hierarchy)
// ---------------------------------------------------------------------------

#[test]
fn recursive_cte() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE trees (id INTEGER PRIMARY KEY, parent_id INTEGER, name TEXT)");
    sql.x("INSERT INTO trees VALUES (1, NULL, 'root'), (2, 1, 'child1'), (3, 1, 'child2'), (4, 2, 'grandchild')");

    let rows = sql.q(
        "WITH RECURSIVE walk(id, name, depth) AS (
            SELECT id, name, 0 FROM trees WHERE parent_id IS NULL
            UNION ALL
            SELECT t.id, t.name, w.depth + 1
            FROM trees t
            JOIN walk w ON t.parent_id = w.id
         )
         SELECT * FROM walk ORDER BY depth, id",
    );

    assert_eq!(
        rows.iter().map(|r| str_(r, "name")).collect::<Vec<_>>(),
        vec!["root", "child1", "child2", "grandchild"]
    );
    assert_eq!(int(&rows[3], "depth"), 2);

    sql.x("DROP TABLE trees");
}

// ---------------------------------------------------------------------------
// 18. Parallel reads
// ---------------------------------------------------------------------------

#[test]
fn parallel_reads() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    // Seed
    for i in 1..=50 {
        sql.exec("INSERT INTO users (id, name) VALUES (?, ?)", json!([i, format!("user{i}")])).unwrap();
    }

    // Fire many concurrent SELECTs
    let results: Vec<Vec<Value>> = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..20)
            .map(|i| sc.spawn(move || sql.sql("SELECT id, name FROM users WHERE id = ?", json!([i + 1]))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap().unwrap()).collect()
    });

    for (i, rows) in results.iter().enumerate() {
        assert_eq!(rows.len(), 1);
        assert_eq!(int(&rows[0], "id"), i as i64 + 1);
        assert_eq!(str_(&rows[0], "name"), format!("user{}", i + 1));
    }

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 19. Parallel inserts
// ---------------------------------------------------------------------------

#[test]
fn parallel_inserts() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    const N: i64 = 30;
    std::thread::scope(|sc| {
        let handles: Vec<_> = (0..N)
            .map(|i| {
                sc.spawn(move || {
                    sql.exec("INSERT INTO users (id, name) VALUES (?, ?)", json!([i + 1, format!("user{}", i + 1)]))
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
    });

    let count = sql.q("SELECT COUNT(*) AS cnt FROM users");
    assert_eq!(int(&count[0], "cnt"), N);

    let names = sql.q("SELECT name FROM users ORDER BY id");
    assert_eq!(
        names.iter().map(|r| str_(r, "name").to_string()).collect::<Vec<_>>(),
        (0..N).map(|i| format!("user{}", i + 1)).collect::<Vec<_>>()
    );

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 20. Parallel mixed read + insert
// ---------------------------------------------------------------------------

#[test]
fn parallel_read_and_insert() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    // Initial seed
    sql.x("INSERT INTO users (id, name) VALUES (0, 'seed')");

    // Run everything concurrently
    let read_results: Vec<Vec<Value>> = std::thread::scope(|sc| {
        let readers: Vec<_> = (0..15)
            .map(|_| sc.spawn(move || sql.sql("SELECT COUNT(*) AS cnt FROM users", json!([]))))
            .collect();
        let writers: Vec<_> = (0..15)
            .map(|i| {
                sc.spawn(move || {
                    sql.exec(
                        "INSERT INTO users (id, name) VALUES (?, ?)",
                        json!([i + 1, format!("parallel-{}", i + 1)]),
                    )
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
        readers.into_iter().map(|h| h.join().unwrap().unwrap()).collect()
    });

    // All readers should have succeeded (count may vary depending on interleaving)
    for rows in &read_results {
        let cnt = int(&rows[0], "cnt");
        assert!(cnt >= 1);
        assert!(cnt <= 16);
    }

    // Final state must contain all inserts
    let fin = sql.q("SELECT COUNT(*) AS cnt FROM users");
    assert_eq!(int(&fin[0], "cnt"), 16); // 1 seed + 15 inserts

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 21. Concurrent writers with conflict (last-write-wins / error)
// ---------------------------------------------------------------------------

#[test]
fn concurrent_updates_on_same_row() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE counters (id INTEGER PRIMARY KEY, val INTEGER NOT NULL)");
    sql.x("INSERT INTO counters (id, val) VALUES (1, 0)");

    // Many concurrent increments – we just check final value is consistent
    // (between 1 and N) and no crash. SQLITE_BUSY failures are tolerated.
    const N: i64 = 20;
    let fulfilled = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..N)
            .map(|_| sc.spawn(move || sql.exec("UPDATE counters SET val = val + 1 WHERE id = 1", json!([]))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).filter(Result::is_ok).count()
    });
    assert!(fulfilled >= 1);

    let fin = sql.q("SELECT val FROM counters WHERE id = 1");
    let val = int(&fin[0], "val");
    assert!((1..=N).contains(&val));

    sql.x("DROP TABLE counters");
}

// ---------------------------------------------------------------------------
// 22. Large batch insert
// ---------------------------------------------------------------------------

#[test]
#[ignore = "fails against the C server too: the JS test interpolates the VALUES list \
            as a bound parameter (`VALUES ?`), which is a SQL syntax error"]
fn large_batch_insert() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    const BATCH: i64 = 500;
    let values = (0..BATCH)
        .map(|i| format!("({}, 'user{}')", i + 1, i + 1))
        .collect::<Vec<_>>()
        .join(",");

    // Faithful to `sql.exec\`INSERT INTO users (id, name) VALUES ${values}\``
    sql.exec("INSERT INTO users (id, name) VALUES ?", json!([values])).unwrap();

    let count = sql.q("SELECT COUNT(*) AS cnt FROM users");
    assert_eq!(int(&count[0], "cnt"), BATCH);

    sql.x("DROP TABLE users");
}

/// Not in the original suite: what test 22 presumably meant to check, with the
/// VALUES list inlined into the SQL text instead of bound.
#[test]
fn large_batch_insert_inlined() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    const BATCH: i64 = 500;
    let values = (0..BATCH)
        .map(|i| format!("({}, 'user{}')", i + 1, i + 1))
        .collect::<Vec<_>>()
        .join(",");

    sql.x(&format!("INSERT INTO users (id, name) VALUES {values}"));

    let count = sql.q("SELECT COUNT(*) AS cnt FROM users");
    assert_eq!(int(&count[0], "cnt"), BATCH);

    sql.x("DROP TABLE users");
}

// ---------------------------------------------------------------------------
// 23. PRAGMA settings
// ---------------------------------------------------------------------------

#[test]
fn pragma_read_write() {
    // Per-connection PRAGMA: needs the single-worker server.
    let s = Server::start();
    let sql = s.db();

    sql.x("PRAGMA foreign_keys = ON");
    let fk = sql.q("PRAGMA foreign_keys");
    assert_eq!(int(&fk[0], "foreign_keys"), 1);

    sql.x("PRAGMA journal_mode = WAL");
    // journal_mode returns a string
    let jm = sql.q("PRAGMA journal_mode");
    assert!(!jm.is_empty());
}

// ---------------------------------------------------------------------------
// 24. Error: syntax error
// ---------------------------------------------------------------------------

#[test]
fn syntax_error_is_rejected() {
    let s = Server::start();
    let sql = s.db();

    assert!(sql.exec("SELCT * FROM nowhere", json!([])).is_err());
}

// ---------------------------------------------------------------------------
// 25. Returning clause with multiple rows
// ---------------------------------------------------------------------------

#[test]
fn returning_with_multi_row_insert() {
    let s = Server::start();
    let sql = s.db();

    sql.x("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

    let rows = sql.q("INSERT INTO users (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c') RETURNING id, name");

    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().map(|r| str_(r, "name")).collect::<Vec<_>>(), vec!["a", "b", "c"]);

    sql.x("DROP TABLE users");
}
