//! Tests for the rotation-flow bounded-grant **heal driver**.
//!
//! Authority: `docs/goal/architecture/encryption-at-rest.md` § Capability
//! tiering — the content-sealing-epochs rotation-heal amendment (2026-07-19).
//!
//! An MSEK hard-revoke resets the mail epoch lineage, so an outstanding
//! **bounded** mail grant's per-epoch wraps (sealed under the retired root)
//! stop covering content sealed under the new root. The heal driver enumerates
//! the outstanding bounded grants at rotation and drives one equal-end
//! `fauna.capabilities.renew` per grant whose `appended_keys` re-wrap the whole
//! window under the **post-rotation generation set** — old root for pre-rotation
//! epochs, new root for post-rotation epochs, and both concatenated (newest
//! first) for the rotation-boundary epochs. Best-effort/log-only: a failed heal
//! never fails the rotation (the grant just heals at its next renew instead).

use ed25519_dalek::SigningKey;
use fauna_client_bridges::HolderInfo;
use fauna_client_capabilities::grant_log;
use fauna_client_mail_settings::testing::FakeSuccessionLedgerStore;
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, MailSettingsMachine, SecretBytes,
};
use fauna_core::data::Timestamp;
use fauna_core::grant_event::GrantEventKind;
use fauna_mls::wrapped_blob::{
    WrappedScopeKey, derive_bridge_service_user_mlkem768, derive_mail_epoch_root,
    derive_recipient_mail_epoch_capability_secret_from_root, generate_x25519_keypair,
    mail_sealing_epoch_of, unseal_capability, unseal_capability_hybrid,
};

mod common;
use common::{ACTOR, SIGNER_SEED};

/// One content-sealing epoch is a week (`mail-content-sealing-epochs` design
/// § 2; the crypto bound is epoch-granular with ≤ 1 epoch slack). Used only to
/// place the seeded grant window across several past + future epochs.
const EPOCH_SECS: u64 = 7 * 24 * 60 * 60;

/// Seed a bounded mail grant into the machine's succession-ledger grant log, signed by
/// the same identity key the machine's signer holds. `holder` is the X25519
/// pubkey the grant is minted to (matched against the roster at heal time).
fn seed_bounded_grant(
    cfg: &FakeSuccessionLedgerStore,
    grant_id: [u8; 16],
    holder: [u8; 32],
    window_start: u64,
    window_end: u64,
) {
    let mut config = cfg.current();
    let signing_key = SigningKey::from_bytes(&SIGNER_SEED);
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(config.actor_id);
    grant_log::record_mint(
        &mut log,
        &signing_key,
        grant_id,
        holder,
        vec![grant_log::bounded_mail_event_scope()],
        window_start,
        window_end,
        window_start,
    )
    .expect("seed bounded grant");
    config.grant_events.extend(log.grant_events);
    cfg.replace(config);
}

/// Seed a **master** (standing) mail grant — the mail scope with NO bounded
/// tier marker. The heal driver must leave these alone (a master grant's window
/// bump is a different lifecycle event, not epoch healing).
fn seed_master_mail_grant(
    cfg: &FakeSuccessionLedgerStore,
    grant_id: [u8; 16],
    holder: [u8; 32],
    window_start: u64,
    window_end: u64,
) {
    use fauna_core::grant_event::GrantEventScope;
    let mut config = cfg.current();
    let signing_key = SigningKey::from_bytes(&SIGNER_SEED);
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(config.actor_id);
    grant_log::record_mint(
        &mut log,
        &signing_key,
        grant_id,
        holder,
        vec![GrantEventScope {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None, // master: no bounded marker
        }],
        window_start,
        window_end,
        window_start,
    )
    .expect("seed master mail grant");
    config.grant_events.extend(log.grant_events);
    cfg.replace(config);
}

async fn enable_and_rotate(machine: &MailSettingsMachine) {
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await
        .unwrap();
}

