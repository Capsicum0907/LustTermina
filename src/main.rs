// LustTermina（ラストテルミナ）— MVP
//
// データフロー: pwsh ⇄ portable-pty(ConPTY) ⇄ alacritty_terminal(グリッド) → egui 描画
// 入力:        egui のキー/文字 → エスケープ列に encode → pty writer → pwsh
//
// この MVP の範囲: 「pwsh の出力が色付きで出て、キー入力が通る」まで。
// 後回し（長い尻尾）: per-cell 背景色, スクロールバック UI, 選択コピー, CJK 全角幅,
//                     マウスレポート, 差分描画, 縦タブ, 設定/テーマ toml。

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Sense, Vec2};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor, Processor};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

const FONT_SIZE: f32 = 14.0;
const PAD: f32 = 6.0;

// ---- グリッド寸法（alacritty の Dimensions を満たす最小実装）----
#[derive(Clone, Copy, PartialEq, Eq)]
struct TermSize {
    cols: usize,
    rows: usize,
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
const DEFAULT_FG: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);
const DEFAULT_BG: Color32 = Color32::from_rgb(0x0C, 0x0C, 0x0C);

// 縦タブ用（サイドバーは本文より少し明るく、アクティブタブだけ本文色で地続きに見せる）
const SIDEBAR_BG: Color32 = Color32::from_gray(26);
const TAB_HOVER_BG: Color32 = Color32::from_gray(42);
const TAB_TEXT: Color32 = Color32::from_gray(225); // 非アクティブ＝白っぽく
const TAB_TEXT_ACTIVE: Color32 = Color32::from_gray(245);
const ACCENT_GREEN: Color32 = Color32::from_rgb(0x19, 0xC3, 0x7D); // アクセント（アイコンと同じ緑）
// 破壊的アクション（データ初期化・アンインストール）用の赤。緑=選択専用とは役割を分ける。
const DANGER_FILL: Color32 = Color32::from_rgb(0x5A, 0x28, 0x28);
const DANGER_BORDER: Color32 = Color32::from_rgb(0x8A, 0x40, 0x40);

// テキスト選択のハイライト（半透明。下地の bg・セル色の上に重ねて使う）
const SELECTION_BG: Color32 = Color32::from_rgba_premultiplied(0x3A, 0x5A, 0x8C, 0x88);

// タブ個別色の選択パレット（背景・アクセント共通。12色）
// アクセント用（鮮やか）。アクセントバーは小面積なので彩度高めでよい。
const ACCENT_PALETTE: [Color32; 12] = [
    Color32::from_rgb(0xE0, 0x52, 0x4A), // red
    Color32::from_rgb(0xE0, 0x7B, 0x39), // orange
    Color32::from_rgb(0xD9, 0xA8, 0x2C), // amber
    Color32::from_rgb(0x9A, 0xC8, 0x2E), // lime
    Color32::from_rgb(0x19, 0xC3, 0x7D), // green
    Color32::from_rgb(0x1F, 0xB5, 0xB5), // teal
    Color32::from_rgb(0x3A, 0xA6, 0xDD), // sky
    Color32::from_rgb(0x4A, 0x78, 0xE0), // blue
    Color32::from_rgb(0x7A, 0x5C, 0xD8), // indigo
    Color32::from_rgb(0xA9, 0x5C, 0xD0), // purple
    Color32::from_rgb(0xD8, 0x5C, 0x9E), // pink
    Color32::from_rgb(0x7A, 0x82, 0x8C), // gray
];

// 背景パレットは「表示色（鮮やかで見分けやすい）」と「適用色（暗いトーン）」を
// index で対応させる。スワッチは明るく見せ、実際に body に敷くのは暗い色。
const BG_DISPLAY: [Color32; 12] = [
    Color32::from_rgb(0x3A, 0x3A, 0x3A), // 既定（黒）の代表
    Color32::from_rgb(0xE0, 0x52, 0x4A), // red
    Color32::from_rgb(0xE0, 0x7B, 0x39), // orange
    Color32::from_rgb(0xD9, 0xA8, 0x2C), // amber
    Color32::from_rgb(0x9A, 0xC8, 0x2E), // lime
    Color32::from_rgb(0x19, 0xC3, 0x7D), // green
    Color32::from_rgb(0x1F, 0xB5, 0xB5), // teal
    Color32::from_rgb(0x3A, 0xA6, 0xDD), // sky
    Color32::from_rgb(0x4A, 0x78, 0xE0), // blue
    Color32::from_rgb(0x7A, 0x5C, 0xD8), // indigo
    Color32::from_rgb(0xA9, 0x5C, 0xD0), // purple
    Color32::from_rgb(0xD8, 0x5C, 0x9E), // pink
];
const BG_APPLY: [Color32; 12] = [
    Color32::from_rgb(0x0C, 0x0C, 0x0C), // black（既定）
    Color32::from_rgb(0x22, 0x12, 0x12), // dark red
    Color32::from_rgb(0x24, 0x18, 0x0E), // dark orange
    Color32::from_rgb(0x22, 0x1D, 0x0E), // dark amber
    Color32::from_rgb(0x16, 0x1E, 0x0E), // dark lime
    Color32::from_rgb(0x10, 0x20, 0x16), // dark green
    Color32::from_rgb(0x0E, 0x20, 0x20), // dark teal
    Color32::from_rgb(0x0E, 0x1A, 0x24), // dark sky
    Color32::from_rgb(0x10, 0x18, 0x28), // dark blue
    Color32::from_rgb(0x16, 0x12, 0x26), // dark indigo
    Color32::from_rgb(0x20, 0x12, 0x28), // dark purple
    Color32::from_rgb(0x26, 0x12, 0x1E), // dark pink
];

const ANSI16: [Color32; 16] = [
    Color32::from_rgb(0x0C, 0x0C, 0x0C), // 0 black
    Color32::from_rgb(0xC5, 0x0F, 0x1F), // 1 red
    Color32::from_rgb(0x13, 0xA1, 0x0E), // 2 green
    Color32::from_rgb(0xC1, 0x9C, 0x00), // 3 yellow
    Color32::from_rgb(0x00, 0x37, 0xDA), // 4 blue
    Color32::from_rgb(0x88, 0x17, 0x98), // 5 magenta
    Color32::from_rgb(0x3A, 0x96, 0xDD), // 6 cyan
    Color32::from_rgb(0xCC, 0xCC, 0xCC), // 7 white
    Color32::from_rgb(0x76, 0x76, 0x76), // 8 bright black
    Color32::from_rgb(0xE7, 0x48, 0x56), // 9 bright red
    Color32::from_rgb(0x16, 0xC6, 0x0C), // 10 bright green
    Color32::from_rgb(0xF9, 0xF1, 0xA5), // 11 bright yellow
    Color32::from_rgb(0x3B, 0x78, 0xFF), // 12 bright blue
    Color32::from_rgb(0xB4, 0x00, 0x9E), // 13 bright magenta
    Color32::from_rgb(0x61, 0xD6, 0xD6), // 14 bright cyan
    Color32::from_rgb(0xF2, 0xF2, 0xF2), // 15 bright white
];

fn palette256(i: u8) -> Color32 {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let x = i - 16;
            let conv = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    v * 40 + 55
                }
            };
            Color32::from_rgb(conv(x / 36), conv((x / 6) % 6), conv(x % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn named_rgb(n: NamedColor, bold: bool, bg_default: Color32) -> Color32 {
    use NamedColor::*;
    let base: usize = match n {
        Black => 0,
        Red => 1,
        Green => 2,
        Yellow => 3,
        Blue => 4,
        Magenta => 5,
        Cyan => 6,
        White => 7,
        BrightBlack => 8,
        BrightRed => 9,
        BrightGreen => 10,
        BrightYellow => 11,
        BrightBlue => 12,
        BrightMagenta => 13,
        BrightCyan => 14,
        BrightWhite => 15,
        Background => return bg_default, // 既定背景＝このセッションの bg（body 色分け）
        _ => return DEFAULT_FG, // Foreground / Cursor / Dim* / Bright・Dim Foreground は既定 fg
    };
    // bold は基本8色を明色(8-15)へ持ち上げる端末の慣習に倣う
    let idx = if bold && base < 8 { base + 8 } else { base };
    ANSI16[idx]
}

fn resolve(c: TermColor, bold: bool, bg_default: Color32) -> Color32 {
    match c {
        TermColor::Named(n) => named_rgb(n, bold, bg_default),
        TermColor::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        TermColor::Indexed(i) => palette256(i),
    }
}

// dim/faint: 明度を落とす
fn dim(c: Color32) -> Color32 {
    let s = |v: u8| (v as f32 * 0.6) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

// ---- シェル定義（探索して見つかったものを並べる。ハードコード列挙はしない）----
#[derive(Clone)]
struct ShellSpec {
    label: String,    // メニュー表示名
    short: String,    // タブ名の接頭辞
    program: String,  // 実行ファイル（フルパス or PATH 上の名前）
    args: Vec<String>, // 起動引数
    // アイコン抽出元（None なら program から抽出）。VS Developer 系のように
    // 実体が cmd/pwsh で固有 exe を持たないものに、別の exe（devenv.exe 等）の
    // アイコンを割り当てるための逃がし。
    icon_source: Option<String>,
}

fn find_existing(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .find(|p| !p.is_empty() && std::path::Path::new(p).exists())
        .cloned()
}

// PATH を走査して実行ファイルを探す（pwsh 等が固定パスに無くても拾う）。
fn which(name: &str) -> Option<String> {
    let path = std::env::var("PATH").ok()?;
    path.split(';')
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}\\{name}"))
        .find(|p| std::path::Path::new(p).exists())
}

// Visual Studio 2022 のインストールディレクトリを探す（VsDevCmd.bat の在処で判定）。
fn find_vs2022() -> Option<String> {
    let pf = std::env::var("ProgramFiles").unwrap_or_default();
    ["Community", "Professional", "Enterprise", "BuildTools", "Preview"]
        .iter()
        .map(|ed| format!("{pf}\\Microsoft Visual Studio\\2022\\{ed}"))
        .find(|dir| std::path::Path::new(&format!("{dir}\\Common7\\Tools\\VsDevCmd.bat")).exists())
}

// システムを探索して、利用可能なシェルを並べる。
fn discover_shells() -> Vec<ShellSpec> {
    let pf = std::env::var("ProgramFiles").unwrap_or_default();
    let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
    let lad = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());

    // PowerShell 系：古い PSReadLine に -PredictionSource が無くてもエラーにしない予測オフ。
    let ps_args = || {
        vec![
            "-NoExit".to_string(),
            "-Command".to_string(),
            "$c = Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue; \
             if ($c -and $c.Parameters.ContainsKey('PredictionSource')) \
             { Set-PSReadLineOption -PredictionSource None }"
                .to_string(),
        ]
    };

    // cmd / Windows PowerShell はフルパスに固定（アイコン抽出・spawn を確実にする）
    let winps = find_existing(&[format!(
        "{sysroot}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
    )])
    .unwrap_or_else(|| "powershell.exe".into());
    let cmd = find_existing(&[format!("{sysroot}\\System32\\cmd.exe")])
        .unwrap_or_else(|| "cmd.exe".into());

    let mut v = Vec::new();

    // PowerShell 7（固定パス→PATH の順で探す。在れば最優先）
    let pwsh = find_existing(&[format!("{pf}\\PowerShell\\7\\pwsh.exe")]).or_else(|| which("pwsh.exe"));
    if let Some(p) = &pwsh {
        v.push(ShellSpec {
            label: "PowerShell 7".into(),
            short: "pwsh".into(),
            // MSIX(Store/winget)版は exe にアイコンが無いので assets の PNG を使う。
            // 本命：Appx API でパス取得＋直接プローブ（非昇格OK）。ダメなら WindowsApps 走査
            // （昇格時のみ成功）、最後に which 解決パス（MSI 版なら exe から抽出）。
            icon_source: pwsh_pkg_icon()
                .or_else(pwsh_msix_icon)
                .or_else(|| msix_icon_for(p)),
            program: p.clone(),
            args: ps_args(),
        });
    }
    // Windows PowerShell（常設）
    v.push(ShellSpec {
        label: "Windows PowerShell".into(),
        short: "powershell".into(),
        program: winps.clone(),
        args: ps_args(),
        icon_source: None,
    });
    // Command Prompt（常設）
    v.push(ShellSpec {
        label: "Command Prompt".into(),
        short: "cmd".into(),
        program: cmd.clone(),
        args: vec![],
        icon_source: None,
    });
    // VS 2022 Developer 系（在れば）
    if let Some(vs) = find_vs2022() {
        // Developer 系は実体が cmd/pwsh なので、固有アイコンとして VS の devenv.exe を
        // 使う（在れば。無ければ program のアイコンにフォールバック）。
        let devenv = find_existing(&[format!("{vs}\\Common7\\IDE\\devenv.exe")]);
        // Developer Command Prompt = cmd /k VsDevCmd.bat
        v.push(ShellSpec {
            label: "Developer Command Prompt for VS 2022".into(),
            short: "devcmd".into(),
            program: cmd.clone(),
            args: vec!["/k".into(), format!("{vs}\\Common7\\Tools\\VsDevCmd.bat")],
            icon_source: devenv.clone(),
        });
        // Developer PowerShell = DevShell モジュールを Import して Enter-VsDevShell
        let dll = format!("{vs}\\Common7\\Tools\\Microsoft.VisualStudio.DevShell.dll");
        let ps_prog = pwsh.clone().unwrap_or_else(|| winps.clone());
        v.push(ShellSpec {
            label: "Developer PowerShell for VS 2022".into(),
            short: "devps".into(),
            program: ps_prog,
            icon_source: devenv.clone(),
            args: vec![
                "-NoExit".into(),
                "-Command".into(),
                format!(
                    "&{{ Import-Module '{dll}'; Enter-VsDevShell -SkipAutomaticLocation \
                     -VsInstallPath '{vs}' -DevCmdArguments '-arch=x64' }}"
                ),
            ],
        });
    }
    // Git Bash（在れば）
    if let Some(p) = find_existing(&[
        format!("{pf}\\Git\\bin\\bash.exe"),
        format!("{pf86}\\Git\\bin\\bash.exe"),
        format!("{lad}\\Programs\\Git\\bin\\bash.exe"),
    ]) {
        // 色付きの Git Bash アイコンは bin\bash.exe ではなくルート直下の
        // git-bash.exe が持つ。在ればそれをアイコン元に、無ければ bash.exe から。
        let gitbash = std::path::Path::new(&p)
            .parent()
            .and_then(|bin| bin.parent())
            .map(|root| root.join("git-bash.exe"))
            .filter(|gb| gb.exists())
            .map(|gb| gb.to_string_lossy().into_owned());
        v.push(ShellSpec {
            label: "Git Bash".into(),
            short: "bash".into(),
            program: p,
            args: vec!["-l".into(), "-i".into()],
            icon_source: gitbash,
        });
    }
    // WSL（在れば、既定ディストロ）
    if let Some(p) = find_existing(&[format!("{sysroot}\\System32\\wsl.exe")]) {
        v.push(ShellSpec {
            label: "WSL".into(),
            short: "wsl".into(),
            program: p,
            args: vec![],
            icon_source: None,
        });
    }
    v
}

// プログラム名（PATH 上の名前 or フルパス）を実在フルパスに解決（アイコン抽出用）。
fn resolve_program(program: &str) -> Option<String> {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        return p.exists().then(|| program.to_string());
    }
    which(program)
}

