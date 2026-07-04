# リリース手順

公開する exe には、**ローカルの絶対パス（ホームディレクトリ＝実ユーザー名を含む）を埋め込まない**。
素の `cargo build --release` は `strip = true` でシンボルこそ消えるが、依存クレートのソースパス
（`%USERPROFILE%\.cargo\registry\...`）がバイナリに残る。そのため公開用ビルドは必ずパスの
remap を付けて作る。

## ビルド

リポジトリ直下で：

```powershell
./scripts/build-release.ps1
```

このスクリプトは `RUSTFLAGS="--remap-path-prefix=$env:USERPROFILE=C:\Users\user"` を付けて
`cargo build --release` し、生成した `target/release/lust_termina.exe` に実ユーザー名が残って
いないか自己検査する（残っていれば失敗して停止＝取りこぼしを防ぐ）。

手で同じことをするなら（PowerShell）：

```powershell
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=C:\Users\user"; cargo build --release
Remove-Item Env:RUSTFLAGS
```

（`RUSTFLAGS` を変えると全再ビルドになるため数分かかる。）

## 公開

1. 上で作った `target/release/lust_termina.exe` を GitHub Releases にアップロードする。
2. バージョンタグ `vX.Y.Z` を push する（`git push origin main --tags` 済みなら不要）。

未署名のため初回起動時に SmartScreen 警告が出るのは既知（README の該当節を参照）。
