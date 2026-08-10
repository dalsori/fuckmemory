//! The write path.
//!
//! No LLM is called here. That is the whole point: Mem0 and Zep spend hundreds
//! of milliseconds to seconds per write asking a model to extract entities, and
//! the agent calling us *is already a model*. So the tool schema invites the
//! agent to hand over structured facts, and when it doesn't we fall back to
//! cheap lexical extraction. A write is an INSERT plus a static embedding.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::now;
use crate::db::{self, VEC_EPISODE, VEC_FACT};
use crate::embed::{self, Embedder};
use crate::scope::Scope;

/// Relations where a subject can only hold one value at a time, so a new value
/// invalidates the old one instead of piling up next to it.
const SINGLE_VALUED: &[&str] = &[
    "uses",
    "prefers",
    "is",
    "has_status",
    "runs_on",
    "lives_in",
    "assigned_to",
    "version",
    "named",
    "defaults_to",
    "targets",
];

#[derive(Debug, Clone, Deserialize)]
pub struct FactInput {
    /// Subject. Free text; resolved to an entity node.
    #[serde(default)]
    pub src: Option<String>,
    #[serde(default = "default_rel")]
    pub rel: String,
    #[serde(default)]
    pub dst: Option<String>,
    /// The sentence that gets injected into a future prompt. Keep it standalone:
    /// it will be read without surrounding context.
    pub statement: String,
    #[serde(default)]
    pub valid_from: Option<i64>,
    #[serde(default)]
    pub valid_to: Option<i64>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Force (or forbid) invalidating prior values of the same `src`+`rel`.
    #[serde(default)]
    pub supersede: Option<bool>,
}

fn default_rel() -> String {
    "relates_to".into()
}
fn default_confidence() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct RememberInput {
    pub text: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub facts: Vec<FactInput>,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
    /// Files the memory was learned against. Each carries a bounded snippet so a
    /// recall can show *where* a convention lives.
    #[serde(default)]
    pub files: Vec<FileInput>,
    /// Guess a fact out of the text when the caller supplied none.
    ///
    /// Autosave sets this to false for anything that doesn't look like durable
    /// knowledge: the prompt is still kept verbatim as a searchable episode, but
    /// it doesn't get to pose as a fact. Without that distinction, "fix the typo
    /// in main.rs" would be recalled next month with the same weight as "we
    /// deploy through fly.io".
    #[serde(default = "default_true")]
    pub derive: bool,
}

/// A file attached to a memory. `snippet` is written by the caller (usually the
/// hook, reading the referenced lines); it is what recall can show.
#[derive(Debug, Clone, Deserialize)]
pub struct FileInput {
    /// Path as the agent knew it, e.g. `src/db.rs` or `/abs/path`.
    pub path: String,
    /// Detected language for `path`, if any.
    #[serde(default)]
    pub lang: Option<String>,
    /// Bounded excerpt of the file. Required: the caller reads the file.
    pub snippet: String,
    #[serde(default)]
    pub line_from: Option<i64>,
    #[serde(default)]
    pub line_to: Option<i64>,
}

fn default_true() -> bool {
    true
}

fn default_kind() -> String {
    "note".into()
}
fn default_source() -> String {
    "unknown".into()
}

#[derive(Debug, Serialize)]
pub struct RememberOutput {
    pub episode_id: i64,
    pub fact_ids: Vec<i64>,
    pub superseded: Vec<i64>,
    /// True when this exact text was already stored in this scope.
    pub duplicate: bool,
}

/// Normalize a name for entity identity: lowercase, collapse whitespace, drop
/// surrounding punctuation. Deliberately conservative — no stemming, so `pnpm`
/// and `pnpm-workspace` stay distinct nodes.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.trim().chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            last_space = false;
        }
    }
    out.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\''))
        .to_string()
}

fn hash_text(parts: &[&str]) -> Vec<u8> {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(&[0]);
    }
    h.finalize().as_bytes()[..16].to_vec()
}