// アイコンを面積平均で正方形 target px へ事前縮小する。GPU の mipmap 無し
// bilinear 縮小だと細い線（PS7 ロゴの白い >_ 等）を取りこぼして潰れるので、
// 読み込み時に CPU で綺麗に縮めてからアップロードする。透過の色滲みを防ぐため
// アルファで重み付け（premultiply）して平均する。
fn downscale_square(src: &egui::ColorImage, target: usize) -> egui::ColorImage {
    let [sw, sh] = src.size;
    if sw <= target && sh <= target {
        return src.clone();
    }
    let mut pixels = Vec::with_capacity(target * target);
    for ty in 0..target {
        for tx in 0..target {
            // この出力画素が覆う元画像の矩形 [x0,x1) x [y0,y1)
            let x0 = tx * sw / target;
            let x1 = (((tx + 1) * sw / target).max(x0 + 1)).min(sw);
            let y0 = ty * sh / target;
            let y1 = (((ty + 1) * sh / target).max(y0 + 1)).min(sh);
            let (mut r, mut g, mut b, mut a, mut n) = (0f32, 0f32, 0f32, 0f32, 0f32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = src.pixels[sy * sw + sx];
                    let pa = p.a() as f32 / 255.0;
                    r += p.r() as f32 * pa;
                    g += p.g() as f32 * pa;
                    b += p.b() as f32 * pa;
                    a += pa;
                    n += 1.0;
                }
            }
            let px = if a > 0.0 {
                egui::Color32::from_rgba_unmultiplied(
                    (r / a).round() as u8,
                    (g / a).round() as u8,
                    (b / a).round() as u8,
                    (a / n * 255.0).round() as u8,
                )
            } else {
                egui::Color32::TRANSPARENT
            };
            pixels.push(px);
        }
    }
    egui::ColorImage {
        size: [target, target],
        pixels,
        ..Default::default()
    }
}

// アイコン画像を読み込む。PNG（MSIX のアセット等）は直接デコードし、
// それ以外（exe/ico）は exe 埋め込みアイコンを Win32 で抽出する。
fn load_icon_image(path: &str) -> Option<egui::ColorImage> {
    if path.to_ascii_lowercase().ends_with(".png") {
        let bytes = std::fs::read(path).ok()?;
        let icon = eframe::icon_data::from_png_bytes(&bytes).ok()?;
        return Some(egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        ));
    }
    extract_icon_rgba(path)
}

// MSIX（WindowsApps）配下の exe は exe にブランドアイコンを埋め込まず、
// パッケージ内の PNG アセットを使う。assets からメニュー向きの1枚を選んで返す
//（小さめ・プレート無し＝透過 を優先）。WindowsApps 配下でなければ None。
fn msix_icon_for(exe: &str) -> Option<String> {
    let dir = std::path::Path::new(exe).parent()?;
    if !dir.to_string_lossy().to_ascii_lowercase().contains("windowsapps") {
        return None;
    }
    let pngs: Vec<std::path::PathBuf> = std::fs::read_dir(dir.join("assets"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
        .collect();
    // 好み順。ファイル名に含まれる最初のキーで決める。
    let prefs = [
        "Square44x44Logo.targetsize-48_altform-unplated",
        "Square44x44Logo.targetsize-48",
        "Square44x44Logo",
        "Square150x150Logo",
        "StoreLogo",
    ];
    for key in prefs {
        if let Some(hit) = pngs.iter().find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(key))
        }) {
            return Some(hit.to_string_lossy().into_owned());
        }
    }
    None
}

