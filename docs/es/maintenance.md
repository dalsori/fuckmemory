# Mantenimiento

Un almacén de memoria que nunca se consolida degenera en un log de
solo-append que envenena la recuperación. Estos comandos lo mantienen sano.

## Consolidar y podar

```bash
fuckmemory consolidate              # fusionar duplicados, cerrar contradicciones
fuckmemory prune --days 90          # borrar hechos retractados y nunca leídos
fuckmemory consolidate && fuckmemory prune --days 90   # la pareja para cron
```

Con autosave activado, el hook de fin de sesión consolida por ti en el único
momento en que nadie está esperando. Si no, ejecuta la pareja desde un cron.

`prune` es conservador: cualquier cosa con un hit se conserva, porque algo la
leyó una vez. Usa `--dry-run` para ver qué quitaría primero.

## Doctor

```bash
fuckmemory doctor              # rutas, esquema, modelo, caché, registraciones
fuckmemory doctor --fix        # reparar lo que encuentre, automáticamente
```

`doctor` verifica el esquema del almacén, el modelo de embeddings y su caché, y
que tus agentes sigan conectados. `--fix` reconstruye índices FTS que falten,
descarga el modelo, re-embedde si el modelo cambió, conecta agentes detectados y
consolida.

## Reindex

¿Cambiaste de modelo de embeddings? Los vectores de dos modelos no son
comparables.

```bash
fuckmemory reindex              # re-embedder cada hecho y episodio
```

`doctor --fix` también hace esto automáticamente cuando detecta un cambio de
modelo.

## Bench

Números de latencia reproducibles en un almacén desechable — tu base de datos
real nunca se toca:

```bash
fuckmemory bench                # medianas de write/recall, en frío y en caliente
./bench.sh                      # lo mismo, más una tabla markdown + gráfico ASCII
```

## Exportar e importar

```bash
fuckmemory export                     # volcar el scope actual como JSON
fuckmemory export --scope global      # volcar un scope con nombre
fuckmemory import dump.json           # cargarlo de vuelta
```

Export/import es un viaje de ida y vuelta: los episodios, sus referencias a
archivos y la procedencia de los hechos se conservan, y re-importar es
idempotente.

## Stats

```bash
fuckmemory stats               # scopes, episodios, hechos, entidades, tamaño
fuckmemory scopes              # listar los scopes de memoria
```

Siguiente: [solución de problemas](troubleshooting.md).
