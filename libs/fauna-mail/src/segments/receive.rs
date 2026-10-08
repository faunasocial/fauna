//! Client-side inbound-mail decrypt — the receive counterpart to the outbound
//! seal.
//!
//! The `fauna.email.inbox.fetch` feed (`docs/goal/behavior/smtp-server.md`
//! § Inbound client receive) hands a client each message's **OUTER**
//! `super::MailRecordEnvelope` (the segment-record framing) as `sealed_envelope`
//! bytes. Opening it back to plaintext RFC 5322 is two layers:
//!
//!   1. decode the outer envelope → its `.encrypted_body` field, which is the
//!      **INNER** `fauna_mls::wrapped_blob::MailRecordEnvelope` (the HPKE seal —
//!      a distinct, same-named type);
//!   2. `unseal_mail_record` that inner envelope under the recipient's x25519
//!      secret (derived from the client-held MSEK via
//!      `fauna_mls::wrapped_blob::derive_recipient_hpke_keypair`).
//!
//! This is the ONE shared decrypt impl every receive-capable client calls
//! (priority #2: no per-app crypto), symmetric to the outbound seal the MTA
//! runs. The seam is the same `encrypted_body` join the nest-side
//! `bins/fauna-nest/tests/mail_inbound_seal_unseal_round_trip.rs` proves; the
//! difference is that the client read path ships the *outer* envelope (this
//! module decodes it first), whereas `fetch_message_ciphertext` (the MDA path)
//! already unwraps to the inner one.

use super::envelope::MailRecordEnvelope;

