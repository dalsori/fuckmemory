//! Autosave and auto-recall, driven by agent hooks.
//!
//! The MCP tools only fire when the model decides to call them, which means the
//! memory is exactly as good as the agent's discipline — and agents forget to
//! call `remember` precisely when they are deep in something worth remembering.
//! So this module gives the same store a second, involuntary path: the agent's
//! own hook system pipes every prompt through `fuckmemory hook prompt`, and
//!
//! - **every prompt is kept**, verbatim, as a searchable episode, and
//! - **only the ones that read like durable knowledge** become facts in the graph.
//!
//! That split is the whole design. Storing every "fix the typo in main.rs" as a
//! fact would poison recall within a week; dropping it entirely would break the
//! promise that nothing is lost. Episodes are cheap, unranked and out of the way;
//! facts are what an agent actually reads back.
//!
//! Two rules hold everywhere in here:
//!
//! 1. **A hook must never break the agent.** Every failure path stays quiet and
//!    exits 0. A memory tool that can wedge your editor is not worth having.
//! 2. **Never store a credential.** Prompts contain pasted tokens far more often
//!    than tool calls do, because a human typed them.

use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

use crate::config::Config;
use crate::embed::{Embedder, VecCache};
use crate::pack::{self, PackOptions};
use crate::retrieve::{self, Query};
use crate::store::{self, RememberInput};
use crate::{db, scope};

/// Which hook fired. Agents name these differently; the CLI maps their names
/// onto this small set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A prompt is on its way to the model. Autosave and auto-recall both run.
    Prompt,
    /// The session ended. Nothing to store — this is when we tidy the store.
    SessionEnd,
}

impl Event {
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['_', ' '], "-")
            .as_str()
        {
            "prompt"
            | "userpromptsubmit"
            | "user-prompt-submit"
            | "userpromptsubmitted"
            | "user-prompt-submitted"
            | "submit"
            | "beforeagent"
            | "before-agent"
            | "preinvocation"
            | "pre-invocation"
            | "beforesubmitprompt"
            | "before-submit-prompt" => Some(Self::Prompt),
            "session-end" | "sessionend" | "session-ended" | "stop" | "end" => {
                Some(Self::SessionEnd)
            }
            _ => None,
        }
    }
}

/// What the hook decided to do, so the CLI can report it under `--verbose` and
/// tests can assert on it without reading the database.
#[derive(Debug, Default)]
pub struct Outcome {
    pub stored: bool,
    pub duplicate: bool,
    pub fact_ids: Vec<i64>,
    pub skipped: Option<&'static str>,
    pub redactions: usize,
    /// Text to hand back to the agent as extra context, if any.
    pub context: Option<String>,
    pub consolidated: usize,
}

/// Pull the prompt text out of whatever the agent sent us.
///
/// Claude Code posts a JSON object on stdin; other agents may pipe raw text, or
/// nothing at all when the user configured `--text`. All three are accepted
/// because the alternative is a hook that silently does nothing on some agent
/// nobody tested.
pub fn prompt_from_stdin(raw: &str) -> (Option<String>, Value) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, json!({}));
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) if v.is_object() => {
            let text = ["prompt", "user_prompt", "message", "text", "input"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str))
                .map(str::to_string);
            (text, v)
        }
        _ => (Some(trimmed.to_string()), json!({})),
    }
}

/// `prompt_from_stdin`, with an agent-specific fallback.
///
/// Antigravity's `PreInvocation` payload never carries the prompt text itself —
/// it points at the conversation's `transcriptPath` instead. The transcript is a
/// JSONL where each user turn is recorded as a `USER_INPUT` entry whose `content`
/// wraps the prompt in `<USER_REQUEST>…</USER_REQUEST>` tags, so we read the file
/// and take the most recent one. Any failure returns `None`: a hook must never
/// break the agent it runs inside.
pub fn prompt_from_agent(raw: &str, agent: &str) -> (Option<String>, Value) {
    let (text, meta) = prompt_from_stdin(raw);
    if text.is_some() || agent != "antigravity" {
        return (text, meta);
    }
    let Some(path) = meta.get("transcriptPath").and_then(Value::as_str) else {
        return (None, meta);
    };
    let path = expand_tilde(path);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return (None, meta);
    };
    let prompt = content.lines().rev().find_map(antigravity_user_request);
    (prompt, meta)
}

