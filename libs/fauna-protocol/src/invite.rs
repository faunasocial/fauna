//! Pre-identity invite-request + invite-code WS-RPC payload types — the
//! behavior-preserving transport migration of the public in-band invite flow
//! (`bins/fauna-nest/src/invite_requests.rs`): `POST /api/v1/invite-requests`,
//! `GET /api/v1/invite-requests/{actor_id}/status`,
//! `DELETE /api/v1/invite-requests/{actor_id}`, and
//! `POST /api/v1/invite-code/verify` — those HTTP routes were retired (S4a2/S4b).
//! The ceremonies live in the shared `bins/fauna-nest/src/invite_core.rs`.
//! All four run on the **anonymous** WS connection (`GET /api/v1/ws`, no bearer)
//! — the "Invite / storage-mode" row of the pre-identity allowlist in
//! `docs/goal/architecture/transport.md` § Pre-identity (anonymous) connection.
//! Track A5 of the WS-RPC-everywhere migration (tracked internally).
//!
//! These are the public onboarding path a prospective user takes when they
//! reach a claimed nest without an invite code (`onboarding.md` §3 "Invite
//! request"): submit a signed request, poll its status, optionally cancel it,
//! and peek-verify an out-of-band invite code. The admin side
//! (`admin.invite_requests.*` approve/deny, `admin.invite_codes.*`) is a
//! distinct Track-C2 surface — not these kinds.
//!
//! Wire convention (matching `account.rs` / `claim.rs`): identity references
//! (`actor_id`, `signature`, `decided_by`) are **hex-encoded `String`**; the
//! dag-cbor wire forbids floats (none here) and does not round-trip
//! `Option<Option>` — every optional reply field is a plain `Option`. The
//! "an invite request already exists for this actor" case (HTTP returned 409
//! with the existing row body) maps on the WS surface to the error
//! `fauna.account.invite_request_exists`; the caller re-queries via
//! `invite_request.status` to read the row.
//!
//! Kind registry: `kind.rs::register_invite_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.account.invite_request.submit (≡ POST /api/v1/invite-requests) ─────

/// The exact bytes a `fauna.account.invite_request.submit` request signs (and
/// the nest verifies): the **length-prefixed** element list
/// `lp(INVITE_SUBMIT_V1) ‖ lp(actor_id) ‖ lp(handle) ‖ lp(message) ‖
/// lp(timestamp_be)` ([`crate::sig_domain::domain_separated_length_prefixed`]).
/// **Single source of the signed-message contract** — the client signers
/// (`fauna_client_core::auth::build_invite_request_submit`,
/// `fauna-onboarding-machine`) and the nest verifier
/// (`invite_core::submit_invite_request_core`) build the message here so they
/// cannot drift. Length-prefixing closes the finding's item 3 (`handle` and
/// the free-text `message` sat adjacent with no delimiter); the tag
/// ([`crate::sig_domain::INVITE_SUBMIT_V1`]) is the rule-#8 cross-context
/// separation. ⚠ The `handle` is signed as the nest verifies it — lowercased —
/// so signers pass the lowercased handle they submit.
pub fn invite_submit_signed_message(
    actor_id: &[u8; 32],
    handle: &str,
    message: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    crate::sig_domain::domain_separated_length_prefixed(
        crate::sig_domain::INVITE_SUBMIT_V1,
        &[
            actor_id,
            handle.as_bytes(),
            message.as_bytes(),
            &timestamp_ms.to_be_bytes(),
        ],
    )
}

/// The exact bytes a `fauna.account.invite_request.cancel` request signs (and
/// the nest verifies): `INVITE_CANCEL_V1 ‖ actor_id ‖ timestamp_be`
/// ([`crate::sig_domain::domain_separated`]). **Single source of the
/// signed-message contract** — the client signers and the nest verifier
/// (`invite_core::cancel_invite_request_core`) build the message here. The
/// registry tag replaces the legacy ad-hoc `b"cancel"` literal, which was doing
/// a domain tag's job without the registry's pairwise-distinct + prefix-free
/// guarantees.
pub fn invite_cancel_signed_message(actor_id: &[u8; 32], timestamp_ms: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(40);
    body.extend_from_slice(actor_id);
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    crate::sig_domain::domain_separated(crate::sig_domain::INVITE_CANCEL_V1, &body)
}

