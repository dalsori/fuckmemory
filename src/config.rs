//! Where things live on disk, and the knobs a user can turn.
//!
//! Settings come from three places, in this order of precedence:
//!
//! 1. `FUCKMEMORY_*` environment variables — one-off overrides, CI, and scripts.
//! 2. `config.toml` in the data dir — what the TUI writes and what persists.
//! 3. The defaults below.
//!
//! The file lives next to the database rather than in `~/.config`, so that the
//! promise in the README still holds: everything this tool owns is under one
//! directory you can delete.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Embedding model we ship with. Static embeddings: no ONNX, no GPU, no API key,
/// ~70x faster than MiniLM on CPU at ~82% of its retrieval quality.
pub const DEFAULT_MODEL: &str = "minishlab/potion-retrieval-32M";

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct Config {
    /// Root for all state. `$FUCKMEMORY_HOME`, else `~/.local/share/fuckmemory`.
    pub home: PathBuf,
    /// Model repo id or local folder.
    pub model: String,
    /// Default token budget for a packed recall.
    pub budget_tokens: usize,
    /// Semantic search on. Off falls back to BM25 only.
    pub semantic: bool,
    /// Use the mmap'd embedding cache when one is available. Off always loads the
    /// real model, which is ~200x slower to start but needs no cache.
    pub fast: bool,

    /// Store every prompt the agent is sent, without being asked to.
    pub autosave: bool,
    /// Prompts shorter than this are skipped — "ok", "yes", "continue" are not
    /// memories, and they would drown out real ones.
    pub autosave_min_chars: usize,
    /// Also derive graph facts from autosaved prompts that look salient. Off keeps
    /// them as raw searchable episodes only.
    pub autosave_facts: bool,
    /// Scope autosaved prompts land in: `project` (default) or `global`.
    pub autosave_scope: String,
    /// Drop text that looks like a credential instead of storing it.
    pub redact: bool,

    /// Inject relevant memories into each prompt automatically, so recall happens
    /// even when the agent doesn't think to ask.
    pub autorecall: bool,
    pub autorecall_limit: usize,
    pub autorecall_budget: usize,

    /// Settings pinned by an environment variable, by config key. The TUI shows
    /// these as locked rather than letting you "change" something that a variable
    /// will keep overriding.
    pub env_locked: Vec<&'static str>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            home: PathBuf::new(),
            model: DEFAULT_MODEL.to_string(),
            budget_tokens: 1_200,
            semantic: true,
            fast: true,
            autosave: false,
            autosave_min_chars: 12,
            autosave_facts: true,
            autosave_scope: "project".into(),
            redact: true,
            autorecall: false,
            autorecall_limit: 6,
            autorecall_budget: 600,
            env_locked: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = match std::env::var_os("FUCKMEMORY_HOME") {
            Some(p) => PathBuf::from(p),
            None => dirs::data_dir()
                .context("no data dir; set FUCKMEMORY_HOME")?
                .join("fuckmemory"),
        };
        let mut cfg = Self {
            home,
            ..Default::default()
        };

        // A malformed config must not brick every agent on the machine, so it is
        // reported and skipped rather than propagated.
        match cfg.read_file() {
            Ok(Some(doc)) => cfg.apply_file(&doc),
            Ok(None) => {}
            Err(e) => eprintln!(
                "fuckmemory: ignoring {}: {e:#}",
                cfg.config_path().display()
            ),
        }
        cfg.apply_env();
        Ok(cfg)
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.join(CONFIG_FILE)
    }

    pub fn db_path(&self) -> PathBuf {
        self.home.join("memory.db")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.home.join("models")
    }

    /// Where persisted vector indexes live, so a one-shot process can open one
    /// with mmap instead of re-reading every vector out of SQLite.
    pub fn index_cache_dir(&self) -> PathBuf {
        self.home.join("index-cache")
    }

    /// Local folder for the configured model, e.g.
    /// `~/.local/share/fuckmemory/models/minishlab__potion-retrieval-32M`.
    pub fn model_dir(&self) -> PathBuf {
        self.models_dir().join(self.model.replace('/', "__"))
    }

    pub fn is_locked(&self, key: &str) -> bool {
        self.env_locked.contains(&key)
    }

    fn read_file(&self) -> Result<Option<toml_edit::DocumentMut>> {
        let path = self.config_path();
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some(text.parse::<toml_edit::DocumentMut>()?))
    }

    fn apply_file(&mut self, doc: &toml_edit::DocumentMut) {
        let s = |key: &str| doc.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let b = |key: &str| doc.get(key).and_then(|v| v.as_bool());
        let n = |key: &str| {
            doc.get(key)
                .and_then(|v| v.as_integer())
                .map(|i| i.max(0) as usize)
        };

        if let Some(v) = s("model") {
            self.model = v;
        }
        if let Some(v) = n("budget_tokens") {
            self.budget_tokens = v;
        }
        if let Some(v) = b("semantic") {
            self.semantic = v;
        }
        if let Some(v) = b("fast") {
            self.fast = v;
        }

        let table = |name: &str| doc.get(name).and_then(|t| t.as_table_like());
        if let Some(t) = table("autosave") {
            let tb = |k: &str| t.get(k).and_then(|v| v.as_bool());
            let tn = |k: &str| {
                t.get(k)
                    .and_then(|v| v.as_integer())
                    .map(|i| i.max(0) as usize)
            };
            if let Some(v) = tb("enabled") {
                self.autosave = v;
            }
            if let Some(v) = tn("min_chars") {
                self.autosave_min_chars = v;
            }
            if let Some(v) = tb("facts") {
                self.autosave_facts = v;
            }
            if let Some(v) = t.get("scope").and_then(|v| v.as_str()) {
                self.autosave_scope = v.to_string();
            }
            if let Some(v) = tb("redact") {
                self.redact = v;
            }
        }
        if let Some(t) = table("autorecall") {
            if let Some(v) = t.get("enabled").and_then(|v| v.as_bool()) {
                self.autorecall = v;
            }
            if let Some(v) = t.get("limit").and_then(|v| v.as_integer()) {
                self.autorecall_limit = v.max(0) as usize;
            }
            if let Some(v) = t.get("budget_tokens").and_then(|v| v.as_integer()) {
                self.autorecall_budget = v.max(0) as usize;
            }
        }
    }

    fn apply_env(&mut self) {
        let mut locked = Vec::new();
        let on = |var: &str, key: &'static str, locked: &mut Vec<&'static str>| -> Option<bool> {
            let raw = env_string(var)?;
            locked.push(key);
            Some(!matches!(
                raw.to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            ))
        };

        if let Some(v) = env_string("FUCKMEMORY_MODEL") {
            self.model = v;
            locked.push("model");
        }
        if let Some(v) = env_string("FUCKMEMORY_BUDGET").and_then(|s| s.parse().ok()) {
            self.budget_tokens = v;
            locked.push("budget_tokens");
        }
        if let Some(v) = on("FUCKMEMORY_SEMANTIC", "semantic", &mut locked) {
            self.semantic = v;
        }
        if let Some(v) = on("FUCKMEMORY_FAST", "fast", &mut locked) {
            self.fast = v;
        }
        if let Some(v) = on("FUCKMEMORY_AUTOSAVE", "autosave.enabled", &mut locked) {
            self.autosave = v;
        }
        if let Some(v) = on("FUCKMEMORY_AUTORECALL", "autorecall.enabled", &mut locked) {
            self.autorecall = v;
        }
        if let Some(v) = on("FUCKMEMORY_REDACT", "autosave.redact", &mut locked) {
            self.redact = v;
        }
        self.env_locked = locked;
    }

    /// Persist the current values, keeping any comments the user added.
    pub fn save(&self) -> Result<PathBuf> {
        let path = self.config_path();
        std::fs::create_dir_all(&self.home)
            .with_context(|| format!("creating {}", self.home.display()))?;

        let mut doc = match self.read_file()? {
            Some(d) => d,
            None => {
                let mut d = toml_edit::DocumentMut::new();
                let header = toml_edit::Item::None;
                let _ = header;
                d.decor_mut().set_prefix(
                    "# fuckmemory settings — written by `fuckmemory tui`, safe to edit by hand.\n\
                     # FUCKMEMORY_* environment variables override everything here.\n\n",
                );
                d
            }
        };

        doc["model"] = toml_edit::value(self.model.clone());
        doc["budget_tokens"] = toml_edit::value(self.budget_tokens as i64);
        doc["semantic"] = toml_edit::value(self.semantic);
        doc["fast"] = toml_edit::value(self.fast);

        for name in ["autosave", "autorecall"] {
            if !doc.get(name).map(|t| t.is_table_like()).unwrap_or(false) {
                doc[name] = toml_edit::Item::Table(toml_edit::Table::new());
            }
        }
        doc["autosave"]["enabled"] = toml_edit::value(self.autosave);
        doc["autosave"]["min_chars"] = toml_edit::value(self.autosave_min_chars as i64);
        doc["autosave"]["facts"] = toml_edit::value(self.autosave_facts);
        doc["autosave"]["scope"] = toml_edit::value(self.autosave_scope.clone());
        doc["autosave"]["redact"] = toml_edit::value(self.redact);
        doc["autorecall"]["enabled"] = toml_edit::value(self.autorecall);
        doc["autorecall"]["limit"] = toml_edit::value(self.autorecall_limit as i64);
        doc["autorecall"]["budget_tokens"] = toml_edit::value(self.autorecall_budget as i64);

        // Temp file + rename: several agents may be reading this concurrently.
        let tmp = path.with_extension(format!("toml.tmp-{}", std::process::id()));
        std::fs::write(&tmp, doc.to_string())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(path)
    }
}

