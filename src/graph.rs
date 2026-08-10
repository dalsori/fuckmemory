//! Graph reads: fact rows, neighbour expansion, and time travel.

use anyhow::Result;
use rusqlite::{Connection, Row};
use serde::Serialize;

/// A fact edge as read back out, with its entity names resolved.
#[derive(Debug, Clone, Serialize)]
pub struct FactRow {
    pub id: i64,
    pub scope_id: i64,
    pub src: Option<String>,
    pub rel: String,
    pub dst: Option<String>,
    pub statement: String,
    pub confidence: f32,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub recorded_at: i64,
    pub invalidated_at: Option<i64>,
    pub hits: i64,
    /// The episode this fact was derived from, when there is one. Used to look
    /// up the files the memory was learned against.
    pub episode_id: Option<i64>,
}

const SELECT_FACT: &str = "SELECT f.id, f.scope_id, se.name, f.rel, de.name, f.statement,
        f.confidence, f.valid_from, f.valid_to, f.recorded_at, f.invalidated_at, f.hits,
        f.episode_id
 FROM facts f
 LEFT JOIN entities se ON se.id = f.src
 LEFT JOIN entities de ON de.id = f.dst";

fn map_fact(r: &Row) -> rusqlite::Result<FactRow> {
    Ok(FactRow {
        id: r.get(0)?,
        scope_id: r.get(1)?,
        src: r.get(2)?,
        rel: r.get(3)?,
        dst: r.get(4)?,
        statement: r.get(5)?,
        confidence: r.get::<_, f64>(6)? as f32,
        valid_from: r.get(7)?,
        valid_to: r.get(8)?,
        recorded_at: r.get(9)?,
        invalidated_at: r.get(10)?,
        hits: r.get(11)?,
        episode_id: r.get(12)?,
    })
}

/// How to filter facts on the time axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Only what we currently believe. The default.
    Live,
    /// What we believed at `t` — the bi-temporal query that flat memory stores
    /// cannot answer.
    AsOf(i64),
    /// Everything, including retracted facts. For audits and timelines.
    Any,
}

impl When {
    /// SQL predicate over alias `f`. No user input is interpolated.
    pub fn predicate(&self) -> String {
        match self {
            When::Live => "f.invalidated_at IS NULL".into(),
            When::AsOf(t) => format!(
                "f.recorded_at <= {t}
                 AND COALESCE(f.valid_from, f.recorded_at) <= {t}
                 AND (f.valid_to IS NULL OR f.valid_to > {t})"
            ),
            When::Any => "1".into(),
        }
    }

    /// A single byte identifying the temporal window, for cache keys.
    pub fn tag(&self) -> u8 {
        match self {
            When::Live => 0,
            When::AsOf(t) => 1u8.wrapping_add((*t as u64 % 255) as u8),
            When::Any => 2,
        }
    }
}

pub fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn as_sql(v: &[i64]) -> Vec<&dyn rusqlite::ToSql> {
    v.iter().map(|x| x as &dyn rusqlite::ToSql).collect()
}

