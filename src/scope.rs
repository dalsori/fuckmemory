//! Memory namespaces.
//!
//! Everything is written into exactly one scope. `global` holds facts that are
//! about *you* and travel between projects ("prefers pnpm", "hates emoji in
//! commits"). A project scope holds facts that only make sense inside one repo.
//! Recall reads the project scope and `global` together, so a fresh clone of a
//! repo still gets your preferences.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

use crate::config::now;

pub const GLOBAL_KEY: &str = "global";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub id: i64,
    pub key: String,
    pub label: String,
}

/// Find the project root for `start`: nearest ancestor holding a repo/package
/// marker. Falls back to `start` itself so we never silently write into `$HOME`'s
/// scope from a subdirectory.
pub fn project_root(start: &Path) -> PathBuf {
    const MARKERS: &[&str] = &[
        ".git",
        ".hg",
        ".jj",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "AGENTS.md",
    ];
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    for dir in start.ancestors() {
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
    }
    start
}

/// Stable key for a filesystem root. Hashed so the key is fixed-width and does
/// not leak the full path into config files, but the readable label is kept too.
fn key_for_root(root: &Path) -> String {
    let h = blake3::hash(root.to_string_lossy().as_bytes());
    format!("p:{}", &h.to_hex()[..16])
}

/// Resolve a scope by name, creating it if needed.
///
/// - `None` or `"."` → the project containing `cwd`
/// - `"global"`      → the shared scope
/// - anything else   → treated as a path
pub fn resolve(conn: &Connection, spec: Option<&str>, cwd: &Path) -> Result<Scope> {
    match spec {
        Some(GLOBAL_KEY) => upsert(conn, GLOBAL_KEY, "global", None),
        Some(s) if s.starts_with("p:") => {
            // An already-resolved key; must exist.
            if let Some(sc) = get(conn, s)? {
                Ok(sc)
            } else {
                upsert(conn, s, s, None)
            }
        }
        Some(s) if s != "." => {
            let root = project_root(Path::new(s));
            let label = label_for(&root);
            upsert(conn, &key_for_root(&root), &label, Some(&root))
        }
        _ => {
            let root = project_root(cwd);
            let label = label_for(&root);
            upsert(conn, &key_for_root(&root), &label, Some(&root))
        }
    }
}

fn label_for(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Scope>> {
    let mut st = conn.prepare_cached("SELECT id, key, label FROM scopes WHERE key = ?1")?;
    let mut rows = st.query([key])?;
    if let Some(r) = rows.next()? {
        Ok(Some(Scope {
            id: r.get(0)?,
            key: r.get(1)?,
            label: r.get(2)?,
        }))
    } else {
        Ok(None)
    }
}

fn upsert(conn: &Connection, key: &str, label: &str, root: Option<&Path>) -> Result<Scope> {
    let root_s = root.map(|p| p.to_string_lossy().to_string());
    conn.execute(
        "INSERT INTO scopes(key, label, root, created_at) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET label = excluded.label, root = excluded.root",
        params![key, label, root_s, now()],
    )?;
    get(conn, key)?.ok_or_else(|| anyhow::anyhow!("scope {key} vanished after insert"))
}

/// The global scope, created on demand.
pub fn global(conn: &Connection) -> Result<Scope> {
    upsert(conn, GLOBAL_KEY, "global", None)
}

/// Scopes a recall should read: the requested one plus `global`.
pub fn read_set(conn: &Connection, scope: &Scope) -> Result<Vec<i64>> {
    let g = global(conn)?;
    let mut ids = vec![scope.id];
    if g.id != scope.id {
        ids.push(g.id);
    }
    Ok(ids)
}

pub fn list(conn: &Connection) -> Result<Vec<(Scope, i64, Option<String>)>> {
    let mut st = conn.prepare(
        "SELECT s.id, s.key, s.label, s.root,
                (SELECT count(*) FROM facts f WHERE f.scope_id = s.id AND f.invalidated_at IS NULL)
         FROM scopes s ORDER BY s.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            Scope {
                id: r.get(0)?,
                key: r.get(1)?,
                label: r.get(2)?,
            },
            r.get::<_, i64>(4)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn same_root_same_key_from_subdir() {
        let tmp = std::env::temp_dir().join(format!("fm-scope-{}", std::process::id()));
        let sub = tmp.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();

        let conn = db::open_memory().unwrap();
        let from_root = resolve(&conn, None, &tmp).unwrap();
        let from_sub = resolve(&conn, None, &sub).unwrap();
        assert_eq!(from_root.id, from_sub.id);
        assert!(from_root.key.starts_with("p:"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn global_is_separate_and_always_read() {
        let conn = db::open_memory().unwrap();
        let proj = resolve(&conn, Some("/nonexistent/xyz"), Path::new("/")).unwrap();
        let g = global(&conn).unwrap();
        assert_ne!(proj.id, g.id);
        let set = read_set(&conn, &proj).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&g.id));
    }
}
