use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Processor;
use eframe::egui;
use log::Level;

use crate::logging::TabLog;
use crate::recording::CAST_EXT;
use crate::session::{EventProxy, TermSize};

pub(crate) const SPEEDS: [u32; 3] = [1, 2, 8];
const TICK: Duration = Duration::from_millis(30);
const PART_MARK: &str = "_part";

pub(crate) struct PlaybackCtl {
    pub(crate) paused: AtomicBool,
    pub(crate) speed: AtomicU32,
    pub(crate) skip: AtomicBool,
    pub(crate) finished: AtomicBool,
    pub(crate) played_ms: AtomicU64,
    stop: AtomicBool,
}

impl PlaybackCtl {
    fn new() -> Self {
        PlaybackCtl {
            paused: AtomicBool::new(false),
            speed: AtomicU32::new(SPEEDS[0]),
            skip: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            played_ms: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn is_cast_path(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == CAST_EXT)
}

pub(crate) fn cast_parts(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else { return vec![path.to_path_buf()] };
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return vec![path.to_path_buf()] };
    let base = match stem.rfind(PART_MARK) {
        Some(at) if stem[at + PART_MARK.len()..].parse::<u32>().is_ok() => &stem[..at],
        _ => stem,
    };
    let mut parts = vec![dir.join(format!("{base}.{CAST_EXT}"))];
    for n in 2.. {
        let next = dir.join(format!("{base}{PART_MARK}{n}.{CAST_EXT}"));
        if !next.exists() {
            break;
        }
        parts.push(next);
    }
    if !parts[0].exists() {
        parts.remove(0);
    }
    parts
}

#[derive(serde::Deserialize)]
struct Header {
    width: usize,
    height: usize,
}

pub(crate) fn read_header_size(path: &Path) -> Result<TermSize, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut line = String::new();
    BufReader::new(file)
        .read_line(&mut line)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let h: Header = serde_json::from_str(&line).map_err(|e| format!("{}: not an asciicast v2 file: {e}", path.display()))?;
    Ok(TermSize { cols: h.width.max(1), rows: h.height.max(1) })
}

pub(crate) struct Player {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) term: Arc<Mutex<Term<EventProxy>>>,
    pub(crate) ctx: egui::Context,
    pub(crate) log: Arc<TabLog>,
    pub(crate) idle_limit: Duration,
}

impl Player {
    pub(crate) fn start(self) -> Result<Arc<PlaybackCtl>, String> {
        let ctl = Arc::new(PlaybackCtl::new());
        let thread_ctl = ctl.clone();
        std::thread::Builder::new()
            .name(format!("playback-{}", self.log.id()))
            .spawn(move || self.run(&thread_ctl))
            .map_err(|e| format!("could not start the playback thread: {e}"))?;
        Ok(ctl)
    }

    fn run(self, ctl: &PlaybackCtl) {
        let mut parser: Processor = Processor::new();
        let mut events = 0u64;
        for path in &self.files {
            if ctl.stop.load(Ordering::Relaxed) {
                break;
            }
            match self.play_file(path, ctl, &mut parser) {
                Ok(n) => events += n,
                Err(e) => {
                    self.log.event(Level::Warn, format_args!("playback stopped: {e}"));
                    break;
                }
            }
        }
        ctl.finished.store(true, Ordering::Relaxed);
        self.log.event(Level::Info, format_args!("playback finished after {events} event(s)"));
        self.ctx.request_repaint();
    }

    fn play_file(&self, path: &Path, ctl: &PlaybackCtl, parser: &mut Processor) -> Result<u64, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = BufReader::new(file).lines();
        let header = lines
            .next()
            .ok_or_else(|| format!("{}: empty file", path.display()))?
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let h: Header = serde_json::from_str(&header).map_err(|e| format!("{}: bad header: {e}", path.display()))?;
        self.resize(h.width, h.height);
        let mut prev = 0.0f64;
        let mut count = 0u64;
        for (n, line) in lines.enumerate() {
            if ctl.stop.load(Ordering::Relaxed) {
                break;
            }
            let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let (t, kind, data): (f64, String, String) = serde_json::from_str(&line)
                .map_err(|e| format!("{} line {}: {e}", path.display(), n + 2))?;
            let gap = Duration::from_secs_f64((t - prev).max(0.0)).min(self.idle_limit);
            prev = t;
            self.wait(gap, ctl);
            match kind.as_str() {
                "o" => {
                    if let Ok(mut term) = self.term.lock() {
                        parser.advance(&mut *term, data.as_bytes());
                    }
                }
                "r" => {
                    if let Some((c, r)) = data.split_once('x') {
                        if let (Ok(c), Ok(r)) = (c.parse(), r.parse()) {
                            self.resize(c, r);
                        }
                    }
                }
                _ => {}
            }
            count += 1;
            self.ctx.request_repaint();
        }
        Ok(count)
    }

    fn resize(&self, cols: usize, rows: usize) {
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize { cols: cols.max(1), rows: rows.max(1) });
        }
    }

    fn wait(&self, gap: Duration, ctl: &PlaybackCtl) {
        let mut left = gap;
        loop {
            if ctl.stop.load(Ordering::Relaxed) || ctl.skip.load(Ordering::Relaxed) {
                return;
            }
            if ctl.paused.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                continue;
            }
            if left.is_zero() {
                return;
            }
            let speed = ctl.speed.load(Ordering::Relaxed).max(1);
            let wall = TICK.min(left / speed);
            let wall = if wall.is_zero() { left / speed } else { wall };
            std::thread::sleep(wall);
            let advanced = wall * speed;
            left = left.saturating_sub(advanced);
            ctl.played_ms.fetch_add(advanced.as_millis() as u64, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_parts_from_any_part() {
        let dir = std::env::temp_dir().join(format!("lust_termina_parts_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        for name in ["x_rec1.cast", "x_rec1_part2.cast", "x_rec1_part3.cast", "x_rec2.cast"] {
            std::fs::write(dir.join(name), "").expect("write");
        }
        let names = |p: &Path| {
            cast_parts(p)
                .iter()
                .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>()
        };
        let want = vec!["x_rec1.cast", "x_rec1_part2.cast", "x_rec1_part3.cast"];
        assert_eq!(names(&dir.join("x_rec1.cast")), want);
        assert_eq!(names(&dir.join("x_rec1_part3.cast")), want);
        assert_eq!(names(&dir.join("x_rec2.cast")), vec!["x_rec2.cast"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
