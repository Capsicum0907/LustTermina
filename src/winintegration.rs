//! Windows 統合：Explorer 右クリック・レジストリ・インストール/アンインストール・PATH・昇格。
use eframe::egui::{self, Color32};
use crate::session::Session;
use crate::shells::ShellSpec;

// ---- Explorer 右クリックメニュー登録（レジストリ, HKCU のみ・管理者不要）----
// 「フォルダを右クリック」= Directory\shell、「フォルダ内の空白を右クリック」= Directory\Background\shell。
// 書くのは HKCU\Software\Classes 配下の自分のキーだけ（親の shell は共有なので触らない）。
// アンインストールは自分のキーをツリーごと消す＝残骸ゼロ。Win11 の従来メニュー側（「その他の
// オプションを確認」）に出る。ラベル・キー名は下の const 一箇所に集約。
pub(crate) const CTX_MENU_LABEL: &str = "LustTermina で開く";
pub(crate) const CTX_KEY_DIR: &str = r"Software\Classes\Directory\shell\LustTermina";
pub(crate) const CTX_KEY_BG: &str = r"Software\Classes\Directory\Background\shell\LustTermina";

// このプロセスの exe の絶対パス（レジストリのコマンド文字列に埋める）。
pub(crate) fn current_exe_string() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_default()
}

// UTF-16 のヌル終端ワイド列。
pub(crate) fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// <root>\<subkey> を作り、(name, value) を REG_SZ で書く。name="" は既定値。
// root は HKCU（このユーザー）か HKLM（PC全体）を渡す。
pub(crate) fn reg_set_sz(
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
pub(crate) fn reg_get_sz(
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
pub(crate) fn reg_delete_tree(root: windows::Win32::System::Registry::HKEY, subkey: &str) -> bool {
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
pub(crate) const SCRIPT_RUN_LABEL: &str = "LustTermina で実行";
pub(crate) const SCRIPT_EXTS: [&str; 3] = [".ps1", ".bat", ".cmd"];

// 「プログラムから開く」(Open with) 一覧に LustTermina を候補として出すための登録先。
// 既定ハンドラは奪わない：一覧に出るだけで、ユーザーが「常に使う」を選んで初めて既定になる。
pub(crate) const APP_REG_KEY: &str = r"Software\Classes\Applications\lust_termina.exe";

// SystemFileAssociations\<ext>\shell\LustTerminaRun：既定ハンドラを奪わず、右クリックに項目を
// 足すだけの非侵襲な場所。アンインストールは自分のキーをツリーごと消す＝残骸ゼロ。
pub(crate) fn script_ctx_key(ext: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{ext}\shell\LustTerminaRun")
}

// スクリプトのあるフォルダ（cwd 用）。取れなければ空＝親プロセス継承。
pub(crate) fn script_parent(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string()
}

// 拡張子ごとの「スクリプト実行シェル」の組込み既定（short 名）。config 未設定時に使う。
// 対応する検出シェルが無ければ script_run_shell 側で常設シェルへフォールバックする。
pub(crate) fn default_runner_short(ext: &str) -> &'static str {
    match ext {
        "ps1" => "pwsh", // 無ければ powershell（常設）へフォールバック（下記）
        "sh" => "bash",  // 将来 .sh 対応時（Git Bash）
        _ => "cmd",      // bat/cmd 等
    }
}

// 必ず在るランナー（Windows 標準で常設）。pwsh(PowerShell 7) 等の優先ランナーが未導入の
// ときの受け皿。これが無いと解決に失敗し「スクリプトが黙って実行されない」不具合になる
// （pwsh 未導入マシンで発生。旧: フォールバックも pwsh を探して None を返していた）。
pub(crate) fn guaranteed_runner_short(ext: &str) -> &'static str {
    match ext {
        "ps1" => "powershell", // Windows PowerShell 5.1 は常設
        _ => "cmd",            // bat/cmd 等は cmd（常設）
    }
}

// 選んだシェル種別（short）でスクリプト path を実行する引数。種別ごとの流儀を一箇所に集約。
// 実行が終わったらシェルも終了する（-NoExit/`/k` を付けない）＝ダブルクリック実行を完結させる。
// タブ側は close_on_exit で正常終了なら自動クローズ／失敗なら出力を残す（app 側で判定）。
// 対応しない種別はパスをそのまま渡すだけ＝「対応外シェルを選んだら動かなくてもユーザーの
// 選択」の割り切り（握り潰さず起動は試みる）。
pub(crate) fn script_run_args(short: &str, path: &str) -> Vec<String> {
    match short {
        "pwsh" | "powershell" | "devps" => vec![
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            path.to_string(),
        ],
        "cmd" | "devcmd" => vec!["/c".into(), path.to_string()],
        "bash" | "wsl" => vec!["-c".into(), format!("'{path}'")],
        _ => vec![path.to_string()],
    }
}

// パスの拡張子が「LustTermina で実行」対応（SCRIPT_EXTS）か。D&D 時の振り分けに使う。
pub(crate) fn is_script_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            SCRIPT_EXTS.iter().any(|x| x.trim_start_matches('.') == e)
        })
        .unwrap_or(false)
}

