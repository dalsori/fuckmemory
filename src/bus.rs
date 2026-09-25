//! Bus / inbox: lightweight pub/sub between agents, scoped per project.
//!
//! The memory graph is passive — an agent has to recall. The bus is active:
//! one agent `send`s a message, another gets it injected on its next prompt
//! or via `bus poll`. No cloud, no server: just two tables in the same SQLite
//! file every agent already shares.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::config::now;
use crate::pack;

/// One bus message.
#[derive(Debug, Clone)]
pub struct BusMessage {
    pub id: i64,
    pub scope_id: i64,
    pub channel: String,
    pub from_agent: String,
    pub from_cid: Option<String>,
    pub to_agent: Option<String>,
    pub body: String,
    pub created_at: i64,
    pub ttl_ms: Option<i64>,
}

/// Send a message on the bus. Returns the inserted id.
pub fn send(
    conn: &Connection,
    scope_id: i64,
    from_agent: &str,
    from_cid: Option<&str>,
    to_agent: Option<&str>,
    channel: &str,
    body: &str,
    ttl_ms: Option<i64>,
) -> Result<i64> {
    let ch = normalize_channel(channel);
    let body = body.trim();
    anyhow::ensure!(!body.is_empty(), "bus message cannot be empty");
    anyhow::ensure!(
        body.chars().count() <= 4000,
        "bus message too long (max 4000 chars)"
    );
    anyhow::ensure!(!from_agent.trim().is_empty(), "from_agent required");
    let ts = now();
    // Best-effort expiry of old TTL messages before inserting (cheap, runs per send).
    let _ = prune_expired(conn, ts);
    conn.execute(
        "INSERT INTO bus_messages(scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![scope_id, ch, from_agent, from_cid, to_agent, body, ts, ttl_ms],
    )?;
    Ok(conn.last_insert_rowid())
}

fn normalize_channel(s: &str) -> String {
    let mut out = s.trim().to_lowercase();
    if out.is_empty() {
        return "general".to_string();
    }
    out = out
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "general".to_string()
    } else {
        out.chars().take(32).collect()
    }
}

/// List recent messages in a scope, newest first.
pub fn list(
    conn: &Connection,
    scope_id: i64,
    channel: Option<&str>,
    limit: usize,
) -> Result<Vec<BusMessage>> {
    let lim = (limit.clamp(1, 100)) as i64;
    // prune expired for reads too (cheap)
    let _ = prune_expired(conn, now());
    let rows: Vec<BusMessage> = if let Some(ch) = channel.map(|c| normalize_channel(c)) {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1 AND channel = ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let iter = st.query_map(params![scope_id, ch, lim], row_bus)?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let iter = st.query_map(params![scope_id, lim], row_bus)?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

/// Messages pending for `agent` (broadcast + directed), ordered oldest first.
/// Advances the cursor to the max id returned.
pub fn poll(
    conn: &Connection,
    scope_id: i64,
    agent: &str,
    channel: Option<&str>,
    limit: usize,
) -> Result<Vec<BusMessage>> {
    let lim = (limit.clamp(1, 100)) as i64;
    let _ = prune_expired(conn, now());
    let last_seen = cursor_get(conn, scope_id, agent)?;
    let ch = channel.map(|c| normalize_channel(c));
    let msgs: Vec<BusMessage> = if let Some(ch) = ch {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1 AND id > ?2 AND channel = ?3
               AND (to_agent IS NULL OR to_agent = ?4)
             ORDER BY id ASC LIMIT ?5",
        )?;
        let iter = st.query_map(params![scope_id, last_seen, ch, agent, lim], row_bus)?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1 AND id > ?2
               AND (to_agent IS NULL OR to_agent = ?4)
             ORDER BY id ASC LIMIT ?3",
        )?;
        // careful with param order: ?3 is limit, ?4 is agent
        let iter = st.query_map(params![scope_id, last_seen, lim, agent], row_bus)?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if let Some(max_id) = msgs.last().map(|m| m.id) {
        cursor_set(conn, scope_id, agent, max_id)?;
    } else {
        // Ensure cursor row exists even when nothing pending, so hook can
        // distinguish "never polled" from "polled and empty".
        cursor_set_if_missing(conn, scope_id, agent, last_seen)?;
    }
    Ok(msgs)
}

/// Pending without advancing cursor (for hook preview). Use `poll` to consume.
pub fn pending(
    conn: &Connection,
    scope_id: i64,
    agent: &str,
    channel: Option<&str>,
    limit: usize,
) -> Result<Vec<BusMessage>> {
    let lim = (limit.clamp(1, 100)) as i64;
    let last_seen = cursor_get(conn, scope_id, agent)?;
    let ch = channel.map(|c| normalize_channel(c));
    let now_ts = now();
    // filter expired in SQL via ttl_ms
    let msgs: Vec<BusMessage> = if let Some(ch) = ch {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1 AND id > ?2 AND channel = ?3
               AND (to_agent IS NULL OR to_agent = ?4)
               AND (ttl_ms IS NULL OR created_at + ttl_ms > ?5)
             ORDER BY id ASC LIMIT ?6",
        )?;
        let iter = st.query_map(
            params![scope_id, last_seen, ch, agent, now_ts, lim],
            row_bus,
        )?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut st = conn.prepare(
            "SELECT id, scope_id, channel, from_agent, from_cid, to_agent, body, created_at, ttl_ms
             FROM bus_messages
             WHERE scope_id = ?1 AND id > ?2
               AND (to_agent IS NULL OR to_agent = ?3)
               AND (ttl_ms IS NULL OR created_at + ttl_ms > ?4)
             ORDER BY id ASC LIMIT ?5",
        )?;
        let iter = st.query_map(params![scope_id, last_seen, agent, now_ts, lim], row_bus)?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(msgs)
}

/// Mark messages as seen up to `max_id`.
pub fn ack(conn: &Connection, scope_id: i64, agent: &str, max_id: i64) -> Result<()> {
    cursor_set(conn, scope_id, agent, max_id)
}

fn cursor_get(conn: &Connection, scope_id: i64, agent: &str) -> Result<i64> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT last_seen_id FROM bus_cursors WHERE scope_id = ?1 AND agent = ?2",
            params![scope_id, agent],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or(0))
}