/// Resolve or create an entity node, following aliases.
pub fn upsert_entity(
    conn: &Connection,
    scope: &Scope,
    name: &str,
    kind: Option<&str>,
) -> Result<i64> {
    let norm = normalize(name);
    if norm.is_empty() {
        anyhow::bail!("empty entity name");
    }
    if let Some(id) = conn
        .query_row(
            "SELECT entity_id FROM aliases WHERE scope_id = ?1 AND norm = ?2",
            params![scope.id, norm],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO entities(scope_id, name, norm, kind, recorded_at) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(scope_id, norm) DO UPDATE SET kind = COALESCE(entities.kind, excluded.kind)",
        params![scope.id, name.trim(), norm, kind, now()],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM entities WHERE scope_id = ?1 AND norm = ?2",
        params![scope.id, norm],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Point an extra spelling at an existing node.
pub fn add_alias(conn: &Connection, scope: &Scope, entity_id: i64, alias: &str) -> Result<()> {
    let norm = normalize(alias);
    if norm.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO aliases(entity_id, scope_id, norm) VALUES(?1, ?2, ?3)
         ON CONFLICT(scope_id, norm) DO UPDATE SET entity_id = excluded.entity_id",
        params![entity_id, scope.id, norm],
    )?;
    Ok(())
}

/// Store an observation. Idempotent per `(scope, text)`.
pub fn remember(
    conn: &mut Connection,
    scope: &Scope,
    emb: Option<&Embedder>,
    input: &RememberInput,
) -> Result<RememberOutput> {
    let ts = now();
    let body = input.text.trim();
    anyhow::ensure!(!body.is_empty(), "nothing to remember: empty text");

    // IMMEDIATE, not the default DEFERRED. A deferred transaction starts as a
    // reader and upgrades on its first write — and when two of them do that at
    // once SQLite returns SQLITE_BUSY *immediately*, ignoring busy_timeout,
    // because waiting cannot break the tie. Several agents writing memories
    // concurrently is the normal case here, so every writing transaction takes
    // the write lock up front and queues politely instead.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let ep_hash = hash_text(&[&normalize(body)]);
    let meta_s = input.meta.as_ref().map(|m| m.to_string());

    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM episodes WHERE scope_id = ?1 AND hash = ?2",
            params![scope.id, ep_hash],
            |r| r.get(0),
        )
        .optional()?;

    let (episode_id, duplicate) = match existing {
        Some(id) => (id, true),
        None => {
            tx.execute(
                "INSERT INTO episodes(scope_id, source, kind, body, meta, hash, recorded_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    scope.id,
                    input.source,
                    input.kind,
                    body,
                    meta_s,
                    ep_hash,
                    ts
                ],
            )?;
            (tx.last_insert_rowid(), false)
        }
    };

    if !duplicate {
        if let Some(e) = emb {
            embed::put_vec(&tx, VEC_EPISODE, episode_id, &e.embed_q(body))?;
        }
        tx.execute(
            "INSERT INTO pending(episode_id, queued_at) VALUES(?1, ?2)",
            params![episode_id, ts],
        )?;

        // Attach the files this memory was learned against. Dedup by path within
        // the episode: the same file named twice only contributes one reference.
        let mut seen: Vec<String> = Vec::new();
        for f in &input.files {
            let path = f.path.trim();
            if path.is_empty() || seen.iter().any(|p| p == path) {
                continue;
            }
            seen.push(path.to_string());
            tx.execute(
                "INSERT INTO file_refs(episode_id, path, lang, snippet, line_from, line_to, recorded_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    episode_id,
                    path,
                    f.lang,
                    f.snippet,
                    f.line_from,
                    f.line_to,
                    ts
                ],
            )?;
        }
    }

    // Facts the agent handed us win; otherwise derive one from the text, unless
    // the caller asked for the episode alone.
    let derived;
    let facts: &[FactInput] = if !input.facts.is_empty() {
        &input.facts
    } else if input.derive {
        derived = vec![derive_fact(body, &input.kind)];
        &derived
    } else {
        &[]
    };

    let mut fact_ids = Vec::new();
    let mut superseded = Vec::new();
    for f in facts {
        let (id, sup) = insert_fact(&tx, scope, emb, f, Some(episode_id), ts)?;
        if let Some(id) = id {
            fact_ids.push(id);
        }
        superseded.extend(sup);
    }

    tx.commit()?;
    Ok(RememberOutput {
        episode_id,
        fact_ids,
        superseded,
        duplicate,
    })
}

