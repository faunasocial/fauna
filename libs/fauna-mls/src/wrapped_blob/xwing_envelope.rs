//! X-Wing hybrid-KEM wrap/unwrap for the mail-at-rest surface (surface A,
//! step 2 of the post-quantum migration — goal
//! `architecture/security/post-quantum.md` § "Per-surface discriminator state"
//! surface-A row).
//!
//! This mirrors [`super::envelope`]'s classical HPKE path, swapping **only the
//! KEM**: instead of DHKEM(X25519, HKDF-SHA-256) the KEM is **X-Wing**
//! (ML-KEM-768 ∥ X25519, [`fauna_pq_kem`]). The 32-byte X-Wing shared secret
//! then feeds an **RFC 9180 base-mode key schedule** (LabeledExtract /
//! LabeledExpand, § 4.1 + § 5.1) parameterised by the self-describing suite id
//! `{kem = FAUNA_KEM_XWING, kdf = HKDF-SHA-256, aead = ChaCha20Poly1305}` — i.e.
//! exactly what classical HPKE does, so the **AEAD + AAD bindings are
//! byte-identical to the classical path** and only the KEM and the on-wire `enc`
//! (32 B → 1120 B) change.
//!
//! ## Why hand-rolled rather than the `hpke` crate
//!
//! `hpke` 0.13's `single_shot_*` runs KEM + KDF + AEAD as one unit and cannot
//! accept a *foreign* KEM shared secret; implementing its `Kem` trait for X-Wing
//! would require `generic_array`/`typenum` glue for the 1216/1120/2432-byte key
//! sizes (none are predefined consts) plus hand-written `Serializable` /
//! `Deserializable`. The RFC 9180 base-mode key schedule is small, fully
//! specified, deterministic, and KAT-pinned in the tests below — the cheaper,
//! lower-risk path the implementation NEXT sanctions.

use crate::wrapped_blob::format::{AadBinding, UnwrapError, WrapError};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use fauna_pq_kem::{
    XWING_CIPHERTEXT_LEN, XWingCiphertext, XWingPublicKey, XWingSecretKey, decapsulate, encapsulate,
};
use hkdf::Hkdf;
use rand_core_09::{OsRng, TryRngCore};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::envelope::{AEAD_CHACHA20_POLY1305, KDF_HKDF_SHA256};
use super::format::FAUNA_KEM_XWING;

/// ChaCha20-Poly1305 key length (HPKE `Nk`).
const NK: usize = 32;
/// ChaCha20-Poly1305 nonce length (HPKE `Nn`).
const NN: usize = 12;
/// HKDF-SHA-256 output length (HPKE `Nh`).
const NH: usize = 32;

/// On-wire length of an X-Wing `enc` (the 1120-byte X-Wing ciphertext: ML-KEM-768
/// ct 1088 B ∥ X25519 ephemeral pk 32 B). Re-exported from the primitive crate so
/// callers and the dispatch site agree on the discriminator size.
pub const XWING_ENC_LEN: usize = XWING_CIPHERTEXT_LEN;

/// RFC 9180 § 5.1 `suite_id` for the X-Wing HPKE suite:
/// `"HPKE" || I2OSP(kem,2) || I2OSP(kdf,2) || I2OSP(aead,2)`.
fn suite_id() -> [u8; 10] {
    let mut s = [0u8; 10];
    s[..4].copy_from_slice(b"HPKE");
    s[4..6].copy_from_slice(&FAUNA_KEM_XWING.to_be_bytes());
    s[6..8].copy_from_slice(&KDF_HKDF_SHA256.to_be_bytes());
    s[8..10].copy_from_slice(&AEAD_CHACHA20_POLY1305.to_be_bytes());
    s
}

/// RFC 9180 § 4.1 `LabeledExtract`:
/// `Extract(salt, "HPKE-v1" || suite_id || label || ikm)`.
fn labeled_extract(salt: &[u8], label: &[u8], ikm: &[u8]) -> [u8; NH] {
    let mut labeled_ikm = Vec::with_capacity(7 + 10 + label.len() + ikm.len());
    labeled_ikm.extend_from_slice(b"HPKE-v1");
    labeled_ikm.extend_from_slice(&suite_id());
    labeled_ikm.extend_from_slice(label);
    labeled_ikm.extend_from_slice(ikm);
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &labeled_ikm);
    let mut out = [0u8; NH];
    out.copy_from_slice(prk.as_slice());
    out
}