/// Expand a leading `~` to the home directory, like the shell would.
fn expand_tilde(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(p)
}

/// Extract the `<USER_REQUEST>…</USER_REQUEST>` body from one transcript line,
/// if that line is a user turn.
fn antigravity_user_request(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("USER_INPUT") {
        return None;
    }
    let content = v.get("content").and_then(Value::as_str)?;
    const OPEN: &str = "<USER_REQUEST>";
    const CLOSE: &str = "</USER_REQUEST>";
    let start = content.find(OPEN)?;
    let end = content.find(CLOSE)?;
    if end <= start + OPEN.len() {
        return None;
    }
    let req = content[start + OPEN.len()..end].trim();
    if req.is_empty() {
        None
    } else {
        Some(req.to_string())
    }
}

/// Longest prompt stored whole. Beyond this it is a paste, not a statement, and
/// the tail rarely carries the point.
const MAX_STORED_CHARS: usize = 4_000;

/// Run one hook. `agent` is what the installer wired in (`claude-code`, …).
pub fn run(
    cfg: &Config,
    event: Event,
    text: Option<String>,
    meta: &Value,
    agent: &str,
) -> Result<Outcome> {
    let mut out = Outcome::default();
    let cwd = meta
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    if event == Event::SessionEnd {
        // The end of a session is the one moment nobody is waiting on us, which
        // makes it the right time to merge duplicates and compact the indexes.
        let mut conn = db::open(&cfg.db_path())?;
        let emb = embedder(cfg);
        let report = crate::consolidate::run(&mut conn, cfg, emb.as_ref(), 200)?;
        out.consolidated = report.episodes_processed;
        return Ok(out);
    }

    let Some(text) = text else {
        out.skipped = Some("no prompt text");
        return Ok(out);
    };

    let conn_needed = cfg.autosave || cfg.autorecall;
    if !conn_needed {
        out.skipped = Some("autosave and auto-recall are both off");
        return Ok(out);
    }
    let mut conn = db::open(&cfg.db_path())?;
    let spec = if cfg.autosave_scope == "global" {
        Some("global")
    } else {
        None
    };
    let sc = scope::resolve(&conn, spec, &cwd)?;
    let emb = embedder(cfg);

    let skip = skip_reason(&text, cfg.autosave_min_chars);

    // Recall first: what we inject should answer the prompt, not include it.
    // "ok" and "/compact" carry no query signal, so injecting memories there
    // would spend context on a coin flip.
    let worth_recalling = !matches!(
        skip,
        Some("acknowledgement") | Some("slash command") | Some("shell passthrough") | Some("empty")
    );
    if cfg.autorecall && worth_recalling {
        let scope_ids = scope::read_set(&conn, &sc)?;
        // One-shot process: a persisted, mmap'd index keeps the SQLite read out
        // of every prompt. The cache is only valid while data_version matches,
        // which the VecCache checks itself.
        let mut cache = VecCache::with_dir(cfg.index_cache_dir());
        let r = retrieve::recall(
            &conn,
            &scope_ids,
            emb.as_ref(),
            &Query {
                text: text.clone(),
                limit: cfg.autorecall_limit.max(1),
                ..Default::default()
            },
            Some(&mut cache),
        )?;
        let rendered = pack::render(
            &r,
            &PackOptions {
                budget_tokens: cfg.autorecall_budget.max(64),
                scope_label: sc.label.clone(),
                debug: false,
            },
            crate::config::now(),
        );
        if !rendered.trim().is_empty() {
            store::mark_hits(&conn, &pack::rendered_ids(&r, &rendered))?;
            out.context = Some(rendered);
        }
    }

    if !cfg.autosave {
        out.skipped = Some("autosave is off");
        return Ok(out);
    }
    if let Some(reason) = skip {
        out.skipped = Some(reason);
        return Ok(out);
    }

    let (mut body, redactions) = if cfg.redact {
        let (b, n) = redact(&text);
        let (b, m) = redact_paths(&b, &cfg.ignore_paths, &crate::config::home_dir());
        (b, n + m)
    } else {
        (text.clone(), 0)
    };
    out.redactions = redactions;
    if body.trim().is_empty() {
        out.skipped = Some("nothing left after redaction");
        return Ok(out);
    }
    if body.chars().count() > MAX_STORED_CHARS {
        let cut = body
            .char_indices()
            .nth(MAX_STORED_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(body.len());
        body.truncate(cut);
        body.push_str(" …[truncated]");
    }

    let salient = cfg.autosave_facts && salience(&body).is_some();
    let kind = salience(&body).unwrap_or("prompt");
    let session = meta.get("session_id").and_then(Value::as_str).unwrap_or("");

    let stored = store::remember(
        &mut conn,
        &sc,
        emb.as_ref(),
        &RememberInput {
            text: body,
            kind: kind.to_string(),
            source: format!("autosave:{agent}"),
            facts: vec![],
            meta: Some(json!({ "session": session, "event": "prompt", "agent": agent })),
            files: vec![],
            // Non-salient prompts stay episodes: kept and searchable, but never
            // ranked next to a real decision.
            derive: salient,
        },
    )?;
    out.stored = true;
    out.duplicate = stored.duplicate;
    out.fact_ids = stored.fact_ids;
    Ok(out)
}

fn embedder(cfg: &Config) -> Option<Embedder> {
    if cfg.semantic {
        Embedder::load_if_cached(cfg)
    } else {
        None
    }
}

/// Acknowledgements and control words, in both languages this gets used in.
/// They are the most common thing a user types and the least worth keeping.
const ACKS: &[&str] = &[
    "ok",
    "okay",
    "oki",
    "k",
    "yes",
    "yep",
    "yeah",
    "no",
    "nope",
    "sure",
    "thanks",
    "thank you",
    "ty",
    "please",
    "continue",
    "go on",
    "go ahead",
    "next",
    "stop",
    "wait",
    "hmm",
    "y",
    "n",
    "si",
    "sí",
    "dale",
    "vale",
    "listo",
    "gracias",
    "sigue",
    "continua",
    "continúa",
    "adelante",
    "hazlo",
    "perfecto",
    "genial",
    "bien",
    "correcto",
    "exacto",
    "eso",
    "arreglalo",
    "arréglalo",
];

/// Why this prompt is not worth storing, or `None` to store it.
pub fn skip_reason(text: &str, min_chars: usize) -> Option<&'static str> {
    let t = text.trim();
    if t.is_empty() {
        return Some("empty");
    }
    // Slash commands and `!` shell passthroughs are invocations, not statements.
    if t.starts_with('/') && !t.contains(' ') {
        return Some("slash command");
    }
    if t.starts_with('!') {
        return Some("shell passthrough");
    }
    let normalized = t
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if ACKS.contains(&normalized.as_str()) {
        return Some("acknowledgement");
    }
    if t.chars().count() < min_chars {
        return Some("too short");
    }
    None
}

