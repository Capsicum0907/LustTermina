//! 単一インスタンス & プロセス間通信（Windows 名前付きパイプ）。
use eframe::egui;
use std::sync::mpsc;
use std::thread;
use crate::config::TabSpec;
use crate::session::UNRESOLVED_PRESET_MARK;

// ---- 単一インスタンス & プロセス間通信（Windows 名前付きパイプ）----
// Explorer の「このディレクトリで開く」等で2つ目以降のプロセスが起動されたら、既に動いて
// いる本体へパスを送って自分は終了する。本体は受け口の名前付きパイプを1本だけ張り、届いた
// パスを UI スレッドへ渡して新規タブを開く。パイプは nMaxInstances=1 で作るので、2つ目の
// 生成は失敗する＝「本体が居るか」の判定を兼ねる。依存は増やさず、既に使っている windows
// クレート（＋Win32_System_Pipes 機能）だけで実装する。

// メッセージ書式は <verb>\t<payload>。cwd＝フォルダで開く、run＝スクリプトを実行。
pub(crate) const IPC_VERB_CWD: &str = "cwd";
pub(crate) const IPC_VERB_RUN: &str = "run";
pub(crate) const IPC_VERB_TAB: &str = "tab";

// 既存インスタンスへ転送する要求。UI スレッドで対応するタブを開く。
pub(crate) enum IpcMsg {
    OpenDir(String),  // フォルダを既定シェルのタブで開く
    RunScript(String), // スクリプトを実行するタブを開く
    OpenTab(TabRequest), // --tab：仕様どおりのタブを1枚開く
}

// --tab のタブ生成要求。パイプでは TOML に畳んで運ぶ（新規依存なし・タブ/改行入り文字列も安全）。
// depth は「何代目のタブか」の系譜スタンプ。生成されるタブの環境変数
// LUSTTERMINA_TAB_DEPTH に入り、タブ内から自分の深度が分かる（連鎖の可視化）。
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct TabRequest {
    #[serde(default)]
    pub(crate) tab: TabSpec,
    #[serde(default)]
    pub(crate) depth: u32,
}

pub(crate) const TAB_DEPTH_ENV: &str = "LUSTTERMINA_TAB_DEPTH";

// 呼び出し元プロセス自身の深度（スタンプ無し＝根本＝0）。
pub(crate) fn current_tab_depth() -> u32 {
    std::env::var(TAB_DEPTH_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

// パイプ名。別ユーザーと衝突しないようユーザー名で分ける（per-user）。
pub(crate) fn ipc_pipe_name() -> Vec<u16> {
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(r"\\.\pipe\LustTermina.{user}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

// 既存インスタンスへ <verb>\t<path> を転送。本体が受け取れたら true、居なければ false。
pub(crate) fn ipc_try_forward(verb: &str, path: &str) -> bool {
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
pub(crate) fn ipc_start_server(ctx: &egui::Context) -> Option<mpsc::Receiver<IpcMsg>> {
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
                } else if let Some(p) = s.strip_prefix(&format!("{IPC_VERB_TAB}\t")) {
                    // 動詞との区切りは最初のタブ1個だけ。本文（TOML）内のタブ・改行は
                    // TOML のエスケープが復元するので衝突しない。壊れた要求も黙って
                    // 捨てず、マーカー付きタブとして可視化する（silent fail 回避）。
                    Some(match toml::from_str::<TabRequest>(p) {
                        Ok(req) => IpcMsg::OpenTab(req),
                        Err(_) => IpcMsg::OpenTab(TabRequest {
                            tab: TabSpec {
                                name: format!("{UNRESOLVED_PRESET_MARK} 不正なタブ要求"),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    })
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

