//! Authentication and registration request builders.

use ed25519_dalek::Signer;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;

/// Signed registration request (for `POST /api/v1/register`).
pub struct RegisterRequest {
    pub actor_id: [u8; 32],
    pub handle: String,
    pub timestamp: u64,
    pub signature: [u8; 64],
}

/// Signed challenge-response verify request (`fauna.auth.verify`). The nonce
/// comes from `fauna.auth.challenge`; freshness is provided by the nonce, so
/// there is no timestamp. `nest_id` is the identity the signature binds — the
/// one `nest_trust::read_login_binding` proved on this connection
/// (`login.md` § Binding the nest).
pub struct ChallengeVerifyRequest {
    pub actor_id: [u8; 32],
    pub nonce: [u8; 32],
    pub nest_id: [u8; 32],
    pub signature: [u8; 64],
}

/// Signed invite-request submission
/// (for `POST /api/v1/invite-requests`). Signs handle + message inline to
/// bind them to the actor_id, with a timestamp for replay protection.
pub struct InviteRequestSubmit {
    pub actor_id: [u8; 32],
    pub handle: String,
    pub message: String,
    pub timestamp: u64,
    pub signature: [u8; 64],
}

/// Signed invite-request cancellation
/// (for `DELETE /api/v1/invite-requests/{actor_id}`). The server consumes
/// `timestamp` + `signature` as query parameters, not a body.
pub struct InviteRequestCancel {
    pub actor_id: [u8; 32],
    pub timestamp: u64,
    pub signature: [u8; 64],
}

/// Build a signed registration request.
///
/// Signs the domain-tagged, length-prefixed
/// `fauna_protocol::account::register_signed_message(actor_id, handle, domain,
/// timestamp)` (timestamp in milliseconds) — the single-source builder the nest
/// verifies with, per candidate domain.
pub fn build_register_request(kp: &ActorKeypair, handle: &str, domain: &str) -> RegisterRequest {
    let actor_id = kp.actor_id();
    let timestamp = Timestamp::now_millis();

    let msg =
        fauna_protocol::account::register_signed_message(&actor_id.0, handle, domain, timestamp);
    let sig = kp.signing_key().sign(&msg);

    RegisterRequest {
        actor_id: actor_id.0,
        handle: handle.to_string(),
        timestamp,
        signature: sig.to_bytes(),
    }
}

/// Build a signed challenge-response verify request.
///
/// Signs the domain-tagged, nest-bound
/// `fauna_protocol::auth::challenge_verify_signed_message(actor_id, nonce, nest_id)`
/// (no timestamp — the nonce provides freshness).
pub fn build_challenge_verify(
    kp: &ActorKeypair,
    nonce: &[u8; 32],
    nest_id: &[u8; 32],
) -> ChallengeVerifyRequest {
    let actor_id = kp.actor_id();

    let msg = fauna_protocol::auth::challenge_verify_signed_message(&actor_id.0, nonce, nest_id);
    let sig = kp.signing_key().sign(&msg);

    ChallengeVerifyRequest {
        actor_id: actor_id.0,
        nonce: *nonce,
        nest_id: *nest_id,
        signature: sig.to_bytes(),
    }
}

/// Build a signed invite-request submission.
///
/// Signs the domain-tagged, length-prefixed
/// `fauna_protocol::invite::invite_submit_signed_message(actor_id, handle,
/// message, timestamp)` (timestamp in milliseconds). ⚠ The nest verifies over
/// the **lowercased** handle, so it is lowercased here before signing.
pub fn build_invite_request_submit(
    kp: &ActorKeypair,
    handle: &str,
    message: &str,
) -> InviteRequestSubmit {
    let actor_id = kp.actor_id();
    let timestamp = Timestamp::now_millis();
    let handle = handle.to_lowercase();

    let msg = fauna_protocol::invite::invite_submit_signed_message(
        &actor_id.0,
        &handle,
        message,
        timestamp,
    );
    let sig = kp.signing_key().sign(&msg);

    InviteRequestSubmit {
        actor_id: actor_id.0,
        handle,
        message: message.to_string(),
        timestamp,
        signature: sig.to_bytes(),
    }
}