/// Does this prompt state something durable? Returns the memory kind if so.
///
/// Deliberately conservative and marker-based: no model runs here, so the only
/// honest options are "looks like a rule" and "don't know". Questions are
/// excluded outright — a question is a request for knowledge, not knowledge.
pub fn salience(text: &str) -> Option<&'static str> {
    let t = text.trim();
    if t.len() < 20 {
        return None;
    }
    let lower = t.to_lowercase();
    let first_word = lower.split_whitespace().next().unwrap_or("");

    const INTERROGATIVE: &[&str] = &[
        "how", "what", "why", "where", "when", "who", "which", "can", "could", "would", "should",
        "is", "are", "does", "do", "did", "cómo", "como", "qué", "que", "por", "dónde", "donde",
        "cuándo", "cuando", "quién", "quien", "cuál", "cual", "puedes", "podrías", "podrias",
        "sabes", "hay",
    ];
    if t.ends_with('?') || INTERROGATIVE.contains(&first_word) {
        return None;
    }

    // Ordered by specificity: the first matching group names the memory.
    const CONSTRAINT: &[&str] = &[
        "never",
        "always",
        "must",
        "don't",
        "do not",
        "avoid",
        "required",
        "requires",
        "forbidden",
        "nunca",
        "siempre",
        "no uses",
        "no use",
        "no hagas",
        "evita",
        "hay que",
        "tienes que",
        "debes",
        "obligatorio",
        "prohibido",
    ];
    const PREFERENCE: &[&str] = &[
        "prefer",
        "prefers",
        "i like",
        "rather than",
        "instead of",
        "please use",
        "from now on",
        "prefiero",
        "preferimos",
        "mejor usa",
        "me gusta",
        "en vez de",
        "en lugar de",
        "de ahora en adelante",
        "a partir de ahora",
    ];
    const DECISION: &[&str] = &[
        "we use",
        "we're using",
        "we are using",
        "we decided",
        "let's use",
        "switch to",
        "moving to",
        "the convention is",
        "the rule is",
        "usamos",
        "vamos a usar",
        "decidimos",
        "cambiamos a",
        "la convención es",
        "la regla es",
        "se usa",
    ];
    const NOTE: &[&str] = &[
        "remember",
        "note that",
        "keep in mind",
        "gotcha",
        "careful",
        "warning",
        "recuerda",
        "acuérdate",
        "acuerdate",
        "ten en cuenta",
        "ojo",
        "importante",
        "cuidado",
    ];

    let has = |set: &[&str]| set.iter().any(|m| contains_phrase(&lower, m));
    if has(CONSTRAINT) {
        Some("constraint")
    } else if has(PREFERENCE) {
        Some("preference")
    } else if has(DECISION) {
        Some("decision")
    } else if has(NOTE) {
        Some("note")
    } else {
        None
    }
}

