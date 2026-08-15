//! Background consolidation.
//!
//! Everything expensive lives here instead of in the write path: collapsing
//! facts that say the same thing twice, closing out contradictions, keeping FTS
//! compact, and dropping retracted facts nobody ever read. Run it from a hook,
//! a cron, or `fuckmemory consolidate`; a memory store that is never consolidated
//! degrades into an append-only log that slowly poisons retrieval.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::config::{now, Config, DAY};
use crate::db::VEC_FACT;
use crate::embed::{cosine_q, Embedder, VecIndex};
use crate::graph::When;

/// Cosine above which two live facts in the same scope are the same fact.
/// Higher than the retrieval-time dedup threshold: merging is destructive, so it
/// should be more conservative than hiding.
const MERGE_AT: f32 = 0.96;

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub episodes_processed: usize,
    pub facts_merged: usize,
    pub facts_pruned: usize,
    pub fts_optimized: bool,
}

/// Drain the pending queue and tidy up.
pub fn run(
    conn: &mut Connection,
    _cfg: &Config,
    emb: Option<&Embedder>,
    limit: usize,
) -> Result<Report> {
    let mut report = Report::default();

    let pending: Vec<(i64, i64)> = conn
        .prepare("SELECT id, episode_id FROM pending ORDER BY id LIMIT ?1")?
        .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let scopes: Vec<i64> = conn
        .prepare("SELECT id FROM scopes")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if emb.is_some() {
        for sid in &scopes {
            report.facts_merged += merge_duplicates_in_scope(conn, *sid)?;
        }
    }

    // IMMEDIATE so concurrent writers queue on the write lock instead of failing
    // with SQLITE_BUSY when a deferred transaction tries to upgrade. See store.rs.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (pid, _episode) in &pending {
        tx.execute("DELETE FROM pending WHERE id = ?1", [pid])?;
        report.episodes_processed += 1;
    }
    tx.commit()?;

    if report.episodes_processed > 0 || report.facts_merged > 0 {
        // Merges the FTS b-tree segments; without this, a store written to in
        // many small transactions gets progressively slower to query.
        conn.execute_batch(
            "INSERT INTO facts_fts(facts_fts) VALUES('optimize');
             INSERT INTO episodes_fts(episodes_fts) VALUES('optimize');",
        )?;
        report.fts_optimized = true;
    }
    Ok(report)
}

/// Collapse semantically identical live facts within one scope.
///
/// The survivor is the most recently recorded one; it inherits the loser's hit
/// count so a long-used memory does not lose its ranking boost by being rephrased.
fn merge_duplicates_in_scope(conn: &mut Connection, scope_id: i64) -> Result<usize> {
    let idx = VecIndex::load_facts(conn, &[scope_id], When::Live)?;
    if idx.ids.len() < 2 {
        return Ok(0);
    }

    // recorded_at per candidate, to decide who survives.
    let mut meta: std::collections::HashMap<i64, (i64, i64)> = std::collections::HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT id, recorded_at, hits FROM facts WHERE scope_id = ?1 AND invalidated_at IS NULL",
        )?;
        let mut rows = st.query([scope_id])?;
        while let Some(r) = rows.next()? {
            meta.insert(r.get(0)?, (r.get(1)?, r.get(2)?));
        }
    }

    let mut merged = 0usize;
    let mut dead: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let ts = now();
    // IMMEDIATE so concurrent writers queue on the write lock instead of failing
    // with SQLITE_BUSY when a deferred transaction tries to upgrade. See store.rs.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    for i in 0..idx.ids.len() {
        let a = idx.ids[i];
        if dead.contains(&a) {
            continue;
        }
        for j in (i + 1)..idx.ids.len() {
            let b = idx.ids[j];
            if dead.contains(&b) {
                continue;
            }
            if cosine_q(idx.row(i), idx.row(j)) < MERGE_AT {
                continue;
            }
            let (ta, ha) = meta.get(&a).copied().unwrap_or((0, 0));
            let (tb, hb) = meta.get(&b).copied().unwrap_or((0, 0));
            let (keep, drop_, drop_hits) = if ta >= tb { (a, b, hb) } else { (b, a, ha) };
            tx.execute(
                "UPDATE facts SET hits = hits + ?2 WHERE id = ?1",
                params![keep, drop_hits],
            )?;
            tx.execute(
                "UPDATE facts SET invalidated_at = ?2, invalidated_by = ?3,
                                  valid_to = COALESCE(valid_to, ?2)
                 WHERE id = ?1 AND invalidated_at IS NULL",
                params![drop_, ts, keep],
            )?;
            dead.insert(drop_);
            merged += 1;
            if drop_ == a {
                break;
            }
        }
    }
    tx.commit()?;
    Ok(merged)
}

