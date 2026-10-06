//! Transport-agnostic one-time admin-claim core. The single source of the
//! `claim_admin` ceremony, called by the pre-identity WS-RPC handler
//! (`claim_handlers::register_claim_handlers`) — the sole transport since the
//! `POST /api/v1/claim-admin` HTTP twin was removed in S4d. The
//! handler is a thin adapter mapping
//! `ClaimError` to an `RpcError` and `ClaimOutcome` to the
//! `fauna_protocol::claim::ClaimAdminReply` wire type. Mirrors `auth_core` /
//! `account_core`. Part of the WS-RPC-everywhere migration (tracked internally).
//!
//! The module is deliberately free of any `fauna_protocol` dependency (the
//! mapping core → wire happens in `claim_handlers`).
//!
//! Unlike `account_core`, there is **no** HTTP-only side effect to defer here
//! (no per-IP rate-limit / new-IP `SecurityEvent`), so the WS path runs the
//! ceremony in full — no A1.1 deferral.

use crate::claim::claim_code_path;
use crate::registration::validate_handle;
use crate::routes::{AppState, parse_actor_id, verify_claim_admin_signature};

/// Maximum age (seconds) of the timestamp in a claim request — matches the
/// legacy `claim::post_claim_admin`.
const MAX_CLAIM_AGE_SECS: u64 = 300; // 5 minutes

/// TTL for the bearer token issued on a successful claim — matches the legacy
/// `claim::post_claim_admin`.
const CLAIM_TOKEN_TTL_SECS: u64 = 3600;

