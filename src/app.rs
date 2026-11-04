//! アプリ本体：App 状態・更新ループ（eframe::App 実装）。
use crate::Launch;
use std::sync::mpsc;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Sense, Vec2};
use crate::color::*;
use crate::config::*;
use crate::session::*;
use crate::shells::*;
use crate::icons::*;
use crate::ipc::*;
use crate::tray::*;
use crate::ui::*;
use crate::winintegration::*;
use crate::FONT_SIZE;

// 隠していたウィンドウを再表示してから前面化するまで置くフレーム数。Visible(true) は
// このフレームの描画後に適用されるため、間を置かずに前面化しても相手がまだ不可視で効かない。
const FRONT_DELAY_FRAMES: u8 = 2;

// ---- アプリ本体（複数セッション ＋ 縦タブ）----
pub(crate) struct App {
    pub(crate) sessions: Vec<Session>,
    pub(crate) active: usize,
    pub(crate) next_id: u32,
    pub(crate) editing: Option<usize>,       // タブ名編集中のインデックス
    pub(crate) editing_init: bool,           // 編集開始フレームでフォーカスを要求するため
    pub(crate) confirm_close: Option<usize>, // ×押下→閉じる確認待ちのインデックス
    pub(crate) confirm_exit: bool,           // ウィンドウを閉じる操作→終了確認待ち
    // 終了を素通しさせる印。ユーザーが確認で「終了する」を選んだとき、および
    // アプリ内部から意図して閉じるとき（アンインストール後・スクリプト完走）に立てる。
    // 一度立てたら下ろさない（Close は次フレーム以降に close_requested として返るため）。
    pub(crate) allow_close: bool,
    // ウィンドウを隠して裏で動かしているか（シェルは生きたまま）。戻す入口はトレイアイコン。
    pub(crate) hidden: bool,
    // 再表示後に前面化するまでの残りフレーム数（0＝予定なし。詳細は show_window）。
    pub(crate) pending_front: u8,
    // 本体ウィンドウのハンドルをトレイ側へ渡し終えたか。
    pub(crate) hwnd_shared: bool,
    pub(crate) drag: Option<TabDrag>,        // ドラッグ中のタブ（D&D 並べ替え）
    pub(crate) settings_open: bool,          // 設定モーダルを表示中か
    pub(crate) settings_tab: SettingsTab,    // 設定モーダルで開いているタブ
    pub(crate) shell_list_open: bool,        // 設定内：既定シェル一覧を展開中か
    pub(crate) shells: Vec<ShellSpec>, // 探索で見つかった利用可能シェル
    pub(crate) shell_icons: Vec<Option<egui::TextureHandle>>, // shells と並行：各シェルの exe アイコン
    // ---- 設定（今はメモリ上。永続化は別途 toml で）----
    pub(crate) default_shell: usize,    // 新規タブの既定シェル（shells のインデックス）
    pub(crate) default_cwd: String,     // 新規タブの初期ディレクトリ（空=継承）
    pub(crate) default_bg: Color32,     // 新規タブの既定背景色
    pub(crate) default_accent: Color32, // 新規タブの既定アクセント色
    pub(crate) panel_right: bool,       // タブパネルを右に置くか（false=左）
    pub(crate) startup_quiet_ms: u64,   // 起動コマンド投入の静穏窓（config 可変、既定 STARTUP_QUIET_MS）
    pub(crate) startup_max_wait_ms: u64, // 同・待ち上限（config 可変、既定 STARTUP_MAX_WAIT_MS）
    pub(crate) log_settings: LogSettings,
    pub(crate) presets: Vec<TabSpec>,     // 型ライブラリ（再利用できるタブ定義。name が識別子）
    pub(crate) startup: Vec<StartupEntry>, // 起動時に開くタブ定義（空＝既定シェル1枚）
    pub(crate) script_runner: std::collections::BTreeMap<String, String>, // 拡張子→スクリプト実行シェル short
    pub(crate) script_dd_open: Option<usize>, // 既定タブ：スクリプト実行シェルのDDを開いている拡張子行
    pub(crate) startup_shell_open: Option<usize>, // 起動タブ編集：シェルDDを開いているカード
    pub(crate) startup_color_open: Option<(usize, bool)>, // 色パレットを開いているカード（bool=accent）
    pub(crate) preset_shell_open: Option<usize>,  // プリセット編集：シェルDDを開いているカード
    pub(crate) preset_color_open: Option<(usize, bool)>, // 同・色パレットを開いているカード（bool=accent）
    pub(crate) startup_pick_open: Option<usize>,  // 起動タブ（参照モード）：プリセット選択DDを開いているカード
    pub(crate) card_drag: Option<TabDrag>,        // カード（起動タブ／プリセット）の D&D 並べ替え状態
    pub(crate) toast: Option<(String, std::time::Instant)>, // 一時通知（保存しました 等）
    // 別インスタンスから転送された「このフォルダで開く」要求の受信口（本体のみ Some）。
    pub(crate) ipc_rx: Option<mpsc::Receiver<IpcMsg>>,
    // 枠（タイトルバー）ダークの再適用管理（詳細は apply_dark_titlebar のコメント）。
    pub(crate) titlebar_last_dark: Option<std::time::Instant>, // 最後に再適用した時刻
    pub(crate) titlebar_was_focused: bool,                     // 前フレームのフォーカス状態
}

impl App {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        setup_fonts(&cc.egui_ctx);
        // egui のテーマをダーク固定（set_visuals は毎フレームのシステム追従に
        // 上書きされるが、theme_preference は設定として永続する）。メニュー等が黒くなる。
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        // タイトルバーをダークに（Windows の immersive dark mode）。
        // ネイティブ枠のままなのでリサイズ/スナップ/最大化は維持。
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        // 通知領域に常駐する。ウィンドウを隠して裏で動かすときの唯一の目印であり戻る入口。
        // 失敗しても起動は続ける（隠す操作だけが使えなくなる＝tray_ready で判定する）。
        install_tray(&cc.egui_ctx);
        match &cc.wgpu_render_state {
            Some(rs) => {
                let info = rs.adapter.get_info();
                log::info!(
                    "renderer wgpu backend={:?} adapter={} type={:?} driver={} {}",
                    info.backend, info.name, info.device_type, info.driver, info.driver_info
                );
            }
            None => log::info!("renderer is not wgpu"),
        }
        let home = home_dir();
        let shells = discover_shells();
        // 各シェルの exe からアイコンを抽出してテクスチャ化（メニュー表示用）
        let shell_icons: Vec<Option<egui::TextureHandle>> = shells
            .iter()
            .map(|s| {
                // icon_source があればそれ、無ければ program からアイコンを読み込む
                //（PNG=直接デコード / exe=埋め込み抽出 を load_icon_image が判別）。
                let icon_path = s.icon_source.as_deref().unwrap_or(&s.program);
                load_icon_image(icon_path).map(|img| {
                    // メニューは16px表示。GPUの粗い縮小を避けるため32pxへ事前縮小。
                    let img = downscale_square(&img, 32);
                    cc.egui_ctx.load_texture(
                        format!("shell-icon-{}", s.short),
                        img,
                        egui::TextureOptions::LINEAR,
                    )
                })
            })
            .collect();
        // 既定値（pwsh＝先頭 / ホーム / テーマ既定）。保存済み設定があれば上書きする。
        let mut default_shell = 0;
        let mut default_cwd = home.clone();
        let mut default_bg = DEFAULT_BG;
        let mut default_accent = ACCENT_GREEN;
        let mut panel_right = false;
        let mut startup_quiet_ms = STARTUP_QUIET_MS;
        let mut startup_max_wait_ms = STARTUP_MAX_WAIT_MS;
        let mut log_settings = LogSettings::default();
        let mut presets: Vec<TabSpec> = Vec::new();
        let mut startup: Vec<StartupEntry> = Vec::new();
        let mut script_runner: std::collections::BTreeMap<String, String> = Default::default();
        if let Some(cfg) = load_settings() {
            panel_right = cfg.panel == "right";
            startup_quiet_ms = cfg.startup_quiet_ms;
            startup_max_wait_ms = cfg.startup_max_wait_ms;
            log_settings = cfg.log.clone();
            if let Some(i) = shells.iter().position(|s| s.short == cfg.default_shell) {
                default_shell = i;
            }
            if !cfg.default_cwd.is_empty() {
                default_cwd = cfg.default_cwd;
            }
            if let Some(c) = color_from_hex(&cfg.default_bg) {
                default_bg = c;
            }
            if let Some(c) = color_from_hex(&cfg.default_accent) {
                default_accent = c;
            }
            presets = cfg.presets;
            startup = cfg.startup;
            script_runner = cfg.script_runner;
        }
        // GUI 編集用の複数行バッファを commands から初期化（プリセット・マニュアル双方）。
        for p in &mut presets {
            p.sync_buf_from_commands();
        }
        for e in &mut startup {
            e.spec.sync_buf_from_commands();
        }