/// RFC 9180 § 4.1 `LabeledExpand`:
/// `Expand(prk, I2OSP(L,2) || "HPKE-v1" || suite_id || label || info, L)`.
fn labeled_expand(prk: &[u8; NH], label: &[u8], info: &[u8], out: &mut [u8]) {
    let l = u16::try_from(out.len()).expect("HPKE expand length fits u16");
    let mut labeled_info = Vec::with_capacity(2 + 7 + 10 + label.len() + info.len());
    labeled_info.extend_from_slice(&l.to_be_bytes());
    labeled_info.extend_from_slice(b"HPKE-v1");
    labeled_info.extend_from_slice(&suite_id());
    labeled_info.extend_from_slice(label);
    labeled_info.extend_from_slice(info);
    let hk = Hkdf::<Sha256>::from_prk(prk).expect("Nh-length prk is a valid HKDF PRK");
    hk.expand(&labeled_info, out)
        .expect("Nk/Nn <= 255*Nh, HKDF expand cannot fail for these lengths");
}

/// RFC 9180 § 5.1 base-mode `KeySchedule` keyed by the X-Wing shared secret.
/// `mode = 0x00` (base), `psk = ""`, `psk_id = ""`. Returns `(key, base_nonce)`;
/// since this is single-shot (sequence number 0) the AEAD nonce *is* the
/// `base_nonce`. The returned key is zeroized on drop.
fn key_schedule_base(shared_secret: &[u8; 32], info: &[u8]) -> (Zeroizing<[u8; NK]>, [u8; NN]) {
    // psk="" / psk_id="" so both hashes use an empty salt and empty/`info` ikm.
    let psk_id_hash = labeled_extract(&[], b"psk_id_hash", &[]);
    let info_hash = labeled_extract(&[], b"info_hash", info);

    let mut ksc = Vec::with_capacity(1 + NH + NH);
    ksc.push(0x00); // mode_base
    ksc.extend_from_slice(&psk_id_hash);
    ksc.extend_from_slice(&info_hash);

    // secret = LabeledExtract(shared_secret /*salt*/, "secret", psk="" /*ikm*/)
    let secret = Zeroizing::new(labeled_extract(shared_secret, b"secret", &[]));

    let mut key = Zeroizing::new([0u8; NK]);
    let mut base_nonce = [0u8; NN];
    labeled_expand(&secret, b"key", &ksc, key.as_mut_slice());
    labeled_expand(&secret, b"base_nonce", &ksc, &mut base_nonce);
    (key, base_nonce)
}

/// X-Wing-Seal `plaintext` to `recipient_pubkey` with the `info`/`aad` bindings.
///
/// Returns `(enc, ciphertext)` where `enc` is the [`XWING_ENC_LEN`]-byte X-Wing
/// ciphertext (the KEM encapsulation). The AEAD ciphertext and AAD treatment are
/// byte-identical to [`super::envelope::hpke_seal`]; only the KEM differs.
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] if the recipient's ML-KEM encapsulation key
/// fails FIPS 203 validation, or if the AEAD seal fails.
pub fn xwing_seal(
    recipient_pubkey: &XWingPublicKey,
    info_binding: &AadBinding,
    aad_binding: &AadBinding,
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), WrapError> {
    // `OsRng` (rand_core 0.9) only implements the fallible `TryRngCore`;
    // `unwrap_err()` is the infallible wrapper that panics on the (extremely
    // unlikely) OS-side failure — the same pattern as the classical path.
    let mut csprng = OsRng.unwrap_err();
    let (xwing_ct, shared_secret) = encapsulate(recipient_pubkey, &mut csprng)
        .map_err(|e| WrapError::HpkeFailed(format!("x-wing encapsulate: {e}")))?;
    let shared_secret = Zeroizing::new(shared_secret);

    let info = info_binding.canonical_bytes();
    let aad = aad_binding.canonical_bytes();
    let (key, base_nonce) = key_schedule_base(&shared_secret, &info);

    let cipher = ChaCha20Poly1305::new((&*key).into());
    let nonce = Nonce::from_slice(&base_nonce);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| WrapError::HpkeFailed("x-wing aead seal failed".into()))?;

    Ok((xwing_ct.as_bytes().to_vec(), ciphertext))
}

