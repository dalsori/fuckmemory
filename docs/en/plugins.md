# Plugins — extend without forking (herdr-style)

Like `herdr`'s `herdr-plugin.toml`, fuckmemory discovers `plugin.toml` folders under `~/.local/share/fuckmemory/plugins/<name>/`. v1.4.0 ships the skeleton: discovery, `plugin list/install/uninstall`, and the `on_store` hook (the only one wired so far).

## Layout

```
~/.local/share/fuckmemory/plugins/<name>/
  plugin.toml        # or plugin.json for JS folks
  index.js           # whatever your command runs, cwd = this dir
```

`FUCKMEMORY_HOME` overrides the root (so `plugins` live in `$FUCKMEMORY_HOME/plugins`).

## Manifest

`plugin.toml`:

```toml
[plugin]
name = "my-redactor"
version = "0.2.0"
description = "redact JIRA keys before storage"
command = "node index.js"      # stdin JSON -> stdout JSON
hooks = ["on_store"]           # only this is honored in 1.4.0
```

`plugin.json` alternative:

```json
{
  "plugin": {
    "name": "my-redactor",
    "command": "python main.py",
    "hooks": ["on_store"]
  }
}
```

Fields: `name` (required), `version` (default `0.1.0`), `description`, `command` (no hook runs without it), `hooks`.

## Commands

```bash
fuckmemory plugin list
fuckmemory plugin install /path/to/local/plug     # copy dir
fuckmemory plugin install owner/repo              # git clone https://github.com/owner/repo.git
fuckmemory plugin install https://github.com/owner/repo.git
fuckmemory plugin uninstall my-redactor
```

`install` from a local path copies the directory; from GitHub it `git clone`s. `list` sorts by name and shows `invalid` when a manifest fails to parse.

## Hook: on_store

Every `hook prompt` (autosaved prompt) runs `on_store` plugins after redaction and before `store::remember`:

- Input on stdin: `{"text": "...", "kind": "note|constraint|...", "source": "autosave:opencode"}`
- Output on stdout: `{"text": "new text"}` (optional) or `{"drop": true}` (currently logged but ignored) or empty (no-op).

Rules:
- **200 ms timeout per plugin** — exceeded → `failed` logged, original text kept.
- **Non-zero exit** → logged, original kept.
- **Non-JSON output** → error logged, original kept.
- **Never breaks the hook** — a plugin crash never stops the prompt.

Example `index.js`:

```js
// stdin -> transform -> stdout
const data = JSON.parse(require('fs').readFileSync(0, 'utf8'));
let text = data.text.replace(/JIRA-\d+/g, '[redacted]');
process.stdout.write(JSON.stringify({ text }));
```

## Roadmap

- v1.4.0: `on_store` only.
- v1.5.0: `on_recall` reranker, `plugin publish` via `herdr-plugin`-like topic, marketplace index.

Next: [bus](bus.md) or [maintenance](maintenance.md).
