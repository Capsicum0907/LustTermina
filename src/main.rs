// LustTermina（ラストテルミナ）— MVP
//
// データフロー: pwsh ⇄ portable-pty(ConPTY) ⇄ alacritty_terminal(グリッド) → egui 描画
// 入力:        egui のキー/文字 → エスケープ列に encode → pty writer → pwsh
//
// この MVP の範囲: 「pwsh の出力が色付きで出て、キー入力が通る」まで。
// 後回し（長い尻尾）: per-cell 背景色, スクロールバック UI, 選択コピー, CJK 全角幅,
//                     マウスレポート, 差分描画, 縦タブ, 設定/テーマ toml。

#![cfg_attr(windows, windows_subsystem = "windows")]

use eframe::egui;

// 責務ごとに分割したモジュール（詳細は各ファイル冒頭の doc）。
mod app;
mod color;
mod config;
mod icons;
mod input;
mod ipc;
mod session;
mod shells;
mod tray;
mod ui;
mod winintegration;

use crate::app::*;
use crate::config::*;
use crate::ipc::*;
use crate::winintegration::*;

pub(crate) const FONT_SIZE: f32 = 14.0;
pub(crate) const PAD: f32 = 6.0;

// 起動時の意図（コマンドライン引数から決まる）。Explorer 連携の入口はここに集約し、
// 将来 --run <script> やインストール系（--install など）もこの enum に足していく。
pub(crate) enum Launch {
    Normal,                    // 通常起動：起動タブ定義に従う（未インストールなら導入を促す）
    Cwd(String),               // --cwd <path>：そのフォルダでタブ1枚（起動タブ定義は無視）
    RunScript(String),         // --run <path>：スクリプトをそのフォルダで実行するタブ1枚
    // --tab [--cwd <dir>] [--shell <short>] [--name <名>] [--cmd <行>]... [--new-window]
    // 仕様どおりのタブを既存インスタンスへ合流（本体不在 or --new-window なら自分が窓になる）。
    Tab { spec: TabSpec, new_window: bool },
    Install,                   // --install：インストール（先を選ばせて）実行し終了
    Uninstall,                 // --uninstall：アンインストールして終了
    DoInstallMachine,          // --do-install-machine：PC全体へ配置（昇格済みの代行ワーカ）
    DoUninstallMachine(bool),  // --do-uninstall-machine <0|1>：PC全体を削除（昇格済みの代行ワーカ）
    InstallContextMenu,        // --install-context-menu：右クリックのみ登録して終了
    UninstallContextMenu,      // --uninstall-context-menu：右クリックのみ解除して終了
    Help,                      // --help / -h / /?：使い方を表示して終了
}

// --help の本文。GUI サブシステム（コンソール無し）なので stdout ではなくダイアログで出す。
const HELP_TEXT: &str = "\
LustTermina — コマンドライン引数

lust_termina.exe [オプション]

タブを開く:
  --tab                 タブを1枚開く（起動済みの窓があればそこへ合流）
    --cwd <dir>           作業フォルダ
    --shell <short>       シェル（pwsh / powershell / cmd など。省略時は既定）
    --name <名前>         タブ名
    --cmd <コマンド行>    起動後に投入するコマンド（複数回指定可・順に実行）
    --new-window          合流せず新しい窓で開く
  --cwd <dir>           そのフォルダで開く（Explorer 連携と同じ）
  --run <script>        スクリプトをそのフォルダで実行

例:
  ltm --tab --cwd \"C:\\work\\proj\" --name proj --cmd \"claude\"

インストール先は PATH に登録され、短い別名 ltm（ltm.cmd）でも呼べます。

セットアップ:
  --install / --uninstall
  --install-context-menu / --uninstall-context-menu

補足: --tab で開いたタブには環境変数 LUSTTERMINA_TAB_DEPTH に
「何代目か」が入ります（直接開いたタブ=無し、子=1, 孫=2, …）。";

// 親プロセスのコンソールへ HELP_TEXT を書く。成功したら true、親にコンソールが
// 無ければ false（呼び元がダイアログにフォールバックする）。GUI サブシステムなので
// println! はどこにも届かない：AttachConsole(親) → CONOUT$ に UTF-16 で直書きする。
// ヘルプを見ながらそのまま次のコマンドを打てるように、シェル内に出すのが本命。
fn print_help_to_parent_console() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{AttachConsole, WriteConsoleW, ATTACH_PARENT_PROCESS};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return false; // 親にコンソールが無い（ダブルクリック起動等）
        }
        let name = wide_nul("CONOUT$");
        let Ok(handle) = CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) else {
            return false;
        };
        let text: Vec<u16> = format!("\r\n{HELP_TEXT}\r\n").encode_utf16().collect();
        let mut written = 0u32;
        let ok = WriteConsoleW(handle, &text, Some(&mut written), None).is_ok();
        let _ = CloseHandle(handle);
        ok
    }
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
            "--tab" => {
                // 以降の引数を全部タブ仕様として消費する。--cmd の値は「直後の1引数を
                // 丸ごと1行のコマンド」として受け取り、中身は一切解釈しない（引用の
                // 消費は各境界で1回ずつ＝入れ子でも破綻しない）。--cmd は複数回可。
                let mut spec = TabSpec::default();
                let mut new_window = false;
                while let Some(f) = args.next() {
                    match f.as_str() {
                        "--cwd" => {
                            if let Some(v) = args.next() {
                                spec.cwd = v;
                            }
                        }
                        "--shell" => {
                            if let Some(v) = args.next() {
                                spec.shell = v;
                            }
                        }
                        "--name" => {
                            if let Some(v) = args.next() {
                                spec.name = v;
                            }
                        }
                        "--cmd" => {
                            if let Some(v) = args.next() {
                                spec.commands.push(v);
                            }
                        }
                        "--new-window" => new_window = true,
                        _ => {}
                    }
                }
                return Launch::Tab { spec, new_window };
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
            "--help" | "-h" | "/?" => return Launch::Help,
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
        Launch::Help => {
            // シェルから呼ばれたらそのコンソール内に出す（ヘルプを見ながら打てるように）。
            // コンソールが無い起動（ダブルクリック等）だけダイアログにフォールバック。
            if !print_help_to_parent_console() {
                msg_box(HELP_TEXT, "LustTermina", MB_ICONINFORMATION);
            }
            return Ok(());
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
        // --tab：本体が居れば TOML に畳んで合流。--new-window は転送せず自分が窓になる。
        // depth は呼び出し元の深度+1（環境変数から。根本＝0 → 子＝1）。
        Launch::Tab { spec, new_window } => {
            if !*new_window {
                let req = TabRequest {
                    tab: spec.clone(),
                    depth: current_tab_depth() + 1,
                };
                if let Ok(payload) = toml::to_string(&req) {
                    if ipc_try_forward(IPC_VERB_TAB, &payload) {
                        return Ok(());
                    }
                }
            }
            // 本体不在 or --new-window：下へ流して自分が窓になる（App::new が Tab を開く）。
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
