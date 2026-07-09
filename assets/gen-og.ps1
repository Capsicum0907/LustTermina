# LustTermina OGP / social card generator (Windows GDI+, no extra installs).
# 16:9. Centered [app icon + "LustTermina" wordmark] on a dark field with a
# soft purple glow radiating from behind the logo. BOOTH center-crops to 1:1
# and X shows the full 16:9 - the centered logo survives both.
# Output: assets/og-image.png (1280x720).
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
$dir     = Split-Path -Parent $MyInvocation.MyCommand.Path
$iconPng = Join-Path $dir 'icon.png'
$outPng  = Join-Path $dir 'og-image.png'

$W = 1280; $H = 720
$cx = $W / 2; $cy = $H / 2

$bg    = [System.Drawing.Color]::FromArgb(255, 12, 10, 18)    # #0C0A12 dark, faint purple
$text  = [System.Drawing.Color]::FromArgb(255, 244, 242, 250) # near-white, slight cool
$glowR = 150; $glowG = 90; $glowB = 255                        # #965AFF violet

$bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
$g.Clear($bg)

# --- purple radial glow (layered: wide halo + brighter core) ---
function Add-Glow([single]$rx, [single]$ry, [int]$centerA) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddEllipse($cx - $rx, $cy - $ry, 2 * $rx, 2 * $ry)
    $pgb = New-Object System.Drawing.Drawing2D.PathGradientBrush($path)
    $pgb.CenterPoint = New-Object System.Drawing.PointF([single]$cx, [single]$cy)
    $pgb.CenterColor = [System.Drawing.Color]::FromArgb($centerA, $glowR, $glowG, $glowB)
    $pgb.SurroundColors = @([System.Drawing.Color]::FromArgb(0, $glowR, $glowG, $glowB))
    $g.FillPath($pgb, $path)
    $pgb.Dispose(); $path.Dispose()
}
# stacked faint linear glows -> smooth, gentle "じわっと" bleed (no hard ring)
Add-Glow 880 700 26    # broad faint base
Add-Glow 620 500 28    # mid spread
Add-Glow 430 350 32    # soft dim core

# --- font (fallback chain) ---
function New-WordFont([single]$px) {
    foreach ($fam in @('Segoe UI Semibold','Segoe UI','Arial')) {
        try { return New-Object System.Drawing.Font($fam, $px, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel) } catch {}
    }
    return New-Object System.Drawing.Font([System.Drawing.FontFamily]::GenericSansSerif, $px, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
}
$font = New-WordFont 76
$label = 'LustTermina'
$sz = $g.MeasureString($label, $font)

$iconS = 240; $gap = 34
$groupH = $iconS + $gap + [int]$sz.Height
$top = [int](($H - $groupH) / 2)

$iconImg = [System.Drawing.Image]::FromFile($iconPng)
$ix = [int](($W - $iconS) / 2)
$g.DrawImage($iconImg, $ix, $top, $iconS, $iconS)

$tx = ($W - $sz.Width) / 2
$ty = $top + $iconS + $gap
$g.DrawString($label, $font, (New-Object System.Drawing.SolidBrush($text)), [single]$tx, [single]$ty)

$g.Dispose()
$bmp.Save($outPng, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose(); $iconImg.Dispose()
Write-Output ("wrote " + $outPng)
