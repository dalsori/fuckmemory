//! Plugin skeleton — 1.4.0 minimal.
//!
//! Like herdr `herdr-plugin.toml`, but for fuckmemory: a folder under
//! `~/.local/share/fuckmemory/plugins/<name>/plugin.toml` that declares
//! a `command` to run and which hooks it handles. For 1.4.0 only `on_store`
//! is wired; the loader itself is the value — `plugin list` and discovery
//! already work, `on_recall` and a marketplace come later.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::Config;

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    plugin: PluginMeta,
}

#[derive(Debug, Clone, Deserialize)]
struct PluginMeta {
    name: String,
    version: Option<String>,
    description: Option<String>,
    /// Shell command to run, cwd = plugin dir. stdin JSON -> stdout JSON.
    /// Example: "node index.js" or "python main.py"
    command: Option<String>,
    /// Hooks this plugin handles, e.g. ["on_store"]
    hooks: Option<Vec<String>>,
}

/// One discovered plugin.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub command: Option<String>,
    pub hooks: Vec<String>,
    /// Absolute path to the plugin directory.
    pub path: PathBuf,
    /// Whether the manifest parsed correctly.
    pub valid: bool,
    pub error: Option<String>,
}

pub fn plugins_dir(cfg: &Config) -> PathBuf {
    cfg.home.join("plugins")
}

