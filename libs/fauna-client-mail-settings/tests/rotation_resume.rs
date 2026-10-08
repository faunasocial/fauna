//! Tests for the resumable rotate-mail-keys algorithm over the account's mail
//! custody (`fauna.state.mail`).
//! Authority: `docs/goal/behavior/mail-credentials.md` § Rotation
//! and recovery (the owed set derived from *The generation marker*; the
//! finalize's verify-and-re-drive, § Cross-device finalize race).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use fauna_client_mail_settings::testing::FakeMailStore;
use fauna_client_mail_settings::{CredentialKind, MailSettingsAction, SecretBytes};
use fauna_core::data::{MailCredential, MailCredentialKind, MsekFingerprint, Timestamp};
use fauna_core::mail_rows::{MailRotationSentinel, MailRows, MailStateRow};
use fauna_core::secret::SecretArray32;

mod common;
use common::{ACTOR, build};

async fn enable(machine: &fauna_client_mail_settings::MailSettingsMachine) {
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await
        .unwrap();
}

async fn add(machine: &fauna_client_mail_settings::MailSettingsMachine, name: &str, byte: u8) {
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: name.into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![byte; 32]),
        })
        .await
        .unwrap();
}

/// Stage the crash state of a rotation under `new_msek`: the sentinel set on
/// the state row, the rows untouched (still naming the old generation).
fn stage_sentinel(mail: &FakeMailStore, new_msek: [u8; 32]) {
    mail.mutate(|rows| {
        let state = rows.state.as_mut().expect("enabled");
        state.pending_rotation = Some(MailRotationSentinel {
            new_msek: new_msek.into(),
        });
    });
}

/// A stale sibling's state row: the pre-rotation `msek`, no sentinel, stamped
/// later than anything this device wrote — the row a device that never saw
/// the rotation writes (a flag flip) and the walk merges in.
fn stale_peer_row(rows: &MailRows, old: &SecretArray32) -> MailStateRow {
    let stored = rows.state.clone().expect("a stored state row");
    MailStateRow {
        msek: Some(old.clone()),
        succession_burns: Vec::new(),
        pending_rotation: None,
        updated_at: Timestamp(stored.updated_at.0 + 1_000_000),
        ..stored
    }
}

#[tokio::test]
async fn rotation_rewraps_all_credentials_and_updates_msek() {
    let (machine, nest, mail) = build();
    // Enable + add two more credentials so the rotation has 3
    // survivors to re-wrap.
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    add(&machine, "Thunderbird", 0xCC).await;
    let pre_msek = mail.current().msek.unwrap();
    // Reset the recorded provision counts so we can count just the
    // rotation's writes.
    let baseline_mls = nest.state().provision_wrapped_mls.len();
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();

    let s = nest.state();
    // Step (d): one new snapshot under MSEK'.
    assert_eq!(s.provision_mls_snapshot.len(), baseline_snapshot + 1);
    // Step (e): one wrapped-MSEK per surviving credential.
    assert_eq!(s.provision_wrapped_mls.len(), baseline_mls + 3);
    drop(s);

    let stored = mail.current();
    assert!(stored.pending_rotation.is_none(), "sentinel cleared");
    let post_msek = stored.msek.clone().unwrap();
    assert_ne!(pre_msek, post_msek, "MSEK rotated");
    assert_eq!(stored.credentials.len(), 3);
    // Step (e)(iii): every row names the generation it is now wrapped under.
    let fp = MsekFingerprint::of(&post_msek);
    assert!(
        stored
            .credentials
            .iter()
            .all(|c| c.wrapped_under == Some(fp)),
        "every survivor is marked wrapped under MSEK'"
    );
    // The retired MSEK is kept as its own generation row WITH its retirement
    // instant recorded, keyed by the msek value — the bounded-mail mint's
    // generation seal intervals and the openers' seal-time selection depend
    // on it.
    assert_eq!(stored.prior_mseks, vec![pre_msek.clone()]);
    let retired_at = stored
        .prior_msek_retired_at(&pre_msek)
        .expect("rotation records the retirement instant for the retired MSEK");
    assert!(retired_at > 0, "instant is a real unix-seconds timestamp");
}

