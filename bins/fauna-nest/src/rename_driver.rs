//! The primary-domain rename's **driver** — the state machine's nest-side
//! motion, as a task of its own (`mail-primary-domain-rename.md` § Behavior —
//! cert chain re-issue ordering → *Issuer-independence — the rename driver*).
//!
//! Two issuers write the deployment's one listener cert into
//! `acme_dir/fullchain.pem`: the nest's own HTTP-01 lifecycle loop
//! ([`crate::acme_http01`]) and the admin's client over DNS-01
//! (`lan_cert::install_client_issued_cert` → `store_acme_material`). A rename
//! must advance whichever wrote it, and a nest on the private axis runs no
//! HTTP-01 loop at all — so the motion lives here, not in an issuer's loop:
//! every tick it re-asserts `requested → cert_issuance`, stamps `cert_ready`
//! once the **installed** cert covers `mail.<new>` **and** `mail.<new>`
//! resolves to the mail-host address (rule 3: a DNS-01 cert can cover a host
//! that has no `A` record yet, and the flip retargets every `MX` at it), then
//! — one tick later — runs [`rename_post_cert_steps`]: the anchor flip, the
//! identity reconcile and its `config_changed` push, the grace watcher. The
//! issuers keep only the ordering half, through the one desired-SAN builder
//! ([`crate::acme_http01::rename_sans_for_cycle`]).
//!
//! Spawned on every nest that has an `acme_dir`, HTTP-01 on or off
//! (`main.rs`). Cadence: the lifecycle cadence ([`RENAME_DRIVER_CADENCE`]),
//! cut short by a cert install — the cert watcher bumps
//! `AppState::relay_cert_changed` on every change of the on-disk chain,
//! whichever issuer wrote it, and that watch channel takes any number of
//! subscribers. (The HTTP-01 loop's `acme_retry_notify` is a single-waiter
//! `notify_one` wake and stays that loop's; a successful issuance it triggers
//! reaches this task as the install that follows.) Every step re-derives from
//! the persisted rename row, so a crash at any point resumes on the next tick
//! (§ Crash recovery).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::acme::CERT_FILENAME;
use crate::routes::AppState;

/// The driver's steady cadence — the cert lifecycle's own 5-minute poll, so a
/// rename moves at the pace the issuer re-checks coverage. A constant, not a
/// knob: nobody chooses how often a state machine re-derives itself.
pub const RENAME_DRIVER_CADENCE: Duration = Duration::from_secs(5 * 60);

/// The production task: [`tick_once`] forever, every
/// [`RENAME_DRIVER_CADENCE`] or on a cert install, whichever comes first.
pub async fn rename_driver_task(app_state: Arc<AppState>, acme_dir: std::path::PathBuf) {
    let mut cert_changed = app_state.relay_cert_changed.subscribe();
    loop {
        tick_once(&app_state, &acme_dir).await;
        tokio::select! {
            _ = tokio::time::sleep(RENAME_DRIVER_CADENCE) => {}
            changed = cert_changed.changed() => {
                if changed.is_err() {
                    // The sender lives in `AppState`, so this is a generation
                    // being torn down: fall back to the cadence alone.
                    tokio::time::sleep(RENAME_DRIVER_CADENCE).await;
                }
            }
        }
    }
}

