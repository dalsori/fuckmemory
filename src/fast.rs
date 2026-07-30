//! A cold-start-free embedding path.
//!
//! Loading the model through `model2vec-rs` costs ~206 ms: it reads a 129 MB f32
//! safetensors file, converts every value into a `Vec<f32>`, and builds a
//! `tokenizers::Tokenizer` out of 1.5 MB of JSON. Encoding one query then takes
//! 16 µs, and a whole recall 0.3 ms. So 99% of every short-lived invocation was
//! setup — and autosave hooks fire a process per message, which made that cost
//! the product.
//!
//! This module removes it by precomputing a cache that needs no parsing at all:
//!
//! - The embedding table is stored as f16, one row per **token id** with any
//!   `mapping`/`weights` already folded in, so pooling is a plain row average.
//! - The vocabulary is a byte-sorted index into a string blob, so a token lookup
//!   is a binary search straight against the mapped file — no HashMap to build.
//! - The file is `mmap`ed, so opening it costs a few page faults and a query only
//!   touches the ~20 rows it actually pools.
//!
//! The catch is that we must then tokenize *exactly* as HuggingFace does, or
//! stored vectors and query vectors stop being comparable. So [`build`] verifies
//! the cache against the real model on a probe corpus before installing it, and
//! refuses to write one that disagrees. If anything is off — missing cache, stale
//! source files, a model whose pipeline we don't recognise — callers fall back to
//! the slow path, which is always still correct.

use anyhow::{Context, Result};
use half::f16;
use memmap2::Mmap;
use std::path::{Path, PathBuf};
use unicode_categories::UnicodeCategories;
use unicode_normalization_alignments::UnicodeNormalization;

use crate::config::Config;

/// Bumped when the layout below changes; an older file is then simply rebuilt.
const MAGIC: &[u8; 8] = b"FMFASTv1";
const HEADER_LEN: usize = 256;
/// The matrix starts on a page boundary so a row read is one mapped page.
const MATRIX_OFF: u64 = 4096;
/// model2vec's own encode defaults, which we have to match to stay comparable.
const MAX_TOKENS: usize = 512;

const F_CLEAN_TEXT: u32 = 1 << 0;
const F_CHINESE: u32 = 1 << 1;
const F_STRIP_ACCENTS: u32 = 1 << 2;
const F_LOWERCASE: u32 = 1 << 3;
const F_NORMALIZE: u32 = 1 << 4;

pub fn cache_path(cfg: &Config) -> PathBuf {
    cfg.model_dir().join("fastembed.fm1")
}

/// Files the cache is derived from. Their length and mtime are the staleness
/// check: hashing 129 MB would cost more than the load we are trying to avoid.
const SOURCES: [&str; 3] = ["tokenizer.json", "model.safetensors", "config.json"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime_ms: i64,
}

fn stamp(path: &Path) -> Result<Stamp> {
    let m = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime_ms = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(Stamp {
        len: m.len(),
        mtime_ms,
    })
}

/// Which of the three source files the cache was built from, in `SOURCES` order.
/// A model folder can hold `config.json` or `config_sentence_transformers.json`;
/// the resolved name is stored so the stamp compares like for like.
fn source_paths(dir: &Path, config_name: &str) -> [PathBuf; 3] {
    [
        dir.join(SOURCES[0]),
        dir.join(SOURCES[1]),
        dir.join(config_name),
    ]
}

struct Header {
    dim: usize,
    rows: usize,
    vocab_count: usize,
    added_count: usize,
    median_token_length: usize,
    unk_id: Option<u32>,
    flags: u32,
    prefix: Vec<u8>,
    matrix_off: u64,
    index_off: u64,
    added_off: u64,
    blob_off: u64,
    blob_len: u64,
    stamps: [Stamp; 3],
    config_name: String,
}

