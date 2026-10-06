use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
/// The published ML-KEM-768 encapsulation-key length (1184 bytes). Re-exported
/// here so the WASM/FFI subscription bindings validate roster ek slots against
/// one canonical length without a direct `fauna-pq-kem` dependency.
pub use fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN;
use fauna_pq_kem::{XWING_CIPHERTEXT_LEN, XWingCiphertext, XWingKeyPair, XWingPublicKey};
use rand_core_09::{OsRng as PqOsRng, TryRngCore};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::Zeroizing;

use crate::data::{Capability, ContentHash, DeviceAuthorization, Timestamp};
use crate::encoding::{EmbedAsBytes, decode_signed_bytes, sign_envelope, verify_envelope};
use crate::identity::{ActorId, ActorKeypair};
use crate::subscription::types::{KemSuiteId, KeyBlob, KeyBlobEntry};

/// Derive a per-post symmetric key from a base key (MLS epoch key or
/// broadcast period key) and the post's content hash.
///
/// Two-step BLAKE3 derivation: `derive_key("fauna.gated.v1", base_key)` for
/// context separation, then `keyed_hash(intermediate, post_id)` for the
/// per-post salt. Returns a 32-byte ChaCha20-Poly1305 key.
pub fn derive_post_key(base_key: &[u8; 32], post_id: &ContentHash) -> [u8; 32] {
    crate::domain_key::derive_domain_key("fauna.gated.v1", base_key, &post_id.digest())
}

/// Derive the seal key for a post's **web-paywall rendered output** (the
/// sealed `web_rendered` class — `architecture/encryption-at-rest.md`
/// § Readable classes item 2) from the tier's period key and the post's
/// record CID.
///
/// Same two-step BLAKE3 shape as [`derive_post_key`], under a distinct
/// context so the rendered-HTML seal and the body-blob seal never share a
/// key even for the same post. The derive input here is the post **record's
/// CID digest** (known at render time — no creation-order circularity, unlike
/// the body blob's `GatedInfo::seal_id`), so re-deriving requires exactly the
/// granted period key: revoking the web-serve holder's grant leaves the
/// sealed rendered bytes unopenable by the box.
pub fn derive_web_render_key(base_key: &[u8; 32], post_id: &ContentHash) -> [u8; 32] {
    crate::domain_key::derive_domain_key("fauna.web-render.v1", base_key, &post_id.digest())
}

/// BLAKE3 `derive_key` context of [`period_key_commitment`]. Distinct from
/// every other context the period key is fed to (`fauna.gated.v1`,
/// `fauna.web-render.v1`, the per-entry `fauna.keyblob.v1`), so the
/// commitment shares no bytes with any key derived from the period key.
pub const KEY_BLOB_COMMITMENT_CONTEXT: &str = "fauna.keyblob.commit.v1";

/// The **key witness** a minted [`KeyBlob`] carries
/// (`KeyBlob::key_commitment`): a one-way commitment to the key its entries
/// wrap, so the author can ask a stored blob *which* key it wraps without
/// unwrapping an entry — none is wrapped to the author. The author compares
/// it against its custody (`TierPeriod::key`, current and prior) to tell a
/// blob that still wraps a rotated-out key from one that wraps the current
/// one.
///
/// A pure function of the key: two blobs wrapping one key commit identically,
/// whoever minted them and whenever. That is the property the witness needs,
/// and it is safe to publish because the blob reaches only principals who
/// can unwrap the key anyway (`KeyBlob::key_commitment`'s doc).
pub fn period_key_commitment(wrapped_key: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(KEY_BLOB_COMMITMENT_CONTEXT, wrapped_key)
}

/// Encrypt plaintext with ChaCha20-Poly1305 under a random nonce.
///
/// Returns: 12-byte nonce ++ ciphertext ++ 16-byte auth tag. Random-nonce
/// mode is required by `docs/goal/ui/media.md` § Encryption at rest — one
/// per-post key seals body + N attachments together, so per-blob nonces
/// must not collide.
pub fn encrypt_content(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("ChaCha20-Poly1305 encryption should not fail");

    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    output
}

