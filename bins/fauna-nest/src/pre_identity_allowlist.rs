//! The fixed pre-identity kind allowlist for the anonymous WS connection
//! (`GET /api/v1/ws`, no bearer). Per `docs/goal/architecture/transport.md`
//! § Pre-identity (anonymous) connection: an anonymous connection routes
//! **only** these kinds; a Request for any other kind gets
//! `RpcError { code: "fauna.protocol.unauthenticated" }` and the connection
//! stays open. This mirrors the single-source-of-truth shape of
//! `bridge_method_allowlist` (but is a flat kind set, not a `CallerClass`
//! matrix — there is no actor on an anonymous connection).
//!
//! The full ratified surface (transport.md § Pre-identity, the allowlist
//! table) is enumerated here so that the discovery / registration / admin-claim
//! handler slices (part of the WS-RPC-everywhere migration, tracked
//! internally) are pure handler additions and never re-touch this gate. A kind that is
//! allowlisted here but not yet registered in the `RpcRouter` falls through to
//! `fauna.protocol.unknown_kind` after the gate — the honest "allowed
//! pre-identity, not implemented yet" outcome. The Track A5 invite /
//! storage-mode kinds were ratified in `api-layers.md` (their names settled at
//! transport.md § Pre-identity) and are now allowlisted alongside A1-A4.

