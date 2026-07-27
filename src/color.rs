//! 配色定数と色解決（ANSI/パレット/UI 色）。
use eframe::egui::Color32;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor};

pub(crate) const DEFAULT_FG: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);
pub(crate) const DEFAULT_BG: Color32 = Color32::from_rgb(0x0C, 0x0C, 0x0C);

// 縦タブ用（サイドバーは本文より少し明るく、アクティブタブだけ本文色で地続きに見せる）
pub(crate) const SIDEBAR_BG: Color32 = Color32::from_gray(26);
pub(crate) const TAB_HOVER_BG: Color32 = Color32::from_gray(42);
pub(crate) const TAB_TEXT: Color32 = Color32::from_gray(225); // 非アクティブ＝白っぽく
pub(crate) const TAB_TEXT_ACTIVE: Color32 = Color32::from_gray(245);
pub(crate) const ACCENT_GREEN: Color32 = Color32::from_rgb(0x19, 0xC3, 0x7D); // アクセント（アイコンと同じ緑）
// 注意喚起（プリセット名の重複/空など、参照できない状態）用の橙。エラーではなく警告の色。
pub(crate) const WARN_COLOR: Color32 = Color32::from_rgb(0xE0, 0x8A, 0x3A);
// 破壊的アクション（データ初期化・アンインストール）用の赤。緑=選択専用とは役割を分ける。
pub(crate) const DANGER_FILL: Color32 = Color32::from_rgb(0x5A, 0x28, 0x28);
pub(crate) const DANGER_BORDER: Color32 = Color32::from_rgb(0x8A, 0x40, 0x40);

// テキスト選択のハイライト（半透明。下地の bg・セル色の上に重ねて使う）
pub(crate) const SELECTION_BG: Color32 = Color32::from_rgba_premultiplied(0x3A, 0x5A, 0x8C, 0x88);

// タブ個別色の選択パレット（背景・アクセント共通。12色）
// アクセント用（鮮やか）。アクセントバーは小面積なので彩度高めでよい。
pub(crate) const ACCENT_PALETTE: [Color32; 12] = [
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
pub(crate) const BG_DISPLAY: [Color32; 12] = [
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
pub(crate) const BG_APPLY: [Color32; 12] = [
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

pub(crate) const ANSI16: [Color32; 16] = [
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

pub(crate) fn palette256(i: u8) -> Color32 {
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

pub(crate) fn named_rgb(n: NamedColor, bold: bool, bg_default: Color32) -> Color32 {
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

pub(crate) fn resolve(c: TermColor, bold: bool, bg_default: Color32) -> Color32 {
    match c {
        TermColor::Named(n) => named_rgb(n, bold, bg_default),
        TermColor::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        TermColor::Indexed(i) => palette256(i),
    }
}

// dim/faint: 明度を落とす
pub(crate) fn dim(c: Color32) -> Color32 {
    let s = |v: u8| (v as f32 * 0.6) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}
