//! Work sessions: the recent activity of a project, grouped across agents.
//!
//! An agent conversation is identified by the agent's own `session_id`. A *work
//! session* is the container that conversation writes into — and, crucially,
//! the container a *different* agent's conversation can keep writing into the
//! next day. That is the whole point: you work in Claude Code, the machine
//! reboots, and the first prompt of a fresh OpenCode conversation in the same
//! project is handed the accumulated context of the work (the "handoff").
//!
//! Sessions are keyed per scope, named (`work-2026-08-26`, or whatever you give
//! `session start <name>`), created automatically from the first prompt, and
//! idle-closed: once a session has been quiet longer than the idle window, a new
//! prompt starts a new one. No command is required for any of this — the CLI
//! exists for the times you want a name, a look back, or an explicit close.
//!
//! Everything here is model-free, like the rest of the write path: the session
//! title is the goal you set, the content is the episodes and facts already
//! stored, and the "context" handed to a resuming agent is a bounded render of
//! exactly that.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::config::{now, DAY};
use crate::graph::{self, FactRow};
use crate::pack;
use crate::scope::Scope;
use crate::task;

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: i64,
    pub scope_id: i64,
    pub name: String,
    pub agent: String,
    pub goal: Option<String>,
    /// The `agent:conversation` keys that have written into this session. A
    /// conversation already in the list just continues; a new one is adopted
    /// and becomes a handoff.
    pub members: Vec<String>,
    pub status: String,
    pub episode_count: i64,
    pub first_at: i64,
    pub last_at: i64,
    pub closed_at: Option<i64>,
}

impl Session {
    /// Whether the session is still receiving work as of `now_ts`. Idle beyond
    /// the window counts as closed even when nobody ran `session end`.
    pub fn is_open(&self, now_ts: i64, idle_ms: i64) -> bool {
        self.status == "open" && self.closed_at.is_none() && now_ts - self.last_at <= idle_ms
    }

    /// The distinct agent names that have written here, in first-seen order.
    pub fn agents(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in &self.members {
            let agent = m.split(':').next().unwrap_or("");
            if !agent.is_empty() && !out.contains(&agent) {
                out.push(agent);
            }
        }
        out
    }
}

const COLS: &str = "id, scope_id, name, agent, goal, status, members, episode_count, \
                    first_at, last_at, closed_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        scope_id: r.get(1)?,
        name: r.get(2)?,
        agent: r.get(3)?,
        goal: r.get(4)?,
        status: r.get(5)?,
        members: r
            .get::<_, Option<String>>(6)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        episode_count: r.get(7)?,
        first_at: r.get(8)?,
        last_at: r.get(9)?,
        closed_at: r.get(10)?,
    })
}

/// Stable identity of one agent conversation, or `None` when the agent gave us
/// no session id (some hooks never do). Conversations without an id still tag
/// their episodes to the current session, but are not tracked as members and can
/// never trigger a handoff — otherwise every prompt of a session-id-less agent
/// would look like a brand-new conversation.
pub fn conversation_key(agent: &str, session_id: &str) -> Option<String> {
    let sid = session_id.trim();
    if sid.is_empty() {
        None
    } else {
        Some(format!("{agent}:{sid}"))
    }
}

fn idle_ms(hours: usize) -> i64 {
    DAY.saturating_mul(hours.min(24 * 30) as i64)
}

/// Resolve which session this prompt belongs to, creating or adopting one.
///
/// Returns `(session, handoff, is_new)`:
/// - `handoff` is true only when a *new* conversation adopted an existing
///   session that already holds work — the moment to inject the accumulated
///   context back into the resuming agent.
/// - `is_new` marks a brand-new session, so a caller can tell "the work just
///   started" apart from "a different agent picked it up".
pub fn ensure(
    conn: &Connection,
    scope: &Scope,
    agent: &str,
    session_id: &str,
    idle_hours: usize,
) -> Result<(Session, bool, bool)> {
    let ts = now();
    let idle = idle_ms(idle_hours);
    let cid = conversation_key(agent, session_id);

    // A conversation already in a session just keeps going — no handoff, ever,
    // however long it runs.
    if let Some(cid) = cid.as_deref() {
        if let Some(s) = session_for_cid(conn, scope.id, cid)? {
            touch(conn, s.id, ts)?;
            return Ok((s, false, false));
        }
    }

    // A new conversation: adopt the scope's current session while it is still
    // warm. This is the cross-agent continuation — Claude yesterday, OpenCode
    // today. It only counts as a handoff when there is something to hand over.
    if let Some(s) = most_recent_open(conn, scope.id, ts, idle)? {
        let handoff = cid.is_some() && s.episode_count > 0;
        if let Some(cid) = cid.as_deref() {
            add_member(conn, s.id, cid)?;
        }
        touch(conn, s.id, ts)?;
        let s = get_by_id(conn, s.id)?
            .ok_or_else(|| anyhow::anyhow!("session vanished after adopt"))?;
        return Ok((s, handoff, false));
    }

    let name = next_auto_name(conn, scope.id)?;
    let s = insert(conn, scope, &name, agent, None, cid.as_deref())?;
    Ok((s, false, true))
}

