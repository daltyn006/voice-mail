#Requires -Version 7
# One-command updater for voice mail (standalone — needs no repo checkout).
# Resolves a GitHub release, downloads the installer + SHA256SUMS, verifies
# the hash, then installs machine-wide. Per-machine installers need admin:
# the script re-launches itself elevated when required.
#
#   pwsh install-update.ps1                          # latest release, MSI preferred
#   pwsh install-update.ps1 -CheckOnly               # report only, change nothing
#   pwsh install-update.ps1 -Tag v1.0.2              # a specific tag instead of latest
#   pwsh install-update.ps1 -CurrentVersion 1.0.1    # skip when already current
param(
  [string]$Feed = 'https://api.github.com/repos/daltyn006/voice-mail/releases/latest',
  [string]$Tag = '',
  [string]$CurrentVersion = '',
  [string]$OutDir = (Join-Path ([System.IO.Path]::GetTempPath()) 'voice-mail-update'),
  [switch]$CheckOnly
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

function Test-Admin {
  ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-Elevated {
  # Re-launch this script elevated, forwarding the original arguments.
  $args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath)
  if ($Feed) { $args += @('-Feed', $Feed) }
  if ($Tag) { $args += @('-Tag', $Tag) }
  if ($CurrentVersion) { $args += @('-CurrentVersion', $CurrentVersion) }
  if ($OutDir) { $args += @('-OutDir', $OutDir) }
  if ($CheckOnly) { $args += '-CheckOnly' }
  Start-Process pwsh -ArgumentList $args -Verb RunAs -Wait
  exit $LASTEXITCODE
}

# Resolve the release body (latest or a pinned tag).
$releaseUrl = if ($Tag) {
  "https://api.github.com/repos/daltyn006/voice-mail/releases/tags/$Tag"
} else {
  $Feed
}
try {
  $body = Invoke-RestMethod -Uri $releaseUrl -TimeoutSec 60 `
    -Headers @{ 'User-Agent' = 'voice-mail-install-update'; Accept = 'application/vnd.github+json' }
} catch {
  if ($_.Exception.Message -match '404') {
    throw 'no releases published yet (GitHub returned 404) — cut a tag first, then re-run (see RELEASE.md).'
  }
  throw "release feed unreachable: $($_.Exception.Message)"
}
$tag = $body.tag_name
if (-not $tag) { throw 'release feed has no tag_name' }
Write-Host "Latest release: $tag"
if ($CurrentVersion -and ($CurrentVersion.TrimStart('v') -eq $tag.TrimStart('v'))) {
  Write-Host "Already current ($CurrentVersion) — nothing to do."
  exit 0
}

# Pick installer: per-machine .msi first, NSIS .exe fallback (mirrors
# pv-backend::update::pick_installer). Plus the SHA256SUMS sidecar.
$msi = $null; $exe = $null; $sumsUrl = $null
foreach ($a in $body.assets) {
  $n = [string]$a.name
  if ($n -eq 'SHA256SUMS') { $sumsUrl = [string]$a.browser_download_url }
  elseif ($n.ToLower().EndsWith('.msi') -and -not $msi) { $msi = $a }
  elseif ($n.ToLower().EndsWith('.exe') -and -not $exe) { $exe = $a }
}
$asset = if ($msi) { $msi } elseif ($exe) { $exe } else { throw 'release has no .msi or .exe asset' }
$isMsi = ([string]$asset.name).ToLower().EndsWith('.msi')
Write-Host "Installer: $($asset.name)"
if ($CheckOnly) { exit 0 }
if (-not (Test-Admin)) {
  Write-Host 'Per-machine install needs admin — re-launching elevated…'
  Invoke-Elevated
}

# Download + verify (fail closed on any mismatch or missing sidecar).
New-Item -ItemType Directory -Force $OutDir | Out-Null
$file = Join-Path $OutDir ([string]$asset.name)
Invoke-WebRequest -Uri ([string]$asset.browser_download_url) -OutFile $file
if (-not $sumsUrl) { throw 'release has no SHA256SUMS asset — refusing to install unverified bytes' }
$sumsFile = Join-Path $OutDir 'SHA256SUMS'
Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsFile
$want = $null
$leaf = [string]$asset.name
foreach ($line in (Get-Content $sumsFile)) {
  $parts = ($line.Trim() -split '\s+', 2)
  # SHA256SUMS may list bare names or full paths — compare leaf names.
  if ($parts.Count -eq 2 -and (Split-Path $parts[1].Trim() -Leaf) -eq $leaf) { $want = $parts[0].ToLower() }
}
if (-not $want) { throw "SHA256SUMS has no entry for $leaf" }
$got = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
if ($got -ne $want) { throw "SHA256 MISMATCH for $([string]$asset.name): got $got, want $want" }
Write-Host 'SHA-256 verified.'

# Install quietly (per-machine MSI = major upgrade, never side-by-side).
if ($isMsi) {
  $log = Join-Path $OutDir 'msi-install.log'
  $p = Start-Process msiexec -ArgumentList @('/i', "`"$file`"", '/quiet', '/norestart', '/log', "`"$log`"") -Wait -PassThru
  if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 1641 -and $p.ExitCode -ne 3010) {
    throw "msiexec failed (exit $($p.ExitCode)) — see $log"
  }
  if ($p.ExitCode -eq 3010 -or $p.ExitCode -eq 1641) { Write-Warning 'installed — a reboot is required to finish.' }
} else {
  $p = Start-Process $file -ArgumentList @('/S') -Wait -PassThru
  if ($p.ExitCode -ne 0) { throw "NSIS installer failed (exit $($p.ExitCode))" }
}
Write-Host "Installed $($asset.name) ($tag)."
