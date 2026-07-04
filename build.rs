// Windows の exe にアイコンを埋め込む（Explorer 等で表示される .exe のアイコン）。
// あわせて comctl32 v6 依存の manifest を埋め込む。これが無いと TaskDialog（インストール／
// アンインストールのダイアログ）が動かない（TaskDialogIndirect が失敗する）。視覚スタイルも有効化される。
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set_manifest(COMCTL6_MANIFEST);
        if let Err(e) = res.compile() {
            // rc.exe が見つからない等で失敗しても、ビルド自体は通す（アイコン無しになるだけ）。
            println!("cargo:warning=resource embed skipped: {e}");
        }
    }
}

#[cfg(windows)]
const COMCTL6_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
</assembly>
"#;
