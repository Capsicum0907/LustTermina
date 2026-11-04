# 公開用の release exe をビルドする。
#
# 素の `cargo build --release` は strip でシンボルは消えるが、依存クレートのソースパス
# （%USERPROFILE%\.cargo\registry\... ＝実ユーザー名を含む）がバイナリに残る。公開する exe に
# ローカルの絶対パスを埋め込まないよう、ここでは --remap-path-prefix でホーム配下のパスを
# 中立なプレースホルダへ置き換えてビルドし、最後に実ユーザー名が残っていないか自己検査する。
#
# さらに +crt-static で C ランタイム（VCRUNTIME140.dll 等）を静的リンクする。既定の MSVC ターゲット
# は CRT を動的リンクするため、VC++ 再頒布可能パッケージ未導入のクリーンな Windows では
# 「VCRUNTIME140.dll が見つからない」で起動すらできない。静的リンクで単一 exe として自己完結させる。
# 注意: RUSTFLAGS 環境変数を立てると .cargo/config.toml の rustflags は無視されるため、
# remap と crt-static は同じ RUSTFLAGS にまとめて渡す必要がある。
#
# 使い方（リポジトリ直下で）:  ./scripts/build-release.ps1
#
# 注意: このスクリプトに実ユーザー名は書かれていない。$env:USERPROFILE / $env:USERNAME を
# ビルド時に環境から解決するだけなので、そのままコミットしてよい。

$ErrorActionPreference = 'Stop'

# どこから起動されても（エクスプローラのダブルクリック含む）リポジトリ直下を基準にする。
# ダブルクリックだと CWD がスクリプト位置等になり、相対パス（target/release/...）がズレて
# スクラブ検査・版付きコピーが空振りする（cargo build 自体は Cargo.toml 上方探索で通るため
# lust_termina.exe だけ更新される）。$PSScriptRoot（=scripts/）の親へ移動して固定する。
Set-Location (Join-Path $PSScriptRoot '..')

$userHome = $env:USERPROFILE
$repo = (Get-Location).Path
$repoAlias = 'lust_termina'
$exe = 'target/release/lust_termina.exe'

$repoForms = @($repo, ($repo -replace '\\', '/'))
if ($repo.StartsWith($userHome, [StringComparison]::OrdinalIgnoreCase)) {
    $repoForms += ($userHome -replace '\\', '/') + $repo.Substring($userHome.Length)
}
$remaps = @("--remap-path-prefix=$userHome=C:\Users\user") +
    ($repoForms | Select-Object -Unique | ForEach-Object { "--remap-path-prefix=$_=$repoAlias" })

Write-Host "Building release with path remap (home -> C:\Users\user, repo -> $repoAlias) and static CRT ..."
$env:RUSTFLAGS = (@($remaps) + '-C target-feature=+crt-static') -join ' '
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
}

$needles = @($env:USERNAME, $env:COMPUTERNAME)
if ($repo.StartsWith($userHome, [StringComparison]::OrdinalIgnoreCase)) {
    $rel = $repo.Substring($userHome.Length).TrimStart('\')
    $needles += $rel, ($rel -replace '\\', '/')
}
$text = [System.Text.Encoding]::Latin1.GetString([System.IO.File]::ReadAllBytes((Resolve-Path $exe)))
$leaks = foreach ($n in ($needles | Where-Object { $_ })) {
    $wide = ($n.ToCharArray() | ForEach-Object { "$_`0" }) -join ''
    if ($text.IndexOf($n, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
        $text.IndexOf($wide, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $n }
}
if ($leaks) {
    Write-Error "Path scrub FAILED: $($leaks -join ', ') still present in $exe. Do NOT publish this build."
    exit 1
}
Write-Host "OK: $exe built and scrubbed (user name, machine name and repository path not found)."

# 配布用の版付きファイル名を生成する。版番号は Cargo.toml の [package] version を唯一の正とし、
# 手打ちのリネーム（タイプミスの元）を廃す。生成物はスクラブ済み exe のバイト同一コピー。
# 版は Cargo.toml を直接正規表現で読む（cargo metadata + ConvertFrom-Json は Windows PowerShell 5.1
# のネイティブ出力エンコーディングで壊れて失敗しうるため。ファイル先頭の最初の version = "..." が
# [package] のもので、依存の version より前に現れる）。
$verLine = Select-String -Path 'Cargo.toml' -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
$version = if ($verLine) { $verLine.Matches[0].Groups[1].Value } else { $null }
if (-not $version) { Write-Error "Could not read package version from Cargo.toml"; exit 1 }
$dist = "target/release/LustTermina-v$version-windows-x64.exe"
Copy-Item $exe $dist -Force
Write-Host "Packaged: $dist"
Write-Host "Upload this file to GitHub Releases. Safe to publish."
exit 0
