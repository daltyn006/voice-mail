#Requires -Version 7
# One-shot dev setup: MSVC build tools, CMake, Rust, Vulkan SDK, ffmpeg DLLs, submodules.
# Fully idempotent — safe to re-run any time.
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference = $true
# Anchor at the repo root (sentinel walk-up): works from any CWD, from scripts/
# or scripts/dev/, and stays put when scripts call each other (idempotent).
$dir = $PSScriptRoot
while ($dir -and -not (Test-Path (Join-Path $dir 'config/models.json'))) {
  $parent = Split-Path $dir -Parent
  if ($parent -eq $dir) { throw "repo root not found above $PSScriptRoot" }
  $dir = $parent
}
Set-Location $dir

function Ensure-WingetPackage {
  param([string]$Id, [string]$Override = '')
  # `winget list` exits non-zero when the package is absent; with
  # $PSNativeCommandUseErrorActionPreference='Stop' that would throw before
  # the $LASTEXITCODE check below runs. Probe with throwing disabled.
  $prevNative = $PSNativeCommandUseErrorActionPreference
  $PSNativeCommandUseErrorActionPreference = $false
  try {
    $installed = winget list --id $Id --accept-source-agreements 2>$null
    $found = ($LASTEXITCODE -eq 0) -and ($installed -match $Id)
  } finally {
    $PSNativeCommandUseErrorActionPreference = $prevNative
  }
  if ($found) {
    Write-Host "$Id already installed, skipping."
    return
  }
  $args = @('install', '--id', $Id, '-e', '--accept-source-agreements', '--accept-package-agreements')
  if ($Override) { $args += '--override', $Override }
  winget @args
}

# Run a native command whose failure is routine, not fatal: returns $true on
# exit 0 without tripping $PSNativeCommandUseErrorActionPreference.
# (A bare `cmd 2>$null; if ($LASTEXITCODE …)` still throws under Stop.)
# Output streams through so long fetches don't look hung.
function Invoke-Probe {
  param([scriptblock]$Command)
  $prevNative = $PSNativeCommandUseErrorActionPreference
  $PSNativeCommandUseErrorActionPreference = $false
  try {
    & $Command
    return $LASTEXITCODE -eq 0
  } finally {
    $PSNativeCommandUseErrorActionPreference = $prevNative
  }
}

Ensure-WingetPackage 'Microsoft.VisualStudio.2022.BuildTools' '--override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
Ensure-WingetPackage 'Kitware.CMake'
Ensure-WingetPackage 'Rustlang.Rustup'
Ensure-WingetPackage 'KhronosGroup.VulkanSDK'
Ensure-WingetPackage 'Gyan.FFmpeg'

# rustup default stable — idempotent
if (-not (Invoke-Probe { rustup default stable })) { Write-Host 'rustup default stable already set or failed, continuing...' }

# Init git repo first (submodules require one)
if (-not (Test-Path .git)) { git init }

# Unpack a backend source zip (codeload wraps the tree in one top folder):
# lift the inner dir contents into $Target. Returns $true on success.
function Expand-BackendZip {
  param([string]$Zip, [string]$Target, [string]$Name)
  $tmpdir = Join-Path $env:TEMP "$Name-src"
  if (Test-Path $tmpdir) { Remove-Item $tmpdir -Recurse -Force }
  Expand-Archive $Zip $tmpdir -Force
  $inner = Get-ChildItem $tmpdir -Directory | Select-Object -First 1
  if ($inner -and (Test-Path (Join-Path $inner.FullName 'CMakeLists.txt'))) {
    New-Item -ItemType Directory -Force $Target | Out-Null
    Copy-Item (Join-Path $inner.FullName '*') $Target -Recurse -Force
  }
  return Test-Path (Join-Path $Target 'CMakeLists.txt')
}