/// Build a signed invite-request cancellation.
///
/// Signs the domain-tagged
/// `fauna_protocol::invite::invite_cancel_signed_message(actor_id, timestamp)`
/// (timestamp in milliseconds) — the registry tag replaced the legacy ad-hoc
/// `b"cancel"` separator.
pub fn build_invite_request_cancel(kp: &ActorKeypair) -> InviteRequestCancel {
    let actor_id = kp.actor_id();
    let timestamp = Timestamp::now_millis();

    let msg = fauna_protocol::invite::invite_cancel_signed_message(&actor_id.0, timestamp);
    let sig = kp.signing_key().sign(&msg);

    InviteRequestCancel {
        actor_id: actor_id.0,
        timestamp,
        signature: sig.to_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // verify-ok(test): every check below signs with a locally generated key and
    // verifies its own signature back — no wire-supplied key reaches this
    // module, so the permissive trait is harmless here. Production verification
    // goes through `fauna_core::identity::verify_detached`; the walk guard
    // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
    // (Hoisted from four per-test imports so one marker covers the module.)
    use ed25519_dalek::Verifier;

    fn test_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([42u8; 32])
    }

    #[test]
    fn register_request_includes_handle() {
        let kp = test_keypair();
        let req = build_register_request(&kp, "alice", "example.com");
        assert_eq!(req.handle, "alice");
        assert_eq!(req.actor_id, kp.actor_id().0);
        assert_ne!(req.signature, [0u8; 64]);
        assert!(req.timestamp > 0);
    }

    #[test]
    fn challenge_verify_signs_the_tagged_nest_bound_builder_message() {
        let kp = test_keypair();
        let nonce: [u8; 32] = [7u8; 32];
        let nest: [u8; 32] = [0x5e; 32];
        let req = build_challenge_verify(&kp, &nonce, &nest);

        let msg = fauna_protocol::auth::challenge_verify_signed_message(
            &req.actor_id,
            &req.nonce,
            &req.nest_id,
        );
        let verifying_key = kp.verifying_key();
        let sig = ed25519_dalek::Signature::from_bytes(&req.signature);
        assert!(verifying_key.verify(&msg, &sig).is_ok());
        assert_eq!(req.nonce, nonce);
        assert_eq!(req.nest_id, nest);
        // The signature names THIS nest: the same bytes under another nest's
        // identity do not verify (`login.md` § Binding the nest).
        let other = fauna_protocol::auth::challenge_verify_signed_message(
            &req.actor_id,
            &req.nonce,
            &[0x5f; 32],
        );
        assert!(verifying_key.verify(&other, &sig).is_err());
    }

    #[test]
    fn invite_request_submit_signature_verifies_and_lowercases() {
        let kp = test_keypair();
        // Mixed case in, lowercased out: the nest lowercases before verifying,
        // so signing any other case could never verify.
        let req = build_invite_request_submit(&kp, "Alice", "please let me in");
        assert_eq!(req.handle, "alice");

        let msg = fauna_protocol::invite::invite_submit_signed_message(
            &req.actor_id,
            &req.handle,
            &req.message,
            req.timestamp,
        );
        let verifying_key = kp.verifying_key();
        let sig = ed25519_dalek::Signature::from_bytes(&req.signature);
        assert!(verifying_key.verify(&msg, &sig).is_ok());
    }

    #[test]
    fn invite_request_cancel_signs_the_tagged_builder_message() {
        let kp = test_keypair();
        let req = build_invite_request_cancel(&kp);

        let msg =
            fauna_protocol::invite::invite_cancel_signed_message(&req.actor_id, req.timestamp);
        let verifying_key = kp.verifying_key();
        let sig = ed25519_dalek::Signature::from_bytes(&req.signature);
        assert!(verifying_key.verify(&msg, &sig).is_ok());
    }
}