/// Insert one fact edge. Returns the new id (None if an identical live fact
/// already existed) plus the ids it invalidated.
pub fn insert_fact(
    conn: &Connection,
    scope: &Scope,
    emb: Option<&Embedder>,
    f: &FactInput,
    episode_id: Option<i64>,
    ts: i64,
) -> Result<(Option<i64>, Vec<i64>)> {
    let statement = f.statement.trim();
    anyhow::ensure!(!statement.is_empty(), "fact with empty statement");

    let src_id = match f.src.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(upsert_entity(conn, scope, s, None)?),
        None => None,
    };
    let dst_id = match f.dst.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(upsert_entity(conn, scope, s, None)?),
        None => None,
    };

    let hash = hash_text(&[
        &f.rel,
        &src_id.map(|i| i.to_string()).unwrap_or_default(),
        &dst_id.map(|i| i.to_string()).unwrap_or_default(),
        &normalize(statement),
    ]);

    // Same statement already live: refresh provenance, don't duplicate.
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM facts WHERE scope_id = ?1 AND hash = ?2 AND invalidated_at IS NULL",
            params![scope.id, hash],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        conn.execute(
            "UPDATE facts SET confidence = MAX(confidence, ?2), recorded_at = ?3 WHERE id = ?1",
            params![id, f.confidence, ts],
        )?;
        return Ok((None, Vec::new()));
    }

    // A newer value of a single-valued relation closes the previous one.
    let supersede = f.supersede.unwrap_or_else(|| {
        src_id.is_some() && SINGLE_VALUED.contains(&f.rel.to_ascii_lowercase().as_str())
    });
    let mut superseded = Vec::new();
    if supersede {
        if let Some(src) = src_id {
            let mut st = conn.prepare_cached(
                "SELECT id FROM facts
                 WHERE scope_id = ?1 AND src = ?2 AND rel = ?3 AND invalidated_at IS NULL",
            )?;
            superseded = st
                .query_map(params![scope.id, src, f.rel], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
        }
    }

    conn.execute(
        "INSERT INTO facts(scope_id, src, dst, rel, statement, confidence, episode_id, hash,
                           valid_from, valid_to, recorded_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            scope.id,
            src_id,
            dst_id,
            f.rel,
            statement,
            f.confidence,
            episode_id,
            hash,
            f.valid_from.unwrap_or(ts),
            f.valid_to,
            ts
        ],
    )?;
    let id = conn.last_insert_rowid();

    for old in &superseded {
        // valid_to is world time: the old fact stopped being true when the new
        // one started. invalidated_at is when we found out.
        conn.execute(
            "UPDATE facts
             SET invalidated_at = ?2, invalidated_by = ?3,
                 valid_to = COALESCE(valid_to, ?4)
             WHERE id = ?1",
            params![old, ts, id, f.valid_from.unwrap_or(ts)],
        )?;
    }

    for e in [src_id, dst_id].into_iter().flatten() {
        conn.execute("UPDATE entities SET degree = degree + 1 WHERE id = ?1", [e])?;
    }
    if let Some(e) = emb {
        embed::put_vec(conn, VEC_FACT, id, &e.embed_q(statement))?;
    }
    Ok((Some(id), superseded))
}

/// A file attached to an episode, as read back for recall.
#[derive(Debug, Clone, Serialize)]
pub struct FileRef {
    pub path: String,
    pub lang: Option<String>,
    pub snippet: String,
    pub line_from: Option<i64>,
    pub line_to: Option<i64>,
}

