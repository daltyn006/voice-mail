#Requires -Version 7
# Downloads default 1 STT + 1 LLM (config/models.json). Extras on demand: -All.
# Verifies exact byte size from the catalog after each download.
param([switch]$All)
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
New-Item -ItemType Directory -Force models | Out-Null
$want = @($cfg.defaults.stt_model, $cfg.defaults.llm_model)
$list = @($cfg.stt_models | Where-Object { $All -or ($want -contains $_.id) }) + @($cfg.llm_models | Where-Object { $All -or ($want -contains $_.id) })
foreach ($m in $list) {
  $dst = "models/$($m.file)"
  if (Test-Path $dst) {
    $size = (Get-Item $dst).Length
    if ($m.bytes -gt 0 -and $size -ne $m.bytes) {
      Write-Warning "$($m.file) size mismatch (got $size, want $($m.bytes)). Re-downloading..."
    } else { Write-Host "exists: $($m.file)"; continue }
  }
  Write-Host "downloading $($m.file) ($([math]::Round($m.bytes/1GB,2)) GB) ..."
  try {
    Invoke-WebRequest $m.url -OutFile $dst -ErrorAction Stop
  } catch {
    Write-Warning "download failed for $($m.file): $_"
    continue
  }
  $size = (Get-Item $dst).Length
  if ($m.bytes -gt 0 -and $size -ne $m.bytes) { throw "size mismatch after download: $($m.file)" }
}
Write-Host 'Models ready in ./models (1 STT + 1 LLM by default).'
