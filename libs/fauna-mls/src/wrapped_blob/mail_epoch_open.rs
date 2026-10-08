//! The content-sealing-epochs § 4 MSEK-holder trial-decrypt chain, shared by
//! the two MSEK-holder open paths so the trial order lives in exactly one place
//! (priority #2 — no per-consumer crypto): the FFI holder/drain opener
//! (`fauna-ffi`'s `MailRecordOpener::open_mail`, the AUTH'd MDA session) and the
//! client receive path (`fauna-mail::segments::open_inbound_record_epoch*`,
//! every receive-capable client — the owner's clients are MSEK holders too,
//! design § 2). Design authority: the content-sealing-epochs design (tracked
//! internally) § 4 (opener key selection) + § 5 (MSEK-rotation grace).

use zeroize::Zeroizing;

use super::{
    MAIL_EPOCH_PUBLISH_HORIZON, MLKEM768_DECAPS_KEY_LEN, MailRecordEnvelope, UnwrapError,
    derive_recipient_mail_epoch_capability_secret_from_root, mail_sealing_epoch_of,
    unseal_mail_record, unseal_mail_record_hybrid,
};

/// Open an already-decoded sealed [`MailRecordEnvelope`] with a
/// `content.read{mail}` derived key — 32 B classical (X25519 secret) or one
/// or more concatenated `32 + MLKEM768_DECAPS_KEY_LEN` hybrid secrets
/// (X25519 secret ∥ ML-KEM-768 decapsulation key, `k × 2432` bytes) —
/// dispatching on the key length. This is the ONE place the length dispatch
/// lives: the FFI grant-key open (`open_mail_record_with_key`) and the epoch
/// trial chain below both call it, and a windowed grant's per-epoch payload
/// ([`derive_recipient_mail_epoch_capability_secret_from_root`]) is exactly
/// this `x25519 ∥ mlkem_dk` layout so one derivation opens either suite.
///
/// A `k ≥ 2` multi-secret payload is a rotation-**boundary** epoch wrap
/// (content-sealing-epochs amendment 2026-07-19): each 2432-byte chunk is one
/// MSEK generation's secret for that epoch, newest generation first; the
/// chunks are trialed in order and the AEAD tag arbitrates — a wrong
/// generation's chunk is one cheap AEAD failure, never a false open. (There
/// is deliberately NO `k × 32` classical form: epoch payloads are always the
/// hybrid shape, and `76 × 32 = 2432` would collide with it.) A key of no
/// valid length is an [`UnwrapError::InvalidFormat`].
pub fn unseal_mail_record_with_derived_key(
    envelope: &MailRecordEnvelope,
    key: &[u8],
) -> Result<Vec<u8>, UnwrapError> {
    const HYBRID_LEN: usize = 32 + MLKEM768_DECAPS_KEY_LEN;
    match key.len() {
        32 => {
            let mut secret = Zeroizing::new([0u8; 32]);
            secret.copy_from_slice(key);
            unseal_mail_record(envelope, &secret)
        }
        len if len != 0 && len % HYBRID_LEN == 0 => {
            let mut last_err = None;
            // The `len % HYBRID_LEN == 0` arm guard means the remainder is always
            // empty, so the typed-array half is the whole key.
            let (chunks, _) = key.as_chunks::<HYBRID_LEN>();
            for chunk in chunks {
                let mut secret = Zeroizing::new([0u8; 32]);
                secret.copy_from_slice(&chunk[..32]);
                let mut dk = Zeroizing::new([0u8; MLKEM768_DECAPS_KEY_LEN]);
                dk.copy_from_slice(&chunk[32..]);
                match unseal_mail_record_hybrid(envelope, &secret, &dk) {
                    Ok(pt) => return Ok(pt),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(last_err.expect("non-empty multiple of the hybrid length has ≥1 chunk"))
        }
        other => Err(UnwrapError::InvalidFormat(format!(
            "content key must be 32 or k×{HYBRID_LEN} bytes (x25519 or concatenated \
             x25519∥mlkem_dk secrets), got {other}"
        ))),
    }
}

/// The content-sealing-epochs § 4 MSEK-holder trial-decrypt chain against an
/// already-decoded inner sealed [`MailRecordEnvelope`].
///
/// Trial order (design § 4), keyed off the record's **seal instant**
/// `record_unix_secs` — its `stored_at`, never a sender-supplied `Date:` header
/// (imported mail diverges by design):
///
///   1. per generation root, the **near pair** `{epoch_of(t), epoch_of(t)-1}`
///      (boundary / clock-skew tolerance);
///   2. the caller's `standing` fallback — standing-key + degraded-schedule
///      content, plus the standing recipient secret;
///   3. per generation root, a bounded **back-scan**
///      `{epoch_of(t)-2 .. epoch_of(t)-MAIL_EPOCH_PUBLISH_HORIZON}` — a record a
///      stale published schedule sealed under an earlier epoch (§ 3 step 2).
///
/// `epoch_roots` are the mail-epoch roots newest-generation-first
/// ([`super::derive_mail_epoch_root`] of the current MSEK, then one per prior
/// generation — § 5 rotation grace, every generation carried), and
/// `retired_at_unix` each prior generation's retirement instant aligned with
/// `epoch_roots[1..]`: steps 1 and 3 walk the roots in
/// [`super::generation_trial_order`] for `record_unix_secs` — the generation
/// current at the seal instant first, then outward (`owner-key-material.md`
/// § Path B-sibling-2 → *Pre-rotation mail at rest*); a `0` instant (unknown,
/// a standing-sealed record) walks them newest first. Empty roots ⇒ only
/// `standing` runs, so the result is byte-identical to a standing-only open
/// (the non-epoch opener path, e.g. a CalDAV/CardDAV session).
///
/// The back-scan is **always** attempted, never gated on "the record postdates
/// the flip" — that is a nest-side constant no opener can observe. The only cost
/// of always trying it is up to `roots × MAIL_EPOCH_PUBLISH_HORIZON` extra AEAD
/// failures on content this holder genuinely cannot open; it is never a
/// false-open (AEAD integrity + the independent per-epoch derivation hold).
/// Every miss folds into `None`, exactly like a standing-only "no matching key".
pub fn open_mail_epoch_chain(
    envelope: &MailRecordEnvelope,
    epoch_roots: &[&[u8; 32]],
    retired_at_unix: &[u64],
    record_unix_secs: u64,
    standing: impl Fn(&MailRecordEnvelope) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let target = mail_sealing_epoch_of(record_unix_secs);
    let roots: Vec<&[u8; 32]> = super::generation_trial_order(
        epoch_roots.len(),
        retired_at_unix,
        (record_unix_secs != 0).then_some(record_unix_secs),
    )
    .into_iter()
    .map(|i| epoch_roots[i])
    .collect();

    // 1. Near pair (target, target-1), per generation root.
    for &root in &roots {
        for e in [Some(target), target.checked_sub(1)].into_iter().flatten() {
            let secret = Zeroizing::new(derive_recipient_mail_epoch_capability_secret_from_root(
                root, e,
            ));
            if let Ok(pt) = unseal_mail_record_with_derived_key(envelope, &secret) {
                return Some(pt);
            }
        }
    }

    // 2. Standing chain (caller-supplied: the FFI leaf keypairs / the client's
    //    MSEK-derived recipient secret).
    if let Some(pt) = standing(envelope) {
        return Some(pt);
    }

    // 3. Bounded back-scan, per generation root.
    for &root in &roots {
        for delta in 2..=MAIL_EPOCH_PUBLISH_HORIZON {
            let Some(e) = target.checked_sub(delta) else {
                break;
            };
            let secret = Zeroizing::new(derive_recipient_mail_epoch_capability_secret_from_root(
                root, e,
            ));
            if let Ok(pt) = unseal_mail_record_with_derived_key(envelope, &secret) {
                return Some(pt);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_blob::{
        MAIL_SEALING_EPOCH_SECS, derive_mail_epoch_root,
        derive_recipient_epoch_hpke_keypair_from_root,
        derive_recipient_epoch_xwing_keypair_from_root, derive_recipient_hpke_keypair,
        seal_to_recipient, seal_to_recipient_xwing,
    };

    /// A unix instant landing squarely inside epoch `e`.
    fn instant_in_epoch(e: u64) -> u64 {
        e * MAIL_SEALING_EPOCH_SECS + 3
    }

    /// Seal `body` to the classical epoch pubkey for `(root, e)`, returning the
    /// decoded inner envelope the chain opens.
    fn sealed_under_epoch(root: &[u8; 32], e: u64, body: &[u8]) -> MailRecordEnvelope {
        let (_sk, pk) = derive_recipient_epoch_hpke_keypair_from_root(root, e);
        seal_to_recipient(body, &pk).expect("seal under epoch key")
    }

    #[test]
    fn opens_a_current_generation_epoch_record_via_the_near_pair() {
        let msek = [0x11; 32];
        let root = derive_mail_epoch_root(&msek);
        let e = 900_123;
        let body = b"From: a@x.test\r\nSubject: epoch body\r\n\r\nhello epoch\r\n";
        let env = sealed_under_epoch(&root, e, body);

        let opened = open_mail_epoch_chain(&env, &[&root], &[], instant_in_epoch(e), |_| None)
            .expect("holder opens the epoch record keyed off its seal instant");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn opens_the_immediately_prior_epoch_via_the_near_pair() {
        // A record sealed under epoch e-1 but classified at e (clock skew /
        // boundary) still opens: the near pair tries {e, e-1}.
        let msek = [0x22; 32];
        let root = derive_mail_epoch_root(&msek);
        let e = 55_555;
        let body = b"prior-epoch near-pair body";
        let env = sealed_under_epoch(&root, e - 1, body);

        let opened = open_mail_epoch_chain(&env, &[&root], &[], instant_in_epoch(e), |_| None)
            .expect("near pair covers epoch e-1");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn opens_a_prior_msek_generation_epoch_record_via_a_grace_root() {
        // MSEK-rotation grace (§ 5): a record sealed under a now-rotated-away
        // generation's epoch key opens because the chain repeats per grace root.
        let current = derive_mail_epoch_root(&[0x33; 32]);
        let grace = derive_mail_epoch_root(&[0x44; 32]); // the prior MSEK's root
        let e = 700_001;
        let body = b"sealed under a rotated-away MSEK's epoch key";
        let env = sealed_under_epoch(&grace, e, body);

        // Current root alone cannot open it.
        assert!(
            open_mail_epoch_chain(&env, &[&current], &[], instant_in_epoch(e), |_| None).is_none(),
            "current-generation root must not open a prior generation's epoch record"
        );
        // Adding the grace root does.
        let opened =
            open_mail_epoch_chain(&env, &[&current, &grace], &[], instant_in_epoch(e), |_| {
                None
            })
            .expect("grace root opens the rotated-away epoch record");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn opens_a_stale_schedule_record_via_the_bounded_back_scan() {
        // A record sealed several epochs before its classification instant
        // (stale published schedule, § 3 step 2) opens via the back-scan.
        let root = derive_mail_epoch_root(&[0x55; 32]);
        let e = 40_000;
        let sealed_at = e - 5; // 5 epochs back — past the near pair, inside the horizon
        let body = b"stale-schedule back-scan body";
        let env = sealed_under_epoch(&root, sealed_at, body);

        let opened = open_mail_epoch_chain(&env, &[&root], &[], instant_in_epoch(e), |_| None)
            .expect("back-scan reaches a stale-schedule epoch within the horizon");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn a_record_older_than_the_horizon_stays_dark() {
        // Beyond MAIL_EPOCH_PUBLISH_HORIZON the back-scan does not reach it →
        // None (fail-closed, never a false-open).
        let root = derive_mail_epoch_root(&[0x66; 32]);
        let e = 40_000;
        let sealed_at = e - (MAIL_EPOCH_PUBLISH_HORIZON + 3);
        let env = sealed_under_epoch(&root, sealed_at, b"too old");

        assert!(
            open_mail_epoch_chain(&env, &[&root], &[], instant_in_epoch(e), |_| None).is_none(),
            "a record older than the horizon must not open (fail-closed)"
        );
    }

    #[test]
    fn opens_a_hybrid_xwing_epoch_record() {
        // The single derived payload (x25519 ∥ mlkem_dk) opens an X-Wing record
        // via the length dispatch — no separate classical/hybrid derivation.
        let root = derive_mail_epoch_root(&[0x77; 32]);
        let e = 123_456;
        let kp = derive_recipient_epoch_xwing_keypair_from_root(&root, e);
        let body = b"post-quantum epoch body";
        let env = seal_to_recipient_xwing(body, &kp.public).expect("seal xwing epoch");

        let opened = open_mail_epoch_chain(&env, &[&root], &[], instant_in_epoch(e), |_| None)
            .expect("hybrid epoch record opens from one derived payload");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn standing_only_open_fails_on_an_epoch_record() {
        // The whole point of the flip gate: a pre-epoch (standing-only) open —
        // no epoch roots, a standing closure that only knows the recipient
        // secret — cannot read epoch-sealed mail.
        let msek = [0x88; 32];
        let root = derive_mail_epoch_root(&msek);
        let (standing_secret, _pk) = derive_recipient_hpke_keypair(&msek);
        let e = 88_888;
        let env = sealed_under_epoch(&root, e, b"epoch-only body");

        let standing = |env: &MailRecordEnvelope| unseal_mail_record(env, &standing_secret).ok();
        assert!(
            open_mail_epoch_chain(&env, &[], &[], instant_in_epoch(e), standing).is_none(),
            "standing-only holder (no epoch roots) must AEAD-fail on epoch mail"
        );
    }

    #[test]
    fn empty_roots_falls_through_to_the_standing_chain() {
        // With no epoch roots, a standing-sealed record still opens via the
        // caller's standing closure — byte-identical to a standing-only open.
        let msek = [0x99; 32];
        let (secret, pk) = derive_recipient_hpke_keypair(&msek);
        let body = b"standing-sealed record";
        let env = seal_to_recipient(body, &pk).expect("standing seal");

        let standing = |env: &MailRecordEnvelope| unseal_mail_record(env, &secret).ok();
        let opened = open_mail_epoch_chain(&env, &[], &[], 1_700_000_000, standing)
            .expect("standing chain opens a standing-sealed record");
        assert_eq!(opened.as_slice(), body);
    }

    #[test]
    fn unseal_with_derived_key_rejects_a_wrong_length_key() {
        let root = derive_mail_epoch_root(&[0xAB; 32]);
        let env = sealed_under_epoch(&root, 1, b"x");
        let err = unseal_mail_record_with_derived_key(&env, &[0u8; 40])
            .expect_err("40-byte key is neither classical nor hybrid");
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn a_boundary_epoch_multi_secret_payload_opens_with_either_chunk() {
        // The 2026-07-19 amendment: a rotation-boundary epoch's wrap payload
        // concatenates two generations' secrets (newest first); the dispatch
        // chunk-trials them, so content sealed under EITHER generation's
        // epoch key opens from the one payload.
        use crate::wrapped_blob::derive_recipient_mail_epoch_capability_secret_from_root;
        let e = 4_242;
        let new_root = derive_mail_epoch_root(&[0xC1; 32]);
        let old_root = derive_mail_epoch_root(&[0xC2; 32]);
        let mut payload = derive_recipient_mail_epoch_capability_secret_from_root(&new_root, e);
        payload.extend_from_slice(&derive_recipient_mail_epoch_capability_secret_from_root(
            &old_root, e,
        ));

        let body_old = b"pre-rotation slice of the boundary epoch";
        let kp_old = derive_recipient_epoch_xwing_keypair_from_root(&old_root, e);
        let env_old = seal_to_recipient_xwing(body_old, &kp_old.public).expect("seal old");
        let opened_old = unseal_mail_record_with_derived_key(&env_old, &payload)
            .expect("old-generation chunk opens pre-rotation content");
        assert_eq!(opened_old.as_slice(), body_old);

        let body_new = b"post-rotation slice of the boundary epoch";
        let kp_new = derive_recipient_epoch_xwing_keypair_from_root(&new_root, e);
        let env_new = seal_to_recipient_xwing(body_new, &kp_new.public).expect("seal new");
        let opened_new = unseal_mail_record_with_derived_key(&env_new, &payload)
            .expect("new-generation chunk opens post-rotation content");
        assert_eq!(opened_new.as_slice(), body_new);
    }

    #[test]
    fn a_multi_secret_payload_with_no_matching_generation_stays_dark() {
        use crate::wrapped_blob::derive_recipient_mail_epoch_capability_secret_from_root;
        let e = 4_243;
        let root_a = derive_mail_epoch_root(&[0xC3; 32]);
        let root_b = derive_mail_epoch_root(&[0xC4; 32]);
        let unrelated = derive_mail_epoch_root(&[0xC5; 32]);
        let mut payload = derive_recipient_mail_epoch_capability_secret_from_root(&root_a, e);
        payload.extend_from_slice(&derive_recipient_mail_epoch_capability_secret_from_root(
            &root_b, e,
        ));
        let env = sealed_under_epoch(&unrelated, e, b"sealed under a third generation");
        assert!(
            unseal_mail_record_with_derived_key(&env, &payload).is_err(),
            "no chunk matches → AEAD-fail, never a false open"
        );
    }

    /// **Every generation's root is walked, the one current at the seal
    /// instant first** (`owner-key-material.md` § Path B-sibling-2 →
    /// *Pre-rotation mail at rest*): a record epoch-sealed under the OLDEST of
    /// five generations, four rotations ago, opens through the chain given
    /// the retirement instants — and with none too (the whole ring, newest
    /// first).
    #[test]
    fn opens_a_record_sealed_four_rotations_ago_selecting_by_seal_time() {
        let roots: Vec<Zeroizing<[u8; 32]>> = (1u8..=5)
            .map(|g| derive_mail_epoch_root(&[g; 32]))
            .collect();
        let refs: Vec<&[u8; 32]> = roots.iter().map(|z| &**z).collect();
        let e = 4_000;
        let sealed_at = instant_in_epoch(e);
        // Priors retired after the record was sealed, one epoch apart.
        let retired: Vec<u64> = (1..=4u64)
            .rev()
            .map(|k| sealed_at + k * MAIL_SEALING_EPOCH_SECS)
            .collect();
        let body = b"sealed under the oldest generation";
        let env = sealed_under_epoch(&roots[4], e, body);
        for instants in [&retired[..], &[]] {
            let opened = open_mail_epoch_chain(&env, &refs, instants, sealed_at, |_| None)
                .expect("the oldest generation's root opens it");
            assert_eq!(opened.as_slice(), body);
        }
    }
}
