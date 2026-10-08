//! `MailRecordOpener` — the per-connection F2 opener (Phase-3 S2).
//!
//! The MDA constructs one opener per AUTH'd session from the cached
//! `MlsSnapshotPlaintext` bytes (parsed ONCE), then opens each record
//! with it — replacing the per-open snapshot marshal + re-parse of
//! `MlsCapability::open_mail_record` (the F2 finding, ~18 ms/FETCH).
//! Semantics must be identical to `open_mail_record`: leaf keypairs
//! tried in order (grace rotations), hybrid dispatch per entry, typed
//! errors, zeroize support. An unsealed payload is refused at open —
//! every mail-plane record rests sealed, so there is no raw pass-through.

use fauna_ffi::{
    KdfKind, MailRecordOpener, is_sealed_mail_record, seal_to_recipient, unwrap_msek_blob,
};
use fauna_mls::wrapped_blob::{
    Argon2idParams, CredentialInput, KdfParams, LeafInitKeypair, MAIL_EPOCH_PUBLISH_HORIZON,
    MAIL_SEALING_EPOCH_SECS, MlsSnapshotPlaintext, build_mls_snapshot_plaintext,
    derive_recipient_epoch_hpke_keypair, derive_recipient_epoch_xwing_keypair,
    derive_recipient_hpke_keypair, derive_recipient_xwing_keypair, generate_x25519_keypair,
    mail_sealing_epoch_of, seal_to_recipient_xwing, seal_wrapped_msek,
};

fn snapshot_bytes(keypairs: Vec<LeafInitKeypair>) -> Vec<u8> {
    MlsSnapshotPlaintext {
        leaf_init_keypairs: keypairs,
        ..Default::default()
    }
    .to_canonical_bytes()
    .expect("encode snapshot plaintext")
}

/// Happy path: parse-once construction + per-record open round-trip.
#[test]
fn opener_round_trip_with_matching_leaf_keypair() {
    let (sk, pk) = generate_x25519_keypair();
    let plaintext = b"From: alice@example.com\r\n\r\nbody".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    let opener =
        MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(pk, sk)])).expect("new");
    let opened = opener.open(envelope_bytes).expect("open round-trip");
    assert_eq!(opened, plaintext);
}

/// Grace-decrypt parity with `open_mail_record`: the seal targets an
/// older rotation; the opener tries every keypair.
#[test]
fn opener_succeeds_with_older_rotation_in_snapshot() {
    let (sk_old, pk_old) = generate_x25519_keypair();
    let (sk_cur, pk_cur) = generate_x25519_keypair();
    let plaintext = b"old rotation body".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk_old.to_vec()).expect("seal");

    let opener = MailRecordOpener::new(snapshot_bytes(vec![
        LeafInitKeypair::new(pk_cur, sk_cur),
        LeafInitKeypair::new(pk_old, sk_old),
    ]))
    .expect("new");
    let opened = opener
        .open(envelope_bytes)
        .expect("open with older rotation");
    assert_eq!(opened, plaintext);
}

/// A snapshot entry carrying the MSEK-derived ML-KEM decapsulation key
/// opens a hybrid (X-Wing) record — the S3d dispatch, per entry.
#[test]
fn opener_opens_hybrid_record_when_entry_carries_mlkem_dk() {
    let msek = [0x77u8; 32];
    let xwing = derive_recipient_xwing_keypair(&msek);
    let plaintext = b"pq-sealed body".to_vec();
    let envelope_bytes = seal_to_recipient_xwing(&plaintext, &xwing.public)
        .expect("seal xwing")
        .to_canonical_bytes()
        .expect("encode");

    // The X25519 half of the X-Wing keypair is the MSEK-derived recipient
    // keypair, reused (the `build_mls_snapshot_plaintext` construction).
    let (sk, pk) = derive_recipient_hpke_keypair(&msek);
    let kp = LeafInitKeypair::new_hybrid(pk, sk, xwing.secret.mlkem_decaps_key());
    let opener = MailRecordOpener::new(snapshot_bytes(vec![kp])).expect("new");
    let opened = opener.open(envelope_bytes).expect("open hybrid");
    assert_eq!(opened, plaintext);
}

/// No matching keypair → HPKE-flavored error (parity with
/// `open_mail_record_fails_when_no_keypair_matches`).
#[test]
fn opener_fails_when_no_keypair_matches() {
    let (_seal_sk, pk_seal) = generate_x25519_keypair();
    let envelope_bytes =
        seal_to_recipient(b"unreachable body".to_vec(), pk_seal.to_vec()).expect("seal");

    let (sk_other, pk_other) = generate_x25519_keypair();
    let opener = MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(
        pk_other, sk_other,
    )]))
    .expect("new");
    let err = opener
        .open(envelope_bytes)
        .expect_err("no matching leaf must HPKE-fail");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("hpke") || msg.contains("no matching"),
        "expected HPKE/no-matching error, got: {err}"
    );
}

