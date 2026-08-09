//! Static embeddings and the int8 vector scan.
//!
//! Two deliberate choices make this fast enough to sit in the write path:
//!
//! 1. **Static embeddings** (Model2Vec / potion). Encoding is a tokenize plus an
//!    embedding-table lookup and a mean pool — no transformer forward pass, no
//!    ONNX runtime, no GPU. Microseconds per short memory instead of tens of ms.
//! 2. **int8 quantization + brute-force scan.** Vectors are L2-normalized, so
//!    cosine is a dot product; quantized to i8 a 512-dim vector is 512 bytes.
//!    Below ~100k memories a linear SIMD scan beats an ANN index on latency and
//!    is exact, with no index to rebuild on every write.

use anyhow::{Context, Result};
use model2vec_rs::model::StaticModel;
use rusqlite::Connection;
use std::path::Path;

use crate::config::Config;
use crate::db;
use crate::fast::{self, Fast};
use crate::graph::When;

/// Scale used to map a unit-norm f32 component into i8.
const Q: f32 = 127.0;

/// Where the embeddings come from. Both backends produce the same vectors — the
/// fast one is verified against the slow one before it is ever installed — so
/// nothing downstream needs to know which is in use.
enum Backend {
    /// The mmap'd cache: ~1 ms to open. See [`crate::fast`].
    Fast(Fast),
    /// The real model2vec model: correct everywhere, ~206 ms to load. Boxed
    /// because it is much larger than the cache handle and this enum sits in
    /// every process.
    Full(Box<StaticModel>),
}

pub struct Embedder {
    backend: Backend,
    pub dim: usize,
}

impl Embedder {
    /// Load the model, downloading it into our own data dir on first use.
    pub fn load(cfg: &Config) -> Result<Self> {
        if cfg.fast {
            if let Some(f) = Fast::open(cfg) {
                let dim = f.dim();
                return Ok(Self {
                    backend: Backend::Fast(f),
                    dim,
                });
            }
        }
        let dir = if is_installed(cfg) {
            cfg.model_dir()
        } else {
            download(cfg).with_context(|| {
                format!(
                    "downloading model {} (set FUCKMEMORY_SEMANTIC=0 to run keyword-only)",
                    cfg.model
                )
            })?
        };
        let model = StaticModel::from_pretrained(&dir, None, Some(true), None)
            .with_context(|| format!("loading model from {}", dir.display()))?;
        let dim = model.encode_single("dimension probe").len();
        anyhow::ensure!(dim > 0, "model produced empty embeddings");

        // Having paid the slow load once, build the cache so nothing pays it
        // again. Failures are non-fatal: this process keeps the model it has.
        if cfg.fast {
            ensure_cache(cfg);
        }
        Ok(Self {
            backend: Backend::Full(Box::new(model)),
            dim,
        })
    }

    /// Load only if the model is already on disk. Used on every hot path, so no
    /// recall can stall on a first-run network download.
    pub fn load_if_cached(cfg: &Config) -> Option<Self> {
        if cfg.fast {
            if let Some(f) = Fast::open(cfg) {
                let dim = f.dim();
                return Some(Self {
                    backend: Backend::Fast(f),
                    dim,
                });
            }
        }
        if is_installed(cfg) {
            Self::load(cfg).ok()
        } else {
            None
        }
    }

    /// True when this embedder is running off the mmap'd cache.
    pub fn is_fast(&self) -> bool {
        matches!(self.backend, Backend::Fast(_))
    }

    pub fn embed(&self, text: &str) -> Vec<f32> {
        match &self.backend {
            Backend::Fast(f) => f.embed(text),
            Backend::Full(m) => m.encode_single(text),
        }
    }