/// Discover plugins by scanning `plugins_dir`. Invalid manifests are returned
/// with `valid = false` rather than erroring the whole scan.
pub fn discover(cfg: &Config) -> Vec<Plugin> {
    let dir = plugins_dir(cfg);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            // Also accept plugin.json for JS folks
            let json_path = path.join("plugin.json");
            if json_path.exists() {
                match parse_json_manifest(&json_path, &path) {
                    Ok(p) => out.push(p),
                    Err(e) => out.push(Plugin {
                        name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                        version: "0.0.0".into(),
                        description: String::new(),
                        command: None,
                        hooks: vec![],
                        path: path.clone(),
                        valid: false,
                        error: Some(e.to_string()),
                    }),
                }
            }
            continue;
        }
        match parse_manifest(&manifest_path, &path) {
            Ok(p) => out.push(p),
            Err(e) => out.push(Plugin {
                name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                version: "0.0.0".into(),
                description: String::new(),
                command: None,
                hooks: vec![],
                path: path.clone(),
                valid: false,
                error: Some(e.to_string()),
            }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn parse_manifest(path: &Path, dir: &Path) -> Result<Plugin> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: Manifest = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let name = manifest.plugin.name.trim().to_string();
    anyhow::ensure!(!name.is_empty(), "plugin.name cannot be empty");
    Ok(Plugin {
        name: name.clone(),
        version: manifest.plugin.version.unwrap_or_else(|| "0.1.0".into()),
        description: manifest.plugin.description.unwrap_or_default(),
        command: manifest.plugin.command,
        hooks: manifest.plugin.hooks.unwrap_or_default(),
        path: dir.to_path_buf(),
        valid: true,
        error: None,
    })
}

fn parse_json_manifest(path: &Path, dir: &Path) -> Result<Plugin> {
    let text = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let obj = v.get("plugin").unwrap_or(&v);
    let name = obj
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(Plugin {
        name: name.clone(),
        version: obj
            .get("version")
            .and_then(|s| s.as_str())
            .unwrap_or("0.1.0")
            .to_string(),
        description: obj
            .get("description")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        command: obj.get("command").and_then(|s| s.as_str()).map(|s| s.to_string()),
        hooks: obj
            .get("hooks")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default(),
        path: dir.to_path_buf(),
        valid: true,
        error: None,
    })
}

/// Apply `on_store` plugins to a text before it is stored.
/// Each plugin with `on_store` in its hooks is run as a subprocess with
/// `{"text": "...", "kind": "...", "source": "..."}` on stdin. It may return
/// `{"text": "new text", "drop": true}`. Failures/timeouts are logged and
/// ignored — a plugin must never break the hook.
///
/// Timeout: 200ms per plugin, as in the spec.
pub fn apply_on_store(
    cfg: &Config,
    mut text: String,
    kind: &str,
    source: &str,
) -> String {
    let plugins = discover(cfg);
    for p in plugins.iter().filter(|pl| pl.valid && pl.hooks.iter().any(|h| h == "on_store")) {
        let Some(cmd) = p.command.as_deref() else {
            continue;
        };
        match run_plugin(&p.path, cmd, &text, kind, source) {
            Ok(Some(new_text)) => text = new_text,
            Ok(None) => {} // no change
            Err(e) => {
                eprintln!("fuckmemory: plugin {} failed: {e:#}", p.name);
            }
        }
    }
    text
}

fn run_plugin(dir: &Path, cmd: &str, text: &str, kind: &str, source: &str) -> Result<Option<String>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let input = serde_json::json!({
        "text": text,
        "kind": kind,
        "source": source,
    })
    .to_string();

    // Run via shell so "node index.js" works as a single string.
    let mut child = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    child.current_dir(dir);
    child.stdin(Stdio::piped());
    child.stdout(Stdio::piped());
    child.stderr(Stdio::piped());

    let mut child = child.spawn().context("spawning plugin")?;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(input.as_bytes());
    }
    // Wait with timeout 200ms
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let mut err = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        use std::io::Read;
                        let _ = stderr.read_to_string(&mut err);
                    }
                    anyhow::bail!("plugin exited with {status}: {err}");
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    let _ = stdout.read_to_string(&mut out);
                }
                let out = out.trim();
                if out.is_empty() {
                    return Ok(None);
                }
                let v: serde_json::Value = serde_json::from_str(out).context("plugin output not JSON")?;
                if v.get("drop").and_then(|d| d.as_bool()).unwrap_or(false) {
                    // dropping means the caller should skip storage; we signal by returning empty marker
                    // For 1.4.0, drop is not yet honored at call site — we just return None and log.
                    eprintln!("fuckmemory: plugin {} requested drop (ignored in 1.4.0)", dir.display());
                    return Ok(None);
                }
                if let Some(t) = v.get("text").and_then(|s| s.as_str()) {
                    return Ok(Some(t.to_string()));
                }
                return Ok(None);
            }
            Ok(None) => {
                if start.elapsed() > Duration::from_millis(200) {
                    let _ = child.kill();
                    anyhow::bail!("timeout after 200ms");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_cfg(tag: &str) -> Config {
        let dir = std::env::temp_dir().join(format!("fm-plug-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Config {
            home: dir,
            ..Config::default()
        }
    }

    #[test]
    fn discover_empty_when_no_plugins() {
        let cfg = tmp_cfg("empty");
        assert_eq!(discover(&cfg).len(), 0);
    }

    #[test]
    fn discover_parses_manifest() {
        let cfg = tmp_cfg("parse");
        let plug_dir = plugins_dir(&cfg).join("my-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        fs::write(
            plug_dir.join("plugin.toml"),
            r#"
[plugin]
name = "my-plug"
version = "0.2.0"
description = "test plugin"
command = "echo hi"
hooks = ["on_store"]
"#,
        )
        .unwrap();
        let plugs = discover(&cfg);
        assert_eq!(plugs.len(), 1);
        assert_eq!(plugs[0].name, "my-plug");
        assert_eq!(plugs[0].version, "0.2.0");
        assert!(plugs[0].valid);
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn apply_on_store_no_plugins_is_noop() {
        let cfg = tmp_cfg("noop");
        let out = apply_on_store(&cfg, "hello".into(), "note", "cli");
        assert_eq!(out, "hello");
    }

    #[test]
    fn discover_handles_invalid_manifest() {
        let cfg = tmp_cfg("invalid");
        let plug_dir = plugins_dir(&cfg).join("bad-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        fs::write(plug_dir.join("plugin.toml"), "this is not toml [[[ ").unwrap();
        let plugs = discover(&cfg);
        assert_eq!(plugs.len(), 1);
        assert!(!plugs[0].valid);
        assert!(plugs[0].error.is_some());
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn discover_ignores_dir_without_manifest() {
        let cfg = tmp_cfg("nomani");
        let plug_dir = plugins_dir(&cfg).join("empty-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        fs::write(plug_dir.join("README.md"), "# hi").unwrap();
        let plugs = discover(&cfg);
        assert_eq!(plugs.len(), 0);
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn discover_parses_json_manifest() {
        let cfg = tmp_cfg("json");
        let plug_dir = plugins_dir(&cfg).join("json-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        fs::write(
            plug_dir.join("plugin.json"),
            r#"{"plugin": {"name": "json-plug", "command": "echo hi", "hooks": ["on_store"]}}"#,
        )
        .unwrap();
        let plugs = discover(&cfg);
        assert_eq!(plugs.len(), 1);
        assert_eq!(plugs[0].name, "json-plug");
        assert!(plugs[0].valid);
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn discover_sorts_by_name() {
        let cfg = tmp_cfg("sort");
        for name in ["zebra", "apple", "middle"] {
            let d = plugins_dir(&cfg).join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("plugin.toml"), format!("[plugin]\nname = \"{name}\"\n")).unwrap();
        }
        let plugs = discover(&cfg);
        assert_eq!(plugs[0].name, "apple");
        assert_eq!(plugs[1].name, "middle");
        assert_eq!(plugs[2].name, "zebra");
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn plugin_without_command_is_skipped() {
        let cfg = tmp_cfg("nocmd");
        let plug_dir = plugins_dir(&cfg).join("no-cmd");
        fs::create_dir_all(&plug_dir).unwrap();
        fs::write(
            plug_dir.join("plugin.toml"),
            r#"[plugin]
name = "no-cmd"
hooks = ["on_store"]
"#,
        )
        .unwrap();
        let out = apply_on_store(&cfg, "hello".into(), "note", "cli");
        // Should not fail, just no-op because no command
        assert_eq!(out, "hello");
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn apply_on_store_transforms_text() {
        let cfg = tmp_cfg("transform");
        let plug_dir = plugins_dir(&cfg).join("xform");
        fs::create_dir_all(&plug_dir).unwrap();
        // Create a file with the JSON payload and cat/type it — more portable than echo quoting
        fs::write(plug_dir.join("out.json"), r#"{"text":"TRANSFORMED"}"#).unwrap();
        let cmd = if cfg!(windows) { "type out.json" } else { "cat out.json" };
        fs::write(
            plug_dir.join("plugin.toml"),
            format!(
                "[plugin]\nname = \"xform\"\ncommand = \"{cmd}\"\nhooks = [\"on_store\"]\n"
            ),
        )
        .unwrap();
        let out = apply_on_store(&cfg, "hello".into(), "note", "cli");
        assert_eq!(out, "TRANSFORMED");
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn apply_on_store_handles_failing_plugin_gracefully() {
        let cfg = tmp_cfg("fail");
        let plug_dir = plugins_dir(&cfg).join("fail-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        // Command that exits non-zero
        let cmd = if cfg!(windows) { "exit 1" } else { "false" };
        fs::write(
            plug_dir.join("plugin.toml"),
            format!("[plugin]\nname = \"fail-plug\"\ncommand = \"{cmd}\"\nhooks = [\"on_store\"]\n"),
        )
        .unwrap();
        let out = apply_on_store(&cfg, "hello".into(), "note", "cli");
        // Should return original text, not crash
        assert_eq!(out, "hello");
        fs::remove_dir_all(&cfg.home).ok();
    }

    #[test]
    fn apply_on_store_handles_timeout() {
        let cfg = tmp_cfg("timeout");
        let plug_dir = plugins_dir(&cfg).join("slow-plug");
        fs::create_dir_all(&plug_dir).unwrap();
        let cmd = if cfg!(windows) {
            "ping -n 2 127.0.0.1 > nul"
        } else {
            "sleep 1"
        };
        fs::write(
            plug_dir.join("plugin.toml"),
            format!("[plugin]\nname = \"slow-plug\"\ncommand = \"{cmd}\"\nhooks = [\"on_store\"]\n"),
        )
        .unwrap();
        let start = std::time::Instant::now();
        let out = apply_on_store(&cfg, "hello".into(), "note", "cli");
        assert_eq!(out, "hello");
        assert!(start.elapsed() < std::time::Duration::from_millis(600), "should not wait 1s, should timeout at 200ms");
        fs::remove_dir_all(&cfg.home).ok();
    }
}