/// Empty / malformed snapshots are rejected at CONSTRUCTION (the parse
/// happens once), not at open time.
#[test]
fn opener_construction_rejects_empty_and_malformed_snapshots() {
    let err = MailRecordOpener::new(snapshot_bytes(vec![]))
        .expect_err("empty snapshot keypair list must fail");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("no leaf") || msg.contains("empty"),
        "expected no-leaf-keypairs error, got: {err}"
    );

    let err = MailRecordOpener::new(b"not-cbor".to_vec())
        .expect_err("malformed snapshot must fail format-side");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("snapshot") || msg.contains("decode") || msg.contains("cbor"),
        "expected snapshot/decode error, got: {err}"
    );
}

/// A non-envelope input (an unsealed payload) is a DECODE error at open,
/// never a pass-through, and the discriminator agrees on both shapes.
#[test]
fn opener_rejects_non_envelope_input_and_discriminator_agrees() {
    let (sk, pk) = generate_x25519_keypair();
    let opener =
        MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(pk, sk)])).expect("new");

    let raw = b"From: a@example.com\r\nSubject: unsealed plaintext\r\n\r\nbody\r\n".to_vec();
    assert!(!is_sealed_mail_record(raw.clone()));
    let err = opener
        .open(raw)
        .expect_err("raw record must be a decode error, not a silent pass-through");
    assert!(
        format!("{err}").to_lowercase().contains("decode"),
        "expected decode error, got: {err}"
    );

    let sealed = seal_to_recipient(b"x".to_vec(), pk.to_vec()).expect("seal");
    assert!(is_sealed_mail_record(sealed));
}

/// After `zeroize()`, the opener refuses further opens (session-close
/// parity with `MlsCapability`).
#[test]
fn opener_fails_after_zeroize() {
    let (sk, pk) = generate_x25519_keypair();
    let envelope_bytes = seal_to_recipient(b"x".to_vec(), pk.to_vec()).expect("seal");
    let opener =
        MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(pk, sk)])).expect("new");
    opener.zeroize();
    let err = opener
        .open(envelope_bytes)
        .expect_err("zeroized opener must refuse open");
    assert!(
        format!("{err}").to_lowercase().contains("zeroized"),
        "expected zeroized error, got: {err}"
    );
}

// ── `open_mail` — the MSEK-holder epoch trial (content-sealing-epochs
// design § 4) ────────────────────────────────────────────────────────

const ACTOR: [u8; 32] = [0x99u8; 32];
const CRED_ID: &str = "cred-1";

fn small_argon2id() -> KdfParams {
    KdfParams::Argon2id(Argon2idParams {
        m: 4096,
        t: 1,
        p: 1,
    })
}

/// Build the epoch-aware opener the same way the IMAP MDA does at AUTH:
/// AEAD-unwrap a `WrappedMsekBlob` into an `MlsCapability`, then construct
/// via `new_epoch_aware_mail_record_opener` (never `MailRecordOpener::new`,
/// which never carries an MSEK).
fn epoch_aware_opener(
    msek: &[u8; 32],
    standing_keypairs: Vec<LeafInitKeypair>,
) -> std::sync::Arc<MailRecordOpener> {
    epoch_aware_opener_with_snapshot(msek, snapshot_bytes(standing_keypairs))
}