// 拡張子から「そのスクリプトを実行するシェル定義」を作る。対応外拡張子は None。
// 実行シェルは config（拡張子→short）で選べる（未設定は組込み既定）。選んだ／既定の short が
// 検出シェルに無ければ更にフォールバック（powershell/cmd は常設）。引数は種別ごとの流儀で作る。
pub(crate) fn script_run_shell(
    path: &str,
    runner: &std::collections::BTreeMap<String, String>,
    shells: &[ShellSpec],
) -> Option<ShellSpec> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    // 対応拡張子のみ（SCRIPT_EXTS を唯一の正に）。
    if !SCRIPT_EXTS.iter().any(|e| e.trim_start_matches('.') == ext) {
        return None;
    }
    // 選択（config）→ 組込み既定（pwsh 等）→ 常設フォールバック（powershell/cmd）の3段で
    // 引く。pwsh は未導入のことがあるので、必ず在るシェルまで落として「黙って実行されない」
    // を防ぐ。
    let chosen = runner.get(&ext).map(String::as_str).unwrap_or_else(|| default_runner_short(&ext));
    let base = shells
        .iter()
        .find(|s| s.short == chosen)
        .or_else(|| shells.iter().find(|s| s.short == default_runner_short(&ext)))
        .or_else(|| shells.iter().find(|s| s.short == guaranteed_runner_short(&ext)))?;
    Some(ShellSpec {
        label: base.label.clone(),
        short: base.short.clone(),
        program: base.program.clone(),
        args: script_run_args(&base.short, path),
        icon_source: base.icon_source.clone(),
    })
}

// スクリプト実行タブ用の Session を1つ作る。拡張子で判定できなければ既定シェルでそのフォルダを
// 開くだけ（実行はしないが、フォルダは開く＝握り潰さない）。
pub(crate) fn spawn_script_session(
    ctx: &egui::Context,
    path: &str,
    shells: &[ShellSpec],
    default_shell: usize,
    runner: &std::collections::BTreeMap<String, String>,
    bg: Color32,
    accent: Color32,
) -> Session {
    // 認識できたスクリプトのみ run-and-close（拡張子不明でフォルダを開くだけのフォールバックは
    // 通常の対話シェルなので閉じない）。
    let script = script_run_shell(path, runner, shells);
    let close_on_exit = script.is_some();
    let spec = script.unwrap_or_else(|| shells[default_shell].clone());
    let dir = script_parent(path);
    let title = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| spec.short.clone());
    let mut s = Session::spawn(ctx, title, &spec, &dir);
    s.bg = bg;
    s.accent = accent;
    s.close_on_exit = close_on_exit;
    s
}