/// One driver tick. A row already `cert_ready` at the START of the tick flips
/// at its END (inside [`rename_post_cert_steps`]), so a row stamped this tick
/// flips on the next one and `cert_ready` stays an observable resting state.
pub(crate) async fn tick_once(app_state: &Arc<AppState>, acme_dir: &Path) {
    let cert_ready_at_tick_start = app_state
        .db
        .get_active_rename()
        .await
        .ok()
        .flatten()
        .filter(|r| r.parsed_state() == Some(fauna_mail::RenameState::CertReady))
        .map(|r| r.rename_id);

    if let Some((rename_id, mail_host, state)) =
        crate::acme_http01::active_rename_new_mail_host(app_state).await
    {
        // `requested → cert_issuance` (idempotent). The `start` handler advances
        // on the RPC; this re-assertion covers a crash between the insert and
        // that advance (§ Crash recovery).
        let state = if state == fauna_mail::RenameState::Requested {
            match app_state
                .db
                .advance_rename_to_cert_issuance(&rename_id)
                .await
            {
                Ok(r) => r
                    .parsed_state()
                    .unwrap_or(fauna_mail::RenameState::CertIssuance),
                Err(e) => {
                    tracing::warn!("rename driver: advance rename to cert_issuance failed: {e:#}");
                    state
                }
            }
        } else {
            state
        };
        // Strict `cert_issuance → cert_ready`: a re-observe on a later tick (row
        // already `cert_ready`) never overwrites the original acquisition stamp.
        if state == fauna_mail::RenameState::CertIssuance {
            stamp_cert_ready_if_covered(app_state, acme_dir, &rename_id, &mail_host).await;
        }
    }

    rename_post_cert_steps(app_state, cert_ready_at_tick_start).await;
}

/// Stamp `cert_ready` when the installed listener cert — whoever issued it —
/// covers `mail_host` AND `mail_host` resolves to the mail-host address (rule
/// 3, strong-check-or-DROP: [`crate::acme_http01::rename_mail_host_reachable`]).
/// The coverage read comes first: it is a file read, the reachability check a
/// DNS lookup.
async fn stamp_cert_ready_if_covered(
    app_state: &Arc<AppState>,
    acme_dir: &Path,
    rename_id: &[u8; 16],
    mail_host: &str,
) {
    let mail_host = mail_host.to_ascii_lowercase();
    let Ok(pem) = tokio::fs::read(acme_dir.join(CERT_FILENAME)).await else {
        return; // no cert installed yet — wait
    };
    if !crate::acme_http01::cert_covers_sans(&pem, std::slice::from_ref(&mail_host)) {
        return; // the issuer has not covered the new MX host yet — wait
    }
    if !crate::acme_http01::rename_mail_host_reachable(app_state, &mail_host).await {
        tracing::info!(
            domain = %mail_host,
            "rename driver: the installed cert covers the new MX host but it does not resolve \
             to the mail address yet — waiting in cert_issuance (the flip would retarget every \
             MX at it)"
        );
        return;
    }
    let Some(fp) = crate::acme_http01::leaf_spki_fingerprint_hex(&pem) else {
        tracing::warn!("rename driver: could not fingerprint the cert leaf; retry next tick");
        return;
    };
    match app_state
        .db
        .mark_rename_cert_ready(rename_id, &fp, crate::db::now_epoch_millis())
        .await
    {
        Ok(_) => tracing::info!(
            domain = %mail_host,
            "primary-domain rename: the installed cert covers the new MX host — cert_ready"
        ),
        Err(e) => tracing::warn!("primary-domain rename: mark cert_ready failed: {e:#}"),
    }
}