/// Transport-agnostic admin-claim failure. The `claim_handlers` adapter maps
/// each variant to an `RpcError` code; the HTTP statuses noted below are the
/// ones the removed `POST /api/v1/claim-admin` twin returned (kept for lineage).
#[derive(Debug)]
pub enum ClaimError {
    /// A 400-class validation failure — malformed actor_id, stale timestamp, or
    /// a bad handle. HTTP 400 with the carried message; WS
    /// `fauna.auth.invalid_request` (detail = the message).
    InvalidRequest(&'static str),
    /// Ed25519 signature verification failed (or the signature/key was
    /// malformed — the twin returned 403 for all of these). HTTP 403 with the
    /// carried message; WS `fauna.auth.signature_failed`.
    SignatureFailed(&'static str),
    /// The supplied claim code did not match. HTTP 403 "invalid claim code";
    /// WS `fauna.auth.invalid_claim_code`.
    InvalidClaimCode,
    /// The nest has already been claimed (the claim-code file is gone). HTTP 410
    /// Gone; WS `fauna.auth.already_claimed`.
    AlreadyClaimed,
    /// The claim-code file is PRESENT but the nest cannot READ it (permission /
    /// I/O) — a misprovisioned box (a root-owned `/data/claim-code` the uid-1000
    /// nest can't open, or a read-only `:ro` seed bind onto `/data/claim-code`).
    /// Distinct from `AlreadyClaimed` (file *gone* = the genuine single-use
    /// replay) so an UNCLAIMED box gets a truthful, terminal diagnosis instead of
    /// a false "already claimed". WS `fauna.auth.claim_code_unreadable`.
    ClaimCodeUnreadable(String),
    /// The requested handle is already assigned to another actor. HTTP 409
    /// "handle already taken"; WS `fauna.account.handle_taken` (the A3 code —
    /// same concept, uniformity #3).
    HandleTaken,
    /// The claiming identity was succeeded on this nest and its key authorizes
    /// nothing here ever again — the successor claims, if anyone does
    /// (`identity-succession.md` § Enforcement on the home nest, step 4 — the
    /// registration doors; `auth_core::successor_of`). WS
    /// `fauna.auth.superseded`.
    Superseded { new_actor_id: [u8; 32] },
    /// Server-side failure (`tracing::error!`/`warn!` carries the real cause).
    /// HTTP 500; WS `fauna.protocol.internal`.
    Internal(String),
}

/// The bearer + nest coordinates issued on a successful claim — `claim_handlers`
/// maps this to the `ClaimAdminReply` wire type.
pub struct ClaimOutcome {
    pub token: String,
    /// Unix seconds at which the token expires.
    pub expires_at: u64,
    pub domain: String,
    /// The handle set during the claim — always present (a claim cannot succeed
    /// without one).
    pub handle: String,
}

/// One-time admin claim — the body of the `fauna.auth.claim_admin` WS-RPC kind.
/// Verifies the timestamp (±300 s, accepting seconds or milliseconds), the
/// domain-tagged signature over `claim_admin_signed_message` (the **raw**
/// timestamp), the
/// required handle, and the claim code; auto-registers the actor, grants admin
/// and superadmin, deletes the single-use claim-code file, writes an audit
/// entry, sets the handle, and mints a 1-hour bearer. The `handle` is required
/// (a handle-less admin is unrepresentable) but still runtime-validated.
pub async fn claim_admin_core(
    state: &AppState,
    actor_id_hex: &str,
    timestamp: u64,
    signature_hex: &str,
    claim_code: &str,
    handle: &str,
    mail_domain: Option<&str>,
) -> Result<ClaimOutcome, ClaimError> {
    // 1. Parse actor_id.
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(ClaimError::InvalidRequest("invalid actor_id"))?;

    // 2. Verify timestamp freshness (5-min window). Accept both seconds and
    //    milliseconds (web apps send ms via buildAuthRequest); the signature
    //    is over the RAW timestamp, so only the freshness check normalizes.
    let now = fauna_core::data::Timestamp::now_secs() as u64;
    let ts_secs = if timestamp > 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    if now.abs_diff(ts_secs) > MAX_CLAIM_AGE_SECS {
        return Err(ClaimError::InvalidRequest("timestamp too old"));
    }

    // 3. Verify the actor signature over the RAW timestamp: domain-tagged only
    //    (`CLAIM_ADMIN_V1 ‖ actor_id ‖ timestamp_be`). The tag
    //    is what stops a captured login handshake signature from doubling as a
    //    claim signature; no untagged accept exists.
    verify_claim_admin_signature(&actor_bytes, timestamp, signature_hex)
        .map_err(ClaimError::SignatureFailed)?;

    // 3'. Supersession consult, after the signature and before every account
    //    gate (`auth_core::successor_of`): a retired key claims nothing, and the
    //    refusal must not depend on its handle-less `users` row surviving.
    match crate::auth_core::successor_of(state, &actor_bytes).await {
        Ok(None) => {}
        Ok(Some(new_actor_id)) => return Err(ClaimError::Superseded { new_actor_id }),
        Err(e) => return Err(ClaimError::Internal(format!("succession consult: {e:?}"))),
    }

    // 3a. DB-positive claimed gate: an admin actor already existing IS the claim
    //    — reject a second claim outright, BEFORE reading the code or creating
    //    anything. The single-use guarantee must not rest solely on the
    //    post-claim file delete (step 7) succeeding: if that delete ever fails
    //    (a read-only cloud-init mount, an `EBUSY`/transient unlink error) the
    //    still-readable code would otherwise let a *different* actor become a
    //    SECOND admin. The authoritative "claimed" signal is an admin row
    //    (`admin_count`, the `admin_actor_ids` table), not the code file's
    //    presence — which is now just a credential consulted below while no
    //    admin exists. Keyed on an *admin* (not `has_any_user`) so a
    //    half-completed claim — a `users` row written before `add_admin_actor`
    //    succeeded, e.g. a crash mid-ceremony — still reads `admin_count == 0`,
    //    leaving its recovery retry below unblocked (§ Client-state recoverability).
    if state
        .db
        .admin_count()
        .await
        .map_err(|e| ClaimError::Internal(format!("admin_count during claim: {e}")))?
        > 0
    {
        return Err(ClaimError::AlreadyClaimed);
    }

    // 4. Verify the claim code FIRST — it is the auth gate. Do NOT consume it
    //    yet: a later failure (missing/invalid/taken handle, DB error) must leave
    //    the code usable for a retry, so a half-completed claim never locks the
    //    admin out.
    // Normalize BOTH sides through the shared `claim_code` helper (uppercase +
    // strip non-alphanumerics) so the display grouping hyphens, case, stray
    // spaces, and the file's trailing newline all wash out — the admin may
    // type the code with or without the groups. A code typed without groups still matches.
    let claim_path = claim_code_path(state);
    let raw_code = match std::fs::read_to_string(&claim_path) {
        Ok(s) => s,
        // NotFound = the single-use code was consumed by a prior successful claim
        // (step 7's delete) — the genuine replay case, kept as `already_claimed`.
        // (We are past the DB-positive admin gate above, which is the authoritative
        // claimed signal; a NotFound here with no admin just means the box has no
        // live claim code to present.)
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClaimError::AlreadyClaimed);
        }
        // Any OTHER read error (PermissionDenied, I/O) means the file is PRESENT but
        // the nest cannot read it — a misprovisioned box (e.g. a root-owned
        // `/data/claim-code` the uid-1000 nest can't open; `docker/entrypoint.sh`
        // now re-chowns it to `fauna` every boot). Reporting this as
        // `already_claimed` on a genuinely UNCLAIMED box misdiagnoses the fault and
        // hides it from the admin, so surface the true cause instead — logged
        // here (visible in `docker compose logs`), returned as a non-terminal
        // Internal error (a retry succeeds once ownership is fixed on the next boot).
        Err(e) => {
            tracing::error!(
                path = %claim_path.display(),
                error = %e,
                "claim-code file present but UNREADABLE by the nest — misprovisioned, \
                 NOT already-claimed; fix its ownership/mode (uid-1000 `fauna` must read it)"
            );
            return Err(ClaimError::ClaimCodeUnreadable(format!(
                "claim-code present but unreadable at {}: {e}",
                claim_path.display()
            )));
        }
    };
    let expected_code = fauna_core::claim_code::normalize(&raw_code);
    if fauna_core::claim_code::normalize(claim_code) != expected_code {
        return Err(ClaimError::InvalidClaimCode);
    }

