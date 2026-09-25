# Bus — mensajería activa entre agentes

El grafo de memoria es pasivo: tienes que hacer `recall`. El bus es activo: un agente `send`, el otro lo recibe en el siguiente prompt sin preguntar.

Por proyecto, en SQLite (`bus_messages` + `bus_cursors`), sin servidor ni nube.

## Comandos

```bash
fuckmemory bus send "revisa el deploy antes de mergear" --to codex       # dirigido
fuckmemory bus send "cuando esté verde, shippea" --channel ops            # broadcast
fuckmemory bus send "ping corto" --ttl 60                                 # expira en 60s
fuckmemory bus poll --agent opencode --limit 10                           # no leídos para opencode (marca visto)
fuckmemory bus list --channel ops --limit 20                              # recientes, no marca visto
fuckmemory inbox --agent opencode                                         # alias de bus poll
fuckmemory inbox --channel ops
```

Flags:
- `--to <agente>` — `claude-code`, `codex`, `opencode`, etc. Sin él es broadcast.
- `--channel <nombre>` — por defecto `general`. Normalizado: minúsculas, no alfanum → `-`, máx 32. `Mi Canal!` → `mi-canal`.
- `--ttl <segundos>` — expira tras N segundos. `0` expira inmediato.
- `--scope <path|global>` — scope del proyecto, por defecto el actual.
- `--from <agente>` — fuerza el remitente (auto-detectado si no).

## Cómo funciona

- **Cursor por agente.** `bus_cursors(scope_id, agent, last_seen_id)` guarda el último mensaje visto por cada agente. `poll`/`inbox` devuelven solo `id > last_seen` donde `to_agent IS NULL OR to_agent = tú`, y avanzan el cursor.
- **Inyección.** Cada `hook prompt` (cuando vale la pena recordar) hace un `poll` del agente del prompt (máx 5 mensajes, 400 tokens) e inyecta `## Inbox` antes del recall. El siguiente prompt no lo repite.
- **TTL.** `created_at + ttl_ms <= now` se borran en cada send/poll, así nunca llegan mensajes expirados.

## Flujo recomendado

1. Claude planifica y ve un gotcha: `fuckmemory bus send "la réplica de staging tarda 2m" --to opencode --channel ops`
2. Cambias a OpenCode y escribes cualquier cosa: el hook inyecta el inbox, OpenCode lo lee sin llamar a una herramienta.
3. `bus list` es la auditoría (más nuevo primero), `bus poll` es el buzón (más viejo primero, marca visto).

## MCP

Dos herramientas extra para el agente:

- `bus_send { body, channel?, to_agent?, ttl_seconds?, scope? }`
- `bus_poll { channel?, limit?, scope? }`

Siguiente: [plugins](plugins.md) o [sesión](session.md).