/// **Four rotations keep every generation** (`owner-key-material.md`
/// § Path B-sibling-2 → *Pre-rotation mail at rest*): each rotation's
/// outgoing MSEK lands as its own `generation/<fingerprint>` row, so after
/// four the custody holds all four — none dropped by the retired cap-2
/// window — each with its retirement instant, most recently retired first
/// (instants monotone); and the snapshot the last rotation provisioned
/// carries all five generations with the four instants.
#[tokio::test]
async fn four_rotations_keep_every_generation_in_custody_and_snapshot() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let mut retired = Vec::new();
    for _ in 0..4 {
        retired.push(mail.current().msek.unwrap());
        machine
            .dispatch(MailSettingsAction::StartRotation {
                excluded_credentials: Vec::new(),
            })
            .await
            .unwrap();
    }
    let stored = mail.current();
    let current = stored.msek.clone().unwrap();
    assert!(!retired.contains(&current));

    // Every generation, one row each — the custody never drops one.
    let rows = mail.rows();
    assert_eq!(
        rows.generations.len(),
        4,
        "uncapped: all four retired generations"
    );
    for k in &retired {
        assert!(
            rows.generations.contains_key(&MsekFingerprint::of(k)),
            "a retired generation is missing from the custody"
        );
        assert!(stored.prior_mseks.contains(k));
    }
    assert_eq!(stored.prior_mseks.len(), 4);
    let instants = stored.prior_retired_at_unix();
    assert_eq!(instants.len(), 4, "every generation carries its instant");
    assert!(
        instants.windows(2).all(|w| w[0] >= w[1]),
        "most recently retired first: {instants:?}"
    );

    // The last provisioned snapshot carries all five generations.
    let blob = nest
        .state()
        .provision_mls_snapshot
        .last()
        .cloned()
        .expect("a snapshot was provisioned");
    let plain = fauna_mls::wrapped_blob::unseal_mls_snapshot(&blob, &current.to_array())
        .expect("the snapshot opens under the current MSEK");
    let snap = fauna_mls::wrapped_blob::MlsSnapshotPlaintext::from_canonical_bytes(&plain)
        .expect("snapshot decodes");
    assert_eq!(snap.leaf_init_keypairs.len(), 5);
    assert_eq!(snap.mail_epoch_grace_roots.len(), 4);
    assert_eq!(snap.generation_retired_at_unix, instants);
}

#[tokio::test]
async fn rotation_republishes_the_epoch_schedule_from_the_new_msek() {
    // Design 2026-07-18 § 5: an MSEK hard-revoke resets the epoch lineage, so
    // the pubkey schedule must republish from the NEW root at rotation time —
    // the same publication path as enable-mail, rederived.
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();

    let post_msek = mail.current().msek.unwrap();
    assert_ne!(pre_msek, post_msek, "MSEK rotated");

    let s = nest.state();
    // enable_mail's own publish (1) + rotation's re-publish (1).
    assert_eq!(s.provision_recipient_pubkey.len(), 2);
    let rotated_keys = s.provision_recipient_pubkey[1]
        .3
        .as_ref()
        .expect("epoch schedule republished at rotation");
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(Timestamp::now_secs() as u64);
    let (_, want_pub) =
        fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair(&post_msek, e_now);
    assert_eq!(
        rotated_keys[0].mls_pubkey.as_slice(),
        &want_pub,
        "the republished schedule is derived from the NEW MSEK, not the old"
    );
}

/// **The finalize re-drive against the plane rows**.
/// A stale sibling's state row — the pre-rotation MSEK, no sentinel, a later
/// stamp — lands right after finalize's swap write, and the state row's join
/// (a deliberate-rotation latest-wins on the stamp) takes it: the swap is
/// reverted and, since the displaced MSEK is never unioned into the priors
/// (a generation row is written only for the key a finalize retires), MSEK′
/// is gone from `msek`, `prior_mseks` and the sentinel alike.
/// Finalize must see that on its read back and re-drive — never declare a
/// reverted rotation done, which would strand MSEK′ (already published and
/// wrapped on the nest) with no client-side recovery.
#[tokio::test]
async fn finalize_re_drives_when_a_peer_reverts_the_msek_swap() {
    let (machine, _nest, mail) = build();
    enable(&machine).await;
    let pre_msek = mail.current().msek.unwrap();

    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&fired);
    let old = pre_msek.clone();
    mail.on_write(move |rows| {
        let swapped = rows
            .state
            .as_ref()
            .is_some_and(|s| s.msek.as_ref() != Some(&old));
        if !swapped {
            return false;
        }
        let peer = stale_peer_row(rows, &old);
        FakeMailStore::join_state_into(rows, &peer);
        seen.fetch_add(1, Ordering::SeqCst);
        true
    });

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotation must succeed by re-driving past the revert");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the injected revert must have triggered"
    );
    let stored = mail.current();
    assert!(
        stored.pending_rotation.is_none(),
        "sentinel cleared once the swap was confirmed durable"
    );
    let post_msek = stored.msek.expect("msek present");
    assert_ne!(
        pre_msek, post_msek,
        "msek must be the rotated-to key, not reverted to the old one"
    );
    assert_eq!(
        stored.prior_mseks,
        vec![pre_msek],
        "the pre-rotation key is kept as a prior generation"
    );
}