/// [`epoch_aware_opener`] over caller-supplied snapshot bytes — for the
/// MSEK-rotation grace tests, which need a snapshot carrying
/// `mail_epoch_grace_roots` (the production `build_mls_snapshot_plaintext`
/// output after a rotation).
fn epoch_aware_opener_with_snapshot(
    msek: &[u8; 32],
    snapshot: Vec<u8>,
) -> std::sync::Arc<MailRecordOpener> {
    let blob = seal_wrapped_msek(
        msek,
        &ACTOR,
        CRED_ID,
        &CredentialInput::Plain(b"correct-password"),
        small_argon2id(),
    )
    .expect("seal wrapped msek")
    .to_canonical_bytes()
    .expect("encode wrapped msek blob");
    let cap = unwrap_msek_blob(
        blob,
        b"correct-password".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap msek blob");
    cap.new_epoch_aware_mail_record_opener(snapshot)
        .expect("new_epoch_aware_mail_record_opener")
}

/// A record sealed under the record's OWN target epoch key opens via the
/// first trial (`K_{epoch_of(t)}`) — the common case once the flip lands.
#[test]
fn open_mail_opens_via_current_epoch_key() {
    let msek = [0x21u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let (_sk, pk) = derive_recipient_epoch_hpke_keypair(&msek, target);
    let plaintext = b"epoch-sealed body".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    // No standing keypair at all — the near-pair epoch trial must be what
    // opens this, not a fallthrough to the (absent) standing chain.
    let (sk_unrelated, pk_unrelated) = generate_x25519_keypair();
    let opener = epoch_aware_opener(
        &msek,
        vec![LeafInitKeypair::new(pk_unrelated, sk_unrelated)],
    );
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("open via current epoch key");
    assert_eq!(opened, plaintext);
}

/// The epoch trial's derived secret is always the full hybrid (X-Wing)
/// payload, so a HYBRID-sealed epoch record must open too, not just a
/// classical one — `unseal_mail_record_hybrid`'s dispatch must correctly
/// pick the epoch X-Wing keypair.
#[test]
fn open_mail_opens_hybrid_record_via_current_epoch_key() {
    let msek = [0x26u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let xwing = derive_recipient_epoch_xwing_keypair(&msek, target);
    let plaintext = b"epoch-sealed hybrid body".to_vec();
    let envelope_bytes = seal_to_recipient_xwing(&plaintext, &xwing.public)
        .expect("seal xwing")
        .to_canonical_bytes()
        .expect("encode");

    let (sk_unrelated, pk_unrelated) = generate_x25519_keypair();
    let opener = epoch_aware_opener(
        &msek,
        vec![LeafInitKeypair::new(pk_unrelated, sk_unrelated)],
    );
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("open hybrid via current epoch key");
    assert_eq!(opened, plaintext);
}

/// A record sealed under the IMMEDIATELY PRIOR epoch's key still opens —
/// the boundary/clock-skew tolerance (design § 4's second trial).
#[test]
fn open_mail_opens_via_prior_epoch_key_boundary_tolerance() {
    let msek = [0x22u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let (_sk, pk) = derive_recipient_epoch_hpke_keypair(&msek, target - 1);
    let plaintext = b"boundary-sealed body".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    let (sk_unrelated, pk_unrelated) = generate_x25519_keypair();
    let opener = epoch_aware_opener(
        &msek,
        vec![LeafInitKeypair::new(pk_unrelated, sk_unrelated)],
    );
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("open via prior epoch key");
    assert_eq!(opened, plaintext);
}

/// An epoch-aware opener still opens standing-key content (no published
/// schedule) sealed to
/// the standing key — the epoch trials miss, then the standing/grace chain
/// (identical to `open`) picks it up.
#[test]
fn open_mail_falls_through_to_standing_key_content() {
    let msek = [0x23u8; 32];
    let (sk, pk) = derive_recipient_hpke_keypair(&msek);
    let plaintext = b"standing-sealed body".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    let opener = epoch_aware_opener(&msek, vec![LeafInitKeypair::new(pk, sk)]);
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("open standing-key content via standing fallback");
    assert_eq!(opened, plaintext);
}

/// A record sealed under a STALE published schedule (older than the near
/// pair, but within the publish horizon) opens via the bounded back-scan —
/// only reached once the near pair AND the standing chain have missed.
#[test]
fn open_mail_finds_stale_schedule_epoch_via_backscan() {
    let msek = [0x24u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let stale_epoch = target - 5; // within MAIL_EPOCH_PUBLISH_HORIZON (26)
    let (_sk, pk) = derive_recipient_epoch_hpke_keypair(&msek, stale_epoch);
    let plaintext = b"stale-schedule body".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    // A standing keypair that does NOT match, so the middle arm genuinely
    // misses too (proves the back-scan arm, not an earlier one).
    let (sk_unrelated, pk_unrelated) = generate_x25519_keypair();
    let opener = epoch_aware_opener(
        &msek,
        vec![LeafInitKeypair::new(pk_unrelated, sk_unrelated)],
    );
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("open via bounded back-scan");
    assert_eq!(opened, plaintext);
}

/// The back-scan is BOUNDED at `MAIL_EPOCH_PUBLISH_HORIZON` — content
/// sealed further back than that stays dark even to the MSEK holder
/// (fail-closed, not an unbounded search).
#[test]
fn open_mail_stays_dark_beyond_the_backscan_horizon() {
    let msek = [0x25u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let too_stale_epoch = target - (MAIL_EPOCH_PUBLISH_HORIZON + 1);
    let (_sk, pk) = derive_recipient_epoch_hpke_keypair(&msek, too_stale_epoch);
    let envelope_bytes = seal_to_recipient(b"unreachable".to_vec(), pk.to_vec()).expect("seal");

    let (sk_unrelated, pk_unrelated) = generate_x25519_keypair();
    let opener = epoch_aware_opener(
        &msek,
        vec![LeafInitKeypair::new(pk_unrelated, sk_unrelated)],
    );
    let err = opener
        .open_mail(envelope_bytes, record_ts)
        .expect_err("content beyond the publish horizon must stay dark");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("hpke") || msg.contains("no matching"),
        "expected HPKE/no-matching error, got: {err}"
    );
}

/// An opener built via the plain (non-epoch-aware) `new` constructor — the
/// CalDAV/CardDAV construction path — has no MSEK, so `open_mail` behaves
/// byte-identically to `open`: standing content opens, epoch-sealed content
/// (which this opener could never have been given a schedule for) does not
/// get any special epoch trial.
#[test]
fn open_mail_matches_open_when_opener_has_no_msek() {
    let (sk, pk) = generate_x25519_keypair();
    let plaintext = b"standing body, non-epoch-aware opener".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), pk.to_vec()).expect("seal");

    let opener =
        MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(pk, sk)])).expect("new");
    let opened = opener
        .open_mail(envelope_bytes, 500 * MAIL_SEALING_EPOCH_SECS + 10)
        .expect("open_mail without msek still opens standing content");
    assert_eq!(opened, plaintext);
}