// MSIX 版 pwsh のアイコンを「列挙せずに」探す（非昇格でも動く本命経路）。
// C:\Program Files\WindowsApps の read_dir は昇格が要るため通常起動だと失敗する。
// 代わりに Appx パッケージ API でインストールパスを取得（FS 列挙不要）し、既知の
// assets ファイル名を直接プローブする（is_file はメタデータ参照で列挙ではない）。
fn pwsh_pkg_icon() -> Option<String> {
    let paths = pwsh_package_paths();
    // x64 実体を優先、その他はフォールバック。
    let mut ordered: Vec<String> =
        paths.iter().filter(|p| p.contains("_x64_")).cloned().collect();
    ordered.extend(paths.iter().filter(|p| !p.contains("_x64_")).cloned());
    let names = [
        "Square44x44Logo.targetsize-48_altform-unplated.png",
        "Square44x44Logo.targetsize-48.png",
        "Square44x44Logo.png",
        "Square150x150Logo.png",
        "StoreLogo.png",
    ];
    for base in ordered {
        let assets = std::path::Path::new(&base).join("assets");
        for n in names {
            let p = assets.join(n);
            if p.is_file() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

// PowerShell パッケージ（family: Microsoft.PowerShell_8wekyb3d8bbwe）のインストールパス一覧を
// Appx API で取得。ファイルシステム列挙を一切しないので非昇格でも動く。
fn pwsh_package_paths() -> Vec<String> {
    use std::iter::once;
    use windows::Win32::Storage::Packaging::Appx::{
        GetPackagePathByFullName, GetPackagesByPackageFamily,
    };
    use windows::core::{PCWSTR, PWSTR};

    let family: Vec<u16> = "Microsoft.PowerShell_8wekyb3d8bbwe"
        .encode_utf16()
        .chain(once(0))
        .collect();
    let mut out = Vec::new();
    unsafe {
        let mut count: u32 = 0;
        let mut chars: u32 = 0;
        // 1回目：個数とバッファ長だけ取得（buffer は None）。
        let _ = GetPackagesByPackageFamily(
            PCWSTR(family.as_ptr()),
            &mut count,
            None,
            &mut chars,
            None,
        );
        if count == 0 {
            return out;
        }
        let mut names: Vec<PWSTR> = vec![PWSTR::null(); count as usize];
        let mut buf: Vec<u16> = vec![0u16; chars as usize];
        let r = GetPackagesByPackageFamily(
            PCWSTR(family.as_ptr()),
            &mut count,
            Some(names.as_mut_ptr()),
            &mut chars,
            Some(PWSTR(buf.as_mut_ptr())),
        );
        if r.0 != 0 {
            return out;
        }
        for nm in names.iter().take(count as usize) {
            if nm.is_null() {
                continue;
            }
            let name = PCWSTR(nm.0 as *const u16);
            let mut plen: u32 = 0;
            // 1回目：必要な長さを取得（buffer は None）。
            let _ = GetPackagePathByFullName(name, &mut plen, None);
            if plen == 0 {
                continue;
            }
            let mut pbuf: Vec<u16> = vec![0u16; plen as usize];
            let r2 = GetPackagePathByFullName(name, &mut plen, Some(PWSTR(pbuf.as_mut_ptr())));
            if r2.0 != 0 {
                continue;
            }
            let end = (plen as usize).saturating_sub(1).min(pbuf.len());
            let s = String::from_utf16_lossy(&pbuf[..end]);
            if !s.is_empty() {
                out.push(s);
            }
        }
    }
    out
}

// MSIX 版 pwsh のアイコン PNG を WindowsApps の走査で探すフォールバック。
// 注意：この read_dir は昇格が要る（通常ユーザーは列挙拒否）。非昇格では失敗するので
// 本命は pwsh_pkg_icon()。両方ダメなときの保険として残す。
fn pwsh_msix_icon() -> Option<String> {
    let pf = std::env::var("ProgramFiles").ok()?;
    let wa = std::path::Path::new(&pf).join("WindowsApps");
    let mut found: Option<String> = None;
    for entry in std::fs::read_dir(&wa).ok()?.flatten() {
        let dir = entry.path();
        let is_ps = dir
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("Microsoft.PowerShell_"))
            .unwrap_or(false);
        if !is_ps {
            continue;
        }
        // assets を持つ（＝実体パッケージ）ものだけ採用。x64 を優先。
        if let Some(png) = msix_icon_for(&dir.join("pwsh.exe").to_string_lossy()) {
            let is_x64 = dir.to_string_lossy().contains("_x64_");
            if is_x64 {
                return Some(png);
            }
            found.get_or_insert(png);
        }
    }
    found
}

// exe のアイコンを抽出して egui の ColorImage にする（Win32）。
fn extract_icon_rgba(program: &str) -> Option<egui::ColorImage> {
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows::core::PCWSTR;

    let path = resolve_program(program)?;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if r == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        let img = hicon_to_image(sfi.hIcon);
        let _ = DestroyIcon(sfi.hIcon);
        img
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn hicon_to_image(
    hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<egui::ColorImage> {
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let mut ii = ICONINFO::default();
    if GetIconInfo(hicon, &mut ii).is_err() {
        return None;
    }
    let color = ii.hbmColor;
    let mask = ii.hbmMask;

    let mut bm = BITMAP::default();
    let got = GetObjectW(
        HGDIOBJ(color.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    );
    if got == 0 {
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight);

    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = w;
    bi.bmiHeader.biHeight = -h; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0 as u32;

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let hdc = GetDC(None);
    let scan = GetDIBits(
        hdc,
        color,
        0,
        h as u32,
        Some(buf.as_mut_ptr() as *mut _),
        &mut bi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(None, hdc);
    let _ = DeleteObject(HGDIOBJ(color.0));
    let _ = DeleteObject(HGDIOBJ(mask.0));
    if scan == 0 {
        return None;
    }

    // BGRA → RGBA。アルファが全ゼロ（古いアイコン）なら不透明にする。
    let mut any_alpha = false;
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
        if px[3] != 0 {
            any_alpha = true;
        }
    }
    if !any_alpha {
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &buf,
    ))
}

// ---- Term → pty 書き戻し ----
// Term は DSR（カーソル位置問い合わせ ESC[6n）等への応答を Event::PtyWrite として出す。
// これを pty writer に流さないと、pwsh が応答待ちで固まってプロンプトが出ない。
#[derive(Clone)]
struct EventProxy {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            if let Ok(mut w) = self.writer.lock() {
                let _ = w.write_all(text.as_bytes());
                let _ = w.flush();
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
const STARTUP_QUIET_MS: u64 = 800;
const STARTUP_MAX_WAIT_MS: u64 = 8000;
fn default_startup_quiet_ms() -> u64 {
    STARTUP_QUIET_MS
}
fn default_startup_max_wait_ms() -> u64 {
    STARTUP_MAX_WAIT_MS
}

// ---- セッション（1タブ = PTY + Term + 読み取りスレッド）----
struct Session {
    term: Arc<Mutex<Term<EventProxy>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    size: TermSize,
    scroll_accum: f32, // ホイールの端数（行）を貯める
    title: String,
    bg: Color32,     // このタブの背景色（既定はテーマの本文色）
    accent: Color32, // このタブのアクセント色（既定は緑）
    shell_short: String, // 開いたシェルの short 名（起動タブのスナップショット用）
    cwd: String,         // 開いた初期ディレクトリ（同上。空=継承）
    // IME 未確定文字列（変換中の合成テキスト）。確定前は pty へ送らずグリッドにも無いので、
    // 描画時にカーソル位置へインライン表示するために保持する。確定/無効化で空に戻す。
    preedit: String,
    // マウスモーション報告(1002/1003)の重複抑制。同じセル上の移動は1回だけ報告する
    // （画素ごとに送るとアプリへ洪水になる）。直近に報告したセル(col,row)。
    last_mouse_cell: Option<(usize, usize)>,
    // PTY から最後に出力が来た時刻。リーダースレッドが読むたびスタンプする。
    // 起動コマンドの投入ペーサ（send_commands_paced）が「出力が静まったか」の判定に使う。
    last_output: Arc<Mutex<Instant>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Session {
    fn spawn(ctx: &egui::Context, title: String, spec: &ShellSpec, cwd: &str) -> Self {
        let init = TermSize { cols: 80, rows: 24 };

        // PTY を開いて pwsh を起動
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: init.rows as u16,
                cols: init.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty failed");

        // シェルの起動定義（プログラム＋引数）は ShellSpec が持つ（探索結果）。
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if !cwd.is_empty() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn shell failed");
        drop(pair.slave); // slave 側はもう要らない

        let reader = pair.master.try_clone_reader().expect("clone reader failed");
        let writer = pair.master.take_writer().expect("take writer failed");
        let master: Box<dyn MasterPty + Send> = pair.master;

        // writer は UI 入力と Term 書き戻しの両方から使うので Arc<Mutex> に
        let writer = Arc::new(Mutex::new(writer));

        // ターミナルのグリッド（書き戻し用プロキシを渡す）
        let proxy = EventProxy {
            writer: writer.clone(),
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
            thread::spawn(move || {
                let mut parser: Processor = Processor::new();
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
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
        }

        Self {
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
            _child: child,
        }
    }

    // 起動コマンド列を「出力が静まってから次」の順で投入する（出力アイドル・ゲーティング）。
    // 各コマンドで: 本文を書く → 出力が静まるまで待つ → Enter を単独で書く → また待つ。
    // 本文と Enter を時間的に分離することで、claude 等の TUI が連続入力を「ペースト」と
    // 誤認して Enter を送信でなく改行として食う問題を避ける。UI スレッドを止めないよう
    // 別スレッドで回す。settle 判定・上限は STARTUP_QUIET_MS / STARTUP_MAX_WAIT_MS。
    fn send_commands_paced(&self, commands: Vec<String>, quiet_ms: u64, max_wait_ms: u64) {
        if commands.is_empty() {
            return;
        }
        let writer = self.writer.clone();
        let last_output = self.last_output.clone();
        thread::spawn(move || {
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
                    if idle || Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(poll);
                }
            };
            let write = |bytes: &[u8]| {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(bytes);
                    let _ = w.flush();
                }
            };
            // まず初期プロンプト（シェル起動時の出力）が落ち着くのを待つ。
            wait_settle();
            for cmd in &commands {
                write(cmd.as_bytes());
                wait_settle(); // 本文と Enter を分離＋エコーが落ち着くまで
                write(b"\r");
                wait_settle(); // コマンド実行に伴う出力が落ち着くまで
            }
        });
    }

    fn scroll_to_bottom(&self) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Bottom);
        }
    }


    fn send(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    // egui の入力イベントを pty 向けバイト列に変換して送る
    // 入力を pty へ。何か送ったら true（呼び元が最下部へスナップするのに使う）
    fn handle_input(&mut self, ctx: &egui::Context) -> bool {
        use egui::{Event, ImeEvent, Key};
        let events = ctx.input(|i| i.events.clone());
        let mut sent = false;
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
                // ペースト（Ctrl+V は Key ではなくこのイベントで届く）。改行は CR に
                Event::Paste(t) => {
                    self.send(t.replace('\n', "\r").as_bytes());
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
                    // Ctrl+Tab はタブ循環（アプリ側で処理）。pty には送らない
                    if modifiers.ctrl && key == Key::Tab {
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
                    let seq: &[u8] = match key {
                        Key::Enter => b"\r",
                        Key::Backspace => b"\x7f",
                        Key::Tab => b"\t",
                        Key::Escape => b"\x1b",
                        Key::ArrowUp => b"\x1b[A",
                        Key::ArrowDown => b"\x1b[B",
                        Key::ArrowRight => b"\x1b[C",
                        Key::ArrowLeft => b"\x1b[D",
                        Key::Home => b"\x1b[H",
                        Key::End => b"\x1b[F",
                        Key::Delete => b"\x1b[3~",
                        Key::PageUp => b"\x1b[5~",
                        Key::PageDown => b"\x1b[6~",
                        _ => b"",
                    };
                    if !seq.is_empty() {
                        self.send(seq);
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
    fn maybe_resize(&mut self, cols: usize, rows: usize) {
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
            let _ = master.resize(PtySize {
                rows: new.rows as u16,
                cols: new.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    // 中央領域にこのセッションのグリッドを描画する
    fn draw(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, font: &FontId, cell_w: f32, cell_h: f32, allow_ime: bool) {
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

// CJK グリフを出すため、MS Gothic（端末向けの等幅JP）をフォールバックに登録。
// 既定の egui フォント（Hack 等）は CJK を持たないので日本語が豆腐になる。
fn setup_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;
    let mut fonts = FontDefinitions::default();

    // Material Icons（chevron 等の UI アイコン）。exe に埋め込む。グリフは PUA(U+E000~)
    // なので通常テキストと衝突せず、フォールバックに足すだけで使える。
    fonts.font_data.insert(
        "icons".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/MaterialIcons-Regular.ttf"
        ))),
    );

    // CJK グリフ用に MS Gothic を（あれば）フォールバック登録。無ければ豆腐になるだけ。
    let has_jp = if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\msgothic.ttc") {
        fonts
            .font_data
            .insert("jp".to_owned(), Arc::new(FontData::from_owned(bytes)));
        true
    } else {
        false
    };

    // 既定フォント優先、無いグリフだけ icons / jp へフォールバック。
    for fam in [FontFamily::Monospace, FontFamily::Proportional] {
        let list = fonts.families.entry(fam).or_default();
        list.push("icons".to_owned());
        if has_jp {
            list.push("jp".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

// マウスレポートのエスケープ列を1件ぶん組み立てる。
// cb_base: ボタン基底コード（0=左 1=中 2=右 / 64=ホイール上 65=ホイール下 /
//          モーションは呼び側で +32 して渡す）。col/row は 0 始まりのセル座標。
// press=false は SGR ではリリース('m')、X10 では全ボタン解放(3) を意味する。
fn mouse_report(
    sgr: bool,
    cb_base: u32,
    col: usize,
    row: usize,
    m: &egui::Modifiers,
    press: bool,
) -> Vec<u8> {
    // 修飾キーのビット（shift=4, alt/meta=8, ctrl=16）を加算。
    let mods = (if m.shift { 4 } else { 0 })
        + (if m.alt { 8 } else { 0 })
        + (if m.ctrl { 16 } else { 0 });
    let (cx, cy) = (col + 1, row + 1); // 端末座標は 1 始まり
    if sgr {
        // SGR(1006): ESC [ < Cb ; Cx ; Cy  (M=押下 / m=解放)。列/行が 223 を超えても安全。
        let cb = cb_base + mods;
        let fin = if press { 'M' } else { 'm' };
        format!("\x1b[<{cb};{cx};{cy}{fin}").into_bytes()
    } else {
        // X10 互換: ESC [ M (Cb+32) (Cx+32) (Cy+32)。解放は全ボタン解放=3。223 超は表現不可。
        let base = if press { cb_base } else { 3 };
        let cb = (base + mods + 32).min(255);
        let x = ((cx as u32) + 32).min(255);
        let y = ((cy as u32) + 32).min(255);
        vec![0x1b, b'[', b'M', cb as u8, x as u8, y as u8]
    }
}

fn ctrl_letter(key: egui::Key) -> Option<u8> {
    // C / V / X は egui-winit が Copy/Paste/Cut に化けさせ Key で来ないので除外
    use egui::Key::*;
    let c = match key {
        A => b'a', B => b'b', D => b'd', E => b'e', F => b'f',
        G => b'g', H => b'h', I => b'i', J => b'j', K => b'k', L => b'l',
        M => b'm', N => b'n', O => b'o', P => b'p', Q => b'q', R => b'r',
        S => b's', T => b't', U => b'u', W => b'w',
        Y => b'y', Z => b'z',
        _ => return None,
    };
    Some(c)
}

// ---- ドラッグ中のタブ（D&D 並べ替え）----
// from＝掴んだ時点のインデックス、grab_dy＝タブ上端から掴んだ点までのYオフセット
//（ゴーストをポインタに自然に追従させるため）。並べ替えの確定はドロップ時のみ。
#[derive(Clone, Copy)]
struct TabDrag {
    from: usize,
    grab_dy: f32,
}

// ---- 設定の永続化（TOML。%APPDATA%\LustTermina\config.toml）----
// 既定シェルは index ではなく short 名で保存（探索結果は環境で変わるため index は脆い）。
// 色は "#RRGGBB" 文字列で保存（手編集しやすく、toml にも素直）。
// 起動時に開くタブ1枚ぶんの定義。空文字の項目は既定にフォールバックする。
// 空文字＝省略は toml 上も出力しない（skip_serializing_if）ので従来の Option 版と互換。
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct StartupTab {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    shell: String, // ShellSpec.short
    #[serde(default, skip_serializing_if = "String::is_empty")]
    cwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    bg: String, // "#RRGGBB"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    accent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commands: Vec<String>, // spawn 後に pty へ各行＋Enter で流す
    // GUI 編集用バッファ：commands を改行区切りの複数行テキストとして編集する。
    // 保存しない（save 時に commands へ変換）。load 後に commands.join で初期化。
    #[serde(skip)]
    commands_buf: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Settings {
    #[serde(default)]
    default_shell: String,
    #[serde(default)]
    default_cwd: String,
    #[serde(default)]
    default_bg: String,
    #[serde(default)]
    default_accent: String,
    #[serde(default)]
    panel: String, // "left" | "right"（タブパネルの位置。空=left）
    #[serde(default)]
    startup_tabs: Vec<StartupTab>,
    // 起動コマンドの投入ペース（ミリ秒）。未指定は既定値。通信待ちで出力が一時停止する
    // コマンド（例: /remote-control）を取りこぼすときは startup_quiet_ms を上げる。
    #[serde(default = "default_startup_quiet_ms")]
    startup_quiet_ms: u64,
    #[serde(default = "default_startup_max_wait_ms")]
    startup_max_wait_ms: u64,
}

// ユーザーのホーム（%USERPROFILE%）。既定 cwd 等の基準。取れなければ空。
fn home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

// 設定ファイルのパス。APPDATA をベースにし、パスをベタ書きしない。
fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(std::path::Path::new(&base).join("LustTermina").join("config.toml"))
}

fn load_settings() -> Option<Settings> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    toml::from_str(&text).ok()
}

fn color_to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

fn color_from_hex(s: &str) -> Option<Color32> {
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
enum SettingsTab {
    General, // 既定（パネル位置・既定シェル・初期ディレクトリ）
    Theme,   // テーマ（既定の背景・アクセント）
    Startup, // 起動タブ
    System,  // システム（データフォルダ・初期化・アンインストール）
}

// ---- アプリ本体（複数セッション ＋ 縦タブ）----
struct App {
    sessions: Vec<Session>,
    active: usize,
    next_id: u32,
    editing: Option<usize>,       // タブ名編集中のインデックス
    editing_init: bool,           // 編集開始フレームでフォーカスを要求するため
    confirm_close: Option<usize>, // ×押下→閉じる確認待ちのインデックス
    drag: Option<TabDrag>,        // ドラッグ中のタブ（D&D 並べ替え）
    settings_open: bool,          // 設定モーダルを表示中か
    settings_tab: SettingsTab,    // 設定モーダルで開いているタブ
    shell_list_open: bool,        // 設定内：既定シェル一覧を展開中か
    shells: Vec<ShellSpec>, // 探索で見つかった利用可能シェル
    shell_icons: Vec<Option<egui::TextureHandle>>, // shells と並行：各シェルの exe アイコン
    // ---- 設定（今はメモリ上。永続化は別途 toml で）----
    default_shell: usize,    // 新規タブの既定シェル（shells のインデックス）
    default_cwd: String,     // 新規タブの初期ディレクトリ（空=継承）
    default_bg: Color32,     // 新規タブの既定背景色
    default_accent: Color32, // 新規タブの既定アクセント色
    panel_right: bool,       // タブパネルを右に置くか（false=左）
    startup_quiet_ms: u64,   // 起動コマンド投入の静穏窓（config 可変、既定 STARTUP_QUIET_MS）
    startup_max_wait_ms: u64, // 同・待ち上限（config 可変、既定 STARTUP_MAX_WAIT_MS）
    startup_tabs: Vec<StartupTab>, // 起動時に開くタブ定義（空＝既定シェル1枚）
    startup_shell_open: Option<usize>, // 起動タブ編集：シェルDDを開いているカード
    startup_color_open: Option<(usize, bool)>, // 色パレットを開いているカード（bool=accent）
    card_drag: Option<TabDrag>,        // 起動タブカードの D&D 並べ替え状態
    toast: Option<(String, std::time::Instant)>, // 一時通知（保存しました 等）
    // 別インスタンスから転送された「このフォルダで開く」要求の受信口（本体のみ Some）。
    ipc_rx: Option<mpsc::Receiver<IpcMsg>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        setup_fonts(&cc.egui_ctx);
        // egui のテーマをダーク固定（set_visuals は毎フレームのシステム追従に
        // 上書きされるが、theme_preference は設定として永続する）。メニュー等が黒くなる。
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        // タイトルバーをダークに（Windows の immersive dark mode）。
        // ネイティブ枠のままなのでリサイズ/スナップ/最大化は維持。
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
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
        let mut startup_tabs: Vec<StartupTab> = Vec::new();
        if let Some(cfg) = load_settings() {
            panel_right = cfg.panel == "right";
            startup_quiet_ms = cfg.startup_quiet_ms;
            startup_max_wait_ms = cfg.startup_max_wait_ms;
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
            startup_tabs = cfg.startup_tabs;
        }
        // GUI 編集用の複数行バッファを commands から初期化。
        for st in &mut startup_tabs {
            st.commands_buf = st.commands.join("\n");
        }

        // 起動タブ：定義があれば順に開く。各項目は未指定なら既定にフォールバック。
        // 定義が無ければ従来どおり既定シェル1枚。
        // ただし --cwd で起動された場合（Explorer の「このディレクトリで開く」）は、
        // 起動タブ定義を無視してそのフォルダで1枚だけ開く（「ここで開く」の意図に素直）。
        let mut sessions: Vec<Session> = Vec::new();
        if let Launch::Cwd(path) = &launch {
            // タブ名はフォルダ名（末尾要素）にする。取れなければ既定シェル名にフォールバック。
            let title = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} 1", shells[default_shell].short));
            let mut s = Session::spawn(&cc.egui_ctx, title, &shells[default_shell], path);
            s.bg = default_bg;
            s.accent = default_accent;
            sessions.push(s);
        } else if let Launch::RunScript(path) = &launch {
            // --run：スクリプトを、そのフォルダで実行するタブ1枚（起動タブ定義は無視）。
            sessions.push(spawn_script_session(
                &cc.egui_ctx,
                path,
                &shells,
                default_shell,
                default_bg,
                default_accent,
            ));
        } else if startup_tabs.is_empty() {
            let mut s = Session::spawn(
                &cc.egui_ctx,
                format!("{} 1", shells[default_shell].short),
                &shells[default_shell],
                &default_cwd,
            );
            s.bg = default_bg;
            s.accent = default_accent;
            sessions.push(s);
        } else {
            for (n, st) in startup_tabs.iter().enumerate() {
                let si = shells
                    .iter()
                    .position(|s| s.short == st.shell)
                    .unwrap_or(default_shell);
                let cwd = if st.cwd.is_empty() {
                    default_cwd.clone()
                } else {
                    st.cwd.clone()
                };
                let name = if st.name.is_empty() {
                    format!("{} {}", shells[si].short, n + 1)
                } else {
                    st.name.clone()
                };
                let bg = color_from_hex(&st.bg).unwrap_or(default_bg);
                let accent = color_from_hex(&st.accent).unwrap_or(default_accent);
                let mut s = Session::spawn(&cc.egui_ctx, name, &shells[si], &cwd);
                s.bg = bg;
                s.accent = accent;
                // 出力が静まってから次を送る（claude 等の TUI にも投入できるように）。
                s.send_commands_paced(st.commands.clone(), startup_quiet_ms, startup_max_wait_ms);
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
            startup_tabs,
            startup_shell_open: None,
            startup_color_open: None,
            card_drag: None,
            toast: None,
            // 受け口の生成に成功した（＝このプロセスが本体の）ときだけ Some。
            ipc_rx: ipc_start_server(&cc.egui_ctx),
        }
    }

    // 別インスタンスからの「このフォルダで開く」要求を、既定シェルの新規タブとして開く。
    fn open_dir_tab(&mut self, ctx: &egui::Context, dir: &str) {
        let spec = self.shells[self.default_shell].clone();
        let id = self.next_id;
        self.next_id += 1;
        let title = std::path::Path::new(dir)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} {}", spec.short, id));
        let mut sess = Session::spawn(ctx, title, &spec, dir);
        sess.bg = self.default_bg;
        sess.accent = self.default_accent;
        self.sessions.push(sess);
        self.active = self.sessions.len() - 1;
    }

    // スクリプト（.ps1/.bat/.cmd）を、そのファイルのあるフォルダで実行するタブを開く。
    fn open_script_tab(&mut self, ctx: &egui::Context, path: &str) {
        let sess = spawn_script_session(
            ctx,
            path,
            &self.shells,
            self.default_shell,
            self.default_bg,
            self.default_accent,
        );
        self.next_id += 1;
        self.sessions.push(sess);
        self.active = self.sessions.len() - 1;
    }

    // 一時通知を出す（数秒で自動的に消える）。
    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), std::time::Instant::now()));
    }

    // 現在の設定を TOML で保存（%APPDATA%\LustTermina\config.toml）。
    fn save_settings(&self) {
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
            // 保存時に commands_buf（複数行テキスト）→ commands（行配列、空行除去）へ。
            startup_tabs: self
                .startup_tabs
                .iter()
                .map(|st| {
                    let mut s = st.clone();
                    s.commands = st
                        .commands_buf
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect();
                    s
                })
                .collect(),
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
    fn reset_settings(&mut self) {
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
        self.startup_tabs.clear();
    }

    // 現在開いているタブ群を起動タブ定義に変換（スナップショット）。
    fn snapshot_startup_tabs(&self) -> Vec<StartupTab> {
        self.sessions.iter().map(startup_tab_of).collect()
    }

    // 設定ページ（端末の代わりに中央へ全面表示）。閉じる時に TOML 保存。
    fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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

                // ===== 起動タブタブ =====
                if self.settings_tab == SettingsTab::Startup {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("起動タブ").color(TAB_TEXT));
                        let status = if self.startup_tabs.is_empty() {
                            "未設定＝起動時は既定シェル1枚".to_string()
                        } else {
                            format!("{} 枚", self.startup_tabs.len())
                        };
                        ui.label(
                            egui::RichText::new(status).color(Color32::from_gray(150)).size(12.0),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, "＋ 追加") {
                            self.startup_tabs.push(StartupTab::default());
                        }
                        if ghost_button(ui, "現在のタブ構成を抜き出す") {
                            let snap = self.snapshot_startup_tabs();
                            self.startup_tabs = snap;
                        }
                        if ghost_button(ui, "クリア") {
                            self.startup_tabs.clear();
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
                            let n = self.startup_tabs.len();
                            for i in 0..n {
                                let dragging_this =
                                    self.card_drag.map_or(false, |d| d.from == i);
                                // カードの色＝実際のタブの見た目（bg＝塗り、accent＝枠）。
                                // 空なら既定にフォールバック。bg スウォッチは明るい表示色で。
                                let res_bg = color_from_hex(&self.startup_tabs[i].bg)
                                    .unwrap_or(self.default_bg);
                                let res_accent = color_from_hex(&self.startup_tabs[i].accent)
                                    .unwrap_or(self.default_accent);
                                let bg_disp = BG_APPLY
                                    .iter()
                                    .position(|c| *c == res_bg)
                                    .map(|k| BG_DISPLAY[k])
                                    .unwrap_or(res_bg);
                                // 左端にドラッグハンドルぶんの余白（24px）を確保。
                                let card_resp = egui::Frame::new()
                                    .fill(if dragging_this {
                                        res_bg.gamma_multiply(0.6)
                                    } else {
                                        res_bg
                                    })
                                    .stroke(egui::Stroke::new(1.0, Color32::from_gray(55)))
                                    .corner_radius(6.0)
                                    .inner_margin(if panel_right {
                                        egui::Margin { left: 8, right: 30, top: 8, bottom: 8 }
                                    } else {
                                        egui::Margin { left: 30, right: 8, top: 8, bottom: 8 }
                                    })
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [150.0, 28.0],
                                                egui::TextEdit::singleline(
                                                    &mut self.startup_tabs[i].name,
                                                )
                                                .hint_text("名前")
                                                .vertical_align(egui::Align::Center),
                                            );
                                            ui.add_space(6.0);
                                            // シェル選択（既定タブと同じ前面ドロップダウン）
                                            let sh = self.startup_tabs[i].shell.clone();
                                            let sh_idx = if sh.is_empty() {
                                                None
                                            } else {
                                                self.shells.iter().position(|s| s.short == sh)
                                            };
                                            // ヘッダは短縮名（アイコン併記で識別可）。長い
                                            // 正式名はドロップダウン側で出す＝幅を溢れさせない。
                                            let sh_label = match sh_idx {
                                                Some(k) => self.shells[k].short.clone(),
                                                None => "既定".to_string(),
                                            };
                                            let open = self.startup_shell_open == Some(i);
                                            let arrow = if open { "▲" } else { "▼" };
                                            let htext = format!("{sh_label}　{arrow}");
                                            let header = match sh_idx
                                                .and_then(|k| self.shell_icons.get(k))
                                                .and_then(|o| o.as_ref())
                                            {
                                                Some(tex) => {
                                                    let img = egui::Image::new(tex)
                                                        .fit_to_exact_size(Vec2::splat(16.0));
                                                    ui.add_sized(
                                                        [170.0, 28.0],
                                                        egui::Button::image_and_text(img, htext),
                                                    )
                                                }
                                                None => ui.add_sized(
                                                    [170.0, 28.0],
                                                    egui::Button::new(htext),
                                                ),
                                            };
                                            let hrect = header.rect;
                                            if header.clicked() {
                                                self.startup_shell_open =
                                                    if open { None } else { Some(i) };
                                            }
                                            // 色スウォッチ（bg＝表示色／accent）。クリックでパレット。
                                            ui.add_space(6.0);
                                            let bg_resp = color_swatch(ui, bg_disp);
                                            if bg_resp.clicked() {
                                                self.startup_color_open =
                                                    if self.startup_color_open == Some((i, false)) {
                                                        None
                                                    } else {
                                                        Some((i, false))
                                                    };
                                            }
                                            ui.add_space(4.0);
                                            let ac_resp = color_swatch(ui, res_accent);
                                            if ac_resp.clicked() {
                                                self.startup_color_open =
                                                    if self.startup_color_open == Some((i, true)) {
                                                        None
                                                    } else {
                                                        Some((i, true))
                                                    };
                                            }
                                            // 色パレットのポップアップ（bg / accent）
                                            let pop_frame = egui::Frame::new()
                                                .fill(Color32::from_gray(40))
                                                .stroke(egui::Stroke::new(
                                                    1.0,
                                                    Color32::from_gray(70),
                                                ))
                                                .corner_radius(6.0)
                                                .inner_margin(egui::Margin::same(6));
                                            if self.startup_color_open == Some((i, false)) {
                                                let mut pick: Option<Option<Color32>> = None;
                                                let area = egui::Area::new(egui::Id::new((
                                                    "startup_bg_pop",
                                                    i,
                                                )))
                                                .order(egui::Order::Tooltip)
                                                .fixed_pos(
                                                    bg_resp.rect.left_bottom()
                                                        + Vec2::new(0.0, 2.0),
                                                )
                                                .show(ctx, |ui| {
                                                    pop_frame.show(ui, |ui| {
                                                        ui.set_width(176.0);
                                                        if ui
                                                            .selectable_label(
                                                                self.startup_tabs[i].bg.is_empty(),
                                                                "既定",
                                                            )
                                                            .clicked()
                                                        {
                                                            pick = Some(None);
                                                        }
                                                        if let Some(c) = palette_picker(
                                                            ui, &BG_DISPLAY, &BG_APPLY, res_bg,
                                                        ) {
                                                            pick = Some(Some(c));
                                                        }
                                                    });
                                                });
                                                let arect = area.response.rect;
                                                if let Some(sel) = pick {
                                                    self.startup_tabs[i].bg = match sel {
                                                        Some(c) => color_to_hex(c),
                                                        None => String::new(),
                                                    };
                                                    self.startup_color_open = None;
                                                } else if ctx.input(|i| i.pointer.any_pressed()) {
                                                    if let Some(p) =
                                                        ctx.input(|i| i.pointer.interact_pos())
                                                    {
                                                        if !bg_resp.rect.contains(p)
                                                            && !arect.contains(p)
                                                        {
                                                            self.startup_color_open = None;
                                                        }
                                                    }
                                                }
                                            }
                                            if self.startup_color_open == Some((i, true)) {
                                                let mut pick: Option<Option<Color32>> = None;
                                                let area = egui::Area::new(egui::Id::new((
                                                    "startup_ac_pop",
                                                    i,
                                                )))
                                                .order(egui::Order::Tooltip)
                                                .fixed_pos(
                                                    ac_resp.rect.left_bottom()
                                                        + Vec2::new(0.0, 2.0),
                                                )
                                                .show(ctx, |ui| {
                                                    pop_frame.show(ui, |ui| {
                                                        ui.set_width(176.0);
                                                        if ui
                                                            .selectable_label(
                                                                self.startup_tabs[i]
                                                                    .accent
                                                                    .is_empty(),
                                                                "既定",
                                                            )
                                                            .clicked()
                                                        {
                                                            pick = Some(None);
                                                        }
                                                        if let Some(c) = palette_picker(
                                                            ui,
                                                            &ACCENT_PALETTE,
                                                            &ACCENT_PALETTE,
                                                            res_accent,
                                                        ) {
                                                            pick = Some(Some(c));
                                                        }
                                                    });
                                                });
                                                let arect = area.response.rect;
                                                if let Some(sel) = pick {
                                                    self.startup_tabs[i].accent = match sel {
                                                        Some(c) => color_to_hex(c),
                                                        None => String::new(),
                                                    };
                                                    self.startup_color_open = None;
                                                } else if ctx.input(|i| i.pointer.any_pressed()) {
                                                    if let Some(p) =
                                                        ctx.input(|i| i.pointer.interact_pos())
                                                    {
                                                        if !ac_resp.rect.contains(p)
                                                            && !arect.contains(p)
                                                        {
                                                            self.startup_color_open = None;
                                                        }
                                                    }
                                                }
                                            }
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    // ゴミ箱＝削除、太い上下矢印＝並べ替え
                                                    if settings_icon_btn(ui, "\u{e872}", true) {
                                                        to_remove = Some(i);
                                                    }
                                                    ui.add_space(2.0);
                                                    if settings_icon_btn(ui, "\u{e5db}", false) {
                                                        to_move = Some((i, 1));
                                                    }
                                                    ui.add_space(2.0);
                                                    if settings_icon_btn(ui, "\u{e5d8}", false) {
                                                        to_move = Some((i, -1));
                                                    }
                                                },
                                            );

                                            // 前面ドロップダウン（このカードが開いている時）
                                            if self.startup_shell_open == Some(i) {
                                                let dd_frame = egui::Frame::new()
                                                    .fill(Color32::from_gray(40))
                                                    .stroke(egui::Stroke::new(
                                                        1.0,
                                                        Color32::from_gray(70),
                                                    ))
                                                    .corner_radius(6.0)
                                                    .inner_margin(egui::Margin::same(4));
                                                let mut picked: Option<String> = None;
                                                let area = egui::Area::new(egui::Id::new((
                                                    "startup_shell_dd",
                                                    i,
                                                )))
                                                .order(egui::Order::Tooltip)
                                                .fixed_pos(
                                                    hrect.left_bottom() + Vec2::new(0.0, 2.0),
                                                )
                                                .show(ctx, |ui| {
                                                    dd_frame.show(ui, |ui| {
                                                        // 幅はヘッダに連動させず内容に合わせる
                                                        // （長い正式名も折返さず1行で表示）。
                                                        egui::ScrollArea::vertical()
                                                            .max_height(220.0)
                                                            .show(ui, |ui| {
                                                                if ui
                                                                    .selectable_label(
                                                                        sh.is_empty(),
                                                                        "既定",
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    picked = Some(String::new());
                                                                }
                                                                if let Some(k) = shell_list(
                                                                    ui,
                                                                    &self.shells,
                                                                    &self.shell_icons,
                                                                    sh_idx,
                                                                ) {
                                                                    picked = Some(
                                                                        self.shells[k]
                                                                            .short
                                                                            .clone(),
                                                                    );
                                                                }
                                                            });
                                                    });
                                                });
                                                let arect = area.response.rect;
                                                if let Some(p) = picked {
                                                    self.startup_tabs[i].shell = p;
                                                    self.startup_shell_open = None;
                                                } else if ctx.input(|i| i.pointer.any_pressed())
                                                {
                                                    if let Some(pos) =
                                                        ctx.input(|i| i.pointer.interact_pos())
                                                    {
                                                        if !hrect.contains(pos)
                                                            && !arect.contains(pos)
                                                        {
                                                            self.startup_shell_open = None;
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.startup_tabs[i].cwd,
                                            )
                                            .hint_text("初期ディレクトリ（空＝既定）")
                                            .desired_width(f32::INFINITY),
                                        );
                                        ui.add_space(4.0);
                                        ui.add(
                                            egui::TextEdit::multiline(
                                                &mut self.startup_tabs[i].commands_buf,
                                            )
                                            .hint_text("起動コマンド（1行に1つ）")
                                            .desired_rows(2)
                                            .desired_width(f32::INFINITY),
                                        );
                                    });
                                // カード矩形を記録し、左端の余白にドラッグハンドル（点）を描く。
                                let card_rect = card_resp.response.rect;
                                card_rects.push(card_rect);
                                // パネル端側の余白を [アクセントバー｜ハンドル点｜内容] と
                                // 等間隔に割り付ける。handle_rect＝ドラッグ判定（余白全体）。
                                let (handle_rect, dots_rect, abar) = if panel_right {
                                    let r = card_rect.right();
                                    (
                                        egui::Rect::from_min_max(
                                            Pos2::new(r - 30.0, card_rect.top()),
                                            card_rect.right_bottom(),
                                        ),
                                        egui::Rect::from_min_max(
                                            Pos2::new(r - 25.0, card_rect.top()),
                                            Pos2::new(r - 13.0, card_rect.bottom()),
                                        ),
                                        egui::Rect::from_min_max(
                                            Pos2::new(r - 8.0, card_rect.top() + 6.0),
                                            Pos2::new(r - 5.0, card_rect.bottom() - 6.0),
                                        ),
                                    )
                                } else {
                                    let l = card_rect.left();
                                    (
                                        egui::Rect::from_min_max(
                                            card_rect.left_top(),
                                            Pos2::new(l + 30.0, card_rect.bottom()),
                                        ),
                                        egui::Rect::from_min_max(
                                            Pos2::new(l + 13.0, card_rect.top()),
                                            Pos2::new(l + 25.0, card_rect.bottom()),
                                        ),
                                        egui::Rect::from_min_max(
                                            Pos2::new(l + 5.0, card_rect.top() + 6.0),
                                            Pos2::new(l + 8.0, card_rect.bottom() - 6.0),
                                        ),
                                    )
                                };
                                let hresp = ui.interact(
                                    handle_rect,
                                    egui::Id::new(("card_grip", i)),
                                    Sense::drag(),
                                );
                                let grip_col = if dragging_this || hresp.hovered() {
                                    TAB_TEXT_ACTIVE
                                } else {
                                    Color32::from_gray(90)
                                };
                                draw_grip_dots(ui.painter(), dots_rect, grip_col);
                                ui.painter().rect_filled(abar, 2.0, res_accent);
                                if hresp.hovered() || dragging_this {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                }
                                if hresp.drag_started() {
                                    if let Some(p) = pointer {
                                        self.card_drag = Some(TabDrag {
                                            from: i,
                                            grab_dy: p.y - card_rect.top(),
                                        });
                                    }
                                }
                                ui.add_space(6.0);
                            }

                            // ---- D&D：挿入線＋前面ゴースト、ドロップで確定 ----
                            if let Some(drag) = self.card_drag {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                let released = ui.input(|i| !i.pointer.any_down());
                                if let (Some(p), true) = (pointer, drag.from < card_rects.len()) {
                                    // 可視範囲の上下端に持っていったら自動スクロール（端に近いほど速い）。
                                    let clip = ui.clip_rect();
                                    let edge = 30.0;
                                    if p.y > clip.bottom() - edge {
                                        let s = ((p.y - (clip.bottom() - edge)) / edge).clamp(0.0, 1.0);
                                        ui.scroll_with_delta(Vec2::new(0.0, -(2.0 + 12.0 * s)));
                                    } else if p.y < clip.top() + edge {
                                        let s = (((clip.top() + edge) - p.y) / edge).clamp(0.0, 1.0);
                                        ui.scroll_with_delta(Vec2::new(0.0, 2.0 + 12.0 * s));
                                    }
                                    let target =
                                        card_rects.iter().filter(|r| p.y > r.center().y).count();
                                    let (x0, x1) = card_rects
                                        .first()
                                        .map(|r| (r.left(), r.right()))
                                        .unwrap_or((0.0, 0.0));
                                    let line_y = if target < card_rects.len() {
                                        card_rects[target].top() - 3.0
                                    } else {
                                        card_rects.last().map_or(0.0, |r| r.bottom() + 2.0)
                                    };
                                    ui.painter().hline(
                                        egui::Rangef::new(x0, x1),
                                        line_y,
                                        egui::Stroke::new(2.0, ACCENT_GREEN),
                                    );
                                    // ゴースト：掴んだカードの実物を再現して前面に追従させる
                                    let card_h = card_rects[drag.from].height();
                                    let top = p.y - drag.grab_dy;
                                    let grect = egui::Rect::from_min_size(
                                        Pos2::new(x0, top),
                                        Vec2::new(x1 - x0, card_h),
                                    );
                                    let gp = ui.ctx().layer_painter(egui::LayerId::new(
                                        egui::Order::Tooltip,
                                        egui::Id::new("card_drag_ghost"),
                                    ));
                                    if let Some(tab) = self.startup_tabs.get(drag.from) {
                                        let gbg =
                                            color_from_hex(&tab.bg).unwrap_or(self.default_bg);
                                        let gac = color_from_hex(&tab.accent)
                                            .unwrap_or(self.default_accent);
                                        draw_card_ghost(
                                            &gp,
                                            grect,
                                            tab,
                                            &self.shells,
                                            &self.shell_icons,
                                            panel_right,
                                            gbg,
                                            gac,
                                        );
                                    }
                                    if released {
                                        move_card(&mut self.startup_tabs, drag.from, target);
                                    }
                                }
                                if released {
                                    self.card_drag = None;
                                }
                                ui.ctx().request_repaint();
                            }
                        });
                    if let Some(i) = to_remove
                        && i < self.startup_tabs.len()
                    {
                        self.startup_tabs.remove(i);
                    }
                    if let Some((i, dir)) = to_move {
                        let j = i as isize + dir;
                        if j >= 0 && (j as usize) < self.startup_tabs.len() {
                            self.startup_tabs.swap(i, j as usize);
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
                                perform_uninstall(scope, delete);
                                // 本体を閉じてファイルロックを解放し、インストール先の自己削除を完了させる。
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
    fn move_tab(&mut self, from: usize, target: usize) {
        if from >= self.sessions.len() {
            return;
        }
        let s = self.sessions.remove(from);
        let insert = if target > from { target - 1 } else { target }.min(self.sessions.len());
        self.sessions.insert(insert, s);
        self.active = insert; // 掴んだタブはドラッグ開始時にアクティブ化済み
    }
}

// 縦タブ1個を描く。戻り値 = (本体レスポンス, ×クリック=閉じる, 起動タブに追加).
// 本体レスポンスから 切替=clicked / 改名=double_clicked / 並べ替え=drag_* を呼び元が拾う。
// dragging＝このタブが今ドラッグ中（元スロットは淡く沈めてゴーストに主役を譲る）。
fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
    session: &mut Session,
    selected: bool,
    closable: bool,
    dragging: bool,
    mirror: bool, // パネルが右配置のとき内容を左右反転
) -> (egui::Response, bool, bool) {
    let w = ui.available_width();
    let h = 30.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click_and_drag());

    // ドラッグ中は元スロットを淡いプレースホルダにして、ポインタ追従のゴーストに主役を譲る。
    if dragging {
        ui.painter()
            .rect_filled(rect, 6.0, Color32::from_gray(34));
        return (resp, false, false);
    }

    // 閉じる×の当たり判定。左配置=右端 / 右配置=左端（本文と地続きな辺の反対側）。
    let close = if closable {
        let cx = if mirror { rect.left() + 13.0 } else { rect.right() - 13.0 };
        let cr = egui::Rect::from_center_size(Pos2::new(cx, rect.center().y), Vec2::splat(18.0));
        let id = ui.id().with((idx, "close"));
        Some((cr, ui.interact(cr, id, Sense::click())))
    } else {
        None
    };

    let painter = ui.painter();
    // アクティブ：このタブの背景色で塗り、外側の角だけ丸める＋外側辺にアクセントバー。
    // （本文と地続きにする辺はフラット。mirror で左右を入れ替える）
    // 非アクティブ：地のまま、hover で薄く反応。
    if selected {
        let radius = if mirror {
            egui::CornerRadius { nw: 0, ne: 7, sw: 0, se: 7 }
        } else {
            egui::CornerRadius { nw: 7, ne: 0, sw: 7, se: 0 }
        };
        painter.rect_filled(rect, radius, session.bg);
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
        painter.rect_filled(bar, 2.0, session.accent);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, TAB_HOVER_BG);
    }
    // タイトルは常に左寄せ（UIが反転しても文字の向きは変えない）。
    // mirror 時は左端に来る × を避けて少し内側から描く。
    let tcol = if selected { TAB_TEXT_ACTIVE } else { TAB_TEXT };
    let title_x = if mirror { rect.left() + 28.0 } else { rect.left() + 12.0 };
    painter.text(
        Pos2::new(title_x, rect.center().y),
        Align2::LEFT_CENTER,
        &session.title,
        FontId::proportional(14.0),
        tcol,
    );
    // ×
    if let Some((cr, cresp)) = &close {
        let cc = if cresp.hovered() {
            Color32::from_gray(235)
        } else {
            Color32::from_gray(125)
        };
        painter.text(cr.center(), Align2::CENTER_CENTER, "×", FontId::proportional(15.0), cc);
    }

    // 右クリック：このタブの背景色・アクセント色の選択／起動タブへの追加
    let mut add_startup = false;
    resp.context_menu(|ui| {
        ui.set_min_width(196.0);
        ui.label(egui::RichText::new("背景").color(TAB_TEXT));
        palette_row(ui, &mut session.bg, &BG_DISPLAY, &BG_APPLY);
        ui.add_space(6.0);
        ui.label(egui::RichText::new("アクセント").color(TAB_TEXT));
        palette_row(ui, &mut session.accent, &ACCENT_PALETTE, &ACCENT_PALETTE);
        ui.add_space(8.0);
        if ui.button("標準に戻す").clicked() {
            session.bg = DEFAULT_BG;
            session.accent = ACCENT_GREEN;
        }
        ui.separator();
        if ui.button("起動タブに追加").clicked() {
            add_startup = true;
            ui.close();
        }
    });

    let close_clicked = close.as_ref().map_or(false, |(_, cresp)| cresp.clicked());
    (resp, close_clicked, add_startup)
}

// 12色パレットから1色選ぶ（選択中は白枠）
// スワッチは display 色で見せ、選択すると apply 色を適用する（index で対応）。
fn palette_row(ui: &mut egui::Ui, current: &mut Color32, display: &[Color32], apply: &[Color32]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(4.0);
        for (d, a) in display.iter().zip(apply.iter()) {
            let mut b = egui::Button::new("")
                .fill(*d)
                .corner_radius(4.0)
                .min_size(Vec2::splat(20.0));
            if *current == *a {
                b = b.stroke(egui::Stroke::new(2.0, Color32::WHITE));
            }
            if ui.add(b).clicked() {
                *current = *a;
            }
        }
    });
}

