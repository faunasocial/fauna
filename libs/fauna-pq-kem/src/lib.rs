//! Fauna's post-quantum **hybrid KEM** — **X-Wing** (ML-KEM-768 ∥ X25519).
//!
//! This is the shared `libs/` primitive behind step 2 of the post-quantum
//! migration (goal doc
//! [`architecture/security/post-quantum.md`](../../../docs/goal/architecture/security/post-quantum.md)
//! § "The hybrid KEM — X-Wing"; design tracked internally). It produces a
//! 32-byte shared secret whose confidentiality survives
//! the failure of *either* component (X25519 broken classically, or ML-KEM
//! broken by a quantum computer), and it is sized to drop into the existing HPKE
//! `enc`/`ct` envelope shape (surface A) and the subscription KeyBlob (surface
//! B). **S2 builds only the primitive — nothing seals to it yet** (S3/S4 wire it
//! into the mail and subscription surfaces).
//!
//! # Construction (X-Wing, `draft-connolly-cfrg-xwing-kem`, combiner stable
//! since -09)
//!
//! - **Encaps** mixes a fresh ML-KEM-768 encapsulation with a fresh X25519 DH
//!   and binds them with the X-Wing combiner:
//!   `ss = SHA3-256(ss_M ∥ ss_X ∥ ct_X ∥ pk_X ∥ label)`, where `ss_M` is the
//!   ML-KEM shared secret, `ss_X` the raw X25519 DH, `ct_X` the X25519 ephemeral
//!   public key, `pk_X` the recipient's static X25519 public key, and `label`
//!   the 6 bytes `5c 2e 2f 2f 5e 5c` (`\.//^\`). The combiner deliberately does
//!   **not** hash the ML-KEM ciphertext or public key (ML-KEM ciphertext-
//!   collision-freedom makes that redundant — IACR 2024/039).
//! - **Decaps** recomputes `ss_M`/`ss_X`/`pk_X` from the decapsulation key and
//!   runs the identical combiner. ML-KEM's implicit rejection means a wrong key
//!   or tampered ciphertext does **not** error — it yields a *different*
//!   (pseudo-random) shared secret, which the outer AEAD then rejects.
//!
//! # Why a hand-rolled combiner over the `x-wing` crate
//!
//! Fauna does **not** use X-Wing's monolithic 32-byte seed. Each surface derives
//! the ML-KEM half from its own seed (`expand(MSEK/identity-seed, context, 64) →
//! ML-KEM-768.KeyGen`) and **reuses the surface's existing MSEK/identity-derived
//! X25519 key** (goal doc § Post-quantum key publication and derivation). The
//! X-Wing combiner/encaps/decaps operate purely on the four component keys and
//! are agnostic to how they were produced, so composing them here is sound — but
//! the `x-wing` crate exposes only its monolithic seed KeyGen, which cannot
//! accept independently-derived halves. The ML-KEM-768 implementation is
//! Cryspen's formally-verified [`libcrux_ml_kem`]; the X25519 half is the bare
//! RFC 7748 `x25519()` primitive.
//!
//! # Randomness
//!
//! [`encapsulate`] takes the caller's CSPRNG (`OsRng` on native, the configured
//! browser RNG on wasm) rather than reaching for `getrandom` itself — this keeps
//! the crate `getrandom`-free and trivially buildable for
//! `wasm32-unknown-unknown` (web seals subscription KeyBlobs client-side).

#![forbid(unsafe_code)]

use hkdf::Hkdf;
use libcrux_ml_kem::mlkem768::{self, MlKem768Ciphertext, MlKem768PrivateKey, MlKem768PublicKey};
use rand_core::{CryptoRng, RngCore};
use sha2::Sha256;
use sha3::{Digest, Sha3_256};
use x25519_dalek::{X25519_BASEPOINT_BYTES, x25519};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

// ---- sizes (all derived from the underlying primitives) ----

/// ML-KEM-768 encapsulation (public) key length — FIPS 203.
pub const MLKEM768_ENCAPS_KEY_LEN: usize = 1184;
/// ML-KEM-768 decapsulation (secret) key length — FIPS 203 (expanded form).
pub const MLKEM768_DECAPS_KEY_LEN: usize = 2400;
/// ML-KEM-768 ciphertext length — FIPS 203.
pub const MLKEM768_CIPHERTEXT_LEN: usize = 1088;
/// X25519 public key / ciphertext / scalar length — RFC 7748.
pub const X25519_LEN: usize = 32;
/// X-Wing shared-secret length (SHA3-256 output).
pub const SHARED_SECRET_LEN: usize = 32;
/// FIPS 203 `KeyGen_internal` seed length (`d ∥ z`); the 64-byte input to
/// [`mlkem768::generate_key_pair`].
pub const MLKEM768_SEED_LEN: usize = 64;