/// Returns `true` iff `kind` may be invoked on the anonymous (pre-identity) WS
/// connection.
pub fn is_pre_identity_kind(kind: &str) -> bool {
    matches!(
        kind,
        // Track A1 — auth bootstrap (built: auth_handlers).
        "fauna.auth.handshake"
            | "fauna.auth.challenge"
            | "fauna.auth.verify"
            // Pre-identity nest-identity handshake: the nest proves ITS identity
            // (a channel binding over the client's nonce) before any actor
            // exists — the first-contact trust leg of a client-provisioned box
            // (security.md § Transport trust Axis 2, client-provisioned row).
            // Not throttled: it is the opening step of every pre-claim
            // connection, signature-work-bounded (one Ed25519 sign per call)
            // and mutation-free (see `is_throttled_anonymous_kind`).
            | "fauna.auth.nest_handshake"
            // Renewal-grant bearer mint (sync-agent.md § Credential model,
            // additive 2026-07-19): the sync agent renews its bearer app-dead
            // by presenting a device-key signature over a stored
            // RenewBearer-scoped DeviceAuthorization. Pre-identity by
            // necessity — the agent's old bearer may already be expired when
            // it renews. NOT throttled: signature-work-bounded like
            // `fauna.auth.handshake` (one Ed25519 verify + grant lookup),
            // replay-guarded, and its availability is what keeps app-dead
            // sync alive.
            | "fauna.auth.device_handshake"
            | "fauna.auth.custody_handshake"
            // Track A3 — account registration (handler pending).
            | "fauna.account.register"
            // The attested age claim's nonce mint (family-safety.md § The
            // account age band). Pre-identity by necessity — the registering
            // actor has no account yet; the actor is bound inside the
            // platform-signed payload, not at mint. Throttled (below).
            | "fauna.account.age_nonce"
            // Emergency no-token account lockout — the recovery channel that
            // must work when no authed WS can be opened (a stolen device). It
            // authenticates by an Ed25519 signature in the payload (over
            // `actor_id ‖ timestamp_be`), like `fauna.auth.{challenge,verify}`,
            // so it rides the anonymous connection. **Throttled** — and note
            // that being signature-gated is the *reason* it is throttled, not
            // an exemption from it: an Ed25519 forgery is not the threat, so
            // the limit bounds the verification work an anonymous source can
            // conscript, exactly as the escrow / veto / succession pair
            // comments below spell out (see `is_throttled_anonymous_kind`).
            // The migration of the `POST /api/v1/account/lockout` HTTP twin
            // (the authed sibling is the bearer kind `fauna.sessions.lockout`).
            | "fauna.account.lockout"
            // Track A2 — public discovery (handlers pending).
            | "fauna.nest.info"
            | "fauna.handle.available"
            | "fauna.nest.resolve"
            | "fauna.actor.by_handle"
            | "fauna.setup.status"
            // Track A4 — one-time admin claim.
            | "fauna.auth.claim_admin"
            // Track A5 — in-band invite flow.
            | "fauna.account.invite_request.submit"
            | "fauna.account.invite_request.status"
            | "fauna.account.invite_request.cancel"
            | "fauna.account.invite_code.verify"
            // NAT-mode set (onboarding nat_mode_choice + admin toggle): an
            // admin-signed pre-identity commit (mutable).
            | "fauna.setup.nat_mode"
            // RecoveryKey registration chain (identity-succession slice 2).
            // Pre-identity by necessity: the caller is a federation peer or
            // another user's client verifying a succession statement against
            // the recovery pubkey registered for an actor id it holds
            // (`identity-succession.md:56`) — it has no account here. The reply
            // is public by construction (the same binding rides the actor's
            // signed Profile). Throttled as a directory oracle, below.
            | "fauna.recovery.registration.chain"
            // Seed-escrow restore (identity-succession slice 2). Pre-identity
            // by necessity and by definition: the caller has lost every device
            // and holds only the recovery phrase, so there is no session to
            // authenticate with — that is the scenario the plane exists for
            // (`identity-succession.md:44`). Authorization is the RecoveryKey
            // signature over the nonce `challenge` issues, verified against the
            // head of the actor's registration chain; `challenge` itself is
            // deliberately unconditional, so it reveals nothing about the
            // account. Both are throttled, below.
            | "fauna.recovery.escrow.challenge"
            | "fauna.recovery.escrow.fetch"
            // Replacement veto (identity-succession slice 2). Pre-identity by
            // necessity: the veto scenario is a seed thief who revoked every
            // session and invoked the seed-signed lockout, leaving the real
            // owner holding only the recovery phrase
            // (`identity-succession.md:37`). Authorization is the RecoveryKey
            // signature over the nonce `challenge` issues, verified against
            // the chain head; `challenge` is unconditional for the same
            // no-probe reason as the escrow pair. Both throttled, below.
            | "fauna.recovery.replacement.challenge"
            | "fauna.recovery.replacement.veto"
            // Succession (identity-succession slice 3). `submit` is
            // pre-identity by necessity and is the sharpest case of it in the
            // tree: the scenario IS an owner whose seed a thief holds, and that
            // thief can revoke every session and invoke the seed-signed
            // emergency lockout (`identity-succession.md:66`). A bearer
            // requirement here would let the attack disable its own remedy.
            // Authorization is the RecoveryKey signature inside the statement,
            // checked against the chain the named identity registered — a
            // credential that never touched a device, so the thief cannot hold
            // it. `lookup` is pre-identity for `registration.chain`'s reason:
            // peers hold OLD actor ids and discovery must work from them
            // (`identity-succession.md:72`). Both throttled, below.
            | "fauna.recovery.succession.submit"
            | "fauna.recovery.succession.lookup"
            // The deployment-seed rotation chain
            // (`fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND`,
            // `nest/box-recovery.md` § Client acceptance). Pre-identity by
            // necessity, and the necessity is sharper than it first reads: the
            // client reaches this kind *because* the identity it was presented
            // does not match its pin, which is the one moment its stored bearer
            // is worthless — the token was minted by, and the channel bound to,
            // an identity this box no longer serves. Requiring a session here
            // would mean the bridge that makes rotation silent is unreachable
            // exactly when rotation happened, i.e. it would never work at all.
            // The reply is public by construction (every hop is signed and
            // meant to be walked by anyone); throttled as a directory read,
            // below.
            | "fauna.auth.rotation_chain"
            // Bridge zero-touch self-enrollment (loopback-gated, see
            // `requires_loopback_peer`). A bridge announces a freshly-generated
            // keypair over the anonymous WS to create a `pending` row.
            | "fauna.bridges.request_enrollment"
    )
}

/// Returns `true` iff `kind` may be invoked **only from a loopback (same-host)
/// peer** — the dispatcher refuses it from any other source IP. Bridge
/// self-enrollment (`fauna.bridges.request_enrollment`) is the sole member: a
/// process that can reach nest's loopback is already inside the deployment trust
/// boundary, so it may create a `pending` enrollment row without a signature;
/// remote (cross-IP) bridge enrollment needs an auth mechanism not yet designed
/// (see `docs/goal/behavior/mail-bridge-lifecycle.md` § Cold boot). The peer
/// addr is `RpcConnection::peer_addr`, populated on **both** of nest's listener
/// paths: the plain-HTTP listener (where axum's
/// `into_make_service_with_connect_info` wires `ConnectInfo`) and the
/// TLS-terminating listener (where `serve_tls`'s `WithConnectInfo` middleware
/// injects the accepted TCP peer — `lib.rs`). The in-container bridge dials nest
/// over loopback TLS, so its `peer_addr` is loopback and this gate passes; a
/// remote (non-loopback) source is refused.
pub fn requires_loopback_peer(kind: &str) -> bool {
    matches!(kind, "fauna.bridges.request_enrollment")
}

