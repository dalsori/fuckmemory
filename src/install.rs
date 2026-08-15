//! Wiring this into every agent on the machine.
//!
//! Two things have to happen for shared memory to actually get used:
//!
//! 1. Register the MCP server in each agent's config. Every agent invented its
//!    own file and its own JSON shape, so there is a table of them below.
//! 2. Tell the agent *when* to call the tools, by writing a marked block into
//!    the instruction file it reads (`AGENTS.md`, or `CLAUDE.md` for Claude Code).
//!    Without this, the tools exist and nothing ever calls them.
//!
//! Everything here is idempotent, backs up before it writes, and can be undone.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::now;

pub const SERVER_NAME: &str = "fuckmemory";
const BEGIN: &str = "<!-- fuckmemory:begin -->";
const END: &str = "<!-- fuckmemory:end -->";

/// The block written into agent instruction files.
///
/// Deliberately short: it is prepended to every single session, so every line
/// costs tokens forever. It only answers "when do I call these tools".
pub const INSTRUCTIONS: &str = r#"## Persistent memory

You share a persistent memory with every other agent on this machine, through the
`fuckmemory` MCP server. It survives across sessions, tools, and repos.

- **Before** assuming a convention, a command, a past decision, or a user
  preference: call `recall` with what you are trying to find out. Do this at the
  start of a task, not after guessing wrong.
- **After** learning something that will still matter in a future session — a
  decision and the reason for it, a preference, a command that actually works, a
  constraint, a gotcha that cost you time — call `remember`. Pass `facts` with
  subject/relation/object when you can.
- Never store secrets, credentials, transient state, or anything that can be
  re-read from the code.
- If something you recalled turns out to be wrong or stale, call `forget` on it.
  Leaving a wrong memory in place is worse than having no memory.
- To ask what changed over time ("what did we use before?"), call `timeline`."#;

/// Shape of the MCP registration each agent expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `{"mcpServers": {name: {command, args}}}` — the de-facto standard.
    McpServers,
    /// `{"servers": {name: {type: "stdio", command, args}}}` — VS Code.
    VsCodeServers,
    /// `{"mcp": {name: {type: "local", command: [...], enabled: true}}}` — OpenCode.
    OpenCode,
    /// `[mcp_servers.name]` in TOML — Codex.
    CodexToml,
    /// Detected, but its MCP format isn't verified. We print a snippet instead of
    /// writing a guess into a real config file.
    Manual,
}

/// Where an agent keeps its event hooks, when we know the format well enough to
/// write it. Autosave needs a hook per prompt, and guessing at an unverified
/// schema would silently do nothing (or worse, break the agent's startup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFormat {
    /// Anthropic's three-level shape: `hooks.<Event>[].hooks[] = {type, command}`.
    /// Claude Code and Codex use `timeout` in **seconds**; Qwen Code in
    /// **milliseconds**. All three are the same JSON structure, so one patcher
    /// serves them; only the timeout unit differs.
    Anthropic,
    /// Qwen Code's JSONC settings file, same shape as Anthropic, timeout in ms.
    QwenSettings,
    /// Gemini CLI's settings.json: same three-level shape as Anthropic, timeout
    /// in ms, and the `hooksConfig.enabled` toggle that gates the whole system.
    GeminiSettings,
    /// Cursor's `hooks.json`: a flat `{version, hooks: {<event>: [{command}]}}`
    /// map — one array per event, no group wrapper, no `type` field. The command
    /// is a shell string.
    Cursor,
    /// GitHub Copilot CLI's `hooks/*.json`: flat, events in camelCase, and each
    /// entry carries `bash`/`powershell` keys instead of `command`.
    Copilot,
    /// Antigravity's `hooks.json`: a *named map* — the top level maps our hook
    /// name to per-event arrays, `{ "<name>": { "<Event>": [{type, command}] } }`.
    /// It lives at `~/.gemini/config/hooks.json` (global) or `.agents/hooks.json`
    /// (workspace). Its prompt event is `PreInvocation`; the loop end is `Stop`.
    /// Timeout is in seconds.
    Antigravity,
    /// OpenCode: there is no settings-file hook channel. Instead OpenCode loads
    /// TS/JS plugins from `~/.config/opencode/plugins/` (global) or
    /// `.opencode/plugins/` (project), subscribing to events. The plugin here
    /// runs `fuckmemory hook prompt` on every user message and injects the
    /// recalled context back through `experimental.chat.system.transform`.
    OpenCodePlugin,
}

pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    /// Binaries that prove this agent is installed.
    pub bins: &'static [&'static str],
    /// `$HOME`-relative directories that also prove it.
    pub dirs: &'static [&'static str],
    /// `$HOME`-relative MCP config candidates, most-preferred first. An existing
    /// file always wins over creating a new one (OpenCode users often have
    /// `opencode.jsonc`, not `opencode.json`).
    pub global_mcp: &'static [&'static str],
    /// Project-relative MCP config.
    pub project_mcp: Option<&'static str>,
    pub format: Format,
    /// Project-relative instruction file.
    pub project_instructions: &'static str,
    /// `$HOME`-relative instruction file.
    pub global_instructions: Option<&'static str>,
    /// `$HOME`-relative settings file that holds event hooks, and its format.
    pub hooks: Option<(&'static str, HookFormat)>,
    /// Project-relative equivalent.
    pub project_hooks: Option<&'static str>,
}

