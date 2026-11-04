use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use log::Level;

use crate::logging::TabLog;

pub(crate) const CAST_EXT: &str = "cast";

pub(crate) struct CastRecorder {
    log: Arc<TabLog>,
    rec_no: u32,
    part: u32,
    file: Option<File>,
    path: PathBuf,
    start: Instant,
    written: u64,
    events_in_part: u64,
    total: u64,
    max_bytes: u64,
    pending: Vec<u8>,
    cols: usize,
    rows: usize,
}

impl CastRecorder {
    pub(crate) fn start(log: Arc<TabLog>, rec_no: u32, cols: usize, rows: usize, max_kb: u64) -> Result<Self, String> {
        let mut rec = CastRecorder {
            log,
            rec_no,
            part: 0,
            file: None,
            path: PathBuf::new(),
            start: Instant::now(),
            written: 0,
            events_in_part: 0,
            total: 0,
            max_bytes: max_kb.max(1).saturating_mul(1024),
            pending: Vec::new(),
            cols,
            rows,
        };
        rec.open_next_part()?;
        rec.log.event(
            Level::Info,
            format_args!("recording {} started: {}", rec.rec_no, rec.path.display()),
        );
        Ok(rec)
    }

    fn part_path(&self) -> Result<PathBuf, String> {
        let dir = crate::logging::tab_dir().ok_or("no data folder")?;
        let stem = self.log.stem();
        let name = if self.part == 1 {
            format!("{stem}_rec{}.{CAST_EXT}", self.rec_no)
        } else {
            format!("{stem}_rec{}_part{}.{CAST_EXT}", self.rec_no, self.part)
        };
        Ok(dir.join(name))
    }

    fn open_next_part(&mut self) -> Result<(), String> {
        self.part += 1;
        let path = self.part_path()?;
        let mut file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let header = format!(
            "{{\"version\": 2, \"width\": {}, \"height\": {}, \"timestamp\": {unix}}}\n",
            self.cols, self.rows
        );
        file.write_all(header.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.written = header.len() as u64;
        self.events_in_part = 0;
        self.file = Some(file);
        self.path = path;
        self.start = Instant::now();
        Ok(())
    }

    fn write_event(&mut self, kind: &str, data: &str) {
        if self.file.is_none() {
            return;
        }
        let line = format!(
            "[{:.6}, \"{kind}\", \"{}\"]\n",
            self.start.elapsed().as_secs_f64(),
            json_escape(data)
        );
        if self.events_in_part > 0 && self.written + line.len() as u64 > self.max_bytes {
            if let Err(e) = self.open_next_part() {
                self.file = None;
                self.log.event(Level::Warn, format_args!("recording {} stopped: {e}", self.rec_no));
                return;
            }
            self.log.event(
                Level::Info,
                format_args!("recording {} continues in {}", self.rec_no, self.path.display()),
            );
            return self.write_event(kind, data);
        }
        let Some(file) = self.file.as_mut() else { return };
        match file.write_all(line.as_bytes()) {
            Ok(()) => {
                self.written += line.len() as u64;
                self.events_in_part += 1;
            }
            Err(e) => {
                self.file = None;
                self.log.event(Level::Warn, format_args!("recording {} stopped: write failed: {e}", self.rec_no));
            }
        }
    }

    pub(crate) fn output(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let text = decode_utf8(&mut self.pending, bytes);
        if !text.is_empty() {
            self.write_event("o", &text);
        }
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.write_event("r", &format!("{cols}x{rows}"));
    }

}

pub(crate) fn recordings_of(stem: &str) -> Vec<(u32, PathBuf)> {
    match crate::logging::tab_dir() {
        Some(dir) => recordings_in(&dir, stem),
        None => Vec::new(),
    }
}

fn recordings_in(dir: &std::path::Path, stem: &str) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let prefix = format!("{stem}_rec");
    let suffix = format!(".{CAST_EXT}");
    let mut found: Vec<(u32, PathBuf)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let n = name.strip_prefix(&prefix)?.strip_suffix(&suffix)?.parse().ok()?;
            Some((n, p))
        })
        .collect();
    found.sort();
    found
}

fn decode_utf8(pending: &mut Vec<u8>, bytes: &[u8]) -> String {
    let mut buf = std::mem::take(pending);
    buf.extend_from_slice(bytes);
    let mut out = String::new();
    let mut rest: &[u8] = &buf;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = &bad[n..];
                    }
                    None => {
                        rest = bad;
                        break;
                    }
                }
            }
        }
    }
    *pending = rest.to_vec();
    out
}

impl Drop for CastRecorder {
    fn drop(&mut self) {
        self.log.event(
            Level::Info,
            format_args!(
                "recording {} ended after {} bytes of output ({} part(s))",
                self.rec_no, self.total, self.part
            ),
        );
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7F}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_first_parts_of_a_tab_in_order() {
        let dir = std::env::temp_dir().join(format!("lust_termina_recs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        for name in ["t_1_rec2.cast", "t_1_rec10.cast", "t_1_rec1.cast", "t_1_rec1_part2.cast", "t_12_rec1.cast", "t_1.log"] {
            std::fs::write(dir.join(name), "").expect("write");
        }
        let got: Vec<u32> = recordings_in(&dir, "t_1").into_iter().map(|(n, _)| n).collect();
        assert_eq!(got, vec![1, 2, 10]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_a_character_split_across_reads() {
        let bytes = "あい".as_bytes();
        let mut pending = Vec::new();
        assert_eq!(decode_utf8(&mut pending, &bytes[..4]), "あ");
        assert_eq!(decode_utf8(&mut pending, &bytes[4..]), "い");
        assert!(pending.is_empty());
    }

    #[test]
    fn replaces_invalid_bytes() {
        let mut pending = Vec::new();
        assert_eq!(decode_utf8(&mut pending, b"a\xffb"), "a\u{FFFD}b");
    }

    #[test]
    fn escapes_control_and_quotes() {
        assert_eq!(json_escape("a\"b\\c\n\r\t\u{1b}[0m"), "a\\\"b\\\\c\\n\\r\\t\\u001b[0m");
    }
}
