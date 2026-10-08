//! Canonical dag-cbor / BARE encoding, content hashing, and cryptographic
//! signing helpers.
//!
//! Every in-tree signed Fauna kind — Profile, Post, DeliveryReceipt,
//! ContactRequest, DeviceAuthorization, Tombstone, KeyBlob, and
//! (since the CBOR-DAG-everywhere Layer 6 at-rest migration) ShareToken — uses
//! the sign-over-CID `Signed` path: the signature covers the value's canonical
//! dag-cbor CID and ships in a [`fauna_cbor::SignedEnvelope`] alongside the
//! signed bytes (embed-as-bytes), never as a field on the value itself. The
//! legacy `Signable` sign-over-bytes trait was removed once ShareToken — its
//! last consumer — migrated.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::data::{
    Capability, ContactRequest, ContentHash, DeliveryReceipt, DeviceAuthorization, Post, PostId,
    Profile, Timestamp, Tombstone,
};
use crate::error::{Error, Result};
use crate::identity::{ActorId, ActorKeypair};
use crate::subscription::types::KeyBlob;

/// A type that can be signed-over-CID via [`fauna_cbor::SignedEnvelope`].
///
/// The signature is not a field on the value — it ships separately in the
/// envelope. The value need only expose its signer's public key.
pub trait Signed: Serialize + DeserializeOwned + Clone {
    fn signer_public_key(&self) -> &[u8; 32];
}

/// Sign `value` over its canonical-dag-cbor CID via [`fauna_cbor::SignedEnvelope`].
///
/// Returns `(canonical_bytes, envelope)`. The bytes ARE the signed encoding —
/// store and broadcast them as-is; recompute the CID from the bytes on
/// receive.
///
/// (The bare `sign`/`verify` names freed up when the last `Signable` consumer,
/// `ShareToken`, migrated in Layer 6 are intentionally NOT reclaimed: the
/// `_envelope` suffix is the clearer name across the ~185 call sites in a
/// crypto-heavy tree. A pure rename, if ever wanted, is its own change.)
pub fn sign_envelope<T: Signed>(
    keypair: &ActorKeypair,
    value: &T,
) -> Result<(Vec<u8>, fauna_cbor::SignedEnvelope)> {
    fauna_cbor::SignedEnvelope::sign(value, keypair.signing_key())
        .map_err(|e| Error::Encoding(e.to_string()))
}

/// Verify a [`fauna_cbor::SignedEnvelope`] against `bytes` under `pk`, through
/// the tree's single strict verification primitive.
///
/// Splits into two independent checks per the Layer 1 contract:
/// 1. BLAKE3(bytes) == envelope.cid.multihash
/// 2. Ed25519_verify(envelope.sig, envelope.cid.bytes, pk)
///
/// ⚠ Step 2 deliberately does **not** call [`fauna_cbor::SignedEnvelope::verify_permissive`]:
/// that method runs `ed25519_dalek`'s permissive `Verifier`, and at both of this
/// module's call sites the key is read off the wire beside the signature
/// (`Signed::signer_public_key`, or a cert's `device_key`), so it is
/// attacker-chosen. For a small-order key an all-zero signature satisfies the
/// permissive equation on ~a quarter of messages — measured 63/256 at this very
/// door, `PROBE-381-A` in `tests/sign_over_cid.rs` — which would let a payload
/// claim authorship under an identity nobody holds a key to. `security.md`
/// § Key material and signature verification therefore requires `verify_strict`
/// and a weak-key refusal for this shape, and
/// [`crate::identity::verify_detached`] is that one primitive.
fn verify_envelope_under(
    pk: &[u8; 32],
    bytes: &[u8],
    env: &fauna_cbor::SignedEnvelope,
) -> Result<()> {
    if !env.cid().matches(bytes) {
        return Err(Error::Encoding(format!(
            "{:?}",
            fauna_cbor::VerifyError::CidMismatch
        )));
    }
    if !crate::identity::verify_detached(pk, env.cid().as_bytes(), env.sig()) {
        return Err(Error::Encoding(format!(
            "{:?}",
            fauna_cbor::VerifyError::SignatureInvalid
        )));
    }
    Ok(())
}

/// Verify a [`fauna_cbor::SignedEnvelope`] against `bytes`, reading the
/// expected public key from `value.signer_public_key()`.
///
/// Splits into two independent checks per the Layer 1 contract — see
/// [`verify_envelope_under`], which also carries why the strict primitive is
/// load-bearing here rather than a nicety.
///
/// (See [`sign_envelope`] on why the bare `verify` name is not reclaimed.)
pub fn verify_envelope<T: Signed>(
    value: &T,
    bytes: &[u8],
    env: &fauna_cbor::SignedEnvelope,
) -> Result<()> {
    verify_envelope_under(value.signer_public_key(), bytes, env)
}

/// How an authored payload's signature chain resolved — see
/// [`verify_authoring_envelope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoringOrigin {
    /// The envelope verifies under the author's own identity key.
    Direct,
    /// The envelope verifies under a delegated sub-key, authorized by a valid
    /// identity-signed `DeviceAuthorization` (`cert.device_key` carried here).
    Delegated { device_key: [u8; 32] },
}

/// Verify an *authored* kind (`Post`, `Tombstone`, `Profile` — kinds whose
/// [`Signed`] impl returns the **author** key) accepting either the author's
/// own signature or a delegated sub-key's, per
/// `docs/goal/architecture/serialization.md` § Delegated authoring.
///
/// The chain, mirroring `subscription::crypto::verify_key_blob_signature`:
///
/// 1. The envelope verifies under the author key → [`AuthoringOrigin::Direct`]
///    (an attached cert is ignored). Otherwise:
/// 2. `signer_auth` is present and the cert's own envelope verifies under
///    `cert.actor_id` (the cert is itself sign-over-CID).
/// 3. `cert.actor_id` == the value's author key.
/// 4. `required` (or [`Capability::All`]) ∈ `cert.capabilities`.
/// 5. `cert.expires_at`, when present, ≥ `created_at` (the value's own
///    signer-asserted creation instant — the caller passes it; the accepted
///    residual is documented at `atproto-pds-full.md` D10 § Revocation).
/// 6. The envelope verifies under `cert.device_key`.
///
/// **Fail-closed by construction:** because authored kinds' `Signed` impls
/// keep returning the author key, a call site running only plain
/// [`verify_envelope`] *rejects* a delegated payload — it can never accept
/// one without this chain. Call sites opt in explicitly.
pub fn verify_authoring_envelope<T: Signed>(
    value: &T,
    bytes: &[u8],
    env: &fauna_cbor::SignedEnvelope,
    signer_auth: Option<&EmbedAsBytes>,
    required: &Capability,
    created_at: Timestamp,
) -> Result<AuthoringOrigin> {
    // Step 1 — direct authoring.
    if verify_envelope(value, bytes, env).is_ok() {
        return Ok(AuthoringOrigin::Direct);
    }
    let Some(auth_wire) = signer_auth else {
        return Err(Error::Encoding(
            "signature is not the author's and no signer_auth cert accompanies it".into(),
        ));
    };
    // Steps 2–5 — the one delegation-cert chain.
    let cert = verify_delegation_cert(auth_wire, value.signer_public_key(), required, created_at)?;
    // Step 6 — the envelope verifies under the delegated key, strictly: the
    // cert names `device_key` on the wire, so it is as attacker-chosen as the
    // author key in step 1 (PROBE-381-B pins the refusal).
    verify_envelope_under(&cert.device_key, bytes, env)
        .map_err(|_| Error::Encoding("signature does not verify under the delegated key".into()))?;
    Ok(AuthoringOrigin::Delegated {
        device_key: cert.device_key,
    })
}