        // 起動タブ：定義があれば順に開く。各項目は未指定なら既定にフォールバック。
        // 定義が無ければ従来どおり既定シェル1枚。
        // ただし --cwd で起動された場合（Explorer の「このディレクトリで開く」）は、
        // 起動タブ定義を無視してそのフォルダで1枚だけ開く（「ここで開く」の意図に素直）。
        let mut sessions: Vec<Session> = Vec::new();
        let mut open_errors: Vec<String> = Vec::new();
        let mut rec_errors: Vec<String> = Vec::new();
        let cast_max_kb = log_settings.cast_max_kb;
        if let Launch::Cwd(path) = &launch {
            // タブ名はフォルダ名（末尾要素）にする。取れなければ既定シェル名にフォールバック。
            let title = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} 1", shells[default_shell].short));
            match Session::spawn(&cc.egui_ctx, title, &shells[default_shell], path) {
                Ok(mut s) => {
                    s.bg = default_bg;
                    s.accent = default_accent;
                    sessions.push(s);
                }
                Err(e) => open_errors.push(e),
            }
        } else if let Launch::RunScript(path) = &launch {
            // --run：スクリプトを、そのフォルダで実行するタブ1枚（起動タブ定義は無視）。
            match spawn_script_session(
                &cc.egui_ctx,
                path,
                &shells,
                default_shell,
                &script_runner,
                default_bg,
                default_accent,
            ) {
                Ok(s) => sessions.push(s),
                Err(e) => open_errors.push(e),
            }
        } else if let Launch::Tab { spec, .. } = &launch {
            // --tab（本体不在 or --new-window）：そのタブ1枚だけ開く（起動タブ定義は無視）。
            // 深度スタンプは「呼び出し元+1」。この新プロセス自体が呼び出し元の子なので、
            // 環境変数は呼び出し元の値を継承している。
            let si = shells
                .iter()
                .position(|s| s.short == spec.shell)
                .unwrap_or(default_shell);
            let cwd = if spec.cwd.is_empty() {
                default_cwd.clone()
            } else {
                spec.cwd.clone()
            };
            let name = if spec.name.is_empty() {
                format!("{} 1", shells[si].short)
            } else {
                spec.name.clone()
            };
            let env = [(
                TAB_DEPTH_ENV.to_string(),
                (current_tab_depth() + 1).to_string(),
            )];
            match Session::spawn_with_env(&cc.egui_ctx, name, &shells[si], &cwd, &env) {
                Ok(mut s) => {
                    s.bg = color_from_hex(&spec.bg).unwrap_or(default_bg);
                    s.accent = color_from_hex(&spec.accent).unwrap_or(default_accent);
                    if spec.record {
                        if let Err(e) = s.set_recording(true, cast_max_kb) {
                            rec_errors.push(e);
                        }
                    }
                    s.send_commands_paced(spec.commands.clone(), startup_quiet_ms, startup_max_wait_ms);
                    sessions.push(s);
                }
                Err(e) => open_errors.push(e),
            }
        } else if startup.is_empty() {
            match Session::spawn(
                &cc.egui_ctx,
                format!("{} 1", shells[default_shell].short),
                &shells[default_shell],
                &default_cwd,
            ) {
                Ok(mut s) => {
                    s.bg = default_bg;
                    s.accent = default_accent;
                    sessions.push(s);
                }
                Err(e) => open_errors.push(e),
            }
        } else {
            for (n, entry) in startup.iter().enumerate() {
                // エントリをタブ定義へ解決する。プリセット参照は presets から name で引く。
                // 解決できない参照は既定シェルで開き、タブ名に未解決マーカーを付けて可視化する
                // （黙って落とさない＝silent fail 回避）。マニュアルは spec をそのまま使う。
                let (spec, unresolved) = if entry.is_preset() {
                    match presets.iter().find(|p| p.name == entry.preset) {
                        Some(p) => (p.clone(), false),
                        None => {
                            // 未選択（preset 名が空）と、名前は入っているが見つからない場合を区別。
                            let label = if entry.preset.is_empty() {
                                format!("{} (未選択)", UNRESOLVED_PRESET_MARK)
                            } else {
                                format!("{} {}", UNRESOLVED_PRESET_MARK, entry.preset)
                            };
                            (TabSpec { name: label, ..Default::default() }, true)
                        }
                    }
                } else {
                    (entry.spec.clone(), false)
                };
                let si = shells
                    .iter()
                    .position(|s| s.short == spec.shell)
                    .unwrap_or(default_shell);
                let cwd = if spec.cwd.is_empty() {
                    default_cwd.clone()
                } else {
                    spec.cwd.clone()
                };
                let name = if spec.name.is_empty() {
                    format!("{} {}", shells[si].short, n + 1)
                } else {
                    spec.name.clone()
                };
                let bg = color_from_hex(&spec.bg).unwrap_or(default_bg);
                let accent = color_from_hex(&spec.accent).unwrap_or(default_accent);
                let mut s = match Session::spawn(&cc.egui_ctx, name, &shells[si], &cwd) {
                    Ok(s) => s,
                    Err(e) => {
                        open_errors.push(e);
                        continue;
                    }
                };
                s.bg = bg;
                s.accent = accent;
                if spec.record {
                    if let Err(e) = s.set_recording(true, cast_max_kb) {
                        rec_errors.push(e);
                    }
                }
                // 出力が静まってから次を送る（claude 等の TUI にも投入できるように）。
                // 未解決参照はコマンドを持たないので送らない。
                if !unresolved {
                    s.send_commands_paced(spec.commands.clone(), startup_quiet_ms, startup_max_wait_ms);
                }
                sessions.push(s);
            }
        }
        let next_id = sessions.len() as u32 + 1;

        Self {
            sessions,
            active: 0,
            next_id,
            editing: None,
            editing_init: false,
            confirm_close: None,
            confirm_exit: false,
            allow_close: false,
            hidden: false,
            pending_front: 0,
            hwnd_shared: false,
            drag: None,
            settings_open: false,
            settings_tab: SettingsTab::General,
            shell_list_open: false,
            shells,
            shell_icons,
            default_shell,
            default_cwd,
            default_bg,
            default_accent,
            panel_right,
            startup_quiet_ms,
            startup_max_wait_ms,
            log_settings,
            presets,
            startup,
            script_runner,
            script_dd_open: None,
            startup_shell_open: None,
            startup_color_open: None,
            preset_shell_open: None,
            preset_color_open: None,
            startup_pick_open: None,
            card_drag: None,
            toast: match open_errors.as_slice() {
                [] => rec_errors
                    .first()
                    .map(|e| (format!("画面の記録を始められませんでした: {e}"), std::time::Instant::now())),
                [e] => Some((format!("タブを開けませんでした: {e}"), std::time::Instant::now())),
                [e, ..] => Some((
                    format!("{} 個のタブを開けませんでした（1つ目: {e}）", open_errors.len()),
                    std::time::Instant::now(),
                )),
            },
            // 受け口の生成に成功した（＝このプロセスが本体の）ときだけ Some。
            ipc_rx: ipc_start_server(&cc.egui_ctx),
            titlebar_last_dark: None,
            titlebar_was_focused: false,
        }
    }

    // 別インスタンスからの「このフォルダで開く」要求を、既定シェルの新規タブとして開く。
    pub(crate) fn open_dir_tab(&mut self, ctx: &egui::Context, dir: &str) {
        let spec = self.shells[self.default_shell].clone();
        let id = self.next_id;
        self.next_id += 1;
        let title = std::path::Path::new(dir)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} {}", spec.short, id));
        let mut sess = match Session::spawn(ctx, title, &spec, dir) {
            Ok(s) => s,
            Err(e) => return self.tab_open_failed(e),
        };
        sess.bg = self.default_bg;
        sess.accent = self.default_accent;
        self.sessions.push(sess);
        self.active = self.sessions.len() - 1;
    }

    // スクリプト（.ps1/.bat/.cmd）を、そのファイルのあるフォルダで実行するタブを開く。
    pub(crate) fn open_script_tab(&mut self, ctx: &egui::Context, path: &str) {
        let sess = match spawn_script_session(
            ctx,
            path,
            &self.shells,
            self.default_shell,
            &self.script_runner,
            self.default_bg,
            self.default_accent,
        ) {
            Ok(s) => s,
            Err(e) => return self.tab_open_failed(e),
        };
        self.next_id += 1;
        self.sessions.push(sess);
        self.active = self.sessions.len() - 1;
    }

    fn tab_open_failed(&mut self, e: String) {
        self.show_toast(format!("タブを開けませんでした: {e}"));
    }

    // 一時通知を出す（数秒で自動的に消える）。
    pub(crate) fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), std::time::Instant::now()));
    }

    // 現在の設定を TOML で保存（%APPDATA%\LustTermina\config.toml）。
    pub(crate) fn save_settings(&self) {
        let Some(path) = config_path() else { return };
        let s = Settings {
            default_shell: self
                .shells
                .get(self.default_shell)
                .map(|s| s.short.clone())
                .unwrap_or_default(),
            default_cwd: self.default_cwd.clone(),
            default_bg: color_to_hex(self.default_bg),
            default_accent: color_to_hex(self.default_accent),
            panel: if self.panel_right { "right".into() } else { "left".into() },
            startup_quiet_ms: self.startup_quiet_ms,
            startup_max_wait_ms: self.startup_max_wait_ms,
            log: self.log_settings.clone(),
            // 保存時に commands_buf（複数行テキスト）→ commands（行配列、空行除去）へ確定。
            presets: self
                .presets
                .iter()
                .map(|p| {
                    let mut s = p.clone();
                    s.commands = p.commands_from_buf();
                    s
                })
                .collect(),
            // マニュアルエントリのみ commands を確定（プリセット参照は spec を持たない）。
            startup: self
                .startup
                .iter()
                .map(|e| {
                    let mut e = e.clone();
                    if e.is_preset() {
                        // 参照モードは spec を持たない＝TOML に残さずクリーンに保つ。
                        e.spec = TabSpec::default();
                    } else {
                        // マニュアルは preset 値を残さない（use_preset=false で無視される値の齟齬回避）。
                        e.preset.clear();
                        e.spec.commands = e.spec.commands_from_buf();
                    }
                    e
                })
                .collect(),
            script_runner: self.script_runner.clone(),
        };
        if let Ok(text) = toml::to_string_pretty(&s) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, text);
        }
    }

    // 保存データ（config.toml）を削除し、永続設定をすべて既定へ戻す（初期化）。
    // 破壊的なので呼び元で確認を取ってから呼ぶ。開いているタブ等の実行時状態は触らない。
    // 既定値は new() と同じ定数を参照（一元管理）。
    pub(crate) fn reset_settings(&mut self) {
        if let Some(path) = config_path() {
            let _ = std::fs::remove_file(path);
        }
        self.default_shell = 0;
        self.default_cwd = home_dir();
        self.default_bg = DEFAULT_BG;
        self.default_accent = ACCENT_GREEN;
        self.panel_right = false;
        self.startup_quiet_ms = STARTUP_QUIET_MS;
        self.startup_max_wait_ms = STARTUP_MAX_WAIT_MS;
        self.log_settings = LogSettings::default();
        self.presets.clear();
        self.startup.clear();
        self.script_runner.clear();
    }

    // 現在開いているタブ群を起動リスト（すべてマニュアル）に変換（スナップショット）。
    pub(crate) fn snapshot_startup(&self) -> Vec<StartupEntry> {
        self.sessions
            .iter()
            .map(|s| StartupEntry { spec: tab_spec_of(s), ..Default::default() })
            .collect()
    }

    // プリセット（型）から新しいタブを実体化して開く。起動時と同じ解決規則で、
    // 未指定フィールドは既定にフォールバック。コマンドは編集中バッファを正とする。
    pub(crate) fn spawn_tab_from_spec(&mut self, ctx: &egui::Context, spec: &TabSpec) {
        self.spawn_tab_from_spec_env(ctx, spec, &[]);
    }

    // 追加環境変数付き版（--tab の系譜スタンプ用。UI からのプリセット実体化は env なし）。
    pub(crate) fn spawn_tab_from_spec_env(
        &mut self,
        ctx: &egui::Context,
        spec: &TabSpec,
        extra_env: &[(String, String)],
    ) {
        let si = self
            .shells
            .iter()
            .position(|s| s.short == spec.shell)
            .unwrap_or(self.default_shell);
        let cwd = if spec.cwd.is_empty() {
            self.default_cwd.clone()
        } else {
            spec.cwd.clone()
        };
        let id = self.next_id;
        self.next_id += 1;
        let name = if spec.name.is_empty() {
            format!("{} {}", self.shells[si].short, id)
        } else {
            spec.name.clone()
        };
        let mut sess = match Session::spawn_with_env(ctx, name, &self.shells[si], &cwd, extra_env) {
            Ok(s) => s,
            Err(e) => return self.tab_open_failed(e),
        };
        sess.bg = color_from_hex(&spec.bg).unwrap_or(self.default_bg);
        sess.accent = color_from_hex(&spec.accent).unwrap_or(self.default_accent);
        if spec.record {
            if let Err(e) = sess.set_recording(true, self.log_settings.cast_max_kb) {
                self.show_toast(format!("画面の記録を始められませんでした: {e}"));
            }
        }
        sess.send_commands_paced(
            spec.commands_from_buf(),
            self.startup_quiet_ms,
            self.startup_max_wait_ms,
        );
        self.sessions.push(sess);
        self.active = self.sessions.len() - 1;
    }

    // --tab（パイプ経由）のタブ生成。不明シェルは既定に落とすが、黙らず toast で可視化する。
    pub(crate) fn open_requested_tab(&mut self, ctx: &egui::Context, mut req: TabRequest) {
        req.tab.sync_buf_from_commands();
        if !req.tab.shell.is_empty() && !self.shells.iter().any(|s| s.short == req.tab.shell) {
            self.show_toast(format!(
                "シェル「{}」が見つからないため既定シェルで開きます",
                req.tab.shell
            ));
        }
        let env = [(TAB_DEPTH_ENV.to_string(), req.depth.to_string())];
        self.spawn_tab_from_spec_env(ctx, &req.tab, &env);
    }

    // 設定ページ（端末の代わりに中央へ全面表示）。閉じる時に TOML 保存。
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut close = false;
        // 全体テーマに合わせ、テキスト選択ハイライトを暗めの緑へ（明るい緑のベタ塗りは
        // 文字が潰れるので避ける）。
        ui.visuals_mut().selection.bg_fill = Color32::from_rgb(0x1E, 0x4D, 0x3A);
        ui.visuals_mut().selection.stroke = egui::Stroke::new(1.0, ACCENT_GREEN);
        // ボタン等のウィジェットも暗いテーマに合わせる（既定の明るいグレーをやめる）。
        {
            let v = ui.visuals_mut();
            v.widgets.inactive.corner_radius = egui::CornerRadius::same(5);
            v.widgets.hovered.corner_radius = egui::CornerRadius::same(5);
            v.widgets.active.corner_radius = egui::CornerRadius::same(5);
            v.widgets.inactive.weak_bg_fill = Color32::from_gray(40);
            v.widgets.hovered.weak_bg_fill = Color32::from_gray(54);
            v.widgets.active.weak_bg_fill = Color32::from_gray(64);
            v.widgets.inactive.fg_stroke.color = TAB_TEXT;
            v.widgets.hovered.fg_stroke.color = TAB_TEXT_ACTIVE;
            v.widgets.active.fg_stroke.color = TAB_TEXT_ACTIVE;
            // ホバー/押下で数px膨らむ既定挙動を止める（枠付きで「ズレる」のを防ぐ）。
            v.widgets.inactive.expansion = 0.0;
            v.widgets.hovered.expansion = 0.0;
            v.widgets.active.expansion = 0.0;
        }

        // ヘッダ：戻る（chevron）＋タイトル＋保存。パネルが右なら左右反転して対称に
        // （戻る＋タイトルを右、保存を左、chevron も右向き）。
        let panel_right = self.panel_right;
        ui.horizontal(|ui| {
            let chevron = if panel_right { "\u{e5cc}" } else { "\u{e5cb}" };
            let back_btn = |ui: &mut egui::Ui| {
                ui.add(
                    egui::Button::new(egui::RichText::new(chevron).size(24.0).color(TAB_TEXT))
                        .fill(Color32::TRANSPARENT)
                        .min_size(Vec2::new(34.0, 30.0)),
                )
                .clicked()
            };
            let mut back_clicked = false;
            let mut save_clicked = false;
            if !panel_right {
                back_clicked = back_btn(ui);
                ui.label(egui::RichText::new("設定").size(18.0).color(TAB_TEXT_ACTIVE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save_clicked = primary_button(ui, "保存");
                });
            } else {
                save_clicked = primary_button(ui, "保存");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    back_clicked = back_btn(ui);
                    ui.label(egui::RichText::new("設定").size(18.0).color(TAB_TEXT_ACTIVE));
                });
            }
            if back_clicked {
                close = true;
            }
            if save_clicked {
                self.save_settings();
                self.show_toast("設定を保存しました");
            }
        });
        ui.separator();
        ui.add_space(8.0);

        // 縦ナビ ／ 内容。ナビはタブパネルの左右設定に追従（パネルが右ならナビも右、
        // 内容が左）。ヘッダは常に上のまま。内容は左右に隙間が出ないよう可変幅。
        let nav_w = 120.0_f32;
        let content_w = (ui.available_width() - nav_w - 16.0).max(320.0);
        ui.horizontal_top(|ui| {
            if !panel_right {
                ui.vertical(|ui| {
                    ui.set_width(nav_w);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::General, "既定", false);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Theme, "UI/テーマ", false);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Presets, "プリセット", false);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Startup, "起動タブ", false);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::System, "システム", false);
                });
                ui.separator();
            }
            // 内容は常に左基準（反転しない）。反転はヘッダとナビ位置のみ。
            ui.vertical(|ui| {
                ui.set_width(content_w);

                // ===== 既定タブ =====
                if self.settings_tab == SettingsTab::General {
                    ui.label(egui::RichText::new("既定シェル（新規タブ）").color(TAB_TEXT));
                    let cur = self.default_shell;
                    let cur_label =
                        self.shells.get(cur).map(|s| s.label.clone()).unwrap_or_default();
                    let arrow = if self.shell_list_open { "▲" } else { "▼" };
                    let header_text = format!("{cur_label}　{arrow}");
                    let header = match self.shell_icons.get(cur).and_then(|o| o.as_ref()) {
                        Some(tex) => {
                            let img = egui::Image::new(tex).fit_to_exact_size(Vec2::splat(16.0));
                            ui.add_sized([300.0, 26.0], egui::Button::image_and_text(img, header_text))
                        }
                        None => ui.add_sized([300.0, 26.0], egui::Button::new(header_text)),
                    };
                    let header_rect = header.rect;
                    if header.clicked() {
                        self.shell_list_open = !self.shell_list_open;
                    }
                    if self.shell_list_open {
                        let dd_frame = egui::Frame::new()
                            .fill(Color32::from_gray(40))
                            .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::same(4));
                        let area = egui::Area::new(egui::Id::new("shell_dropdown"))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(header_rect.left_bottom() + Vec2::new(0.0, 2.0))
                            .show(ctx, |ui| {
                                dd_frame.show(ui, |ui| {
                                    ui.set_width(header_rect.width() - 8.0);
                                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                        if let Some(i) =
                                            shell_list(ui, &self.shells, &self.shell_icons, Some(cur))
                                        {
                                            self.default_shell = i;
                                            self.shell_list_open = false;
                                        }
                                    });
                                });
                            });
                        let area_rect = area.response.rect;
                        if ctx.input(|i| i.pointer.any_pressed()) {
                            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                                if !header_rect.contains(p) && !area_rect.contains(p) {
                                    self.shell_list_open = false;
                                }
                            }
                        }
                    }
                    ui.add_space(12.0);

                    ui.label(egui::RichText::new("初期ディレクトリ").color(TAB_TEXT));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.default_cwd)
                            .desired_width(300.0)
                            .hint_text("空＝親プロセスを継承"),
                    );
                    ui.add_space(14.0);

                    // スクリプト実行に使うシェルを拡張子ごとに選ぶ（右クリック「LustTermina で実行」）。
                    // 拡張子リストは SCRIPT_EXTS が唯一の正＝将来 .sh を足せば自動でこの欄も増える。
                    ui.label(egui::RichText::new("スクリプト実行に使うシェル").color(TAB_TEXT));
                    ui.label(
                        egui::RichText::new(
                            "右クリック「LustTermina で実行」で使うシェルを拡張子ごとに選べます（未設定は既定）。",
                        )
                        .color(Color32::from_gray(130))
                        .size(11.0),
                    );
                    ui.add_space(4.0);
                    for (idx, ext) in SCRIPT_EXTS.iter().enumerate() {
                        let key = ext.trim_start_matches('.');
                        // 実効シェル：config 選択 → 組込み既定。未設定なら「（既定）」を付す。
                        let cur_short = self
                            .script_runner
                            .get(key)
                            .cloned()
                            .unwrap_or_else(|| default_runner_short(key).to_string());
                        let cur_idx = self.shells.iter().position(|s| s.short == cur_short);
                        let cur_label = cur_idx
                            .map(|k| self.shells[k].label.clone())
                            .unwrap_or_else(|| cur_short.clone());
                        let is_default = !self.script_runner.contains_key(key);
                        let open = self.script_dd_open == Some(idx);
                        let arrow = if open { "▲" } else { "▼" };
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [46.0, 26.0],
                                egui::Label::new(egui::RichText::new(*ext).color(TAB_TEXT)),
                            );
                            let suffix = if is_default { "（既定）" } else { "" };
                            let htext = format!("{cur_label}{suffix}　{arrow}");
                            let header = match cur_idx
                                .and_then(|k| self.shell_icons.get(k))
                                .and_then(|o| o.as_ref())
                            {
                                Some(tex) => {
                                    let img =
                                        egui::Image::new(tex).fit_to_exact_size(Vec2::splat(16.0));
                                    ui.add_sized(
                                        [254.0, 26.0],
                                        egui::Button::image_and_text(img, htext),
                                    )
                                }
                                None => ui.add_sized([254.0, 26.0], egui::Button::new(htext)),
                            };
                            let hrect = header.rect;
                            if header.clicked() {
                                self.script_dd_open = if open { None } else { Some(idx) };
                            }
                            if open {
                                let dd_frame = egui::Frame::new()
                                    .fill(Color32::from_gray(40))
                                    .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
                                    .corner_radius(6.0)
                                    .inner_margin(egui::Margin::same(4));
                                let mut picked: Option<usize> = None;
                                let area = egui::Area::new(egui::Id::new(("script_dd", idx)))
                                    .order(egui::Order::Tooltip)
                                    .fixed_pos(hrect.left_bottom() + Vec2::new(0.0, 2.0))
                                    .show(ctx, |ui| {
                                        dd_frame.show(ui, |ui| {
                                            ui.set_min_width(hrect.width() - 8.0);
                                            egui::ScrollArea::vertical().max_height(220.0).show(
                                                ui,
                                                |ui| {
                                                    if let Some(k) = shell_list(
                                                        ui,
                                                        &self.shells,
                                                        &self.shell_icons,
                                                        cur_idx,
                                                    ) {
                                                        picked = Some(k);
                                                    }
                                                },
                                            );
                                        });
                                    });
                                let arect = area.response.rect;
                                if let Some(k) = picked {
                                    self.script_runner
                                        .insert(key.to_string(), self.shells[k].short.clone());
                                    self.script_dd_open = None;
                                } else if ctx.input(|i| i.pointer.any_pressed()) {
                                    if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                                        if !hrect.contains(p) && !arect.contains(p) {
                                            self.script_dd_open = None;
                                        }
                                    }
                                }
                            }
                        });
                        ui.add_space(2.0);
                    }
                }

                // ===== UI/テーマタブ =====
                if self.settings_tab == SettingsTab::Theme {
                    ui.label(egui::RichText::new("タブパネルの位置").color(TAB_TEXT));
                    ui.horizontal(|ui| {
                        for (val, label) in [(false, "左"), (true, "右")] {
                            let sel = self.panel_right == val;
                            let mut b = egui::Button::new(
                                egui::RichText::new(label)
                                    .color(if sel { TAB_TEXT_ACTIVE } else { TAB_TEXT }),
                            )
                            .min_size(Vec2::new(46.0, 26.0))
                            .corner_radius(5.0)
                            .fill(if sel {
                                Color32::from_gray(56)
                            } else {
                                Color32::from_gray(34)
                            });
                            if sel {
                                b = b.stroke(egui::Stroke::new(1.0, ACCENT_GREEN));
                            }
                            if ui.add(b).clicked() {
                                self.panel_right = val;
                            }
                        }
                    });
                    ui.add_space(12.0);

                    ui.label(egui::RichText::new("既定テーマ — 背景").color(TAB_TEXT));
                    palette_row(ui, &mut self.default_bg, &BG_DISPLAY, &BG_APPLY);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("既定テーマ — アクセント").color(TAB_TEXT));
                    palette_row(ui, &mut self.default_accent, &ACCENT_PALETTE, &ACCENT_PALETTE);
                    ui.add_space(10.0);
                    if primary_button(ui, "既定テーマを全タブに適用") {
                        let (b, a) = (self.default_bg, self.default_accent);
                        for s in &mut self.sessions {
                            s.bg = b;
                            s.accent = a;
                        }
                    }
                }

                // ===== プリセットタブ =====
                if self.settings_tab == SettingsTab::Presets {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("プリセット").color(TAB_TEXT));
                        let status = if self.presets.is_empty() {
                            "未登録＝起動タブ／新規タブで選べる型がありません".to_string()
                        } else {
                            format!("{} 個", self.presets.len())
                        };
                        ui.label(
                            egui::RichText::new(status).color(Color32::from_gray(150)).size(12.0),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "再利用できるタブ定義（型）。名前は一意に。起動タブや新規タブから選んで実体化します。",
                        )
                        .color(Color32::from_gray(130))
                        .size(11.0),
                    );
                    ui.add_space(6.0);
                    if primary_button(ui, "＋ 追加") {
                        let name = unique_preset_name("プリセット", &self.presets);
                        self.presets.push(TabSpec { name, ..Default::default() });
                    }
                    ui.add_space(8.0);

                    // 名前の重複／空を検出して警告（参照できない型を可視化＝silent fail 回避）。
                    let mut seen = std::collections::HashSet::new();
                    let mut bad: Vec<String> = Vec::new();
                    let mut has_empty = false;
                    for p in &self.presets {
                        if p.name.trim().is_empty() {
                            has_empty = true;
                        } else if !seen.insert(p.name.as_str()) && !bad.contains(&p.name) {
                            bad.push(p.name.clone());
                        }
                    }
                    if has_empty {
                        bad.insert(0, "(空の名前)".to_string());
                    }
                    let warning = if bad.is_empty() {
                        String::from(" ")
                    } else {
                        format!("⚠ 参照できない名前（重複/空）: {}", bad.join(" , "))
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(warning).color(WARN_COLOR).size(12.0))
                            .truncate(),
                    );
                    ui.add_space(6.0);

                    let mut to_remove: Option<usize> = None;
                    let mut to_move: Option<(usize, isize)> = None;
                    let mut to_dup: Option<usize> = None;
                    let pointer = ctx.input(|i| i.pointer.interact_pos());
                    let mut card_rects: Vec<egui::Rect> = Vec::new();
                    let avail_h = ui.available_height();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(avail_h)
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .show(ui, |ui| {
                            let n = self.presets.len();
                            for i in 0..n {
                                let (act, rect) = tab_spec_card(
                                    ui,
                                    ctx,
                                    "preset",
                                    i,
                                    &mut self.presets[i],
                                    &self.shells,
                                    &self.shell_icons,
                                    self.default_bg,
                                    self.default_accent,
                                    panel_right,
                                    &mut self.preset_shell_open,
                                    &mut self.preset_color_open,
                                    &mut self.card_drag,
                                    pointer,
                                    true,
                                    None,
                                );
                                card_rects.push(rect);
                                match act {
                                    CardAction::Remove => to_remove = Some(i),
                                    CardAction::Move(d) => to_move = Some((i, d)),
                                    CardAction::Duplicate => to_dup = Some(i),
                                    CardAction::None => {}
                                }
                            }
                            let ghost =
                                self.card_drag.and_then(|d| self.presets.get(d.from)).map(|p| {
                                    let gbg = color_from_hex(&p.bg).unwrap_or(self.default_bg);
                                    let gac =
                                        color_from_hex(&p.accent).unwrap_or(self.default_accent);
                                    (p.clone(), gbg, gac)
                                });
                            if let Some((from, target)) = card_dnd_resolve(
                                ui,
                                &mut self.card_drag,
                                &card_rects,
                                pointer,
                                panel_right,
                                ghost,
                                &self.shells,
                                &self.shell_icons,
                            ) {
                                move_card(&mut self.presets, from, target);
                            }
                        });
                    // 複製：直後に挿入し、名前は一意化（型の識別子を衝突させない）。
                    if let Some(i) = to_dup {
                        if let Some(mut c) = self.presets.get(i).cloned() {
                            c.name = unique_preset_name(&c.name, &self.presets);
                            self.presets.insert(i + 1, c);
                        }
                    }
                    if let Some(i) = to_remove
                        && i < self.presets.len()
                    {
                        self.presets.remove(i);
                    }
                    if let Some((i, dir)) = to_move {
                        let j = i as isize + dir;
                        if j >= 0 && (j as usize) < self.presets.len() {
                            self.presets.swap(i, j as usize);
                        }
                    }
                }

                // ===== 起動タブタブ =====
                if self.settings_tab == SettingsTab::Startup {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("起動タブ").color(TAB_TEXT));
                        let status = if self.startup.is_empty() {
                            "未設定＝起動時は既定シェル1枚".to_string()
                        } else {
                            format!("{} 枚", self.startup.len())
                        };
                        ui.label(
                            egui::RichText::new(status).color(Color32::from_gray(150)).size(12.0),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, "＋ 追加") {
                            // マニュアルエントリを1枚追加（プリセット参照の追加はスライスBで）。
                            self.startup.push(StartupEntry::default());
                        }
                        if ghost_button(ui, "現在のタブ構成を抜き出す") {
                            let snap = self.snapshot_startup();
                            self.startup = snap;
                        }
                        if ghost_button(ui, "クリア") {
                            self.startup.clear();
                        }
                    });
                    ui.add_space(8.0);

                    let mut to_remove: Option<usize> = None;
                    let mut to_move: Option<(usize, isize)> = None;
                    let pointer = ctx.input(|i| i.pointer.interact_pos());
                    let mut card_rects: Vec<egui::Rect> = Vec::new();
                    let avail_h = ui.available_height();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(avail_h)
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .show(ui, |ui| {
                            let n = self.startup.len();
                            for i in 0..n {
                                // 行ごとにマニュアル／プリセット参照を切替（tab_spec_card にトグルを渡す）。
                                let entry = &mut self.startup[i];
                                let (act, rect) = tab_spec_card(
                                    ui,
                                    ctx,
                                    "startup",
                                    i,
                                    &mut entry.spec,
                                    &self.shells,
                                    &self.shell_icons,
                                    self.default_bg,
                                    self.default_accent,
                                    panel_right,
                                    &mut self.startup_shell_open,
                                    &mut self.startup_color_open,
                                    &mut self.card_drag,
                                    pointer,
                                    false,
                                    Some(StartupToggle {
                                        use_preset: &mut entry.use_preset,
                                        preset_ref: &mut entry.preset,
                                        presets: &self.presets,
                                        pick_open: &mut self.startup_pick_open,
                                    }),
                                );
                                card_rects.push(rect);
                                match act {
                                    CardAction::Remove => to_remove = Some(i),
                                    CardAction::Move(d) => to_move = Some((i, d)),
                                    _ => {}
                                }
                            }

                            // D&D：掴んだカードの内容をクローンしてゴーストへ渡し、ドロップで移動確定。
                            let ghost = self
                                .card_drag
                                .and_then(|d| self.startup.get(d.from))
                                .map(|e| {
                                    let gbg =
                                        color_from_hex(&e.spec.bg).unwrap_or(self.default_bg);
                                    let gac = color_from_hex(&e.spec.accent)
                                        .unwrap_or(self.default_accent);
                                    (e.spec.clone(), gbg, gac)
                                });
                            if let Some((from, target)) = card_dnd_resolve(
                                ui,
                                &mut self.card_drag,
                                &card_rects,
                                pointer,
                                panel_right,
                                ghost,
                                &self.shells,
                                &self.shell_icons,
                            ) {
                                move_card(&mut self.startup, from, target);
                            }
                        });
                    if let Some(i) = to_remove
                        && i < self.startup.len()
                    {
                        self.startup.remove(i);
                    }
                    if let Some((i, dir)) = to_move {
                        let j = i as isize + dir;
                        if j >= 0 && (j as usize) < self.startup.len() {
                            self.startup.swap(i, j as usize);
                        }
                    }
                }

                // ===== システムタブ =====
                if self.settings_tab == SettingsTab::System {
                    ui.label(egui::RichText::new("バージョン").color(TAB_TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "LustTermina v{}",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .color(Color32::from_gray(150))
                        .size(12.0),
                    );
                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(10.0);

                    ui.label(egui::RichText::new("データ").color(TAB_TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "設定は %APPDATA%\\LustTermina\\config.toml に保存されます。",
                        )
                        .color(Color32::from_gray(150))
                        .size(12.0),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, "データフォルダを開く") {
                            if let Some(dir) = config_dir() {
                                let _ = std::fs::create_dir_all(&dir);
                                let _ =
                                    std::process::Command::new("explorer").arg(&dir).spawn();
                            }
                        }
                        if danger_button(ui, "データを初期化") {
                            if confirm_yes_no(
                                "設定（config.toml）を削除し、すべて既定値に戻します。\nこの操作は元に戻せません。",
                                "データの初期化",
                            ) {
                                self.reset_settings();
                                self.show_toast("データを初期化しました");
                            }
                        }
                    });

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("アンインストール").color(TAB_TEXT));
                    ui.add_space(2.0);
                    if let Some(scope) = installed_scope() {
                        ui.label(
                            egui::RichText::new(
                                "アプリ本体・スタートメニュー・右クリック項目を削除します（データ削除は選択式）。",
                            )
                            .color(Color32::from_gray(150))
                            .size(12.0),
                        );
                        ui.add_space(8.0);
                        if danger_button(ui, "アンインストール") {
                            // 確認とデータ削除選択は共通の TaskDialog（--uninstall と同じ）。
                            if let Some(delete) = ask_uninstall() {
                                log::info!("closing after uninstall (delete_data={delete})");
                                perform_uninstall(scope, delete);
                                // 本体を閉じてファイルロックを解放し、インストール先の自己削除を完了させる。
                                // 内部由来の終了なので、終了確認は挟まない。
                                self.allow_close = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "このアプリはインストールされていません（開発ビルド等）。",
                            )
                            .color(Color32::from_gray(150))
                            .size(12.0),
                        );
                    }
                }
            });
            // パネルが右ならナビを内容の右側に出す（タブパネルと同じ向き）。
            if panel_right {
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(nav_w);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::General, "既定", true);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Theme, "UI/テーマ", true);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Presets, "プリセット", true);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::Startup, "起動タブ", true);
                    ui.add_space(2.0);
                    settings_nav_item(ui, &mut self.settings_tab, SettingsTab::System, "システム", true);
                });
            }
        });

        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            // 自動保存はしない（保存は明示ボタンのみ）。閉じるだけ。
            self.settings_open = false;
            self.shell_list_open = false;
            self.startup_shell_open = None;
        }
    }

    // ドラッグ中タブ(from)を、挿入境界 target へ並べ替える。
    // target は「中心が掴み位置より上にあるタブ数」＝元リスト座標での挿入境界(0..=len)。
    // remove→insert でズレるぶんを補正し、移動したタブをアクティブに保つ。
    pub(crate) fn move_tab(&mut self, from: usize, target: usize) {
        if from >= self.sessions.len() {
            return;
        }
        let s = self.sessions.remove(from);
        let insert = if target > from { target - 1 } else { target }.min(self.sessions.len());
        self.sessions.insert(insert, s);
        self.active = insert; // 掴んだタブはドラッグ開始時にアクティブ化済み
    }
}
impl App {
    // ウィンドウを表に戻す（隠していないときは前面に出すだけ）。トレイのクリック、
    // Explorer からの合流（新しいタブが生えた）で使う。
    fn show_window(&mut self, ctx: &egui::Context) {
        if self.hidden {
            log::info!("showing window from tray");
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            self.hidden = false;
        }
        // Visible(true) はこのフレームの描画後に適用されるため、同じフレームで前面化しても
        // まだ不可視のウィンドウが相手で効かない。数フレーム置いてから前面化する。
        self.pending_front = FRONT_DELAY_FRAMES;
        ctx.request_repaint();
    }