/// The registry. Paths and shapes verified against each tool's documentation.
pub const AGENTS: &[Agent] = &[
    Agent {
        id: "claude-code",
        name: "Claude Code",
        bins: &["claude"],
        dirs: &[".claude"],
        global_mcp: &[".claude.json"],
        project_mcp: Some(".mcp.json"),
        format: Format::McpServers,
        // Claude Code reads CLAUDE.md natively and AGENTS.md only via import.
        project_instructions: "CLAUDE.md",
        global_instructions: Some(".claude/CLAUDE.md"),
        hooks: Some((".claude/settings.json", HookFormat::Anthropic)),
        project_hooks: Some(".claude/settings.json"),
    },
    Agent {
        id: "codex",
        name: "OpenAI Codex CLI",
        bins: &["codex"],
        dirs: &[".codex"],
        global_mcp: &[".codex/config.toml"],
        project_mcp: None,
        format: Format::CodexToml,
        project_instructions: "AGENTS.md",
        global_instructions: Some(".codex/AGENTS.md"),
        // Codex reads hooks from `~/.codex/hooks.json` (or inline in
        // config.toml); the JSON file keeps us away from the TOML parser.
        hooks: Some((".codex/hooks.json", HookFormat::Anthropic)),
        project_hooks: Some(".codex/hooks.json"),
    },
    Agent {
        id: "gemini-cli",
        name: "Gemini CLI",
        bins: &["gemini"],
        dirs: &[".gemini"],
        global_mcp: &[".gemini/settings.json"],
        project_mcp: Some(".gemini/settings.json"),
        format: Format::McpServers,
        project_instructions: "AGENTS.md",
        global_instructions: Some(".gemini/GEMINI.md"),
        hooks: Some((".gemini/settings.json", HookFormat::GeminiSettings)),
        project_hooks: Some(".gemini/settings.json"),
    },
    Agent {
        id: "antigravity",
        name: "Antigravity CLI",
        bins: &["agy"],
        dirs: &[".antigravity", ".gemini/config"],
        global_mcp: &[".gemini/config/mcp_config.json"],
        project_mcp: Some(".agents/mcp_config.json"),
        format: Format::McpServers,
        project_instructions: "AGENTS.md",
        global_instructions: None,
        // Antigravity reads hooks from `~/.gemini/config/hooks.json` (global) or
        // `.agents/hooks.json` (workspace), in a named-map shape with
        // PreInvocation/Stop events and a seconds timeout.
        hooks: Some((".gemini/config/hooks.json", HookFormat::Antigravity)),
        project_hooks: Some(".agents/hooks.json"),
    },
    Agent {
        id: "opencode",
        name: "OpenCode",
        bins: &["opencode"],
        dirs: &[".config/opencode", ".opencode"],
        global_mcp: &[
            ".config/opencode/opencode.jsonc",
            ".config/opencode/opencode.json",
        ],
        project_mcp: Some("opencode.json"),
        format: Format::OpenCode,
        project_instructions: "AGENTS.md",
        global_instructions: Some(".config/opencode/AGENTS.md"),
        hooks: Some((
            ".config/opencode/plugins/fuckmemory.js",
            HookFormat::OpenCodePlugin,
        )),
        project_hooks: Some(".opencode/plugins/fuckmemory.js"),
    },
    Agent {
        id: "qwen",
        name: "Qwen Code",
        bins: &["qwen"],
        dirs: &[".qwen"],
        global_mcp: &[".qwen/settings.json"],
        project_mcp: Some(".qwen/settings.json"),
        format: Format::McpServers,
        project_instructions: "AGENTS.md",
        global_instructions: Some(".qwen/QWEN.md"),
        hooks: Some((".qwen/settings.json", HookFormat::QwenSettings)),
        project_hooks: Some(".qwen/settings.json"),
    },
    Agent {
        id: "cursor",
        name: "Cursor",
        bins: &["cursor-agent", "cursor"],
        dirs: &[".cursor"],
        global_mcp: &[".cursor/mcp.json"],
        project_mcp: Some(".cursor/mcp.json"),
        format: Format::McpServers,
        project_instructions: "AGENTS.md",
        global_instructions: None,
        hooks: Some((".cursor/hooks.json", HookFormat::Cursor)),
        project_hooks: Some(".cursor/hooks.json"),
    },
    Agent {
        id: "copilot-cli",
        name: "GitHub Copilot CLI",
        bins: &["copilot"],
        dirs: &[".copilot", ".config/github-copilot"],
        global_mcp: &[".copilot/mcp-config.json"],
        project_mcp: Some(".mcp.json"),
        format: Format::McpServers,
        project_instructions: "AGENTS.md",
        global_instructions: None,
        hooks: Some((".copilot/hooks/fuckmemory.json", HookFormat::Copilot)),
        project_hooks: Some(".github/hooks/fuckmemory.json"),
    },
    Agent {
        id: "vscode",
        name: "VS Code (Copilot Chat)",
        bins: &["code", "code-insiders"],
        dirs: &[".vscode", ".config/Code"],
        // The user-profile mcp.json path is platform- and profile-dependent, so
        // only the workspace file is written.
        global_mcp: &[],
        project_mcp: Some(".vscode/mcp.json"),
        format: Format::VsCodeServers,
        project_instructions: "AGENTS.md",
        global_instructions: None,
        hooks: None,
        project_hooks: None,
    },
    Agent {
        id: "kimi-code",
        name: "Kimi Code",
        bins: &["kimi"],
        dirs: &[".kimi-code"],
        global_mcp: &[],
        project_mcp: None,
        format: Format::Manual,
        project_instructions: "AGENTS.md",
        global_instructions: None,
        hooks: None,
        project_hooks: None,
    },
];

pub fn agent_by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|p| {
        let c = p.join(bin);
        c.is_file() || c.with_extension("exe").is_file() || c.with_extension("cmd").is_file()
    })
}

/// Is this agent installed? Either its CLI is reachable or it left a config dir.
pub fn detect(agent: &Agent, home: &Path) -> bool {
    agent.bins.iter().any(|b| on_path(b)) || agent.dirs.iter().any(|d| home.join(d).exists())
}

pub fn detected(home: &Path) -> Vec<&'static Agent> {
    AGENTS.iter().filter(|a| detect(a, home)).collect()
}

/// One file modification, so `--dry-run` can describe the whole plan up front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub agent: &'static str,
    pub path: PathBuf,
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    RegisterMcp,
    UnregisterMcp,
    WriteInstructions,
    RemoveInstructions,
    /// Autosave hooks written into an agent's settings.
    WriteHooks,
    RemoveHooks,
    /// Nothing to do — already in the desired state.
    AlreadyDone,
    /// Format unverified; the snippet is printed for the user to paste.
    ManualSnippet(String),
}

pub struct Options {
    /// Absolute path to the binary agents should spawn.
    pub command: String,
    /// Patch user-level configs.
    pub global: bool,
    /// Patch the current project's configs.
    pub project: Option<PathBuf>,
    /// Restrict to these agent ids.
    pub only: Option<Vec<String>>,
    pub instructions: bool,
    /// Wire the autosave hooks. Independent of the MCP registration: an agent can
    /// have the tools without autosave, and vice versa.
    pub hooks: bool,
    pub dry_run: bool,
}

impl HookFormat {
    /// The `timeout` to write into each hook entry. Claude Code, Codex and
    /// Cursor measure it in seconds; Qwen Code, Gemini CLI in milliseconds;
    /// Copilot CLI in `timeoutSec`. Same intent either way (~10s): autosave must
    /// never hold a prompt, so if the store is locked by a long consolidation
    /// we'd rather drop one memory than stall the agent.
    fn timeout(&self) -> i64 {
        match self {
            HookFormat::Anthropic | HookFormat::Cursor | HookFormat::Copilot => 10,
            HookFormat::QwenSettings | HookFormat::GeminiSettings => 10_000,
            HookFormat::Antigravity => 10,
            HookFormat::OpenCodePlugin => 10,
        }
    }

    /// The event names this agent uses, paired with the CLI hook argument each
    /// one maps to.
    fn events(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            HookFormat::Anthropic | HookFormat::QwenSettings => EVENTS_PASCAL,
            HookFormat::GeminiSettings => HOOK_EVENTS_GEMINI,
            HookFormat::Cursor => HOOK_EVENTS_CURSOR,
            HookFormat::Copilot => HOOK_EVENTS_COPILOT,
            HookFormat::Antigravity => HOOK_EVENTS_ANTIGRAVITY,
            HookFormat::OpenCodePlugin => EVENTS_PASCAL,
        }
    }

    /// True when the settings file stores hooks in Anthropic's grouped shape
    /// (`hooks.<Event>[].hooks[]`); false for the flat `hooks.<Event>[]` maps
    /// Cursor and Copilot use.
    fn grouped(&self) -> bool {
        matches!(
            self,
            HookFormat::Anthropic | HookFormat::QwenSettings | HookFormat::GeminiSettings
        )
    }
}

impl Options {
    fn selected(&self, home: &Path) -> Vec<&'static Agent> {
        detected(home)
            .into_iter()
            .filter(|a| match &self.only {
                Some(ids) => ids.iter().any(|i| i == a.id),
                None => true,
            })
            .collect()
    }
}

/// Register (or, with `remove`, deregister) across every selected agent.
pub fn apply(home: &Path, opts: &Options, remove: bool) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for agent in opts.selected(home) {
        if agent.format == Format::Manual {
            if !remove {
                changes.push(Change {
                    agent: agent.id,
                    path: PathBuf::from("(manual)"),
                    what: What::ManualSnippet(snippet(agent, &opts.command)),
                });
            }
            continue;
        }

        if opts.global {
            if let Some(path) = pick_global(agent, home) {
                changes.push(patch_mcp(agent, &path, opts, remove)?);
            }
        }
        if let (Some(root), Some(rel)) = (&opts.project, agent.project_mcp) {
            changes.push(patch_mcp(agent, &root.join(rel), opts, remove)?);
        }
    }

    if opts.instructions {
        for path in instruction_targets(home, opts) {
            changes.push(patch_instructions(&path, opts.dry_run, remove)?);
        }
    }
    changes.extend(apply_hooks(home, opts, remove)?);
    Ok(changes)
}

/// The events autosave listens on, paired with the `fuckmemory hook` argument
/// each one maps to. Claude Code, Codex and Qwen all spell them in PascalCase.
const EVENTS_PASCAL: &[(&str, &str)] = &[
    ("UserPromptSubmit", "prompt"),
    ("SessionEnd", "session-end"),
];

/// Gemini CLI's equivalents. `BeforeAgent` fires after a prompt is submitted but
/// before planning, which is exactly where autosave and recall want to run.
const HOOK_EVENTS_GEMINI: &[(&str, &str)] =
    &[("BeforeAgent", "prompt"), ("SessionEnd", "session-end")];