    // 5. A handle is REQUIRED to claim the nest — the wire type (`claim.rs`)
    //    makes a handle-less request unrepresentable, so this signature takes a
    //    plain `&str`. The admin's handle is its mail address AND its identity,
    //    and the canonical mail-recipient alias is materialized from it
    //    (`docs/goal/behavior/mail-aliases.md` § Kind 1 — Exact); a handle-less
    //    admin is a degenerate, unusable state — mail/AUTH login
    //    (`validate_recipient`) resolves nobody. An empty / too-short / malformed
    //    `String` is still rejected here (`validate_handle`), before creating
    //    anything and before the single-use code is consumed.
    let handle = handle.to_lowercase();
    validate_handle(&handle).map_err(ClaimError::InvalidRequest)?;
    // Deliberate carve-out from `RESERVED_HANDLES` (register/discovery/invite/
    // handle-change all refuse it): the box owner may claim `admin` — the
    // ordinary, expected self-hosted admin handle, exercised throughout this
    // file's own test suite — or any other reserved name. Self-inflicted only
    // (nobody but the owner is affected), recoverable via handle-change, and
    // web-derivation is independently blocked at consumption
    // (`subdomain_host` → `None` for a reserved local-part). Adding the
    // uniform check here would be a real behavior change to the one
    // ceremony every deployment goes through — flagged, not silently fixed.
    match state.db.resolve_handle(&handle).await {
        Ok(Some(existing)) if existing != actor_bytes => return Err(ClaimError::HandleTaken),
        Ok(_) => {}
        Err(e) => {
            return Err(ClaimError::Internal(format!(
                "resolve handle during claim: {e}"
            )));
        }
    }

    // 6. Create the admin atomically WITH the handle — claiming the nest *is*
    //    setting the admin handle (no best-effort afterthought that could leave
    //    the admin handle-less). Any step here failing aborts before the
    //    single-use code is consumed below.
    match state.db.is_actor_registered(&actor_bytes).await {
        Ok(false) => {
            state
                .db
                .create_user_with_handle(&actor_bytes, "free", &handle, None)
                .await
                .map_err(|e| ClaimError::Internal(format!("failed to create admin user: {e}")))?;
        }
        Ok(true) => {
            state
                .db
                .set_handle(&actor_bytes, &handle)
                .await
                .map_err(|e| ClaimError::Internal(format!("set handle during claim: {e}")))?;
        }
        Err(e) => return Err(ClaimError::Internal(format!("database error: {e}"))),
    }