    // ウィンドウだけ消して、シェル（PTY と読み取りスレッド）は動かし続ける。
    // トレイアイコンが出せていないときは隠さない＝戻す入口の無いプロセスを作らない。
    fn hide_window(&mut self, ctx: &egui::Context) {
        if !tray_ready() {
            self.show_toast("通知領域に常駐できていないため、隠せません");
            return;
        }
        log::info!("hiding window to tray");
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.hidden = true;
    }
}

// 終了時にトレイアイコンを消す。残すとプロセスが居なくなった後も、通知領域を
// マウスが通るまでアイコンの残骸が表示され続ける。
impl Drop for App {
    fn drop(&mut self) {
        log::info!("app dropped with {} tab(s)", self.sessions.len());
        remove_tray();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ウィンドウを閉じる操作（タイトルバーの×・Alt+F4・タスクバー・タスクバーの右クリック）は、
        // 一度差し止めて確認を挟む。誤クリック1回で全タブのシェル（実行中のプロセス）が
        // まとめて落ちるのを防ぐため。allow_close が立っているときだけ素通しする＝
        // 「確認で終了を選んだ」「内部から意図して閉じた」の2経路のみが実際の終了に到達する。
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            log::info!("close requested; asking for confirmation");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = None; // モーダルを2枚重ねない（タブの確認は畳む）
            self.confirm_exit = true;
        }