/// Submit a signed invite request to a claimed nest, proving actor ownership
/// with a domain-tagged signature over
/// [`invite_submit_signed_message`]`(actor_id, handle, message, timestamp)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRequestSubmit {
    /// 64-char hex of the requesting actor's 32-byte Ed25519 public key.
    pub actor_id: String,
    /// Bare handle (no domain) the user wants. Validated + lowercased server-side.
    pub handle: String,
    /// Optional free-text message to the admin (≤ 500 bytes); empty if omitted.
    #[serde(default)]
    pub message: String,
    /// Client wall-clock in Unix **milliseconds**; must be within ±30 s of
    /// server time.
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature over
    /// [`invite_submit_signed_message`]`(actor_id, handle, message, timestamp)`.
    pub signature: String,
    /// The registering app's age claim (`public-mode.md` § Age at
    /// registration). On this path the claim is **absence-as-signal for the
    /// deciding admin** — it never gates the request: a verified attested
    /// claim records `attested-*` provenance on the request row, a
    /// declared-only claim records `none`, and no claim records nothing.
    /// Deliberately outside [`invite_submit_signed_message`] — an attested
    /// claim carries the platform's own signature binding it to this actor
    /// ([`crate::age::age_claim_signed_message`]), and a declared claim is
    /// advisory data the admin reads. Additive 2026-08-24.
    #[serde(default)]
    pub age_claim: Option<crate::age::AgeClaim>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.invite_request.status (≡ GET …/{actor_id}/status) ──────────

/// Look up the current state of an actor's invite request. Public, unauth'd —
/// the onboarding wizard polls this while awaiting an admin decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRequestStatusQuery {
    /// 64-char hex of the actor whose request to look up.
    pub actor_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.invite_request.cancel (≡ DELETE …/{actor_id}) ──────────────

/// Cancel (delete) one's own pending invite request, proving ownership with a
/// domain-tagged signature over
/// [`invite_cancel_signed_message`]`(actor_id, timestamp)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRequestCancel {
    /// 64-char hex of the actor whose request to cancel.
    pub actor_id: String,
    /// Client wall-clock in Unix **milliseconds**; within ±30 s of server time.
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature over
    /// [`invite_cancel_signed_message`]`(actor_id, timestamp)`.
    pub signature: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.invite_code.verify (≡ POST /api/v1/invite-code/verify) ─────

/// Peek-only check that an out-of-band invite code exists and has uses left.
/// Public, unauth'd; the actual decrement happens at `fauna.account.register`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteCodeVerify {
    /// The invite code to verify (case-sensitive, as the user typed it).
    pub code: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Replies ──────────────────────────────────────────────────────────────────