/// Decrypt ciphertext produced by `encrypt_content`.
///
/// Input format: 12-byte nonce ++ ciphertext ++ 16-byte auth tag.
pub fn decrypt_content(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, DecryptError> {
    if data.len() < 12 {
        return Err(DecryptError::TooShort);
    }

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new(key.into());

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| DecryptError::AuthenticationFailed)
}

/// Errors from decrypting gated content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecryptError {
    /// Ciphertext too short to contain a nonce.
    TooShort,
    /// AEAD authentication failed (wrong key or tampered data).
    AuthenticationFailed,
    /// The entry was wrapped with a KEM suite this build does not name
    /// ([`KemSuiteId::Unknown`]): this one entry is unopenable here.
    UnknownSuite,
}

impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "ciphertext too short"),
            Self::AuthenticationFailed => {
                write!(f, "decryption failed: wrong key or tampered data")
            }
            Self::UnknownSuite => {
                write!(f, "key wrapped with a KEM suite this build does not know")
            }
        }
    }
}

impl std::error::Error for DecryptError {}

/// Create a key blob entry for a subscriber.
///
/// Encrypts `period_key` using an ephemeral X25519 key exchange with the
/// subscriber's public key. Wrap key is `blake3::derive_key("fauna.keyblob.v1",
/// shared_secret)`; AEAD is ChaCha20-Poly1305 with a random nonce. Output
/// format in `encrypted_key`:
/// 32-byte ephemeral public key ++ 12-byte nonce ++ ciphertext ++ 16-byte tag.
pub fn create_key_blob_entry(subscriber_id: &ActorId, period_key: &[u8; 32]) -> KeyBlobEntry {
    let subscriber_x25519 = subscriber_id.to_x25519_public();

    let ephemeral_secret = X25519StaticSecret::random_from_rng(OsRng);
    let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);
    let shared = ephemeral_secret.diffie_hellman(&subscriber_x25519);

    let derived_key = blake3::derive_key("fauna.keyblob.v1", shared.as_bytes());

    let cipher = ChaCha20Poly1305::new((&derived_key).into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, period_key.as_slice())
        .expect("ChaCha20-Poly1305 encryption should not fail");

    let mut encrypted_key = Vec::with_capacity(32 + 12 + ciphertext.len());
    encrypted_key.extend_from_slice(ephemeral_public.as_bytes());
    encrypted_key.extend_from_slice(&nonce);
    encrypted_key.extend_from_slice(&ciphertext);

    KeyBlobEntry {
        subscriber: *subscriber_id,
        encrypted_key,
        // Today's ephemeral-X25519 wrap. Omitted on the wire (classical default)
        // so this entry stays byte-identical to a pre-agility one.
        suite: KemSuiteId::Classical,
    }
}

