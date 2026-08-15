# Guía de uso

Todo lo necesario para instalar, usar y mantener `fuckmemory`, por sistema
operativo. El README da el resumen de un vistazo; esto es el detalle.

## Instalación

`fuckmemory` es un único binario estático. No corre nada en segundo plano y no
se envía nada a ningún sitio: tus memorias viven en un único directorio local.

### Linux / macOS

**Instalación en un comando** (sin toolchain de Rust, sin clonar) — instala el
binario precompilado más nuevo y conecta todos los agentes detectados, con
autosave incluido:

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
```

Pasa `--no-autosave` para saltarte los hooks por prompt. En macOS y Linux
también puedes usar Homebrew:

```bash
brew tap dalsori/fuckmemory
brew install fuckmemory
```

O descarga el binario precompilado desde la
[página de releases](https://github.com/dalsori/fuckmemory/releases). Los
assets se llaman `fuckmemory-<target>.tar.gz`:

| Target | Cuándo |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86_64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64 (p. ej. Raspberry Pi) |
| `x86_64-apple-darwin` | Macs Intel |
| `aarch64-apple-darwin` | Macs Apple Silicon |

Después ejecuta `fuckmemory install` para conectar tus agentes.

### Windows

Abre **PowerShell** y descarga el zip para `x86_64-pc-windows-msvc` desde la
[página de releases](https://github.com/dalsori/fuckmemory/releases):

```powershell
$target = "x86_64-pc-windows-msvc"
$ver = "<última-versión>"
Invoke-WebRequest `
  "https://github.com/dalsori/fuckmemory/releases/download/v$ver/fuckmemory-$target.zip" `
  -OutFile "$env:TEMP\fuckmemory.zip"
Expand-Archive "$env:TEMP\fuckmemory.zip" "$env:LOCALAPPDATA\fuckmemory" -Force
```

Añade `%LOCALAPPDATA%\fuckmemory` a tu PATH, o referencia `fuckmemory.exe` por
su ruta completa al instalar:

```powershell
fuckmemory install --command "$env:LOCALAPPDATA\fuckmemory\fuckmemory.exe" --autosave
```

### Cualquier SO, desde crates.io o el código fuente

Con Rust 1.85+:

```bash
cargo install fuckmemory
fuckmemory install
```

O desde un clon:

```bash
git clone https://github.com/dalsori/fuckmemory && cd fuckmemory
./install.sh
```

`install` es idempotente, hace copia de seguridad de cada archivo que edita, y
`fuckmemory uninstall` lo revierte. Usa `--dry-run` para ver el plan antes.

## Conectar tus agentes

`fuckmemory install` detecta los agentes de código de esta máquina y se registra
como servidor MCP en cada uno. También escribe un bloque de instrucciones que le
dice al agente cuándo usar las herramientas.

```bash
fuckmemory install            # registrar MCP en todos
fuckmemory install --autosave # además conectar los hooks por prompt
fuckmemory agents             # ver qué se detectó y dónde está configurado
```

Reinicia tus agentes después de instalar para que carguen el servidor MCP.

## Autosave

Con `--autosave`, cada prompt que envías se guarda, y las memorias relevantes se
inyectan de vuelta en cada prompt — tanto si el modelo pensó en preguntar como
si no. Funciona mediante hooks por prompt en Claude Code, Codex, Gemini CLI,
Antigravity, Qwen, Cursor, Copilot CLI y OpenCode.

```bash
fuckmemory install --autosave
```

Puedes activarlo/desactivarlo después con `fuckmemory tui`, o cambiando la
configuración en `$FUCKMEMORY_HOME/config.toml`.

## Comandos de uso diario

```bash
fuckmemory remember "deploys go out through fly.io"          # guardar un hecho
fuckmemory recall "how do we deploy"                          # buscar memoria
fuckmemory recall "deploy" --raw                              # incluir notas crudas
fuckmemory forget 42                                          # retractar (suave)
fuckmemory timeline fly.io                                    # historial de una entidad
fuckmemory explain "deploy"                                   # ¿por qué este ranking?
```

Los hechos se guardan una vez y los comparten todos los agentes de la máquina:
Claude Code aprende tu proceso de deploy, y OpenCode lo recuerda en la siguiente
sesión.

## Retomar un trabajo interrumpido

Cuando una sesión se corta — se agota el presupuesto de tokens, la máquina se
reinicia, o toma el relevo otro agente — deja un checkpoint para que la siguiente
sesión continúe donde te quedaste:

```bash
fuckmemory task save "conecté el plugin de opencode, verificando install/uninstall" \
  --file src/install.rs --goal "añadir hooks de autosave a opencode"
