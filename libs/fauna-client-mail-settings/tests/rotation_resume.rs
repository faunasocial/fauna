//! Tests for the resumable rotate-mail-keys algorithm over the account's mail
//! custody (`fauna.state.mail`).
//! Authority: `docs/goal/behavior/mail-credentials.md` § Rotation
//! and recovery (the owed set derived from *The generation marker*; the
//! finalize's verify-and-re-drive, § Cross-device finalize race).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use fauna_client_mail_settings::testing::FakeMailStore;
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, RetirementClock, SecretBytes,
};
use fauna_core::data::{MailCredential, MailCredentialKind, MsekFingerprint, Timestamp};
use fauna_core::mail_rows::{MailRotationSentinel, MailRows, MailStateRow};
use fauna_core::secret::SecretArray32;

mod common;
use common::{ACTOR, build, machine_over};

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

/// The retirement instant a pinned clock reads (unix seconds, 2027-01-15).
const PINNED_AT: u64 = 1_800_000_000;

/// A retirement clock that reads `at` every time: step (d) and finalize stamp
/// the same instant, so a rotation writes its snapshot exactly once.
fn pinned(at: u64) -> RetirementClock {
    Arc::new(move || at)
}

/// A retirement clock one second later on every read: the re-wrap loop
/// crossing a clock-second boundary between step (d) and finalize.
fn stepping(from: u64) -> RetirementClock {
    let next = Arc::new(AtomicU64::new(from));
    Arc::new(move || next.fetch_add(1, Ordering::SeqCst))
}

/// The retirement instants the snapshot provisioned `back` writes before the
/// last carries (`0` = the last), opened under `msek`.
fn snapshot_retired_at(
    nest: &fauna_client_mail_settings::testing::FakeNestClient,
    back: usize,
    msek: &SecretArray32,
) -> Vec<u64> {
    let s = nest.state();
    let blob = &s.provision_mls_snapshot[s.provision_mls_snapshot.len() - 1 - back];
    let plain = fauna_mls::wrapped_blob::unseal_mls_snapshot(blob, &msek.to_array())
        .expect("the snapshot opens under the current MSEK");
    fauna_mls::wrapped_blob::MlsSnapshotPlaintext::from_canonical_bytes(&plain)
        .expect("snapshot decodes")
        .generation_retired_at_unix
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
    // Step (d) and finalize read one instant — the count below is the
    // mechanism's, not the box clock's (the boundary case is
    // `a_rotation_across_a_clock_second_writes_the_snapshot_twice`).
    let machine = machine.with_retirement_clock(pinned(PINNED_AT));
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
    assert_eq!(
        retired_at, PINNED_AT,
        "the instant the rotation's clock read"
    );
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

/// **The snapshot carries the instant finalize RECORDED, not step (d)'s
/// clock read.** Step (d) provisions the snapshot with a fresh clock read for
/// the outgoing generation; finalize then records its own instant — or keeps
/// one already recorded (a re-drive or a resume after a crash between the
/// generation row's write and the swap). The re-wrap loop between the two
/// takes seconds to minutes, so the snapshot must be re-provisioned after
/// finalize or the MDA's seal-time trial order reads a different instant than
/// the custody. Seeding a recorded instant makes the two reads differ on
/// purpose; without the post-finalize re-provision this failed only when the
/// clock crossed a second inside one rotation (public CI, 2026-10-08).
#[tokio::test]
async fn the_snapshot_carries_the_instant_finalize_recorded() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let old = mail.current().msek.unwrap();
    mail.mutate(|rows| {
        rows.generations.insert(
            MsekFingerprint::of(&old),
            fauna_core::data::PriorMsekRetirement {
                msek: old.clone(),
                retired_at_unix: 7,
            },
        );
    });
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();
    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();
    let stored = mail.current();
    assert_eq!(
        stored.prior_msek_retired_at(&old),
        Some(7),
        "finalize kept the recorded instant"
    );
    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        baseline_snapshot + 2,
        "step (d)'s write, then the re-provision carrying the recorded instant"
    );
    let current = stored.msek.clone().unwrap();
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
    assert_eq!(
        snap.generation_retired_at_unix,
        stored.prior_retired_at_unix()
    );
}

