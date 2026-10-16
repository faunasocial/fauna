//! The enqueue door's submitter proof (`mail-multidomain.md` § From: header
//! ownership → *Per door, nest-side*, the enqueue door's submitter).
//!
//! The MTA names the actor it submits for in `on_behalf_of_actor`; the nest
//! holds the message's `From:` to that actor (`mail_from_ownership`). Taken on
//! the MTA's word, a compromised MTA could name whichever owner it wanted. So
//! a `BridgeMta` caller also hands over the `SubmissionToken` SMTP AUTH
//! unwrapped — the plaintext the user's primary client Ed25519-signed over
//! `(actor_id, credential_id, issued_at, expires_at, send policy)`, whose
//! `actor_id` IS the verifying key — and the nest accepts the named submitter
//! only when all of these hold:
//!
//! 1. the token's `actor_id` is `on_behalf_of_actor`;
//! 2. its `user_sig` verifies against that actor (the shared strict primitive,
//!    `fauna_core::identity::verify_detached`, which refuses a weak key);
//! 3. it has not expired;
//! 4. the `(actor, credential_id)` wrapped token is still on file — revoking
//!    the credential in the app ends the token's authority here too;
//!
//! and the call stays inside the token's own recipient bound. Any failure is
//! [`SUBMITTER_NOT_PROVEN_CODE`]. What this cannot buy (a per-message proof;
//! the MDA's iMIP door, whose AUTH blob carries no user signature) is the goal
//! doc's accepted residual.

use std::sync::Arc;

use fauna_mls::wrapped_blob::{SIGNATURE_LEN, SubmissionToken};
use fauna_protocol::RpcError;
use fauna_protocol::bridge_routing::SUBMITTER_NOT_PROVEN_CODE;
use serde_bytes::ByteBuf;

use crate::routes::AppState;

fn not_proven(why: &str) -> RpcError {
    RpcError::new(
        SUBMITTER_NOT_PROVEN_CODE,
        "error.bridges.submitter_not_proven",
    )
    .with_details_text(format!(
        "submission token does not prove the submitter: {why}"
    ))
}

/// Accept `submitter` as the actor a `BridgeMta` enqueue of `recipients`
/// recipients is made for, on the strength of `token_bytes` — the canonical
/// plaintext of the user-signed `SubmissionToken` — or refuse with
/// [`SUBMITTER_NOT_PROVEN_CODE`]. The expiry compares against the outbound
/// clock seam (`AppState::outbound_now`, epoch seconds, the token's unit).
pub async fn prove_submitter(
    state: &Arc<AppState>,
    submitter: &[u8; 32],
    token_bytes: &[u8],
    recipients: usize,
) -> Result<(), RpcError> {
    let token = SubmissionToken::from_canonical_bytes(token_bytes)
        .map_err(|_| not_proven("the token does not decode"))?;
    if token.actor_id.as_slice() != submitter.as_slice() {
        return Err(not_proven("the token names another actor"));
    }
    let mut placeholder = token.clone();
    placeholder.user_sig = ByteBuf::from(vec![0u8; SIGNATURE_LEN]);
    let signed = placeholder
        .to_canonical_bytes()
        .map_err(crate::rpc_errors::internal)?;
    if !fauna_core::identity::verify_detached(submitter, &signed, &token.user_sig) {
        return Err(not_proven(
            "the signature does not verify against the actor",
        ));
    }
    let now = u64::try_from(state.outbound_now()).unwrap_or(0);
    if now >= token.expires_at {
        return Err(not_proven("the token has expired"));
    }
    if recipients > token.max_recipients as usize {
        return Err(not_proven("more recipients than the token allows"));
    }
    if state
        .db
        .get_wrapped_submission_token(submitter, &token.credential_id)
        .await
        .map_err(crate::rpc_errors::internal)?
        .is_none()
    {
        return Err(not_proven("the token's credential is no longer on file"));
    }
    Ok(())
}
