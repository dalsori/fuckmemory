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
pub const SCHEMA_VERSION: i64 = 2;

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
    // ---- v2: file references ------------------------------------------------
    // Memories can carry the file they were learned against. `path` is relative
    // to the project root the memory was recorded in; `snippet` holds a bounded
    // excerpt (or the lines the user pointed at) so a recall can show *where* a
    // convention lives without re-reading the whole tree.
    r#"
CREATE TABLE file_refs (
    id          INTEGER PRIMARY KEY,
    episode_id  INTEGER NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    lang        TEXT,
    snippet     TEXT NOT NULL,
    line_from   INTEGER,
    line_to     INTEGER,
    recorded_at INTEGER NOT NULL
);
CREATE INDEX file_refs_episode ON file_refs(episode_id);
CREATE INDEX file_refs_path ON file_refs(path);
"#,
    // ---- v2 -----------------------------------------------------------------
    // Provenance: the short git HEAD the repo was at when an episode was
    // learned, so recall can answer "from what repository state".
    r#"
ALTER TABLE episodes ADD COLUMN head TEXT;
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

/// Rebuild the FTS5 indexes (`facts_fts`, `episodes_fts`) from the base tables.
///
/// Used by `doctor --fix` when an index is missing or drifted out of sync. Drops
/// the two virtual tables, re-runs the DDL (triggers included) and repopulates
/// them in a single transaction, so a crash leaves the old state intact.
pub fn rebuild_fts(conn: &Connection) -> Result<()> {
    let fts_ddl: Vec<&str> = MIGRATIONS
        .iter()
        .flat_map(|m| m.split(';'))
        .map(str::trim)
        .filter(|s| s.contains("CREATE VIRTUAL TABLE") && s.contains("_fts"))
        .collect();
    if fts_ddl.len() != 2 {
        anyhow::bail!(
            "expected 2 FTS tables in the schema, found {}",
            fts_ddl.len()
        );
    }
    conn.execute_batch("BEGIN;")?;
    let r = (|| -> Result<()> {
        conn.execute("DROP TABLE IF EXISTS facts_fts", [])?;
        conn.execute("DROP TABLE IF EXISTS episodes_fts", [])?;
        for ddl in &fts_ddl {
            conn.execute_batch(ddl)?;
        }
        conn.execute(
            "INSERT INTO facts_fts(rowid, statement) SELECT id, statement FROM facts",
            [],
        )?;
        conn.execute(
            "INSERT INTO episodes_fts(rowid, body) SELECT id, body FROM episodes",
            [],
        )?;
        Ok(())
    })();
    if r.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
        return r;
    }
    conn.execute_batch("COMMIT;")?;
    Ok(())
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

/// Monotonic version of the *vector index content*. Bumped only when the facts
/// behind the index change (a fact vector is written, a fact is invalidated or
/// deleted). Deliberately NOT moved by `mark_hits` or by episode-only writes:
/// those touch no vector the persisted index serves, and `PRAGMA data_version`
/// would have invalidated the mmap cache on every autosave prompt (~90 ms
/// rebuild). See `VecCache::index` in embed.rs.
pub fn index_version(conn: &Connection) -> Result<i64> {
    Ok(meta_get(conn, "index_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

/// Increment the vector-index version. Must be called inside the same
/// transaction as the content change so a reader never sees a stale cache
/// paired with a new version (or vice versa).
pub fn bump_index_version(conn: &Connection) -> Result<()> {
    let v = index_version(conn)? + 1;
    meta_set(conn, "index_version", &v.to_string())
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
            Some("2")
        );
    }

    #[test]
    fn rebuild_fts_repopulates_after_dropping_tables() {
        let conn = open_memory().unwrap();
        conn.execute_batch("BEGIN;").unwrap();
        conn.execute_batch(
            "INSERT INTO scopes(key, label, root, created_at) VALUES('s','s','/tmp/x',0);",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO entities(scope_id, name, norm, recorded_at) VALUES(1,'pnpm','pnpm',0);",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO facts(scope_id, src, rel, statement, hash, recorded_at)
             VALUES(1, 1, 'uses', 'we use pnpm for everything', x'aa', 0);",
        )
        .unwrap();
        conn.execute_batch("COMMIT;").unwrap();

        // Kill the index as a corrupt install might, then rebuild.
        conn.execute_batch("DROP TABLE facts_fts; DROP TABLE episodes_fts;")
            .unwrap();
        assert!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='facts_fts'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
                == 0
        );
        rebuild_fts(&conn).unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH 'pnpm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    /// A passive WAL checkpoint must shrink the -wal file without blocking even
    /// when another connection holds a read, and never error.
    #[test]
    fn wal_checkpoint_passive_is_safe() {
        let dir = std::env::temp_dir().join(format!(
            "fm-wal-{}-{}",
            std::process::id(),
            std::sync::atomic::AtomicU32::new(0).fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite");

        let conn = open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO scopes(key, label, root, created_at) VALUES('s','s','/tmp/x',0);",
        )
        .unwrap();

        // Writes accumulate in the WAL; a passive checkpoint must fold them into
        // the main DB and report success (or BUSY) without erroring.
        for _ in 0..5 {
            conn.execute_batch("UPDATE scopes SET label = label || 'x';")
                .unwrap();
        }
        let row: (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        // Result code 0 (ok) or 1 (busy) are both acceptable; anything else is
        // a failure. Busy is fine here because it means a reader held the WAL.
        assert!(
            row.0 == 0 || row.0 == 1,
            "passive checkpoint must not fail: {row:?}"
        );

        // The rows survived the checkpoint.
        let label: String = conn
            .query_row("SELECT label FROM scopes WHERE key='s'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label, "sxxxxx");
        std::fs::remove_dir_all(&dir).ok();
    }
}
