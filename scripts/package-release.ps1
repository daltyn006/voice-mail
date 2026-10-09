#Requires -Version 7
# Release bundling for voice mail: version lockstep -> pins report ->
# build.ps1 -Package -> dist verification (exe + msi + SHA256SUMS re-hash).
# Single entry point for cutting a release bundle; everything heavy lives in
# scripts/build.ps1, this file only gates and verifies.
#
#   pwsh scripts/package-release.ps1              # bundle without update feed
#   pwsh scripts/package-release.ps1 -WithFeed    # + bake PV_UPDATE_FEED (refuses with empty pins)
#   pwsh scripts/package-release.ps1 -SkipTests   # CI already ran them
param(
  [switch]$WithFeed,
  [switch]$SkipTests
)
$ErrorActionPreference = 'Stop'
# Fail fast on native-command failure too (cargo/node non-zero exits
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

# ---- 0. Version lockstep (RELEASE.md: app + backend + packager move together).
function Get-FirstVersion([string]$path) {
  $line = Select-String -Path $path -Pattern '^version = "([^"]+)"' | Select-Object -First 1
  if (-not $line) { throw "no version found in $path" }
  $line.Matches[0].Groups[1].Value
}
$appVer = Get-FirstVersion 'app/Cargo.toml'
$beVer = Get-FirstVersion 'pv-backend/Cargo.toml'
$inPackager = $false
$pkgVer = $null
foreach ($line in (Get-Content 'app/Cargo.toml')) {
  if ($line.Trim() -eq '[package.metadata.packager]') { $inPackager = $true; continue }
  if ($inPackager -and ($line -match '^version = "([^"]+)"')) { $pkgVer = $Matches[1]; break }
  if ($inPackager -and $line.Trim().StartsWith('[')) { break }
}
if (-not $pkgVer) { throw 'no version found in [package.metadata.packager]' }
if (($appVer -ne $beVer) -or ($appVer -ne $pkgVer)) {
  throw "version drift: app=$appVer backend=$beVer packager=$pkgVer — bump all three (see RELEASE.md)"
}
Write-Host "Version lockstep: $appVer"

# ---- 1. Pins report (informational here; feed+empty-pins fails closed
# inside build.ps1 per the packaging gate — populate via measure-models.ps1
# + Hub-OID diff before any feed-backed release).
$unpinned = @(node -e "const m=require('./config/models.json');const e=[];for(const k of ['stt_models','llm_models'])for(const x of (m[k]||[]))if(!(x.sha256||'').trim())e.push(x.id);for(const x of (m.vision_models||[]))if(!(x.text_sha256||x.sha256||'').trim()||!(x.mmproj_sha256||x.sha256||'').trim())e.push(x.id);console.log(e.join(' '))" 2>$null | Out-String).Trim() -split '\s+' | Where-Object { $_ }
if ($unpinned.Count -gt 0) {
  Write-Warning "unpinned models ($($unpinned.Count)): $($unpinned -join ', ') — size-only enforcement."
  if ($WithFeed) {
    throw "REFUSING feed-backed bundle with empty pins — populate via measure-models.ps1 + Hub-OID diff first (see RELEASE.md)."
  }
} else {
  Write-Host 'All model pins populated.'
}

# ---- 2. Feed (baked at compile time via option_env! — must be set before
# cargo runs inside build.ps1, which inherits this process env).
if ($WithFeed) {
  $env:PV_UPDATE_FEED = 'https://api.github.com/repos/daltyn006/voice-mail/releases/latest'
  Write-Host "PV_UPDATE_FEED=$env:PV_UPDATE_FEED"
} else {
  Write-Warning 'no update feed baked in: Settings → Check for updates will report "not configured". Pass -WithFeed for feed-backed releases.'
}

# ---- 3. Build + package.
$buildArgs = @()
if ($SkipTests) { $buildArgs += '-SkipTests' }
& "$PSScriptRoot/build.ps1" -Package @buildArgs

# ---- 4. Verify dist (fail closed: a half-written bundle must never pass).
$exe = Get-ChildItem dist -Recurse -Filter *.exe -ErrorAction SilentlyContinue | Select-Object -First 1
$msi = Get-ChildItem dist -Recurse -Filter *.msi -ErrorAction SilentlyContinue | Select-Object -First 1
$sums = Join-Path 'dist' 'SHA256SUMS'
foreach ($f in @($exe, $msi)) {
  if (-not $f -or $f.Length -eq 0) { throw "dist verification failed: missing or empty package artifact" }
}
if (-not (Test-Path $sums)) { throw 'dist verification failed: dist/SHA256SUMS missing' }
$bad = 0
foreach ($line in (Get-Content $sums)) {
  if ($line.Trim() -eq '') { continue }
  $parts = $line -split '\s+', 2
  if ($parts.Count -ne 2) { Write-Warning "SHA256SUMS: skipping malformed line: $line"; $bad++; continue }
  $want, $rel = $parts[0].ToLower(), $parts[1].Trim()
  $full = Join-Path 'dist' (Split-Path $rel -Leaf)
  if (-not (Test-Path $full)) { $full = $rel }
  if (-not (Test-Path $full)) { Write-Warning "SHA256SUMS: file not found: $rel"; $bad++; continue }
  $got = (Get-FileHash $full -Algorithm SHA256).Hash.ToLower()
  if ($got -ne $want) { Write-Warning "SHA256SUMS: MISMATCH for $rel"; $bad++ }
}
if ($bad -gt 0) { throw "dist verification failed: $bad SHA256SUMS problem(s)" }
Write-Host 'dist verification: hashes match.'

# ---- 5. Summary.
Write-Host "Bundle complete (v$appVer):"
Get-ChildItem dist -Recurse -Include *.exe, *.msi | ForEach-Object { Write-Host "  $($_.Name)  $([math]::Round($_.Length / 1MB, 1)) MB" }
if (Test-Path ffmpeg-dlls/build-info.json) {
  Write-Host "Backend provenance: $(Get-Content ffmpeg-dlls/build-info.json -Raw)".Trim()
}
