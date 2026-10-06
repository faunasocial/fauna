//! Actor identity management.
//!
//! Each Actor is identified by an Ed25519 keypair. The public key serves as the
//! Actor's globally unique, self-certifying identifier (ActorId).

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};

/// Generate a random `n`-byte token, lowercase-hex encoded (`2*n` hex chars).
///
/// The shared shape behind the codebase's `getrandom::fill` + `hex::encode`
/// token/ID-minting call sites — not for raw key material (those keep their
/// own fixed-size arrays and typed constructors).
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("getrandom failed");
    hex::encode(buf)
}

/// A 32-byte Actor identifier derived from an Ed25519 public key.
///
/// **Wire shape — frozen as a byte string.** An `ActorId` encodes as a
/// 32-byte CBOR **byte string** (`0x58 0x20` + the key), like every
/// serialized fixed-width byte field; the hand-written serde impl below
/// carries it, so every field of this type inherits the shape without an
/// attribute. The signed canonical payloads it sits in (`ShareToken.author`,
/// `RecoveryKeyRegistration.actor_id`, the succession statements) have
/// signatures over the CID of exactly these bytes, so from the 2026-10
/// baseline on the shape can never change again, not at a major bump. Owner
/// of the rule: `docs/goal/architecture/serialization.md` § Canonical IPLD
/// dag-cbor, "Fixed-size byte arrays". The `tests` module below pins the
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId(pub [u8; 32]);

impl Serialize for ActorId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_bytes::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde_bytes::deserialize(deserializer).map(Self)
    }
}

impl ActorId {
    /// Parse a hex-encoded 32-byte ActorId. Surrounding whitespace is trimmed.
    pub fn from_hex(s: &str) -> Result<Self, crate::hex32::Hex32Error> {
        crate::hex32::decode(s).map(Self)
    }

    /// [`Self::from_hex`] for a `String`-error boundary (UI/RPC glue that
    /// surfaces the failure as plain text) — same parse, the error rendered
    /// as `"actor_id {e}"` (tui's and linux's `conversations::conv_backend`
    /// each carried this identical wrapper before the lift).
    pub fn from_hex_labeled(s: &str) -> Result<Self, String> {
        Self::from_hex(s).map_err(|e| format!("actor_id {e}"))
    }

    /// Lowercase-hex encoding of this ActorId.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Returns the first byte, used for firehose shard assignment.
    pub fn shard_index(&self) -> u8 {
        self.0[0]
    }

    /// Convert this Ed25519 public key to an X25519 public key for
    /// Diffie-Hellman key agreement (used by broadcast key blobs).
    pub fn to_x25519_public(&self) -> X25519PublicKey {
        let verifying_key =
            VerifyingKey::from_bytes(&self.0).expect("ActorId contains a valid Ed25519 public key");
        let montgomery = verifying_key.to_montgomery();
        X25519PublicKey::from(montgomery.to_bytes())
    }
}

impl From<VerifyingKey> for ActorId {
    fn from(key: VerifyingKey) -> Self {
        Self(key.to_bytes())
    }
}

