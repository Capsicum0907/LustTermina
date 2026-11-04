# リリース手順

公開する exe には、**ローカルの絶対パス（ホームディレクトリ＝実ユーザー名を含む）を埋め込まない**。
素の `cargo build --release` は `strip = true` でシンボルこそ消えるが、依存クレートのソースパス
（`%USERPROFILE%\.cargo\registry\...`）がバイナリに残る。そのため公開用ビルドは必ずパスの
remap を付けて作る。

加えて、公開ビルドは **C ランタイムを静的リンク**する（`+crt-static`）。既定の MSVC ターゲットは
`VCRUNTIME140.dll` 等を動的リンクするため、VC++ 再頒布可能パッケージ未導入のクリーンな Windows では
起動時に「VCRUNTIME140.dll が見つからない」で**起動すらできない**（開発機には VS 等の副産物で常駐して
いるため気づけない）。静的リンクで単一 exe として自己完結させる。`RUSTFLAGS` を立てると
`.cargo/config.toml` の rustflags は無視されるので、remap と crt-static は同じ `RUSTFLAGS` に並べる。

## ビルド

リポジトリ直下で：

```powershell
./scripts/build-release.ps1
```

このスクリプトは `RUSTFLAGS="--remap-path-prefix=$env:USERPROFILE=C:\Users\user -C target-feature=+crt-static"`
を付けて `cargo build --release` し、生成した `target/release/lust_termina.exe` に実ユーザー名が残って
いないか自己検査する（残っていれば失敗して停止＝取りこぼしを防ぐ）。

ホームの置き換えだけでは、依存クレートがビルド時に生成したファイルの場所（`target/release/build/...`）から
リポジトリまでの道筋（ホーム以下のフォルダ構成）が残る（v0.5.0 の準備中に glutin_wgl_sys で実際に
見つかった）。そのためリポジトリのフォルダ自体も `lust_termina` に置き換える（区切りが `\`・`/`・混在の
3形）。自己検査も、ユーザー名・コンピューター名・リポジトリまでの道筋を、UTF-8 と UTF-16 の両方で探す。

最後に、配布用の版付きファイル
`target/release/LustTermina-v<version>-windows-x64.exe` を生成する（版番号は Cargo.toml が唯一の正。
スクラブ済み exe のバイト同一コピーなので、これをそのまま公開に使う）。

手で同じことをするなら（PowerShell）：

```powershell
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=C:\Users\user -C target-feature=+crt-static"; cargo build --release
Remove-Item Env:RUSTFLAGS
```

（`RUSTFLAGS` を変えると全再ビルドになるため数分かかる。）

## 公開

1. 上で生成された `target/release/LustTermina-v<version>-windows-x64.exe` を GitHub Releases にアップロードする。
2. バージョンタグ `vX.Y.Z` を push する（`git push origin main --tags` 済みなら不要）。

未署名のため初回起動時に SmartScreen 警告が出るのは既知（README の該当節を参照）。
