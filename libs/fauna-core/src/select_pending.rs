//! A `tokio::select!` arm that goes quiet when its resource is absent, instead
//! of a per-caller `match` on `None => pend forever`.
//!
//! `select!` has no way to omit a branch at runtime — an `Option`-gated
//! resource (a nudge channel only wired by some callers, a command channel
//! that closes when its owner drops) needs its `None` case to simply never
//! win. [`std::future::pending`] never resolves, so matching `None` to it
//! makes exactly that arm permanently silent while the other arms keep
//! running. This one shape was hand-copied at three optional-`mpsc::Receiver`
//! `select!` sites across `fauna-sync-engine` and `fauna-nest` (found by the
//! dev-fleet near-duplicate-function scanner's cross-crate containment pass).

use tokio::sync::mpsc;

/// Receive from `rx` when present; pend forever (never win a `select!`) when
/// absent.
///
/// Cancel-safe: both `mpsc::Receiver::recv` and `std::future::pending` are, so
/// dropping this future — as `select!` does to every losing arm — can never
/// lose a value it had already taken.
pub async fn recv_or_pending<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}
