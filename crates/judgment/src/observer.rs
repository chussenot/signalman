//! The seam between this crate and an application's metrics.
//!
//! A library should emit `tracing` spans and nothing else: it must not pick a
//! metrics backend for the application that embeds it. But the two numbers an
//! application always wants from a decision model, tokens per response and
//! failed attempts per upstream, are only known inside the client and the
//! retry loop. [`Observer`] hands them out; the application counts them where
//! it counts everything else. The default is a no-op, so nothing changes for a
//! caller that does not care.
//!
//! Set one per client with [`crate::ClientBuilder::observer`], or once per
//! process with [`set_global`]: a client without its own observer reports to
//! the global one, the way `tracing` falls back to its global subscriber.

use std::sync::{Arc, OnceLock};

use crate::answer::Usage;

/// What the client and the retry loop report.
pub trait Observer: Send + Sync + 'static {
    /// One response arrived: its model name and token usage.
    fn on_usage(&self, model: &str, usage: &Usage) {
        let _ = (model, usage);
    }

    /// One attempt against `service` failed with `status`: an HTTP status
    /// code, or `transport` when no response came back. Retried attempts are
    /// reported too; they are load whether or not a later attempt succeeds.
    fn on_failed_attempt(&self, service: &'static str, status: &str) {
        let _ = (service, status);
    }
}

/// Reports nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {}

static GLOBAL: OnceLock<Arc<dyn Observer>> = OnceLock::new();

/// Install the process-wide observer. The first call wins; later calls are
/// ignored and return `false`, so an application installs it once at start-up
/// and a library never overrides it.
pub fn set_global(observer: Arc<dyn Observer>) -> bool {
    GLOBAL.set(observer).is_ok()
}

/// The process-wide observer: the one installed by [`set_global`], or a
/// no-op.
pub fn global() -> Arc<dyn Observer> {
    static NOOP: OnceLock<Arc<dyn Observer>> = OnceLock::new();
    GLOBAL
        .get()
        .unwrap_or_else(|| NOOP.get_or_init(|| Arc::new(NoopObserver)))
        .clone()
}
