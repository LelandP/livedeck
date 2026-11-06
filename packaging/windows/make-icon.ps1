<#
.SYNOPSIS
  Draws the LiveDeck icon and writes packaging\windows\livedeck.ico (embedded in livedeck.exe by
  crates\app\build.rs) and assets\livedeck.png (the window icon, set in main.slint).

  The shapes are the ones in packaging\linux\io.github.livedeck.LiveDeck.svg, on the same 256x256
  grid. Keep the two in step when the logo changes. Each size is drawn from the shapes rather than
  scaled down from 256, so the small sizes stay sharp.

  Both outputs are committed; run this only after changing the logo:
    powershell -File packaging\windows\make-icon.ps1
#>
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$repo = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256

$tile = [Drawing.ColorTranslator]::FromHtml('#121022')
$violet = [Drawing.ColorTranslator]::FromHtml('#7b61ff')
$live = [Drawing.ColorTranslator]::FromHtml('#ff4d5e')

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $d = 2 * $r
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    $p
}

function New-IconPng([int]$size) {
    $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([Drawing.Color]::Transparent)
    $g.ScaleTransform($size / 256.0, $size / 256.0)

    $tileBrush = New-Object Drawing.SolidBrush $tile
    $violetBrush = New-Object Drawing.SolidBrush $violet
    $liveBrush = New-Object Drawing.SolidBrush $live

    $g.FillPath($tileBrush, (New-RoundRect 0 0 256 256 56))
    # D-pad: the vertical bar and the left arm...
    $g.FillPath($violetBrush, (New-RoundRect 100 42 56 172 16))
    $g.FillPath($violetBrush, (New-RoundRect 42 100 90 56 16))
    # ...and the right arm as a play arrow with rounded corners (fill plus a round-joined stroke).
    $arrow = New-Object Drawing.Drawing2D.GraphicsPath
    $arrow.AddPolygon([Drawing.PointF[]]@(
        (New-Object Drawing.PointF 156, 96),
        (New-Object Drawing.PointF 210, 128),
        (New-Object Drawing.PointF 156, 160)))
    $pen = New-Object Drawing.Pen $violet, 16
    $pen.LineJoin = [Drawing.Drawing2D.LineJoin]::Round
    $g.FillPath($violetBrush, $arrow)
    $g.DrawPath($pen, $arrow)
    # Centre of the D-pad and the live dot.
    $g.FillEllipse($tileBrush, 116, 116, 24, 24)
    $g.FillEllipse($liveBrush, 188, 46, 24, 24)

    $g.Dispose()
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    , $ms.ToArray()
}

$images = foreach ($s in $sizes) { , (New-IconPng $s) }

# ICO container with PNG entries (supported since Windows Vista): ICONDIR, one ICONDIRENTRY per
# size, then the images.
$ms = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $ms
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$images[$i].Length); $w.Write([uint32]$offset)
    $offset += $images[$i].Length
}
foreach ($img in $images) { $w.Write($img) }
$w.Flush()

[IO.File]::WriteAllBytes((Join-Path $repo 'packaging\windows\livedeck.ico'), $ms.ToArray())
[IO.File]::WriteAllBytes((Join-Path $repo 'assets\livedeck.png'), $images[-1])
Write-Host "Wrote packaging\windows\livedeck.ico ($($sizes -join ', ') px) and assets\livedeck.png"