/// Substring match that respects word boundaries, so "do not" matches but the
/// "no" inside "node" does not.
fn contains_phrase(haystack: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while let Some(i) = haystack[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack[..start]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false);
        let after_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.len().max(1);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// Token prefixes that identify a secret on sight. Everything here is a public,
/// documented format, which is what makes prefix matching reliable enough to act
/// on without a regex engine.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "pk_live_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "xapp-",
    "AKIA",
    "ASIA",
    "ya29.",
    "AIza",
    "hf_",
    "npm_",
    "dop_v1_",
    "shpat_",
    "sq0atp-",
    "SG.",
    "eyJ",
];

/// Keys whose value is a secret whatever it looks like. Words that are merely
/// *about* security ("auth", "login") are deliberately absent: they appear in
/// ordinary prose constantly, and redacting the word after them would quietly
/// mangle real memories.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "secret_key",
    "private_key",
    "client_secret",
    "credential",
    "credentials",
    "bearer",
    "contraseña",
    "contrasena",
    "clave",
];

/// How many words a secret-looking key stays armed for, so that `the token is
/// <value>` is caught along with `token=<value>`.
const ARM_WORDS: usize = 3;

/// Replace anything that looks like a credential with `[redacted]`.
///
/// Token-at-a-time rather than regex: the rules that matter here are "known
/// prefix", "assignment to a secret-ish key" and "long opaque blob", and all
/// three are cheaper and clearer to express directly than as patterns — and it
/// keeps a 1 MB regex engine out of a binary that has to start in 4 ms.
pub fn redact(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut armed = 0usize;

    for token in text.split_inclusive(char::is_whitespace) {
        let word = token.trim_end();
        let ws = &token[word.len()..];
        if word.is_empty() {
            out.push_str(token);
            continue;
        }

        // `API_KEY=sk-…`: the value rides along in the same token.
        if let Some((key, value_at)) = split_assignment(word) {
            if is_secret_key(&key) && word[value_at..].trim_matches(QUOTES).len() >= 6 {
                out.push_str(&word[..value_at]);
                out.push_str("[redacted]");
                out.push_str(ws);
                count += 1;
                armed = 0;
                continue;
            }
        }

        if looks_secret(word) || (armed > 0 && value_like(word)) {
            out.push_str("[redacted]");
            out.push_str(ws);
            count += 1;
            armed = 0;
            continue;
        }

        // `the password is …`: arm, and let a couple of connector words pass.
        if is_secret_key(&clean_key(word)) {
            armed = ARM_WORDS;
        } else {
            armed = armed.saturating_sub(1);
        }
        out.push_str(token);
    }
    (out, count)
}