/// An invite request's full state — the reply to `submit` and `status`,
/// mirroring the former HTTP twin's `status_json` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRequestStatus {
    /// Server-assigned row id.
    pub id: i64,
    /// 64-char hex of the requesting actor (echo of the request).
    pub actor_id: String,
    /// The requested handle (lowercased).
    pub handle: String,
    /// The free-text message (empty if none).
    pub message: String,
    /// Lifecycle state: `"pending"`, `"approved"`, or `"denied"`.
    pub status: String,
    /// Unix-time the request was created (the DB row's `created_at`).
    pub created_at: i64,
    /// Unix-time the admin decided, if decided yet.
    pub decided_at: Option<i64>,
    /// 64-char hex of the deciding admin, if decided yet.
    pub decided_by: Option<String>,
    /// Admin-supplied denial reason, if denied.
    pub denial_reason: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `cancel` — `ok` is always `true` on success (failure is an
/// `RpcError`, e.g. `fauna.account.invite_request_not_found`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRequestCancelReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `invite_code.verify` — carries the opaque `invite_id` the wizard
/// round-trips into the eventual `register` call (the code itself, keeping the
/// round-trip 1:1, exactly as the HTTP twin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteCodeVerifyReply {
    pub invite_id: String,
    /// Supervised admission disclosure: the handle of the guardian the account
    /// created from this code will be linked to — rendered by onboarding as
    /// `invite-code-supervised-notice` BEFORE redemption (`family-safety.md`
    /// § Wire & data shape, transparency at creation). `None` = an ordinary
    /// code. Additive 2026-07-09.
    #[serde(default)]
    pub supervised_by: Option<String>,
    /// The age band the code admits under, when its mint chose one
    /// (`family-safety.md` § The account age band — rendered beside
    /// `supervised_by` so the applicant sees the band BEFORE redemption, the
    /// same transparency-at-creation rule). [`crate::age::AgeBand`] wire
    /// token. Additive 2026-08-24.
    #[serde(default)]
    pub age_band: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// Source-level pin on the two invite builders' tag structure (rule #8).
    /// Because signer and verifier share one builder, a builder that silently
    /// lost its tag (or its length prefixes) would stay green in every
    /// behavioural test — only this structural pin catches it.
    #[test]
    fn invite_signed_messages_are_tagged_and_cross_context_distinct() {
        let actor = [0xab_u8; 32];
        let ts: u64 = 1_700_000_000_000;

        // Submit: length-prefixed element list, tag first.
        let submit = invite_submit_signed_message(&actor, "alice", "hi", ts);
        let stag = crate::sig_domain::INVITE_SUBMIT_V1;
        assert_eq!(&submit[..8], &(stag.len() as u64).to_be_bytes());
        assert_eq!(&submit[8..8 + stag.len()], stag);
        // The item-3 resplit is structurally impossible (handle/message split).
        assert_ne!(
            invite_submit_signed_message(&actor, "alice", "hi", ts),
            invite_submit_signed_message(&actor, "aliceh", "i", ts),
        );

        // Cancel: plain tag ‖ actor ‖ ts_be.
        let cancel = invite_cancel_signed_message(&actor, ts);
        let ctag = crate::sig_domain::INVITE_CANCEL_V1;
        assert!(cancel.starts_with(ctag));
        assert_eq!(&cancel[ctag.len()..ctag.len() + 32], &actor);
        // Never confusable with the other actor ‖ ts_be contexts.
        assert_ne!(cancel, crate::claim::claim_admin_signed_message(&actor, ts));
        assert_ne!(
            cancel,
            crate::account::account_lockout_signed_message(&actor, ts)
        );
        assert_ne!(
            cancel,
            crate::auth::handshake_signed_message(&actor, ts, &[0x5e; 32], &[0x42; 32])
        );
    }

    #[test]
    fn submit_round_trips_with_and_without_message() {
        let with = InviteRequestSubmit {
            actor_id: "ab".repeat(32),
            handle: "alice".into(),
            message: "let me in please".into(),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            // Declared-only age claim (additive 2026-08-24) — the
            // absence-as-signal carry for the deciding admin.
            age_claim: Some(crate::age::AgeClaim {
                band: "u13".into(),
                attestation: None,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(with, decode::<InviteRequestSubmit>(&bytes).unwrap());

        let without = InviteRequestSubmit {
            message: String::new(),
            ..with
        };
        let bytes = encode_canonical(&without).unwrap();
        assert_eq!(without, decode::<InviteRequestSubmit>(&bytes).unwrap());
    }

    #[test]
    fn status_query_and_cancel_round_trip() {
        let q = InviteRequestStatusQuery {
            actor_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&q).unwrap();
        assert_eq!(q, decode::<InviteRequestStatusQuery>(&bytes).unwrap());

        let c = InviteRequestCancel {
            actor_id: "ab".repeat(32),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&c).unwrap();
        assert_eq!(c, decode::<InviteRequestCancel>(&bytes).unwrap());
    }

    #[test]
    fn status_reply_round_trips_pending_and_decided() {
        // Decided (all optionals present).
        let decided = InviteRequestStatus {
            id: 7,
            actor_id: "ab".repeat(32),
            handle: "alice".into(),
            message: "hi".into(),
            status: "approved".into(),
            created_at: 1_700_000_000,
            decided_at: Some(1_700_000_100),
            decided_by: Some("ef".repeat(32)),
            denial_reason: None,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&decided).unwrap();
        let back: InviteRequestStatus = decode(&bytes1).unwrap();
        assert_eq!(decided, back);
        // Canonical re-encode is stable.
        assert_eq!(bytes1, encode_canonical(&back).unwrap());

        // Pending (optionals None → null → round-trip back to None).
        let pending = InviteRequestStatus {
            status: "pending".into(),
            decided_at: None,
            decided_by: None,
            denial_reason: None,
            ..decided
        };
        let bytes = encode_canonical(&pending).unwrap();
        let back: InviteRequestStatus = decode(&bytes).unwrap();
        assert_eq!(pending, back);
        assert!(back.decided_at.is_none() && back.decided_by.is_none());
    }

    #[test]
    fn cancel_and_verify_replies_round_trip() {
        let cancel = InviteRequestCancelReply {
            ok: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&cancel).unwrap();
        assert_eq!(cancel, decode::<InviteRequestCancelReply>(&bytes).unwrap());

        let verify = InviteCodeVerifyReply {
            invite_id: "WELCOME2026".into(),
            supervised_by: None,
            age_band: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&verify).unwrap();
        assert_eq!(verify, decode::<InviteCodeVerifyReply>(&bytes).unwrap());

        // Supervised disclosure round-trips, and an ordinary-code reply without the
        // field decodes to None (additive wire evolution).
        let supervised = InviteCodeVerifyReply {
            supervised_by: Some("parent".into()),
            ..verify.clone()
        };
        let bytes = encode_canonical(&supervised).unwrap();
        assert_eq!(supervised, decode::<InviteCodeVerifyReply>(&bytes).unwrap());
        #[derive(serde::Serialize)]
        struct OrdinaryVerifyReply {
            invite_id: String,
        }
        let ordinary = encode_canonical(&OrdinaryVerifyReply {
            invite_id: "WELCOME2026".into(),
        })
        .unwrap();
        assert_eq!(
            decode::<InviteCodeVerifyReply>(&ordinary)
                .unwrap()
                .supervised_by,
            None
        );
    }
}