fuckmemory task status   # quien retome lee esto
fuckmemory task done --note "desplegado"
```

Cada `task save` también se guarda como episodio buscable, así que `recall` lo
encuentra aunque nadie pida `task status`.

## Configuración

Los ajustes viven en `$FUCKMEMORY_HOME/config.toml` (por defecto
`~/.local/share/fuckmemory/config.toml`, se cambia con `FUCKMEMORY_HOME`):

```toml
model = "minishlab/potion-retrieval-32M"
budget_tokens = 1200
semantic = true
fast = true

[autosave]
enabled = true
min_chars = 12
facts = true
scope = "project"
redact = true

[autorecall]
enabled = true
limit = 6
budget_tokens = 600

[ignore]
paths = [".env", "*.pem", "~/.aws/*"]
```

Cada ajuste tiene una variable de entorno que lo sobreescribe (`FUCKMEMORY_*`),
documentada en el README. Las variables siempre ganan al archivo.

## Mantenerlo ordenado

Un almacén que nunca se consolida degenera en un log de solo-append. Con
autosave activado, el hook de fin de sesión consolida por ti. Si no, ejecútalo
desde un cron:

```bash
fuckmemory consolidate && fuckmemory prune --days 90
```

## Actualizar

Tus memorias viven en `$FUCKMEMORY_HOME`, que el binario nunca toca. Actualizar
solo reemplaza el ejecutable:

```bash
fuckmemory update                # descargar el release más nuevo y reemplazarse
fuckmemory update --check        # informar sin tocar nada
fuckmemory doctor                # confirmar que el almacén y las conexiones están bien
```

## Desinstalar

```bash
fuckmemory uninstall
```

Esto revierte cada byte que escribió `install`. Tus datos bajo
`$FUCKMEMORY_HOME` se dejan intactos; borra también ese directorio si quieres
que desaparezcan del todo.

## Qué escribe y dónde

| Agente | Config MCP | Hooks de autosave |
|---|---|---|
| Claude Code | `~/.claude.json`, `./.mcp.json` | `./.claude/settings.json` |
| OpenAI Codex CLI | `~/.codex/config.toml` | `./.codex/hooks.json` |
| Gemini CLI | `~/.gemini/settings.json` | `./.gemini/settings.json` |
| Antigravity CLI | `~/.gemini/config/mcp_config.json`, `./.agents/mcp_config.json` | `~/.gemini/config/hooks.json`, `./.agents/hooks.json` |
| OpenCode | `~/.config/opencode/opencode.json[c]` | `~/.config/opencode/plugins/fuckmemory.js`, `./.opencode/plugins/fuckmemory.js` |
| Qwen Code | `~/.qwen/settings.json` | `./.qwen/settings.json` |
| Cursor | `~/.cursor/mcp.json` | `./.cursor/hooks.json` |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` | `~/.copilot/hooks/fuckmemory.json`, `./.github/hooks/fuckmemory.json` |
| VS Code | `./.vscode/mcp.json` | — |
| Kimi Code | imprime un fragmento — formato sin verificar | — |

## Solución de problemas

- **El hook dice "unknown hook event"** — tu binario es más nuevo (o más viejo)
  que la configuración de tu agente. Vuelve a ejecutar
  `fuckmemory install --autosave` para reescribir los hooks, y
  `fuckmemory update` para asegurarte de que estás al día.
- **`doctor` reporta un mismatch de modelo** — los vectores se guardaron con un
  modelo y cambiaste a otro. Ejecuta `fuckmemory reindex` para re-embedderlo todo.
- **Los agentes no recogen las memorias** — reinícialos después de `install`, y
  comprueba con `fuckmemory agents` que estén conectados.
- **El autosave ralentiza un prompt** — no debería: todo el viaje de ida y vuelta
  son unos pocos milisegundos gracias a la caché mmap del modelo. Ejecuta
  `fuckmemory bench` para ver tus propios números.