# Fetch one C++ backend source tree so core builds with the real backend.
# Order: existing dir > git submodule > codeload zip (CDN; sometimes works
# when git:443 doesn't) > $EnvZip override (local zip or extracted dir —
# the sneakernet path: copy once, setup lays it out) > manual-drop message.
function Ensure-BackendSource {
  param([string]$Name, [string]$GitUrl, [string]$ZipUrl, [string]$Target, [string]$EnvZip)
  $marker = Join-Path $Target 'CMakeLists.txt'
  if (Test-Path $marker) { Write-Host "$Name source present, skipping."; return }
  $ov = [Environment]::GetEnvironmentVariable($EnvZip)
  if ($ov -and (Test-Path $ov)) {
    if (((Get-Item $ov) -is [IO.DirectoryInfo]) -and (Test-Path (Join-Path $ov 'CMakeLists.txt'))) {
      New-Item -ItemType Directory -Force $Target | Out-Null
      Copy-Item (Join-Path $ov '*') $Target -Recurse -Force
      if (Test-Path $marker) { Write-Host "$Name staged from $EnvZip override."; return }
    } elseif ($ov -match '\.zip$') {
      try {
        if (Expand-BackendZip -Zip $ov -Target $Target -Name $Name) {
          Write-Host "$Name staged from $EnvZip override."
          return
        }
      } catch {
        Write-Host "$Name override zip failed: $_"
      }
    }
  }
  if (Invoke-Probe { git submodule add $GitUrl $Target }) {
    if (Invoke-Probe { git submodule update --init --recursive -- $Target }) {
      if (Test-Path $marker) { Write-Host "$Name submodule ready."; return }
    }
  } else {
    Write-Host "$Name submodule add failed, trying zip fallback..."
  }
  try {
    $tmp = Join-Path $env:TEMP "$Name-src.zip"
    Invoke-WebRequest $ZipUrl -OutFile $tmp -ErrorAction Stop
    if (Expand-BackendZip -Zip $tmp -Target $Target -Name $Name) {
      Write-Host "$Name fetched via zip fallback."
      return
    }
  } catch {
    Write-Host "$Name zip fallback failed: $_"
  }
  Write-Warning "$Name source missing: real transcription needs it. Copy the repo so that $marker exists (or set $EnvZip to a local zip/dir), then re-run setup."
}
Ensure-BackendSource -Name 'whisper.cpp' -GitUrl 'https://github.com/ggerganov/whisper.cpp' -ZipUrl 'https://codeload.github.com/ggerganov/whisper.cpp/zip/refs/heads/master' -Target 'core/thirdparty/whisper.cpp' -EnvZip 'PV_WHISPER_ZIP'
Ensure-BackendSource -Name 'llama.cpp' -GitUrl 'https://github.com/ggerganov/llama.cpp' -ZipUrl 'https://codeload.github.com/ggerganov/llama.cpp/zip/refs/heads/master' -Target 'core/thirdparty/llama.cpp' -EnvZip 'PV_LLAMA_ZIP'

# sqlite amalgamation (~1 file, keeps DB with zero new deps) — idempotent
New-Item -ItemType Directory -Force core/thirdparty/sqlite | Out-Null
$sqlite_done = Test-Path core/thirdparty/sqlite/sqlite3.c
if (-not $sqlite_done) {
  try {
    Invoke-WebRequest https://www.sqlite.org/2024/sqlite-amalgamation-3460100.zip -OutFile $env:TEMP/sqlite.zip -ErrorAction Stop
    Expand-Archive $env:TEMP/sqlite.zip core/thirdparty/sqlite -Force
    Copy-Item core/thirdparty/sqlite/sqlite-amalgamation-*/sqlite3.* core/thirdparty/sqlite/ -Force
  } catch {
    Write-Warning "sqlite download failed ($_); continuing without sqlite support."
  }
} else {
  Write-Host 'sqlite already present, skipping.'
}

# ffmpeg exe + shared DLLs beside the app (all-formats decode) — idempotent
New-Item -ItemType Directory -Force ffmpeg-dlls | Out-Null
$ff = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if ($ff) {
  $ffDir = Split-Path $ff
  Copy-Item (Join-Path $ffDir 'ffmpeg.exe') ffmpeg-dlls/ -Force -ErrorAction SilentlyContinue
  Copy-Item (Join-Path $ffDir '*.dll') ffmpeg-dlls/ -Force -ErrorAction SilentlyContinue
} else {
  Write-Warning 'ffmpeg not found on PATH; decode will fail until Gyan.FFmpeg install succeeds.'
}
Write-Host 'Setup done. Next: pwsh scripts/build.ps1 -FetchModels'