/// Steps 2–5 of [`verify_authoring_envelope`]'s chain, as the one shared
/// delegation-cert check every delegated-signature verifier composes: the cert
/// (an embed-as-bytes `DeviceAuthorization`) verifies under its own
/// `actor_id`, that `actor_id` is `author`, the cert grants `required` (or
/// [`Capability::All`] — [`Capability::grants`]), and it had not expired at
/// `created_at`. Returns the decoded cert; the caller then verifies its own
/// signature under `cert.device_key` (step 6 — the signed-message shape is the
/// caller's: a sign-over-CID envelope here, a domain-tagged statement for a
/// change record — `fauna_protocol::sync_writer_sig`).
///
/// Always over the cert's **carried** bytes, never a re-encoding: the
/// capability list decodes open-set, so a re-encode is not guaranteed to be the
/// signed form.
pub fn verify_delegation_cert(
    auth_wire: &EmbedAsBytes,
    author: &[u8; 32],
    required: &Capability,
    created_at: Timestamp,
) -> Result<DeviceAuthorization> {
    // Step 2 — the cert itself verifies under its own actor_id.
    let (auth_bytes, auth_env) = auth_wire.clone().into_signed()?;
    let cert: DeviceAuthorization = decode_signed_bytes(&auth_bytes)?;
    verify_envelope(&cert, &auth_bytes, &auth_env)
        .map_err(|_| Error::Encoding("signer_auth cert signature invalid".into()))?;
    // Step 3 — the cert's grantor is this value's author.
    if cert.actor_id.0 != *author {
        return Err(Error::Encoding(
            "signer_auth cert actor_id does not match the payload author".into(),
        ));
    }
    // Step 4 — the cert grants the required capability.
    if !cert.capabilities.iter().any(|c| c.grants(required)) {
        return Err(Error::Encoding(
            "signer_auth cert does not grant the required capability".into(),
        ));
    }
    // Step 5 — expiry against the value's own creation instant.
    if let Some(expires_at) = cert.expires_at
        && expires_at.0 < created_at.0
    {
        return Err(Error::Encoding(
            "signer_auth cert expired before the payload's created_at".into(),
        ));
    }
    Ok(cert)
}

/// A verified same-account peer admission — what the `DeviceAuthorization`
/// witness proves about a channel-proven peer key. Consumed by the peer-leg
/// admission seam (`account-data-plane.md` § The peer leg → *The admission
/// seam*), which turns it into a per-connection verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAdmission {
    /// The account whose root signed the witness.
    pub actor_id: ActorId,
    /// The admitted device key — equal to the channel-proven peer key by
    /// construction (checked here, never assumed).
    pub device_key: [u8; 32],
    /// The witness's expiry, when it carries one. The verdict's validity bound
    /// is `min(connection lifetime, this)`; a connection outliving it must
    /// re-present before continuing past it (the seam's rule).
    pub expires_at: Option<Timestamp>,
}

/// Verify a `DeviceAuthorization` **admission witness** for the same-account
/// peer leg (`account-data-plane.md` § The peer leg → *The admission seam*).
///
/// This is the `DeviceAuthorization` witness verifier the seam names — it
/// lives here because the cert's mechanics live here (beside
/// [`verify_authoring_envelope`], which checks the same cert for the
/// *authoring* axis). The seam's rules, in order:
///
/// 1. The cert's own envelope verifies under `cert.actor_id` — root-signed,
///    self-contained (carriage is inline; no registry lookup).
/// 2. **A witness admits a proven key, never a bearer**: `cert.device_key`
///    must equal `proven_key`, the channel-proven identity
///    (`PeerConn::peer_identity`). Possession of the envelope alone conveys
///    nothing.
/// 3. The cert's grantor is the expected account (same-account admission).
/// 4. The witness has not expired at `now`. Expiry here bounds the verdict;
///    the *authoring* twin checks expiry against the payload's `created_at`
///    instead — different axes, deliberately different instants.
///
/// **`cert.capabilities` is deliberately not consulted** — admission is
/// carrier-level, never writer-level (the seam's ruling: a
/// `[RenewBearer]`-only sync-agent device is an admitted carrier; row-level
/// writer auth stays with T14's AAD + the journal's accounting).
pub fn verify_device_admission_witness(
    witness: &EmbedAsBytes,
    proven_key: &[u8; 32],
    expected_account: &ActorId,
    now: Timestamp,
) -> Result<DeviceAdmission> {
    // Step 1 — the cert verifies under its own actor_id.
    let (cert_bytes, cert_env) = witness.clone().into_signed()?;
    let cert: DeviceAuthorization = decode_signed_bytes(&cert_bytes)?;
    verify_envelope(&cert, &cert_bytes, &cert_env)
        .map_err(|_| Error::Encoding("admission witness signature invalid".into()))?;
    // Step 2 — the witness admits the channel-proven key, never a bearer.
    if cert.device_key != *proven_key {
        return Err(Error::Encoding(
            "admission witness names a device key that is not the channel-proven peer".into(),
        ));
    }
    // Step 3 — same-account admission.
    if cert.actor_id != *expected_account {
        return Err(Error::Encoding(
            "admission witness is signed by a different account".into(),
        ));
    }
    // Step 4 — validity bound.
    if let Some(expires_at) = cert.expires_at
        && expires_at.0 < now.0
    {
        return Err(Error::Encoding("admission witness has expired".into()));
    }
    Ok(DeviceAdmission {
        actor_id: cert.actor_id,
        device_key: cert.device_key,
        expires_at: cert.expires_at,
    })
}

/// The embed-as-bytes wire shape for signed payloads.
///
/// Per `docs/goal/architecture/serialization.md` § Embed-as-bytes and
/// `docs/goal/architecture/transport.md` § Embed-as-bytes for signed
/// payloads: every signed Fauna payload travels on the wire / persists at
/// rest as `{envelope, bytes}`, where:
///
/// * `envelope` is the fixed 100-byte serialization (36-byte CID || 64-byte
///   Ed25519 signature) of a [`fauna_cbor::SignedEnvelope`], and
/// * `bytes` is the raw canonical dag-cbor of the inner kind.
///
/// Both fields ride as CBOR byte strings (major type 2) via `serde_bytes`,
/// so the canonical dag-cbor wire shape is the spec's `{bytes, envelope}`
/// (canonical key order is length-first: `bytes` before `envelope`).
///
/// Receivers run the two-step verification (BLAKE3 on `bytes` against the
/// CID's multihash + Ed25519 over the CID with the signer's pubkey) before
/// decoding `bytes` — see [`verify_envelope`] for the Rust helper.
///
/// The envelope is intentionally a flat 100-byte buffer rather than a
/// nested map: `SignedEnvelope` itself isn't `Serialize` (its Cid + sig are
/// raw byte arrays, not serde structs), and the cross-language fixtures in
/// `libs/fauna-cbor/tests/cross_language_interop.rs` already use the
/// `[36-byte CID || 64-byte sig]` layout. Keeping the envelope a flat buffer
/// avoids a translation step at the Go bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedAsBytes {
    /// 100 bytes: 36-byte CID || 64-byte Ed25519 signature.
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Canonical-encoded bytes of the inner kind (the bytes the publisher
    /// signed — receivers re-hash these to verify the CID).
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// Delegated-authoring cert (`docs/goal/architecture/serialization.md`
    /// § Delegated authoring): the embed-as-bytes of the identity-signed
    /// `DeviceAuthorization` that authorizes this payload's signer when the
    /// signer is not the author. Absent (the overwhelmingly common case) the
    /// wire is byte-identical to the historical two-field shape, and a reader
    /// that predates the field fail-closed-rejects a delegated payload at
    /// signature verify — see [`verify_authoring_envelope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_auth: Option<Box<EmbedAsBytes>>,
}

impl EmbedAsBytes {
    /// Build the wire shape from a freshly-signed `(bytes, env)` pair.
    pub fn from_signed(bytes: Vec<u8>, env: fauna_cbor::SignedEnvelope) -> Self {
        let mut envelope = Vec::with_capacity(100);
        envelope.extend_from_slice(env.cid().as_bytes());
        envelope.extend_from_slice(env.sig());
        Self {
            envelope,
            bytes,
            signer_auth: None,
        }
    }