impl Header {
    fn write(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(MAGIC);
        let u32s = [
            self.dim as u32,
            self.rows as u32,
            self.vocab_count as u32,
            self.added_count as u32,
            self.median_token_length as u32,
            self.unk_id.map(|i| i as i64).unwrap_or(-1) as u32,
            self.flags,
            self.prefix.len() as u32,
        ];
        for v in u32s {
            h.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            self.matrix_off,
            self.index_off,
            self.added_off,
            self.blob_off,
            self.blob_len,
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        for s in self.stamps {
            h.extend_from_slice(&s.len.to_le_bytes());
            h.extend_from_slice(&s.mtime_ms.to_le_bytes());
        }
        let mut prefix = self.prefix.clone();
        prefix.resize(8, 0);
        h.extend_from_slice(&prefix);
        let mut name = self.config_name.clone().into_bytes();
        name.resize(48, 0);
        h.extend_from_slice(&name);
        h.resize(HEADER_LEN, 0);
        h
    }

    fn read(b: &[u8]) -> Option<Self> {
        if b.len() < HEADER_LEN || &b[..8] != MAGIC {
            return None;
        }
        let u32_at = |i: usize| -> u32 {
            let o = 8 + i * 4;
            u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
        };
        let u64_at = |i: usize| -> u64 {
            let o = 8 + 8 * 4 + i * 8;
            u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
        };
        let stamps_off = 8 + 8 * 4 + 5 * 8;
        let stamp_at = |i: usize| -> Stamp {
            let o = stamps_off + i * 16;
            Stamp {
                len: u64::from_le_bytes(b[o..o + 8].try_into().unwrap()),
                mtime_ms: i64::from_le_bytes(b[o + 8..o + 16].try_into().unwrap()),
            }
        };
        let prefix_off = stamps_off + 3 * 16;
        let prefix_len = u32_at(7) as usize;
        if prefix_len > 8 || prefix_off + 8 + 48 > HEADER_LEN {
            return None;
        }
        let name_off = prefix_off + 8;
        let name = &b[name_off..name_off + 48];
        let name_end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let unk_raw = u32_at(5);
        Some(Self {
            dim: u32_at(0) as usize,
            rows: u32_at(1) as usize,
            vocab_count: u32_at(2) as usize,
            added_count: u32_at(3) as usize,
            median_token_length: u32_at(4) as usize,
            unk_id: if unk_raw == u32::MAX {
                None
            } else {
                Some(unk_raw)
            },
            flags: u32_at(6),
            prefix: b[prefix_off..prefix_off + prefix_len].to_vec(),
            matrix_off: u64_at(0),
            index_off: u64_at(1),
            added_off: u64_at(2),
            blob_off: u64_at(3),
            blob_len: u64_at(4),
            stamps: [stamp_at(0), stamp_at(1), stamp_at(2)],
            config_name: String::from_utf8_lossy(&name[..name_end]).into_owned(),
        })
    }
}

/// One vocabulary entry: where its text lives in the blob, and its token id.
const ENTRY: usize = 12;

/// A memory-mapped static embedder.
pub struct Fast {
    map: Mmap,
    hdr: Header,
}

impl Fast {
    /// Open the cache for `cfg`'s model, or `None` when there isn't a usable one.
    ///
    /// Every rejection here is silent and benign: the caller falls back to loading
    /// the real model, which is slower but always right.
    pub fn open(cfg: &Config) -> Option<Self> {
        let path = cache_path(cfg);
        let file = std::fs::File::open(&path).ok()?;
        // SAFETY: the cache is written atomically (temp file + rename) and only
        // ever read. A concurrent rebuild replaces the inode rather than editing
        // it in place, so this mapping stays valid for its whole lifetime.
        let map = unsafe { Mmap::map(&file) }.ok()?;
        let hdr = Header::read(&map)?;

        // Every offset the reader will use is checked against the real file size
        // here, once. Past this point lookups index the mapping directly, and a
        // truncated or corrupt file would otherwise panic — which, in an autosave
        // hook, means a non-zero exit and a blocked prompt.
        let len = map.len() as u64;
        let table_bytes = |n: usize| (n as u64).saturating_mul(ENTRY as u64);
        let ok = hdr.dim > 0
            && hdr.rows > 0
            && hdr.vocab_count > 0
            && hdr.matrix_off >= HEADER_LEN as u64
            && hdr
                .matrix_off
                .saturating_add((hdr.rows as u64).saturating_mul(hdr.dim as u64 * 2))
                <= hdr.index_off
            && hdr.index_off.saturating_add(table_bytes(hdr.vocab_count)) <= hdr.added_off
            && hdr.added_off.saturating_add(table_bytes(hdr.added_count)) <= hdr.blob_off
            && hdr.blob_off.saturating_add(hdr.blob_len) <= len;
        if !ok {
            return None;
        }
        // Source files changed (re-download, different model): rebuild required.
        let dir = cfg.model_dir();
        for (p, want) in source_paths(&dir, &hdr.config_name)
            .iter()
            .zip(hdr.stamps.iter())
        {
            if stamp(p).ok().as_ref() != Some(want) {
                return None;
            }
        }
        Some(Self { map, hdr })
    }