/// Decrypt a key blob entry using the subscriber's keypair.
///
/// Returns the 32-byte period key.
pub fn decrypt_key_blob_entry(
    subscriber: &ActorKeypair,
    encrypted_key: &[u8],
) -> Result<[u8; 32], DecryptError> {
    // Minimum: 32 (ephemeral pubkey) + 12 (nonce) + 32 (encrypted key) + 16 (tag)
    if encrypted_key.len() < 92 {
        return Err(DecryptError::TooShort);
    }

    let ephemeral_public = {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&encrypted_key[..32]);
        X25519PublicKey::from(bytes)
    };

    let nonce_bytes = &encrypted_key[32..44];
    let ciphertext = &encrypted_key[44..];

    let subscriber_secret = subscriber.to_x25519_secret();
    let shared = subscriber_secret.diffie_hellman(&ephemeral_public);
    let derived_key = blake3::derive_key("fauna.keyblob.v1", shared.as_bytes());

    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new((&derived_key).into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| DecryptError::AuthenticationFailed)?;

    if plaintext.len() != 32 {
        return Err(DecryptError::AuthenticationFailed);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

// ---- Post-quantum (X-Wing) subscription wrap — surface B, slice S4 ----
//
// The hybrid sibling of [`create_key_blob_entry`] / [`decrypt_key_blob_entry`].
// Unlike X25519 — where the author derives the subscriber's wrap key from the
// public ActorId for free (Ed25519 → Montgomery) — an ML-KEM key has no such
// trick, so the subscriber derives an X-Wing keypair from their **identity
// seed** and **publishes** the 1184-byte ML-KEM encapsulation key; the author
// reconstructs the X-Wing public from `(published_ek, ActorId-derived X25519)`
// and wraps to it. Goal doc `architecture/security/post-quantum.md`
// § Post-quantum key publication and derivation (subscriptions).

/// HKDF-`info` domain separator for a subscriber's identity-seed-derived ML-KEM
/// keypair (the X-Wing post-quantum half of the subscription wrap). Distinct
/// from mail's MSEK-derived `fauna.mail.recipient-mlkem.v1` — the subscription
/// half derives from the identity seed, pairing with the existing identity
/// scalar X25519 subscriber wrap key.
pub const SUBSCRIBER_MLKEM_DERIVE_CONTEXT: &str = "fauna.subscription.subscriber-mlkem.v1";

/// Derive a subscriber's full X-Wing keypair: the ML-KEM-768 half
/// deterministically from the identity seed + [`SUBSCRIBER_MLKEM_DERIVE_CONTEXT`],
/// and the X25519 half reusing the existing identity scalar
/// ([`ActorKeypair::to_x25519_secret`]). Deterministic in the seed, so every
/// device of the same subscriber re-derives the identical key — the
/// fleet-consistency the published-ek read path relies on. The X25519 half of
/// the resulting public key equals [`ActorId::to_x25519_public`] (the author
/// reconstructs the public from that + the published ML-KEM ek).
pub fn derive_subscriber_xwing_keypair(subscriber: &ActorKeypair) -> XWingKeyPair {
    let x25519_secret = subscriber.to_x25519_secret();
    fauna_pq_kem::derive_keypair(
        subscriber.secret_bytes(),
        SUBSCRIBER_MLKEM_DERIVE_CONTEXT,
        &x25519_secret.to_bytes(),
    )
}

/// The subscriber's 1184-byte ML-KEM-768 encapsulation key — the value a
/// subscriber publishes so authors can wrap hybrid `KeyBlob` entries to them.
/// The X25519 half is the public ActorId (not published separately).
pub fn subscriber_mlkem_encaps_key(subscriber: &ActorKeypair) -> [u8; MLKEM768_ENCAPS_KEY_LEN] {
    *derive_subscriber_xwing_keypair(subscriber)
        .public
        .mlkem_encaps_key()
}

/// Error wrapping a period key to a subscriber's published X-Wing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyBlobWrapError {
    /// The subscriber's published ML-KEM encapsulation key failed FIPS 203
    /// input validation. The published key is malformed; the caller degrades
    /// to the classical suite (PQ-4b) rather than producing an unsound entry.
    InvalidEncapsKey,
}

impl std::fmt::Display for KeyBlobWrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEncapsKey => {
                write!(
                    f,
                    "subscriber's published ML-KEM encapsulation key is malformed"
                )
            }
        }
    }
}

impl std::error::Error for KeyBlobWrapError {}

