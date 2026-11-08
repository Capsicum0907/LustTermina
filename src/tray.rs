//! 通知領域（タスクトレイ）のアイコンとメニュー。
//!
//! ウィンドウを隠して裏でシェルを動かし続けるとき、トレイアイコンが「まだ生きている」ことを
//! 示す唯一の目印であり、窓を呼び戻す唯一の入口でもある。だから隠す前に必ずアイコンの登録に
//! 成功していること（`tray_ready()`）を確かめる＝見えないまま戻せないプロセスを作らない。
//!
//! トレイのクリックは Win32 のウィンドウプロシージャ（プレーンな関数）へ届くため、egui 側とは
//! 静的な受け渡し口でつなぐ：wndproc は「何が押されたか」だけを積んで再描画を要求し、実際の
//! 表示／非表示は次のフレームの `ui()` が eframe のビューポート命令として行う。ウィンドウの
//! 可視状態を eframe と Win32 の二重管理にしないため、直接 ShowWindow は呼ばない。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ExtractIconW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos,
    PostMessageW, RegisterClassW, SetForegroundWindow, TrackPopupMenu, MF_STRING, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW,
};

use crate::winintegration::{current_exe_string, wide_nul};

// ---- 表に出る文字列・識別子（ベタ書きを散らさないためここに集約）----
const TRAY_CLASS: &str = "LustTerminaTrayWindow";
const TRAY_TIP: &str = "LustTermina";
const MENU_SHOW: &str = "ウィンドウを表示";
const MENU_HIDE: &str = "ウィンドウを隠す（裏で動かす）";
const MENU_EXIT: &str = "LustTermina を終了";

// トレイからのコールバックを受け取る独自メッセージ。WM_APP 以降はアプリが自由に使える帯域。
const TRAY_CALLBACK: u32 = WM_APP + 1;
// 1プロセスに1つしか出さないのでアイコンの識別子は固定でよい。
const TRAY_UID: u32 = 1;

// メニュー項目の識別子。0 は TrackPopupMenu の「選ばれなかった」と衝突するので 1 から。
const CMD_SHOW: usize = 1;
const CMD_HIDE: usize = 2;
const CMD_EXIT: usize = 3;

// ---- wndproc と egui をつなぐ受け渡し口 ----
// wndproc は静的な関数なので App を直接触れない。押された内容だけを積んでおき、
// 次のフレームで `take_tray_request()` が回収する。取りこぼしを避けるため上書きはしない
// （未処理の要求が残っている間は新しい要求で潰さない）。
const REQ_NONE: u8 = 0;
const REQ_SHOW: u8 = 1;
const REQ_HIDE: u8 = 2;
const REQ_EXIT: u8 = 3;
static TRAY_REQ: AtomicU8 = AtomicU8::new(REQ_NONE);

// 要求を積んだ直後に再描画を促すための egui コンテキスト。これが無いと、隠している間の
// フレーム間隔（アイドル時は数秒に1回）ぶんクリックの反応が遅れる。
static TRAY_CTX: Mutex<Option<eframe::egui::Context>> = Mutex::new(None);

// 本体ウィンドウのハンドル。最小化からの復元はウィンドウプロシージャの中で直接行う必要が
// あるため（最小化中は eframe のフレームが止まり、egui 経由の指示が処理されない）、
// アプリ起動後にここへ預けてもらう。0＝未設定。
static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

// トレイアイコンを出せているか。隠す操作の可否はこれで決める。
static TRAY_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// アイコンの宛先ウィンドウ（削除に必要）。0＝未作成。
static TRAY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// トレイのクリック／メニューで要求された操作。`ui()` が毎フレーム回収して実行する。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TrayRequest {
    None,
    Show,
    Hide,
    Exit,
}

/// 溜まっている要求を1つ取り出す（取り出したら空に戻る）。
pub(crate) fn take_tray_request() -> TrayRequest {
    match TRAY_REQ.swap(REQ_NONE, Ordering::AcqRel) {
        REQ_SHOW => TrayRequest::Show,
        REQ_HIDE => TrayRequest::Hide,
        REQ_EXIT => TrayRequest::Exit,
        _ => TrayRequest::None,
    }
}

/// トレイアイコンを出せているか（＝隠しても戻す道があるか）。
pub(crate) fn tray_ready() -> bool {
    TRAY_READY.load(Ordering::Acquire)
}

/// 本体ウィンドウのハンドルを預ける（起動後に一度）。最小化からの復元に使う。
pub(crate) fn set_main_window(hwnd: isize) {
    MAIN_HWND.store(hwnd, Ordering::Release);
}

