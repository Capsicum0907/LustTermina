//! egui の描画ヘルパ（フォント・タブ・カード・ボタン・設定 UI 部品）。
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Sense, Vec2};
use crate::color::*;
use crate::config::*;
use crate::session::Session;
use crate::shells::ShellSpec;

// CJK グリフを出すため、MS Gothic（端末向けの等幅JP）をフォールバックに登録。
// 既定の egui フォント（Hack 等）は CJK を持たないので日本語が豆腐になる。
pub(crate) fn setup_fonts(ctx: &egui::Context) {
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
// 縦タブ1個を描く。戻り値 = (本体レスポンス, ×クリック=閉じる, 起動タブに追加).
// 本体レスポンスから 切替=clicked / 改名=double_clicked / 並べ替え=drag_* を呼び元が拾う。
// dragging＝このタブが今ドラッグ中（元スロットは淡く沈めてゴーストに主役を譲る）。
pub(crate) fn draw_tab(
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
pub(crate) fn palette_row(ui: &mut egui::Ui, current: &mut Color32, display: &[Color32], apply: &[Color32]) {
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
pub(crate) fn palette_picker(
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
pub(crate) fn color_swatch(ui: &mut egui::Ui, color: Color32) -> egui::Response {
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
// プリセット名を一意にする。base が空なら既定名に、既存と衝突するなら " 2"," 3"… を付す。
// 追加・複製で使い、生成名が既存プリセットと被らないことを保証する（名前＝参照キーのため）。
pub(crate) fn unique_preset_name(base: &str, existing: &[TabSpec]) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "プリセット" } else { base };
    let taken: std::collections::HashSet<&str> =
        existing.iter().map(|p| p.name.as_str()).collect();
    if !taken.contains(base) {
        return base.to_string();
    }
    let mut k = 2;
    loop {
        let cand = format!("{base} {k}");
        if !taken.contains(cand.as_str()) {
            return cand;
        }
        k += 1;
    }
}

pub(crate) fn tab_spec_of(s: &Session) -> TabSpec {
    TabSpec {
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
pub(crate) fn settings_btn(ui: &mut egui::Ui, label: &str, fill: Color32, border: Color32) -> bool {
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

// モード切替トグル1個ぶん（マニュアル／プリセット）。Material アイコンのみの正方ボタンで、
// 自前描画によりサイズ・位置を固定（縮み・ズレを防ぐ）。選択中はアクセント枠＋明るい地・
// 明色グリフ、非選択は暗め。文字なしなので tip をホバーのツールチップに出して意味を補う。
pub(crate) fn mode_toggle_btn(ui: &mut egui::Ui, glyph: &str, tip: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let hovered = resp.hovered();
    let base = if selected {
        Color32::from_gray(56)
    } else {
        Color32::from_gray(34)
    };
    let bg = if hovered {
        Color32::from_rgb(
            base.r().saturating_add(14),
            base.g().saturating_add(14),
            base.b().saturating_add(14),
        )
    } else {
        base
    };
    let fg = if selected || hovered {
        TAB_TEXT_ACTIVE
    } else {
        Color32::from_gray(170)
    };
    let border = if selected { ACCENT_GREEN } else { Color32::from_gray(60) };
    let galley =
        ui.ctx()
            .fonts_mut(|f| f.layout_no_wrap(glyph.to_owned(), FontId::proportional(18.0), fg));
    let painter = ui.painter();
    painter.rect(rect, 6.0, bg, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() * 0.5, galley, fg);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip).clicked()
}

// 主アクション（やや明るい地）／副次・中立アクション（暗い地）。緑は選択専用なので使わない。
pub(crate) fn primary_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, Color32::from_gray(50), Color32::from_gray(78))
}

pub(crate) fn ghost_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, Color32::from_gray(34), Color32::from_gray(60))
}

// 破壊的アクション（初期化・アンインストール）。赤地で「戻せない操作」を示す。
pub(crate) fn danger_button(ui: &mut egui::Ui, label: &str) -> bool {
    settings_btn(ui, label, DANGER_FILL, DANGER_BORDER)
}

// ---- 確認モーダル（破壊的操作の直前に挟む共通形）----
const MODAL_WIDTH: f32 = 240.0;
const MODAL_BUTTON_SIZE: Vec2 = Vec2::new(96.0, 30.0);

fn modal_button(ui: &mut egui::Ui, label: &str, fill: Color32, text: Color32) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(text))
            .fill(fill)
            .corner_radius(6.0)
            .min_size(MODAL_BUTTON_SIZE),
    )
    .clicked()
}