/// A tiny glob matcher for path redaction. Supports `*` (any run within a
/// segment), `**` (any number of segments) and `?` (one character). `~` at the
/// start expands to the home directory. Everything else is a literal prefix
/// match. Deliberately small: a full `glob` crate would drag a big dependency
/// into a binary that has to start in milliseconds.
fn path_glob_match(glob: &str, path: &str, home: &Path) -> bool {
    let g = if let Some(rest) = glob.strip_prefix("~/") {
        if let Some(h) = home.to_str() {
            if let Some(p) = path.strip_prefix(h) {
                return path_glob_match(&format!("**/{rest}"), p.trim_start_matches('/'), home);
            }
            return false;
        }
        glob
    } else {
        glob
    };
    // A leading slash anchors to the root; otherwise match against the whole
    // path and any trailing segment (so `.env` matches `project/.env`).
    let anchored = g.starts_with('/');
    let g = g.trim_start_matches('/');
    let target = if anchored {
        path
    } else {
        path.trim_start_matches('/')
    };

    if anchored {
        glob_scan(g, target)
    } else {
        // Match the whole path, or any single trailing segment — so a bare
        // name like `.env` catches `project/.env` while `src/*.pem` still
        // needs the full path to align.
        glob_scan(g, target) || target.split('/').any(|seg| glob_scan(g, seg))
    }
}

/// Recursive glob scan over segments. `**` swallows zero or more segments.
fn glob_scan(pat: &str, path: &str) -> bool {
    let p: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let t: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    scan_seg(&p, &t)
}

fn scan_seg(pats: &[&str], segs: &[&str]) -> bool {
    match pats.first() {
        None => segs.is_empty(),
        Some(&"**") => {
            // `**` alone eats everything; `**/rest` eats any prefix.
            if pats.len() == 1 {
                return true;
            }
            for i in 0..=segs.len() {
                if scan_seg(&pats[1..], &segs[i..]) {
                    return true;
                }
            }
            false
        }
        Some(&pat) => {
            if segs.is_empty() {
                return false;
            }
            seg_match(pat, segs[0]) && scan_seg(&pats[1..], &segs[1..])
        }
    }
}

/// Match a single path segment against a `*`/`?` pattern (no `/`).
fn seg_match(pat: &str, seg: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    let seg: Vec<char> = seg.chars().collect();
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&'*') => {
                for i in 0..=s.len() {
                    if go(&p[1..], &s[i..]) {
                        return true;
                    }
                }
                false
            }
            Some(&'?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some(&c) => !s.is_empty() && s[0] == c && go(&p[1..], &s[1..]),
        }
    }
    go(&pat, &seg)
}

/// Redact path-shaped tokens that match any of `globs`, returning the redacted
/// text and how many paths were replaced. This runs *after* the token
/// redaction, so the path itself is replaced with a marker regardless of what
/// follows it — `cat ~/.aws/credentials` loses the path even though no token
/// rule would catch it.
pub fn redact_paths(text: &str, globs: &[String], home: &Path) -> (String, usize) {
    if globs.is_empty() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    for token in text.split_inclusive(char::is_whitespace) {
        let word = token.trim_end();
        if !word.is_empty() && path_glob_match_any(word, globs, home) {
            out.push_str("[redacted]");
            out.push_str(&token[word.len()..]);
            count += 1;
        } else {
            out.push_str(token);
        }
    }
    (out, count)
}

fn path_glob_match_any(word: &str, globs: &[String], home: &Path) -> bool {
    // Paths in prose often carry trailing punctuation or quotes. Trim only the
    // end: leading dots are significant (`*.env`, `~/.aws`), so a bare `.env`
    // must keep its dot.
    let word = word.trim_end_matches([',', ';', ')', ']', '.', '"', '\'']);
    globs.iter().any(|g| path_glob_match(g, word, home))
}

const QUOTES: &[char] = &['"', '\'', '`'];

fn clean_key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .to_lowercase()
}

fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// Split `key=value` / `key: value` into the normalized key and where the value
/// starts. Returns `None` when the token isn't an assignment.
fn split_assignment(word: &str) -> Option<(String, usize)> {
    let pos = word.find(['=', ':'])?;
    if pos + 1 >= word.len() {
        return None;
    }
    Some((clean_key(&word[..pos]), pos + 1))
}

/// Does this token look like a *value* rather than a word of prose? Used only
/// after a secret-ish key, where the cost of a miss is storing a credential and
/// the cost of a false positive is one mangled word.
fn value_like(word: &str) -> bool {
    let w = word.trim_matches(QUOTES);
    if w.len() < 8 {
        return false;
    }
    let has_digit = w.chars().any(|c| c.is_ascii_digit());
    let mixed_case = w.chars().any(char::is_uppercase) && w.chars().any(char::is_lowercase);
    let symbolic = w
        .chars()
        .any(|c| matches!(c, '.' | '-' | '_' | '+' | '/' | '='));
    has_digit || mixed_case || symbolic
}

