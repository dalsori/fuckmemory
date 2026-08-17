# Solución de problemas

Problemas comunes y qué hacer con ellos.

## El hook dice "unknown hook event"

Tu binario es más nuevo (o más viejo) que la configuración de tu agente. Vuelve
a ejecutar `fuckmemory install --autosave` para reescribir los hooks, y
`fuckmemory update` para asegurarte de que estás al día.

## `doctor` reporta un mismatch de modelo

Los vectores se guardaron con un modelo de embeddings y cambiaste a otro.
Ejecuta `fuckmemory reindex` para re-embedderlo todo (o `doctor --fix`, que lo
hace automáticamente).

## Los agentes no recogen las memorias

Reinícialos después de `install` — los servidores MCP y los hooks se leen al
arrancar. Después comprueba con `fuckmemory agents` que estén conectados.

## El autosave ralentiza un prompt

No debería: todo el viaje de ida y vuelta son unos pocos milisegundos gracias a
la caché mmap del modelo. Ejecuta `fuckmemory bench` para ver tus propios
números, y asegúrate de que `fast = true` esté en la configuración.

## Un recall devuelve algo raro

```bash
fuckmemory explain "tu consulta"   # ver la vista cruda de cada recuperador
```

Esto muestra el coseno de cada hecho y si superó el umbral de relevancia, así un
ranking sorprendente se puede razonar en vez de adivinar.

## Pegué un secreto y quiero que desaparezca

```bash
fuckmemory forget <id> --hard     # borrar definitivamente
```

Después ejecuta `fuckmemory consolidate && fuckmemory prune --days 0` para
limpiar el episodio crudo.

## Compilar en Windows falla en `ar.exe` con el error `0xc000012f`

Algunos builds de `gcc` de Scoop/winlibs traen un `ar.exe` roto que crashea con
`STATUS_BAD_IMAGE (0xc000012f)` en cuanto intenta cargar el plugin LTO de
`lib\bfd-plugins\libdep.a` (un bug de empaquetado de binutils ya conocido). Cualquier
`cargo build --release` sobre el toolchain `x86_64-pc-windows-gnu` puede
tropezar con él, porque el perfil de release usa `lto = "fat"`.

Arregla el entorno, no el proyecto:

- Usa el **instalador de un comando** en vez de compilar — `irm
  https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex`
  descarga el binario precompilado y nunca toca cargo/gcc.
- O instala el toolchain estable: `rustup toolchain install stable-x86_64-pc-windows-msvc`
  y ponlo como predeterminado — el build MSVC no usa `ar.exe`.
- O elimina el plugin roto: renombra `lib\bfd-plugins\libdep.a` en el directorio
  del gcc de Scoop para que `ar.exe` deje de intentar cargarlo.

## El `fuckmemory` del PATH está desactualizado

Si una flag que existe en el código fuente se rechaza como "unexpected
argument", el binario de tu PATH es un build viejo — reconstruye y reinstala en
vez de añadir la flag. `fuckmemory update` arregla el caso común.

Volver al [índice de docs](../README.md).