    pub fn dim(&self) -> usize {
        self.hdr.dim
    }

    fn blob(&self) -> &[u8] {
        let a = self.hdr.blob_off as usize;
        &self.map[a..a + self.hdr.blob_len as usize]
    }

    /// One `(token bytes, id)` pair from a table. Bounds were validated in
    /// [`Fast::open`]; a string that still points outside the blob (only possible
    /// from a file corrupted after that check) reads as empty rather than panics.
    fn entry(&self, table_off: u64, i: usize) -> (&[u8], u32) {
        let o = table_off as usize + i * ENTRY;
        let so = u32::from_le_bytes(self.map[o..o + 4].try_into().unwrap()) as usize;
        let sl = u32::from_le_bytes(self.map[o + 4..o + 8].try_into().unwrap()) as usize;
        let id = u32::from_le_bytes(self.map[o + 8..o + 12].try_into().unwrap());
        let blob = self.blob();
        let key = blob.get(so..so.saturating_add(sl)).unwrap_or(&[]);
        (key, id)
    }

    /// Look up `prefix + word` without building that string: the comparison walks
    /// the two pieces in order. Called O(word_len) times per word, so avoiding the
    /// allocation is worth the small amount of hand-rolling.
    fn token_id(&self, prefixed: bool, s: &str) -> Option<u32> {
        let prefix: &[u8] = if prefixed { &self.hdr.prefix } else { &[] };
        let mut lo = 0usize;
        let mut hi = self.hdr.vocab_count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let (key, id) = self.entry(self.hdr.index_off, mid);
            match cmp_split(key, prefix, s.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(id),
            }
        }
        None
    }

    fn row(&self, id: u32) -> Option<&[u8]> {
        let id = id as usize;
        if id >= self.hdr.rows {
            return None;
        }
        let w = self.hdr.dim * 2;
        let o = self.hdr.matrix_off as usize + id * w;
        Some(&self.map[o..o + w])
    }

    /// Token ids for `text`, matching `StaticModel::encode`'s pipeline: char-level
    /// truncation, added-token splitting, BERT normalization, BERT pre-tokenization,
    /// WordPiece, then dropping `[UNK]` and capping the count.
    fn token_ids(&self, text: &str) -> Vec<u32> {
        let text = truncate_chars(
            text,
            MAX_TOKENS.saturating_mul(self.hdr.median_token_length),
        );
        let mut ids: Vec<u32> = Vec::new();
        for seg in self.split_added(text) {
            match seg {
                Seg::Added(id) => ids.push(id),
                Seg::Text(t) => {
                    let norm = self.normalize(t);
                    for word in pre_tokenize(&norm) {
                        self.wordpiece(word, &mut ids);
                    }
                }
            }
        }
        if let Some(unk) = self.hdr.unk_id {
            ids.retain(|&id| id != unk);
        }
        ids.truncate(MAX_TOKENS);
        ids
    }