/// Returns `true` iff `kind` is an anonymous kind subject to the generic
/// per-source rate limit (`anonymous_rate_limit`). Membership is a **property of
/// the allowlist** — a future anonymous kind is added here, not gated by a
/// per-kind hack at the call site.
///
/// **Two classes are in the set, for two different reasons.**
///
/// 1. **Unsigned directory/metadata oracles**, where the limit is the *primary*
///    DoS / enumeration defense (`docs/goal/architecture/federation.md`
///    § Security): `by_handle` is a handle→actor_id oracle, `handle.available`
///    its inverse, `nest.resolve` triggers DNS/SRV work (amplification),
///    `nest.info` is a cheap fingerprint probe, and `registration.chain` /
///    `succession.lookup` are public directory reads of the same shape.
/// 2. **Signature-bound recovery ceremonies**, where the limit bounds the
///    **verification work an anonymous source can conscript**. ⚠ Being
///    signature-gated is the *reason these are throttled*, never an exemption:
///    an Ed25519 forgery is not the threat, so the signature check is exactly
///    the unmetered work a flood buys. None can lock a legitimate holder out —
///    each is a handful of calls, once, and the bucket keys on the unspoofable
///    TCP peer, so a flood throttles only the flooder's own source.
///
/// The auth-bootstrap / claim / invite / storage-mode / enrollment pre-identity
/// kinds are deliberately **not** throttled *here*: `claim_admin`,
/// `invite_code.verify`, `register` and `invite_request.submit` each carry their
/// **own** dedicated dispatcher limiter with a distinct budget (a shared budget
/// would be wrong for a credential-guess surface), enrollment is loopback-gated,
/// and the auth ceremony is left open because throttling it per source could
/// lock a legitimate actor out of bootstrap. (`device_handshake`'s one bound is
/// the refusal-counting `crate::failed_credential_throttle`, which a
/// successful mint never spends.)
///
/// ⚠ When adding an anonymous signature-bound kind, class 2 is the default —
/// "it is signature-gated" argues *for* membership here, not against it.
pub fn is_throttled_anonymous_kind(kind: &str) -> bool {
    matches!(
        kind,
        "fauna.nest.info"
            | "fauna.handle.available"
            | "fauna.nest.resolve"
            | "fauna.actor.by_handle"
            // The RecoveryKey chain is the same shape of oracle: an
            // `actor_id → recovery_pubkey` directory read with no signature
            // requirement. Its contents are public by design, so the limit is
            // an enumeration/DoS bound, not a confidentiality one.
            | "fauna.recovery.registration.chain"
            // The escrow-restore pair is throttled for a *different* reason
            // than the oracles above, and `identity-succession.md:44` requires
            // it explicitly. `challenge` is an unauthenticated write to an
            // in-memory nonce store — unbounded issuance is a memory-growth
            // surface, and it is the one kind here a caller can invoke with no
            // credential whatsoever. `fetch` is signature-gated (an Ed25519
            // forgery is not the threat), so its limit bounds the verification
            // work an anonymous source can conscript. Neither throttle can lock
            // a legitimate holder out: restore is a handful of calls, once.
            | "fauna.recovery.escrow.challenge"
            | "fauna.recovery.escrow.fetch"
            // The veto pair is throttled for the escrow pair's reasons
            // exactly: `challenge` is an unauthenticated in-memory nonce
            // write, `veto` bounds anonymous signature-verification work. A
            // legitimate holder needs a handful of calls, once.
            | "fauna.recovery.replacement.challenge"
            | "fauna.recovery.replacement.veto"
            // Succession. `lookup` is a public directory oracle of exactly the
            // `by_handle` shape, so it is throttled for the same enumeration/DoS
            // reason. `submit` is signature-gated (an Ed25519 forgery is not
            // the threat), so its limit bounds the verification work an
            // anonymous source can conscript — and it cannot lock a legitimate
            // owner out, because a succession is a handful of calls, once, and
            // the throttle is per source.
            | "fauna.recovery.succession.submit"
            | "fauna.recovery.succession.lookup"
            // The emergency no-token lockout is the same profile as the
            // signature-gated members above, and was the one kind left holding
            // the pre-D10 "signature-gated ⇒ needs no throttle" reasoning: its
            // cheap pre-checks (parse, ±300 s timestamp) all pass, so an
            // anonymous flood reached the unmetered Ed25519 verify in
            // `account_core::lockout_core` with nothing bounding it. The
            // legitimate owner is unaffected — reaching for the panic button is
            // a handful of calls, once, well inside the budget, and the bucket
            // keys on the source.
            | "fauna.account.lockout"
            // The age-nonce mint is the escrow/veto `challenge` profile
            // exactly: an unauthenticated write to an in-memory nonce store,
            // so unbounded issuance is a memory-growth surface. It cannot
            // lock a legitimate applicant out — onboarding mints one nonce
            // (a retry after expiry is a second), well inside the budget.
            | "fauna.account.age_nonce"
            // The rotation chain is a class-1 read: an unsigned, unauthenticated
            // directory answer of the `nest.info` shape (it fingerprints the box
            // — how many times it has rotated, and when). The limit is an
            // enumeration/DoS bound, not a confidentiality one; the contents are
            // public by design. It cannot lock a legitimate client out: a client
            // fetches the chain once per observed identity change, and the
            // bucket keys on the unspoofable TCP peer.
            | "fauna.auth.rotation_chain"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_bootstrap_kinds_are_permitted() {
        assert!(is_pre_identity_kind("fauna.auth.handshake"));
        assert!(is_pre_identity_kind("fauna.auth.challenge"));
        assert!(is_pre_identity_kind("fauna.auth.verify"));
        assert!(is_pre_identity_kind("fauna.auth.nest_handshake"));
        assert!(is_pre_identity_kind("fauna.auth.device_handshake"));
        assert!(is_pre_identity_kind("fauna.auth.custody_handshake"));
    }

    #[test]
    fn discovery_and_claim_kinds_are_permitted() {
        for kind in [
            "fauna.account.register",
            "fauna.account.lockout",
            "fauna.nest.info",
            "fauna.handle.available",
            "fauna.nest.resolve",
            "fauna.actor.by_handle",
            "fauna.setup.status",
            "fauna.auth.claim_admin",
        ] {
            assert!(is_pre_identity_kind(kind), "{kind} should be allowlisted");
        }
    }

    #[test]
    fn authenticated_kinds_are_rejected() {
        // Layer-1 content kinds must never route on an anonymous connection.
        for kind in [
            "fauna.posts.create",
            "fauna.posts.get",
            "fauna.spam.get_preferences",
            "fauna.conversations.channel.send",
            "fauna.protocol.echo",
            "",
            "not.a.fauna.kind",
        ] {
            assert!(!is_pre_identity_kind(kind), "{kind} must be rejected");
        }
    }

    #[test]
    fn request_enrollment_is_pre_identity_and_loopback_only() {
        // Bridge self-enrollment routes on the anonymous connection ...
        assert!(is_pre_identity_kind("fauna.bridges.request_enrollment"));
        // ... but only from a loopback peer.
        assert!(requires_loopback_peer("fauna.bridges.request_enrollment"));
        // Other pre-identity / bridge kinds are NOT loopback-gated.
        assert!(!requires_loopback_peer("fauna.auth.verify"));
        assert!(!requires_loopback_peer("fauna.bridges.whoami"));
        assert!(!requires_loopback_peer("fauna.account.register"));
    }

    #[test]
    fn the_generic_gate_covers_the_oracles_and_the_signature_bound_ceremonies() {
        // The world-readable discovery oracles are throttled per source.
        for kind in [
            "fauna.nest.info",
            "fauna.handle.available",
            "fauna.nest.resolve",
            "fauna.actor.by_handle",
        ] {
            assert!(
                is_throttled_anonymous_kind(kind),
                "{kind} should be throttled"
            );
            // Every throttled kind must itself be a pre-identity kind.
            assert!(is_pre_identity_kind(kind));
        }
        // Auth-bootstrap / claim / invite / enrollment are pre-identity but NOT
        // throttled *here* — the credential-guess surfaces carry their own
        // dedicated limiters, enrollment is loopback-gated, and the auth
        // ceremony must stay reachable.
        for kind in [
            "fauna.auth.handshake",
            "fauna.auth.verify",
            "fauna.auth.nest_handshake",
            "fauna.auth.device_handshake",
            "fauna.auth.custody_handshake",
            "fauna.account.register",
            "fauna.auth.claim_admin",
            "fauna.setup.status",
            "fauna.account.invite_code.verify",
            "fauna.bridges.request_enrollment",
        ] {
            assert!(
                !is_throttled_anonymous_kind(kind),
                "{kind} must not be throttled"
            );
        }
    }

    #[test]
    fn the_emergency_lockout_is_pre_identity_and_throttled() {
        // The panic button must be reachable with no session at all — the
        // thief revokes every session, so a bearer requirement would let the
        // attack disable its own remedy.
        assert!(is_pre_identity_kind("fauna.account.lockout"));
        // ... and it is throttled for the escrow pair's reason exactly: being
        // signature-gated means an Ed25519 forgery is not the threat, so the
        // limit bounds the verification work an anonymous source can conscript.
        // ⚠ This kind is the regression guard for that rationale — it is the
        // one that used to carry "signature-gated ⇒ NOT throttled", which the
        // escrow pair's own comment rejects one screen away.
        assert!(is_throttled_anonymous_kind("fauna.account.lockout"));
        // The authenticated sibling is a bearer kind, not an anonymous one.
        assert!(!is_pre_identity_kind("fauna.sessions.lockout"));
    }

    #[test]
    fn the_recovery_chain_is_pre_identity_and_throttled() {
        // A peer verifying a succession holds no account here, so the chain
        // must route anonymously ...
        assert!(is_pre_identity_kind("fauna.recovery.registration.chain"));
        // ... and it is a directory oracle, so it is throttled like the others.
        assert!(is_throttled_anonymous_kind(
            "fauna.recovery.registration.chain"
        ));
        // Submitting a registration is NOT anonymous — it writes into an
        // identity's chain and is bound to the authenticated connection's
        // actor.
        assert!(!is_pre_identity_kind("fauna.recovery.registration.submit"));
        assert!(!requires_loopback_peer("fauna.recovery.registration.chain"));
    }

    #[test]
    fn the_escrow_restore_pair_is_pre_identity_and_throttled() {
        // Restore runs after total device loss, so the fetch ceremony MUST be
        // reachable with no account and no session — if either of these ever
        // stopped being pre-identity, seed escrow would only work for users who
        // did not need it.
        for kind in [
            "fauna.recovery.escrow.challenge",
            "fauna.recovery.escrow.fetch",
        ] {
            assert!(is_pre_identity_kind(kind), "{kind} must route anonymously");
            // `identity-succession.md:44` requires the restore path be
            // rate-limited; the generic gate is where that is applied.
            assert!(
                is_throttled_anonymous_kind(kind),
                "{kind} must be throttled"
            );
            assert!(!requires_loopback_peer(kind));
        }
        // The put is the owner writing its own row over its own authenticated
        // connection — never anonymous.
        assert!(!is_pre_identity_kind("fauna.recovery.escrow.put"));
        assert!(!is_throttled_anonymous_kind("fauna.recovery.escrow.put"));
    }

    #[test]
    fn the_replacement_veto_pair_is_pre_identity_and_throttled() {
        // The veto exists for the locked-out owner: a seed thief can revoke
        // every session and invoke the seed-signed lockout, so the contest
        // MUST be reachable with nothing but the recovery phrase. If either
        // of these stopped being pre-identity, the 30-day window would be
        // uncontestable in exactly the attack it guards against.
        for kind in [
            "fauna.recovery.replacement.challenge",
            "fauna.recovery.replacement.veto",
        ] {
            assert!(is_pre_identity_kind(kind), "{kind} must route anonymously");
            assert!(
                is_throttled_anonymous_kind(kind),
                "{kind} must be throttled"
            );
            assert!(!requires_loopback_peer(kind));
        }
        // The request and status halves ride the owner's authenticated
        // session — never anonymous.
        assert!(!is_pre_identity_kind("fauna.recovery.replacement.request"));
        assert!(!is_pre_identity_kind("fauna.recovery.replacement.status"));
    }

    #[test]
    fn the_succession_pair_is_pre_identity_and_throttled() {
        // The sharpest pre-identity case in the tree: a thief holding the seed
        // can revoke every session AND invoke the emergency lockout, so if
        // `submit` ever needed a bearer the attack would disable its own
        // remedy. If this assertion ever fails, seed-theft recovery is broken
        // in exactly the scenario it exists for.
        for kind in [
            "fauna.recovery.succession.submit",
            "fauna.recovery.succession.lookup",
        ] {
            assert!(is_pre_identity_kind(kind), "{kind} must route anonymously");
            assert!(
                is_throttled_anonymous_kind(kind),
                "{kind} must be throttled"
            );
            assert!(!requires_loopback_peer(kind));
        }
        // ⚠ The *third* succession kind is the exact opposite, and the boundary
        // is the whole reason it is a separate kind. `status` serves a
        // **server-observed** commit stamp — not part of any signed artifact,
        // so it inherits none of the statements' disclosure licence. Anonymous
        // routing here would publish "this account was compromised and
        // recovered at second T" to anyone who asks, plus a correlation handle
        // (two accounts succeeding in one second ⇒ one incident). Gate 1b in
        // `routes::dispatch_request` is what refuses it, and this assertion is
        // what keeps that true: a well-meaning "the succession kinds belong
        // together" edit to the list above would silently build the oracle.
        assert!(!is_pre_identity_kind(
            fauna_protocol::recovery::SUCCESSION_STATUS_KIND
        ));
        // And it needs no anonymous throttle, because it cannot be reached
        // anonymously at all — its bound is the authenticated per-actor budget.
        assert!(!is_throttled_anonymous_kind(
            fauna_protocol::recovery::SUCCESSION_STATUS_KIND
        ));
        // `owed_settle` deletes from the list `status` serves, so it sits on
        // the same side of the boundary: an anonymous caller could otherwise
        // clear a successor's owed nests and leave the retired identity
        // standing at every one of them.
        assert!(!is_pre_identity_kind(
            fauna_protocol::recovery::SUCCESSION_OWED_SETTLE_KIND
        ));
        assert!(!is_throttled_anonymous_kind(
            fauna_protocol::recovery::SUCCESSION_OWED_SETTLE_KIND
        ));
    }

    #[test]
    fn the_rotation_chain_is_pre_identity_and_throttled() {
        use fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND;
        // The scenario IS a pin mismatch, so the client's stored bearer was
        // minted by an identity this box no longer serves. If this ever stopped
        // being pre-identity, a rotated box would be unreachable by precisely
        // the clients the rotation chain exists to carry across — i.e. the
        // ceremony would silently revert to the outage it was designed to end.
        assert!(is_pre_identity_kind(ROTATION_CHAIN_KIND));
        // A public directory read of the `nest.info` shape — throttled as an
        // enumeration/DoS bound.
        assert!(is_throttled_anonymous_kind(ROTATION_CHAIN_KIND));
        assert!(!requires_loopback_peer(ROTATION_CHAIN_KIND));
        // Its Admin-class producer is the opposite of anonymous: rotating is
        // the most privileged act on the box, and only the *reading* half is
        // public.
        assert!(!is_pre_identity_kind("fauna.admin.deployment_seed.rotate"));
        assert!(!is_throttled_anonymous_kind(
            "fauna.admin.deployment_seed.rotate"
        ));
        // The co-admin seed hand-off (`fauna.admin.deployment_seed.get`) is
        // the same Admin-class secret read as rotation's reverse direction —
        // an authenticated roster admin only, never reachable anonymously.
        assert!(!is_pre_identity_kind("fauna.admin.deployment_seed.get"));
        assert!(!is_throttled_anonymous_kind(
            "fauna.admin.deployment_seed.get"
        ));
    }

    #[test]
    fn a5_invite_kinds_are_permitted() {
        // A5 invite names ratified in api-layers.md (Track A5) — allowlisted
        // on the anonymous connection. (The storage-mode commit that rode
        // beside them left the wire 2026-09-24.)
        for kind in [
            "fauna.account.invite_request.submit",
            "fauna.account.invite_request.status",
            "fauna.account.invite_request.cancel",
            "fauna.account.invite_code.verify",
        ] {
            assert!(is_pre_identity_kind(kind), "{kind} should be allowlisted");
        }
    }
}