// パレットから1色選ばせ、選ばれた「適用色」を返す（palette_row の戻り値版）。
fn palette_picker(
    ui: &mut egui::Ui,
    display: &[Color32],
    apply: &[Color32],
    selected: Color32,
) -> Option<Color32> {
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(4.0);
        for (d, a) in display.iter().zip(apply.iter()) {
            let mut b = egui::Button::new("")
                .fill(*d)
                .corner_radius(4.0)
                .min_size(Vec2::splat(20.0));
            if selected == *a {
                b = b.stroke(egui::Stroke::new(2.0, Color32::WHITE));
            }
            if ui.add(b).clicked() {
                chosen = Some(*a);
            }
        }
    });
    chosen
}

// 28x28 の色スウォッチ（カード内の bg/accent 表示＆クリックでパレットを開く）。
fn color_swatch(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 5.0, color);
    let border = if resp.hovered() {
        Color32::WHITE
    } else {
        Color32::from_gray(90)
    };
    painter.rect_stroke(rect, 5.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// 1セッション→起動タブ定義へ変換（スナップショット／右クリック追加で共用）。
// コマンドは追跡していないので空。
fn startup_tab_of(s: &Session) -> StartupTab {
    StartupTab {
        name: s.title.clone(),
        shell: s.shell_short.clone(),
        cwd: s.cwd.clone(),
        bg: color_to_hex(s.bg),
        accent: color_to_hex(s.accent),
        commands: Vec::new(),
        ..Default::default()
    }
}

// 設定ページのボタンを自前描画する（egui Button の状態切替でホバー時に位置が
// ズレるのを避け、サイズ・位置を完全固定。ホバーは塗りを少し明るくするだけ）。
fn settings_btn(ui: &mut egui::Ui, label: &str, fill: Color32, border: Color32) -> bool {
    let font = FontId::proportional(14.0);
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font, TAB_TEXT_ACTIVE));
    let size = Vec2::new(galley.size().x + 28.0, (galley.size().y + 14.0).max(30.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let lift = |c: Color32, d: u8| {
        Color32::from_rgb(
            c.r().saturating_add(d),
            c.g().saturating_add(d),
            c.b().saturating_add(d),
        )
    };
    let bg = if resp.hovered() { lift(fill, 14) } else { fill };
    let painter = ui.painter();
    painter.rect(
        rect,
        6.0,
        bg,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let text_pos = rect.center() - galley.size() * 0.5;
    painter.galley(text_pos, galley, TAB_TEXT_ACTIVE);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

// 主アクション（やや明るい地）／副次・中立アクション（暗い地）。緑は選択専用なので使わない。
fn primary_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, Color32::from_gray(50), Color32::from_gray(78))
}

fn ghost_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, Color32::from_gray(34), Color32::from_gray(60))
}

