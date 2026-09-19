# Rebuilds the program icon set from assets/voice_mail.jpg.
#
# - Crops the envelope art (drops the mockup's white/checker margins),
#   center-crops to a square, resizes with high-quality bicubic.
# - Writes assets/voice-mail.png (256px, used by the packager).
# - Writes assets/voice-mail.ico as a multi-size Vista-style icon with
#   PNG-compressed entries: 16/32/48/256 (taskbar, window chrome,
#   Explorer — 256 is the ICO standard max).
#
# Idempotent: outputs are overwritten in place. Usage:
#   pwsh scripts/make-icon.ps1
param(
  [string]$SrcPath = (Join-Path $PSScriptRoot '..' 'assets' 'voice_mail.jpg')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$assetsDir = Split-Path -Parent $SrcPath
$pngOut = Join-Path $assetsDir 'voice-mail.png'
$icoOut = Join-Path $assetsDir 'voice-mail.ico'

# Envelope art region in the 1024px source (excludes the mockup margins
# and the envelope's own outer border so no checker or frame lines remain).
$cropX, $cropY, $cropW, $cropH = 150, 290, 725, 625

$srcImg = [System.Drawing.Image]::FromFile($SrcPath)
try {
  # Square crop of the art region.
  $side = [Math]::Min($cropW, $cropH)
  $cx = $cropX + [int](($cropW - $side) / 2)
  $cy = $cropY + [int](($cropH - $side) / 2)
  $square = New-Object System.Drawing.Bitmap($side, $side)
  $g = [System.Drawing.Graphics]::FromImage($square)
  try {
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($srcImg, (New-Object System.Drawing.Rectangle(0, 0, $side, $side)),
      (New-Object System.Drawing.Rectangle($cx, $cy, $side, $side)),
      [System.Drawing.GraphicsUnit]::Pixel)
  } finally { $g.Dispose() }

  $sizes = @(16, 32, 48, 256)
  $pngs = @()
  foreach ($s in $sizes) {
    $bmp = New-Object System.Drawing.Bitmap($s, $s)
    $g2 = [System.Drawing.Graphics]::FromImage($bmp)
    try {
      $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
      $g2.DrawImage($square, 0, 0, $s, $s)
    } finally { $g2.Dispose() }
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $pngs += ,@($s, $ms.ToArray())
    $ms.Dispose()
    if ($s -eq 256) {
      [System.IO.File]::WriteAllBytes($pngOut, $pngs[-1][1])
    }
  }
  $square.Dispose()

  # ICONDIR + entries + PNG payloads (width/height byte 0 means 256).
  $fs = [System.IO.File]::Create($icoOut)
  try {
    $bw = New-Object System.IO.BinaryWriter($fs)
    $bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$pngs.Count)
    $offset = 6 + 16 * $pngs.Count
    foreach ($e in $pngs) {
      $size = $e[0]; $data = $e[1]
      $b = if ($size -ge 256) { 0 } else { $size }
      $bw.Write([byte]$b); $bw.Write([byte]$b)
      $bw.Write([byte]0); $bw.Write([byte]0)
      $bw.Write([uint16]1); $bw.Write([uint16]32)
      $bw.Write([uint32]$data.Length); $bw.Write([uint32]$offset)
      $offset += $data.Length
    }
    foreach ($e in $pngs) { $bw.Write($e[1]) }
    $bw.Flush()
  } finally { $fs.Close() }
} finally { $srcImg.Dispose() }

Write-Host "Wrote $pngOut and $icoOut"
