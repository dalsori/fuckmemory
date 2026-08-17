<#
install.ps1 — install fuckmemory on Windows and wire it into every agent.

Works two ways:

  1. One-liner (no Rust toolchain, no clone):
        irm https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex
     Downloads the newest prebuilt binary for Windows, installs it to
     %LOCALAPPDATA%\fuckmemory, adds it to your PATH, and registers every
     detected agent — autosave included.

  2. From a repo checkout (builds from source, like install.sh):
        powershell -ExecutionPolicy Bypass -File install.ps1
        powershell -ExecutionPolicy Bypass -File install.ps1 -NoAutosave -NoModel -DryRun
#>

[CmdletBinding()]
param(
    # Skip the per-prompt autosave hooks
    [switch]$NoAutosave,
    # Skip the ~125 MB embedding model download
    [switch]$NoModel,
    # Show what would change, change nothing
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 defaults to older TLS; GitHub only speaks TLS 1.2+.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Say($m) { Write-Host $m -ForegroundColor Cyan }
function Die($m) { Write-Error $m; exit 1 }

# The release workflow publishes only x86_64-pc-windows-msvc for Windows.
$nativeArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($nativeArch -like 'ARM*') {
    Die 'no prebuilt Windows ARM64 binary is published yet — install from source with: cargo install fuckmemory'
}

$target = 'x86_64-pc-windows-msvc'
$dest = Join-Path $env:LOCALAPPDATA 'fuckmemory'
$exe = Join-Path $dest 'fuckmemory.exe'

# A repo checkout has Cargo.toml in the current directory; `irm | iex` does not.
# Build from source when we can, otherwise fetch the prebuilt binary.
if ((Test-Path 'Cargo.toml') -and (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Say 'Building (release)…'
    cargo build --release
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item -Force 'target/release/fuckmemory.exe' $exe
}
else {
    Say "Downloading fuckmemory ($target)…"
    New-Item -ItemType Directory -Force $dest | Out-Null
    $zip = Join-Path $env:TEMP 'fuckmemory-install.zip'
    Invoke-WebRequest "https://github.com/dalsori/fuckmemory/releases/latest/download/fuckmemory-$target.zip" -OutFile $zip
    Expand-Archive -Force $zip $dest
    Remove-Item -Force $zip
}

Say "Installed to $dest"

# `cargo install` leaves a copy in ~\.cargo\bin. Whichever one comes first on
# PATH wins, so the two drift apart silently and you end up debugging a binary
# you did not just install. This script owns the install.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin\fuckmemory.exe'
if ((Test-Path $cargoBin) -and ($cargoBin -ne $exe)) {
    if ($DryRun) {
        Say "would remove the shadowing copy at $cargoBin"
    }
    else {
        Remove-Item -Force $cargoBin
        Say "removed the shadowing copy at $cargoBin"
    }
}

# Make the install dir findable from a fresh shell. User-level PATH, so no
# admin rights are needed.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $dest } else { "$userPath;$dest" }
    if ($DryRun) {
        Say "would add $dest to your user PATH"
    }
    else {
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        $env:Path = "$env:Path;$dest"
        Say "added $dest to your PATH"
    }
}

# Register with every agent, using the absolute path so agents launched from a
# GUI (with a different PATH than your shell) still find the binary.
$installArgs = @('install', '--command', $exe)
if (-not $NoAutosave) { $installArgs += '--autosave' }
if ($NoModel) { $installArgs += '--no-model' }
if ($DryRun) { $installArgs += '--dry-run' }

Write-Host
& $exe @installArgs

Write-Host
Say "Done. Check it with:  fuckmemory doctor"