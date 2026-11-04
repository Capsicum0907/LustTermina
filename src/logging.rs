use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, TryLockError};

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::config::LogSettings;
use crate::winintegration::config_dir;

const LOG_DIR: &str = "logs";
const LOG_FILE: &str = "lust_termina.log";
const ROTATED_SUFFIX: &str = ".1";
const OWN_CRATE: &str = env!("CARGO_CRATE_NAME");

struct FileLogger {
    file: Mutex<Option<File>>,
    path: PathBuf,
    max_bytes: u64,
    own: LevelFilter,
    deps: LevelFilter,
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

pub(crate) fn log_dir() -> Option<PathBuf> {
    Some(config_dir()?.join(LOG_DIR))
}

fn parse_level(s: &str, fallback: LevelFilter) -> LevelFilter {
    s.trim().parse().unwrap_or(fallback)
}

fn open_append(path: &PathBuf) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

pub(crate) fn init(settings: &LogSettings) {
    let defaults = LogSettings::default();
    let Some(dir) = log_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(LOG_FILE);
    let logger = FileLogger {
        file: Mutex::new(open_append(&path)),
        path,
        max_bytes: settings.max_kb.max(1).saturating_mul(1024),
        own: parse_level(&settings.level, parse_level(&defaults.level, LevelFilter::Info)),
        deps: parse_level(&settings.deps_level, parse_level(&defaults.deps_level, LevelFilter::Warn)),
    };
    let max = logger.own.max(logger.deps);
    if LOGGER.set(logger).is_err() {
        return;
    }
    if let Some(l) = LOGGER.get() {
        if log::set_logger(l).is_ok() {
            log::set_max_level(max);
        }
    }
    install_panic_hook();
}

pub(crate) fn close() {
    if let Some(l) = LOGGER.get() {
        let mut guard = match l.file.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *guard = None;
    }
}

fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

fn thread_label() -> String {
    let cur = std::thread::current();
    match cur.name() {
        Some(n) => n.to_string(),
        None => format!("{:?}", cur.id()),
    }
}

fn format_line(level: Level, target: &str, args: &Arguments) -> String {
    format!(
        "{} [{}] [{}] {:<5} {}: {}\n",
        timestamp(),
        std::process::id(),
        thread_label(),
        level,
        target,
        args
    )
}

impl FileLogger {
    fn filter_for(&self, target: &str) -> LevelFilter {
        let own = target == OWN_CRATE
            || target
                .strip_prefix(OWN_CRATE)
                .is_some_and(|rest| rest.starts_with("::"));
        if own { self.own } else { self.deps }
    }

    fn rotate_if_needed(&self, slot: &mut Option<File>, incoming: usize) {
        let Some(f) = slot.as_ref() else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len + incoming as u64 <= self.max_bytes {
            return;
        }
        *slot = None;
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(ROTATED_SUFFIX);
        let _ = std::fs::rename(&self.path, PathBuf::from(rotated));
        *slot = open_append(&self.path);
    }

    fn write_line(&self, line: &str) {
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        self.rotate_if_needed(&mut guard, line.len());
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn write_emergency(&self, line: &str) {
        match self.file.try_lock() {
            Ok(mut g) => {
                if let Some(f) = g.as_mut() {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            Err(TryLockError::Poisoned(p)) => {
                if let Some(f) = p.into_inner().as_mut() {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            Err(TryLockError::WouldBlock) => {
                if let Some(mut f) = open_append(&self.path) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.filter_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.write_line(&format_line(record.level(), record.target(), record.args()));
    }

    fn flush(&self) {
        if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(l) = LOGGER.get() {
            let payload = info.payload();
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());
            let backtrace = std::backtrace::Backtrace::force_capture();
            let line = format_line(
                Level::Error,
                "panic",
                &format_args!("panicked at {location}: {message}\nbacktrace:\n{backtrace}"),
            );
            l.write_emergency(&line);
        }
        previous(info);
    }));
}
