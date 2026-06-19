//! Local SQLite store for forum post history — a read-through cache that
//! doubles as a queryable corpus.
//!
//! Forum post history is effectively immutable (old posts rarely change), so we
//! cache it aggressively and only fetch the new tail on each run. The DB lives
//! at the platform data dir (override with `INDERES_FORUM_DB`); posts are keyed
//! by their stable Discourse `id` so upserts are idempotent.
//!
//! Beyond avoiding re-downloads, having posts in real tables (with
//! `username`/`created_at`/`post_number` columns) makes downstream analysis —
//! sentiment, per-user activity, posting-volume over time — a SQL query over
//! local data instead of a re-fetch.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};

/// Handle to the on-disk (or in-memory, for tests) cache.
#[derive(Debug)]
pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open the cache at the resolved platform path, creating it if needed.
    pub fn open() -> Result<Self> {
        let path = db_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        Self::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening forum cache at {}", path.display()))?;
        let cache = Self { conn };
        cache.migrate()?;
        Ok(cache)
    }

    /// Open the cache **read-only** for querying. Errors if it doesn't exist
    /// yet — there's nothing to analyze until a `forum topic` has populated it.
    /// Read-only means an arbitrary user/agent query can't mutate the cache.
    pub fn open_readonly() -> Result<Self> {
        Self::open_readonly_at(&db_path()?)
    }

    pub fn open_readonly_at(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!(
                "no forum cache at {} yet — run `inderes forum topic <id>` first",
                path.display()
            );
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening forum cache (read-only) at {}", path.display()))?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let cache = Self { conn };
        cache.migrate()?;
        Ok(cache)
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS topics (
                    id          INTEGER PRIMARY KEY,
                    title       TEXT,
                    posts_count INTEGER,
                    last_page   INTEGER NOT NULL DEFAULT 0,
                    synced_at   TEXT
                 );
                 CREATE TABLE IF NOT EXISTS posts (
                    id          INTEGER PRIMARY KEY,
                    topic_id    INTEGER NOT NULL,
                    post_number INTEGER,
                    username    TEXT,
                    created_at  TEXT,
                    updated_at  TEXT,
                    cooked      TEXT,
                    raw         TEXT,
                    fetched_at  TEXT
                 );
                 CREATE INDEX IF NOT EXISTS idx_posts_topic
                    ON posts(topic_id, post_number);",
            )
            .context("migrating forum cache schema")?;
        Ok(())
    }

    /// Highest page already fetched for a topic (0 if the topic is uncached).
    /// Used as the resume point for incremental fetching.
    pub fn last_page(&self, topic_id: i64) -> Result<u32> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_page FROM topics WHERE id = ?1",
                [topic_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0).max(0) as u32)
    }

    /// Record topic metadata and set the page watermark to the last page
    /// fetched. The walk calls this with monotonically increasing page numbers,
    /// so the final call stores the true high-water mark — and a `--refresh`
    /// (or shrink re-walk) that ends on a lower page correctly lowers it.
    pub fn set_topic_meta(
        &self,
        topic_id: i64,
        title: Option<&str>,
        posts_count: Option<i64>,
        last_page: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO topics (id, title, posts_count, last_page, synced_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                title       = COALESCE(excluded.title, topics.title),
                posts_count = COALESCE(excluded.posts_count, topics.posts_count),
                last_page   = excluded.last_page,
                synced_at   = excluded.synced_at",
            params![topic_id, title, posts_count, last_page as i64],
        )?;
        Ok(())
    }

    /// Upsert a batch of Discourse post objects. The common fields are pulled
    /// into typed columns for SQL analysis; the whole object is also stored in
    /// `raw` so `--json` keeps full fidelity (reactions, post_url, …). Returns
    /// how many had an `id`.
    pub fn upsert_posts(&self, topic_id: i64, posts: &[Value]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut count = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO posts
                    (id, topic_id, post_number, username, created_at, updated_at, cooked, raw, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET
                    post_number = excluded.post_number,
                    username    = excluded.username,
                    created_at  = excluded.created_at,
                    updated_at  = excluded.updated_at,
                    cooked      = excluded.cooked,
                    raw         = excluded.raw,
                    fetched_at  = excluded.fetched_at",
            )?;
            for p in posts {
                let Some(id) = p.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                stmt.execute(params![
                    id,
                    topic_id,
                    p.get("post_number").and_then(Value::as_i64),
                    p.get("username").and_then(Value::as_str),
                    p.get("created_at").and_then(Value::as_str),
                    p.get("updated_at").and_then(Value::as_str),
                    p.get("cooked").and_then(Value::as_str),
                    serde_json::to_string(p)?,
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// All cached posts for a topic, ordered by post number. Returns the full
    /// original post objects (from `raw`) so `--json` stays faithful; falls
    /// back to the typed columns if `raw` is somehow missing.
    pub fn get_posts(&self, topic_id: i64) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT raw, id, post_number, username, created_at, updated_at, cooked
             FROM posts WHERE topic_id = ?1 ORDER BY post_number",
        )?;
        let rows = stmt.query_map([topic_id], |r| {
            let raw: Option<String> = r.get(0)?;
            if let Some(v) = raw
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            {
                return Ok(v);
            }
            Ok(json!({
                "id": r.get::<_, i64>(1)?,
                "post_number": r.get::<_, Option<i64>>(2)?,
                "username": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, Option<String>>(4)?,
                "updated_at": r.get::<_, Option<String>>(5)?,
                "cooked": r.get::<_, Option<String>>(6)?,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Run an arbitrary SQL query and return its columns and rows. Intended for
    /// a read-only connection (see `open_readonly`) so a query can never mutate
    /// the cache — a write statement errors with "readonly database".
    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            bail!("no SQL provided");
        }
        // `conn.prepare` compiles only the first statement and silently ignores
        // the rest, so reject multi-statement input rather than drop it.
        if !is_single_statement(trimmed) {
            bail!("only a single SQL statement is supported");
        }
        let mut stmt = self.conn.prepare(trimmed).context("preparing SQL query")?;
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let n = columns.len();
        let rows = stmt
            .query_map([], |row| {
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    out.push(sqlite_to_json(row.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(out)
            })
            .context("running SQL query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("reading query results")?;
        Ok(QueryResult { columns, rows })
    }

    pub fn topic_title(&self, topic_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT title FROM topics WHERE id = ?1", [topic_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    pub fn post_count(&self, topic_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE topic_id = ?1",
            [topic_id],
            |r| r.get(0),
        )?)
    }
}

/// Columns and rows returned by [`Cache::query`]; each row is one value per
/// column, in `columns` order.
#[derive(Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Convert a dynamically-typed SQLite cell into JSON. Blobs become a short
/// placeholder rather than dumping binary (the cache has no blob columns; this
/// only matters for contrived queries like `randomblob()`).
fn sqlite_to_json(v: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as Sql;
    match v {
        Sql::Null => Value::Null,
        Sql::Integer(i) => Value::from(i),
        // serde_json can't represent NaN/Infinity (Value::from returns Null for
        // them), which would masquerade as a SQL NULL. Surface them as strings.
        Sql::Real(f) if f.is_finite() => Value::from(f),
        Sql::Real(f) => Value::String(f.to_string()),
        Sql::Text(s) => Value::String(s),
        Sql::Blob(b) => Value::String(format!("[blob {} bytes]", b.len())),
    }
}

/// True unless `sql` contains a `;` (outside a string literal) followed by more
/// SQL — i.e. it's a single statement, optionally with a trailing semicolon.
fn is_single_statement(sql: &str) -> bool {
    let mut in_str = false;
    for (i, c) in sql.char_indices() {
        match c {
            '\'' => in_str = !in_str,
            ';' if !in_str => return sql[i + 1..].trim().is_empty(),
            _ => {}
        }
    }
    true
}

/// Resolve the cache DB path: `INDERES_FORUM_DB` if set, else the platform
/// data dir.
pub fn db_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("INDERES_FORUM_DB") {
        if !explicit.is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }
    let dirs = ProjectDirs::from("com", "inderes", "inderes-cli")
        .context("could not determine platform data directory")?;
    Ok(dirs.data_dir().join("forum-cache.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: i64, n: i64, user: &str, cooked: &str) -> Value {
        json!({
            "id": id, "post_number": n, "username": user,
            "created_at": "2026-01-01", "cooked": cooked
        })
    }

    #[test]
    fn upsert_then_get_returns_posts_in_post_number_order() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_posts(
            7,
            &[post(20, 2, "bob", "second"), post(10, 1, "alice", "first")],
        )
        .unwrap();
        let got = c.get_posts(7).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["username"], "alice"); // post_number 1 first
        assert_eq!(got[1]["username"], "bob");
        assert_eq!(got[0]["cooked"], "first");
    }

    #[test]
    fn upsert_is_idempotent_and_updates_on_conflict() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_posts(7, &[post(10, 1, "alice", "original")])
            .unwrap();
        let n = c
            .upsert_posts(7, &[post(10, 1, "alice", "edited")])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(c.post_count(7).unwrap(), 1); // still one row
        assert_eq!(c.get_posts(7).unwrap()[0]["cooked"], "edited"); // updated
    }

    #[test]
    fn upsert_skips_posts_without_id() {
        let c = Cache::open_in_memory().unwrap();
        let n = c
            .upsert_posts(7, &[json!({"post_number": 1, "cooked": "no id"})])
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(c.post_count(7).unwrap(), 0);
    }

    #[test]
    fn last_page_defaults_to_zero_then_tracks_watermark() {
        let c = Cache::open_in_memory().unwrap();
        assert_eq!(c.last_page(7).unwrap(), 0);
        c.set_topic_meta(7, Some("Title"), Some(40), 2).unwrap();
        assert_eq!(c.last_page(7).unwrap(), 2);
        // The watermark is absolute (a refresh/shrink re-walk that ends lower
        // must lower it); title is preserved via COALESCE on a None update.
        c.set_topic_meta(7, None, None, 1).unwrap();
        assert_eq!(c.last_page(7).unwrap(), 1);
        assert_eq!(c.topic_title(7).unwrap().as_deref(), Some("Title"));
    }

    #[test]
    fn get_posts_preserves_full_raw_object() {
        // Fields beyond the typed columns (e.g. reactions) must survive a
        // round-trip so `--json` stays faithful.
        let c = Cache::open_in_memory().unwrap();
        let rich = json!({
            "id": 10, "post_number": 1, "username": "alice",
            "cooked": "hi", "post_url": "/t/7/1", "reactions": [{"id": "heart", "count": 3}]
        });
        c.upsert_posts(7, &[rich]).unwrap();
        let got = c.get_posts(7).unwrap();
        assert_eq!(got[0]["post_url"], "/t/7/1");
        assert_eq!(got[0]["reactions"][0]["count"], 3);
    }

    #[test]
    fn posts_are_scoped_per_topic() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_posts(7, &[post(10, 1, "a", "x")]).unwrap();
        c.upsert_posts(8, &[post(11, 1, "b", "y")]).unwrap();
        assert_eq!(c.post_count(7).unwrap(), 1);
        assert_eq!(c.post_count(8).unwrap(), 1);
        assert_eq!(c.get_posts(8).unwrap()[0]["username"], "b");
    }

    #[test]
    fn query_returns_columns_and_rows() {
        let c = Cache::open_in_memory().unwrap();
        c.upsert_posts(7, &[post(10, 1, "alice", "x"), post(11, 2, "alice", "y")])
            .unwrap();
        c.upsert_posts(7, &[post(12, 3, "bob", "z")]).unwrap();
        let r = c
            .query("SELECT username, COUNT(*) n FROM posts GROUP BY username ORDER BY n DESC")
            .unwrap();
        assert_eq!(r.columns, vec!["username", "n"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0][0], serde_json::json!("alice"));
        assert_eq!(r.rows[0][1], serde_json::json!(2)); // integer mapped to JSON number
    }

    #[test]
    fn readonly_connection_rejects_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("forum.db");
        {
            let c = Cache::open_at(&path).unwrap();
            c.upsert_posts(7, &[post(10, 1, "alice", "x")]).unwrap();
        }
        let ro = Cache::open_readonly_at(&path).unwrap();
        // Reads work...
        assert_eq!(
            ro.query("SELECT COUNT(*) FROM posts").unwrap().rows[0][0],
            serde_json::json!(1)
        );
        // ...writes are refused by the read-only connection.
        let err = ro.query("DELETE FROM posts").unwrap_err();
        assert!(
            format!("{err:#}").to_lowercase().contains("readonly")
                || format!("{err:#}").to_lowercase().contains("read-only")
                || format!("{err:#}").to_lowercase().contains("read only"),
            "got: {err:#}"
        );
    }

    #[test]
    fn query_rejects_empty_and_multi_statement() {
        let c = Cache::open_in_memory().unwrap();
        assert!(format!("{:#}", c.query("").unwrap_err()).contains("no SQL"));
        assert!(format!("{:#}", c.query("   ").unwrap_err()).contains("no SQL"));
        assert!(format!("{:#}", c.query("SELECT 1; SELECT 2").unwrap_err())
            .contains("single SQL statement"));
        // A trailing semicolon is fine, and a semicolon inside a string literal
        // is not a statement separator.
        assert!(c.query("SELECT 1;").is_ok());
        assert!(c.query("SELECT 'a;b' AS s").is_ok());
    }

    #[test]
    fn query_surfaces_non_finite_floats_as_strings() {
        let c = Cache::open_in_memory().unwrap();
        let r = c.query("SELECT 1e308 * 10 AS x").unwrap();
        // Would be JSON null via Value::from(f64); we surface it as a string.
        assert!(r.rows[0][0].is_string(), "got: {:?}", r.rows[0][0]);
    }

    #[test]
    fn open_readonly_errors_when_cache_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = Cache::open_readonly_at(&dir.path().join("nope.db")).unwrap_err();
        assert!(
            format!("{err:#}").contains("no forum cache"),
            "got: {err:#}"
        );
    }

    #[test]
    fn db_path_honors_env_override() {
        // SAFETY: single-threaded test mutating a process-wide var, restored after.
        unsafe { std::env::set_var("INDERES_FORUM_DB", "/tmp/inderes-test.db") };
        assert_eq!(db_path().unwrap(), PathBuf::from("/tmp/inderes-test.db"));
        unsafe { std::env::remove_var("INDERES_FORUM_DB") };
    }
}