// 「表示」を押されたときの即応部分。最小化の復元と前面化はここで済ませる：
// 最小化中は eframe のフレームが止まるため、egui 側へ要求を積むだけでは誰も処理しない。
// 隠している（Visible(false)）場合の再表示は eframe が可視状態を持っているので、
// そちらは要求として積み、次のフレームで `ui()` に処理させる（フレームは回っている）。
pub(crate) fn main_window() -> isize {
    MAIN_HWND.load(Ordering::Acquire)
}

fn restore_main_window() {
    crate::winintegration::bring_hwnd_to_front(MAIN_HWND.load(Ordering::Acquire));
}

fn push_request(req: u8) {
    // 未処理の要求があるときは上書きしない（先に押されたものを優先）。
    let _ = TRAY_REQ.compare_exchange(REQ_NONE, req, Ordering::AcqRel, Ordering::Acquire);
    if let Ok(guard) = TRAY_CTX.lock() {
        if let Some(ctx) = guard.as_ref() {
            ctx.request_repaint(); // 隠していてもすぐ次のフレームが回るように
        }
    }
}

// トレイの右クリックメニュー。TPM_RETURNCMD で選択結果を戻り値として受け取る
//（WM_COMMAND を待たずに済む）。表示前の SetForegroundWindow と表示後の WM_NULL は、
// メニュー外をクリックしても閉じないという Win32 の既知の癖への定石の対処。
fn show_menu(hwnd: HWND) {
    let show = wide_nul(MENU_SHOW);
    let hide = wide_nul(MENU_HIDE);
    let exit = wide_nul(MENU_EXIT);
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, PCWSTR(show.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, CMD_HIDE, PCWSTR(hide.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, PCWSTR(exit.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        chosen
    };

    match chosen.0 as usize {
        CMD_SHOW => {
            restore_main_window();
            push_request(REQ_SHOW);
        }
        CMD_HIDE => push_request(REQ_HIDE),
        CMD_EXIT => {
            restore_main_window(); // 終了確認は見えるところで出す
            push_request(REQ_EXIT);
        }
        _ => {} // メニュー外クリック等＝何も選ばれなかった
    }
}

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TRAY_CALLBACK {
        // lParam の下位ワードに、アイコン上で起きたマウスイベントが入る。
        match (lparam.0 as u32) & 0xFFFF {
            WM_LBUTTONUP => {
                restore_main_window();
                push_request(REQ_SHOW);
            }
            WM_RBUTTONUP => show_menu(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// アイコンの実体は exe に埋め込んだリソース（build.rs が assets/icon.ico を埋める）から取る。
// ファイルを別途配らないため、単一ファイル配布のままトレイにも同じ絵が出る。
fn app_icon() -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    let exe = wide_nul(&current_exe_string());
    let icon = unsafe { ExtractIconW(None, PCWSTR(exe.as_ptr()), 0) };
    // 失敗時は NULL、「アイコンが無い」ときは 1 が返る仕様。どちらも使えない。
    if icon.is_invalid() || icon.0 as usize == 1 {
        None
    } else {
        Some(icon)
    }
}

/// トレイアイコンを登録する。成功したら `tray_ready()` が true になる。
/// 受け皿として非表示のウィンドウを1枚作る（トレイのコールバックには宛先の HWND が要る）。
/// メッセージ専用ウィンドウ（HWND_MESSAGE）にしないのは、右クリックメニューの表示に
/// SetForegroundWindow が必要で、それが効く普通のウィンドウである必要があるため。
pub(crate) fn install_tray(ctx: &eframe::egui::Context) -> bool {
    if let Ok(mut guard) = TRAY_CTX.lock() {
        *guard = Some(ctx.clone());
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return false };
        let hinst: windows::Win32::Foundation::HINSTANCE = module.into();
        let class = wide_nul(TRAY_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_wndproc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return false;
        }
        let name = wide_nul(TRAY_TIP);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(name.as_ptr()),
            WINDOW_STYLE(0), // WS_VISIBLE を立てない＝画面には出ない受け皿
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return false;
        };
        let Some(icon) = app_icon() else { return false };

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: icon,
            ..Default::default()
        };
        for (dst, src) in nid.szTip.iter_mut().zip(wide_nul(TRAY_TIP)) {
            *dst = src;
        }
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            return false;
        }
        TRAY_HWND.store(hwnd.0 as isize, Ordering::Release);
        TRAY_READY.store(true, Ordering::Release);
        true
    }
}

/// トレイアイコンを消す（終了時）。放っておくとプロセス終了後もアイコンの残骸が残る。
pub(crate) fn remove_tray() {
    let raw = TRAY_HWND.swap(0, Ordering::AcqRel);
    if raw == 0 {
        return;
    }
    TRAY_READY.store(false, Ordering::Release);
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(raw as *mut core::ffi::c_void),
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}
