# Draws Glide's mark (an island hanging from the top edge of a dark tile) and
# writes assets\glide.ico (PNG images at 16..256 px) and the installer's small
# wizard image. Run again after changing the drawing.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$assets = Join-Path $root 'assets'
New-Item -ItemType Directory -Force $assets | Out-Null

function Draw-Mark([int]$size, [System.Drawing.Color]$background) {
    $bitmap = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0
    # The tile: a rounded square.
    $tile = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = 56 * $s; $m = 8 * $s; $w = $size - 2 * $m
    $tile.AddArc($m, $m, 2 * $r, 2 * $r, 180, 90)
    $tile.AddArc($m + $w - 2 * $r, $m, 2 * $r, 2 * $r, 270, 90)
    $tile.AddArc($m + $w - 2 * $r, $m + $w - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $tile.AddArc($m, $m + $w - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $tile.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush $background), $tile)
    # The island: hangs from the tile's top edge with soft shoulders.
    $island = New-Object System.Drawing.Drawing2D.GraphicsPath
    $cx = $size / 2; $half = 62 * $s; $h = 70 * $s; $f = 22 * $s; $cr = 34 * $s
    $top = $m
    $island.AddBezier($cx - $half - $f, $top, $cx - $half, $top, $cx - $half, $top + $f * 0.5, $cx - $half, $top + $f)
    $island.AddLine($cx - $half, $top + $f, $cx - $half, $top + $h - $cr)
    $island.AddArc($cx - $half, $top + $h - 2 * $cr, 2 * $cr, 2 * $cr, 180, -90)
    $island.AddLine($cx - $half + $cr, $top + $h, $cx + $half - $cr, $top + $h)
    $island.AddArc($cx + $half - 2 * $cr, $top + $h - 2 * $cr, 2 * $cr, 2 * $cr, 90, -90)
    $island.AddLine($cx + $half, $top + $h - $cr, $cx + $half, $top + $f)
    $island.AddBezier($cx + $half, $top + $f, $cx + $half, $top + $f * 0.5, $cx + $half, $top, $cx + $half + $f, $top)
    $island.CloseFigure()
    $g.SetClip($tile)
    $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0, 0, 0))), $island)
    # A thin lit line inside the island, in Glide's default accent.
    $accent = [System.Drawing.Color]::FromArgb(255, 74, 168, 255)
    $line = 10 * $s
    $g.FillRectangle((New-Object System.Drawing.SolidBrush $accent), [float]($cx - 34 * $s), [float]($top + $h * 0.52), [float](68 * $s), [float]$line)
    $g.Dispose()
    return $bitmap
}

$tileColor = [System.Drawing.Color]::FromArgb(255, 28, 28, 32)
$images = foreach ($size in 16, 24, 32, 48, 64, 256) {
    $bitmap = Draw-Mark $size $tileColor
    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    , @($size, $stream.ToArray())
}

# ICO container: header, one directory entry per image, then the PNG data.
$out = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $out
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($image in $images) {
    $size = $image[0]; $data = $image[1]
    $dim = if ($size -ge 256) { 0 } else { $size }
    $writer.Write([byte]$dim); $writer.Write([byte]$dim); $writer.Write([byte]0); $writer.Write([byte]0)
    $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$data.Length); $writer.Write([uint32]$offset)
    $offset += $data.Length
}
foreach ($image in $images) { $writer.Write([byte[]]$image[1]) }
$writer.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets 'glide.ico'), $out.ToArray())

# The installer's small image (shown at the top right while installing).
$small = Draw-Mark 110 ([System.Drawing.Color]::FromArgb(255, 28, 28, 32))
$canvas = New-Object System.Drawing.Bitmap 110, 110
$g = [System.Drawing.Graphics]::FromImage($canvas); $g.Clear([System.Drawing.Color]::White); $g.DrawImage($small, 0, 0); $g.Dispose()
$canvas.Save((Join-Path $assets 'setup-small.bmp'), [System.Drawing.Imaging.ImageFormat]::Bmp)
"Wrote $(Join-Path $assets 'glide.ico') and setup-small.bmp"