// 破壊的アクション（初期化・アンインストール）。赤地で「戻せない操作」を示す。
fn danger_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, DANGER_FILL, DANGER_BORDER)
}

// カード左端のドラッグハンドル＝2列×3行の点（「クリリンの額」）。
fn draw_grip_dots(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let dx = 3.0;
    let dy = 6.0;
    for &x in &[cx - dx, cx + dx] {
        for &y in &[cy - dy, cy, cy + dy] {
            painter.circle_filled(Pos2::new(x, y), 1.4, color);
        }
    }
}

// ゴースト内の入力欄1つ（角丸の箱＋テキスト or ヒント）を半透明で描く。
fn ghost_field(painter: &egui::Painter, rect: egui::Rect, text: &str, hint: &str, a: u8) {
    painter.rect_filled(rect, 5.0, Color32::from_rgba_unmultiplied(18, 18, 18, a));
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(70, 70, 70, a)),
        egui::StrokeKind::Inside,
    );
    let (s, col) = if text.is_empty() {
        (hint, Color32::from_rgba_unmultiplied(120, 120, 120, a))
    } else {
        (text, Color32::from_rgba_unmultiplied(220, 220, 220, a))
    };
    painter.text(
        Pos2::new(rect.left() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        s,
        FontId::proportional(13.0),
        col,
    );
}

// ドラッグ中のゴースト＝掴んだカードの実物を半透明で再現（名前/シェル/cwd/コマンド）。
fn draw_card_ghost(
    painter: &egui::Painter,
    rect: egui::Rect,
    tab: &StartupTab,
    shells: &[ShellSpec],
    icons: &[Option<egui::TextureHandle>],
    mirror: bool,
    bg: Color32,
    accent: Color32,
) {
    let a = 240u8;
    let bg_a = Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), a);
    painter.rect_filled(rect, 6.0, bg_a);
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(55, 55, 55, a)),
        egui::StrokeKind::Inside,
    );
    // ハンドル点とアクセントバー（カードと同じ割り付け：等間隔）
    let (dots_rect, abar) = if mirror {
        let r = rect.right();
        (
            egui::Rect::from_min_max(
                Pos2::new(r - 25.0, rect.top()),
                Pos2::new(r - 13.0, rect.bottom()),
            ),
            egui::Rect::from_min_max(
                Pos2::new(r - 8.0, rect.top() + 6.0),
                Pos2::new(r - 5.0, rect.bottom() - 6.0),
            ),
        )
    } else {
        let l = rect.left();
        (
            egui::Rect::from_min_max(
                Pos2::new(l + 13.0, rect.top()),
                Pos2::new(l + 25.0, rect.bottom()),
            ),
            egui::Rect::from_min_max(
                Pos2::new(l + 5.0, rect.top() + 6.0),
                Pos2::new(l + 8.0, rect.bottom() - 6.0),
            ),
        )
    };
    draw_grip_dots(painter, dots_rect, TAB_TEXT_ACTIVE);
    painter.rect_filled(abar, 2.0, accent);
    let cl = rect.left() + if mirror { 8.0 } else { 30.0 };
    let cr = rect.right() - if mirror { 30.0 } else { 8.0 };
    let top = rect.top() + 8.0;
    // 名前
    ghost_field(
        painter,
        egui::Rect::from_min_size(Pos2::new(cl, top), Vec2::new(150.0, 28.0)),
        &tab.name,
        "名前",
        a,
    );
    // シェル（アイコン＋短縮名）
    let sh_idx = if tab.shell.is_empty() {
        None
    } else {
        shells.iter().position(|s| s.short == tab.shell)
    };
    let sh_label = match sh_idx {
        Some(k) => shells[k].short.clone(),
        None => "既定".to_string(),
    };
    let shell_box = egui::Rect::from_min_size(Pos2::new(cl + 156.0, top), Vec2::new(150.0, 28.0));
    painter.rect_filled(shell_box, 5.0, Color32::from_rgba_unmultiplied(40, 40, 40, a));
    painter.rect_stroke(
        shell_box,
        5.0,
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(70, 70, 70, a)),
        egui::StrokeKind::Inside,
    );
    let mut tx = shell_box.left() + 8.0;
    if let Some(tex) = sh_idx.and_then(|k| icons.get(k)).and_then(|o| o.as_ref()) {
        let img_rect =
            egui::Rect::from_min_size(Pos2::new(tx, shell_box.center().y - 8.0), Vec2::splat(16.0));
        painter.image(
            tex.id(),
            img_rect,
            egui::Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::from_white_alpha(a),
        );
        tx += 22.0;
    }
    painter.text(
        Pos2::new(tx, shell_box.center().y),
        Align2::LEFT_CENTER,
        &sh_label,
        FontId::proportional(13.0),
        Color32::from_rgba_unmultiplied(220, 220, 220, a),
    );
    // cwd
    ghost_field(
        painter,
        egui::Rect::from_min_size(Pos2::new(cl, top + 32.0), Vec2::new(cr - cl, 22.0)),
        &tab.cwd,
        "初期ディレクトリ（空＝既定）",
        a,
    );
    // コマンド（先頭1行のみ表示）
    let cmd_first = tab.commands_buf.lines().next().unwrap_or("");
    ghost_field(
        painter,
        egui::Rect::from_min_max(Pos2::new(cl, top + 58.0), Pos2::new(cr, rect.bottom() - 8.0)),
        cmd_first,
        "起動コマンド（1行に1つ）",
        a,
    );
}

// 起動タブカードを from から target（挿入境界 0..=len）へ移動。move_tab と同じ補正。
fn move_card(tabs: &mut Vec<StartupTab>, from: usize, target: usize) {
    if from >= tabs.len() {
        return;
    }
    let item = tabs.remove(from);
    let insert = if target > from { target - 1 } else { target }.min(tabs.len());
    tabs.insert(insert, item);
}