/// Create an X-Wing (ML-KEM-768 ∥ X25519) key blob entry for a subscriber.
///
/// Encapsulates to the subscriber's X-Wing public key, reconstructed from their
/// **published** 1184-byte ML-KEM ek and their ActorId-derived X25519 public.
/// Wrap key is `blake3::derive_key("fauna.keyblob.xwing.v1", shared_secret)`;
/// AEAD is ChaCha20-Poly1305 with a random nonce — the same construction as the
/// classical [`create_key_blob_entry`], only the KEM differs. Output format in
/// `encrypted_key`:
/// 1120-byte X-Wing ciphertext ++ 12-byte nonce ++ ciphertext ++ 16-byte tag.
pub fn create_key_blob_entry_xwing(
    subscriber_id: &ActorId,
    subscriber_mlkem_ek: &[u8; MLKEM768_ENCAPS_KEY_LEN],
    period_key: &[u8; 32],
) -> Result<KeyBlobEntry, KeyBlobWrapError> {
    let x25519_pk = subscriber_id.to_x25519_public();
    let recipient_pk = XWingPublicKey::from_parts(*subscriber_mlkem_ek, *x25519_pk.as_bytes());

    // `rand_core` 0.9 `OsRng` (the same RNG the mail X-Wing seal uses);
    // `unwrap_err()` is the infallible wrapper that panics only on OS RNG
    // failure. Distinct from the `aead::OsRng` used for the nonce below.
    let mut csprng = PqOsRng.unwrap_err();
    let (xwing_ct, shared_secret) = fauna_pq_kem::encapsulate(&recipient_pk, &mut csprng)
        .map_err(|_| KeyBlobWrapError::InvalidEncapsKey)?;
    let shared_secret = Zeroizing::new(shared_secret);

    let derived_key = blake3::derive_key("fauna.keyblob.xwing.v1", shared_secret.as_slice());

    let cipher = ChaCha20Poly1305::new((&derived_key).into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, period_key.as_slice())
        .expect("ChaCha20-Poly1305 encryption should not fail");

    let mut encrypted_key = Vec::with_capacity(XWING_CIPHERTEXT_LEN + 12 + ciphertext.len());
    encrypted_key.extend_from_slice(xwing_ct.as_bytes());
    encrypted_key.extend_from_slice(&nonce);
    encrypted_key.extend_from_slice(&ciphertext);

    Ok(KeyBlobEntry {
        subscriber: *subscriber_id,
        encrypted_key,
        suite: KemSuiteId::Xwing,
    })
}

/// Wrap the period key for one subscriber, selecting the suite automatically —
/// the single gate both wrap sites (the client's `mint_key_blob` and the
/// plaintext-mode nest mint) share, so the policy lives in shared Rust once.
///
/// X-Wing is chosen iff the subscriber published an ML-KEM ek; otherwise
/// classical (no capability token gates it — `post-quantum.md` § Capability
/// negotiation, the 2026-09-24 ruling). On an
/// X-Wing wrap *error* (a malformed published ek) it degrades to classical
/// (PQ-4(b)) rather than failing the mint — the read path accepts both suites,
/// so a degraded entry is always openable. Mirrors the mail seal gate
/// (`bins/fauna-nest/src/bridge_routing_handlers.rs` `seal_and_persist_local`).
pub fn create_key_blob_entry_auto(
    subscriber_id: &ActorId,
    subscriber_mlkem_ek: Option<&[u8; MLKEM768_ENCAPS_KEY_LEN]>,
    period_key: &[u8; 32],
) -> KeyBlobEntry {
    if let Some(ek) = subscriber_mlkem_ek {
        match create_key_blob_entry_xwing(subscriber_id, ek, period_key) {
            Ok(entry) => return entry,
            Err(_) => {
                // PQ-4(b): malformed published ek → degrade to classical.
                tracing::warn!(
                    subscriber = %subscriber_id.to_hex(),
                    "X-Wing KeyBlob wrap failed; degrading to classical",
                );
            }
        }
    }
    create_key_blob_entry(subscriber_id, period_key)
}

