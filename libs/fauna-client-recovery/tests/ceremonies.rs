//! The four recovery ceremonies, driven end to end against a faithful
//! in-memory nest (`support::FakeNest` — it runs the real `fauna_core::recovery`
//! verification, so a ceremony that signs the wrong bytes or orders its calls
//! wrongly fails here rather than in production).
//!
//! Owner goal doc: `docs/goal/behavior/identity-succession.md`.

mod support;

use fauna_client_core::linked_nests::{LinkedConnection, LinkedNestTarget};
use fauna_client_recovery::kit::EscrowOutcome;
use fauna_client_recovery::{
    LinkedNestDial, LinkedNestOutcome, request_seed_alone_replacement_everywhere, veto_with_status,
};
use fauna_client_recovery::{
    ReconciledSuccession, RecoveryClient, RecoveryError, RecoveryKitStatus, SuccessionAttempt,
    SuccessionHandoff, SupersededNotice, UnconfirmedSuccession, create_kit, kit_status, parse_kit,
    pending_replacement, reconcile_succession, request_seed_alone_replacement,
    request_seed_alone_replacement_at, reseal_escrow_with_held_kit, resolve_successor,
    restore_seed, succeed_identity, succeed_with_held_kit, veto_pending_replacement,
    veto_pending_replacement_everywhere,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_protocol::RpcError;
use std::sync::Arc;
use support::{FakeNest, LossyNest};

fn signed_in() -> (ActorKeypair, RecoveryClient<FakeNest>) {
    let identity = ActorKeypair::generate();
    let nest = FakeNest::new().signed_in_as(&identity.actor_id());
    (identity, RecoveryClient::new(nest))
}

/// Unwrap the arm a healthy nest always produces.
///
/// Spelled out rather than `unwrap()`ed because the *other* arm is the whole
/// point of the type: a test that silently accepted an `Unconfirmed` would pass
/// while the ceremony never reached the nest.
fn expect_confirmed(attempt: SuccessionAttempt) -> SuccessionHandoff {
    match attempt {
        SuccessionAttempt::Confirmed(handoff) => handoff,
        SuccessionAttempt::Unconfirmed(unconfirmed) => {
            panic!("expected a confirmed succession, got {unconfirmed:?}")
        }
    }
}

// ── Kit creation (leg 2's logic half) ───────────────────────────────────────

#[tokio::test]
async fn a_first_kit_registers_at_seq_1_and_escrows_the_seed() {
    let (identity, client) = signed_in();

    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    assert_eq!(kit.seq, 1, "a first registration lands at seq 1");
    assert_eq!(kit.secret_hex().len(), 64, "the kit is 64-hex");
    assert!(
        kit.escrow.is_stored(),
        "kit creation escrows the seed in the same ceremony: {:?}",
        kit.escrow
    );
    assert_eq!(
        client
            .transport()
            .head(&identity.actor_id())
            .unwrap()
            .recovery_pubkey,
        kit.recovery_pubkey,
        "the registered head is the kit that was just minted"
    );
    assert!(
        client
            .transport()
            .escrow_blob(&identity.actor_id())
            .is_some(),
        "a blob rests after kit creation"
    );
}

// ── The successor's kit carries the predecessor seed (the re-seal window's
//    device-loss backstop — `identity-succession.md` § Seed escrow) ──────────

#[tokio::test]
async fn the_successors_kit_seals_the_predecessor_seed_and_a_restore_recovers_it() {
    // The whole point of the threading: post-succession the predecessor seed
    // exists only on the user's own devices while the corpus is still sealed
    // under it. This asserts the end-to-end property that closes the race — a
    // phrase-only restore, which by definition happens after every device is
    // gone, still yields the material that opens the predecessor's corpus.
    let (successor, client) = signed_in();
    let predecessor = ActorKeypair::generate();

    let kit = create_kit(
        &client,
        &successor,
        None,
        &[fauna_client_recovery::PredecessorSeed {
            actor_id: predecessor.actor_id().0,
            seed: *predecessor.secret_bytes(),
        }],
    )
    .await
    .unwrap();

    let parsed = parse_kit(&kit.uri(None)).unwrap();
    let restored = restore_seed(&client, &parsed, None).await.unwrap();

    assert_eq!(
        &*restored.seed,
        successor.secret_bytes(),
        "the primary is still the account's own seed"
    );
    let opened = restored.opened_predecessors();
    assert_eq!(opened.len(), 1, "the predecessor section opened");
    assert_eq!(
        opened[0].actor_id,
        predecessor.actor_id().0,
        "the entry names WHICH corpus this seed opens — without it the seed is unusable"
    );
    assert_eq!(
        &*opened[0].seed,
        predecessor.secret_bytes(),
        "and it is the predecessor's real seed"
    );
}

#[tokio::test]
async fn an_ordinary_kit_carries_no_predecessor_section() {
    // The absence guarantee at ceremony level: every non-succession call site
    // passes `&[]`, and a restore must then report `Absent` — not an empty
    // `Opened`, which would make "no section" and "section opened to nothing"
    // indistinguishable to the surfaces that branch on it.
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let parsed = parse_kit(&kit.uri(None)).unwrap();
    let restored = restore_seed(&client, &parsed, None).await.unwrap();

    assert_eq!(&*restored.seed, identity.secret_bytes());
    assert!(
        matches!(
            restored.predecessors,
            fauna_client_recovery::PredecessorsOutcome::Absent
        ),
        "an ordinary kit's blob has no predecessor section at all: {:?}",
        restored
    );
}

#[tokio::test]
async fn a_corrupted_predecessor_section_still_restores_the_account_but_is_reported() {
    // The auxiliary must never cost the primary — the account comes back even
    // when the section is destroyed — but the loss is REPORTED, because a
    // corpus sealed under the predecessor has just become unopenable and only
    // the user can act on that. A silent `Absent` here would be the failure.
    let (successor, client) = signed_in();
    let predecessor = ActorKeypair::generate();
    let kit = create_kit(
        &client,
        &successor,
        None,
        &[fauna_client_recovery::PredecessorSeed {
            actor_id: predecessor.actor_id().0,
            seed: *predecessor.secret_bytes(),
        }],
    )
    .await
    .unwrap();

    // Corrupt the resting bytes the way a faulty or hostile nest would: flip a
    // byte inside the predecessor section's ciphertext, leaving the primary's
    // untouched.
    let actor = successor.actor_id();
    let resting = client.transport().escrow_blob(&actor).unwrap();
    let mut blob = fauna_mls::wrapped_blob::SeedEscrowBlob::from_canonical_bytes(&resting).unwrap();
    let mut pred = blob.pred.take().expect("the section was sealed");
    pred.ciphertext[0] ^= 0x01;
    blob.pred = Some(pred);
    client
        .transport()
        .overwrite_escrow_blob(&actor, blob.to_canonical_bytes().unwrap());

    let parsed = parse_kit(&kit.uri(None)).unwrap();
    let restored = restore_seed(&client, &parsed, None).await.unwrap();

    assert_eq!(
        &*restored.seed,
        successor.secret_bytes(),
        "the account is recovered regardless — the auxiliary never takes the primary down"
    );
    assert!(
        matches!(
            restored.predecessors,
            fauna_client_recovery::PredecessorsOutcome::Unreadable(_)
        ),
        "a destroyed section is reported, never flattened into Absent: {:?}",
        restored
    );
    assert!(
        restored.opened_predecessors().is_empty(),
        "and it yields no seeds"
    );
}

#[tokio::test]
async fn the_kit_uri_is_the_recovery_host_and_carries_the_account() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let uri = kit.uri(None);
    assert!(
        uri.starts_with("fauna://recovery?secret="),
        "a recovery kit is never minted on the identity host: {}",
        uri.as_str()
    );
    // Round-trips through the shared parser all 7 apps read.
    let parsed = parse_kit(&uri).unwrap();
    assert_eq!(
        parsed.actor_id.unwrap(),
        identity.actor_id(),
        "the QR names the account, so a restore screen need not ask"
    );
    assert_eq!(parsed.recovery.public(), kit.recovery_pubkey);
    assert_eq!(
        parsed.handle, None,
        "no handle was passed, so none is claimed"
    );
}

/// A Settings-minted kit carries the handle too, and that is what makes a
/// later restore need nothing typed: the actor id says *which* account, the
/// handle's `@domain` says *where* it lives — and only the second can locate
/// the home nest the pre-identity ceremony connects to
/// (`onboarding.md` § 1 Identity, `recovery-entry-account-field`).
#[tokio::test]
async fn a_kit_minted_where_the_handle_is_known_carries_it_for_the_restore() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let parsed = parse_kit(&kit.uri(Some("alice@fauna.test"))).unwrap();
    assert_eq!(parsed.actor_id.unwrap(), identity.actor_id());
    assert_eq!(
        parsed.handle.as_deref(),
        Some("alice@fauna.test"),
        "the restore screen reads the home nest out of this, not out of the actor id"
    );
    assert_eq!(parsed.recovery.public(), kit.recovery_pubkey);
}

