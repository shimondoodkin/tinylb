//! Log throttling for errors that repeat on every request.
//!
//! When a backend breaks (crashed, disk full, refusing connections) every
//! client request fails the same way. Logging each failure floods the log —
//! WebSocket clients in a reconnect loop can produce thousands of lines per
//! second. An `ErrorThrottle` lets the first error through, counts the
//! repeats, and lets one line (carrying the suppressed count) through per
//! window. When requests start succeeding again it reports recovery once.
//!
//! The window is global and hot-reloadable via `[log] error_summary_secs`.
//! A window of 0 disables throttling (every error is logged).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

pub const DEFAULT_WINDOW_SECS: u64 = 60;

static WINDOW_MS: AtomicU64 = AtomicU64::new(DEFAULT_WINDOW_SECS * 1000);

/// Set the summary window for all throttles. 0 = log every error.
pub fn set_window_secs(secs: u64) {
    WINDOW_MS.store(secs.saturating_mul(1000), Ordering::Relaxed);
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    // +1 so a real timestamp is never 0 (0 means "never logged").
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

pub struct ErrorThrottle {
    /// An error was logged and no success has been seen since.
    in_error: AtomicBool,
    /// Time (now_ms) of the last logged error; 0 = never.
    last_log_ms: AtomicU64,
    /// Errors suppressed since the last logged line.
    suppressed: AtomicU64,
}

impl ErrorThrottle {
    pub const fn new() -> Self {
        Self {
            in_error: AtomicBool::new(false),
            last_log_ms: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// Record an error. Returns `Some(suppressed)` if this error should be
    /// logged (with `suppressed` similar errors swallowed since the last
    /// logged one), or `None` if it should be dropped.
    pub fn on_error(&self) -> Option<u64> {
        self.on_error_at(now_ms(), WINDOW_MS.load(Ordering::Relaxed))
    }

    /// Record a success. Returns `Some(suppressed)` once when recovering
    /// from a logged error state, so the caller can log "recovered".
    pub fn on_success(&self) -> Option<u64> {
        // Fast path: a single relaxed load on every successful request.
        if !self.in_error.load(Ordering::Relaxed) {
            return None;
        }
        if self.in_error.swap(false, Ordering::Relaxed) {
            Some(self.suppressed.swap(0, Ordering::Relaxed))
        } else {
            None
        }
    }

    fn on_error_at(&self, now: u64, window: u64) -> Option<u64> {
        if window == 0 {
            self.in_error.store(true, Ordering::Relaxed);
            return Some(0);
        }
        let last = self.last_log_ms.load(Ordering::Relaxed);
        if (last == 0 || now.saturating_sub(last) >= window)
            && self
                .last_log_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.in_error.store(true, Ordering::Relaxed);
            return Some(self.suppressed.swap(0, Ordering::Relaxed));
        }
        self.suppressed.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// Log an error through a throttle. Usage:
/// `throttled!(throttle, error, backend = %url, error = %e, "Failed ...");`
/// Adds a `suppressed` field when repeats were swallowed.
#[macro_export]
macro_rules! throttled {
    ($throttle:expr, $level:ident, $($args:tt)+) => {
        if let Some(suppressed) = $throttle.on_error() {
            if suppressed > 0 {
                tracing::$level!(suppressed, $($args)+);
            } else {
                tracing::$level!($($args)+);
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u64 = 60_000;

    #[test]
    fn first_error_is_logged() {
        let t = ErrorThrottle::new();
        assert_eq!(t.on_error_at(1, W), Some(0));
    }

    #[test]
    fn repeats_within_window_are_suppressed_and_counted() {
        let t = ErrorThrottle::new();
        assert_eq!(t.on_error_at(1, W), Some(0));
        for i in 0..1000 {
            assert_eq!(t.on_error_at(2 + i, W), None);
        }
        // Next window: logged, carrying the count.
        assert_eq!(t.on_error_at(1 + W, W), Some(1000));
        assert_eq!(t.on_error_at(2 + W, W), None);
    }

    #[test]
    fn zero_window_logs_everything() {
        let t = ErrorThrottle::new();
        for i in 0..10 {
            assert_eq!(t.on_error_at(1 + i, 0), Some(0));
        }
    }

    #[test]
    fn success_without_error_reports_nothing() {
        let t = ErrorThrottle::new();
        assert_eq!(t.on_success(), None);
    }

    #[test]
    fn recovery_reported_once_with_suppressed_count() {
        let t = ErrorThrottle::new();
        t.on_error_at(1, W);
        t.on_error_at(2, W);
        t.on_error_at(3, W);
        assert_eq!(t.on_success(), Some(2));
        assert_eq!(t.on_success(), None);
    }

    #[test]
    fn flapping_backend_stays_throttled() {
        // Alternating success/failure must not log every failure.
        let t = ErrorThrottle::new();
        assert_eq!(t.on_error_at(1, W), Some(0));
        assert_eq!(t.on_success(), Some(0)); // one recovery line
        for i in 0..100 {
            assert_eq!(t.on_error_at(10 + i, W), None);
            assert_eq!(t.on_success(), None);
        }
    }
}
