//! 設定（Settings）・タブプリセット（TabSpec/StartupEntry）・保存/読込。
use eframe::egui::Color32;
use crate::session::{default_startup_quiet_ms, default_startup_max_wait_ms};

// ---- ドラッグ中のタブ（D&D 並べ替え）----
// from＝掴んだ時点のインデックス、grab_dy＝タブ上端から掴んだ点までのYオフセット
//（ゴーストをポインタに自然に追従させるため）。並べ替えの確定はドロップ時のみ。
#[derive(Clone, Copy)]
pub(crate) struct TabDrag {
    pub(crate) from: usize,
    pub(crate) grab_dy: f32,
}

// ---- 設定の永続化（TOML。%APPDATA%\LustTermina\config.toml）----
// 既定シェルは index ではなく short 名で保存（探索結果は環境で変わるため index は脆い）。
// 色は "#RRGGBB" 文字列で保存（手編集しやすく、toml にも素直）。
// タブ1枚ぶんのインライン定義（名前・シェル・cwd・色・起動コマンド）。空文字の項目は
// 既定にフォールバックする。空文字＝省略は toml 上も出力しない（skip_serializing_if）。
// 用途は2つ：(1) プリセット（型）の中身、(2) 起動リストの「マニュアル」エントリの中身。
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub(crate) struct TabSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) shell: String, // ShellSpec.short
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) cwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) bg: String, // "#RRGGBB"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) accent: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) record: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commands: Vec<String>, // spawn 後に pty へ各行＋Enter で流す
    // GUI 編集用バッファ：commands を改行区切りの複数行テキストとして編集する。
    // 保存しない（save 時に commands へ変換）。load 後に commands.join で初期化。
    #[serde(skip)]
    pub(crate) commands_buf: String,
}

impl TabSpec {
    // GUI 編集バッファ（複数行テキスト）を commands から初期化する。load 後に呼ぶ。
    pub(crate) fn sync_buf_from_commands(&mut self) {
        self.commands_buf = self.commands.join("\n");
    }
    // 保存前：GUI バッファ → commands（各行 trim、空行除去）へ確定する。
    pub(crate) fn commands_from_buf(&self) -> Vec<String> {
        self.commands_buf
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }
}

// skip_serializing_if 用（false は TOML に出力しない）。
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// 起動リストの1エントリ。参照モード（use_preset）なら presets から preset 名で解決、
// そうでなければマニュアル（spec をそのまま使う）。行は3状態を取る：マニュアル／参照・選択済／
// 参照・未選択。preset 名だけでは未選択と区別できないため use_preset で明示する。
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub(crate) struct StartupEntry {
    // 参照モードか。これがモードの唯一の真＝false なら preset 値があってもマニュアル（preset 無視）。
    // 旧形式（このキー無し）は false＝マニュアルとして読める＝後方互換。
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) use_preset: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) preset: String, // 参照するプリセット名（未選択なら空）。use_preset=false のときは無視。
    #[serde(flatten)]
    pub(crate) spec: TabSpec, // マニュアル時の定義（参照時は未使用＝空のまま出力されない）
}

