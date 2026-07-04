# 公開用の release exe をビルドする。
#
# 素の `cargo build --release` は strip でシンボルは消えるが、依存クレートのソースパス
# （%USERPROFILE%\.cargo\registry\... ＝実ユーザー名を含む）がバイナリに残る。公開する exe に
# ローカルの絶対パスを埋め込まないよう、ここでは --remap-path-prefix でホーム配下のパスを
# 中立なプレースホルダへ置き換えてビルドし、最後に実ユーザー名が残っていないか自己検査する。
#
# 使い方（リポジトリ直下で）:  ./scripts/build-release.ps1
#
# 注意: このスクリプトに実ユーザー名は書かれていない。$env:USERPROFILE / $env:USERNAME を
# ビルド時に環境から解決するだけなので、そのままコミットしてよい。

$ErrorActionPreference = 'Stop'

$userHome = $env:USERPROFILE
$exe = 'target/release/lust_termina.exe'

Write-Host "Building release with path remap ($userHome -> C:\Users\user) ..."
$env:RUSTFLAGS = "--remap-path-prefix=$userHome=C:\Users\user"
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
}

# 実ユーザー名がバイナリに残っていたら失敗させる（握り潰さない）。
$name = $env:USERNAME
& findstr /C:"$name" $exe > $null 2>&1
if ($LASTEXITCODE -eq 0) {
    Write-Error "Path scrub FAILED: '$name' is still present in $exe. Do NOT publish this build."
    exit 1
}

# findstr は「見つからない」で exit 1 を返す（＝望ましい）。その $LASTEXITCODE が残るので、
# 成功終了を明示する。
Write-Host "OK: $exe built and scrubbed (no '$name' found). Safe to publish."
exit 0
