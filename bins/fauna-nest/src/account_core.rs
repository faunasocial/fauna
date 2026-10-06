//! Transport-agnostic account-registration core. The single source of the
//! self-service `register` ceremony, consumed by the pre-identity WS-RPC handler
//! (`account_handlers::register_account_handlers`) — the sole transport since
//! the `registration::post_register` HTTP twin was retired
//! (S4f). The handler is a thin adapter mapping
//! `RegisterError` → `RpcError` and `RegisterOutcome` → the
//! `fauna_protocol::account::RegisterReply` wire type. Mirrors `auth_core` /
//! `discovery_core`. Track A3 of the WS-RPC-everywhere migration (tracked internally).
//!
//! The module is deliberately free of any `fauna_protocol` dependency; the
//! mapping core → wire happens in `account_handlers`.
//!
//! What lives where: everything `post_register` did **except** the leading
//! per-IP rate-limit lives here. The rate-limit is now applied in the dispatcher
//! (`routes::dispatch_request` gate 1b⁗, `register_rate_limit`, keyed on the real
//! client IP) rather than in this transport-agnostic core — restored now that the
//! SNI router conveys the client IP to the anonymous WS path (the earlier
//! "no peer IP yet" A1.1 deferral is closed; the dead `registration_limiter`
//! governor field that never re-wired it was removed). See
//! `crate::anonymous_rate_limit::register_config`.

use crate::registration::validate_handle;
use crate::routes::{AppState, parse_actor_id, verify_account_lockout_signature};

/// Maximum tolerated drift between the client-supplied timestamp and server
/// time (matches the value in the legacy `registration::post_register`).
const MAX_TIMESTAMP_DRIFT_MS: u64 = 30_000;