    /// Added tokens (`[CLS]`, `[MASK]`, …) are matched against the *raw* text,
    /// before normalization, exactly as HuggingFace's `AddedVocabulary` does. Miss
    /// this and a memory that happens to mention `[UNK]` tokenizes differently on
    /// the two paths.
    fn split_added<'a>(&self, text: &'a str) -> Vec<Seg<'a>> {
        if self.hdr.added_count == 0 {
            return vec![Seg::Text(text)];
        }
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut last = 0usize;
        while i < text.len() {
            if !text.is_char_boundary(i) {
                i += 1;
                continue;
            }
            let rest = &text[i..];
            // Leftmost-longest: several added tokens can share a prefix.
            let mut best: Option<(usize, u32)> = None;
            for k in 0..self.hdr.added_count {
                let (key, id) = self.entry(self.hdr.added_off, k);
                if rest.as_bytes().starts_with(key)
                    && best.map(|(l, _)| key.len() > l).unwrap_or(true)
                {
                    best = Some((key.len(), id));
                }
            }
            match best {
                Some((len, id)) => {
                    if last < i {
                        out.push(Seg::Text(&text[last..i]));
                    }
                    out.push(Seg::Added(id));
                    i += len;
                    last = i;
                }
                None => i += 1,
            }
        }
        if last < text.len() {
            out.push(Seg::Text(&text[last..]));
        }
        out
    }

    /// `BertNormalizer`, in its documented order: clean, pad CJK, strip accents,
    /// lowercase.
    fn normalize(&self, text: &str) -> String {
        let f = self.hdr.flags;
        let mut s = String::with_capacity(text.len());

        if f & F_CLEAN_TEXT != 0 {
            for c in text.chars() {
                if c as u32 == 0 || c as u32 == 0xfffd || is_control(c) {
                    continue;
                }
                s.push(if c.is_whitespace() { ' ' } else { c });
            }
        } else {
            s.push_str(text);
        }

        if f & F_CHINESE != 0 && s.chars().any(is_chinese_char) {
            let mut t = String::with_capacity(s.len() + 8);
            for c in s.chars() {
                if is_chinese_char(c) {
                    t.push(' ');
                    t.push(c);
                    t.push(' ');
                } else {
                    t.push(c);
                }
            }
            s = t;
        }

        if f & F_STRIP_ACCENTS != 0 {
            s = s
                .nfd()
                .map(|(c, _)| c)
                .filter(|c| !c.is_mark_nonspacing())
                .collect();
        }

        if f & F_LOWERCASE != 0 {
            s = s.chars().flat_map(char::to_lowercase).collect();
        }
        s
    }

    /// Greedy longest-match-first WordPiece. A word that cannot be fully covered
    /// becomes a single `[UNK]`, and one longer than the model's per-word limit is
    /// `[UNK]` outright — both are what makes the two implementations agree on
    /// pathological input like a base64 blob.
    fn wordpiece(&self, word: &str, out: &mut Vec<u32>) {
        let unk = self.hdr.unk_id;
        let push_unk = |out: &mut Vec<u32>| {
            if let Some(u) = unk {
                out.push(u);
            }
        };
        if word.chars().count() > 100 {
            push_unk(out);
            return;
        }
        let mark = out.len();
        let mut start = 0usize;
        while start < word.len() {
            let mut end = word.len();
            let mut found = None;
            while start < end {
                let sub = &word[start..end];
                if let Some(id) = self.token_id(start > 0, sub) {
                    found = Some((id, end));
                    break;
                }
                end -= sub.chars().next_back().map_or(1, char::len_utf8);
            }
            match found {
                Some((id, e)) => {
                    out.push(id);
                    start = e;
                }
                None => {
                    out.truncate(mark);
                    push_unk(out);
                    return;
                }
            }
        }
    }

    /// Mean-pool the rows of `text`'s tokens, then L2-normalize like the model's
    /// own config asks. Identical arithmetic to `StaticModel::pool_ids`, only
    /// reading f16 out of the mapping instead of an owned f32 matrix.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let dim = self.hdr.dim;
        let mut sum = vec![0.0f32; dim];
        let mut count = 0usize;
        for id in self.token_ids(text) {
            let Some(row) = self.row(id) else { continue };
            for (s, chunk) in sum.iter_mut().zip(row.chunks_exact(2)) {
                *s += f16::from_le_bytes([chunk[0], chunk[1]]).to_f32();
            }
            count += 1;
        }
        let denom = count.max(1) as f32;
        for x in &mut sum {
            *x /= denom;
        }
        if self.hdr.flags & F_NORMALIZE != 0 {
            let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
            for x in &mut sum {
                *x /= norm;
            }
        }
        sum
    }
}

enum Seg<'a> {
    Text(&'a str),
    Added(u32),
}

/// Compare a stored key against `prefix` followed by `word`, without joining them.
fn cmp_split(key: &[u8], prefix: &[u8], word: &[u8]) -> std::cmp::Ordering {
    let n = prefix.len();
    let head = key.len().min(n);
    match key[..head].cmp(prefix) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }
    key[head..].cmp(word)
}

fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// HuggingFace counts every `Other` category character as control, except the
/// three whitespace ones it deliberately keeps.
fn is_control(c: char) -> bool {
    !matches!(c, '\t' | '\n' | '\r') && c.is_other()
}

fn is_chinese_char(c: char) -> bool {
    matches!(
        c as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B920..=0x2CEAF
            | 0xF900..=0xFAFF
            | 0x2F800..=0x2FA1F
    )
}

fn is_bert_punc(c: char) -> bool {
    c.is_ascii_punctuation() || c.is_punctuation()
}