// 右クリック「LustTermina で開く」を登録。指定した exe パスを指す（インストーラは
// インストール先を、単独の --install-context-menu は現在の exe を渡す）。
// フォルダ選択時は %1（そのフォルダ）、背景クリック時は %V（今開いているフォルダ）。
pub(crate) fn install_context_menu(root: windows::Win32::System::Registry::HKEY, exe: &str) -> bool {
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
pub(crate) fn uninstall_context_menu(root: windows::Win32::System::Registry::HKEY) -> bool {
    // 全て試みてから結果を返す（一部失敗でも残りは消す）。& は非短絡。
    let mut ok = reg_delete_tree(root, CTX_KEY_DIR) & reg_delete_tree(root, CTX_KEY_BG);
    for ext in SCRIPT_EXTS {
        ok &= reg_delete_tree(root, &script_ctx_key(ext));
    }
    ok &= reg_delete_tree(root, APP_REG_KEY);
    ok
}

// HKCU\<subkey> に DWORD を書く（アンインストール項目の NoModify/NoRepair 用）。
pub(crate) fn reg_set_dword(
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
pub(crate) const UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LustTermina";

// インストールのスコープ（このユーザーだけ / PC全体）。PC全体は管理者権限が要る。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum InstallScope {
    PerUser, // %LOCALAPPDATA% ＋ HKCU（UAC 不要）
    Machine, // %ProgramFiles% ＋ HKLM（UAC 必要）
}

// スコープに対応するレジストリのルート。
pub(crate) fn scope_root(scope: InstallScope) -> windows::Win32::System::Registry::HKEY {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    match scope {
        InstallScope::PerUser => HKEY_CURRENT_USER,
        InstallScope::Machine => HKEY_LOCAL_MACHINE,
    }
}

// インストール先フォルダ。PerUser=%LOCALAPPDATA%\Programs\LustTermina / Machine=%ProgramFiles%\LustTermina。
pub(crate) fn install_dir(scope: InstallScope) -> Option<std::path::PathBuf> {
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
pub(crate) fn installed_exe(scope: InstallScope) -> Option<std::path::PathBuf> {
    install_dir(scope).map(|d| d.join("lust_termina.exe"))
}
// スタートメニューのショートカット（.lnk）。PerUser=自分用 / Machine=全ユーザー用。
pub(crate) fn start_menu_lnk(scope: InstallScope) -> Option<std::path::PathBuf> {
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
pub(crate) fn config_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(std::path::Path::new(&base).join("LustTermina"))
}
// どちらかのスコープでインストール済みなら、そのスコープを返す（PC全体を優先）。
pub(crate) fn installed_scope() -> Option<InstallScope> {
    [InstallScope::Machine, InstallScope::PerUser]
        .into_iter()
        .find(|&s| installed_exe(s).map(|p| p.exists()).unwrap_or(false))
}
// インストール済みの版（「アプリと機能」の DisplayVersion）を読む。無ければ None。
pub(crate) fn installed_version(scope: InstallScope) -> Option<String> {
    reg_get_sz(scope_root(scope), UNINSTALL_KEY, "DisplayVersion")
}
// "a.b.c" を数値タプルにして比較。running が installed より新しければ true。
// 末尾の -beta 等の非数値要素は無視する（数値の major.minor.patch のみで判定）。
pub(crate) fn version_is_newer(running: &str, installed: &str) -> bool {
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
pub(crate) fn running_from_install() -> bool {
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
pub(crate) fn cleanup_old_exe() {
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
pub(crate) fn current_install_scope() -> InstallScope {
    match (std::env::current_exe(), install_dir(InstallScope::Machine)) {
        (Ok(cur), Some(m)) if cur.starts_with(&m) => InstallScope::Machine,
        _ => InstallScope::PerUser,
    }
}
// 管理者権限で動いているか。
pub(crate) fn is_elevated() -> bool {
    use windows::Win32::UI::Shell::IsUserAnAdmin;
    unsafe { IsUserAnAdmin().as_bool() }
}
// 自分自身を管理者として起動し直す（UAC）。指定引数で走らせ、終了を待って成否を返す。
// 未署名なので UAC は「発行元不明」の警告付きプロンプトになる。
pub(crate) fn relaunch_elevated(args: &str) -> bool {
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

// 枠（タイトルバー）ダークの再適用間隔＝剥がれてから戻るまでの最大待ち時間。
pub(crate) const TITLEBAR_REDARK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

// タイトルバー（ネイティブ枠）をダークにする（DWMWA_USE_IMMERSIVE_DARK_MODE）。
// この属性は DWM（コンポジタ）側がウィンドウごとに持つ状態で、システムの
// テーマ再評価やスリープ復帰などの外部要因で既定（ライト）に戻ることがある。
// 起動時の SetTheme 一発では剥がれたときに戻す経路がないため、これを直接
// 叩いて張り直す（冪等・数マイクロ秒なので頻繁に呼んで害はない）。
// 失敗しても実害は「枠が白いまま」で、次の間隔で自動再試行されるため流す。
pub(crate) fn apply_dark_titlebar(frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use windows::core::BOOL;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    let dark = BOOL(1);
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        )
    };
}

// 窓を前面へ出す（最小化なら復元も）。Explorer 転送でタブが生えたときに呼ぶ。
// winit の set_minimized(false) が Windows で不安定なため Win32 を直接叩く。前面化は
// 転送プロセス側の AllowSetForegroundWindow で許可済み。SW_RESTORE は最大化も戻して
// しまうので、IsIconic（最小化）判定を挟み「最小化のときだけ復元」する。
pub(crate) fn bring_to_front(frame: &eframe::Frame) {
    if let Some(hwnd) = main_hwnd(frame) {
        bring_hwnd_to_front(hwnd);
    }
}

// 本体ウィンドウのハンドル（数値）。eframe の Frame を持たない場所（トレイの
// ウィンドウプロシージャ等）へ渡せるよう、生の isize で取り出す。
pub(crate) fn main_hwnd(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    let handle = frame.window_handle().ok()?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return None;
    };
    Some(h.hwnd.get() as isize)
}

// 前面化の実体。eframe の描画フレームの中からも、トレイのクリック（ウィンドウ
// プロシージャの中＝egui のフレームが回っていない状況）からも、同じ処理を使う。
pub(crate) fn bring_hwnd_to_front(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    if hwnd == 0 {
        return;
    }
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE); // 最小化のときだけ復元
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

// MessageBox（はい/いいえ等）。UX は標準ダイアログのみで完結させ、依存を増やさない。
pub(crate) fn msg_box(
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
pub(crate) fn confirm_yes_no(text: &str, caption: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONWARNING, MB_YESNO};
    msg_box(text, caption, MB_YESNO | MB_ICONWARNING).0 == IDYES.0
}

// アンインストール確認ダイアログ（TaskDialog）。データ削除はチェックボックスで選ぶ（既定オフ）。
// 戻り値: None=キャンセル、Some(delete_data)=実行（delete_data はチェック状態）。
pub(crate) fn ask_uninstall() -> Option<bool> {
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
pub(crate) fn create_shortcut(
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
pub(crate) fn write_uninstall_entry(
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
// ---- PATH 登録（インストール先をユーザー/システムの Path に追記・除去）----
// PerUser は HKCU\Environment、Machine は HKLM のシステム環境キー（昇格済みで呼ばれる前提）。
// Path は REG_EXPAND_SZ（%SystemRoot% 等の変数を含む）なので、既存 reg_get_sz/reg_set_sz
// （REG_SZ 専用）は使わず、生読み（NOEXPAND）と EXPAND_SZ 書き込みの専用ヘルパで扱う。
// 変更後は WM_SETTINGCHANGE("Environment") を放送（既存シェルは再起動まで反映されない）。

// PATH 経由で使う短い別名（シム .cmd の名前）。exe 名は変えず、ここ一箇所で決める。
// lt は他ツールと被りやすいので 3 文字（本人指定 2026-07-15）。
pub(crate) const CLI_ALIAS: &str = "ltm";

pub(crate) fn env_reg_key(scope: InstallScope) -> (windows::Win32::System::Registry::HKEY, &'static str) {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    match scope {
        InstallScope::PerUser => (HKEY_CURRENT_USER, "Environment"),
        InstallScope::Machine => (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    }
}

// Path 値を「変数を展開せず」に読む（REG_SZ / REG_EXPAND_SZ 両対応）。
pub(crate) fn reg_get_path_raw(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegGetValueW, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };
    let subkey_w = wide_nul(subkey);
    let name_w = wide_nul("Path");
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    unsafe {
        let mut size: u32 = 0;
        let rc = RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            flags,
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
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if rc2 != ERROR_SUCCESS {
            return None;
        }
        let s: Vec<u16> = buf[..(size as usize)]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&s))
    }
}

// Path 値を REG_EXPAND_SZ で書く（%VAR% 入りエントリを壊さない型）。
pub(crate) fn reg_set_path_raw(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    value: &str,
) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, KEY_WRITE, REG_EXPAND_SZ,
        REG_OPTION_NON_VOLATILE,
    };
    let subkey_w = wide_nul(subkey);
    let name_w = wide_nul("Path");
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
        let rc2 = RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), None, REG_EXPAND_SZ, Some(&data));
        let _ = RegCloseKey(hkey);
        rc2 == ERROR_SUCCESS
    }
}

