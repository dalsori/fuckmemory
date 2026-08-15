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
///
/// The storage is either an owned `Vec<i8>` (built from SQLite) or a memory
/// map of a [`VecIndexFile`] — same bytes, but the mapped form costs an open
/// and a few page faults instead of a full `SELECT` + parse of every vector,
/// which is what a one-shot process (autosave hook, CLI recall) pays on each
/// invocation.
#[derive(Debug)]
pub struct VecIndex {
    pub dim: usize,
    pub ids: Vec<i64>,
    data: IndexData,
}

#[derive(Debug)]
enum IndexData {
    Owned(Vec<i8>),
    /// The full file mapping, plus the byte offset where the vector rows start.
    /// The mapping covers header + ids + rows; `rows()` slices from `data_off`
    /// so only the matrix is ever treated as vectors.
    Mapped {
        map: memmap2::Mmap,
        data_off: usize,
    },
}

impl IndexData {
    /// The flat row matrix as `&[i8]`. `u8` and `i8` share layout and
    /// representation, so a mapped byte slice is a valid `&[i8]` of the same
    /// length; the header validation guarantees the slice is exactly
    /// `count * dim` bytes before any read is handed out.
    fn rows(&self) -> &[i8] {
        match self {
            IndexData::Owned(v) => v,
            IndexData::Mapped { map, data_off } => {
                // SAFETY: u8 and i8 have identical layout; the mapping was sized
                // and validated against the header before being stored, and
                // `data_off` points at the start of the row matrix.
                unsafe {
                    std::slice::from_raw_parts(
                        map.as_ptr().add(*data_off).cast::<i8>(),
                        map.len() - *data_off,
                    )
                }
            }
        }
    }
}

impl VecIndex {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn from_parts(dim: usize, ids: Vec<i64>, data: Vec<i8>) -> Self {
        Self {
            dim,
            ids,
            data: IndexData::Owned(data),
        }
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
            // Borrow the blob instead of materializing a Vec<u8> per row: at
            // 100k facts that is 100k short-lived allocations on the cold path
            // (a persisted-index rebuild or a fresh one-shot recall).
            let blob = r.get_ref(1)?.as_blob()?;
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
        Ok(Self::from_parts(dim, ids, data))
    }

    /// Load the episode vectors for the given scopes. Episodes are immutable
    /// observations, so there is no temporal filter — every episode's vector is
    /// live. Powers semantic search over raw notes (`recall --raw`).
    pub fn load_episodes(conn: &Connection, scope_ids: &[i64]) -> Result<Self> {
        let placeholders = vec!["?"; scope_ids.len()].join(",");
        let sql = format!(
            "SELECT v.ref_id, v.q FROM vecs v
             JOIN episodes e ON e.id = v.ref_id
             WHERE v.kind = {kind} AND e.scope_id IN ({placeholders})",
            kind = db::VEC_EPISODE,
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
            let blob = r.get_ref(1)?.as_blob()?;
            if dim == 0 {
                dim = blob.len();
            }
            if blob.len() != dim {
                continue;
            }
            ids.push(id);
            data.extend(blob.iter().map(|&x| x as i8));
        }
        Ok(Self::from_parts(dim, ids, data))
    }