/// X-Wing encapsulation (public) key length: ML-KEM-768 ek ∥ X25519 pk.
pub const XWING_ENCAPS_KEY_LEN: usize = MLKEM768_ENCAPS_KEY_LEN + X25519_LEN; // 1216
/// X-Wing ciphertext length: ML-KEM-768 ct ∥ X25519 ephemeral pk.
pub const XWING_CIPHERTEXT_LEN: usize = MLKEM768_CIPHERTEXT_LEN + X25519_LEN; // 1120

/// X-Wing combiner domain separator (`draft-connolly-cfrg-xwing-kem` §5.3):
/// the 6 ASCII bytes `\.//^\` = `5c 2e 2f 2f 5e 5c`.
const XWING_LABEL: &[u8; 6] = br"\.//^\";

/// A 32-byte X-Wing shared secret (feeds the existing HKDF-SHA-256 / AEAD).
pub type SharedSecret = [u8; SHARED_SECRET_LEN];

/// Error from [`encapsulate`] — the only failure mode of this primitive.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XWingError {
    /// The ML-KEM-768 half of the encapsulation key failed FIPS 203 §7.2 input
    /// validation (a coefficient ≥ q). The published key is malformed; refuse
    /// to encapsulate rather than produce an unsound ciphertext.
    #[error("ML-KEM-768 encapsulation key failed FIPS 203 input validation")]
    InvalidEncapsKey,
}

/// Error from [`XWingPublicKey::parse_and_validate`] — a wire-form key that
/// is not [`XWING_ENCAPS_KEY_LEN`] bytes, or is that length but its ML-KEM
/// half fails the same FIPS 203 check [`encapsulate`] runs. Distinct from
/// [`XWingError`] because it fires at PARSE time (a writer deciding whether
/// to store the bytes at all), not at seal time.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XWingKeyError {
    /// The bytes are not the one shape every wrap seals to.
    #[error("X-Wing public key must be {expected} bytes, got {got}")]
    WrongLength {
        /// [`XWING_ENCAPS_KEY_LEN`].
        expected: usize,
        /// The bytes' actual length.
        got: usize,
    },
    /// Right length, but the ML-KEM-768 half fails FIPS 203 §7.2 input
    /// validation — [`XWingError::InvalidEncapsKey`]'s parse-time twin.
    #[error("ML-KEM-768 encapsulation key failed FIPS 203 input validation")]
    InvalidEncapsKey,
}

// ---- key / ciphertext types ----

/// X-Wing **public** (encapsulation) key: ML-KEM-768 ek ∥ X25519 pk. Serialized
/// as [`XWING_ENCAPS_KEY_LEN`] (1216) bytes — this is what a recipient publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XWingPublicKey {
    mlkem_ek: [u8; MLKEM768_ENCAPS_KEY_LEN],
    x25519_pk: [u8; X25519_LEN],
}

/// X-Wing **secret** (decapsulation) key: ML-KEM-768 dk + X25519 scalar.
/// Zeroized on drop. Serialized form is dk ∥ x25519-scalar (2432 bytes), but
/// surfaces normally re-derive it on demand (fleet-consistency) rather than
/// storing it.
#[derive(Clone, ZeroizeOnDrop)]
pub struct XWingSecretKey {
    mlkem_dk: [u8; MLKEM768_DECAPS_KEY_LEN],
    x25519_sk: [u8; X25519_LEN],
}

/// X-Wing ciphertext (an encapsulation): ML-KEM-768 ct ∥ X25519 ephemeral pk.
/// Serialized as [`XWING_CIPHERTEXT_LEN`] (1120) bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XWingCiphertext([u8; XWING_CIPHERTEXT_LEN]);

/// A full X-Wing keypair (see [`derive_keypair`]).
#[derive(Clone)]
pub struct XWingKeyPair {
    pub public: XWingPublicKey,
    pub secret: XWingSecretKey,
}

impl XWingPublicKey {
    /// Assemble from the two component public halves.
    pub fn from_parts(
        mlkem_ek: [u8; MLKEM768_ENCAPS_KEY_LEN],
        x25519_pk: [u8; X25519_LEN],
    ) -> Self {
        Self {
            mlkem_ek,
            x25519_pk,
        }
    }