#[tokio::test]
async fn an_identity_qr_is_never_accepted_as_a_recovery_kit() {
    // The two secrets have different custody rules; taking one for the other
    // would let a user register their identity seed as their recovery root.
    let err = parse_kit("fauna://identity?secret=aa").unwrap_err();
    assert!(matches!(err, RecoveryError::Malformed(_)), "got {err:?}");
}

#[tokio::test]
async fn replacing_a_kit_advances_the_seq_and_reseals_the_escrow_in_the_same_ceremony() {
    // `identity-succession.md:51` — the nest deletes the escrow row on any
    // registration that changes the registered pubkey, so a ceremony that
    // registers without re-putting leaves the account with NO escrow at all.
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let first_blob = client
        .transport()
        .escrow_blob(&identity.actor_id())
        .unwrap();
    let prior = RecoveryKey::from_hex(first.secret_hex()).unwrap();

    let second = create_kit(&client, &identity, Some(&prior), &[])
        .await
        .unwrap();

    assert_eq!(second.seq, 2, "a replacement lands at seq + 1");
    assert_ne!(second.recovery_pubkey, first.recovery_pubkey);
    assert!(
        second.escrow.is_stored(),
        "the replacement re-put the blob: {:?}",
        second.escrow
    );
    let second_blob = client
        .transport()
        .escrow_blob(&identity.actor_id())
        .expect("a blob rests after the replacement — not the deleted row");
    assert_ne!(
        first_blob, second_blob,
        "the blob was re-sealed to the NEW kit, not left as the old ciphertext"
    );
}

#[tokio::test]
async fn the_new_kit_opens_the_reseal_and_the_retired_one_does_not() {
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let prior = RecoveryKey::from_hex(first.secret_hex()).unwrap();
    let second = create_kit(&client, &identity, Some(&prior), &[])
        .await
        .unwrap();

    // The new kit restores the seed.
    let new_kit = parse_kit(&second.uri(None)).unwrap();
    let seed = restore_seed(&client, &new_kit, None).await.unwrap();
    assert_eq!(
        &*seed.seed,
        identity.secret_bytes(),
        "the re-sealed blob opens to the same identity seed"
    );

    // The retired kit still signs valid bytes — they just authorize nothing.
    let old_kit = parse_kit(&first.uri(None)).unwrap();
    let err = restore_seed(&client, &old_kit, None).await.unwrap_err();
    assert!(
        matches!(err, RecoveryError::SignatureFailed),
        "a retired kit must be refused against the chain HEAD, got {err:?}"
    );
}

// ── A replacement may never narrow the resting predecessor section
//    — the property is about the RESTING BLOB, not this device's registry ────

/// Mint a successor kit whose blob carries `predecessors`, then hand back the
/// prior key — the state every test below replaces *from*.
async fn kit_carrying(
    client: &RecoveryClient<FakeNest>,
    successor: &ActorKeypair,
    predecessors: &[&ActorKeypair],
) -> RecoveryKey {
    let section: Vec<_> = predecessors.iter().map(|p| seed_of(p)).collect();
    let kit = create_kit(client, successor, None, &section).await.unwrap();
    RecoveryKey::from_hex(kit.secret_hex()).unwrap()
}

fn seed_of(identity: &ActorKeypair) -> fauna_client_recovery::PredecessorSeed {
    fauna_client_recovery::PredecessorSeed {
        actor_id: identity.actor_id().0,
        seed: *identity.secret_bytes(),
    }
}

#[tokio::test]
async fn a_replacement_carries_the_resting_section_forward_from_a_registry_that_holds_nothing() {
    // The ceremony-level pin. The re-put REPLACES the blob, and the
    // caller here resolves `&[]` — which is what an ordinary second device, or
    // the one device after the user removed the retired account, genuinely
    // resolves: `add_account` carries no succession link, so the registry has
    // no predecessor row to find. Before the fix that wrote a blob with no
    // section, destroying the only material that opens the predecessor's
    // corpus after total device loss, while the ceremony was holding the very
    // key that reads it.
    let (successor, client) = signed_in();
    let predecessor = ActorKeypair::generate();
    let prior = kit_carrying(&client, &successor, &[&predecessor]).await;

    let replaced = create_kit(&client, &successor, Some(&prior), &[])
        .await
        .expect("a replacement from a registry-less device must still succeed");

    let parsed = parse_kit(&replaced.uri(None)).unwrap();
    let restored = restore_seed(&client, &parsed, None).await.unwrap();
    let opened = restored.opened_predecessors();
    assert_eq!(
        opened.len(),
        1,
        "the section survived the re-put: {:?}",
        restored
    );
    assert_eq!(opened[0].actor_id, predecessor.actor_id().0);
    assert_eq!(
        &*opened[0].seed,
        predecessor.secret_bytes(),
        "and it is the predecessor's real seed, read out of the blob being overwritten"
    );
}

#[tokio::test]
async fn a_replacement_unions_the_resting_section_with_the_registry_without_duplicating() {
    // The two sources overlap in the common case (the ceremony's own device
    // holds a row the blob also names) and each holds something the other does
    // not — an A→B→C chain where this device kept only B's row, against a blob
    // sealed when both were known. The union must be by identity, not by
    // concatenation: a duplicated entry is a second copy of an identity secret
    // in a blob for no gain, and a dropped one is the loss the fix is about.
    let (successor, client) = signed_in();
    let shared = ActorKeypair::generate();
    let blob_only = ActorKeypair::generate();
    let registry_only = ActorKeypair::generate();
    let prior = kit_carrying(&client, &successor, &[&shared, &blob_only]).await;

    let replaced = create_kit(
        &client,
        &successor,
        Some(&prior),
        &[seed_of(&shared), seed_of(&registry_only)],
    )
    .await
    .unwrap();

    let parsed = parse_kit(&replaced.uri(None)).unwrap();
    let restored = restore_seed(&client, &parsed, None).await.unwrap();
    let opened = restored.opened_predecessors();
    assert_eq!(opened.len(), 3, "one entry per identity: {:?}", restored);
    let mut ids: Vec<[u8; 32]> = opened.iter().map(|p| p.actor_id).collect();
    ids.sort();
    let mut want = vec![
        shared.actor_id().0,
        blob_only.actor_id().0,
        registry_only.actor_id().0,
    ];
    want.sort();
    assert_eq!(ids, want, "both sources are represented, neither twice");
}

#[tokio::test]
async fn a_replacement_refuses_rather_than_overwriting_a_section_it_could_not_read() {
    // Fail toward carrying. The blob rests and holds a section, but this
    // ceremony cannot open it — so what it holds is unknown, and a re-put
    // would be the last event in that material's history. Refusing costs a
    // retry; proceeding costs a corpus.
    let (successor, client) = signed_in();
    let predecessor = ActorKeypair::generate();
    let prior = kit_carrying(&client, &successor, &[&predecessor]).await;

    let actor = successor.actor_id();
    let resting = client.transport().escrow_blob(&actor).unwrap();
    let mut blob = fauna_mls::wrapped_blob::SeedEscrowBlob::from_canonical_bytes(&resting).unwrap();
    let mut pred = blob.pred.take().expect("the section was sealed");
    pred.ciphertext[0] ^= 0x01;
    blob.pred = Some(pred);
    client
        .transport()
        .overwrite_escrow_blob(&actor, blob.to_canonical_bytes().unwrap());

    let err = create_kit(&client, &successor, Some(&prior), &[])
        .await
        .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorEscrowUnreadable { .. }),
        "an unreadable resting section must refuse the ceremony, got {err:?}"
    );
    assert_eq!(
        client.transport().chain_len(&actor),
        1,
        "and it refuses BEFORE the registration — nothing landed, no secret was minted, \
         so the retry is free and the account is untouched"
    );
    assert!(
        client.transport().escrow_blob(&actor).is_some(),
        "the resting blob is still there to be read by a device that can"
    );
}

#[tokio::test]
async fn a_replacement_that_cannot_reach_the_resting_blob_refuses_too() {
    // Same rule one layer out: an unreachable blob is as unknown as an
    // unreadable one. `forget_kind` is the nest that cannot serve the fetch —
    // the transport-shaped arm of the same "we do not know what we are about
    // to destroy" state.
    let (successor, client) = signed_in();
    let predecessor = ActorKeypair::generate();
    let prior = kit_carrying(&client, &successor, &[&predecessor]).await;
    client
        .transport()
        .forget_kind("fauna.recovery.escrow.fetch");

    let err = create_kit(&client, &successor, Some(&prior), &[])
        .await
        .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorEscrowUnreadable { .. }),
        "got {err:?}"
    );
    assert_eq!(
        client.transport().chain_len(&successor.actor_id()),
        1,
        "nothing was submitted"
    );
}

