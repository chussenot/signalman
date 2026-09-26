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
//! Set one per client with [`crate::client::ClientBuilder::observer`], or
//! once per process with [`set_global`]. Token usage goes to the client's own
//! observer when it has one and to the global one otherwise, the way
//! `tracing` falls back to its global subscriber. Failed attempts always go
//! to the global one: the retry loop in [`crate::http`] is shared with
//! clients that carry no observer, so it has no client to ask. The loop
//! reports every failed attempt by status, and the TypeSafe client also
//! reports a 2xx it could not use, which the loop saw as a success: one
//! whose body did not decode, or one that did not answer the questions it
//! was sent.

use std::sync::{Arc, OnceLock};

use crate::answer::Usage;

/// What the client and the retry loop report.
pub trait Observer: Send + Sync + 'static {
    /// One response arrived: its model name and token usage.
    fn on_usage(&self, model: &str, usage: &Usage) {
        let _ = (model, usage);
    }

    /// One attempt against `service` failed with `status`: an HTTP status
    /// code, `transport` when no response came back, `too_large` when the
    /// body was over the policy's cap, or, from the TypeSafe client,
    /// `decode` (a 2xx whose body did not decode) or `unfit` (a 2xx that did
    /// not fit the questions sent, [`crate::Response::verify`]). Retried
    /// attempts are reported too; they are load whether or not a later
    /// attempt succeeds. `too_large`, `decode` and `unfit` are never
    /// retried, and a `unfit` response's usage is reported through
    /// [`Observer::on_usage`] as well, since it was billed.
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
