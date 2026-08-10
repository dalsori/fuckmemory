//! Tests that exercise the real binary and real concurrency, rather than the
//! library in isolation.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_fuckmemory");

fn scratch(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("fm-it-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(d.join("proj/.git")).unwrap();
    d
}

fn run(home: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(home.join("proj"))
        .env("FUCKMEMORY_HOME", home.join("data"))
        // Keyword-only: these tests must not depend on a 125 MB model download.
        .env("FUCKMEMORY_SEMANTIC", "0")
        .output()
        .expect("failed to run the binary");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

/// A live MCP server on stdio, driven request by request.
struct Mcp {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(home: &Path) -> Self {
        let mut child = Command::new(BIN)
            .arg("serve")
            .current_dir(home.join("proj"))
            .env("FUCKMEMORY_HOME", home.join("data"))
            .env("FUCKMEMORY_SEMANTIC", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn the server");
        let reader = BufReader::new(child.stdout.take().unwrap());
        Self { child, reader }
    }

    fn send(&mut self, msg: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("server closed");
        serde_json::from_str(&line).expect("server emitted non-JSON on stdout")
    }

    fn call(&mut self, id: i64, tool: &str, args: serde_json::Value) -> String {
        self.send(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            })
            .to_string(),
        );
        let r = self.recv();
        assert_eq!(r["id"], id, "response id mismatch: {r}");
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn cli_remember_recall_forget_round_trip() {
    let home = scratch("cli");
    let (out, err, ok) = run(&home, &["remember", "deploys go out through fly.io"]);
    assert!(ok, "remember failed: {err}");
    assert!(out.contains("remembered"), "got {out}");

    let (out, _, ok) = run(&home, &["recall", "how", "do", "deploys", "work"]);
    assert!(ok);
    assert!(out.contains("fly.io"), "got {out}");

    let (out, _, ok) = run(&home, &["forget", "--query", "deploys"]);
    assert!(ok, "got {out}");
    assert!(out.contains("retracted"), "got {out}");

    let (out, _, _) = run(&home, &["recall", "how", "do", "deploys", "work"]);
    assert!(out.contains("nothing found"), "got {out}");
}

#[test]
fn cli_reports_failure_with_a_nonzero_exit() {
    let home = scratch("fail");
    let (_, err, ok) = run(&home, &["recall", "--as-of", "not-a-date", "anything"]);
    assert!(!ok, "bad --as-of must fail");
    assert!(err.contains("as-of"), "got {err}");
}

#[test]
fn remember_with_a_file_points_recall_at_the_source() {
    let home = scratch("files");
    let file = home.join("proj/Makefile");
    std::fs::write(&file, "deploy:\n\tfly deploy\n\nlint:\n\tcargo clippy\n").unwrap();

    let (out, err, ok) = run(
        &home,
        &[
            "remember",
            "--file",
            "Makefile",
            "deploys run through the Makefile",
        ],
    );
    assert!(ok, "got {err}: {out}");

    // Normal recall shows the path (not the snippet).
    let (out, _, ok) = run(&home, &["recall", "how", "do", "we", "deploy"]);
    assert!(ok);
    assert!(out.contains("Makefile"), "recall must name the file: {out}");
    assert!(
        !out.contains("fly deploy"),
        "snippet hidden in normal recall: {out}"
    );

    // Debug recall shows the stored excerpt.
    let (out, _, _) = run(&home, &["recall", "--debug", "how", "do", "we", "deploy"]);
    assert!(
        out.contains("fly deploy"),
        "snippet visible in --debug: {out}"
    );

    // A line range keeps only those lines.
    std::fs::write(
        &file,
        (1..=40).map(|i| format!("line {i}\n")).collect::<String>(),
    )
    .unwrap();
    let (out, err, ok) = run(
        &home,
        &[
            "remember",
            "--file",
            "Makefile:10-12",
            "the file is now long",
        ],
    );
    assert!(ok, "got {err}: {out}");
    let (out, _, _) = run(&home, &["recall", "--debug", "now", "long"]);
    assert!(
        out.contains("line 10") && !out.contains("line 13"),
        "got {out}"
    );

    // A missing file fails the remember.
    let (_, err, ok) = run(&home, &["remember", "--file", "no-such.rs", "x"]);
    assert!(!ok, "missing file must fail");
    assert!(err.contains("reading"), "got {err}");
}

#[test]
fn global_scope_is_visible_from_a_project() {
    let home = scratch("scopes");
    run(
        &home,
        &[
            "remember",
            "--scope",
            "global",
            "never use emoji in commits",
        ],
    );
    run(&home, &["remember", "this project ships on fridays"]);

    let (out, _, _) = run(&home, &["recall", "emoji", "commits"]);
    assert!(
        out.contains("never use emoji"),
        "global must be readable: {out}"
    );

    // ...but a project fact must not leak into the global scope.
    let (out, _, _) = run(&home, &["recall", "--scope", "global", "fridays"]);
    assert!(
        !out.contains("fridays"),
        "project fact leaked into global: {out}"
    );
}

#[test]
fn superseding_a_fact_keeps_the_history_queryable() {
    let home = scratch("temporal");
    run(
        &home,
        &[
            "remember",
            "--src",
            "project",
            "--rel",
            "uses",
            "--dst",
            "npm",
            "the project uses npm",
        ],
    );
    run(
        &home,
        &[
            "remember",
            "--src",
            "project",
            "--rel",
            "uses",
            "--dst",
            "pnpm",
            "the project uses pnpm",
        ],
    );

    // These tests run keyword-only, so the query has to share a token with the
    // stored text — "package manager" would only match through the vector leg.
    let (out, _, _) = run(&home, &["recall", "pnpm", "npm"]);
    assert!(out.contains("uses pnpm"), "got {out}");
    assert!(
        !out.contains("uses npm"),
        "retracted fact still recalled: {out}"
    );

    let (out, _, ok) = run(&home, &["timeline", "project"]);
    assert!(ok);
    assert!(out.contains("uses npm"), "history lost: {out}");
    assert!(out.contains("uses pnpm"), "got {out}");
}

#[test]
fn export_then_import_into_a_fresh_store_preserves_facts() {
    let a = scratch("exp");
    run(
        &a,
        &["remember", "the api talks to postgres over pgbouncer"],
    );
    let (dump, _, ok) = run(&a, &["export"]);
    assert!(ok);
    let path = a.join("dump.json");
    std::fs::write(&path, &dump).unwrap();

    let b = scratch("imp");
    let (out, err, ok) = run(&b, &["import", path.to_str().unwrap()]);
    assert!(ok, "import failed: {err}");
    assert!(out.contains("imported 1 fact"), "got {out}");
    let (out, _, _) = run(&b, &["recall", "pgbouncer"]);
    assert!(out.contains("pgbouncer"), "got {out}");
}

#[test]
fn mcp_handshake_and_tool_round_trip_over_stdio() {
    let home = scratch("mcp");
    let mut s = Mcp::start(&home);

    s.send(
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "test", "version": "1" } }
        })
        .to_string(),
    );
    let init = s.recv();
    assert_eq!(init["result"]["serverInfo"]["name"], "fuckmemory");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    // A notification must produce no response at all; if it did, the next read
    // would return it instead of the tools/list result.
    s.send(
        &serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
    );

    s.send(&serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string());
    let list = s.recv();
    assert_eq!(list["id"], 2, "a notification was answered: {list}");
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["recall", "remember", "forget", "timeline"]);

    let out = s.call(
        3,
        "remember",
        serde_json::json!({ "text": "the queue drains via a cron every 5 minutes" }),
    );
    assert!(out.starts_with("Remembered"), "got {out}");

    let out = s.call(
        4,
        "recall",
        serde_json::json!({ "query": "how does the queue drain" }),
    );
    assert!(out.contains("cron"), "got {out}");
}

