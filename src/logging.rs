//! File-logging facility for the TUI and the headless `agent` commands.
//!
//! A TUI owns stdout/stderr (the terminal), so debug output goes to a file
//! instead, off by default. Two switches open a sink (see [`crate::cli`]): the
//! `MUDPUPPY_LOG` env var (an explicit single file) and the `mudpuppy.debug_log`
//! config setting (a directory, split per role). Logging is a cheap no-op until
//! a sink is installed, and a failed write is swallowed rather than allowed to
//! break the app.
//!
//! Logs must never record review content. Privacy-sensitive strings (branch
//! names, file paths) go through [`hash`] — a salted, non-reversible short
//! digest — so occurrences can be correlated without the value ever hitting
//! disk. The salt is the per-session [`SEED`], rotated by `agent reset`.
//!
//! Sink resolution has two layers so tests stay isolated under `cargo test`'s
//! thread-parallel execution: a process-global sink installed once by the
//! binary, and a per-thread capture buffer (see [`capture`]) that, when set,
//! redirects only the current thread's log writes into memory.

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use jiff::Timestamp;
use sha2::{Digest, Sha256};

/// Log severity, rendered as the line's leading tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Process-global sink, installed once by the binary via [`init_file`].
static GLOBAL: OnceLock<Mutex<Box<dyn Write + Send>>> = OnceLock::new();

/// Process-global hash seed for [`hash`]. Set from the store's `log_seed` once
/// the session's state is loaded; rotated by `agent reset` so a new round's
/// hashed labels don't correlate with the previous one's. Empty until set,
/// which still obscures the input — it just doesn't rotate.
static SEED: RwLock<String> = RwLock::new(String::new());

/// Install the hash seed used by [`hash`]. Idempotent: callers set it from the
/// store's `log_seed` after loading or creating the state.
pub fn set_seed(seed: &str) {
    if let Ok(mut s) = SEED.write() {
        if *s != seed {
            *s = seed.to_string();
        }
    }
}

/// A short, stable, non-reversible label for a privacy-sensitive string (a
/// branch name, a file path) so logs can correlate occurrences without ever
/// recording the value itself. The digest is salted with the per-session
/// [`SEED`], so the same input logs differently across sessions and after a
/// reset. Returns a 12-hex-char prefix of `sha256(seed ‖ 0x1f ‖ value)`.
pub fn hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    if let Ok(seed) = SEED.read() {
        hasher.update(seed.as_bytes());
    }
    // A separator byte that can't appear in the seed/value boundary keeps
    // `hash("a", "b")` distinct from `hash("ab", "")`.
    hasher.update([0x1f]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(12);
    for byte in &digest[..6] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

thread_local! {
    /// Per-thread capture buffer. When `Some`, the current thread's log writes
    /// go here instead of the global sink — this is what keeps parallel tests
    /// from seeing each other's output.
    static CAPTURE: RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { RefCell::new(None) };
}

/// Open `path` for append (creating it if missing) and install it as the global
/// sink. Has no effect on subsequent calls: the global sink is set once.
pub fn init_file(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    // `set` only succeeds the first time; a later call is a harmless no-op.
    let _ = GLOBAL.set(Mutex::new(Box::new(file)));
    Ok(())
}

/// Format and emit one log line. Routes to the thread-local capture buffer if
/// one is installed, else the global sink, else nothing. Write errors are
/// intentionally swallowed: logging must never break the app.
pub fn write_log(level: Level, args: std::fmt::Arguments) {
    let captured = CAPTURE.with(|c| c.borrow().clone());
    if let Some(buf) = captured {
        if let Ok(mut buf) = buf.lock() {
            let _ = writeln!(buf, "{}  {}  {}", Timestamp::now(), level.label(), args);
        }
        return;
    }
    if let Some(sink) = GLOBAL.get() {
        if let Ok(mut sink) = sink.lock() {
            let _ = writeln!(sink, "{}  {}  {}", Timestamp::now(), level.label(), args);
        }
    }
}

/// Captures the current thread's logs into an in-memory buffer for assertions.
/// Installing the guard redirects this thread's log writes away from the global
/// sink; dropping it restores the previous override.
pub struct CaptureGuard {
    buffer: Arc<Mutex<Vec<u8>>>,
    // The override that was in place before this guard, restored on drop so
    // nested captures behave.
    previous: Option<Arc<Mutex<Vec<u8>>>>,
}

impl CaptureGuard {
    /// The text logged on this thread since the guard was installed.
    pub fn contents(&self) -> String {
        self.buffer
            .lock()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE.with(|c| *c.borrow_mut() = self.previous.take());
    }
}

/// Install a fresh capture buffer on the current thread. See [`CaptureGuard`].
pub fn capture() -> CaptureGuard {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let previous = CAPTURE.with(|c| c.borrow_mut().replace(buffer.clone()));
    CaptureGuard { buffer, previous }
}

/// Log at `DEBUG`.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logging::write_log($crate::logging::Level::Debug, format_args!($($arg)*))
    };
}

/// Log at `INFO`.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::write_log($crate::logging::Level::Info, format_args!($($arg)*))
    };
}

/// Log at `WARN`.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::write_log($crate::logging::Level::Warn, format_args!($($arg)*))
    };
}

/// Log at `ERROR`.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::write_log($crate::logging::Level::Error, format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_sees_own_messages() {
        let logs = capture();
        crate::log_info!("hello world");
        assert!(logs.contents().contains("hello world"));
        assert!(logs.contents().contains("INFO"));
    }

    #[test]
    fn no_op_without_capture_does_not_panic() {
        // No capture guard and (in tests) no global sink: must be a silent
        // no-op, never a panic.
        crate::log_debug!("goes nowhere");
    }

    #[test]
    fn each_macro_tags_its_level() {
        let logs = capture();
        crate::log_debug!("d");
        crate::log_info!("i");
        crate::log_warn!("w");
        crate::log_error!("e");
        let out = logs.contents();
        assert!(out.contains("DEBUG  d"));
        assert!(out.contains("INFO  i"));
        assert!(out.contains("WARN  w"));
        assert!(out.contains("ERROR  e"));
    }

    #[test]
    fn macros_accept_format_args() {
        let logs = capture();
        crate::log_info!("x = {}", 5);
        assert!(logs.contents().contains("x = 5"));
    }

    #[test]
    fn hash_is_stable_short_hex_and_salted() {
        set_seed("seed-one");
        let a = hash("refs/heads/feature");
        // Stable for the same (seed, value).
        assert_eq!(a, hash("refs/heads/feature"));
        // 12 lowercase hex chars, and never the cleartext.
        assert_eq!(a.len(), 12);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert!(!a.contains("feature"));
        // Different inputs differ; a new seed re-salts the same input.
        assert_ne!(a, hash("refs/heads/other"));
        set_seed("seed-two");
        assert_ne!(a, hash("refs/heads/feature"));
    }

    #[test]
    fn capture_is_isolated_from_earlier_writes() {
        // A write made before a guard exists must not appear in that guard:
        // the guard only collects what was logged while it was installed.
        crate::log_warn!("before guard");
        let logs = capture();
        crate::log_warn!("after guard");
        let out = logs.contents();
        assert!(out.contains("after guard"));
        assert!(!out.contains("before guard"));
    }
}