/// **A rotation whose re-wrap loop crosses a clock-second writes the snapshot
/// twice, the last carrying the instant finalize recorded.** Step (d) stamps
/// the outgoing generation at `T`; finalize, reading the clock a second later,
/// records `T + 1`; so the post-finalize re-provision runs. The clock steps by
/// hand, so this holds on every run — the shape public CI hit by chance
/// (2026-10-17, 3 snapshot writes where a same-second run makes 2).
#[tokio::test]
async fn a_rotation_across_a_clock_second_writes_the_snapshot_twice() {
    let (machine, nest, mail) = build();
    let machine = machine.with_retirement_clock(stepping(PINNED_AT));
    enable(&machine).await;
    let old = mail.current().msek.unwrap();
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        baseline_snapshot + 2,
        "step (d)'s write, then the re-provision carrying the recorded instant"
    );
    let stored = mail.current();
    assert_eq!(
        stored.prior_msek_retired_at(&old),
        Some(PINNED_AT + 1),
        "finalize recorded its own, later read"
    );
    let current = stored.msek.clone().unwrap();
    assert_eq!(
        snapshot_retired_at(&nest, 1, &current),
        vec![PINNED_AT],
        "step (d)'s snapshot carried step (d)'s read"
    );
    assert_eq!(
        snapshot_retired_at(&nest, 0, &current),
        vec![PINNED_AT + 1],
        "the last snapshot carries the instant finalize recorded"
    );
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
/// clear names MSEK′ and touches nothing else, so it cannot revert the swap
/// itself, and a stale sibling's flag write cannot either (its absent MSEK is
/// no change at the door) — but the fake's raw peer join below stands in for
/// what still can, another device's own swap landing right after the clear,
/// which would drop MSEK′ from every field. Finalize must see that on its read
/// back and re-drive.
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
    // One instant for step (d) and finalize: exactly one snapshot write.
    let machine = machine.with_retirement_clock(pinned(PINNED_AT));
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
            token_minted_at: None,
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
            token_minted_at: None,
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

/// Whether the snapshot the nest holds last opens under `msek`.
fn snapshot_opens_under(
    nest: &fauna_client_mail_settings::testing::FakeNestClient,
    msek: &SecretArray32,
) -> bool {
    let s = nest.state();
    let last = s
        .provision_mls_snapshot
        .last()
        .expect("a snapshot was provisioned");
    fauna_mls::wrapped_blob::unseal_mls_snapshot(last, &msek.to_array()).is_ok()
}

/// **A relaunch right after the rotate form closes** (`mail-credentials.md`
/// § Hard revoke → *Published before the nest moves*). The relaunched app
/// hydrates its custody from the nest, then runs the connect-time refresh,
/// which re-seals the snapshot under the custody's `msek`. Were the swap
/// still unsent when the app died, the fresh replica would read the
/// pre-rotation MSEK and seal the snapshot under it while every credential's
/// blob is wrapped under MSEK′ — the MDA unwraps MSEK′ and the snapshot fails
/// its AEAD (`decrypt mls-snapshot: AEAD verify failed`), the tier_3 finding
/// this pins. The rotation publishes the custody before it returns, so the
/// replica reads MSEK′ and the refresh re-seals under it.
#[tokio::test]
async fn a_relaunch_right_after_rotation_reseals_the_snapshot_under_the_new_key() {
    use fauna_client_config::MailStore as _;
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "Thunderbird", 0xCC).await;
    // Enabled long ago: the pump has published it.
    mail.publish().await.unwrap();

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();
    let rotated = mail.current().msek.unwrap();
    assert!(
        snapshot_opens_under(&nest, &rotated),
        "the rotation sealed under MSEK′"
    );

    let fresh = mail.fresh_replica();
    let custody = fresh.current();
    assert_eq!(
        (custody.msek.as_ref(), custody.pending_rotation.is_none()),
        (Some(&rotated), true),
        "the nest holds the finished rotation, so the relaunched page reads \
         up to date rather than offering a resume"
    );
    let relaunched = machine_over(&nest, &fresh);
    relaunched.refresh_epoch_schedule().await.unwrap();

    assert!(
        snapshot_opens_under(&nest, &rotated),
        "the relaunched app's refresh must not re-seal the snapshot under a \
         generation the credentials are not wrapped under"
    );
}