/// Verify a detached Ed25519 signature by `public_key` over `message`.
///
/// **The one verification primitive for every "prove you hold the key you
/// name" gate** — the ceremonies where the public key arrives *from the wire*
/// alongside the signature, so the key is attacker-chosen.
///
/// ⚠ **Do not look for the call sites in a list.** This doc used to name them
/// (the nest's auth/register/invite/claim/lockout/device routes and the push
/// relay's signed routes) and the list was wrong: it omitted the nest's
/// storage-mode, NAT-mode and share-link ceremonies, which are the identical
/// shape wearing different names, and they stayed permissive through two
/// deliberate sweeps that both concluded the nest was fully converted. The
/// authority is now a walk — `bins/fauna-nest/src/state.rs::nest_has_one_ed25519_verification_shape`
/// reds if any nest source reaches the permissive trait at all. Applying this
/// primitive is a judgement about the *key's provenance* (does it come from the
/// wire?), never about whether a ceremony's name appears in a list.
///
/// It carries two properties the permissive [`VerifyingKey::verify`]
/// does not, both load-bearing precisely because the key is attacker-chosen:
///
/// * **Small-order keys are refused outright** (`is_weak`). For a small-order
///   public key the permissive `verify` is not a signature check at all: the
///   all-zero key with an all-zero signature satisfies the verification
///   equation for every message whose challenge scalar clears the key's order —
///   measured 13 of 64 against the nest's own gate, 19 of 64 against the
///   relay's. That admits requests for identities nobody holds a key to.
/// * **`verify_strict`, not `verify`** — additionally rejecting a small-order
///   `R`, which closes the cofactor-malleability gap.
///
/// Honest signers are unaffected: a real keypair is never small-order, and a
/// real signature never carries a small-order `R`.
///
/// ⚠ The two refusals **overlap on the key axis and no test can tell them
/// apart**: `verify_strict` already rejects a small-order `A`, so `is_weak` is
/// belt-and-braces here, not load-bearing, and
/// `a_small_order_public_key_never_verifies_at_the_primitive` (this module) pins
/// the *property* rather than either implementation half. It is kept because
/// it names the refusal at the point of refusal — but do not read it as a
/// separately-witnessed gate, and do not delete `verify_strict` believing
/// `is_weak` covers it (it does not cover `R`).
///
/// The consumer-side twins named `a_small_order_actor_id_never_verifies`
/// (`bins/fauna-nest/src/routes.rs`, `bins/fauna-push-relay/src/api.rs`) pin the
/// same property at their own ceremonies. They are additional witnesses, not
/// this one's substitute: a third such twin was orphaned on 2026-08-16 when the
/// share route it guarded was deleted, which is why the primitive now witnesses
/// itself.
///
/// This unifies only the *verification primitive*. It deliberately does **not**
/// unify the surrounding idiom (timestamp-drift window, replay-guard scoping,
/// message construction) — those genuinely differ per ceremony, and collapsing
/// them would level every caller to the weakest one's assumptions.
pub fn verify_detached(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    if verifying_key.is_weak() {
        return false;
    }
    verifying_key
        .verify_strict(message, &ed25519_dalek::Signature::from_bytes(&signature))
        .is_ok()
}

/// An Actor's keypair for signing content.
///
/// The secret key material is zeroed from memory when this struct is dropped.
pub struct ActorKeypair {
    secret_bytes: zeroize::Zeroizing<[u8; 32]>,
    signing_key: SigningKey,
}