#[test]
fn mcp_remember_with_a_file_path_reads_the_snippet() {
    let home = scratch("mcp-files");
    std::fs::write(
        home.join("proj/pnpm-workspace.yaml"),
        "packages:\n  - 'apps/*'\n",
    )
    .unwrap();
    let mut s = Mcp::start(&home);

    let out = s.call(
        1,
        "remember",
        serde_json::json!({
            "text": "all apps live in the pnpm workspace",
            "files": [{ "path": "pnpm-workspace.yaml" }]
        }),
    );
    assert!(out.starts_with("Remembered"), "got {out}");

    let out = s.call(
        2,
        "recall",
        serde_json::json!({ "query": "where do the apps live" }),
    );
    assert!(
        out.contains("pnpm-workspace.yaml"),
        "recall names the file: {out}"
    );

    // An explicit snippet is trusted without touching the disk.
    let out = s.call(
        3,
        "remember",
        serde_json::json!({
            "text": "the root uses packageManager pnpm@9",
            "files": [{ "path": "package.json", "snippet": "\"packageManager\": \"pnpm@9.0.0\"" }]
        }),
    );
    assert!(out.starts_with("Remembered"), "got {out}");

    // A missing file warns but does not fail the whole write.
    let out = s.call(
        4,
        "remember",
        serde_json::json!({
            "text": "something valuable anyway",
            "files": [{ "path": "definitely/not/here.ts" }]
        }),
    );
    assert!(out.contains("warning"), "missing file should warn: {out}");
}