#[tokio::test]
async fn a_replacement_with_no_resting_blob_is_not_refused() {
    // `no_escrow` is an answer, not a fault: nothing rests, so the re-put
    // creates rather than replaces and there is nothing to lose. This is the
    // `RegisteredNoEscrow` state a failed escrow half leaves behind — the one
    // a user is most likely to fix by replacing the kit, so refusing it would
    // brick the repair path in the name of protecting nothing.
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let prior = RecoveryKey::from_hex(first.secret_hex()).unwrap();
    let actor = identity.actor_id();
    client.transport().clear_escrow_blob(&actor);

    let replaced = create_kit(&client, &identity, Some(&prior), &[])
        .await
        .expect("no resting blob means nothing to protect");

    assert_eq!(replaced.seq, 2);
    assert!(replaced.escrow.is_stored(), "{:?}", replaced.escrow);
}

#[tokio::test]
async fn replacing_without_the_prior_kit_is_refused_before_anything_is_signed() {
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();

    let err = create_kit(&client, &identity, None, &[]).await.unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorKitRequired),
        "the honest-loss path is the 30-day window, not this ceremony; got {err:?}"
    );
    assert_eq!(
        client.transport().chain_len(&identity.actor_id()),
        1,
        "nothing was submitted"
    );
}

#[tokio::test]
async fn a_kit_that_is_not_the_registered_head_is_refused_locally() {
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();
    let stranger = RecoveryKey::generate();

    let err = create_kit(&client, &identity, Some(&stranger), &[])
        .await
        .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorKitMismatch),
        "got {err:?}"
    );
    assert_eq!(client.transport().chain_len(&identity.actor_id()), 1);
}

#[tokio::test]
async fn a_failed_escrow_put_still_returns_the_kit() {
    // The registration has already landed, so the freshly minted secret exists
    // in exactly one place: the value being returned. Collapsing this into Err
    // would destroy the user's only copy of a key that is now registered.
    let (identity, client) = signed_in();
    client
        .transport()
        .fail_escrow_put("fauna.protocol.internal");

    let kit = create_kit(&client, &identity, None, &[])
        .await
        .expect("the kit must come back even though escrow failed");

    assert_eq!(kit.secret_hex().len(), 64);
    assert!(
        matches!(kit.escrow, EscrowOutcome::Failed { .. }),
        "the failure is surfaced, not swallowed: {:?}",
        kit.escrow
    );
    assert_eq!(
        client
            .transport()
            .head(&identity.actor_id())
            .unwrap()
            .recovery_pubkey,
        kit.recovery_pubkey,
        "the kit is genuinely registered — it authorizes succession"
    );
}

#[tokio::test]
async fn the_kit_secret_never_appears_in_debug_output() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let rendered = format!("{kit:?}");
    assert!(
        !rendered.contains(kit.secret_hex()),
        "a kit in a log line or panic message is the exact leak the \
         offline-only rule exists to prevent: {rendered}"
    );
    assert!(rendered.contains("<redacted>"));
}

// ── Escrow restore (leg 3's logic half) ─────────────────────────────────────

#[tokio::test]
async fn the_phrase_alone_recovers_the_identity_seed() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let phrase = kit.secret_hex().to_string();
    drop(kit);

    // A fresh client: nothing but the phrase and the account it belongs to.
    let parsed = parse_kit(&phrase).unwrap();
    assert!(
        parsed.actor_id.is_none(),
        "a bare hex secret names no account"
    );

    let seed = restore_seed(&client, &parsed, Some(identity.actor_id()))
        .await
        .unwrap();

    assert_eq!(&*seed.seed, identity.secret_bytes());
}

#[tokio::test]
async fn a_bare_phrase_with_no_account_is_refused_rather_than_guessed() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let parsed = parse_kit(kit.secret_hex()).unwrap();

    let err = restore_seed(&client, &parsed, None).await.unwrap_err();

    assert!(matches!(err, RecoveryError::Malformed(_)), "got {err:?}");
}

#[tokio::test]
async fn no_escrow_is_an_answer_the_screen_can_act_on() {
    // `identity-succession.md:51` — honest degradation, not a fault: the kit is
    // registered but no blob rests, so phrase-only restore is unavailable until
    // a signed-in device re-creates the kit.
    let (identity, client) = signed_in();
    client
        .transport()
        .fail_escrow_put("fauna.protocol.internal");
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let parsed = parse_kit(&kit.uri(None)).unwrap();

    let err = restore_seed(&client, &parsed, None).await.unwrap_err();

    assert!(matches!(err, RecoveryError::NoEscrow), "got {err:?}");
    assert!(!err.is_retryable(), "a re-put is owed, not a retry");
}

#[tokio::test]
async fn a_challenge_nonce_is_single_use() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let parsed = parse_kit(&kit.uri(None)).unwrap();

    // Two restores each mint their own nonce and both succeed…
    restore_seed(&client, &parsed, None).await.unwrap();
    restore_seed(&client, &parsed, None).await.unwrap();

    // …but a nonce already spent cannot be redeemed again.
    let actor = identity.actor_id();
    let challenge = client.escrow_challenge(&actor).await.unwrap();
    let sig = fauna_core::recovery::EscrowChallenge::new(actor, challenge.nonce)
        .sign(&parsed.recovery)
        .unwrap();
    client
        .escrow_fetch(&actor, &challenge.nonce, &sig)
        .await
        .expect("first redemption");
    let err = client
        .escrow_fetch(&actor, &challenge.nonce, &sig)
        .await
        .unwrap_err();
    assert!(matches!(err, RecoveryError::InvalidNonce), "got {err:?}");
}

// ── The seed-initiated replacement window ───────────────────────────────────

#[tokio::test]
async fn a_seed_alone_replacement_parks_for_the_grace_window_and_leaves_escrow_alone() {
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let blob_before = client
        .transport()
        .escrow_blob(&identity.actor_id())
        .unwrap();

    let pending = request_seed_alone_replacement(&client, &identity)
        .await
        .unwrap();

    assert_eq!(pending.secret_hex().len(), 64, "the new kit is shown NOW");
    assert_eq!(
        client
            .transport()
            .head(&identity.actor_id())
            .unwrap()
            .recovery_pubkey,
        first.recovery_pubkey,
        "the chain head is untouched until the window closes"
    );
    assert_eq!(
        client
            .transport()
            .escrow_blob(&identity.actor_id())
            .as_ref(),
        Some(&blob_before),
        "nothing has changed yet, so the resting blob still opens under the \
         current kit — re-putting is owed at LANDING, not at request"
    );

    let banner = pending_replacement(&client).await.unwrap().unwrap();
    assert_eq!(banner.lands_at, pending.lands_at);
    assert_eq!(
        banner.remaining_secs(banner.requested_at),
        fauna_client_recovery::REPLACE_GRACE_SECS,
        "the banner counts down the full 30-day window"
    );
    assert_eq!(
        banner.remaining_secs(banner.lands_at + 10),
        0,
        "a window already past reads zero, never a wrapped value"
    );
}

#[tokio::test]
async fn the_current_kit_vetoes_whatever_pends() {
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    request_seed_alone_replacement(&client, &identity)
        .await
        .unwrap();
    let kit = parse_kit(&first.uri(None)).unwrap();

    let cancelled = veto_pending_replacement(&client, &kit, None).await.unwrap();

    assert!(cancelled);
    assert!(
        pending_replacement(&client).await.unwrap().is_none(),
        "the pending replacement is gone"
    );

    // Idempotent: vetoing again is an honest `false`, not a failure — the
    // vetoer's goal state holds either way.
    let again = veto_pending_replacement(&client, &kit, None).await.unwrap();
    assert!(!again);
}

// ── Every nest the identity is linked to (clause (c)) ───────────────────────

/// One identity signed in at its bound nest and at one linked nest, holding
/// the same kit at both (the chain carried by the reconcile, as the link and
/// the secondary leg do).
async fn bound_and_linked() -> (
    ActorKeypair,
    RecoveryClient<FakeNest>,
    RecoveryClient<FakeNest>,
    fauna_client_recovery::ParsedKit,
) {
    use fauna_client_recovery::chain_reconcile::{RpcChainDoor, reconcile_registration_chains};
    let identity = ActorKeypair::generate();
    let nest = || RecoveryClient::new(FakeNest::new().signed_in_as(&identity.actor_id()));
    let (bound, linked) = (nest(), nest());
    let first = create_kit(&bound, &identity, None, &[]).await.unwrap();
    let door = |c| RpcChainDoor {
        rpc: c,
        actor_id: identity.actor_id().0,
    };
    reconcile_registration_chains(&door(bound.transport()), &door(linked.transport()))
        .await
        .unwrap();
    let kit = parse_kit(&first.uri(None)).unwrap();
    (identity, bound, linked, kit)
}

