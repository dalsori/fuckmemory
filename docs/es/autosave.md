# Autosave

Con `--autosave`, cada prompt que envías se guarda, y las memorias relevantes se
inyectan de vuelta en cada prompt — tanto si el modelo pensó en preguntar como
si no.

```bash
fuckmemory install --autosave     # o actívalo en `fuckmemory tui`
```

Esto conecta `fuckmemory hook prompt` al hook por prompt de cada agente. Cada
agente usa su propio canal: Claude Code, Codex, Qwen y Gemini usan hooks en
settings JSON; Cursor y Copilot CLI usan `hooks.json` planos; Antigravity un
`hooks.json` de mapa-nombrado; OpenCode un plugin en
`~/.config/opencode/plugins/`.

La inyección de recall (la mitad de "las memorias se devuelven") funciona en los
agentes cuyo canal de hook puede llevar contexto de vuelta: Claude Code, Codex,
Qwen, Gemini, Antigravity y OpenCode. Cursor y Copilot CLI guardan cada prompt
pero sus hooks ignoran el contexto inyectado, así que el autorecall ahí depende
de que el modelo llame a las herramientas MCP.

## Qué hace

- **Cada prompt se guarda**, verbatim, como episodio buscable. No se pierde nada.
- **Solo los prompts que parecen conocimiento durable se vuelven hechos.**
  "never force push to main" y "prefiero pnpm en este repo" entran al grafo como
  restricción y preferencia; "fix the flaky test in `src/db.rs`" se queda como
  episodio.
- **Los acuses se descartan.** "ok", "sí", "continue", `/compact`.
- **Las credenciales nunca llegan al disco.** Los tokens con prefijos conocidos,
  asignaciones a claves tipo-secreto y blobs opacos se reemplazan con
  `[redacted]` primero.
- **Los archivos sensibles se ignoran por nombre.** Añade globs a
  `[ignore] paths` (`.env`, `*.pem`, `~/.aws/*`) y cualquier prompt que nombre
  un archivo que coincida se redacta — los globs también funcionan con paths de
  Windows.
- **Nada se parafrasea.** No corre ningún modelo en este camino, así que el
  texto guardado es exactamente lo que escribiste, menos las redacciones.

## Auto-recall

Antes de que el prompt llegue al modelo, se busca en la memoria y los resultados
se devuelven como contexto, limitados por `autorecall.budget_tokens`. Los
prompts sin señal de búsqueda ("ok", `/clear`) se saltan esto.

Al final de la sesión, el hook consolida en vez de guardar: los duplicados se
fusionan y los índices se compactan, en el único momento en que nadie está
esperando.

Todo el viaje de ida y vuelta cuesta unos pocos milisegundos por prompt, gracias
a la caché mmap del modelo.

## Actívalo o desactívalo

`fuckmemory tui`, o cambia los ajustes en `$FUCKMEMORY_HOME/config.toml`:

```toml
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
```

Ver [configuración](config.md) para la referencia completa.

Siguiente: [comandos de uso diario](commands.md).
