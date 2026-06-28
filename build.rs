// Windows の exe にアイコンを埋め込む（Explorer 等で表示される .exe のアイコン）。
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // rc.exe が見つからない等で失敗しても、ビルド自体は通す（アイコン無しになるだけ）。
            println!("cargo:warning=icon embed skipped: {e}");
        }
    }
}
