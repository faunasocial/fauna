//! `RecoveryClient` — one typed method per `fauna.recovery.*` kind.
//!
//! This is the transport surface only: encode, call, decode, map the refusal.
//! The multi-call ceremonies that compose these (kit creation, escrow restore,
//! the replacement window, succession) live in the sibling modules, so a client
//! that needs a single call is not forced through a ceremony and a ceremony is
//! never re-assembled per app (priority #2).
//!
//! Every method that carries a signed record moves it as **canonical DAG-CBOR
//! bytes**, never a re-encoded nested map — the embed-as-bytes rule for signed
//! payloads (`transport.md` § SignedEnvelope and embed-as-bytes). The nest
//! stores and replays those bytes verbatim, so this crate must hand it the same
//! bytes it signed.

use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorId;
use fauna_core::recovery::{ChainHead, SignedIdentitySuccession, SignedRecoveryKeyRegistration};
use fauna_mls::wrapped_blob::SeedEscrowBlob;
use fauna_protocol::recovery as wire;
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

use crate::error::{RecoveryError, Result};

/// Kind names, as shared constants rather than literals at call sites.
pub mod kinds {
    pub use fauna_client_core::recovery_chain::{
        REGISTRATION_CHAIN_KIND as REGISTRATION_CHAIN,
        REGISTRATION_SUBMIT_KIND as REGISTRATION_SUBMIT,
    };
    pub const ESCROW_PUT: &str = "fauna.recovery.escrow.put";
    pub const ESCROW_CHALLENGE: &str = "fauna.recovery.escrow.challenge";
    pub const ESCROW_STATUS: &str = "fauna.recovery.escrow.status";
    pub const ESCROW_FETCH: &str = "fauna.recovery.escrow.fetch";
    pub const REPLACEMENT_REQUEST: &str = "fauna.recovery.replacement.request";
    pub const REPLACEMENT_CHALLENGE: &str = "fauna.recovery.replacement.challenge";
    pub const REPLACEMENT_VETO: &str = "fauna.recovery.replacement.veto";
    pub const REPLACEMENT_STATUS: &str = "fauna.recovery.replacement.status";
    pub const SUCCESSION_SUBMIT: &str = "fauna.recovery.succession.submit";
    pub const SUCCESSION_LOOKUP: &str = "fauna.recovery.succession.lookup";
}

/// A single-use challenge nonce a recovery kind issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The 32-byte nonce to sign with the RecoveryKey.
    pub nonce: [u8; 32],
    /// Unix seconds the nonce stops being accepted.
    pub expires_at: u64,
}

/// Typed calls into one nest's recovery plane.
///
/// **Which connection to use is the caller's choice, and it matters.** Four of
/// these kinds are *pre-identity* (`registration.chain`, both `escrow`
/// challenge/fetch, both `replacement` challenge/veto, and both `succession`
/// kinds) precisely because the scenarios they serve have no working session —
/// a thief can revoke every session and invoke the seed-signed lockout
/// (`identity-succession.md:66`). Driving those over an authenticated
/// connection would reproduce the lock-out this plane exists to escape, so the
/// recovery-entry and succession surfaces construct this over an **anonymous**
/// connector. The USER-class kinds (`registration.submit`, `escrow.put`,
/// `replacement.{request,status}`) need the signed-in connection.
pub struct RecoveryClient<R> {
    nest: R,
}

impl<R> RecoveryClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// The underlying transport, for a caller that also drives other planes.
    pub fn transport(&self) -> &R {
        &self.nest
    }
}