    pub fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        match &self.backend {
            // One text at a time is already ~16 µs; parallelising pays off for the
            // thousands of texts a `reindex` hands over.
            Backend::Fast(f) => {
                use rayon::prelude::*;
                texts.par_iter().map(|t| f.embed(t)).collect()
            }
            Backend::Full(m) => m.encode(texts),
        }
    }

    /// Embed and quantize in one step.
    pub fn embed_q(&self, text: &str) -> Vec<i8> {
        quantize(&self.embed(text))
    }
}

/// Marker written when a cache build fails, so a broken or unsupported model
/// doesn't make every single invocation retry a two-second build.
fn disabled_marker(cfg: &Config) -> std::path::PathBuf {
    cfg.model_dir().join("fastembed.disabled")
}

/// Read the recorded reason the fast path is off for this model, if any.
pub fn fast_disabled_reason(cfg: &Config) -> Option<String> {
    std::fs::read_to_string(disabled_marker(cfg))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Build the fast cache unless a previous attempt already failed for this model.
fn ensure_cache(cfg: &Config) {
    if disabled_marker(cfg).exists() {
        return;
    }
    match fast::build(cfg, false) {
        Ok(_) => {}
        Err(e) => {
            let reason = format!("{e:#}");
            eprintln!("fuckmemory: fast embedding cache unavailable — {reason}");
            std::fs::write(disabled_marker(cfg), &reason).ok();
        }
    }
}

/// Build the cache on demand, clearing any previous failure. Used by
/// `model cache` and by `install`.
pub fn build_cache(cfg: &Config, force: bool) -> Result<usize> {
    std::fs::remove_file(disabled_marker(cfg)).ok();
    let r = fast::build(cfg, force);
    if let Err(e) = &r {
        std::fs::write(disabled_marker(cfg), format!("{e:#}")).ok();
    }
    r
}

/// Files `StaticModel::from_pretrained` needs to find in a local folder. The
/// config is looked up under either name because Model2Vec exports vary.
const TOKENIZER: &str = "tokenizer.json";
const WEIGHTS: &str = "model.safetensors";
const CONFIGS: &[&str] = &["config.json", "config_sentence_transformers.json"];

/// Is the model fully present in our own data dir?
pub fn is_installed(cfg: &Config) -> bool {
    let dir = cfg.model_dir();
    dir.join(TOKENIZER).is_file()
        && dir.join(WEIGHTS).is_file()
        && CONFIGS.iter().any(|c| dir.join(c).is_file())
}

/// Fetch the model into `cfg.model_dir()` as a flat folder.
///
/// Downloading it ourselves rather than letting model2vec-rs do it keeps every
/// byte this tool writes under one directory the user can delete, and makes
/// `doctor` able to tell the truth about where the model is.
fn download(cfg: &Config) -> Result<std::path::PathBuf> {
    use hf_hub::{api::sync::ApiBuilder, Cache};

    let dir = cfg.model_dir();
    if let Some(local) = Path::new(&cfg.model)
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
    {
        // The user pointed FUCKMEMORY_MODEL at a folder; use it in place.
        return Ok(local);
    }
    std::fs::create_dir_all(&dir)?;

    // Staging area inside our home, so a partial download is never mistaken for
    // an installed model and never pollutes the shared HF cache.
    let cache = Cache::new(cfg.models_dir().join(".hub-cache"));
    let api = ApiBuilder::from_cache(cache)
        .with_progress(false)
        .build()
        .context("initializing the HuggingFace client")?;
    let repo = api.model(cfg.model.clone());

    let fetch = |name: &str| -> Result<std::path::PathBuf> {
        repo.get(name)
            .map_err(|e| anyhow::anyhow!("fetching {name}: {e}"))
    };

    let tokenizer = fetch(TOKENIZER)?;
    let weights = fetch(WEIGHTS)?;
    let (config_name, config) = CONFIGS
        .iter()
        .find_map(|c| fetch(c).ok().map(|p| (*c, p)))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} has none of {CONFIGS:?} — is it a Model2Vec model?",
                cfg.model
            )
        })?;

    for (src, name) in [
        (tokenizer, TOKENIZER),
        (weights, WEIGHTS),
        (config, config_name),
    ] {
        std::fs::copy(&src, dir.join(name))
            .with_context(|| format!("installing {name} into {}", dir.display()))?;
    }
    // The staging cache is a duplicate once the files are copied.
    std::fs::remove_dir_all(cfg.models_dir().join(".hub-cache")).ok();
    Ok(dir)
}

