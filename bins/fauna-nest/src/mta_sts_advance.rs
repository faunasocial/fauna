//! The nest's own advance of a mail domain's stored MTA-STS mode from `testing`
//! to `enforce` (`docs/goal/behavior/mail-multidomain.md` § The advance).
//!
//! No human sets the mode. Every domain is stored `testing` at add; this pass
//! moves it to `enforce` once both hold:
//!
//! - **(a) the mail name is trusted** — the listener serves a CA-issued, covering
//!   certificate for `mail.<primary>`, the same `cert_health_state` reading the
//!   published-mode coupling uses (`tls-certificates.md` § D);
//! - **(b) the window has run out** — 7 days from the later of the domain's add
//!   and its last restore (`db::mail_domains::MTA_STS_TESTING_WINDOW_MS`).
//!
//! It runs on the nest's hourly maintenance tick (`main.rs`), whose first tick
//! fires at boot, so a restart never strands a domain. It is one-way and
//! idempotent: nothing here ever moves a domain back, and a lapsed certificate
//! lowers only what is *published* (`MtaStsMode::coupled_to_cert`), never the
//! stored mode.

use std::sync::Arc;

use crate::routes::AppState;

/// One advance pass at the wall clock. Returns the names of the domains moved.
pub async fn advance_mta_sts_modes(state: &Arc<AppState>) -> Vec<String> {
    advance_mta_sts_modes_at(state, crate::db::now_epoch_millis()).await
}

