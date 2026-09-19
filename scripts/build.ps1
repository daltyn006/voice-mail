#Requires -Version 7
# One-go build for voice mail (native GPUI app, no Tauri).
# Single entry point; everything else in scripts/ is a pipeline stage this
# file calls, scripts/dev/ holds test tools.
#
#   pwsh scripts/build.ps1                     # smoke -> DLL -> release binary
#   pwsh scripts/build.ps1 -Dev                # fast dev-profile build (iteration, not shipping)
#   pwsh scripts/build.ps1 -Setup -FetchModels # full first-time run (toolchains + models)
#   pwsh scripts/build.ps1 -SkipTests          # skip the Node smoke test
#   pwsh scripts/build.ps1 -Package            # + NSIS per-machine exe + per-user MSI via cargo-packager
param(
  [switch]$Setup,
  [switch]$FetchModels,
  [switch]$AllModels,
  [switch]$Dev,
  [switch]$SkipTests,
  [switch]$Package
)
$ErrorActionPreference = 'Stop'
# Fail fast on native-command failure too (cmake/cargo/node non-zero exits
# must abort the pipeline, never fall through to a misleading "Done").
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

if ($Setup) {
  & "$PSScriptRoot/setup-windows.ps1"
}
if ($FetchModels) {
  if ($AllModels) { & "$PSScriptRoot/fetch-models.ps1" -All }
  else { & "$PSScriptRoot/fetch-models.ps1" }
}
if (-not $SkipTests) {
  & "$PSScriptRoot/dev/smoke-test.ps1"
}

# ---- 1. present_core.dll (mock-tolerant: warns, keeps going) ----
if (-not (Test-Path core/thirdparty/whisper.cpp/CMakeLists.txt)) {
  Write-Warning 'whisper.cpp submodule missing: building with MOCK STT (re-run with -Setup for real transcription).'
}
if (-not (Test-Path core/thirdparty/llama.cpp/CMakeLists.txt)) {
  Write-Warning 'llama.cpp submodule missing: building with MOCK LLM (re-run with -Setup for real summaries).'
}
if (Get-Command cmake -ErrorAction SilentlyContinue) {
  # Vulkan SDK installers set machine-wide VULKAN_SDK, but this process may
  # predate it — resolve fresh (machine env, then default location) and
  # export for the cmake child so FindVulkan succeeds in-process.
  # core/CMakeLists decides GPU vs CPU from what it actually finds.
  $vulkanSdk = [Environment]::GetEnvironmentVariable('VULKAN_SDK', 'Machine')
  if (-not $vulkanSdk) {
    $vkRoot = Get-ChildItem 'C:\VulkanSDK' -Directory -ErrorAction SilentlyContinue |
      Sort-Object Name -Descending | Select-Object -First 1
    if ($vkRoot) { $vulkanSdk = $vkRoot.FullName }
  }
  if ($vulkanSdk -and (Test-Path $vulkanSdk)) { $env:VULKAN_SDK = $vulkanSdk }
  cmake -S . -B core/build -A x64
  cmake --build core/build --config Release
} else {
  Write-Warning 'cmake not found; skipping core DLL build. GUI will run with mock backends.'
}