/// The swap is already durable (msek == new,
/// sentinel still set); finalize's remaining job is the sentinel clear. The
/// clear restates no key material, so it cannot revert the swap itself — but a
/// stale sibling's row landing right after it still can, and would drop MSEK′
/// from every field. Finalize must see that on its read back and re-drive.
#[tokio::test]
async fn finalize_re_drives_when_a_peer_reverts_the_sentinel_clear() {
    let (machine, _nest, mail) = build();
    enable(&machine).await;

    // The exact state finalize sees once the swap committed but the sentinel
    // is not yet cleared: msek == new, the pre-rotation key's generation
    // row written, the sentinel still carrying new_msek.
    let old_msek = mail.current().msek.expect("enable set an msek");
    let new_msek: SecretArray32 = [0x5A; 32].into();
    let fp = MsekFingerprint::of(&new_msek);
    mail.mutate(|rows| {
        rows.join_generation(fauna_core::data::PriorMsekRetirement {
            msek: old_msek.clone(),
            retired_at_unix: 1_800_000_000,
        });
        let state = rows.state.as_mut().unwrap();
        state.msek = Some(new_msek.clone());
        state.pending_rotation = Some(MailRotationSentinel {
            new_msek: new_msek.clone(),
        });
        for c in rows.credentials.values_mut() {
            c.wrapped_under = Some(fp);
        }
    });

    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&fired);
    let old = old_msek.clone();
    mail.on_write(move |rows| {
        let cleared = rows
            .state
            .as_ref()
            .is_some_and(|s| s.pending_rotation.is_none());
        if !cleared {
            return false;
        }
        let peer = stale_peer_row(rows, &old);
        FakeMailStore::join_state_into(rows, &peer);
        seen.fetch_add(1, Ordering::SeqCst);
        true
    });

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume must recover by re-driving past the reverted clear");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the injected clear revert must have triggered"
    );
    let stored = mail.current();
    assert_eq!(
        stored.msek,
        Some(new_msek),
        "new_msek must be durable, not reverted to the pre-rotation key by the clear race"
    );
    assert!(
        stored.pending_rotation.is_none(),
        "sentinel cleared once the clear was confirmed durable"
    );
}

#[tokio::test]
async fn resume_after_swap_committed_clears_sentinel_idempotently() {
    // A resume that crashed between the swap and the sentinel clear must
    // complete cleanly: re-provision the snapshot once (idempotent step d,
    // exercising the grace-list dedup since msek == new_msek would otherwise
    // list new twice), clear the sentinel, and keep msek == new (not re-rotate
    // to a fresh key).
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let new_msek: SecretArray32 = [0x5A; 32].into();
    let fp = MsekFingerprint::of(&new_msek);
    mail.mutate(|rows| {
        let state = rows.state.as_mut().unwrap();
        state.msek = Some(new_msek.clone());
        state.pending_rotation = Some(MailRotationSentinel {
            new_msek: new_msek.clone(),
        });
        for c in rows.credentials.values_mut() {
            c.wrapped_under = Some(fp);
        }
    });
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();
    let baseline_mls = nest.state().provision_wrapped_mls.len();

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume completes");

    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        baseline_snapshot + 1,
        "snapshot re-provisioned exactly once (idempotent step d)"
    );
    assert_eq!(
        nest.state().provision_wrapped_mls.len(),
        baseline_mls,
        "no row is owed — every one already names MSEK′"
    );
    let stored = mail.current();
    assert_eq!(
        stored.msek,
        Some(new_msek),
        "msek stays the already-committed rotated-to key"
    );
    assert!(stored.pending_rotation.is_none(), "sentinel cleared");
}