    /// Parse the 1216-byte wire form (ML-KEM ek ∥ X25519 pk).
    pub fn from_bytes(bytes: &[u8; XWING_ENCAPS_KEY_LEN]) -> Self {
        let mut mlkem_ek = [0u8; MLKEM768_ENCAPS_KEY_LEN];
        let mut x25519_pk = [0u8; X25519_LEN];
        mlkem_ek.copy_from_slice(&bytes[..MLKEM768_ENCAPS_KEY_LEN]);
        x25519_pk.copy_from_slice(&bytes[MLKEM768_ENCAPS_KEY_LEN..]);
        Self {
            mlkem_ek,
            x25519_pk,
        }
    }

    /// The 1216-byte wire form (ML-KEM ek ∥ X25519 pk).
    pub fn to_bytes(&self) -> [u8; XWING_ENCAPS_KEY_LEN] {
        let mut out = [0u8; XWING_ENCAPS_KEY_LEN];
        out[..MLKEM768_ENCAPS_KEY_LEN].copy_from_slice(&self.mlkem_ek);
        out[MLKEM768_ENCAPS_KEY_LEN..].copy_from_slice(&self.x25519_pk);
        out
    }

    /// Parse the [`XWING_ENCAPS_KEY_LEN`]-byte wire form AND validate that its
    /// ML-KEM-768 half passes FIPS 203 §7.2 input validation — the same check
    /// [`encapsulate`] runs against [`XWingError::InvalidEncapsKey`] (the
    /// X25519 half is any 32 bytes; X25519 has no invalid public keys this
    /// combiner cares about). Run this at every point that STORES a caller-
    /// supplied key, not only at seal time: a key admitted by length alone
    /// can still fail FIPS 203 later, and by then it is bound to whatever
    /// wrote it — this is the one place both checks live, so a stored key
    /// this rejects up front never reaches [`encapsulate`] to fail there
    /// instead (the length check that let one seat freeze a room's
    /// mints and rotations).
    pub fn parse_and_validate(bytes: &[u8]) -> Result<Self, XWingKeyError> {
        let arr: &[u8; XWING_ENCAPS_KEY_LEN] =
            bytes.try_into().map_err(|_| XWingKeyError::WrongLength {
                expected: XWING_ENCAPS_KEY_LEN,
                got: bytes.len(),
            })?;
        let key = Self::from_bytes(arr);
        let mlkem_pk = MlKem768PublicKey::from(&key.mlkem_ek);
        if !mlkem768::validate_public_key(&mlkem_pk) {
            return Err(XWingKeyError::InvalidEncapsKey);
        }
        Ok(key)
    }

    /// The ML-KEM-768 encapsulation-key half.
    pub fn mlkem_encaps_key(&self) -> &[u8; MLKEM768_ENCAPS_KEY_LEN] {
        &self.mlkem_ek
    }

    /// The X25519 public-key half.
    pub fn x25519_public(&self) -> &[u8; X25519_LEN] {
        &self.x25519_pk
    }
}

impl XWingSecretKey {
    /// Assemble from the two component secret halves.
    pub fn from_parts(
        mlkem_dk: [u8; MLKEM768_DECAPS_KEY_LEN],
        x25519_sk: [u8; X25519_LEN],
    ) -> Self {
        Self {
            mlkem_dk,
            x25519_sk,
        }
    }

    /// The ML-KEM-768 decapsulation-key half (2400 bytes). The hybrid mail
    /// reader threads this into
    /// `fauna_mls::wrapped_blob::unseal_mail_record_hybrid` alongside the X25519
    /// secret; symmetric to [`XWingPublicKey::mlkem_encaps_key`].
    pub fn mlkem_decaps_key(&self) -> &[u8; MLKEM768_DECAPS_KEY_LEN] {
        &self.mlkem_dk
    }

    /// The X25519 secret-key half (32 bytes).
    pub fn x25519_secret(&self) -> &[u8; X25519_LEN] {
        &self.x25519_sk
    }
}

impl XWingCiphertext {
    /// Wrap raw 1120 bytes (ML-KEM ct ∥ X25519 ephemeral pk).
    pub fn from_bytes(bytes: [u8; XWING_CIPHERTEXT_LEN]) -> Self {
        Self(bytes)
    }

    /// The 1120-byte wire form.
    pub fn as_bytes(&self) -> &[u8; XWING_CIPHERTEXT_LEN] {
        &self.0
    }
}

// ---- key derivation ----

/// Expand `ikm` to the 64-byte ML-KEM-768 `KeyGen` seed (`d ∥ z`) via
/// HKDF-SHA-256, domain-separated by `context`. The goal doc's
/// `expand(seed, context, 64)`. Salt is empty; `context` is the HKDF `info`.
fn expand_mlkem_seed(ikm: &[u8], context: &str) -> [u8; MLKEM768_SEED_LEN] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut seed = [0u8; MLKEM768_SEED_LEN];
    hk.expand(context.as_bytes(), &mut seed)
        .expect("64 <= 255*32, HKDF expand cannot fail for this length");
    seed
}