/// Read the files attached to a set of episodes, one `FileRef` per `(episode,
/// path)`, ordered by episode then recorded line range.
pub fn files_for_episodes(conn: &Connection, episode_ids: &[i64]) -> Result<Vec<FileRef>> {
    if episode_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (0..episode_ids.len()).map(|_| "?".into()).collect();
    let sql = format!(
        "SELECT path, lang, snippet, line_from, line_to
         FROM file_refs WHERE episode_id IN ({})
         ORDER BY episode_id, line_from",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(episode_ids), |r| {
        Ok(FileRef {
            path: r.get(0)?,
            lang: r.get(1)?,
            snippet: r.get(2)?,
            line_from: r.get(3)?,
            line_to: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Build a fact from raw text when the agent gave us no structure.
///
/// The statement is the text itself — never paraphrased, because paraphrasing
/// without a model is how memory systems start lying. Only the graph anchors
/// are guessed.
fn derive_fact(text: &str, kind: &str) -> FactInput {
    let ents = extract_entities(text);
    FactInput {
        src: ents.first().cloned(),
        rel: match kind {
            "preference" => "prefers".into(),
            "decision" => "decided".into(),
            "constraint" => "requires".into(),
            "error" => "failed_with".into(),
            _ => "relates_to".into(),
        },
        dst: ents.get(1).cloned(),
        statement: text.to_string(),
        valid_from: None,
        valid_to: None,
        confidence: 1.0,
        supersede: Some(false),
    }
}

/// Cheap lexical entity candidates: backticked spans, paths, identifiers with
/// internal punctuation, and capitalized words that aren't sentence-initial.
/// Returns at most 6, in order of appearance, deduped.
pub fn extract_entities(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |s: &str, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        let s = s.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '_');
        if s.len() < 2 || s.len() > 64 {
            return;
        }
        let n = normalize(s);
        if n.is_empty() || !seen.insert(n) {
            return;
        }
        out.push(s.to_string());
    };

    // Backticked spans are the strongest signal an agent gives us.
    let mut rest = text;
    while let Some(a) = rest.find('`') {
        let after = &rest[a + 1..];
        match after.find('`') {
            Some(b) => {
                push(&after[..b], &mut out, &mut seen);
                rest = &after[b + 1..];
            }
            None => break,
        }
    }

    for (i, tok) in text.split_whitespace().enumerate() {
        let clean =
            tok.trim_matches(|c: char| matches!(c, '(' | ')' | '"' | '\'' | ',' | '.' | ';' | ':'));
        if clean.len() < 2 {
            continue;
        }
        let looks_pathy = clean.contains('/') || clean.contains("::") || clean.contains('_');
        let looks_named = clean
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && i > 0;
        let looks_dotted = clean.contains('.')
            && !clean.ends_with('.')
            && clean.chars().any(|c| c.is_alphabetic());
        if looks_pathy || looks_named || looks_dotted {
            push(clean, &mut out, &mut seen);
        }
        if out.len() >= 6 {
            break;
        }
    }
    out.truncate(6);
    out
}

/// Soft-forget by id: the fact stops being live but stays queryable in a timeline.
pub fn invalidate(conn: &Connection, scope: &Scope, fact_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE facts SET invalidated_at = ?3, valid_to = COALESCE(valid_to, ?3)
         WHERE id = ?1 AND scope_id = ?2 AND invalidated_at IS NULL",
        params![fact_id, scope.id, now()],
    )?;
    Ok(n > 0)
}

/// Hard delete: fact, its FTS row and its vector really go away. For secrets
/// pasted by mistake.
pub fn purge(conn: &Connection, scope: &Scope, fact_id: i64) -> Result<bool> {
    let tx_n = conn.execute(
        "DELETE FROM facts WHERE id = ?1 AND scope_id = ?2",
        params![fact_id, scope.id],
    )?;
    conn.execute(
        "DELETE FROM vecs WHERE kind = ?1 AND ref_id = ?2",
        params![VEC_FACT, fact_id],
    )?;
    Ok(tx_n > 0)
}

/// Hard delete an episode and everything derived from it.
pub fn purge_episode(conn: &Connection, scope: &Scope, episode_id: i64) -> Result<usize> {
    let mut st = conn.prepare("SELECT id FROM facts WHERE episode_id = ?1 AND scope_id = ?2")?;
    let ids: Vec<i64> = st
        .query_map(params![episode_id, scope.id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for id in &ids {
        purge(conn, scope, *id)?;
    }
    conn.execute(
        "DELETE FROM vecs WHERE kind = ?1 AND ref_id = ?2",
        params![VEC_EPISODE, episode_id],
    )?;
    let n = conn.execute(
        "DELETE FROM episodes WHERE id = ?1 AND scope_id = ?2",
        params![episode_id, scope.id],
    )?;
    Ok(n + ids.len())
}

/// Record that a fact was actually used, which feeds the popularity term in
/// ranking. Cheap enough to call on every recall.
pub fn mark_hits(conn: &Connection, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let ts = now();
    let mut st =
        conn.prepare_cached("UPDATE facts SET hits = hits + 1, last_hit = ?2 WHERE id = ?1")?;
    for id in ids {
        st.execute(params![id, ts])?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub scopes: i64,
    pub episodes: i64,
    pub facts_live: i64,
    pub facts_invalid: i64,
    pub entities: i64,
    pub vectors: i64,
    pub pending: i64,
    pub db_bytes: i64,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(Stats {
        scopes: one("SELECT count(*) FROM scopes")?,
        episodes: one("SELECT count(*) FROM episodes")?,
        facts_live: one("SELECT count(*) FROM facts WHERE invalidated_at IS NULL")?,
        facts_invalid: one("SELECT count(*) FROM facts WHERE invalidated_at IS NOT NULL")?,
        entities: one("SELECT count(*) FROM entities")?,
        vectors: one("SELECT count(*) FROM vecs")?,
        pending: one("SELECT count(*) FROM pending")?,
        db_bytes: one("SELECT page_count * page_size FROM pragma_page_count, pragma_page_size")
            .unwrap_or(0),
    })
}

/// Everything in a scope, for `export`.
pub fn export_scope(conn: &Connection, scope: &Scope) -> Result<serde_json::Value> {
    let mut st = conn.prepare(
        "SELECT id, source, kind, body, recorded_at FROM episodes WHERE scope_id = ?1 ORDER BY id",
    )?;
    let episodes: Vec<serde_json::Value> = st
        .query_map([scope.id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "source": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "text": r.get::<_, String>(3)?,
                "recorded_at": r.get::<_, i64>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut st = conn.prepare(
        "SELECT f.id, se.name, f.rel, de.name, f.statement, f.confidence,
                f.valid_from, f.valid_to, f.recorded_at, f.invalidated_at
         FROM facts f
         LEFT JOIN entities se ON se.id = f.src
         LEFT JOIN entities de ON de.id = f.dst
         WHERE f.scope_id = ?1 ORDER BY f.id",
    )?;
    let facts: Vec<serde_json::Value> = st
        .query_map([scope.id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "src": r.get::<_, Option<String>>(1)?,
                "rel": r.get::<_, String>(2)?,
                "dst": r.get::<_, Option<String>>(3)?,
                "statement": r.get::<_, String>(4)?,
                "confidence": r.get::<_, f64>(5)?,
                "valid_from": r.get::<_, Option<i64>>(6)?,
                "valid_to": r.get::<_, Option<i64>>(7)?,
                "recorded_at": r.get::<_, i64>(8)?,
                "invalidated_at": r.get::<_, Option<i64>>(9)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(serde_json::json!({
        "version": db::SCHEMA_VERSION,
        "scope": { "key": scope.key, "label": scope.label },
        "episodes": episodes,
        "facts": facts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, scope};
    use std::path::Path;

    fn setup() -> (Connection, Scope) {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-test-proj"), Path::new("/")).unwrap();
        let _ = &mut conn;
        (conn, sc)
    }

    fn note(text: &str) -> RememberInput {
        RememberInput {
            text: text.into(),
            kind: "note".into(),
            source: "test".into(),
            facts: vec![],
            files: vec![],
            meta: None,
            derive: true,
        }
    }

    #[test]
    fn remember_is_idempotent() {
        let (mut conn, sc) = setup();
        let a = remember(&mut conn, &sc, None, &note("build with just, not make")).unwrap();
        let b = remember(&mut conn, &sc, None, &note("Build with just, not make.")).unwrap();
        assert!(!a.duplicate);
        assert!(
            b.duplicate,
            "normalization should collapse punctuation/case"
        );
        assert_eq!(a.episode_id, b.episode_id);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM episodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn empty_text_is_rejected() {
        let (mut conn, sc) = setup();
        assert!(remember(&mut conn, &sc, None, &note("   ")).is_err());
    }

    #[test]
    fn files_are_attached_and_read_back() {
        let (mut conn, sc) = setup();
        let mut input = note("the deploy script lives in the Makefile");
        input.files = vec![FileInput {
            path: "Makefile".into(),
            lang: Some("make".into()),
            snippet: "deploy:\n\tfly deploy\n".into(),
            line_from: Some(1),
            line_to: Some(2),
        }];
        let out = remember(&mut conn, &sc, None, &input).unwrap();

        let files = files_for_episodes(&conn, &[out.episode_id]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "Makefile");
        assert_eq!(files[0].lang.as_deref(), Some("make"));
        assert_eq!(files[0].line_from, Some(1));
        assert!(files[0].snippet.contains("fly deploy"));

        // Duplicate path within one episode collapses to a single reference.
        input.files.push(FileInput {
            path: "Makefile".into(),
            lang: None,
            snippet: "deploy:\n\tfly deploy\n".into(),
            line_from: None,
            line_to: None,
        });
        let again = remember(&mut conn, &sc, None, &input).unwrap();
        // Same text → same episode → nothing new appended.
        assert!(again.duplicate);
        let files = files_for_episodes(&conn, &[again.episode_id]).unwrap();
        assert_eq!(files.len(), 1, "path dedup within an episode");
    }

    #[test]
    fn single_valued_relation_supersedes_and_keeps_history() {
        let (mut conn, sc) = setup();
        let f = |dst: &str| RememberInput {
            text: format!("the project uses {dst}"),
            kind: "decision".into(),
            source: "test".into(),
            facts: vec![FactInput {
                src: Some("project".into()),
                rel: "uses".into(),
                dst: Some(dst.into()),
                statement: format!("the project uses {dst}"),
                valid_from: None,
                valid_to: None,
                confidence: 1.0,
                supersede: None,
            }],
            files: vec![],
            meta: None,
            derive: true,
        };
        remember(&mut conn, &sc, None, &f("npm")).unwrap();
        let out = remember(&mut conn, &sc, None, &f("pnpm")).unwrap();
        assert_eq!(out.superseded.len(), 1, "npm fact should be closed out");

        let live: Vec<String> = conn
            .prepare("SELECT statement FROM facts WHERE invalidated_at IS NULL AND rel = 'uses'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(live, vec!["the project uses pnpm"]);

        // History survives, with a closed validity window.
        let (vt, ib): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT valid_to, invalidated_by FROM facts WHERE statement LIKE '%npm' AND invalidated_at IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(vt.is_some());
        assert!(ib.is_some());
    }

    #[test]
    fn multi_valued_relation_accumulates() {
        let (mut conn, sc) = setup();
        for dep in ["serde", "clap"] {
            remember(
                &mut conn,
                &sc,
                None,
                &RememberInput {
                    text: format!("depends on {dep}"),
                    kind: "note".into(),
                    source: "test".into(),
                    facts: vec![FactInput {
                        src: Some("project".into()),
                        rel: "depends_on".into(),
                        dst: Some(dep.into()),
                        statement: format!("project depends on {dep}"),
                        valid_from: None,
                        valid_to: None,
                        confidence: 1.0,
                        supersede: None,
                    }],
                    files: vec![],
                    meta: None,
                    derive: true,
                },
            )
            .unwrap();
        }
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM facts WHERE rel='depends_on' AND invalidated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn entity_extraction_prefers_backticks_and_paths() {
        let e = extract_entities("Always run `cargo nextest` before touching src/retrieve.rs");
        assert!(e.iter().any(|s| s == "cargo nextest"), "got {e:?}");
        assert!(e.iter().any(|s| s == "src/retrieve.rs"), "got {e:?}");
    }

    #[test]
    fn extraction_ignores_leading_capital() {
        let e = extract_entities("Never force push");
        assert!(!e.iter().any(|s| s == "Never"), "got {e:?}");
    }

    #[test]
    fn aliases_resolve_to_one_node() {
        let (conn, sc) = setup();
        let id = upsert_entity(&conn, &sc, "PostgreSQL", Some("tool")).unwrap();
        add_alias(&conn, &sc, id, "postgres").unwrap();
        assert_eq!(upsert_entity(&conn, &sc, "postgres", None).unwrap(), id);
        assert_eq!(upsert_entity(&conn, &sc, "  POSTGRES ", None).unwrap(), id);
    }

    #[test]
    fn purge_removes_fact_and_vector() {
        let (mut conn, sc) = setup();
        let out = remember(&mut conn, &sc, None, &note("secret token abc123")).unwrap();
        let fid = out.fact_ids[0];
        assert!(purge(&conn, &sc, fid).unwrap());
        let n: i64 = conn
            .query_row("SELECT count(*) FROM facts WHERE id = ?1", [fid], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
        let fts: i64 = conn
            .query_row(
                "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH 'abc123'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0, "FTS row must go with the fact");
    }

    #[test]
    fn invalidate_is_soft() {
        let (mut conn, sc) = setup();
        let out = remember(&mut conn, &sc, None, &note("temporary workaround in place")).unwrap();
        let fid = out.fact_ids[0];
        assert!(invalidate(&conn, &sc, fid).unwrap());
        assert!(
            !invalidate(&conn, &sc, fid).unwrap(),
            "second call is a no-op"
        );
        let n: i64 = conn
            .query_row("SELECT count(*) FROM facts WHERE id = ?1", [fid], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1, "row still there for timeline queries");
    }
}