/// L2-normalize then map to i8. Normalizing here means a stored vector is
/// comparable to any other regardless of the model's own normalize flag.
pub fn quantize(v: &[f32]) -> Vec<i8> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let inv = if norm > 0.0 { 1.0 / norm } else { 0.0 };
    v.iter()
        .map(|x| (x * inv * Q).round().clamp(-Q, Q) as i8)
        .collect()
}

pub fn to_blob(q: &[i8]) -> Vec<u8> {
    q.iter().map(|&x| x as u8).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<i8> {
    b.iter().map(|&x| x as i8).collect()
}

/// Cosine similarity of two quantized vectors, in `[-1, 1]`.
///
/// Accumulating in i32 keeps this exact and lets the autovectorizer emit SIMD;
/// a 512-dim pair is a handful of nanoseconds.
#[inline]
pub fn cosine_q(a: &[i8], b: &[i8]) -> f32 {
    let n = a.len().min(b.len());
    let mut acc: i32 = 0;
    for i in 0..n {
        acc += a[i] as i32 * b[i] as i32;
    }
    acc as f32 / (Q * Q)
}

/// Flat int8 matrix of candidate vectors, loaded once per query (or once per
/// process for the long-lived MCP server).
#[derive(Debug)]
pub struct VecIndex {
    pub dim: usize,
    pub ids: Vec<i64>,
    data: Vec<i8>,
}

impl VecIndex {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Load the fact vectors for the given scopes, restricted to `when`.
    ///
    /// The temporal filter has to be applied *here*, not just in the SQL legs. It
    /// was missed at first, and the effect was subtle but bad: `--as-of` filtered
    /// BM25 and the graph correctly while the vector leg still returned only
    /// currently-live facts, so a time-travel query leaked today's beliefs and
    /// could never surface the historical ones it exists to find.
    pub fn load_facts(conn: &Connection, scope_ids: &[i64], when: When) -> Result<Self> {
        let placeholders = vec!["?"; scope_ids.len()].join(",");
        let sql = format!(
            "SELECT v.ref_id, v.q FROM vecs v
             JOIN facts f ON f.id = v.ref_id
             WHERE v.kind = {kind} AND f.scope_id IN ({placeholders}) AND {when}",
            kind = db::VEC_FACT,
            when = when.predicate(),
        );
        let mut st = conn.prepare_cached(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = scope_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let mut rows = st.query(params.as_slice())?;

        let mut ids = Vec::new();
        let mut data: Vec<i8> = Vec::new();
        let mut dim = 0usize;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            if dim == 0 {
                dim = blob.len();
            }
            // A model change mid-history would leave stale widths behind; skip
            // them rather than corrupt every score.
            if blob.len() != dim {
                continue;
            }
            ids.push(id);
            data.extend(blob.iter().map(|&x| x as i8));
        }
        Ok(Self { dim, ids, data })
    }

    #[inline]
    pub fn row(&self, i: usize) -> &[i8] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    /// Top-`k` by cosine. Returns `(id, score)` descending.
    pub fn topk(&self, query: &[i8], k: usize) -> Vec<(i64, f32)> {
        if self.ids.is_empty() || self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let mut scored: Vec<(i64, f32)> = if self.ids.len() >= 4_096 {
            use rayon::prelude::*;
            self.data
                .par_chunks(self.dim)
                .enumerate()
                .map(|(i, row)| (self.ids[i], cosine_q(query, row)))
                .collect()
        } else {
            (0..self.ids.len())
                .map(|i| (self.ids[i], cosine_q(query, self.row(i))))
                .collect()
        };
        let n = scored.len();
        let k = k.min(n);
        // Partial sort: only the top k need to be ordered, which matters when the
        // candidate set is large.
        let pivot = k.saturating_sub(1).min(n - 1);
        scored.select_nth_unstable_by(pivot, |a, b| b.1.total_cmp(&a.1));
        scored.truncate(k);
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        scored
    }
}

/// Per-connection cache of the last-built vector index.
///
/// The MCP server answers many recalls over one long-lived connection, and at
/// ~100k facts re-reading every vector from SQLite on each one becomes the
/// bottleneck. This keeps the most recently built index around and reuses it
/// while the store provably unchanged.
///
/// Validity is decided by `PRAGMA data_version` read on the *same* connection:
/// it changes when any other connection commits — other processes (hooks, CLI)
/// included — and not for writes made by this one. So external writes
/// invalidate automatically, and the server must call [`VecCache::invalidate`]
/// after its own `remember`/`forget`, which `data_version` cannot see.
///
/// One slot is enough. An agent tends to repeat the same question set, so
/// consecutive recalls hit; a key change just rebuilds, which is no worse than
/// today.
#[derive(Debug, Default)]
pub struct VecCache {
    /// The key the cached index was built for, if any.
    key: Option<(Vec<i64>, When)>,
    /// `data_version` of this connection when the index was built.
    version: i64,
    idx: Option<VecIndex>,
}

impl VecCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop whatever is cached. The server calls this after a write on this
    /// connection, which `data_version` does not reflect.
    pub fn invalidate(&mut self) {
        self.key = None;
        self.idx = None;
    }

    /// The index for these scopes at `when`: the cached one when it is fresh,
    /// otherwise a reload that replaces the cache.
    pub fn index(&mut self, conn: &Connection, scope_ids: &[i64], when: When) -> Result<&VecIndex> {
        let version = data_version(conn)?;
        let key = (scope_ids.to_vec(), when);
        let fresh =
            self.key.as_ref() == Some(&key) && self.version == version && self.idx.is_some();
        if !fresh {
            self.idx = Some(VecIndex::load_facts(conn, scope_ids, when)?);
            self.key = Some(key);
            self.version = version;
        }
        Ok(self.idx.as_ref().expect("just set"))
    }
}

