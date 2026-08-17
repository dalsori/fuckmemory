# Instalación

`fuckmemory` es un único binario estático. No corre nada en segundo plano y no
se envía nada a ningún sitio: tus memorias viven en un único directorio local.

## Linux / macOS

**Instalación en un comando** (sin toolchain de Rust, sin clonar) — instala el
binario precompilado más nuevo y conecta todos los agentes detectados, con
autosave incluido:

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
```

Pasa `--no-autosave` para saltarte los hooks por prompt. En macOS y Linux
también puedes usar Homebrew:

```bash
brew tap dalsori/fuckmemory
brew install fuckmemory
```

O descarga el binario precompilado desde la
[página de releases](https://github.com/dalsori/fuckmemory/releases). Los
assets se llaman `fuckmemory-<target>.tar.gz`:

| Target | Cuándo |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86_64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64 (p. ej. Raspberry Pi) |
| `x86_64-apple-darwin` | Macs Intel |
| `aarch64-apple-darwin` | Macs Apple Silicon |

Después ejecuta `fuckmemory install` para conectar tus agentes — ver
[agentes](agents.md).

## Windows

**Instalación en un comando** (sin toolchain de Rust, sin clonar) — instala el
binario precompilado más nuevo en `%LOCALAPPDATA%\fuckmemory`, lo añade a tu
PATH y conecta todos los agentes detectados, con autosave incluido:

```powershell
irm https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex
```

Desde un clon del repositorio, para compilar desde el código fuente:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1        # compilar + instalar + conectar
powershell -ExecutionPolicy Bypass -File install.ps1 -NoAutosave -NoModel -DryRun
```

O a mano: descarga el asset `fuckmemory-x86_64-pc-windows-msvc.zip` de la
[página de releases](https://github.com/dalsori/fuckmemory/releases), extráelo
en algún lugar de tu PATH (o en `%LOCALAPPDATA%\fuckmemory`), y ejecuta:

```powershell
fuckmemory install --command "$env:LOCALAPPDATA\fuckmemory\fuckmemory.exe" --autosave
```

El instalador pasa la ruta absoluta del binario, así que los agentes lanzados
desde una GUI (que pueden tener un PATH distinto al de tu shell) también lo
encuentran.

## Cualquier SO, desde crates.io o el código fuente

Con Rust 1.85+:

```bash
cargo install fuckmemory
fuckmemory install
```

O desde un clon:

```bash
git clone https://github.com/dalsori/fuckmemory && cd fuckmemory
./install.sh
```

`install` es idempotente, hace copia de seguridad de cada archivo que edita, y
`fuckmemory uninstall` lo revierte. Usa `--dry-run` para ver el plan antes.

## Actualizar

Tus memorias viven en `$FUCKMEMORY_HOME`, que el binario nunca toca. Actualizar
solo reemplaza el ejecutable:

```bash
fuckmemory update                # descargar el release más nuevo y reemplazarse
fuckmemory update --check        # informar sin tocar nada
fuckmemory doctor                # confirmar que el almacén y las conexiones están bien
```

## Desinstalar

```bash
fuckmemory uninstall
```

Esto revierte cada byte que escribió `install`. Tus datos bajo
`$FUCKMEMORY_HOME` se dejan intactos; borra también ese directorio si quieres
que desaparezcan del todo.

Siguiente: [conectar tus agentes](agents.md).