/// The happy path with a **hybrid** (X-Wing) holder: an outstanding bounded
/// grant spanning three past + two future epochs heals across the rotation with
/// exactly one equal-end renew, and the appended wraps derivation-check against
/// the post-rotation generation set — old root for past epochs, new root for
/// future epochs, both (newest first) for the boundary epochs.
#[tokio::test]
async fn rotation_heals_a_hybrid_bounded_grant_across_the_generation_boundary() {
    let common::Fixture {
        machine,
        nest,
        config: cfg,
        mail,
    } = common::fixture();

    enable_and_rotate(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    // A hybrid holder: an X25519 keypair (the grant's seal target) + an ML-KEM
    // keypair (the X-Wing arm). Its published ek in the roster drives the heal's
    // X-Wing wrap selection.
    let (holder_sk, holder_pk) = generate_x25519_keypair();
    let (holder_dk, holder_ek) = derive_bridge_service_user_mlkem768(b"heal-hybrid-holder");
    nest.state().content_processor_holders = vec![HolderInfo {
        pubkey: holder_pk,
        mlkem_ek: Some(holder_ek.as_slice().to_vec()),
        role: "mda".into(),
        bridge_id: "mda-1".into(),
    }];

    // Window: three past epochs → two future epochs, so the heal produces
    // old-only, boundary (both), and new-only wraps.
    let now = Timestamp::now_secs() as u64;
    let window_start = now - 3 * EPOCH_SECS;
    let window_end = now + 2 * EPOCH_SECS;
    let grant_id = [0xB0; 16];
    seed_bounded_grant(&cfg, grant_id, holder_pk, window_start, window_end);

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotation succeeds");

    let stored = cfg.current();
    let stored_mail = mail.current();
    let post_msek = stored_mail.msek.clone().unwrap();
    assert_ne!(pre_msek, post_msek, "MSEK rotated");
    let retired_at = stored_mail
        .prior_msek_retired_at(&pre_msek)
        .expect("rotation records the retirement instant");
    let e_boundary = mail_sealing_epoch_of(retired_at);

    // Exactly one renew, equal-end, for the seeded grant.
    let renews = nest.state().renewed_grants.clone();
    assert_eq!(renews.len(), 1, "exactly one heal renew fired");
    let (renewed_id, new_start, new_end, appended) = &renews[0];
    assert_eq!(*renewed_id, grant_id, "renew targets the seeded grant");
    assert_eq!(
        *new_start, window_start,
        "equal-window renew — the heal never slides the recorded start"
    );
    assert_eq!(
        *new_end, window_end,
        "equal-end renew — a pure key refresh, never a window change"
    );

    // The appended wraps cover exactly the window's inclusive epoch range.
    let wraps: Vec<WrappedScopeKey> = appended
        .iter()
        .map(|b| WrappedScopeKey::from_canonical_bytes(b).expect("wrap decodes"))
        .collect();
    let mut got_epochs: Vec<u64> = wraps
        .iter()
        .map(|w| {
            w.epoch
                .expect("bounded wrap is epoch-scoped, never standing")
        })
        .collect();
    got_epochs.sort_unstable();
    let want_epochs: Vec<u64> =
        (mail_sealing_epoch_of(window_start)..=mail_sealing_epoch_of(window_end)).collect();
    assert_eq!(got_epochs, want_epochs, "one wrap per window epoch");

    let new_root = derive_mail_epoch_root(&post_msek);
    let old_root = derive_mail_epoch_root(&pre_msek);
    let secret =
        |root: &[u8; 32], e: u64| derive_recipient_mail_epoch_capability_secret_from_root(root, e);
    let unseal = |e: u64| -> Vec<u8> {
        let w = wraps
            .iter()
            .find(|w| w.epoch == Some(e))
            .unwrap_or_else(|| panic!("wrap for epoch {e}"));
        unseal_capability_hybrid(w, &ACTOR, &holder_sk, &holder_dk)
            .unwrap_or_else(|e| panic!("hybrid unseal failed: {e}"))
    };

    // Boundary epoch: both generations, newest (new root) first.
    let want_boundary = [secret(&new_root, e_boundary), secret(&old_root, e_boundary)].concat();
    assert_eq!(
        unseal(e_boundary),
        want_boundary,
        "boundary-epoch payload = new-gen secret ∥ old-gen secret"
    );
    // The propagation-tail boundary (retirement epoch + 1) also carries both.
    let want_tail = [
        secret(&new_root, e_boundary + 1),
        secret(&old_root, e_boundary + 1),
    ]
    .concat();
    assert_eq!(unseal(e_boundary + 1), want_tail, "boundary+1 carries both");

    // A pre-rotation epoch: OLD generation only (the new root did not exist).
    let e_past = e_boundary - 2;
    assert_eq!(
        unseal(e_past),
        secret(&old_root, e_past),
        "a past epoch heals under the OLD root only"
    );

    // A post-rotation epoch beyond the boundary tail: NEW generation only.
    let e_future = e_boundary + 2;
    assert_eq!(
        unseal(e_future),
        secret(&new_root, e_future),
        "a future epoch heals under the NEW root only"
    );

    // The heal is recorded as a Renew grant-log event (History forensics).
    let renew_events: Vec<_> = stored
        .grant_events
        .iter()
        .filter(|e| e.kind == GrantEventKind::Renew)
        .collect();
    assert_eq!(
        renew_events.len(),
        1,
        "one Renew event recorded for the heal"
    );
    let still_current = grant_log::current_grants(&stored)
        .into_iter()
        .find(|g| g.grant_id == grant_id.as_slice())
        .expect("healed grant is still current");
    assert_eq!(
        still_current.window_end, window_end,
        "the equal-end renew keeps the window unchanged"
    );
    // The heal-Renew must carry the bounded `tier` marker forward from the fold —
    // otherwise a second rotation would no longer recognise the grant as bounded
    // and would never heal it again (the Renew
    // must self-carry scope, which `build_renew_event` does).
    assert!(
        grant_log::is_bounded_mail_grant(&still_current.scope),
        "the heal-renew keeps the grant marked bounded for future rotations"
    );
}

/// A holder that resolves from the roster WITHOUT an ML-KEM ek heals
/// **classical** (matching its mint-time posture) — the wraps unseal with the
/// classical opener. This is distinct from an *unresolved* holder (next test),
/// which is skipped: a resolved-classical holder is not a PQ downgrade.
#[tokio::test]
async fn rotation_heals_a_classical_holder_without_an_ek() {
    let common::Fixture {
        machine,
        nest,
        config: cfg,
        mail,
    } = common::fixture();
    enable_and_rotate(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    let (holder_sk, holder_pk) = generate_x25519_keypair();
    nest.state().content_processor_holders = vec![HolderInfo {
        pubkey: holder_pk,
        mlkem_ek: None, // classical-only holder
        role: "mda".into(),
        bridge_id: "mda-1".into(),
    }];

    let now = Timestamp::now_secs() as u64;
    let window_start = now - EPOCH_SECS;
    let window_end = now + EPOCH_SECS;
    let grant_id = [0xC0; 16];
    seed_bounded_grant(&cfg, grant_id, holder_pk, window_start, window_end);

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotation succeeds");

    let stored_mail = mail.current();
    let post_msek = stored_mail.msek.clone().unwrap();
    let e_boundary = mail_sealing_epoch_of(
        stored_mail
            .prior_msek_retired_at(&pre_msek)
            .expect("retirement instant"),
    );

    let renews = nest.state().renewed_grants.clone();
    assert_eq!(renews.len(), 1, "the classical holder still heals");
    let (_, _, _, appended) = &renews[0];
    let wraps: Vec<WrappedScopeKey> = appended
        .iter()
        .map(|b| WrappedScopeKey::from_canonical_bytes(b).unwrap())
        .collect();
    let boundary = wraps
        .iter()
        .find(|w| w.epoch == Some(e_boundary))
        .expect("boundary wrap");
    // Classical opener (no ML-KEM decaps key) reads the boundary payload.
    let opened = unseal_capability(boundary, &ACTOR, &holder_sk).expect("classical unseal");
    let new_root = derive_mail_epoch_root(&post_msek);
    let old_root = derive_mail_epoch_root(&pre_msek);
    let want = [
        derive_recipient_mail_epoch_capability_secret_from_root(&new_root, e_boundary),
        derive_recipient_mail_epoch_capability_secret_from_root(&old_root, e_boundary),
    ]
    .concat();
    assert_eq!(opened, want, "classical boundary payload derivation-checks");
}

/// A failed heal-renew must NOT fail the rotation — the heal is best-effort /
/// log-only, so the grant simply heals at its next renew instead. The rotation
/// itself (MSEK swap, sentinel clear) still commits.
#[tokio::test]
async fn a_failed_heal_renew_does_not_fail_the_rotation() {
    let common::Fixture {
        machine,
        nest,
        config: cfg,
        mail,
    } = common::fixture();
    enable_and_rotate(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    let (_holder_sk, holder_pk) = generate_x25519_keypair();
    nest.state().content_processor_holders = vec![HolderInfo {
        pubkey: holder_pk,
        mlkem_ek: None,
        role: "mda".into(),
        bridge_id: "mda-1".into(),
    }];
    // Arm the renew to fail.
    nest.state().fail_renew_grant_with = Some(fauna_client_mail_settings::NestError::Transient(
        "simulated renew failure".into(),
    ));

    let now = Timestamp::now_secs() as u64;
    seed_bounded_grant(
        &cfg,
        [0xD0; 16],
        holder_pk,
        now - EPOCH_SECS,
        now + EPOCH_SECS,
    );

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("a failed heal never fails the rotation");

    let stored = cfg.current();
    let stored_mail = mail.current();
    assert!(stored_mail.pending_rotation.is_none(), "rotation finalized");
    assert_ne!(
        pre_msek,
        stored_mail.msek.clone().unwrap(),
        "the MSEK swap committed despite the heal failure"
    );
    // No Renew event was recorded (the failed renew is not logged as healed).
    assert!(
        !stored
            .grant_events
            .iter()
            .any(|e| e.kind == GrantEventKind::Renew),
        "a failed heal records no Renew event"
    );
}

/// A **master** (unmarked) mail grant is left untouched by the heal driver —
/// only bounded (epoch-tiered) grants heal at rotation.
#[tokio::test]
async fn a_master_mail_grant_is_not_healed() {
    let common::Fixture {
        machine,
        nest,
        config: cfg,
        mail: _,
    } = common::fixture();
    enable_and_rotate(&machine).await;

    let (_holder_sk, holder_pk) = generate_x25519_keypair();
    nest.state().content_processor_holders = vec![HolderInfo {
        pubkey: holder_pk,
        mlkem_ek: None,
        role: "mda".into(),
        bridge_id: "mda-1".into(),
    }];
    let now = Timestamp::now_secs() as u64;
    seed_master_mail_grant(
        &cfg,
        [0xE0; 16],
        holder_pk,
        now - EPOCH_SECS,
        now + EPOCH_SECS,
    );

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotation succeeds");

    assert!(
        nest.state().renewed_grants.is_empty(),
        "a master mail grant is never epoch-healed"
    );
}

/// A bounded grant whose holder is NOT in the live roster is skipped — never a
/// classical-blind re-wrap (a PQ-downgrade guard: a heal without the holder's
/// resolved key material could void a hybrid grant's coverage via
/// replace-on-append). The rotation still succeeds.
#[tokio::test]
async fn an_unresolved_holder_is_skipped() {
    let common::Fixture {
        machine,
        nest,
        config: cfg,
        mail,
    } = common::fixture();
    enable_and_rotate(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    // Empty roster: the seeded grant's holder cannot be resolved.
    nest.state().content_processor_holders = Vec::new();
    let (_holder_sk, holder_pk) = generate_x25519_keypair();
    let now = Timestamp::now_secs() as u64;
    seed_bounded_grant(
        &cfg,
        [0xF0; 16],
        holder_pk,
        now - EPOCH_SECS,
        now + EPOCH_SECS,
    );

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotation succeeds even with no resolvable holder");

    assert!(
        nest.state().renewed_grants.is_empty(),
        "an unresolved holder is skipped, not healed classical-blind"
    );
    let stored_mail = mail.current();
    assert!(stored_mail.pending_rotation.is_none(), "rotation finalized");
    assert_ne!(
        pre_msek,
        stored_mail.msek.clone().unwrap(),
        "MSEK still rotated"
    );
}
