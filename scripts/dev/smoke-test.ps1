#Requires -Version 7
# Runs the no-toolchain smoke test (Node only).
$ErrorActionPreference = 'Stop'
# Anchor at the repo root (sentinel walk-up): works from any CWD, from scripts/
# or scripts/dev/, and stays put when scripts call each other (idempotent).
$dir = $PSScriptRoot
while ($dir -and -not (Test-Path (Join-Path $dir 'config/models.json'))) {
  $parent = Split-Path $dir -Parent
  if ($parent -eq $dir) { throw "repo root not found above $PSScriptRoot" }
  $dir = $parent
}
Set-Location $dir
if (Get-Command node -ErrorAction SilentlyContinue) {
  node tests/smoke.mjs
} else {
  Write-Warning 'node not found; skipping smoke test.'
}