/// The standing recipient pubkey the nest holds for this actor now — the last
/// provision's (an atomic replace).
fn nest_pubkey(nest: &fauna_client_mail_settings::testing::FakeNestClient) -> Option<[u8; 32]> {
    nest.state()
        .provision_recipient_pubkey
        .iter()
        .rev()
        .find(|r| r.0 == ACTOR)
        .map(|r| r.1)
}

/// The standing recipient pubkey `msek` derives.
fn pubkey_of(msek: &SecretArray32) -> [u8; 32] {
    fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&msek.to_array()).1
}

/// **A stale sibling's connect-time refresh** (`mail-credentials.md` § Hard
/// revoke → *Published before the nest moves*, the stale-sibling rule). The
/// sibling listed the fleet scope before this device rotated and has not
/// walked since, so it reads the pre-rotation `msek` with no sentinel — the
/// read gate lets it through, because it has listed. A reconnect in the same
/// process (a laptop waking) runs the refresh before the runtime's own walk:
/// re-sealing under that custody would strand every credential the rotation
/// re-wrapped under MSEK′. The nest's recipient pubkey names a generation the
/// sibling's custody has never held, so the refresh walks first — and moves
/// nothing when it cannot walk, re-sealing under MSEK′ once it can.
#[tokio::test]
async fn a_stale_sibling_refresh_never_reseals_under_a_generation_it_has_not_walked() {
    use fauna_client_config::MailStore as _;
    let (machine, nest, mail) = build();
    enable(&machine).await;
    add(&machine, "Thunderbird", 0xCC).await;
    mail.publish().await.unwrap();
    // The sibling lists the fleet scope now, before the rotation.
    let sibling = mail.stale_replica();
    let old = sibling.current().msek.expect("enabled");

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();
    let rotated = mail.current().msek.unwrap();
    assert_ne!(old, rotated);
    let custody = sibling.current();
    assert_eq!(
        (custody.msek.as_ref(), custody.pending_rotation.is_none()),
        (Some(&old), true),
        "the sibling has not walked: it reads the pre-rotation custody, no sentinel"
    );

    // A runtime that cannot walk now: the refresh moves nothing.
    let stale = machine_over(&nest, &sibling);
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();
    let baseline_pubkey = nest.state().provision_recipient_pubkey.len();
    sibling.refuse_next_walks(1);
    stale.refresh_epoch_schedule().await.unwrap();
    assert!(
        snapshot_opens_under(&nest, &rotated),
        "a stale sibling's refresh must not re-seal the snapshot under a \
         generation the credentials are not wrapped under"
    );
    let moved = {
        let s = nest.state();
        (
            s.provision_mls_snapshot.len(),
            s.provision_recipient_pubkey.len(),
        )
    };
    assert_eq!(
        moved,
        (baseline_snapshot, baseline_pubkey),
        "nothing moved on the nest while the custody could not be walked"
    );

    // Once the walk can be taken, the refresh reads MSEK′ and re-seals under it.
    stale.refresh_epoch_schedule().await.unwrap();
    assert_eq!(sibling.current().msek.as_ref(), Some(&rotated), "walked");
    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        baseline_snapshot + 1,
        "the refresh ran on the walked custody"
    );
    assert!(snapshot_opens_under(&nest, &rotated));
    assert_eq!(nest_pubkey(&nest), Some(pubkey_of(&rotated)));
}

/// **An up-to-date replica's refresh takes no walk.** The nest's recipient
/// pubkey is the custody's own generation, so the refresh re-seals and
/// republishes as it always did — the walk is paid only on a mismatch.
#[tokio::test]
async fn a_current_replica_refresh_takes_no_walk() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();

    machine.refresh_epoch_schedule().await.unwrap();

    assert_eq!(mail.walks(), 0, "the nest's key is the custody's: no walk");
    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        baseline_snapshot + 1
    );
}

