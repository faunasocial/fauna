//! HPKE base-mode wrap/unwrap for service-user-keyed blobs (TLS certs, ATProto keys).
//!
//! Cipher suite per spec § Primitives:
//!   KEM  = DHKEM(X25519, HKDF-SHA-256) (0x0020)
//!   KDF  = HKDF-SHA-256                (0x0001)
//!   AEAD = ChaCha20-Poly1305           (0x0003)

use crate::wrapped_blob::format::{AadBinding, UnwrapError, WrapError};
use hpke::{
    Deserializable, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305, kdf::HkdfSha256,
    kem::X25519HkdfSha256,
};
use rand_core_09::{OsRng, TryRngCore};

/// Cipher suite IDs (RFC 9180 § 7).
pub const KEM_X25519_HKDF_SHA256: u16 = 0x0020;
pub const KDF_HKDF_SHA256: u16 = 0x0001;
pub const AEAD_CHACHA20_POLY1305: u16 = 0x0003;

/// Length of the HPKE encapsulated key (X25519 ephemeral pubkey).
pub const HPKE_ENC_LEN: usize = 32;

type Aead = ChaCha20Poly1305;
type Kdf = HkdfSha256;
type Kem = X25519HkdfSha256;

/// HPKE-Seal `plaintext` to `recipient_pubkey` with `info` binding.
///
/// `aad_binding` provides the AAD for the inner AEAD operation; we
/// already bind the same context via HPKE info, but the AAD layer is
/// kept for defense-in-depth and to mirror the symmetric path.
///
/// Returns `(enc, ciphertext)` where `enc` is the 32-byte
/// X25519-ephemeral-pubkey-encoded shared secret.
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` if the recipient pubkey decode
/// fails or the HPKE single-shot seal fails.
pub fn hpke_seal(
    recipient_pubkey: &[u8; 32],
    info_binding: &AadBinding,
    aad_binding: &AadBinding,
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), WrapError> {
    let pk_recip = <<Kem as hpke::Kem>::PublicKey as Deserializable>::from_bytes(recipient_pubkey)
        .map_err(|e| WrapError::HpkeFailed(format!("decode recipient pubkey: {e}")))?;

    let info = info_binding.canonical_bytes();
    let aad = aad_binding.canonical_bytes();

    // hpke 0.13 wants `RngCore + CryptoRng` (rand_core 0.9). `OsRng`
    // in rand_core 0.9 only implements the fallible `TryRngCore`/
    // `TryCryptoRng` traits; `unwrap_err()` produces an infallible
    // wrapper that panics on the (extremely unlikely) OS-side error.
    let mut csprng = OsRng.unwrap_err();
    let (encapped_key, ciphertext) = hpke::single_shot_seal::<Aead, Kdf, Kem, _>(
        &OpModeS::Base,
        &pk_recip,
        &info,
        plaintext,
        &aad,
        &mut csprng,
    )
    .map_err(|e| WrapError::HpkeFailed(format!("single_shot_seal: {e}")))?;

    Ok((encapped_key.to_bytes().as_slice().to_vec(), ciphertext))
}

/// HPKE-Open with `recipient_secret`. Returns plaintext on success.
///
/// # Errors
///
/// Returns `UnwrapError::InvalidFormat` if `enc` is not 32 bytes.
/// Returns `UnwrapError::HpkeFailed` for any HPKE-side failure
/// (wrong recipient secret, tampered ciphertext, info mismatch).
pub fn hpke_open(
    recipient_secret: &[u8; 32],
    info_binding: &AadBinding,
    aad_binding: &AadBinding,
    enc: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, UnwrapError> {
    if enc.len() != HPKE_ENC_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "hpke enc must be {HPKE_ENC_LEN} bytes"
        )));
    }
    let sk_recip = <<Kem as hpke::Kem>::PrivateKey as Deserializable>::from_bytes(recipient_secret)
        .map_err(|_| UnwrapError::HpkeFailed)?;
    let encapped = <<Kem as hpke::Kem>::EncappedKey as Deserializable>::from_bytes(enc)
        .map_err(|_| UnwrapError::HpkeFailed)?;

    let info = info_binding.canonical_bytes();
    let aad = aad_binding.canonical_bytes();

    hpke::single_shot_open::<Aead, Kdf, Kem>(
        &OpModeR::Base,
        &sk_recip,
        &encapped,
        &info,
        ciphertext,
        &aad,
    )
    .map_err(|_| UnwrapError::HpkeFailed)
}

/// Generate a fresh X25519 keypair for service-user enrollment.
pub fn generate_x25519_keypair() -> ([u8; 32], [u8; 32]) {
    let mut csprng = OsRng.unwrap_err();
    let (sk, pk) = <Kem as hpke::Kem>::gen_keypair(&mut csprng);
    keypair_bytes(sk, pk)
}

/// Deterministically derive an X25519 HPKE keypair from input key
/// material via RFC 9180 § 7.1.3 `DeriveKeyPair` — the same KEM
/// (`X25519HkdfSha256`) used by [`hpke_seal`] / [`hpke_open`], so the
/// result is guaranteed usable as a `seal_to_recipient` recipient key.
/// Deterministic in `ikm`: identical `ikm` always yields the identical
/// keypair (the fleet-consistency property the recipient-mail keypair
/// relies on; see `docs/goal/architecture/key-material-hierarchy.md`
/// § Path B-sibling-2). Returns `(secret, public)`.
pub fn derive_x25519_keypair_from_ikm(ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
    let (sk, pk) = <Kem as hpke::Kem>::derive_keypair(ikm);
    keypair_bytes(sk, pk)
}

fn keypair_bytes(
    sk: <Kem as hpke::Kem>::PrivateKey,
    pk: <Kem as hpke::Kem>::PublicKey,
) -> ([u8; 32], [u8; 32]) {
    let sk_bytes: [u8; 32] = sk
        .to_bytes()
        .as_slice()
        .try_into()
        .expect("X25519 secret is 32 bytes");
    let pk_bytes: [u8; 32] = pk
        .to_bytes()
        .as_slice()
        .try_into()
        .expect("X25519 public is 32 bytes");
    (sk_bytes, pk_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls_info_aad() -> (AadBinding, AadBinding) {
        let info = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let aad = AadBinding::for_tls_cert("mta", "bridge-1", "example.com"); // same binding
        (info, aad)
    }

    #[test]
    fn hpke_round_trip_succeeds() {
        let (sk, pk) = generate_x25519_keypair();
        let (info, aad) = tls_info_aad();
        let (enc, ct) = hpke_seal(&pk, &info, &aad, b"tls cert bundle bytes").unwrap();
        assert_eq!(enc.len(), HPKE_ENC_LEN);
        let opened = hpke_open(&sk, &info, &aad, &enc, &ct).unwrap();
        assert_eq!(opened, b"tls cert bundle bytes");
    }

    #[test]
    fn wrong_recipient_secret_fails_open() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let (info, aad) = tls_info_aad();
        let (enc, ct) = hpke_seal(&pk, &info, &aad, b"x").unwrap();
        let err = hpke_open(&other_sk, &info, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn cross_info_binding_fails_open() {
        let (sk, pk) = generate_x25519_keypair();
        let info_a = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let info_b = AadBinding::for_tls_cert("mta", "bridge-2", "example.com"); // different bridge
        let aad = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let (enc, ct) = hpke_seal(&pk, &info_a, &aad, b"x").unwrap();
        let err = hpke_open(&sk, &info_b, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn cross_kind_info_binding_fails_open() {
        // tls-cert vs atproto-session-secret under same key -- both should
        // be impossible to mix because info differs.
        let (sk, pk) = generate_x25519_keypair();
        let tls_info = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let session_info = AadBinding::for_atproto_session_secret("atproto.pds", "bridge-1");
        let aad = AadBinding::for_tls_cert("mta", "bridge-1", "example.com");
        let (enc, ct) = hpke_seal(&pk, &tls_info, &aad, b"x").unwrap();
        let err = hpke_open(&sk, &session_info, &aad, &enc, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn enc_length_is_32() {
        let (_, pk) = generate_x25519_keypair();
        let (info, aad) = tls_info_aad();
        let (enc, _) = hpke_seal(&pk, &info, &aad, b"x").unwrap();
        assert_eq!(enc.len(), HPKE_ENC_LEN);
    }
}
