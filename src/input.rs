//! キー/マウス入力のエスケープ列エンコード。
use eframe::egui;

// マウスレポートのエスケープ列を1件ぶん組み立てる。
// cb_base: ボタン基底コード（0=左 1=中 2=右 / 64=ホイール上 65=ホイール下 /
//          モーションは呼び側で +32 して渡す）。col/row は 0 始まりのセル座標。
// press=false は SGR ではリリース('m')、X10 では全ボタン解放(3) を意味する。
pub(crate) fn mouse_report(
    sgr: bool,
    cb_base: u32,
    col: usize,
    row: usize,
    m: &egui::Modifiers,
    press: bool,
) -> Vec<u8> {
    // 修飾キーのビット（shift=4, alt/meta=8, ctrl=16）を加算。
    let mods = (if m.shift { 4 } else { 0 })
        + (if m.alt { 8 } else { 0 })
        + (if m.ctrl { 16 } else { 0 });
    let (cx, cy) = (col + 1, row + 1); // 端末座標は 1 始まり
    if sgr {
        // SGR(1006): ESC [ < Cb ; Cx ; Cy  (M=押下 / m=解放)。列/行が 223 を超えても安全。
        let cb = cb_base + mods;
        let fin = if press { 'M' } else { 'm' };
        format!("\x1b[<{cb};{cx};{cy}{fin}").into_bytes()
    } else {
        // X10 互換: ESC [ M (Cb+32) (Cx+32) (Cy+32)。解放は全ボタン解放=3。223 超は表現不可。
        let base = if press { cb_base } else { 3 };
        let cb = (base + mods + 32).min(255);
        let x = ((cx as u32) + 32).min(255);
        let y = ((cy as u32) + 32).min(255);
        vec![0x1b, b'[', b'M', cb as u8, x as u8, y as u8]
    }
}

pub(crate) fn ctrl_letter(key: egui::Key) -> Option<u8> {
    // C / V / X は egui-winit が Copy/Paste/Cut に化けさせ Key で来ないので除外
    use egui::Key::*;
    let c = match key {
        A => b'a', B => b'b', D => b'd', E => b'e', F => b'f',
        G => b'g', H => b'h', I => b'i', J => b'j', K => b'k', L => b'l',
        M => b'm', N => b'n', O => b'o', P => b'p', Q => b'q', R => b'r',
        S => b's', T => b't', U => b'u', W => b'w',
        Y => b'y', Z => b'z',
        _ => return None,
    };
    Some(c)
}

// xterm 方式の修飾パラメータ。Shift=1, Alt=2, Ctrl=4 のビット和に 1 を足す
// （例: Ctrl=5, Shift=2, Ctrl+Shift=6）。修飾なしは None。
pub(crate) fn csi_mod(m: &egui::Modifiers) -> Option<u8> {
    let mut bits = 0u8;
    if m.shift {
        bits |= 1;
    }
    if m.alt {
        bits |= 2;
    }
    if m.ctrl {
        bits |= 4;
    }
    if bits == 0 {
        None
    } else {
        Some(bits + 1)
    }
}

// 矢印・編集キー等を、アプリカーソルキーモードと修飾を織り込んで符号化する。
// - カーソル/Home/End: 修飾ありは CSI 1;m<終端>、修飾なしはアプリモードで SS3(\x1bO)、
//   通常モードで CSI(\x1b[)。
// - チルダ系(Delete/PageUp/PageDown/Insert): CSI n~ / 修飾ありは CSI n;m~。
// 該当なしは空 Vec（＝送らない）。
pub(crate) fn encode_key(key: egui::Key, m: &egui::Modifiers, app_cursor: bool) -> Vec<u8> {
    use egui::Key;
    let modc = csi_mod(m);
    let cursor = |fin: u8| -> Vec<u8> {
        match modc {
            Some(code) => format!("\x1b[1;{code}{}", fin as char).into_bytes(),
            None if app_cursor => vec![0x1b, b'O', fin],
            None => vec![0x1b, b'[', fin],
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        match modc {
            Some(code) => format!("\x1b[{n};{code}~").into_bytes(),
            None => format!("\x1b[{n}~").into_bytes(),
        }
    };
    match key {
        Key::ArrowUp => cursor(b'A'),
        Key::ArrowDown => cursor(b'B'),
        Key::ArrowRight => cursor(b'C'),
        Key::ArrowLeft => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        // 基本キーは従来どおり（修飾での区別はレガシー・エンコードでは表現できない）。
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        _ => Vec::new(),
    }
}