// 「本当にやりますか？」を一枚のモーダルで尋ねる。タブを閉じる／アプリを終了する等、
// 取り返しのつかない操作の直前に挟む。呼び出し側は戻り値だけを見ればよい：
//   Some(true)  = 実行する
//   Some(false) = やめる（キャンセル・Esc・外側クリック）＝呼び出し側で表示状態を畳む
//   None        = まだ選んでいない＝出したまま次フレームへ
pub(crate) fn confirm_modal(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    detail: &str,
    confirm_label: &str,
) -> Option<bool> {
    let frame = egui::Frame::new()
        .fill(MODAL_BG)
        .inner_margin(egui::Margin::same(16))
        .corner_radius(10.0)
        .stroke(egui::Stroke::new(1.0, MODAL_BORDER));
    let mut answer = None;
    let mr = egui::Modal::new(egui::Id::new(id)).frame(frame).show(ctx, |ui| {
        ui.set_width(MODAL_WIDTH);
        ui.label(egui::RichText::new(title).size(16.0).color(MODAL_TITLE_TEXT));
        if !detail.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(detail).color(MODAL_DETAIL_TEXT));
        }
        ui.add_space(18.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // 実行＝破壊的操作なので赤系、キャンセル＝中立でダークに馴染ませる。
            if modal_button(ui, confirm_label, MODAL_DANGER_FILL, Color32::WHITE) {
                answer = Some(true);
            }
            if modal_button(ui, "キャンセル", MODAL_NEUTRAL_FILL, MODAL_TITLE_TEXT) {
                answer = Some(false);
            }
        });
    });
    // 外側クリック / Esc でキャンセル扱い
    if mr.backdrop_response.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        answer = Some(false);
    }
    answer
}

// カード左端のドラッグハンドル＝2列×3行の点（「クリリンの額」）。
pub(crate) fn draw_grip_dots(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
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
pub(crate) fn ghost_field(painter: &egui::Painter, rect: egui::Rect, text: &str, hint: &str, a: u8) {
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

// カード編集で発生する操作。呼び元がリストに対して適用する。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CardAction {
    None,
    Remove,
    Move(isize),
    Duplicate,
}

// 起動タブ行のモード切替に要る参照束。プリセットペインでは None（トグル無し・常にエディタ）。
pub(crate) struct StartupToggle<'a> {
    pub(crate) use_preset: &'a mut bool,         // 参照モードか
    pub(crate) preset_ref: &'a mut String,       // 選択中のプリセット名（未選択なら空）
    pub(crate) presets: &'a [TabSpec],           // 選択肢＆プレビューの解決元
    pub(crate) pick_open: &'a mut Option<usize>, // プリセット選択DDを開いているカード
}