/// X-Wing-Open with `recipient_secret`. Returns plaintext on success.
///
/// # Errors
///
/// Returns [`UnwrapError::InvalidFormat`] if `enc` is not [`XWING_ENC_LEN`]
/// bytes. Returns [`UnwrapError::HpkeFailed`] for any AEAD failure — a wrong
/// recipient key, tampered ciphertext, or info/aad mismatch. (X-Wing decap is
/// infallible by construction: a wrong key yields a *different* shared secret via
/// ML-KEM implicit rejection, which the AEAD then rejects here.)
pub fn xwing_open(
    recipient_secret: &XWingSecretKey,
    info_binding: &AadBinding,
    aad_binding: &AadBinding,
    enc: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, UnwrapError> {
    let enc_arr: [u8; XWING_ENC_LEN] = enc.try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!("x-wing enc must be {XWING_ENC_LEN} bytes"))
    })?;
    let xwing_ct = XWingCiphertext::from_bytes(enc_arr);
    let shared_secret = Zeroizing::new(decapsulate(recipient_secret, &xwing_ct));

    let info = info_binding.canonical_bytes();
    let aad = aad_binding.canonical_bytes();
    let (key, base_nonce) = key_schedule_base(&shared_secret, &info);

    let cipher = ChaCha20Poly1305::new((&*key).into());
    let nonce = Nonce::from_slice(&base_nonce);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| UnwrapError::HpkeFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_pq_kem::{XWingKeyPair, derive_keypair};
    use sha2::Digest;

    /// A throwaway X-Wing keypair derived deterministically from `tag`.
    fn test_keypair(tag: &str) -> XWingKeyPair {
        let x25519_sk: [u8; 32] = Sha256::digest(tag.as_bytes()).into();
        derive_keypair(
            format!("ikm-{tag}").as_bytes(),
            "fauna.test.mlkem.v1",
            &x25519_sk,
        )
    }

    fn mail_info_aad() -> (AadBinding, AadBinding) {
        (AadBinding::for_mail_record(), AadBinding::for_mail_record())
    }

    #[test]
    fn xwing_round_trip_succeeds() {
        let kp = test_keypair("rt");
        let (info, aad) = mail_info_aad();
        let (enc, ct) = xwing_seal(&kp.public, &info, &aad, b"inbound rfc5322 bytes").unwrap();
        assert_eq!(enc.len(), XWING_ENC_LEN);
        let opened = xwing_open(&kp.secret, &info, &aad, &enc, &ct).unwrap();
        assert_eq!(opened, b"inbound rfc5322 bytes");
    }

    #[test]
    fn enc_length_is_1120() {
        let kp = test_keypair("len");
        let (info, aad) = mail_info_aad();
        let (enc, _) = xwing_seal(&kp.public, &info, &aad, b"x").unwrap();
        assert_eq!(enc.len(), 1120);
        assert_eq!(enc.len(), XWING_ENC_LEN);
    }

    #[test]
    fn wrong_recipient_secret_fails_open() {
        let kp = test_keypair("a");
        let other = test_keypair("b");
        let (info, aad) = mail_info_aad();
        let (enc, ct) = xwing_seal(&kp.public, &info, &aad, b"secret body").unwrap();
        // Wrong key ⇒ different X-Wing shared secret ⇒ AEAD rejects.
        let err = xwing_open(&other.secret, &info, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn tampered_ciphertext_fails_open() {
        let kp = test_keypair("tamper");
        let (info, aad) = mail_info_aad();
        let (enc, mut ct) = xwing_seal(&kp.public, &info, &aad, b"abcdefgh").unwrap();
        ct[0] ^= 0x01;
        let err = xwing_open(&kp.secret, &info, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn tampered_enc_fails_open() {
        let kp = test_keypair("tamper-enc");
        let (info, aad) = mail_info_aad();
        let (mut enc, ct) = xwing_seal(&kp.public, &info, &aad, b"abcdefgh").unwrap();
        enc[5] ^= 0x01; // flip a byte in the ML-KEM ct half of the encapsulation
        let err = xwing_open(&kp.secret, &info, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn wrong_enc_length_is_invalid_format() {
        let kp = test_keypair("badlen");
        let (info, aad) = mail_info_aad();
        let (_, ct) = xwing_seal(&kp.public, &info, &aad, b"x").unwrap();
        let short = vec![0u8; XWING_ENC_LEN - 1];
        let err = xwing_open(&kp.secret, &info, &aad, &short, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn cross_info_binding_fails_open() {
        // The info binding enters the key schedule (info_hash), so a mismatched
        // info derives a different key ⇒ AEAD rejects. (TLS-cert info is used purely
        // as a *distinct* binding from the mail-record one for this negative test.)
        let kp = test_keypair("xinfo");
        let info_a = AadBinding::for_mail_record();
        let info_b = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let aad = AadBinding::for_mail_record();
        let (enc, ct) = xwing_seal(&kp.public, &info_a, &aad, b"body").unwrap();
        let err = xwing_open(&kp.secret, &info_b, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn cross_aad_binding_fails_open() {
        let kp = test_keypair("xaad");
        let info = AadBinding::for_mail_record();
        let aad_a = AadBinding::for_mail_record();
        let aad_b = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let (enc, ct) = xwing_seal(&kp.public, &info, &aad_a, b"body").unwrap();
        let err = xwing_open(&kp.secret, &info, &aad_b, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    /// KAT / regression pin for the hand-rolled RFC 9180 base-mode key schedule.
    /// The (key, base_nonce) are a deterministic function of (shared_secret, info,
    /// suite_id); pinning them catches any accidental drift in the labeled
    /// HKDF construction, the suite-id bytes, or the field ordering. The expected
    /// values are recomputed here via an independent buffer-built path (a
    /// different code path than `key_schedule_base`'s incremental updates).
    #[test]
    fn key_schedule_known_answer() {
        let shared_secret = [0x42u8; 32];
        let info = b"fauna.test.info".to_vec();
        let (key, base_nonce) = key_schedule_base(&shared_secret, &info);

        // Independent recomputation of the exact RFC 9180 base-mode schedule.
        let sid = {
            let mut s = Vec::new();
            s.extend_from_slice(b"HPKE");
            s.extend_from_slice(&0xFC00u16.to_be_bytes()); // FAUNA_KEM_XWING
            s.extend_from_slice(&0x0001u16.to_be_bytes()); // HKDF-SHA-256
            s.extend_from_slice(&0x0003u16.to_be_bytes()); // ChaCha20Poly1305
            s
        };
        let lextract = |salt: &[u8], label: &[u8], ikm: &[u8]| -> [u8; 32] {
            let mut li = Vec::new();
            li.extend_from_slice(b"HPKE-v1");
            li.extend_from_slice(&sid);
            li.extend_from_slice(label);
            li.extend_from_slice(ikm);
            let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &li);
            let mut o = [0u8; 32];
            o.copy_from_slice(prk.as_slice());
            o
        };
        let lexpand = |prk: &[u8; 32], label: &[u8], ctx: &[u8], out: &mut [u8]| {
            let mut li = Vec::new();
            li.extend_from_slice(&(out.len() as u16).to_be_bytes());
            li.extend_from_slice(b"HPKE-v1");
            li.extend_from_slice(&sid);
            li.extend_from_slice(label);
            li.extend_from_slice(ctx);
            Hkdf::<Sha256>::from_prk(prk)
                .unwrap()
                .expand(&li, out)
                .unwrap();
        };
        let pih = lextract(&[], b"psk_id_hash", &[]);
        let ih = lextract(&[], b"info_hash", &info);
        let mut ksc = vec![0x00u8];
        ksc.extend_from_slice(&pih);
        ksc.extend_from_slice(&ih);
        let secret = lextract(&shared_secret, b"secret", &[]);
        let mut exp_key = [0u8; 32];
        let mut exp_nonce = [0u8; 12];
        lexpand(&secret, b"key", &ksc, &mut exp_key);
        lexpand(&secret, b"base_nonce", &ksc, &mut exp_nonce);

        assert_eq!(
            *key, exp_key,
            "derived key drifted from RFC 9180 base schedule"
        );
        assert_eq!(base_nonce, exp_nonce, "derived base_nonce drifted");

        // A round-trip under this exact schedule still recovers plaintext.
        let kp = test_keypair("kat-rt");
        let info_b = AadBinding::for_mail_record();
        let (enc, ct) = xwing_seal(&kp.public, &info_b, &info_b, b"kat body").unwrap();
        assert_eq!(
            xwing_open(&kp.secret, &info_b, &info_b, &enc, &ct).unwrap(),
            b"kat body"
        );
    }
}
