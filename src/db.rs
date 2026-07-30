//! SQLite storage: schema, migrations, connection tuning.
//!
//! Two layers live here. `episodes` keep the raw text verbatim so the original
//! wording is never lost. `entities` + `facts` are the distilled graph, where
//! every fact edge is bi-temporal: `valid_from`/`valid_to` say when the fact was
//! true in the world, `recorded_at`/`invalidated_at` say when we learned it. A
//! superseded fact is invalidated, never deleted, which is what makes "what did
//! I believe last March?" answerable.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Bumped whenever `MIGRATIONS` grows. Stored in `meta`.
pub const SCHEMA_VERSION: i64 = 1;

/// Discriminator for the shared `vecs` table.
pub const VEC_FACT: i64 = 1;
pub const VEC_EPISODE: i64 = 2;

const MIGRATIONS: &[&str] = &[
    // ---- v1 -----------------------------------------------------------------
    r#"
CREATE TABLE meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
) WITHOUT ROWID;

-- A scope is a memory namespace: one per project plus a shared 'global'.
CREATE TABLE scopes (
    id         INTEGER PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    label      TEXT NOT NULL,
    root       TEXT,
    created_at INTEGER NOT NULL
);

-- Raw, immutable observations.
CREATE TABLE episodes (
    id          INTEGER PRIMARY KEY,
    scope_id    INTEGER NOT NULL REFERENCES scopes(id) ON DELETE CASCADE,
    source      TEXT NOT NULL DEFAULT 'unknown',
    kind        TEXT NOT NULL DEFAULT 'note',
    body        TEXT NOT NULL,
    meta        TEXT,
    hash        BLOB NOT NULL,
    recorded_at INTEGER NOT NULL,
    UNIQUE(scope_id, hash)
);
CREATE INDEX episodes_scope_time ON episodes(scope_id, recorded_at DESC);

-- Graph nodes.
CREATE TABLE entities (
    id          INTEGER PRIMARY KEY,
    scope_id    INTEGER NOT NULL REFERENCES scopes(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    norm        TEXT NOT NULL,
    kind        TEXT,
    summary     TEXT,
    degree      INTEGER NOT NULL DEFAULT 0,
    recorded_at INTEGER NOT NULL,
    UNIQUE(scope_id, norm)
);

-- Alternate spellings that should resolve to the same node.
CREATE TABLE aliases (
    entity_id INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    scope_id  INTEGER NOT NULL,
    norm      TEXT NOT NULL,
    PRIMARY KEY(scope_id, norm)
) WITHOUT ROWID;

-- Graph edges = facts. Bi-temporal.
CREATE TABLE facts (
    id             INTEGER PRIMARY KEY,
    scope_id       INTEGER NOT NULL REFERENCES scopes(id) ON DELETE CASCADE,
    src            INTEGER REFERENCES entities(id) ON DELETE SET NULL,
    dst            INTEGER REFERENCES entities(id) ON DELETE SET NULL,
    rel            TEXT NOT NULL DEFAULT 'relates_to',
    statement      TEXT NOT NULL,
    confidence     REAL NOT NULL DEFAULT 1.0,
    episode_id     INTEGER REFERENCES episodes(id) ON DELETE SET NULL,
    hash           BLOB NOT NULL,
    -- world time
    valid_from     INTEGER,
    valid_to       INTEGER,
    -- transaction time
    recorded_at    INTEGER NOT NULL,
    invalidated_at INTEGER,
    invalidated_by INTEGER REFERENCES facts(id) ON DELETE SET NULL,
    -- usage feedback, drives the popularity term in ranking
    hits           INTEGER NOT NULL DEFAULT 0,
    last_hit       INTEGER
);
CREATE INDEX facts_scope_live  ON facts(scope_id, invalidated_at, recorded_at DESC);
CREATE INDEX facts_src         ON facts(src) WHERE src IS NOT NULL;
CREATE INDEX facts_dst         ON facts(dst) WHERE dst IS NOT NULL;
CREATE INDEX facts_episode     ON facts(episode_id);
CREATE UNIQUE INDEX facts_dedup ON facts(scope_id, hash) WHERE invalidated_at IS NULL;

-- int8-quantized embeddings, one row per fact/episode.
CREATE TABLE vecs (
    kind   INTEGER NOT NULL,
    ref_id INTEGER NOT NULL,
    q      BLOB NOT NULL,
    PRIMARY KEY(kind, ref_id)
) WITHOUT ROWID;

-- Full-text side of hybrid retrieval. tokenchars keeps identifiers like
-- `use_pnpm` and `src/db.rs` as single tokens. '-' is deliberately NOT a token
-- char: it would make `--no-verify` one token including the leading dashes, which
-- then fails to match a query written as `no-verify`. Splitting on '-' on both
-- sides is what makes flags findable. Keep this in sync with `retrieve::fts_query`.
CREATE VIRTUAL TABLE facts_fts USING fts5(
    statement,
    content='facts',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2 tokenchars '_.@/'",
    prefix='2 3'
);
CREATE TRIGGER facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, statement) VALUES (new.id, new.statement);
END;
CREATE TRIGGER facts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, statement) VALUES ('delete', old.id, old.statement);
END;
CREATE TRIGGER facts_au AFTER UPDATE OF statement ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, statement) VALUES ('delete', old.id, old.statement);
    INSERT INTO facts_fts(rowid, statement) VALUES (new.id, new.statement);