/// Decrypt a key blob entry, dispatching on its self-describing `suite`. This
/// is the read-path entry point that opens **both** suites — classical entries
/// via [`decrypt_key_blob_entry`], X-Wing entries via the hybrid opener — so a
/// mixed-suite roster (some subscribers published an ek, some did not) reads
/// uniformly. Returns the 32-byte period key.
pub fn decrypt_key_blob_entry_for(
    subscriber: &ActorKeypair,
    entry: &KeyBlobEntry,
) -> Result<[u8; 32], DecryptError> {
    match entry.suite {
        KemSuiteId::Classical => decrypt_key_blob_entry(subscriber, &entry.encrypted_key),
        KemSuiteId::Xwing => decrypt_key_blob_entry_xwing(subscriber, &entry.encrypted_key),
        KemSuiteId::Unknown => Err(DecryptError::UnknownSuite),
    }
}

/// Decrypt an X-Wing key blob entry using the subscriber's keypair.
///
/// Re-derives the subscriber's X-Wing secret from the identity seed, decapsulates
/// (infallible — a wrong key yields a different shared secret the AEAD rejects),
/// and opens the ChaCha20-Poly1305 wrap. Input format in `encrypted_key`:
/// 1120-byte X-Wing ciphertext ++ 12-byte nonce ++ ciphertext ++ 16-byte tag.
fn decrypt_key_blob_entry_xwing(
    subscriber: &ActorKeypair,
    encrypted_key: &[u8],
) -> Result<[u8; 32], DecryptError> {
    // 1120 (X-Wing ct) + 12 (nonce) + 32 (wrapped key) + 16 (tag).
    if encrypted_key.len() < XWING_CIPHERTEXT_LEN + 12 + 32 + 16 {
        return Err(DecryptError::TooShort);
    }

    let ct_bytes: [u8; XWING_CIPHERTEXT_LEN] = encrypted_key[..XWING_CIPHERTEXT_LEN]
        .try_into()
        .expect("length checked above");
    let xwing_ct = XWingCiphertext::from_bytes(ct_bytes);

    let nonce_bytes = &encrypted_key[XWING_CIPHERTEXT_LEN..XWING_CIPHERTEXT_LEN + 12];
    let ciphertext = &encrypted_key[XWING_CIPHERTEXT_LEN + 12..];

    let keypair = derive_subscriber_xwing_keypair(subscriber);
    let shared_secret = Zeroizing::new(fauna_pq_kem::decapsulate(&keypair.secret, &xwing_ct));
    let derived_key = blake3::derive_key("fauna.keyblob.xwing.v1", shared_secret.as_slice());

    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new((&derived_key).into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| DecryptError::AuthenticationFailed)?;

    if plaintext.len() != 32 {
        return Err(DecryptError::AuthenticationFailed);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

/// Errors from minting a broadcast `KeyBlob` author-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// `signer`'s actor_id does not match `signer_auth.device_key`.
    SignerMismatch,
    /// `signer_auth` does not grant `ManageSubscribers` (or `All`).
    MissingCapability,
    /// `signer_auth`'s embedded signature does not verify against `actor_id`.
    InvalidAuth,
    /// The KeyBlob signing operation itself failed (should not happen in
    /// practice; surfaces an internal encoding/sign error).
    SignFailed(String),
}

impl std::fmt::Display for MintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignerMismatch => {
                write!(f, "signer keypair does not match device_auth.device_key")
            }
            Self::MissingCapability => {
                write!(f, "device_auth does not grant ManageSubscribers")
            }
            Self::InvalidAuth => write!(f, "device_auth signature does not verify"),
            Self::SignFailed(s) => write!(f, "sign KeyBlob: {s}"),
        }
    }
}

impl std::error::Error for MintError {}

/// A minted, signed broadcast `KeyBlob` ready for the encrypted-mode upload
/// wire shape. Carries the canonical bytes + envelope pair that ships in
/// `EncryptedKeyBlobUpload.key_blob` (an `EmbedAsBytes`).
#[derive(Debug, Clone)]
pub struct MintedKeyBlob {
    /// The assembled `KeyBlob` value (no signature field — sign-over-CID).
    pub blob: KeyBlob,
    /// Canonical dag-cbor bytes the publisher signed.
    pub bytes: Vec<u8>,
    /// The envelope binding `bytes` to the signer's pubkey.
    pub envelope: fauna_cbor::SignedEnvelope,
}

