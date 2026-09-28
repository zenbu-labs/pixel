use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const CAPACITY: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub seq: u64,
    pub epoch_ms: f64,
    pub level: LogLevel,
    pub target: &'static str,
    pub message: String,
}

struct LogStore {
    next_seq: u64,
    entries: VecDeque<LogEntry>,
}

static LOGS: Mutex<LogStore> = Mutex::new(LogStore {
    next_seq: 0,
    entries: VecDeque::new(),
});

pub fn log(level: LogLevel, target: &'static str, message: impl Into<String>) {
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
    let message = message.into();
    if level != LogLevel::Debug {
        append_to_file(&entry_json(epoch_ms, level, target, &message));
    }
    let Ok(mut store) = LOGS.lock() else {
        return;
    };
    let seq = store.next_seq;
    store.next_seq += 1;
    if store.entries.len() >= CAPACITY {
        store.entries.pop_front();
    }
    store.entries.push_back(LogEntry {
        seq,
        epoch_ms,
        level,
        target,
        message,
    });
}

/// One file per process under the pixel state dir, so tools outside the app (the cpu
/// monitor) can follow what the engine is doing without opening devtools.
fn log_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("pixel/logs"))
}

fn append_to_file(line: &str) {
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let dir = log_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        remove_dead_logs(&dir);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{}.jsonl", std::process::id())))
            .ok()?;
        Some(Mutex::new(file))
    });
    if let Some(file) = file
        && let Ok(mut file) = file.lock()
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn remove_dead_logs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|pid| pid.parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw)
        else {
            continue;
        };
        if rustix::process::test_kill_process(pid).is_err() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn entry_json(epoch_ms: f64, level: LogLevel, target: &str, message: &str) -> String {
    let mut line = serde_json::json!({
        "t": epoch_ms,
        "level": level.as_str(),
        "target": target,
        "message": message,
    })
    .to_string();
    line.push('\n');
    line
}

pub fn debug(target: &'static str, message: impl Into<String>) {
    log(LogLevel::Debug, target, message);
}

pub fn info(target: &'static str, message: impl Into<String>) {
    log(LogLevel::Info, target, message);
}

pub fn warn(target: &'static str, message: impl Into<String>) {
    log(LogLevel::Warn, target, message);
}

pub fn error(target: &'static str, message: impl Into<String>) {
    log(LogLevel::Error, target, message);
}

pub fn entries_after(after: u64) -> Vec<LogEntry> {
    let Ok(store) = LOGS.lock() else {
        return Vec::new();
    };
    store
        .entries
        .iter()
        .filter(|e| e.seq >= after)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_entries_are_sequenced_and_drainable() {
        info("test-a", "first");
        warn("test-a", "second");
        let all = entries_after(0);
        let ours: Vec<_> = all.iter().filter(|e| e.target == "test-a").collect();
        assert!(ours.len() >= 2);
        assert!(ours[0].seq < ours[1].seq);
        let after = entries_after(ours[1].seq + 1);
        assert!(after.iter().all(|e| e.target != "test-a"));
    }
}