fn cursor_set(conn: &Connection, scope_id: i64, agent: &str, id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO bus_cursors(scope_id, agent, last_seen_id) VALUES(?1, ?2, ?3)
         ON CONFLICT(scope_id, agent) DO UPDATE SET last_seen_id = max(last_seen_id, excluded.last_seen_id)",
        params![scope_id, agent, id],
    )?;
    Ok(())
}

fn cursor_set_if_missing(conn: &Connection, scope_id: i64, agent: &str, id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO bus_cursors(scope_id, agent, last_seen_id) VALUES(?1, ?2, ?3)
         ON CONFLICT(scope_id, agent) DO NOTHING",
        params![scope_id, agent, id],
    )?;
    Ok(())
}

fn prune_expired(conn: &Connection, now_ts: i64) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM bus_messages WHERE ttl_ms IS NOT NULL AND created_at + ttl_ms <= ?1",
        [now_ts],
    )?;
    Ok(n)
}

fn row_bus(r: &rusqlite::Row) -> rusqlite::Result<BusMessage> {
    Ok(BusMessage {
        id: r.get(0)?,
        scope_id: r.get(1)?,
        channel: r.get(2)?,
        from_agent: r.get(3)?,
        from_cid: r.get(4)?,
        to_agent: r.get(5)?,
        body: r.get(6)?,
        created_at: r.get(7)?,
        ttl_ms: r.get(8)?,
    })
}

