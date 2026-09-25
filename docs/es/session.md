# Sesión — retomar el trabajo, entre agentes

Claude Code trabaja en una feature toda la mañana, la máquina se reinicia, y
OpenCode abre en el mismo proyecto. Los *hechos* viajan solos (`recall` los
inyecta), pero la **narrativa** — el objetivo, las decisiones tomadas, los
archivos tocados, lo que quedó en vuelo — se perdía con la conversación. Una
**sesión** es el contenedor que conserva esa narrativa, y el primer prompt de la
conversación nueva la recibe automáticamente.

Una sesión está ligada a un proyecto, agrupa los episodios y hechos de un tramo
de trabajo, y la comparten todos los agentes: el id de conversación del agente
es lo que escribe en ella, pero *de quién* era la conversación no importa.

## Los comandos

```bash
fuckmemory session start release --goal "sacar 1.3"   # opcional: un nombre
fuckmemory session list                                 # qué hemos estado haciendo
fuckmemory session show release                         # la narrativa completa
fuckmemory session end release                          # cerrarla explícitamente
```

## Cómo funciona

- **Automático.** No hace falta empezar una sesión. Con el autosave activo, el
  primer prompt de una conversación crea una (`work-2026-08-26`), y cada prompt
  guardado después se etiqueta dentro de ella. `session start <nombre>` solo
  sirve para cuando quieres un nombre legible y un objetivo.
- **Se cierra por inactividad.** Una sesión sigue abierta mientras esté activa;
  cuando lleva más tiempo quieta que la ventana de inactividad
  (`[session] idle_hours`, por defecto 24h), el siguiente prompt arranca una
  nueva. `session end` la cierra antes.
- **Traspaso entre agentes.** Cuando una conversación de un agente *distinto*
  empieza en un proyecto cuya sesión aún tiene trabajo, el primer prompt real
  recibe inyectado el contexto acumulado — nombre, objetivo, hechos aprendidos,
  episodios recientes y el checkpoint de `task` activo — antes de las memorias
  recordadas. Claude termina, OpenCode empieza, y lee "aquí está el trabajo en
  curso" sin que nadie se lo diga.
- **Sin modelo.** El contexto es un render acotado de lo ya guardado, así que el
  traspaso no cuesta nada y nunca puede comerse la ventana de contexto.

## Heartbeats — quién sigue trabajando

Cada `hook prompt` toca un heartbeat por agente y proyecto (`heartbeats`, ventana 3m). `session list` lo muestra estilo herdr:

```
● opencode       working 2026-09-25
○ claude-code    idle    2026-09-25
```

`● working` = visto hace <3m; `○ idle` en caso contrario. Sin servidor, solo SQLite.

## Flujo recomendado

1. Cuando empieces algo no trivial, `session start <nombre> --goal "..."`.
2. Trabaja en el agente que quieras. `session list` muestra las sesiones y los heartbeats.
3. Cuando otro agente retome el trabajo, el contexto se devuelve automáticamente en el primer prompt. `session show <nombre>` da la misma narrativa bajo demanda.
4. Cuando el trabajo se entregue, `session end <nombre>` para que el siguiente tramo arranque limpio.

El traspaso se puede apagar (`[autorecall] session = false`) y la ventana de
inactividad se ajusta (`[session] idle_hours = 24`).

Siguiente: [configuración](config.md).