/// **A nest behind or beside the custody is re-sealed.** When the nest's
/// recipient pubkey is a generation the custody has RETIRED, the custody is
/// the newer side: the refresh re-seals under its `msek` with no walk. When
/// the nest names a generation the custody has never held even after a walk
/// — the losing side of two concurrent rotations, whose key the MSEK join
/// displaced — the walked custody is the account's, and the refresh re-seals
/// under it too.
#[tokio::test]
async fn a_nest_behind_or_beside_the_walked_custody_is_resealed() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let old = mail.current().msek.unwrap();
    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();
    let rotated = mail.current().msek.unwrap();

    // Behind: the nest holds the retired generation's pubkey.
    nest.state()
        .provision_recipient_pubkey
        .push((ACTOR, pubkey_of(&old), vec![0u8; 1184], None));
    machine.refresh_epoch_schedule().await.unwrap();
    assert_eq!(mail.walks(), 0, "a retired generation needs no walk");
    assert_eq!(nest_pubkey(&nest), Some(pubkey_of(&rotated)));
    assert!(snapshot_opens_under(&nest, &rotated));

    // Beside: a generation the custody never held, still unknown after a walk.
    nest.state().provision_recipient_pubkey.push((
        ACTOR,
        pubkey_of(&SecretArray32::from([0x5A; 32])),
        vec![0u8; 1184],
        None,
    ));
    machine.refresh_epoch_schedule().await.unwrap();
    assert_eq!(mail.walks(), 1, "an unknown generation is walked first");
    assert_eq!(nest_pubkey(&nest), Some(pubkey_of(&rotated)));
    assert!(snapshot_opens_under(&nest, &rotated));
}

/// **The connect-time refresh leaves a pending rotation's nest state alone.**
/// While the sentinel stands the custody cannot say whether step (d) already
/// moved the snapshot and the pubkey to MSEK′, so the refresh re-seals and
/// republishes nothing — the resume (the banner the page shows) is what moves
/// them. Re-sealing under `msek` here would break every credential step (e)
/// already re-wrapped.
#[tokio::test]
async fn refresh_while_a_rotation_is_pending_moves_nothing_on_the_nest() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    stage_sentinel(&mail, [0x77; 32]);
    let baseline_snapshot = nest.state().provision_mls_snapshot.len();
    let baseline_pubkey = nest.state().provision_recipient_pubkey.len();

    machine.refresh_epoch_schedule().await.unwrap();

    let s = nest.state();
    assert_eq!(
        s.provision_mls_snapshot.len(),
        baseline_snapshot,
        "no re-seal"
    );
    assert_eq!(
        s.provision_recipient_pubkey.len(),
        baseline_pubkey,
        "no republish"
    );
}

/// **The sentinel reaches the nest before the nest moves.** MSEK′ is
/// irrecoverable once its pubkey is published and its blobs wrapped
/// (§ Cross-device finalize race), and a replica that hydrates from the nest
/// must read the sentinel — never the bare pre-rotation `msek` — beside blobs
/// wrapped under MSEK′. So a publish the nest does not acknowledge stops the
/// rotation before its first nest write, resumable; the resume publishes and
/// completes.
#[tokio::test]
async fn an_unacknowledged_sentinel_stops_the_rotation_before_the_nest_moves() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let (snapshots, pubkeys, wraps) = {
        let s = nest.state();
        (
            s.provision_mls_snapshot.len(),
            s.provision_recipient_pubkey.len(),
            s.provision_wrapped_mls.len(),
        )
    };
    mail.refuse_next_publishes(1);

    let result = machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await;
    assert!(result.is_err(), "an unsent sentinel stops the rotation");
    {
        let s = nest.state();
        assert_eq!(
            s.provision_mls_snapshot.len(),
            snapshots,
            "snapshot untouched"
        );
        assert_eq!(
            s.provision_recipient_pubkey.len(),
            pubkeys,
            "pubkey untouched"
        );
        assert_eq!(s.provision_wrapped_mls.len(), wraps, "no blob re-wrapped");
    }
    assert!(mail.current().pending_rotation.is_some(), "resumable");

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("the resume publishes and completes");
    let published = mail.published().config();
    assert!(
        published.pending_rotation.is_none(),
        "the nest holds the cleared sentinel"
    );
    assert_eq!(
        published.msek,
        mail.current().msek,
        "the nest holds the swap"
    );
}

// ─── First-enable is a degenerate rotation (`mail-credentials.md`
//     § Partial-state-during-minting): it runs under the resume sentinel, so a
//     stop anywhere in its blob set reads pending and resumes to a working
//     mailbox. ───