/// Render pending messages into hook context, respecting a token budget.
pub fn render_inbox(msgs: &[BusMessage], budget_tokens: usize) -> Option<String> {
    if msgs.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("## Inbox — unread messages from other agents\n");
    let mut used = pack::est_tokens(&out);
    let budget = budget_tokens.max(64);
    for m in msgs {
        let to = m
            .to_agent
            .as_deref()
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        let line = format!(
            "- [{}] {}@{} -> {}: {}\n",
            m.channel,
            m.from_agent,
            pack::ymd(m.created_at),
            to.trim_start_matches(" -> "),
            m.body.chars().take(300).collect::<String>()
        );
        // Alternative compact line
        let line2 = if m.to_agent.is_some() {
            format!(
                "- [{}] {} -> {}: {}\n",
                m.channel,
                m.from_agent,
                m.to_agent.as_deref().unwrap_or(""),
                truncate(&m.body, 300)
            )
        } else {
            format!(
                "- [{}] {} (broadcast): {}\n",
                m.channel,
                m.from_agent,
                truncate(&m.body, 300)
            )
        };
        let _ = to; // suppress unused
        let cost = pack::est_tokens(&line2);
        if used + cost > budget {
            break;
        }
        out.push_str(&line2);
        used += cost;
        let _ = line;
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, scope};
    use std::path::Path;

    fn setup() -> (Connection, crate::scope::Scope) {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-bus"), Path::new("/")).unwrap();
        (conn, sc)
    }

    #[test]
    fn send_and_list() {
        let (conn, sc) = setup();
        send(
            &conn,
            sc.id,
            "claude-code",
            None,
            None,
            "general",
            "hello",
            None,
        )
        .unwrap();
        send(
            &conn,
            sc.id,
            "opencode",
            None,
            Some("claude-code"),
            "ops",
            "hi claude",
            None,
        )
        .unwrap();
        let all = list(&conn, sc.id, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        let ops = list(&conn, sc.id, Some("ops"), 10).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].body, "hi claude");
    }

    #[test]
    fn poll_delivers_broadcast_and_directed() {
        let (conn, sc) = setup();
        send(
            &conn,
            sc.id,
            "claude-code",
            None,
            None,
            "general",
            "broadcast",
            None,
        )
        .unwrap();
        send(
            &conn,
            sc.id,
            "claude-code",
            None,
            Some("opencode"),
            "general",
            "private",
            None,
        )
        .unwrap();
        // opencode sees both
        let msgs = poll(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        // second poll returns 0 (cursor advanced)
        let again = poll(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(again.len(), 0);
        // other agent sees only broadcast
        let other = poll(&conn, sc.id, "codex", None, 10).unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].body, "broadcast");
    }

    #[test]
    fn ttl_expires() {
        let (conn, sc) = setup();
        send(
            &conn,
            sc.id,
            "a",
            None,
            None,
            "general",
            "short lived",
            Some(1),
        )
        .unwrap();
        // artificially age it
        conn.execute(
            "UPDATE bus_messages SET created_at = created_at - 10000 WHERE scope_id = ?1",
            [sc.id],
        )
        .unwrap();
        let msgs = poll(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(msgs.len(), 0, "expired message should not be delivered");
    }

    #[test]
    fn channel_normalization() {
        assert_eq!(normalize_channel("  "), "general");
        assert_eq!(normalize_channel("OPS"), "ops");
        assert_eq!(normalize_channel("my channel!"), "my-channel");
    }

    #[test]
    fn pending_does_not_advance_cursor() {
        let (conn, sc) = setup();
        send(&conn, sc.id, "a", None, None, "general", "msg1", None).unwrap();
        let p1 = pending(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(p1.len(), 1);
        let p2 = pending(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(p2.len(), 1, "pending should not advance");
        let polled = poll(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(polled.len(), 1);
        let p3 = pending(&conn, sc.id, "opencode", None, 10).unwrap();
        assert_eq!(p3.len(), 0);
    }

    #[test]
    fn send_rejects_empty_and_too_long() {
        let (conn, sc) = setup();
        assert!(send(&conn, sc.id, "a", None, None, "general", "", None).is_err());
        assert!(send(&conn, sc.id, "a", None, None, "general", "   ", None).is_err());
        assert!(send(&conn, sc.id, "", None, None, "general", "hi", None).is_err());
        let long = "x".repeat(4001);
        assert!(send(&conn, sc.id, "a", None, None, "general", &long, None).is_err());
        // exactly 4000 is ok
        let ok = "y".repeat(4000);
        assert!(send(&conn, sc.id, "a", None, None, "general", &ok, None).is_ok());
    }

    #[test]
    fn channel_normalization_exhaustive() {
        assert_eq!(normalize_channel(""), "general");
        assert_eq!(normalize_channel("---"), "general");
        assert_eq!(normalize_channel("a--b"), "a-b");
        assert_eq!(normalize_channel(" My Channel 123!@# "), "my-channel-123");
        assert_eq!(normalize_channel("OPS"), "ops");
        assert_eq!(normalize_channel("a/b"), "a-b");
        assert_eq!(normalize_channel(&"a".repeat(100)), "a".repeat(32));
    }

    #[test]
    fn list_limits_and_ordering() {
        let (conn, sc) = setup();
        for i in 0..5 {
            send(
                &conn,
                sc.id,
                "a",
                None,
                None,
                "general",
                &format!("msg {i}"),
                None,
            )
            .unwrap();
        }
        let all = list(&conn, sc.id, None, 10).unwrap();
        assert_eq!(all.len(), 5);
        // list is newest first
        assert_eq!(all[0].body, "msg 4");
        assert_eq!(all[4].body, "msg 0");
        let limited = list(&conn, sc.id, None, 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].body, "msg 4");
    }

    #[test]
    fn poll_channel_filtering() {
        let (conn, sc) = setup();
        send(&conn, sc.id, "a", None, None, "general", "gen", None).unwrap();
        send(&conn, sc.id, "a", None, None, "ops", "opsmsg", None).unwrap();
        let gen = poll(&conn, sc.id, "x", Some("general"), 10).unwrap();
        assert_eq!(gen.len(), 1);
        assert_eq!(gen[0].channel, "general");
        // ops message still pending for this agent because cursor advanced only to gen's id?
        // Actually cursor advanced to max id returned (which is gen's id if ops was later id, but we filtered)
        // So ops message with higher id is still > cursor? Let's check: gen has id 1, ops has id 2
        // poll with channel=general returns only id 1, cursor goes to 1, ops id 2 still >1 and matches ops filter
        let ops = poll(&conn, sc.id, "x", Some("ops"), 10).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].channel, "ops");
    }

    #[test]
    fn ttl_zero_expires_immediately() {
        let (conn, sc) = setup();
        send(
            &conn,
            sc.id,
            "a",
            None,
            None,
            "general",
            "expire now",
            Some(0),
        )
        .unwrap();
        let msgs = poll(&conn, sc.id, "opencode", None, 10).unwrap();
        // ttl 0 means created_at + 0 <= now, so expired by the time we poll (now has advanced by 1ms due to now() monotonic)
        assert_eq!(msgs.len(), 0);
    }

    #[test]
    fn scope_isolation() {
        let conn = db::open_memory().unwrap();
        let sc1 = scope::resolve(&conn, Some("/tmp/scope1"), Path::new("/")).unwrap();
        let sc2 = scope::resolve(&conn, Some("/tmp/scope2"), Path::new("/")).unwrap();
        send(&conn, sc1.id, "a", None, None, "general", "for sc1", None).unwrap();
        let m1 = poll(&conn, sc1.id, "x", None, 10).unwrap();
        assert_eq!(m1.len(), 1);
        let m2 = poll(&conn, sc2.id, "x", None, 10).unwrap();
        assert_eq!(m2.len(), 0);
        let list2 = list(&conn, sc2.id, None, 10).unwrap();
        assert_eq!(list2.len(), 0);
    }

    #[test]
    fn concurrent_sends_do_not_corrupt() {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/concurrent-bus"), Path::new("/")).unwrap();
        // Use a file-backed DB for real concurrency
        let dir = std::env::temp_dir().join(format!(
            "fm-bus-conc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus.db");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let c = crate::db::open(&path).unwrap();
                    let sc =
                        scope::resolve(&c, Some("/tmp/concurrent-bus"), Path::new("/")).unwrap();
                    for j in 0..5 {
                        send(
                            &c,
                            sc.id,
                            "agent",
                            None,
                            None,
                            "general",
                            &format!("msg {i}-{j}"),
                            None,
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let c = crate::db::open(&path).unwrap();
        let sc = scope::resolve(&c, Some("/tmp/concurrent-bus"), Path::new("/")).unwrap();
        let all = list(&c, sc.id, None, 100).unwrap();
        assert_eq!(all.len(), 40);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_inbox_respects_budget() {
        let (conn, sc) = setup();
        for i in 0..10 {
            send(
                &conn,
                sc.id,
                "a",
                None,
                None,
                "general",
                &format!("message number {}", i),
                None,
            )
            .unwrap();
        }
        let msgs = pending(&conn, sc.id, "x", None, 10).unwrap();
        assert_eq!(msgs.len(), 10);
        // Tiny budget should truncate
        let rendered = render_inbox(&msgs, 20).unwrap();
        // Should contain header but not all 10 messages
        assert!(rendered.contains("Inbox"));
        let lines = rendered.lines().filter(|l| l.starts_with("- [")).count();
        assert!(lines < 10, "budget should have truncated: {rendered}");
        // Large budget contains all
        let rendered2 = render_inbox(&msgs, 10000).unwrap();
        assert_eq!(
            rendered2.lines().filter(|l| l.starts_with("- [")).count(),
            10
        );
        // Empty returns None
        assert!(render_inbox(&[], 100).is_none());
    }

    #[test]
    fn poll_with_limit() {
        let (conn, sc) = setup();
        for i in 0..10 {
            send(
                &conn,
                sc.id,
                "a",
                None,
                None,
                "general",
                &format!("m{i}"),
                None,
            )
            .unwrap();
        }
        let first = poll(&conn, sc.id, "x", None, 3).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].body, "m0");
        let second = poll(&conn, sc.id, "x", None, 3).unwrap();
        assert_eq!(second.len(), 3);
        assert_eq!(second[0].body, "m3");
        let third = poll(&conn, sc.id, "x", None, 10).unwrap();
        assert_eq!(third.len(), 4);
    }
}