/// `PRAGMA data_version`: changes when another connection commits to the same
/// database file; unchanged for writes on this connection. Only meaningful
/// compared between two reads on the same connection, which is all we do.
fn data_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA data_version", [], |r| r.get(0))?)
}

/// Fetch vectors for a specific set of ids. Used by MMR, which needs random
/// access to a few dozen candidates rather than a full scan.
pub fn load_vecs(
    conn: &Connection,
    kind: i64,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<i8>>> {
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    let sql = format!(
        "SELECT ref_id, q FROM vecs WHERE kind = ?1 AND ref_id IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut st = conn.prepare_cached(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&kind];
    params.extend(ids.iter().map(|i| i as &dyn rusqlite::ToSql));
    let mut rows = st.query(params.as_slice())?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let blob: Vec<u8> = r.get(1)?;
        out.insert(id, from_blob(&blob));
    }
    Ok(out)
}

/// Store (or replace) a quantized vector.
pub fn put_vec(conn: &Connection, kind: i64, ref_id: i64, q: &[i8]) -> Result<()> {
    conn.execute(
        "INSERT INTO vecs(kind, ref_id, q) VALUES(?1, ?2, ?3)
         ON CONFLICT(kind, ref_id) DO UPDATE SET q = excluded.q",
        rusqlite::params![kind, ref_id, to_blob(q)],
    )?;
    Ok(())
}

/// True if the model behind stored vectors differs from the configured one, in
/// which case scores would be meaningless and a reindex is required.
pub fn model_matches(conn: &Connection, cfg: &Config, dim: usize) -> Result<bool> {
    let want = format!("{}:{}", cfg.model, dim);
    match db::meta_get(conn, "embed_model")? {
        Some(got) => Ok(got == want),
        None => {
            db::meta_set(conn, "embed_model", &want)?;
            Ok(true)
        }
    }
}

/// Path helper for `doctor` and `model which`.
pub fn describe_model_location(cfg: &Config) -> String {
    if is_installed(cfg) {
        let bytes: u64 = [TOKENIZER, WEIGHTS]
            .iter()
            .chain(CONFIGS.iter())
            .filter_map(|f| std::fs::metadata(cfg.model_dir().join(f)).ok())
            .map(|m| m.len())
            .sum();
        return format!(
            "{} ({:.0} MB)",
            cfg.model_dir().display(),
            bytes as f64 / 1_048_576.0
        );
    }
    if Path::new(&cfg.model).is_dir() {
        return format!("{} (local folder)", cfg.model);
    }
    "not downloaded".to_string()
}

/// `install` warms the model so the first recall is not the one that waits.
pub fn prefetch(cfg: &Config) -> Result<usize> {
    let e = Embedder::load(cfg)?;
    Ok(e.dim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope;

    #[test]
    fn quantize_roundtrip_preserves_similarity() {
        let a = vec![1.0f32, 0.0, 0.0, 0.0];
        let b = vec![1.0f32, 0.0, 0.0, 0.0];
        let c = vec![0.0f32, 1.0, 0.0, 0.0];
        assert!((cosine_q(&quantize(&a), &quantize(&b)) - 1.0).abs() < 1e-3);
        assert!(cosine_q(&quantize(&a), &quantize(&c)).abs() < 1e-3);
    }

    #[test]
    fn quantize_handles_zero_vector() {
        let z = vec![0.0f32; 8];
        let q = quantize(&z);
        assert!(q.iter().all(|&x| x == 0));
        assert_eq!(cosine_q(&q, &q), 0.0);
    }

    #[test]
    fn blob_roundtrip_keeps_negatives() {
        let q: Vec<i8> = vec![-128, -1, 0, 1, 127];
        assert_eq!(from_blob(&to_blob(&q)), q);
    }

    #[test]
    fn topk_ranks_and_truncates() {
        let dim = 4;
        let idx = VecIndex {
            dim,
            ids: vec![10, 20, 30],
            data: [
                quantize(&[1.0, 0.0, 0.0, 0.0]),
                quantize(&[0.9, 0.1, 0.0, 0.0]),
                quantize(&[0.0, 0.0, 1.0, 0.0]),
            ]
            .concat(),
        };
        let got = idx.topk(&quantize(&[1.0, 0.0, 0.0, 0.0]), 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 10);
        assert_eq!(got[1].0, 20);
    }

    /// Regression test for the bug where the vector leg ignored the temporal
    /// filter, so `--as-of` could never see a retracted fact and always leaked
    /// the currently-live one instead.
    #[test]
    fn load_facts_respects_the_temporal_filter() {
        use crate::config::now;
        use crate::{db, scope, store};
        use std::path::Path;

        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-embed-when"), Path::new("/")).unwrap();
        let mk = |dst: &str| store::RememberInput {
            text: format!("the project uses {dst}"),
            kind: "decision".into(),
            source: "test".into(),
            facts: vec![store::FactInput {
                src: Some("project".into()),
                rel: "uses".into(),
                dst: Some(dst.into()),
                statement: format!("the project uses {dst}"),
                valid_from: None,
                valid_to: None,
                confidence: 1.0,
                supersede: None,
            }],
            meta: None,
            derive: true,
        };
        let old = store::remember(&mut conn, &sc, None, &mk("npm"))
            .unwrap()
            .fact_ids[0];
        let t_mid = now();
        let new = store::remember(&mut conn, &sc, None, &mk("pnpm"))
            .unwrap()
            .fact_ids[0];

        // Vectors, added by hand since no model is loaded in tests.
        for id in [old, new] {
            put_vec(&conn, db::VEC_FACT, id, &quantize(&[1.0, 0.0, 0.0, 0.0])).unwrap();
        }

        let live = VecIndex::load_facts(&conn, &[sc.id], When::Live).unwrap();
        assert_eq!(live.ids, vec![new], "live must exclude the retracted fact");

        let past = VecIndex::load_facts(&conn, &[sc.id], When::AsOf(t_mid)).unwrap();
        assert_eq!(
            past.ids,
            vec![old],
            "as_of must see only what was true then"
        );

        let any = VecIndex::load_facts(&conn, &[sc.id], When::Any).unwrap();
        assert_eq!(any.ids.len(), 2);
    }

    #[test]
    fn topk_on_empty_index_is_empty() {
        let idx = VecIndex {
            dim: 0,
            ids: vec![],
            data: vec![],
        };
        assert!(idx.topk(&[1, 2, 3], 5).is_empty());
    }

    /// The cache serves the same index while nothing else writes, and reloads
    /// as soon as a *different* connection commits (which is how the hook and
    /// the CLI make writes visible to the long-lived MCP server).
    #[test]
    fn vec_cache_reloads_when_another_connection_writes() {
        use crate::store::{self, FactInput, RememberInput};
        use std::path::Path;

        let dir = std::env::temp_dir().join(format!(
            "fm-vcache-{}-{}",
            std::process::id(),
            std::sync::atomic::AtomicU32::new(0).fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let path = dir.join("store.sqlite");
        let mk = |conn: &mut Connection, dst: &str| -> i64 {
            let sc = scope::resolve(conn, Some("/tmp/fm-vcache"), Path::new("/")).unwrap();
            let out = store::remember(
                conn,
                &sc,
                None,
                &RememberInput {
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
                    meta: None,
                    derive: true,
                },
            )
            .unwrap();
            let id = out.fact_ids[0];
            put_vec(conn, db::VEC_FACT, id, &quantize(&[1.0, 0.0, 0.0, 0.0])).unwrap();
            id
        };

        let mut first = db::open(&path).unwrap();
        let sc = scope::resolve(&first, Some("/tmp/fm-vcache"), Path::new("/")).unwrap();
        let a = mk(&mut first, "npm");
        let mut cache = VecCache::new();

        let idx = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert_eq!(idx.ids, vec![a]);
        let first_ver = cache.version;
        assert_ne!(first_ver, 0, "data_version must be non-zero on a real file");

        // Another connection writing is the only thing data_version sees.
        let mut other = db::open(&path).unwrap();
        let b = mk(&mut other, "pnpm");

        // The value of data_version changed, so the next read must reload.
        let idx2 = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(
            idx2.ids.contains(&b),
            "external write must invalidate the cache: {:?}",
            idx2.ids
        );
        // And it stays cached now — a second read must not change the version.
        let ver_after = cache.version;
        assert_ne!(ver_after, first_ver);
        let idx3 = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(idx3.ids.contains(&b));
        assert_eq!(cache.version, ver_after, "no rewrite when nothing changed");

        // Our own connection writing does NOT touch data_version, so the cache
        // stays as it was — the server invalidates explicitly in that case.
        let c = mk(&mut first, "yarn");
        {
            let idx4 = cache.index(&first, &[sc.id], When::Live).unwrap();
            assert!(
                !idx4.ids.contains(&c),
                "explicit invalidate() is the contract"
            );
        }
        assert_eq!(
            cache.version, ver_after,
            "own writes don't move data_version"
        );

        cache.invalidate();
        let idx5 = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(idx5.ids.contains(&c), "invalidate must force a reload");

        std::fs::remove_dir_all(&dir).ok();
    }
}
