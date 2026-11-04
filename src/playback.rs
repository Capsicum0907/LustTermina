use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Processor;
use eframe::egui;
use log::Level;

use crate::logging::TabLog;
use crate::recording::CAST_EXT;
use crate::session::{EventProxy, TermSize};

pub(crate) const SPEEDS: [u32; 3] = [1, 2, 8];
const TICK: Duration = Duration::from_millis(30);
const PART_MARK: &str = "_part";
const RESET_SEQUENCE: &[u8] = b"\x1bc";
const NO_SEEK: u64 = u64::MAX;
const UNKNOWN: u64 = u64::MAX;
const SNAPSHOT_GAP_MS: f64 = 150.0;
const MIN_OVERLAP: usize = 2;

pub(crate) struct PlaybackCtl {
    pub(crate) paused: AtomicBool,
    pub(crate) speed: AtomicU32,
    pub(crate) finished: AtomicBool,
    pub(crate) pos_ms: AtomicU64,
    pub(crate) history_view: AtomicBool,
    pub(crate) transcript: Mutex<Transcript>,
    total_ms: AtomicU64,
    seek_to: AtomicU64,
    stop: AtomicBool,
}

impl PlaybackCtl {
    fn new() -> Self {
        PlaybackCtl {
            paused: AtomicBool::new(false),
            speed: AtomicU32::new(SPEEDS[0]),
            finished: AtomicBool::new(false),
            pos_ms: AtomicU64::new(0),
            history_view: AtomicBool::new(false),
            transcript: Mutex::new(Transcript::default()),
            total_ms: AtomicU64::new(UNKNOWN),
            seek_to: AtomicU64::new(NO_SEEK),
            stop: AtomicBool::new(false),
        }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(crate) fn total_ms(&self) -> Option<u64> {
        let t = self.total_ms.load(Ordering::Relaxed);
        (t != UNKNOWN).then_some(t)
    }

    pub(crate) fn seek(&self, ms: u64) {
        self.seek_to.store(ms.min(NO_SEEK - 1), Ordering::Relaxed);
    }

    fn seek_pending(&self) -> bool {
        self.seek_to.load(Ordering::Relaxed) != NO_SEEK
    }

    fn take_seek(&self) -> Option<u64> {
        let t = self.seek_to.swap(NO_SEEK, Ordering::Relaxed);
        (t != NO_SEEK).then_some(t)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
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

fn event_time(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix('[')?;
    rest[..rest.find(',')?].trim().parse().ok()
}

struct Clock {
    idle_limit_ms: f64,
    prev_in_part: f64,
    now_ms: f64,
}

impl Clock {
    fn new(idle_limit: Duration) -> Self {
        Clock { idle_limit_ms: idle_limit.as_secs_f64() * 1000.0, prev_in_part: 0.0, now_ms: 0.0 }
    }

    fn new_part(&mut self) {
        self.prev_in_part = 0.0;
    }

    fn gap_ms(&mut self, t: f64) -> f64 {
        let gap = ((t - self.prev_in_part) * 1000.0).max(0.0).min(self.idle_limit_ms);
        self.prev_in_part = t;
        gap
    }
}

fn measure(files: &[PathBuf], idle_limit: Duration) -> Result<u64, String> {
    let mut clock = Clock::new(idle_limit);
    for path in files {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        clock.new_part();
        for line in BufReader::new(file).lines().skip(1) {
            let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(t) = event_time(&line) {
                clock.now_ms += clock.gap_ms(t);
            }
        }
    }
    Ok(clock.now_ms as u64)
}

enum Outcome {
    Done,
    Stopped,
    Restart(u64),
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
        let measure_ctl = ctl.clone();
        let measure_files = self.files.clone();
        let measure_log = self.log.clone();
        let measure_ctx = self.ctx.clone();
        let idle_limit = self.idle_limit;
        let measured = std::thread::Builder::new()
            .name(format!("playback-measure-{}", self.log.id()))
            .spawn(move || match measure(&measure_files, idle_limit) {
                Ok(ms) => {
                    measure_ctl.total_ms.store(ms, Ordering::Relaxed);
                    measure_ctx.request_repaint();
                }
                Err(e) => measure_log.event(Level::Warn, format_args!("could not measure the recording: {e}")),
            });
        if let Err(e) = measured {
            self.log.event(Level::Warn, format_args!("could not start measuring the recording: {e}"));
        }
        let thread_ctl = ctl.clone();
        std::thread::Builder::new()
            .name(format!("playback-{}", self.log.id()))
            .spawn(move || self.run(&thread_ctl))
            .map_err(|e| format!("could not start the playback thread: {e}"))?;
        Ok(ctl)
    }

    fn run(self, ctl: &PlaybackCtl) {
        let mut fast_until: Option<u64> = None;
        'restart: loop {
            let mut parser: Processor = Processor::new();
            if let Ok(mut term) = self.term.lock() {
                parser.advance(&mut *term, RESET_SEQUENCE);
            }
            ctl.pos_ms.store(0, Ordering::Relaxed);
            ctl.finished.store(false, Ordering::Relaxed);
            if let Ok(mut t) = ctl.transcript.lock() {
                *t = Transcript::default();
            }
            let mut clock = Clock::new(self.idle_limit);
            match self.play_all(ctl, &mut parser, &mut clock, &mut fast_until) {
                Ok(Outcome::Stopped) => return,
                Ok(Outcome::Restart(t)) => {
                    fast_until = Some(t);
                    continue 'restart;
                }
                Ok(Outcome::Done) => {
                    self.log.event(Level::Info, format_args!("playback reached the end"));
                }
                Err(e) => {
                    self.log.event(Level::Warn, format_args!("playback stopped: {e}"));
                }
            }
            ctl.finished.store(true, Ordering::Relaxed);
            self.ctx.request_repaint();
            loop {
                if ctl.stopped() {
                    return;
                }
                if let Some(t) = ctl.take_seek() {
                    fast_until = Some(t);
                    continue 'restart;
                }
                std::thread::sleep(TICK);
            }
        }
    }

    fn play_all(
        &self,
        ctl: &PlaybackCtl,
        parser: &mut Processor,
        clock: &mut Clock,
        fast_until: &mut Option<u64>,
    ) -> Result<Outcome, String> {
        let mut dirty = false;
        for path in &self.files {
            let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut lines = BufReader::new(file).lines();
            let header = lines
                .next()
                .ok_or_else(|| format!("{}: empty file", path.display()))?
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let h: Header = serde_json::from_str(&header).map_err(|e| format!("{}: bad header: {e}", path.display()))?;
            self.resize(h.width, h.height);
            clock.new_part();
            for (n, line) in lines.enumerate() {
                if ctl.stopped() {
                    return Ok(Outcome::Stopped);
                }
                let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
                if line.trim().is_empty() {
                    continue;
                }
                let (t, kind, data): (f64, String, String) = serde_json::from_str(&line)
                    .map_err(|e| format!("{} line {}: {e}", path.display(), n + 2))?;
                let before = clock.now_ms;
                let gap = clock.gap_ms(t);
                let at = before + gap;
                if dirty && gap >= SNAPSHOT_GAP_MS {
                    self.snapshot(ctl);
                }
                if let Some(seek) = ctl.take_seek() {
                    if (seek as f64) < before {
                        return Ok(Outcome::Restart(seek));
                    }
                    *fast_until = Some(seek);
                }
                let fast = fast_until.is_some_and(|f| at <= f as f64);
                if !fast {
                    if fast_until.take().is_some() {
                        self.ctx.request_repaint();
                    }
                    if !self.wait(gap, before, ctl) {
                        if ctl.stopped() {
                            return Ok(Outcome::Stopped);
                        }
                        if let Some(seek) = ctl.take_seek() {
                            if (seek as f64) < before {
                                return Ok(Outcome::Restart(seek));
                            }
                            *fast_until = Some(seek);
                        }
                    }
                }
                clock.now_ms = at;
                ctl.pos_ms.store(at as u64, Ordering::Relaxed);
                self.apply(parser, &kind, &data);
                dirty = true;
                if !fast {
                    self.ctx.request_repaint();
                }
            }
        }
        if dirty {
            self.snapshot(ctl);
        }
        if fast_until.take().is_some() {
            self.ctx.request_repaint();
        }
        Ok(Outcome::Done)
    }

    fn snapshot(&self, ctl: &PlaybackCtl) {
        let screen = match self.term.lock() {
            Ok(term) => screen_text(&term),
            Err(_) => return,
        };
        if let Ok(mut t) = ctl.transcript.lock() {
            t.merge(screen);
        }
    }

    fn apply(&self, parser: &mut Processor, kind: &str, data: &str) {
        match kind {
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
    }

    fn resize(&self, cols: usize, rows: usize) {
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize { cols: cols.max(1), rows: rows.max(1) });
        }
    }

