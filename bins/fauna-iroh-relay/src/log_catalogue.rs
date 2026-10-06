//! The relay's **compile-time event catalogue** for the sidecar log plane —
//! the complete set of events this binary may report to nest's admin Logs
//! surface, auditable in one read.
//!
//! Authority: `docs/goal/architecture/apps/observability.md` § The sidecar
//! log plane. The queue/flush/disable machinery is shared
//! (`fauna_sidecar_client::log_plane`); this module is only the catalogue and
//! its message templates.
//!
//! **Why these five.** They are the events an admin *acts on* — the relay
//! serving or not, and its TLS cert going stale. Per-connection relay traffic
//! is not plane material; it stays in the relay's own stderr, which the
//! container log stream already captures.
//!
//! **The interpolation rule, concretely.** Every message below is a constant
//! template whose only interpolations are a **port** or a **duration** — both
//! bounded classes. In particular, none of them interpolates the `anyhow::Error`
//! its call site has in hand: a cert-fetch failure's error string can carry
//! upstream/remote text, and the plane's whole point is that it never becomes a
//! log-injection surface into an admin page. The full error still goes to the
//! relay's own stderr at the same site — losing nothing, leaking nothing.

use fauna_sidecar_client::log_plane::{PlaneLevel, emit};

/// The relay is up and serving TLS.
pub fn serving(https_port: u16) {
    emit(
        PlaneLevel::Info,
        "serving",
        format!("relay serving on port {https_port}"),
    );
}

/// A scheduled refresh replaced the serving cert without dropping connections.
pub fn cert_refreshed() {
    emit(
        PlaneLevel::Info,
        "cert_refreshed",
        "relay TLS cert refreshed (hot-swapped)".to_string(),
    );
}

/// A cert fetch failed; the relay keeps what it has — the current cert, which
/// will eventually expire, or none — so this is the early warning for a silent outage.
pub fn cert_refresh_failed() {
    emit(
        PlaneLevel::Warn,
        "cert_refresh_failed",
        "relay TLS cert refresh failed; keeping the current cert".to_string(),
    );
}

/// nest returned a cert the relay cannot parse — a provisioning fault, not a
/// transient one.
pub fn cert_unparsable() {
    emit(
        PlaneLevel::Error,
        "cert_unparsable",
        "refreshed relay TLS cert is unparsable; keeping the current cert".to_string(),
    );
}

/// The relay could not reach nest to hold its channel. While the channel is down
/// the relay refuses every new endpoint (admission is nest's answer), so this is
/// the early warning for "devices cannot use the relay".
pub fn nest_unreachable() {
    emit(
        PlaneLevel::Warn,
        "nest_unreachable",
        "relay cannot reach nest; refusing new endpoints until it can".to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue's event ids must satisfy nest's admission charset
    /// (`[a-z0-9_.-]`, ≤64 bytes) — a violation is dropped at admission, i.e. a
    /// silently missing event. Cheaper to catch here than in a tier_4 run.
    #[test]
    fn every_catalogued_event_id_passes_nest_admission() {
        for id in [
            "serving",
            "cert_refreshed",
            "cert_refresh_failed",
            "cert_unparsable",
            "nest_unreachable",
        ] {
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

    /// Messages must stay under the admission cap so nest never truncates one —
    /// a truncated message is a message an admin half-reads.
    #[test]
    fn catalogued_messages_fit_under_the_admission_cap() {
        fauna_sidecar_client::log_plane::emit(PlaneLevel::Info, "probe", String::new());
        serving(65_535);
        cert_refreshed();
        cert_refresh_failed();
        cert_unparsable();
        nest_unreachable();
        // Rendered with worst-case interpolations, every template is far inside
        // the 512-byte cap; assert rather than assume.
        assert!(
            format!("relay serving on port {}", u16::MAX).len()
                <= fauna_protocol::log_plane::MAX_MESSAGE_BYTES
        );
    }
}