/// MSEK-rotation grace for epoch keys (design § 5, Track 1c): a record
/// epoch-sealed under the OLD generation's root before a hard-revoke opens
/// through an opener built with the NEW MSEK, because the post-rotation
/// snapshot (the production `build_mls_snapshot_plaintext([new, old], ..)`
/// output) carries the old generation's `mail_epoch_grace_root`. The same
/// record stays dark to an opener whose snapshot has no grace root — the
/// grace material, not the new MSEK, is what opens it.
#[test]
fn open_mail_opens_old_root_epoch_content_via_grace_root() {
    let old_msek = [0x31u8; 32];
    let new_msek = [0x32u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let target = mail_sealing_epoch_of(record_ts);
    let (_, old_pk) = derive_recipient_epoch_hpke_keypair(&old_msek, target);
    let plaintext = b"epoch-sealed before rotation".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), old_pk.to_vec()).expect("seal");

    // The post-rotation snapshot, exactly as rotation.rs builds it.
    let rotated = build_mls_snapshot_plaintext(&[new_msek, old_msek], &[])
        .to_canonical_bytes()
        .expect("encode rotated snapshot");
    let opener = epoch_aware_opener_with_snapshot(&new_msek, rotated);
    let opened = opener
        .open_mail(envelope_bytes.clone(), record_ts)
        .expect("grace root opens old-root epoch content");
    assert_eq!(opened, plaintext);

    // Without the grace root (single-generation snapshot) the record is
    // dark — proving the grace material is load-bearing.
    let bare = build_mls_snapshot_plaintext(&[new_msek], &[])
        .to_canonical_bytes()
        .expect("encode bare snapshot");
    let opener = epoch_aware_opener_with_snapshot(&new_msek, bare);
    assert!(opener.open_mail(envelope_bytes, record_ts).is_err());
}

/// The grace trial composes with the § 3 stale-schedule back-scan: a record
/// sealed under an OLD-root epoch key several epochs behind its stored
/// instant (stale schedule at seal time, then a rotation) still opens —
/// the back-scan runs per generation root.
#[test]
fn open_mail_backscan_runs_per_grace_root() {
    let old_msek = [0x33u8; 32];
    let new_msek = [0x34u8; 32];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    let stale_epoch = mail_sealing_epoch_of(record_ts) - 5;
    let (_, old_stale_pk) = derive_recipient_epoch_hpke_keypair(&old_msek, stale_epoch);
    let plaintext = b"stale-schedule seal, then rotation".to_vec();
    let envelope_bytes = seal_to_recipient(plaintext.clone(), old_stale_pk.to_vec()).expect("seal");

    let rotated = build_mls_snapshot_plaintext(&[new_msek, old_msek], &[])
        .to_canonical_bytes()
        .expect("encode rotated snapshot");
    let opener = epoch_aware_opener_with_snapshot(&new_msek, rotated);
    let opened = opener
        .open_mail(envelope_bytes, record_ts)
        .expect("per-root back-scan opens stale old-root epoch content");
    assert_eq!(opened, plaintext);
}