    fn wait(&self, gap_ms: f64, base_ms: f64, ctl: &PlaybackCtl) -> bool {
        let mut left = Duration::from_secs_f64(gap_ms / 1000.0);
        let mut done = Duration::ZERO;
        loop {
            if ctl.stopped() || ctl.seek_pending() {
                return false;
            }
            if ctl.paused.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                continue;
            }
            if left.is_zero() {
                return true;
            }
            let speed = ctl.speed.load(Ordering::Relaxed).max(1);
            let wall = TICK.min(left / speed);
            let wall = if wall.is_zero() { left / speed } else { wall };
            std::thread::sleep(wall);
            let advanced = (wall * speed).min(left);
            left -= advanced;
            done += advanced;
            ctl.pos_ms.store((base_ms + done.as_secs_f64() * 1000.0) as u64, Ordering::Relaxed);
        }
    }
}

fn screen_text(term: &Term<EventProxy>) -> Vec<String> {
    let grid = term.grid();
    (0..term.screen_lines())
        .map(|i| {
            let row = &grid[Line(i as i32)];
            let mut line: String = (0..term.columns())
                .map(|c| &row[Column(c)])
                .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|cell| if cell.c == '\0' { ' ' } else { cell.c })
                .collect();
            line.truncate(line.trim_end().len());
            line
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct Transcript {
    committed: Vec<String>,
    screen: Vec<String>,
}

impl Transcript {
    fn screen_used(&self) -> usize {
        self.screen.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1)
    }

    pub(crate) fn lines(&self) -> impl Iterator<Item = &String> {
        self.committed.iter().chain(self.screen[..self.screen_used()].iter())
    }

    pub(crate) fn len(&self) -> usize {
        self.committed.len() + self.screen_used()
    }

    fn merge(&mut self, next: Vec<String>) {
        if next == self.screen {
            return;
        }
        match scrolled_off(&self.screen, &next) {
            Some(k) => self.committed.extend(self.screen.drain(..k)),
            None => {
                let kept = self.screen.len() - common_footer(&self.screen, &next);
                let mut gone: Vec<String> = self.screen.drain(..kept).collect();
                gone.truncate(gone.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1));
                self.committed.extend(gone);
            }
        }
        self.screen = next;
    }
}