#[tokio::test]
async fn a_seed_alone_request_reaches_every_linked_nest_and_each_runs_its_own_window() {
    let (identity, bound, linked, _) = bound_and_linked().await;
    let actor = identity.actor_id();

    let pending = request_seed_alone_replacement(&bound, &identity)
        .await
        .unwrap();
    request_seed_alone_replacement_at(&linked, &pending)
        .await
        .unwrap();

    for nest in [&bound, &linked] {
        let window = nest
            .transport()
            .pending(&actor)
            .expect("a window at each nest");
        assert_eq!(
            window.new_recovery_pubkey.as_ref(),
            pending.recovery_pubkey.as_slice()
        );
    }
    // A replayed identical request keeps the clock the first one started.
    let first_lands = linked.transport().pending(&actor).unwrap().lands_at;
    let again = request_seed_alone_replacement_at(&linked, &pending)
        .await
        .unwrap();
    assert!(again >= first_lands);
}

/// A seed thief opens a window at the linked nest alone, which no device is
/// bound to: the leg's reading reaches the alert on a device bound to the
/// other nest, and the bound nest reading nothing never clears it.
#[tokio::test]
async fn a_window_at_the_linked_nest_alone_raises_the_alert_on_a_device_bound_to_the_other() {
    use fauna_client_recovery::alerts::{
        LinkedReading, alert_key, record_linked_readings, refresh_pending_replacement_alert,
        sync_linked_readings,
    };
    let (identity, bound, linked, kit) = bound_and_linked().await;
    let actor = identity.actor_id();
    let linked_id = [0x4c; 32];
    let alerts = fauna_client_alerts::CriticalAlerts::new();

    request_seed_alone_replacement(&linked, &identity)
        .await
        .unwrap();

    // The leg's half: read the linked nest's window over its connection.
    let window =
        fauna_client_recovery::chain_reconcile::fetch_replacement_status(linked.transport())
            .await
            .unwrap();
    record_linked_readings(
        &actor,
        &[(linked_id, LinkedReading::Read(window.map(Into::into)))],
    );
    // The sweep's half: the bound nest reads nothing, then the linked readings post.
    refresh_pending_replacement_alert(&bound, &alerts, &actor, 0)
        .await
        .unwrap();
    sync_linked_readings(&alerts, &actor, 0);
    assert_eq!(
        alerts.active().len(),
        1,
        "the linked nest's window is loud here"
    );
    assert_eq!(alerts.active()[0].key, alert_key(&actor));

    // The next sweep, the bound nest still reading nothing, leaves it standing.
    refresh_pending_replacement_alert(&bound, &alerts, &actor, 0)
        .await
        .unwrap();
    sync_linked_readings(&alerts, &actor, 0);
    assert_eq!(alerts.active().len(), 1);

    // Vetoed at the linked nest, the leg's next reading clears it.
    veto_pending_replacement(&linked, &kit, Some(actor))
        .await
        .unwrap();
    let window =
        fauna_client_recovery::chain_reconcile::fetch_replacement_status(linked.transport())
            .await
            .unwrap();
    record_linked_readings(
        &actor,
        &[(linked_id, LinkedReading::Read(window.map(Into::into)))],
    );
    sync_linked_readings(&alerts, &actor, 0);
    assert!(alerts.active().is_empty());
}

/// One veto gesture contests the window at every nest that holds one, each
/// over its own (pre-identity) challenge.
#[tokio::test]
async fn one_veto_clears_the_window_at_every_nest() {
    let (identity, bound, linked, kit) = bound_and_linked().await;
    let actor = identity.actor_id();
    let pending = request_seed_alone_replacement(&bound, &identity)
        .await
        .unwrap();
    request_seed_alone_replacement_at(&linked, &pending)
        .await
        .unwrap();

    let answers = veto_pending_replacement_everywhere(&[&bound, &linked], &kit, Some(actor)).await;
    assert_eq!(
        answers.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![true, true]
    );
    assert!(bound.transport().pending(&actor).is_none());
    assert!(linked.transport().pending(&actor).is_none());

    // A nest with nothing pending answers an honest `false`.
    let again = veto_pending_replacement_everywhere(&[&bound, &linked], &kit, Some(actor)).await;
    assert_eq!(
        again.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![false, false]
    );
}

// ── Clause (c) as one gesture: the composition every app's Settings runs ────

/// The host's dial over in-memory nests: a nest is reached by its address
/// and presents the identity it was registered under; an address it does not
/// know is unreachable. Records which connection each gesture opened.
#[derive(Default)]
struct FakeDial {
    nests: std::collections::HashMap<String, ([u8; 32], Arc<FakeNest>)>,
    opened: std::sync::Mutex<Vec<(&'static str, String)>>,
}

impl FakeDial {
    fn reach(
        &self,
        how: &'static str,
        target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Arc<FakeNest>>, String> {
        self.opened
            .lock()
            .unwrap()
            .push((how, target.nest_url.clone()));
        let (presented, nest) = self
            .nests
            .get(&target.nest_url)
            .ok_or_else(|| format!("{}: unreachable", target.nest_url))?;
        Ok(LinkedConnection {
            rpc: Arc::clone(nest),
            bound_identity: *presented,
        })
    }

    fn opened(&self) -> Vec<(&'static str, String)> {
        self.opened.lock().unwrap().clone()
    }

    fn nest(&self, url: &str) -> &FakeNest {
        &self.nests[url].1
    }
}

#[async_trait::async_trait]
impl LinkedNestDial for FakeDial {
    type Owner = Arc<FakeNest>;
    type Anonymous = Arc<FakeNest>;

