//! The shared POSIX shutdown-signal future: SIGTERM (service/launchd stop) or
//! SIGINT (foreground Ctrl-C) — the shutdown future both the standalone
//! `fauna-nest` binary (`main.rs`) and the macOS `fauna-nest-daemon`
//! `LaunchDaemon` (`bins/fauna-nest-daemon/src/main.rs::macos::shutdown_signal`,
//! which now delegates here) wait on. **POSIX-only by construction, not a
//! cross-platform abstraction**: Windows resolves its own SCM-driven shutdown
//! future entirely separately, matching `desktop_serve.rs`'s explicit
//! caller-supplied-per-OS design (that module deliberately does not own any
//! platform's shutdown-future construction — this one just gives the two
//! POSIX callers a single copy of theirs instead of two hand-duplicated ones).

/// Wait for SIGTERM or SIGINT. Panics if the SIGTERM handler cannot be
/// registered — an environment fault neither pre-existing copy this unifies
/// attempted to recover from, so panicking here preserves both callers'
/// existing behavior rather than picking a new posture unilaterally.
pub async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM, shutting down gracefully");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT, shutting down gracefully");
        }
    }
}
