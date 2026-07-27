//! シェル探索と program 解決。

use crate::icons::*;

// ---- シェル定義（探索して見つかったものを並べる。ハードコード列挙はしない）----
#[derive(Clone)]
pub(crate) struct ShellSpec {
    pub(crate) label: String,    // メニュー表示名
    pub(crate) short: String,    // タブ名の接頭辞
    pub(crate) program: String,  // 実行ファイル（フルパス or PATH 上の名前）
    pub(crate) args: Vec<String>, // 起動引数
    // アイコン抽出元（None なら program から抽出）。VS Developer 系のように
    // 実体が cmd/pwsh で固有 exe を持たないものに、別の exe（devenv.exe 等）の
    // アイコンを割り当てるための逃がし。
    pub(crate) icon_source: Option<String>,
}

pub(crate) fn find_existing(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .find(|p| !p.is_empty() && std::path::Path::new(p).exists())
        .cloned()
}

// PATH を走査して実行ファイルを探す（pwsh 等が固定パスに無くても拾う）。
pub(crate) fn which(name: &str) -> Option<String> {
    let path = std::env::var("PATH").ok()?;
    path.split(';')
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}\\{name}"))
        .find(|p| std::path::Path::new(p).exists())
}

// Visual Studio 2022 のインストールディレクトリを探す（VsDevCmd.bat の在処で判定）。
pub(crate) fn find_vs2022() -> Option<String> {
    let pf = std::env::var("ProgramFiles").unwrap_or_default();
    ["Community", "Professional", "Enterprise", "BuildTools", "Preview"]
        .iter()
        .map(|ed| format!("{pf}\\Microsoft Visual Studio\\2022\\{ed}"))
        .find(|dir| std::path::Path::new(&format!("{dir}\\Common7\\Tools\\VsDevCmd.bat")).exists())
}

// システムを探索して、利用可能なシェルを並べる。
pub(crate) fn discover_shells() -> Vec<ShellSpec> {
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
pub(crate) fn resolve_program(program: &str) -> Option<String> {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        return p.exists().then(|| program.to_string());
    }
    which(program)
}