// 環境変数の変更をシステムに知らせる（新しく開くシェルから有効になる）。
pub(crate) fn broadcast_env_change() {
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let env = wide_nul("Environment");
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(env.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            3000,
            None,
        );
    }
}

// インストール先を Path に追記（既にあれば何もしない。比較は大小無視・空白無視）。
pub(crate) fn add_to_path(scope: InstallScope, dir: &str) -> bool {
    let (root, key) = env_reg_key(scope);
    let cur = reg_get_path_raw(root, key).unwrap_or_default();
    if cur
        .split(';')
        .any(|p| p.trim().eq_ignore_ascii_case(dir))
    {
        return true; // 登録済み
    }
    let new = if cur.trim().is_empty() {
        dir.to_string()
    } else {
        format!("{};{}", cur.trim_end_matches(';'), dir)
    };
    let ok = reg_set_path_raw(root, key, &new);
    if ok {
        broadcast_env_change();
    }
    ok
}

// インストール先を Path から除去（無ければ成功扱い）。
pub(crate) fn remove_from_path(scope: InstallScope, dir: &str) -> bool {
    let (root, key) = env_reg_key(scope);
    let Some(cur) = reg_get_path_raw(root, key) else {
        return true;
    };
    if !cur.split(';').any(|p| p.trim().eq_ignore_ascii_case(dir)) {
        return true;
    }
    let parts: Vec<&str> = cur
        .split(';')
        .filter(|p| !p.trim().is_empty() && !p.trim().eq_ignore_ascii_case(dir))
        .collect();
    let ok = reg_set_path_raw(root, key, &parts.join(";"));
    if ok {
        broadcast_env_change();
    }
    ok
}