        // 本体ウィンドウのハンドルをトレイ側へ預ける（最小化からの復元に使う）。
        // Frame はこの中でしか手に入らず、ウィンドウは初回フレームより後に確定することも
        // あるため、未設定のうちは毎フレーム試す。
        if !self.hwnd_shared {
            if let Some(hwnd) = main_hwnd(frame) {
                set_main_window(hwnd);
                self.hwnd_shared = true;
            }
        }

        // トレイアイコンで押されたものを回収して実行する。可視状態の管理は eframe 側に一本化し、
        // ここでビューポート命令に変換する（wndproc から直接 ShowWindow は呼ばない）。
        match take_tray_request() {
            TrayRequest::Show => self.show_window(&ctx),
            TrayRequest::Hide => self.hide_window(&ctx),
            TrayRequest::Exit => {
                // 隠したまま確認を出すと見えないので、先に窓を戻してから通常の終了確認へ。
                self.show_window(&ctx);
                self.confirm_exit = true;
            }
            TrayRequest::None => {}
        }
        // 再表示の予約：Visible(true) が効いたころ合いで前面に出す（show_window のコメント）。
        if self.pending_front > 0 {
            self.pending_front -= 1;
            if self.pending_front == 0 {
                bring_to_front(frame);
            }
            ctx.request_repaint();
        }