/// Transport-agnostic registration failure. Adapters map each variant to an
/// HTTP status (the twin reproduces the exact status + message) or an
/// `RpcError` code (`fauna.account.*`).
#[derive(Debug)]
pub enum RegisterError {
    /// Self-service registration is disabled (`registration.open == false`).
    /// HTTP 403 "registration is closed"; WS `fauna.account.registration_closed`.
    RegistrationClosed,
    /// A 400-class validation failure — bad/reserved handle, malformed
    /// actor_id, stale timestamp, malformed signature/key, or an invalid
    /// invite code. HTTP 400 with the carried message; WS
    /// `fauna.account.invalid_request` (detail = the message).
    InvalidRequest(&'static str),
    /// Ed25519 signature verification failed. HTTP 401
    /// "signature verification failed"; WS `fauna.account.signature_failed`.
    SignatureFailed,
    /// An invite code is required but none was supplied. HTTP 403
    /// "invite code required"; WS `fauna.account.invite_required`.
    InviteRequired,
    /// The free-tier user cap is reached. HTTP 403 "free user limit reached";
    /// WS `fauna.account.free_limit_reached`.
    FreeLimitReached,
    /// The actor already has an account. HTTP 409 "actor already registered";
    /// WS `fauna.account.actor_exists`.
    ActorAlreadyRegistered,
    /// The identity was succeeded on this nest and its key registers nothing
    /// here ever again (`identity-succession.md` § Enforcement on the home
    /// nest, step 4 — the registration doors). Consulted from
    /// `actor_successions`, never inferred from a `users` row: the retired
    /// identity's row goes with the chain when the successor is deleted, and
    /// the refusal must not go with it. WS `fauna.auth.superseded`.
    Superseded { new_actor_id: [u8; 32] },
    /// The handle is already assigned (also the `UNIQUE`-constraint race
    /// fallback). HTTP 409 "handle already taken"; WS `fauna.account.handle_taken`.
    HandleTaken,
    /// The handle is in release cooldown for a different actor. HTTP 409
    /// "handle is in cooldown"; WS `fauna.account.handle_cooldown`.
    HandleCooldown,
    /// The admin require-knob is on and this self-service admission carries no
    /// **verified** attested age claim (`public-mode.md` § Age at
    /// registration; the knob gates exactly this ceremony — open registration
    /// and code redemption alike — never the admin-judged request-approval
    /// path). WS `fauna.account.age_verification_required`.
    AgeVerificationRequired,
    /// The carried age claim says **minor** and this path would create an
    /// *unsupervised* account — refused toward guardian-mediated admission
    /// (a guardian-designated invite code, or the request-approval queue).
    /// WS `fauna.account.guardian_admission_required`.
    GuardianAdmissionRequired,
    /// The claim's platform attestation failed verification (bad chain, wrong
    /// nonce, band/actor mismatch, unknown platform, …). Distinct from
    /// `InvalidRequest` so a knob-armed nest's refusals diagnose themselves.
    /// WS `fauna.account.age_attestation_invalid` (detail = the message).
    AgeAttestationInvalid(&'static str),
    /// Server-side failure (`tracing::error!` carries the real cause; the
    /// message is the twin's fixed "internal error"). HTTP 500; WS
    /// `fauna.protocol.internal`.
    Internal(String),
}

/// The newly-registered account's public coordinates — adapters map this to
/// the HTTP `201 Created` JSON or the `RegisterReply` wire type.
#[derive(Debug)]
pub struct RegisterOutcome {
    /// 32-byte actor public key (adapters hex-encode).
    pub actor_id: [u8; 32],
    pub handle: String,
    pub domain: String,
    pub tier: String,
    /// `https://{domain}/api/v1`.
    pub node_url: String,
    /// Subhandle addresses (`handle@domain`, `@handle.domain`); empty when
    /// subhandles are disabled.
    pub addresses: Vec<String>,
}

/// Subhandle addresses (`handle@domain`, `@handle.domain`) — empty unless the
/// nest's client-set `subhandles` policy is on. The live value comes from
/// `AppState.subhandles` (boot-resolved from the `nest_subhandles` DB row, else
/// the `config.nest.subhandles` seed); the caller reads it and passes it in.
///
/// `pub(crate)` — also used by `discovery_core`'s `fauna.nest.info` reply,
/// which builds the same address list for the same handle/domain/policy.
pub(crate) fn subhandle_addresses(subhandles: bool, handle: &str, domain: &str) -> Vec<String> {
    if subhandles {
        vec![format!("{handle}@{domain}"), format!("@{handle}.{domain}")]
    } else {
        vec![]
    }
}

/// Self-service registration — the body of `POST /api/v1/register` minus the
/// HTTP-only per-IP rate-limit. Validates the handle, actor_id, timestamp
/// (±30 s) and the signature over `actor_id ‖ handle ‖ domain ‖ timestamp_be`,
/// enforces the registration policy (open / invite / free-cap / handle
/// availability + cooldown), creates the user, and fires the best-effort DNS +
/// FTS side effects. Behavior is preserved exactly as `post_register` did it.
pub async fn register_core(
    state: &AppState,
    actor_id_hex: &str,
    handle: &str,
    timestamp_ms: u64,
    signature_hex: &str,
    invite_code: Option<&str>,
    age_claim: Option<&fauna_protocol::age::AgeClaim>,
) -> Result<RegisterOutcome, RegisterError> {
    let domain = state.handle_domain();

    // The registration posture + the orthogonal free-tier ceiling, read once so a
    // concurrent `fauna.admin.set_registration_mode` can't flip the gate between
    // the endpoint check and the cap check below.
    let (mode, max_free_users) = *state.registration_mode.read().await;

    if mode == fauna_protocol::node_policy::RegistrationMode::Closed {
        return Err(RegisterError::RegistrationClosed);
    }

    // Validate handle.
    validate_handle(handle).map_err(RegisterError::InvalidRequest)?;
    if state
        .auth
        .registration
        .reserved_handles
        .iter()
        .any(|r| r == handle)
    {
        return Err(RegisterError::InvalidRequest("handle is reserved"));
    }

    // Parse actor_id.
    let actor_bytes = parse_actor_id(actor_id_hex)
        .ok_or(RegisterError::InvalidRequest("invalid actor_id hex"))?;

    // Validate timestamp.
    let now_ms = fauna_core::data::Timestamp::now_millis();
    if timestamp_ms.abs_diff(now_ms) > MAX_TIMESTAMP_DRIFT_MS {
        return Err(RegisterError::InvalidRequest(
            "timestamp too far from server time",
        ));
    }

    // Verify signature over (actor_id || handle || <domain> || timestamp). The
    // signed domain is whichever active local domain the client reached the nest
    // via — a user who found the nest as `bob@domain2` signs over `domain2`, not
    // the primary (multi-domain handles — `mail-multidomain.md` § Registration).
    // The domain is NOT carried on the wire (`build_register_request` bakes it
    // into the signature only), so the server tries each candidate and accepts
    // the first that verifies: the identity domain (`handle_domain()`, always
    // tried — the sole candidate on a box with no `mail_domains` rows yet) plus
    // every active local domain. The handle is stored bare + globally unique
    // regardless of which domain signed it.
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(RegisterError::InvalidRequest("invalid signature hex")),
    };
    let mut candidate_domains = vec![domain.clone()];
    if let Ok(active) = state.db.list_active_mail_domains().await {
        for d in active {
            if !candidate_domains.contains(&d.domain_name) {
                candidate_domains.push(d.domain_name);
            }
        }
    }
    let signature_ok = candidate_domains.iter().any(|cand| {
        // Domain-tagged + length-prefixed: the
        // single-source builder is what the client signed with, applied once
        // per candidate domain — one trial each, tagged-only.
        let msg = fauna_protocol::account::register_signed_message(
            &actor_bytes,
            handle,
            cand,
            timestamp_ms,
        );
        fauna_core::identity::verify_detached(&actor_bytes, &msg, &sig_bytes)
    });
    if !signature_ok {
        return Err(RegisterError::SignatureFailed);
    }

    // Supersession consult, after the signature and before the account gates
    // (`auth_core::successor_of` argues the placement): a retired key is
    // refused whether or not its handle-less `users` row still exists.
    match crate::auth_core::successor_of(state, &actor_bytes).await {
        Ok(None) => {}
        Ok(Some(new_actor_id)) => return Err(RegisterError::Superseded { new_actor_id }),
        Err(_) => return Err(RegisterError::Internal("internal error".into())),
    }

    // Check actor not already registered — a suspended one included, by ruling
    // (`login.md` § Errors, the registration doors' accepted exception to the
    // opaque `not_registered` code): the door answers `actor_exists` for every
    // `users` row, and a suspended account's one way back is the admin's
    // Restore. `is_actor_registered` reads no standing on purpose.
    match state.db.is_actor_registered(&actor_bytes).await {
        Ok(true) => return Err(RegisterError::ActorAlreadyRegistered),
        Err(e) => {
            tracing::error!("registration db error: {e}");
            return Err(RegisterError::Internal("internal error".into()));
        }
        _ => {}
    }

    // Check handle not already taken.
    match state.db.resolve_handle(handle).await {
        Ok(Some(_)) => return Err(RegisterError::HandleTaken),
        Err(e) => {
            tracing::error!("handle resolve error: {e}");
            return Err(RegisterError::Internal("internal error".into()));
        }
        Ok(None) => {}
    }

    // Check handle not in cooldown for a different actor.
    match state.db.check_handle_cooldown(handle, &actor_bytes).await {
        Ok(false) => return Err(RegisterError::HandleCooldown),
        Err(e) => {
            tracing::error!("handle cooldown check error: {e}");
            return Err(RegisterError::Internal("internal error".into()));
        }
        _ => {}
    }

    // ── The account age band (family-safety.md § The account age band;
    //    public-mode.md § Registration Modes → *Age at registration*) ──
    //
    // Everything claim-only-dependent runs BEFORE the invite code is consumed,
    // so a refusal here never burns a code use. The band token is validated
    // closed-set (the nest refuses what it cannot name), and an attestation is
    // verified against the platform root + the nest-minted single-use nonce
    // (`age_attest`) before anything believes it.
    let claim_band = match age_claim {
        Some(claim) => Some(
            fauna_protocol::age::AgeBand::from_wire(&claim.band)
                .ok_or(RegisterError::InvalidRequest("unknown age band"))?,
        ),
        None => None,
    };
    // Three outcomes (`family-safety.md` § The account age band → *An
    // attestation the nest cannot check*): verified earns the provenance, a
    // failed check refuses, and an attestation this build cannot check leaves
    // the claim exactly as declared-only — `None` here, like no attestation.
    let attested_provenance = match age_claim.and_then(|c| c.attestation.as_ref()) {
        Some(attestation) => match crate::age_attest::verify_age_attestation(
            state,
            attestation,
            // `claim_band` is Some whenever the claim (and so its
            // attestation) exists — the parse above ran first.
            claim_band.expect("attestation implies a claim band"),
            &actor_bytes,
        )
        .await
        .map_err(RegisterError::AgeAttestationInvalid)?
        {
            crate::age_attest::AttestationOutcome::Verified(provenance) => Some(provenance),
            crate::age_attest::AttestationOutcome::CannotCheck => None,
        },
        None => None,
    };
    // The admin require-knob: ON refuses every self-service admission — this
    // whole ceremony, open registration and code redemption alike — that
    // carries no VERIFIED attested claim (declared-only does not satisfy it;
    // `public-mode.md` § Age at registration). Checked before the code is
    // consumed for the same no-burn reason.
    if attested_provenance.is_none() && *state.age_verification_required.read().await {
        return Err(RegisterError::AgeVerificationRequired);
    }
    // The store-says-minor refusal, pre-consumption arm: a minor claim
    // redeeming a code with no guardian designation would land unsupervised —
    // refuse via a non-consuming peek so the code survives for a
    // guardian-mediated retry. (A benign TOCTOU: codes are immutable rows
    // apart from `uses_left`, so the peeked designation cannot change under
    // us.) The no-code arm is checked after resolution below.
    if let (Some(band), Some(code)) = (claim_band, invite_code)
        && band.is_minor()
    {
        match state.db.peek_invite_code(code).await {
            Ok(Some(grant)) if grant.guardian_actor.is_none() => {
                return Err(RegisterError::GuardianAdmissionRequired);
            }
            // A missing/spent code falls through to the resolution below,
            // which owns that refusal's shape.
            Ok(_) => {}
            Err(e) => {
                tracing::error!("invite code peek error: {e}");
                return Err(RegisterError::Internal("internal error".into()));
            }
        }
    }

    // Determine tier (and a supervised admission's guardian designation +
    // age band — `family-safety.md` § Wire & data shape + § The account age
    // band).
    let mut guardian: Option<Vec<u8>> = None;
    let mut code_band: Option<String> = None;
    let mut membership_admission: Option<MembershipAdmission> = None;
    let tier = if let Some(code) = invite_code {
        match state.db.validate_invite_code(code).await {
            Ok(Some(grant)) => {
                guardian = grant.guardian_actor;
                code_band = grant.age_band;
                grant.tier
            }
            Ok(None) => {
                // The invite-code lookup missed — a paid membership claim code
                // doubles as an invite code (monetization.md § Pillar 4 Rail C
                // step 1). Consult the payment-claim store; a valid membership
                // claim admits at the linked quota tier. Closed mode already
                // refused this ceremony at the top gate; the free cap is bypassed
                // because its check lives only in the no-invite branch below —
                // paid/coded admissions are not free-tier accounts.
                match resolve_membership_claim(state, code, &actor_bytes).await? {
                    Some(admission) => {
                        let t = admission.admin_tier.clone();
                        membership_admission = Some(admission);
                        t
                    }
                    None => {
                        return Err(RegisterError::InvalidRequest(
                            "invalid or expired invite code",
                        ));
                    }
                }
            }
            Err(e) => {
                tracing::error!("invite code error: {e}");
                return Err(RegisterError::Internal("internal error".into()));
            }
        }
    } else {
        if mode == fauna_protocol::node_policy::RegistrationMode::InviteRequired {
            return Err(RegisterError::InviteRequired);
        }
        // The free-tier ceiling is orthogonal to the mode — it caps free accounts
        // in `Open` and `InviteRequired` alike (`public-mode.md` § Registration Modes).
        if let Some(max) = max_free_users {
            match state.db.count_users_by_tier("free").await {
                Ok(count) if count as u64 >= max => return Err(RegisterError::FreeLimitReached),
                Err(e) => {
                    tracing::error!("user count error: {e}");
                    return Err(RegisterError::Internal("internal error".into()));
                }
                _ => {}
            }
        }
        "free".to_string()
    };

    // Supervised admission: the admitted actor must never equal the guardian
    // (supervised-by-self is unrepresentable) — a clean typed error, not a
    // UNIQUE-constraint mapping. The link itself is written inside the same
    // transaction as the user row below.
    if guardian.as_deref() == Some(actor_bytes.as_slice()) {
        return Err(RegisterError::InvalidRequest(
            "the guardian cannot redeem their own supervised invite code",
        ));
    }

    // The store-says-minor refusal, post-resolution arm — covers the paths the
    // pre-consumption peek could not see (no code at all, or a membership
    // claim): a minor claim on an admission that resolved UNSUPERVISED is
    // refused toward guardian-mediated admission. Never fires for a
    // guardian-designated code (that IS the intended path for a minor).
    if guardian.is_none() && claim_band.is_some_and(|b| b.is_minor()) {
        return Err(RegisterError::GuardianAdmissionRequired);
    }

    // Resolve the band row this admission mints (`family-safety.md` § The
    // account age band):
    //  - a guardian-designated band wins outright (provenance
    //    `guardian-asserted`) — an attested claim CORROBORATES the admitting
    //    guardian's judgment and never overrides it, in either direction;
    //  - otherwise a VERIFIED attested claim mints its band with `attested-*`
    //    provenance (a supervised admission whose code named no band included:
    //    the defaults dial then keys on the attested band);
    //  - otherwise nothing: an open-mode self-registration IS `18+`/`none` by
    //    construction (no row — public-mode.md § Age at registration), and a
    //    declared-only claim mints nothing (it exists for the refusal above
    //    and the request-row signal, never as a stored fact).
    let minted_age: Option<(String, String)> = if let Some(band) = &code_band {
        Some((
            band.clone(),
            fauna_protocol::age::AgeBandProvenance::GuardianAsserted
                .as_str()
                .to_string(),
        ))
    } else if let (Some(band), Some(provenance)) = (claim_band, attested_provenance) {
        Some((band.as_str().to_string(), provenance.as_str().to_string()))
    } else {
        None
    };

    // Create user, handle, and (for a supervised admission) the guardianship
    // link + its policy row — banded-defaults-keyed when a band rode the
    // admission — plus the band row itself, in a single transaction.
    if let Err(e) = state
        .db
        .create_user_with_handle_and_age(
            &actor_bytes,
            &tier,
            handle,
            guardian.as_deref(),
            minted_age.as_ref().map(|(b, p)| (b.as_str(), p.as_str())),
        )
        .await
    {
        let debug_msg = format!("{e:?}");
        if debug_msg.contains("UNIQUE") {
            return Err(RegisterError::HandleTaken);
        }
        tracing::error!("create user error: {e}");
        return Err(RegisterError::Internal("internal error".into()));
    }

    // A membership-claim admission (monetization.md § Pillar 4 Rail C step 1):
    // record the membership subscription — a `subscribers` row under the payee's
    // tier carrying the claim's paid window. The claim was already stamped
    // redeemed by `resolve_membership_claim` (redeem-first, so a double-spend
    // can't create two accounts). Best-effort by design: the account exists at
    // the linked quota tier (the load-bearing admission); a failed roster write
    // must not fail the registration the user just completed — their client
    // re-syncs the roster, and the paid window survives on the redeemed claim row.
    if let Some(admission) = &membership_admission {
        if let Err(e) = state
            .db
            .add_subscriber(&admission.payee, &actor_bytes, &admission.tier_name, None)
            .await
        {
            tracing::error!("membership admission: add_subscriber failed: {e:?}");
        } else if let Err(e) = state
            .db
            .set_subscriber_valid_until(
                &admission.payee,
                &actor_bytes,
                &admission.tier_name,
                admission.valid_until,
            )
            .await
        {
            tracing::error!("membership admission: set_subscriber_valid_until failed: {e:?}");
        } else if let Err(e) = state
            .db
            .stamp_membership_admission(
                &admission.payee,
                &actor_bytes,
                &admission.tier_name,
                &admission.admin_tier,
                &admission.lapse_tier,
            )
            .await
        {
            // Same best-effort posture as the two writes above: the account
            // exists at the linked quota tier (the load-bearing admission).
            // An unstamped row simply never lapses — the pre-existing failure
            // mode, not a new one.
            tracing::error!("membership admission: stamp_membership_admission failed: {e:?}");
        }
    }

    // The Search corpus's profile row was written with the account, in
    // `create_user_with_handle_and_age` (`db::fts::sync_profile_row`).

    tracing::info!(handle = %handle, domain = %domain, "new user registered");

    let addresses = subhandle_addresses(*state.subhandles.read().await, handle, &domain);
    let node_url = format!("https://{domain}/api/v1");

    Ok(RegisterOutcome {
        actor_id: actor_bytes,
        handle: handle.to_string(),
        domain,
        tier,
        node_url,
        addresses,
    })
}

/// A resolved, just-redeemed **membership** claim code (monetization.md
/// § Pillar 4 Rail C step 1: a paid claim code doubles as an invite code).
/// Returned by [`resolve_membership_claim`] once the claim is atomically stamped
/// redeemed, so registration proceeds knowing the admission is committed.
struct MembershipAdmission {
    /// The payee (admin) who minted the claim — the `subscribers` row's author.
    payee: [u8; 32],
    /// The subscription tier the claim entitles (the membership tier).
    tier_name: String,
    /// The linked quota tier the admitted actor is assigned (`users.tier`).
    admin_tier: String,
    /// The linked quota tier this admission lapses to, frozen onto the
    /// `subscribers` row so a later re-point/clear of the designation cannot
    /// strand the member (monetization.md § Pillar 4 Rail C step 3).
    lapse_tier: String,
    /// The claim's paid window (epoch seconds; `None` = no expiry until refunded).
    valid_until: Option<i64>,
}

/// A payment claim's peek-only membership check — the shared half of
/// [`resolve_membership_claim`] and `invite_core::verify_invite_code_core`.
/// Neither redeems: the caller decides what "a hit" means (register_core
/// redeems atomically; the wizard's pre-submit verify must NOT consume a
/// one-shot claim before the user actually registers, or a user who verifies
/// then abandons the form burns their own claim).
pub(crate) struct PeekedMembershipClaim {
    payee: [u8; 32],
    tier_name: String,
    admin_tier: String,
    lapse_tier: String,
    valid_until: Option<i64>,
}

/// Does `code` name a valid, unredeemed, unvoided payment claim for a tier its
/// payee has designated a **membership** tier (monetization.md § Pillar 4 Rail
/// C)? A **non**-membership claim (an ordinary content-tier claim, redeemable
/// only by an already-logged-in client via `fauna.payments.claims.redeem`)
/// is not a hit — it does not admit at registration and must not verify as one.
pub(crate) async fn peek_membership_claim(
    state: &AppState,
    code: &str,
) -> anyhow::Result<Option<PeekedMembershipClaim>> {
    let claim = match state.db.get_payment_claim(code).await? {
        Some(c) => c,
        None => return Ok(None),
    };
    if claim.voided_at.is_some() || claim.redeemed_by.is_some() {
        return Ok(None);
    }
    let payee: [u8; 32] = match claim.author_id.as_slice().try_into() {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let designation = match state
        .db
        .get_membership_tier(&payee, &claim.tier_name)
        .await?
    {
        Some(d) => d,
        // A content-tier claim (no designation) never admits at registration.
        None => return Ok(None),
    };
    Ok(Some(PeekedMembershipClaim {
        payee,
        tier_name: claim.tier_name,
        admin_tier: designation.admin_tier,
        lapse_tier: designation.lapse_tier,
        valid_until: claim.valid_until,
    }))
}

/// Resolve an invite-code miss against the payment-claim store. If `code`
/// peeks as a hit ([`peek_membership_claim`]), atomically stamp it redeemed to
/// `redeemer` and return the admission. Redeem-FIRST (the `redeem_payment_claim`
/// guard is atomic) so one paid claim admits exactly one account — a lost
/// redeem race yields `None`, and the caller reports the same "invalid or
/// expired" error a bad invite code gets.
async fn resolve_membership_claim(
    state: &AppState,
    code: &str,
    redeemer: &[u8; 32],
) -> Result<Option<MembershipAdmission>, RegisterError> {
    let peeked = match peek_membership_claim(state, code)
        .await
        .map_err(|e| RegisterError::Internal(format!("{e}")))?
    {
        Some(p) => p,
        None => return Ok(None),
    };
    // Atomically stamp redeemed — the double-spend guard. A lost race → None,
    // reported as the generic "invalid or expired" refusal.
    if !state
        .db
        .redeem_payment_claim(code, redeemer)
        .await
        .map_err(|e| RegisterError::Internal(format!("{e}")))?
    {
        return Ok(None);
    }
    Ok(Some(MembershipAdmission {
        payee: peeked.payee,
        tier_name: peeked.tier_name,
        admin_tier: peeked.admin_tier,
        lapse_tier: peeked.lapse_tier,
        valid_until: peeked.valid_until,
    }))
}

// ── Emergency no-token account lockout ──────────────────────────────────────

/// Tolerated drift between the client-supplied lockout timestamp and server
/// time, in **seconds** (the recovery path uses second-granularity timestamps,
/// unlike `register`'s ms). Matches the legacy `session_routes::lockout`.
const LOCKOUT_TIMESTAMP_WINDOW_SECS: u64 = 300;
/// The emergency-lockout window — a **hard-coded product constant**, shared by
/// both lockout kinds (`fauna.account.lockout` here and the authed
/// `fauna.sessions.lockout` in `session_handlers`), ruled 2026-08-24.
///
/// Neither kind carries a duration (the unsigned `duration_secs` left the wire
/// 2026-09-24 in the compat-remnant sweep). It was
/// configuration-file theatre (`principles.md` § One configuration surface):
/// no app in all history has ever exposed a duration picker — no client calls
/// either kind at all — so the "choice" existed only on the wire, and on the
/// pre-identity kind it was a live escalation: the Ed25519 signature covers
/// `actor_id ‖ timestamp_be` and NOT the duration, so an on-path capture of a
/// 1-hour lockout could be re-issued as a 24-hour one inside the ±300 s
/// freshness window. A constant closed that seam and makes the re-lock
/// genuinely idempotent in effect.
///
/// 24 hours, the old clamp's maximum, on purpose: the ceremony is the panic
/// button for a believed key/device compromise, and its job is protective
/// containment until the owner completes a real remedy (device revocation,
/// seed-escrow restore, succession — all reachable while locked: the recovery
/// ceremonies are pre-identity and never behind the ordinary bearer auth). A
/// window that silently expires overnight defeats that intent; 24 h is still
/// bounded, so a false alarm cannot permanently brick the account.
pub const EMERGENCY_LOCKOUT_SECS: u64 = 86_400; // 24 hours

/// Transport-agnostic emergency-lockout failure. The adapter maps each variant
/// to an `RpcError` code (`fauna.account.*`) — the WS-RPC counterpart of the
/// status mapping the retired `session_routes::lockout` HTTP twin did.
#[derive(Debug)]
pub enum LockoutError {
    /// A 400-class validation failure — malformed actor_id, stale timestamp, or
    /// malformed signature/key. WS `fauna.account.invalid_request` (detail = msg).
    InvalidRequest(&'static str),
    /// Ed25519 signature verification failed. WS `fauna.account.signature_failed`.
    SignatureFailed,
    /// Server-side failure (`tracing::error!` carries the cause). WS
    /// `fauna.protocol.internal`.
    Internal(String),
    /// The identity was succeeded and this key authorizes nothing here any
    /// more (`identity-succession.md:71` — the lockout kind is named
    /// explicitly among the ceremonies that must refuse). WS
    /// `fauna.auth.superseded`.
    Superseded { new_actor_id: [u8; 32] },
}

/// Emergency lockout — the body of `POST /api/v1/account/lockout`. Authenticates
/// by a domain-tagged Ed25519 signature over
/// `fauna_protocol::account::account_lockout_signed_message` (**no bearer**),
/// validates timestamp freshness (±300 s), revokes every token for the actor,
/// and sets `locked_until = now + EMERGENCY_LOCKOUT_SECS`. Returns the applied
/// `locked_until` (Unix seconds). Takes no duration — see
/// [`EMERGENCY_LOCKOUT_SECS`].
pub async fn lockout_core(
    state: &AppState,
    actor_id_hex: &str,
    timestamp_secs: u64,
    signature_hex: &str,
) -> Result<i64, LockoutError> {
    // Parse actor_id.
    let actor_bytes =
        parse_actor_id(actor_id_hex).ok_or(LockoutError::InvalidRequest("invalid actor_id hex"))?;

    // Validate timestamp freshness (±300 s).
    let now_secs = fauna_core::data::Timestamp::now_secs() as u64;
    if timestamp_secs.abs_diff(now_secs) > LOCKOUT_TIMESTAMP_WINDOW_SECS {
        return Err(LockoutError::InvalidRequest(
            "timestamp too far from server time",
        ));
    }

    // Verify the actor signature over the seconds timestamp: domain-tagged only
    // (`ACCOUNT_LOCKOUT_V1 ‖ actor_id ‖ timestamp_be`) — structural separation
    // from claim-admin / login instead of the seconds-vs-ms value range.
    verify_account_lockout_signature(&actor_bytes, timestamp_secs, signature_hex).map_err(|e| {
        // Preserve the twin's split: malformed signature hex is a 400-class
        // InvalidRequest; a well-formed but non-matching signature is
        // SignatureFailed (403). `verify_detached_hex` returns the former string.
        if e == "invalid signature hex" {
            LockoutError::InvalidRequest("invalid signature hex")
        } else {
            LockoutError::SignatureFailed
        }
    })?;

    // Supersession consult (`identity-succession.md:71`). Without it the
    // *thief* keeps a working weapon after losing the account: the old seed
    // still signs these 40 bytes perfectly, so they could re-lock the recovered
    // identity — except the lock lands on the old actor id, which by then owns
    // nothing. Refusing is still the right answer rather than a harmless no-op:
    // the succeeded owner who reaches for the panic button deserves to be told
    // their account already moved, and told where.
    if let Some(row) = state
        .db
        .succession_for(&actor_bytes[..])
        .await
        .map_err(|e| {
            tracing::error!("lockout: succession consult failed: {e}");
            LockoutError::Internal("database error".into())
        })?
    {
        let new_actor_id: [u8; 32] =
            row.new_actor_id.as_slice().try_into().map_err(|_| {
                LockoutError::Internal("stored successor id is not 32 bytes".into())
            })?;
        return Err(LockoutError::Superseded { new_actor_id });
    }

    // The hard-coded window ([`EMERGENCY_LOCKOUT_SECS`]'s escalation rationale).
    let locked_until = (now_secs + EMERGENCY_LOCKOUT_SECS) as i64;

    state
        .auth
        .token_store
        .revoke_actor(&fauna_core::identity::ActorId(actor_bytes))
        .await;

    state
        .db
        .set_locked_until(&actor_bytes, Some(locked_until))
        .await
        .map_err(|e| {
            tracing::error!("lockout: set_locked_until failed: {e}");
            LockoutError::Internal("database error".into())
        })?;

    // The no-token recovery channel exists for "my key is compromised" — so it
    // must also close the sockets the attacker already holds, not just the
    // tokens they would need to open a new one (`transport.md` § Revocation
    // teardown).
    state.close_actor_sockets(&actor_bytes);

    tracing::info!(
        actor = %hex::encode(actor_bytes),
        locked_until,
        "emergency account lockout applied (WS-RPC)"
    );

    Ok(locked_until)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Build the register signature over the tagged, length-prefixed
    /// `register_signed_message` (the client-side `build_register_request`
    /// ceremony; the `domain` is baked into the signature but never sent on the
    /// wire).
    fn sign_register(sk: &SigningKey, handle: &str, domain: &str, ts_ms: u64) -> String {
        let vk = sk.verifying_key();
        let msg =
            fauna_protocol::account::register_signed_message(vk.as_bytes(), handle, domain, ts_ms);
        hex::encode(sk.sign(&msg).to_bytes())
    }

    /// `identity-succession.md` § Enforcement on the home nest, step 4 — the
    /// registration doors. The refusal of a retired key must rest on
    /// `actor_successions`, never on the retired identity's handle-less `users`
    /// row: that row goes with the chain when the successor is deleted
    /// (`account-data-plane.md` § Nest-side requirements item 1), so this test
    /// runs the production deletion path FIRST and only then knocks on all
    /// three doors with the retired key. Before the doors consulted the table
    /// themselves, the first door admitted it.
    #[tokio::test]
    async fn a_retired_key_is_refused_at_every_registration_door_after_its_successor_is_deleted() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = std::sync::Arc::new(crate::routes::AppState::for_test(db.clone()));
        *state.registration_mode.write().await =
            (fauna_protocol::node_policy::RegistrationMode::Open, None);
        let old_sk = SigningKey::from_bytes(&[0xA1u8; 32]);
        let old: [u8; 32] = *old_sk.verifying_key().as_bytes();
        let new: [u8; 32] = [0xB2u8; 32];
        db.create_user_with_handle(&old, "free", "alice", None)
            .await
            .unwrap();
        db.record_succession(&old, &new, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        crate::pending_actions::finalize_user_deletion(&state, &new)
            .await
            .unwrap();
        // The premise this test exists for: neither `users` row survives, so
        // `actor_exists` cannot be what refuses below.
        assert!(!db.is_actor_registered(&old).await.unwrap());
        assert!(!db.is_actor_registered(&new).await.unwrap());
        let old_hex = hex::encode(old);

        // Door 1: open registration.
        let domain = state.handle_domain();
        let ts = now_ms();
        let sig = sign_register(&old_sk, "alice2", &domain, ts);
        let res = register_core(&state, &old_hex, "alice2", ts, &sig, None, None).await;
        let Err(RegisterError::Superseded { new_actor_id }) = res else {
            panic!("fauna.account.register admitted a retired key");
        };
        assert_eq!(new_actor_id, new);

        // Door 2: an invite request.
        let msg = fauna_protocol::invite::invite_submit_signed_message(&old, "alice2", "hi", ts);
        let sig = hex::encode(old_sk.sign(&msg).to_bytes());
        let res = crate::invite_core::submit_invite_request_core(
            &state, &old_hex, "alice2", "hi", ts, &sig, None,
        )
        .await;
        let Err(crate::invite_core::InviteError::Superseded { new_actor_id }) = res else {
            panic!("fauna.account.invite_request.submit admitted a retired key");
        };
        assert_eq!(new_actor_id, new);

        // Door 3: the admin claim. Refused before the claim code is even read,
        // so no code file is needed here.
        let ts_secs = fauna_core::data::Timestamp::now_secs() as u64;
        let msg = fauna_protocol::claim::claim_admin_signed_message(&old, ts_secs);
        let sig = hex::encode(old_sk.sign(&msg).to_bytes());
        let res = crate::claim_core::claim_admin_core(
            &state, &old_hex, ts_secs, &sig, "code", "admin", None,
        )
        .await;
        let Err(crate::claim_core::ClaimError::Superseded { new_actor_id }) = res else {
            panic!("fauna.auth.claim_admin admitted a retired key");
        };
        assert_eq!(new_actor_id, new);
    }

    fn now_ms() -> u64 {
        fauna_core::data::Timestamp::now_millis()
    }

    /// A registration whose Ed25519 signature is over a **secondary** active
    /// local domain (not the primary/identity domain) must be accepted — a user
    /// who reached the nest via `bob@domain2` signs over `domain2`, and the
    /// domain is not carried on the wire, so the server tries each active local
    /// domain (`mail-multidomain.md` § Registration). A signature over a
    /// **non-local** domain must still be rejected. The handle stays canonical
    /// under the identity (primary) domain regardless.
    #[tokio::test]
    async fn register_accepts_signature_over_any_active_local_domain() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db.clone());
        // `for_test` mirrors production's `Closed` default, so a test exercising
        // the *ceremony* must open registration explicitly.
        *state.registration_mode.write().await =
            (fauna_protocol::node_policy::RegistrationMode::Open, None);
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("primary.example".to_string())));
        db.add_mail_domain(
            "primary.example",
            true,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "domain2.example",
            false,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        // Signed over the SECONDARY active local domain → must be accepted.
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let actor_hex = hex::encode(sk.verifying_key().as_bytes());
        let ts = now_ms();
        let sig = sign_register(&sk, "bob", "domain2.example", ts);
        let out = register_core(&state, &actor_hex, "bob", ts, &sig, None, None)
            .await
            .expect("register signed over a secondary active local domain must succeed");
        assert_eq!(out.handle, "bob");
        assert_eq!(
            out.domain, "primary.example",
            "the handle is canonical under the identity (primary) domain"
        );

        // Signed over a NON-local domain → must be rejected.
        let sk2 = SigningKey::from_bytes(&[9u8; 32]);
        let actor2_hex = hex::encode(sk2.verifying_key().as_bytes());
        let ts2 = now_ms();
        let sig2 = sign_register(&sk2, "carol", "not-local.invalid", ts2);
        let err = register_core(&state, &actor2_hex, "carol", ts2, &sig2, None, None).await;
        assert!(
            matches!(err, Err(RegisterError::SignatureFailed)),
            "register signed over a non-local domain must fail with SignatureFailed"
        );
    }