    /// Wrap a validated [`VecIndexFile`] mapping as an index. Returns `None`
    /// when the mapping does not match the file header's expectations — the
    /// caller then falls back to `load_facts`.
    fn from_mapped(file: &VecIndexFile, mmap: memmap2::Mmap) -> Option<Self> {
        let n = file.count as usize;
        let dim = file.dim as usize;
        let ids_off = file.ids_off;
        let data_off = file.data_off;
        let expected = data_off + n * dim;
        if mmap.len() < expected {
            return None;
        }
        // Copy the ids (a few hundred KB at 100k facts); the vectors stay
        // mapped and are only touched for the rows a query actually pools.
        let ids: Vec<i64> = {
            let bytes = &mmap[ids_off..data_off];
            // SAFETY: validated alignment (ids start at offset 64) and length
            // (`n * 8` fits before data_off, itself validated above).
            let slice = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<i64>(), n) };
            slice.to_vec()
        };
        Some(Self {
            dim,
            ids,
            data: IndexData::Mapped {
                map: mmap,
                data_off,
            },
        })
    }

    #[inline]
    pub fn row(&self, i: usize) -> &[i8] {
        &self.data.rows()[i * self.dim..(i + 1) * self.dim]
    }

    /// Top-`k` by cosine. Returns `(id, score)` descending.
    ///
    /// Keeps a bounded `BinaryHeap` of size `k` instead of scoring and sorting
    /// all `n` rows: memory stays O(k), and the sort is O(n log k) rather than
    /// a partial sort of the whole candidate set. With rayon the chunks fold
    /// their own heaps and reduce pairwise, so the full `Vec<(i64, f32)>` of
    /// every row is never materialized.
    pub fn topk(&self, query: &[i8], k: usize) -> Vec<(i64, f32)> {
        if self.ids.is_empty() || self.dim == 0 || query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let rows = self.data.rows();
        let mut scored = if self.ids.len() >= 4_096 {
            use rayon::prelude::*;
            let partial = |(i, row): (usize, &[i8])| (self.ids[i], cosine_q(query, row));
            rows.par_chunks(self.dim)
                .enumerate()
                .map(partial)
                .fold(
                    || BoundedTopK::new(k),
                    |mut acc, (id, s)| {
                        acc.push(id, s);
                        acc
                    },
                )
                .reduce(
                    || BoundedTopK::new(k),
                    |mut a, mut b| {
                        let mut out = BoundedTopK::new(k);
                        for (id, s) in a.drain() {
                            out.push(id, s);
                        }
                        for (id, s) in b.drain() {
                            out.push(id, s);
                        }
                        out
                    },
                )
                .into_vec()
        } else {
            let mut acc = BoundedTopK::new(k);
            for (i, row) in rows.chunks(self.dim).enumerate() {
                acc.push(self.ids[i], cosine_q(query, row));
            }
            acc.into_vec()
        };
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        scored
    }
}

/// A min-heap that keeps only the top-`k` scored ids, used by [`VecIndex::topk`]
/// to bound memory to `k` entries instead of one per vector. `Scored` orders by
/// score (ascending, via `total_cmp`), and the heap is wrapped in `Reverse` so
/// the root is the *worst* candidate: an overflow evicts it.
#[derive(Clone, Copy)]
struct Scored {
    id: i64,
    score: f32,
}

impl PartialEq for Scored {
    fn eq(&self, other: &Self) -> bool {
        self.score.total_cmp(&other.score) == std::cmp::Ordering::Equal
    }
}
impl Eq for Scored {}
impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scored {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score.total_cmp(&other.score)
    }
}

struct BoundedTopK {
    k: usize,
    heap: std::collections::BinaryHeap<std::cmp::Reverse<Scored>>,
}

impl BoundedTopK {
    fn new(k: usize) -> Self {
        Self {
            k,
            heap: std::collections::BinaryHeap::new(),
        }
    }

    /// Insert one scored id. The heap holds at most `k`; the worst element is
    /// dropped when it overflows, so the survivor set is the top-`k` seen so far.
    fn push(&mut self, id: i64, score: f32) {
        use std::cmp::Reverse;
        let cand = Scored { id, score };
        if self.heap.len() < self.k {
            self.heap.push(Reverse(cand));
        } else if let Some(Reverse(worst)) = self.heap.peek().copied() {
            if score > worst.score {
                self.heap.pop();
                self.heap.push(Reverse(cand));
            }
        }
    }

    /// Drain the survivors as `(id, score)` in arbitrary order.
    fn drain(&mut self) -> Vec<(i64, f32)> {
        use std::cmp::Reverse;
        self.heap
            .drain()
            .map(|Reverse(s)| (s.id, s.score))
            .collect()
    }

    fn into_vec(self) -> Vec<(i64, f32)> {
        use std::cmp::Reverse;
        self.heap
            .into_iter()
            .map(|Reverse(s)| (s.id, s.score))
            .collect()
    }
}

/// On-disk form of a [`VecIndex`], so a one-shot process can open it with mmap
/// instead of re-reading every vector out of SQLite.
///
/// Layout (all little-endian):
///
/// ```text
/// offset 0   magic   b"FMVECv01"  (8 bytes)
/// offset 8   dim     u32
/// offset 12  count   u32
/// offset 16  version i64   — the `index_version` the index was built from
/// offset 24  key     [u8; 32]  — blake3 of (scope_ids, when)
/// offset 56  layout  u32
/// offset 60  reserved u32
/// offset 64  ids     i64 × count
/// offset 64 + 8·count  data  i8 × count × dim
/// ```
///
/// The header is fixed and the ids start on an 8-byte boundary, which is what
/// makes the byte-slice-to-`i64` cast in [`VecIndex::from_mapped`] sound.
const VEC_MAGIC: &[u8; 8] = b"FMVECv01";
const VEC_HEADER: usize = 64;
/// Layout version. Bump when the byte layout below changes; an older file is
/// simply rebuilt on first use.
const VEC_LAYOUT: u32 = 1;