/// Relaunch over the same custody and nest after a stopped first-enable, and
/// assert what the page then shows.
async fn relaunch_reads_pending(
    nest: &fauna_client_mail_settings::testing::FakeNestClient,
    mail: &FakeMailStore,
) -> fauna_client_mail_settings::MailSettingsMachine {
    let relaunched = machine_over(nest, mail);
    relaunched.hydrate().await.expect("hydrate");
    assert!(
        relaunched.snapshot().pending_rotation.is_some(),
        "the resume banner shows after the stopped first-enable"
    );
    relaunched
}

/// Assert a resumed first-enable left one working mailbox: mail on, the
/// sentinel cleared, the one credential wrapped under the account's MSEK and
/// holding a submission token, and the deployment subsystem flipped on.
fn assert_first_enable_completed(
    nest: &fauna_client_mail_settings::testing::FakeNestClient,
    mail: &FakeMailStore,
    machine: &fauna_client_mail_settings::MailSettingsMachine,
) {
    let stored = mail.current();
    assert!(stored.pending_rotation.is_none(), "sentinel cleared");
    assert!(stored.is_mail_enabled(), "email is on");
    assert_eq!(
        stored.credentials.len(),
        1,
        "one credential, no second mint"
    );
    let fp = MsekFingerprint::of(stored.msek.as_ref().expect("an msek"));
    assert_eq!(
        stored.credentials[0].wrapped_under,
        Some(fp),
        "the credential is wrapped under the account's MSEK"
    );
    let s = nest.state();
    assert!(
        s.provision_submission_token
            .iter()
            .any(|t| t.index.1 == stored.credentials[0].credential_id),
        "step 4 — the submission token — landed for the credential"
    );
    assert!(
        s.provision_wrapped_mls
            .iter()
            .any(|b| b.index.1 == stored.credentials[0].credential_id),
        "step 3 — the wrapped MSEK — landed for the credential"
    );
    assert_eq!(
        s.set_mail_enabled.last(),
        Some(&true),
        "the mail subsystem is flipped on"
    );
    drop(s);
    let snap = machine.snapshot();
    assert!(snap.pending_rotation.is_none(), "the banner is gone");
    assert!(snap.error.is_none(), "no error left on the page");
}

#[tokio::test]
async fn a_first_enable_stopped_before_its_submission_token_resumes_to_a_working_mailbox() {
    let (machine, nest, mail) = build();
    nest.state().fail_provision_submission_token_for_n_calls = 1;

    let result = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await;
    assert!(result.is_err(), "the refused step 4 stops the enable");
    assert!(
        nest.state().provision_submission_token.is_empty(),
        "no submission token reached the nest"
    );
    assert_eq!(
        mail.current().credentials.len(),
        1,
        "the credential row — and its secret — outlives the stop"
    );

    let relaunched = relaunch_reads_pending(&nest, &mail).await;
    relaunched
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("the resume finishes the enable");
    assert_first_enable_completed(&nest, &mail, &relaunched);
}

#[tokio::test]
async fn a_first_enable_stopped_before_its_wrapped_msek_resumes_to_a_working_mailbox() {
    let (machine, nest, mail) = build();
    nest.state().fail_provision_wrapped_mls_for_n_calls = 1;

    let result = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await;
    assert!(result.is_err(), "the refused step 3 stops the enable");
    let msek = mail.current().msek.expect("the MSEK persisted first");

    let relaunched = relaunch_reads_pending(&nest, &mail).await;
    relaunched
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("the resume finishes the enable");
    assert_first_enable_completed(&nest, &mail, &relaunched);
    assert_eq!(
        mail.current().msek,
        Some(msek),
        "the resume keeps the minted MSEK — never a fresh one"
    );
}

/// The submission token is the *email* half: resuming a rotation on a
/// calendar-only mailbox (email off) that stopped after its swap — the one
/// rotation state shaped like a first-enable — must not turn email on.
#[tokio::test]
async fn resuming_a_calendar_only_rotation_never_turns_email_on() {
    let (machine, nest, mail) = build();
    machine
        .enable_caldav_mailbox_with_password("default".into(), "a-calendar-password".into())
        .await
        .expect("calendar-only mailbox");
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

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume completes");
    let stored = mail.current();
    assert!(stored.pending_rotation.is_none(), "sentinel cleared");
    assert!(!stored.is_mail_enabled(), "email stays off");
    let s = nest.state();
    assert!(
        s.provision_submission_token.is_empty(),
        "no submission token for a calendar-only mailbox"
    );
    assert!(
        s.set_mail_enabled.is_empty(),
        "the mail subsystem untouched"
    );
}