// タブ定義カード1枚の編集UI（名前・シェルDD・色パレット・cwd・コマンド＋操作ボタン）と
// ドラッググリップを描く。起動タブとプリセットで共用＝見た目と操作を1本化する。
// ポップアップの開閉状態・ドラッグ状態は呼び元が保持し、参照で受け渡す。id_scope で Area ID を
// リストごとに分ける（衝突回避）。show_duplicate＝複製ボタンを出すか（プリセット側のみ）。
// toggle=Some のとき先頭にモードトグルを出し、参照モードなら項目エディタの代わりにプリセット
// 選択＋プレビューを描く。プリセット→マニュアルはその型を spec にコピー、マニュアル→プリセットは
// 未選択にする。返り値：発生した操作と、このカードの矩形（呼び元の D&D 判定に使う）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn tab_spec_card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    id_scope: &str,
    i: usize,
    spec: &mut TabSpec,
    shells: &[ShellSpec],
    shell_icons: &[Option<egui::TextureHandle>],
    default_bg: Color32,
    default_accent: Color32,
    panel_right: bool,
    shell_open: &mut Option<usize>,
    color_open: &mut Option<(usize, bool)>,
    card_drag: &mut Option<TabDrag>,
    pointer: Option<Pos2>,
    show_duplicate: bool,
    mut toggle: Option<StartupToggle<'_>>,
) -> (CardAction, egui::Rect) {
    let mut action = CardAction::None;
    let dragging_this = card_drag.map_or(false, |d| d.from == i);
    // カードの色＝実体化したタブの見た目（bg＝塗り、accent＝枠）。参照モードは解決した型の色を、
    // マニュアルは spec の色を使う（未指定は既定にフォールバック）。
    let disp: Option<&TabSpec> = match &toggle {
        Some(t) if *t.use_preset => t.presets.iter().find(|p| p.name == *t.preset_ref),
        _ => None,
    };
    let (bg_s, ac_s) = match disp {
        Some(p) => (p.bg.as_str(), p.accent.as_str()),
        None => (spec.bg.as_str(), spec.accent.as_str()),
    };
    let res_bg = color_from_hex(bg_s).unwrap_or(default_bg);
    let res_accent = color_from_hex(ac_s).unwrap_or(default_accent);
    let bg_disp = BG_APPLY
        .iter()
        .position(|c| *c == res_bg)
        .map(|k| BG_DISPLAY[k])
        .unwrap_or(res_bg);
    // 左端（or 右端）にドラッグハンドルぶんの余白（30px）を確保。
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
            let mut preset_mode = toggle.as_ref().map_or(false, |t| *t.use_preset);
            ui.horizontal(|ui| {
                // モードトグル（マニュアル / プリセット）。切替時に spec / 参照を整える。
                if let Some(t) = toggle.as_mut() {
                    // \u{e3c9}=edit（マニュアル＝その場編集）／\u{e866}=bookmark（プリセット＝保存した型）
                    if mode_toggle_btn(ui, "\u{e3c9}", "マニュアル", !*t.use_preset) && *t.use_preset
                    {
                        // プリセット→マニュアル：参照していた型の内容を spec にコピー。
                        if let Some(p) = t.presets.iter().find(|p| p.name == *t.preset_ref) {
                            let mut c = p.clone();
                            c.sync_buf_from_commands();
                            *spec = c;
                        }
                        *t.use_preset = false;
                        *t.preset_ref = String::new();
                    }
                    ui.add_space(2.0);
                    if mode_toggle_btn(ui, "\u{e866}", "プリセット", *t.use_preset) && !*t.use_preset
                    {
                        // マニュアル→プリセット：未選択にする（自動で先頭を選ばない）。
                        *t.use_preset = true;
                        *t.preset_ref = String::new();
                    }
                    ui.add_space(8.0);
                }
                preset_mode = toggle.as_ref().map_or(false, |t| *t.use_preset);
                if preset_mode {
                    // ---- 参照モード：プリセット選択＋プレビュー（読み取り専用） ----
                    let t = toggle.as_mut().unwrap();
                    let cur = t.preset_ref.clone();
                    let sel = t.presets.iter().find(|p| p.name == cur);
                    let label = if cur.is_empty() {
                        "（未選択）".to_string()
                    } else if sel.is_some() {
                        cur.clone()
                    } else {
                        format!("⚠ {cur}")
                    };
                    let open = *t.pick_open == Some(i);
                    let arrow = if open { "▲" } else { "▼" };
                    let picker =
                        ui.add_sized([220.0, 28.0], egui::Button::new(format!("{label}　{arrow}")));
                    let brect = picker.rect;
                    if picker.clicked() {
                        *t.pick_open = if open { None } else { Some(i) };
                    }
                    // 解決先の色はカード枠（bg 塗り＋アクセントバー）が示すので、ここでは
                    // プレビューを重ねない（冗長・横幅圧迫を避ける）。
                    // 選択ドロップダウン（未選択＋各プリセット名）。
                    if *t.pick_open == Some(i) {
                        let dd_frame = egui::Frame::new()
                            .fill(Color32::from_gray(40))
                            .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::same(4));
                        let mut picked: Option<String> = None;
                        let area = egui::Area::new(egui::Id::new((id_scope, "preset_pick", i)))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(brect.left_bottom() + Vec2::new(0.0, 2.0))
                            .show(ctx, |ui| {
                                dd_frame.show(ui, |ui| {
                                    ui.set_min_width(200.0);
                                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                                        if ui.selectable_label(cur.is_empty(), "（未選択）").clicked()
                                        {
                                            picked = Some(String::new());
                                        }
                                        for p in t.presets {
                                            if p.name.trim().is_empty() {
                                                continue;
                                            }
                                            if ui
                                                .selectable_label(p.name == cur, p.name.as_str())
                                                .clicked()
                                            {
                                                picked = Some(p.name.clone());
                                            }
                                        }
                                    });
                                });
                            });
                        let arect = area.response.rect;
                        if let Some(pn) = picked {
                            *t.preset_ref = pn;
                            *t.pick_open = None;
                        } else if ctx.input(|i| i.pointer.any_pressed()) {
                            if let Some(pp) = ctx.input(|i| i.pointer.interact_pos()) {
                                if !brect.contains(pp) && !arect.contains(pp) {
                                    *t.pick_open = None;
                                }
                            }
                        }
                    }
                    // 操作ボタン（削除・並べ替え）。参照モードに複製は無し。
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if settings_icon_btn(ui, "\u{e872}", true) {
                            action = CardAction::Remove;
                        }
                        ui.add_space(2.0);
                        if settings_icon_btn(ui, "\u{e5db}", false) {
                            action = CardAction::Move(1);
                        }
                        ui.add_space(2.0);
                        if settings_icon_btn(ui, "\u{e5d8}", false) {
                            action = CardAction::Move(-1);
                        }
                    });
                } else {
                    ui.add_sized(
                        [150.0, 28.0],
                        egui::TextEdit::singleline(&mut spec.name)
                            .hint_text("名前")
                            .vertical_align(egui::Align::Center),
                    );
                    ui.add_space(6.0);
                    // シェル選択(既定タブと同じ前面ドロップダウン)
                    let sh = spec.shell.clone();
                    let sh_idx = if sh.is_empty() {
                        None
                    } else {
                        shells.iter().position(|s| s.short == sh)
                    };
                    let sh_label = match sh_idx {
                        Some(k) => shells[k].short.clone(),
                        None => "既定".to_string(),
                    };
                    let open = *shell_open == Some(i);
                    let arrow = if open { "▲" } else { "▼" };
                    let htext = format!("{sh_label}　{arrow}");
                    let header = match sh_idx
                        .and_then(|k| shell_icons.get(k))
                        .and_then(|o| o.as_ref())
                    {
                        Some(tex) => {
                            let img = egui::Image::new(tex).fit_to_exact_size(Vec2::splat(16.0));
                            ui.add_sized([170.0, 28.0], egui::Button::image_and_text(img, htext))
                        }
                        None => ui.add_sized([170.0, 28.0], egui::Button::new(htext)),
                    };
                    let hrect = header.rect;
                    if header.clicked() {
                        *shell_open = if open { None } else { Some(i) };
                    }
                    // 色スウォッチ(bg＝表示色／accent)。クリックでパレット。
                    ui.add_space(6.0);
                    let bg_resp = color_swatch(ui, bg_disp);
                    if bg_resp.clicked() {
                        *color_open = if *color_open == Some((i, false)) {
                            None
                        } else {
                            Some((i, false))
                        };
                    }
                    ui.add_space(4.0);
                    let ac_resp = color_swatch(ui, res_accent);
                    if ac_resp.clicked() {
                        *color_open = if *color_open == Some((i, true)) {
                            None
                        } else {
                            Some((i, true))
                        };
                    }
                    // 色パレットのポップアップ(bg / accent)
                    let pop_frame = egui::Frame::new()
                        .fill(Color32::from_gray(40))
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(6));
                    if *color_open == Some((i, false)) {
                        let mut pick: Option<Option<Color32>> = None;
                        let area = egui::Area::new(egui::Id::new((id_scope, "bg_pop", i)))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(bg_resp.rect.left_bottom() + Vec2::new(0.0, 2.0))
                            .show(ctx, |ui| {
                                pop_frame.show(ui, |ui| {
                                    ui.set_width(176.0);
                                    if ui.selectable_label(spec.bg.is_empty(), "既定").clicked() {
                                        pick = Some(None);
                                    }
                                    if let Some(c) =
                                        palette_picker(ui, &BG_DISPLAY, &BG_APPLY, res_bg)
                                    {
                                        pick = Some(Some(c));
                                    }
                                });
                            });
                        let arect = area.response.rect;
                        if let Some(sel) = pick {
                            spec.bg = match sel {
                                Some(c) => color_to_hex(c),
                                None => String::new(),
                            };
                            *color_open = None;
                        } else if ctx.input(|i| i.pointer.any_pressed()) {
                            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                                if !bg_resp.rect.contains(p) && !arect.contains(p) {
                                    *color_open = None;
                                }
                            }
                        }
                    }
                    if *color_open == Some((i, true)) {
                        let mut pick: Option<Option<Color32>> = None;
                        let area = egui::Area::new(egui::Id::new((id_scope, "ac_pop", i)))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(ac_resp.rect.left_bottom() + Vec2::new(0.0, 2.0))
                            .show(ctx, |ui| {
                                pop_frame.show(ui, |ui| {
                                    ui.set_width(176.0);
                                    if ui.selectable_label(spec.accent.is_empty(), "既定").clicked()
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
                            spec.accent = match sel {
                                Some(c) => color_to_hex(c),
                                None => String::new(),
                            };
                            *color_open = None;
                        } else if ctx.input(|i| i.pointer.any_pressed()) {
                            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                                if !ac_resp.rect.contains(p) && !arect.contains(p) {
                                    *color_open = None;
                                }
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // ゴミ箱＝削除、(プリセットのみ)複製、太い上下矢印＝並べ替え
                        if settings_icon_btn(ui, "\u{e872}", true) {
                            action = CardAction::Remove;
                        }
                        ui.add_space(2.0);
                        if show_duplicate {
                            if settings_icon_btn(ui, "\u{e14d}", false) {
                                action = CardAction::Duplicate;
                            }
                            ui.add_space(2.0);
                        }
                        if settings_icon_btn(ui, "\u{e5db}", false) {
                            action = CardAction::Move(1);
                        }
                        ui.add_space(2.0);
                        if settings_icon_btn(ui, "\u{e5d8}", false) {
                            action = CardAction::Move(-1);
                        }
                    });

                    // 前面ドロップダウン(このカードが開いている時)
                    if *shell_open == Some(i) {
                        let dd_frame = egui::Frame::new()
                            .fill(Color32::from_gray(40))
                            .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::same(4));
                        let mut picked: Option<String> = None;
                        let area = egui::Area::new(egui::Id::new((id_scope, "shell_dd", i)))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(hrect.left_bottom() + Vec2::new(0.0, 2.0))
                            .show(ctx, |ui| {
                                dd_frame.show(ui, |ui| {
                                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                        if ui.selectable_label(sh.is_empty(), "既定").clicked() {
                                            picked = Some(String::new());
                                        }
                                        if let Some(k) = shell_list(ui, shells, shell_icons, sh_idx)
                                        {
                                            picked = Some(shells[k].short.clone());
                                        }
                                    });
                                });
                            });
                        let arect = area.response.rect;
                        if let Some(p) = picked {
                            spec.shell = p;
                            *shell_open = None;
                        } else if ctx.input(|i| i.pointer.any_pressed()) {
                            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                                if !hrect.contains(pos) && !arect.contains(pos) {
                                    *shell_open = None;
                                }
                            }
                        }
                    }
                }
            });
            // マニュアルのみ cwd／コマンドを出す（参照モードは型が持つ）。
            if !preset_mode {
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut spec.cwd)
                        .hint_text("初期ディレクトリ（空＝既定）")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut spec.commands_buf)
                        .hint_text("起動コマンド（1行に1つ）")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
            }
        });
    // カード矩形を記録し、端の余白にドラッグハンドル（点）＋アクセントバーを描く。
    let card_rect = card_resp.response.rect;
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
        egui::Id::new((id_scope, "card_grip", i)),
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
            *card_drag = Some(TabDrag { from: i, grab_dy: p.y - card_rect.top() });
        }
    }
    ui.add_space(6.0);
    (action, card_rect)
}

