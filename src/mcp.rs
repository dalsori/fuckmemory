//! MCP stdio server — the universal interface.
//!
//! MCP is the one protocol every current coding agent speaks, so implementing it
//! once gets Claude Code, Codex, Gemini CLI, Antigravity, OpenCode, Cursor,
//! Copilot, Qwen and the rest for free. Framing is newline-delimited JSON-RPC
//! 2.0 on stdin/stdout.
//!
//! Hard rule: **stdout carries protocol only**. Every diagnostic goes to stderr,
//! because one stray `println!` corrupts the stream and the client just sees a
//! dead server.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::config::{now, Config};
use crate::embed::{Embedder, VecCache};
use crate::graph::When;
use crate::pack::{self, PackOptions};
use crate::retrieve::{self, Query};
use crate::store::{self, RememberInput};
use crate::{db, scope};

/// Protocol revision we advertise when the client doesn't state one.
const FALLBACK_PROTOCOL: &str = "2025-06-18";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Server {
    conn: Connection,
    cfg: Config,
    emb: Option<Embedder>,
    /// Load the model at most once, and never retry a failure on every call.
    emb_resolved: bool,
    cwd: PathBuf,
    /// Reuses the vector index across recalls; invalidated after our own writes.
    vec_cache: VecCache,
}

impl Server {
    pub fn new(cfg: Config) -> Result<Self> {
        let conn = db::open(&cfg.db_path())?;
        Ok(Self {
            conn,
            cfg,
            emb: None,
            emb_resolved: false,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            vec_cache: VecCache::new(),
        })
    }

    /// Lazily bring up the embedder. Only a *cached* model is used: a recall must
    /// not hang for minutes on a first-run download inside an agent session.
    ///
    /// Returns nothing on purpose — callers then take `&self.emb` as a plain field
    /// borrow, which coexists with `&mut self.conn`.
    fn ensure_embedder(&mut self) {
        if self.emb_resolved {
            return;
        }
        self.emb_resolved = true;
        if !self.cfg.semantic {
            return;
        }
        self.emb = Embedder::load_if_cached(&self.cfg);
        if self.emb.is_none() {
            eprintln!(
                "fuckmemory: no embedding model cached, running keyword-only. \
                 Run `fuckmemory model pull` to enable semantic recall."
            );
        }
    }