        // 枠（タイトルバー）のダークが外部要因で剥がれることがあるため、
        // フォーカス獲得時＋一定間隔で再適用する（詳細は apply_dark_titlebar のコメント）。
        let focused = ctx.input(|i| i.focused);
        if (focused && !self.titlebar_was_focused)
            || self
                .titlebar_last_dark
                .map_or(true, |t| t.elapsed() >= TITLEBAR_REDARK_INTERVAL)
        {
            apply_dark_titlebar(frame);
            self.titlebar_last_dark = Some(std::time::Instant::now());
        }
        self.titlebar_was_focused = focused;
        // 入力も PTY 出力も無い完全アイドル時はフレームが走らず、上の再適用も
        // 動けない。次のフレームを間隔付きで予約し、放置中でも自己修復させる。
        ctx.request_repaint_after(TITLEBAR_REDARK_INTERVAL);

        // 別インスタンスから転送された「このフォルダで開く」要求を取り込む。
        // 借用を切るため一旦集めてから開く（open_dir_tab が self を可変借用するため）。
        if self.ipc_rx.is_some() {
            let msgs: Vec<IpcMsg> =
                self.ipc_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
            let forwarded = !msgs.is_empty();
            for m in msgs {
                match m {
                    IpcMsg::OpenDir(d) => self.open_dir_tab(&ctx, &d),
                    IpcMsg::RunScript(p) => self.open_script_tab(&ctx, &p),
                    IpcMsg::OpenTab(req) => self.open_requested_tab(&ctx, req),
                }
            }
            // Explorer 等から転送されてタブが生えたら、窓を前面へ出す（最小化なら復元、
            // 裏で動かしている最中なら再表示）。転送したのに何も起きないと、送った側からは
            // 失敗と区別が付かないため、隠している状態こそ必ず表に戻す。
            if forwarded {
                self.show_window(&ctx);
            }
        }