fn looks_secret(word: &str) -> bool {
    let core = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
    if SECRET_PREFIXES
        .iter()
        .any(|p| core.starts_with(p) && core.len() > p.len() + 6)
    {
        return true;
    }
    if core.contains("BEGIN") && core.contains("PRIVATE") {
        return true;
    }
    // A long opaque blob: mixed case plus digits and nothing a sentence would
    // contain. Paths are excluded outright — `src/a_long_module/Name2.rs` would
    // otherwise qualify, and mangling file paths in stored memories is a worse
    // failure than missing a bare base64 blob that has no recognisable prefix.
    if core.len() >= 32
        && !core.contains('/')
        && !core.contains('.')
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '=' | '_' | '-'))
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_uppercase())
        && core.chars().any(|c| c.is_ascii_lowercase())
    {
        return true;
    }
    false
}

/// The JSON an agent reads back when we have context to add.
///
/// Claude Code, Codex, Qwen Code and Gemini all speak the same
/// `hookSpecificOutput.additionalContext` shape; only the event name differs.
/// Returns `None` for agents whose hook channel cannot carry context — Cursor
/// `beforeSubmitPrompt` is informational-only, and Copilot CLI ignores the
/// output of `userPromptSubmitted` entirely — so printing anything there is noise.
///
/// Antigravity has its own channel: it injects steps into the trajectory, and the
/// only way to add prose is an `ephemeralMessage` inside `injectSteps`. Its `Stop`
/// event also demands a JSON `decision` back, so the session-end reply is
/// `{"decision": "allow"}` — we never want to force the loop to continue.
pub fn hook_output(agent: &str, event: Event, context: &str) -> Option<Value> {
    if agent == "antigravity" {
        return match event {
            Event::Prompt => Some(json!({
                "injectSteps": [
                    { "ephemeralMessage": context }
                ]
            })),
            Event::SessionEnd => Some(json!({ "decision": "allow" })),
        };
    }
    if matches!(agent, "cursor" | "copilot-cli") {
        return None;
    }
    let name = if agent == "gemini-cli" {
        match event {
            Event::Prompt => "BeforeAgent",
            Event::SessionEnd => "SessionEnd",
        }
    } else {
        match event {
            Event::Prompt => "UserPromptSubmit",
            Event::SessionEnd => "SessionEnd",
        }
    };
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": name,
            "additionalContext": context,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_code_hook_json() {
        let (text, meta) = prompt_from_stdin(
            r#"{"session_id":"abc","cwd":"/tmp/x","hook_event_name":"UserPromptSubmit","prompt":"we use pnpm"}"#,
        );
        assert_eq!(text.as_deref(), Some("we use pnpm"));
        assert_eq!(meta["session_id"], "abc");
        assert_eq!(meta["cwd"], "/tmp/x");
    }

    #[test]
    fn antigravity_preinvocation_reads_prompt_from_transcript() {
        let dir = std::env::temp_dir().join(format!("fm-agy-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".system_generated/logs")).unwrap();
        let transcript = dir.join(".system_generated/logs/transcript.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                "{\"type\":\"SYSTEM\",\"content\":\"<SYSTEM_INSTRUCTION>boot</SYSTEM_INSTRUCTION>\"}\n",
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>we use pnpm for everything</USER_REQUEST>\"}\n",
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>second turn</USER_REQUEST>\"}\n",
            ),
        )
        .unwrap();
        let payload = json!({
            "invocationNum": 0,
            "transcriptPath": transcript.to_str().unwrap(),
        });
        let (text, meta) = prompt_from_agent(&payload.to_string(), "antigravity");
        assert_eq!(text.as_deref(), Some("second turn"), "take the most recent");
        assert_eq!(meta["invocationNum"], 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn antigravity_missing_transcript_yields_no_prompt() {
        let payload = json!({ "invocationNum": 0, "transcriptPath": "/nonexistent/x.jsonl" });
        let (text, _) = prompt_from_agent(&payload.to_string(), "antigravity");
        assert_eq!(text, None, "a missing transcript must not error");
    }

    #[test]
    fn antigravity_without_transcript_path_falls_back_to_stdin_keys() {
        let payload = json!({ "prompt": "plain prompt works too", "invocationNum": 2 });
        let (text, _) = prompt_from_agent(&payload.to_string(), "antigravity");
        assert_eq!(text.as_deref(), Some("plain prompt works too"));
    }

    #[test]
    fn antigravity_prompt_reads_latest_user_request_even_with_tool_noise() {
        let dir = std::env::temp_dir().join(format!("fm-agy2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("transcript.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>first</USER_REQUEST>\"}\n",
                "{\"type\":\"TOOL_USE\",\"content\":\"<TOOL>ls</TOOL>\"}\n",
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>now make the tests pass</USER_REQUEST>\"}\n",
            ),
        )
        .unwrap();
        let payload = json!({ "transcriptPath": transcript.to_str().unwrap() });
        let (text, _) = prompt_from_agent(&payload.to_string(), "antigravity");
        assert_eq!(text.as_deref(), Some("now make the tests pass"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_raw_text_for_agents_that_just_pipe() {
        let (text, meta) = prompt_from_stdin("  deploys go through fly.io  ");
        assert_eq!(text.as_deref(), Some("deploys go through fly.io"));
        assert_eq!(meta, json!({}));
    }

    #[test]
    fn empty_stdin_yields_nothing() {
        assert_eq!(prompt_from_stdin("   ").0, None);
    }

    #[test]
    fn acknowledgements_and_commands_are_skipped() {
        for noise in [
            "ok", "Sí", "dale", "continue", "thanks!", "/compact", "!ls -la",
        ] {
            assert!(
                skip_reason(noise, 12).is_some(),
                "{noise:?} should have been skipped"
            );
        }
    }

    #[test]
    fn real_prompts_are_kept_even_when_they_are_not_facts() {
        assert_eq!(skip_reason("fix the failing test in src/db.rs", 12), None);
    }

    #[test]
    fn a_slash_command_with_arguments_is_still_a_prompt() {
        // `/loop check the deploy` carries intent worth keeping; a bare `/compact`
        // does not.
        assert_eq!(skip_reason("/loop check the deploy every 5m", 12), None);
    }

    #[test]
    fn salience_classifies_rules_by_kind() {
        assert_eq!(
            salience("never force push to main, it breaks everyone's history"),
            Some("constraint")
        );
        assert_eq!(
            salience("nunca hagas force push a main, rompe el historial"),
            Some("constraint")
        );
        assert_eq!(
            salience("I prefer tabs over spaces in this repository, always"),
            Some("constraint"),
            "the strongest marker wins when several are present"
        );
        assert_eq!(
            salience("prefiero que uses pnpm en este repositorio"),
            Some("preference")
        );
        assert_eq!(
            salience("we decided to move the API to fly.io last week"),
            Some("decision")
        );
        assert_eq!(
            salience("remember that the staging database resets every night"),
            Some("note")
        );
    }

    #[test]
    fn questions_are_never_salient() {
        assert_eq!(salience("how do we deploy this thing to production?"), None);
        assert_eq!(salience("cómo se despliega esto en producción"), None);
        assert_eq!(salience("can you always use pnpm here?"), None);
    }

    #[test]
    fn ordinary_work_requests_are_not_salient() {
        assert_eq!(salience("fix the failing test in src/db.rs please"), None);
        assert_eq!(salience("add a spinner to the login page component"), None);
    }

    #[test]
    fn word_boundaries_stop_false_positives() {
        // "no" inside "node", "do not" as a phrase.
        assert!(!contains_phrase("we run node 22 in production", "no"));
        assert!(contains_phrase("do not commit that file", "do not"));
        assert!(!contains_phrase("nunca", "nunc"));
    }

    #[test]
    fn redacts_known_token_formats() {
        let (out, n) = redact("deploy with ghp_abcdefghijklmnopqrstuvwxyz012345 then push");
        assert!(out.contains("[redacted]"), "got {out}");
        assert!(!out.contains("ghp_abc"), "got {out}");
        assert_eq!(n, 1);
    }

    #[test]
    fn redacts_assignments_inline_and_next_word() {
        let (a, _) = redact("run with API_KEY=sk-livetokenvalue123456 in the env");
        assert!(a.contains("API_KEY=[redacted]"), "got {a}");
        let (b, _) = redact("the password is hunter2000000 by the way");
        assert!(b.contains("[redacted]"), "got {b}");
        assert!(!b.contains("hunter2000000"), "got {b}");
    }

    #[test]
    fn leaves_ordinary_prose_and_paths_alone() {
        let text = "always run `cargo nextest run` before touching src/retrieve.rs, \
                    it catches the regression in tests/integration.rs";
        let (out, n) = redact(text);
        assert_eq!(n, 0, "redacted something in: {out}");
        assert_eq!(out, text);
    }

    #[test]
    fn preserves_whitespace_exactly() {
        let text = "line one\n\tindented  double  spaced\n";
        assert_eq!(redact(text).0, text);
    }

    #[test]
    fn redacts_paths_matching_ignore_globs() {
        let home = std::path::Path::new("/home/test");
        let globs = vec![".env".to_string(), "*.pem".to_string()];
        let (out, n) = redact_paths(
            "check cat .env and /srv/app/secret.pem, then src/main.rs",
            &globs,
            home,
        );
        assert!(out.contains("[redacted]"), "got {out}");
        assert!(!out.contains(".env"), "got {out}");
        assert!(!out.contains("secret.pem"), "got {out}");
        assert!(
            out.contains("src/main.rs"),
            "unrelated path must survive: {out}"
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn redact_paths_matches_trailing_segment_and_home_tilde() {
        let home = std::path::Path::new("/home/test");
        // `~/.aws/*` should catch a path under the home dir even when spelled
        // with an absolute path.
        let globs = vec!["~/.aws/*".to_string()];
        let (out, n) = redact_paths(
            "the key is in /home/test/.aws/credentials, do not commit it",
            &globs,
            home,
        );
        assert!(out.contains("[redacted]"), "got {out}");
        assert!(!out.contains("credentials"), "got {out}");
        assert_eq!(n, 1);
    }

    #[test]
    fn redact_paths_ignores_globs_that_do_not_match() {
        let home = std::path::Path::new("/home/test");
        let globs = vec!["src/**.env".to_string()];
        let (out, n) = redact_paths("use .env.example for defaults", &globs, home);
        assert_eq!(n, 0, "got {out}");
        assert!(out.contains(".env.example"));
    }

    #[test]
    fn glob_matching_handles_star_globstar_and_question() {
        let home = std::path::Path::new("/home/test");
        assert!(path_glob_match("*.pem", "key.pem", home));
        assert!(path_glob_match("**/*.pem", "a/b/key.pem", home));
        assert!(path_glob_match("src/?.rs", "src/a.rs", home));
        assert!(
            path_glob_match("*.pem", "a/b/key.pem", home),
            "bare name matches trailing segment"
        );
        assert!(!path_glob_match("*.pem", "a/b/key.crt", home));
        assert!(path_glob_match("**/.env", ".env", home));
    }

    #[test]
    fn event_names_map_from_every_agents_spelling() {
        assert_eq!(Event::parse("UserPromptSubmit"), Some(Event::Prompt));
        assert_eq!(Event::parse("prompt"), Some(Event::Prompt));
        assert_eq!(Event::parse("BeforeAgent"), Some(Event::Prompt));
        assert_eq!(Event::parse("beforeSubmitPrompt"), Some(Event::Prompt));
        assert_eq!(Event::parse("userPromptSubmitted"), Some(Event::Prompt));
        assert_eq!(Event::parse("PreInvocation"), Some(Event::Prompt));
        assert_eq!(Event::parse("SessionEnd"), Some(Event::SessionEnd));
        assert_eq!(Event::parse("sessionEnd"), Some(Event::SessionEnd));
        assert_eq!(Event::parse("stop"), Some(Event::SessionEnd));
        assert_eq!(Event::parse("nonsense"), None);
    }

    #[test]
    fn antigravity_output_injects_ephemeral_message() {
        let v = hook_output("antigravity", Event::Prompt, "remember: pnpm").unwrap();
        assert_eq!(v["injectSteps"][0]["ephemeralMessage"], "remember: pnpm");
    }

    #[test]
    fn antigravity_stop_always_replies_with_a_decision() {
        let v = hook_output("antigravity", Event::SessionEnd, "").unwrap();
        assert_eq!(v["decision"], "allow");
    }

    #[test]
    fn cursor_and_copilot_never_get_output() {
        assert!(hook_output("cursor", Event::Prompt, "x").is_none());
        assert!(hook_output("copilot-cli", Event::Prompt, "x").is_none());
    }
}
