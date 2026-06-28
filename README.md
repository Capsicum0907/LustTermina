# LustTermina（ラストテルミナ）

Windows 向けの自作ターミナルエミュレータ。**Rust + egui**。

`pwsh` 等のシェルを ConPTY 経由で起動し、VT/ANSI を解釈してグリッドを GPU 描画する。縦タブ・スクロールバック・設定の TOML 永続化を備える。

## できること
- **PTY 接続**：`portable-pty`(ConPTY) でシェルを起動。DSR（`ESC[6n`）等の問い合わせに応答（EventListener → pty 書き戻し）。
- **VT/ANSI**：`alacritty_terminal` が解釈し画面グリッドを保持。
- **描画**：egui で等幅グリフを**色付き**描画（16色 + 256色 + truecolor）。**per-cell 背景色** ＋ 属性（**反転 / bold / dim / 下線 / 取り消し線**）反映。
- **入力**：文字・Enter・Backspace・Tab・矢印・Home/End・Ctrl+英字、**IME 確定（日本語）**、**ペースト（Ctrl+V）**、**Ctrl+C = 割り込み**、**Ctrl+Backspace = 単語削除**。
- **マウス選択 → コピー**：ドラッグで範囲選択、**Ctrl+C** でクリップボードへ。
- **日本語表示**：MS Gothic（`msgothic.ttc`）を CJK フォールバックに登録。
- **縦タブ**：左（または右）サイドパネルに session 一覧。＋で新規タブ、×は**確認ダイアログ**経由で閉じる、クリックで切替、**ダブルクリックで改名**。**ドラッグで並べ替え**（端でオートスクロール）。1タブ = (PTY + Term)。シェル種別ごとのアイコン表示。**タブごとに背景色・アクセントカラー**を設定可。
- **スクロールバック**：マウスホイールで履歴（10000 行）を遡り、入力で最下部＝ライブ表示へ戻る。
- **ウィンドウのリサイズに追従**（列数・行数を再計算して term と pty をリサイズ）。
- **設定画面**：既定シェル・起動 cwd・テーマ・タブパネルの左右配置。**起動タブ（startup tabs）**を GUI カードで編集（タブごとに name/shell/cwd/背景色/アクセント/起動コマンド。spawn 後に各行を pty へ流す）。
- **永続化**：`%APPDATA%\LustTermina\config.toml`（serde + toml）。既定シェルは環境非依存に名前で保存、色は `#RRGGBB`。
- **独自アイコン**（ウィンドウ／タスクバー ＋ exe 埋め込み）。`assets/gen-icon.ps1`（GDI で生成）→ `icon.png`(ウィンドウ) / `icon.ico`(exe, `build.rs` で埋め込み)。

## まだ
選択範囲のスクロール追従 / CJK 全角幅の厳密化（描画はされるが幅計算は素朴）/ イタリック字面（ITALIC 属性は取るが既定フォントに斜体面が無い）/ マウスレポート / 差分描画（性能）/ IME インライン変換（今は候補窓のみ）。

## 構成
```
shell ⇄ portable-pty(ConPTY) ⇄ alacritty_terminal(グリッド) → egui 描画
                                            ↑ 入力: egui → encode → pty
```
- `src/main.rs` … 本体（1 ファイル）。
- PTY 読み取りは別スレッド → `Arc<Mutex<Term>>` を更新 → `ctx.request_repaint()`。

## 依存
- `eframe` 0.35（egui 同梱、wgpu バックエンド）
- `portable-pty` 0.9（ConPTY）
- `alacritty_terminal` 0.26（VT/グリッド。`vte` 同梱）

## ビルド / 実行
開発時：
```
cargo run
```
（初回は依存クレートのコンパイルで数分。以降は数秒。）

日常使い（ダブルクリック起動）：
```
cargo build --release
```
で `target/release/lust_termina.exe` を作り、それを指すショートカットを置く。`windows_subsystem = "windows"` 指定済みなのでコンソール窓は出ない。WorkingDirectory をホームにするとシェルがホーム始まりで開く。

## 名前について
**L**ust ×（**R**ust）の L/R 引っかけ ＋ **Termina**（terminus＝終端）。カナ「ラスト」は Lust / Rust / Last の三重掛け。