// 28x28 の正方アイコンボタン（Material Icons グリフ）。行内の他コントロールと高さを
// 揃えるため固定サイズ。danger=true はホバーで赤みを帯びる（削除用）。
fn settings_icon_btn(ui: &mut egui::Ui, glyph: &str, danger: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let hovered = resp.hovered();
    let bg = if hovered {
        if danger {
            Color32::from_rgb(90, 40, 40)
        } else {
            Color32::from_gray(52)
        }
    } else {
        Color32::from_gray(34)
    };
    let fg = if hovered {
        if danger {
            Color32::from_rgb(0xF0, 0xA0, 0xA0)
        } else {
            TAB_TEXT_ACTIVE
        }
    } else {
        Color32::from_gray(170)
    };
    let galley =
        ui.ctx()
            .fonts_mut(|f| f.layout_no_wrap(glyph.to_owned(), FontId::proportional(18.0), fg));
    let painter = ui.painter();
    painter.rect(
        rect,
        6.0,
        bg,
        egui::Stroke::new(1.0, Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, fg);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

// 設定ページ左の縦ナビ1項目。メインタブと同じ「選択＝緑アクセントバー＋暗い地」。
fn settings_nav_item(
    ui: &mut egui::Ui,
    current: &mut SettingsTab,
    this: SettingsTab,
    label: &str,
    mirror: bool,
) {
    let selected = *current == this;
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 4.0, Color32::from_gray(40));
        // ナビが右にある時はアクセントバーも右端（窓の外側辺）に出す。
        let bar = if mirror {
            egui::Rect::from_min_max(
                Pos2::new(rect.right() - 3.0, rect.top() + 5.0),
                Pos2::new(rect.right(), rect.bottom() - 5.0),
            )
        } else {
            egui::Rect::from_min_max(
                Pos2::new(rect.left(), rect.top() + 5.0),
                Pos2::new(rect.left() + 3.0, rect.bottom() - 5.0),
            )
        };
        painter.rect_filled(bar, 2.0, ACCENT_GREEN);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, TAB_HOVER_BG);
    }
    let col = if selected { TAB_TEXT_ACTIVE } else { TAB_TEXT };
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        col,
    );
    if resp.clicked() {
        *current = this;
    }
}

// シェル一覧を「アイコン＋ラベル」のボタンで縦に並べ、選ばれた index を返す。
// 新規タブメニューと設定モーダルで共用。selected を渡すとその行を強調表示する。
fn shell_list(
    ui: &mut egui::Ui,
    shells: &[ShellSpec],
    icons: &[Option<egui::TextureHandle>],
    selected: Option<usize>,
) -> Option<usize> {
    let mut chosen = None;
    for (idx, sh) in shells.iter().enumerate() {
        let img = icons
            .get(idx)
            .and_then(|o| o.as_ref())
            .map(|tex| egui::Image::new(tex).fit_to_exact_size(Vec2::splat(16.0)));
        // 地は透過＝モーダル/メニューの暗い背景に乗せる（PS7 の濃紺ロゴが沈まない）。
        // 選択中はアクセント枠だけで示す（塗りつぶすと暗いアイコンが消えるため）。
        let mut btn = match img {
            Some(img) => egui::Button::image_and_text(img, sh.label.as_str()),
            None => egui::Button::new(sh.label.as_str()),
        }
        .fill(Color32::TRANSPARENT);
        if selected == Some(idx) {
            btn = btn.stroke(egui::Stroke::new(1.0, ACCENT_GREEN));
        }
        if ui.add(btn).clicked() {
            chosen = Some(idx);
        }
    }
    chosen
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 別インスタンスから転送された「このフォルダで開く」要求を取り込む。
        // 借用を切るため一旦集めてから開く（open_dir_tab が self を可変借用するため）。
        if self.ipc_rx.is_some() {
            let msgs: Vec<IpcMsg> =
                self.ipc_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
            for m in msgs {
                match m {
                    IpcMsg::OpenDir(d) => self.open_dir_tab(&ctx, &d),
                    IpcMsg::RunScript(p) => self.open_script_tab(&ctx, &p),
                }
            }
        }

        // Ctrl+Tab で一つ下、Ctrl+Shift+Tab で一つ上のタブへ（端は循環）
        let n = self.sessions.len();
        if n > 0 && self.editing.is_none() && self.confirm_close.is_none() && !self.settings_open {
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
                        let closable = self.sessions.len() > 1;
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
                                    let (resp, close, add_startup) = draw_tab(
                                        ui,
                                        i,
                                        &mut self.sessions[i],
                                        selected,
                                        closable,
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
                            // クリックでシェル選択メニュー（アイコン付きリストは共通ヘルパ）
                            egui::Popup::menu(&add).show(|ui| {
                                ui.set_min_width(170.0);
                                let chosen = shell_list(ui, &self.shells, &self.shell_icons, None);
                                if let Some(idx) = chosen {
                                    let spec = self.shells[idx].clone();
                                    let cwd = self.default_cwd.clone();
                                    let id = self.next_id;
                                    self.next_id += 1;
                                    let mut sess = Session::spawn(
                                        &ctx,
                                        format!("{} {}", spec.short, id),
                                        &spec,
                                        &cwd,
                                    );
                                    sess.bg = self.default_bg;
                                    sess.accent = self.default_accent;
                                    self.sessions.push(sess);
                                    self.active = self.sessions.len() - 1;
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
        if let Some(i) = to_add_startup {
            if let Some(s) = self.sessions.get(i) {
                let st = startup_tab_of(s);
                // 名前・シェル・cwd が一致する起動タブが既にあれば重複追加しない。
                let dup = self
                    .startup_tabs
                    .iter()
                    .any(|e| e.name == st.name && e.shell == st.shell && e.cwd == st.cwd);
                if !dup {
                    self.startup_tabs.push(st);
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
        let block_input =
            self.editing.is_some() || self.confirm_close.is_some() || self.settings_open;

        // 入力・スクロールはアクティブなセッションにのみ流す（設定ページ表示中は止める）。
        let active_idx = self.active;
        if !block_input {
            let active = &mut self.sessions[active_idx];
            if active.handle_input(&ctx) {
                active.scroll_to_bottom();
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

        // タブを閉じる確認ダイアログ（誤クリック対策）
        if let Some(idx) = self.confirm_close {
            let title = self
                .sessions
                .get(idx)
                .map(|s| s.title.clone())
                .unwrap_or_default();
            let frame = egui::Frame::new()
                .fill(Color32::from_gray(32))
                .inner_margin(egui::Margin::same(16))
                .corner_radius(10.0)
                .stroke(egui::Stroke::new(1.0, Color32::from_gray(60)));
            let mr = egui::Modal::new(egui::Id::new("confirm_close"))
                .frame(frame)
                .show(&ctx, |ui| {
                    ui.set_width(240.0);
                    ui.label(
                        egui::RichText::new("タブを閉じますか？")
                            .size(16.0)
                            .color(Color32::from_gray(235)),
                    );
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(format!("「{title}」")).color(Color32::from_gray(185)));
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        // 「閉じる」＝破壊的操作なので赤系
                        let close = ui.add(
                            egui::Button::new(egui::RichText::new("閉じる").color(Color32::WHITE))
                                .fill(Color32::from_rgb(0xC0, 0x4A, 0x4A))
                                .corner_radius(6.0)
                                .min_size(Vec2::new(96.0, 30.0)),
                        );
                        if close.clicked() {
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
                        // 「キャンセル」＝中立。ダークに馴染ませる
                        let cancel = ui.add(
                            egui::Button::new(
                                egui::RichText::new("キャンセル").color(Color32::from_gray(235)),
                            )
                            .fill(Color32::from_gray(52))
                            .corner_radius(6.0)
                            .min_size(Vec2::new(96.0, 30.0)),
                        );
                        if cancel.clicked() {
                            self.confirm_close = None;
                        }
                    });
                });
            // 外側クリック / Esc でキャンセル
            if mr.backdrop_response.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.confirm_close = None;
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

// ---- 単一インスタンス & プロセス間通信（Windows 名前付きパイプ）----
// Explorer の「このディレクトリで開く」等で2つ目以降のプロセスが起動されたら、既に動いて
// いる本体へパスを送って自分は終了する。本体は受け口の名前付きパイプを1本だけ張り、届いた
// パスを UI スレッドへ渡して新規タブを開く。パイプは nMaxInstances=1 で作るので、2つ目の
// 生成は失敗する＝「本体が居るか」の判定を兼ねる。依存は増やさず、既に使っている windows
// クレート（＋Win32_System_Pipes 機能）だけで実装する。

// メッセージ書式は <verb>\t<payload>。cwd＝フォルダで開く、run＝スクリプトを実行。
const IPC_VERB_CWD: &str = "cwd";
const IPC_VERB_RUN: &str = "run";

// 既存インスタンスへ転送する要求。UI スレッドで対応するタブを開く。
enum IpcMsg {
    OpenDir(String),  // フォルダを既定シェルのタブで開く
    RunScript(String), // スクリプトを実行するタブを開く
}

// パイプ名。別ユーザーと衝突しないようユーザー名で分ける（per-user）。
fn ipc_pipe_name() -> Vec<u16> {
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(r"\\.\pipe\LustTermina.{user}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

// 既存インスタンスへ <verb>\t<path> を転送。本体が受け取れたら true、居なければ false。
fn ipc_try_forward(verb: &str, path: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    let name = ipc_pipe_name();
    unsafe {
        let handle = match CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return false, // 本体が居ない（パイプ無し）
        };
        let msg = format!("{verb}\t{path}");
        let ok = WriteFile(handle, Some(msg.as_bytes()), None, None).is_ok();
        let _ = CloseHandle(handle);
        ok
    }
}

// 受け口（サーバ）を1本だけ張る。既に他インスタンスが持っていれば None＝自分は本体でない。
// 本体なら常駐スレッドでクライアントを順に捌き、受信したパスを Receiver 経由で UI に渡す。
fn ipc_start_server(ctx: &egui::Context) -> Option<mpsc::Receiver<IpcMsg>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    let name = ipc_pipe_name();
    // CreateNamedPipeW は生 HANDLE を返し、失敗時は INVALID_HANDLE_VALUE。
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, // nMaxInstances=1：2つ目の生成失敗＝単一インスタンス判定を兼ねる
            0,
            8192,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None; // 既に本体が居る（受け口は張らない）
    }
    let (tx, rx) = mpsc::channel::<IpcMsg>();
    let ctx = ctx.clone();
    let raw = handle.0 as isize; // HANDLE は Send でないので数値で運ぶ
    thread::spawn(move || {
        let handle = HANDLE(raw as *mut core::ffi::c_void);
        loop {
            // クライアント接続待ち（既に接続済みのエラーも捌きたいので結果は無視）。
            unsafe {
                let _ = ConnectNamedPipe(handle, None);
            }
            let mut msg = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let mut read = 0u32;
                let ok = unsafe {
                    ReadFile(handle, Some(&mut buf), Some(&mut read as *mut u32), None).is_ok()
                };
                if !ok || read == 0 {
                    break;
                }
                msg.extend_from_slice(&buf[..read as usize]);
            }
            unsafe {
                let _ = DisconnectNamedPipe(handle);
            }
            if let Ok(s) = String::from_utf8(msg) {
                let parsed = if let Some(p) = s.strip_prefix(&format!("{IPC_VERB_CWD}\t")) {
                    (!p.is_empty()).then(|| IpcMsg::OpenDir(p.to_string()))
                } else if let Some(p) = s.strip_prefix(&format!("{IPC_VERB_RUN}\t")) {
                    (!p.is_empty()).then(|| IpcMsg::RunScript(p.to_string()))
                } else {
                    None
                };
                if let Some(m) = parsed {
                    let _ = tx.send(m);
                    ctx.request_repaint();
                }
            }
        }
    });
    Some(rx)
}

// ---- Explorer 右クリックメニュー登録（レジストリ, HKCU のみ・管理者不要）----
// 「フォルダを右クリック」= Directory\shell、「フォルダ内の空白を右クリック」= Directory\Background\shell。
// 書くのは HKCU\Software\Classes 配下の自分のキーだけ（親の shell は共有なので触らない）。
// アンインストールは自分のキーをツリーごと消す＝残骸ゼロ。Win11 の従来メニュー側（「その他の
// オプションを確認」）に出る。ラベル・キー名は下の const 一箇所に集約。
const CTX_MENU_LABEL: &str = "LustTermina で開く";
const CTX_KEY_DIR: &str = r"Software\Classes\Directory\shell\LustTermina";
const CTX_KEY_BG: &str = r"Software\Classes\Directory\Background\shell\LustTermina";

// このプロセスの exe の絶対パス（レジストリのコマンド文字列に埋める）。
fn current_exe_string() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_default()
}

// UTF-16 のヌル終端ワイド列。
fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// <root>\<subkey> を作り、(name, value) を REG_SZ で書く。name="" は既定値。
// root は HKCU（このユーザー）か HKLM（PC全体）を渡す。
fn reg_set_sz(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    name: &str,
    value: &str,
) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        REG_SZ,
    };
    let subkey_w = wide_nul(subkey);
    let name_w = wide_nul(name);
    // REG_SZ データは UTF-16LE ＋ 終端NUL をバイト列にしたもの。
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    unsafe {
        let mut hkey = HKEY::default();
        let rc = RegCreateKeyExW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if rc != ERROR_SUCCESS {
            return false;
        }
        let name_ptr = if name.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(name_w.as_ptr())
        };
        let rc2 = RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(&data));
        let _ = RegCloseKey(hkey);
        rc2 == ERROR_SUCCESS
    }
}

// <root>\<subkey> の名前付き REG_SZ 値を読む。無ければ None。
fn reg_get_sz(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    name: &str,
) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let subkey_w = wide_nul(subkey);
    let name_w = wide_nul(name);
    unsafe {
        // まず必要バイト数（終端NUL込み）を問い合わせる。
        let mut size: u32 = 0;
        let rc = RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        );
        if rc != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let rc2 = RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if rc2 != ERROR_SUCCESS {
            return None;
        }
        // バイト列を UTF-16LE として解釈し、終端NULまでを文字列にする。
        let s: Vec<u16> = buf[..(size as usize)]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&s))
    }
}

// <root>\<subkey> をツリーごと削除。既に無い場合も成功扱い。
fn reg_delete_tree(root: windows::Win32::System::Registry::HKEY, subkey: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::RegDeleteTreeW;
    let w = wide_nul(subkey);
    let rc = unsafe { RegDeleteTreeW(root, PCWSTR(w.as_ptr())) };
    rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND
}

// ---- スクリプト（.ps1/.bat/.cmd）を「LustTermina で実行」----
// 右クリックの「LustTermina で実行」で、そのスクリプトのあるフォルダを cwd にして、拡張子に
// 応じたインタープリタで直接起動する（実行後もシェルは開いたまま＝出力を確認できる）。
// 対応拡張子・ラベル・レジストリキーはここ一箇所に集約（ベタ書き散逸を避ける）。
const SCRIPT_RUN_LABEL: &str = "LustTermina で実行";
const SCRIPT_EXTS: [&str; 3] = [".ps1", ".bat", ".cmd"];

// 「プログラムから開く」(Open with) 一覧に LustTermina を候補として出すための登録先。
// 既定ハンドラは奪わない：一覧に出るだけで、ユーザーが「常に使う」を選んで初めて既定になる。
const APP_REG_KEY: &str = r"Software\Classes\Applications\lust_termina.exe";

// SystemFileAssociations\<ext>\shell\LustTerminaRun：既定ハンドラを奪わず、右クリックに項目を
// 足すだけの非侵襲な場所。アンインストールは自分のキーをツリーごと消す＝残骸ゼロ。
fn script_ctx_key(ext: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{ext}\shell\LustTerminaRun")
}