pub(crate) fn do_install(scope: InstallScope) -> bool {
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
    // PATH 登録＋短い別名シム（`lt` で本体に委譲する 1 行の cmd）。
    // シムはインストールフォルダ内なのでアンインストール時はフォルダ削除に含まれる。
    add_to_path(scope, &dir_s);
    let _ = std::fs::write(
        dir.join(format!("{CLI_ALIAS}.cmd")),
        "@\"%~dp0lust_termina.exe\" %*\r\n",
    );
    true
}

// 指定スコープの登録物を全て消す。delete_config で設定・データも消す（選択式）。
// PC全体スコープなら管理者権限で呼ばれている前提。
pub(crate) fn do_uninstall(scope: InstallScope, delete_config: bool) -> bool {
    let root = scope_root(scope);
    uninstall_context_menu(root);
    let _ = reg_delete_tree(root, UNINSTALL_KEY);
    if let Some(lnk) = start_menu_lnk(scope) {
        let _ = std::fs::remove_file(lnk);
    }
    if delete_config {
        if let Some(cfg) = config_dir() {
            log::info!("deleting data folder on uninstall");
            crate::logging::close();
            let _ = std::fs::remove_dir_all(cfg);
        }
    }
    // 実行中の自分自身（インストール先 exe）はロックされていて消せないので、本体終了を
    // 少し待ってからフォルダごと削除する小さな外部プロセスを投げておく。
    if let Some(dir) = install_dir(scope) {
        remove_from_path(scope, &dir.to_string_lossy());
        schedule_dir_deletion(&dir.to_string_lossy());
    }
    true
}

// インストールを実行。PC全体スコープで未昇格なら、UAC で自分を昇格し直して代行させる。
pub(crate) fn perform_install(scope: InstallScope) -> bool {
    if scope == InstallScope::Machine && !is_elevated() {
        relaunch_elevated("--do-install-machine")
    } else {
        do_install(scope)
    }
}

// アンインストールを実行。PC全体スコープで未昇格なら UAC 昇格して代行させる。
pub(crate) fn perform_uninstall(scope: InstallScope, delete_config: bool) -> bool {
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
pub(crate) enum InstallChoice {
    Install(InstallScope), // 選んだスコープへ導入
    OpenPortable,          // 導入せずこのまま（ポータブル）起動
    Cancel,                // 何もしない（起動もしない）
}

// インストール確認＋インストール先の選択（TaskDialog のラジオボタン）。
// 各ラジオに「そのスコープの現在の版」を出す（ユーザー専用／PC全体で別々に入り得るため）。
// 既にインストール済みなら導入ボタンの文言を「更新／再インストール／ダウングレード」に切替。
// ボタンは3系統: 導入 ／「インストールせずに開く」＝今回だけ起動 ／「キャンセル」＝何もしない。
pub(crate) fn ask_install_scope() -> InstallChoice {
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
pub(crate) fn schedule_dir_deletion(dir: &str) {
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