/// Mint and sign a broadcast `KeyBlob` on the author's client.
///
/// Wraps `wrapped_key` (the broadcast period key for ordinary tier rotation,
/// or the final MLS epoch secret for archival blobs on MLS→broadcast
/// transition) once per subscriber via [`create_key_blob_entry_auto`],
/// assembles the [`KeyBlob`], and signs it with `signer`. The result is
/// structurally identical to a nest-minted blob and verifies under
/// [`verify_key_blob_signature`] with the same `signer_auth`.
///
/// **Per-subscriber suite selection (surface B, slice S4b).** `subscriber_eks`
/// runs parallel to `subscribers` (entry `i` is subscriber `i`'s published
/// 1184-byte ML-KEM encapsulation key, or `None` if they published none). When
/// a subscriber published an ek, that subscriber's
/// entry is X-Wing (ML-KEM-768 ∥ X25519); otherwise it degrades to classical —
/// the same non-erroring degrade [`create_key_blob_entry_auto`] applies, so a
/// **mixed-suite roster** (some published, some did not) mints uniformly. A
/// short/empty `subscriber_eks` reads as `None` for the uncovered subscribers
/// (a classical caller passes `&[]`).
///
/// In encrypted storage mode the nest holds no period key, so broadcast
/// `KeyBlob` minting and rotation move to the author's client; in plaintext
/// mode the nest mints server-side via its own helpers. `signer_auth` must
/// already be signed by the author and grant `Capability::ManageSubscribers`
/// (or `Capability::All`) to `signer`. Both conditions are checked
/// pre-mint — calling this with malformed state returns the matching
/// [`MintError`] rather than producing an unverifiable blob.
///
/// `signer_auth_bytes` + `signer_auth_env` are the embed-as-bytes payload
/// for the device authorization; the function re-verifies them before
/// minting to keep the existing pre-flight invariants.
// clippy: the parameters are distinct crypto inputs (signer, its device
// authorization in three forms, tier, rotation time, subscriber set + their
// published eks, wrapped key); a params struct would relabel
// them, not clarify the call.
#[allow(clippy::too_many_arguments)]
pub fn mint_key_blob(
    signer: &ActorKeypair,
    signer_auth: &DeviceAuthorization,
    signer_auth_bytes: &[u8],
    signer_auth_env: &fauna_cbor::SignedEnvelope,
    tier: String,
    rotated_at: Timestamp,
    subscribers: &[ActorId],
    subscriber_eks: &[Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>],
    wrapped_key: &[u8; 32],
) -> Result<MintedKeyBlob, MintError> {
    if signer_auth.device_key != signer.actor_id().0 {
        return Err(MintError::SignerMismatch);
    }
    let has_capability = signer_auth
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::ManageSubscribers | Capability::All));
    if !has_capability {
        return Err(MintError::MissingCapability);
    }
    verify_envelope(signer_auth, signer_auth_bytes, signer_auth_env)
        .map_err(|_| MintError::InvalidAuth)?;

    let entries: Vec<KeyBlobEntry> = subscribers
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let ek = subscriber_eks.get(i).and_then(|e| e.as_ref());
            create_key_blob_entry_auto(s, ek, wrapped_key)
        })
        .collect();

    let blob = KeyBlob {
        author: signer_auth.actor_id,
        tier,
        rotated_at,
        entries,
        signer: signer.actor_id().0,
        key_commitment: period_key_commitment(wrapped_key),
    };
    let (bytes, envelope) =
        sign_envelope(signer, &blob).map_err(|e| MintError::SignFailed(e.to_string()))?;
    Ok(MintedKeyBlob {
        blob,
        bytes,
        envelope,
    })
}