// スクリプトのあるフォルダ（cwd 用）。取れなければ空＝親プロセス継承。
fn script_parent(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string()
}

// 拡張子から「そのスクリプトを実行するシェル定義」を作る。対応外は None。
// .ps1 → PowerShell（pwsh 優先、無ければ Windows PowerShell）を -File で。
// .bat/.cmd → cmd /k（実行後もプロンプトを残す）。
fn script_run_shell(path: &str) -> Option<ShellSpec> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    match ext.as_str() {
        "ps1" => {
            let prog = which("pwsh.exe").unwrap_or_else(|| {
                format!("{sysroot}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            });
            Some(ShellSpec {
                label: "PowerShell".into(),
                short: "ps1".into(),
                program: prog,
                args: vec![
                    "-NoExit".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    path.to_string(),
                ],
                icon_source: None,
            })
        }
        "bat" | "cmd" => Some(ShellSpec {
            label: "cmd".into(),
            short: ext,
            program: format!("{sysroot}\\System32\\cmd.exe"),
            args: vec!["/k".into(), path.to_string()],
            icon_source: None,
        }),
        _ => None,
    }
}

// スクリプト実行タブ用の Session を1つ作る。拡張子で判定できなければ既定シェルでそのフォルダを
// 開くだけ（実行はしないが、フォルダは開く＝握り潰さない）。
fn spawn_script_session(
    ctx: &egui::Context,
    path: &str,
    shells: &[ShellSpec],
    default_shell: usize,
    bg: Color32,
    accent: Color32,
) -> Session {
    let spec = script_run_shell(path).unwrap_or_else(|| shells[default_shell].clone());
    let dir = script_parent(path);
    let title = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| spec.short.clone());
    let mut s = Session::spawn(ctx, title, &spec, &dir);
    s.bg = bg;
    s.accent = accent;
    s
}

// 右クリック「LustTermina で開く」を登録。指定した exe パスを指す（インストーラは
// インストール先を、単独の --install-context-menu は現在の exe を渡す）。
// フォルダ選択時は %1（そのフォルダ）、背景クリック時は %V（今開いているフォルダ）。
fn install_context_menu(root: windows::Win32::System::Registry::HKEY, exe: &str) -> bool {
    if exe.is_empty() {
        return false;
    }
    let cmd_dir = format!("\"{exe}\" --cwd \"%1\"");
    let cmd_bg = format!("\"{exe}\" --cwd \"%V\"");
    let mut ok = reg_set_sz(root, CTX_KEY_DIR, "", CTX_MENU_LABEL)
        && reg_set_sz(root, CTX_KEY_DIR, "Icon", exe)
        && reg_set_sz(root, &format!(r"{CTX_KEY_DIR}\command"), "", &cmd_dir)
        && reg_set_sz(root, CTX_KEY_BG, "", CTX_MENU_LABEL)
        && reg_set_sz(root, CTX_KEY_BG, "Icon", exe)
        && reg_set_sz(root, &format!(r"{CTX_KEY_BG}\command"), "", &cmd_bg);
    // スクリプト（.ps1/.bat/.cmd）右クリックに「LustTermina で実行」を追加。%1 はそのファイル。
    let cmd_run = format!("\"{exe}\" --run \"%1\"");
    for ext in SCRIPT_EXTS {
        let key = script_ctx_key(ext);
        ok = ok
            && reg_set_sz(root, &key, "", SCRIPT_RUN_LABEL)
            && reg_set_sz(root, &key, "Icon", exe)
            && reg_set_sz(root, &format!(r"{key}\command"), "", &cmd_run);
    }
    // 「プログラムから開く」一覧に候補として出す（既定は奪わない。SupportedTypes で対象拡張子の
    // 「おすすめ」に載せる）。open verb は右クリックの実行と同じ --run。
    ok = ok
        && reg_set_sz(root, APP_REG_KEY, "FriendlyAppName", "LustTermina")
        && reg_set_sz(root, &format!(r"{APP_REG_KEY}\DefaultIcon"), "", exe)
        && reg_set_sz(root, &format!(r"{APP_REG_KEY}\shell\open\command"), "", &cmd_run);
    for ext in SCRIPT_EXTS {
        ok = ok && reg_set_sz(root, &format!(r"{APP_REG_KEY}\SupportedTypes"), ext, "");
    }
    ok
}

// 右クリック登録を解除（自分のキーをツリーごと削除）。フォルダ用・スクリプト用の両方。
fn uninstall_context_menu(root: windows::Win32::System::Registry::HKEY) -> bool {
    // 全て試みてから結果を返す（一部失敗でも残りは消す）。& は非短絡。
    let mut ok = reg_delete_tree(root, CTX_KEY_DIR) & reg_delete_tree(root, CTX_KEY_BG);
    for ext in SCRIPT_EXTS {
        ok &= reg_delete_tree(root, &script_ctx_key(ext));
    }
    ok &= reg_delete_tree(root, APP_REG_KEY);
    ok
}

// HKCU\<subkey> に DWORD を書く（アンインストール項目の NoModify/NoRepair 用）。
fn reg_set_dword(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    name: &str,
    value: u32,
) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, KEY_WRITE, REG_DWORD,
        REG_OPTION_NON_VOLATILE,
    };
    let subkey_w = wide_nul(subkey);
    let name_w = wide_nul(name);
    let data = value.to_le_bytes();
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let rc = RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), None, REG_DWORD, Some(&data));
        let _ = RegCloseKey(hkey);
        rc == ERROR_SUCCESS
    }
}

// ---- 自己インストール（固定パス配置・ショートカット・アンインストール項目）----
// 外部インストーラを使わず、アプリ自身が --install / --uninstall で入れる/消す。固定パスに
// 置くのでバージョンが変わってもショートカットや右クリックの参照先は不変（中身だけ差し替え）。
// アンインストールは書いたものを全部消す（残骸ゼロ）。config は選択式。

// 「アプリと機能」に出すアンインストール項目のキー。
const UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LustTermina";

// インストールのスコープ（このユーザーだけ / PC全体）。PC全体は管理者権限が要る。
#[derive(Clone, Copy, PartialEq)]
enum InstallScope {
    PerUser, // %LOCALAPPDATA% ＋ HKCU（UAC 不要）
    Machine, // %ProgramFiles% ＋ HKLM（UAC 必要）
}

// スコープに対応するレジストリのルート。
fn scope_root(scope: InstallScope) -> windows::Win32::System::Registry::HKEY {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    match scope {
        InstallScope::PerUser => HKEY_CURRENT_USER,
        InstallScope::Machine => HKEY_LOCAL_MACHINE,
    }
}

// インストール先フォルダ。PerUser=%LOCALAPPDATA%\Programs\LustTermina / Machine=%ProgramFiles%\LustTermina。
fn install_dir(scope: InstallScope) -> Option<std::path::PathBuf> {
    let base = match scope {
        InstallScope::PerUser => std::env::var_os("LOCALAPPDATA")?,
        InstallScope::Machine => std::env::var_os("ProgramFiles")?,
    };
    let p = std::path::Path::new(&base);
    Some(match scope {
        InstallScope::PerUser => p.join("Programs").join("LustTermina"),
        InstallScope::Machine => p.join("LustTermina"),
    })
}
fn installed_exe(scope: InstallScope) -> Option<std::path::PathBuf> {
    install_dir(scope).map(|d| d.join("lust_termina.exe"))
}
// スタートメニューのショートカット（.lnk）。PerUser=自分用 / Machine=全ユーザー用。
fn start_menu_lnk(scope: InstallScope) -> Option<std::path::PathBuf> {
    let base = match scope {
        InstallScope::PerUser => std::env::var_os("APPDATA")?,
        InstallScope::Machine => std::env::var_os("ProgramData")?,
    };
    Some(
        std::path::Path::new(&base)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("LustTermina.lnk"),
    )
}
// 設定・データのフォルダ（%APPDATA%\LustTermina）。スコープに依らず常にユーザー配下。
fn config_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(std::path::Path::new(&base).join("LustTermina"))
}
// どちらかのスコープでインストール済みなら、そのスコープを返す（PC全体を優先）。
fn installed_scope() -> Option<InstallScope> {
    [InstallScope::Machine, InstallScope::PerUser]
        .into_iter()
        .find(|&s| installed_exe(s).map(|p| p.exists()).unwrap_or(false))
}
// インストール済みの版（「アプリと機能」の DisplayVersion）を読む。無ければ None。
fn installed_version(scope: InstallScope) -> Option<String> {
    reg_get_sz(scope_root(scope), UNINSTALL_KEY, "DisplayVersion")
}
// "a.b.c" を数値タプルにして比較。running が installed より新しければ true。
// 末尾の -beta 等の非数値要素は無視する（数値の major.minor.patch のみで判定）。
fn version_is_newer(running: &str, installed: &str) -> bool {
    fn parse(v: &str) -> (u64, u64, u64) {
        let mut it = v.split(['.', '-', '+']).filter_map(|p| p.parse::<u64>().ok());
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    }
    parse(running) > parse(installed)
}
// 今このプロセスがどちらかのインストール先 exe として動いているか。
fn running_from_install() -> bool {
    let Ok(cur) = std::env::current_exe() else {
        return false;
    };
    [InstallScope::Machine, InstallScope::PerUser]
        .into_iter()
        .any(|s| installed_exe(s).as_deref() == Some(cur.as_path()))
}
// アップグレードで退避された旧 exe（lust_termina.old.exe）を起動時に掃除する。
// do_install はロック回避のため旧版をリネームで残すが、実行中プロセスは lust_termina.exe
// なので隣の .old.exe はロックされておらず安全に消せる。インストール先起動時のみ行う。
fn cleanup_old_exe() {
    if !running_from_install() {
        return;
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let _ = std::fs::remove_file(dir.join("lust_termina.old.exe"));
        }
    }
}
// 現在の exe の位置からインストールスコープを判定（アンインストール時に使う）。
// Program Files 配下なら PC全体、それ以外は このユーザー。
fn current_install_scope() -> InstallScope {
    match (std::env::current_exe(), install_dir(InstallScope::Machine)) {
        (Ok(cur), Some(m)) if cur.starts_with(&m) => InstallScope::Machine,
        _ => InstallScope::PerUser,
    }
}
// 管理者権限で動いているか。
fn is_elevated() -> bool {
    use windows::Win32::UI::Shell::IsUserAnAdmin;
    unsafe { IsUserAnAdmin().as_bool() }
}
// 自分自身を管理者として起動し直す（UAC）。指定引数で走らせ、終了を待って成否を返す。
// 未署名なので UAC は「発行元不明」の警告付きプロンプトになる。
fn relaunch_elevated(args: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe_w = wide_nul(&exe.to_string_lossy());
    let verb = wide_nul("runas");
    let params = wide_nul(args);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = PCWSTR(verb.as_ptr());
    info.lpFile = PCWSTR(exe_w.as_ptr());
    info.lpParameters = PCWSTR(params.as_ptr());
    info.nShow = SW_SHOWNORMAL.0;
    unsafe {
        if ShellExecuteExW(&mut info).is_err() {
            return false; // ユーザーが UAC を拒否した等
        }
        let h = info.hProcess;
        if h.is_invalid() {
            return false;
        }
        WaitForSingleObject(h, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(h, &mut code);
        let _ = CloseHandle(h);
        code == 0
    }
}

// MessageBox（はい/いいえ等）。UX は標準ダイアログのみで完結させ、依存を増やさない。
fn msg_box(
    text: &str,
    caption: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::MessageBoxW;
    let t = wide_nul(text);
    let c = wide_nul(caption);
    unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style) }
}

// はい／いいえの確認（警告アイコン）。はい＝true。破壊的操作の前段に使う。
fn confirm_yes_no(text: &str, caption: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONWARNING, MB_YESNO};
    msg_box(text, caption, MB_YESNO | MB_ICONWARNING).0 == IDYES.0
}

// アンインストール確認ダイアログ（TaskDialog）。データ削除はチェックボックスで選ぶ（既定オフ）。
// 戻り値: None=キャンセル、Some(delete_data)=実行（delete_data はチェック状態）。
fn ask_uninstall() -> Option<bool> {
    use windows::core::{BOOL, PCWSTR};
    use windows::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDCBF_YES_BUTTON,
        TDF_ALLOW_DIALOG_CANCELLATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::IDYES;
    let title = wide_nul("LustTermina のアンインストール");
    let main = wide_nul("LustTermina をアンインストールしますか？");
    let content =
        wide_nul("アプリ本体・スタートメニューのショートカット・右クリックの項目を削除します。");
    let verify = wide_nul("アンインストール時にデータ（設定）も削除する");
    let mut cfg: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    cfg.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as u32;
    cfg.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
    cfg.dwCommonButtons = TDCBF_YES_BUTTON | TDCBF_CANCEL_BUTTON;
    cfg.pszWindowTitle = PCWSTR(title.as_ptr());
    cfg.pszMainInstruction = PCWSTR(main.as_ptr());
    cfg.pszContent = PCWSTR(content.as_ptr());
    cfg.pszVerificationText = PCWSTR(verify.as_ptr());
    let mut pressed = 0i32;
    let mut checked = BOOL(0);
    let ok = unsafe {
        TaskDialogIndirect(&cfg, Some(&mut pressed), None, Some(&mut checked)).is_ok()
    };
    if !ok || pressed != IDYES.0 {
        return None;
    }
    Some(checked.as_bool())
}

// スタートメニュー等に .lnk ショートカットを作る（COM: IShellLinkW + IPersistFile）。
// args は起動時に渡す引数（アンインストーラ用に "--uninstall" 等。無ければ ""）。
fn create_shortcut(
    lnk: &std::path::Path,
    target: &str,
    args: &str,
    workdir: &str,
    icon: &str,
) -> bool {
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let target_w = wide_nul(target);
    let args_w = wide_nul(args);
    let work_w = wide_nul(workdir);
    let icon_w = wide_nul(icon);
    let lnk_w = wide_nul(&lnk.to_string_lossy());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(target_w.as_ptr()))?;
            link.SetArguments(PCWSTR(args_w.as_ptr()))?;
            link.SetWorkingDirectory(PCWSTR(work_w.as_ptr()))?;
            link.SetIconLocation(PCWSTR(icon_w.as_ptr()), 0)?;
            let persist: IPersistFile = link.cast()?;
            persist.Save(PCWSTR(lnk_w.as_ptr()), true)?;
            Ok(())
        })();
        CoUninitialize();
        result.is_ok()
    }
}

// 「アプリと機能」エントリを書く（UninstallString は本体の --uninstall を指す）。
fn write_uninstall_entry(
    root: windows::Win32::System::Registry::HKEY,
    exe: &str,
    dir: &str,
) -> bool {
    reg_set_sz(root, UNINSTALL_KEY, "DisplayName", "LustTermina")
        && reg_set_sz(root, UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))
        && reg_set_sz(root, UNINSTALL_KEY, "Publisher", "Capsicum0907")
        && reg_set_sz(root, UNINSTALL_KEY, "DisplayIcon", exe)
        && reg_set_sz(root, UNINSTALL_KEY, "InstallLocation", dir)
        && reg_set_sz(root, UNINSTALL_KEY, "UninstallString", &format!("\"{exe}\" --uninstall"))
        && reg_set_dword(root, UNINSTALL_KEY, "NoModify", 1)
        && reg_set_dword(root, UNINSTALL_KEY, "NoRepair", 1)
}

