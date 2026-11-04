//! 端末セッション（PTY ⇄ alacritty グリッド）と関連の型。
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Sense, Vec2};
use crate::input::*;
use crate::PAD;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use crate::color::*;
use crate::shells::ShellSpec;
use crate::logging::TabLog;
use log::Level;

// ---- グリッド寸法（alacritty の Dimensions を満たす最小実装）----
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TermSize {
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

// ---- 配色（Campbell 系の 16 色 + 既定 fg/bg）----


// ---- Term → pty 書き戻し ----
// Term は DSR（カーソル位置問い合わせ ESC[6n）等への応答を Event::PtyWrite として出す。
// これを pty writer に流さないと、pwsh が応答待ちで固まってプロンプトが出ない。
#[derive(Clone)]
pub(crate) struct EventProxy {
    pub(crate) writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub(crate) log: Arc<TabLog>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            if let Ok(mut w) = self.writer.lock() {
                if let Err(e) = w.write_all(text.as_bytes()).and_then(|_| w.flush()) {
                    self.log.write_error(&e);
                }
            }
        }
    }
}

// 起動コマンドの投入ペース（出力アイドル・ゲーティング）の既定値。
// 本文/Enter を送るたびに「PTY 出力が quiet ミリ秒静まった」＝相手が入力を受け付けられる
// 状態、とみなして次を送る。素のシェル（cooked mode の行バッファ）でも claude 等の TUI
// （raw mode で1バイトずつ自前処理）でも同じロジックで成立させるための仕組み。出力が延々
// 続くケース（top 等）で固まらないよう max_wait で打ち切る。claude 等をプログラム名で特別
// 扱いはしない（ハードコード回避）。
// これらは既定値で、config.toml の startup_quiet_ms / startup_max_wait_ms で上書きできる
// （通信待ちで一時的に出力が止まるコマンドの取りこぼしは、quiet を上げて詰める）。
pub(crate) const STARTUP_QUIET_MS: u64 = 800;
pub(crate) const STARTUP_MAX_WAIT_MS: u64 = 8000;
// 起動リストの「プリセット参照」が presets に見つからないとき、タブ名の頭に付ける印。
// 黙って既定シェルにフォールバックせず、未解決だと気付ける形にする（silent fail 回避）。
pub(crate) const UNRESOLVED_PRESET_MARK: &str = "⚠";
pub(crate) fn default_startup_quiet_ms() -> u64 {
    STARTUP_QUIET_MS
}
pub(crate) fn default_startup_max_wait_ms() -> u64 {
    STARTUP_MAX_WAIT_MS
}

// ---- セッション（1タブ = PTY + Term + 読み取りスレッド）----
pub(crate) struct Session {
    pub(crate) term: Arc<Mutex<Term<EventProxy>>>,
    pub(crate) writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub(crate) size: TermSize,
    pub(crate) scroll_accum: f32, // ホイールの端数（行）を貯める
    pub(crate) title: String,
    pub(crate) bg: Color32,     // このタブの背景色（既定はテーマの本文色）
    pub(crate) accent: Color32, // このタブのアクセント色（既定は緑）
    pub(crate) shell_short: String, // 開いたシェルの short 名（起動タブのスナップショット用）
    pub(crate) cwd: String,         // 開いた初期ディレクトリ（同上。空=継承）
    // IME 未確定文字列（変換中の合成テキスト）。確定前は pty へ送らずグリッドにも無いので、
    // 描画時にカーソル位置へインライン表示するために保持する。確定/無効化で空に戻す。
    pub(crate) preedit: String,
    // マウスモーション報告(1002/1003)の重複抑制。同じセル上の移動は1回だけ報告する
    // （画素ごとに送るとアプリへ洪水になる）。直近に報告したセル(col,row)。
    pub(crate) last_mouse_cell: Option<(usize, usize)>,
    // PTY から最後に出力が来た時刻。リーダースレッドが読むたびスタンプする。
    // 起動コマンドの投入ペーサ（send_commands_paced）が「出力が静まったか」の判定に使う。
    pub(crate) last_output: Arc<Mutex<Instant>>,
    // スクリプト実行タブ用: 子プロセスが正常終了したらこのタブを自動で閉じる（run-and-close）。
    // 通常の対話シェルは false（残す）。失敗終了(非0)時は出力を残すため閉じない（app 側で判定）。
    pub(crate) close_on_exit: bool,
    pub(crate) child: Box<dyn portable_pty::Child + Send + Sync>,
    pub(crate) log: Arc<TabLog>,
    pub(crate) exit_logged: bool,
    logged_props: Option<(String, Color32, Color32)>,
}

