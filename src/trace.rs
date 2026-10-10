//! Lightweight always-on diagnostic trace.
//!
//! OpenDeck does not reliably capture this plugin's stdout/stderr (the
//! plugin log file stays empty), so runtime diagnostics are written to a
//! dedicated file: `%TEMP%\rs-ulanzi-d200-trace.log` (truncated on start).
//! Every HID report, forwarded key event, action trigger and outgoing
//! small-window frame is timestamped there, which makes "who sent what,
//! when" questions answerable without attaching a debugger.

use std::fmt::Arguments;
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Open (and truncate) the trace file. Called once at startup.
pub fn init() {
    let _ = LOG.get_or_init(|| {
        let path = std::env::temp_dir().join("rs-ulanzi-d200-trace.log");
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            Ok(f) => Some(Mutex::new(f)),
            Err(e) => {
                eprintln!("trace: cannot open {}: {}", path.display(), e);
                None
            }
        }
    });
}

/// Append one timestamped line. Silently a no-op if the file is unavailable.
pub fn logf(args: Arguments<'_>) {
    let Some(Some(file)) = LOG.get() else { return };
    let ts = chrono::Local::now().format("%H:%M:%S%.3f");
    let Ok(mut guard) = file.lock() else { return };
    let _ = writeln!(guard, "[{}] {}", ts, args);
    let _ = guard.flush();
}