    fn tools() -> Value {
        json!([
            {
                "name": "recall",
                "description": "Search the memory shared by every agent on this machine for what is \
        already known about this project and this user. Call it at the start of a task and any time you are \
        about to assume a convention, a command, a preference or a past decision. Returns a short, \
        token-budgeted list of standalone facts, newest and most-used first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What you want to know, in natural language. Include concrete tokens (file paths, flags, tool names) when you have them." },
                        "limit": { "type": "integer", "description": "Max memories to return. Default 12." },
                        "budget_tokens": { "type": "integer", "description": "Hard cap on the size of the answer. Default 1200." },
                        "scope": { "type": "string", "description": "'global' for cross-project memory, a path for another project, omitted for the current one." },
                        "as_of": { "type": "string", "description": "YYYY-MM-DD or epoch seconds. Returns what was believed at that time, including facts since retracted." },
                        "hops": { "type": "integer", "description": "Graph expansion depth, 0-2. Default 1. 0 disables related-fact expansion." },
                        "include_raw": { "type": "boolean", "description": "Also return the original unedited notes. Off by default." }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "remember",
                "description": "Store something that will still matter in a future session. Worth storing: \
        decisions and the reason behind them, user preferences, commands that actually work, constraints, \
        gotchas that cost you time. Not worth storing: transient state, anything re-readable from the code, \
        and never secrets or credentials. Prefer passing `facts` with subject/relation/object — it makes \
        recall far better, and a new value of a single-valued relation (uses, prefers, is, version…) \
        automatically retires the old one instead of contradicting it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "The memory as a standalone sentence. It will be read with no surrounding context." },
                        "kind": { "type": "string", "enum": ["note", "decision", "preference", "constraint", "error"], "description": "Default 'note'." },
                        "scope": { "type": "string", "description": "'global' for things true of the user everywhere (preferences, style). Omit for project-specific facts." },
                        "facts": {
                            "type": "array",
                            "description": "Structured form. One entry per distinct claim.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "src": { "type": "string", "description": "Subject, e.g. 'the API', 'CI', 'the user'." },
                                    "rel": { "type": "string", "description": "Relation, snake_case, e.g. uses, prefers, requires, forbids, depends_on, runs_on." },
                                    "dst": { "type": "string", "description": "Object, e.g. 'pnpm', 'Node 22'." },
                                    "statement": { "type": "string", "description": "The full sentence to inject later." },
                                    "valid_from": { "type": "string", "description": "YYYY-MM-DD when this became true in the world, if not now." },
                                    "confidence": { "type": "number", "description": "0-1. Below 1 for things you inferred rather than were told." }
                                },
                                "required": ["statement"]
                            }
                        },
                        "files": {
                            "type": "array",
                            "description": "Files this memory was learned against. Pass `path` (plus optional `line_from`/`line_to`) and the snippet is read from disk; or pass `snippet` directly to store exactly what you saw.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string", "description": "File path, relative to the workspace or absolute." },
                                    "line_from": { "type": "integer", "description": "First line kept (1-based, inclusive)." },
                                    "line_to": { "type": "integer", "description": "Last line kept (1-based, inclusive)." },
                                    "snippet": { "type": "string", "description": "Optional: the exact excerpt to store, overriding a disk read." }
                                },
                                "required": ["path"]
                            }
                        }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "forget",
                "description": "Retract a memory that is wrong or obsolete. Soft by default: it stops \
        being recalled but stays in the timeline so history stays honest. Use mode='hard' only for secrets \
        or anything that must genuinely disappear.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "description": "Fact id, as shown by recall in debug mode or by the CLI." },
                        "query": { "type": "string", "description": "Alternative to id: retract the single best match for this text." },
                        "mode": { "type": "string", "enum": ["soft", "hard"], "description": "Default 'soft'." },
                        "scope": { "type": "string" }
                    }
                }
            },
            {
                "name": "timeline",
                "description": "Show how what we know about one entity changed over time, retracted \
        values included. Use it for 'when did this change', 'what did we use before', or to check whether a \
        belief is stale.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "entity": { "type": "string", "description": "Entity name, e.g. 'CI', 'the API', 'pnpm'." },
                        "limit": { "type": "integer", "description": "Default 40." },
                        "scope": { "type": "string" }
                    },
                    "required": ["entity"]
                }
            }
        ])
    }

    fn handle(&mut self, req: &Value) -> Option<Value> {
        // A message with no id is a notification, and answering one is a protocol
        // error — so bail out before doing any work.
        let id = req.get("id").cloned()?;
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        let result = match method {
            "initialize" => {
                let proto = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(FALLBACK_PROTOCOL)
                    .to_string();
                Ok(json!({
                    "protocolVersion": proto,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "fuckmemory", "version": SERVER_VERSION },
                    "instructions": "Persistent memory shared across every agent on this machine. \
                Call `recall` before assuming anything about this project; call `remember` when you learn something \
                that will matter next session."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": Self::tools() })),
            // Declared as unsupported in capabilities, but some clients probe anyway.
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.call_tool(name, &args) {
                    Ok(text) => Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    })),
                    // Tool failures come back as results, not JSON-RPC errors, so
                    // the model can read the message and correct itself.
                    Err(e) => Ok(json!({
                        "content": [{ "type": "text", "text": format!("error: {e:#}") }],
                        "isError": true
                    })),
                }
            }
            other => Err((-32601i64, format!("unknown method: {other}"))),
        };

        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        })
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<String> {
        match name {
            "recall" => self.tool_recall(args),
            "remember" => self.tool_remember(args),
            "forget" => self.tool_forget(args),
            "timeline" => self.tool_timeline(args),
            other => anyhow::bail!("unknown tool: {other}"),
        }
    }

    fn scope_of(&self, args: &Value) -> Result<scope::Scope> {
        let spec = args.get("scope").and_then(Value::as_str);
        scope::resolve(&self.conn, spec, &self.cwd)
    }

    fn tool_recall(&mut self, args: &Value) -> Result<String> {
        let text = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        anyhow::ensure!(!text.trim().is_empty(), "query is required");

        let sc = self.scope_of(args)?;
        let scope_ids = scope::read_set(&self.conn, &sc)?;
        let when = match args.get("as_of").and_then(Value::as_str) {
            Some(s) => When::AsOf(
                pack::parse_when(s)
                    .ok_or_else(|| anyhow::anyhow!("as_of must be YYYY-MM-DD or epoch seconds"))?,
            ),
            None => When::Live,
        };
        let q = Query {
            text,
            limit: uint(args, "limit").unwrap_or(12).clamp(1, 100),
            when,
            hops: uint(args, "hops").unwrap_or(1).min(2),
            include_episodes: args
                .get("include_raw")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };

        self.ensure_embedder();
        let r = retrieve::recall(
            &self.conn,
            &scope_ids,
            self.emb.as_ref(),
            &q,
            Some(&mut self.vec_cache),
        )?;

        let out = pack::render(
            &r,
            &PackOptions {
                budget_tokens: uint(args, "budget_tokens")
                    .unwrap_or(self.cfg.budget_tokens)
                    .clamp(64, 20_000),
                scope_label: sc.label.clone(),
                debug: false,
            },
            now(),
        );
        if out.is_empty() {
            return Ok("No memories match. Nothing is known about this yet — consider `remember` once you find out.".into());
        }
        store::mark_hits(&self.conn, &pack::rendered_ids(&r, &out))?;
        Ok(out)
    }

    fn tool_remember(&mut self, args: &Value) -> Result<String> {
        let sc = self.scope_of(args)?;
        // Reuse the store input shape; `valid_from` arrives as a date string here.
        let mut normalized = args.clone();
        if let Some(facts) = normalized.get_mut("facts").and_then(Value::as_array_mut) {
            for f in facts.iter_mut() {
                if let Some(vf) = f.get("valid_from").and_then(Value::as_str) {
                    match pack::parse_when(vf) {
                        Some(ts) => {
                            f["valid_from"] = json!(ts);
                        }
                        None => anyhow::bail!("valid_from must be YYYY-MM-DD or epoch seconds"),
                    }
                }
            }
        }
        if normalized.get("source").is_none() {
            normalized["source"] = json!(client_name());
        }
        // Resolve files: a bare `path` (optionally with `lines`) is read off
        // disk so recall can point at the actual excerpt. A caller that already
        // handed us a `snippet` is trusted as-is.
        if let Some(files) = normalized.get_mut("files").and_then(Value::as_array_mut) {
            let base = self.cwd.clone();
            for f in files.iter_mut() {
                if f.get("snippet").map(Value::is_string).unwrap_or(false) {
                    continue;
                }
                let Some(path) = f.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let lines = match (f.get("line_from"), f.get("line_to")) {
                    (Some(a), Some(b)) => {
                        let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) else {
                            anyhow::bail!("line_from/line_to must be integers");
                        };
                        Some((a, b))
                    }
                    (None, None) => None,
                    _ => anyhow::bail!("pass both line_from and line_to, or neither"),
                };
                match store::read_file_input(path, &base, lines) {
                    Ok(fi) => {
                        f["snippet"] = json!(fi.snippet);
                        f["lang"] = fi.lang.map(Value::String).unwrap_or(Value::Null);
                        f["line_from"] = fi.line_from.map(Value::from).unwrap_or(Value::Null);
                        f["line_to"] = fi.line_to.map(Value::from).unwrap_or(Value::Null);
                    }
                    Err(e) => {
                        // Don't fail the whole write for one unreadable file; the
                        // memory itself is still valuable. Tell the caller.
                        return Ok(format!("warning: {e:#}"));
                    }
                }
            }
        }
        let input: RememberInput = serde_json::from_value(normalized)
            .map_err(|e| anyhow::anyhow!("bad arguments: {e}"))?;

        self.ensure_embedder();
        let out = store::remember(&mut self.conn, &sc, self.emb.as_ref(), &input)?;
        // Our own write does not move `data_version`, so the vector cache would
        // silently miss the new fact unless dropped here.
        self.vec_cache.invalidate();

        let mut msg = if out.duplicate {
            format!("Already remembered (episode {}).", out.episode_id)
        } else {
            format!(
                "Remembered in scope '{}': {} fact(s).",
                sc.label,
                out.fact_ids.len()
            )
        };
        if !out.superseded.is_empty() {
            msg.push_str(&format!(
                " Retired {} now-outdated fact(s).",
                out.superseded.len()
            ));
        }
        if !out.fact_ids.is_empty() {
            msg.push_str(&format!(" ids: {:?}", out.fact_ids));
        }
        Ok(msg)
    }

    fn tool_forget(&mut self, args: &Value) -> Result<String> {
        let sc = self.scope_of(args)?;
        let hard = args.get("mode").and_then(Value::as_str) == Some("hard");

        let id = match args.get("id").and_then(Value::as_i64) {
            Some(id) => id,
            None => {
                let q = args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("pass either id or query"))?;
                let scope_ids = scope::read_set(&self.conn, &sc)?;
                self.ensure_embedder();
                let r = retrieve::recall(
                    &self.conn,
                    &scope_ids,
                    self.emb.as_ref(),
                    &Query {
                        text: q.to_string(),
                        limit: 1,
                        ..Default::default()
                    },
                    Some(&mut self.vec_cache),
                )?;
                r.hits
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("nothing matched {q:?}"))?
                    .fact
                    .id
            }
        };

        let ok = if hard {
            store::purge(&self.conn, &sc, id)?
        } else {
            store::invalidate(&self.conn, &sc, id)?
        };
        // Same rationale as `remember`: a retraction is a write this connection
        // made, invisible to `data_version`.
        self.vec_cache.invalidate();
        Ok(if ok {
            format!("{} fact {id}.", if hard { "Deleted" } else { "Retracted" })
        } else {
            format!(
                "Fact {id} not found in scope '{}' (or already retracted).",
                sc.label
            )
        })
    }

    fn tool_timeline(&mut self, args: &Value) -> Result<String> {
        let entity = args
            .get("entity")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("entity is required"))?;
        let sc = self.scope_of(args)?;
        let scope_ids = scope::read_set(&self.conn, &sc)?;
        let limit = uint(args, "limit").unwrap_or(40).clamp(1, 200);
        let rows = crate::graph::timeline(&self.conn, &scope_ids, entity, limit)?;
        if rows.is_empty() {
            return Ok(format!("Nothing recorded about {entity:?}."));
        }
        let mut out = format!("## Timeline — {entity}\n");
        for f in rows {
            let start = pack::ymd(f.valid_from.unwrap_or(f.recorded_at));
            let end = match (f.invalidated_at, f.valid_to) {
                (Some(_), Some(t)) => pack::ymd(t),
                (Some(t), None) => pack::ymd(t),
                _ => "now".to_string(),
            };
            out.push_str(&format!("- {start} → {end}  {}\n", f.statement.trim()));
        }
        Ok(out)
    }
}