/// `BertPreTokenizer`: drop whitespace, isolate punctuation.
fn pre_tokenize(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for chunk in s.split(char::is_whitespace).filter(|c| !c.is_empty()) {
        let mut start = 0usize;
        for (i, c) in chunk.char_indices() {
            if is_bert_punc(c) {
                if start < i {
                    out.push(&chunk[start..i]);
                }
                out.push(&chunk[i..i + c.len_utf8()]);
                start = i + c.len_utf8();
            }
        }
        if start < chunk.len() {
            out.push(&chunk[start..]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Building the cache
// ---------------------------------------------------------------------------

/// Probe corpus for [`build`]'s self-check. Each line targets a place where a
/// hand-written tokenizer can drift from HuggingFace: accents, CJK, punctuation
/// runs, identifiers, casing, added tokens, emoji, and words with no coverage.
const PROBES: &[&str] = &[
    "the project uses pnpm, not npm",
    "Deploys go out through fly.io — never through Vercel.",
    "configuración de la caché en español, con acentos y ñ",
    "run `cargo nextest run --no-capture` before touching src/retrieve.rs",
    "FUCKMEMORY_HOME=/tmp/x fuckmemory recall 'what did we use before?'",
    "野口里佳 Noguchi Rika 中文字符测试",
    "Hey friend!     How are you?!?",
    "[CLS] literal added tokens [SEP] and [MASK] inside text [UNK]",
    "emoji 🚀 and symbols ±§¶ and math ∑∫≈",
    "zzzqwxjvk unpronounceable gibberish qqq",
    "aVeryLongBase64ishTokenWithNoSpaces0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
    "\tTABS\tand\nnewlines\r\nand  double  spaces",
    "ALL CAPS SHOUTING ABOUT CI FAILURES",
    "mixed_snake_case and kebab-case and camelCase and PascalCase",
    "1234 5678 90 numbers and 3.14159 decimals and -42 negatives",
    "",
    "a",
    "İstanbul Ünïcödé ﬁligree ǅungla",
];

/// Minimum cosine between the fast path and the real model over [`PROBES`].
///
/// Not 1.0: the cache stores f16, so tiny rounding differences are expected and
/// harmless. Anything below this means the *tokenization* diverged, which would
/// make stored and query vectors incomparable — a silent retrieval failure, so we
/// refuse to install the cache instead.
const MIN_COSINE: f32 = 0.9995;

/// Build (and verify) the cache for `cfg`'s model. Returns the number of rows.
///
/// Costs one full slow load plus ~1 s of conversion, once per model.
pub fn build(cfg: &Config, force: bool) -> Result<usize> {
    let dir = cfg.model_dir();
    let path = cache_path(cfg);
    if !force && Fast::open(cfg).is_some() {
        let hdr = Header::read(&std::fs::read(&path)?).context("re-reading the cache header")?;
        return Ok(hdr.rows);
    }

    let tok_path = dir.join(SOURCES[0]);
    let weights_path = dir.join(SOURCES[1]);
    let config_name = ["config.json", "config_sentence_transformers.json"]
        .into_iter()
        .find(|c| dir.join(c).is_file())
        .context("model folder has no config.json")?;

    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&tok_path).with_context(|| format!("reading {}", tok_path.display()))?,
    )
    .context("parsing tokenizer.json")?;
    let model_cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(config_name))?)
            .context("parsing config.json")?;

    // Only the pipeline we have reimplemented is accepted. Any other shape falls
    // back to the slow path rather than guessing.
    let m = &spec["model"];
    anyhow::ensure!(
        m["type"] == "WordPiece",
        "fast path supports WordPiece models only, this one is {}",
        m["type"]
    );
    let norm = &spec["normalizer"];
    anyhow::ensure!(
        norm["type"] == "BertNormalizer",
        "fast path supports BertNormalizer only, this one is {}",
        norm["type"]
    );
    anyhow::ensure!(
        spec["pre_tokenizer"]["type"] == "BertPreTokenizer",
        "fast path supports BertPreTokenizer only, this one is {}",
        spec["pre_tokenizer"]["type"]
    );
    let boolean = |v: &serde_json::Value, default: bool| v.as_bool().unwrap_or(default);
    let lowercase = boolean(&norm["lowercase"], true);
    let mut flags = 0u32;
    if boolean(&norm["clean_text"], true) {
        flags |= F_CLEAN_TEXT;
    }
    if boolean(&norm["handle_chinese_chars"], true) {
        flags |= F_CHINESE;
    }
    // `strip_accents: null` means "follow lowercase", which is easy to miss and
    // silently mangles every accented word if you do.
    if norm["strip_accents"].as_bool().unwrap_or(lowercase) {
        flags |= F_STRIP_ACCENTS;
    }
    if lowercase {
        flags |= F_LOWERCASE;
    }
    if model_cfg["normalize"].as_bool().unwrap_or(true) {
        flags |= F_NORMALIZE;
    }

    let prefix = m["continuing_subword_prefix"]
        .as_str()
        .unwrap_or("##")
        .as_bytes()
        .to_vec();
    anyhow::ensure!(prefix.len() <= 8, "continuing_subword_prefix too long");
    anyhow::ensure!(
        m["max_input_chars_per_word"].as_u64().unwrap_or(100) == 100,
        "unexpected max_input_chars_per_word"
    );

    let vocab = m["vocab"]
        .as_object()
        .context("tokenizer.json has no model.vocab object")?;
    let unk_token = m["unk_token"].as_str();
    let unk_id = unk_token
        .and_then(|t| vocab.get(t))
        .and_then(|v| v.as_u64());
    if unk_token.is_some() {
        anyhow::ensure!(unk_id.is_some(), "unk_token is not in the vocabulary");
    }

    // model2vec's median is over byte lengths of the model vocab, and it decides
    // where a long text gets cut, so it has to be computed the same way.
    let mut lens: Vec<usize> = vocab.keys().map(|k| k.len()).collect();
    lens.sort_unstable();
    let median_token_length = lens.get(lens.len() / 2).copied().unwrap_or(1);

    let mut entries: Vec<(&str, u32)> = Vec::with_capacity(vocab.len());
    let mut max_id = 0u32;
    for (tok, id) in vocab {
        let id = id.as_u64().context("vocab id is not a number")? as u32;
        max_id = max_id.max(id);
        entries.push((tok.as_str(), id));
    }
    entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let added: Vec<(&str, u32)> = spec["added_tokens"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|t| !t["normalized"].as_bool().unwrap_or(false))
                .filter_map(|t| Some((t["content"].as_str()?, t["id"].as_u64()? as u32)))
                .collect()
        })
        .unwrap_or_default();

    let (matrix, dim) = read_embedding_matrix(&weights_path, max_id as usize + 1)?;
    let rows = max_id as usize + 1;

    // Blob + index. Both tables point into one string blob.
    let mut blob: Vec<u8> = Vec::new();
    let mut index: Vec<u8> = Vec::with_capacity(entries.len() * ENTRY);
    for (tok, id) in &entries {
        index.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        index.extend_from_slice(&(tok.len() as u32).to_le_bytes());
        index.extend_from_slice(&id.to_le_bytes());
        blob.extend_from_slice(tok.as_bytes());
    }
    let mut added_tbl: Vec<u8> = Vec::with_capacity(added.len() * ENTRY);
    for (tok, id) in &added {
        added_tbl.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        added_tbl.extend_from_slice(&(tok.len() as u32).to_le_bytes());
        added_tbl.extend_from_slice(&id.to_le_bytes());
        blob.extend_from_slice(tok.as_bytes());
    }

    let matrix_bytes = (rows * dim * 2) as u64;
    let index_off = MATRIX_OFF + matrix_bytes;
    let added_off = index_off + index.len() as u64;
    let blob_off = added_off + added_tbl.len() as u64;

    let sources = source_paths(&dir, config_name);
    let hdr = Header {
        dim,
        rows,
        vocab_count: entries.len(),
        added_count: added.len(),
        median_token_length,
        unk_id: unk_id.map(|v| v as u32),
        flags,
        prefix,
        matrix_off: MATRIX_OFF,
        index_off,
        added_off,
        blob_off,
        blob_len: blob.len() as u64,
        stamps: [
            stamp(&sources[0])?,
            stamp(&sources[1])?,
            stamp(&sources[2])?,
        ],
        config_name: config_name.to_string(),
    };

    let mut out: Vec<u8> = Vec::with_capacity(HEADER_LEN + matrix.len() * 2 + blob.len() + 4096);
    out.extend_from_slice(&hdr.write());
    out.resize(MATRIX_OFF as usize, 0);
    for v in &matrix {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&index);
    out.extend_from_slice(&added_tbl);
    out.extend_from_slice(&blob);

    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, &out).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("installing {}", path.display()))?;

    // Verify against the real model, and uninstall the cache if it disagrees.
    match verify(cfg) {
        Ok(worst) if worst >= MIN_COSINE => Ok(rows),
        Ok(worst) => {
            std::fs::remove_file(&path).ok();
            anyhow::bail!(
                "fast cache disagreed with the model (worst cosine {worst:.5} < {MIN_COSINE}); \
                 keeping the slow path"
            )
        }
        Err(e) => {
            std::fs::remove_file(&path).ok();
            Err(e.context("verifying the fast cache"))
        }
    }
}

