//! An in-progress work checkpoint.
//!
//! Agents lose their context when a session ends — a token budget hits, the
//! machine reboots, a different agent takes over. `task save` records what a
//! task is doing *right now*: the goal, the current state, and the files it
//! touches. `task status` hands that back to whoever resumes the work, so a
//! fresh agent continues instead of re-deriving the plan and re-touching files.
//!
//! The checkpoint lives in `meta` (one active task), and the last save is also
//! stored as an episode so `recall` can find it even before someone asks for
//! `task status`.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::config::now;
use crate::db;
use crate::store;

const META_KEY: &str = "task_checkpoint";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCheckpoint {
    /// The original request or goal, if the caller supplied one.
    pub goal: Option<String>,
    /// The state of the work: what's done, what's in flight, what's next.
    pub state: String,
    /// Files this task is touching, so a resuming agent knows the blast radius.
    #[serde(default)]
    pub files: Vec<String>,
    pub opened_at: i64,
    pub updated_at: i64,
}

/// Read the active checkpoint, if any.
pub fn current(conn: &Connection) -> Result<Option<TaskCheckpoint>> {
    let Some(raw) = db::meta_get(conn, META_KEY)? else {
        return Ok(None);
    };
    let cp = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("task checkpoint in meta is corrupt: {e}"))?;
    Ok(Some(cp))
}

/// Save or replace the active checkpoint. Preserves `opened_at` across updates
/// so a long task keeps one birth date.
pub fn save(
    conn: &mut Connection,
    state: &str,
    files: &[String],
    goal: Option<&str>,
) -> Result<TaskCheckpoint> {
    let ts = now();
    let prev = current(conn)?;
    let opened_at = prev.as_ref().map(|p| p.opened_at).unwrap_or(ts);
    let cp = TaskCheckpoint {
        goal: goal
            .map(str::to_string)
            .or_else(|| prev.and_then(|p| p.goal)),
        state: state.trim().to_string(),
        files: files.to_vec(),
        opened_at,
        updated_at: ts,
    };
    let raw = serde_json::to_string(&cp)?;
    db::meta_set(conn, META_KEY, &raw)?;
    Ok(cp)
}

/// Close the active task. The checkpoint is kept in `meta` so `--as-of` and a
/// final `task status` can still see it, but it is no longer "active"; the next
/// `task save` starts fresh. An optional note is folded into the state so the
/// closing agent's last words survive.
pub fn done(conn: &mut Connection, note: Option<&str>) -> Result<Option<TaskCheckpoint>> {
    let Some(cp) = current(conn)? else {
        return Ok(None);
    };
    let mut closed = cp.clone();
    if let Some(note) = note {
        closed.state = format!("{}\n\n[closed] {note}", cp.state.trim());
    } else {
        closed.state = format!("{}\n\n[closed]", cp.state.trim());
    }
    let raw = serde_json::to_string(&closed)?;
    db::meta_set(conn, META_KEY, &raw)?;
    Ok(Some(closed))
}

/// Store the checkpoint as a searchable episode too. Returns the episode id.
/// The text is prefixed so a `recall` for "task" or the goal finds it.
pub fn remember_as_episode(
    conn: &mut Connection,
    scope: &crate::scope::Scope,
    emb: Option<&crate::embed::Embedder>,
    cp: &TaskCheckpoint,
    done: bool,
) -> Result<i64> {
    let mut text = String::from("[in-progress task]\n");
    if let Some(g) = &cp.goal {
        text.push_str(&format!("goal: {g}\n"));
    }
    text.push_str(&cp.state);
    if done {
        text.push_str("\n(status: closed)");
    }
    let mut input = store::RememberInput {
        text,
        kind: "task".into(),
        source: "task".into(),
        facts: vec![],
        files: vec![],
        meta: None,
        derive: false,
    };
    for f in &cp.files {
        let path = f.trim();
        if path.is_empty() {
            continue;
        }
        // Best-effort: read what we can of the file to attach a bounded snippet.
        if let Ok(fi) = store::read_file_input(path, &cwd_path(), None) {
            input.files.push(fi);
        } else {
            input.files.push(store::FileInput {
                path: path.to_string(),
                lang: None,
                snippet: String::new(),
                line_from: None,
                line_to: None,
            });
        }
    }
    let out = store::remember(conn, scope, emb, &input)?;
    Ok(out.episode_id)
}