    /// The DEFAULT reserved list is live and non-empty. `for_test` — like every
    /// production constructor — carries `RegistrationConfig::default()`, i.e.
    /// the shared `fauna_protocol::handle::RESERVED_HANDLES` constant, with no
    /// knob feeding the field. This pins the property the deleted
    /// `--reserved-handle` flag's plumbing silently broke: the flag's empty
    /// clap default used to REPLACE this list on every boot that didn't pass
    /// it (i.e. every real deployment), leaving `postmaster`/`admin`/`www`/
    /// `mta-sts` mintable by strangers on an open nest.
    #[tokio::test]
    async fn register_refuses_reserved_handles_on_the_default_config() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db.clone());
        *state.registration_mode.write().await =
            (fauna_protocol::node_policy::RegistrationMode::Open, None);
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("primary.example".to_string())));
        db.add_mail_domain(
            "primary.example",
            true,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();

        let sk = SigningKey::from_bytes(&[11u8; 32]);
        let actor_hex = hex::encode(sk.verifying_key().as_bytes());
        let ts = now_ms();
        let sig = sign_register(&sk, "postmaster", "primary.example", ts);
        let err = register_core(&state, &actor_hex, "postmaster", ts, &sig, None, None).await;
        assert!(
            matches!(
                err,
                Err(RegisterError::InvalidRequest("handle is reserved"))
            ),
            "a default-config nest must refuse a reserved handle, got {:?}",
            err.as_ref().err()
        );

        // Control: the same ceremony with an unreserved handle succeeds.
        let sk2 = SigningKey::from_bytes(&[12u8; 32]);
        let actor2_hex = hex::encode(sk2.verifying_key().as_bytes());
        let ts2 = now_ms();
        let sig2 = sign_register(&sk2, "alice", "primary.example", ts2);
        register_core(&state, &actor2_hex, "alice", ts2, &sig2, None, None)
            .await
            .expect("an unreserved handle must register on the same nest");
    }
}