    async fn owner(
        &self,
        target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Self::Owner>, String> {
        self.reach("owner", target)
    }

    async fn anonymous(
        &self,
        target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Self::Anonymous>, String> {
        self.reach("anonymous", target)
    }
}

const LINKED_ID: [u8; 32] = [0x4c; 32];
const LINKED_URL: &str = "https://linked.test";
const GONE_URL: &str = "https://gone.test";
const IMPOSTOR_URL: &str = "https://impostor.test";

/// One identity at its bound nest and one linked nest, the same kit at both,
/// and the bound nest's pairing rows naming three nests: the linked one, one
/// that is unreachable, and an address answered by a box presenting another
/// identity than its pairing row names (it holds the account too, so only the
/// binding check keeps the gesture off it).
async fn bound_and_linked_by_pairing() -> (
    ActorKeypair,
    RecoveryClient<Arc<FakeNest>>,
    FakeDial,
    String,
) {
    use fauna_client_recovery::chain_reconcile::{RpcChainDoor, reconcile_registration_chains};
    let identity = ActorKeypair::generate();
    let nest = || Arc::new(FakeNest::new().signed_in_as(&identity.actor_id()));
    let (linked, impostor) = (nest(), nest());
    let bound = RecoveryClient::new(nest());
    let first = create_kit(&bound, &identity, None, &[]).await.unwrap();
    let door = |c| RpcChainDoor {
        rpc: c,
        actor_id: identity.actor_id().0,
    };
    for other in [&linked, &impostor] {
        reconcile_registration_chains(&door(bound.transport().as_ref()), &door(other.as_ref()))
            .await
            .unwrap();
    }
    bound.transport().pair_with(LINKED_ID, LINKED_URL);
    bound.transport().pair_with([0x60; 32], GONE_URL);
    bound.transport().pair_with([0x1a; 32], IMPOSTOR_URL);
    let mut dial = FakeDial::default();
    dial.nests.insert(LINKED_URL.into(), (LINKED_ID, linked));
    dial.nests
        .insert(IMPOSTOR_URL.into(), ([0xee; 32], impostor));
    (identity, bound, dial, first.uri(None).to_string())
}

/// The request gesture opens the window at the bound nest and, over an
/// owner-authenticated connection, at every linked nest it can reach and
/// verify — reporting, never failing on, the rest.
#[tokio::test]
async fn one_request_gesture_opens_the_window_at_the_bound_and_every_linked_nest() {
    let (identity, bound, dial, _) = bound_and_linked_by_pairing().await;
    let actor = identity.actor_id();

    let (pending, linked) = request_seed_alone_replacement_everywhere(&bound, &identity, &dial)
        .await
        .unwrap();

    let window = |nest: &FakeNest| nest.pending(&actor).map(|w| w.new_recovery_pubkey.to_vec());
    let want = Some(pending.recovery_pubkey.to_vec());
    assert_eq!(window(bound.transport()), want, "the bound nest's window");
    assert_eq!(
        window(dial.nest(LINKED_URL)),
        want,
        "the linked nest's own window"
    );
    assert_eq!(
        window(dial.nest(IMPOSTOR_URL)),
        None,
        "nothing sent to the impostor"
    );

    assert_eq!(linked.unlisted, None);
    let outcomes: Vec<_> = linked
        .nests
        .iter()
        .map(|n| (n.nest_url.as_str(), n.outcome.clone()))
        .collect();
    assert!(matches!(
        outcomes[0],
        (LINKED_URL, LinkedNestOutcome::Answered(_))
    ));
    assert!(matches!(
        outcomes[1],
        (GONE_URL, LinkedNestOutcome::Unreachable(_))
    ));
    assert_eq!(
        outcomes[2],
        (
            IMPOSTOR_URL,
            LinkedNestOutcome::IdentityMismatch {
                presented: [0xee; 32]
            }
        )
    );
    assert!(dial.opened().iter().all(|(how, _)| *how == "owner"));
}

/// A refusal at the bound nest is the gesture's answer, and no linked nest is
/// asked: no kit was shown, so no window may open anywhere.
#[tokio::test]
async fn a_request_the_bound_nest_refuses_asks_no_linked_nest() {
    let (_, bound, dial, _) = bound_and_linked_by_pairing().await;
    // Another identity, registered nowhere.
    let stranger = ActorKeypair::generate();

    let err = request_seed_alone_replacement_everywhere(&bound, &stranger, &dial)
        .await
        .unwrap_err();

    assert!(matches!(err, RecoveryError::NotRegistered), "got {err:?}");
    assert!(dial.opened().is_empty());
}

/// Settings' veto gesture (`veto_with_status`) clears the window at the bound
/// nest and, over anonymous connections, at every linked nest; the fresh
/// status is the bound nest's.
#[tokio::test]
async fn one_veto_gesture_clears_the_window_at_the_bound_and_every_linked_nest() {
    let (identity, bound, dial, kit_uri) = bound_and_linked_by_pairing().await;
    let actor = identity.actor_id();
    request_seed_alone_replacement_everywhere(&bound, &identity, &dial)
        .await
        .unwrap();
    dial.opened.lock().unwrap().clear();

    let (cancelled, status) = veto_with_status(&bound, actor, &kit_uri, &dial)
        .await
        .unwrap();

    assert!(cancelled);
    assert_eq!(status, RecoveryKitStatus::Registered);
    assert!(bound.transport().pending(&actor).is_none());
    assert!(dial.nest(LINKED_URL).pending(&actor).is_none());
    assert_eq!(dial.opened().len(), 3, "every linked nest was dialled");
    assert!(dial.opened().iter().all(|(how, _)| *how == "anonymous"));
}

/// A window at a linked nest alone — a thief who asked there — is contested
/// by the veto made on a device bound to the other nest.
#[tokio::test]
async fn the_veto_gesture_contests_a_window_held_at_a_linked_nest_alone() {
    let (identity, bound, dial, kit_uri) = bound_and_linked_by_pairing().await;
    let actor = identity.actor_id();
    let thief_side = RecoveryClient::new(Arc::clone(&dial.nests[LINKED_URL].1));
    request_seed_alone_replacement(&thief_side, &identity)
        .await
        .unwrap();
    assert!(bound.transport().pending(&actor).is_none());

    let (cancelled, _) = veto_with_status(&bound, actor, &kit_uri, &dial)
        .await
        .unwrap();

    assert!(cancelled, "the linked nest's window was there to cancel");
    assert!(dial.nest(LINKED_URL).pending(&actor).is_none());
}

#[tokio::test]
async fn resealing_with_the_held_kit_restores_phrase_recovery() {
    // The ruling: a landed re-put is user-prompted by construction (the
    // new key exists only on the paper the user kept), so the repair takes the
    // kit-in-hand, verifies it IS the registered head, and re-puts.
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let prior = RecoveryKey::from_hex(first.secret_hex()).unwrap();
    let landed = create_kit(&client, &identity, Some(&prior), &[])
        .await
        .unwrap();
    // The `RegisteredNoEscrow` state every arrival shares: no row rests, the
    // head is the kit on paper.
    client.transport().clear_escrow_blob(&identity.actor_id());

    let held = parse_kit(&landed.uri(None)).unwrap();
    reseal_escrow_with_held_kit(&client, &identity, &held, &[])
        .await
        .unwrap();

    let seed = restore_seed(&client, &held, None).await.unwrap();
    assert_eq!(&*seed.seed, identity.secret_bytes());
}

#[tokio::test]
async fn a_stale_kit_cannot_reseal_the_escrow() {
    // The retired kit still PARSES — paper outlives every replacement — but a
    // blob sealed to it would rest unfetchably: the nest cannot look inside,
    // so only this client-side head check keeps that state unrepresentable.
    let (identity, client) = signed_in();
    let first = create_kit(&client, &identity, None, &[]).await.unwrap();
    let prior = RecoveryKey::from_hex(first.secret_hex()).unwrap();
    create_kit(&client, &identity, Some(&prior), &[])
        .await
        .unwrap();
    client.transport().clear_escrow_blob(&identity.actor_id());

    let stale = parse_kit(&first.uri(None)).unwrap();
    let err = reseal_escrow_with_held_kit(&client, &identity, &stale, &[])
        .await
        .unwrap_err();

    assert!(matches!(err, RecoveryError::KitNotCurrent), "got {err:?}");
    assert!(
        client
            .transport()
            .escrow_blob(&identity.actor_id())
            .is_none(),
        "nothing landed — the refusal precedes the put"
    );
}

#[tokio::test]
async fn a_reseal_with_a_kit_naming_another_account_is_refused() {
    let (identity, client) = signed_in();
    let (other, other_client) = signed_in();
    let _own_kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let other_kit = create_kit(&other_client, &other, None, &[]).await.unwrap();
    client.transport().clear_escrow_blob(&identity.actor_id());

    // A Settings-minted kit embeds its actor id; entering another account's
    // kit must be refused on that id, before any head comparison could
    // accidentally pass on a colliding chain.
    let foreign = parse_kit(&other_kit.uri(None)).unwrap();
    let err = reseal_escrow_with_held_kit(&client, &identity, &foreign, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, RecoveryError::Malformed(_)), "got {err:?}");
    assert!(
        client
            .transport()
            .escrow_blob(&identity.actor_id())
            .is_none(),
        "nothing landed"
    );
}

#[tokio::test]
async fn the_held_kit_reseal_carries_the_predecessor_section() {
    // A successor inside the corpus re-seal window can arrive at
    // `RegisteredNoEscrow` too (a failed put in its minting ceremony); the
    // repair must carry the registry-resolved section or the device-loss
    // backstop silently drops — one step later.
    let (identity, client) = signed_in();
    let predecessor = ActorKeypair::generate();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    client.transport().clear_escrow_blob(&identity.actor_id());

    let held = parse_kit(&kit.uri(None)).unwrap();
    reseal_escrow_with_held_kit(
        &client,
        &identity,
        &held,
        &[fauna_client_recovery::PredecessorSeed {
            actor_id: predecessor.actor_id().0,
            seed: *predecessor.secret_bytes(),
        }],
    )
    .await
    .unwrap();

    let restored = restore_seed(&client, &held, None).await.unwrap();
    assert_eq!(&*restored.seed, identity.secret_bytes());
    let opened = restored.opened_predecessors();
    assert_eq!(opened.len(), 1, "the predecessor section rode the re-put");
    assert_eq!(opened[0].actor_id, predecessor.actor_id().0);
}

// ── Succession + the superseded refusal (legs 1/4/5's logic half) ───────────

#[tokio::test]
async fn a_recovery_key_holder_repoints_the_account_and_the_link_verifies() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let recovery = RecoveryKey::from_hex(kit.secret_hex()).unwrap();
    let successor = ActorKeypair::generate();

    let outcome = succeed_identity(
        &client,
        identity.actor_id(),
        &recovery,
        &successor,
        Some(&identity),
    )
    .await
    .unwrap();

    assert_eq!(outcome.new_actor_id, successor.actor_id());

    // Every other consumer VERIFIES the link rather than trusting the reply.
    let verified = resolve_successor(&client, identity.actor_id(), None)
        .await
        .unwrap()
        .expect("the identity was succeeded");
    assert_eq!(verified.old_actor_id, identity.actor_id());
    assert_eq!(verified.new_actor_id, successor.actor_id());
    assert_eq!(
        verified.chain_head.recovery_pubkey, kit.recovery_pubkey,
        "authorized under the head the walk established, not the carried value"
    );