    state
        .db
        .add_admin_actor(&actor_bytes)
        .await
        .map_err(|e| ClaimError::Internal(format!("failed to add admin: {e}")))?;
    if let Err(e) = state.db.set_admin_role(&actor_bytes, "superadmin").await {
        tracing::warn!("set superadmin role during claim: {e}");
    }
    // The first admin's pairing rows now name the deployment's topology
    // (`private-mode.md` § Pairing Flow): rebuild the dialer's table, as the
    // `admin.add` arm does.
    crate::nest_sync_worker::refresh_pairing_targets(state).await;

    // 7. Consume the single-use claim code now that the admin (with handle) is
    //    fully established, then audit.
    let _ = std::fs::remove_file(&claim_path);
    let _ = state
        .db
        .audit(Some(&actor_bytes), "admin.claim", None, None)
        .await;
    tracing::info!(
        "Admin claimed by actor {} with handle {handle:?}",
        hex::encode(actor_bytes)
    );
    let final_handle = handle;

    // The handle IS the deployment's identity: the wizard handle carries the
    // domain (`alice@example.com`), the client sends that `@suffix` here, and it is
    // the **sole** determinant of the deployment domain — there is no `FAUNA_DOMAIN`
    // (the box boots domainless; `domains-and-tls-bootstrap.md` § Env contract).
    //
    // Registering it as the **primary `mail_domains` row** is what sets the identity:
    // that row IS the nest's identity (`dns-management.md`), and
    // `ensure_mail_domain_registered` refreshes the sync identity cache
    // (`AppState.identity_domain`, read at top precedence by `handle_domain()` /
    // `web_serving_domain()`) + self-heals the TLS floor/ACME for it, so
    // discovery/web/TLS all follow the claimed domain immediately — no split-brain,
    // no second store to drift.
    //
    // Gated to a real domain: a **local target** (IP literal, `localhost`/`*.localhost`,
    // `.local` — `alice@10.1.8.51`) is the *access address*, not a mail domain, and
    // `mail.<ip>` would be nonsense (breaks mail + CalDAV host-routing — the home2
    // symptom). So a local claim registers nothing and the identity stays the
    // `localhost` fallback (as today); the admin adds a real domain later from a
    // client, which sets the primary + identity the same way. Best-effort + idempotent.
    //
    // `is_hostname_syntax` refuses a `d` carrying userinfo/a path/whitespace
    // BEFORE the classifier runs — `is_public_dns_name` is a negative test, so
    // such a `d` would otherwise still read as "public" and reach the identity
    // cache (`security.md` § Transport trust).
    if let Some(d) = mail_domain
        && fauna_core::web::is_hostname_syntax(d)
        && fauna_provisioning::probe::resolve_handle_domain(d).is_public_dns_name
    {
        crate::mail_enable::ensure_mail_domain_registered(state, d).await;
    }

    // 7. Issue the bearer token.
    let actor_id = fauna_core::identity::ActorId(actor_bytes);
    let token = state
        .auth
        .token_store
        .insert(actor_id, CLAIM_TOKEN_TTL_SECS)
        .await;
    let expires_at = now + CLAIM_TOKEN_TTL_SECS;

    // Return the just-set identity domain (top-precedence `identity_domain`), not
    // the stale `registration.handle_domain` — so a domainless-booted box reports
    // the domain the admin just claimed instead of "localhost".
    let domain = state.handle_domain();

    Ok(ClaimOutcome {
        token,
        expires_at,
        domain,
        handle: final_handle,
    })
}

// The claim-time mail-domain auto-registration now lives in
// `crate::mail_enable::ensure_mail_domain_registered` (shared with the
// enable/boot-time safety net `ensure_primary_mail_domain`); the claim
// path calls it above when the claim carries a `mail_domain`.
