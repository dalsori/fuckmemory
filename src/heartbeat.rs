//! Heartbeats: per-agent liveness in each scope, for status detection.
//!
//! Herdr shows `working/blocked/idle` by reading panes. We get the same signal
//! from the hook: every prompt touches a row, so `session list` can show who
//! is still working without a server.

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::config::now;

/// How long after the last prompt an agent is still considered `working`.
/// Herdr uses live pane reading; we approximate with a short window.
pub const WORKING_WINDOW_MS: i64 = 3 * 60 * 1000; // 3 minutes

#[derive(Debug, Clone)]
pub struct Heartbeat {
    pub scope_id: i64,
    pub agent: String,
    pub last_at: i64,
    pub last_cid: Option<String>,
}

impl Heartbeat {
    pub fn status(&self, now_ts: i64) -> &'static str {
        if now_ts - self.last_at <= WORKING_WINDOW_MS {
            "working"
        } else {
            "idle"
        }
    }
}

/// Touch the heartbeat for `agent` in `scope_id`. Call on every prompt.
pub fn touch(conn: &Connection, scope_id: i64, agent: &str, cid: Option<&str>) -> Result<()> {
    let ts = now();
    conn.execute(
        "INSERT INTO heartbeats(scope_id, agent, last_at, last_cid) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(scope_id, agent) DO UPDATE SET last_at = excluded.last_at, last_cid = excluded.last_cid",
        params![scope_id, agent, ts, cid],
    )?;
    Ok(())
}

/// List heartbeats for a scope, newest first.
pub fn list(conn: &Connection, scope_id: i64) -> Result<Vec<Heartbeat>> {
    let mut st = conn.prepare(
        "SELECT scope_id, agent, last_at, last_cid FROM heartbeats WHERE scope_id = ?1 ORDER BY last_at DESC",
    )?;
    let iter = st.query_map([scope_id], |r| {
        Ok(Heartbeat {
            scope_id: r.get(0)?,
            agent: r.get(1)?,
            last_at: r.get(2)?,
            last_cid: r.get(3)?,
        })
    })?;
    Ok(iter.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// All heartbeats across all scopes (for global view).
pub fn all(conn: &Connection) -> Result<Vec<Heartbeat>> {
    let mut st = conn.prepare(
        "SELECT scope_id, agent, last_at, last_cid FROM heartbeats ORDER BY last_at DESC",
    )?;
    let iter = st.query_map([], |r| {
        Ok(Heartbeat {
            scope_id: r.get(0)?,
            agent: r.get(1)?,
            last_at: r.get(2)?,
            last_cid: r.get(3)?,
        })
    })?;
    Ok(iter.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, scope};
    use std::path::Path;

    fn setup() -> (Connection, crate::scope::Scope) {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-hb"), Path::new("/")).unwrap();
        (conn, sc)
    }

    #[test]
    fn touch_and_list() {
        let (conn, sc) = setup();
        touch(&conn, sc.id, "claude-code", Some("abc")).unwrap();
        touch(&conn, sc.id, "opencode", None).unwrap();
        let all = list(&conn, sc.id).unwrap();
        assert_eq!(all.len(), 2);
        // touch again updates timestamp, not duplicate
        touch(&conn, sc.id, "claude-code", Some("abc")).unwrap();
        let again = list(&conn, sc.id).unwrap();
        assert_eq!(again.len(), 2);
    }

    #[test]
    fn status_working_vs_idle() {
        let mut hb = Heartbeat {
            scope_id: 1,
            agent: "x".into(),
            last_at: now(),
            last_cid: None,
        };
        assert_eq!(hb.status(now()), "working");
        hb.last_at -= WORKING_WINDOW_MS + 1000;
        assert_eq!(hb.status(now()), "idle");
    }

    #[test]
    fn status_boundary_exact() {
        let now_ts = now();
        let hb = Heartbeat {
            scope_id: 1,
            agent: "x".into(),
            last_at: now_ts - WORKING_WINDOW_MS,
            last_cid: None,
        };
        assert_eq!(hb.status(now_ts), "working", "exact boundary should be working");
        let hb2 = Heartbeat {
            scope_id: 1,
            agent: "x".into(),
            last_at: now_ts - WORKING_WINDOW_MS - 1,
            last_cid: None,
        };
        assert_eq!(hb2.status(now_ts), "idle");
    }

    #[test]
    fn touch_updates_timestamp_and_cid() {
        let (conn, sc) = setup();
        touch(&conn, sc.id, "agent", Some("cid1")).unwrap();
        let hb = list(&conn, sc.id).unwrap();
        assert_eq!(hb[0].last_cid.as_deref(), Some("cid1"));
        let first_at = hb[0].last_at;
        std::thread::sleep(std::time::Duration::from_millis(2));
        touch(&conn, sc.id, "agent", Some("cid2")).unwrap();
        let hb2 = list(&conn, sc.id).unwrap();
        assert_eq!(hb2[0].last_cid.as_deref(), Some("cid2"));
        assert!(hb2[0].last_at > first_at, "timestamp should advance");
    }

    #[test]
    fn scope_isolation() {
        let conn = db::open_memory().unwrap();
        let sc1 = scope::resolve(&conn, Some("/tmp/hb-scope1"), Path::new("/")).unwrap();
        let sc2 = scope::resolve(&conn, Some("/tmp/hb-scope2"), Path::new("/")).unwrap();
        touch(&conn, sc1.id, "a", None).unwrap();
        assert_eq!(list(&conn, sc1.id).unwrap().len(), 1);
        assert_eq!(list(&conn, sc2.id).unwrap().len(), 0);
        assert_eq!(all(&conn).unwrap().len(), 1);
        touch(&conn, sc2.id, "b", None).unwrap();
        assert_eq!(all(&conn).unwrap().len(), 2);
    }

    #[test]
    fn list_ordering_newest_first() {
        let (conn, sc) = setup();
        touch(&conn, sc.id, "first", None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        touch(&conn, sc.id, "second", None).unwrap();
        let all = list(&conn, sc.id).unwrap();
        assert_eq!(all[0].agent, "second");
        assert_eq!(all[1].agent, "first");
    }

    #[test]
    fn concurrent_touch_no_corruption() {
        let dir = std::env::temp_dir().join(format!("fm-hb-conc-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hb.db");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let c = crate::db::open(&path).unwrap();
                    let sc = scope::resolve(&c, Some("/tmp/hb-conc"), Path::new("/")).unwrap();
                    for _ in 0..10 {
                        touch(&c, sc.id, &format!("agent-{i}"), None).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let c = crate::db::open(&path).unwrap();
        let sc = scope::resolve(&c, Some("/tmp/hb-conc"), Path::new("/")).unwrap();
        let all = list(&c, sc.id).unwrap();
        assert_eq!(all.len(), 8);
        std::fs::remove_dir_all(&dir).ok();
    }
}
