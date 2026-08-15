# Comandos

El CLI de uso diario, para cuando quieres guardar y buscar memoria a mano en vez
de depender del autosave.

## Guardar

```bash
fuckmemory remember "deploys go out through fly.io"   # una nota simple
fuckmemory remember "preferimos pnpm aquí" --kind preference
fuckmemory remember "la api habla con postgres" \
  --src api --rel talks_to --dst postgres            # un hecho estructurado
fuckmemory remember "sin emojis en los commits" --scope global
```

`--file PATH:DESDE-HASTA` adjunta el archivo contra el que se aprendió una
memoria, para que un recall apunte directo a la fuente:

```bash
fuckmemory remember "el build corre en node 22" --file Makefile:1-6
```

## Buscar

```bash
fuckmemory recall "how do we deploy"         # lo que crees ahora
fuckmemory recall "deploy" --raw              # incluir las notas crudas originales
fuckmemory recall "pnpm" --as-of 2026-03-01   # lo que creías entonces
fuckmemory recall "deploy" --limit 5          # menos hits, mejor rankeados
fuckmemory recall "deploy" --json             # salida legible por máquina
```

El recall usa tres recuperadores independientes — BM25, embeddings estáticos y
el grafo de entidades — fusiona sus rankings, elimina casi-duplicados, y limita
la salida a un presupuesto de tokens para que nunca inunde la ventana de
contexto.

## Retractar y corregir

```bash
fuckmemory forget 42              # suave: deja de ser vivo, sigue en timeline
fuckmemory forget --query "the replica lags"
fuckmemory forget 42 --hard       # borrar definitivamente (para secretos)
fuckmemory timeline fly.io        # cómo cambió la historia de esa entidad
```

El forget suave es el default a propósito: un hecho nunca se "borra", se cierra,
así que `--as-of` puede seguir respondiendo lo que creías antes de cambiar de
opinión.

## Depurar un ranking

```bash
fuckmemory explain "deploy"       # desglose por recuperador
```

`explain` muestra la vista cruda de cada recuperador, incluido el coseno de cada
hecho y si superó el umbral de relevancia. Úsalo cuando un recall te sorprenda o
después de cambiar de modelo de embeddings.

Siguiente: [retomar un trabajo interrumpido](task.md).