#[test]
fn mcp_survives_malformed_input_and_keeps_serving() {
    let home = scratch("mcp-bad");
    let mut s = Mcp::start(&home);
    s.send("this is not json at all");
    s.send("");
    s.send(&serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" }).to_string());
    let r = s.recv();
    assert_eq!(r["id"], 9, "server did not recover from junk input: {r}");
}

#[test]
fn concurrent_writers_do_not_corrupt_the_store() {
    let home = scratch("concurrent");
    // First run migrates; the rest race on a ready database.
    run(&home, &["stats"]);

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let home = home.clone();
            std::thread::spawn(move || {
                let (_, err, ok) =
                    run(&home, &["remember", &format!("concurrent fact number {i}")]);
                assert!(ok, "writer {i} failed: {err}");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let (out, _, ok) = run(&home, &["stats", "--json"]);
    assert!(ok);
    let s: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(s["facts_live"], 8, "lost or duplicated writes: {out}");
}

#[test]
fn concurrent_first_run_migrations_do_not_collide() {
    // Eight processes opening a brand-new database at once. Without the migration
    // lock, several race to CREATE TABLE and all but one fail.
    let home = scratch("migrate-race");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let home = home.clone();
            std::thread::spawn(move || run(&home, &["stats"]))
        })
        .collect();
    for h in handles {
        let (_, err, ok) = h.join().unwrap();
        assert!(ok, "migration race: {err}");
    }
}

#[test]
fn install_dry_run_writes_nothing() {
    let home = scratch("install-dry");
    let fake_home = home.join("fakehome");
    std::fs::create_dir_all(fake_home.join(".cursor")).unwrap();

    let out = Command::new(BIN)
        .args(["install", "--dry-run", "--only", "cursor", "--no-model"])
        .current_dir(home.join("proj"))
        .env("HOME", &fake_home)
        .env("FUCKMEMORY_HOME", home.join("data"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Dry run"), "got {stdout}");
    assert!(
        !fake_home.join(".cursor/mcp.json").exists(),
        "dry run created a config file"
    );
}

#[test]
fn install_then_uninstall_leaves_config_as_it_was() {
    let home = scratch("install-cycle");
    let fake_home = home.join("fakehome");
    std::fs::create_dir_all(fake_home.join(".cursor")).unwrap();
    let cfg = fake_home.join(".cursor/mcp.json");
    std::fs::write(
        &cfg,
        "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  }\n}\n",
    )
    .unwrap();

    let install = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(home.join("proj"))
            .env("HOME", &fake_home)
            .env("FUCKMEMORY_HOME", home.join("data"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    install(&[
        "install",
        "--only",
        "cursor",
        "--no-model",
        "--no-instructions",
    ]);
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(after["mcpServers"]["fuckmemory"]["command"].is_string());
    assert_eq!(after["mcpServers"]["other"]["command"], "x");

    install(&["uninstall", "--only", "cursor"]);
    let final_cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(final_cfg["mcpServers"].get("fuckmemory").is_none());
    assert_eq!(final_cfg["mcpServers"]["other"]["command"], "x");
}

/// Feed a hook payload to the real binary, the way an agent would.
fn hook(home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (String, String, bool) {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(home.join("proj"))
        .env("FUCKMEMORY_HOME", home.join("data"))
        .env("FUCKMEMORY_SEMANTIC", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to run the binary");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

const ON: &[(&str, &str)] = &[("FUCKMEMORY_AUTOSAVE", "1"), ("FUCKMEMORY_AUTORECALL", "1")];

fn payload(cwd: &Path, prompt: &str) -> String {
    serde_json::json!({
        "session_id": "test-session",
        "cwd": cwd.to_string_lossy(),
        "hook_event_name": "UserPromptSubmit",
        "prompt": prompt,
    })
    .to_string()
}

#[test]
fn autosave_keeps_every_prompt_but_only_promotes_the_durable_ones() {
    let home = scratch("autosave");
    let proj = home.join("proj");

    let rule = "never run migrations straight against production, always use the staging replica";
    let chore = "rename the variable in src/db.rs to something clearer";
    for prompt in [rule, chore] {
        let (_, err, ok) = hook(
            &home,
            &["hook", "prompt", "--agent", "claude-code"],
            &payload(&proj, prompt),
            ON,
        );
        assert!(ok, "hook must always exit 0: {err}");
    }

    // The rule became a fact: recall, which only reads facts, can see it.
    let (out, _, _) = run(&home, &["recall", "migrations", "production"]);
    assert!(
        out.contains("staging replica"),
        "rule was not promoted: {out}"
    );

    // The chore did not — but it was still stored, and `--raw` proves it.
    let (out, _, _) = run(&home, &["recall", "rename", "variable", "--raw"]);
    assert!(out.contains("src/db.rs"), "chore was lost entirely: {out}");
    let (out, _, _) = run(&home, &["recall", "rename", "variable"]);
    assert!(
        !out.contains("src/db.rs"),
        "a chore must not be ranked as a fact: {out}"
    );
}

#[test]
fn autosave_stays_out_of_the_way_when_it_is_off() {
    let home = scratch("autosave-off");
    let proj = home.join("proj");
    let (out, _, ok) = hook(
        &home,
        &["hook", "prompt", "--agent", "claude-code"],
        &payload(
            &proj,
            "we always deploy through fly.io and never through vercel",
        ),
        &[],
    );
    assert!(ok);
    assert!(out.is_empty(), "nothing should be injected: {out}");
    let (stats, _, _) = run(&home, &["stats"]);
    assert!(stats.contains("episodes        0"), "got {stats}");
}

#[test]
fn autorecall_hands_claude_code_the_memories_it_did_not_ask_for() {
    let home = scratch("autorecall");
    let proj = home.join("proj");
    run(
        &home,
        &["remember", "deploys go out through fly.io, never vercel"],
    );

    let (out, _, ok) = hook(
        &home,
        &["hook", "prompt", "--agent", "claude-code"],
        &payload(&proj, "how do deploys work here?"),
        ON,
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("hook must emit JSON");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(ctx.contains("fly.io"), "got {ctx}");
}

#[test]
fn a_hook_never_blocks_the_prompt_even_when_everything_is_wrong() {
    let home = scratch("hook-robust");
    // Garbage stdin, an unreadable data dir, and an unknown event: all must
    // still exit 0, because a non-zero exit here stops the user from working.
    for (args, stdin) in [
        (vec!["hook", "prompt"], "}{ not json at all"),
        (vec!["hook", "nonsense-event"], "{}"),
        (vec!["hook", "prompt"], ""),
    ] {
        let (_, _, ok) = hook(&home, &args, stdin, ON);
        assert!(ok, "hook exited non-zero for {args:?}");
    }
}

#[test]
fn secrets_pasted_into_a_prompt_are_never_written_to_disk() {
    let home = scratch("redact");
    let proj = home.join("proj");
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let (_, _, ok) = hook(
        &home,
        &["hook", "prompt", "--agent", "claude-code"],
        &payload(
            &proj,
            &format!("always deploy with the token {secret} in the environment"),
        ),
        ON,
    );
    assert!(ok);

    let (dump, _, _) = run(&home, &["export"]);
    assert!(!dump.contains(secret), "the token was stored: {dump}");
    assert!(dump.contains("[redacted]"), "got {dump}");
}

#[test]
fn ignore_paths_redact_file_references_from_autosaved_prompts() {
    let home = scratch("ignore");
    let proj = home.join("proj");
    std::fs::write(proj.join(".env"), "AWS_ACCESS_KEY=secret\n").unwrap();
    let env: &[(&str, &str)] = &[
        ("FUCKMEMORY_AUTOSAVE", "1"),
        ("FUCKMEMORY_IGNORE_PATHS", ".env,*.pem"),
    ];

    let (_, _, ok) = hook(
        &home,
        &["hook", "prompt", "--agent", "claude-code"],
        &payload(&proj, "check the .env for the deploy key before starting"),
        env,
    );
    assert!(ok);

    let (dump, _, _) = run(&home, &["export"]);
    assert!(
        !dump.contains(".env"),
        "the ignored path was stored: {dump}"
    );
    assert!(dump.contains("[redacted]"), "got {dump}");

    // Unrelated paths survive.
    std::fs::write(proj.join("main.rs"), "fn main() {}\n").unwrap();
    let (_, _, ok) = hook(
        &home,
        &["hook", "prompt", "--agent", "claude-code"],
        &payload(&proj, "read src/main.rs and fix the lint"),
        env,
    );
    assert!(ok);
    let (dump, _, _) = run(&home, &["export"]);
    assert!(
        dump.contains("main.rs"),
        "unrelated path was redacted: {dump}"
    );
}

#[test]
fn the_session_end_hook_consolidates_instead_of_storing() {
    let home = scratch("session-end");
    run(&home, &["remember", "the api runs on fly.io"]);
    let (before, _, _) = run(&home, &["stats"]);
    assert!(before.contains("unconsolidated  1"), "got {before}");

    let (_, err, ok) = hook(&home, &["hook", "session-end"], "{}", ON);
    assert!(ok, "{err}");
    let (after, _, _) = run(&home, &["stats"]);
    assert!(after.contains("unconsolidated  0"), "got {after}");
}

/// The fast path must agree with the model it replaces. Skipped when no model is
/// installed, since the alternative is a 125 MB download inside a test run.
#[test]
fn the_fast_embedding_cache_matches_the_real_model() {
    let cfg = match fuckmemory::Config::load() {
        Ok(c) if fuckmemory::embed::is_installed(&c) => c,
        _ => return,
    };
    fuckmemory::embed::build_cache(&cfg, false).expect("cache build");
    let worst = fuckmemory::fast::verify(&cfg).expect("verify");
    assert!(
        worst >= 0.9995,
        "fast path drifted from the model: worst cosine {worst}"
    );

    // And it must actually be the path in use, or the speedup is imaginary.
    let e = fuckmemory::embed::Embedder::load_if_cached(&cfg).expect("embedder");
    assert!(e.is_fast(), "the cache exists but was not used");
}

/// The whole point of the cache: a cold process must not pay for a model load.
#[test]
fn a_cold_recall_is_fast_when_the_cache_is_warm() {
    let cfg = match fuckmemory::Config::load() {
        Ok(c) if fuckmemory::embed::is_installed(&c) => c,
        _ => return,
    };
    if fuckmemory::embed::build_cache(&cfg, false).is_err() {
        return;
    }
    let started = std::time::Instant::now();
    let e = fuckmemory::embed::Embedder::load_if_cached(&cfg).expect("embedder");
    let load = started.elapsed();
    assert!(e.is_fast());
    // The real number is ~1 ms against ~206 ms for the model; 50 ms is a floor
    // loose enough to survive a loaded CI machine and still catch a regression
    // to the slow path.
    assert!(
        load < std::time::Duration::from_millis(50),
        "cold load took {load:?} — is it falling back to the full model?"
    );
}