impl StartupEntry {
    // モードは use_preset のみで決まる。preset 値だけで参照扱いにはしない（手編集の齟齬回避）。
    pub(crate) fn is_preset(&self) -> bool {
        self.use_preset
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    // flatten + toml の直列化が壊れていないことを round-trip で担保する。
    // プリセット参照エントリ／マニュアルエントリの双方が正しく往復すること。
    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            default_shell: "pwsh".into(),
            presets: vec![TabSpec {
                name: "Atrium".into(),
                shell: "pwsh".into(),
                cwd: "C:\\work".into(),
                accent: "#33CC77".into(),
                commands: vec!["claude".into()],
                ..Default::default()
            }],
            startup: vec![
                StartupEntry { use_preset: true, preset: "Atrium".into(), ..Default::default() },
                StartupEntry {
                    spec: TabSpec {
                        name: "作業".into(),
                        shell: "cmd".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            startup_quiet_ms: default_startup_quiet_ms(),
            startup_max_wait_ms: default_startup_max_wait_ms(),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&s).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.presets.len(), 1);
        assert_eq!(back.presets[0].name, "Atrium");
        assert_eq!(back.presets[0].commands, vec!["claude".to_string()]);
        assert_eq!(back.startup.len(), 2);
        assert!(back.startup[0].is_preset());
        assert_eq!(back.startup[0].preset, "Atrium");
        assert!(!back.startup[1].is_preset());
        assert_eq!(back.startup[1].spec.name, "作業");
        assert_eq!(back.startup[1].spec.shell, "cmd");
    }

    // 参照モードだが未選択（use_preset=true, preset 空）が、マニュアルと区別されて往復すること。
    #[test]
    fn preset_mode_unselected_roundtrip() {
        let s = Settings {
            startup: vec![StartupEntry { use_preset: true, ..Default::default() }],
            startup_quiet_ms: default_startup_quiet_ms(),
            startup_max_wait_ms: default_startup_max_wait_ms(),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&s).expect("serialize");
        assert!(text.contains("use_preset = true"), "mode flag must persist: {text}");
        let back: Settings = toml::from_str(&text).expect("deserialize");
        assert!(back.startup[0].is_preset(), "unselected preset is still preset mode");
        assert!(back.startup[0].preset.is_empty());
    }

    #[test]
    fn record_flag_roundtrip() {
        let s = Settings {
            presets: vec![
                TabSpec { name: "on".into(), record: true, ..Default::default() },
                TabSpec { name: "off".into(), ..Default::default() },
            ],
            startup_quiet_ms: default_startup_quiet_ms(),
            startup_max_wait_ms: default_startup_max_wait_ms(),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&s).expect("serialize");
        assert_eq!(text.matches("record = true").count(), 1, "{text}");
        assert!(!text.contains("record = false"), "{text}");
        let back: Settings = toml::from_str(&text).expect("deserialize");
        assert!(back.presets[0].record);
        assert!(!back.presets[1].record);
    }

    // 手編集で use_preset 無し（=false）かつ preset 値ありは、マニュアル扱い＝preset を無視する。
    #[test]
    fn preset_value_ignored_without_flag() {
        let src = r##"
[[startup_tabs]]
preset = "Atrium"
name = "手動"
shell = "cmd"
"##;
        let s: Settings = toml::from_str(src).expect("parse");
        assert_eq!(s.startup.len(), 1);
        assert!(!s.startup[0].is_preset(), "use_preset 無し＝マニュアル");
        assert_eq!(s.startup[0].spec.name, "手動");
    }

    // 0.2.x 形式（インライン定義のみの [[startup_tabs]]）が、移行なしで
    // マニュアルエントリとして読めること＝上位互換を担保する。
    #[test]
    fn loads_legacy_startup_tabs() {
        let legacy = r##"
default_shell = "pwsh"

[[startup_tabs]]
name = "Atrium"
shell = "pwsh"
cwd = "C:\\Atrium"
accent = "#33CC77"
commands = ["claude --remote-control"]
"##;
        let s: Settings = toml::from_str(legacy).expect("legacy parse");
        assert_eq!(s.startup.len(), 1);
        assert!(!s.startup[0].is_preset());
        assert_eq!(s.startup[0].spec.name, "Atrium");
        assert_eq!(s.startup[0].spec.shell, "pwsh");
        assert_eq!(s.startup[0].spec.commands, vec!["claude --remote-control".to_string()]);
        assert!(s.presets.is_empty());
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct Settings {
    #[serde(default)]
    pub(crate) default_shell: String,
    #[serde(default)]
    pub(crate) default_cwd: String,
    #[serde(default)]
    pub(crate) default_bg: String,
    #[serde(default)]
    pub(crate) default_accent: String,
    #[serde(default)]
    pub(crate) panel: String, // "left" | "right"（タブパネルの位置。空=left）
    // 型ライブラリ：再利用できるタブ定義（プリセット）。name が識別子を兼ねる。
    #[serde(default)]
    pub(crate) presets: Vec<TabSpec>,
    // 起動時に開くタブのリスト。各行はプリセット参照 or マニュアル（StartupEntry 参照）。
    // TOML キーは 0.2.x から不変の `startup_tabs`。旧形式（インライン定義のみ）は
    // flatten のおかげでマニュアルエントリとしてそのまま読める＝完全な上位互換で移行不要。
    #[serde(default, rename = "startup_tabs")]
    pub(crate) startup: Vec<StartupEntry>,
    // 「LustTermina で実行」で使うシェルを拡張子ごとに選ぶ（拡張子 → シェル short 名）。
    // 未設定の拡張子は組込み既定（default_runner_short）。TOML では [script_runner] テーブル。
    #[serde(default)]
    pub(crate) script_runner: std::collections::BTreeMap<String, String>,
    // 起動コマンドの投入ペース（ミリ秒）。未指定は既定値。通信待ちで出力が一時停止する
    // コマンド（例: /remote-control）を取りこぼすときは startup_quiet_ms を上げる。
    #[serde(default = "default_startup_quiet_ms")]
    pub(crate) startup_quiet_ms: u64,
    #[serde(default = "default_startup_max_wait_ms")]
    pub(crate) startup_max_wait_ms: u64,
    #[serde(default)]
    pub(crate) log: LogSettings,
    #[serde(default)]
    pub(crate) playback: PlaybackSettings,
    #[serde(default)]
    pub(crate) font: FontSettings,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub(crate) struct FontSettings {
    pub(crate) terminal: String,
    pub(crate) terminal_index: u32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            terminal: "BIZ-UDGothicR.ttc".into(),
            terminal_index: 0,
        }
    }
}

pub(crate) fn resolve_font_path(name: &str) -> Option<std::path::PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let direct = std::path::PathBuf::from(name);
    if direct.is_absolute() {
        return direct.is_file().then_some(direct);
    }
    let system = std::env::var_os("WINDIR").map(|w| std::path::Path::new(&w).join("Fonts"));
    let user = std::env::var_os("LOCALAPPDATA")
        .map(|l| std::path::Path::new(&l).join("Microsoft").join("Windows").join("Fonts"));
    [system, user]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub(crate) struct PlaybackSettings {
    pub(crate) idle_limit_ms: u64,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self { idle_limit_ms: 2000 }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub(crate) struct LogSettings {
    pub(crate) level: String,
    pub(crate) deps_level: String,
    pub(crate) max_kb: u64,
    pub(crate) tab_keep: usize,
    pub(crate) cast_max_kb: u64,
    pub(crate) cast_keep: usize,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: "info".into(),
            deps_level: "warn".into(),
            max_kb: 4096,
            tab_keep: 100,
            cast_max_kb: 51200,
            cast_keep: 30,
        }
    }
}

// ユーザーのホーム（%USERPROFILE%）。既定 cwd 等の基準。取れなければ空。
pub(crate) fn home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

// 設定ファイルのパス。APPDATA をベースにし、パスをベタ書きしない。
pub(crate) fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(std::path::Path::new(&base).join("LustTermina").join("config.toml"))
}

pub(crate) fn load_settings() -> Option<Settings> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    toml::from_str(&text).ok()
}

pub(crate) fn color_to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

pub(crate) fn color_from_hex(s: &str) -> Option<Color32> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

// 設定モーダル内のタブ。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SettingsTab {
    General, // 既定（パネル位置・既定シェル・初期ディレクトリ）
    Theme,   // テーマ（既定の背景・アクセント）
    Presets, // プリセット（型ライブラリの編集）
    Startup, // 起動タブ
    System,  // システム（データフォルダ・初期化・アンインストール）
}

