//! In-app log buffer for display in the Logs panel (instead of println/eprintln).

use std::sync::Mutex;

const MAX_LOGS: usize = 2000;

static LOG_BUF: std::sync::OnceLock<Mutex<Vec<LogEntry>>> = std::sync::OnceLock::new();

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub message: String,
}

fn buf() -> &'static Mutex<Vec<LogEntry>> {
    LOG_BUF.get_or_init(|| Mutex::new(Vec::new()))
}

/// Append a log line. Safe to call from any thread (e.g. from async fetch).
pub fn app_log(level: &str, message: impl Into<String>) {
    let entry = LogEntry {
        time: chrono::Utc::now().format("%H:%M:%S%.3f").to_string(),
        level: level.to_string(),
        message: message.into(),
    };
    if let Ok(mut v) = buf().lock() {
        v.push(entry);
        let n = v.len();
        if n > MAX_LOGS {
            v.drain(0..n - MAX_LOGS);
        }
    }
}

/// Take a snapshot of current logs for display. Call from UI.
pub fn app_logs_snapshot() -> Vec<LogEntry> {
    buf().lock().map(|v| v.clone()).unwrap_or_default()
}