#[derive(Debug, Clone, Copy)]
struct VecIndexFile {
    dim: u32,
    count: u32,
    /// `index_version` this index was built against.
    version: i64,
    /// blake3 of the scope set + temporal window, so a different project or an
    /// `--as-of` query never reuses another query's index.
    key: [u8; 32],
    ids_off: usize,
    data_off: usize,
}

impl VecIndexFile {
    /// Parse and validate a header from the start of a mapping. `None` for
    /// anything that does not match the layout or that would make the reads
    /// below unsafe.
    fn read(buf: &[u8]) -> Option<Self> {
        if buf.len() < VEC_HEADER || &buf[0..8] != VEC_MAGIC {
            return None;
        }
        let dim = u32::from_le_bytes(buf[8..12].try_into().ok()?) as usize;
        let count = u32::from_le_bytes(buf[12..16].try_into().ok()?) as usize;
        let version = i64::from_le_bytes(buf[16..24].try_into().ok()?);
        let layout = u32::from_le_bytes(buf[56..60].try_into().ok()?);
        if layout != VEC_LAYOUT || dim == 0 || count == 0 {
            return None;
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&buf[24..56]);

        let ids_off = VEC_HEADER;
        let data_off = ids_off + count * 8;
        let total = data_off + count * dim;
        // Overflow-guard the arithmetic; with realistic counts (≤ millions) this
        // never fires, but a corrupt header must not turn into a huge offset.
        if data_off < ids_off || total < data_off || buf.len() < total {
            return None;
        }
        Some(Self {
            dim: dim as u32,
            count: count as u32,
            version,
            key,
            ids_off,
            data_off,
        })
    }

    /// Serialize an in-memory index to `path` atomically (temp file + rename),
    /// so a crash can't leave a half-written index that later opens as valid.
    fn write(path: &std::path::Path, idx: &VecIndex, version: i64, key: [u8; 32]) -> Result<()> {
        let data = idx.data.rows();
        let dim = idx.dim as u32;
        let count = idx.ids.len() as u32;
        let mut buf = Vec::with_capacity(VEC_HEADER + idx.ids.len() * 8 + data.len());
        buf.extend_from_slice(VEC_MAGIC);
        buf.extend_from_slice(&dim.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&key);
        buf.extend_from_slice(&VEC_LAYOUT.to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]); // reserved, keep header 56 bytes
        for id in &idx.ids {
            buf.extend_from_slice(&id.to_le_bytes());
        }
        // i8 and u8 share representation; copying bytes is exact.
        buf.extend_from_slice(unsafe {
            std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len())
        });

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating index cache dir {}", parent.display()))?;
        }
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, &buf)
            .with_context(|| format!("writing index cache {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("installing index cache {}", path.display()))?;
        Ok(())
    }
}

/// Per-connection cache of the last-built vector index.
///
/// The MCP server answers many recalls over one long-lived connection, and at
/// ~100k facts re-reading every vector from SQLite on each one becomes the
/// bottleneck. This keeps the most recently built index around and reuses it
/// while the store provably unchanged.
///
/// Validity is decided by `index_version` read on the *same* connection: it
/// changes when another connection commits fact-vector content — other
/// processes (hooks, CLI) included — and not for writes made by this one. So
/// external writes invalidate automatically, and the server must call
/// [`VecCache::invalidate`] after its own `remember`/`forget`, which the
/// version bump on this connection does not propagate to the cached slot.
///
/// One slot is enough. An agent tends to repeat the same question set, so
/// consecutive recalls hit; a key change just rebuilds, which is no worse than
/// today.
///
/// With [`VecCache::with_dir`], the index is also written to disk (mmap'd on
/// open) so a one-shot process — an autosave hook, a CLI recall — pays the
/// SQLite read once per store version instead of on every single invocation.
/// A file for a stale `index_version` or a different key is ignored and
/// rebuilt, exactly like the in-memory slot.
#[derive(Debug, Default)]
pub struct VecCache {
    /// The key the cached index was built for, if any.
    key: Option<(Vec<i64>, When)>,
    /// `index_version` of this connection when the index was built.
    version: i64,
    idx: Option<VecIndex>,
    /// Where to persist the index, if anywhere.
    dir: Option<std::path::PathBuf>,
}