END;

CREATE VIRTUAL TABLE episodes_fts USING fts5(
    body,
    content='episodes',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2 tokenchars '_.@/'",
    prefix='2 3'
);
CREATE TRIGGER episodes_ai AFTER INSERT ON episodes BEGIN
    INSERT INTO episodes_fts(rowid, body) VALUES (new.id, new.body);
END;
CREATE TRIGGER episodes_ad AFTER DELETE ON episodes BEGIN
    INSERT INTO episodes_fts(episodes_fts, rowid, body) VALUES ('delete', old.id, old.body);
END;
CREATE TRIGGER episodes_au AFTER UPDATE OF body ON episodes BEGIN
    INSERT INTO episodes_fts(episodes_fts, rowid, body) VALUES ('delete', old.id, old.body);
    INSERT INTO episodes_fts(rowid, body) VALUES (new.id, new.body);
END;

-- Work queue for background consolidation, so writes never block on it.
CREATE TABLE pending (
    id         INTEGER PRIMARY KEY,
    episode_id INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    queued_at  INTEGER NOT NULL
);
"#,
];

/// Open (creating if needed) the database at `path` and bring it up to date.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data dir {}", parent.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    tune(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// In-memory database, used by tests and `--dry-run`.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    tune(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn tune(conn: &Connection) -> Result<()> {
    // FIRST, before any other pragma. Switching journal mode needs the database
    // lock, so when several agents open a fresh store at the same moment they all
    // contend on it — and with the default zero timeout the losers get
    // SQLITE_BUSY instead of waiting. Everything below assumes this is set.
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    // WAL so a recall never blocks on a concurrent remember from another agent.
    enable_wal(conn)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -32_000i64)?; // ~32 MB
    conn.pragma_update(None, "mmap_size", 268_435_456i64)?; // 256 MB
    Ok(())
}

/// Put the database into WAL, tolerating other processes doing the same thing.
///
/// `PRAGMA journal_mode = WAL` needs an exclusive lock, and unlike an ordinary
/// write it returns SQLITE_BUSY *without consulting the busy handler* when
/// another connection holds the file. So `busy_timeout` does not protect this
/// one statement, and a fresh store opened by several agents at once had one of
/// them fail outright with "database is locked" — which autosave makes far more
/// likely, since it spawns a process per prompt.
///
/// Two facts make the retry safe and short: the mode is stored in the file
/// header, so whoever wins sets it for everybody, and losing changes nothing but
/// concurrency. A connection that still can't switch after a moment therefore
/// carries on in whatever mode it has rather than failing the user's command.
fn enable_wal(conn: &Connection) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap_or_default();
        // "memory" is an in-memory database, which cannot be WAL and must not
        // be retried — that would spin for the whole deadline on every test.
        if mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory") {
            return Ok(());
        }
        if let Ok(m) = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
            if m.eq_ignore_ascii_case("wal") {
                return Ok(());
            }
        }
        if started.elapsed() > std::time::Duration::from_secs(5) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    // Fast path: already current, no lock taken. This is the common case, and
    // every agent process runs it on startup.
    if version(conn)? as usize >= MIGRATIONS.len() {
        return Ok(());
    }

    // Several agents can open the database simultaneously. Without an exclusive
    // write lock held across the version check *and* the DDL, two processes both
    // read version 0 and both try to create the tables. BEGIN IMMEDIATE takes the
    // write lock up front; the loser waits on busy_timeout, then re-reads the
    // version inside the transaction and finds nothing left to do.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("acquiring the write lock to migrate")?;
    let result = (|| -> Result<()> {
        let applied = version(conn)? as usize;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            conn.execute_batch(sql)
                .with_context(|| format!("applying migration {}", i + 1))?;
        }
        if applied < MIGRATIONS.len() {
            // pragma_update can't be parameterized, and the value is a constant.
            conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;
            conn.execute(
                "INSERT INTO meta(k, v) VALUES('schema_version', ?1)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK").ok();
            Err(e)
        }
    }
}

fn version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Read a `meta` value.
pub fn meta_get(conn: &Connection, k: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0))
        .ok())
}

/// Write a `meta` value.
pub fn meta_set(conn: &Connection, k: &str, v: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(k, v) VALUES(?1, ?2)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        rusqlite::params![k, v],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_and_has_fts5() {
        let conn = open_memory().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        // Proves the bundled SQLite really has FTS5 compiled in.
        conn.execute_batch("INSERT INTO scopes(key,label,created_at) VALUES('t','t',0)")
            .unwrap();
        conn.execute_batch(
            "INSERT INTO facts(scope_id,statement,hash,recorded_at)
             VALUES(1,'uses pnpm not npm',x'00',0)",
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH 'pnpm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = open_memory().unwrap();
        migrate(&conn).unwrap();
        assert_eq!(
            meta_get(&conn, "schema_version").unwrap().as_deref(),
            Some("1")
        );
    }
}