        for (i, sess) in self.sessions.iter_mut().enumerate() {
            sess.note_exit();
            if self.editing != Some(i) {
                sess.note_props();
            }
        }

        // スクリプト実行タブ（close_on_exit）: 子プロセスの終了を監視する。
        // 正常終了(exit 0)なら閉じる（最後の1枚ならダブルクリック完結として窓ごと閉じる）。
        // 失敗(非0)や監視不能なら閉じずに出力を残す＝失敗を握り潰さない。
        {
            let mut i = 0;
            while i < self.sessions.len() {
                if self.sessions[i].close_on_exit {
                    let res = self.sessions[i].child.try_wait();
                    match res {
                        Ok(Some(status)) => {
                            if status.success() {
                                if self.sessions.len() == 1 {
                                    // スクリプトの完走による終了＝ユーザーの意図どおりなので確認は挟まない。
                                    log::info!("closing after the last script tab exited successfully");
                                    self.allow_close = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    self.sessions[i].close_on_exit = false; // 多重送信を避ける
                                } else {
                                    self.sessions.remove(i);
                                    if self.active >= self.sessions.len() {
                                        self.active = self.sessions.len().saturating_sub(1);
                                    } else if self.active > i {
                                        self.active -= 1;
                                    }
                                    continue; // 詰めたので同じ i を再検査
                                }
                            } else {
                                self.sessions[i].close_on_exit = false; // 失敗＝出力を残す
                            }
                        }
                        Ok(None) => {}                                    // まだ実行中
                        Err(_) => self.sessions[i].close_on_exit = false, // 監視不能なら諦める
                    }
                }
                i += 1;
            }
        }