/// [`advance_mta_sts_modes`] with the clock as a parameter. The nest has no
/// clock seam and gets none for this: tests pass `now_ms`, production passes
/// the wall clock.
pub async fn advance_mta_sts_modes_at(state: &Arc<AppState>, now_ms: i64) -> Vec<String> {
    // (a) — every local domain MXes to the one `mail.<primary>` host, so the
    // whole deployment shares one trust reading. No primary, no resolver, the
    // floor, or a non-covering certificate all read as untrusted.
    let primary = match state.db.lookup_primary_mail_domain().await {
        Ok(Some(p)) => p,
        Ok(None) => return Vec::new(),
        Err(e) => {
            tracing::warn!("mta-sts advance: primary lookup failed: {e:#}");
            return Vec::new();
        }
    };
    let mx_host = format!("mail.{}", primary.domain_name);
    let mx_facts = state
        .served_cert_spki
        .as_ref()
        .and_then(|r| r.served_cert_facts(&mx_host));
    if matches!(
        crate::acme::cert_health_state(mx_facts, now_ms / 1000),
        fauna_protocol::tls::CertHealthState::OnFloorRenewNeeded
    ) {
        return Vec::new();
    }

    // (b) — the clock half, one idempotent statement.
    let advanced = match state.db.advance_mta_sts_testing_to_enforce_at(now_ms).await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!("mta-sts advance failed: {e:#}");
            return Vec::new();
        }
    };
    if !advanced.is_empty() {
        tracing::info!(
            domains = ?advanced,
            "MTA-STS: testing window over on a trusted mail name — stored mode advanced to enforce"
        );
        crate::bridge_routing_handlers::notify_bridges_config_changed(
            state,
            fauna_protocol::bridge_routing::config_change_reason::LOCAL_DOMAINS,
        )
        .await;
    }
    advanced
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::{ServedCertFacts, ServedCertSpki};
    use crate::db::CacheDb;
    use crate::db::mail_domains::MTA_STS_TESTING_WINDOW_MS;

    /// A [`ServedCertSpki`] double serving one canned reading for every name.
    struct Served(Option<ServedCertFacts>);
    impl ServedCertSpki for Served {
        fn current_spki_sha256(&self) -> Option<[u8; 32]> {
            None
        }
        fn served_cert_facts(&self, _sni: &str) -> Option<ServedCertFacts> {
            self.0
        }
        fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
            None
        }
    }

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    /// A certificate valid for a year either side of `now_ms`.
    fn facts(now_ms: i64, is_floor: bool) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: now_ms / 1000 - 365 * 86_400,
            not_after_unix: now_ms / 1000 + 365 * 86_400,
            is_floor,
            covers: true,
        }
    }

    /// A nest with one primary domain stored `testing`, serving `served` for the
    /// mail name. Returns the state and the domain's `added_at`.
    async fn nest(served: Option<ServedCertFacts>) -> (Arc<AppState>, i64) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let row = db
            .add_mail_domain("example.com", true, "testing", "expand_primary", None, None)
            .await
            .unwrap();
        let state = Arc::new(AppState {
            served_cert_spki: Some(Arc::new(Served(served))),
            ..AppState::for_test(db)
        });
        (state, row.added_at)
    }

    async fn stored_mode(state: &Arc<AppState>) -> String {
        state
            .db
            .lookup_active_mail_domain("example.com")
            .await
            .unwrap()
            .unwrap()
            .mta_sts_mode
    }

    #[tokio::test]
    async fn not_before_the_window() {
        let now = crate::db::now_epoch_millis();
        let (state, added_at) = nest(Some(facts(now, false))).await;
        let early = added_at + MTA_STS_TESTING_WINDOW_MS - 1;
        assert!(advance_mta_sts_modes_at(&state, early).await.is_empty());
        assert_eq!(stored_mode(&state).await, "testing");
    }

    #[tokio::test]
    async fn not_on_the_self_signed_floor() {
        let now = crate::db::now_epoch_millis();
        let (state, added_at) = nest(Some(facts(now, true))).await;
        let late = added_at + MTA_STS_TESTING_WINDOW_MS + DAY_MS;
        assert!(advance_mta_sts_modes_at(&state, late).await.is_empty());
        assert_eq!(stored_mode(&state).await, "testing");
    }

    #[tokio::test]
    async fn not_without_a_served_certificate() {
        let (state, added_at) = nest(None).await;
        let late = added_at + MTA_STS_TESTING_WINDOW_MS + DAY_MS;
        assert!(advance_mta_sts_modes_at(&state, late).await.is_empty());
        assert_eq!(stored_mode(&state).await, "testing");
    }

    #[tokio::test]
    async fn advances_when_trusted_and_past_the_window_and_is_idempotent() {
        let now = crate::db::now_epoch_millis();
        let (state, added_at) = nest(Some(facts(now, false))).await;
        let due = added_at + MTA_STS_TESTING_WINDOW_MS;
        assert_eq!(
            advance_mta_sts_modes_at(&state, due).await,
            vec!["example.com".to_string()]
        );
        assert_eq!(stored_mode(&state).await, "enforce");
        assert!(advance_mta_sts_modes_at(&state, due).await.is_empty());
    }

    /// The boot pass: a nest that was down while the window ran out advances on
    /// the first pass after it comes back — the pass reads only persisted rows
    /// and the served certificate, so "a boot after the window" is one late call.
    #[tokio::test]
    async fn a_boot_long_after_the_window_advances() {
        let now = crate::db::now_epoch_millis();
        let (state, added_at) = nest(Some(facts(now, false))).await;
        let boot = added_at + 30 * DAY_MS;
        assert_eq!(advance_mta_sts_modes_at(&state, boot).await.len(), 1);
        assert_eq!(stored_mode(&state).await, "enforce");
    }

    /// A certificate that lapses after the advance lowers the published mode
    /// only: the stored mode stays `enforce`, and no later pass moves it back.
    #[tokio::test]
    async fn a_lapsed_certificate_never_moves_the_stored_mode_back() {
        let now = crate::db::now_epoch_millis();
        let (trusted, added_at) = nest(Some(facts(now, false))).await;
        let due = added_at + MTA_STS_TESTING_WINDOW_MS;
        advance_mta_sts_modes_at(&trusted, due).await;

        // The same database, now behind the floor.
        let lapsed = Arc::new(AppState {
            served_cert_spki: Some(Arc::new(Served(Some(facts(now, true))))),
            ..AppState::for_test(trusted.db.clone())
        });
        assert!(
            advance_mta_sts_modes_at(&lapsed, due + DAY_MS)
                .await
                .is_empty()
        );
        assert_eq!(stored_mode(&lapsed).await, "enforce");
        assert_eq!(
            fauna_mail::outbound::mta_sts::MtaStsMode::from_stored("enforce")
                .coupled_to_cert(false),
            fauna_mail::outbound::mta_sts::MtaStsMode::Testing
        );
    }
}