fn uint(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .map(|n| n as usize)
}

/// Best guess at which agent is talking to us, for provenance. Each CLI leaves
/// a fingerprint in the environment of the processes it spawns.
fn client_name() -> String {
    for (var, name) in [
        ("CLAUDECODE", "claude-code"),
        ("CLAUDE_CODE_SSE_PORT", "claude-code"),
        ("CODEX_SANDBOX", "codex"),
        ("GEMINI_CLI", "gemini-cli"),
        ("OPENCODE", "opencode"),
        ("CURSOR_TRACE_ID", "cursor"),
        ("QWEN_CODE", "qwen"),
    ] {
        if std::env::var_os(var).is_some() {
            return name.to_string();
        }
    }
    std::env::var("FUCKMEMORY_CLIENT").unwrap_or_else(|_| "mcp".into())
}

/// Run the server until stdin closes.
pub fn serve(cfg: Config) -> Result<()> {
    let mut server = Server::new(cfg)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Parse errors have no id to answer to; report and keep going.
                eprintln!("fuckmemory: bad JSON on stdin: {e}");
                continue;
            }
        };

        // JSON-RPC batches were dropped in 2025-06-18 but older clients send them.
        let responses: Vec<Value> = match &parsed {
            Value::Array(items) => items.iter().filter_map(|r| server.handle(r)).collect(),
            single => server.handle(single).into_iter().collect(),
        };
        for r in responses {
            writeln!(stdout, "{r}")?;
        }
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own database. Sharing one across parallel tests would
    /// make them order-dependent.
    fn test_server() -> Server {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fm-mcp-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = Config {
            home: dir,
            model: "none".into(),
            budget_tokens: 800,
            semantic: false,
            ..Config::default()
        };
        Server::new(cfg).unwrap()
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(s: &mut Server, tool: &str, args: Value) -> String {
        let r = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": tool, "arguments": args }),
            ))
            .unwrap();
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn initialize_echoes_the_clients_protocol_version() {
        let mut s = test_server();
        let r = s
            .handle(&req(
                1,
                "initialize",
                json!({ "protocolVersion": "2025-03-26" }),
            ))
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "fuckmemory");
    }

    #[test]
    fn initialize_falls_back_when_unstated() {
        let mut s = test_server();
        let r = s.handle(&req(1, "initialize", json!({}))).unwrap();
        assert_eq!(r["result"]["protocolVersion"], FALLBACK_PROTOCOL);
    }

    #[test]
    fn notifications_get_no_response() {
        let mut s = test_server();
        assert!(s
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
    }

    #[test]
    fn tools_list_exposes_four_tools_with_schemas() {
        let mut s = test_server();
        let r = s.handle(&req(1, "tools/list", json!({}))).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        for t in tools {
            assert!(t["name"].is_string());
            assert!(t["description"].as_str().unwrap().len() > 40);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let mut s = test_server();
        let r = s.handle(&req(7, "does/not/exist", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], 7);
    }

    #[test]
    fn tool_error_is_a_result_not_a_protocol_error() {
        let mut s = test_server();
        let r = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": "recall", "arguments": {} }),
            ))
            .unwrap();
        assert!(r.get("error").is_none(), "must not be a JSON-RPC error");
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn remember_then_recall_round_trips() {
        let mut s = test_server();
        let scope = format!("/tmp/fm-mcp-rt-{}", std::process::id());
        let out = call(
            &mut s,
            "remember",
            json!({ "text": "deploys go out through fly.io, never through vercel", "scope": scope }),
        );
        assert!(
            out.starts_with("Remembered") || out.starts_with("Already"),
            "got {out}"
        );

        let got = call(
            &mut s,
            "recall",
            json!({ "query": "how do deploys work", "scope": scope }),
        );
        assert!(got.contains("fly.io"), "got {got}");
    }

    #[test]
    fn recall_with_no_matches_says_so_plainly() {
        let mut s = test_server();
        let got = call(
            &mut s,
            "recall",
            json!({ "query": "zzzz nonexistent topic", "scope": format!("/tmp/fm-empty-{}", std::process::id()) }),
        );
        assert!(got.contains("No memories match"), "got {got}");
    }

    #[test]
    fn remember_rejects_a_bad_valid_from() {
        let mut s = test_server();
        let out = call(
            &mut s,
            "remember",
            json!({
                "text": "x",
                "facts": [{ "statement": "x", "valid_from": "last march" }]
            }),
        );
        assert!(out.contains("valid_from"), "got {out}");
    }

    #[test]
    fn forget_without_id_or_query_explains_itself() {
        let mut s = test_server();
        let out = call(&mut s, "forget", json!({}));
        assert!(out.contains("id or query"), "got {out}");
    }

    #[test]
    fn forget_by_query_retracts_the_match() {
        let mut s = test_server();
        let scope = format!("/tmp/fm-forget-{}", std::process::id());
        call(
            &mut s,
            "remember",
            json!({ "text": "the staging url is old and wrong", "scope": scope }),
        );
        let out = call(
            &mut s,
            "forget",
            json!({ "query": "staging url", "scope": scope }),
        );
        assert!(out.starts_with("Retracted"), "got {out}");
        let after = call(
            &mut s,
            "recall",
            json!({ "query": "staging url", "scope": scope }),
        );
        assert!(after.contains("No memories match"), "got {after}");
    }

    #[test]
    fn timeline_reports_absence_clearly() {
        let mut s = test_server();
        let out = call(&mut s, "timeline", json!({ "entity": "nothing-here" }));
        assert!(out.contains("Nothing recorded"), "got {out}");
    }
}