fn cwd_path() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Render a checkpoint for a resuming agent.
pub fn render(cp: &TaskCheckpoint) -> String {
    let mut out = String::new();
    out.push_str("# In-progress task\n\n");
    if let Some(g) = &cp.goal {
        out.push_str(&format!("**Goal:** {g}\n\n"));
    }
    out.push_str(&format!("**State:**\n{}\n\n", cp.state.trim()));
    if !cp.files.is_empty() {
        out.push_str("**Files touched:**\n");
        for f in &cp.files {
            out.push_str(&format!("- `{f}`\n"));
        }
    }
    out.push_str(&format!(
        "\nOpened {} · last updated {}\n",
        ymd(cp.opened_at),
        ymd(cp.updated_at)
    ));
    out
}

fn ymd(t: i64) -> String {
    let days = t.div_euclid(86_400_000); // now() is epoch milliseconds
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-epoch to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope;
    use std::path::Path;

    fn setup() -> (Connection, crate::scope::Scope) {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-task"), Path::new("/")).unwrap();
        (conn, sc)
    }

    #[test]
    fn save_then_status_round_trips() {
        let (mut conn, _sc) = setup();
        assert!(current(&conn).unwrap().is_none());

        let cp = save(
            &mut conn,
            "wired the opencode plugin; now verifying install/uninstall",
            &["src/install.rs".into()],
            Some("add opencode hooks"),
        )
        .unwrap();
        assert_eq!(cp.goal.as_deref(), Some("add opencode hooks"));
        assert_eq!(cp.files, vec!["src/install.rs".to_string()]);

        let read = current(&conn).unwrap().unwrap();
        assert_eq!(read.state, cp.state);
        assert_eq!(read.opened_at, read.updated_at);
    }

    #[test]
    fn updating_keeps_the_original_opened_at_and_goal() {
        let (mut conn, _sc) = setup();
        let first = save(&mut conn, "started", &[], Some("ship the retry loop")).unwrap();
        let second = save(&mut conn, "nearly done", &["src/main.rs".into()], None).unwrap();
        assert_eq!(second.opened_at, first.opened_at);
        assert_eq!(second.goal.as_deref(), Some("ship the retry loop"));
        assert_eq!(second.files, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn done_marks_the_checkpoint_and_clears_nothing() {
        let (mut conn, _sc) = setup();
        save(&mut conn, "in flight", &[], None).unwrap();
        let closed = done(&mut conn, Some("shipped to prod")).unwrap().unwrap();
        assert!(closed.state.contains("[closed] shipped to prod"));
        assert!(current(&conn).unwrap().is_some(), "checkpoint is kept");
    }

    #[test]
    fn done_with_no_active_task_is_a_noop() {
        let (mut conn, _sc) = setup();
        assert!(done(&mut conn, None).unwrap().is_none());
    }

    #[test]
    fn remember_stores_the_checkpoint_as_a_searchable_episode() {
        let (mut conn, sc) = setup();
        let cp = save(
            &mut conn,
            "implementing task checkpoints",
            &[],
            Some("resumable work"),
        )
        .unwrap();
        let eid = remember_as_episode(&mut conn, &sc, None, &cp, false).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM episodes WHERE id = ?1", [eid], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "task");
        let body: String = conn
            .query_row("SELECT body FROM episodes WHERE id = ?1", [eid], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(body.contains("goal: resumable work"));
        assert!(body.contains("implementing task checkpoints"));
    }
}