fn common_footer(prev: &[String], next: &[String]) -> usize {
    let prev = &prev[..prev.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1)];
    let next = &next[..next.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1)];
    prev.iter().rev().zip(next.iter().rev()).take_while(|(a, b)| a == b).count()
}

fn scrolled_off(prev: &[String], next: &[String]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for k in 0..prev.len() {
        let run = prev[k..].iter().zip(next).take_while(|(a, b)| a == b).count();
        let meaningful = prev[k..k + run].iter().filter(|l| !l.is_empty()).count();
        if meaningful < MIN_OVERLAP.min(next.len()) {
            continue;
        }
        if best.is_none_or(|(r, _)| run > r) {
            best = Some((run, k));
        }
    }
    best.map(|(_, k)| k)
}

#[cfg(test)]
mod tests {
    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keeps_lines_that_scroll_off_and_drops_redrawn_footers() {
        let mut t = Transcript::default();
        t.merge(lines(&["a", "b", "c", "d", "> input 1"]));
        t.merge(lines(&["a", "b", "c", "d", "> input 2"]));
        t.merge(lines(&["c", "d", "e", "f", "> input 3"]));
        let all: Vec<&String> = t.lines().collect();
        assert_eq!(all, lines(&["a", "b", "c", "d", "e", "f", "> input 3"]).iter().collect::<Vec<_>>());
    }

    #[test]
    fn drops_blank_screens_and_a_footer_that_stays() {
        let mut t = Transcript::default();
        t.merge(lines(&["", "", ""]));
        t.merge(lines(&["one", "", "> box", "status"]));
        t.merge(lines(&["two", "three", "> box", "status"]));
        let all: Vec<&String> = t.lines().collect();
        assert_eq!(all, lines(&["one", "two", "three", "> box", "status"]).iter().collect::<Vec<_>>());
    }

    #[test]
    fn keeps_the_whole_screen_when_it_is_replaced() {
        let mut t = Transcript::default();
        t.merge(lines(&["one", "two", "three"]));
        t.merge(lines(&["x", "y", "z"]));
        let all: Vec<&String> = t.lines().collect();
        assert_eq!(all, lines(&["one", "two", "three", "x", "y", "z"]).iter().collect::<Vec<_>>());
    }

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

    #[test]
    fn measures_capped_duration_across_parts() {
        let dir = std::env::temp_dir().join(format!("lust_termina_measure_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let header = "{\"version\": 2, \"width\": 10, \"height\": 2}\n";
        let a = dir.join("a.cast");
        let b = dir.join("b.cast");
        std::fs::write(&a, format!("{header}[0.5, \"o\", \"x\"]\n[10.5, \"o\", \"y\"]\n")).expect("write");
        std::fs::write(&b, format!("{header}[1.0, \"o\", \"z\"]\n")).expect("write");
        let total = measure(&[a, b], Duration::from_secs(2)).expect("measure");
        assert_eq!(total, 500 + 2000 + 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_event_time_without_parsing_data() {
        assert_eq!(event_time("[12.5, \"o\", \"[,]\"]"), Some(12.5));
        assert_eq!(event_time("{\"version\": 2}"), None);
    }
}