/// Deterministically derive the **ML-KEM-768** half of an X-Wing keypair from
/// `ikm` and a `context` domain separator: `expand(ikm, context, 64) →
/// ML-KEM-768.KeyGen`. Identical `(ikm, context)` ⇒ identical keypair — the
/// fleet-consistency property surface A relies on (every device re-derives the
/// same recipient key). Mirrors
/// `fauna_mls::wrapped_blob::envelope::derive_x25519_keypair_from_ikm`. Returns
/// `(decaps_key, encaps_key)` (secret, public).
pub fn derive_mlkem768_keypair_from_ikm(
    ikm: &[u8],
    context: &str,
) -> ([u8; MLKEM768_DECAPS_KEY_LEN], [u8; MLKEM768_ENCAPS_KEY_LEN]) {
    let seed = expand_mlkem_seed(ikm, context);
    let kp = mlkem768::generate_key_pair(seed);
    (*kp.sk(), *kp.pk())
}

/// Derive a full X-Wing keypair: the **ML-KEM-768** half deterministically from
/// `mlkem_ikm` + `context` (see [`derive_mlkem768_keypair_from_ikm`]), and the
/// **X25519** half from the caller-supplied 32-byte X25519 secret scalar (its
/// public key is `x25519(sk, base)`).
///
/// The X25519 secret is supplied by the caller because each surface **reuses its
/// existing** MSEK/identity-derived X25519 recipient key rather than minting a
/// fresh one (goal doc § Post-quantum key publication and derivation): mail
/// reuses the `fauna.mail.recipient-hpke.v1` key, subscriptions reuse the
/// identity scalar. Only the ML-KEM half is newly derived and published.
pub fn derive_keypair(
    mlkem_ikm: &[u8],
    context: &str,
    x25519_secret: &[u8; X25519_LEN],
) -> XWingKeyPair {
    let (mlkem_dk, mlkem_ek) = derive_mlkem768_keypair_from_ikm(mlkem_ikm, context);
    let x25519_pk = x25519(*x25519_secret, X25519_BASEPOINT_BYTES);
    XWingKeyPair {
        public: XWingPublicKey::from_parts(mlkem_ek, x25519_pk),
        secret: XWingSecretKey::from_parts(mlkem_dk, *x25519_secret),
    }
}

/// Derive a full X-Wing keypair with **both** halves deterministically from a
/// single `ikm`: the ML-KEM half under `mlkem_context` (see
/// [`derive_mlkem768_keypair_from_ikm`]) and an **independent** X25519 scalar
/// HKDF-SHA-256-expanded from the same `ikm` under `x25519_context`.
///
/// Use this when a surface has **no pre-existing published X25519 key to reuse**
/// — unlike mail (reuses the MSEK-derived recipient key) or subscriptions
/// (reuse the identity scalar), the device-KEM keypair
/// (`fauna_core::generation::derive_device_xwing_keypair`) is derived wholly
/// from the device's Ed25519 secret. The two distinct contexts guarantee the
/// X25519 half is *not* the same scalar as any other key derived from that same
/// `ikm` (no cross-protocol key reuse). Identical `ikm`/contexts ⇒ identical
/// keypair (survives restart, so a later ciphertext still decapsulates).
pub fn derive_keypair_from_ikm(
    ikm: &[u8],
    mlkem_context: &str,
    x25519_context: &str,
) -> XWingKeyPair {
    let x25519_secret = expand_x25519_scalar(ikm, x25519_context);
    derive_keypair(ikm, mlkem_context, &x25519_secret)
}

/// HKDF-SHA-256 expand `ikm` to a 32-byte X25519 secret scalar, domain-separated
/// by `context` (clamping is applied by `x25519()` per RFC 7748).
fn expand_x25519_scalar(ikm: &[u8], context: &str) -> [u8; X25519_LEN] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut scalar = [0u8; X25519_LEN];
    hk.expand(context.as_bytes(), &mut scalar)
        .expect("32 <= 255*32, HKDF expand cannot fail for this length");
    scalar
}

// ---- the KEM ----

