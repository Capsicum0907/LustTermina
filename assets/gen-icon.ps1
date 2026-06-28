# LustTermina アイコン生成（Windows GDI+。追加インストール不要）。
# ダークな角丸地に、緑のプロンプト ">" ＋ アンダースコアのカーソル。
# 出力: icon.png(256) ＝ ウィンドウ用 / icon.ico(マルチサイズ) ＝ exe 埋め込み用。
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
$outDir = Split-Path -Parent $MyInvocation.MyCommand.Path

$bg     = [System.Drawing.Color]::FromArgb(255, 18, 20, 24)   # #121418
$border = [System.Drawing.Color]::FromArgb(255, 44, 48, 56)
$green  = [System.Drawing.Color]::FromArgb(255, 25, 195, 125) # #19C37D

function New-IconBitmap([int]$S) {
    $bmp = New-Object System.Drawing.Bitmap($S, $S, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    # 角丸の地
    $pad = [single]($S * 0.05)
    $rad = [single]($S * 0.20)
    $d   = $rad * 2
    $L = $pad; $T = $pad; $R = $S - $pad; $B = $S - $pad
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($L, $T, $d, $d, 180, 90)
    $path.AddArc($R - $d, $T, $d, $d, 270, 90)
    $path.AddArc($R - $d, $B - $d, $d, $d, 0, 90)
    $path.AddArc($L, $B - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush($bg)), $path)
    $bw = [single]([math]::Max(1.0, $S * 0.012))
    $g.DrawPath((New-Object System.Drawing.Pen($border, $bw)), $path)

    # プロンプト ">"
    $pen = New-Object System.Drawing.Pen($green, [single]($S * 0.10))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap   = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $p1 = New-Object System.Drawing.PointF([single]($S*0.31), [single]($S*0.34))
    $p2 = New-Object System.Drawing.PointF([single]($S*0.52), [single]($S*0.50))
    $p3 = New-Object System.Drawing.PointF([single]($S*0.31), [single]($S*0.66))
    $g.DrawLines($pen, [System.Drawing.PointF[]]@($p1, $p2, $p3))

    # カーソル（アンダースコア）
    $uw = [single]($S*0.21); $uh = [single]($S*0.075)
    $ux = [single]($S*0.55); $uy = [single]($S*0.595)
    $ur = $uh
    $up = New-Object System.Drawing.Drawing2D.GraphicsPath
    $up.AddArc($ux, $uy, $ur, $ur, 180, 90)
    $up.AddArc($ux+$uw-$ur, $uy, $ur, $ur, 270, 90)
    $up.AddArc($ux+$uw-$ur, $uy+$uh-$ur, $ur, $ur, 0, 90)
    $up.AddArc($ux, $uy+$uh-$ur, $ur, $ur, 90, 90)
    $up.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush($green)), $up)

    $g.Dispose()
    return $bmp
}

# PNG (256)
$png = New-IconBitmap 256
$png.Save((Join-Path $outDir 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$png.Dispose()

# ICO (マルチサイズ。各サイズを PNG で格納＝Vista+ 対応)
$sizes = @(256, 128, 64, 48, 32, 16)
$blobs = @()
foreach ($s in $sizes) {
    $b = New-IconBitmap $s
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $blobs += , ($ms.ToArray())
    $ms.Dispose(); $b.Dispose()
}
$fs = [System.IO.File]::Create((Join-Path $outDir 'icon.ico'))
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $blobs[$i].Length
    $wh = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$wh); $bw.Write([byte]$wh); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$len); $bw.Write([uint32]$offset)
    $offset += $len
}
foreach ($blob in $blobs) { $bw.Write($blob) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()
Write-Output ("wrote icon.png + icon.ico to " + $outDir)