    // The line walk is the same verified walk with every hop kept — and the
    // successor, never succeeded itself, has an empty line, not an error.
    let line = fauna_client_recovery::resolve_succession_line(&client, identity.actor_id(), None)
        .await
        .unwrap();
    assert_eq!(line, vec![verified]);
    assert!(
        fauna_client_recovery::resolve_succession_line(&client, successor.actor_id(), None)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The anchored walk tells a definitive "never succeeded" from a walk that
/// settled nothing — the split the calendar's per-session succession memo
/// needs (`caldav-server.md` § Who may mutate an existing event over the
/// inbound rail → *A succeeded organizer*): the first is re-asked, the second
/// remembered.
#[tokio::test]
async fn the_anchored_walk_tells_never_succeeded_from_a_walk_that_settled_nothing() {
    use fauna_client_recovery::{AnchoredWalk, kinds, walk_outcome};

    let identity = ActorKeypair::generate();
    let nest = FakeNest::new().signed_in_as(&identity.actor_id());
    let client = RecoveryClient::new(&nest);
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    assert_eq!(
        walk_outcome(&nest, identity.actor_id(), None).await,
        AnchoredWalk::NeverSucceeded
    );

    let successor = ActorKeypair::generate();
    succeed_identity(
        &client,
        identity.actor_id(),
        &RecoveryKey::from_hex(kit.secret_hex()).unwrap(),
        &successor,
        Some(&identity),
    )
    .await
    .unwrap();
    match walk_outcome(&nest, identity.actor_id(), None).await {
        AnchoredWalk::Succeeded(step) => assert_eq!(step.new_actor_id, successor.actor_id()),
        other => panic!("expected the verified successor, got {other:?}"),
    }

    nest.forget_kind(kinds::SUCCESSION_LOOKUP);
    assert_eq!(
        walk_outcome(&nest, identity.actor_id(), None).await,
        AnchoredWalk::Unsettled
    );
}

#[tokio::test]
async fn the_held_kit_ceremony_mints_a_successor_the_caller_can_sign_in_as() {
    // The composed ceremony every "my identity was stolen" screen drives: a
    // pasted phrase in, a persistable successor seed out. What makes it worth
    // sharing is the seed — it exists nowhere but this return value, so the
    // handoff must hand back something the caller can actually store.
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let handoff = expect_confirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            &kit.uri(Some("user@example.test")),
            Some(&identity),
        )
        .await
        .unwrap(),
    );

    // The returned seed IS the account now — the property a screen relies on
    // when it persists the hex and signs in with it.
    let successor = ActorKeypair::from_secret_hex(handoff.successor_secret_hex()).unwrap();
    assert_eq!(successor.actor_id(), handoff.new_actor_id);

    let verified = resolve_successor(&client, identity.actor_id(), None)
        .await
        .unwrap()
        .expect("the identity was succeeded");
    assert_eq!(verified.new_actor_id, handoff.new_actor_id);
}

#[tokio::test]
async fn the_ceremony_hands_back_the_statement_the_group_sweep_needs() {
    // `sweep_groups` takes the `SignedIdentitySuccession` and posts its
    // **verbatim canonical bytes** in-group between the two commits
    // (`identity-succession.md` § Implementation status → the in-group-statement
    // bullet: "never re-encoded — the same encoding every other plane stores and
    // serves"). The value is already in scope at submit, so the ceremony hands
    // it back rather than making every one of the 7 app call-sites spend a
    // `succession.lookup` round trip to re-fetch what it just authored.
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let handoff = expect_confirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            &kit.uri(Some("user@example.test")),
            Some(&identity),
        )
        .await
        .unwrap(),
    );

    // It names the pair the sweep will refuse a mismatch on.
    assert_eq!(
        handoff.statement.statement.old_actor_id,
        identity.actor_id()
    );
    assert_eq!(
        handoff.statement.statement.new_actor_id,
        handoff.new_actor_id
    );

    // The load-bearing property: byte-identical to what the nest stores and
    // `succession.lookup` serves. A member verifies the posted bytes against
    // the chain, so a re-encode here would post a statement whose signatures
    // cover different bytes than the ones every other plane carries.
    let served = client
        .succession_lookup(&identity.actor_id())
        .await
        .unwrap();
    let served = served.first().expect("the identity was succeeded");
    assert_eq!(
        fauna_core::encoding::canonical_encode(&handoff.statement).unwrap(),
        fauna_core::encoding::canonical_encode(served).unwrap(),
        "the handed-back statement must encode to the bytes the nest serves"
    );
}

#[tokio::test]
async fn the_held_kit_ceremony_accepts_the_bare_hex_a_user_wrote_down() {
    // The display is bare 64-hex (§ The RecoveryKey — *Kit payload*), so the
    // phrase a user copies off paper names no account. The ceremony must take
    // it: the screen already knows whose account it is.
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let handoff = expect_confirmed(
        succeed_with_held_kit(&client, identity.actor_id(), kit.secret_hex(), None)
            .await
            .unwrap(),
    );

    assert_ne!(handoff.new_actor_id, identity.actor_id());
}

#[tokio::test]
async fn a_kit_naming_another_account_is_refused_before_a_successor_is_minted() {
    // A kit that names an account is authoritative for *which* one. Catching
    // the mismatch here is what lets the screen say "that is not this account's
    // kit" rather than relaying `PriorKitMismatch`, which reads as "your kit is
    // stale" when in fact it is simply someone else's.
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();

    // Minted by hand rather than through `create_kit`: the nest refuses a
    // registration naming an account other than the caller's, which is exactly
    // why this payload could only ever have come from somewhere else.
    let stranger = ActorKeypair::generate();
    let strangers_key = RecoveryKey::generate();
    let strangers_kit = fauna_core::recovery::RecoveryKitQr::to_uri(
        &strangers_key.to_hex(),
        Some(&stranger.actor_id_hex()),
        None,
    );

    let err = succeed_with_held_kit(
        &client,
        identity.actor_id(),
        &strangers_kit,
        Some(&identity),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorKitMismatch),
        "got {err:?}"
    );
    assert!(
        resolve_successor(&client, identity.actor_id(), None)
            .await
            .unwrap()
            .is_none(),
        "a refused phrase must not have re-pointed the account"
    );
}

#[tokio::test]
async fn a_malformed_phrase_is_refused_without_a_round_trip() {
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();

    let err = succeed_with_held_kit(&client, identity.actor_id(), "not-a-recovery-phrase", None)
        .await
        .unwrap_err();

    assert!(
        !matches!(err, RecoveryError::PriorKitMismatch),
        "a parse failure must not masquerade as a stale kit: {err:?}"
    );
    assert!(
        resolve_successor(&client, identity.actor_id(), None)
            .await
            .unwrap()
            .is_none()
    );
}

// ── The lost reply ───────────────────────────────────────────
//
// The nest commits a succession before it replies, so a dropped connection in
// that gap tells the client "failed" about an account that has already moved.
// Until this group of tests existed, `succeed_with_held_kit` answered that with
// an `Err` that took the successor seed with it — and since the succession row
// is keyed on the old actor id, a retry is refused `AlreadySucceeded` forever.
// The account, handle, admin role and data were then unreachable by anyone: by
// product invariant there is no operator to appeal to.

/// A client over a nest whose succession reply can be dropped mid-ceremony.
fn signed_in_lossy() -> (ActorKeypair, RecoveryClient<LossyNest>) {
    let identity = ActorKeypair::generate();
    let nest = LossyNest::new(FakeNest::new().signed_in_as(&identity.actor_id()));
    (identity, RecoveryClient::new(nest))
}

fn expect_unconfirmed(attempt: SuccessionAttempt) -> UnconfirmedSuccession {
    match attempt {
        SuccessionAttempt::Unconfirmed(unconfirmed) => unconfirmed,
        SuccessionAttempt::Confirmed(handoff) => {
            panic!("expected an unconfirmed succession, got {handoff:?}")
        }
    }
}

#[tokio::test]
async fn a_lost_submit_reply_hands_the_seed_back_instead_of_destroying_the_account() {
    // The probe from the finding, with its verdict inverted. Everything it
    // asserted about the *nest* still holds — the succession really landed while
    // the client was told it failed — and the one thing that changed is the only
    // thing that mattered: the key the account now belongs to comes back to the
    // caller instead of being dropped at a `?`.
    let (identity, client) = signed_in_lossy();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    client
        .transport()
        .lose_reply_to(fauna_client_recovery::kinds::SUCCESSION_SUBMIT);

    let attempt = succeed_with_held_kit(
        &client,
        identity.actor_id(),
        kit.secret_hex(),
        Some(&identity),
    )
    .await
    .expect("a lost reply is an outcome to reconcile, never an outer Err");
    let unconfirmed = expect_unconfirmed(attempt);

    // It presents as what it is: a transport fault, not a decision.
    assert!(
        matches!(unconfirmed.error, RecoveryError::Transport(_)),
        "got {:?}",
        unconfirmed.error
    );

    // The nest's side of the probe, unchanged: it really committed.
    assert_eq!(
        client
            .transport()
            .nest()
            .succession_count(&identity.actor_id()),
        1,
        "the handler ran to completion — that is why the reply was worth losing"
    );
    assert!(
        client
            .transport()
            .nest()
            .escrow_blob(&identity.actor_id())
            .is_none(),
        "the escrow row died inside the succession transaction"
    );

    // And a retry cannot save the user — which is why the seed had to survive.
    let retry = succeed_with_held_kit(
        &client,
        identity.actor_id(),
        kit.secret_hex(),
        Some(&identity),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            expect_unconfirmed(retry).error,
            RecoveryError::AlreadySucceeded | RecoveryError::Superseded { .. }
        ),
        "first-succession-wins is structural; the retry path is closed"
    );

    // The fix, end to end: the caller persists this seed, asks, and is in.
    let successor = ActorKeypair::from_secret_hex(unconfirmed.successor_secret_hex()).unwrap();
    assert_eq!(successor.actor_id(), unconfirmed.successor_actor_id);
    let handoff = match reconcile_succession(&client, identity.actor_id(), &successor, None)
        .await
        .unwrap()
    {
        ReconciledSuccession::Landed(handoff) => handoff,
        other => panic!("the succession landed; reconciliation must say so: {other:?}"),
    };
    assert_eq!(
        handoff.new_actor_id,
        successor.actor_id(),
        "the account belongs to the key the user still holds"
    );
}