        // ウィンドウへのファイル D&D ＝右クリック「LustTermina で開く／実行」の D&D 版。
        // フォルダ→そのフォルダで開く、スクリプト→設定シェルで実行、その他ファイル→親フォルダを開く。
        // 借用を切るため先にパスを集めてから処理する（open_* が self を可変借用するため）。
        let dropped: Vec<std::path::PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for path in dropped {
            if path.is_dir() {
                self.open_dir_tab(&ctx, &path.to_string_lossy());
            } else if is_script_path(&path) {
                self.open_script_tab(&ctx, &path.to_string_lossy());
            } else if let Some(parent) = path.parent().and_then(|d| d.to_str()) {
                self.open_dir_tab(&ctx, parent);
            }
        }
        // ドラッグ中は全面オーバーレイで「ドロップで何が起きるか」を予告する。
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ui.max_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(150));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "ドロップで開く（スクリプトは実行）",
                FontId::proportional(20.0),
                TAB_TEXT_ACTIVE,
            );
        }

        // Ctrl+Tab で一つ下、Ctrl+Shift+Tab で一つ上のタブへ（端は循環）
        let n = self.sessions.len();
        if n > 0
            && self.editing.is_none()
            && self.confirm_close.is_none()
            && !self.confirm_exit
            && !self.settings_open
        {
            let (next, prev) = ctx.input(|i| {
                let ct = i.modifiers.ctrl && i.key_pressed(egui::Key::Tab);
                (ct && !i.modifiers.shift, ct && i.modifiers.shift)
            });
            if next {
                self.active = (self.active + 1) % n;
            } else if prev {
                self.active = (self.active + n - 1) % n;
            }
        }

        // 縦タブ（サイドパネル）。1タブ = 1セッション。設定で左右どちらにも置ける。
        let mut to_close: Option<usize> = None;
        let mut to_add_startup: Option<usize> = None;
        let mut to_toggle_record: Option<usize> = None;
        // 設定ページ表示中はタブパネルを隠し、設定を窓いっぱいに表示する。
        if !self.settings_open {
        let tab_panel = if self.panel_right {
            egui::Panel::right("tabs")
        } else {
            egui::Panel::left("tabs")
        };
        // 余白も左右反転：本文と接する辺は 0、窓の外側辺に 8。
        let panel_margin = if self.panel_right {
            egui::Margin { left: 0, right: 8, top: 8, bottom: 8 }
        } else {
            egui::Margin { left: 8, right: 0, top: 8, bottom: 8 }
        };
        tab_panel
            .resizable(false)
            .exact_size(180.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(SIDEBAR_BG).inner_margin(panel_margin))
            .show(ui, |ui| {
                // 設定（歯車）はパネル最下部に固定。タブの増減で位置が動かないよう、
                // スクロール領域の前に下部パネルとして確保する。
                egui::Panel::bottom("settings_bar")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(
                        egui::Frame::new()
                            .fill(SIDEBAR_BG)
                            .inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 2 }),
                    )
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        let gear = ui.add_sized(
                            [w, 28.0],
                            egui::Button::new(
                                egui::RichText::new("⚙ 設定").color(Color32::from_gray(170)),
                            )
                            .fill(Color32::TRANSPARENT),
                        );
                        if gear.clicked() {
                            self.settings_open = true;
                            self.settings_tab = SettingsTab::General;
                        }
                    });

                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        // ドロップ位置の算定に使うため、各タブの矩形を順に集める。
                        let mut tab_rects: Vec<egui::Rect> = Vec::with_capacity(self.sessions.len());
                        let pointer = ui.input(|i| i.pointer.interact_pos());
                        for i in 0..self.sessions.len() {
                            let is_dragged = self.drag.map_or(false, |d| d.from == i);
                            // タブごとに ID を固定。編集⇔表示で widget 構成が変わっても
                            // 他タブ／＋ボタンの自動 ID と衝突しない（赤枠の ID clash 防止）。
                            let rect = ui.push_id(i, |ui| {
                                if self.editing == Some(i) {
                                    // タブ名インライン編集。見た目はアクティブタブのまま
                                    //（本文色の地＋左角丸）にして、枠なしテキスト欄を重ねる。
                                    let w = ui.available_width();
                                    let (tab_bg, tab_accent) =
                                        (self.sessions[i].bg, self.sessions[i].accent);
                                    let (rect, _) =
                                        ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::hover());
                                    let mirror = self.panel_right;
                                    let radius = if mirror {
                                        egui::CornerRadius { nw: 0, ne: 7, sw: 0, se: 7 }
                                    } else {
                                        egui::CornerRadius { nw: 7, ne: 0, sw: 7, se: 0 }
                                    };
                                    ui.painter().rect_filled(rect, radius, tab_bg);
                                    let bar = if mirror {
                                        egui::Rect::from_min_max(
                                            Pos2::new(rect.right() - 4.0, rect.top() + 6.0),
                                            Pos2::new(rect.right(), rect.bottom() - 6.0),
                                        )
                                    } else {
                                        egui::Rect::from_min_max(
                                            Pos2::new(rect.left(), rect.top() + 6.0),
                                            Pos2::new(rect.left() + 4.0, rect.bottom() - 6.0),
                                        )
                                    };
                                    ui.painter().rect_filled(bar, 2.0, tab_accent);
                                    // 編集欄の内側矩形。アクセントバー側に余白を空ける。
                                    let inner = if mirror {
                                        egui::Rect::from_min_max(
                                            rect.min + Vec2::new(8.0, 0.0),
                                            Pos2::new(rect.right() - 12.0, rect.bottom()),
                                        )
                                    } else {
                                        egui::Rect::from_min_max(
                                            Pos2::new(rect.left() + 12.0, rect.top()),
                                            rect.max - Vec2::new(8.0, 0.0),
                                        )
                                    };
                                    let edit = ui.put(
                                        inner,
                                        egui::TextEdit::singleline(&mut self.sessions[i].title)
                                            .frame(egui::Frame::NONE)
                                            .margin(egui::Margin::same(0))
                                            .vertical_align(egui::Align::Center)
                                            .font(egui::FontId::proportional(14.0))
                                            .text_color(TAB_TEXT_ACTIVE),
                                    );
                                    if self.editing_init {
                                        edit.request_focus();
                                        self.editing_init = false;
                                    }
                                    // Enter またはフォーカス外しで確定
                                    if edit.lost_focus() {
                                        if self.sessions[i].title.trim().is_empty() {
                                            self.sessions[i].title = format!("pwsh {}", i + 1);
                                        }
                                        self.editing = None;
                                    }
                                    rect
                                } else {
                                    let selected = i == self.active;
                                    let (resp, close, add_startup, toggle_record) = draw_tab(
                                        ui,
                                        i,
                                        &mut self.sessions[i],
                                        selected,
                                        is_dragged,
                                        self.panel_right,
                                    );
                                    // ドラッグ開始：掴んだタブをアクティブにして D&D 状態を持つ。
                                    // grab_dy はゴーストをポインタに自然に追従させるためのオフセット。
                                    if resp.drag_started() {
                                        if let Some(p) = pointer {
                                            self.active = i;
                                            self.drag = Some(TabDrag {
                                                from: i,
                                                grab_dy: p.y - resp.rect.top(),
                                            });
                                        }
                                    } else if resp.double_clicked() {
                                        self.active = i;
                                        self.editing = Some(i);
                                        self.editing_init = true;
                                    } else if resp.clicked() {
                                        self.active = i;
                                    }
                                    if close {
                                        to_close = Some(i);
                                    }
                                    if add_startup {
                                        to_add_startup = Some(i);
                                    }
                                    if toggle_record {
                                        to_toggle_record = Some(i);
                                    }
                                    resp.rect
                                }
                            })
                            .inner;
                            tab_rects.push(rect);
                            ui.add_space(3.0);
                        }

                        // ---- D&D：挿入線＋ポインタ追従ゴースト、ドロップで確定 ----
                        if let Some(drag) = self.drag {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                            // ポインタが押されていない＝ドロップ。座標が取れない時も終了扱い。
                            let released = ui.input(|i| !i.pointer.any_down());
                            if let (Some(p), false) = (pointer, drag.from >= tab_rects.len()) {
                                // 中心が掴み位置より上にあるタブ数＝挿入境界(0..=len)。
                                let target = tab_rects.iter().filter(|r| p.y > r.center().y).count();
                                let accent = self.sessions[drag.from].accent;
                                let (x0, x1) = tab_rects
                                    .first()
                                    .map(|r| (r.left(), r.right()))
                                    .unwrap_or((0.0, 0.0));
                                // 挿入線（境界の少し上、末尾なら最後のタブ下）
                                let line_y = if target < tab_rects.len() {
                                    tab_rects[target].top() - 2.0
                                } else {
                                    tab_rects.last().map_or(0.0, |r| r.bottom() + 1.0)
                                };
                                ui.painter().hline(
                                    egui::Rangef::new(x0, x1),
                                    line_y,
                                    egui::Stroke::new(2.0, accent),
                                );
                                // ゴースト（前面レイヤにアクティブタブ風の見た目で描く）
                                let s = &self.sessions[drag.from];
                                let top = p.y - drag.grab_dy;
                                let grect = egui::Rect::from_min_size(
                                    Pos2::new(x0, top),
                                    Vec2::new(x1 - x0, 30.0),
                                );
                                let gp = ui.ctx().layer_painter(egui::LayerId::new(
                                    egui::Order::Tooltip,
                                    egui::Id::new("tab_drag_ghost"),
                                ));
                                gp.rect_filled(grect, 6.0, s.bg);
                                gp.rect_stroke(
                                    grect,
                                    6.0,
                                    egui::Stroke::new(1.0, s.accent),
                                    egui::StrokeKind::Inside,
                                );
                                let mirror = self.panel_right;
                                let bar = if mirror {
                                    egui::Rect::from_min_max(
                                        Pos2::new(grect.right() - 4.0, grect.top() + 6.0),
                                        Pos2::new(grect.right(), grect.bottom() - 6.0),
                                    )
                                } else {
                                    egui::Rect::from_min_max(
                                        Pos2::new(grect.left(), grect.top() + 6.0),
                                        Pos2::new(grect.left() + 4.0, grect.bottom() - 6.0),
                                    )
                                };
                                gp.rect_filled(bar, 2.0, s.accent);
                                gp.text(
                                    Pos2::new(grect.left() + 12.0, grect.center().y),
                                    Align2::LEFT_CENTER,
                                    &s.title,
                                    FontId::proportional(14.0),
                                    TAB_TEXT_ACTIVE,
                                );
                                if released {
                                    self.move_tab(drag.from, target);
                                }
                            }
                            if released {
                                self.drag = None;
                            }
                            ui.ctx().request_repaint(); // ドラッグ中は追従のため毎フレーム
                        }
                        // 「新しいタブ」はタブ一覧の下に続く（ID 固定）
                        ui.add_space(6.0);
                        ui.push_id("new_tab", |ui| {
                            let w = ui.available_width();
                            let add = ui.add_sized(
                                [w, 30.0],
                                egui::Button::new(
                                    egui::RichText::new("＋ 新しいタブ")
                                        .color(Color32::from_gray(210)),
                                )
                                .fill(Color32::from_gray(30)),
                            );
                            // クリックでシェル選択メニュー（アイコン付きリストは共通ヘルパ）。
                            // プリセットがあれば下段に並べ、選ぶと型から実体化する。
                            egui::Popup::menu(&add).show(|ui| {
                                ui.set_min_width(170.0);
                                let chosen = shell_list(ui, &self.shells, &self.shell_icons, None);
                                if let Some(idx) = chosen {
                                    let spec = self.shells[idx].clone();
                                    let cwd = self.default_cwd.clone();
                                    let id = self.next_id;
                                    self.next_id += 1;
                                    match Session::spawn(
                                        &ctx,
                                        format!("{} {}", spec.short, id),
                                        &spec,
                                        &cwd,
                                    ) {
                                        Ok(mut sess) => {
                                            sess.bg = self.default_bg;
                                            sess.accent = self.default_accent;
                                            self.sessions.push(sess);
                                            self.active = self.sessions.len() - 1;
                                        }
                                        Err(e) => self.tab_open_failed(e),
                                    }
                                }
                                if !self.presets.is_empty() {
                                    ui.separator();
                                    ui.label(
                                        egui::RichText::new("プリセット")
                                            .color(Color32::from_gray(140))
                                            .size(11.0),
                                    );
                                    let mut chosen_preset: Option<usize> = None;
                                    for (pi, p) in self.presets.iter().enumerate() {
                                        // 名前が空の型は参照できないので出さない（silent fail 回避）。
                                        if p.name.trim().is_empty() {
                                            continue;
                                        }
                                        let sidx = self.shells.iter().position(|s| s.short == p.shell);
                                        let btn = match sidx
                                            .and_then(|k| self.shell_icons.get(k))
                                            .and_then(|o| o.as_ref())
                                        {
                                            Some(tex) => {
                                                let img = egui::Image::new(tex)
                                                    .fit_to_exact_size(Vec2::splat(16.0));
                                                egui::Button::image_and_text(img, p.name.as_str())
                                                    .fill(Color32::TRANSPARENT)
                                            }
                                            None => egui::Button::new(p.name.as_str())
                                                .fill(Color32::TRANSPARENT),
                                        };
                                        if ui.add(btn).clicked() {
                                            chosen_preset = Some(pi);
                                        }
                                    }
                                    if let Some(pi) = chosen_preset {
                                        let spec = self.presets[pi].clone();
                                        self.spawn_tab_from_spec(&ctx, &spec);
                                    }
                                }
                            });
                        });

                    });
            });
        } // /設定中はタブパネル非表示

        // × は即閉じず、確認ダイアログ待ちにする
        if let Some(i) = to_close {
            self.confirm_close = Some(i);
        }

        // 右クリック「起動タブに追加」：このタブを起動タブ定義へ追加し即保存。
        // 完全に同一の定義が既にあれば重複追加しない。
        if let Some(i) = to_toggle_record {
            let max_kb = self.log_settings.cast_max_kb;
            if let Some(s) = self.sessions.get_mut(i) {
                let on = !s.is_recording();
                if let Err(e) = s.set_recording(on, max_kb) {
                    self.show_toast(format!("画面の記録を始められませんでした: {e}"));
                }
            }
        }
        if let Some(i) = to_add_startup {
            if let Some(s) = self.sessions.get(i) {
                let spec = tab_spec_of(s);
                // 名前・シェル・cwd が一致するマニュアル起動エントリが既にあれば重複追加しない。
                let dup = self.startup.iter().any(|e| {
                    !e.is_preset()
                        && e.spec.name == spec.name
                        && e.spec.shell == spec.shell
                        && e.spec.cwd == spec.cwd
                });
                if !dup {
                    self.startup.push(StartupEntry { spec, ..Default::default() });
                    self.save_settings();
                }
            }
        }

        let font = FontId::monospace(FONT_SIZE);
        // セル幅・高さを物理ピクセル境界に丸める。端数だと列ごとに 1px ズレて
        // 文字間隔が不揃いに見える（等幅なのにガタつく）ため。
        let ppp = ctx.pixels_per_point();
        let (cw, ch) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let cell_w = (cw * ppp).round().max(1.0) / ppp;
        let cell_h = (ch * ppp).round().max(1.0) / ppp;

        // タブ名編集中／確認ダイアログ／設定モーダル表示中は端末入力を止める
        let block_input = self.editing.is_some()
            || self.confirm_close.is_some()
            || self.confirm_exit
            || self.settings_open;

        // 入力・スクロールはアクティブなセッションにのみ流す（設定ページ表示中は止める）。
        let active_idx = self.active;
        if !block_input {
            if let Some(active) = self.sessions.get_mut(active_idx) {
                if active.handle_input(&ctx) {
                    active.scroll_to_bottom();
                }
            }
        }

        // 中央領域：設定中は設定ページ、通常はアクティブ端末。
        if self.settings_open {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_gray(22))
                        .inner_margin(egui::Margin::same(20)),
                )
                .show(ui, |ui| {
                    self.settings_page(ui, &ctx);
                });
        } else if self.sessions.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(DEFAULT_BG))
                .show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("開いているタブがありません。＋ から新しいタブを開いてください。")
                                .color(TAB_TEXT),
                        );
                    });
                });
        } else {
            // body の背景もアクティブセッションの bg にする（タブと地続き＋色分け）
            let body_bg = self.sessions[active_idx].bg;
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(body_bg))
                .show(ui, |ui| {
                    self.sessions[active_idx]
                        .draw(ui, &ctx, &font, cell_w, cell_h, !block_input);
                });
        }

        // 終了確認ダイアログ（ウィンドウの×等の誤操作対策）。開いているシェルが全部落ちる
        // ＝タブ1枚を閉じるより影響が大きいので、こちらを優先して1枚だけ出す。
        if self.confirm_exit {
            let n = self.sessions.len();
            // 「裏で動かす」はトレイアイコンが出ているときだけ並べる。押しても失敗する
            // ボタンを見せない＝戻る道が無い状態を選ばせない。
            let alt = tray_ready().then_some("裏で動かす");
            let detail = if alt.is_some() {
                format!(
                    "開いている {n} 個のタブのシェルが、すべて終了します。\n\
                     裏で動かせば、ウィンドウを閉じてもシェルは動いたまま通知領域に残ります。"
                )
            } else {
                format!("開いている {n} 個のタブのシェルが、すべて終了します。")
            };
            match confirm_modal(
                &ctx,
                "confirm_exit",
                "LustTermina を終了しますか？",
                &detail,
                "終了する",
                alt,
            ) {
                Some(ConfirmChoice::Confirm) => {
                    log::info!("exit confirmed by user");
                    // 確認を通った終了だけが素通しの印を持つ。次フレームの close_requested で実際に閉じる。
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(ConfirmChoice::Alt) => {
                    // 終了せず、ウィンドウだけ畳む。シェルと PTY はそのまま動き続ける。
                    self.confirm_exit = false;
                    self.hide_window(&ctx);
                }
                Some(ConfirmChoice::Cancel) => self.confirm_exit = false,
                None => {}
            }
        }
        // タブを閉じる確認ダイアログ（誤クリック対策）
        else if let Some(idx) = self.confirm_close {
            let title = self
                .sessions
                .get(idx)
                .map(|s| s.title.clone())
                .unwrap_or_default();
            match confirm_modal(
                &ctx,
                "confirm_close",
                "タブを閉じますか？",
                &format!("「{title}」"),
                "閉じる",
                None,
            ) {
                Some(ConfirmChoice::Confirm) => {
                    if idx < self.sessions.len() {
                        self.sessions.remove(idx);
                        if self.active >= self.sessions.len() {
                            self.active = self.sessions.len().saturating_sub(1);
                        } else if self.active > idx {
                            self.active -= 1;
                        }
                    }
                    self.confirm_close = None;
                }
                Some(_) => self.confirm_close = None, // キャンセル（第三の選択肢は出していない）
                None => {}
            }
        }

        // トースト通知（保存しました 等）。約2秒で自動的にフェードアウト。
        if let Some((msg, t)) = self.toast.clone() {
            let elapsed = t.elapsed().as_secs_f32();
            let life = 2.2;
            if elapsed >= life {
                self.toast = None;
            } else {
                let alpha = if elapsed > life - 0.5 {
                    ((life - elapsed) / 0.5).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let a = (alpha * 255.0) as u8;
                egui::Area::new(egui::Id::new("toast"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -28.0))
                    .show(&ctx, |ui| {
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(0x2A, 0x2A, 0x2A, a))
                            .stroke(egui::Stroke::new(
                                1.0,
                                Color32::from_rgba_unmultiplied(0x3C, 0x7A, 0x5A, a),
                            ))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(16, 10))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&msg)
                                        .color(Color32::from_rgba_unmultiplied(
                                            0xEC, 0xEC, 0xEC, a,
                                        ))
                                        .size(13.0),
                                );
                            });
                    });
                ctx.request_repaint();
            }
        }
    }
}