// 指定スコープへ実際に配置・登録する（PC全体スコープなら管理者権限で呼ばれている前提）。
// 旧版が実行中でロックされていてもリネームで避けて置き換える。
fn do_install(scope: InstallScope) -> bool {
    let (Some(dir), Some(dst)) = (install_dir(scope), installed_exe(scope)) else {
        return false;
    };
    let Ok(src) = std::env::current_exe() else {
        return false;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    if src != dst {
        // 既存 exe が実行中だと上書きコピーは失敗するので、先にリネームして退ける
        //（実行中でもリネームは可能）。退けた旧版は次回起動時の掃除に任せる。
        if dst.exists() {
            let old = dir.join("lust_termina.old.exe");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&dst, &old);
        }
        if std::fs::copy(&src, &dst).is_err() {
            return false;
        }
    }
    let exe = dst.to_string_lossy().to_string();
    let dir_s = dir.to_string_lossy().to_string();
    let root = scope_root(scope);
    if let Some(lnk) = start_menu_lnk(scope) {
        if let Some(parent) = lnk.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        create_shortcut(&lnk, &exe, "", &dir_s, &exe);
    }
    // インストールフォルダ内にアンインストーラのショートカット（--uninstall を叩く）。
    // スタートメニューは汚さず、フォルダを開けば必ず目に入る場所に置く。アンインストール時は
    // フォルダごと消えるので後始末は不要。
    create_shortcut(
        &dir.join("Uninstall LustTermina.lnk"),
        &exe,
        "--uninstall",
        &dir_s,
        &exe,
    );
    install_context_menu(root, &exe);
    write_uninstall_entry(root, &exe, &dir_s);
    true
}

// 指定スコープの登録物を全て消す。delete_config で設定・データも消す（選択式）。
// PC全体スコープなら管理者権限で呼ばれている前提。
fn do_uninstall(scope: InstallScope, delete_config: bool) -> bool {
    let root = scope_root(scope);
    uninstall_context_menu(root);
    let _ = reg_delete_tree(root, UNINSTALL_KEY);
    if let Some(lnk) = start_menu_lnk(scope) {
        let _ = std::fs::remove_file(lnk);
    }
    if delete_config {
        if let Some(cfg) = config_dir() {
            let _ = std::fs::remove_dir_all(cfg);
        }
    }
    // 実行中の自分自身（インストール先 exe）はロックされていて消せないので、本体終了を
    // 少し待ってからフォルダごと削除する小さな外部プロセスを投げておく。
    if let Some(dir) = install_dir(scope) {
        schedule_dir_deletion(&dir.to_string_lossy());
    }
    true
}

// インストールを実行。PC全体スコープで未昇格なら、UAC で自分を昇格し直して代行させる。
fn perform_install(scope: InstallScope) -> bool {
    if scope == InstallScope::Machine && !is_elevated() {
        relaunch_elevated("--do-install-machine")
    } else {
        do_install(scope)
    }
}

// アンインストールを実行。PC全体スコープで未昇格なら UAC 昇格して代行させる。
fn perform_uninstall(scope: InstallScope, delete_config: bool) -> bool {
    if scope == InstallScope::Machine && !is_elevated() {
        relaunch_elevated(&format!(
            "--do-uninstall-machine {}",
            if delete_config { 1 } else { 0 }
        ))
    } else {
        do_uninstall(scope, delete_config)
    }
}

// ask_install_scope の結果。導入する／導入せず今回だけ起動する／何もしない、の3択。
enum InstallChoice {
    Install(InstallScope), // 選んだスコープへ導入
    OpenPortable,          // 導入せずこのまま（ポータブル）起動
    Cancel,                // 何もしない（起動もしない）
}

// インストール確認＋インストール先の選択（TaskDialog のラジオボタン）。
// 各ラジオに「そのスコープの現在の版」を出す（ユーザー専用／PC全体で別々に入り得るため）。
// 既にインストール済みなら導入ボタンの文言を「更新／再インストール／ダウングレード」に切替。
// ボタンは3系統: 導入 ／「インストールせずに開く」＝今回だけ起動 ／「キャンセル」＝何もしない。
fn ask_install_scope() -> InstallChoice {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDCBF_CANCEL_BUTTON,
        TDF_ALLOW_DIALOG_CANCELLATION,
    };
    const ID_INSTALL: i32 = 1000;
    const ID_OPEN: i32 = 1001;
    const ID_PERUSER: i32 = 201;
    const ID_MACHINE: i32 = 202;
    let running_ver = env!("CARGO_PKG_VERSION");
    // 各スコープの現在の版（exe が実在する時だけ版を読む＝実際に入っている物だけ表示）。
    let scope_ver = |s: InstallScope| -> Option<String> {
        if installed_exe(s).map(|p| p.exists()).unwrap_or(false) {
            installed_version(s)
        } else {
            None
        }
    };
    let peruser_ver = scope_ver(InstallScope::PerUser);
    let machine_ver = scope_ver(InstallScope::Machine);
    let existing = installed_scope();
    let existing_ver = existing.and_then(installed_version);
    let title = wide_nul(&format!("LustTermina v{running_ver} のインストール"));
    // 文面は「新規インストール」と「既存への更新／再インストール」で切り替える。
    let (main, content, btn) = match existing_ver.as_deref() {
        Some(iv) => {
            let verb = if version_is_newer(running_ver, iv) {
                "更新"
            } else if version_is_newer(iv, running_ver) {
                "ダウングレード"
            } else {
                "再インストール"
            };
            (
                wide_nul("LustTermina は既にインストールされています"),
                wide_nul(&format!(
                    "この配布物は v{running_ver} です。インストール先を選んで{verb}してください。"
                )),
                wide_nul(verb),
            )
        }
        None => (
            wide_nul(&format!("LustTermina v{running_ver} をインストールしますか？")),
            wide_nul(
                "スタートメニューのショートカットと、右クリックの「LustTermina で開く」を追加します。",
            ),
            wide_nul("インストール"),
        ),
    };
    // ラジオ末尾に現在版を添える。未導入は「未インストール」。
    let ver_suffix = |v: &Option<String>| match v {
        Some(s) => format!("現在 v{s}"),
        None => "未インストール".to_string(),
    };
    let r1 = wide_nul(&format!(
        "このユーザー専用（管理者権限は不要）／{}",
        ver_suffix(&peruser_ver)
    ));
    let r2 = wide_nul(&format!(
        "PC 全体（管理者権限が必要）／{}",
        ver_suffix(&machine_ver)
    ));
    let open = wide_nul("インストールせずに開く");
    let radios = [
        TASKDIALOG_BUTTON { nButtonID: ID_PERUSER, pszButtonText: PCWSTR(r1.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: ID_MACHINE, pszButtonText: PCWSTR(r2.as_ptr()) },
    ];
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ID_INSTALL, pszButtonText: PCWSTR(btn.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: ID_OPEN, pszButtonText: PCWSTR(open.as_ptr()) },
    ];
    let mut cfg: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    cfg.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as u32;
    cfg.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
    cfg.dwCommonButtons = TDCBF_CANCEL_BUTTON;
    cfg.cxWidth = 200; // 幅を明示してラジオのラベル折り返しを防ぐ（0=自動だと狭くて wrap する）
    cfg.pszWindowTitle = PCWSTR(title.as_ptr());
    cfg.pszMainInstruction = PCWSTR(main.as_ptr());
    cfg.pszContent = PCWSTR(content.as_ptr());
    cfg.cButtons = buttons.len() as u32;
    cfg.pButtons = buttons.as_ptr();
    cfg.nDefaultButton = ID_INSTALL;
    cfg.cRadioButtons = radios.len() as u32;
    cfg.pRadioButtons = radios.as_ptr();
    // 既定のラジオ：既に導入先があればそこ、無ければ自分専用（UAC 不要側）。
    cfg.nDefaultRadioButton = if existing == Some(InstallScope::Machine) {
        ID_MACHINE
    } else {
        ID_PERUSER
    };
    let mut pressed = 0i32;
    let mut radio = 0i32;
    let ok =
        unsafe { TaskDialogIndirect(&cfg, Some(&mut pressed), Some(&mut radio), None).is_ok() };
    if !ok {
        return InstallChoice::Cancel;
    }
    match pressed {
        ID_INSTALL => InstallChoice::Install(if radio == ID_MACHINE {
            InstallScope::Machine
        } else {
            InstallScope::PerUser
        }),
        ID_OPEN => InstallChoice::OpenPortable,
        _ => InstallChoice::Cancel, // キャンセル / ×閉じ / Esc
    }
}

// 本体終了後にインストールフォルダをまるごと削除する（自己削除）。窓は出さない。
fn schedule_dir_deletion(dir: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let line = format!("ping 127.0.0.1 -n 3 >nul & rmdir /s /q \"{dir}\"");
    let _ = std::process::Command::new("cmd")
        .arg("/c")
        .raw_arg(&line)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
}

// 起動時の意図（コマンドライン引数から決まる）。Explorer 連携の入口はここに集約し、
// 将来 --run <script> やインストール系（--install など）もこの enum に足していく。
enum Launch {
    Normal,                    // 通常起動：起動タブ定義に従う（未インストールなら導入を促す）
    Cwd(String),               // --cwd <path>：そのフォルダでタブ1枚（起動タブ定義は無視）
    RunScript(String),         // --run <path>：スクリプトをそのフォルダで実行するタブ1枚
    Install,                   // --install：インストール（先を選ばせて）実行し終了
    Uninstall,                 // --uninstall：アンインストールして終了
    DoInstallMachine,          // --do-install-machine：PC全体へ配置（昇格済みの代行ワーカ）
    DoUninstallMachine(bool),  // --do-uninstall-machine <0|1>：PC全体を削除（昇格済みの代行ワーカ）
    InstallContextMenu,        // --install-context-menu：右クリックのみ登録して終了
    UninstallContextMenu,      // --uninstall-context-menu：右クリックのみ解除して終了
}

// 引数列（プログラム名は除いたもの）を解釈して起動意図を返す。
fn parse_launch<I: Iterator<Item = String>>(mut args: I) -> Launch {
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" => {
                if let Some(p) = args.next() {
                    return Launch::Cwd(p);
                }
            }
            "--run" => {
                if let Some(p) = args.next() {
                    return Launch::RunScript(p);
                }
            }
            "--install" => return Launch::Install,
            "--uninstall" => return Launch::Uninstall,
            "--do-install-machine" => return Launch::DoInstallMachine,
            "--do-uninstall-machine" => {
                let del = args.next().as_deref() == Some("1");
                return Launch::DoUninstallMachine(del);
            }
            "--install-context-menu" => return Launch::InstallContextMenu,
            "--uninstall-context-menu" => return Launch::UninstallContextMenu,
            _ => {}
        }
    }
    Launch::Normal
}

fn main() -> eframe::Result {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    use windows::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION;
    let launch = parse_launch(std::env::args().skip(1));
    // 窓を出さずに完結する起動（インストール系・右クリック登録/解除）は、ここで実行して終了。
    match &launch {
        Launch::Install => {
            // インストール先（このユーザー／PC全体）をラジオで選ばせて実行。
            // 明示的な導入コマンドなので、「導入」以外（キャンセル／インストールせずに開く）は何もせず終了。
            let InstallChoice::Install(scope) = ask_install_scope() else {
                return Ok(());
            };
            let ok = perform_install(scope);
            msg_box(
                if ok {
                    "LustTermina をインストールしました。"
                } else {
                    "インストールに失敗しました（または中止されました）。"
                },
                "LustTermina",
                MB_ICONINFORMATION,
            );
            std::process::exit(if ok { 0 } else { 1 });
        }
        // 昇格済みの代行ワーカ：PC全体へ配置。ダイアログは出さず終了コードで返す。
        Launch::DoInstallMachine => {
            std::process::exit(if do_install(InstallScope::Machine) { 0 } else { 1 });
        }
        Launch::Uninstall => {
            // 入っている場所（このユーザー／PC全体）を判定し、1枚の TaskDialog で
            // 確認＋データ削除の可否（チェックボックス, 既定オフ）を選ぶ。
            let scope = current_install_scope();
            let Some(delete_data) = ask_uninstall() else {
                return Ok(()); // キャンセル
            };
            perform_uninstall(scope, delete_data);
            // 削除は本体終了後に実行される。ここで静かに終了。
            return Ok(());
        }
        // 昇格済みの代行ワーカ：PC全体を削除。
        Launch::DoUninstallMachine(delete_data) => {
            do_uninstall(InstallScope::Machine, *delete_data);
            return Ok(());
        }
        Launch::InstallContextMenu => {
            // 単独の右クリック登録は このユーザー（HKCU）に対して行う。
            std::process::exit(
                if install_context_menu(HKEY_CURRENT_USER, &current_exe_string()) {
                    0
                } else {
                    1
                },
            );
        }
        Launch::UninstallContextMenu => {
            std::process::exit(if uninstall_context_menu(HKEY_CURRENT_USER) { 0 } else { 1 });
        }
        // Explorer の「このディレクトリで開く」等で2つ目以降が起動されたら、既存インスタンスへ
        // 転送して自分は窓を出さずに終了する。転送先が居なければ通常起動（自分が本体になる）。
        Launch::Cwd(path) => {
            if ipc_try_forward(IPC_VERB_CWD, path) {
                return Ok(());
            }
        }
        // スクリプト実行も、本体が居れば新タブとして合流させる。居なければ自分が本体になる。
        Launch::RunScript(path) => {
            if ipc_try_forward(IPC_VERB_RUN, path) {
                return Ok(());
            }
        }
        Launch::Normal => {
            // インストール先以外（＝ダウンロードした配布 exe 等）から起動されたら、まず
            // インストール画面を出す。未インストールなら新規、既にあれば更新／再インストールの
            // 文面になり、どちらの場合もインストール先（このユーザー／PC全体）を選べる。
            // 「インストールせずに開く」を選んだ時だけ、このままポータブル起動する。
            // 「キャンセル」は何もせず終了（起動もしない）＝直感通り。
            // ※ 開発ビルド（target/debug 等）では毎回ダイアログが出ると煩わしいので出さない。
            //   配布物は release ビルドなので実配布では常に出る。
            if !running_from_install() && !cfg!(debug_assertions) {
                match ask_install_scope() {
                    InstallChoice::Install(scope) => {
                        if perform_install(scope) {
                            // 以後は本体＝インストール版にしたいので、そちらを起動して自分は終了。
                            if let Some(exe) = installed_exe(scope) {
                                let _ = std::process::Command::new(exe).spawn();
                            }
                            return Ok(());
                        }
                        // 導入に失敗したら、フォールバックでこのまま起動する（下へ流す）。
                    }
                    InstallChoice::OpenPortable => {} // このまま起動：下へ流す
                    InstallChoice::Cancel => return Ok(()), // 何もせず終了
                }
            }
        }
    }
    // インストール先から起動した通常ケースでは、アップグレードで退避された旧 exe を掃除する。
    cleanup_old_exe();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("load window icon");
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 560.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "LustTermina",
        native_options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, launch)))),
    )
}
