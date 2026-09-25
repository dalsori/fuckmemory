# Plugins — extiende sin forquear (estilo herdr)

Como `herdr-plugin.toml` de herdr, fuckmemory descubre carpetas `plugin.toml` en `~/.local/share/fuckmemory/plugins/<nombre>/`. La v1.4.0 trae el esqueleto: discovery, `plugin list/install/uninstall` y el hook `on_store`.

## Estructura

```
~/.local/share/fuckmemory/plugins/<nombre>/
  plugin.toml        # o plugin.json
  index.js
```

`FUCKMEMORY_HOME` cambia la raíz (`$FUCKMEMORY_HOME/plugins`).

## Manifiesto

`plugin.toml`:

```toml
[plugin]
name = "my-redactor"
version = "0.2.0"
description = "redacta claves JIRA antes de guardar"
command = "node index.js"
hooks = ["on_store"]
```

Campos: `name` (obligatorio), `version` (defecto `0.1.0`), `description`, `command`, `hooks`.

## Comandos

```bash
fuckmemory plugin list
fuckmemory plugin install /ruta/a/mi/plug
fuckmemory plugin install owner/repo
fuckmemory plugin uninstall my-redactor
```

## Hook: on_store

Cada `hook prompt` ejecuta plugins `on_store` después de redactar y antes de guardar:

- Entrada: `{"text": "...", "kind": "...", "source": "..."}`
- Salida: `{"text": "nuevo texto"}` o vacío.

Reglas: timeout 200 ms, si falla se loguea y se conserva el texto original. Nunca rompe el hook.

Siguiente: [bus](bus.md).
