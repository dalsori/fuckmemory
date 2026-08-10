//! Hybrid retrieval.
//!
//! Three independent retrievers vote, then their ranks are fused:
//!
//! - **BM25** (SQLite FTS5) nails exact tokens — `--no-verify`, `src/db.rs`,
//!   `potion-retrieval-32M`. Embeddings are bad at these.
//! - **Vector** (static embeddings, int8 scan) catches paraphrase — "how do I
//!   ship this" vs "deploy runs through fly.io".
//! - **Graph** pulls in facts that neither matched but that hang off an entity
//!   which did. This is the part flat vector stores structurally cannot do.
//!
//! Ranks are combined with Reciprocal Rank Fusion rather than raw scores,
//! because a BM25 score and a cosine are not on a comparable scale and
//! normalizing them is guesswork. RRF only needs the ordering.
//!
//! Then MMR drops near-duplicates, because ten phrasings of "use pnpm" is the
//! failure mode that quietly eats an agent's context window.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

use crate::config::now;
use crate::db::VEC_FACT;
use crate::embed::{self, cosine_q, Embedder, VecCache, VecIndex};
use crate::graph::{self, FactRow, When};

/// RRF damping. 60 is the value from the original RRF paper and is what every
/// hybrid-search implementation converged on; it makes rank 1 vs 2 matter much
/// more than rank 40 vs 41.
const RRF_K: f32 = 60.0;

const W_BM25: f32 = 1.0;
/// Below BM25 on purpose. Measured on a real store (see `explain`), static
/// embeddings rank the correct memory first only some of the time, while exact
/// token matches on identifiers, flags and paths are almost always right. Vectors
/// earn their place by catching paraphrase BM25 cannot — as a supporting vote.
const W_VECTOR: f32 = 0.7;
/// Graph hits are supporting context, not answers, so they vote lower still.
const W_GRAPH: f32 = 0.5;

/// How many candidates each retriever contributes before fusion.
const PER_RETRIEVER: usize = 60;

/// Relevance floor for the vector leg.
///
/// Without a floor the vector leg returns the *entire store* on every query:
/// static embeddings give a positive cosine to almost any pair of sentences. Each
/// unrelated fact then earns real RRF credit — being "rank 7 of 7" scores 1/67
/// against the best hit's 1/61, only 10% apart — and recall degenerates into
/// "dump everything", the worst thing a memory tool can do to a context window.
///
/// The floor is **relative**, because measured cosines rule out an absolute one.
/// On a real store, `potion-retrieval-32M` puts good matches at 0.09–0.21 while an
/// unrelated query still peaks at 0.12: the signal and noise ranges overlap, so no
/// fixed cutoff separates them. What does hold is that within *one* query the
/// relevant hits cluster near the top. `VEC_FLOOR_ABS` therefore only discards
/// non-positive noise; the relative term does the real work.
///
/// Re-check these with `fuckmemory explain` if you change the model.
pub const VEC_FLOOR_ABS: f32 = 0.02;
pub const VEC_FLOOR_REL: f32 = 0.65;

pub fn vector_floor(top: f32) -> f32 {
    VEC_FLOOR_ABS.max(top * VEC_FLOOR_REL)
}

pub fn passes_vector_floor(cos: f32, top: f32) -> bool {
    cos >= vector_floor(top)
}

/// MMR trade-off: 1.0 = pure relevance, 0.0 = pure diversity.
const MMR_LAMBDA: f32 = 0.72;
/// Above this cosine, two facts are treated as the same fact.
const NEAR_DUP_COSINE: f32 = 0.93;
/// Same idea for the lexical fallback. Lower, because word-overlap saturates
/// further from 1.0 than cosine does: two rephrasings of one sentence land around
/// 0.85-0.9 on Jaccard but 0.95+ on embeddings.
const NEAR_DUP_LEXICAL: f32 = 0.82;

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub limit: usize,
    pub when: When,
    pub hops: usize,
    /// Also search raw episodes. Off by default: distilled facts are what an
    /// agent should read, raw text is for humans debugging the memory.
    pub include_episodes: bool,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            text: String::new(),
            limit: 12,
            when: When::Live,
            hops: 1,
            include_episodes: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub fact: FactRow,
    /// Relevance within this result set, min-max normalized to `[0, 1]`. Not
    /// comparable across queries — it says "how good relative to the others here".
    pub score: f32,
    /// Which retrievers found it. Makes bad rankings debuggable.
    pub via: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EpisodeHit {
    pub id: i64,
    pub kind: String,
    pub source: String,
    pub text: String,
    pub recorded_at: i64,
}