impl ActorKeypair {
    /// Reconstruct an Actor keypair from a 32-byte secret.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret);
        Self {
            secret_bytes: zeroize::Zeroizing::new(secret),
            signing_key,
        }
    }

    /// Reconstruct an Actor keypair from a hex-encoded 32-byte secret.
    /// Surrounding whitespace is trimmed.
    pub fn from_secret_hex(secret_hex: &str) -> Result<Self, crate::hex32::Hex32Error> {
        crate::hex32::decode(secret_hex).map(Self::from_secret)
    }

    /// Lowercase-hex encoding of this keypair's public [`ActorId`].
    pub fn actor_id_hex(&self) -> String {
        self.actor_id().to_hex()
    }

    /// Generate a new random Actor keypair.
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("getrandom failed");
        let signing_key = SigningKey::from_bytes(&secret);
        let zeroizing = zeroize::Zeroizing::new(secret);
        Self {
            secret_bytes: zeroizing,
            signing_key,
        }
    }

    /// Returns the public ActorId for this keypair.
    pub fn actor_id(&self) -> ActorId {
        ActorId::from(self.signing_key.verifying_key())
    }

    /// Returns a reference to the signing key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Returns the verifying (public) key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Returns the raw 32-byte secret key (needed for ECIES decryption).
    pub fn secret_bytes(&self) -> &[u8; 32] {
        &self.secret_bytes
    }

    /// Convert this Ed25519 signing key to an X25519 static secret for
    /// Diffie-Hellman key agreement.
    pub fn to_x25519_secret(&self) -> X25519StaticSecret {
        use sha2::{Digest, Sha512};
        let hash = Sha512::digest(self.signing_key.to_bytes());
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&hash[..32]);
        // Clamping (same as X25519 spec)
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        X25519StaticSecret::from(scalar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical bytes of an `ActorId` are frozen: a 32-byte CBOR byte
    /// string (`0x58 0x20 …`). Signed payloads that carry an `ActorId`
    /// (`ShareToken`, `RecoveryKeyRegistration`, the succession statements)
    /// sign the CID of exactly these bytes, so from the 2026-10 baseline on a
    /// change here would invalidate every stored signature with no migration
    /// able to re-sign (`docs/goal/architecture/serialization.md` § Canonical
    /// IPLD dag-cbor, "Fixed-size byte arrays").
    #[test]
    fn actor_id_canonical_bytes_are_a_32_byte_string() {
        let id = ActorId([0x11; 32]);
        let bytes = fauna_cbor::encode_canonical(&id).unwrap();
        let mut expect = vec![0x58, 0x20];
        expect.extend_from_slice(&[0x11; 32]);
        assert_eq!(bytes, expect);
        let back: ActorId = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, id);
        // The retired integer-array spelling is refused, not tolerated.
        let mut array = vec![0x98, 0x20];
        array.extend_from_slice(&[0x11; 32]);
        assert!(fauna_cbor::decode_strict::<ActorId>(&array).is_err());
        // A wrong-width byte string is refused.
        let mut short = vec![0x58, 0x1f];
        short.extend_from_slice(&[0x11; 31]);
        assert!(fauna_cbor::decode_strict::<ActorId>(&short).is_err());
    }

    /// A non-binary format keeps the shape it always had: serde_json writes
    /// the byte string as an array of 32 numbers and reads that array back,
    /// so the JSON surfaces (and the WASM bridge's JS arrays) see no change.
    #[test]
    fn actor_id_json_is_an_array_of_numbers_both_ways() {
        let id = ActorId([7; 32]);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("[{}]", vec!["7"; 32].join(",")));
        assert_eq!(serde_json::from_str::<ActorId>(&json).unwrap(), id);
    }

    /// The small-order refusal, pinned at the PRIMITIVE — not only at
    /// consumers of it.
    ///
    /// [`verify_detached`]'s doc cited `a_small_order_actor_id_never_verifies`
    /// for this property, but both witnesses of that name live in *other*
    /// crates' consumer test modules (`bins/fauna-nest/src/routes.rs`,
    /// `bins/fauna-push-relay/src/api.rs`). A citation across a crate boundary
    /// is orphanable by an ordinary refactor of the consumer: a third witness
    /// was lost exactly that way on 2026-08-16, when the share route's
    /// `verify_fauna_identity` (and its small-order test with it) was deleted
    /// as part of finding's remedy. The property survived, because it
    /// is structural here — `is_weak()` plus `verify_strict` — but nothing in
    /// this crate said so.
    ///
    /// So this is the local witness: the guarantee is stated where it is
    /// implemented, and it cannot be orphaned by any caller's rework.
    #[test]
    fn a_small_order_public_key_never_verifies_at_the_primitive() {
        // The all-zero key is small-order: for a signature of all zeroes it
        // satisfies the verification equation whenever the challenge scalar
        // clears the key's order — which a naive `verify` accepts on a sizable
        // fraction of messages.
        let weak = [0u8; 32];
        let zero_sig = [0u8; 64];

        let accepted = (0u64..64)
            .filter(|i| verify_detached(&weak, &i.to_be_bytes(), &zero_sig))
            .count();

        assert_eq!(
            accepted, 0,
            "the all-zero (small-order) public key forged a valid signature on \
             {accepted} of 64 messages at the verification primitive itself"
        );
    }

    #[test]
    fn actor_id_hex_round_trips() {
        let id = ActorKeypair::generate().actor_id();
        let parsed = ActorId::from_hex(&id.to_hex()).expect("round-trip");
        assert_eq!(parsed, id);
    }

    #[test]
    fn from_secret_hex_matches_from_secret() {
        let secret = [0x42u8; 32];
        let kp = ActorKeypair::from_secret_hex(&hex::encode(secret)).expect("valid secret hex");
        assert_eq!(kp.actor_id(), ActorKeypair::from_secret(secret).actor_id());
    }

    #[test]
    fn actor_id_hex_equals_legacy_derivation() {
        // Equivalence guard for the linux/wasm `actor_id_from_secret_hex` swap:
        // `from_secret_hex(x).actor_id_hex()` must equal the old hand-rolled
        // `SigningKey::from_bytes(x).verifying_key()` hex path.
        let secret = [0x07u8; 32];
        let legacy = hex::encode(SigningKey::from_bytes(&secret).verifying_key().to_bytes());
        let lifted = ActorKeypair::from_secret_hex(&hex::encode(secret))
            .expect("valid secret hex")
            .actor_id_hex();
        assert_eq!(lifted, legacy);
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert!(ActorId::from_hex("nothex").is_err());
        assert!(ActorId::from_hex("deadbeef").is_err()); // valid hex, wrong length
    }

    #[test]
    fn from_hex_labeled_matches_from_hex_and_labels_the_error() {
        let id = ActorKeypair::generate().actor_id();
        assert_eq!(ActorId::from_hex_labeled(&id.to_hex()), Ok(id));
        let err = ActorId::from_hex_labeled("nothex").unwrap_err();
        assert!(err.starts_with("actor_id "), "unexpected message: {err}");
    }

    #[test]
    fn random_hex_has_the_right_length_and_is_valid_hex() {
        let s = random_hex(16);
        assert_eq!(s.len(), 32);
        assert!(hex::decode(&s).is_ok());
    }

    #[test]
    fn random_hex_is_not_constant() {
        assert_ne!(random_hex(16), random_hex(16));
    }
}