/// Fetch specific facts, preserving the caller's ordering.
pub fn fact_rows(conn: &Connection, ids: &[i64]) -> Result<Vec<FactRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!("{SELECT_FACT} WHERE f.id IN ({})", placeholders(ids.len()));
    let mut st = conn.prepare_cached(&sql)?;
    let rows = st.query_map(as_sql(ids).as_slice(), map_fact)?;
    let mut by_id = std::collections::HashMap::new();
    for r in rows {
        let r = r?;
        by_id.insert(r.id, r);
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// Entity ids touched by the given facts.
pub fn entities_of(conn: &Connection, fact_ids: &[i64]) -> Result<Vec<i64>> {
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT DISTINCT e FROM (
             SELECT src AS e FROM facts WHERE id IN ({p}) AND src IS NOT NULL
             UNION
             SELECT dst AS e FROM facts WHERE id IN ({p}) AND dst IS NOT NULL
         )",
        p = placeholders(fact_ids.len())
    );
    let mut st = conn.prepare_cached(&sql)?;
    let mut params = as_sql(fact_ids);
    params.extend(as_sql(fact_ids));
    let rows = st.query_map(params.as_slice(), |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Facts adjacent to `entity_ids` — one hop of graph expansion.
///
/// This is what lets a query about "the build" surface "CI runs on Node 22" even
/// when neither BM25 nor the embedding matched it: it hangs off an entity that
/// did match. Ordered so that facts on rarely-connected entities come first;
/// a hub entity like "project" would otherwise drown everything.
pub fn neighbors(
    conn: &Connection,
    scope_ids: &[i64],
    entity_ids: &[i64],
    exclude: &[i64],
    when: When,
    limit: usize,
) -> Result<Vec<i64>> {
    if entity_ids.is_empty() || scope_ids.is_empty() {
        return Ok(Vec::new());
    }
    // One indexed branch per direction, unioned.
    //
    // The obvious form — `JOIN entities e ON e.id = f.src OR e.id = f.dst` — is a
    // trap: an OR across two columns in a join condition makes SQLite abandon both
    // `facts_src` and `facts_dst` and scan the whole table for every seed entity.
    // At 10k facts that was ~15ms of a 20ms query. Split into two `IN` lookups it
    // is two index range scans.
    let excl = if exclude.is_empty() {
        String::new()
    } else {
        format!("AND f.id NOT IN ({})", placeholders(exclude.len()))
    };
    let branch = |col: &str| {
        format!(
            "SELECT f.id AS fid, e.degree AS deg, f.hits AS hits, f.recorded_at AS rec
             FROM facts f
             JOIN entities e ON e.id = f.{col}
             WHERE f.{col} IN ({ents}) AND f.scope_id IN ({scopes}) AND {when} {excl}",
            ents = placeholders(entity_ids.len()),
            scopes = placeholders(scope_ids.len()),
            when = when.predicate(),
        )
    };
    let sql = format!(
        "SELECT fid FROM ({src} UNION ALL {dst})
         GROUP BY fid
         ORDER BY MIN(deg) ASC, MAX(hits) DESC, MAX(rec) DESC
         LIMIT {limit}",
        src = branch("src"),
        dst = branch("dst"),
    );

    let mut st = conn.prepare_cached(&sql)?;
    let mut params = Vec::new();
    for _ in 0..2 {
        params.extend(as_sql(entity_ids));
        params.extend(as_sql(scope_ids));
        params.extend(as_sql(exclude));
    }
    let rows = st.query_map(params.as_slice(), |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every fact ever recorded about an entity, oldest first, retracted ones
/// included. Answers "when did this change, and what did it used to be?".
pub fn timeline(
    conn: &Connection,
    scope_ids: &[i64],
    entity: &str,
    limit: usize,
) -> Result<Vec<FactRow>> {
    let norm = crate::store::normalize(entity);
    let sql = format!(
        "{SELECT_FACT}
         WHERE f.scope_id IN ({scopes})
           AND (f.src IN (SELECT id FROM entities WHERE norm = ?{n1} AND scope_id IN ({scopes2}))
             OR f.dst IN (SELECT id FROM entities WHERE norm = ?{n1} AND scope_id IN ({scopes2}))
             OR f.statement LIKE ?{n2})
         ORDER BY COALESCE(f.valid_from, f.recorded_at) ASC, f.id ASC
         LIMIT {limit}",
        scopes = placeholders(scope_ids.len()),
        scopes2 = (1..=scope_ids.len())
            .map(|i| format!("?{}", i + 2 + scope_ids.len()))
            .collect::<Vec<_>>()
            .join(","),
        n1 = scope_ids.len() + 1,
        n2 = scope_ids.len() + 2,
    );
    let mut st = conn.prepare(&sql)?;
    let like = format!("%{}%", entity.trim());
    let mut params: Vec<&dyn rusqlite::ToSql> = as_sql(scope_ids);
    params.push(&norm);
    params.push(&like);
    params.extend(as_sql(scope_ids));
    let rows = st.query_map(params.as_slice(), map_fact)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Best-connected entities in a scope, for `stats` and for orienting a new session.
pub fn top_entities(
    conn: &Connection,
    scope_ids: &[i64],
    limit: usize,
) -> Result<Vec<(String, i64)>> {
    let sql = format!(
        "SELECT name, degree FROM entities
         WHERE scope_id IN ({}) ORDER BY degree DESC, id ASC LIMIT {limit}",
        placeholders(scope_ids.len())
    );
    let mut st = conn.prepare_cached(&sql)?;
    let rows = st.query_map(as_sql(scope_ids).as_slice(), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{remember, FactInput, RememberInput};
    use crate::{db, scope};
    use std::path::Path;

    fn seed() -> (Connection, crate::scope::Scope) {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-graph"), Path::new("/")).unwrap();
        let add = |conn: &mut Connection, src: &str, rel: &str, dst: &str, stmt: &str| {
            remember(
                conn,
                &sc,
                None,
                &RememberInput {
                    text: stmt.into(),
                    kind: "note".into(),
                    source: "test".into(),
                    facts: vec![FactInput {
                        src: Some(src.into()),
                        rel: rel.into(),
                        dst: Some(dst.into()),
                        statement: stmt.into(),
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
            .unwrap()
        };
        add(&mut conn, "ci", "runs_on", "node 22", "CI runs on Node 22");
        add(
            &mut conn,
            "ci",
            "uses",
            "github actions",
            "CI uses GitHub Actions",
        );
        add(
            &mut conn,
            "api",
            "uses",
            "postgres",
            "the api uses postgres",
        );
        (conn, sc)
    }

    #[test]
    fn neighbors_finds_siblings_of_a_matched_entity() {
        let (conn, sc) = seed();
        let ci: i64 = conn
            .query_row("SELECT id FROM entities WHERE norm='ci'", [], |r| r.get(0))
            .unwrap();
        let got = neighbors(&conn, &[sc.id], &[ci], &[], When::Live, 10).unwrap();
        assert_eq!(got.len(), 2, "both CI facts hang off the ci node");
    }

    #[test]
    fn neighbors_honors_exclude() {
        let (conn, sc) = seed();
        let ci: i64 = conn
            .query_row("SELECT id FROM entities WHERE norm='ci'", [], |r| r.get(0))
            .unwrap();
        let all = neighbors(&conn, &[sc.id], &[ci], &[], When::Live, 10).unwrap();
        let rest = neighbors(&conn, &[sc.id], &[ci], &all[..1], When::Live, 10).unwrap();
        assert_eq!(rest.len(), all.len() - 1);
        assert!(!rest.contains(&all[0]));
    }

    #[test]
    fn as_of_hides_facts_recorded_later() {
        let (conn, sc) = seed();
        let past = When::AsOf(1);
        let sql = format!(
            "SELECT count(*) FROM facts f WHERE f.scope_id = {} AND {}",
            sc.id,
            past.predicate()
        );
        let n: i64 = conn.query_row(&sql, [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "nothing was known at t=1");
    }

    #[test]
    fn timeline_includes_retracted_versions() {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-tl"), Path::new("/")).unwrap();
        for pm in ["npm", "pnpm"] {
            remember(
                &mut conn,
                &sc,
                None,
                &RememberInput {
                    text: format!("uses {pm}"),
                    kind: "decision".into(),
                    source: "test".into(),
                    facts: vec![FactInput {
                        src: Some("project".into()),
                        rel: "uses".into(),
                        dst: Some(pm.into()),
                        statement: format!("project uses {pm}"),
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
        let tl = timeline(&conn, &[sc.id], "project", 50).unwrap();
        assert!(tl.len() >= 2, "timeline should show both eras: {tl:?}");
        assert!(tl.iter().any(|f| f.invalidated_at.is_some()));
    }

    #[test]
    fn fact_rows_preserves_order() {
        let (conn, sc) = seed();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM facts WHERE scope_id = ?1 ORDER BY id DESC")
            .unwrap()
            .query_map([sc.id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let rows = fact_rows(&conn, &ids).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            ids,
            "ranking order must survive the fetch"
        );
    }
}