#[tokio::test]
async fn the_seed_is_reachable_on_both_arms_without_matching_on_which() {
    // The obligation every one of the 7 app call-sites shares — persist the
    // successor seed before anything that can fail or block — does not depend on
    // which arm this is. Reaching it through a `match` is what lets a surface
    // persist on the confirmed arm only, which reproduces the same bug one layer up in
    // the app; the accessor exists so no surface has to.
    let (identity, client) = signed_in_lossy();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let healthy = succeed_with_held_kit(
        &client,
        identity.actor_id(),
        kit.secret_hex(),
        Some(&identity),
    )
    .await
    .unwrap();
    let from_confirmed = healthy.successor_secret_hex().to_string();
    assert_eq!(
        ActorKeypair::from_secret_hex(&from_confirmed)
            .unwrap()
            .actor_id(),
        healthy.successor_actor_id()
    );

    // Same account, now with the reply dropped — a second (doomed) ceremony,
    // which still mints a seed the accessor must surface.
    let (other, lossy) = signed_in_lossy();
    let other_kit = create_kit(&lossy, &other, None, &[]).await.unwrap();
    lossy
        .transport()
        .lose_reply_to(fauna_client_recovery::kinds::SUCCESSION_SUBMIT);
    let lost = succeed_with_held_kit(
        &lossy,
        other.actor_id(),
        other_kit.secret_hex(),
        Some(&other),
    )
    .await
    .unwrap();
    assert!(
        matches!(lost, SuccessionAttempt::Unconfirmed(_)),
        "the reply was dropped after the nest committed"
    );
    assert_eq!(
        ActorKeypair::from_secret_hex(lost.successor_secret_hex())
            .unwrap()
            .actor_id(),
        lost.successor_actor_id(),
        "the unconfirmed arm's seed is the key the account may already belong to"
    );
}

#[tokio::test]
async fn a_reconciled_handoff_carries_the_verbatim_statement_the_sweep_needs() {
    // Recovering the seed is not enough on its own: the ceremony's remaining
    // half is the per-group sweep, which refuses a statement whose pair does not
    // match its two engines and posts the bytes **verbatim**. A reconciliation
    // that handed back only actor ids would leave the successor unable to
    // finish, so the handoff must be the same shape a confirmed ceremony
    // produces — which is why it re-fetches rather than re-encoding.
    let (identity, client) = signed_in_lossy();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    client
        .transport()
        .lose_reply_to(fauna_client_recovery::kinds::SUCCESSION_SUBMIT);
    let unconfirmed = expect_unconfirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            kit.secret_hex(),
            Some(&identity),
        )
        .await
        .unwrap(),
    );

    let successor = ActorKeypair::from_secret_hex(unconfirmed.successor_secret_hex()).unwrap();
    let ReconciledSuccession::Landed(handoff) =
        reconcile_succession(&client, identity.actor_id(), &successor, None)
            .await
            .unwrap()
    else {
        panic!("the succession landed");
    };

    assert_eq!(
        handoff.statement.statement.old_actor_id,
        identity.actor_id()
    );
    assert_eq!(
        handoff.statement.statement.new_actor_id,
        successor.actor_id()
    );
    let served = client
        .succession_lookup(&identity.actor_id())
        .await
        .unwrap();
    assert_eq!(
        fauna_core::encoding::canonical_encode(&handoff.statement).unwrap(),
        fauna_core::encoding::canonical_encode(served.first().unwrap()).unwrap(),
        "the reconciled statement must be the bytes the nest serves, not a re-encode"
    );
    assert!(
        handoff.succeeded_at.is_none(),
        "the applied stamp lives in the reply that was lost — claiming one would be a guess"
    );
}

#[tokio::test]
async fn a_submit_that_never_landed_reconciles_to_not_landed() {
    // The other world the client cannot see from the error alone. Here the
    // submit failed *without* the nest acting, so the minted seed authorizes
    // nothing and the account is untouched — and the user may simply try again.
    //
    // The failure is a refusal, not a transport fault, which is the point:
    // `succeed_with_held_kit` deliberately makes **no** attempt to classify a
    // submit error, because the cost of classifying one wrongly is the account.
    // The nest, asked a second time, is what decides.
    let (identity, client) = signed_in_lossy();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    client
        .transport()
        .nest()
        .forget_kind(fauna_client_recovery::kinds::SUCCESSION_SUBMIT);

    let unconfirmed = expect_unconfirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            kit.secret_hex(),
            Some(&identity),
        )
        .await
        .unwrap(),
    );

    let successor = ActorKeypair::from_secret_hex(unconfirmed.successor_secret_hex()).unwrap();
    assert!(
        matches!(
            reconcile_succession(&client, identity.actor_id(), &successor, None)
                .await
                .unwrap(),
            ReconciledSuccession::NotLanded
        ),
        "nothing committed, so the seed is worthless and the account is still the old identity's"
    );
}

#[tokio::test]
async fn a_succession_that_landed_for_another_successor_is_named_as_such() {
    // Two devices, one kit, both pressed. The first re-points the account; the
    // second mints a successor the chain never authorizes. Reconciliation must
    // not round that up to "you are in" just because *a* succession exists —
    // the seed this caller holds is dead, and saying so is the only honest
    // answer available.
    let (identity, client) = signed_in_lossy();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();

    let first = expect_confirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            kit.secret_hex(),
            Some(&identity),
        )
        .await
        .unwrap(),
    );

    let second = expect_unconfirmed(
        succeed_with_held_kit(
            &client,
            identity.actor_id(),
            kit.secret_hex(),
            Some(&identity),
        )
        .await
        .unwrap(),
    );
    let loser = ActorKeypair::from_secret_hex(second.successor_secret_hex()).unwrap();

    match reconcile_succession(&client, identity.actor_id(), &loser, None)
        .await
        .unwrap()
    {
        ReconciledSuccession::LandedForAnother { new_actor_id } => {
            assert_eq!(new_actor_id, first.new_actor_id);
            assert_ne!(new_actor_id, loser.actor_id());
        }
        other => panic!("expected the first successor to be named, got {other:?}"),
    }
}

#[tokio::test]
async fn a_pre_submit_failure_stays_an_ordinary_error() {
    // The boundary is the submit call, not the mint. A kit the chain does not
    // name is caught before anything is sent, so the seed minted moments earlier
    // provably authorizes nothing — and the screen keeps the actionable "that is
    // not this account's kit" instead of being handed an unconfirmed ceremony to
    // explain. Widening `Unconfirmed` to cover this would make every wrong
    // phrase look like a possible account move.
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    // Replace it: the first kit is now retired, so it signs valid bytes that
    // authorize nothing.
    create_kit(
        &client,
        &identity,
        Some(&parse_kit(kit.secret_hex()).unwrap().recovery),
        &[],
    )
    .await
    .unwrap();

    let err = succeed_with_held_kit(
        &client,
        identity.actor_id(),
        kit.secret_hex(),
        Some(&identity),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorKitMismatch),
        "got {err:?}"
    );
    assert!(
        resolve_successor(&client, identity.actor_id(), None)
            .await
            .unwrap()
            .is_none(),
        "nothing was submitted, so nothing can have landed"
    );
}

#[tokio::test]
async fn succession_without_the_registered_kit_is_refused() {
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();
    let thiefs_key = RecoveryKey::generate();

    let err = succeed_identity(
        &client,
        identity.actor_id(),
        &thiefs_key,
        &ActorKeypair::generate(),
        Some(&identity),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, RecoveryError::PriorKitMismatch),
        "got {err:?}"
    );
    assert!(
        resolve_successor(&client, identity.actor_id(), None)
            .await
            .unwrap()
            .is_none(),
        "a seed holder without the RecoveryKey cannot succeed the account"
    );
}