/// The X-Wing combiner (`draft-connolly-cfrg-xwing-kem` §5.3):
/// `SHA3-256(ss_M ∥ ss_X ∥ ct_X ∥ pk_X ∥ label)`.
fn combiner(
    ss_m: &[u8; 32],
    ss_x: &[u8; X25519_LEN],
    ct_x: &[u8; X25519_LEN],
    pk_x: &[u8; X25519_LEN],
) -> SharedSecret {
    let mut h = Sha3_256::new();
    h.update(ss_m);
    h.update(ss_x);
    h.update(ct_x);
    h.update(pk_x);
    h.update(XWING_LABEL);
    h.finalize().into()
}

/// Encapsulate to `pk`, drawing the ML-KEM and X25519 ephemeral randomness from
/// `rng`. Returns the 1120-byte ciphertext and the 32-byte shared secret.
///
/// # Errors
///
/// [`XWingError::InvalidEncapsKey`] if the recipient's ML-KEM-768 encapsulation
/// key fails FIPS 203 input validation.
pub fn encapsulate<R: RngCore + CryptoRng>(
    pk: &XWingPublicKey,
    rng: &mut R,
) -> Result<(XWingCiphertext, SharedSecret), XWingError> {
    let mut eph_x25519_sk = [0u8; X25519_LEN];
    let mut mlkem_coins = [0u8; 32];
    rng.fill_bytes(&mut eph_x25519_sk);
    rng.fill_bytes(&mut mlkem_coins);
    let out = encapsulate_derand(pk, &eph_x25519_sk, &mlkem_coins);
    eph_x25519_sk.zeroize();
    mlkem_coins.zeroize();
    out
}

/// Deterministic core of [`encapsulate`]. `eph_x25519_sk` (the X25519 ephemeral
/// scalar) and `mlkem_coins` (the ML-KEM encapsulation randomness) MUST be fresh
/// CSPRNG output in production. Kept private — exposed to tests in-crate only.
fn encapsulate_derand(
    pk: &XWingPublicKey,
    eph_x25519_sk: &[u8; X25519_LEN],
    mlkem_coins: &[u8; 32],
) -> Result<(XWingCiphertext, SharedSecret), XWingError> {
    let mlkem_pk = MlKem768PublicKey::from(&pk.mlkem_ek);
    if !mlkem768::validate_public_key(&mlkem_pk) {
        return Err(XWingError::InvalidEncapsKey);
    }
    let (ct_m, ss_m) = mlkem768::encapsulate(&mlkem_pk, *mlkem_coins);
    // The ML-KEM and X25519 component secrets are wiped from the stack as soon
    // as the combiner has consumed them; only the combined `ss` leaves this
    // function (and the caller Zeroizing's that in turn). (PQ-2)
    let ss_m = Zeroizing::new(ss_m);

    let ct_x = x25519(*eph_x25519_sk, X25519_BASEPOINT_BYTES);
    let ss_x = Zeroizing::new(x25519(*eph_x25519_sk, pk.x25519_pk));

    let ss = combiner(&ss_m, &ss_x, &ct_x, &pk.x25519_pk);

    let mut ct = [0u8; XWING_CIPHERTEXT_LEN];
    ct[..MLKEM768_CIPHERTEXT_LEN].copy_from_slice(ct_m.as_slice());
    ct[MLKEM768_CIPHERTEXT_LEN..].copy_from_slice(&ct_x);
    Ok((XWingCiphertext(ct), ss))
}