/// Failure opening an inbound mail record. Distinguishes the two decode layers
/// so a caller can tell a malformed segment envelope (corruption / version
/// skew) from a seal that does not open under this recipient's secret (wrong
/// key / not the recipient / tampered ciphertext).
#[derive(Debug, thiserror::Error)]
pub enum InboundOpenError {
    /// The outer segment-record envelope did not decode as canonical dag-cbor.
    #[error("decode outer mail segment envelope: {0}")]
    OuterDecode(#[from] fauna_cbor::DecodeError),
    /// The inner HPKE seal did not parse or did not open under the recipient
    /// secret.
    #[error("open inner sealed mail record: {0}")]
    InnerOpen(#[from] fauna_mls::wrapped_blob::UnwrapError),
}

/// Decode the OUTER segment-record envelope (`sealed_envelope` as shipped by
/// `fauna.email.inbox.fetch`) and unseal its inner HPKE body under the
/// recipient's MSEK-derived x25519 secret, returning the plaintext RFC 5322
/// bytes.
///
/// `recipient_x25519_secret` is the secret half of
/// `fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&msek)`; the nest
/// only ever holds the public half.
///
/// Every mail record rests sealed (the nest verifies the seal shape on every
/// write), so a payload that is not a sealed `MailRecordEnvelope` is refused
/// as [`InboundOpenError::InnerOpen`], never served verbatim.
pub fn open_inbound_record(
    outer_envelope_bytes: &[u8],
    recipient_x25519_secret: &[u8; 32],
) -> Result<Vec<u8>, InboundOpenError> {
    let outer = MailRecordEnvelope::decode(outer_envelope_bytes)?;
    open_sealed_inner_record(&outer.encrypted_body, recipient_x25519_secret)
}

/// Open a **bare INNER** sealed record — one HPKE layer, no outer segment-record
/// framing — under the recipient's MSEK-derived x25519 secret, returning the
/// plaintext bytes.
///
/// This is the shape `fauna.bridges.fetch_spam_model` returns (the per-user spam
/// model **sealed to the actor** via `seal_recipient_blob`, i.e. a
/// `fauna_mls::wrapped_blob::MailRecordEnvelope` encoded directly) and the shape
/// the MDA's `fetch_message_ciphertext` returns — **not** the two-layer
/// `fauna.email.inbox.fetch` feed [`open_inbound_record`] opens (which wraps this
/// inner envelope in an outer segment envelope). The on-device spam scorer
/// unwraps its model with this so every app uses the one shared decrypt
/// (priority #2). Both decode + seal-open failures surface as
/// [`InboundOpenError::InnerOpen`] (there is no outer layer here).
pub fn open_sealed_inner_record(
    inner_envelope_bytes: &[u8],
    recipient_x25519_secret: &[u8; 32],
) -> Result<Vec<u8>, InboundOpenError> {
    let inner =
        fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(inner_envelope_bytes)?;
    let plaintext = fauna_mls::wrapped_blob::unseal_mail_record(&inner, recipient_x25519_secret)?;
    Ok(plaintext)
}

/// Post-quantum-capable counterpart of [`open_inbound_record`]: opens a sealed
/// inbound record of **either** suite — classical X25519 or hybrid X-Wing
/// (ML-KEM-768 ∥ X25519). The caller derives both secret halves from the
/// client-held MSEK — `recipient_x25519_secret` via
/// `fauna_mls::wrapped_blob::derive_recipient_hpke_keypair`, and
/// `recipient_mlkem_dk` (the 2400-byte ML-KEM decapsulation key) via
/// `fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(&msek).secret.mlkem_decaps_key()`.
///
/// A classical blob ignores the ML-KEM half; a hybrid blob uses both. This is
/// the reader every receive-capable client migrates to so it can read both
/// pre-migration (classical) and post-migration (hybrid) mail — post-quantum
/// slice S3d, `architecture/security/post-quantum.md`. Kept as a sibling of the
/// classical opener (mirroring `unseal_mail_record` /
/// `unseal_mail_record_hybrid`) so callers migrate one at a time.
pub fn open_inbound_record_hybrid(
    outer_envelope_bytes: &[u8],
    recipient_x25519_secret: &[u8; 32],
    recipient_mlkem_dk: &[u8; fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN],
) -> Result<Vec<u8>, InboundOpenError> {
    let outer = MailRecordEnvelope::decode(outer_envelope_bytes)?;
    open_sealed_inner_record_hybrid(
        &outer.encrypted_body,
        recipient_x25519_secret,
        recipient_mlkem_dk,
    )
}

/// Post-quantum-capable counterpart of [`open_sealed_inner_record`]: opens a
/// **bare INNER** sealed record of either suite (classical X25519 or hybrid
/// X-Wing), the shape `fetch_spam_model` returns. See [`open_sealed_inner_record`]
/// for the one-vs-two-layer distinction and [`open_inbound_record_hybrid`] for
/// the secret-derivation of `recipient_mlkem_dk`.
pub fn open_sealed_inner_record_hybrid(
    inner_envelope_bytes: &[u8],
    recipient_x25519_secret: &[u8; 32],
    recipient_mlkem_dk: &[u8; fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN],
) -> Result<Vec<u8>, InboundOpenError> {
    let inner =
        fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(inner_envelope_bytes)?;
    let plaintext = fauna_mls::wrapped_blob::unseal_mail_record_hybrid(
        &inner,
        recipient_x25519_secret,
        recipient_mlkem_dk,
    )?;
    Ok(plaintext)
}

/// Epoch-aware counterpart of [`open_inbound_record_hybrid`]: open a sealed
/// inbound record (of **either** suite) via the content-sealing-epochs § 4
/// MSEK-holder trial chain, so a receive-capable client reads mail sealed under
/// a mail epoch key once the write flip is thrown (encryption-at-rest.md
/// § Capability tiering → Content-sealing epochs, flip-checklist line 1 — the
/// owner's client is an MSEK holder too, design § 2). The client is the caller
/// this exists for; the FFI holder/drain uses `MailRecordOpener::open_mail`, and
/// both share the trial order via
/// [`fauna_mls::wrapped_blob::open_mail_epoch_chain`] (priority #2).
///
/// - `epoch_roots` — the client's mail-epoch roots newest-generation-first:
///   `derive_mail_epoch_root(msek)` for the current MSEK, then one per prior
///   grace generation from `MailConfig.n` (§ 5 rotation grace). Empty ⇒
///   byte-identical to [`open_inbound_record_hybrid`].
/// - `record_unix_secs` — the record's **seal instant** (`InboxMessage.stored_at`).
///   NEVER a sender-supplied `Date:` header — imported mail diverges from its
///   seal instant by design.
///
/// The chain falls back to the standing recipient secret, so
/// standing-sealed mail reads unchanged.
pub fn open_inbound_record_epoch_hybrid(
    outer_envelope_bytes: &[u8],
    epoch_roots: &[&[u8; 32]],
    record_unix_secs: u64,
    standing_recipient_secret: &[u8; 32],
    standing_recipient_mlkem_dk: &[u8; fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN],
) -> Result<Vec<u8>, InboundOpenError> {
    open_inbound_epoch_inner(
        outer_envelope_bytes,
        epoch_roots,
        &[],
        record_unix_secs,
        |inner| {
            fauna_mls::wrapped_blob::unseal_mail_record_hybrid(
                inner,
                standing_recipient_secret,
                standing_recipient_mlkem_dk,
            )
            .ok()
        },
    )
}

/// The client receive path's opener: [`open_inbound_record_epoch_hybrid`] with
/// the **complete standing key set** — current generation plus EVERY prior
/// generation in `MailConfig.prior_mseks` — instead of a single standing
/// keypair, trialed in the seal-time order `retired_at_unix` (aligned with
/// `standing[1..]`, `MailConfig.prior_msek_retirements`) and
/// `record_unix_secs` select (`fauna_mls::wrapped_blob::generation_trial_order`). `standing` is
/// [`fauna_mls::wrapped_blob::derive_standing_mail_keypairs`] over the
/// client's MSEK history, the exact set the MDA reads out of the snapshot and
/// trials in `MailRecordOpener::open_mail`; the standing arm is the shared
/// [`fauna_mls::wrapped_blob::open_mail_record_standing`] on both, so a record
/// either holder can open, the other can too (`owner-key-material.md`
/// § Path B-sibling-2; `mail-app-surface.md` § Inbound client receive).
///
/// A miss here is **deterministic** for this key set: the seal is either not
/// addressed to any generation this client holds (a mailbox torn down and
/// re-enabled under a fresh MSEK — no rotation retires a generation out of
/// the set) or tampered.
/// Retrying with the same keys cannot change the answer, which is why the
/// receive loop skips such a record rather than blocking on it.
pub fn open_inbound_record_with_keys(
    outer_envelope_bytes: &[u8],
    epoch_roots: &[&[u8; 32]],
    record_unix_secs: u64,
    standing: &[fauna_mls::wrapped_blob::StandingMailKeypair],
    retired_at_unix: &[u64],
) -> Result<Vec<u8>, InboundOpenError> {
    open_inbound_epoch_inner(
        outer_envelope_bytes,
        epoch_roots,
        retired_at_unix,
        record_unix_secs,
        |inner| {
            fauna_mls::wrapped_blob::open_mail_record_standing(
                inner,
                standing,
                retired_at_unix,
                seal_basis(record_unix_secs),
            )
        },
    )
}

/// A record's seal basis for the generation trial: its seal instant, `0`
/// meaning unknown (the whole ring walks newest first).
fn seal_basis(record_unix_secs: u64) -> Option<u64> {
    (record_unix_secs != 0).then_some(record_unix_secs)
}

/// The **bare-INNER** twin of [`open_inbound_record_with_keys`]: one HPKE
/// layer, no outer segment-record framing, opened with the account's complete
/// standing key set and its mail-epoch roots.
///
/// This is the shape `fauna.bridges.fetch_export_chunk_ciphertext` returns —
/// and `fetch_message_ciphertext` before it — because both read the record's
/// `encrypted_body` straight out of the segment store rather than re-wrapping
/// it the way the `inbox.fetch` feed does. See [`open_sealed_inner_record`]
/// for the one-vs-two-layer distinction; this function differs from it in
/// exactly the way [`open_inbound_record_with_keys`] differs from
/// [`open_inbound_record`]: the **complete** current+grace key set and the
/// § 4 epoch chain, not a single generation.
///
/// **Use this, not the single-keypair siblings, on any path that reads a
/// user's own stored mail.** A single-generation opener goes blind to
/// pre-rotation standing mail and to everything sealed under a mail epoch key
/// — which on the receive path shows up as skipped messages and on the
/// **export** path would show up as an archive that is silently short, the
/// one outcome `mail-export.md` § Architectural rules puts below failing
/// outright. Callers on an egress path must therefore treat an `Err` here as
/// fatal to the run; the *skip* rule of `mail-app-surface.md` § Inbound client
/// receive is a receive-path rule and does not extend here.
pub fn open_sealed_inner_record_with_keys(
    inner_envelope_bytes: &[u8],
    epoch_roots: &[&[u8; 32]],
    record_unix_secs: u64,
    standing: &[fauna_mls::wrapped_blob::StandingMailKeypair],
    retired_at_unix: &[u64],
) -> Result<Vec<u8>, InboundOpenError> {
    let inner =
        fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(inner_envelope_bytes)?;
    fauna_mls::wrapped_blob::open_mail_epoch_chain(
        &inner,
        epoch_roots,
        retired_at_unix,
        record_unix_secs,
        |env| {
            fauna_mls::wrapped_blob::open_mail_record_standing(
                env,
                standing,
                retired_at_unix,
                seal_basis(record_unix_secs),
            )
        },
    )
    .ok_or(InboundOpenError::InnerOpen(
        fauna_mls::wrapped_blob::UnwrapError::HpkeFailed,
    ))
}

/// Classical-only sibling of [`open_inbound_record_epoch_hybrid`] — the standing
/// fallback is the X25519 recipient secret alone (an X-Wing record's *standing*
/// leg will not open, mirroring [`open_inbound_record`] vs its hybrid sibling).
/// The epoch trials themselves still open either suite from their single derived
/// `x25519 ∥ mlkem_dk` payload.
pub fn open_inbound_record_epoch(
    outer_envelope_bytes: &[u8],
    epoch_roots: &[&[u8; 32]],
    record_unix_secs: u64,
    standing_recipient_secret: &[u8; 32],
) -> Result<Vec<u8>, InboundOpenError> {
    open_inbound_epoch_inner(
        outer_envelope_bytes,
        epoch_roots,
        &[],
        record_unix_secs,
        |inner| fauna_mls::wrapped_blob::unseal_mail_record(inner, standing_recipient_secret).ok(),
    )
}

/// Shared body of the two epoch openers: outer segment-envelope decode, then
/// the § 4 trial chain on the inner HPKE envelope with the caller's standing
/// closure. A `None` from the chain
/// (no epoch key and the standing secret both missed) surfaces as
/// [`InboundOpenError::InnerOpen`], exactly like the standing openers' AEAD miss.
fn open_inbound_epoch_inner(
    outer_envelope_bytes: &[u8],
    epoch_roots: &[&[u8; 32]],
    retired_at_unix: &[u64],
    record_unix_secs: u64,
    standing: impl Fn(&fauna_mls::wrapped_blob::MailRecordEnvelope) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, InboundOpenError> {
    let outer = MailRecordEnvelope::decode(outer_envelope_bytes)?;
    let inner =
        fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(&outer.encrypted_body)?;
    fauna_mls::wrapped_blob::open_mail_epoch_chain(
        &inner,
        epoch_roots,
        retired_at_unix,
        record_unix_secs,
        standing,
    )
    .ok_or(InboundOpenError::InnerOpen(
        fauna_mls::wrapped_blob::UnwrapError::HpkeFailed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{
        derive_recipient_hpke_keypair, derive_recipient_xwing_keypair, seal_to_recipient,
    };

    /// Build the production outer `sealed_envelope` bytes the `inbox.fetch`
    /// feed ships: seal `body` to `recipient_pubkey`, canonical-encode the inner
    /// HPKE envelope into the outer envelope's `encrypted_body`, encode the
    /// outer. (The index-hint half is sealed too in production; not exercised
    /// here, so an empty hint keeps the fixture focused on the body seam.)
    fn build_sealed_envelope(body: &[u8], recipient_pubkey: &[u8; 32]) -> Vec<u8> {
        let inner = seal_to_recipient(body, recipient_pubkey)
            .expect("seal body")
            .to_canonical_bytes()
            .expect("encode inner envelope");
        MailRecordEnvelope::new(inner, Vec::new())
            .encode()
            .expect("encode outer envelope")
    }

    #[test]
    fn open_inbound_record_recovers_plaintext() {
        // Recipient key derived from a client-held MSEK exactly as `enable_mail`
        // does; the secret stands in for the user's client.
        let msek = [0x5e; 32];
        let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);

        let body: &[u8] = b"From: External Sender <sender@external.test>\r\n\
            To: alice@local.test\r\n\
            Subject: receive round-trip\r\n\
            \r\n\
            Hello from the client-side open_inbound_record test.\r\n";
        let sealed_envelope = build_sealed_envelope(body, &recipient_pubkey);

        let opened = open_inbound_record(&sealed_envelope, &recipient_secret)
            .expect("recipient opens the outer→inner sealed record");
        assert_eq!(
            opened.as_slice(),
            body,
            "received mail decrypts byte-for-byte back to the sent body"
        );
    }

    #[test]
    fn open_sealed_inner_record_recovers_model_shape() {
        // The `fetch_spam_model` blob is a BARE inner envelope — `seal_recipient_blob`
        // = `seal_to_recipient(...).to_canonical_bytes()`, NO outer segment framing
        // (unlike the `inbox.fetch` body feed). Prove the inner opener round-trips
        // exactly that shape (the on-device scorer's model-unwrap path).
        let msek = [0x7c; 32];
        let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
        let model_json: &[u8] = br#"{"version":1,"spam_ngrams":{},"ham_ngrams":{}}"#;
        let sealed_inner = seal_to_recipient(model_json, &recipient_pubkey)
            .expect("seal model")
            .to_canonical_bytes()
            .expect("encode inner envelope"); // == seal_recipient_blob's classical output

        let opened = open_sealed_inner_record(&sealed_inner, &recipient_secret)
            .expect("recipient opens the bare inner sealed model");
        assert_eq!(
            opened.as_slice(),
            model_json,
            "model decrypts byte-for-byte"
        );

        // Negative control: a non-recipient secret must not open it.
        let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0x11; 32]);
        assert!(
            matches!(
                open_sealed_inner_record(&sealed_inner, &wrong_secret),
                Err(InboundOpenError::InnerOpen(_))
            ),
            "wrong-key inner open must fail as InnerOpen"
        );
    }

    #[test]
    fn open_inbound_record_refuses_an_unsealed_payload() {
        // Every mail record rests sealed: the nest verifies the seal shape on
        // every write (`SealedRecordBytes::verify`). A payload that is not a
        // sealed `MailRecordEnvelope` is therefore corruption, and every opener
        // refuses it rather than serving the bytes verbatim.
        let msek = [0x5f; 32];
        let (recipient_secret, _) = derive_recipient_hpke_keypair(&msek);
        let raw_body: &[u8] = b"From: raw@plaintext.test\r\n\
            Subject: never sealed\r\n\
            \r\n\
            an unsealed payload inside the outer framing\r\n";
        let outer = MailRecordEnvelope::new(raw_body.to_vec(), Vec::new())
            .encode()
            .expect("encode outer envelope");

        assert!(
            matches!(
                open_inbound_record(&outer, &recipient_secret),
                Err(InboundOpenError::InnerOpen(_))
            ),
            "an unsealed payload must be refused, never passed through"
        );
        let xwing = derive_recipient_xwing_keypair(&msek);
        assert!(
            open_inbound_record_hybrid(&outer, &recipient_secret, xwing.secret.mlkem_decaps_key())
                .is_err(),
            "the hybrid opener refuses it too"
        );
        let standing = fauna_mls::wrapped_blob::derive_standing_mail_keypairs(&[msek]);
        assert!(
            open_inbound_record_with_keys(&outer, &[], 1_700_000_000, &standing, &[]).is_err(),
            "the keyset opener refuses it too"
        );
    }

    #[test]
    fn open_inbound_record_rejects_wrong_recipient() {
        // Negative control — gives the round-trip teeth. A different MSEK's
        // secret must NOT open the record (else success could be unrelated to
        // correct targeting).
        let (_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x42; 32]);
        let sealed_envelope = build_sealed_envelope(b"secret body", &recipient_pubkey);

        let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0xAB; 32]);
        let err = open_inbound_record(&sealed_envelope, &wrong_secret)
            .expect_err("a non-recipient secret must fail to open the record");
        assert!(
            matches!(err, InboundOpenError::InnerOpen(_)),
            "wrong-key failure must surface as an inner-open error, got {err:?}"
        );
    }

    #[test]
    fn open_inbound_record_rejects_malformed_outer_envelope() {
        // A blob that is not canonical dag-cbor must fail at the outer-decode
        // layer, distinct from an inner-open failure.
        let (secret, _) = derive_recipient_hpke_keypair(&[0x01; 32]);
        let err = open_inbound_record(&[0xff, 0xff, 0xff, 0xff], &secret)
            .expect_err("garbage outer bytes must fail to decode");
        assert!(
            matches!(err, InboundOpenError::OuterDecode(_)),
            "malformed outer envelope must surface as an outer-decode error, got {err:?}"
        );
    }

    #[test]
    fn open_inbound_record_hybrid_recovers_classical_and_xwing() {
        // S3d: the hybrid opener reads BOTH a classical (X25519) record and a
        // post-quantum (X-Wing) one sealed to the same MSEK-derived recipient,
        // given the x25519 secret + the MSEK-derived ML-KEM decaps key.
        use fauna_mls::wrapped_blob::{derive_recipient_xwing_keypair, seal_to_recipient_xwing};
        let msek = [0x5e; 32];
        let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let mdk = xwing.secret.mlkem_decaps_key();

        // Classical record opens via the hybrid path.
        let classical_env = build_sealed_envelope(b"classical body", &recipient_pubkey);
        let opened_classical = open_inbound_record_hybrid(&classical_env, &recipient_secret, mdk)
            .expect("hybrid opener reads a classical record");
        assert_eq!(opened_classical.as_slice(), b"classical body");

        // Hybrid (X-Wing) record opens via the hybrid path.
        let inner = seal_to_recipient_xwing(b"pq body", &xwing.public)
            .expect("seal xwing")
            .to_canonical_bytes()
            .expect("encode inner");
        let hybrid_env = MailRecordEnvelope::new(inner, Vec::new())
            .encode()
            .expect("encode outer");
        let opened_hybrid = open_inbound_record_hybrid(&hybrid_env, &recipient_secret, mdk)
            .expect("hybrid opener reads an X-Wing record");
        assert_eq!(opened_hybrid.as_slice(), b"pq body");

        // The classical opener must refuse the X-Wing record (loud, not silent).
        let err = open_inbound_record(&hybrid_env, &recipient_secret)
            .expect_err("classical opener must reject an X-Wing record");
        assert!(matches!(err, InboundOpenError::InnerOpen(_)));
    }

    /// Build the production outer `sealed_envelope` for a record sealed under
    /// the classical mail-epoch key `(root, e)` — the post-flip inbound shape.
    fn build_sealed_envelope_under_epoch(body: &[u8], root: &[u8; 32], e: u64) -> Vec<u8> {
        use fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair_from_root;
        let (_sk, pk) = derive_recipient_epoch_hpke_keypair_from_root(root, e);
        let inner = seal_to_recipient(body, &pk)
            .expect("seal body under epoch key")
            .to_canonical_bytes()
            .expect("encode inner envelope");
        MailRecordEnvelope::new(inner, Vec::new())
            .encode()
            .expect("encode outer envelope")
    }

    #[test]
    fn epoch_opener_reads_a_current_generation_epoch_record() {
        use fauna_mls::wrapped_blob::{MAIL_SEALING_EPOCH_SECS, derive_mail_epoch_root};
        let msek = [0x5e; 32];
        let (recipient_secret, _) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let root = derive_mail_epoch_root(&msek);
        let e = 900_500;
        let body: &[u8] = b"From: s@x.test\r\nSubject: epoch\r\n\r\nepoch body\r\n";
        let env = build_sealed_envelope_under_epoch(body, &root, e);
        let seal_instant = e * MAIL_SEALING_EPOCH_SECS + 3;

        // The epoch-aware opener reads it, keyed off the record's seal instant.
        let opened = open_inbound_record_epoch_hybrid(
            &env,
            &[&root],
            seal_instant,
            &recipient_secret,
            xwing.secret.mlkem_decaps_key(),
        )
        .expect("client reads its own epoch-sealed mail");
        assert_eq!(opened.as_slice(), body);

        // A pre-epoch (standing-only) client CANNOT — the exact § 6 gate reason.
        assert!(
            matches!(
                open_inbound_record_hybrid(
                    &env,
                    &recipient_secret,
                    xwing.secret.mlkem_decaps_key()
                ),
                Err(InboundOpenError::InnerOpen(_))
            ),
            "standing-only opener must AEAD-fail on epoch-sealed mail"
        );
    }

    #[test]
    fn epoch_opener_reads_a_prior_generation_epoch_record_via_grace_root() {
        use fauna_mls::wrapped_blob::{MAIL_SEALING_EPOCH_SECS, derive_mail_epoch_root};
        let current_msek = [0x33; 32];
        let prior_msek = [0x44; 32];
        // The recipient/standing material is the CURRENT generation's.
        let (recipient_secret, _) = derive_recipient_hpke_keypair(&current_msek);
        let xwing = derive_recipient_xwing_keypair(&current_msek);
        let current_root = derive_mail_epoch_root(&current_msek);
        let grace_root = derive_mail_epoch_root(&prior_msek);
        let e = 700_010;
        let body: &[u8] = b"sealed under a rotated-away MSEK's epoch key";
        let env = build_sealed_envelope_under_epoch(body, &grace_root, e);
        let seal_instant = e * MAIL_SEALING_EPOCH_SECS + 3;

        // Current generation's roots alone cannot open it.
        assert!(
            open_inbound_record_epoch_hybrid(
                &env,
                &[&current_root],
                seal_instant,
                &recipient_secret,
                xwing.secret.mlkem_decaps_key(),
            )
            .is_err(),
            "current-generation root must not open a prior generation's epoch record"
        );
        // Adding the grace root (from MailConfig.n) does (§ 5 rotation grace).
        let opened = open_inbound_record_epoch_hybrid(
            &env,
            &[&current_root, &grace_root],
            seal_instant,
            &recipient_secret,
            xwing.secret.mlkem_decaps_key(),
        )
        .expect("grace root opens the rotated-away epoch record");
        assert_eq!(opened.as_slice(), body);
    }

    /// The client receive path's key set after a rotate-mail-keys: a standing
    /// record sealed to the OUTGOING generation's pubkey still opens under the
    /// complete set (current + grace, `derive_standing_mail_keypairs` over the
    /// MSEK history), and does NOT open under the current generation alone —
    /// the drift this opener closes. And it STILL opens four rotations later:
    /// every generation is carried (`owner-key-material.md` § Path
    /// B-sibling-2 → *Pre-rotation mail at rest*) — the inversion of the
    /// retired 2-rotation window's past-cap arm — with or without the
    /// retirement instants that order the trial.
    #[test]
    fn keyset_opener_reads_pre_rotation_standing_mail_after_any_number_of_rotations() {
        use fauna_mls::wrapped_blob::derive_standing_mail_keypairs;
        let msek_old = [0x5e; 32];
        let msek_new = [0x6f; 32];
        let (_, old_pubkey) = derive_recipient_hpke_keypair(&msek_old);
        let body: &[u8] = b"From: s@x.test\r\nSubject: before rotation\r\n\r\nold body\r\n";
        let env = build_sealed_envelope(body, &old_pubkey);

        // Current only — what the client used to hold: the pre-rotation
        // record is unopenable.
        let current_only = derive_standing_mail_keypairs(&[msek_new]);
        let err = open_inbound_record_with_keys(&env, &[], 0, &current_only, &[])
            .expect_err("the current generation alone cannot open pre-rotation mail");
        assert!(matches!(err, InboundOpenError::InnerOpen(_)));

        // Current + grace — the snapshot's set: it opens.
        let with_grace = derive_standing_mail_keypairs(&[msek_new, msek_old]);
        assert_eq!(with_grace.len(), 2);
        let opened = open_inbound_record_with_keys(&env, &[], 0, &with_grace, &[])
            .expect("the grace keypair opens a record sealed before the rotation");
        assert_eq!(opened.as_slice(), body);

        // Three more rotations: the outgoing generation is now the oldest of
        // five, and still opens — sealed at 50, before every retirement.
        let mut history = vec![[0x70; 32], [0x71; 32], [0x72; 32]];
        history.push(msek_new);
        history.push(msek_old);
        let ring = derive_standing_mail_keypairs(&history);
        assert_eq!(ring.len(), 5, "uncapped");
        for (instants, at) in [(&[400u64, 300, 200, 100][..], 50), (&[][..], 0)] {
            let opened = open_inbound_record_with_keys(&env, &[], at, &ring, instants)
                .expect("a generation four rotations old still opens its mail");
            assert_eq!(opened.as_slice(), body);
        }
    }

    /// The export down-leg's opener over the shape that leg actually returns:
    /// a **bare inner** envelope (`fetch_export_chunk_ciphertext` hands back
    /// the record's `encrypted_body` verbatim, not the outer-framed
    /// `inbox.fetch` shape). Same rotation property as the two-layer twin —
    /// and the single-generation sibling that predates it is measurably blind
    /// to the pre-rotation record an export would then silently omit.
    #[test]
    fn bare_inner_keyset_opener_reads_pre_rotation_mail_the_single_key_one_misses() {
        use fauna_mls::wrapped_blob::derive_standing_mail_keypairs;
        let msek_old = [0x21; 32];
        let msek_new = [0x22; 32];
        let (old_secret, old_pubkey) = derive_recipient_hpke_keypair(&msek_old);
        let body: &[u8] = b"From: s@x.test\r\nSubject: exportable\r\n\r\nbefore rotation\r\n";
        // Exactly what the nest reads out of the segment store: the inner
        // envelope's canonical bytes, with no outer framing.
        let inner = seal_to_recipient(body, &old_pubkey)
            .expect("seal body")
            .to_canonical_bytes()
            .expect("encode inner envelope");

        // Sanity: the single-keypair sibling opens it under the matching
        // generation, so the failure below is about the key set, not the shape.
        assert_eq!(
            open_sealed_inner_record(&inner, &old_secret).expect("matching generation opens"),
            body
        );

        let current_only = derive_standing_mail_keypairs(&[msek_new]);
        let err = open_sealed_inner_record_with_keys(&inner, &[], 0, &current_only, &[])
            .expect_err("the current generation alone cannot open pre-rotation mail");
        assert!(matches!(err, InboundOpenError::InnerOpen(_)));

        let with_grace = derive_standing_mail_keypairs(&[msek_new, msek_old]);
        assert_eq!(
            open_sealed_inner_record_with_keys(&inner, &[], 0, &with_grace, &[])
                .expect("the grace keypair opens a record sealed before the rotation"),
            body
        );
    }

    /// The epoch arm on the bare-inner opener: an epoch-sealed record opens
    /// from the roots, and a body that was never sealed at all is refused.
    #[test]
    fn bare_inner_keyset_opener_reads_epoch_sealed_and_refuses_raw_bodies() {
        use fauna_mls::wrapped_blob::{
            derive_mail_epoch_root, derive_recipient_epoch_xwing_keypair_from_root,
            mail_sealing_epoch_of, seal_to_recipient_xwing,
        };
        let msek = [0x31; 32];
        let root = derive_mail_epoch_root(&msek);
        let at = 1_760_000_000u64;
        let epoch = mail_sealing_epoch_of(at);
        let epoch_kp = derive_recipient_epoch_xwing_keypair_from_root(&root, epoch);
        let body: &[u8] = b"From: s@x.test\r\nSubject: epoch\r\n\r\nepoch-sealed\r\n";
        let inner = seal_to_recipient_xwing(body, &epoch_kp.public)
            .expect("epoch seal")
            .to_canonical_bytes()
            .expect("encode inner envelope");

        let standing = fauna_mls::wrapped_blob::derive_standing_mail_keypairs(&[msek]);
        assert_eq!(
            open_sealed_inner_record_with_keys(&inner, &[&*root], at, &standing, &[])
                .expect("the epoch root opens an epoch-sealed record"),
            body
        );
        // Without the root the standing arm alone must miss — the epoch leg is
        // load-bearing here, not decoration.
        assert!(open_sealed_inner_record_with_keys(&inner, &[], at, &standing, &[]).is_err());

        // An unsealed body is refused, never served verbatim.
        let raw: &[u8] = b"From: s@x.test\r\nSubject: raw\r\n\r\nnever sealed\r\n";
        assert!(
            matches!(
                open_sealed_inner_record_with_keys(raw, &[], at, &standing, &[]),
                Err(InboundOpenError::InnerOpen(_))
            ),
            "a raw body must be refused"
        );
    }

    #[test]
    fn epoch_opener_reads_standing_records_and_refuses_raw_ones() {
        use fauna_mls::wrapped_blob::derive_mail_epoch_root;
        let msek = [0x5e; 32];
        let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let root = derive_mail_epoch_root(&msek);

        // A standing-sealed record opens via the epoch opener's standing fallback
        // (standing-sealed content stays readable).
        let body: &[u8] = b"standing-sealed body";
        let env = build_sealed_envelope(body, &recipient_pubkey);
        let opened = open_inbound_record_epoch_hybrid(
            &env,
            &[&root],
            1_700_000_000,
            &recipient_secret,
            xwing.secret.mlkem_decaps_key(),
        )
        .expect("epoch opener still reads standing-sealed mail");
        assert_eq!(opened.as_slice(), body);

        // An unsealed payload is refused.
        let raw: &[u8] = b"From: raw@x.test\r\n\r\nnever-sealed body\r\n";
        let outer = MailRecordEnvelope::new(raw.to_vec(), Vec::new())
            .encode()
            .expect("encode outer");
        assert!(
            open_inbound_record_epoch_hybrid(
                &outer,
                &[&root],
                1_700_000_000,
                &recipient_secret,
                xwing.secret.mlkem_decaps_key(),
            )
            .is_err(),
            "an unsealed payload must be refused"
        );
    }
}