impl Session {
    pub(crate) fn spawn(ctx: &egui::Context, title: String, spec: &ShellSpec, cwd: &str) -> Result<Self, String> {
        Self::spawn_with_env(ctx, title, spec, cwd, &[])
    }

    // 追加の環境変数付きでタブを開く版。--tab の系譜スタンプ（LUSTTERMINA_TAB_DEPTH）用。
    pub(crate) fn spawn_with_env(
        ctx: &egui::Context,
        title: String,
        spec: &ShellSpec,
        cwd: &str,
        extra_env: &[(String, String)],
    ) -> Result<Self, String> {
        let init = TermSize { cols: 80, rows: 24 };
        let log = TabLog::open();
        log.event(
            Level::Info,
            format_args!(
                "opening title={title:?} shell={} program={:?} args={:?} cwd={cwd:?} extra_env={:?}",
                spec.short,
                spec.program,
                spec.args,
                extra_env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
            ),
        );

        // PTY を開いて pwsh を起動
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: init.rows as u16,
                cols: init.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| spawn_failed(&log, "could not open a pseudo console", e))?;

        // シェルの起動定義（プログラム＋引数）は ShellSpec が持つ（探索結果）。
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if !cwd.is_empty() {
            cmd.cwd(cwd);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| spawn_failed(&log, &format!("could not start {}", spec.program), e))?;
        drop(pair.slave); // slave 側はもう要らない
        log.event(Level::Info, format_args!("spawned pid={:?}", child.process_id()));

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| spawn_failed(&log, "could not read from the pseudo console", e))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| spawn_failed(&log, "could not write to the pseudo console", e))?;
        let master: Box<dyn MasterPty + Send> = pair.master;

        // writer は UI 入力と Term 書き戻しの両方から使うので Arc<Mutex> に
        let writer = Arc::new(Mutex::new(writer));

        // ターミナルのグリッド（書き戻し用プロキシを渡す）
        let proxy = EventProxy {
            writer: writer.clone(),
            log: log.clone(),
        };
        let term = Term::new(Config::default(), &init, proxy);
        let term = Arc::new(Mutex::new(term));

        // 起動コマンド投入ペーサ用：最後に PTY 出力が来た時刻。
        let last_output = Arc::new(Mutex::new(Instant::now()));

        // PTY 読み取りスレッド: bytes → VT パーサ → グリッド更新 → 再描画要求
        {
            let term = term.clone();
            let ctx = ctx.clone();
            let last_output = last_output.clone();
            let mut reader = reader;
            let thread_log = log.clone();
            let spawned = thread::Builder::new().name(format!("pty-reader-{}", log.id())).spawn(move || {
                let log = thread_log;
                let mut parser: Processor = Processor::new();
                let mut buf = [0u8; 8192];
                let mut total: u64 = 0;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            log.event(Level::Info, format_args!("pty output ended (EOF) after {total} bytes"));
                            break;
                        }
                        Err(e) => {
                            log.event(Level::Warn, format_args!("pty read failed after {total} bytes: {e}"));
                            break;
                        }
                        Ok(n) => {
                            total += n as u64;
                            if let Ok(mut term) = term.lock() {
                                parser.advance(&mut *term, &buf[..n]);
                            }
                            if let Ok(mut t) = last_output.lock() {
                                *t = Instant::now();
                            }
                            ctx.request_repaint();
                        }
                    }
                }
            });
            if let Err(e) = spawned {
                log.event(Level::Error, format_args!("could not start the pty reader thread: {e}"));
            }
        }

        Ok(Self {
            term,
            writer,
            master: Arc::new(Mutex::new(master)),
            size: init,
            scroll_accum: 0.0,
            title,
            bg: DEFAULT_BG,
            accent: ACCENT_GREEN,
            shell_short: spec.short.clone(),
            cwd: cwd.to_string(),
            preedit: String::new(),
            last_mouse_cell: None,
            last_output,
            close_on_exit: false,
            child,
            log,
            exit_logged: false,
            logged_props: None,
        })
    }

    // 起動コマンド列を「出力が静まってから次」の順で投入する（出力アイドル・ゲーティング）。
    // 各コマンドで: 本文を書く → 出力が静まるまで待つ → Enter を単独で書く → また待つ。
    // 本文と Enter を時間的に分離することで、claude 等の TUI が連続入力を「ペースト」と
    // 誤認して Enter を送信でなく改行として食う問題を避ける。UI スレッドを止めないよう
    // 別スレッドで回す。settle 判定・上限は STARTUP_QUIET_MS / STARTUP_MAX_WAIT_MS。
    pub(crate) fn send_commands_paced(&self, commands: Vec<String>, quiet_ms: u64, max_wait_ms: u64) {
        if commands.is_empty() {
            return;
        }
        let writer = self.writer.clone();
        let last_output = self.last_output.clone();
        let log = self.log.clone();
        log.event(Level::Info, format_args!("sending {} startup command(s)", commands.len()));
        let thread_log = log.clone();
        let spawned = thread::Builder::new().name(format!("startup-cmds-{}", log.id())).spawn(move || {
            let log = thread_log;
            let quiet = Duration::from_millis(quiet_ms);
            let max_wait = Duration::from_millis(max_wait_ms);
            let poll = Duration::from_millis(20);
            // 「送信して良い状態」まで待つ。まず quiet だけ必ず空ける（直前の write と
            // 次の write を時間的に分離＝ペースト誤認回避。エコーが届く前に last_output が
            // 古いままで即 return してしまう競合も、この床で防ぐ）。その後、出力がまだ
            // 続いていれば静まるまで延長する（claude 起動時の描画バースト等）。max_wait で打ち切り。
            let wait_settle = || {
                thread::sleep(quiet);
                let deadline = Instant::now() + max_wait;
                loop {
                    let idle = last_output
                        .lock()
                        .map(|t| t.elapsed())
                        .unwrap_or(quiet)
                        >= quiet;
                    if idle {
                        return true;
                    }
                    if Instant::now() >= deadline {
                        return false;
                    }
                    thread::sleep(poll);
                }
            };
            let write = |bytes: &[u8]| -> Result<(), String> {
                let mut w = writer.lock().map_err(|_| "pty writer lock poisoned".to_string())?;
                w.write_all(bytes).and_then(|_| w.flush()).map_err(|e| {
                    log.write_error(&e);
                    e.to_string()
                })
            };
            let total = commands.len();
            let mut sent = 0usize;
            // まず初期プロンプト（シェル起動時の出力）が落ち着くのを待つ。
            if !wait_settle() {
                log.event(Level::Info, format_args!("initial output did not settle within {max_wait_ms} ms; sending anyway"));
            }
            for (i, cmd) in commands.iter().enumerate() {
                let n = i + 1;
                let body_settled = match write(cmd.as_bytes()) {
                    Ok(()) => wait_settle(),
                    Err(e) => {
                        log.event(Level::Warn, format_args!("startup command {n}/{total} not sent: {cmd:?}: {e}"));
                        continue;
                    }
                };
                if let Err(e) = write(b"\r") {
                    log.event(Level::Warn, format_args!("startup command {n}/{total} typed but Enter not sent: {cmd:?}: {e}"));
                    continue;
                }
                sent += 1;
                let after_settled = wait_settle();
                let note = if body_settled && after_settled {
                    String::new()
                } else {
                    format!(" (output still busy after {max_wait_ms} ms)")
                };
                log.event(Level::Info, format_args!("startup command {n}/{total} sent: {cmd:?}{note}"));
            }
            log.event(Level::Info, format_args!("==== startup commands finished: {sent}/{total} sent ===="));
        });
        if let Err(e) = spawned {
            log.event(Level::Error, format_args!("could not start the startup command thread: {e}"));
        }
    }

    pub(crate) fn scroll_to_bottom(&self) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Bottom);
        }
    }


    pub(crate) fn send(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            if let Err(e) = w.write_all(bytes).and_then(|_| w.flush()) {
                self.log.write_error(&e);
            }
        }
    }

    pub(crate) fn note_props(&mut self) {
        let hex = crate::config::color_to_hex;
        let now = (self.title.clone(), self.bg, self.accent);
        match &self.logged_props {
            None => self.log.event(
                Level::Info,
                format_args!("appearance title={:?} bg={} accent={}", now.0, hex(now.1), hex(now.2)),
            ),
            Some(old) if *old == now => return,
            Some(old) => {
                if old.0 != now.0 {
                    self.log.event(Level::Info, format_args!("title changed {:?} -> {:?}", old.0, now.0));
                }
                if old.1 != now.1 {
                    self.log.event(Level::Info, format_args!("bg changed {} -> {}", hex(old.1), hex(now.1)));
                }
                if old.2 != now.2 {
                    self.log.event(Level::Info, format_args!("accent changed {} -> {}", hex(old.2), hex(now.2)));
                }
            }
        }
        self.logged_props = Some(now);
    }

    pub(crate) fn note_exit(&mut self) {
        if self.exit_logged {
            return;
        }
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                self.exit_logged = true;
                self.log.event(Level::Info, format_args!("shell exited with code {}", status.exit_code()));
            }
            Err(e) => {
                self.exit_logged = true;
                self.log.event(Level::Warn, format_args!("cannot watch the shell process: {e}"));
            }
        }
    }

    // egui の入力イベントを pty 向けバイト列に変換して送る
    // 入力を pty へ。何か送ったら true（呼び元が最下部へスナップするのに使う）
    pub(crate) fn handle_input(&mut self, ctx: &egui::Context) -> bool {
        use egui::{Event, ImeEvent, Key};
        let events = ctx.input(|i| i.events.clone());
        let mut sent = false;
        // 端末モードを一度だけ読む（ロックはすぐ手放す。ループ中保持するとリーダースレッドの
        // パースを止めてしまう）。貼り付けの囲み・矢印のアプリカーソルキーモードに使う。
        let (bracketed_paste, app_cursor) = match self.term.lock() {
            Ok(term) => {
                let m = *term.mode();
                (
                    m.contains(TermMode::BRACKETED_PASTE),
                    m.contains(TermMode::APP_CURSOR),
                )
            }
            Err(_) => (false, false),
        };
        for ev in events {
            match ev {
                // 修飾なしの可読文字（英数記号）はここで来る
                Event::Text(t) => {
                    self.send(t.as_bytes());
                    sent = true;
                }
                // IME 変換中の未確定文字列。まだ確定していないので pty へは送らず、
                // 合成テキストとして保持だけする（描画側がカーソル位置に表示）。
                // 空文字列は「IME 解除」を意味するので、そのまま preedit が空に戻る。
                Event::Ime(ImeEvent::Preedit { text, .. }) => {
                    self.preedit = text;
                }
                // IME 確定（日本語など）。egui-winit が Commit で渡す。
                // 確定したら合成テキストは用済みなので消す。
                Event::Ime(ImeEvent::Commit(t)) => {
                    self.send(t.as_bytes());
                    self.preedit.clear();
                    sent = true;
                }
                // ペースト（Ctrl+V は Key ではなくこのイベントで届く）。改行は CR に統一。
                // アプリがブラケットペーストを有効化(\x1b[?2004h)していれば \x1b[200~ … \x1b[201~
                // で囲む。claude 等はこれで「複数行の貼り付け」と「送信」を区別する。囲む場合は
                // 中身の終端マーカーを除去して、貼り付け文字列でモードを抜けさせる注入を防ぐ。
                Event::Paste(t) => {
                    let body = t.replace("\r\n", "\r").replace('\n', "\r");
                    if bracketed_paste {
                        let safe = body.replace("\x1b[201~", "");
                        self.send(b"\x1b[200~");
                        self.send(safe.as_bytes());
                        self.send(b"\x1b[201~");
                    } else {
                        self.send(body.as_bytes());
                    }
                    sent = true;
                }
                // Ctrl+C は egui-winit が Copy に化ける。選択があればクリップボードへコピー、
                // 無ければ従来どおり割り込み(^C)を送る。
                Event::Copy => {
                    let copied = if let Ok(mut term) = self.term.lock() {
                        if term.selection.is_some() {
                            let s = term.selection_to_string().filter(|s| !s.is_empty());
                            if s.is_some() {
                                term.selection = None; // コピーしたら選択解除
                            }
                            s
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    match copied {
                        Some(s) => ctx.copy_text(s),
                        None => {
                            self.send(&[0x03]);
                            sent = true;
                        }
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // Ctrl+Tab / Ctrl+Shift+Tab はタブ循環（アプリ側で処理）。pty には送らない
                    if modifiers.ctrl && key == Key::Tab {
                        continue;
                    }
                    // Shift+Tab（Ctrl なし）→ back-tab \x1b[Z（CBT）。claude の権限モード循環など、
                    // 逆送りショートカットはこれを見ている。素の Tab(\t) に化ける前に横取りする。
                    if modifiers.shift && !modifiers.ctrl && key == Key::Tab {
                        self.send(b"\x1b[Z");
                        sent = true;
                        continue;
                    }
                    // Ctrl+Backspace → 直前の単語を空白まで一括削除。
                    // ^W（0x17 = readline の unix-word-rubout）を送る。bash/PSReadLine 双方で
                    // 単語削除として解釈される。素の Backspace(1文字)より手前で横取りする。
                    if modifiers.ctrl && key == Key::Backspace {
                        self.send(&[0x17]);
                        sent = true;
                        continue;
                    }
                    // Ctrl + 英字 → 制御コード（C-c, C-d など）
                    if modifiers.ctrl && !modifiers.alt {
                        if let Some(ascii) = ctrl_letter(key) {
                            self.send(&[ascii & 0x1f]);
                            sent = true;
                            continue;
                        }
                    }
                    // Alt + 英字（Ctrl なし）→ Meta。ESC を前置して文字を送る（\x1b + 文字）。
                    // Shift 併用時は大文字。egui は Alt 併用の可読文字を Text で流さないので、
                    // ここで拾わないと Alt 系バインドが丸ごと死ぬ。
                    if modifiers.alt && !modifiers.ctrl {
                        if let Some(lower) = ctrl_letter(key) {
                            let ch = if modifiers.shift {
                                lower.to_ascii_uppercase()
                            } else {
                                lower
                            };
                            self.send(&[0x1b, ch]);
                            sent = true;
                            continue;
                        }
                    }
                    // 残り（矢印・Home/End・編集キー・Enter/Tab/Backspace/Escape）は
                    // モード（アプリカーソルキー）と修飾（Ctrl/Shift/Alt）を織り込んで符号化する。
                    let seq = encode_key(key, &modifiers, app_cursor);
                    if !seq.is_empty() {
                        self.send(&seq);
                        sent = true;
                    }
                }
                _ => {}
            }
        }
        // 端末へ入力を送ったら選択は解除（古いハイライトを残さない）
        if sent {
            if let Ok(mut term) = self.term.lock() {
                term.selection = None;
            }
        }
        sent
    }

    // ウィンドウサイズ → 列数・行数を計算し、変化したら term と pty をリサイズ
    pub(crate) fn maybe_resize(&mut self, cols: usize, rows: usize) {
        let new = TermSize {
            cols: cols.max(1),
            rows: rows.max(1),
        };
        if new == self.size {
            return;
        }
        self.size = new;
        if let Ok(mut term) = self.term.lock() {
            term.resize(new);
        }
        if let Ok(master) = self.master.lock() {
            if let Err(e) = master.resize(PtySize {
                rows: new.rows as u16,
                cols: new.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                self.log.event(Level::Warn, format_args!("pty resize to {}x{} failed: {e}", new.cols, new.rows));
            }
        }
    }

    // 中央領域にこのセッションのグリッドを描画する
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, font: &FontId, cell_w: f32, cell_h: f32, allow_ime: bool) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        // 原点も物理ピクセルに合わせ、グリッドをピクセルに揃える
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let origin = Pos2::new(snap(rect.min.x + PAD), snap(rect.min.y + PAD));

        let cols = ((rect.width() - 2.0 * PAD) / cell_w).floor() as usize;
        let rows = ((rect.height() - 2.0 * PAD) / cell_h).floor() as usize;
        self.maybe_resize(cols, rows);

        // 既定背景＝このセッションの bg（body も色分け）。CentralPanel の塗りと一致させる。
        let body_bg = self.bg;

        let mut term = match self.term.lock() {
            Ok(t) => t,
            Err(_) => return,
        };

        // 端末側のマウスモード（アプリがマウス操作を要求しているか）を読む。
        // Claude 等のフルスクリーン TUI は SGR マウスを有効化し、スクロール/クリックを自前処理する。
        let mode = *term.mode();
        let mouse_app = mode.intersects(TermMode::MOUSE_MODE); // 1000/1002/1003 のいずれか
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        let report_all_motion = mode.contains(TermMode::MOUSE_MOTION); // 1003: 常時モーション
        let report_btn_motion = mode.contains(TermMode::MOUSE_DRAG); // 1002: ボタン押下中モーション
        // 代替スクリーンで alt-scroll だけ有効なアプリ（less 等）向けのホイール→↑↓変換。
        let alt_scroll =
            mode.contains(TermMode::ALTERNATE_SCROLL) && mode.contains(TermMode::ALT_SCREEN);
        // Shift 押下中はマウスモードを無視してローカル選択を優先（xterm 慣習）。
        // これが無いとアプリ起動中に端末側のコピー用選択が一切できなくなる。
        let shift = ctx.input(|i| i.modifiers.shift);
        let route_to_app = mouse_app && !shift;

        // 画素→セル（0 始まり・可視範囲にクランプ）。マウスレポートの座標に使う。
        let to_cell = |p: Pos2| -> (usize, usize) {
            let cx = (((p.x - origin.x) / cell_w).floor().max(0.0) as usize)
                .min(cols.saturating_sub(1));
            let cy = (((p.y - origin.y) / cell_h).floor().max(0.0) as usize)
                .min(rows.saturating_sub(1));
            (cx, cy)
        };

        // pty へ送るバイト列をここに貯め、term ロックを保持したまま最後に一度だけ send する。
        let mut to_pty: Vec<u8> = Vec::new();

        // ---- テキスト選択（マウスモード外、または Shift 併用時のみ）----
        // 画面座標→グリッド座標。display_offset を引いて履歴含む絶対 Line に戻す。
        // セルの左右どちら寄りかで Side を決める（選択端の包含/除外に効く）。
        let doff = term.grid().display_offset() as i32;
        let to_point = |p: Pos2| -> (Point, Side) {
            let relx = (p.x - origin.x) / cell_w;
            let rely = (p.y - origin.y) / cell_h;
            let col = (relx.floor().max(0.0) as usize).min(cols.saturating_sub(1));
            let row = (rely.floor().max(0.0) as i32).min(rows.max(1) as i32 - 1);
            let side = if relx - relx.floor() < 0.5 { Side::Left } else { Side::Right };
            (Point::new(Line(row - doff), Column(col)), side)
        };
        let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
        if !route_to_app {
            if resp.drag_started() {
                if let Some(p) = pointer {
                    let (point, side) = to_point(p);
                    term.selection = Some(Selection::new(SelectionType::Simple, point, side));
                }
            } else if resp.dragged() {
                if let (Some(p), Some(sel)) = (pointer, term.selection.as_mut()) {
                    let (point, side) = to_point(p);
                    sel.update(point, side);
                }
            } else if resp.clicked() {
                // 単クリック（ドラッグ無し）は選択解除
                term.selection = None;
            }
        }

        // ---- マウスレポート（アプリへボタン/モーションを転送）----
        if route_to_app {
            let events = ctx.input(|i| i.events.clone());
            for ev in &events {
                match ev {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } if rect.contains(*pos) => {
                        let base = match button {
                            egui::PointerButton::Primary => 0,
                            egui::PointerButton::Middle => 1,
                            egui::PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        let (cx, cy) = to_cell(*pos);
                        to_pty.extend(mouse_report(sgr, base, cx, cy, modifiers, *pressed));
                    }
                    egui::Event::PointerMoved(pos) if rect.contains(*pos) => {
                        // 押下中のボタン（1002/1003 のモーション基底コード）。無押下は 3。
                        let (held, any) = ctx.input(|i| {
                            let p = &i.pointer;
                            if p.button_down(egui::PointerButton::Primary) {
                                (0u32, true)
                            } else if p.button_down(egui::PointerButton::Middle) {
                                (1, true)
                            } else if p.button_down(egui::PointerButton::Secondary) {
                                (2, true)
                            } else {
                                (3, false)
                            }
                        });
                        if report_all_motion || (report_btn_motion && any) {
                            let (cx, cy) = to_cell(*pos);
                            // 同一セル上の移動は 1 回だけ（画素ごとの洪水を防ぐ）。
                            if self.last_mouse_cell != Some((cx, cy)) {
                                self.last_mouse_cell = Some((cx, cy));
                                let mods = ctx.input(|i| i.modifiers);
                                to_pty.extend(mouse_report(sgr, held + 32, cx, cy, &mods, true));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- ホイール（モードで振り分け）----
        // route_to_app: ボタン 64/65 として報告 ／ alt_scroll: ↑↓へ変換 ／ 他: スクロールバック。
        let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            self.scroll_accum += wheel / cell_h;
            let notches = self.scroll_accum.trunc() as i32;
            if notches != 0 {
                self.scroll_accum -= notches as f32;
                if route_to_app {
                    let (cx, cy) = pointer.map(|p| to_cell(p)).unwrap_or((0, 0));
                    let mods = ctx.input(|i| i.modifiers);
                    let btn = if notches > 0 { 64 } else { 65 }; // 上:64 下:65
                    for _ in 0..notches.abs() {
                        to_pty.extend(mouse_report(sgr, btn, cx, cy, &mods, true));
                    }
                } else if alt_scroll {
                    let seq: &[u8] = if notches > 0 { b"\x1b[A" } else { b"\x1b[B" };
                    for _ in 0..notches.abs() {
                        to_pty.extend_from_slice(seq);
                    }
                } else {
                    term.scroll_display(Scroll::Delta(notches));
                }
            }
        }

        if !to_pty.is_empty() {
            self.send(&to_pty);
        }

        let content = term.renderable_content();
        // スクロール時、display_iter の行は絶対座標（履歴は負）。
        // display_offset を足して画面行(0..rows)へ変換する。
        let off = content.display_offset as i32;
        // このフレームの選択範囲（term.selection から算出済み）。セル塗りで使う。
        let sel = content.selection;

        // セル描画は 2 パスに分ける。理由:
        // 全角(WIDE_CHAR)グリフは 2 セル幅で描かれるが、右半分に当たるスペーサセルの
        // 背景塗りが「同フレームで後から」重なると、全角の右半分を塗り潰してしまう
        // （既定背景では bg==body_bg で塗られず露見しないが、色付き背景＝Claude の
        // ハイライト等では全角が左半分だけになる）。背景を全部塗ってからグリフを
        // 全部描く順にすれば、背景がグリフを上書きすることが原理的に起きない。
        struct DrawCell {
            pos: Pos2,
            rect: egui::Rect,
            fg: Color32,
            bg: Color32,
            ch: char,
            flags: Flags,
            selected: bool,
        }
        let mut cells: Vec<DrawCell> = Vec::new();
        for cell in content.display_iter {
            let flags = cell.flags;
            let col = cell.point.column.0 as f32;
            let line = (cell.point.line.0 + off) as f32;
            let pos = Pos2::new(origin.x + col * cell_w, origin.y + line * cell_h);
            let rect = egui::Rect::from_min_size(pos, Vec2::new(cell_w, cell_h));

            // 色を解決（bold は基本色を明色化、dim は減光、inverse は前後景を入替）
            let bold = flags.contains(Flags::BOLD);
            let mut fg = resolve(cell.fg, bold, body_bg);
            let mut bg = resolve(cell.bg, false, body_bg);
            if flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let selected = sel.as_ref().is_some_and(|r| r.contains(cell.point));
            cells.push(DrawCell {
                pos,
                rect,
                fg,
                bg,
                ch: cell.c,
                flags,
                selected,
            });
        }

        // パス1: 背景（セッション背景と異なる時だけ塗る。同色は CentralPanel の塗りで足りる）
        //        ＋選択ハイライト（半透明をセル地の上、グリフより先に）
        for c in &cells {
            if c.bg != body_bg {
                painter.rect_filled(c.rect, 0.0, c.bg);
            }
            if c.selected {
                painter.rect_filled(c.rect, 0.0, SELECTION_BG);
            }
        }

        // パス2: グリフ（隠し属性 / ワイド文字の右半分スペーサ / 空白 は描かない）
        //        ＋下線 / 取り消し線
        for c in &cells {
            let draw_glyph = !c.flags.contains(Flags::HIDDEN)
                && !c.flags.contains(Flags::WIDE_CHAR_SPACER)
                && c.ch != ' '
                && c.ch != '\0';
            if draw_glyph {
                painter.text(c.pos, Align2::LEFT_TOP, c.ch, font.clone(), c.fg);
            }

            // 全角は 2 セル幅なので下線/取り消し線もその幅で引く
            let line_w = if c.flags.contains(Flags::WIDE_CHAR) {
                cell_w * 2.0
            } else {
                cell_w
            };
            if c.flags.intersects(Flags::ALL_UNDERLINES) {
                let y = c.pos.y + cell_h - 1.5;
                painter.hline(
                    egui::Rangef::new(c.pos.x, c.pos.x + line_w),
                    y,
                    egui::Stroke::new(1.0, c.fg),
                );
            }
            if c.flags.contains(Flags::STRIKEOUT) {
                let y = c.pos.y + cell_h * 0.5;
                painter.hline(
                    egui::Rangef::new(c.pos.x, c.pos.x + line_w),
                    y,
                    egui::Stroke::new(1.0, c.fg),
                );
            }
        }

        // カーソル位置（ブロック描画 ＋ IME 候補窓の位置に使う）
        let col = content.cursor.point.column.0 as f32;
        let line = (content.cursor.point.line.0 + off) as f32;
        let cpos = Pos2::new(origin.x + col * cell_w, origin.y + line * cell_h);
        let crect = egui::Rect::from_min_size(cpos, Vec2::new(cell_w, cell_h));
        if content.cursor.shape != CursorShape::Hidden {
            painter.rect_filled(crect, 0.0, Color32::from_white_alpha(96));
        }

        // IME 未確定文字列（変換中の合成テキスト）をカーソル位置にインライン表示。
        // まだ pty へ送っておらずグリッドに無いので、ここで直に描く。下線＝未確定の合図。
        // 候補窓は合成テキストの末尾に出したいので、その分の表示幅を測って基準を右へ送る。
        let mut ime_anchor = crect;
        if allow_ime && !self.preedit.is_empty() {
            let text = self.preedit.clone();
            // 全角/半角混在に対応するため、実グリフ幅の総和で表示幅を求める。
            let w: f32 = ctx.fonts_mut(|f| text.chars().map(|c| f.glyph_width(font, c)).sum());
            let pre_rect = egui::Rect::from_min_size(cpos, Vec2::new(w.max(cell_w), cell_h));
            // 端末地で塗り直してから合成テキストを重ね、下線で未確定を示す。
            painter.rect_filled(pre_rect, 0.0, body_bg);
            painter.text(cpos, Align2::LEFT_TOP, &text, font.clone(), DEFAULT_FG);
            painter.hline(
                egui::Rangef::new(cpos.x, cpos.x + w),
                cpos.y + cell_h - 1.5,
                egui::Stroke::new(1.0, DEFAULT_FG),
            );
            ime_anchor =
                egui::Rect::from_min_size(Pos2::new(cpos.x + w, cpos.y), Vec2::new(cell_w, cell_h));
        }

        // IME を毎フレーム有効化（ime=Some で winit が set_ime_allowed(true)）。
        // 候補窓は合成テキスト末尾（無ければカーソルセル）に出す。タブ名編集中は欄側に譲る。
        if allow_ime {
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: ime_anchor,
                    cursor_rect: ime_anchor,
                    should_interrupt_composition: false,
                });
            });
        }
    }
}


impl Drop for Session {
    fn drop(&mut self) {
        self.note_exit();
        self.note_props();
        self.log.event(Level::Info, format_args!("tab closed (title={:?})", self.title));
    }
}

fn spawn_failed(log: &TabLog, what: &str, e: impl std::fmt::Display) -> String {
    let msg = format!("{what}: {e}");
    log.event(Level::Error, format_args!("tab not opened: {msg}"));
    msg
}