/// Open a named session (tmux-style), creating or reopening it. A freshly
/// started session is the most recently active one, so the next prompt in the
/// scope flows into it.
pub fn start(conn: &Connection, scope: &Scope, name: &str, goal: Option<&str>) -> Result<Session> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "session name cannot be empty");
    anyhow::ensure!(
        name.chars().count() <= 64,
        "session name must be 64 characters or fewer"
    );
    let ts = now();
    if let Some(existing) = get_by_name(conn, scope.id, name)? {
        conn.execute(
            "UPDATE sessions SET status = 'open', closed_at = NULL, last_at = ?2,
             goal = COALESCE(?3, goal)
             WHERE id = ?1",
            params![existing.id, ts, goal],
        )?;
        return get_by_id(conn, existing.id)?
            .ok_or_else(|| anyhow::anyhow!("session vanished after update"));
    }
    insert(conn, scope, name, "", goal, None)
}

/// Close a session explicitly. It stays readable via `session show` and the
/// timeline of the store; only new prompts stop flowing into it.
pub fn close(conn: &Connection, id: i64) -> Result<Session> {
    conn.execute(
        "UPDATE sessions SET status = 'closed', closed_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    get_by_id(conn, id)?.ok_or_else(|| anyhow::anyhow!("session {id} not found"))
}

/// Mark that a session was seen right now. Runs on every prompt (and on
/// session-end), so the idle clock starts from the last real activity.
pub fn touch(conn: &Connection, id: i64, ts: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET last_at = ?2 WHERE id = ?1",
        params![id, ts],
    )?;
    Ok(())
}

/// Count one stored (non-duplicate) episode into the session.
pub fn bump_count(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET episode_count = episode_count + 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

fn insert(
    conn: &Connection,
    scope: &Scope,
    name: &str,
    agent: &str,
    goal: Option<&str>,
    cid: Option<&str>,
) -> Result<Session> {
    let ts = now();
    let members = match cid {
        Some(cid) => format!("[\"{cid}\"]"),
        None => "[]".to_string(),
    };
    conn.execute(
        "INSERT INTO sessions(scope_id, name, agent, goal, status, members,
                              episode_count, first_at, last_at)
         VALUES(?1, ?2, ?3, ?4, 'open', ?5, 0, ?6, ?6)",
        params![scope.id, name, agent, goal, members, ts],
    )?;
    let id = conn.last_insert_rowid();
    get_by_id(conn, id)?.ok_or_else(|| anyhow::anyhow!("session {name:?} vanished after insert"))
}

/// Auto-generated name for the current day, disambiguated on collision:
/// `work-2026-08-26`, then `work-2026-08-26-2`, and so on.
fn next_auto_name(conn: &Connection, scope_id: i64) -> Result<String> {
    let base = format!("work-{}", pack::ymd(now()));
    let mut st = conn.prepare(
        "SELECT count(*) FROM sessions WHERE scope_id = ?1 AND (name = ?2 OR name LIKE ?3)",
    )?;
    let n: i64 = st.query_row(params![scope_id, base, format!("{base}-%")], |r| r.get(0))?;
    Ok(if n == 0 {
        base
    } else {
        format!("{base}-{}", n + 1)
    })
}

fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Session>> {
    let sql = format!("SELECT {COLS} FROM sessions WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], row).optional()?)
}

pub fn get_by_name(conn: &Connection, scope_id: i64, name: &str) -> Result<Option<Session>> {
    let sql = format!("SELECT {COLS} FROM sessions WHERE scope_id = ?1 AND name = ?2");
    Ok(conn
        .query_row(&sql, params![scope_id, name], row)
        .optional()?)
}

/// The open session a conversation currently belongs to, if any.
pub fn session_for_cid(conn: &Connection, scope_id: i64, cid: &str) -> Result<Option<Session>> {
    let sql = format!(
        "SELECT {COLS} FROM sessions
         WHERE scope_id = ?1 AND status = 'open' AND closed_at IS NULL
           AND EXISTS (SELECT 1 FROM json_each(sessions.members) WHERE json_each.value = ?2)
         ORDER BY last_at DESC LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, params![scope_id, cid], row)
        .optional()?)
}

/// The most recent open session in the scope, provided it is still within the
/// idle window.
fn most_recent_open(
    conn: &Connection,
    scope_id: i64,
    ts: i64,
    idle: i64,
) -> Result<Option<Session>> {
    let sql = format!(
        "SELECT {COLS} FROM sessions
         WHERE scope_id = ?1 AND status = 'open' AND closed_at IS NULL AND last_at >= ?2
         ORDER BY last_at DESC LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, params![scope_id, ts - idle], row)
        .optional()?)
}

fn add_member(conn: &Connection, id: i64, cid: &str) -> Result<()> {
    let s = get_by_id(conn, id)?;
    if let Some(s) = s {
        if s.members.iter().any(|m| m == cid) {
            return Ok(());
        }
        let mut members = s.members;
        members.push(cid.to_string());
        conn.execute(
            "UPDATE sessions SET members = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&members)?],
        )?;
    }
    Ok(())
}