impl VecCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist indexes under `dir` as well as keeping the in-memory slot.
    pub fn with_dir(dir: std::path::PathBuf) -> Self {
        Self {
            dir: Some(dir),
            ..Self::default()
        }
    }

    /// Drop whatever is cached. The server calls this after a write on this
    /// connection, which `data_version` does not reflect. The on-disk file is
    /// left alone: its own version stamp will reject it on the next open.
    pub fn invalidate(&mut self) {
        self.key = None;
        self.idx = None;
    }

    /// The index for these scopes at `when`: the cached one when it is fresh,
    /// otherwise a reload that replaces the cache. Validity is decided by
    /// `index_version` rather than `PRAGMA data_version`: the latter changes on
    /// *any* write from another connection, including the `mark_hits` popularity
    /// counter every autosave prompt performs — which would rebuild the mmap
    /// cache on every prompt even when no fact changed. `index_version` only
    /// moves when the facts behind the index actually change.
    pub fn index(&mut self, conn: &Connection, scope_ids: &[i64], when: When) -> Result<&VecIndex> {
        let version = db::index_version(conn)?;
        let key = (scope_ids.to_vec(), when);
        let fresh =
            self.key.as_ref() == Some(&key) && self.version == version && self.idx.is_some();
        if !fresh {
            self.idx = Some(self.load(conn, scope_ids, when, version)?);
            self.key = Some(key);
            self.version = version;
        }
        Ok(self.idx.as_ref().expect("just set"))
    }

    /// Build the index for `(scope_ids, when)` at `version`, preferring the
    /// on-disk cache when one exists and is valid.
    fn load(
        &self,
        conn: &Connection,
        scope_ids: &[i64],
        when: When,
        version: i64,
    ) -> Result<VecIndex> {
        if let Some(dir) = &self.dir {
            if let Some(file) = self.open_cached(dir, scope_ids, when, version) {
                return Ok(file);
            }
        }
        let idx = VecIndex::load_facts(conn, scope_ids, when)?;
        if let Some(dir) = &self.dir {
            // Best-effort: a write failure is not worth failing the query over.
            let _ = VecIndexFile::write(
                &self.cache_path(dir, scope_ids, when),
                &idx,
                version,
                key_hash(scope_ids, when),
            );
        }
        Ok(idx)
    }

    /// Open the persisted index for `(scope_ids, when)` if it exists and was
    /// built against `version` (the current `index_version`).
    fn open_cached(
        &self,
        dir: &std::path::Path,
        scope_ids: &[i64],
        when: When,
        version: i64,
    ) -> Option<VecIndex> {
        let path = self.cache_path(dir, scope_ids, when);
        let file = std::fs::File::open(&path).ok()?;
        // SAFETY: the file is written atomically (temp + rename) and only ever
        // read; a concurrent rebuild replaces the inode rather than editing it.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        let hdr = VecIndexFile::read(&mmap)?;
        if hdr.version != version || hdr.key != key_hash(scope_ids, when) {
            return None;
        }
        VecIndex::from_mapped(&hdr, mmap)
    }

    /// The cache file for a key. The key hash goes into the filename so a
    /// different project or window never collides on the same file.
    fn cache_path(
        &self,
        dir: &std::path::Path,
        scope_ids: &[i64],
        when: When,
    ) -> std::path::PathBuf {
        dir.join(format!("vec-{}.fm2", hex16(&key_hash(scope_ids, when))))
    }
}

/// blake3 of the cache key (scope set + temporal window).
///
/// The full timestamp is hashed, never the truncated `tag()` byte: `tag()`
/// reduces `t % 255` (a ~17-day cycle), so two `--as-of` dates landing in the
/// same bucket would otherwise share a cache file and serve one another's
/// historical snapshot.
fn key_hash(scope_ids: &[i64], when: When) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for s in scope_ids {
        h.update(&s.to_le_bytes());
    }
    match when {
        When::Live => h.update(&[0u8]),
        When::Any => h.update(&[2u8]),
        When::AsOf(t) => {
            h.update(&[1u8]);
            h.update(&t.to_le_bytes())
        }
    };
    let mut out = [0u8; 32];
    out.copy_from_slice(h.finalize().as_bytes());
    out
}