impl<R> RecoveryClient<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    // ── Registration chain ──────────────────────────────────────────────────

    /// Register (or replace) the authenticated actor's RecoveryKey. USER class.
    ///
    /// Returns the `seq` now at the head of the chain.
    pub async fn submit_registration(
        &self,
        registration: &SignedRecoveryKeyRegistration,
    ) -> Result<u64> {
        let bytes = canonical_encode(registration)
            .map_err(|e| RecoveryError::Crypto(format!("encoding the registration: {e}")))?;
        let reply: wire::RegistrationSubmitReply = self
            .call(
                kinds::REGISTRATION_SUBMIT,
                wire::RegistrationSubmitRequest {
                    registration: ByteBuf::from(bytes),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.seq)
    }

    /// Fetch an identity's whole registration chain, oldest first. Pre-identity.
    ///
    /// An identity that never registered answers **empty** rather than
    /// erroring — the honest "no succession capability" answer
    /// (`identity-succession.md:53`), so an empty vec is data, not a failure.
    pub async fn registration_chain(
        &self,
        actor_id: &ActorId,
    ) -> Result<Vec<SignedRecoveryKeyRegistration>> {
        let reply: wire::RegistrationChainReply = self
            .call(
                kinds::REGISTRATION_CHAIN,
                wire::RegistrationChainRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        reply
            .registrations
            .iter()
            .map(|b| {
                canonical_decode::<SignedRecoveryKeyRegistration>(b.as_ref())
                    .map_err(|e| RecoveryError::Malformed(format!("registration record: {e}")))
            })
            .collect()
    }

    /// The head of an identity's registration chain, or `None` when it has
    /// never registered a RecoveryKey.
    ///
    /// The head is taken from the **last** entry because the nest serves the
    /// chain oldest-first; a caller that needs to sign at `seq + 1` (every
    /// replacement does) reads it from here rather than counting entries, so a
    /// gap in `seq` can never be mistaken for a length.
    pub async fn chain_head(&self, actor_id: &ActorId) -> Result<Option<ChainHead>> {
        let chain = self.registration_chain(actor_id).await?;
        Ok(chain
            .last()
            .map(|last| ChainHead::new(last.registration.recovery_pubkey, last.registration.seq)))
    }

    // ── Seed escrow ─────────────────────────────────────────────────────────

    /// Store the opaque seed-escrow blob. USER class.
    ///
    /// Returns the unix seconds the nest recorded it at — the client's
    /// confirmation that the *new* value landed rather than an earlier one
    /// being kept.
    pub async fn escrow_put(&self, blob: &SeedEscrowBlob) -> Result<i64> {
        let bytes = canonical_encode(blob)
            .map_err(|e| RecoveryError::Crypto(format!("encoding the escrow blob: {e}")))?;
        let reply: wire::EscrowPutReply = self
            .call(
                kinds::ESCROW_PUT,
                wire::EscrowPutRequest {
                    blob: ByteBuf::from(bytes),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.updated_at)
    }

    /// Whether an escrow blob rests for the authenticated actor, and when it
    /// was written. USER class.
    ///
    /// The one escrow call a signed-in device can make: `fetch` is gated on a
    /// RecoveryKey signature the device does not hold (offline-only custody),
    /// so without this the "registered but no blob rests" state — a real
    /// loss-protection gap — would be invisible to the only surface that can
    /// repair it. Answers presence, never bytes.
    pub async fn escrow_status(&self) -> Result<Option<i64>> {
        let reply: wire::EscrowStatusReply = self
            .call(kinds::ESCROW_STATUS, wire::EscrowStatusRequest::default())
            .await?;
        // `updated_at` is absent only from a non-conforming nest (the current
        // handler sets it iff present); treat a
        // `present` answer with no stamp as present-at-unknown-time rather than
        // as absent, so the status never regresses to a false gap warning.
        Ok(reply.present.then(|| reply.updated_at.unwrap_or_default()))
    }

    /// Ask for a nonce to sign with the RecoveryKey. Pre-identity.
    ///
    /// Issued **unconditionally**, so it reveals nothing about whether the
    /// account exists or holds a blob.
    pub async fn escrow_challenge(&self, actor_id: &ActorId) -> Result<Challenge> {
        let reply: wire::EscrowChallengeReply = self
            .call(
                kinds::ESCROW_CHALLENGE,
                wire::EscrowChallengeRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(Challenge {
            nonce: nonce32(&reply.nonce)?,
            expires_at: reply.expires_at,
        })
    }

    /// Redeem a signed challenge for the escrow blob. Pre-identity.
    pub async fn escrow_fetch(
        &self,
        actor_id: &ActorId,
        nonce: &[u8; 32],
        signature: &[u8],
    ) -> Result<SeedEscrowBlob> {
        let reply: wire::EscrowFetchReply = self
            .call(
                kinds::ESCROW_FETCH,
                wire::EscrowFetchRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    nonce: ByteBuf::from(nonce.to_vec()),
                    signature: ByteBuf::from(signature.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        canonical_decode::<SeedEscrowBlob>(reply.blob.as_ref())
            .map_err(|e| RecoveryError::Malformed(format!("escrow blob: {e}")))
    }

    // ── Seed-initiated replacement (the 30-day window) ──────────────────────

    /// Park a seed-alone replacement record. USER class.
    ///
    /// Returns the unix seconds it lands if uncontested.
    pub async fn replacement_request(
        &self,
        registration: &SignedRecoveryKeyRegistration,
    ) -> Result<i64> {
        let bytes = canonical_encode(registration)
            .map_err(|e| RecoveryError::Crypto(format!("encoding the registration: {e}")))?;
        let reply: wire::ReplacementRequestReply = self
            .call(
                kinds::REPLACEMENT_REQUEST,
                wire::ReplacementRequestRequest {
                    registration: ByteBuf::from(bytes),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.lands_at)
    }

    /// The authenticated actor's own pending replacement, if any. USER class.
    ///
    /// This is the read the standing whole-window banner derives from
    /// (`identity-succession.md:43` — loud on every device for the whole
    /// window), separate from the one-shot notification.
    pub async fn replacement_status(&self) -> Result<Option<wire::ReplacementPendingInfo>> {
        let reply: wire::ReplacementStatusReply = self
            .call(
                kinds::REPLACEMENT_STATUS,
                wire::ReplacementStatusRequest::default(),
            )
            .await?;
        Ok(reply.pending)
    }

    /// Ask for a nonce to sign a veto. Pre-identity — the veto scenario leaves
    /// the real owner holding only the recovery phrase.
    pub async fn replacement_challenge(&self, actor_id: &ActorId) -> Result<Challenge> {
        let reply: wire::ReplacementChallengeReply = self
            .call(
                kinds::REPLACEMENT_CHALLENGE,
                wire::ReplacementChallengeRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(Challenge {
            nonce: nonce32(&reply.nonce)?,
            expires_at: reply.expires_at,
        })
    }

    /// Cancel whatever replacement currently pends. Pre-identity.
    ///
    /// `false` means nothing was pending — an idempotent success, not a
    /// failure: the vetoer's goal state holds either way.
    pub async fn replacement_veto(
        &self,
        actor_id: &ActorId,
        nonce: &[u8; 32],
        signature: &[u8],
    ) -> Result<bool> {
        let reply: wire::ReplacementVetoReply = self
            .call(
                kinds::REPLACEMENT_VETO,
                wire::ReplacementVetoRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    nonce: ByteBuf::from(nonce.to_vec()),
                    signature: ByteBuf::from(signature.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.cancelled)
    }

    // ── Succession ──────────────────────────────────────────────────────────

    /// Submit a succession statement. **Pre-identity** — see the type docs.
    ///
    /// Returns the successor actor id the account now belongs to and the unix
    /// seconds the nest applied it.
    pub async fn submit_succession(
        &self,
        statement: &SignedIdentitySuccession,
    ) -> Result<(ActorId, i64)> {
        let bytes = canonical_encode(statement)
            .map_err(|e| RecoveryError::Crypto(format!("encoding the statement: {e}")))?;
        let reply: wire::SuccessionSubmitReply = self
            .call(
                kinds::SUCCESSION_SUBMIT,
                wire::SuccessionSubmitRequest {
                    statement: ByteBuf::from(bytes),
                    ..Default::default()
                },
            )
            .await?;
        Ok((ActorId(nonce32(&reply.new_actor_id)?), reply.succeeded_at))
    }

    /// "Has this identity been succeeded, and by whom?" Pre-identity.
    ///
    /// Returns the whole `old → terminal` path oldest-first, empty when the
    /// identity was never succeeded. The caller **verifies** each link
    /// (`crate::succession::verify_chain`); a reply is a hint to check, never
    /// an authorization (`identity-succession.md:63`).
    pub async fn succession_lookup(
        &self,
        actor_id: &ActorId,
    ) -> Result<Vec<SignedIdentitySuccession>> {
        let reply: wire::SuccessionLookupReply = self
            .call(
                kinds::SUCCESSION_LOOKUP,
                wire::SuccessionLookupRequest {
                    actor_id: ByteBuf::from(actor_id.0.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        reply
            .statements
            .iter()
            .map(|b| {
                canonical_decode::<SignedIdentitySuccession>(b.as_ref())
                    .map_err(|e| RecoveryError::Malformed(format!("succession statement: {e}")))
            })
            .collect()
    }

    // ── internals ───────────────────────────────────────────────────────────

    async fn call<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.nest.request(kind, payload).await.map_err(|e| {
            let mapped = RecoveryError::from_transport(e);
            // Log on the event, not the paint: which leg of a multi-call
            // ceremony failed is otherwise unrecoverable from a UI banner.
            tracing::debug!(kind, error = %mapped, "recovery kind refused");
            mapped
        })
    }
}

/// Read a wire byte string as exactly 32 bytes.
///
/// Length is checked here rather than trusted, because every caller feeds the
/// result into a signature or an actor id — a short nonce silently zero-padded
/// would produce a signature over something the nest never issued.
fn nonce32(bytes: &ByteBuf) -> Result<[u8; 32]> {
    bytes
        .as_ref()
        .try_into()
        .map_err(|_| RecoveryError::Malformed(format!("expected 32 bytes, got {}", bytes.len())))
}
