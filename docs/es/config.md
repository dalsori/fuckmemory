# Configuración

Los ajustes viven en `$FUCKMEMORY_HOME/config.toml` (por defecto
`~/.local/share/fuckmemory/config.toml`), escritos por `fuckmemory tui` y
seguros de editar a mano.

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

## Variables de entorno

Cada ajuste tiene una variable `FUCKMEMORY_*` que gana al archivo, así que una
variable puesta para un shell siempre manda:

| Variable | Default | Significado |
|---|---|---|
| `FUCKMEMORY_HOME` | `~/.local/share/fuckmemory` | base de datos, ajustes, modelo |
| `FUCKMEMORY_MODEL` | `minishlab/potion-retrieval-32M` | id de repo HF o carpeta local |
| `FUCKMEMORY_BUDGET` | `1200` | presupuesto de tokens del recall |
| `FUCKMEMORY_SEMANTIC` | on | `0` para solo BM25 + grafo, sin modelo |
| `FUCKMEMORY_FAST` | on | `0` para cargar siempre el modelo real |
| `FUCKMEMORY_AUTOSAVE` | off | `1` para guardar cada prompt |
| `FUCKMEMORY_AUTORECALL` | off | `1` para inyectar memorias en cada prompt |
| `FUCKMEMORY_REDACT` | on | `0` para guardar prompts sin filtrar |
| `FUCKMEMORY_IGNORE_PATHS` | — | globs separados por coma de archivos a redactar |

## El modelo

El modelo por defecto ocupa **125 MB** en disco (32M parámetros en f32).
`FUCKMEMORY_MODEL=minishlab/potion-base-8M` es ~4x más pequeño y carga ~4x más
rápido, con algo de calidad de recuperación de menos. Ejecuta
`fuckmemory reindex` después de cambiar — los vectores de dos modelos no son
comparables.

`fast = true` usa una caché mmap precompilada para que una invocación en frío
cueste ~1 ms en vez de ~206 ms. `fuckmemory model cache --force` la reconstruye
y `fuckmemory model verify` la re-comprueba contra el modelo real.

## Scopes

Las memorias tienen espacio de nombres. Un scope de proyecto se ancla a la raíz
del repositorio, así que los subdirectorios lo comparten. El scope `global`
guarda hechos sobre *ti* que viajan entre proyectos — un recall lee el proyecto
actual **y** `global` juntos.

```bash
fuckmemory remember --scope global "no emoji in commit messages, ever"
fuckmemory recall "commit" --scope global
```

## La pantalla de ajustes

`fuckmemory tui` es la forma interactiva de cambiar todo esto: autosave,
auto-recall, búsqueda semántica, la caché rápida, presupuestos de tokens, y un
panel que lista tus memorias reales (con `/` para buscar y `x` para retractar).
Los ajustes fijados por una variable `FUCKMEMORY_*` se marcan `env` y no se
pueden cambiar desde la pantalla, porque la variable seguiría ganando.

Siguiente: [mantenimiento](maintenance.md).