/// Parse the raw byte inputs an FFI/WASM author-side binding holds, mint a
/// broadcast `KeyBlob`, and return the minted blob's embed-as-bytes
/// `(envelope, bytes)` pair — exactly the `EncryptedKeyBlobUpload.key_blob`
/// field value the nest verifies.
///
/// This is the shared parse-and-mint core behind `fauna_ffi::mint_key_blob`
/// and `fauna_wasm::mint_key_blob`: it validates the 32-byte `signer_secret`
/// and `wrapped_key`, parses the `signer_auth` embed-as-bytes pair into a
/// [`DeviceAuthorization`], then mints via [`mint_key_blob`] and re-packages
/// the result with [`EmbedAsBytes::from_signed`]. Only the **subscriber-roster
/// marshalling** genuinely differs between the two bindings (UniFFI
/// `Vec<Vec<u8>>` vs WASM `32·N` flat bytes), so callers pass an already-parsed
/// `&[ActorId]` (and parallel `&[Option<ek>]`) and keep that platform-specific
/// decode at their own boundary.
///
/// `subscriber_eks` carries the surface-B suite selection exactly as
/// [`mint_key_blob`] documents: entry `i` is subscriber `i`'s published ML-KEM
/// ek (or `None`), and an X-Wing entry is minted only when the subscriber
/// published one (else classical — the non-erroring degrade).
///
/// Errors are human-readable strings, because both bindings ultimately surface
/// them as a platform error string (UniFFI `FfiError::General`, WASM `JsValue`).
/// For structured errors, call [`mint_key_blob`] directly with pre-parsed
/// inputs.
#[allow(clippy::too_many_arguments)]
pub fn mint_key_blob_from_bytes(
    signer_secret: &[u8],
    signer_auth_envelope: &[u8],
    signer_auth_bytes: &[u8],
    tier: String,
    rotated_at: u64,
    subscribers: &[ActorId],
    subscriber_eks: &[Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>],
    wrapped_key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let secret: [u8; 32] = signer_secret.try_into().map_err(|_| {
        format!(
            "signer_secret must be 32 bytes, got {}",
            signer_secret.len()
        )
    })?;
    let signer = ActorKeypair::from_secret(secret);

    let auth_wire = EmbedAsBytes {
        envelope: signer_auth_envelope.to_vec(),
        bytes: signer_auth_bytes.to_vec(),
        signer_auth: None,
    };
    let (auth_bytes, auth_env) = auth_wire
        .into_signed()
        .map_err(|e| format!("signer_auth envelope: {e}"))?;
    let device_auth: DeviceAuthorization =
        decode_signed_bytes(&auth_bytes).map_err(|e| format!("decode signer_auth: {e}"))?;

    let wrapped: [u8; 32] = wrapped_key
        .try_into()
        .map_err(|_| format!("wrapped_key must be 32 bytes, got {}", wrapped_key.len()))?;

    let minted = mint_key_blob(
        &signer,
        &device_auth,
        &auth_bytes,
        &auth_env,
        tier,
        Timestamp(rotated_at),
        subscribers,
        subscriber_eks,
        &wrapped,
    )
    .map_err(|e| e.to_string())?;

    let wire = EmbedAsBytes::from_signed(minted.bytes, minted.envelope);
    Ok((wire.envelope, wire.bytes))
}

/// A signed `DeviceAuthorization` in the embed-as-bytes wire shape — the
/// canonical bytes the author signed plus the envelope binding them to the
/// author's pubkey. This is the triple `mint_key_blob` consumes as
/// `signer_auth` / `signer_auth_bytes` / `signer_auth_env`, and [`Self::wire`]
/// is the form the `EncryptedKeyBlobUpload.signer_auth` field carries.
#[derive(Debug, Clone)]
pub struct SignedDeviceAuth {
    /// The assembled `DeviceAuthorization` (no signature field — sign-over-CID).
    pub auth: DeviceAuthorization,
    /// Canonical dag-cbor bytes the author signed.
    pub bytes: Vec<u8>,
    /// The envelope binding `bytes` to the author's pubkey.
    pub envelope: fauna_cbor::SignedEnvelope,
}