/// All sessions in the scope, newest first.
pub fn list(conn: &Connection, scope_id: i64) -> Result<Vec<Session>> {
    let sql = format!("SELECT {COLS} FROM sessions WHERE scope_id = ?1 ORDER BY last_at DESC");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([scope_id], row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A snippet of one episode, for a session render. Bodies are verbatim prompt
/// text and can be long, so renders clip them.
#[derive(Debug)]
pub struct EpisodeBrief {
    pub kind: String,
    pub body: String,
    pub recorded_at: i64,
}

pub fn episodes(conn: &Connection, session_id: i64, limit: usize) -> Result<Vec<EpisodeBrief>> {
    let mut st = conn.prepare(
        "SELECT kind, body, recorded_at FROM episodes
         WHERE session_id = ?1 ORDER BY recorded_at DESC LIMIT ?2",
    )?;
    let rows = st.query_map(params![session_id, limit as i64], |r| {
        Ok(EpisodeBrief {
            kind: r.get(0)?,
            body: r.get(1)?,
            recorded_at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Live facts learned from episodes of this session.
pub fn facts(
    conn: &Connection,
    scope_id: i64,
    session_id: i64,
    limit: usize,
) -> Result<Vec<FactRow>> {
    let mut st = conn.prepare(
        "SELECT f.id FROM facts f
         WHERE f.scope_id = ?1 AND f.invalidated_at IS NULL
           AND f.episode_id IN (SELECT id FROM episodes WHERE session_id = ?2)
         ORDER BY f.recorded_at DESC LIMIT ?3",
    )?;
    let ids: Vec<i64> = st
        .query_map(params![scope_id, session_id, limit as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    graph::fact_rows(conn, &ids)
}

/// Full render for `session show` — the whole narrative of the work, including
/// the active task checkpoint. Meant for a human or a fresh agent that is about
/// to continue.
pub fn render_show(
    conn: &Connection,
    s: &Session,
    idle_hours: usize,
    scope_label: &str,
) -> Result<String> {
    let ts = now();
    let idle = idle_ms(idle_hours);
    let state = if s.is_open(ts, idle) {
        "open"
    } else if s.closed_at.is_some() {
        "closed"
    } else {
        "idle"
    };
    let mut out = String::new();
    out.push_str(&format!("# Session: {} ({})\n\n", s.name, scope_label));
    out.push_str(&format!(
        "{state} · opened {} · last activity {} · {} episode(s)\n",
        pack::ymd(s.first_at),
        pack::ymd(s.last_at),
        s.episode_count
    ));
    let agents = s.agents();
    if !agents.is_empty() {
        out.push_str(&format!("agents: {}\n", agents.join(", ")));
    }
    if let Some(g) = &s.goal {
        out.push_str(&format!("\n**goal:** {g}\n"));
    }
    out.push('\n');

    let eps = episodes(conn, s.id, 40)?;
    if !eps.is_empty() {
        out.push_str("## What happened\n");
        for e in eps {
            out.push_str(&format!(
                "- ({}) [`{}`] {}\n",
                pack::ymd(e.recorded_at),
                e.kind,
                clip(&e.body, 180)
            ));
        }
        out.push('\n');
    }

    let facts = facts(conn, s.scope_id, s.id, 25)?;
    if !facts.is_empty() {
        out.push_str("## Decisions & facts learned here\n");
        for f in &facts {
            let start = f.valid_from.unwrap_or(f.recorded_at);
            let when = if ts - start > 2 * DAY {
                format!(" since {}", pack::ymd(start))
            } else {
                String::new()
            };
            let by = f
                .source
                .as_deref()
                .filter(|s| !s.is_empty() && *s != "unknown")
                .map(|s| format!(" [by {s}]"))
                .unwrap_or_default();
            out.push_str(&format!("- {}{}{}\n", f.statement.trim(), when, by));
        }
        out.push('\n');
    }

    out.push_str("## In-progress task\n");
    match task::current(conn)? {
        Some(cp) => out.push_str(&task::render(&cp)),
        None => out.push_str("none\n"),
    }
    Ok(out)
}

/// Compact render of a session, for the automatic handoff injection on the
/// first prompt of a fresh conversation. Respects a token budget so it can never
/// eat the prompt's context window.
pub fn render_context(
    conn: &Connection,
    s: &Session,
    budget_tokens: usize,
    scope_label: &str,
) -> Result<Option<String>> {
    let ts = now();
    let budget = budget_tokens.max(64);
    let mut out = String::with_capacity(512);
    out.push_str(&format!("## Work context — {} ({})\n", s.name, scope_label));
    let mut used = pack::est_tokens(&out);
    let mut push = |line: &str, used: &mut usize| -> bool {
        let cost = pack::est_tokens(line);
        if *used + cost > budget {
            return false;
        }
        out.push_str(line);
        *used += cost;
        true
    };

    let mut any = false;
    if let Some(g) = &s.goal {
        if push(&format!("goal: {g}\n"), &mut used) {
            any = true;
        }
    }
    let agents = s.agents();
    if !agents.is_empty() {
        let line = format!("agents: {}\n", agents.join(", "));
        if push(&line, &mut used) {
            any = true;
        }
    }

    for f in facts(conn, s.scope_id, s.id, 10)? {
        let start = f.valid_from.unwrap_or(f.recorded_at);
        let when = if ts - start > 2 * DAY {
            format!(" since {}", pack::ymd(start))
        } else {
            String::new()
        };
        if !push(
            &format!("- {}{}\n", clip(&f.statement, 220), when),
            &mut used,
        ) {
            break;
        }
        any = true;
    }
    for e in episodes(conn, s.id, 8)? {
        if !push(
            &format!("- ({}) {}\n", pack::ymd(e.recorded_at), clip(&e.body, 140)),
            &mut used,
        ) {
            break;
        }
        any = true;
    }

    if let Some(cp) = task::current(conn)? {
        let state = clip(&cp.state, 400);
        let block = if let Some(g) = &cp.goal {
            format!("in-progress task — goal: {g}\n{state}\n")
        } else {
            format!("in-progress task:\n{state}\n")
        };
        if push(&block, &mut used) {
            any = true;
        }
    }

    if !any {
        return Ok(None);
    }
    Ok(Some(out))
}

fn clip(s: &str, max: usize) -> String {
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

    fn setup() -> (Connection, Scope) {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-sess"), Path::new("/")).unwrap();
        (conn, sc)
    }

    /// Age a session out of the idle window, so "a different day" is testable
    /// without waiting on the wall clock.
    fn age(conn: &Connection, id: i64, ms: i64) {
        conn.execute(
            "UPDATE sessions SET last_at = last_at - ?1, first_at = first_at - ?1 WHERE id = ?2",
            params![ms, id],
        )
        .unwrap();
    }

    #[test]
    fn first_prompt_creates_a_session() {
        let (conn, sc) = setup();
        let (s, handoff, is_new) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        assert!(is_new);
        assert!(!handoff);
        assert!(s.name.starts_with("work-"));
        assert_eq!(s.episode_count, 0);
        assert_eq!(s.agents(), vec!["claude-code"]);
    }

    #[test]
    fn same_conversation_continues_without_handoff() {
        let (conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        let (s2, handoff, is_new) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        assert_eq!(s.id, s2.id);
        assert!(!handoff);
        assert!(!is_new);
    }

    #[test]
    fn new_conversation_within_idle_adopts_and_hands_off() {
        let (conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        bump_count(&conn, s.id).unwrap();

        let (s2, handoff, is_new) = ensure(&conn, &sc, "opencode", "xyz", 24).unwrap();
        assert_eq!(s.id, s2.id, "adopted the same session across agents");
        assert!(handoff, "a new conversation in a warm session is a handoff");
        assert!(!is_new);
        assert_eq!(s2.agents().len(), 2, "both agents tracked");
    }

    #[test]
    fn idle_session_is_not_adopted() {
        let (conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        bump_count(&conn, s.id).unwrap();
        age(&conn, s.id, 30 * DAY);

        let (s2, handoff, is_new) = ensure(&conn, &sc, "opencode", "xyz", 24).unwrap();
        assert_ne!(s.id, s2.id, "cold session must not be adopted");
        assert!(!handoff);
        assert!(is_new);
    }

    #[test]
    fn session_without_content_never_hands_off() {
        let (conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        // No bump_count: nothing was ever stored, so there is nothing to hand off.
        let (_, handoff, _) = ensure(&conn, &sc, "opencode", "xyz", 24).unwrap();
        assert!(!handoff);
        assert_eq!(s.episode_count, 0);
    }

    #[test]
    fn session_id_less_prompts_tag_but_never_hand_off() {
        let (conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        bump_count(&conn, s.id).unwrap();
        assert_eq!(conversation_key("opencode", ""), None);

        // A session-id-less agent is not a new conversation: no member is added
        // and no handoff fires, or every prompt would inject the session.
        let (s2, handoff, _) = ensure(&conn, &sc, "opencode", "", 24).unwrap();
        assert_eq!(s.id, s2.id);
        assert!(!handoff);
        assert_eq!(s2.agents().len(), 1, "anon agent is not tracked");
    }

    #[test]
    fn auto_names_do_not_collide() {
        let (conn, sc) = setup();
        let a = next_auto_name(&conn, sc.id).unwrap();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        assert_eq!(a, s.name);
        age(&conn, s.id, 30 * DAY);
        let b = next_auto_name(&conn, sc.id).unwrap();
        assert_eq!(b, format!("{a}-2"));
    }

    #[test]
    fn start_reopens_and_lists() {
        let (conn, sc) = setup();
        let s = start(&conn, &sc, "release", Some("ship 1.3")).unwrap();
        assert_eq!(s.name, "release");
        assert_eq!(s.goal.as_deref(), Some("ship 1.3"));
        let closed = close(&conn, s.id).unwrap();
        assert_eq!(closed.status, "closed");

        let reopened = start(&conn, &sc, "release", None).unwrap();
        assert_eq!(reopened.status, "open");
        assert_eq!(
            reopened.goal.as_deref(),
            Some("ship 1.3"),
            "reopen keeps the original goal"
        );

        let all = list(&conn, sc.id).unwrap();
        assert_eq!(all.len(), 1, "start on the same name must not duplicate");
    }

    #[test]
    fn named_sessions_are_scoped_per_project() {
        let (conn, sc) = setup();
        start(&conn, &sc, "release", None).unwrap();
        let other = scope::resolve(&conn, Some("/tmp/fm-other"), Path::new("/")).unwrap();
        assert!(
            get_by_name(&conn, other.id, "release").unwrap().is_none(),
            "names must not leak across scopes"
        );
    }

    #[test]
    fn context_renders_facts_episodes_and_task() {
        let (mut conn, sc) = setup();
        let (s, _, _) = ensure(&conn, &sc, "claude-code", "abc", 24).unwrap();
        crate::store::remember(
            &mut conn,
            &sc,
            None,
            &crate::store::RememberInput {
                text: "we deploy through fly.io".into(),
                kind: "decision".into(),
                source: "autosave:claude-code".into(),
                facts: vec![],
                files: vec![],
                meta: None,
                derive: true,
                session_id: Some(s.id),
            },
        )
        .unwrap();
        bump_count(&conn, s.id).unwrap();

        let ctx = render_context(&conn, &s, 300, "proj").unwrap().unwrap();
        assert!(ctx.contains("Work context"), "got {ctx}");
        assert!(ctx.contains("fly.io"), "got {ctx}");

        let show = render_show(&conn, &s, 24, "proj").unwrap();
        assert!(show.contains("Session:"), "got {show}");
        assert!(show.contains("fly.io"), "got {show}");
    }
}