/// Decapsulate `ct` with `sk`, returning the 32-byte shared secret. Never
/// errors: a wrong key or tampered ciphertext yields a *different* shared secret
/// (ML-KEM implicit rejection), which the outer AEAD rejects.
pub fn decapsulate(sk: &XWingSecretKey, ct: &XWingCiphertext) -> SharedSecret {
    let mut ct_m = [0u8; MLKEM768_CIPHERTEXT_LEN];
    let mut ct_x = [0u8; X25519_LEN];
    ct_m.copy_from_slice(&ct.0[..MLKEM768_CIPHERTEXT_LEN]);
    ct_x.copy_from_slice(&ct.0[MLKEM768_CIPHERTEXT_LEN..]);

    let mlkem_dk = MlKem768PrivateKey::from(&sk.mlkem_dk);
    let mlkem_ct = MlKem768Ciphertext::from(ct_m);
    // Component secrets wiped from the stack once combined; only the returned
    // `ss` survives (Zeroizing'd by the caller). (PQ-2)
    let ss_m = Zeroizing::new(mlkem768::decapsulate(&mlkem_dk, &mlkem_ct));

    let ss_x = Zeroizing::new(x25519(sk.x25519_sk, ct_x));
    let pk_x = x25519(sk.x25519_sk, X25519_BASEPOINT_BYTES);

    combiner(&ss_m, &ss_x, &ct_x, &pk_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, in-crate CSPRNG for tests (SHA3-256 of a counter). Quality
    /// is irrelevant for correctness tests; reproducibility is what we want.
    struct TestRng {
        ctr: u64,
    }
    impl TestRng {
        fn seeded(seed: u64) -> Self {
            Self {
                ctr: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            }
        }
        fn block(&mut self) -> [u8; 32] {
            let mut h = Sha3_256::new();
            h.update(b"fauna-pq-kem-test-rng");
            h.update(self.ctr.to_le_bytes());
            self.ctr = self.ctr.wrapping_add(1);
            h.finalize().into()
        }
    }
    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            u32::from_le_bytes(self.block()[..4].try_into().unwrap())
        }
        fn next_u64(&mut self) -> u64 {
            u64::from_le_bytes(self.block()[..8].try_into().unwrap())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(32) {
                let b = self.block();
                chunk.copy_from_slice(&b[..chunk.len()]);
            }
        }
    }
    impl CryptoRng for TestRng {}

    fn test_keypair(tag: &str) -> XWingKeyPair {
        // A throwaway X25519 secret derived from the tag (any 32 bytes works).
        let x25519_sk: [u8; 32] = Sha3_256::digest(tag.as_bytes()).into();
        derive_keypair(
            format!("ikm-{tag}").as_bytes(),
            "fauna.test.mlkem.v1",
            &x25519_sk,
        )
    }

    #[test]
    fn sizes_match_goal_doc() {
        assert_eq!(MLKEM768_ENCAPS_KEY_LEN, 1184);
        assert_eq!(MLKEM768_DECAPS_KEY_LEN, 2400);
        assert_eq!(MLKEM768_CIPHERTEXT_LEN, 1088);
        assert_eq!(XWING_ENCAPS_KEY_LEN, 1216);
        assert_eq!(XWING_CIPHERTEXT_LEN, 1120);
        assert_eq!(SHARED_SECRET_LEN, 32);
        assert_eq!(MLKEM768_SEED_LEN, 64);

        let kp = test_keypair("sizes");
        assert_eq!(kp.public.to_bytes().len(), 1216);
        let mut rng = TestRng::seeded(1);
        let (ct, ss) = encapsulate(&kp.public, &mut rng).unwrap();
        assert_eq!(ct.as_bytes().len(), 1120);
        assert_eq!(ss.len(), 32);
    }

    #[test]
    fn round_trip() {
        let kp = test_keypair("rt");
        let mut rng = TestRng::seeded(42);
        let (ct, ss_sender) = encapsulate(&kp.public, &mut rng).unwrap();
        let ss_recipient = decapsulate(&kp.secret, &ct);
        assert_eq!(ss_sender, ss_recipient);
    }

    #[test]
    fn public_key_round_trips_through_bytes() {
        let kp = test_keypair("pkbytes");
        let bytes = kp.public.to_bytes();
        let parsed = XWingPublicKey::from_bytes(&bytes);
        assert_eq!(parsed, kp.public);

        // Encaps to the parsed key, decaps with the original secret.
        let mut rng = TestRng::seeded(7);
        let (ct, ss_a) = encapsulate(&parsed, &mut rng).unwrap();
        assert_eq!(ss_a, decapsulate(&kp.secret, &ct));
    }

    #[test]
    fn deterministic_keygen_is_fleet_consistent() {
        let ikm = b"master-secret";
        let (dk1, ek1) = derive_mlkem768_keypair_from_ikm(ikm, "fauna.mail.recipient-mlkem.v1");
        let (dk2, ek2) = derive_mlkem768_keypair_from_ikm(ikm, "fauna.mail.recipient-mlkem.v1");
        assert_eq!(ek1, ek2);
        assert_eq!(dk1, dk2);

        // Different context ⇒ different key (domain separation).
        let (_, ek_other) = derive_mlkem768_keypair_from_ikm(ikm, "fauna.subs.subscriber-mlkem.v1");
        assert_ne!(ek1, ek_other);

        // Different ikm ⇒ different key.
        let (_, ek_ikm2) =
            derive_mlkem768_keypair_from_ikm(b"other", "fauna.mail.recipient-mlkem.v1");
        assert_ne!(ek1, ek_ikm2);
    }

    #[test]
    fn derive_keypair_from_ikm_is_deterministic_and_round_trips() {
        let ikm = b"device-ed25519-secret";
        let mlkem_ctx = "fauna.generation.device-kem.mlkem.v1 2026-08-13";
        let x25519_ctx = "fauna.generation.device-kem.x25519.v1 2026-08-13";

        // Deterministic: same (ikm, contexts) ⇒ identical keypair (survives
        // restart, so a later ciphertext still decapsulates).
        let kp1 = derive_keypair_from_ikm(ikm, mlkem_ctx, x25519_ctx);
        let kp2 = derive_keypair_from_ikm(ikm, mlkem_ctx, x25519_ctx);
        assert_eq!(kp1.public, kp2.public);

        // Round-trip: encaps to the derived public, decaps with the derived secret.
        let mut rng = TestRng::seeded(2026);
        let (ct, ss_enc) = encapsulate(&kp1.public, &mut rng).unwrap();
        assert_eq!(ss_enc, decapsulate(&kp2.secret, &ct));

        // The X25519 half is an INDEPENDENT scalar (no cross-protocol reuse):
        // a different x25519 context yields a different X25519 public even when
        // the ML-KEM half is identical, and it is never the bare ikm-as-scalar.
        let kp_other_x = derive_keypair_from_ikm(ikm, mlkem_ctx, "fauna.other.x25519.v1");
        assert_eq!(
            kp1.public.mlkem_encaps_key(),
            kp_other_x.public.mlkem_encaps_key()
        );
        assert_ne!(
            kp1.public.x25519_public(),
            kp_other_x.public.x25519_public()
        );

        // Different ikm ⇒ different keypair.
        let kp_other_ikm = derive_keypair_from_ikm(b"other-secret", mlkem_ctx, x25519_ctx);
        assert_ne!(kp1.public, kp_other_ikm.public);
    }

    #[test]
    fn wrong_secret_yields_different_shared_secret() {
        // ML-KEM uses implicit rejection: decap never errors, it returns a
        // *different* secret for the wrong key.
        let kp = test_keypair("a");
        let other = test_keypair("b");
        let mut rng = TestRng::seeded(99);
        let (ct, ss) = encapsulate(&kp.public, &mut rng).unwrap();
        assert_ne!(ss, decapsulate(&other.secret, &ct));
    }

    #[test]
    fn tampered_ciphertext_changes_secret() {
        let kp = test_keypair("tamper");
        let mut rng = TestRng::seeded(123);
        let (ct, ss) = encapsulate(&kp.public, &mut rng).unwrap();

        // Flip a byte in the ML-KEM ciphertext half.
        let mut bytes = *ct.as_bytes();
        bytes[10] ^= 0x01;
        let tampered = XWingCiphertext::from_bytes(bytes);
        assert_ne!(ss, decapsulate(&kp.secret, &tampered));

        // Flip a byte in the X25519 ephemeral-pk half.
        let mut bytes2 = *ct.as_bytes();
        bytes2[MLKEM768_CIPHERTEXT_LEN] ^= 0x01;
        let tampered2 = XWingCiphertext::from_bytes(bytes2);
        assert_ne!(ss, decapsulate(&kp.secret, &tampered2));
    }

    #[test]
    fn combiner_label_is_exactly_the_draft_bytes() {
        assert_eq!(XWING_LABEL, &[0x5c, 0x2e, 0x2f, 0x2f, 0x5e, 0x5c]);
        assert_eq!(XWING_LABEL.len(), 6);
    }

    #[test]
    fn combiner_known_answer() {
        // Pins the exact field order + label. Inputs are fixed patterns; the
        // expected output is SHA3-256(ss_m ∥ ss_x ∥ ct_x ∥ pk_x ∥ label) — a
        // regression anchor against accidental reordering / label drift.
        let ss_m = [0x11u8; 32];
        let ss_x = [0x22u8; 32];
        let ct_x = [0x33u8; 32];
        let pk_x = [0x44u8; 32];
        let got = combiner(&ss_m, &ss_x, &ct_x, &pk_x);

        // Independently recompute with an explicitly-built buffer (a different
        // code path than `combiner`'s incremental updates) — catches an
        // ordering bug that a tautological re-update would miss.
        let mut buf = Vec::with_capacity(32 * 4 + 6);
        buf.extend_from_slice(&ss_m);
        buf.extend_from_slice(&ss_x);
        buf.extend_from_slice(&ct_x);
        buf.extend_from_slice(&pk_x);
        buf.extend_from_slice(XWING_LABEL);
        let expected: [u8; 32] = Sha3_256::digest(&buf).into();
        assert_eq!(got, expected);
    }

    #[test]
    fn invalid_encaps_key_is_rejected() {
        // An all-0xFF ML-KEM ek has coefficients ≥ q and must fail validation.
        let kp = test_keypair("invalid");
        let bad = XWingPublicKey::from_parts(
            [0xFFu8; MLKEM768_ENCAPS_KEY_LEN],
            *kp.public.x25519_public(),
        );
        let mut rng = TestRng::seeded(5);
        assert_eq!(
            encapsulate(&bad, &mut rng),
            Err(XWingError::InvalidEncapsKey)
        );
    }

    // ── `parse_and_validate` — the storage-time gate ──────────
    //
    // A wire-form key a caller wants a nest (or any other writer) to STORE
    // must be refused up front when it would fail `encapsulate`'s own FIPS
    // 203 check later — a length-only gate lets exactly this key through,
    // and no writer today runs it, so this is red until `parse_and_validate`
    // exists and does both checks.

    #[test]
    fn parse_and_validate_accepts_a_well_formed_key() {
        let kp = test_keypair("parse-ok");
        let bytes = kp.public.to_bytes();
        let parsed =
            XWingPublicKey::parse_and_validate(&bytes).expect("a derived key is always valid");
        assert_eq!(parsed, kp.public);
    }

    #[test]
    fn parse_and_validate_rejects_the_wrong_length() {
        assert_eq!(
            XWingPublicKey::parse_and_validate(&[0x11u8; 5]),
            Err(XWingKeyError::WrongLength {
                expected: XWING_ENCAPS_KEY_LEN,
                got: 5,
            })
        );
    }

    #[test]
    fn parse_and_validate_rejects_a_fips_invalid_key_of_the_right_length() {
        // The exact shape `invalid_encaps_key_is_rejected` above catches only
        // at seal time: 1216 bytes, so every length gate admits it, and an
        // all-0xFF ML-KEM half fails FIPS 203 (coefficients >= q).
        let bad = [0xFFu8; XWING_ENCAPS_KEY_LEN];
        assert_eq!(
            XWingPublicKey::parse_and_validate(&bad),
            Err(XWingKeyError::InvalidEncapsKey)
        );
    }

    #[test]
    fn pinned_vectors_survive_libcrux_bumps() {
        // Persisted X-Wing envelopes (wrapped blobs, subscription KeyBlobs, WG
        // PSKs) depend on every device re-deriving byte-identical keys from
        // identical seeds, so a libcrux-ml-kem bump must not change any derived
        // byte (version-compatibility.md § additive-everywhere). These digests
        // were captured on libcrux-ml-kem 0.0.8 (unchanged on 0.0.10, 2026-10-05)
        // and cover keygen, derand
        // encaps, honest decaps, and the implicit-rejection branch (the
        // const-time-select path RUSTSEC-2026-0212 concerned on aarch64).
        fn hex(b: &[u8]) -> String {
            b.iter().map(|x| format!("{x:02x}")).collect()
        }
        let (dk, ek) =
            derive_mlkem768_keypair_from_ikm(b"fauna-pq-kem-pin-vector-ikm", "fauna.test.pin.v1");
        assert_eq!(
            hex(&Sha3_256::digest(ek)),
            "3148c794ab68f8cd44089e67e014fd357e4fab9546c7e2c51c1e06a565967439"
        );
        assert_eq!(
            hex(&Sha3_256::digest(dk)),
            "a2f371134e49bd1a855eccbf81e7af5e8719fafb7553b50d2ebb5c4d02664be9"
        );

        let kp = derive_keypair_from_ikm(
            b"fauna-pq-kem-pin-vector-ikm",
            "fauna.test.pin.mlkem.v1",
            "fauna.test.pin.x25519.v1",
        );
        let (ct, ss) = encapsulate_derand(&kp.public, &[0x42u8; 32], &[0x24u8; 32]).unwrap();
        assert_eq!(ss, decapsulate(&kp.secret, &ct));
        assert_eq!(
            hex(&Sha3_256::digest(ct.as_bytes())),
            "7deb8c9e0dac724d37a596690bae25a88b3a70c67d750961eb90ea045b6f489b"
        );
        assert_eq!(
            hex(&Sha3_256::digest(ss)),
            "5c0eab841d9823dff15e378c2ab90cad7d21e499abae41208c836dd1aa42bd42"
        );

        // Implicit rejection: a tampered ML-KEM half must yield the SAME
        // pseudo-random secret on every version (it feeds the outer AEAD's
        // deterministic failure, and it is where the const-time select runs).
        let mut bytes = *ct.as_bytes();
        bytes[7] ^= 0x80;
        let rejected = decapsulate(&kp.secret, &XWingCiphertext::from_bytes(bytes));
        assert_eq!(
            hex(&Sha3_256::digest(rejected)),
            "77f642fb24061c68a1153f4d1b91752165bab5e926bb5f480e032199990b3589"
        );
    }
}