/// Cursor spells its events in camelCase.
const HOOK_EVENTS_CURSOR: &[(&str, &str)] = &[
    ("beforeSubmitPrompt", "prompt"),
    ("sessionEnd", "session-end"),
];

/// Copilot CLI is camelCase too, and calls the prompt event `userPromptSubmitted`.
const HOOK_EVENTS_COPILOT: &[(&str, &str)] = &[
    ("userPromptSubmitted", "prompt"),
    ("sessionEnd", "session-end"),
];

/// Antigravity's lifecycle events. `PreInvocation` fires before each model call
/// (the first one is the user's freshly-submitted prompt); `Stop` fires when the
/// execution loop terminates — Antigravity has no `SessionEnd`.
const HOOK_EVENTS_ANTIGRAVITY: &[(&str, &str)] =
    &[("PreInvocation", "prompt"), ("Stop", "session-end")];

/// The events autosave listens on, paired with the `fuckmemory hook` argument
/// each one maps to.
pub const HOOK_EVENTS: &[(&str, &str)] = EVENTS_PASCAL;

/// Marks our entries in a settings file we share with the user's own hooks, so
/// removal never touches a hook somebody else wrote.
const HOOK_TAG: &str = "hook";

pub fn hook_command(command: &str, agent: &str, event_arg: &str) -> String {
    format!("{command} {HOOK_TAG} {event_arg} --agent {agent}")
}

/// Write (or remove) the autosave hooks for every selected agent that has a hook
/// format we actually know.
pub fn apply_hooks(home: &Path, opts: &Options, remove: bool) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    if !opts.hooks {
        return Ok(changes);
    }
    for agent in opts.selected(home) {
        let Some((rel, format)) = agent.hooks else {
            continue;
        };
        let mut targets: Vec<PathBuf> = Vec::new();
        if opts.global {
            targets.push(home.join(rel));
        }
        if let (Some(root), Some(prel)) = (&opts.project, agent.project_hooks) {
            targets.push(root.join(prel));
        }
        for path in targets {
            let what = patch_hooks(&path, &opts.command, agent.id, format, opts.dry_run, remove)?;
            changes.push(Change {
                agent: agent.id,
                path,
                what,
            });
        }
    }
    Ok(changes)
}

/// Dispatch a hook patch to the right patcher for the settings file's shape.
fn patch_hooks(
    path: &Path,
    command: &str,
    agent: &str,
    format: HookFormat,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    match format {
        HookFormat::Antigravity => patch_named(path, command, agent, format, dry_run, remove),
        HookFormat::OpenCodePlugin => patch_opencode_plugin(path, command, agent, dry_run, remove),
        _ if format.grouped() => patch_grouped(path, command, agent, format, dry_run, remove),
        _ => patch_flat(path, command, agent, format, dry_run, remove),
    }
}