/// Hard-delete retracted facts that are older than `days` and were never used.
///
/// Retracted facts are the bi-temporal history, so this is opt-in and
/// conservative: anything with a hit is kept, because something read it once.
pub fn prune(conn: &Connection, days: i64, dry_run: bool) -> Result<usize> {
    let cutoff = now() - days * DAY;
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM facts
             WHERE invalidated_at IS NOT NULL AND invalidated_at < ?1 AND hits = 0",
        )?
        .query_map([cutoff], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if dry_run {
        return Ok(ids.len());
    }
    for id in &ids {
        conn.execute("DELETE FROM facts WHERE id = ?1", [id])?;
        conn.execute(
            "DELETE FROM vecs WHERE kind = ?1 AND ref_id = ?2",
            params![VEC_FACT, id],
        )?;
    }
    Ok(ids.len())
}

/// Recompute every embedding. Needed after switching models, since vectors from
/// two different models are not comparable. `model` is the configured model id,
/// stored so `doctor` can verify the stored vectors still match it.
pub fn reindex(conn: &mut Connection, emb: &Embedder, model: &str) -> Result<usize> {
    let rows: Vec<(i64, String)> = conn
        .prepare("SELECT id, statement FROM facts")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // IMMEDIATE so concurrent writers queue on the write lock instead of failing
    // with SQLITE_BUSY when a deferred transaction tries to upgrade. See store.rs.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM vecs", [])?;
    let texts: Vec<String> = rows.iter().map(|(_, s)| s.clone()).collect();
    for (chunk_rows, chunk_vecs) in rows
        .chunks(512)
        .zip(texts.chunks(512).map(|c| emb.embed_batch(c)))
    {
        for ((id, _), v) in chunk_rows.iter().zip(chunk_vecs.iter()) {
            crate::embed::put_vec(&tx, VEC_FACT, *id, &crate::embed::quantize(v))?;
        }
    }
    let n = rows.len();
    tx.commit()?;
    crate::db::meta_set(conn, "embed_model", &format!("{model}:{}", emb.dim))?;
    Ok(n)
}

/// Entity nodes nothing points at any more, e.g. after pruning.
pub fn drop_orphan_entities(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM entities WHERE id NOT IN (
             SELECT src FROM facts WHERE src IS NOT NULL
             UNION SELECT dst FROM facts WHERE dst IS NOT NULL
         )",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{invalidate, remember, RememberInput};
    use crate::{db, scope};
    use std::path::Path;

    fn setup() -> (Connection, crate::scope::Scope) {
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-cons"), Path::new("/")).unwrap();
        (conn, sc)
    }

    fn note(t: &str) -> RememberInput {
        RememberInput {
            text: t.into(),
            kind: "note".into(),
            source: "test".into(),
            facts: vec![],
            files: vec![],
            meta: None,
            derive: true,
        }
    }

    #[test]
    fn run_drains_pending_queue() {
        let (mut conn, sc) = setup();
        remember(&mut conn, &sc, None, &note("first thing")).unwrap();
        remember(&mut conn, &sc, None, &note("second thing")).unwrap();
        let before: i64 = conn
            .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 2);

        let cfg = Config {
            home: std::env::temp_dir(),
            model: "x".into(),
            budget_tokens: 500,
            semantic: false,
            ..Config::default()
        };
        let rep = run(&mut conn, &cfg, None, 100).unwrap();
        assert_eq!(rep.episodes_processed, 2);
        let after: i64 = conn
            .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0);
    }

    #[test]
    fn prune_keeps_history_that_was_read() {
        let (mut conn, sc) = setup();
        let a = remember(&mut conn, &sc, None, &note("never read fact")).unwrap();
        let b = remember(&mut conn, &sc, None, &note("useful fact")).unwrap();
        let (fa, fb) = (a.fact_ids[0], b.fact_ids[0]);
        crate::store::mark_hits(&conn, &[fb]).unwrap();
        invalidate(&conn, &sc, fa).unwrap();
        invalidate(&conn, &sc, fb).unwrap();
        // Backdate both retractions past the cutoff.
        conn.execute("UPDATE facts SET invalidated_at = 1", [])
            .unwrap();

        assert_eq!(
            prune(&conn, 1, true).unwrap(),
            1,
            "dry run counts only the unread one"
        );
        assert_eq!(prune(&conn, 1, false).unwrap(), 1);
        let left: i64 = conn
            .query_row("SELECT count(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1);
        let kept: i64 = conn
            .query_row("SELECT id FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, fb);
    }

    #[test]
    fn prune_leaves_live_facts_alone() {
        let (mut conn, sc) = setup();
        remember(&mut conn, &sc, None, &note("still true")).unwrap();
        conn.execute("UPDATE facts SET recorded_at = 1", [])
            .unwrap();
        assert_eq!(prune(&conn, 1, false).unwrap(), 0);
    }

    #[test]
    fn orphan_entities_are_dropped_only_when_unreferenced() {
        let (mut conn, sc) = setup();
        remember(&mut conn, &sc, None, &note("uses `pnpm` in `packages/web`")).unwrap();
        let before: i64 = conn
            .query_row("SELECT count(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert!(before > 0);
        assert_eq!(
            drop_orphan_entities(&conn).unwrap(),
            0,
            "all are referenced"
        );

        conn.execute("DELETE FROM facts", []).unwrap();
        assert_eq!(drop_orphan_entities(&conn).unwrap() as i64, before);
    }
}
