//! アイコン抽出・縮小。
use eframe::egui;
use crate::shells::resolve_program;

// アイコンを面積平均で正方形 target px へ事前縮小する。GPU の mipmap 無し
// bilinear 縮小だと細い線（PS7 ロゴの白い >_ 等）を取りこぼして潰れるので、
// 読み込み時に CPU で綺麗に縮めてからアップロードする。透過の色滲みを防ぐため
// アルファで重み付け（premultiply）して平均する。
pub(crate) fn downscale_square(src: &egui::ColorImage, target: usize) -> egui::ColorImage {
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
pub(crate) fn load_icon_image(path: &str) -> Option<egui::ColorImage> {
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
pub(crate) fn msix_icon_for(exe: &str) -> Option<String> {
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
pub(crate) fn pwsh_pkg_icon() -> Option<String> {
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
pub(crate) fn pwsh_package_paths() -> Vec<String> {
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
pub(crate) fn pwsh_msix_icon() -> Option<String> {
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
pub(crate) fn extract_icon_rgba(program: &str) -> Option<egui::ColorImage> {
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