# ---- 2. Stage the DLL beside the app binary (loaded via libloading) ----
$dll = Get-ChildItem core/build -Recurse -Filter present_core.dll -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
if ($dll) {
  New-Item -ItemType Directory -Force ffmpeg-dlls | Out-Null
  Copy-Item $dll ffmpeg-dlls/present_core.dll -Force
  # Backend runtime: whisper/llama/ggml link SHARED on Windows, so the
  # Windows loader needs their DLLs beside present_core.dll too.
  Get-ChildItem core/build/bin/Release/*.dll -ErrorAction SilentlyContinue |
    Copy-Item -Destination ffmpeg-dlls/ -Force
} else {
  Write-Warning 'present_core.dll not found after CMake build; the GUI will run without a backend.'
}
if (-not (Test-Path ffmpeg-dlls/ffmpeg.exe)) {
  Write-Warning 'ffmpeg-dlls/ffmpeg.exe missing: re-run with -Setup (decode will fail without it).'
}

# ---- 3. Rust workspace (GPUI app + backend lib) ----
    # No console window: app/src/main.rs sets #![windows_subsystem = "windows"],
    # so plain cargo build already targets the GUI subsystem (dev and release).
    # (cargo rustc flag-forwarding is avoided: this toolchain misparses the
    # forwarded -C link-args, and the attribute makes it redundant anyway.)
    if ($Dev) {
      cargo build --workspace
      cargo test -p pv-backend
      cargo test -p voice-mail --bins
      Write-Host 'Done. Binary: target/debug/voice-mail.exe (dev build, do not ship)'
    } else {
      # GUI subsystem comes from #![windows_subsystem = "windows"] in
      # app/src/main.rs (cargo rustc --workspace is invalid: cargo rustc
      # takes -p/--bin, not --workspace — and its -- flag forwarding
      # misparses -C link-args here, so plain cargo build is used).
      cargo build --workspace --release
      cargo test -p pv-backend --release
      cargo test -p voice-mail --bins --release
      Write-Host 'Done. Binary: target/release/voice-mail.exe'
    }

# ---- 3b. Dev staging: backend beside dev binaries ----
# `cargo run` / direct exe launches resolve the DLL, catalog, and ffmpeg
# from the exe dir — without these, dev runs report unknown hardware and no
# backend. Packaging (ffmpeg-dlls/) is unaffected.
foreach ($prof in @('debug', 'release')) {
  $bindir = "target/$prof"
  if ((Test-Path $bindir) -and $dll) {
    Copy-Item $dll "$bindir/present_core.dll" -Force
    Copy-Item config/models.json "$bindir/models.json" -Force
    if (Test-Path ffmpeg-dlls/ffmpeg.exe) { Copy-Item ffmpeg-dlls/ffmpeg.exe "$bindir/ffmpeg.exe" -Force }
    Get-ChildItem ffmpeg-dlls/*.dll -ErrorAction SilentlyContinue |
      Copy-Item -Destination "$bindir/" -Force
  }
}

# ---- 4. Installers (M5): NSIS per-machine exe + per-user MSI, program only ----
if ($Package) {
  # Pin gate (SECURITY.md / RELEASE.md): an update feed combined with empty
  # sha256 pins is a false promise — fail closed, never ship that combo.
  # Local iteration without pins still works (loud warning); populating pins
  # needs a networked dev box (measure-models.ps1 + Hub-OID diff).
  $unpinned = @(node -e "const m=require('./config/models.json');const e=[];for(const k of ['stt_models','llm_models'])for(const x of (m[k]||[]))if(!(x.sha256||'').trim())e.push(x.id);for(const x of (m.vision_models||[]))if(!(x.text_sha256||x.sha256||'').trim()||!(x.mmproj_sha256||x.sha256||'').trim())e.push(x.id);console.log(e.join(' '))" 2>$null | Out-String).Trim() -split '\s+' | Where-Object { $_ }
  if ($unpinned.Count -gt 0) {
    if ($env:PV_UPDATE_FEED) {
      throw "REFUSING to package: $($unpinned.Count) model pin(s) empty ($($unpinned -join ', ')) with PV_UPDATE_FEED set — populate via measure-models.ps1 + Hub-OID diff first (see RELEASE.md)."
    }
    Write-Warning "packaging with $($unpinned.Count) unpinned model(s) ($($unpinned -join ', ')): size-only enforcement. Never combine with PV_UPDATE_FEED (see RELEASE.md)."
  }
  Push-Location app
  cargo install cargo-packager --locked 2>$null
  cargo packager --release
  Pop-Location
  # Optional Authenticode signing: set PV_SIGN_PFX to a .pfx path
  # (PV_SIGN_PASSWORD when needed). Unsigned builds warn, never fail here.
  $pfx = $env:PV_SIGN_PFX
  if ($pfx -and (Test-Path $pfx) -and (Get-Command signtool -ErrorAction SilentlyContinue)) {
    $ts = 'http://timestamp.digicert.com'
    Get-ChildItem dist -Recurse -Include *.exe, *.msi -ErrorAction SilentlyContinue | ForEach-Object {
      if ($env:PV_SIGN_PASSWORD) { & signtool sign /f $pfx /p $env:PV_SIGN_PASSWORD /tr $ts /td sha256 /fd sha256 $_.FullName }
      else { & signtool sign /f $pfx /tr $ts /td sha256 /fd sha256 $_.FullName }
    }
    Write-Host 'Signed installers via signtool.'
  } else {
    Write-Warning 'unsigned build (set PV_SIGN_PFX to sign with Authenticode). SHA256SUMS below still lets users verify bytes.'
  }
  # Checksums over every packaged artifact (always; feeds the SBOM release).
  Get-ChildItem dist -Recurse -Include *.exe, *.msi -ErrorAction SilentlyContinue | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.FullName } | Set-Content dist/SHA256SUMS -Encoding ascii
  # SBOM (best-effort locally; CI generates + uploads it authoritatively).
  if (Get-Command cargo-cyclonedx -ErrorAction SilentlyContinue) {
    cargo cyclonedx --all --format json
    Write-Host 'SBOM under target/cyclonedx/.'
  } else {
    Write-Warning 'cargo-cyclonedx not installed; skipping local SBOM (CI still generates it).'
  }
  Write-Host 'Done. Installers under dist/ (nsis/*.exe, msi/*.msi) + dist/SHA256SUMS'
}
