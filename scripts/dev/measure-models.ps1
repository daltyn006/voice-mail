#Requires -Version 7
# Dev-only: re-pins config/models.json `bytes` (+ `sha256` when online) from
# local ./models files. Run after fetch-models.ps1 if upstream files ever change.
# Hash pinning (user-approved policy): the pinned value is the HF LFS OID
# (== file SHA-256). When online, this script also records local SHA-256s so a
# human can diff them against the HF Hub "Files" page (lfs oid) before commit.
# Enforcement lives in pv-backend (download finish + boot re-verify); empty
# pins keep the legacy size-only behavior until populated.
$ErrorActionPreference='Stop'
# Anchor at the repo root (sentinel walk-up): works from any CWD, from scripts/
# or scripts/dev/, and stays put when scripts call each other (idempotent).
$dir = $PSScriptRoot
while ($dir -and -not (Test-Path (Join-Path $dir 'config/models.json'))) {
  $parent = Split-Path $dir -Parent
  if ($parent -eq $dir) { throw "repo root not found above $PSScriptRoot" }
  $dir = $parent
}
Set-Location $dir
$cfg = Get-Content config/models.json | ConvertFrom-Json
foreach ($m in @($cfg.stt_models) + @($cfg.llm_models)) {
  $p = "models/$($m.file)"
  if (Test-Path $p) {
    $m.bytes = (Get-Item $p).Length
    $m.sha256 = (Get-FileHash $p -Algorithm SHA256).Hash.ToLower()
    Write-Host "$($m.id): $($m.bytes) sha256=$($m.sha256)"
  } else { Write-Warning "missing locally, keeping catalog value: $($m.file)" }
}
foreach ($m in @($cfg.vision_models)) {
  foreach ($k in @(@('text_file','text_bytes','text_sha256'), @('mmproj_file','mmproj_bytes','mmproj_sha256'))) {
    $p = "models/$($m.($k[0]))"
    if (Test-Path $p) {
      $m.($k[1]) = (Get-Item $p).Length
      $m.($k[2]) = (Get-FileHash $p -Algorithm SHA256).Hash.ToLower()
      Write-Host "$($m.id)/$($k[0]): $($m.($k[1]))"
    } else { Write-Warning "missing locally, keeping catalog value: $($m.($k[0]))" }
  }
}
$cfg | ConvertTo-Json -Depth 6 | Set-Content config/models.json -Encoding UTF8
Write-Host 'models.json bytes+sha256 re-pinned. VERIFY the hashes against the HF Hub Files page (lfs oid) before committing.'