/// An unfinished rotation is resumed, never overtaken: turning email on while
/// one stands is refused with the resume hint, the sentinel untouched.
#[tokio::test]
async fn enabling_email_while_a_rotation_is_pending_is_refused() {
    let (machine, nest, mail) = build();
    machine
        .enable_caldav_mailbox_with_password("default".into(), "a-calendar-password".into())
        .await
        .expect("calendar-only mailbox");
    stage_sentinel(&mail, [0x5A; 32]);

    let err = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await
        .expect_err("refused while a rotation is pending");
    assert!(err.to_string().contains("resume"), "{err}");
    let stored = mail.current();
    assert_eq!(
        stored.pending_rotation.as_ref().map(|p| p.new_msek.clone()),
        Some([0x5A; 32].into()),
        "the sentinel is untouched"
    );
    assert!(!stored.is_mail_enabled(), "email stays off");
    assert!(nest.state().provision_submission_token.is_empty());
}

/// An email-only teardown abandons a standing rotation: it clears the
/// sentinel by name, and its own `mail_enabled` write — a recreatable-half
/// intent built from the load that still SAW the sentinel — must never set it
/// again above that clear (`mail-credentials.md` § Cross-device finalize race).
#[tokio::test]
async fn an_email_only_teardown_leaves_no_sentinel_behind() {
    use fauna_client_config::MailStore as _;

    let (machine, _nest, mail) = build();
    enable(&machine).await;
    // A rotation under way, its sentinel landed through the store's door.
    mail.write_state(MailStateRow {
        pending_rotation: Some(MailRotationSentinel {
            new_msek: [0x5A; 32].into(),
        }),
        ..MailStateRow::recreatable_of(&mail.current())
    })
    .await
    .unwrap();
    assert!(mail.current().pending_rotation.is_some(), "staged");

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("teardown");

    let stored = mail.current();
    assert_eq!(stored.mail_enabled, Some(false), "email is off");
    assert!(
        stored.pending_rotation.is_none(),
        "the abandoned rotation's sentinel stays cleared"
    );
}

/// Resume over an email-only teardown's leftover sentinel — a mailbox
/// whose every credential is revoked — is made unreachable rather than
/// handled: after the teardown nothing is pending, `ResumeRotation` moves
/// nothing on the nest, and turning email back on is not refused with the
/// resume hint.
#[tokio::test]
async fn after_an_email_only_teardown_nothing_is_left_to_resume() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    stage_sentinel(&mail, [0x5A; 32]);

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("teardown");
    assert!(
        mail.current().pending_rotation.is_none(),
        "nothing is left to resume"
    );
    let baseline_wrapped = nest.state().provision_wrapped_mls.len();
    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("nothing pending is a no-op");
    assert_eq!(
        nest.state().provision_wrapped_mls.len(),
        baseline_wrapped,
        "resume re-wraps nothing over a torn-down mailbox"
    );

    enable(&machine).await;
    let stored = mail.current();
    assert!(stored.is_mail_enabled(), "email is back on");
    assert!(stored.pending_rotation.is_none());
}

/// The teardown clears only the sentinel its own load saw, so a
/// rotation another device starts after that load stands over the revoked
/// mailbox (staged here after the teardown — the same end state). Resume
/// completes it: nothing is owed a re-wrap, finalize swaps and clears, and
/// turning email back on is not refused (`nest/common.md` § Client-state
/// recoverability).
#[tokio::test]
async fn a_rotation_left_standing_over_a_torn_down_mailbox_resumes_to_completion() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("teardown");
    let new_msek = [0x6B; 32];
    stage_sentinel(&mail, new_msek);
    let baseline_wrapped = nest.state().provision_wrapped_mls.len();

    machine
        .dispatch(MailSettingsAction::ResumeRotation)
        .await
        .expect("resume completes over an empty owed set");
    assert_eq!(
        nest.state().provision_wrapped_mls.len(),
        baseline_wrapped,
        "no revoked credential is re-wrapped"
    );
    let stored = mail.current();
    assert!(stored.pending_rotation.is_none(), "the rotation finished");
    assert_eq!(stored.msek, Some(new_msek.into()));

    enable(&machine).await;
    assert!(mail.current().is_mail_enabled(), "email is back on");
}