#[derive(Debug, Serialize)]
pub struct Recall {
    pub hits: Vec<Hit>,
    pub episodes: Vec<EpisodeHit>,
    /// Files attached to the episodes behind the returned facts, keyed by
    /// episode id. Lets a recall point at *where* a memory was learned.
    pub files: std::collections::HashMap<i64, Vec<crate::store::FileRef>>,
    /// False when the model wasn't available and we ran BM25-only.
    pub semantic: bool,
    pub took_us: u128,
}

/// Words that carry no retrieval signal in either language this is used in.
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "are", "was", "were", "be",
    "do", "does", "did", "how", "what", "when", "why", "which", "that", "this", "it", "with", "at",
    "by", "from", "as", "we", "i", "you", "my", "our", "el", "la", "los", "las", "un", "una", "de",
    "del", "y", "o", "que", "es", "son", "en", "con", "para", "por", "como", "se", "lo", "al",
    "mi", "nuestro", "cual", "cuando",
];

/// Turn free text into an FTS5 MATCH expression.
///
/// Every token is quoted, so nothing the user types can be read as FTS5 syntax
/// (`NEAR`, `*`, `:`, `^`, parens). Tokens of 3+ chars also match by prefix so
/// "deploys" finds "deploy".
///
/// The split set must mirror the FTS5 `tokenchars` in [`crate::db`] exactly, or
/// queries silently stop matching stored text. In particular `-` splits here
/// because it splits there, which is what lets `--no-verify` match.
pub fn fts_query(text: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in
        text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '/' || c == '@'))
    {
        let t = raw.trim_matches(|c: char| c == '.' || c == '/');
        if t.len() < 2 || STOP.contains(&t.to_ascii_lowercase().as_str()) {
            continue;
        }
        let escaped = t.replace('"', "\"\"");
        terms.push(if t.chars().count() >= 3 {
            format!("\"{escaped}\"*")
        } else {
            format!("\"{escaped}\"")
        });
        if terms.len() >= 24 {
            break;
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

fn bm25_facts(
    conn: &Connection,
    scope_ids: &[i64],
    match_expr: &str,
    when: When,
    limit: usize,
) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT f.id, bm25(facts_fts) AS s
         FROM facts_fts
         JOIN facts f ON f.id = facts_fts.rowid
         WHERE facts_fts MATCH ?1 AND f.scope_id IN ({scopes}) AND {when}
         ORDER BY s ASC
         LIMIT {limit}",
        scopes = graph::placeholders(scope_ids.len()),
        when = when.predicate(),
    );
    let mut st = conn.prepare_cached(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&match_expr];
    params.extend(scope_ids.iter().map(|s| s as &dyn rusqlite::ToSql));
    let rows = st.query_map(params.as_slice(), |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn bm25_episodes(
    conn: &Connection,
    scope_ids: &[i64],
    match_expr: &str,
    limit: usize,
) -> Result<Vec<EpisodeHit>> {
    let sql = format!(
        "SELECT e.id, e.kind, e.source, e.body, e.recorded_at
         FROM episodes_fts
         JOIN episodes e ON e.id = episodes_fts.rowid
         WHERE episodes_fts MATCH ?1 AND e.scope_id IN ({scopes})
         ORDER BY bm25(episodes_fts) ASC
         LIMIT {limit}",
        scopes = graph::placeholders(scope_ids.len()),
    );
    let mut st = conn.prepare_cached(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&match_expr];
    params.extend(scope_ids.iter().map(|s| s as &dyn rusqlite::ToSql));
    let rows = st.query_map(params.as_slice(), |r| {
        Ok(EpisodeHit {
            id: r.get(0)?,
            kind: r.get(1)?,
            source: r.get(2)?,
            text: r.get(3)?,
            recorded_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every live fact scored against the query by the vector leg alone, best first.
/// Powers `fuckmemory explain`, and is how the relevance floor below was calibrated.
pub fn explain_vectors(
    conn: &Connection,
    scope_ids: &[i64],
    emb: &Embedder,
    text: &str,
    cache: Option<&mut VecCache>,
) -> Result<Vec<(i64, f32)>> {
    let idx = IndexSource::get(conn, scope_ids, When::Live, cache)?;
    let idx = idx.as_ref();
    if idx.is_empty() || idx.dim != emb.dim {
        return Ok(Vec::new());
    }
    Ok(idx.topk(&emb.embed_q(text), idx.ids.len()))
}

/// Reciprocal Rank Fusion over any number of ranked id lists.
pub fn rrf(lists: &[(&[i64], f32, &'static str)]) -> Vec<(i64, f32, Vec<&'static str>)> {
    let mut acc: HashMap<i64, (f32, Vec<&'static str>)> = HashMap::new();
    for (ids, weight, name) in lists {
        for (rank, id) in ids.iter().enumerate() {
            let e = acc.entry(*id).or_insert((0.0, Vec::new()));
            e.0 += weight / (RRF_K + rank as f32 + 1.0);
            if !e.1.contains(name) {
                e.1.push(name);
            }
        }
    }
    let mut out: Vec<(i64, f32, Vec<&'static str>)> =
        acc.into_iter().map(|(id, (s, v))| (id, s, v)).collect();
    out.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Multiplicative adjustments applied after fusion. Kept small and separate so
/// a surprising ranking can be reasoned about instead of reverse-engineered.
fn adjust(base: f32, f: &FactRow, now_ts: i64) -> f32 {
    let confidence = 0.7 + 0.3 * f.confidence.clamp(0.0, 1.0);
    let age_days = ((now_ts - f.recorded_at).max(0) as f32) / crate::config::DAY as f32;
    // Old facts are not wrong, just less likely to be what you meant now.
    let recency = 0.85 + 0.15 * (-age_days / 90.0).exp();
    let popularity = 1.0 + 0.05 * (1.0 + f.hits as f32).ln();
    base * confidence * recency * popularity
}

/// Word-overlap similarity, the fallback when we have no embeddings.
fn jaccard(a: &str, b: &str) -> f32 {
    let ta: std::collections::HashSet<String> = a
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s.len() > 2)
        .collect();
    let tb: std::collections::HashSet<String> = b
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s.len() > 2)
        .collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    inter / (ta.len() + tb.len()) as f32 * 2.0
}

/// Maximal Marginal Relevance: greedily take the best remaining candidate,
/// penalized by how similar it already is to what we picked.
///
/// Scores are min-max normalized to `[0, 1]` first, and this is not cosmetic. Raw
/// RRF scores sit around 0.016 and differ by ~0.001, while the similarity penalty
/// spans 0 to 1. Un-normalized, `(1 - λ) * max_sim` dwarfs `λ * score` and MMR
/// silently stops being a relevance ranking at all — it sorts by dissimilarity.
fn mmr(
    scored: Vec<(FactRow, f32, Vec<&'static str>)>,
    vecs: &HashMap<i64, Vec<i8>>,
    limit: usize,
) -> Vec<Hit> {
    let hi = scored.iter().map(|(_, s, _)| *s).fold(f32::MIN, f32::max);
    let lo = scored.iter().map(|(_, s, _)| *s).fold(f32::MAX, f32::min);
    let span = hi - lo;
    let mut pool: Vec<(FactRow, f32, Vec<&'static str>)> = scored
        .into_iter()
        .map(|(f, s, v)| {
            // All-equal scores normalize to 1.0, which leaves MMR ordering to the
            // diversity term — correct, since relevance carries no signal there.
            let n = if span > f32::EPSILON {
                (s - lo) / span
            } else {
                1.0
            };
            (f, n, v)
        })
        .collect();
    let mut picked: Vec<Hit> = Vec::with_capacity(limit);

    // Returns the similarity *and* the near-duplicate threshold for whichever
    // metric was used, since the two are not on the same scale.
    let sim = |a: &FactRow, b: &FactRow| -> (f32, f32) {
        match (vecs.get(&a.id), vecs.get(&b.id)) {
            (Some(x), Some(y)) => (cosine_q(x, y), NEAR_DUP_COSINE),
            _ => (jaccard(&a.statement, &b.statement), NEAR_DUP_LEXICAL),
        }
    };

    while picked.len() < limit && !pool.is_empty() {
        let mut best = 0usize;
        let mut best_val = f32::MIN;
        for (i, (fact, score, _)) in pool.iter().enumerate() {
            let (max_sim, is_dup) = picked.iter().fold((0.0f32, false), |(m, dup), h| {
                let (s, threshold) = sim(fact, &h.fact);
                (m.max(s), dup || s >= threshold)
            });
            // A true near-duplicate is dropped outright rather than ranked low.
            if is_dup {
                continue;
            }
            let val = MMR_LAMBDA * score - (1.0 - MMR_LAMBDA) * max_sim;
            if val > best_val {
                best_val = val;
                best = i;
            }
        }
        if best_val == f32::MIN {
            break; // everything left is a duplicate of something picked
        }
        let (fact, score, via) = pool.swap_remove(best);
        picked.push(Hit { fact, score, via });
    }
    picked
}

/// Where a vector index comes from: the per-connection cache when one was
/// passed, else a fresh load. Keeping both behind one pointer means `recall`
/// does not care which side produced it.
enum IndexSource<'a> {
    Cached(&'a VecIndex),
    Owned(VecIndex),
}

impl<'a> IndexSource<'a> {
    fn get(
        conn: &Connection,
        scope_ids: &[i64],
        when: When,
        cache: Option<&'a mut VecCache>,
    ) -> Result<Self> {
        match cache {
            Some(c) => Ok(IndexSource::Cached(c.index(conn, scope_ids, when)?)),
            None => Ok(IndexSource::Owned(VecIndex::load_facts(
                conn, scope_ids, when,
            )?)),
        }
    }

    fn as_ref(&self) -> &VecIndex {
        match self {
            IndexSource::Cached(i) => i,
            IndexSource::Owned(i) => i,
        }
    }
}

/// Run a recall. `cache` is the per-connection vector index cache (the MCP
/// server keeps one across calls); one-shot processes pass `None`.
pub fn recall(
    conn: &Connection,
    scope_ids: &[i64],
    emb: Option<&Embedder>,
    q: &Query,
    cache: Option<&mut VecCache>,
) -> Result<Recall> {
    let t0 = std::time::Instant::now();
    let ts = now();

    let match_expr = fts_query(&q.text);
    let lex: Vec<i64> = match &match_expr {
        Some(m) => bm25_facts(conn, scope_ids, m, q.when, PER_RETRIEVER)?,
        None => Vec::new(),
    };

    // Semantic leg. Skipped when no model is present, or when the stored vectors
    // came from a different model and would score nonsense.
    let mut vec_ids: Vec<i64> = Vec::new();
    let mut semantic = false;
    if let Some(e) = emb {
        let idx = IndexSource::get(conn, scope_ids, q.when, cache)?;
        let idx = idx.as_ref();
        if !idx.is_empty() && idx.dim == e.dim {
            let scored = idx.topk(&e.embed_q(&q.text), PER_RETRIEVER);
            let top = scored.first().map(|(_, s)| *s).unwrap_or(0.0);
            vec_ids = scored
                .into_iter()
                .filter(|(_, s)| passes_vector_floor(*s, top))
                .map(|(id, _)| id)
                .collect();
            semantic = true;
        }
    }

    // Graph leg, seeded by whatever the other two agreed on most.
    let mut seeds: Vec<i64> = rrf(&[(&lex, W_BM25, "bm25"), (&vec_ids, W_VECTOR, "vector")])
        .into_iter()
        .take(10)
        .map(|(id, _, _)| id)
        .collect();
    let mut graph_ids: Vec<i64> = Vec::new();
    let mut frontier = seeds.clone();
    for _ in 0..q.hops {
        if frontier.is_empty() {
            break;
        }
        let ents = graph::entities_of(conn, &frontier)?;
        let mut exclude = seeds.clone();
        exclude.extend(graph_ids.iter().copied());
        let found = graph::neighbors(conn, scope_ids, &ents, &exclude, q.when, PER_RETRIEVER / 2)?;
        frontier = found.clone();
        graph_ids.extend(found);
        seeds.extend(graph_ids.iter().copied());
    }

    let fused = rrf(&[
        (&lex, W_BM25, "bm25"),
        (&vec_ids, W_VECTOR, "vector"),
        (&graph_ids, W_GRAPH, "graph"),
    ]);

    // Oversample before MMR so diversification has something to choose from.
    let take = (q.limit * 4).max(24);
    let cand_ids: Vec<i64> = fused.iter().take(take).map(|(id, _, _)| *id).collect();
    let rows = graph::fact_rows(conn, &cand_ids)?;
    let by_id: HashMap<i64, (f32, Vec<&'static str>)> =
        fused.into_iter().map(|(id, s, v)| (id, (s, v))).collect();

    let mut scored: Vec<(FactRow, f32, Vec<&'static str>)> = rows
        .into_iter()
        .filter_map(|f| {
            let (base, via) = by_id.get(&f.id)?.clone();
            let s = adjust(base, &f, ts);
            Some((f, s, via))
        })
        .collect();
    scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

    let vecs = if semantic {
        embed::load_vecs(conn, VEC_FACT, &cand_ids)?
    } else {
        HashMap::new()
    };
    let hits = mmr(scored, &vecs, q.limit);

    let episodes = match (&match_expr, q.include_episodes) {
        (Some(m), true) => bm25_episodes(conn, scope_ids, m, q.limit)?,
        _ => Vec::new(),
    };

    // Files behind the returned facts: collect the distinct episodes they came
    // from and group the references by episode so rendering can attach them.
    let mut episode_ids: Vec<i64> = Vec::new();
    for h in &hits {
        if let Some(eid) = h.fact.episode_id {
            if !episode_ids.contains(&eid) {
                episode_ids.push(eid);
            }
        }
    }
    let mut files: std::collections::HashMap<i64, Vec<crate::store::FileRef>> =
        std::collections::HashMap::new();
    let by_episode: std::collections::HashMap<i64, Vec<crate::store::FileRef>> = episode_ids
        .iter()
        .map(|eid| {
            (
                *eid,
                crate::store::files_for_episodes(conn, &[*eid]).unwrap_or_default(),
            )
        })
        .collect();
    for h in &hits {
        if let Some(eid) = h.fact.episode_id {
            if let Some(list) = by_episode.get(&eid) {
                if !list.is_empty() {
                    files.insert(eid, list.clone());
                }
            }
        }
    }

    Ok(Recall {
        hits,
        episodes,
        files,
        semantic,
        took_us: t0.elapsed().as_micros(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{remember, FactInput, RememberInput};
    use crate::{db, scope};
    use std::path::Path;

    fn add(
        conn: &mut Connection,
        sc: &crate::scope::Scope,
        src: &str,
        rel: &str,
        dst: &str,
        stmt: &str,
    ) {
        remember(
            conn,
            sc,
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
        .unwrap();
    }

    fn seeded() -> (Connection, crate::scope::Scope) {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-retr"), Path::new("/")).unwrap();
        add(
            &mut conn,
            &sc,
            "deploy",
            "uses",
            "fly.io",
            "deploys go out through fly.io",
        );
        add(
            &mut conn,
            &sc,
            "ci",
            "runs_on",
            "node 22",
            "CI runs on Node 22",
        );
        add(
            &mut conn,
            &sc,
            "ci",
            "uses",
            "github actions",
            "CI uses GitHub Actions",
        );
        add(
            &mut conn,
            &sc,
            "tests",
            "run_with",
            "cargo nextest",
            "run tests with `cargo nextest`",
        );
        add(
            &mut conn,
            &sc,
            "repo",
            "forbids",
            "--no-verify",
            "never commit with --no-verify",
        );
        (conn, sc)
    }

    #[test]
    fn fts_query_quotes_and_prefixes() {
        let q = fts_query("deploy the app").unwrap();
        assert!(q.contains("\"deploy\"*"));
        assert!(!q.contains("the"), "stopwords dropped: {q}");
    }

    #[test]
    fn fts_query_neutralizes_syntax() {
        // These would be operators or a syntax error if passed through raw.
        for evil in [
            "NEAR(a b)",
            "foo*",
            "col:val",
            "^start",
            "a AND (b",
            "\"quoted\"",
        ] {
            let q = fts_query(evil).unwrap_or_default();
            let (conn, sc) = seeded();
            if q.is_empty() {
                continue;
            }
            let r = bm25_facts(&conn, &[sc.id], &q, When::Live, 5);
            assert!(r.is_ok(), "input {evil:?} produced invalid MATCH {q:?}");
        }
    }

    #[test]
    fn fts_query_empty_for_noise() {
        assert!(fts_query("   ??? ").is_none());
        assert!(fts_query("the a of to").is_none());
    }

    #[test]
    fn bm25_finds_exact_flag_token() {
        let (conn, sc) = seeded();
        let q = fts_query("--no-verify").unwrap();
        let ids = bm25_facts(&conn, &[sc.id], &q, When::Live, 10).unwrap();
        let rows = graph::fact_rows(&conn, &ids).unwrap();
        assert!(
            rows.iter().any(|r| r.statement.contains("--no-verify")),
            "exact flags are exactly what BM25 is here for: {rows:?}"
        );
    }

    #[test]
    fn recall_ranks_lexical_match_first() {
        let (conn, sc) = seeded();
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "how do deploys work".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(!r.hits.is_empty());
        assert!(
            r.hits[0].fact.statement.contains("fly.io"),
            "got {:?}",
            r.hits.iter().map(|h| &h.fact.statement).collect::<Vec<_>>()
        );
        assert!(!r.semantic, "no embedder was supplied");
    }

    #[test]
    fn graph_expansion_surfaces_a_sibling_fact() {
        let (conn, sc) = seeded();
        // "github actions" matches lexically; "CI runs on Node 22" shares the ci
        // node but shares no query token, so only the graph leg can find it.
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "github actions".into(),
                limit: 10,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let found = r.hits.iter().find(|h| h.fact.statement.contains("Node 22"));
        assert!(
            found.is_some(),
            "graph leg missing: {:?}",
            r.hits
                .iter()
                .map(|h| (&h.fact.statement, &h.via))
                .collect::<Vec<_>>()
        );
        assert!(found.unwrap().via.contains(&"graph"));
    }

    #[test]
    fn hops_zero_disables_graph_leg() {
        let (conn, sc) = seeded();
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "github actions".into(),
                hops: 0,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(r.hits.iter().all(|h| !h.via.contains(&"graph")));
    }

    #[test]
    fn superseded_facts_are_not_recalled_but_are_visible_as_of() {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-retr2"), Path::new("/")).unwrap();
        add(
            &mut conn,
            &sc,
            "project",
            "uses",
            "npm",
            "the project uses npm",
        );
        let t_mid = now();
        add(
            &mut conn,
            &sc,
            "project",
            "uses",
            "pnpm",
            "the project uses pnpm",
        );

        let live = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "package manager npm".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(live
            .hits
            .iter()
            .all(|h| !h.fact.statement.ends_with("npm") || h.fact.statement.contains("pnpm")));

        let past = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "package manager npm".into(),
                when: When::AsOf(t_mid),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(
            past.hits
                .iter()
                .any(|h| h.fact.statement == "the project uses npm"),
            "as_of should see the old belief: {:?}",
            past.hits
                .iter()
                .map(|h| &h.fact.statement)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mmr_collapses_near_duplicates() {
        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-dup"), Path::new("/")).unwrap();
        for (i, s) in [
            "always use pnpm to install",
            "always use pnpm to install deps",
            "always use pnpm to install dependencies",
        ]
        .iter()
        .enumerate()
        {
            add(&mut conn, &sc, &format!("pm{i}"), "prefers", "pnpm", s);
        }
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "pnpm install".into(),
                limit: 10,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        // Without MMR all three come back; jaccard sim between them is ~0.8-0.9.
        assert!(
            r.hits.len() < 3,
            "expected dedup, got {:?}",
            r.hits.iter().map(|h| &h.fact.statement).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_query_returns_nothing_rather_than_everything() {
        let (conn, sc) = seeded();
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "  ".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(r.hits.is_empty());
    }

    #[test]
    fn rrf_prefers_agreement_across_retrievers() {
        let a = [1i64, 2, 3];
        let b = [3i64, 4, 5];
        let out = rrf(&[(&a, 1.0, "bm25"), (&b, 1.0, "vector")]);
        assert_eq!(out[0].0, 3, "id 3 is the only one both lists rank");
        assert_eq!(out[0].2.len(), 2);
    }

    #[test]
    fn recall_respects_limit() {
        let (conn, sc) = seeded();
        let r = recall(
            &conn,
            &[sc.id],
            None,
            &Query {
                text: "ci deploy tests repo".into(),
                limit: 2,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(r.hits.len() <= 2);
    }
}