/// Read the `embeddings` tensor, folding in `weights`/`mapping` when the model
/// carries them, so a cache row is exactly what `pool_ids` would have summed.
fn read_embedding_matrix(path: &Path, rows_needed: usize) -> Result<(Vec<f16>, usize)> {
    use safetensors::tensor::Dtype;
    use safetensors::SafeTensors;

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let st = SafeTensors::deserialize(&bytes).context("parsing safetensors")?;
    let t = st
        .tensor("embeddings")
        .or_else(|_| st.tensor("0"))
        .or_else(|_| st.tensor("embedding.weight"))
        .context("no embeddings tensor")?;
    let [erows, dim]: [usize; 2] = t.shape().try_into().context("embeddings is not 2-D")?;

    let raw = t.data();
    let value = |i: usize| -> f32 {
        match t.dtype() {
            Dtype::F32 => f32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap()),
            Dtype::F16 => f16::from_le_bytes(raw[i * 2..i * 2 + 2].try_into().unwrap()).to_f32(),
            Dtype::I8 => raw[i] as i8 as f32,
            _ => f32::NAN,
        }
    };
    anyhow::ensure!(
        matches!(t.dtype(), Dtype::F32 | Dtype::F16 | Dtype::I8),
        "unsupported embeddings dtype {:?}",
        t.dtype()
    );

    let weights: Option<Vec<f32>> = st.tensor("weights").ok().map(|w| {
        let d = w.data();
        match w.dtype() {
            Dtype::F64 => d
                .chunks_exact(8)
                .map(|b| f64::from_le_bytes(b.try_into().unwrap()) as f32)
                .collect(),
            Dtype::F16 => d
                .chunks_exact(2)
                .map(|b| f16::from_le_bytes(b.try_into().unwrap()).to_f32())
                .collect(),
            _ => d
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect(),
        }
    });
    let mapping: Option<Vec<usize>> = st.tensor("mapping").ok().map(|t| {
        let d = t.data();
        match t.dtype() {
            Dtype::I32 => d
                .chunks_exact(4)
                .map(|b| i32::from_le_bytes(b.try_into().unwrap()) as usize)
                .collect(),
            _ => d
                .chunks_exact(8)
                .map(|b| i64::from_le_bytes(b.try_into().unwrap()) as usize)
                .collect(),
        }
    });

    let mut out = vec![f16::ZERO; rows_needed * dim];
    for id in 0..rows_needed {
        let row_idx = mapping
            .as_ref()
            .and_then(|m| m.get(id))
            .copied()
            .unwrap_or(id);
        if row_idx >= erows {
            continue;
        }
        let scale = weights
            .as_ref()
            .and_then(|w| w.get(id))
            .copied()
            .unwrap_or(1.0);
        for c in 0..dim {
            out[id * dim + c] = f16::from_f32(value(row_idx * dim + c) * scale);
        }
    }
    Ok((out, dim))
}

