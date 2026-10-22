//! The router's **compile-time event catalogue** for the sidecar log plane —
//! the complete set of events this binary may report to nest's admin Logs
//! surface, auditable in one read.
//!
//! Authority: `docs/goal/architecture/apps/observability.md` § The sidecar
//! log plane. The queue/flush/disable machinery is shared
//! (`fauna_sidecar_client::log_plane`); this module is only the catalogue and
//! its message templates.
//!
//! **Why these five.** They are what an admin *acts on* at the box's front door:
//! the router serving, a backend it cannot reach, the two connection caps
//! shedding, and the router losing its own link to nest. Per-connection traffic
//! is not plane material; it stays in the router's stderr, which the container
//! log stream already captures.
//!
//! **No `listener_bind_failed`.** A failed `:443` bind ends the process before
//! any channel to nest could exist, and with `:443` unbound the admin's app has
//! no path to the Logs page anyway; the bind error stays on stderr, where s6's
//! restart loop and the container log stream carry it.
//!
//! **Counts, aggregated where the stderr line is.** The shed and
//! backend-failure events are emitted where the router's rate-limited stderr
//! warn fires (`fauna_conn_limit::ShedCounter`, one line per
//! `SHED_LOG_INTERVAL`), carrying the same batch count — so a flood costs one
//! plane event per minute per counter, and the plane and stderr agree.
//!
//! **Flush cadence: a timer of its own** ([`FLUSH_INTERVAL`]), the mail
//! bridge's shape, not the relay's. The relay flushes whenever its channel
//! does work; the router's channel carries nothing but this plane, so "when
//! the channel does work" would never come.
//!
//! **The interpolation rule, concretely.** Every message below is a constant
//! template whose only interpolations are a **port**, a **count**, or a
//! **backend name** — the route's leading DNS label (`mail`, `relay`, `pds`),
//! `nest` for the default route, or `exact-route` — all artifact-set, never the
//! dialled address and never anything a client sent.

use std::time::Duration;

use fauna_sidecar_client::log_plane::{PlaneLevel, emit};

/// How often the router sends what it has queued while its channel is up.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(10);

/// The router is bound and serving.
pub fn serving(port: u16) {
    emit(
        PlaneLevel::Info,
        "serving",
        format!("router serving on port {port}"),
    );
}

/// Connections routed to `backend` failed to connect to it — `count` of them
/// since the last report.
pub fn backend_unreachable(backend: &str, count: u64) {
    emit(
        PlaneLevel::Warn,
        "backend_unreachable",
        format!("router could not reach the {backend} backend ({count} connections)"),
    );
}

/// The global connection cap shed `count` new connections since the last report.
pub fn connection_cap_shed(count: u64) {
    emit(
        PlaneLevel::Warn,
        "connection_cap_shed",
        format!("router connection cap reached; shed {count} connections"),
    );
}

/// The per-source-IP cap shed `count` new connections since the last report,
/// summed across every source it metered.
pub fn per_ip_cap_shed(count: u64) {
    emit(
        PlaneLevel::Warn,
        "per_ip_cap_shed",
        format!("router per-IP connection cap reached; shed {count} connections"),
    );
}

/// The router could not reach nest to hold its channel. Routing is unaffected;
/// this plane's events wait in the bounded queue until the channel is back.
pub fn nest_unreachable() {
    emit(
        PlaneLevel::Warn,
        "nest_unreachable",
        "router cannot reach nest; its log events wait until it can".to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: [&str; 5] = [
        "serving",
        "backend_unreachable",
        "connection_cap_shed",
        "per_ip_cap_shed",
        "nest_unreachable",
    ];

    /// The catalogue's event ids must satisfy nest's admission charset
    /// (`[a-z0-9_.-]`, ≤64 bytes) — a violation is dropped at admission, i.e. a
    /// silently missing event.
    #[test]
    fn every_catalogued_event_id_passes_nest_admission() {
        for id in EVENTS {
            assert!(!id.is_empty());
            assert!(id.len() <= fauna_protocol::log_plane::MAX_EVENT_BYTES);
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '_' | '.' | '-')),
                "{id} would be rejected by nest admission"
            );
        }
    }

    /// Messages must stay under the admission cap so nest never truncates one,
    /// rendered with worst-case interpolations.
    #[test]
    fn catalogued_messages_fit_under_the_admission_cap() {
        let longest_backend = "x".repeat(63); // a DNS label's ceiling
        serving(u16::MAX);
        backend_unreachable(&longest_backend, u64::MAX);
        connection_cap_shed(u64::MAX);
        per_ip_cap_shed(u64::MAX);
        nest_unreachable();
        for message in [
            format!("router serving on port {}", u16::MAX),
            format!(
                "router could not reach the {longest_backend} backend ({} connections)",
                u64::MAX
            ),
            format!(
                "router connection cap reached; shed {} connections",
                u64::MAX
            ),
            format!(
                "router per-IP connection cap reached; shed {} connections",
                u64::MAX
            ),
        ] {
            assert!(message.len() <= fauna_protocol::log_plane::MAX_MESSAGE_BYTES);
        }
    }
}