    /// Attach a delegated-authoring cert (the `DeviceAuthorization`'s own
    /// embed-as-bytes) to this wire — the delegated-signing counterpart of
    /// [`Self::from_signed`].
    pub fn with_signer_auth(mut self, signer_auth: EmbedAsBytes) -> Self {
        self.signer_auth = Some(Box::new(signer_auth));
        self
    }

    /// Split into the inner canonical bytes and the reconstructed envelope.
    /// Returns `Err` if `envelope.len() != 100` or the CID prefix is wrong.
    pub fn into_signed(self) -> Result<(Vec<u8>, fauna_cbor::SignedEnvelope)> {
        if self.envelope.len() != 100 {
            return Err(Error::Encoding(format!(
                "embed-as-bytes envelope must be 100 bytes (36 CID + 64 sig), got {}",
                self.envelope.len()
            )));
        }
        let mut cid_arr = [0u8; 36];
        cid_arr.copy_from_slice(&self.envelope[0..36]);
        let cid = fauna_cbor::Cid::from_bytes(cid_arr)
            .map_err(|e| Error::Encoding(format!("envelope CID prefix: {e:?}")))?;
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&self.envelope[36..100]);
        let env = fauna_cbor::SignedEnvelope::from_parts(cid, sig_arr);
        Ok((self.bytes, env))
    }
}

/// Longest prefix of `s` that is `≤ max` bytes and ends on a UTF-8 char
/// boundary — for capping a string to a fixed on-disk/wire byte budget
/// without splitting a multi-byte character. Never panics: `end` only ever
/// walks down from `max` toward 0, and byte offset 0 is always a boundary.
pub fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// [`truncate_to_char_boundary`] plus a trailing `"..."` when truncation
/// actually happens — the ellipsis's 3 bytes count against `max` (so the
/// result stays within `max` bytes for any `max >= 3`; below that there is no
/// room left for content and the ellipsis alone (3 bytes) is returned as-is).
/// For a fixed on-disk/wire byte budget that also wants a visible "this was
/// cut" marker (e.g. a summary field).
pub fn truncate_to_char_boundary_with_ellipsis(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}...", truncate_to_char_boundary(s, max.saturating_sub(3)))
}

/// Decode canonical dag-cbor bytes into a `Signed` kind. Receivers use this
/// AFTER [`verify_envelope`] succeeds, per the third-party verification
/// recipe in `docs/goal/architecture/serialization.md` § Sign-over-CID.
///
/// Delegates to [`fauna_cbor::decode_strict`] (which runs the pre-parse
/// canonical-form validator before serde decode).
pub fn decode_signed_bytes<T: Signed>(bytes: &[u8]) -> Result<T> {
    fauna_cbor::decode_strict(bytes).map_err(|e| Error::Encoding(format!("decode_strict: {e:?}")))
}

/// Sign + wrap-as-embed-as-bytes + canonical-dag-cbor-encode in one call. The
/// result is the at-rest / wire shape callers send for `fauna.posts.create`
/// and equivalent kinds.
///
/// Convenience over the explicit `sign_envelope` + `EmbedAsBytes::from_signed` +
/// `encode_canonical` chain that test fixtures and client builders would
/// otherwise repeat.
pub fn sign_and_pack<T: Signed>(keypair: &ActorKeypair, value: &T) -> Result<Vec<u8>> {
    let (bytes, env) = sign_envelope(keypair, value)?;
    let wire = EmbedAsBytes::from_signed(bytes, env);
    canonical_encode(&wire)
}

/// Base64url-decode + verify + decode a signed capability token: the mirror
/// of [`sign_and_pack`]'s encode side, base64url-wrapped for URL-segment
/// transport. Base64url (no-padding) decode -> canonical-dag-cbor decode into
/// [`EmbedAsBytes`] -> split into `(inner, envelope)` -> strict-decode `T` from
/// `inner` -> [`verify_envelope`]. Callers cannot hold an unverified `T`.
///
/// The shared form of the sign-over-CID capability-token decode that
/// `ShareToken::from_base64url` and the web-content paywall token's `verify`
/// both hand-rolled identically.
pub fn decode_and_verify_base64url<T: Signed>(s: &str) -> Result<T> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| Error::Encoding(format!("base64url decode: {e}")))?;
    let wire: EmbedAsBytes = canonical_decode(&bytes)?;
    let (inner, env) = wire.into_signed()?;
    let token: T = decode_signed_bytes(&inner)?;
    verify_envelope(&token, &inner, &env)?;
    Ok(token)
}

/// Decode a stored / served `Profile` content-row payload into a typed
/// [`Profile`] — the one shared verify-then-decode used by **both**
/// `fauna-client-profile::decode_profile` (the `fauna.profile.get` read) **and**
/// the nest's `activitypub::actor_routes` federation serve, so the two never
/// drift in how they verify a profile (priority #2; `docs/goal/ui/profile.md`
/// § Where logic lives → *Profile publish/edit* + § Encryption at rest).
///
/// **Signed-only**: decodes the signed [`EmbedAsBytes`] wire — the inner
/// `Profile` with its Ed25519 signature **verified** against the profile's own
/// `actor_id` (the verify-on-receipt rule) — and refuses anything else. The
/// `fauna.profile.set` publish path only ever stores the signed wire; the bare
/// canonical `Profile` fallback that once covered pre-publish-path rows went
/// with the compat-remnant sweep (`version-compatibility.md` § Dimension 2,
/// program 4), so no host can serve an unsigned profile as one.
///
/// Accepts a **delegated** authoring signature as well as the owner's own, via
/// [`verify_authoring_envelope`] (`atproto-pds-full.md` D10 + F2.3): an
/// external-app profile edit arriving through the PDS write path is signed by
/// the account's server-held authoring sub-key, with the identity-signed
/// delegation cert riding in the wire's `signer_auth`. The required capability
/// is [`Capability::UpdateProfile`] — deliberately *not* `Post`, so a cert
/// minted for posting alone cannot rewrite the account's profile. Step 5's
/// operand is the profile's own `updated_at`, the field a `Profile` carries in
/// place of a `created_at`.
///
/// This is the one profile verify face **all** consumers share — the
/// `fauna.profile.get` read on all 7 apps, the federation/ActivityPub actor
/// serve, and the ATProto projection's `translate_profile_bytes_for_projection`
/// — so upgrading it here is what makes a delegated profile readable
/// everywhere at once, with no second door.
///
/// Returns `(Profile, origin)`. The `origin` is the D10 audit answer
/// (`atproto-pds-full.md` § D10 → *Audit*, ratified 2026-07-29): whether the
/// account's own identity key or a delegated authoring sub-key made this edit.
/// Consumers with no audit surface bind it as `_`.
pub fn decode_profile(body: &[u8]) -> Result<(Profile, AuthoringOrigin)> {
    // Signed embed-as-bytes wire (the one stored shape; verify-on-receipt).
    let wire = canonical_decode::<EmbedAsBytes>(body)?;
    let signer_auth = wire.signer_auth.clone();
    let (bytes, env) = wire.into_signed()?;
    let profile = decode_signed_bytes::<Profile>(&bytes)?;
    let origin = verify_authoring_envelope(
        &profile,
        &bytes,
        &env,
        signer_auth.as_deref(),
        &Capability::UpdateProfile,
        profile.updated_at,
    )?;
    Ok((profile, origin))
}

