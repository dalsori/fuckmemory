# Tarea — retomar un trabajo interrumpido

Un agente pierde su contexto cuando termina una sesión — se agota el presupuesto
de tokens, la máquina se reinicia, o toma el relevo otro agente. `task` registra
lo que el trabajo está haciendo *ahora mismo* para que la siguiente sesión
continúe exactamente donde se quedó la anterior, en vez de re-derivar el plan y
volver a tocar archivos.

## Los comandos

```bash
# El agente que está a punto de ser interrumpido deja un checkpoint:
fuckmemory task save "conecté el plugin de opencode, verificando install/uninstall" \
  --file src/install.rs --goal "añadir hooks de autosave a opencode"

# Quien retome (quizá otro agente, quizá mañana):
fuckmemory task status

# Cierra la tarea cuando se entregue; el último estado queda legible:
fuckmemory task done --note "verificado, desplegado"
```

`task remember` hace `task save` y además guarda el checkpoint como memoria
durable, para cuando quieras que `recall` lo encuentre de forma destacada.

## Cómo funciona

- El checkpoint activo es un documento JSON pequeño en la base de datos: el
  **objetivo (goal)**, el **estado actual**, los **archivos** que se están
  tocando, y las fechas de apertura/actualización.
- Hay una sola tarea activa a la vez. Actualizar conserva el objetivo y la fecha
  de apertura originales, así una tarea larga mantiene una sola fecha de
  nacimiento.
- Cada `task save` también se guarda como **episodio buscable** con
  `kind=task`. Así, un agente que retoma encuentra el checkpoint mediante
  `recall` incluso antes de pensar en pedir `task status` — la máquina nunca
  deja que se pierda el hilo.
- `task done` cierra la tarea; el checkpoint se conserva en la base de datos
  para que un `task status` final (o `--as-of`) todavía pueda leerlo.

## Flujo recomendado

1. Cuando empieces algo no trivial, `task save` una vez con el objetivo.
2. A medida que avances, `task save` de nuevo con el estado actualizado — el
   objetivo y la fecha de apertura se conservan automáticamente.
3. Si la sesión se corta, quien continúe ejecuta `task status` primero, y revisa
   con `recall` cualquier episodio de tarea que pudiera haberse perdido.
4. Cuando el trabajo se entregue, `task done --note "..."` para que la siguiente
   tarea arranque limpia.

Siguiente: [configuración](config.md).
