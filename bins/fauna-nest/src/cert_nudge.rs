//! At-risk TLS-certificate renewal push (`tls-certificates.md` § C.4, mitigation 4).
//!
//! On a **client-driven DNS-01** deployment the nest cannot renew its own cert —
//! only an admin's online client can (it holds the DNS-provider key; the nest
//! never does, `dns-management.md` § Where the credential lives). An
//! iOS/Android-only admin who never opens the app would let the cert drift onto
//! the self-signed floor. The floor keeps the *Fauna* app working (identity-pin,
//! § C.1), but browsers / MUAs / inbound-mail-trust degrade until a client
//! completes a renewal. This task closes that gap: it periodically checks the
//! cert the listener **actually serves** for each deployment domain and, when any
//! is on-floor / expiring **and** no client has renewed within the lead window,
//! pushes the admin a "open your Fauna app to renew your certificate" nudge —
//! reaching a *closed* mobile app via the existing [`crate::push::PushService`]
//! (web-push + APNs). It never renews anything itself and never touches DNS.
//!
//! **The served cert's `notAfter` is the "no client has renewed" truth** — a
//! client renewal moves it forward, so the same [`crate::acme::cert_health_state`]
//! policy A1 built (`!= ValidTrusted` ⇒ at-risk) is the trigger; no separate
//! last-renewal bookkeeping. De-dup is one deployment-wide [`NudgeState`]
//! timestamp persisted in `acme_dir` (mirroring `acme_http01::RetryState`), so a
//! restart loop cannot spam the admin. The push is per-admin-actor and
//! deployment-wide ("renew your certificate"); the admin opens the app and sees
//! the full per-domain cert-status list (the A2 `snapshot.cert_statuses` surface).
//!
//! **Spawn gate** (`main.rs`, mirroring `floor_renew_task` + the mail-domain
//! real-domain predicate): only when ACME HTTP-01 is **off** (an HTTP-01 box
//! self-renews — a nudge there is wrong, the fix is server-side), a **real**
//! public domain is configured (a localhost/IP/`.local` box is happy on the
//! floor — never nag it), and a `PushService` + a served-cert resolver exist.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::tls::CertHealthState;

use crate::acme::{ServedCertFacts, ServedCertSpki, cert_health_state};
use crate::push::DevicePresence;
use crate::routes::AppState;

/// How often the task re-checks served-cert health, and the minimum spacing
/// between two nudges to the same deployment. One nudge per day while a cert is
/// at-risk is enough attention without nagging — and the renewal lead
/// ([`CERT_RENEWAL_LEAD_SECS`], 30 days) leaves a wide window to act.
pub const CERT_NUDGE_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// Notification title for the at-risk renewal nudge. Hardcoded English, matching
/// the other nest-originated pushes (`conversations_handlers` "Group invite",
/// etc.); nest-side push localization is a separate, unsolved concern. Web-push
/// shows generic content (privacy — see [`crate::push`]); the rich title/body
/// reach APNs (the iOS admin this mitigation primarily targets).
const NUDGE_TITLE: &str = "Certificate renewal needed";
/// Notification body — deployment-wide, no domain names (avoid leaking the
/// deployment's domains through a vendor push relay). The app shows the
/// per-domain detail on open.
const NUDGE_BODY: &str = "Open your Fauna app to renew your TLS certificate.";
/// Deep link the notification tap routes to — the `admin-dns` page that renders
/// the cert-status row (A2). The per-app render is the entrusted UI lift; the
/// path is the convention the other pushes use (`/app/...`).
const NUDGE_URL: &str = "/app/admin-dns";

/// Persisted, deployment-wide de-dup timestamp for the at-risk nudge. Lives in
/// `acme_dir` next to the cert + `acme-retry-state.json`, so it shares the
/// deployment's persistent volume and survives container restarts — a restart
/// loop cannot re-nudge before [`CERT_NUDGE_INTERVAL_SECS`] elapses.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NudgeState {
    /// Unix seconds of the last nudge sent, `0` if never.
    #[serde(default)]
    pub last_nudge_unix: i64,
}

impl NudgeState {
    /// Filename within `acme_dir`.
    pub const FILENAME: &'static str = "cert-nudge-state.json";