/// First 16 bytes of a hash as hex, for filenames.
fn hex16(h: &[u8; 32]) -> String {
    h[..16].iter().map(|b| format!("{b:02x}")).collect()
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
    // A fact vector is exactly what the persisted index serves; episode vectors
    // are only read live, so they must not bump the cache version.
    if kind == db::VEC_FACT {
        db::bump_index_version(conn)?;
    }
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
        let idx = VecIndex::from_parts(
            dim,
            vec![10, 20, 30],
            [
                quantize(&[1.0, 0.0, 0.0, 0.0]),
                quantize(&[0.9, 0.1, 0.0, 0.0]),
                quantize(&[0.0, 0.0, 1.0, 0.0]),
            ]
            .concat(),
        );
        let got = idx.topk(&quantize(&[1.0, 0.0, 0.0, 0.0]), 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 10);
        assert_eq!(got[1].0, 20);
    }

    #[test]
    fn index_file_roundtrips_and_survives_a_stale_version() {
        let dir = std::env::temp_dir().join(format!("fm-vec-{}", std::process::id()));
        let path = dir.join("vec-abc.fm2");
        let idx = VecIndex::from_parts(
            4,
            vec![1, 2, 3],
            [
                quantize(&[1.0, 0.0, 0.0, 0.0]),
                quantize(&[0.0, 1.0, 0.0, 0.0]),
                quantize(&[0.0, 0.0, 1.0, 0.0]),
            ]
            .concat(),
        );
        let key = [7u8; 32];
        VecIndexFile::write(&path, &idx, 42, key).unwrap();

        // Fresh version, matching key: readable, same topk results.
        let f = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&f) }.unwrap();
        let hdr = VecIndexFile::read(&mmap).expect("valid header");
        assert_eq!(hdr.version, 42);
        assert_eq!(hdr.key, key);
        let loaded = VecIndex::from_mapped(&hdr, mmap).expect("valid index");
        let got = loaded.topk(&quantize(&[1.0, 0.0, 0.0, 0.0]), 1);
        assert_eq!(got[0].0, 1, "mapped index ranks like the owned one");

        // Stale version: must be rejected so the caller rebuilds.
        let f = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&f) }.unwrap();
        let hdr = VecIndexFile::read(&mmap).unwrap();
        assert_ne!(hdr.version, 43, "sanity");
        // The file itself still opens; rejection happens in VecCache by version.
        let _ = VecIndex::from_mapped(&hdr, mmap);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn index_file_rejects_corrupt_headers() {
        let dir = std::env::temp_dir().join(format!("fm-vec-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.fm2");

        // Truncated file.
        std::fs::write(&path, b"F").unwrap();
        let f = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&f) }.unwrap();
        assert!(VecIndexFile::read(&mmap).is_none(), "too short must fail");

        // Wrong magic.
        std::fs::write(&path, b"NOTVECv1").unwrap();
        let f = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&f) }.unwrap();
        assert!(VecIndexFile::read(&mmap).is_none(), "bad magic must fail");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vec_cache_uses_and_rebuilds_the_disk_cache() {
        use crate::db;
        use std::path::Path;

        let dir = std::env::temp_dir().join(format!("fm-vc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-vc"), Path::new("/")).unwrap();
        let scope_ids = scope::read_set(&conn, &sc).unwrap();

        // A stale file (version 5) must be rejected; the fresh empty store has
        // no vectors, so the rebuilt index is empty too — but it gets written
        // back to disk for next time.
        let idx = VecIndex::from_parts(2, vec![1, 2], vec![1, 0, 0, 1]);
        VecIndexFile::write(
            &dir.join("vec-x.fm2"),
            &idx,
            5,
            key_hash(&scope_ids, When::Live),
        )
        .unwrap();

        let mut cache = VecCache::with_dir(dir.clone());
        let idx = cache.index(&conn, &scope_ids, When::Live).unwrap();
        assert!(
            idx.ids.is_empty(),
            "fresh store has no vectors: {:?}",
            idx.ids
        );

        // A second cache on the same dir should be able to read the file back,
        // proving the write path produces something readable.
        let mut cache2 = VecCache::with_dir(dir.clone());
        let idx2 = cache2.index(&conn, &scope_ids, When::Live).unwrap();
        assert_eq!(idx2.ids, idx.ids);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The `--as-of` cache key must include the full timestamp: `When::tag()`
    /// truncates to `t % 255` (a ~17-day cycle), so two dates in the same bucket
    /// used to collide on one cache file and serve each other's snapshot.
    #[test]
    fn as_of_cache_keys_do_not_collide_across_buckets() {
        let a = key_hash(&[1], When::AsOf(1_700_000_000_000));
        let b = key_hash(&[1], When::AsOf(1_700_000_000_000 + 86_400_000 * 17));
        assert_ne!(a, b, "17 days apart must produce different cache keys");
        assert_ne!(
            key_hash(&[1], When::Live),
            key_hash(&[1], When::Any),
            "Live and Any must stay distinct"
        );
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
            files: vec![],
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

    /// Episode vectors are written on autosave but must be loadable back for the
    /// semantic `--raw` search; previously nothing read them and `reindex`
    /// silently left them from an old model.
    #[test]
    fn load_episodes_reads_the_written_episode_vectors() {
        use crate::store::{self, RememberInput};
        use std::path::Path;

        let mut conn = db::open_memory().unwrap();
        let sc = scope::resolve(&conn, Some("/tmp/fm-embed-ep"), Path::new("/")).unwrap();
        let out = store::remember(
            &mut conn,
            &sc,
            None,
            &RememberInput {
                text: "the deploy pipeline runs on node 22".into(),
                kind: "note".into(),
                source: "test".into(),
                facts: vec![],
                files: vec![],
                meta: None,
                derive: false,
            },
        )
        .unwrap();

        // No model in tests, so the episode vector was skipped — add it by hand
        // to prove the read path finds it.
        put_vec(
            &conn,
            db::VEC_EPISODE,
            out.episode_id,
            &quantize(&[1.0, 0.0, 0.0, 0.0]),
        )
        .unwrap();

        let idx = VecIndex::load_episodes(&conn, &[sc.id]).unwrap();
        assert_eq!(
            idx.ids,
            vec![out.episode_id],
            "episode vector must be readable"
        );
        assert_eq!(idx.dim, 4);
    }

    #[test]
    fn topk_on_empty_index_is_empty() {
        let idx = VecIndex::from_parts(0, vec![], vec![]);
        assert!(idx.topk(&[1, 2, 3], 5).is_empty());
    }

    /// The bounded heap must keep exactly the top-`k` ids by score, dropping the
    /// worst when it overflows, and stay exact no matter the insertion order.
    #[test]
    fn bounded_topk_keeps_only_the_highest_scores() {
        let mut h = BoundedTopK::new(3);
        for (id, score) in [(1, 0.2), (2, 0.9), (3, 0.5), (4, 0.8), (5, 0.1)] {
            h.push(id, score);
        }
        let mut out = h.into_vec();
        out.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        assert_eq!(
            out,
            vec![(2, 0.9), (4, 0.8), (3, 0.5)],
            "only the top 3 survive"
        );
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
                    files: vec![],
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
        assert_ne!(first_ver, 0, "index_version must be non-zero after a fact");

        // Another connection writing a fact vector bumps index_version, so the
        // next read must reload.
        let mut other = db::open(&path).unwrap();
        let b = mk(&mut other, "pnpm");
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

        // A hits-only write (what mark_hits does) must NOT invalidate the
        // cache: it changes no vector the index serves, and doing so used to
        // rebuild the persisted mmap file on every autosave prompt.
        let hit_ver_before = cache.version;
        store::mark_hits(&first, &[a]).unwrap();
        let idx_h = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(
            !idx_h.ids.is_empty() && cache.version == hit_ver_before,
            "mark_hits must not invalidate the index cache"
        );

        // Our own connection writing a fact vector DOES bump index_version now
        // (the server calls invalidate() too, but the version alone reloads).
        let c = mk(&mut first, "yarn");
        let idx4 = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(idx4.ids.contains(&c), "own write must reload the cache");

        cache.invalidate();
        let idx5 = cache.index(&first, &[sc.id], When::Live).unwrap();
        assert!(idx5.ids.contains(&c), "invalidate must force a reload");

        std::fs::remove_dir_all(&dir).ok();
    }
}