/// **Every generation is carried** (`owner-key-material.md` § Path
/// B-sibling-2 → *Pre-rotation mail at rest*): after FOUR rotations the MDA's
/// opener, built from the five-generation snapshot with its retirement
/// instants, still opens both a standing-sealed and an epoch-sealed record
/// sealed under the oldest generation — the window the retired cap-3
/// snapshot closed — keyed off the record's seal basis.
#[test]
fn open_mail_opens_the_oldest_of_five_generations_by_seal_time() {
    let history: Vec<[u8; 32]> = (0x41u8..=0x45).map(|b| [b; 32]).collect();
    let oldest = history[4];
    let record_ts = 500 * MAIL_SEALING_EPOCH_SECS + 10;
    // Each prior retired one epoch apart, all after the record was sealed.
    let retired: Vec<u64> = (1..=4u64)
        .rev()
        .map(|k| record_ts + k * MAIL_SEALING_EPOCH_SECS)
        .collect();
    let snapshot = build_mls_snapshot_plaintext(&history, &retired)
        .to_canonical_bytes()
        .expect("encode five-generation snapshot");
    let opener = epoch_aware_opener_with_snapshot(&history[0], snapshot);

    let (_, standing_pk) = derive_recipient_hpke_keypair(&oldest);
    let standing = seal_to_recipient(
        b"standing, four rotations ago".to_vec(),
        standing_pk.to_vec(),
    )
    .expect("seal standing");
    assert_eq!(
        opener
            .open_mail(standing.clone(), record_ts)
            .expect("opens by seal time"),
        b"standing, four rotations ago"
    );
    assert_eq!(
        opener
            .open_mail(standing, 0)
            .expect("opens with an unknown basis too"),
        b"standing, four rotations ago"
    );

    let (_, epoch_pk) =
        derive_recipient_epoch_hpke_keypair(&oldest, mail_sealing_epoch_of(record_ts));
    let epoch = seal_to_recipient(b"epoch, four rotations ago".to_vec(), epoch_pk.to_vec())
        .expect("seal epoch");
    assert_eq!(
        opener
            .open_mail(epoch, record_ts)
            .expect("the oldest grace root opens it"),
        b"epoch, four rotations ago"
    );
}

/// The example.com CalDAV flood, reproduced at its mechanism.
///
/// An actor whose snapshot carries no `mdk` field
/// has a snapshot whose sole leaf
/// keypair is CLASSICAL. The moment that actor's client publishes the ML-KEM
/// encapsulation key, the MDA seals collection metadata X-Wing — and this
/// opener, holding only the classical half, cannot open a single one. The
/// error text is verbatim what `caldav: skipping calendar with undecryptable
/// metadata` logs, and the CalDAV read path answers it by dropping the
/// calendar from the PROPFIND home set: silent invisibility, not an error the
/// user ever sees.
///
/// This test pins the FAILURE (it is the harm being prevented, not a
/// regression target) — the pairing that stops an actor from ever reaching
/// this state is enforced client-side, where both halves are published:
/// `fauna-client-mail-settings`'s
/// `a_connect_refresh_that_publishes_an_ek_also_rewrites_the_snapshot`.
#[test]
fn a_classical_only_snapshot_cannot_open_hybrid_sealed_collection_metadata() {
    let msek = [0x5au8; 32];
    // What the MDA seals to once the client has published the ek.
    let xwing = derive_recipient_xwing_keypair(&msek);
    let plaintext = b"{displayname: Personal, color: #3273dc}".to_vec();
    let envelope_bytes = seal_to_recipient_xwing(&plaintext, &xwing.public)
        .expect("seal hybrid")
        .to_canonical_bytes()
        .expect("encode");

    // What a classical-only snapshot carries: the X25519 half alone, no `mdk`.
    let (sk, pk) = derive_recipient_hpke_keypair(&msek);
    let opener =
        MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new(pk, sk)])).expect("new");

    let err = opener
        .open(envelope_bytes.clone())
        .expect_err("a classical leaf keypair cannot open an X-Wing envelope");
    assert!(
        format!("{err:?}").contains("no matching leaf keypair in snapshot"),
        "the harm surfaces as the CalDAV flood's exact error, got: {err:?}",
    );

    // Same MSEK, same envelope — carrying the `mdk` half is the whole
    // difference, which is what makes the paired publish load-bearing.
    let healed = MailRecordOpener::new(snapshot_bytes(vec![LeafInitKeypair::new_hybrid(
        pk,
        sk,
        xwing.secret.mlkem_decaps_key(),
    )]))
    .expect("new");
    healed
        .open(envelope_bytes)
        .expect("the hybrid leaf keypair opens what the MDA sealed");
}