/// The cert-independent tail of one driver tick for a primary-domain rename
/// (`mail-primary-domain-rename.md` § Lifecycle, § Crash recovery): the anchor
/// flip of a row that was `cert_ready` at the start of the tick, the
/// unconditional identity-projection reconcile, and the grace watcher.
/// [`tick_once`] runs it every tick; the `test-hooks` rename hook
/// (`crate::rename_cert_test_hook`) runs the same steps on a test nest, where
/// no CA can issue.
pub(crate) async fn rename_post_cert_steps(
    app_state: &Arc<AppState>,
    rename_cert_ready_at_tick_start: Option<[u8; 16]>,
) {
    // Primary-domain rename (SLICE 3): the anchor flip. A rename that was
    // `cert_ready` at the START of this tick (captured by the caller) flips now — the
    // tick *after* cert_ready, so `cert_ready` stays an observable resting
    // state. `advance_rename_to_grace` atomically flips `is_primary` + enters
    // `grace` (`mail-primary-domain-rename.md` § Lifecycle / § Data — Mutations
    // to mail_domains). The runtime identity projection + the
    // `config_changed`/`local_domains` push follow from the **unconditional
    // reconcile below**, which reads the freshly-flipped DB primary — so the
    // flip itself only owns the DB transition. Crash-safe: the flip is ONE DB
    // transaction, so a restart re-derives the identity from the flipped
    // primary row at boot.
    if let Some(rid) = rename_cert_ready_at_tick_start {
        match app_state.db.advance_rename_to_grace(&rid).await {
            Ok(_) => tracing::info!(
                "primary-domain rename: anchor flip committed — is_primary flipped, entering grace"
            ),
            Err(e) => tracing::warn!("primary-domain rename: anchor flip failed: {e:#}"),
        }
    }

    // Identity-projection self-heal.
    // The deployment's identity domain is an in-memory projection of the
    // primary `mail_domains` row (`identity_domain_core`), so re-derive it
    // from the DB primary **unconditionally each tick** — NOT only while a
    // rename is post-flip-active. This one reconcile covers every source of
    // divergence with a single self-healing rule: the anchor flip (new
    // primary just promoted), the abort re-flip (old primary restored), AND
    // any raced divergence the old rename-gated reconcile could not heal —
    // a stale tick re-applying a since-aborted `new` (which then had no
    // active rename left to trigger a heal), or the abort handler's own
    // silent skip-on-lookup-failure. Whatever the source, the next tick
    // converges the projection onto the DB truth
    // (`mail-primary-domain-rename.md` § Crash recovery — self-healing).
    //
    // Gated exactly like the other identity setter (`add_local_domain_handler`)
    // and `apply_primary_identity`'s own contract: act only when a primary
    // mail domain exists and is non-local. An IP / domainless-claimed box
    // registers no `mail_domains` row (`lookup_primary_mail_domain` → None),
    // so its identity comes from the registration handle and is left
    // untouched. The `!=` guard makes the steady state a pure no-op — boot's
    // `resolve_identity_domain` seeds `identity_domain` from the same
    // `domain_name`, so no reconcile fires and no `config_changed` is pushed
    // unless the projection has actually drifted.
    if let Ok(Some(primary)) = app_state.db.lookup_primary_mail_domain().await
        && fauna_core::resolve::is_public_dns_name(&primary.domain_name)
        && app_state.handle_domain() != primary.domain_name
    {
        crate::identity_domain_core::apply_primary_identity(app_state, &primary.domain_name);
        crate::bridge_routing_handlers::notify_bridges_config_changed(
            app_state,
            fauna_protocol::bridge_routing::config_change_reason::LOCAL_DOMAINS,
        )
        .await;
        // Every ATProto handle on the box is derived from this domain, so the
        // identity domain moving renames the WHOLE roster at once
        // (`atproto-pds-bridge.md` § Identity — a Fauna handle *or domain*
        // change re-derives). No actor hint: the nudge covers all of them.
        // This one site catches every cause — the anchor flip, the abort
        // re-flip, and any raced divergence — because it is the single place
        // the projection actually converges onto the DB primary. Latency
        // only: the bridge's own pass compares derived-vs-published, so a
        // dropped push delays a rename, never loses it.
        crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(app_state, None)
            .await;
        tracing::info!(
            domain = %primary.domain_name,
            "primary-domain identity reconcile: projection swapped to the DB primary + config_changed(local_domains) pushed"
        );
    }

    // Primary-domain rename (SLICE 4): the grace watcher. When a `grace` row's
    // wall-clock window has elapsed (`NOW() > grace_ends_at`), promote it to
    // `ready_to_complete` — the source-of-truth advancement past grace
    // (`mail-primary-domain-rename.md` § Architectural rules — no admin action
    // is needed for `grace → ready_to_complete`; the admin's `complete` gates
    // only `ready_to_complete → completed`). No cert/DNS/identity side effect:
    // `ready_to_complete` is functionally identical to `grace` for the keep-
    // alives (both `is_post_flip_active`), so nothing to re-derive. Idempotent
    // + crash-safe — re-read fresh from the persisted row each tick, so a
    // restart mid-grace resumes the watch; a row that just entered `grace` this
    // tick has `grace_ends_at = now + grace_days`, so it won't promote early.
    if let Some(rename) = app_state.db.get_active_rename().await.ok().flatten()
        && rename.parsed_state() == Some(fauna_mail::RenameState::Grace)
        && let Some(ends_at) = rename.grace_ends_at
        && crate::db::now_epoch_millis() > ends_at
    {
        match app_state
            .db
            .advance_rename_to_ready_to_complete(&rename.rename_id)
            .await
        {
            Ok(_) => tracing::info!(
                "primary-domain rename: grace window elapsed — ready_to_complete (awaiting admin complete)"
            ),
            Err(e) => tracing::warn!(
                "primary-domain rename: grace watcher advance to ready_to_complete failed: {e:#}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme_http01::tests::rename_test_state;
    use crate::dns_verifier::LookupOutcome;
    use fauna_acme_http01::test_support::make_multi_san_cert_pem;

    /// Install a cert the way `store_acme_material` does — the one listener
    /// chain at `acme_dir/fullchain.pem` — with NO HTTP-01 loop anywhere: the
    /// shape of a client-issued (DNS-01) cert on the private axis.
    async fn install_cert(acme_dir: &Path, sans: &[&str]) {
        tokio::fs::write(acme_dir.join(CERT_FILENAME), make_multi_san_cert_pem(sans))
            .await
            .expect("write installed cert");
    }

    #[tokio::test]
    async fn driver_advances_a_rename_off_a_client_installed_cert_with_no_http01_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // `mail.<new>` resolves to the mail address (every name → 203.0.113.7).
        let st = rename_test_state(&db, LookupOutcome::Records(vec!["203.0.113.7".into()])).await;
        install_cert(
            dir.path(),
            &[
                "nest.example.com",
                "mail.nest.example.com",
                "new.example.com",
                "mail.new.example.com",
            ],
        )
        .await;

        tick_once(&st, dir.path()).await;
        let row = db.get_active_rename().await.unwrap().expect("active");
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::CertReady),
            "tick 1: the installed cert covers mail.<new> and it resolves → cert_ready"
        );
        assert!(
            row.new_cert_fingerprint.is_some(),
            "cert_ready stamps the fingerprint"
        );

        tick_once(&st, dir.path()).await;
        let row = db.get_active_rename().await.unwrap().expect("active");
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::Grace),
            "tick 2: the anchor flip — the tick after cert_ready"
        );
        assert_eq!(
            st.handle_domain(),
            "new.example.com",
            "the identity reconcile swapped the projection to the new primary"
        );
    }

    #[tokio::test]
    async fn driver_waits_when_the_cert_covers_mail_new_but_it_does_not_resolve() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // A DNS-01 cert can cover a host with no A record (rule 3): nothing
        // resolves here, yet the installed cert names `mail.<new>`.
        let st = rename_test_state(&db, LookupOutcome::Empty).await;
        install_cert(dir.path(), &["nest.example.com", "mail.new.example.com"]).await;

        tick_once(&st, dir.path()).await;
        tick_once(&st, dir.path()).await;
        let row = db.get_active_rename().await.unwrap().expect("active");
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance),
            "coverage without reachability must not stamp cert_ready — the flip would \
             retarget every MX at a host that does not resolve"
        );
        assert!(row.new_cert_fingerprint.is_none());
    }

    #[tokio::test]
    async fn driver_waits_while_the_installed_cert_lacks_mail_new() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state(&db, LookupOutcome::Records(vec!["203.0.113.7".into()])).await;
        install_cert(dir.path(), &["nest.example.com", "mail.nest.example.com"]).await;

        tick_once(&st, dir.path()).await;
        assert_eq!(
            db.get_active_rename()
                .await
                .unwrap()
                .expect("active")
                .parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance),
            "the driver re-asserts requested → cert_issuance and waits for a covering cert"
        );
    }
}