/// Decode + verify a `fauna.posts.delete` payload into a typed [`Tombstone`]
/// — the one shared verify-then-decode for the self-service post-deletion
/// wire (`feed.md` § State & data shape → *Post deletion*). **Signed-only**:
/// unlike [`decode_profile`] there is deliberately no bare fallback — the
/// surface is new, so every payload is the signed [`EmbedAsBytes`] wire,
/// verified on receipt against the tombstone's own `author`
/// (verify-on-receipt; a caller still enforces its own authz on top —
/// signature validity alone never authorizes a deletion).
///
/// Accepts a **delegated** authoring signature as well as the author's own,
/// via [`verify_authoring_envelope`] (`atproto-pds-full.md` D10): an
/// external-app-authored delete may be signed by the account's server-held
/// authoring sub-key when the identity-signed delegation cert rides in the
/// wire's `signer_auth`. `Capability::Post` authorizes the whole post
/// create/delete pair, so a tombstone verifies under the same capability a
/// post does. Fail-closed by construction: a reader that predates this chain
/// rejects a delegated tombstone at plain signature verify.
pub fn decode_tombstone(body: &[u8]) -> Result<Tombstone> {
    let wire = canonical_decode::<EmbedAsBytes>(body)?;
    let signer_auth = wire.signer_auth.clone();
    let (bytes, env) = wire.into_signed()?;
    let tombstone = decode_signed_bytes::<Tombstone>(&bytes)?;
    verify_authoring_envelope(
        &tombstone,
        &bytes,
        &env,
        signer_auth.as_deref(),
        &Capability::Post,
        tombstone.created_at,
    )?;
    Ok(tombstone)
}

impl Signed for Post {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.author.0
    }
}

impl Signed for Profile {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.actor_id.0
    }
}

impl Signed for Tombstone {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.author.0
    }
}

impl Signed for ContactRequest {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.sender.0
    }
}

impl Signed for DeliveryReceipt {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.recipient.0
    }
}

impl Signed for KeyBlob {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.signer
    }
}

impl Signed for DeviceAuthorization {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.actor_id.0
    }
}

/// Canonical dag-cbor encode — the single at-rest/wire serializer (the
/// Layer-6 CBOR-DAG-everywhere migration retired the earlier per-domain BARE
/// `bare_encode` path entirely). Delegates to [`fauna_cbor::encode_canonical`]
/// and maps the error into the `fauna_core` shape.
pub fn canonical_encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fauna_cbor::encode_canonical(value).map_err(|e| Error::Encoding(e.to_string()))
}

/// The **merge tiebreak key** for a value — a total order two replicas are
/// guaranteed to compute identically, with no local preference in it.
///
/// A CRDT merge cannot prefer "ours": each replica would answer the tie
/// differently and the two would never compare equal again
/// (every
/// merge in this crate that has a tie to break routes through here, so the rule
/// is one rule). Which side a tie picks is arbitrary — *identical everywhere* is
/// the only property required — so the key is the BLAKE3 of the value's
/// canonical encoding rather than the encoding itself.
///
/// **Hashing is what keeps key material out of the comparison.** The values
/// carrying ties include subtrees full of irrecoverable secrets (subscription
/// period keys, folder content keys, mail and app-credential secrets), each
/// held in a zeroizing newtype precisely so its plaintext does not outlive its
/// use. Sorting on the encoding would strand that plaintext in a `Vec` nobody
/// zeroizes — and in a *cached* sort key, one per element. The transient buffer
/// here is `Zeroizing`, and the 32-byte digest that escapes carries none of it;
/// it is also a cheaper sort key than a whole re-encoded record.
///
/// Encoding cannot fail for anything reachable from a merge — every value
/// either decoded from this very shape or was built from typed fields — and the
/// all-zero key on that impossible arm keeps the comparison total rather than
/// panicking inside a merge, which is the worse failure.
pub(crate) fn canonical_tiebreak_key<T: Serialize>(value: &T) -> [u8; 32] {
    let Ok(bytes) = canonical_encode(value).map(zeroize::Zeroizing::new) else {
        return [0u8; 32];
    };
    *blake3::hash(&bytes).as_bytes()
}

/// Strict canonical dag-cbor decode — the counterpart to [`canonical_encode`].
/// Runs the pre-parse canonical-form validator before serde decode (via
/// [`fauna_cbor::decode_strict`]).
pub fn canonical_decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    fauna_cbor::decode_strict(bytes).map_err(|e| Error::Encoding(format!("{e:?}")))
}

/// Compute the BLAKE3 content hash of arbitrary bytes.
pub fn content_hash(bytes: &[u8]) -> ContentHash {
    let hash = blake3::hash(bytes);
    ContentHash::from_digest_raw(*hash.as_bytes())
}