/// Encode [`PROBES`] both ways and return the worst cosine. Loads the real model,
/// so it is only ever called from `build`, `doctor` and tests.
pub fn verify(cfg: &Config) -> Result<f32> {
    let fast = Fast::open(cfg).context("no usable fast cache to verify")?;
    let slow = model2vec_rs::model::StaticModel::from_pretrained(cfg.model_dir(), None, None, None)
        .map_err(|e| anyhow::anyhow!("loading the reference model: {e}"))?;

    let mut worst = 1.0f32;
    for probe in PROBES {
        let a = fast.embed(probe);
        let b = slow.encode_single(probe);
        anyhow::ensure!(
            a.len() == b.len(),
            "dimension mismatch: fast {} vs model {}",
            a.len(),
            b.len()
        );
        let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        // An all-zero vector on both sides (empty text) is agreement, not failure.
        let cos = if na < 1e-9 && nb < 1e-9 {
            1.0
        } else {
            dot / (na.max(1e-12) * nb.max(1e-12))
        };
        if cos < worst {
            worst = cos;
        }
    }
    Ok(worst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_tokenize_isolates_punctuation_like_bert() {
        assert_eq!(
            pre_tokenize("Hey friend!     How are you?!?"),
            vec!["Hey", "friend", "!", "How", "are", "you", "?", "!", "?"]
        );
    }

    #[test]
    fn pre_tokenize_splits_paths_and_flags() {
        assert_eq!(
            pre_tokenize("src/db.rs --no-verify"),
            vec!["src", "/", "db", ".", "rs", "-", "-", "no", "-", "verify"]
        );
    }

    #[test]
    fn cmp_split_orders_as_if_the_pieces_were_joined() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_split(b"##ing", b"##", b"ing"), Equal);
        assert_eq!(cmp_split(b"##inf", b"##", b"ing"), Less);
        assert_eq!(cmp_split(b"##inh", b"##", b"ing"), Greater);
        // A key shorter than the prefix must still compare, not panic.
        assert_eq!(cmp_split(b"#", b"##", b"x"), Less);
        assert_eq!(cmp_split(b"ing", b"", b"ing"), Equal);
    }

    #[test]
    fn truncate_chars_counts_chars_not_bytes() {
        assert_eq!(truncate_chars("ñññññ", 3), "ñññ");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn control_and_whitespace_classification_matches_bert() {
        assert!(!is_control('\t') && !is_control('\n') && !is_control('\r'));
        assert!(is_control('\u{0000}'));
        assert!(is_control('\u{200B}'), "zero-width space is Cf");
        assert!(!is_control('a'));
    }

    #[test]
    fn header_round_trips() {
        let h = Header {
            dim: 512,
            rows: 63091,
            vocab_count: 63091,
            added_count: 5,
            median_token_length: 7,
            unk_id: Some(1),
            flags: F_CLEAN_TEXT | F_LOWERCASE | F_NORMALIZE,
            prefix: b"##".to_vec(),
            matrix_off: MATRIX_OFF,
            index_off: 100,
            added_off: 200,
            blob_off: 300,
            blob_len: 400,
            stamps: [
                Stamp {
                    len: 1,
                    mtime_ms: 2,
                },
                Stamp {
                    len: 3,
                    mtime_ms: 4,
                },
                Stamp {
                    len: 5,
                    mtime_ms: 6,
                },
            ],
            config_name: "config.json".into(),
        };
        let back = Header::read(&h.write()).expect("header must parse");
        assert_eq!(back.dim, 512);
        assert_eq!(back.rows, 63091);
        assert_eq!(back.added_count, 5);
        assert_eq!(back.unk_id, Some(1));
        assert_eq!(back.prefix, b"##");
        assert_eq!(
            back.stamps[2],
            Stamp {
                len: 5,
                mtime_ms: 6
            }
        );
        assert_eq!(back.config_name, "config.json");
        assert_eq!(back.blob_len, 400);
    }

    /// A cache file that is truncated, or whose header claims more than the file
    /// holds, must be rejected — not indexed into. The fallback is the slow path,
    /// which is always correct.
    #[test]
    fn a_corrupt_cache_is_refused_rather_than_read() {
        let dir = std::env::temp_dir().join(format!("fm-fast-bad-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cfg = Config {
            home: dir.clone(),
            model: "fake/model".into(),
            ..Config::default()
        };
        let model_dir = cfg.model_dir();
        std::fs::create_dir_all(&model_dir).unwrap();
        for f in [SOURCES[0], SOURCES[1], "config.json"] {
            std::fs::write(model_dir.join(f), b"x").unwrap();
        }

        let mut hdr = Header {
            dim: 512,
            rows: 1_000_000, // far more than the file can hold
            vocab_count: 10,
            added_count: 0,
            median_token_length: 7,
            unk_id: Some(1),
            flags: F_LOWERCASE,
            prefix: b"##".to_vec(),
            matrix_off: MATRIX_OFF,
            index_off: MATRIX_OFF + 8,
            added_off: MATRIX_OFF + 16,
            blob_off: MATRIX_OFF + 24,
            blob_len: 8,
            stamps: [stamp(&model_dir.join(SOURCES[0])).unwrap(); 3],
            config_name: "config.json".into(),
        };
        let mut file = hdr.write();
        file.resize(MATRIX_OFF as usize + 64, 0);
        std::fs::write(cache_path(&cfg), &file).unwrap();
        assert!(
            Fast::open(&cfg).is_none(),
            "oversized matrix must be refused"
        );

        // Offsets that overlap each other are just as bad as ones past the end.
        hdr.rows = 1;
        hdr.index_off = 0;
        let mut file = hdr.write();
        file.resize(MATRIX_OFF as usize + 64, 0);
        std::fs::write(cache_path(&cfg), &file).unwrap();
        assert!(
            Fast::open(&cfg).is_none(),
            "overlapping tables must be refused"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn header_rejects_foreign_files() {
        assert!(Header::read(b"not a cache at all").is_none());
        let mut bad = vec![0u8; HEADER_LEN];
        bad[..8].copy_from_slice(b"FMFASTv0");
        assert!(Header::read(&bad).is_none());
    }
}