fn env_string(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

/// One day, in the unit [`now`] returns.
pub const DAY: i64 = 86_400_000;

/// Strictly increasing milliseconds since the Unix epoch.
///
/// Two properties matter here, and both are load-bearing for the bi-temporal
/// model:
///
/// - **Milliseconds, not seconds.** Agents write several memories per second.
/// - **Strictly increasing.** Even at millisecond resolution, an agent storing a
///   fact and immediately superseding it lands both writes in the same tick. The
///   old fact then gets `valid_from == valid_to`, a zero-width validity window,
///   and no `as_of` query can ever see it — the history silently disappears.
///   Bumping by 1ms when the clock hasn't moved guarantees every window is
///   non-empty and every write is orderable.
///
/// Monotonicity is per process. Two agents writing in the same millisecond can
/// still collide, but at that point their relative order is genuinely undefined.
pub fn now() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);

    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // `fetch_update` hands back the value that was there *before* the update, not
    // the one it stored. Returning it directly would make the first call of every
    // process return 0 and every later call return the previous timestamp — which
    // silently wrote `invalidated_at = 0` onto retracted facts and broke `prune`.
    // So recompute the stored value from the same closure.
    let next = |prev: i64| if wall > prev { wall } else { prev + 1 };
    let prev = LAST
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| Some(next(p)))
        .unwrap_or(0);
    next(prev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_a_real_timestamp_and_strictly_increasing() {
        let a = now();
        // Would have caught the fetch_update bug: the first call returned 0.
        assert!(a > 1_700_000_000_000, "now() must be epoch millis, got {a}");
        let b = now();
        let c = now();
        assert!(a < b && b < c, "not strictly increasing: {a} {b} {c}");
    }

    #[test]
    fn now_never_goes_backwards_under_threads() {
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..200).map(|_| now()).collect::<Vec<_>>()))
            .collect();
        let mut all: Vec<i64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "two calls returned the same millisecond");
    }

    fn tmp_home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fm-cfg-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_then_reload_round_trips_every_setting() {
        let home = tmp_home("round");
        let mut cfg = Config {
            home: home.clone(),
            ..Default::default()
        };
        cfg.autosave = true;
        cfg.autosave_min_chars = 42;
        cfg.autosave_facts = false;
        cfg.autosave_scope = "global".into();
        cfg.autorecall = true;
        cfg.autorecall_limit = 9;
        cfg.autorecall_budget = 777;
        cfg.budget_tokens = 2_000;
        cfg.semantic = false;
        cfg.fast = false;
        cfg.redact = false;
        cfg.save().unwrap();

        let mut back = Config {
            home,
            ..Default::default()
        };
        let doc = back.read_file().unwrap().expect("file must exist");
        back.apply_file(&doc);
        assert!(back.autosave);
        assert_eq!(back.autosave_min_chars, 42);
        assert!(!back.autosave_facts);
        assert_eq!(back.autosave_scope, "global");
        assert!(back.autorecall);
        assert_eq!(back.autorecall_limit, 9);
        assert_eq!(back.autorecall_budget, 777);
        assert_eq!(back.budget_tokens, 2_000);
        assert!(!back.semantic);
        assert!(!back.fast);
        assert!(!back.redact);
    }

    #[test]
    fn saving_twice_keeps_user_comments() {
        let home = tmp_home("comments");
        let cfg = Config {
            home: home.clone(),
            ..Default::default()
        };
        cfg.save().unwrap();
        let path = cfg.config_path();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("# my own note\n{text}")).unwrap();

        let mut again = Config {
            home,
            autosave: true,
            ..Default::default()
        };
        again.autosave = true;
        again.save().unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# my own note"),
            "comment was dropped:\n{after}"
        );
        assert!(after.contains("enabled = true"));
    }

    #[test]
    fn a_broken_config_file_is_ignored_not_fatal() {
        let home = tmp_home("broken");
        std::fs::write(home.join(CONFIG_FILE), "this is not = = toml").unwrap();
        let mut cfg = Config {
            home,
            ..Default::default()
        };
        assert!(cfg.read_file().is_err());
        // load() swallows it; defaults survive.
        cfg.apply_env();
        assert_eq!(cfg.model, DEFAULT_MODEL);
    }
}
