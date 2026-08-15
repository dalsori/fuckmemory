# Agentes

`fuckmemory install` detecta los agentes de código de esta máquina y se registra
como servidor MCP en cada uno. También escribe un bloque de instrucciones que le
dice al agente cuándo usar las herramientas.

```bash
fuckmemory install            # registrar MCP en todos
fuckmemory install --autosave # además conectar los hooks por prompt
fuckmemory agents             # ver qué se detectó y dónde está configurado
```

Reinicia tus agentes después de instalar para que carguen el servidor MCP.

## Qué escribe y dónde

| Agente | Config MCP | Instrucciones | Hooks de autosave |
|---|---|---|---|
| Claude Code | `~/.claude.json`, `./.mcp.json` | `CLAUDE.md` | `./.claude/settings.json` |
| OpenAI Codex CLI | `~/.codex/config.toml` | `AGENTS.md` | `./.codex/hooks.json` |
| Gemini CLI | `~/.gemini/settings.json` | `AGENTS.md` | `./.gemini/settings.json` |
| Antigravity CLI | `~/.gemini/config/mcp_config.json`, `./.agents/mcp_config.json` | `AGENTS.md` | `~/.gemini/config/hooks.json`, `./.agents/hooks.json` |
| OpenCode | `~/.config/opencode/opencode.json[c]` | `AGENTS.md` | `~/.config/opencode/plugins/fuckmemory.js`, `./.opencode/plugins/fuckmemory.js` |
| Qwen Code | `~/.qwen/settings.json` | `AGENTS.md` | `./.qwen/settings.json` |
| Cursor | `~/.cursor/mcp.json` | `AGENTS.md` | `./.cursor/hooks.json` |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` | `AGENTS.md` | `~/.copilot/hooks/fuckmemory.json`, `./.github/hooks/fuckmemory.json` |
| VS Code | `./.vscode/mcp.json` | `AGENTS.md` | — |
| Kimi Code | imprime un fragmento — formato sin verificar | `AGENTS.md` | — |

El bloque de instrucciones vive entre los marcadores
`<!-- fuckmemory:begin -->` y `<!-- fuckmemory:end -->`, así que volver a
ejecutar `install` lo reemplaza en su sitio sin duplicarlo ni pisar lo que
escribiste alrededor.

Todo lo que escribe la herramienta vive en un único directorio que puedes
borrar: `~/.local/share/fuckmemory` (se cambia con `FUCKMEMORY_HOME`).

## Las cuatro herramientas MCP que ve el agente

- **`recall`** — buscar memoria antes de asumir nada.
- **`remember`** — guardar algo que importará la próxima sesión.
- **`forget`** — retractar algo incorrecto u obsoleto (suave por defecto).
- **`timeline`** — cómo cambió el conocimiento sobre una entidad con el tiempo.

Siguiente: [activar el autosave](autosave.md).