/// Patch Anthropic's grouped shape, `hooks.<Event>[].hooks[]`, as used by Claude
/// Code `settings.json`, Codex `hooks.json`, Qwen and Gemini `settings.json`.
/// The shape is identical across all four; only the timeout unit differs, and
/// Gemini additionally gates the whole system behind `hooksConfig.enabled`.
///
/// The file belongs to the user and usually already has hooks in it, so this
/// edits surgically: our command is identified by the `fuckmemory hook` prefix,
/// entries that match are replaced or dropped, and every other hook — including
/// other tools' entries in the same event — is left exactly as found.
fn patch_grouped(
    path: &Path,
    command: &str,
    agent: &str,
    format: HookFormat,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    let (mut root, had_comments) = read_json(path)?;
    if !root.is_object() {
        anyhow::bail!(
            "{} has a non-object top level; refusing to touch it",
            path.display()
        );
    }
    let mut changed = false;

    // The `hooksConfig.enabled` toggle is off by default in some Gemini builds;
    // our hooks are the whole point of the install, so flip it on when wiring.
    if format == HookFormat::GeminiSettings && !remove {
        let on = root
            .get("hooksConfig")
            .and_then(|c| c.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !on {
            if !root
                .get("hooksConfig")
                .map(Value::is_object)
                .unwrap_or(false)
            {
                root["hooksConfig"] = json!({});
            }
            root["hooksConfig"]["enabled"] = json!(true);
            changed = true;
        }
    }

    for (event, arg) in format.events() {
        // Identify our entry by the argument shape rather than by the binary
        // name: people rename or relocate the binary, and matching on the path
        // would then append a second copy on every install instead of updating
        // the one that is already there.
        let marker = format!(" {HOOK_TAG} {arg} --agent ");
        let is_ours = |entry: &Value| -> bool {
            entry
                .get("command")
                .and_then(Value::as_str)
                .map(|c| c.contains(&marker))
                .unwrap_or(false)
        };
        let desired = json!({
            "type": "command",
            "command": hook_command(command, agent, arg),
            // Autosave must never hold up a prompt. If the store is locked by a
            // long consolidation, we would rather drop one memory than stall the
            // agent, so the timeout is short and deliberate.
            "timeout": format.timeout()
        });

        if !root.get("hooks").map(Value::is_object).unwrap_or(false) {
            if remove {
                continue;
            }
            root["hooks"] = json!({});
        }
        if !root["hooks"]
            .get(*event)
            .map(Value::is_array)
            .unwrap_or(false)
        {
            if remove {
                continue;
            }
            root["hooks"][*event] = json!([]);
        }
        let groups = root["hooks"][*event].as_array_mut().unwrap();

        // Find the group that already holds one of ours, if any.
        let mut found = false;
        for group in groups.iter_mut() {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = list.len();
            if remove {
                list.retain(|e| !is_ours(e));
                changed |= list.len() != before;
            } else if let Some(slot) = list.iter_mut().find(|e| is_ours(e)) {
                found = true;
                if *slot != desired {
                    *slot = desired.clone();
                    changed = true;
                }
            }
        }
        if remove {
            // Drop groups (and then the event) we emptied, so uninstall leaves no
            // dangling scaffolding behind.
            groups.retain(|g| {
                g.get("hooks")
                    .and_then(Value::as_array)
                    .map(|l| !l.is_empty())
                    .unwrap_or(true)
            });
            if groups.is_empty() {
                root["hooks"].as_object_mut().unwrap().remove(*event);
                changed = true;
            }
        } else if !found {
            groups.push(json!({ "hooks": [desired] }));
            changed = true;
        }
    }

    if remove
        && root
            .get("hooks")
            .and_then(Value::as_object)
            .map(|h| h.is_empty())
            .unwrap_or(false)
    {
        root.as_object_mut().unwrap().remove("hooks");
    }
    if !changed {
        return Ok(What::AlreadyDone);
    }
    if had_comments {
        eprintln!(
            "fuckmemory: {} contained comments; they are not preserved (backup kept alongside)",
            path.display()
        );
    }
    if !dry_run {
        backup(path, dry_run)?;
        write_atomic(path, &format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    }
    Ok(if remove {
        What::RemoveHooks
    } else {
        What::WriteHooks
    })
}

/// Patch the flat shape Cursor and Copilot CLI use, where each event maps
/// straight to an array of hook entries with no group wrapper.
///
/// Cursor (`hooks.json`) writes `{command, timeout}`; Copilot CLI
/// (`hooks/*.json`) writes `{type, bash, powershell, timeoutSec}` and spells
/// its events in camelCase. Both keep a `version` header that we fill in when
/// creating the file and leave alone otherwise.
fn patch_flat(
    path: &Path,
    command: &str,
    agent: &str,
    format: HookFormat,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    let (mut root, had_comments) = read_json(path)?;
    if !root.is_object() {
        anyhow::bail!(
            "{} has a non-object top level; refusing to touch it",
            path.display()
        );
    }
    let mut changed = false;

    if matches!(format, HookFormat::Copilot | HookFormat::Cursor)
        && !root
            .get("version")
            .and_then(Value::as_u64)
            .is_some_and(|v| v == 1)
    {
        if remove {
            return Ok(What::AlreadyDone);
        }
        root["version"] = json!(1);
        changed = true;
    }

    for (event, arg) in format.events() {
        let marker = format!(" {HOOK_TAG} {arg} --agent ");
        // Copilot keyed its entry field differently (`bash` vs `command`), but
        // the marker lives inside whichever one holds the command, so matching
        // is the same against every entry.
        let is_ours = |entry: &Value| -> bool {
            entry
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| entry.get("bash").and_then(Value::as_str))
                .map(|c| c.contains(&marker))
                .unwrap_or(false)
        };

        if !root.get("hooks").map(Value::is_object).unwrap_or(false) {
            if remove {
                continue;
            }
            root["hooks"] = json!({});
        }
        if !root["hooks"]
            .get(*event)
            .map(Value::is_array)
            .unwrap_or(false)
        {
            if remove {
                continue;
            }
            root["hooks"][*event] = json!([]);
        }
        let list = root["hooks"][*event].as_array_mut().unwrap();
        let cmd = hook_command(command, agent, arg);

        if remove {
            let before = list.len();
            list.retain(|e| !is_ours(e));
            changed |= list.len() != before;
            if list.is_empty() {
                root["hooks"].as_object_mut().unwrap().remove(*event);
            }
        } else if let Some(slot) = list.iter_mut().find(|e| is_ours(e)) {
            if format == HookFormat::Cursor {
                let desired = json!({ "command": cmd, "timeout": format.timeout() });
                if *slot != desired {
                    *slot = desired;
                    changed = true;
                }
            } else {
                let desired = json!({
                    "type": "command",
                    "bash": cmd,
                    "powershell": cmd,
                    "timeoutSec": format.timeout(),
                });
                if *slot != desired {
                    *slot = desired;
                    changed = true;
                }
            }
        } else if format == HookFormat::Cursor {
            list.push(json!({ "command": cmd, "timeout": format.timeout() }));
            changed = true;
        } else {
            list.push(json!({
                "type": "command",
                "bash": cmd,
                "powershell": cmd,
                "timeoutSec": format.timeout(),
            }));
            changed = true;
        }
    }

    if remove
        && root
            .get("hooks")
            .and_then(Value::as_object)
            .map(|h| h.is_empty())
            .unwrap_or(false)
    {
        root.as_object_mut().unwrap().remove("hooks");
    }
    if !changed {
        return Ok(What::AlreadyDone);
    }
    if had_comments {
        eprintln!(
            "fuckmemory: {} contained comments; they are not preserved (backup kept alongside)",
            path.display()
        );
    }
    if !dry_run {
        backup(path, dry_run)?;
        write_atomic(path, &format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    }
    Ok(if remove {
        What::RemoveHooks
    } else {
        What::WriteHooks
    })
}

/// Patch OpenCode's plugin file. OpenCode has no settings-file hook channel —
/// it loads TS/JS plugins from `plugins/`, which is what the "hooks" slot points
/// at here. The file is entirely ours (named `fuckmemory.js`), so wiring writes
/// it wholesale and removal deletes it; unlike the shared settings files there
/// is nothing of the user's inside to preserve.
///
/// The plugin subscribes to `chat.message` (fired when a user prompt lands),
/// runs `fuckmemory hook prompt` on it, and hands the recalled context back
/// through `experimental.chat.system.transform` — the closest OpenCode has to a
/// prompt hook. Autosave and autorecall therefore both work, and the plugin
/// catches its own failures so OpenCode never breaks on a memory error.
fn patch_opencode_plugin(
    path: &Path,
    command: &str,
    agent: &str,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    if remove {
        if !path.exists() {
            return Ok(What::AlreadyDone);
        }
        if !dry_run {
            std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
        return Ok(What::RemoveHooks);
    }

    if path.exists() {
        let existing =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let ours =
            existing.contains("FuckmemoryAutosave") && existing.contains("fuckmemory hook prompt");
        if ours {
            return Ok(What::AlreadyDone);
        }
        // A plugin file from something else lives here; back it up before
        // replacing it, like every other file install touches.
        backup(path, dry_run)?;
    }

    let contents = opencode_plugin_source(command, agent);
    if !dry_run {
        write_atomic(path, &contents)?;
    }
    Ok(What::WriteHooks)
}

/// The OpenCode plugin body, with the resolved `fuckmemory` command embedded.
fn opencode_plugin_source(command: &str, agent: &str) -> String {
    // Deliberately plain JS, no imports: OpenCode loads these with Bun and the
    // plugin must work even when the user has no package.json in the config dir.
    // Single quotes in the JS body: the outer template literal is a Rust raw
    // string delimited by `"#`, so double quotes in the JS would close it.
    format!(
        r#"{BEGIN}
// Autosave + autorecall for OpenCode, written by `fuckmemory install --autosave`.
// Removes cleanly with `fuckmemory uninstall`. The hook process is the single
// source of truth for what to store and what to inject.
export const FuckmemoryAutosave = async ({{ $, directory }}) => {{
  let pendingContext = '';

  // A user prompt has just landed. Run the same hook every other agent runs.
  'chat.message': async (input, output) => {{
    try {{
      const text = (output.parts || [])
        .filter((p) => p.type === 'text')
        .map((p) => p.text || '')
        .join('\n')
        .trim();
      if (!text) return;
      const res = await $`{command} hook prompt --agent {agent} --text ${{text}}`.nothrow().quiet();
      const body = res.stdout.toString();
      try {{
        const parsed = JSON.parse(body);
        const ctx = parsed?.hookSpecificOutput?.additionalContext;
        if (typeof ctx === 'string' && ctx.trim()) pendingContext = ctx.trim();
      }} catch {{ /* not JSON: nothing to inject */ }}
    }} catch {{ /* a memory error must never break the agent */ }}
  }},

  // Before the LLM call, hand the recalled memories back as extra system
  // context, then clear the slot so each prompt injects at most once.
  'experimental.chat.system.transform': async (input, output) => {{
    if (!pendingContext) return;
    output.system = output.system || [];
    output.system.push('# Memory\\n\\n' + pendingContext);
    pendingContext = '';
  }},
}};
{END}"#,
        BEGIN = "// fuckmemory:begin",
        END = "// fuckmemory:end"
    )
}

/// Patch Antigravity's named-map shape, `{ "<name>": { "<Event>": [handlers] } }`.
///
/// Antigravity maps a *hook name* (ours is `SERVER_NAME`) to per-event arrays of
/// `{type, command, timeout}` handlers, with no group wrapper and no matcher for
/// the lifecycle events we use (`PreInvocation`, `Stop`). The file belongs to the
/// user and may hold other tools' hooks too, so this edits surgically under our
/// own key — entries are matched by the `fuckmemory hook` command prefix, and
/// everything else is left exactly as found. Timeout is in seconds.
fn patch_named(
    path: &Path,
    command: &str,
    agent: &str,
    format: HookFormat,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    let (mut root, had_comments) = read_json(path)?;
    if !root.is_object() {
        anyhow::bail!(
            "{} has a non-object top level; refusing to touch it",
            path.display()
        );
    }
    let mut changed = false;

    if remove && root.get(SERVER_NAME).is_none() {
        return Ok(What::AlreadyDone);
    }

    for (event, arg) in format.events() {
        let marker = format!(" {HOOK_TAG} {arg} --agent ");
        let is_ours = |entry: &Value| -> bool {
            entry
                .get("command")
                .and_then(Value::as_str)
                .map(|c| c.contains(&marker))
                .unwrap_or(false)
        };
        let desired = json!({
            "type": "command",
            "command": hook_command(command, agent, arg),
            "timeout": format.timeout()
        });

        if !root.get(SERVER_NAME).map(Value::is_object).unwrap_or(false) {
            if remove {
                continue;
            }
            root[SERVER_NAME] = json!({});
        }
        let ours = root.get_mut(SERVER_NAME).unwrap().as_object_mut().unwrap();
        if !ours.get(*event).map(Value::is_array).unwrap_or(false) {
            if remove {
                continue;
            }
            ours.insert((*event).to_string(), json!([]));
        }
        let list = ours.get_mut(*event).unwrap().as_array_mut().unwrap();

        if remove {
            let before = list.len();
            list.retain(|e| !is_ours(e));
            changed |= list.len() != before;
            if list.is_empty() {
                ours.remove(*event);
            }
        } else if let Some(slot) = list.iter_mut().find(|e| is_ours(e)) {
            if *slot != desired {
                *slot = desired;
                changed = true;
            }
        } else {
            list.push(desired);
            changed = true;
        }
    }

    if remove
        && root
            .get(SERVER_NAME)
            .and_then(Value::as_object)
            .map(|o| o.is_empty())
            .unwrap_or(false)
    {
        root.as_object_mut().unwrap().remove(SERVER_NAME);
    }
    if !changed {
        return Ok(What::AlreadyDone);
    }
    if had_comments {
        eprintln!(
            "fuckmemory: {} contained comments; they are not preserved (backup kept alongside)",
            path.display()
        );
    }
    if !dry_run {
        backup(path, dry_run)?;
        write_atomic(path, &format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    }
    Ok(if remove {
        What::RemoveHooks
    } else {
        What::WriteHooks
    })
}

/// Prefer a config file that already exists; otherwise create the first candidate.
fn pick_global(agent: &Agent, home: &Path) -> Option<PathBuf> {
    let existing = agent
        .global_mcp
        .iter()
        .map(|p| home.join(p))
        .find(|p| p.exists());
    existing.or_else(|| agent.global_mcp.first().map(|p| home.join(p)))
}

/// Instruction files to write, deduplicated — several agents share `AGENTS.md`.
fn instruction_targets(home: &Path, opts: &Options) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for agent in opts.selected(home) {
        if opts.global {
            if let Some(rel) = agent.global_instructions {
                out.push(home.join(rel));
            }
        }
        if let Some(root) = &opts.project {
            out.push(root.join(agent.project_instructions));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The server entry, in the shape a given agent wants.
fn entry(format: Format, command: &str) -> Value {
    match format {
        Format::McpServers => json!({ "command": command, "args": ["serve"] }),
        Format::VsCodeServers => {
            json!({ "type": "stdio", "command": command, "args": ["serve"] })
        }
        Format::OpenCode => {
            json!({ "type": "local", "command": [command, "serve"], "enabled": true })
        }
        Format::CodexToml | Format::Manual => json!({ "command": command, "args": ["serve"] }),
    }
}

fn root_key(format: Format) -> &'static str {
    match format {
        Format::VsCodeServers => "servers",
        Format::OpenCode => "mcp",
        _ => "mcpServers",
    }
}

/// A copy-pasteable snippet, for agents we won't write to automatically.
pub fn snippet(agent: &Agent, command: &str) -> String {
    let v = json!({ root_key(agent.format): { SERVER_NAME: entry(agent.format, command) } });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

fn patch_mcp(agent: &Agent, path: &Path, opts: &Options, remove: bool) -> Result<Change> {
    let what = if agent.format == Format::CodexToml {
        patch_toml(path, &opts.command, opts.dry_run, remove)?
    } else {
        patch_json(path, agent.format, &opts.command, opts.dry_run, remove)?
    };
    Ok(Change {
        agent: agent.id,
        path: path.to_path_buf(),
        what,
    })
}

/// Read a JSON or JSONC file into a `Value`.
///
/// Returns `(value, had_comments)`. JSONC is accepted because real OpenCode and
/// VS Code configs contain comments; we cannot preserve them through a
/// serde round-trip, so the caller warns when any were dropped.
fn read_json(path: &Path) -> Result<(Value, bool)> {
    if !path.exists() {
        return Ok((json!({}), false));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok((json!({}), false));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => Ok((v, false)),
        Err(strict_err) => {
            let parsed = jsonc_parser::parse_to_serde_value(&text, &Default::default())
                .map_err(|e| anyhow::anyhow!("{} is not valid JSON or JSONC: {e}", path.display()))?
                .ok_or_else(|| anyhow::anyhow!("{} parsed to nothing", path.display()))?;
            let _ = strict_err;
            Ok((parsed, true))
        }
    }
}

fn backup(path: &Path, dry_run: bool) -> Result<()> {
    if dry_run || !path.exists() {
        return Ok(());
    }
    let bak = path.with_extension(format!(
        "{}.fuckmemory-{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("cfg"),
        now()
    ));
    std::fs::copy(path, &bak).with_context(|| format!("backing up to {}", bak.display()))?;
    Ok(())
}

/// Write via a temp file + rename, so a crash can't leave a half-written config.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("fuckmemory-tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn patch_json(
    path: &Path,
    format: Format,
    command: &str,
    dry_run: bool,
    remove: bool,
) -> Result<What> {
    let (mut root, had_comments) = read_json(path)?;
    if !root.is_object() {
        anyhow::bail!(
            "{} has a non-object top level; refusing to touch it",
            path.display()
        );
    }
    let key = root_key(format);
    let desired = entry(format, command);

    if remove {
        let existed = root.get(key).and_then(|m| m.get(SERVER_NAME)).is_some();
        if !existed {
            return Ok(What::AlreadyDone);
        }
        if let Some(map) = root.get_mut(key).and_then(Value::as_object_mut) {
            map.remove(SERVER_NAME);
        }
    } else {
        if root.get(key).and_then(|m| m.get(SERVER_NAME)) == Some(&desired) {
            return Ok(What::AlreadyDone);
        }
        if !root.get(key).map(Value::is_object).unwrap_or(false) {
            root[key] = json!({});
        }
        root[key][SERVER_NAME] = desired;
    }

    if had_comments {
        eprintln!(
            "fuckmemory: {} contained comments; they are not preserved (backup kept alongside)",
            path.display()
        );
    }
    if !dry_run {
        backup(path, dry_run)?;
        write_atomic(path, &format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    }
    Ok(if remove {
        What::UnregisterMcp
    } else {
        What::RegisterMcp
    })
}

fn patch_toml(path: &Path, command: &str, dry_run: bool, remove: bool) -> Result<What> {
    let text = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    // toml_edit keeps the user's comments and formatting intact, which matters
    // for a hand-maintained file like ~/.codex/config.toml.
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;

    let present = doc
        .get("mcp_servers")
        .and_then(|t| t.as_table_like())
        .map(|t| t.contains_key(SERVER_NAME))
        .unwrap_or(false);

    if remove {
        if !present {
            return Ok(What::AlreadyDone);
        }
        if let Some(t) = doc
            .get_mut("mcp_servers")
            .and_then(|t| t.as_table_like_mut())
        {
            t.remove(SERVER_NAME);
        }
    } else {
        let already = present
            && doc["mcp_servers"][SERVER_NAME]
                .get("command")
                .and_then(|v| v.as_str())
                == Some(command);
        if already {
            return Ok(What::AlreadyDone);
        }
        if doc.get("mcp_servers").is_none() {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            doc["mcp_servers"] = toml_edit::Item::Table(t);
        }
        let mut server = toml_edit::Table::new();
        server["command"] = toml_edit::value(command);
        let mut args = toml_edit::Array::new();
        args.push("serve");
        server["args"] = toml_edit::value(args);
        doc["mcp_servers"][SERVER_NAME] = toml_edit::Item::Table(server);
    }

    if !dry_run {
        backup(path, dry_run)?;
        write_atomic(path, &doc.to_string())?;
    }
    Ok(if remove {
        What::UnregisterMcp
    } else {
        What::RegisterMcp
    })
}

/// Replace (or insert, or drop) our marked block in a markdown instruction file,
/// leaving everything the user wrote untouched.
pub fn splice_block(existing: &str, block: Option<&str>) -> String {
    let body = block.map(|b| format!("{BEGIN}\n{b}\n{END}"));

    if let (Some(s), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        if s < e {
            let head = &existing[..s];
            let tail = &existing[e + END.len()..];
            return match body {
                Some(b) => format!("{head}{b}{tail}"),
                None => {
                    // Drop the block and the blank line it introduced.
                    let joined = format!("{}{}", head.trim_end(), tail);
                    let t = joined.trim();
                    if t.is_empty() {
                        String::new()
                    } else {
                        format!("{t}\n")
                    }
                }
            };
        }
    }

    match body {
        None => existing.to_string(),
        Some(b) => {
            if existing.trim().is_empty() {
                format!("{b}\n")
            } else {
                format!("{}\n\n{b}\n", existing.trim_end())
            }
        }
    }
}

fn patch_instructions(path: &Path, dry_run: bool, remove: bool) -> Result<Change> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let next = splice_block(&existing, if remove { None } else { Some(INSTRUCTIONS) });
    let what = if next == existing {
        What::AlreadyDone
    } else if remove {
        What::RemoveInstructions
    } else {
        What::WriteInstructions
    };
    if what != What::AlreadyDone && !dry_run {
        backup(path, dry_run)?;
        if next.is_empty() {
            std::fs::remove_file(path).ok();
        } else {
            write_atomic(path, &next)?;
        }
    }
    Ok(Change {
        agent: "instructions",
        path: path.to_path_buf(),
        what,
    })
}

/// Absolute path to the running binary, for embedding in configs. An absolute
/// path is used rather than the bare name so registration doesn't silently break
/// when an agent is launched with a different `PATH` (GUI apps often are).
pub fn self_command() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| SERVER_NAME.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fm-inst-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_agent_has_a_usable_registration_route() {
        for a in AGENTS {
            assert!(!a.id.is_empty() && !a.name.is_empty());
            let has_route =
                !a.global_mcp.is_empty() || a.project_mcp.is_some() || a.format == Format::Manual;
            assert!(has_route, "{} can never be registered", a.id);
        }
    }

    #[test]
    fn entry_shapes_match_each_tool() {
        assert_eq!(entry(Format::McpServers, "fm")["args"][0], "serve");
        assert_eq!(entry(Format::VsCodeServers, "fm")["type"], "stdio");
        // OpenCode takes a single command array, not command + args.
        let oc = entry(Format::OpenCode, "fm");
        assert_eq!(oc["type"], "local");
        assert_eq!(oc["command"][0], "fm");
        assert_eq!(oc["command"][1], "serve");
        assert_eq!(oc["enabled"], true);
    }

    #[test]
    fn json_patch_creates_registers_and_is_idempotent() {
        let d = tmpdir("json");
        let p = d.join("mcp.json");
        assert_eq!(
            patch_json(&p, Format::McpServers, "/bin/fm", false, false).unwrap(),
            What::RegisterMcp
        );
        let (v, _) = read_json(&p).unwrap();
        assert_eq!(v["mcpServers"]["fuckmemory"]["command"], "/bin/fm");

        assert_eq!(
            patch_json(&p, Format::McpServers, "/bin/fm", false, false).unwrap(),
            What::AlreadyDone,
            "second run must be a no-op"
        );
    }

    #[test]
    fn json_patch_preserves_other_servers_and_keys() {
        let d = tmpdir("keep");
        let p = d.join("mcp.json");
        std::fs::write(
            &p,
            r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        patch_json(&p, Format::McpServers, "/bin/fm", false, false).unwrap();
        let (v, _) = read_json(&p).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["fuckmemory"]["command"], "/bin/fm");
    }

    #[test]
    fn json_patch_reads_jsonc_with_comments() {
        let d = tmpdir("jsonc");
        let p = d.join("opencode.jsonc");
        std::fs::write(
            &p,
            "{\n  // my config\n  \"$schema\": \"https://opencode.ai/config.json\",\n}\n",
        )
        .unwrap();
        patch_json(&p, Format::OpenCode, "/bin/fm", false, false).unwrap();
        let (v, _) = read_json(&p).unwrap();
        assert_eq!(v["$schema"], "https://opencode.ai/config.json");
        assert_eq!(v["mcp"]["fuckmemory"]["type"], "local");
    }

    #[test]
    fn json_patch_refuses_a_non_object_config() {
        let d = tmpdir("bad");
        let p = d.join("mcp.json");
        std::fs::write(&p, "[1,2,3]").unwrap();
        assert!(patch_json(&p, Format::McpServers, "/bin/fm", false, false).is_err());
    }

    #[test]
    fn json_patch_makes_a_backup_before_writing() {
        let d = tmpdir("backup");
        let p = d.join("mcp.json");
        std::fs::write(&p, r#"{"keep":1}"#).unwrap();
        patch_json(&p, Format::McpServers, "/bin/fm", false, false).unwrap();
        let baks: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("fuckmemory-"))
            .collect();
        assert_eq!(baks.len(), 1, "expected exactly one backup");
    }

    #[test]
    fn dry_run_touches_nothing() {
        let d = tmpdir("dry");
        let p = d.join("mcp.json");
        assert_eq!(
            patch_json(&p, Format::McpServers, "/bin/fm", true, false).unwrap(),
            What::RegisterMcp
        );
        assert!(!p.exists(), "dry run must not create files");
    }

    #[test]
    fn unregister_removes_only_our_entry() {
        let d = tmpdir("unreg");
        let p = d.join("mcp.json");
        std::fs::write(&p, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        patch_json(&p, Format::McpServers, "/bin/fm", false, false).unwrap();
        assert_eq!(
            patch_json(&p, Format::McpServers, "/bin/fm", false, true).unwrap(),
            What::UnregisterMcp
        );
        let (v, _) = read_json(&p).unwrap();
        assert!(v["mcpServers"].get("fuckmemory").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(
            patch_json(&p, Format::McpServers, "/bin/fm", false, true).unwrap(),
            What::AlreadyDone
        );
    }

    #[test]
    fn toml_patch_keeps_comments_and_existing_tables() {
        let d = tmpdir("toml");
        let p = d.join("config.toml");
        std::fs::write(
            &p,
            "# my codex config\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        assert_eq!(
            patch_toml(&p, "/bin/fm", false, false).unwrap(),
            What::RegisterMcp
        );
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("# my codex config"), "comments lost:\n{out}");
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("[mcp_servers.other]"));
        assert!(out.contains("[mcp_servers.fuckmemory]"), "got:\n{out}");
        assert!(out.contains("args = [\"serve\"]"), "got:\n{out}");

        assert_eq!(
            patch_toml(&p, "/bin/fm", false, false).unwrap(),
            What::AlreadyDone
        );
        assert_eq!(
            patch_toml(&p, "/bin/fm", false, true).unwrap(),
            What::UnregisterMcp
        );
        assert!(!std::fs::read_to_string(&p).unwrap().contains("fuckmemory"));
    }

    #[test]
    fn toml_patch_rejects_broken_toml() {
        let d = tmpdir("badtoml");
        let p = d.join("config.toml");
        std::fs::write(&p, "this is [not toml").unwrap();
        assert!(patch_toml(&p, "/bin/fm", false, false).is_err());
    }

    #[test]
    fn splice_inserts_after_user_content() {
        let out = splice_block("# My project\n\nSome rules.\n", Some(INSTRUCTIONS));
        assert!(out.starts_with("# My project"));
        assert!(out.contains(BEGIN) && out.contains(END));
        assert!(out.contains("Persistent memory"));
    }

    #[test]
    fn splice_replaces_in_place_without_duplicating() {
        let first = splice_block("# Doc\n", Some(INSTRUCTIONS));
        let second = splice_block(&first, Some("NEW BODY"));
        assert_eq!(second.matches(BEGIN).count(), 1);
        assert!(second.contains("NEW BODY"));
        assert!(!second.contains("Persistent memory"));
        assert!(
            second.starts_with("# Doc"),
            "user content preserved: {second:?}"
        );
    }

    #[test]
    fn splice_removes_block_and_leaves_user_content() {
        let with = splice_block("# Doc\n\nrules\n", Some(INSTRUCTIONS));
        let without = splice_block(&with, None);
        assert!(!without.contains(BEGIN));
        assert!(without.contains("# Doc"));
        assert!(without.contains("rules"));
    }

    #[test]
    fn splice_removal_of_our_only_content_yields_empty() {
        let only = splice_block("", Some(INSTRUCTIONS));
        assert!(splice_block(&only, None).is_empty());
    }

    #[test]
    fn splice_ignores_reversed_markers() {
        // Malformed input must not corrupt the file; append instead.
        let weird = format!("{END} stray {BEGIN}");
        let out = splice_block(&weird, Some("BODY"));
        assert!(out.contains("stray"));
        assert!(out.contains("BODY"));
    }

    #[test]
    fn instruction_targets_dedupe_shared_agents_md() {
        let home = tmpdir("targets");
        let proj = tmpdir("targets-proj");
        // Make several AGENTS.md-reading agents look installed.
        for d in [".codex", ".gemini", ".qwen"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let opts = Options {
            command: "fm".into(),
            global: false,
            project: Some(proj.clone()),
            only: Some(vec!["codex".into(), "gemini-cli".into(), "qwen".into()]),
            instructions: true,
            hooks: false,
            dry_run: true,
        };
        let t = instruction_targets(&home, &opts);
        assert_eq!(t, vec![proj.join("AGENTS.md")], "got {t:?}");
    }

    #[test]
    fn apply_end_to_end_is_idempotent_and_reversible() {
        let home = tmpdir("e2e-home");
        let proj = tmpdir("e2e-proj");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();

        let opts = Options {
            command: "/bin/fm".into(),
            global: true,
            project: Some(proj.clone()),
            only: Some(vec!["codex".into(), "cursor".into()]),
            instructions: true,
            hooks: false,
            dry_run: false,
        };

        let first = apply(&home, &opts, false).unwrap();
        assert!(first.iter().any(|c| c.what == What::RegisterMcp));
        assert!(home.join(".codex/config.toml").exists());
        assert!(home.join(".cursor/mcp.json").exists());
        assert!(proj.join("AGENTS.md").exists());

        let second = apply(&home, &opts, false).unwrap();
        assert!(
            second.iter().all(|c| c.what == What::AlreadyDone),
            "re-install should change nothing: {second:?}"
        );

        let undo = apply(&home, &opts, true).unwrap();
        assert!(undo
            .iter()
            .any(|c| matches!(c.what, What::UnregisterMcp | What::RemoveInstructions)));
        let toml = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(!toml.contains("fuckmemory"));
    }

    #[test]
    fn detection_finds_agents_by_config_dir() {
        let home = tmpdir("detect");
        assert!(detected(&home).iter().all(|a| !a.bins.is_empty()));
        std::fs::create_dir_all(home.join(".qwen")).unwrap();
        assert!(detected(&home).iter().any(|a| a.id == "qwen"));
    }

    /// Options that wire hooks for Claude Code against a throwaway home.
    fn hook_opts(dry_run: bool) -> Options {
        Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["claude-code".into()]),
            instructions: false,
            hooks: true,
            dry_run,
        }
    }

    #[test]
    fn hooks_are_written_idempotently_and_removed_cleanly() {
        let home = tmpdir("hooks");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let path = home.join(".claude/settings.json");

        let first = apply_hooks(&home, &hook_opts(false), false).unwrap();
        assert!(first.iter().any(|c| c.what == What::WriteHooks));
        let text = std::fs::read_to_string(&path).unwrap();
        for (event, arg) in HOOK_EVENTS {
            assert!(text.contains(event), "missing {event}: {text}");
            assert!(
                text.contains(&format!("hook {arg} --agent claude-code")),
                "{text}"
            );
        }

        let again = apply_hooks(&home, &hook_opts(false), false).unwrap();
        assert!(
            again.iter().all(|c| c.what == What::AlreadyDone),
            "second run should be a no-op: {again:?}"
        );

        let undo = apply_hooks(&home, &hook_opts(false), true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            after.get("hooks").is_none(),
            "an emptied hooks block should be dropped: {after}"
        );
    }

    /// The settings file belongs to the user. Their hooks, and their other
    /// settings, must survive both the install and the uninstall untouched.
    #[test]
    fn other_peoples_hooks_are_never_disturbed() {
        let home = tmpdir("hooks-coexist");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let path = home.join(".claude/settings.json");
        std::fs::write(
            &path,
            r#"{
              "model": "opus",
              "hooks": {
                "UserPromptSubmit": [
                  {"hooks": [{"type": "command", "command": "their-logger --quiet"}]}
                ],
                "PreToolUse": [
                  {"matcher": "Bash", "hooks": [{"type": "command", "command": "their-guard"}]}
                ]
              }
            }"#,
        )
        .unwrap();

        apply_hooks(&home, &hook_opts(false), false).unwrap();
        let mid = std::fs::read_to_string(&path).unwrap();
        assert!(mid.contains("their-logger --quiet"));
        assert!(mid.contains("their-guard"));
        assert!(mid.contains("\"model\": \"opus\""));

        apply_hooks(&home, &hook_opts(false), true).unwrap();
        let end: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(end["model"], "opus");
        assert_eq!(
            end["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            "their-logger --quiet"
        );
        assert_eq!(
            end["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "their-guard"
        );
        let text = serde_json::to_string(&end).unwrap();
        assert!(
            !text.contains(SERVER_NAME),
            "our hook should be gone: {text}"
        );
    }

    #[test]
    fn a_changed_binary_path_updates_the_existing_hook_instead_of_duplicating_it() {
        let home = tmpdir("hooks-move");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        apply_hooks(&home, &hook_opts(false), false).unwrap();

        let mut moved = hook_opts(false);
        moved.command = "/usr/local/bin/fm".into();
        apply_hooks(&home, &moved, false).unwrap();

        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let groups = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "the old entry should have been rewritten");
        assert!(groups[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with("/usr/local/bin/fm"));
    }

    #[test]
    fn dry_run_hooks_write_nothing() {
        let home = tmpdir("hooks-dry");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let changes = apply_hooks(&home, &hook_opts(true), false).unwrap();
        assert!(changes.iter().any(|c| c.what == What::WriteHooks));
        assert!(!home.join(".claude/settings.json").exists());
    }

    #[test]
    fn agents_without_a_known_hook_format_are_left_alone() {
        let home = tmpdir("hooks-unknown");
        std::fs::create_dir_all(home.join(".vscode")).unwrap();
        let mut opts = hook_opts(false);
        opts.only = Some(vec!["vscode".into()]);
        assert!(apply_hooks(&home, &opts, false).unwrap().is_empty());
    }

    /// Codex and Qwen use the same Anthropic JSON shape but different files and
    /// timeouts (seconds vs milliseconds). Both must be written and removed
    /// exactly like Claude's, idempotently, without clobbering existing keys.
    #[test]
    fn codex_and_qwen_hooks_are_written_and_removed() {
        let home = tmpdir("hooks-codex-qwen");
        for sub in [".codex", ".qwen"] {
            std::fs::create_dir_all(home.join(sub)).unwrap();
        }

        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["codex".into(), "qwen".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        let first = apply_hooks(&home, &base, false).unwrap();
        assert!(first.iter().any(|c| c.what == What::WriteHooks));
        assert!(first.len() == 2, "codex and qwen wired: {first:?}");

        let codex: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert_eq!(
            codex["hooks"]["UserPromptSubmit"][0]["hooks"][0]["timeout"], 10,
            "codex measures timeout in seconds"
        );

        let qwen: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".qwen/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            qwen["hooks"]["UserPromptSubmit"][0]["hooks"][0]["timeout"], 10_000,
            "qwen measures timeout in milliseconds"
        );
        assert!(qwen["hooks"]["SessionEnd"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("--agent qwen"));

        let again = apply_hooks(&home, &base, false).unwrap();
        assert!(
            again.iter().all(|c| c.what == What::AlreadyDone),
            "second run should be a no-op: {again:?}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        assert!(
            !std::fs::read_to_string(home.join(".qwen/settings.json"))
                .unwrap()
                .contains("hooks"),
            "emptied qwen hooks block should be dropped"
        );
    }

    /// Gemini is Anthropic-shaped with a milliseconds timeout, CamelCase events
    /// of its own, and a `hooksConfig.enabled` toggle that must be on for the
    /// hooks to fire at all.
    #[test]
    fn gemini_hooks_turn_on_hooksconfig_and_use_beforeagent() {
        let home = tmpdir("hooks-gemini");
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["gemini-cli".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        apply_hooks(&home, &base, false).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            v["hooksConfig"]["enabled"], true,
            "hooksConfig.enabled must be on"
        );
        assert!(v["hooks"].get("BeforeAgent").is_some());
        assert!(v["hooks"].get("SessionEnd").is_some());
        assert!(v["hooks"].get("UserPromptSubmit").is_none());
        assert_eq!(
            v["hooks"]["BeforeAgent"][0]["hooks"][0]["timeout"], 10_000,
            "gemini measures timeout in milliseconds"
        );
        assert!(v["hooks"]["BeforeAgent"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("--agent gemini-cli"));

        let again = apply_hooks(&home, &base, false).unwrap();
        assert!(
            again.iter().all(|c| c.what == What::AlreadyDone),
            "second run should be a no-op: {again:?}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
    }

    /// Antigravity uses a named-map shape (`{"<name>": {"<Event>": [...]}}`) with
    /// `PreInvocation`/`Stop` events and a seconds timeout, written under our own
    /// hook name so a user's other hooks survive.
    #[test]
    fn antigravity_hooks_use_named_map_with_preinvocation_and_stop() {
        let home = tmpdir("hooks-antigravity");
        std::fs::create_dir_all(home.join(".gemini/config")).unwrap();
        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["antigravity".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        apply_hooks(&home, &base, false).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".gemini/config/hooks.json")).unwrap(),
        )
        .unwrap();
        let ours = &v["fuckmemory"];
        assert!(ours.get("PreInvocation").is_some());
        assert!(ours.get("Stop").is_some());
        assert!(
            ours.get("SessionEnd").is_none(),
            "antigravity has no SessionEnd"
        );
        let entry = &ours["PreInvocation"][0];
        assert_eq!(entry["type"], "command");
        assert_eq!(
            entry["timeout"], 10,
            "antigravity measures timeout in seconds"
        );
        assert!(
            entry["command"]
                .as_str()
                .unwrap()
                .contains("hook prompt --agent antigravity"),
            "got {entry}"
        );
        assert!(v["fuckmemory"]["Stop"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook session-end --agent antigravity"));

        let again = apply_hooks(&home, &base, false).unwrap();
        assert!(
            again.iter().all(|c| c.what == What::AlreadyDone),
            "second run should be a no-op: {again:?}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        assert!(
            !std::fs::read_to_string(home.join(".gemini/config/hooks.json"))
                .unwrap()
                .contains("fuckmemory"),
            "uninstall must clear our hook name"
        );
    }

    /// OpenCode has no settings-file hook channel; install writes a plugin into
    /// the global plugins dir, and uninstall removes the whole file.
    #[test]
    fn opencode_hooks_are_a_plugin_file_removed_wholesale() {
        let home = tmpdir("hooks-opencode");
        let base = Options {
            command: "/usr/local/bin/fuckmemory".into(),
            global: true,
            project: None,
            only: Some(vec!["opencode".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        apply_hooks(&home, &base, false).unwrap();
        let path = home.join(".config/opencode/plugins/fuckmemory.js");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("FuckmemoryAutosave"), "got {text}");
        assert!(text.contains("hook prompt --agent opencode"));
        assert!(text.contains("chat.message"));
        assert!(text.contains("experimental.chat.system.transform"));
        assert!(text.contains("/usr/local/bin/fuckmemory"));
        assert!(text.contains("// fuckmemory:begin"));

        // Idempotent: the plugin file is already ours.
        let again = apply_hooks(&home, &base, false).unwrap();
        assert!(
            again.iter().all(|c| c.what == What::AlreadyDone),
            "second run should be a no-op: {again:?}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        assert!(!path.exists(), "uninstall must delete the plugin file");
    }

    /// Cursor keeps a flat `{command, timeout}` array per event with a `version`
    /// header, so the patch must not build Anthropic's nested group shape.
    #[test]
    fn cursor_hooks_are_flat_with_version_and_timeout_in_seconds() {
        let home = tmpdir("hooks-cursor");
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["cursor".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        apply_hooks(&home, &base, false).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".cursor/hooks.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["version"], 1);
        assert!(v["hooks"].get("beforeSubmitPrompt").is_some());
        assert!(v["hooks"].get("sessionEnd").is_some());
        let entry = &v["hooks"]["beforeSubmitPrompt"][0];
        assert_eq!(entry["timeout"], 10);
        assert!(entry.get("type").is_none(), "cursor needs no type field");
        assert!(
            entry["command"]
                .as_str()
                .unwrap()
                .contains("hook prompt --agent cursor"),
            "got {entry}"
        );
        assert!(
            v["hooks"]["beforeSubmitPrompt"][0].get("hooks").is_none(),
            "must stay flat, not nested: {v}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        assert!(
            !std::fs::read_to_string(home.join(".cursor/hooks.json"))
                .unwrap()
                .contains("fuckmemory"),
            "uninstall must clear our entries"
        );
    }

    /// Copilot CLI uses `{type, bash, powershell, timeoutSec}` flat entries and
    /// camelCase event names, and user-level hooks live under `hooks/`.
    #[test]
    fn copilot_hooks_use_bash_keys_and_camelcase_events() {
        let home = tmpdir("hooks-copilot");
        std::fs::create_dir_all(home.join(".copilot")).unwrap();
        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["copilot-cli".into()]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        apply_hooks(&home, &base, false).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".copilot/hooks/fuckmemory.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["version"], 1);
        assert!(v["hooks"].get("userPromptSubmitted").is_some());
        assert!(v["hooks"].get("sessionEnd").is_some());
        let entry = &v["hooks"]["userPromptSubmitted"][0];
        assert_eq!(entry["type"], "command");
        assert_eq!(entry["timeoutSec"], 10);
        assert!(
            entry["bash"]
                .as_str()
                .unwrap()
                .contains("hook prompt --agent copilot-cli"),
            "got {entry}"
        );

        let undo = apply_hooks(&home, &base, true).unwrap();
        assert!(undo.iter().any(|c| c.what == What::RemoveHooks));
        assert!(
            !std::fs::read_to_string(home.join(".copilot/hooks/fuckmemory.json"))
                .unwrap()
                .contains("fuckmemory")
        );
    }

    #[test]
    fn project_and_global_hook_targets_are_distinct() {
        let home = tmpdir("hooks-proj");
        let proj = tmpdir("hooks-proj-ws");
        for d in [".gemini", ".cursor", ".copilot"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::create_dir_all(proj.join(".github")).unwrap();
        let base = Options {
            command: "/bin/fm".into(),
            global: true,
            project: Some(proj.clone()),
            only: Some(vec![
                "gemini-cli".into(),
                "cursor".into(),
                "copilot-cli".into(),
            ]),
            instructions: false,
            hooks: true,
            dry_run: false,
        };

        let first = apply_hooks(&home, &base, false).unwrap();
        assert!(
            first.len() >= 5,
            "each agent gets a global and where applicable a project hook: {first:?}"
        );
        assert!(
            proj.join(".github/hooks/fuckmemory.json").exists(),
            "copilot project hooks live in .github/hooks/"
        );
        assert!(proj.join(".cursor/hooks.json").exists());
        assert!(proj.join(".gemini/settings.json").exists());
    }

    #[test]
    fn manual_agents_get_a_snippet_not_a_write() {
        let home = tmpdir("manual");
        std::fs::create_dir_all(home.join(".kimi-code")).unwrap();
        let opts = Options {
            command: "/bin/fm".into(),
            global: true,
            project: None,
            only: Some(vec!["kimi-code".into()]),
            instructions: false,
            hooks: false,
            dry_run: false,
        };
        let ch = apply(&home, &opts, false).unwrap();
        assert_eq!(ch.len(), 1);
        assert!(matches!(ch[0].what, What::ManualSnippet(_)));
    }
}