/// Compute the PostId — the [`fauna_cbor::Cid`] of the canonical dag-cbor
/// encoding of the `Post` value.
///
/// Task 2.8 of the CBOR-DAG-everywhere Layer 2 plan collapsed `PostId`
/// into `fauna_cbor::Cid`. The hash domain is canonical dag-cbor
/// (`encode_canonical`), so the PostId is byte-for-byte the CID that
/// [`sign_envelope`] signs for the same Post (`SignedEnvelope::sign` hashes
/// the same canonical bytes) — see `docs/goal/architecture/serialization.md`
/// § Sign-over-CID.
pub fn compute_post_id(post: &Post) -> Result<PostId> {
    let bytes = canonical_encode(post)?;
    Ok(fauna_cbor::Cid::of_dag_cbor(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ContactRequest, DeliveryReceipt, PostBody, Timestamp};

    // ── verify_device_admission_witness ──────────────────────────────────

    /// A root-signed `DeviceAuthorization` for `device_key`, as the
    /// embed-as-bytes witness the admission exchange carries.
    fn admission_witness(
        root: &ActorKeypair,
        device_key: [u8; 32],
        expires_at: Option<Timestamp>,
    ) -> EmbedAsBytes {
        let cert = DeviceAuthorization {
            actor_id: root.actor_id(),
            device_key,
            // Deliberately narrow — admission must not consult capabilities
            // (carrier-level), which the accepted-carrier test pins.
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at,
        };
        let (bytes, env) = sign_envelope(root, &cert).expect("sign cert");
        EmbedAsBytes::from_signed(bytes, env)
    }

    #[test]
    fn admission_witness_admits_the_proven_key() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let device = [0xD1u8; 32];
        let witness = admission_witness(&root, device, Some(Timestamp(5_000)));

        let admission =
            verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_000))
                .expect("witness admits");
        assert_eq!(admission.actor_id, root.actor_id());
        assert_eq!(admission.device_key, device);
        assert_eq!(admission.expires_at, Some(Timestamp(5_000)));
    }

    /// The seam's rule 2: a witness admits a proven key, never a bearer — a
    /// valid envelope presented from a *different* channel-proven key conveys
    /// nothing.
    #[test]
    fn admission_witness_rejects_a_bearer_with_someone_elses_cert() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let witness = admission_witness(&root, [0xD1u8; 32], None);

        let err = verify_device_admission_witness(
            &witness,
            &[0xD2u8; 32], // the channel proved a different key
            &root.actor_id(),
            Timestamp(2_000),
        )
        .expect_err("bearer must be refused");
        assert!(err.to_string().contains("channel-proven"), "{err}");
    }

    /// Same-account admission: a cert signed by a different account's root is
    /// refused even when the device key matches.
    #[test]
    fn admission_witness_rejects_a_foreign_account() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let other = ActorKeypair::from_secret([10u8; 32]);
        let device = [0xD1u8; 32];
        let witness = admission_witness(&other, device, None);

        let err =
            verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_000))
                .expect_err("foreign account must be refused");
        assert!(err.to_string().contains("different account"), "{err}");
    }

    /// Validity bound: an expired witness is refused; `expires_at == now` is
    /// still valid (the bound is "past", not "at").
    #[test]
    fn admission_witness_rejects_expired_and_accepts_at_the_bound() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let device = [0xD1u8; 32];
        let witness = admission_witness(&root, device, Some(Timestamp(2_000)));

        verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_000))
            .expect("at the bound is still valid");
        let err =
            verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_001))
                .expect_err("past expiry must be refused");
        assert!(err.to_string().contains("expired"), "{err}");
    }

    /// A tampered envelope (signature over different bytes) is refused before
    /// any field is trusted.
    #[test]
    fn admission_witness_rejects_a_tampered_cert() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let device = [0xD1u8; 32];
        let mut witness = admission_witness(&root, device, None);
        // Flip one byte of the signed cert bytes — the CID check must fail.
        let last = witness.bytes.len() - 1;
        witness.bytes[last] ^= 0x01;

        verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_000))
            .expect_err("tampered witness must be refused");
    }

    /// The carrier-level ruling, pinned: a `[RenewBearer]`-only device — no
    /// authoring capability at all — is an admitted carrier. (The fixture
    /// above grants only `RenewBearer`, so every green test here is also this
    /// assertion; this test exists to name the rule so a future "check
    /// capabilities" edit meets a red test, not just a comment.)
    #[test]
    fn admission_is_carrier_level_a_renewbearer_only_device_is_admitted() {
        let root = ActorKeypair::from_secret([9u8; 32]);
        let device = [0xD1u8; 32];
        let witness = admission_witness(&root, device, None);
        verify_device_admission_witness(&witness, &device, &root.actor_id(), Timestamp(2_000))
            .expect("a RenewBearer-only device is an admitted carrier");
    }

    // ── truncate_to_char_boundary ────────────────────────────────────────

    #[test]
    fn truncate_to_char_boundary_under_budget_is_unchanged() {
        assert_eq!(truncate_to_char_boundary("hello", 10), "hello");
        assert_eq!(truncate_to_char_boundary("hello", 5), "hello");
    }

    #[test]
    fn truncate_to_char_boundary_ascii_cuts_exactly() {
        assert_eq!(truncate_to_char_boundary("hello world", 5), "hello");
    }

    #[test]
    fn truncate_to_char_boundary_never_splits_a_multibyte_char() {
        // "é" is 2 bytes (0xC3 0xA9); a cap landing mid-character must back off
        // to the last full character rather than producing invalid UTF-8.
        let s = "café";
        assert_eq!(s.len(), 5); // c-a-f-é(2 bytes)
        assert_eq!(truncate_to_char_boundary(s, 4), "caf");
    }

    #[test]
    fn truncate_to_char_boundary_zero_max_yields_empty() {
        assert_eq!(truncate_to_char_boundary("hello", 0), "");
    }

    // ── truncate_to_char_boundary_with_ellipsis ──────────────────────────

    #[test]
    fn truncate_with_ellipsis_under_budget_is_unchanged_no_dots() {
        assert_eq!(
            truncate_to_char_boundary_with_ellipsis("hello", 10),
            "hello"
        );
        assert_eq!(truncate_to_char_boundary_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_with_ellipsis_over_budget_cuts_and_appends_dots() {
        assert_eq!(
            truncate_to_char_boundary_with_ellipsis("hello world", 8),
            "hello..."
        );
    }

    #[test]
    fn truncate_with_ellipsis_never_splits_a_multibyte_char() {
        let s = "café latte"; // "é" is 2 bytes
        assert_eq!(truncate_to_char_boundary_with_ellipsis(s, 6), "caf...");
    }

    #[test]
    fn truncate_with_ellipsis_tiny_budget_yields_just_the_dots() {
        // max <= 3 leaves no room for any content byte — the ellipsis alone
        // (3 bytes) is returned even though it exceeds `max`, matching the
        // pre-dedup `fauna_client_core::truncate` behavior exactly (pinned
        // below in `agrees_with_the_original_char_indices_walk_across_edge_cases`).
        assert_eq!(truncate_to_char_boundary_with_ellipsis("hello", 0), "...");
        assert_eq!(truncate_to_char_boundary_with_ellipsis("hello", 3), "...");
    }

    #[test]
    fn truncate_with_ellipsis_result_within_budget_for_max_at_or_above_three() {
        let s = "hello world";
        for max in 3..s.len() {
            assert!(truncate_to_char_boundary_with_ellipsis(s, max).len() <= max);
        }
    }

    /// Pins behavior-preservation against `fauna_client_core::truncate`'s own
    /// (pre-dedup) algorithm — char_indices-based, `take_while(i <=
    /// max.saturating_sub(3)).last()` — across the same edge cases an earlier
    /// `url_host`/`url_host_opt` agreement test used as its template.
    #[test]
    fn agrees_with_the_original_char_indices_walk_across_edge_cases() {
        fn original_truncate(s: &str, max: usize) -> String {
            if s.len() <= max {
                s.to_string()
            } else {
                let end = s
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= max.saturating_sub(3))
                    .last()
                    .unwrap_or(0);
                format!("{}...", &s[..end])
            }
        }
        for s in [
            "hello world",
            "café latte",
            "",
            "a",
            "日本語のテスト文字列です",
            "exactly-forty-bytes-long-string-here!!!",
        ] {
            for max in 0..=s.len() + 5 {
                assert_eq!(
                    truncate_to_char_boundary_with_ellipsis(s, max),
                    original_truncate(s, max),
                    "mismatch for s={s:?} max={max}"
                );
            }
        }
    }

    #[test]
    fn sign_and_verify_post() {
        let kp = ActorKeypair::generate();
        let mut post = Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "hello fauna".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(&kp, &post).unwrap();
        verify_envelope(&post, &bytes, &env).unwrap();

        // Tamper with content — produces different canonical bytes, so the
        // CID no longer matches and verification fails.
        if let PostBody::Text {
            ref mut content, ..
        } = post.body
        {
            *content = "tampered".into();
        }
        let (tampered_bytes, _) = sign_envelope(&kp, &post).unwrap();
        assert!(verify_envelope(&post, &tampered_bytes, &env).is_err());
    }

    #[test]
    fn post_id_deterministic() {
        let kp = ActorKeypair::generate();
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "deterministic".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let id1 = compute_post_id(&post).unwrap();
        let id2 = compute_post_id(&post).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn canonical_roundtrip() {
        let ts = Timestamp(42);
        let bytes = canonical_encode(&ts).unwrap();
        let decoded: Timestamp = canonical_decode(&bytes).unwrap();
        assert_eq!(ts, decoded);
    }

    fn sample_post(kp: &ActorKeypair) -> Post {
        Post {
            author: kp.actor_id(),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "layer-6 at-rest dag-cbor".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    /// Layer 6: `compute_post_id` must hash the *canonical dag-cbor*
    /// of the Post, so the PostId equals the CID the sign-over-CID envelope
    /// signs. Before the Layer-6 at-rest flip `compute_post_id` hashed BARE
    /// bytes while `sign_envelope` signed the dag-cbor CID — the two diverged.
    #[test]
    fn compute_post_id_equals_signed_envelope_cid() {
        let kp = ActorKeypair::generate();
        let post = sample_post(&kp);
        let pid = compute_post_id(&post).unwrap();
        let (_bytes, env) = sign_envelope(&kp, &post).unwrap();
        assert_eq!(
            pid,
            *env.cid(),
            "compute_post_id must equal the signed-envelope CID (Layer 6)"
        );
    }

    /// Layer 6: the embed-as-bytes wire shape produced by `sign_and_pack`
    /// must be strict canonical dag-cbor (decodable by `decode_strict`), not
    /// BARE. The inner signed bytes were already dag-cbor; this pins the outer
    /// `EmbedAsBytes` wrapper.
    #[test]
    fn sign_and_pack_wire_is_canonical_dag_cbor() {
        let kp = ActorKeypair::generate();
        let post = sample_post(&kp);
        let wire_bytes = sign_and_pack(&kp, &post).unwrap();
        let wire: EmbedAsBytes = fauna_cbor::decode_strict(&wire_bytes)
            .expect("packed wire must be strict canonical dag-cbor");
        let (inner, env) = wire.into_signed().unwrap();
        verify_envelope(&post, &inner, &env).expect("envelope verifies over inner bytes");
        let decoded: Post = decode_signed_bytes(&inner).unwrap();
        assert_eq!(decoded.author, post.author);
        match decoded.body {
            PostBody::Text { content, .. } => assert_eq!(content, "layer-6 at-rest dag-cbor"),
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn sign_and_verify_delivery_receipt() {
        let kp = ActorKeypair::generate();
        let post_id = fauna_cbor::Cid::of_dag_cbor(b"delivery-receipt-post-9");

        let receipt = DeliveryReceipt {
            post_id,
            recipient: kp.actor_id(),
            received_at: Timestamp::now(),
        };
        let (bytes, env) = sign_envelope(&kp, &receipt).unwrap();
        verify_envelope(&receipt, &bytes, &env).unwrap();

        // Tamper with post_id — different CID, verification fails.
        let mut tampered = receipt.clone();
        tampered.post_id = fauna_cbor::Cid::of_dag_cbor(b"delivery-receipt-tampered");
        let (tampered_bytes, _) = sign_envelope(&kp, &tampered).unwrap();
        assert!(verify_envelope(&receipt, &tampered_bytes, &env).is_err());
    }

    #[test]
    fn sign_and_verify_contact_request() {
        let kp = ActorKeypair::generate();
        let post_id = fauna_cbor::Cid::of_dag_cbor(b"contact-request-post-7");

        let cr = ContactRequest {
            sender: kp.actor_id(),
            post_id,
            sender_node: b"http://localhost:3000".to_vec(),
            summary: "test".into(),
            created_at: Timestamp::now(),
        };
        let (bytes, env) = sign_envelope(&kp, &cr).unwrap();
        verify_envelope(&cr, &bytes, &env).unwrap();

        // Tamper with sender — wrong pubkey, verification fails.
        let mut tampered = cr.clone();
        tampered.sender.0[0] ^= 0xff;
        assert!(verify_envelope(&tampered, &bytes, &env).is_err());
    }

    // ── decode_profile (shared signed-only verify) ──────

    fn sample_profile(actor: [u8; 32]) -> Profile {
        use crate::data::InboxMode;
        use crate::identity::ActorId;
        Profile {
            actor_id: ActorId(actor),
            display_name: Some("Alice".into()),
            bio: Some("hi".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        }
    }

    #[test]
    fn profile_recovery_head_round_trips_signed() {
        use crate::recovery::ChainHead;
        let kp = ActorKeypair::generate();
        let mut profile = sample_profile(kp.actor_id().0);
        profile.recovery_head = Some(ChainHead::new([0xA7; 32], 4));

        let body = sign_and_pack(&kp, &profile).expect("sign+pack");
        let (decoded, _) = decode_profile(&body).expect("decodes + verifies");
        assert_eq!(decoded.recovery_head, Some(ChainHead::new([0xA7; 32], 4)));
    }

    #[test]
    fn bytes_carrying_the_retired_pubkey_only_key_decode_with_no_head() {
        // The pubkey-only `recovery_pubkey` field was REPLACED by the coupled
        // `recovery_head` before any producer shipped (a pubkey the
        // consumer holds no `seq` for cannot anchor the chain
        // rewrite/truncation guard, so mirroring it alone was unconsumable).
        // Bytes carrying the retired key — none should exist, but the decode
        // rule is load-bearing either way — yield `None`, i.e. "no binding",
        // never a partial one.
        use crate::data::{AccountLoadHint, AdminNestEntry, InboxMode, NestEntry, ProfileLink};
        use crate::identity::ActorId;

        #[derive(Serialize)]
        struct RetiredShapeProfile {
            actor_id: ActorId,
            display_name: Option<String>,
            bio: Option<String>,
            avatar: Option<ContentHash>,
            banner: Option<ContentHash>,
            links: Vec<ProfileLink>,
            nests: Vec<NestEntry>,
            admin_nests: Vec<AdminNestEntry>,
            load_hint: Option<AccountLoadHint>,
            inbox_mode: InboxMode,
            #[serde(default, with = "serde_bytes")]
            recovery_pubkey: Option<[u8; 32]>,
            updated_at: Timestamp,
        }

        let retired = RetiredShapeProfile {
            actor_id: ActorId([7u8; 32]),
            display_name: None,
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_pubkey: Some([0xA7; 32]),
            updated_at: Timestamp(0),
        };
        let bytes = canonical_encode(&retired).expect("encode retired shape");
        let decoded: Profile = canonical_decode(&bytes).expect("still decodes");
        assert_eq!(
            decoded.recovery_head, None,
            "a retired pubkey-only key must decode as NO binding, never a partial one"
        );
    }

    #[test]
    fn a_keyless_profile_encodes_byte_identically_to_the_pre_field_shape() {
        // The additive-compat pin: `skip_serializing_if` means a profile with
        // no registered RecoveryKey produces the exact bytes an old client
        // produces (no `recovery_head` key, never `null`), and old bytes
        // decode with the field defaulting to `None`. The mirror struct below
        // IS the pre-field `Profile` shape — keep its serde attrs in sync with
        // everything except the new field.
        use crate::data::{AccountLoadHint, AdminNestEntry, InboxMode, NestEntry, ProfileLink};
        use crate::identity::ActorId;

        #[derive(Serialize, Deserialize)]
        struct PreFieldProfile {
            actor_id: ActorId,
            display_name: Option<String>,
            bio: Option<String>,
            avatar: Option<ContentHash>,
            banner: Option<ContentHash>,
            links: Vec<ProfileLink>,
            nests: Vec<NestEntry>,
            #[serde(default)]
            admin_nests: Vec<AdminNestEntry>,
            load_hint: Option<AccountLoadHint>,
            inbox_mode: InboxMode,
            updated_at: Timestamp,
        }

        let new_shape = sample_profile([7u8; 32]);
        assert_eq!(new_shape.recovery_head, None);
        let old_shape = PreFieldProfile {
            actor_id: ActorId([7u8; 32]),
            display_name: Some("Alice".into()),
            bio: Some("hi".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            updated_at: Timestamp(0),
        };

        let new_bytes = canonical_encode(&new_shape).expect("encode new");
        let old_bytes = canonical_encode(&old_shape).expect("encode old");
        assert_eq!(
            new_bytes, old_bytes,
            "a key-less profile must stay byte-identical to the pre-field encoding"
        );

        let decoded: Profile = canonical_decode(&old_bytes).expect("old bytes decode");
        assert_eq!(decoded.recovery_head, None);
    }

    #[test]
    fn decode_profile_accepts_signed_embed_as_bytes_and_verifies() {
        let kp = ActorKeypair::generate();
        let profile = sample_profile(kp.actor_id().0);
        // The canonical stored shape: signed embed-as-bytes wire.
        let body = sign_and_pack(&kp, &profile).expect("sign+pack");
        let (decoded, origin) = decode_profile(&body).expect("decodes + verifies");
        assert_eq!(origin, AuthoringOrigin::Direct);
        // Profile has no PartialEq; compare the load-bearing fields.
        assert_eq!(decoded.actor_id.0, profile.actor_id.0);
        assert_eq!(decoded.display_name, profile.display_name);
        assert_eq!(decoded.bio, profile.bio);
    }

    #[test]
    fn decode_profile_refuses_a_bare_canonical_profile() {
        // The unsigned bare shape is any host's fabrication; no writer stores
        // it (the compat-remnant sweep removed the fallback that read it).
        let profile = sample_profile([7u8; 32]);
        let body = canonical_encode(&profile).expect("encode");
        assert!(
            decode_profile(&body).is_err(),
            "an unsigned bare profile must be refused, never decoded"
        );
    }

    #[test]
    fn decode_profile_rejects_signature_from_a_different_actor() {
        // A profile whose actor_id does NOT match the signing key must fail
        // verify (the verify-on-receipt guard): sign someone else's profile.
        let signer = ActorKeypair::generate();
        let other = ActorKeypair::generate();
        let profile = sample_profile(other.actor_id().0); // actor_id != signer
        let body = sign_and_pack(&signer, &profile).expect("sign+pack");
        assert!(
            decode_profile(&body).is_err(),
            "a profile signed by a non-matching actor must not verify"
        );
    }

    fn sample_tombstone(kp: &ActorKeypair) -> crate::data::Tombstone {
        crate::data::Tombstone {
            author: kp.actor_id(),
            post_id: crate::data::PostId::from_digest_dag_cbor([7u8; 32]),
            created_at: crate::data::Timestamp(1_000_000),
        }
    }

    #[test]
    fn decode_tombstone_round_trips_signed_wire() {
        let kp = ActorKeypair::generate();
        let tombstone = sample_tombstone(&kp);
        let wire = sign_and_pack(&kp, &tombstone).expect("sign+pack");
        let decoded = decode_tombstone(&wire).expect("verifies + decodes");
        assert_eq!(decoded.author.0, kp.actor_id().0);
        assert_eq!(decoded.post_id, tombstone.post_id);
    }

    #[test]
    fn decode_tombstone_rejects_bare_and_foreign_signature() {
        let kp = ActorKeypair::generate();
        let tombstone = sample_tombstone(&kp);
        // Bare canonical Tombstone: signed-only surface, no bare fallback.
        let bare = canonical_encode(&tombstone).expect("encode");
        assert!(
            decode_tombstone(&bare).is_err(),
            "a bare unsigned tombstone must not decode"
        );
        // Signed by a key that is not the tombstone's author.
        let signer = ActorKeypair::generate();
        let forged = sign_and_pack(&signer, &tombstone).expect("sign+pack");
        assert!(
            decode_tombstone(&forged).is_err(),
            "a foreign-signed tombstone must not verify"
        );
    }

    // ── Delegated authoring (serialization.md § Delegated authoring) ──────

    use crate::data::Capability;

    /// Identity-sign a `[Post]`-scoped delegation cert for `device_key` and
    /// return its embed-as-bytes wire.
    fn post_cert(
        identity: &ActorKeypair,
        device_key: [u8; 32],
        capabilities: Vec<Capability>,
        expires_at: Option<Timestamp>,
    ) -> EmbedAsBytes {
        let cert = DeviceAuthorization {
            actor_id: identity.actor_id(),
            device_key,
            capabilities,
            created_at: Timestamp(1_000),
            expires_at,
        };
        let (bytes, env) = sign_envelope(identity, &cert).expect("sign cert");
        EmbedAsBytes::from_signed(bytes, env)
    }

    /// A post authored by `identity` but signed by the delegated `sub_key`.
    fn delegated_signed_post(
        identity: &ActorKeypair,
        sub_key: &ActorKeypair,
    ) -> (Post, Vec<u8>, fauna_cbor::SignedEnvelope) {
        let post = sample_post(identity);
        let (bytes, env) = sign_envelope(sub_key, &post).expect("sign post with sub-key");
        (post, bytes, env)
    }

    #[test]
    fn wire_without_signer_auth_is_byte_identical_to_two_field_shape() {
        // The historical two-field wire, encoded independently of the struct.
        #[derive(Serialize)]
        struct LegacyWire {
            #[serde(with = "serde_bytes")]
            envelope: Vec<u8>,
            #[serde(with = "serde_bytes")]
            bytes: Vec<u8>,
        }
        let kp = ActorKeypair::generate();
        let post = sample_post(&kp);
        let (bytes, env) = sign_envelope(&kp, &post).unwrap();
        let wire = EmbedAsBytes::from_signed(bytes, env);
        let legacy = LegacyWire {
            envelope: wire.envelope.clone(),
            bytes: wire.bytes.clone(),
        };
        assert_eq!(
            canonical_encode(&wire).unwrap(),
            canonical_encode(&legacy).unwrap(),
            "an absent signer_auth must leave the wire byte-identical"
        );
    }

    #[test]
    fn old_reader_tolerates_signer_auth_field() {
        // A reader whose EmbedAsBytes predates `signer_auth` (modeled by a
        // two-field struct) must still decode a delegated wire — it drops the
        // cert and then fail-closed-rejects at signature verify.
        #[derive(Serialize, Deserialize)]
        struct LegacyWire {
            #[serde(with = "serde_bytes")]
            envelope: Vec<u8>,
            #[serde(with = "serde_bytes")]
            bytes: Vec<u8>,
        }
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (_, bytes, env) = delegated_signed_post(&identity, &sub);
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Post], None);
        let wire = EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert);
        let encoded = canonical_encode(&wire).unwrap();

        let legacy: LegacyWire = canonical_decode(&encoded).expect("old reader decodes");
        // …and the old reader's plain verify rejects (author key expected).
        let (inner, env) = EmbedAsBytes {
            envelope: legacy.envelope,
            bytes: legacy.bytes,
            signer_auth: None,
        }
        .into_signed()
        .unwrap();
        let post: Post = decode_signed_bytes(&inner).unwrap();
        assert!(
            verify_envelope(&post, &inner, &env).is_err(),
            "old reader must reject the delegated post, never accept it"
        );
    }

    #[test]
    fn delegated_post_verifies_via_chain() {
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Post], None);
        let origin = verify_authoring_envelope(
            &post,
            &bytes,
            &env,
            Some(&cert),
            &Capability::Post,
            post.created_at,
        )
        .expect("delegated chain verifies");
        assert_eq!(
            origin,
            AuthoringOrigin::Delegated {
                device_key: sub.actor_id().0
            }
        );
    }

    #[test]
    fn direct_post_verifies_as_direct_with_or_without_cert() {
        let identity = ActorKeypair::generate();
        let post = sample_post(&identity);
        let (bytes, env) = sign_envelope(&identity, &post).unwrap();
        // No cert.
        let origin = verify_authoring_envelope(
            &post,
            &bytes,
            &env,
            None,
            &Capability::Post,
            post.created_at,
        )
        .unwrap();
        assert_eq!(origin, AuthoringOrigin::Direct);
        // An attached cert (even a junk-scoped one) is ignored on the direct path.
        let other = ActorKeypair::generate();
        let cert = post_cert(&other, [9u8; 32], vec![Capability::Follow], None);
        let origin = verify_authoring_envelope(
            &post,
            &bytes,
            &env,
            Some(&cert),
            &Capability::Post,
            post.created_at,
        )
        .unwrap();
        assert_eq!(origin, AuthoringOrigin::Direct);
    }

    #[test]
    fn plain_verify_envelope_rejects_delegated_post() {
        // The fail-closed pin: `Signed for Post` keeps returning the author
        // key, so an un-upgraded verify site rejects a delegated post.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        assert!(verify_envelope(&post, &bytes, &env).is_err());
    }

    #[test]
    fn delegated_chain_rejects_missing_cert() {
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                None,
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
    }

    #[test]
    fn delegated_chain_rejects_wrong_capability_and_accepts_all() {
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        // [Follow] does not grant Post authoring.
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Follow], None);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&cert),
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
        // [All] does.
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::All], None);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&cert),
                &Capability::Post,
                post.created_at
            )
            .is_ok()
        );
    }

    #[test]
    fn delegated_chain_rejects_cert_from_another_identity() {
        // Cert signed by (and naming) a different actor: the payload author
        // never authorized this sub-key.
        let identity = ActorKeypair::generate();
        let other = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        let cert = post_cert(&other, sub.actor_id().0, vec![Capability::Post], None);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&cert),
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
    }

    #[test]
    fn delegated_chain_rejects_forged_cert_naming_the_author() {
        // Cert CLAIMS actor_id = the author but is signed by an attacker: the
        // cert's own envelope check (step 2) refuses it.
        let identity = ActorKeypair::generate();
        let attacker = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        let forged_cert = DeviceAuthorization {
            actor_id: identity.actor_id(), // claims the victim
            device_key: sub.actor_id().0,
            capabilities: vec![Capability::Post],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (cert_bytes, cert_env) = sign_envelope(&attacker, &forged_cert).unwrap();
        let cert_wire = EmbedAsBytes::from_signed(cert_bytes, cert_env);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&cert_wire),
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
    }

    #[test]
    fn delegated_chain_rejects_signer_not_named_by_cert() {
        // Cert names sub-key K, but the post is signed by K2.
        let identity = ActorKeypair::generate();
        let k = ActorKeypair::generate();
        let k2 = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &k2);
        let cert = post_cert(&identity, k.actor_id().0, vec![Capability::Post], None);
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&cert),
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
    }

    #[test]
    fn delegated_chain_enforces_expiry_against_created_at() {
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (post, bytes, env) = delegated_signed_post(&identity, &sub);
        // Expired before the post's created_at → refused.
        let expired = post_cert(
            &identity,
            sub.actor_id().0,
            vec![Capability::Post],
            Some(Timestamp(post.created_at.0 - 1)),
        );
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&expired),
                &Capability::Post,
                post.created_at
            )
            .is_err()
        );
        // Still-valid expiry → accepted.
        let valid = post_cert(
            &identity,
            sub.actor_id().0,
            vec![Capability::Post],
            Some(Timestamp(post.created_at.0 + 1)),
        );
        assert!(
            verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                Some(&valid),
                &Capability::Post,
                post.created_at
            )
            .is_ok()
        );
    }

    #[test]
    fn delegated_tombstone_verifies_via_chain_under_post_capability() {
        // Capability::Post covers the create/delete pair: a delegated
        // tombstone verifies under the same cert.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let tombstone = sample_tombstone(&identity);
        let (bytes, env) = sign_envelope(&sub, &tombstone).unwrap();
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Post], None);
        let origin = verify_authoring_envelope(
            &tombstone,
            &bytes,
            &env,
            Some(&cert),
            &Capability::Post,
            tombstone.created_at,
        )
        .expect("delegated tombstone verifies");
        assert_eq!(
            origin,
            AuthoringOrigin::Delegated {
                device_key: sub.actor_id().0
            }
        );
    }

    #[test]
    fn delegated_wire_round_trips_signer_auth() {
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let (_, bytes, env) = delegated_signed_post(&identity, &sub);
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Post], None);
        let wire = EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert.clone());
        let encoded = canonical_encode(&wire).unwrap();
        let decoded: EmbedAsBytes = canonical_decode(&encoded).unwrap();
        assert_eq!(decoded.signer_auth.as_deref(), Some(&cert));
    }

    #[test]
    fn decode_tombstone_accepts_delegated_wire_and_rejects_uncertified() {
        // Site (d): the whole `decode_tombstone` path — canonical_decode →
        // capture signer_auth → into_signed → verify_authoring_envelope —
        // accepts a delegated tombstone whose cert rides in `signer_auth`.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let tombstone = sample_tombstone(&identity);
        let (bytes, env) = sign_envelope(&sub, &tombstone).expect("sign with sub-key");
        let cert = post_cert(&identity, sub.actor_id().0, vec![Capability::Post], None);
        let wire = EmbedAsBytes::from_signed(bytes.clone(), env).with_signer_auth(cert);
        let body = canonical_encode(&wire).expect("encode delegated wire");
        let decoded = decode_tombstone(&body).expect("delegated tombstone decodes");
        assert_eq!(decoded.author.0, identity.actor_id().0);

        // Fail-closed: the same sub-key signature with NO cert must not decode.
        let uncertified = EmbedAsBytes::from_signed(bytes, env);
        let body = canonical_encode(&uncertified).expect("encode uncertified wire");
        assert!(
            decode_tombstone(&body).is_err(),
            "a delegated tombstone without its cert must fail-closed-reject"
        );
    }

    // ── decode_profile's delegated arm (F2.3 — the Profile verify site) ────

    /// A profile owned by `identity` but signed by the delegated `sub_key`,
    /// as the `signer_auth`-carrying wire a delegated edit is stored as.
    fn delegated_profile_wire(
        identity: &ActorKeypair,
        sub_key: &ActorKeypair,
        capabilities: Vec<Capability>,
    ) -> Vec<u8> {
        let profile = sample_profile(identity.actor_id().0);
        let (bytes, env) = sign_envelope(sub_key, &profile).expect("sign profile with sub-key");
        let cert = post_cert(identity, sub_key.actor_id().0, capabilities, None);
        let wire = EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert);
        canonical_encode(&wire).expect("encode delegated profile wire")
    }

    #[test]
    fn decode_profile_accepts_a_delegated_wire_and_rejects_it_uncertified() {
        // The whole `decode_profile` path — canonical_decode → capture
        // signer_auth → into_signed → verify_authoring_envelope — accepts an
        // external-app profile edit whose cert rides in `signer_auth`, and
        // attributes it to the OWNER, not the sub-key.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let body = delegated_profile_wire(&identity, &sub, vec![Capability::UpdateProfile]);
        let (decoded, origin) = decode_profile(&body).expect("delegated profile decodes");
        // The D10 audit surface, profile edition: the shared read face must say
        // an external app made this edit, not merely that it verified.
        assert_eq!(
            origin,
            AuthoringOrigin::Delegated {
                device_key: sub.actor_id().0
            },
            "a validly-delegated profile edit must read as DELEGATED"
        );
        assert_eq!(decoded.actor_id.0, identity.actor_id().0);

        // Fail-closed: the same sub-key signature with NO cert must not decode
        // as a verified profile.
        let profile = sample_profile(identity.actor_id().0);
        let (bytes, env) = sign_envelope(&sub, &profile).expect("sign");
        let uncertified = canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("encode uncertified wire");
        assert!(
            decode_profile(&uncertified).is_err(),
            "a delegated profile without its cert must fail-closed-reject"
        );
    }

    #[test]
    fn decode_profile_refuses_a_post_only_cert() {
        // Capability scoping is the point of naming `UpdateProfile` here: a
        // cert minted for posting alone must not let the sub-key rewrite the
        // account's profile. `Post` covers the post create/delete pair and
        // NOTHING else.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let body = delegated_profile_wire(&identity, &sub, vec![Capability::Post]);
        assert!(
            decode_profile(&body).is_err(),
            "a Post-only cert must not authorize a profile edit"
        );

        // The control arm: the same wire with `UpdateProfile` granted decodes,
        // so the refusal above is the capability check and not some other
        // failure on the path.
        let body = delegated_profile_wire(&identity, &sub, vec![Capability::UpdateProfile]);
        assert!(decode_profile(&body).is_ok());
    }

    #[test]
    fn plain_verify_envelope_rejects_a_delegated_profile() {
        // The fail-closed pin, profile edition: `Signed for Profile` keeps
        // returning the OWNER's actor_id, so any verify site that was not
        // upgraded to the chain rejects a delegated profile rather than
        // accepting an unverified one.
        let identity = ActorKeypair::generate();
        let sub = ActorKeypair::generate();
        let profile = sample_profile(identity.actor_id().0);
        let (bytes, env) = sign_envelope(&sub, &profile).expect("sign");
        assert!(verify_envelope(&profile, &bytes, &env).is_err());
    }

    #[test]
    fn decode_profile_accepts_the_direct_shape_and_refuses_the_bare_one() {
        // A self-signed profile verifies direct; the same profile unsigned is
        // refused.
        let kp = ActorKeypair::generate();
        let profile = sample_profile(kp.actor_id().0);
        let signed = sign_and_pack(&kp, &profile).expect("sign+pack");
        let (decoded, origin) = decode_profile(&signed).expect("direct decodes");
        assert_eq!(decoded.actor_id.0, kp.actor_id().0);
        assert_eq!(origin, AuthoringOrigin::Direct);
        let bare = canonical_encode(&profile).expect("encode bare");
        assert!(decode_profile(&bare).is_err(), "the bare shape is refused");
    }
}