impl SignedDeviceAuth {
    /// The embed-as-bytes wire form for the upload envelope's `signer_auth`
    /// field (mirrors `MintedKeyBlob`'s `key_blob` packaging).
    pub fn wire(&self) -> EmbedAsBytes {
        EmbedAsBytes::from_signed(self.bytes.clone(), self.envelope)
    }
}

/// Error building the self-delegation `DeviceAuthorization`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfDelegationError {
    /// Canonical-encoding or signing the authorization failed.
    SignFailed(String),
}

impl std::fmt::Display for SelfDelegationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelfDelegationError::SignFailed(e) => write!(f, "sign self-delegation: {e}"),
        }
    }
}

impl std::error::Error for SelfDelegationError {}

/// Build + sign the single-device self-delegation `DeviceAuthorization`
/// granting `ManageSubscribers` that the encrypted-mode broadcast-`KeyBlob`
/// mint's auth chain requires.
///
/// In encrypted storage mode the author's own client mints the `KeyBlob`, so it
/// both *is* the author and *acts as* the authorized device — the
/// "single-device case, self-signed delegation" the 2026-05-15 broadcast-KeyBlob
/// spec § Auth chain step 3 describes. `signer` is the author's keypair; the
/// resulting authorization names `signer` as **both** `actor_id` and
/// `device_key`, carries exactly `[ManageSubscribers]`, and is signed by
/// `signer`. It does not expire (the device *is* the author). The returned
/// triple feeds [`mint_key_blob`]'s `signer_auth` / `signer_auth_bytes` /
/// `signer_auth_env`; it verifies under [`verify_key_blob_signature`] for the
/// blob `signer` mints with it.
pub fn build_manage_subscribers_self_delegation(
    signer: &ActorKeypair,
    created_at: Timestamp,
) -> Result<SignedDeviceAuth, SelfDelegationError> {
    let auth = DeviceAuthorization {
        actor_id: signer.actor_id(),
        device_key: signer.actor_id().0,
        capabilities: vec![Capability::ManageSubscribers],
        created_at,
        expires_at: None,
    };
    let (bytes, envelope) =
        sign_envelope(signer, &auth).map_err(|e| SelfDelegationError::SignFailed(e.to_string()))?;
    Ok(SignedDeviceAuth {
        auth,
        bytes,
        envelope,
    })
}

/// Verify a key blob's signature by checking the full delegation chain:
/// 1. The key blob's author matches the device authorization's actor_id.
/// 2. The key blob's signer matches the device authorization's device_key.
/// 3. The device authorization includes ManageSubscribers or All capability.
/// 4. The device authorization signature is valid (signed by the author).
/// 5. The key blob signature is valid (signed by the nest/device).
///
/// Both `key_blob` and `device_auth` ride the embed-as-bytes wire shape —
/// callers pass the `(bytes, envelope)` pair for each.
pub fn verify_key_blob_signature(
    key_blob: &KeyBlob,
    key_blob_bytes: &[u8],
    key_blob_env: &fauna_cbor::SignedEnvelope,
    device_auth: &DeviceAuthorization,
    device_auth_bytes: &[u8],
    device_auth_env: &fauna_cbor::SignedEnvelope,
) -> Result<bool, Box<dyn std::error::Error>> {
    if key_blob.author != device_auth.actor_id {
        return Ok(false);
    }
    if key_blob.signer != device_auth.device_key {
        return Ok(false);
    }
    let has_capability = device_auth
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::ManageSubscribers | Capability::All));
    if !has_capability {
        return Ok(false);
    }
    if verify_envelope(device_auth, device_auth_bytes, device_auth_env).is_err() {
        return Ok(false);
    }
    if verify_envelope(key_blob, key_blob_bytes, key_blob_env).is_err() {
        return Ok(false);
    }
    Ok(true)
}