#[tokio::test]
async fn an_identity_never_succeeded_resolves_to_nothing() {
    let (identity, client) = signed_in();
    create_kit(&client, &identity, None, &[]).await.unwrap();

    assert!(
        resolve_successor(&client, identity.actor_id(), None)
            .await
            .unwrap()
            .is_none(),
        "an empty lookup is the honest answer, never an error"
    );
}

#[tokio::test]
async fn a_supplied_head_refuses_a_truncated_chain_a_tofu_consumer_would_accept() {
    use fauna_core::data::Timestamp;
    use fauna_core::encoding::canonical_encode;
    use fauna_core::recovery::IdentitySuccession;

    // The pin: `Profile.recovery_head` mirrors the WHOLE chain head so
    // a consumer can anchor this walk. The scenario the anchor kills: the owner
    // replaced kit A with kit B (chain `[A@1, B@2]`); a thief who later
    // recovers the RETIRED paper kit A colludes with a chain server that
    // serves the chain rewound to `[A@1]` plus a succession signed by A at
    // seq 2 — every signature genuine, the rewind the only lie. A consumer
    // that cached the profile's head (B@2) refuses it; a first-contact
    // consumer (`None`) gets TOFU grade and accepts, which is exactly why a
    // consumer that HOLDS a head must pass it — and why the profile mirrors
    // both halves rather than a pubkey alone.
    let (identity, client) = signed_in();
    let kit_a = create_kit(&client, &identity, None, &[]).await.unwrap();
    let recovery_a = RecoveryKey::from_hex(kit_a.secret_hex()).unwrap();
    let kit_b = create_kit(&client, &identity, Some(&recovery_a), &[])
        .await
        .unwrap();
    // What a peer cached from the owner's signed profile: the coupled head.
    let cached_head = kit_b.chain_head();
    assert_eq!(cached_head.seq, 2, "the replacement landed at seq 2");

    // The hostile rewind + the retired-kit statement.
    let nest = client.transport();
    nest.truncate_chain(&identity.actor_id(), 1);
    let successor = ActorKeypair::generate();
    let statement = IdentitySuccession {
        old_actor_id: identity.actor_id(),
        new_actor_id: successor.actor_id(),
        recovery_pubkey: kit_a.recovery_pubkey,
        // Extends the TRUNCATED chain perfectly.
        seq: 2,
        created_at: Timestamp(1_753_000_000),
    };
    let signed = statement
        .sign(&recovery_a, successor.signing_key(), None)
        .unwrap();
    nest.plant_succession(&identity.actor_id(), canonical_encode(&signed).unwrap());

    // Anchored: the served chain never visits the head this consumer saw.
    let err = resolve_successor(&client, identity.actor_id(), Some(cached_head))
        .await
        .unwrap_err();
    assert!(
        matches!(err, RecoveryError::Crypto(_)),
        "a rewound chain must be refused against a supplied head, got {err:?}"
    );

    // Unanchored first contact: TOFU-grade, accepts the same bytes.
    let verified = resolve_successor(&client, identity.actor_id(), None)
        .await
        .unwrap()
        .expect("first contact has no head to anchor on and gets TOFU grade");
    assert_eq!(verified.new_actor_id, successor.actor_id());
}

#[tokio::test]
async fn the_old_device_is_refused_with_a_notice_that_routes_to_the_import() {
    let (identity, client) = signed_in();
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    let recovery = RecoveryKey::from_hex(kit.secret_hex()).unwrap();
    let successor = ActorKeypair::generate();
    succeed_identity(&client, identity.actor_id(), &recovery, &successor, None)
        .await
        .unwrap();

    // The old device's next call gets the refusal.
    let parsed = parse_kit(&kit.uri(None)).unwrap();
    let err = restore_seed(&client, &parsed, None).await.unwrap_err();

    let RecoveryError::Superseded { new_actor_id } = err else {
        panic!("expected a superseded refusal, got {err:?}");
    };
    assert_eq!(new_actor_id, successor.actor_id().0);
    assert!(
        !RecoveryError::Superseded { new_actor_id }.is_retryable(),
        "retrying strands the user in a reconnect loop instead of the import"
    );
}

#[tokio::test]
async fn the_superseded_notice_reads_only_its_own_code() {
    let successor = ActorKeypair::generate();
    let notice = SupersededNotice::from_error(&RpcError::superseded(&successor.actor_id().0))
        .expect("a superseded refusal yields a notice");
    assert_eq!(notice.claimed_successor, successor.actor_id());
    assert_eq!(notice.successor_hex(), successor.actor_id_hex());
    assert_eq!(
        notice.statement_kind, "fauna.recovery.succession.lookup",
        "the refusal names where to verify the claim"
    );

    // Any other code yields nothing — a client can call this on every error
    // without risk of pulling a successor out of an unrelated refusal.
    assert!(SupersededNotice::from_error(&RpcError::new("fauna.auth.forbidden", "nope")).is_none());
}

// ── The Settings status line (`ui/settings.md` § Recovery kit) ──────────────

#[tokio::test]
async fn the_status_walks_the_four_states_the_settings_section_renders() {
    // One test over the whole lifecycle rather than four isolated ones: the
    // states are defined by the *transitions* between them, and a per-state
    // test with a hand-built fixture would pass even if the ceremonies never
    // moved the account between them.
    let (identity, client) = signed_in();
    let actor = identity.actor_id();

    // 1. Never created — nothing on the chain. The standing post-skip warning.
    assert_eq!(
        kit_status(&client, &actor).await.unwrap(),
        RecoveryKitStatus::NeverCreated,
    );

    // 2. Registered — the kit ceremony also escrows, so the neutral state is
    //    what a completed creation leaves behind.
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    assert_eq!(
        kit_status(&client, &actor).await.unwrap(),
        RecoveryKitStatus::Registered,
    );

    // 3. Replacement pending — outranks the escrow question, and is the only
    //    state carrying a countdown.
    let _pending_kit = request_seed_alone_replacement(&client, &identity)
        .await
        .unwrap();
    let status = kit_status(&client, &actor).await.unwrap();
    let RecoveryKitStatus::ReplacementPending(pending) = &status else {
        panic!("a parked seed-alone replacement is the pending state, got {status:?}");
    };
    assert!(status.allows_veto(), "the section carries the veto action");
    assert!(
        pending.remaining_secs(0) > 0,
        "a freshly parked window has time left"
    );

    // 4. Vetoing with the kit in hand returns the account to Registered — the
    //    banner and the status line drop together, since both are projections
    //    of the nest's answer rather than stored flags.
    let parsed = parse_kit(&kit.uri(None)).unwrap();
    assert!(
        veto_pending_replacement(&client, &parsed, Some(actor))
            .await
            .unwrap()
    );
    assert_eq!(
        kit_status(&client, &actor).await.unwrap(),
        RecoveryKitStatus::Registered,
    );
}

#[tokio::test]
async fn a_registered_kit_whose_escrow_row_is_gone_reads_as_the_gap_not_as_healthy() {
    // The state that motivated the `escrow.status` probe. A signed-in device
    // cannot call `escrow.fetch` — that needs the offline-only RecoveryKey — so
    // without the probe this account would render as healthy while phrase
    // recovery was in fact unavailable.
    let (identity, client) = signed_in();
    let actor = identity.actor_id();

    // A registration lands but the escrow put fails; `create_kit` still returns
    // the kit, because by then the registration has landed and the returned
    // secret is the only copy in existence.
    client
        .transport()
        .fail_escrow_put("fauna.protocol.internal");
    let kit = create_kit(&client, &identity, None, &[]).await.unwrap();
    assert!(
        !kit.escrow.is_stored(),
        "this test needs the put to have failed: {:?}",
        kit.escrow
    );

    let status = kit_status(&client, &actor).await.unwrap();
    assert_eq!(status, RecoveryKitStatus::RegisteredNoEscrow);
    assert!(
        status.allows_replace(),
        "replacing with the kit in hand is the repair — it re-seals and re-puts"
    );
    assert!(
        !status.allows_create(),
        "a kit IS registered; offering create would open the wrong ceremony"
    );
}

#[tokio::test]
async fn an_unknown_kind_refusal_of_the_probe_is_an_error_not_a_neutral_state() {
    // The "older nest ⇒ neutral Registered" arm left with the compat-remnant
    // sweep: every nest serves `escrow.status`, so a refusal is reported.
    let (identity, client) = signed_in();
    let actor = identity.actor_id();
    create_kit(&client, &identity, None, &[]).await.unwrap();

    client
        .transport()
        .forget_kind("fauna.recovery.escrow.status");

    assert!(
        kit_status(&client, &actor).await.is_err(),
        "an unknown-kind refusal is an error, never a degraded status"
    );
}