// カードリストの D&D を確定する。挿入線とゴーストを描き、ドロップされたら移動 (from, target) を返す。
// ghost＝掴んだカードの内容と色（呼び元がクローンして渡す）。tab_spec_card と対で使う。
#[allow(clippy::too_many_arguments)]
pub(crate) fn card_dnd_resolve(
    ui: &mut egui::Ui,
    card_drag: &mut Option<TabDrag>,
    card_rects: &[egui::Rect],
    pointer: Option<Pos2>,
    panel_right: bool,
    ghost: Option<(TabSpec, Color32, Color32)>,
    shells: &[ShellSpec],
    shell_icons: &[Option<egui::TextureHandle>],
) -> Option<(usize, usize)> {
    let mut do_move = None;
    if let Some(drag) = *card_drag {
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
            let target = card_rects.iter().filter(|r| p.y > r.center().y).count();
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
            let grect = egui::Rect::from_min_size(Pos2::new(x0, top), Vec2::new(x1 - x0, card_h));
            let gp = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("card_drag_ghost"),
            ));
            if let Some((tab, gbg, gac)) = &ghost {
                draw_card_ghost(
                    &gp, grect, tab, shells, shell_icons, panel_right, *gbg, *gac,
                );
            }
            if released {
                do_move = Some((drag.from, target));
            }
        }
        if released {
            *card_drag = None;
        }
        ui.ctx().request_repaint();
    }
    do_move
}

// ドラッグ中のゴースト＝掴んだカードの実物を半透明で再現（名前/シェル/cwd/コマンド）。
pub(crate) fn draw_card_ghost(
    painter: &egui::Painter,
    rect: egui::Rect,
    tab: &TabSpec,
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
pub(crate) fn move_card<T>(tabs: &mut Vec<T>, from: usize, target: usize) {
    if from >= tabs.len() {
        return;
    }
    let item = tabs.remove(from);
    let insert = if target > from { target - 1 } else { target }.min(tabs.len());
    tabs.insert(insert, item);
}

// 28x28 の正方アイコンボタン（Material Icons グリフ）。行内の他コントロールと高さを
// 揃えるため固定サイズ。danger=true はホバーで赤みを帯びる（削除用）。
pub(crate) fn settings_icon_btn(ui: &mut egui::Ui, glyph: &str, danger: bool) -> bool {
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
pub(crate) fn settings_nav_item(
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
pub(crate) fn shell_list(
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