    /// Load the persisted state, defaulting (never-nudged) on a missing or
    /// corrupt file — the same fail-open-to-default policy `RetryState` uses.
    pub fn load(acme_dir: &Path) -> Self {
        match std::fs::read(acme_dir.join(Self::FILENAME)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist best-effort (a write failure just means a possible duplicate
    /// nudge next tick — never fatal).
    pub fn save(&self, acme_dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(acme_dir.join(Self::FILENAME), json);
        }
    }
}

/// Pure policy: the domains whose served cert is **not** `ValidTrusted` (on the
/// floor, expiring within the lead, non-covering, or absent) — the at-risk set.
/// Reuses the single [`cert_health_state`] policy point (A1, decision D1), so the
/// nudge, the `fauna.tls.cert_status` row, and (later) auto-issue all agree.
pub fn at_risk_domains(facts: &[(String, Option<ServedCertFacts>)], now_unix: i64) -> Vec<String> {
    facts
        .iter()
        .filter(|(_, f)| cert_health_state(*f, now_unix) != CertHealthState::ValidTrusted)
        .map(|(domain, _)| domain.clone())
        .collect()
}

/// Pure policy: whether enough time has passed since the last nudge to send
/// another (≥ [`CERT_NUDGE_INTERVAL_SECS`]).
pub fn nudge_due(last_nudge_unix: i64, now_unix: i64) -> bool {
    now_unix - last_nudge_unix >= CERT_NUDGE_INTERVAL_SECS
}

/// Production entry point: run the at-risk nudge loop forever, pushing via the
/// deployment's [`crate::push::PushService`]. No-op (logs + returns) if there is
/// no served-cert resolver or no push service — the spawn gate in `main.rs`
/// already excludes those, this is just defensive.
pub async fn cert_at_risk_nudge_task(app_state: Arc<AppState>, apex: String, acme_dir: PathBuf) {
    let Some(resolver) = app_state.served_cert_spki.clone() else {
        tracing::warn!("cert-nudge: no served-cert resolver — at-risk renewal push not started");
        return;
    };
    let Some(push) = app_state.push_service.clone() else {
        tracing::warn!("cert-nudge: no push service — at-risk renewal push not started");
        return;
    };
    tracing::info!("cert-nudge: at-risk renewal push task started for {apex}");
    let ws = Arc::clone(&app_state.ws);
    let push_op = move |actor: Vec<u8>, presence: DevicePresence, _at_risk: Vec<String>| {
        let (push, ws) = (push.clone(), Arc::clone(&ws));
        async move {
            // The same per-device rule as every other push (`push.rs` § The
            // per-device decision): a device of the admin's with the app open
            // is not nudged (it shows the cert-status row, and a synced device
            // can auto-issue — Slice C); their other, absent devices are.
            let _ = push
                .maybe_send_push(&ws, &presence, &actor, NUDGE_TITLE, NUDGE_BODY, NUDGE_URL)
                .await;
        }
    };
    cert_at_risk_nudge_loop(app_state, apex, resolver, acme_dir, push_op, None).await;
}

/// The loop body, parameterised over the push operation and an optional
/// iteration cap (mirrors [`crate::acme_http01`]'s `cert_lifecycle_loop`):
/// production passes the real per-actor push + `None` (forever); tests pass a
/// recording closure + a small cap.
///
/// Each tick: derive the deployment's domains
/// ([`crate::acme_http01::desired_san_domains`] over `list_active_mail_domains` —
/// the same set the issuer covers), read what the listener serves for each, and
/// if any is at-risk **and** a nudge is due, push every admin actor and persist
/// the timestamp.
async fn cert_at_risk_nudge_loop<P, Fut>(
    app_state: Arc<AppState>,
    apex: String,
    resolver: Arc<dyn ServedCertSpki>,
    acme_dir: PathBuf,
    push: P,
    max_iterations: Option<usize>,
) where
    P: Fn(Vec<u8>, DevicePresence, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut iterations: usize = 0;
    loop {
        let now = crate::acme_http01::now_unix() as i64;

        let mail_domain_names: Vec<String> = match app_state.db.list_active_mail_domains().await {
            Ok(rows) => rows.into_iter().map(|d| d.domain_name).collect(),
            Err(e) => {
                tracing::warn!("cert-nudge: list_active_mail_domains failed: {e:#}");
                Vec::new()
            }
        };
        // Same SAN set the issuer covers — including the infra-subdomain hosts
        // `relay.<apex>` / `pds.<apex>`, gated identically (`relay_san_included` /
        // `pds_san_included` = the service being on AND the name resolving) AND the
        // secondary mail domains, resolve-gated identically
        // (`reachable_mail_domains`) — so the nudge's at-risk check matches what
        // ACME actually orders. Sharing every gate is load-bearing: a flag-only or
        // ungated check here would nudge forever, flagging a `relay.<apex>` /
        // `pds.<apex>` / deferred-secondary SAN at-risk (served by the floor) while
        // the resolve-gated issuer declines to add it.
        let infra = crate::acme_http01::InfraSans {
            relay: crate::acme_http01::relay_san_included(&app_state, &apex).await,
            pds: crate::acme_http01::pds_san_included(&app_state, &apex).await,
        };
        // The apex's own SANs carry a gate too while a primary-domain rename is
        // pre-flip (`apex_sans_for_cycle`): a dead old primary's `<apex>` /
        // `mail.<apex>` drop out of the issuer's order, so nudging about them would
        // be the same forever-nudge this shared-gate rule prevents — they are served
        // by the floor precisely because the issuer declines to order them.
        let reachable =
            crate::acme_http01::reachable_mail_domains(&app_state, &apex, &mail_domain_names).await;
        let apex_sans = crate::acme_http01::apex_sans_for_cycle(&app_state, &apex).await;
        let domains = crate::acme_http01::desired_san_domains(&apex, &reachable, &infra, apex_sans);
        let facts: Vec<(String, Option<ServedCertFacts>)> = domains
            .into_iter()
            .map(|d| {
                let f = resolver.served_cert_facts(&d);
                (d, f)
            })
            .collect();
        let at_risk = at_risk_domains(&facts, now);

        if !at_risk.is_empty() && nudge_due(NudgeState::load(&acme_dir).last_nudge_unix, now) {
            match app_state.db.list_admin_actors().await {
                Ok(admins) => {
                    for (actor, _added_at) in admins {
                        let presence = <[u8; 32]>::try_from(actor.as_slice())
                            .map(|a| app_state.ws.device_presence(&a))
                            .unwrap_or_default();
                        push(actor, presence, at_risk.clone()).await;
                    }
                    NudgeState {
                        last_nudge_unix: now,
                    }
                    .save(&acme_dir);
                    tracing::info!(
                        ?at_risk,
                        "cert-nudge: pushed at-risk renewal reminder to admins"
                    );
                }
                Err(e) => tracing::warn!("cert-nudge: list_admin_actors failed: {e:#}"),
            }
        }

        iterations += 1;
        if let Some(max) = max_iterations
            && iterations >= max
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(CERT_NUDGE_INTERVAL_SECS as u64)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::CERT_RENEWAL_LEAD_SECS;
    use std::collections::HashMap;
    use std::sync::Arc as StdArc;
    use std::sync::Mutex as StdMutex;

    const DAY: i64 = 24 * 60 * 60;

    fn floor_facts(now: i64) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: now - DAY,
            not_after_unix: now + 10 * DAY,
            is_floor: true,
            covers: true,
        }
    }

    fn trusted_facts(now: i64) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: now - DAY,
            not_after_unix: now + 2 * CERT_RENEWAL_LEAD_SECS,
            is_floor: false,
            covers: true,
        }
    }

    fn expiring_facts(now: i64) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: now - DAY,
            // Within the renewal lead → Expiring.
            not_after_unix: now + CERT_RENEWAL_LEAD_SECS - DAY,
            is_floor: false,
            covers: true,
        }
    }

    // --- Pure policy --------------------------------------------------------

    #[test]
    fn at_risk_flags_floor_expiring_and_missing_but_not_trusted() {
        let now = 1_700_000_000;
        let facts = vec![
            ("trusted.example.com".to_string(), Some(trusted_facts(now))),
            ("floor.example.com".to_string(), Some(floor_facts(now))),
            (
                "expiring.example.com".to_string(),
                Some(expiring_facts(now)),
            ),
            ("missing.example.com".to_string(), None),
        ];
        let at_risk = at_risk_domains(&facts, now);
        assert_eq!(
            at_risk,
            vec![
                "floor.example.com".to_string(),
                "expiring.example.com".to_string(),
                "missing.example.com".to_string(),
            ],
            "floor / expiring / absent are at-risk; a valid-trusted cert is not"
        );
    }

    #[test]
    fn nudge_due_respects_the_interval() {
        let now = 1_700_000_000;
        assert!(!nudge_due(now, now), "just nudged → not due");
        assert!(
            !nudge_due(now - CERT_NUDGE_INTERVAL_SECS + 10, now),
            "within the window → not due"
        );
        assert!(
            nudge_due(now - CERT_NUDGE_INTERVAL_SECS, now),
            "exactly at the window boundary → due"
        );
        assert!(nudge_due(0, now), "never nudged → due");
    }

    #[test]
    fn nudge_state_round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            NudgeState::load(dir.path()),
            NudgeState::default(),
            "missing file → default (never nudged)"
        );
        let state = NudgeState {
            last_nudge_unix: 1_700_000_123,
        };
        state.save(dir.path());
        assert_eq!(
            NudgeState::load(dir.path()),
            state,
            "round-trips through disk"
        );
    }

    // --- Loop integration ---------------------------------------------------

    /// A [`ServedCertSpki`] double returning canned facts per SNI.
    struct FixedSpki {
        facts: HashMap<String, ServedCertFacts>,
    }
    impl ServedCertSpki for FixedSpki {
        fn current_spki_sha256(&self) -> Option<[u8; 32]> {
            None
        }
        fn served_cert_facts(&self, sni: &str) -> Option<ServedCertFacts> {
            self.facts.get(sni).copied()
        }
        fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
            None
        }
    }

    type Pushes = StdArc<StdMutex<Vec<(Vec<u8>, Vec<String>)>>>;

    fn recorder() -> (
        Pushes,
        impl Fn(Vec<u8>, DevicePresence, Vec<String>) -> std::future::Ready<()> + Clone,
    ) {
        let pushes: Pushes = StdArc::new(StdMutex::new(Vec::new()));
        let sink = pushes.clone();
        let push = move |actor: Vec<u8>, _presence: DevicePresence, at_risk: Vec<String>| {
            sink.lock().unwrap().push((actor, at_risk));
            std::future::ready(())
        };
        (pushes, push)
    }

    async fn test_state_with_admin(admin: &[u8; 32]) -> StdArc<AppState> {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        db.add_admin_actor(admin).await.expect("seed admin");
        StdArc::new(AppState::for_test(db))
    }

    fn spki_for(apex: &str, facts: ServedCertFacts) -> StdArc<dyn ServedCertSpki> {
        let mut m = HashMap::new();
        m.insert(apex.to_string(), facts);
        StdArc::new(FixedSpki { facts: m })
    }

    #[tokio::test(start_paused = true)]
    async fn loop_pushes_admin_when_apex_on_floor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let apex = "nest.example.com";
        let admin = [0xA1u8; 32];
        let state = test_state_with_admin(&admin).await;
        let now = crate::acme_http01::now_unix() as i64;
        let (pushes, push) = recorder();

        cert_at_risk_nudge_loop(
            state,
            apex.to_string(),
            spki_for(apex, floor_facts(now)),
            dir.path().to_path_buf(),
            push,
            Some(1),
        )
        .await;

        let recorded = pushes.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "one push to the one admin");
        assert_eq!(recorded[0].0, admin.to_vec(), "targeted the admin actor");
        assert_eq!(
            recorded[0].1,
            vec![apex.to_string()],
            "the at-risk domain set is the on-floor apex"
        );
        assert_ne!(
            NudgeState::load(dir.path()).last_nudge_unix,
            0,
            "the nudge timestamp was persisted"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn loop_does_not_push_when_apex_trusted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let apex = "nest.example.com";
        let admin = [0xB2u8; 32];
        let state = test_state_with_admin(&admin).await;
        let now = crate::acme_http01::now_unix() as i64;
        let (pushes, push) = recorder();

        cert_at_risk_nudge_loop(
            state,
            apex.to_string(),
            spki_for(apex, trusted_facts(now)),
            dir.path().to_path_buf(),
            push,
            Some(1),
        )
        .await;

        assert!(
            pushes.lock().unwrap().is_empty(),
            "a valid-trusted cert is not at-risk → no nudge"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn loop_dedups_within_window_across_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let apex = "nest.example.com";
        let admin = [0xC3u8; 32];
        let now = crate::acme_http01::now_unix() as i64;

        // First "process": on-floor → nudges once, persists the timestamp.
        let state1 = test_state_with_admin(&admin).await;
        let (pushes1, push1) = recorder();
        cert_at_risk_nudge_loop(
            state1,
            apex.to_string(),
            spki_for(apex, floor_facts(now)),
            dir.path().to_path_buf(),
            push1,
            Some(1),
        )
        .await;
        assert_eq!(pushes1.lock().unwrap().len(), 1, "first run nudges");

        // Second "process" (restart) sharing the same acme_dir, still within the
        // window → the persisted timestamp suppresses a duplicate nudge.
        let state2 = test_state_with_admin(&admin).await;
        let (pushes2, push2) = recorder();
        cert_at_risk_nudge_loop(
            state2,
            apex.to_string(),
            spki_for(apex, floor_facts(now)),
            dir.path().to_path_buf(),
            push2,
            Some(1),
        )
        .await;
        assert!(
            pushes2.lock().unwrap().is_empty(),
            "a restart within the window must not re-nudge (persisted de-dup)"
        );

        // Backdate the persisted nudge past the window → due again.
        NudgeState {
            last_nudge_unix: now - CERT_NUDGE_INTERVAL_SECS - 1,
        }
        .save(dir.path());
        let state3 = test_state_with_admin(&admin).await;
        let (pushes3, push3) = recorder();
        cert_at_risk_nudge_loop(
            state3,
            apex.to_string(),
            spki_for(apex, floor_facts(now)),
            dir.path().to_path_buf(),
            push3,
            Some(1),
        )
        .await;
        assert_eq!(
            pushes3.lock().unwrap().len(),
            1,
            "once the window passes, an at-risk cert nudges again"
        );
    }
}