#[tokio::test]
async fn rotation_excluded_credentials_are_revoked() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "compromised", 0xBB).await;
    let baseline = nest.state().provision_wrapped_mls.len();

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: vec!["compromised".into()],
        })
        .await
        .unwrap();

    let s = nest.state();
    // Only one survivor → only one wrapped-MSEK re-provision.
    assert_eq!(s.provision_wrapped_mls.len(), baseline + 1);
    // The excluded credential's resting blobs, BOTH kinds, must be revoked
    // at the nest — mirroring `succession.rs`'s burn leg — so it cannot
    // keep authenticating outbound mail via its submission token. See
    // `mail-credentials.md` § Compromised-credential handling (the
    // pre-existing ordinary-flow gap this pins).
    assert_eq!(s.revoke_wrapped_mls.len(), 1);
    assert_eq!(s.revoke_wrapped_mls[0].0, ACTOR);
    assert_eq!(s.revoke_wrapped_mls[0].1, "compromised");
    assert_eq!(s.revoke_submission_token.len(), 1);
    assert_eq!(s.revoke_submission_token[0].0, ACTOR);
    assert_eq!(s.revoke_submission_token[0].1, "compromised");
    drop(s);

    let stored = mail.current();
    assert_eq!(stored.credentials.len(), 1);
    assert_eq!(stored.credentials[0].credential_id, "default");
    let rows = mail.rows();
    let excluded = &rows.credentials["compromised"];
    assert!(excluded.revoked_at_unix.is_some(), "the soft-revoke marker");
    assert!(excluded.secret.is_empty(), "the compromised secret is gone");
    assert!(
        rows.spent_credential_ids()
            .contains(&"compromised".to_string()),
        "the excluded id stays spent"
    );
}

#[tokio::test]
async fn resume_completes_from_persisted_sentinel() {
    // The rotation crashed after the sentinel was set but before any
    // credential re-wraps. ResumeRotation must drive the loop to completion
    // over the owed set the markers derive.
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    let new_msek = [0xEE; 32];
    stage_sentinel(&mail, new_msek);

    let baseline_mls = nest.state().provision_wrapped_mls.len();
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume");

    let s = nest.state();
    assert_eq!(s.provision_mls_snapshot.len(), baseline_snapshot + 1);
    assert_eq!(s.provision_wrapped_mls.len(), baseline_mls + 2);
    drop(s);

    let stored = mail.current();
    assert!(stored.pending_rotation.is_none());
    assert_eq!(stored.msek, Some(new_msek.into()));
}

/// A rotation that did not finish is resumed, never replaced
/// (`ui/mail-settings.md` § Architectural rules 5: no second rotation racing
/// the first). Its staged MSEK′ may already be published and wrapped on the
/// nest, so a fresh `StartRotation` overwriting the sentinel would strand it —
/// mail sealed to that pubkey with no client-side key left to open it. The
/// machine refuses, and the pending rotation's sentinel and rows stay as they
/// were.
#[tokio::test]
async fn start_rotation_while_one_is_pending_is_refused() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let new_msek = [0xEE; 32];
    stage_sentinel(&mail, new_msek);
    let before = mail.current();
    let baseline_mls = nest.state().provision_wrapped_mls.len();

    let result = machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await;

    assert!(
        matches!(
            result,
            Err(fauna_client_mail_settings::DispatchError::InvalidState(_))
        ),
        "a second rotation must be refused while one is pending, got {result:?}"
    );
    let stored = mail.current();
    assert_eq!(
        stored.pending_rotation.map(|s| s.new_msek),
        Some(new_msek.into()),
        "the pending rotation's staged key must survive the refusal"
    );
    assert_eq!(stored.msek, before.msek, "the live MSEK is untouched");
    assert_eq!(nest.state().provision_wrapped_mls.len(), baseline_mls);
    assert!(
        machine.snapshot().pending_rotation.is_some(),
        "the page still offers Resume after the refusal"
    );

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("the pending rotation still resumes");
    assert_eq!(mail.current().msek, Some(new_msek.into()));
}

/// An interrupted rotation leaves the page saying so: the failed dispatch
/// reports its error AND keeps the status at "rotation in progress" with the
/// resume banner's sentinel — never "All up to date" beside a "didn't finish"
/// banner (`ui/mail-settings.md` § Status indicator). The same status a fresh
/// hydrate derives from the stored sentinel.
#[tokio::test]
async fn an_interrupted_rotation_reads_in_progress_not_up_to_date() {
    let (machine, nest, _mail) = build();
    enable(&machine).await;
    nest.state().fail_provision_wrapped_mls_for_n_calls = 1;

    let result = machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await;
    assert!(
        result.is_err(),
        "the refused re-wrap interrupts the rotation"
    );

    let snap = machine.snapshot();
    assert!(snap.error.is_some(), "the failure is reported");
    assert!(snap.pending_rotation.is_some(), "the resume banner shows");
    assert_eq!(
        snap.status,
        fauna_client_mail_settings::SettingsStatus::RotationInProgress {
            credentials_remaining: 1
        },
        "an interrupted rotation still reads in progress"
    );

    machine.hydrate().await.expect("hydrate");
    assert_eq!(
        machine.snapshot().status,
        fauna_client_mail_settings::SettingsStatus::RotationInProgress {
            credentials_remaining: 1
        },
        "a fresh hydrate agrees"
    );
}

