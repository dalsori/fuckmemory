//! CLI entry point.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use fuckmemory::config::{now, Config};
use fuckmemory::embed::Embedder;
use fuckmemory::graph::When;
use fuckmemory::install::{self, What};
use fuckmemory::pack::{self, PackOptions};
use fuckmemory::retrieve::{self, Query};
use fuckmemory::store::{self, FactInput, RememberInput};
use fuckmemory::{consolidate, db, graph, mcp, scope, update};

#[derive(Parser)]
#[command(
    name = "fuckmemory",
    version,
    about = "One local memory, shared by every AI coding agent you use",
    long_about = "A local-first temporal knowledge graph that every agent on this machine reads and \
writes through MCP. No cloud, no API keys, no LLM call in the write path."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect installed agents and wire this in as an MCP server
    Install {
        /// Show what would change and exit
        #[arg(long)]
        dry_run: bool,
        /// Restrict to these agent ids (see `agents`)
        #[arg(long, value_delimiter = ',')]
        only: Option<Vec<String>>,
        /// Also wire it into the current project's config files
        #[arg(long)]
        project: bool,
        /// Skip user-level configs
        #[arg(long)]
        no_global: bool,
        /// Don't write the "when to use memory" block into AGENTS.md / CLAUDE.md
        #[arg(long)]
        no_instructions: bool,
        /// Don't download the embedding model
        #[arg(long)]
        no_model: bool,
        /// Turn on autosave and wire the per-prompt hooks
        #[arg(long)]
        autosave: bool,
        /// Don't touch hooks, even when autosave is already enabled
        #[arg(long)]
        no_hooks: bool,
        /// Command agents should spawn (default: this binary's absolute path)
        #[arg(long)]
        command: Option<String>,
    },
    /// Remove the MCP registration and the instructions block
    Uninstall {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_delimiter = ',')]
        only: Option<Vec<String>>,
        #[arg(long)]
        project: bool,
    },
    /// Run the MCP server on stdio (agents call this; you normally don't)
    Serve,
    /// Interactive settings screen
    Tui,
    /// Handle an agent hook: autosave the prompt, inject recalled memories
    ///
    /// Agents call this, one process per event. It reads the hook payload on
    /// stdin, never fails loudly, and prints context for the agent on stdout.
    Hook {
        /// prompt | session-end (agent spellings like UserPromptSubmit also work)
        event: String,
        /// Which agent is calling, for provenance
        #[arg(long, default_value = "unknown")]
        agent: String,
        /// Use this text instead of reading stdin
        #[arg(long)]
        text: Option<String>,
        /// Report what was stored on stderr — for `--dry-run`-style debugging
        #[arg(long)]
        verbose: bool,
    },
    /// List agents this machine has, and where each one is configured
    Agents,
    /// Store a memory
    Remember {
        /// The memory, as a standalone sentence
        text: Vec<String>,
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long)]
        scope: Option<String>,
        /// Subject of the fact
        #[arg(long)]
        src: Option<String>,
        /// Relation, snake_case (uses, prefers, requires, forbids…)
        #[arg(long)]
        rel: Option<String>,
        /// Object of the fact
        #[arg(long)]
        dst: Option<String>,
        /// When this became true in the world (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Attach a file (repeatable): its head is stored so recall can point at
        /// it. `path:from-to` attaches only those lines.
        #[arg(long, value_name = "PATH[:FROM-TO]", value_delimiter = ',')]
        file: Vec<String>,
    },
    /// Search memory
    Recall {
        query: Vec<String>,
        #[arg(long, short, default_value_t = 12)]
        limit: usize,
        #[arg(long)]
        scope: Option<String>,
        /// What was believed at this date (YYYY-MM-DD or epoch)
        #[arg(long)]
        as_of: Option<String>,
        /// Graph expansion depth, 0-2
        #[arg(long, default_value_t = 1)]
        hops: usize,
        #[arg(long)]
        budget: Option<usize>,
        /// Also show the raw original notes
        #[arg(long)]
        raw: bool,
        /// Show scores, ids and which retriever found each hit
        #[arg(long)]
        debug: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show each retriever's raw view of a query, for tuning and debugging
    Explain {
        query: Vec<String>,
        #[arg(long)]
        scope: Option<String>,
    },
    /// Check for and apply the latest release
    Update {
        /// Report what's available without downloading anything
        #[arg(long)]
        check: bool,
        /// Re-download and replace even when already up to date
        #[arg(long)]
        force: bool,
    },
    /// Retract a memory (soft by default)
    Forget {
        /// Fact id
        id: Option<i64>,
        /// Retract the best match for this text instead
        #[arg(long)]
        query: Option<String>,
        /// Delete permanently — for secrets
        #[arg(long)]
        hard: bool,
        #[arg(long)]
        scope: Option<String>,
    },
    /// How knowledge about one entity changed over time
    Timeline {
        entity: String,
        #[arg(long, default_value_t = 40)]
        limit: usize,
        #[arg(long)]
        scope: Option<String>,
    },
    /// Counts and database size
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// List memory scopes
    Scopes,
    /// Check the install: paths, model, schema, registrations
    Doctor {
        /// Repair what the check finds: rebuild missing FTS indexes, fetch the
        /// model and cache, re-embed, wire detected agents, consolidate
        #[arg(long)]
        fix: bool,
    },
    /// Merge duplicates, close contradictions, compact the indexes
    Consolidate {
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Hard-delete retracted, never-read facts older than N days
    Prune {
        #[arg(long, default_value_t = 90)]
        days: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-embed everything (after changing models)
    Reindex,
    /// Manage the embedding model
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Dump a scope as JSON
    Export {
        #[arg(long)]
        scope: Option<String>,
    },
    /// Load a JSON dump produced by `export`
    Import {
        file: PathBuf,
        #[arg(long)]
        scope: Option<String>,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Download (or verify) the embedding model
    Pull,
    /// Show which model is in use and where it lives
    Which,
    /// Build the mmap cache that makes cold starts ~50x faster
    Cache {
        /// Rebuild even if a valid cache is already there
        #[arg(long)]
        force: bool,
    },
    /// Check the fast cache against the real model on a probe corpus
    Verify,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fuckmemory: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;

    match cli.cmd {
        Cmd::Serve => mcp::serve(cfg),
        Cmd::Tui => fuckmemory::tui::run(cfg),
        Cmd::Hook {
            event,
            agent,
            text,
            verbose,
        } => cmd_hook(&cfg, event, agent, text, verbose),
        Cmd::Install {
            dry_run,
            only,
            project,
            no_global,
            no_instructions,
            no_model,
            autosave,
            no_hooks,
            command,
        } => cmd_install(
            &cfg,
            InstallArgs {
                dry_run,
                only,
                project,
                global: !no_global,
                instructions: !no_instructions,
                want_model: !no_model,
                autosave,
                hooks: !no_hooks,
                command,
            },
        ),
        Cmd::Uninstall {
            dry_run,
            only,
            project,
        } => cmd_uninstall(&cfg, dry_run, only, project),
        Cmd::Agents => cmd_agents(),
        Cmd::Remember {
            text,
            kind,
            scope,
            src,
            rel,
            dst,
            since,
            file,
        } => cmd_remember(&cfg, text, kind, scope, src, rel, dst, since, file),
        Cmd::Recall {
            query,
            limit,
            scope,
            as_of,
            hops,
            budget,
            raw,
            debug,
            json,
        } => cmd_recall(
            &cfg, query, limit, scope, as_of, hops, budget, raw, debug, json,
        ),
        Cmd::Explain { query, scope } => cmd_explain(&cfg, query, scope),
        Cmd::Update { check, force } => cmd_update(check, force),
        Cmd::Forget {
            id,
            query,
            hard,
            scope,
        } => cmd_forget(&cfg, id, query, hard, scope),
        Cmd::Timeline {
            entity,
            limit,
            scope,
        } => cmd_timeline(&cfg, entity, limit, scope),
        Cmd::Stats { json } => cmd_stats(&cfg, json),
        Cmd::Scopes => cmd_scopes(&cfg),
        Cmd::Doctor { fix } => cmd_doctor(&cfg, fix),
        Cmd::Consolidate { limit } => cmd_consolidate(&cfg, limit),
        Cmd::Prune { days, dry_run } => {
            let conn = db::open(&cfg.db_path())?;
            let n = consolidate::prune(&conn, days, dry_run)?;
            println!(
                "{n} retracted, never-read fact(s) older than {days}d{}",
                if dry_run {
                    " would be deleted"
                } else {
                    " deleted"
                }
            );
            if !dry_run {
                consolidate::drop_orphan_entities(&conn)?;
            }
            Ok(())
        }
        Cmd::Reindex => {
            let mut conn = db::open(&cfg.db_path())?;
            let emb = Embedder::load(&cfg)?;
            let n = consolidate::reindex(&mut conn, &emb)?;
            println!("re-embedded {n} fact(s) at dim {}", emb.dim);
            Ok(())
        }
        Cmd::Model { cmd } => match cmd {
            ModelCmd::Pull => {
                let dim = fuckmemory::embed::prefetch(&cfg)?;
                println!("{} ready ({dim} dims)", cfg.model);
                println!("  {}", fuckmemory::embed::describe_model_location(&cfg));
                Ok(())
            }
            ModelCmd::Which => {
                println!("model: {}", cfg.model);
                println!(
                    "where: {}",
                    fuckmemory::embed::describe_model_location(&cfg)
                );
                println!("cache: {}", describe_cache(&cfg));
                Ok(())
            }
            ModelCmd::Cache { force } => {
                let t = std::time::Instant::now();
                let rows = fuckmemory::embed::build_cache(&cfg, force)?;
                println!(
                    "cache ready — {rows} tokens in {:.1}s at {}",
                    t.elapsed().as_secs_f64(),
                    fuckmemory::fast::cache_path(&cfg).display()
                );
                Ok(())
            }
            ModelCmd::Verify => {
                let worst = fuckmemory::fast::verify(&cfg)?;
                println!("worst cosine against the real model: {worst:.6}");
                println!(
                    "{}",
                    if worst >= 0.9995 {
                        "the fast path agrees with the model"
                    } else {
                        "MISMATCH — run `fuckmemory model cache --force`"
                    }
                );
                Ok(())
            }
        },
        Cmd::Export { scope: s } => {
            let conn = db::open(&cfg.db_path())?;
            let sc = scope::resolve(&conn, s.as_deref(), &cwd())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store::export_scope(&conn, &sc)?)?
            );
            Ok(())
        }
        Cmd::Import { file, scope: s } => cmd_import(&cfg, file, s),
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Parse a `FROM-TO` line range as used by `--file PATH:FROM-TO`. Returns `None`
/// for anything malformed.
fn parse_line_range(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once('-')?;
    let a: i64 = a.trim().parse().ok()?;
    let b: i64 = b.trim().parse().ok()?;
    if a < 1 || b < a {
        return None;
    }
    Some((a, b))
}

fn home() -> Result<PathBuf> {
    dirs::home_dir().context("cannot find your home directory")
}

/// Load the embedder only if it's already on disk. Every CLI path uses this
/// rather than `load`, so no command silently blocks on a download.
fn cached_embedder(cfg: &Config) -> Option<Embedder> {
    if cfg.semantic {
        Embedder::load_if_cached(cfg)
    } else {
        None
    }
}

struct InstallArgs {
    dry_run: bool,
    only: Option<Vec<String>>,
    project: bool,
    global: bool,
    instructions: bool,
    want_model: bool,
    autosave: bool,
    hooks: bool,
    command: Option<String>,
}

fn cmd_install(cfg: &Config, args: InstallArgs) -> Result<()> {
    let InstallArgs {
        dry_run,
        only,
        project,
        global,
        instructions,
        want_model,
        autosave,
        hooks,
        command,
    } = args;
    let home = home()?;

    // `--autosave` is a settings change, not just a wiring change: without it
    // persisted, the hooks would fire into a binary that has autosave off.
    let mut cfg = cfg.clone();
    if autosave && !dry_run {
        cfg.autosave = true;
        cfg.autorecall = true;
        cfg.save()?;
    }
    let cfg = &cfg;

    let opts = install::Options {
        command: command.unwrap_or_else(install::self_command),
        global,
        project: if project {
            Some(scope::project_root(&cwd()))
        } else {
            None
        },
        only,
        instructions,
        // Only wire hooks for a store that will actually use them.
        hooks: hooks && (autosave || cfg.autosave || cfg.autorecall),
        dry_run,
    };

    let found = install::detected(&home);
    if found.is_empty() {
        println!("No supported agents detected — nothing to do.");
        println!("`fuckmemory agents` shows what is looked for.");
        return Ok(());
    }
    println!(
        "detected  {}",
        found.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
    );
    println!("command   {}\n", opts.command);

    let changes = install::apply(&home, &opts, false)?;
    print_changes(&changes, dry_run);

    // Create the database up front, so the first agent call doesn't pay for
    // migrations while a user waits.
    if !dry_run {
        db::open(&cfg.db_path())?;
        println!("\ndatabase  {}", cfg.db_path().display());
    }

    if want_model && !dry_run {
        use std::io::Write;
        print!("model     downloading {} … ", cfg.model);
        std::io::stdout().flush().ok();
        match fuckmemory::embed::prefetch(cfg) {
            Ok(dim) => println!("ready ({dim} dims)"),
            Err(e) => {
                println!("failed");
                eprintln!(
                    "  {e:#}\n  Recall still works on keyword search. Retry with `fuckmemory model pull`."
                );
            }
        }
        // Build the mmap cache now, so no agent call ever pays the 200 ms model
        // load — least of all an autosave hook, which runs on every prompt.
        if cfg.fast && fuckmemory::embed::is_installed(cfg) {
            print!("cache     building … ");
            std::io::stdout().flush().ok();
            match fuckmemory::embed::build_cache(cfg, false) {
                Ok(rows) => println!("ready ({rows} tokens, ~1 ms cold start)"),
                Err(e) => println!("skipped — {e:#}"),
            }
        }
    }

    if !dry_run {
        if opts.hooks {
            println!("autosave  on — every prompt is stored, memories are injected back");
        } else {
            println!("autosave  off — turn it on with `fuckmemory tui` or `install --autosave`");
        }
        println!("\nRestart your agents so they pick up the new MCP server.");
    }
    Ok(())
}

fn cmd_uninstall(
    cfg: &Config,
    dry_run: bool,
    only: Option<Vec<String>>,
    project: bool,
) -> Result<()> {
    let home = home()?;
    let opts = install::Options {
        command: install::self_command(),
        global: true,
        project: if project {
            Some(scope::project_root(&cwd()))
        } else {
            None
        },
        only,
        instructions: true,
        hooks: true,
        dry_run,
    };
    let changes = install::apply(&home, &opts, true)?;
    print_changes(&changes, dry_run);
    if !dry_run {
        println!(
            "\nYour memories are untouched. To delete them too: rm -rf {}",
            cfg.home.display()
        );
    }
    Ok(())
}

fn print_changes(changes: &[install::Change], dry_run: bool) {
    let mut any = false;
    for c in changes {
        let (tag, snippet) = match &c.what {
            What::RegisterMcp => ("register", None),
            What::UnregisterMcp => ("unregister", None),
            What::WriteInstructions => ("instructions", None),
            What::RemoveInstructions => ("un-instruct", None),
            What::WriteHooks => ("autosave", None),
            What::RemoveHooks => ("un-autosave", None),
            What::AlreadyDone => ("ok", None),
            What::ManualSnippet(s) => ("manual", Some(s.clone())),
        };
        if c.what == What::AlreadyDone {
            println!("  · {:<14} {:<13} already correct", c.agent, tag);
            continue;
        }
        any = true;
        println!("  ✓ {:<14} {:<13} {}", c.agent, tag, c.path.display());
        if let Some(s) = snippet {
            println!("    config format unverified for this agent — add it yourself:");
            for line in s.lines() {
                println!("      {line}");
            }
        }
    }
    if !any {
        println!("  (nothing to change)");
    } else if dry_run {
        println!("\nDry run — nothing was written. Re-run without --dry-run to apply.");
    }
}

/// One line describing the state of the fast-path cache, for `doctor` and
/// `model which`.
fn describe_cache(cfg: &Config) -> String {
    let path = fuckmemory::fast::cache_path(cfg);
    if let Some(reason) = fuckmemory::embed::fast_disabled_reason(cfg) {
        return format!("disabled — {reason}");
    }
    if !cfg.fast {
        return "off (FUCKMEMORY_FAST=0 or fast = false)".into();
    }
    match std::fs::metadata(&path) {
        Ok(m) => format!(
            "{} ({:.0} MB)",
            path.display(),
            m.len() as f64 / 1_048_576.0
        ),
        Err(_) => "not built — run `fuckmemory model cache`".into(),
    }
}

/// Run one agent hook.
///
/// The contract with the agent is the important part: this must not fail in a
/// way the user sees. Every error goes to stderr and the exit code stays 0,
/// because a non-zero exit from a `UserPromptSubmit` hook blocks the prompt —
/// meaning a bug in a memory tool would stop somebody from working.
fn cmd_hook(
    cfg: &Config,
    event: String,
    agent: String,
    text: Option<String>,
    verbose: bool,
) -> Result<()> {
    use std::io::Read;

    let Some(ev) = fuckmemory::hook::Event::parse(&event) else {
        eprintln!("fuckmemory: unknown hook event {event:?}");
        return Ok(());
    };

    let mut raw = String::new();
    let mut meta = serde_json::json!({});
    let text = match text {
        Some(t) => Some(t),
        None => {
            std::io::stdin().read_to_string(&mut raw).ok();
            let (t, m) = fuckmemory::hook::prompt_from_agent(&raw, &agent);
            meta = m;
            t
        }
    };

    match fuckmemory::hook::run(cfg, ev, text, &meta, &agent) {
        Ok(out) => {
            // Antigravity's `Stop` requires a JSON `decision` back, and its
            // `PreInvocation` wants a valid `injectSteps` object — so it always
            // gets a reply, even when there is nothing to inject. The other
            // agents only read our output when we actually have context.
            let ctx = out.context.as_deref().unwrap_or("");
            if agent == "antigravity" {
                if let Some(v) = fuckmemory::hook::hook_output(&agent, ev, ctx) {
                    println!("{v}");
                }
            } else if let Some(v) = out
                .context
                .as_deref()
                .and_then(|c| fuckmemory::hook::hook_output(&agent, ev, c))
            {
                println!("{v}");
            }
            if verbose {
                eprintln!("fuckmemory: {out:?}");
            }
        }
        Err(e) => eprintln!("fuckmemory: hook failed, continuing anyway: {e:#}"),
    }
    Ok(())
}
fn cmd_update(check_only: bool, force: bool) -> Result<()> {
    let check = update::latest_release()?;
    if !check.update_available && !force {
        println!(
            "fuckmemory {} is up to date (latest {})",
            check.current, check.latest
        );
        return Ok(());
    }
    if check.update_available {
        println!(
            "fuckmemory {} → {} ({} is available)",
            check.current, check.latest, check.asset_name
        );
    } else if force {
        println!("already on {}; re-downloading anyway", check.current);
    }
    if check_only {
        println!("run `fuckmemory update` to apply");
        return Ok(());
    }

    let exe = std::env::current_exe().context("finding this binary")?;
    let applied = update::apply(&check, &exe)?;
    println!("updated to {} at {}", check.latest, applied.display());
    println!("restart your agents so they pick up the new binary.");
    Ok(())
}

fn cmd_agents() -> Result<()> {
    let home = home()?;
    println!("{:<14} {:<24} {:<9} MCP CONFIG", "ID", "AGENT", "DETECTED");
    for a in install::AGENTS {
        let path = a
            .global_mcp
            .first()
            .map(|p| format!("~/{p}"))
            .or_else(|| a.project_mcp.map(|p| format!("./{p}")))
            .unwrap_or_else(|| "(manual)".into());
        println!(
            "{:<14} {:<24} {:<9} {}",
            a.id,
            a.name,
            if install::detect(a, &home) {
                "yes"
            } else {
                "-"
            },
            path
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_remember(
    cfg: &Config,
    text: Vec<String>,
    kind: String,
    scope_spec: Option<String>,
    src: Option<String>,
    rel: Option<String>,
    dst: Option<String>,
    since: Option<String>,
    files: Vec<String>,
) -> Result<()> {
    let text = text.join(" ");
    anyhow::ensure!(!text.trim().is_empty(), "nothing to remember");
    let mut conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let emb = cached_embedder(cfg);

    let valid_from = match &since {
        Some(s) => {
            Some(pack::parse_when(s).ok_or_else(|| anyhow::anyhow!("--since must be YYYY-MM-DD"))?)
        }
        None => None,
    };
    let facts = if src.is_some() || rel.is_some() || dst.is_some() {
        vec![FactInput {
            src,
            rel: rel.unwrap_or_else(|| "relates_to".into()),
            dst,
            statement: text.clone(),
            valid_from,
            valid_to: None,
            confidence: 1.0,
            supersede: None,
        }]
    } else {
        vec![]
    };

    // Parse each `--file` (repeatable) as `path[:from-to]` and read it.
    let base = cwd();
    let mut file_inputs = Vec::new();
    for spec in &files {
        let (path, lines) = match spec.split_once(':') {
            Some((p, range)) => {
                let r = parse_line_range(range).ok_or_else(|| {
                    anyhow::anyhow!("--file range must look like PATH:FROM-TO, got {spec:?}")
                })?;
                (p, Some(r))
            }
            None => (spec.as_str(), None),
        };
        file_inputs.push(store::read_file_input(path, &base, lines)?);
    }

    let out = store::remember(
        &mut conn,
        &sc,
        emb.as_ref(),
        &RememberInput {
            text,
            kind,
            source: "cli".into(),
            facts,
            files: file_inputs,
            meta: None,
            derive: true,
        },
    )?;
    if out.duplicate {
        println!("already known (episode {})", out.episode_id);
    } else {
        println!("remembered in '{}' — fact ids {:?}", sc.label, out.fact_ids);
    }
    if !out.superseded.is_empty() {
        println!(
            "retired {} outdated fact(s): {:?}",
            out.superseded.len(),
            out.superseded
        );
    }
    if emb.is_none() && cfg.semantic {
        eprintln!(
            "note: no model cached, stored without an embedding. \
             Run `fuckmemory model pull` then `fuckmemory reindex`."
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_recall(
    cfg: &Config,
    query: Vec<String>,
    limit: usize,
    scope_spec: Option<String>,
    as_of: Option<String>,
    hops: usize,
    budget: Option<usize>,
    raw: bool,
    debug: bool,
    json: bool,
) -> Result<()> {
    let text = query.join(" ");
    anyhow::ensure!(!text.trim().is_empty(), "nothing to search for");
    let conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let scope_ids = scope::read_set(&conn, &sc)?;
    let emb = cached_embedder(cfg);

    let when = match &as_of {
        Some(s) => When::AsOf(
            pack::parse_when(s)
                .ok_or_else(|| anyhow::anyhow!("--as-of must be YYYY-MM-DD or epoch seconds"))?,
        ),
        None => When::Live,
    };

    let r = retrieve::recall(
        &conn,
        &scope_ids,
        emb.as_ref(),
        &Query {
            text,
            limit,
            when,
            hops: hops.min(2),
            include_episodes: raw,
        },
        None,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }

    let out = pack::render(
        &r,
        &PackOptions {
            budget_tokens: budget.unwrap_or(cfg.budget_tokens),
            scope_label: sc.label.clone(),
            debug,
        },
        now(),
    );
    if out.is_empty() {
        println!("nothing found");
    } else {
        print!("{out}");
        store::mark_hits(&conn, &pack::rendered_ids(&r, &out))?;
    }
    if debug {
        eprintln!(
            "\n{} hit(s) in {:.2}ms, semantic={}",
            r.hits.len(),
            r.took_us as f64 / 1000.0,
            r.semantic
        );
        for h in &r.hits {
            eprintln!(
                "  #{:<5} {:.5} {:<14} {}",
                h.fact.id,
                h.score,
                h.via.join("+"),
                h.fact.statement
            );
        }
    }
    Ok(())
}

/// Print the raw output of each retriever, before fusion. Useful when a recall
/// surprises you, and the tool used to calibrate the relevance floor.
fn cmd_explain(cfg: &Config, query: Vec<String>, scope_spec: Option<String>) -> Result<()> {
    let text = query.join(" ");
    anyhow::ensure!(!text.trim().is_empty(), "nothing to explain");
    let conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let scope_ids = scope::read_set(&conn, &sc)?;

    match retrieve::fts_query(&text) {
        Some(m) => println!("FTS MATCH   {m}"),
        None => println!("FTS MATCH   (none — query is all stopwords or punctuation)"),
    }

    match cached_embedder(cfg) {
        Some(e) => {
            let scored = retrieve::explain_vectors(&conn, &scope_ids, &e, &text, None)?;
            println!("\ncosine  kept  fact");
            let top = scored.first().map(|(_, s)| *s).unwrap_or(0.0);
            let rows =
                graph::fact_rows(&conn, &scored.iter().map(|(i, _)| *i).collect::<Vec<_>>())?;
            for ((id, cos), f) in scored.iter().zip(rows.iter()) {
                let kept = retrieve::passes_vector_floor(*cos, top);
                println!(
                    "{:.4}  {:<4}  #{:<4} {}",
                    cos,
                    if kept { "yes" } else { "no" },
                    id,
                    f.statement
                );
            }
            println!(
                "\nfloor       {:.4}  (max of {:.2} absolute and {:.0}% of the top hit)",
                retrieve::vector_floor(top),
                retrieve::VEC_FLOOR_ABS,
                retrieve::VEC_FLOOR_REL * 100.0
            );
        }
        None => println!("\n(no embedding model cached — vector leg disabled)"),
    }
    Ok(())
}

fn cmd_forget(
    cfg: &Config,
    id: Option<i64>,
    query: Option<String>,
    hard: bool,
    scope_spec: Option<String>,
) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let id = match (id, query) {
        (Some(id), _) => id,
        (None, Some(q)) => {
            let scope_ids = scope::read_set(&conn, &sc)?;
            let emb = cached_embedder(cfg);
            let r = retrieve::recall(
                &conn,
                &scope_ids,
                emb.as_ref(),
                &Query {
                    text: q.clone(),
                    limit: 1,
                    ..Default::default()
                },
                None,
            )?;
            let hit = r
                .hits
                .first()
                .ok_or_else(|| anyhow::anyhow!("nothing matched {q:?}"))?;
            println!("matched #{}: {}", hit.fact.id, hit.fact.statement);
            hit.fact.id
        }
        (None, None) => anyhow::bail!("pass a fact id or --query"),
    };
    let ok = if hard {
        store::purge(&conn, &sc, id)?
    } else {
        store::invalidate(&conn, &sc, id)?
    };
    println!(
        "{}",
        match (ok, hard) {
            (true, true) => format!("deleted #{id}"),
            (true, false) => format!("retracted #{id} (still visible with --as-of)"),
            (false, _) => format!("#{id} not found in '{}', or already retracted", sc.label),
        }
    );
    Ok(())
}

fn cmd_timeline(
    cfg: &Config,
    entity: String,
    limit: usize,
    scope_spec: Option<String>,
) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let scope_ids = scope::read_set(&conn, &sc)?;
    let rows = graph::timeline(&conn, &scope_ids, &entity, limit)?;
    if rows.is_empty() {
        println!("nothing recorded about {entity:?}");
        return Ok(());
    }
    for f in rows {
        let start = pack::ymd(f.valid_from.unwrap_or(f.recorded_at));
        let end = match (f.invalidated_at, f.valid_to) {
            (Some(_), Some(t)) => pack::ymd(t),
            (Some(t), None) => pack::ymd(t),
            _ => "now".into(),
        };
        println!("{start} → {:<10} #{:<5} {}", end, f.id, f.statement);
    }
    Ok(())
}

fn cmd_stats(cfg: &Config, json: bool) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    let s = store::stats(&conn)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!("scopes          {}", s.scopes);
    println!("episodes        {}", s.episodes);
    println!("facts live      {}", s.facts_live);
    println!("facts retired   {}", s.facts_invalid);
    println!("entities        {}", s.entities);
    println!("vectors         {}", s.vectors);
    println!("unconsolidated  {}", s.pending);
    println!("db size         {:.1} KiB", s.db_bytes as f64 / 1024.0);
    Ok(())
}

fn cmd_scopes(cfg: &Config) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    println!("{:<24} {:<7} ROOT", "LABEL", "FACTS");
    for (sc, n, root) in scope::list(&conn)? {
        println!(
            "{:<24} {:<7} {}",
            sc.label,
            n,
            root.unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

fn cmd_doctor(cfg: &Config, fix: bool) -> Result<()> {
    // Collect what needs fixing first, so `--fix` can act on all of it.
    let mut problems: Vec<String> = Vec::new();

    println!("home      {}", cfg.home.display());
    println!("database  {}", cfg.db_path().display());
    println!(
        "settings  {}{}",
        cfg.config_path().display(),
        if cfg.config_path().exists() {
            ""
        } else {
            "  (defaults; none written yet)"
        }
    );
    println!(
        "autosave  {}{}",
        if cfg.autosave { "on" } else { "off" },
        if cfg.autosave {
            format!(
                ", facts {}, scope {}, min {} chars",
                if cfg.autosave_facts { "on" } else { "off" },
                cfg.autosave_scope,
                cfg.autosave_min_chars
            )
        } else {
            String::new()
        }
    );
    println!(
        "autorecall {}",
        if cfg.autorecall {
            format!(
                "on — up to {} memories, {} tokens",
                cfg.autorecall_limit, cfg.autorecall_budget
            )
        } else {
            "off".to_string()
        }
    );
    println!("cache     {}", describe_cache(cfg));
    let conn = db::open(&cfg.db_path())?;
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    println!("schema    v{v} (expected v{})", db::SCHEMA_VERSION);

    let fts = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'facts_fts'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if fts {
        println!("fts5      ok");
    } else {
        println!("fts5      MISSING");
        problems.push("fts index missing — rebuild it".into());
    }
    println!(
        "semantic  {}",
        if cfg.semantic {
            "enabled"
        } else {
            "disabled (FUCKMEMORY_SEMANTIC=0)"
        }
    );
    println!(
        "model     {} — {}",
        cfg.model,
        fuckmemory::embed::describe_model_location(cfg)
    );
    if cfg.semantic {
        match Embedder::load_if_cached(cfg) {
            Some(e) => {
                let ok = fuckmemory::embed::model_matches(&conn, cfg, e.dim)?;
                println!(
                    "dims      {}{}",
                    e.dim,
                    if ok {
                        ""
                    } else {
                        problems.push("embeddings mismatch the model — re-embed".into());
                        "  ← MISMATCH with stored vectors, run `fuckmemory reindex`"
                    }
                );
            }
            None => {
                problems.push("embedding model not cached — pull and build the cache".into());
                println!("dims      n/a — run `fuckmemory model pull`")
            }
        }
    }

    let s = store::stats(&conn)?;
    println!(
        "content   {} facts live, {} retired, {} entities, {} pending",
        s.facts_live, s.facts_invalid, s.entities, s.pending
    );
    if s.facts_live > 0 && s.vectors == 0 && cfg.semantic {
        problems.push("facts have no embeddings — re-embed".into());
        println!("          ← facts have no embeddings; run `fuckmemory reindex`");
    }
    if s.pending > 0 {
        problems.push(format!("{} episode(s) awaiting consolidation", s.pending));
    }

    println!("\nagents");
    let home_dir = home()?;
    let mut any = false;
    for a in install::AGENTS {
        if !install::detect(a, &home_dir) {
            continue;
        }
        any = true;
        let wired = a
            .global_mcp
            .iter()
            .map(|p| home_dir.join(p))
            .find(|p| p.exists())
            .map(|p| {
                std::fs::read_to_string(&p)
                    .map(|t| t.contains(install::SERVER_NAME))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        // Read the agent's own settings rather than ours: autosave being "on"
        // here means nothing if no hook was ever written into the agent.
        let hooked = a
            .hooks
            .map(|(rel, _)| home_dir.join(rel))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.contains(" hook prompt --agent "))
            .unwrap_or(false);
        println!(
            "  {:<14} {:<48} autosave {}",
            a.id,
            if wired {
                "registered"
            } else {
                "detected, not registered — run `fuckmemory install`"
            },
            match (a.hooks.is_some(), hooked, cfg.autosave || cfg.autorecall) {
                (false, _, _) => "n/a (agent has no hook support)".to_string(),
                (true, true, true) => "wired".to_string(),
                (true, true, false) => "wired, but disabled in settings".to_string(),
                (true, false, true) => {
                    problems.push(format!("{} autosave on but hooks not wired", a.id));
                    "ENABLED BUT NOT WIRED — run `fuckmemory install`".to_string()
                }
                (true, false, false) => "off".to_string(),
            }
        );
    }
    if !any {
        println!("  (none detected)");
    }

    if !fix {
        return Ok(());
    }

    // --fix: repair everything the check flagged, then let the user re-check.
    if problems.is_empty() {
        println!("\nfix: nothing to do");
        return Ok(());
    }
    println!("\nfix: repairing {} issue(s)", problems.len());

    let mut conn = db::open(&cfg.db_path())?;
    let fts = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'facts_fts'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if !fts {
        println!("  · rebuilding the FTS index");
        db::rebuild_fts(&conn)?;
    }

    if cfg.semantic && Embedder::load_if_cached(cfg).is_none() {
        println!("  · fetching the embedding model");
        fuckmemory::embed::prefetch(cfg)?;
        println!("  · building the fast cache");
        fuckmemory::embed::build_cache(cfg, false)?;
    }

    let s = store::stats(&conn)?;
    let reembed = if let Some(e) = Embedder::load_if_cached(cfg) {
        !fuckmemory::embed::model_matches(&conn, cfg, e.dim)?
    } else {
        false
    } || (s.facts_live > 0 && s.vectors == 0 && cfg.semantic);
    if reembed {
        println!("  · re-embedding facts");
        let emb = Embedder::load(cfg)?;
        consolidate::reindex(&mut conn, &emb)?;
    }

    if s.pending > 0 {
        println!("  · consolidating {} episode(s)", s.pending);
        let emb = cached_embedder(cfg);
        let r = consolidate::run(&mut conn, cfg, emb.as_ref(), s.pending as usize)?;
        println!("    merged {} duplicate fact(s)", r.facts_merged);
    }

    // Wire any detected agent whose hooks are enabled but missing. Runs the
    // same idempotent path as `install`, restricted to what's broken.
    let home_dir = home()?;
    let broken: Vec<String> = install::AGENTS
        .iter()
        .filter(|a| install::detect(a, &home_dir))
        .filter(|a| {
            a.hooks.is_some() && (cfg.autosave || cfg.autorecall) && {
                let hooked = a.hooks.map(|(rel, _)| home_dir.join(rel));
                hooked
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .map(|t| !t.contains(" hook prompt --agent "))
                    .unwrap_or(true)
            }
        })
        .map(|a| a.id.to_string())
        .collect();
    if !broken.is_empty() {
        println!("  · wiring hooks into: {}", broken.join(", "));
        install::apply(
            &home_dir,
            &install::Options {
                command: std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "fuckmemory".into()),
                global: true,
                project: None,
                only: Some(broken),
                instructions: false,
                hooks: true,
                dry_run: false,
            },
            false,
        )?;
    }

    println!("\nrun `fuckmemory doctor` again to confirm everything is green.");
    Ok(())
}

fn cmd_consolidate(cfg: &Config, limit: usize) -> Result<()> {
    let mut conn = db::open(&cfg.db_path())?;
    let emb = cached_embedder(cfg);
    let r = consolidate::run(&mut conn, cfg, emb.as_ref(), limit)?;
    println!(
        "processed {} episode(s), merged {} duplicate fact(s), indexes {}",
        r.episodes_processed,
        r.facts_merged,
        if r.fts_optimized {
            "compacted"
        } else {
            "unchanged"
        }
    );
    if emb.is_none() && cfg.semantic {
        eprintln!("note: semantic dedup skipped — no model cached");
    }
    Ok(())
}

fn cmd_import(cfg: &Config, file: PathBuf, scope_spec: Option<String>) -> Result<()> {
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let doc: serde_json::Value = serde_json::from_str(&text)?;
    let facts = doc
        .get("facts")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("no `facts` array — is this a fuckmemory export?"))?;

    let conn = db::open(&cfg.db_path())?;
    let sc = scope::resolve(&conn, scope_spec.as_deref(), &cwd())?;
    let emb = cached_embedder(cfg);

    let ts = now();
    let mut added = 0usize;
    for f in facts {
        // Retracted facts are skipped on import: bringing them back live would
        // resurrect beliefs the source had already abandoned.
        if !f.get("invalidated_at").map(|v| v.is_null()).unwrap_or(true) {
            continue;
        }
        let Some(statement) = f.get("statement").and_then(|v| v.as_str()) else {
            continue;
        };
        let input = FactInput {
            src: f.get("src").and_then(|v| v.as_str()).map(str::to_string),
            rel: f
                .get("rel")
                .and_then(|v| v.as_str())
                .unwrap_or("relates_to")
                .to_string(),
            dst: f.get("dst").and_then(|v| v.as_str()).map(str::to_string),
            statement: statement.to_string(),
            valid_from: f.get("valid_from").and_then(|v| v.as_i64()),
            valid_to: None,
            confidence: f.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            supersede: Some(false),
        };
        let (id, _) = store::insert_fact(&conn, &sc, emb.as_ref(), &input, None, ts)?;
        if id.is_some() {
            added += 1;
        }
    }
    println!("imported {added} fact(s) into '{}'", sc.label);
    Ok(())
}