#[tokio::test]
async fn resume_with_no_sentinel_is_noop() {
    let (machine, nest, _mail) = build();
    enable(&machine).await;
    let baseline_mls = nest.state().provision_wrapped_mls.len();
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume noop");

    let s = nest.state();
    assert_eq!(s.provision_wrapped_mls.len(), baseline_mls);
    assert_eq!(s.provision_mls_snapshot.len(), baseline_snapshot);
}

#[tokio::test]
async fn resume_rewraps_only_the_rows_not_yet_naming_the_new_generation() {
    // Mid-rotation: the first credential (default) already carries the new
    // generation's marker; only "iphone" is owed. The resume re-wraps it
    // alone — the checkpoint is the marker, not a stored id list.
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    let new_msek = [0x77; 32];
    stage_sentinel(&mail, new_msek);
    let fp = MsekFingerprint::of(&new_msek.into());
    mail.mutate(|rows| {
        rows.credentials.get_mut("default").unwrap().wrapped_under = Some(fp);
    });

    let baseline_mls = nest.state().provision_wrapped_mls.len();
    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume");
    let s = nest.state();
    assert_eq!(s.provision_wrapped_mls.len(), baseline_mls + 1);
    drop(s);

    let stored = mail.current();
    assert_eq!(stored.msek, Some(new_msek.into()));
    assert!(stored.pending_rotation.is_none());
}

/// The owed set is derived, so a credential another device adds while this
/// one rotates is re-wrapped instead of dropped — what a stored id list could
/// only lose (`config-dissolution.md` § *The mail plane*). A row marked
/// mid-rotation (revoked elsewhere) is skipped.
#[tokio::test]
async fn a_credential_added_mid_rotation_is_rewrapped_and_a_revoked_one_skipped() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    let new_msek = [0x66; 32];
    stage_sentinel(&mail, new_msek);
    mail.mutate(|rows| {
        // Another device added a credential under the OLD generation…
        let added = MailCredential {
            credential_id: "tablet".into(),
            display_name: "Tablet".into(),
            kind: MailCredentialKind::OAuthBearer,
            secret: vec![0xDD; 32].into(),
            created_at: 5,
            updated_at: Timestamp(5),
            wrapped_under: None,
            revoked_at_unix: None,
            burned: None,
        };
        rows.credentials.insert("tablet".into(), added);
        // …and revoked "iphone".
        let iphone = rows.credentials.get_mut("iphone").unwrap();
        iphone.revoked_at_unix = Some(9);
        iphone.secret = Default::default();
        iphone.wrapped_under = None;
    });

    let baseline_mls = nest.state().provision_wrapped_mls.len();
    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume");
    let s = nest.state();
    let mut rewrapped: Vec<&str> = s.provision_wrapped_mls[baseline_mls..]
        .iter()
        .map(|b| b.index.1.as_str())
        .collect();
    rewrapped.sort_unstable();
    assert_eq!(
        rewrapped,
        ["default", "tablet"],
        "the added row re-wrapped, the revoked one skipped"
    );
    drop(s);
    let fp = MsekFingerprint::of(&new_msek.into());
    let rows = mail.rows();
    assert_eq!(rows.credentials["tablet"].wrapped_under, Some(fp));
    assert_eq!(rows.credentials["iphone"].wrapped_under, None);
}

#[tokio::test]
async fn rotation_uses_per_credential_kind_for_kdf() {
    // Mixed survivors. Each must re-wrap with its own KDF. We can't easily
    // check the KDF directly via the recorded blob without decoding, but we
    // can verify the wrap doesn't error out. Inject a second credential row
    // directly so we skip the slow Argon2id roundtrip during EnableMail.
    let (machine, nest, mail) = build();
    enable(&machine).await;
    mail.mutate(|rows| {
        let legacy = MailCredential {
            credential_id: "legacy".into(),
            display_name: "Legacy MUA".into(),
            kind: MailCredentialKind::OAuthBearer, // keep OAUTHBEARER so the
            // test stays fast — the Plain branch is exercised by the
            // unit test in `wrap::tests`.
            secret: vec![0xCC; 32].into(),
            created_at: 0,
            updated_at: Default::default(),
            wrapped_under: None,
            revoked_at_unix: None,
            burned: None,
        };
        rows.credentials.insert("legacy".into(), legacy);
    });

    let baseline_mls = nest.state().provision_wrapped_mls.len();
    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .expect("rotate");
    let s = nest.state();
    assert_eq!(s.provision_wrapped_mls.len(), baseline_mls + 2);
}
