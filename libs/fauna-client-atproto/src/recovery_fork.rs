//! The 72 h senior-key **recovery fork** — the in-product remedy for a
//! box-authored PLC operation (`atproto-pds-bridge.md` § State & data shape,
//! *The 72 h recovery-fork contest*; ten ratified decisions, 2026-08-02).
//!
//! The custody split (S4-C) gives the user a rotation key that is *senior* to
//! every bridge-reachable key, but seniority is a **contest position, not the
//! box's incapability**: the bridge's junior key is listed for the identity's
//! whole life, so a compromised box can author operations up to and including
//! a tombstone. What the user gets is PLC's recovery rule — a fork signed by a
//! strictly-higher-priority rotation key of the fork-point op, submitted
//! within 72 h of the op it displaces, nullifies that op and every descendant.
//! Until this module existed, detection was built and the remedy was not; a
//! box op that went uncontested for 72 h simply stood.
//!
//! **Detection and remedy are two readings of ONE verdict** (decision 1). This
//! module never forms its own opinion about which op is bad: it calls
//! [`verify_audit_log`] — the same function the alarm fires from — and takes
//! the standing op immediately before the violation it reports as the fork
//! point. Everything from the violation onward is displaced, *including*
//! good-looking later ops, which chain through the violation and were authored
//! under the attacker's authority (the sandwich case).
//!
//! **The op is the fork point carried forward verbatim, minus box authority**
//! (decision 3). Every field of the fork-point op's published JSON survives —
//! known or unknown, because a contest must not silently strip a field a
//! future PLC schema added — with exactly three changes: `prev` = the fork
//! point's CID, `rotationKeys` = the fork point's list ∩ the held ring with
//! order preserved, and a fresh `sig`. The box's repo *signing* key
//! (`verificationMethods`) and the `atproto_pds` service entry are deliberately
//! **kept**: identity authority is severed, availability is not.
//!
//! **Nothing here talks to the nest** (decision 9). Build, sign, submit and
//! verify-back all run on the client's own connections, exactly as the custody
//! check does, so a hostile box is neither consulted about nor notified of its
//! own contest. And the scalar keeps having no export surface: the in-product
//! fork builder IS the remedy.
//!
//! I/O split mirrors [`crate::tombstone`]: [`contest_plan`], [`build_fork_op`]
//! and [`sign_fork_op`] are pure and are the units under test;
//! [`converge_contest`] is the one function that touches the network.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use fauna_core::data::{AtprotoContestIntent, AtprotoRotationKey};
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer as _;

use crate::genesis_verify::{
    MismatchReason, SeniorityVerdict, VerifyFailure, fetch_audit_log, parse_audit_log_with_raw,
    plc_directory_base_url, verify_audit_log,
};

/// PLC's recovery window: a fork must reach the directory within 72 h of the
/// operation it displaces. Advisory *here* — the directory's own ruling at
/// submit is the truth — so this drives what the ceremony shows and when the
/// plan stops offering a button, never a claim the client makes about
/// acceptance.
pub const CONTEST_WINDOW_SECS: u64 = 72 * 60 * 60;

/// Why a contest could not be built, signed or submitted.
#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    /// The directory could not be read. Quiet-retry class, exactly as for the
    /// seniority check.
    #[error("could not read the operation log: {0}")]
    Directory(#[from] VerifyFailure),
    /// A standing entry the contest needs to name carried no `cid`, so there
    /// is nothing to chain to or to scope a consent by. A misbehaving
    /// directory, not a user-reachable state.
    #[error("a standing operation carries no cid")]
    EntryWithoutCid,
    /// The fork-point operation's published JSON is not an object, so it
    /// cannot be carried forward. Also a misbehaving directory.
    #[error("the fork-point operation is not a JSON object")]
    ForkPointNotAnObject,
    /// The held scalar did not parse as a P-256 secret key.
    #[error("the held rotation key is not a valid P-256 scalar")]
    BadRotationKey,
    /// The ring does not hold the key the plan selected. Unreachable when the
    /// caller passes the same ring it planned from.
    #[error("the held ring no longer carries the signing key the plan selected")]
    SignerNotHeld,
    /// The fork point's `rotationKeys[0]` is not a key this client holds, so
    /// no held key out-ranks the displaced op's signer and the directory would
    /// reject the fork.
    ///
    /// **Unreachable given the log that produced the verdict, and deliberately
    /// checked anyway.** [`verify_audit_log`] gates *every* standing op on
    /// index-0 membership and stops at the first that fails, so every op
    /// before the violation — the fork point among them — already passed the
    /// identical test; a log whose fork point fails it here is
    /// self-contradictory. It is checked rather than assumed because this is
    /// the check that **licenses a signature**, and a license must be
    /// established locally, never inherited from a caller's verdict: the day
    /// someone plans from a different ring than the one that alarmed, the
    /// fork must refuse rather than sign something the directory rejects.
    /// Diagnostic/quiet-retry class, not a user-facing "you cannot contest" —
    /// the honest not-contestable states are [`ContestEligibility`]'s.
    #[error("the fork point's senior rotation key is not held (published: {published:?})")]
    ForkPointSeniorNotHeld { published: Option<String> },
    /// Canonical dag-cbor encoding failed — in practice only reachable if a
    /// future PLC field carries a value dag-cbor forbids (a float), in which
    /// case refusing to sign is the correct answer.
    #[error("could not encode the fork operation: {0}")]
    Encode(String),
    // NOTE: a failed chain authentication is NOT an error — it is
    // [`ContestEligibility::Unauthenticated`], a planned no-remedy state. The
    // close returned it here instead, and its own doc comment claimed
    // the ceremony "still renders" for such a log; it did not, because an `Err`
    // never reaches the card, so the one surface that could explain a
    // hostile-directory attack went blank while the banner still shouted. Both
    // shapes refuse to sign; only the eligibility also speaks.
    /// The directory refused the submission, or was unreachable during it.
    #[error("the directory refused the fork: {0}")]
    Submit(String),
}

/// What one read of the log says about contesting it. Pure — the unit under
/// test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContestPlan {
    /// Custody holds across the whole standing chain, or the standing
    /// tombstone is the user's own retirement. Nothing to contest, and no
    /// ceremony surface renders.
    ///
    /// Also the state a *completed* contest converges to: once the fork
    /// stands, the nullified suffix leaves the chain and the same
    /// [`verify_audit_log`] the alarm reads answers `Verified` — which is how
    /// the banner comes down with zero new mechanism (decision 8).
    NoViolation,
    /// A standing operation violates custody. Whether it can be fought is
    /// [`Violation::eligibility`].
    /// Boxed: `Violation` is far larger than the other variants, and an
    /// unboxed payload made every `ContestPlan` (including the common
    /// `NoViolation`) pay its size — `clippy::large_enum_variant`, which is
    /// denied workspace-wide.
    Violation(Box<Violation>),
}

/// The standing violation a contest would displace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// CID of the first standing op violating custody — the op the fork
    /// displaces, and the scope of the user's consent
    /// ([`AtprotoContestIntent::contested_op_cid`]).
    pub contested_op_cid: String,
    /// The verdict that named it, for the machine-composed detail line. The
    /// contest never re-derives this; it is feeder #1's own reading.
    pub reason: MismatchReason,
    /// Advisory 72 h deadline, unix seconds — the contested op's published
    /// `createdAt` plus [`CONTEST_WINDOW_SECS`]. `None` when the directory
    /// published no parseable timestamp, which deliberately does **not** block
    /// the contest: an unknown deadline is not a closed window, and the
    /// directory rules on lateness at submit either way.
    pub deadline_unix: Option<u64>,
    pub eligibility: ContestEligibility,
}

/// Whether, and how, a fork may be built for this violation.
///
/// The two not-contestable arms are honest states the ceremony renders as such
/// (decision 2) rather than a dead button — a user whose identity cannot be
/// recovered must be told so, not left clicking. They are exactly the states
/// `ui/atproto.md`'s contest card carries
/// (`state=contestable|window-closed|not-contestable`).
///
/// Decision 5's "a held key listed at a lower position is honestly
/// not-contestable" needs no arm of its own: an op whose `rotationKeys[0]` is
/// not held is itself a custody violation, so it becomes the *contested* op
/// (or, at the genesis, [`Self::GenesisViolation`]) rather than a fork point
/// this plan would offer. Pinned by
/// `a_held_key_below_index_zero_never_licenses_a_contest`; the residual
/// license check lives in [`ForkError::ForkPointSeniorNotHeld`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContestEligibility {
    /// A fork can be built and signed now.
    Contestable(ForkPoint),
    /// The violation is at standing index 0: there is no earlier op to chain
    /// to. That is the TOFU failure, whose remedy remains abandoning the DID
    /// (a fresh mint — nothing was ever burned for it).
    GenesisViolation,
    /// The advisory 72 h window has already closed.
    WindowClosed,
    /// The standing chain through the contested op does not authenticate: a
    /// CID that does not bind its operation, a broken `prev` link, a signature
    /// from no key the previous op lists, or a genesis that does not derive
    /// this DID. Nothing may be signed against such a log —
    /// but it is a **no-remedy state, not a read failure**, and the difference
    /// is what this variant exists to carry.
    ///
    /// Detection stays deliberately permissive, so the alarm has already fired
    /// and the user has already been sent to the ceremony surface. Reporting
    /// this as an error instead would leave that surface blank — the user told
    /// their identity may be seized, and shown nothing where the explanation
    /// belongs (decision 2: *the ceremony surface renders the honest no-remedy
    /// state rather than a dead button*). It is also durable: unlike an
    /// unreachable directory, re-reading the same log will not change the
    /// answer, so quiet-and-retry is the wrong shape for it.
    ///
    /// The failure is carried so a caller can pin *why* the log was rejected —
    /// no surface renders it, deliberately: the user-facing copy says the
    /// record does not check out, never which cryptographic check failed.
    Unauthenticated(crate::plc_chain::ChainFailure),
}

/// The standing op a fork chains to, and everything needed to build one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPoint {
    /// CID the fork op's `prev` names.
    pub cid: String,
    /// The fork-point op's published JSON, untouched — the verbatim source
    /// [`build_fork_op`] carries forward.
    pub op: serde_json::Value,
    /// The held `did:key` at the fork point's `rotationKeys[0]`: the one key
    /// whose signature the directory will accept over every other possible
    /// signer of the displaced op.
    pub signer_did_key: String,
    /// What the fork publishes as `rotationKeys`: the fork point's list ∩ the
    /// held ring, order preserved. In product terms the user's senior key
    /// alone — the bridge's junior key and any unknown key are pruned, which
    /// is what locks the box out of this DID's **rotation** layer permanently
    /// (decision 4): no bridge-reachable key can author another PLC operation
    /// for this DID.
    ///
    /// ⚠ **Not the whole PLC document**. The fork
    /// carries `verificationMethods` and `services` forward verbatim by
    /// deliberate design (decision 3 — identity authority is severed,
    /// availability is not), so a compromised box keeps the repo *signing* key
    /// and can still author posts as the user afterwards. Replacing that key is
    /// a separate, unbuilt gesture; the user-facing copy
    /// (`atproto_settings.contest_detail_contestable`) says so in as many
    /// words, and this comment must not drift back to the broader claim.
    pub rotation_keys: Vec<String>,
}

/// Plan a contest from a raw `/{did}/log/audit` body. Pure — no I/O, no key
/// material, the unit under test.
///
/// `held_did_keys` is the whole held ring's public half; `now_unix_secs` drives
/// only the advisory window check.
///
/// `did` is the identity being contested, and it is **load-bearing rather than
/// informational**: a did:plc is the hash of its own genesis operation, so it
/// is the only thing that roots the log in something the directory cannot
/// choose. Before any [`ContestEligibility::Contestable`] is
/// offered, the standing chain from that genesis through the contested op must
/// authenticate — see [`crate::plc_chain::verify_standing_chain`].
pub fn contest_plan(
    audit_json: &[u8],
    did: &str,
    held_did_keys: &[String],
    now_unix_secs: u64,
) -> Result<ContestPlan, ForkError> {
    // Decision 1: the fork point comes from the SAME verdict the alarm fires
    // from. Calling the detector rather than re-walking the log is what makes
    // "the contest never invents its own notion of bad" structural instead of
    // reviewed — the two readings cannot drift.
    let reason = match verify_audit_log(audit_json, held_did_keys)? {
        SeniorityVerdict::Verified { .. } => return Ok(ContestPlan::NoViolation),
        // The user's own retirement is the expected end state, not an attack.
        SeniorityVerdict::Mismatch(MismatchReason::OwnRetirement { .. }) => {
            return Ok(ContestPlan::NoViolation);
        }
        SeniorityVerdict::Mismatch(reason) => reason,
    };
    // Decision 10 — the contest's scope is exactly feeder #1's other three
    // verdicts, each of which names the violating op's standing index.
    // (Feeder #3's rogue-handle alarm names no index and is a recorded
    // follow-on, not silently included.)
    let standing_index = match reason {
        MismatchReason::SeniorKeyDiffers { standing_index, .. }
        | MismatchReason::NotAPlcOperation { standing_index, .. }
        | MismatchReason::RetirementByUnheldKey { standing_index } => standing_index,
        MismatchReason::OwnRetirement { .. } => unreachable!("returned above"),
    };

    let rows = parse_audit_log_with_raw(audit_json)?;
    let standing: Vec<_> = rows.into_iter().filter(|(e, _)| !e.nullified).collect();
    let (contested_entry, _) = standing
        .get(standing_index)
        .ok_or(VerifyFailure::NoStandingOps)?;
    if contested_entry.cid.is_empty() {
        return Err(ForkError::EntryWithoutCid);
    }
    let contested_op_cid = contested_entry.cid.clone();
    let deadline_unix = contested_entry
        .created_at
        .as_deref()
        .and_then(parse_plc_timestamp)
        .map(|t| t + CONTEST_WINDOW_SECS);

    let violation = |eligibility| {
        Ok(ContestPlan::Violation(Box::new(Violation {
            contested_op_cid: contested_op_cid.clone(),
            reason: reason.clone(),
            deadline_unix,
            eligibility,
        })))
    };

    // Structural impossibility before temporal: telling a user the window
    // closed on a contest they never could have brought would be a lie of
    // emphasis, and the genesis case is the more fundamental of the two.
    //
    // Decision 2: standing index 0 leaves no earlier op to chain to.
    let Some(fork_index) = standing_index.checked_sub(1) else {
        return violation(ContestEligibility::GenesisViolation);
    };
    let (fork_entry, fork_raw) = &standing[fork_index];
    if fork_entry.cid.is_empty() {
        return Err(ForkError::EntryWithoutCid);
    }

    // Decision 5: eligibility is `rotationKeys[0]` and only `rotationKeys[0]`.
    // Made here, locally, off the log — never inherited from the verdict —
    // because this is the check that LICENSES a signature. Its failure arm is
    // unreachable against the log that produced the verdict and is an error
    // rather than an eligibility state; the reasoning is on
    // [`ForkError::ForkPointSeniorNotHeld`].
    let senior = fork_entry.operation.rotation_keys.first();
    let Some(signer_did_key) = senior.filter(|k| held_did_keys.contains(k)).cloned() else {
        return Err(ForkError::ForkPointSeniorNotHeld {
            published: senior.cloned(),
        });
    };

    if deadline_unix.is_some_and(|d| now_unix_secs > d) {
        return violation(ContestEligibility::WindowClosed);
    }

    // everything above ruled on what the directory SAID. Nothing below
    // may be signed until the log is authenticated against the DID itself.
    //
    // Through the contested op, not merely the fork point: a fabricated
    // violation spliced onto the user's real operations chains and CIDs
    // perfectly, so only its signature betrays it — and left unchecked it would
    // let an attacker with no compromise at all induce the user to nullify
    // their own recent history. Ops *after* the violation are displaced by the
    // fork anyway, and chain through it under the attacker's authority, so
    // requiring them to verify would let a malformed tail deny the remedy.
    let chain: Vec<crate::plc_chain::ChainEntry<'_>> = standing
        .iter()
        .map(|(entry, raw)| crate::plc_chain::ChainEntry {
            cid: &entry.cid,
            raw_op: raw,
            rotation_keys: &entry.operation.rotation_keys,
        })
        .collect();
    // A failure here is reported as a no-remedy ELIGIBILITY, not as an error.
    // Both refuse to sign — `converge_contest` acts only on `Contestable` — but
    // only this shape reaches the surface, and the surface is the whole point:
    // the alarm has already told the user their identity may be seized, so a
    // blank ceremony card is the one outcome that leaves them with a warning and
    // no explanation. See [`ContestEligibility::Unauthenticated`].
    if let Err(failure) = crate::plc_chain::verify_standing_chain(&chain, did, standing_index) {
        tracing::warn!(
            %did,
            %failure,
            "the published log does not authenticate through the contested op; \
             no fork can be signed against it"
        );
        return violation(ContestEligibility::Unauthenticated(failure));
    }

    // Decision 3's pruning: intersection with the held ring, ORDER PRESERVED —
    // the published list's order is the directory's priority order, and
    // re-ordering it would silently re-rank the user's own keys.
    let rotation_keys: Vec<String> = fork_entry
        .operation
        .rotation_keys
        .iter()
        .filter(|k| held_did_keys.contains(k))
        .cloned()
        .collect();

    violation(ContestEligibility::Contestable(ForkPoint {
        cid: fork_entry.cid.clone(),
        op: fork_raw.clone(),
        signer_did_key,
        rotation_keys,
    }))
}

/// Build the **unsigned** fork operation: the fork point's published JSON
/// carried forward verbatim, with `prev` and `rotationKeys` replaced and any
/// inherited `sig` removed.
///
/// Verbatim is load-bearing (decision 3). The op is copied as raw JSON rather
/// than re-serialized from a typed view precisely so fields this crate does
/// not model — a `services` entry's `type`, anything a future PLC schema adds
/// — survive into the signed bytes. Re-serializing a typed view would drop
/// them silently, and the directory validates the signature over exactly what
/// it is handed.
pub fn build_fork_op(fork_point: &ForkPoint) -> Result<serde_json::Value, ForkError> {
    let mut op = fork_point.op.clone();
    let obj = op.as_object_mut().ok_or(ForkError::ForkPointNotAnObject)?;
    obj.insert(
        "prev".to_string(),
        serde_json::Value::String(fork_point.cid.clone()),
    );
    obj.insert(
        "rotationKeys".to_string(),
        serde_json::Value::Array(
            fork_point
                .rotation_keys
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    // The fork point's own signature must not ride along into the bytes we
    // sign — the directory verifies the new `sig` over the op WITHOUT a `sig`
    // field, exactly as `unsigned_tombstone_cbor` omits one.
    obj.remove("sig");
    Ok(op)
}

/// The canonical dag-cbor of an unsigned op — the exact byte string the
/// rotation key signs.
///
/// dag-cbor, not JSON: the directory validates the signature against this
/// encoding, and JSON is only ever the HTTP body. The encoder sorts map keys
/// length-first then bytewise, so a `serde_json::Value` carried forward
/// verbatim still encodes to the canonical form regardless of the order the
/// directory happened to serialize its JSON in.
pub fn unsigned_fork_cbor(unsigned: &serde_json::Value) -> Result<Vec<u8>, ForkError> {
    fauna_protocol::encode_canonical(unsigned)
        .map(|b| b.to_vec())
        .map_err(|e| ForkError::Encode(e.to_string()))
}

/// Sign an unsigned fork op with the held senior rotation key, returning the
/// submittable JSON (the unsigned object plus its `sig`).
///
/// Same signature discipline as [`crate::tombstone::sign_tombstone`], and for
/// the same reason: ECDSA-SHA256 over the canonical dag-cbor, **normalized to
/// low-S**, serialized as the fixed-size `r‖s` pair, base64url-no-pad. Low-S is
/// not optional — RustCrypto's P-256 signer does not normalize on its own, so a
/// signature that skipped it would verify locally and be rejected by the
/// directory roughly half the time.
pub fn sign_fork_op(
    unsigned: serde_json::Value,
    senior_secret_scalar: &[u8; 32],
) -> Result<serde_json::Value, ForkError> {
    let signing =
        SigningKey::from_slice(senior_secret_scalar).map_err(|_| ForkError::BadRotationKey)?;
    let msg = unsigned_fork_cbor(&unsigned)?;
    let sig: p256::ecdsa::Signature = signing.sign(&msg);
    let sig = sig.normalize_s().unwrap_or(sig);
    let mut signed = unsigned;
    signed
        .as_object_mut()
        .ok_or(ForkError::ForkPointNotAnObject)?
        .insert(
            "sig".to_string(),
            serde_json::Value::String(B64URL.encode(sig.to_bytes())),
        );
    Ok(signed)
}

/// What one whole contest converge step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContestProgress {
    /// The fork was signed and the directory accepted it. The contested op and
    /// every descendant are nullified; the next custody pass reads `Verified`
    /// and takes the banner down on its own.
    Contested {
        /// The fork point the op chained to.
        fork_prev_cid: String,
        /// Which held key signed — the same value the burn writer records.
        signed_with_did_key: String,
    },
    /// No consent covers this log's current first standing violation. Either
    /// there is no intent for this DID at all, or the intent names a
    /// **different** op — a different attack, which needs a fresh human
    /// decision (decision 7). Nothing was signed.
    NoConsentForThisViolation,
    /// Custody holds (or the user's own retirement stands): there is nothing
    /// to contest. Also the terminal state after a successful contest, which
    /// is what makes a stale intent inert forever rather than a standing
    /// authorization.
    NothingToContest,
    /// A violation stands and is consented to, but no fork can be built for
    /// it. Nothing was signed.
    NotContestable(ContestEligibility),
}

/// One whole contest converge step: re-read the DID's published log, re-derive
/// the plan from that fresh read, and sign only if the user's recorded consent
/// still matches what the log says now.
///
/// **Everything is re-derived every pass** (decision 7). The intent is a
/// consent record, not a cached plan: the log is fetched again, the first
/// standing violation recomputed, the fork point re-selected and the window
/// re-checked before any bytes are signed. That is what makes the flow
/// crash-safe and fleet-completable — a crash mid-submit simply retries (the
/// directory is the durable record, so no client-side "contested" flag exists),
/// and a sibling device holding the synced ring and intent finishes what this
/// device started.
///
/// The consent is looked up by the `(did, contested_op_cid)` pair, which IS its
/// scope: an intent for an op that is no longer the standing violation
/// authorizes nothing, so a completed contest cannot be replayed into a second
/// signature and a *new* hostile op requires a new gesture.
pub async fn converge_contest(
    directory_base_url: &str,
    did: &str,
    held_keys: &[AtprotoRotationKey],
    intents: &[AtprotoContestIntent],
    now_unix_secs: u64,
) -> Result<ContestProgress, ForkError> {
    let held_did_keys: Vec<String> = held_keys.iter().map(|k| k.pubkey_did_key.clone()).collect();
    let body = fetch_audit_log(directory_base_url, did).await?;
    let violation = match contest_plan(&body, did, &held_did_keys, now_unix_secs)? {
        ContestPlan::NoViolation => return Ok(ContestProgress::NothingToContest),
        ContestPlan::Violation(v) => v,
    };
    // The consent gate. Checked against the FRESH read's violation, never
    // against anything the intent carries about the world.
    if !intents
        .iter()
        .any(|i| i.did == did && i.contested_op_cid == violation.contested_op_cid)
    {
        return Ok(ContestProgress::NoConsentForThisViolation);
    }
    let ContestEligibility::Contestable(fork_point) = violation.eligibility else {
        return Ok(ContestProgress::NotContestable(violation.eligibility));
    };
    let signer = held_keys
        .iter()
        .find(|k| k.pubkey_did_key == fork_point.signer_did_key)
        .ok_or(ForkError::SignerNotHeld)?;
    let unsigned = build_fork_op(&fork_point)?;
    let signed = sign_fork_op(unsigned, &signer.secret_scalar.to_array())?;
    submit_fork(directory_base_url, did, &signed).await?;
    Ok(ContestProgress::Contested {
        fork_prev_cid: fork_point.cid,
        signed_with_did_key: signer.pubkey_did_key.clone(),
    })
}

/// POST a signed fork operation to the directory — the client's own
/// connection, no nest in the path.
pub async fn submit_fork(
    directory_base_url: &str,
    did: &str,
    signed: &serde_json::Value,
) -> Result<(), ForkError> {
    let body = serde_json::to_vec(signed).map_err(|e| ForkError::Encode(e.to_string()))?;
    crate::directory_submit::post_plc_op(directory_base_url, did, body)
        .await
        .map_err(ForkError::Submit)
}

/// The production directory base URL (honours the e2e test seam).
pub fn directory_base_url() -> String {
    plc_directory_base_url()
}

/// Parse a PLC `createdAt` (`YYYY-MM-DDTHH:MM:SS[.frac]Z`) to unix seconds.
///
/// Deliberately narrow: PLC publishes UTC, and this value is **advisory** —
/// it drives the countdown the ceremony shows and the plan's window check, not
/// a claim about acceptance, which is the directory's own ruling at submit.
/// Anything else — a numeric offset, a truncated string — answers `None`,
/// which reads downstream as "deadline unknown" and therefore does **not**
/// close the window: refusing to offer the remedy because a timestamp was
/// unparseable would hand the box a win it did not earn.
///
/// Civil-date arithmetic delegates to [`fauna_core::caltime::days_from_civil`],
/// the workspace's canonical implementation (priority #2).
fn parse_plc_timestamp(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let time = rest
        .strip_suffix('Z')
        .or_else(|| rest.strip_suffix('z'))?
        // Fractional seconds are dropped: the window is 72 hours.
        .split('.')
        .next()?;
    let mut d = date.split('-');
    let year: i32 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut t = time.split(':');
    let hour: u64 = t.next()?.parse().ok()?;
    let min: u64 = t.next()?.parse().ok()?;
    let sec: u64 = t.next().unwrap_or("0").parse().ok()?;
    if t.next().is_some() || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = fauna_core::caltime::days_from_civil(year, month, day);
    let secs = days.checked_mul(86_400)? + (hour * 3600 + min * 60 + sec) as i64;
    u64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plc_chain::test_log::{self, Chain, RowSpec, mint};
    use crate::rotation_key::generate_rotation_key;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;

    // Real keys on their production curves — the user's senior key P-256, the
    // box's junior key K-256. These fixtures used to be `did:key:zDnae…`
    // placeholders over ops carrying `"sig": "AAAA"`, which is the second proof
    // finding offered: nothing looked, so nothing could tell a genuine
    // log from a fabricated one. See `plc_chain::test_log`.
    fn user_key() -> String {
        test_log::user_key()
    }
    fn box_key() -> String {
        test_log::box_key()
    }
    fn other_key() -> String {
        test_log::other_key()
    }

    /// 2026-08-02T00:00:00Z
    const T0: u64 = 1_785_628_800;

    fn ts(offset_secs: u64) -> String {
        // Render an RFC 3339 UTC stamp `offset_secs` after T0, via the same
        // civil arithmetic the parser inverts.
        let total = T0 + offset_secs;
        let (y, mo, d) = fauna_core::caltime::civil_from_days((total / 86_400) as i64);
        let rem = total % 86_400;
        format!(
            "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    /// One audit-log row, named by a nickname the test refers to it by — the
    /// real CID is only knowable after signing, so tests ask the built chain
    /// ([`Chain::cid`]) rather than inventing one. `rotation_keys: None`
    /// renders a `plc_tombstone`.
    fn entry(name: &str, rotation_keys: Option<&[String]>, created_at: &str) -> RowSpec {
        test_log::row(name, rotation_keys, created_at)
    }

    /// Mint a REAL log: chained on real CIDs, every op genuinely signed by a
    /// key the op it chains to listed.
    fn log(rows: Vec<RowSpec>) -> Chain {
        mint(rows)
    }

    /// An honest op's published ring: the user senior, the box junior — the
    /// shape the bridge actually mints (`plc.go`'s `[userRotationDIDKey,
    /// bridgeRotationDIDKey]`). Listing the box key is not decoration: it is
    /// what lets the box sign, which is the whole premise of the custody split
    /// and the reason a seizure is contestable rather than impossible.
    fn honest() -> Vec<String> {
        vec![user_key(), box_key()]
    }

    /// A seizure: the box promotes its own key to the senior slot.
    fn seized() -> Vec<String> {
        vec![box_key()]
    }

    fn held() -> Vec<String> {
        vec![user_key()]
    }

    fn violation_of(plan: ContestPlan) -> Violation {
        match plan {
            ContestPlan::Violation(v) => *v,
            other => panic!("expected a violation, got {other:?}"),
        }
    }

    // ── contest_plan: which op is the fork point ────────────────────────────

    /// The fork point is the standing op immediately BEFORE the first
    /// violation, and the contested op is the violation itself — decision 1,
    /// the load-bearing mapping between detection and remedy.
    #[test]
    fn fork_point_is_the_op_before_the_first_violation() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&honest()), &ts(100)),
            entry("bafy2", Some(&seized()), &ts(200)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 300).unwrap());
        assert_eq!(v.contested_op_cid, c.cid("bafy2"));
        let ContestEligibility::Contestable(fp) = &v.eligibility else {
            panic!("expected contestable, got {:?}", v.eligibility);
        };
        assert_eq!(fp.cid, c.cid("bafy1"));
        assert_eq!(fp.signer_did_key, user_key());
    }

    /// The sandwich case: a good-LOOKING op after the violation does not move
    /// the fork point. It chains through the violation and was authored under
    /// the attacker's authority, so it is displaced too.
    #[test]
    fn a_good_looking_op_after_the_violation_is_still_displaced() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&seized()), &ts(100)),
            entry("bafy2", Some(&honest()), &ts(200)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 300).unwrap());
        assert_eq!(
            v.contested_op_cid,
            c.cid("bafy1"),
            "the FIRST violation is contested"
        );
        let ContestEligibility::Contestable(fp) = &v.eligibility else {
            panic!("expected contestable, got {:?}", v.eligibility);
        };
        assert_eq!(fp.cid, c.cid("bafy0"), "fork point stays the last good op");
    }

    /// Nullified ops are not standing, so they shift no index — the contest
    /// reads the same chain the alarm did.
    #[test]
    fn nullified_ops_do_not_shift_the_fork_point() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafyX", Some(&seized()), &ts(50)).nullified(),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        assert_eq!(v.contested_op_cid, c.cid("bafy1"));
        let ContestEligibility::Contestable(fp) = &v.eligibility else {
            panic!("expected contestable");
        };
        assert_eq!(fp.cid, c.cid("bafy0"));
    }

    // ── contest_plan: the honest not-contestable states ─────────────────────

    /// Decision 2: a violation at the genesis has no contest — there is no
    /// earlier op to chain to.
    #[test]
    fn genesis_violation_is_not_contestable() {
        let c = log(vec![
            entry("bafy0", Some(&seized()), &ts(0)),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        assert_eq!(v.contested_op_cid, c.cid("bafy0"));
        assert_eq!(v.eligibility, ContestEligibility::GenesisViolation);
    }

    /// Structural impossibility outranks the clock: a genesis violation reads
    /// as "no contest exists", never as "you were too late".
    #[test]
    fn genesis_violation_outranks_a_closed_window() {
        let c = log(vec![entry("bafy0", Some(&seized()), &ts(0))]);
        let v = violation_of(
            contest_plan(&c.body(), c.did(), &held(), T0 + CONTEST_WINDOW_SECS + 1).unwrap(),
        );
        assert_eq!(v.eligibility, ContestEligibility::GenesisViolation);
    }

    /// Decision 5: eligibility is `rotationKeys[0]` and **only** index 0 — a
    /// held key listed lower down proves nothing about relative priority, and
    /// must never license a contest.
    ///
    /// It cannot, structurally: an op whose senior slot is not ours is itself
    /// the custody violation, so it is what gets *contested*, never what a
    /// fork chains to. Here that op is the genesis, so the honest answer is
    /// "no contest exists" — not a fork signed by a key the directory would
    /// out-rank.
    #[test]
    fn a_held_key_below_index_zero_never_licenses_a_contest() {
        let c = log(vec![
            entry("bafy0", Some(&[other_key(), user_key()]), &ts(0)),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        assert_eq!(
            v.contested_op_cid,
            c.cid("bafy0"),
            "the op listing our key below index 0 is the violation, not a fork point"
        );
        assert_eq!(v.eligibility, ContestEligibility::GenesisViolation);
    }

    /// The same shape one op later: the lower-position op is contested and the
    /// fork point is the genuinely-senior op before it. Together with the test
    /// above this pins that no path reaches a fork point whose
    /// `rotationKeys[0]` we do not hold — the invariant
    /// [`ForkError::ForkPointSeniorNotHeld`] is the residual guard for.
    #[test]
    fn a_lower_position_held_key_is_contested_not_forked_from() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&[other_key(), user_key()]), &ts(100)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        assert_eq!(v.contested_op_cid, c.cid("bafy1"));
        let ContestEligibility::Contestable(fp) = &v.eligibility else {
            panic!("expected contestable, got {:?}", v.eligibility);
        };
        assert_eq!(fp.cid, c.cid("bafy0"));
        assert_eq!(fp.signer_did_key, user_key());
    }

    /// The window closes 72 h after the CONTESTED op's own publication.
    #[test]
    fn window_closes_seventy_two_hours_after_the_contested_op() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        let deadline = T0 + 100 + CONTEST_WINDOW_SECS;
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), deadline).unwrap());
        assert_eq!(v.deadline_unix, Some(deadline));
        assert!(
            matches!(v.eligibility, ContestEligibility::Contestable(_)),
            "on the deadline itself the contest is still offered"
        );

        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), deadline + 1).unwrap());
        assert_eq!(v.eligibility, ContestEligibility::WindowClosed);
    }

    /// An unparseable `createdAt` leaves the deadline unknown — which must NOT
    /// close the window. Refusing the remedy over a timestamp we could not read
    /// would hand the box a win it did not earn; the directory rules at submit.
    #[test]
    fn an_unknown_deadline_does_not_close_the_window() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), "not-a-timestamp"),
            entry("bafy1", Some(&seized()), "also-not-a-timestamp"),
        ]);
        let v = violation_of(
            contest_plan(&c.body(), c.did(), &held(), T0 + 10 * CONTEST_WINDOW_SECS).unwrap(),
        );
        assert_eq!(v.deadline_unix, None);
        assert!(matches!(v.eligibility, ContestEligibility::Contestable(_)));
    }

    // ── contest_plan: what is NOT a contest ─────────────────────────────────

    #[test]
    fn a_verified_log_has_nothing_to_contest() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&honest()), &ts(100)),
        ]);
        assert_eq!(
            contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap(),
            ContestPlan::NoViolation
        );
    }

    /// A tombstone signed by a key the ring does NOT hold is the box
    /// destroying the identity — in scope, and contestable.
    #[test]
    fn a_foreign_signed_tombstone_is_contestable() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            // Signed by the box's own listed key — the destruction the custody
            // split cannot prevent, only contest.
            entry("bafy1", None, &ts(100)).signed_by(box_key()),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        assert_eq!(v.contested_op_cid, c.cid("bafy1"));
        assert!(matches!(
            v.reason,
            MismatchReason::RetirementByUnheldKey { .. }
        ));
        assert!(matches!(v.eligibility, ContestEligibility::Contestable(_)));
    }

    /// The user's OWN retirement is the expected end state — never contested,
    /// which is exactly the auto-actor decision 6 rules out one layer down.
    #[test]
    fn our_own_retirement_is_not_a_violation() {
        // Signed by the user's OWN senior key, so the tombstone attributes to
        // us rather than to the box — the distinction the attribution rule
        // turns on.
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", None, &ts(100)).signed_by(user_key()),
        ]);
        assert_eq!(
            contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap(),
            ContestPlan::NoViolation
        );
    }

    // ── what must never reach the signed bytes ─────────────────────

    const ATTACKER_KEY: &str = "did:key:zATTACKER_SIGNING_KEY";
    const ATTACKER_PDS: &str = "https://attacker.example";

    /// The finding's own probe, graded where the finding asks for it: at the
    /// **signed op**, not at `contest_plan`'s verdict.
    ///
    /// The plan is allowed to see a hostile log — the alarm is deliberately
    /// permissive. What must never happen is the user's senior rotation key
    /// signing bytes the directory chose, because that converts *"the directory
    /// lied"* (catchable by any third party) into *"the user authorized it"*.
    ///
    /// Deliberately written to survive a future relaxation: it drives the whole
    /// pipeline and asserts on the produced signature *if one is produced*, so
    /// it still grades correctly if someone later makes planning lenient and
    /// strips the fields instead.
    #[test]
    fn a_tampered_fork_point_never_reaches_the_signed_bytes() {
        let mut c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        // Rewrite the fork point in flight while keeping its published `cid` —
        // the shape that lands against the REAL directory, because the fork
        // then names a `prev` the directory genuinely knows.
        c.tamper("bafy0", |op| {
            op["verificationMethods"]["atproto"] = ATTACKER_KEY.into();
            op["services"]["atproto_pds"]["endpoint"] = ATTACKER_PDS.into();
        });

        let plan = contest_plan(&c.body(), c.did(), &held(), T0 + 200);
        if let Ok(ContestPlan::Violation(v)) = &plan
            && let ContestEligibility::Contestable(fp) = &v.eligibility
        {
            let unsigned = build_fork_op(fp).expect("build");
            let key = generate_rotation_key(1);
            let signed = sign_fork_op(unsigned, &key.secret_scalar.to_array()).expect("sign");
            let bytes = serde_json::to_string(&signed).expect("serialize");
            panic!("the attacker's fork point was SIGNED: {bytes}");
        }
        // No signature exists at all, and no consent was spent — reported as a
        // no-remedy eligibility rather than an error, so the ceremony surface
        // can say WHY (the user is already looking at a custody alarm).
        assert!(
            matches!(
                &plan,
                Ok(ContestPlan::Violation(v))
                    if matches!(
                        v.eligibility,
                        ContestEligibility::Unauthenticated(
                            crate::plc_chain::ChainFailure::CidMismatch { .. }
                        )
                    )
            ),
            "expected a refusal to authenticate, got {plan:?}"
        );
    }

    /// The companion half: a fabricated *violation* must not drive the ceremony
    /// into signing either. This is the harm needing no compromise at all — the
    /// attacker appends one op to the user's real history, and an unchecked
    /// client would fork off a genuine fork point, nullifying the user's own
    /// recent operations and pruning the bridge's key out with them.
    #[test]
    fn a_fabricated_violation_never_reaches_the_signed_bytes() {
        let c = log(vec![
            entry("bafy0", Some(&honest()), &ts(0)),
            entry("bafy1", Some(&honest()), &ts(100)),
            entry("bafy2", Some(&seized()), &ts(200)).forged(),
        ]);
        let plan = contest_plan(&c.body(), c.did(), &held(), T0 + 300);
        assert!(
            matches!(
                &plan,
                Ok(ContestPlan::Violation(v))
                    if matches!(
                        v.eligibility,
                        ContestEligibility::Unauthenticated(
                            crate::plc_chain::ChainFailure::BadSignature { index: 2 }
                        )
                    )
            ),
            "expected the fabricated op to fail signature verification, got {plan:?}"
        );
        // …and it is a *planned* refusal, so the surface has something to say:
        // the contested op is still named, which is what the card's copy and the
        // consent scope are keyed on. A bare error carried none of this.
        let Ok(ContestPlan::Violation(v)) = &plan else {
            unreachable!("asserted above")
        };
        assert!(
            !v.contested_op_cid.is_empty(),
            "the no-remedy card must still name the op the alarm is about; a bare \
             error carried no CID, so neither the copy nor the consent scope had one"
        );
    }

    // ── build_fork_op: the verbatim carry-forward ───────────────────────────

    fn contestable_fork_point() -> ForkPoint {
        let c = log(vec![
            // A field no version of this crate models — the exact thing a
            // re-serialized typed view would drop. Minted INTO the signed
            // bytes, so carrying it forward is a real property of a real op
            // rather than an artefact of the fixture editing JSON afterwards.
            entry("bafy0", Some(&honest()), &ts(0))
                .extra_field("futureSchemaField", serde_json::json!({"nested": ["a", 1]})),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        let v = violation_of(contest_plan(&c.body(), c.did(), &held(), T0 + 200).unwrap());
        match v.eligibility {
            ContestEligibility::Contestable(fp) => fp,
            other => panic!("expected contestable, got {other:?}"),
        }
    }

    #[test]
    fn the_fork_carries_unknown_fields_forward_verbatim() {
        let op = build_fork_op(&contestable_fork_point()).unwrap();
        assert_eq!(
            op["futureSchemaField"],
            serde_json::json!({"nested": ["a", 1]}),
            "a field this crate does not model must survive into the signed bytes"
        );
        // …and so must the fields it models but must NOT change: identity
        // authority is severed, availability is not (decision 3).
        assert_eq!(
            op["verificationMethods"]["atproto"],
            "did:key:zBoxSigningKey"
        );
        assert_eq!(
            op["services"]["atproto_pds"]["endpoint"],
            "https://pds.example.com"
        );
        assert_eq!(
            op["services"]["atproto_pds"]["type"], "AtprotoPersonalDataServer",
            "the service entry's own unmodelled `type` survives"
        );
        assert_eq!(
            op["alsoKnownAs"],
            serde_json::json!(["at://alice.example.com"])
        );
        assert_eq!(op["type"], "plc_operation");
    }

    /// The exhaustive form of the verbatim rule, scoped precisely: **relative
    /// to the op it is handed**, `build_fork_op` changes `prev`,
    /// `rotationKeys` and `sig` and nothing else. Written as a whole-object
    /// diff rather than a list of `assert_eq!`s on the fields we happen to
    /// remember, so a field nobody thought to name still counts.
    ///
    /// ⚠ It does NOT pin the carrier, and reading it as if it does was the
    /// overclaim this comment used to make.
    /// The diff's baseline is `fp.op` — the carrier is compared against itself, so
    /// a carrier that starts dropping fields shrinks both operands together
    /// and this test stays green while the exact defect this finding is about
    /// goes undetected. The carrier claim is pinned by
    /// [`the_fork_carries_unknown_fields_forward_verbatim`] alone, which
    /// reaches `fp.op` through the real `contest_plan` read; keep that test
    /// alive on its own merits, not as this one's redundant twin.
    #[test]
    fn prev_rotation_keys_and_sig_are_the_only_keys_that_change() {
        let fp = contestable_fork_point();
        let before = fp.op.as_object().unwrap().clone();
        let after = build_fork_op(&fp).unwrap();
        let after = after.as_object().unwrap();

        let changed: Vec<&str> = before
            .keys()
            .chain(after.keys())
            .map(String::as_str)
            .filter(|k| before.get(*k) != after.get(*k))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(changed, vec!["prev", "rotationKeys", "sig"]);
    }

    #[test]
    fn the_fork_replaces_prev_and_prunes_box_keys() {
        let fp = contestable_fork_point();
        let op = build_fork_op(&fp).unwrap();
        assert_eq!(op["prev"], fp.cid, "prev names the FORK POINT");
        assert_eq!(
            op["rotationKeys"],
            serde_json::json!([user_key()]),
            "the box's junior key is pruned; only held keys remain"
        );
        assert!(
            op.get("sig").is_none(),
            "the fork point's own signature must not ride into the signed bytes"
        );
    }

    /// Pruning preserves the published order — the directory's priority order
    /// is not ours to re-rank.
    #[test]
    fn pruning_preserves_the_published_order() {
        let second = other_key();
        let c = log(vec![
            entry(
                "bafy0",
                Some(&[user_key(), box_key(), second.clone()]),
                &ts(0),
            ),
            entry("bafy1", Some(&seized()), &ts(100)),
        ]);
        // Ring order deliberately the reverse of the published order.
        let held = vec![second.clone(), user_key()];
        let v = violation_of(contest_plan(&c.body(), c.did(), &held, T0 + 200).unwrap());
        let ContestEligibility::Contestable(fp) = v.eligibility else {
            panic!("expected contestable");
        };
        assert_eq!(
            fp.rotation_keys,
            vec![user_key(), second],
            "published order, not ring order"
        );
    }

    // ── signing ─────────────────────────────────────────────────────────────

    /// The signed bytes are the canonical dag-cbor of the SIG-LESS op, and the
    /// encoding is byte-stable across runs — the property the directory's
    /// verification depends on.
    #[test]
    fn unsigned_cbor_is_canonical_and_byte_stable() {
        let unsigned = build_fork_op(&contestable_fork_point()).unwrap();
        let a = unsigned_fork_cbor(&unsigned).unwrap();
        let b = unsigned_fork_cbor(&unsigned).unwrap();
        assert_eq!(a, b);
        // Canonical dag-cbor sorts map keys length-first then bytewise, so the
        // 4-byte `prev` and `type` lead the map regardless of JSON order, and
        // `prev` precedes `type` bytewise.
        //
        // Scanned over the WHOLE encoding rather than a fixed-size prefix: a
        // real PLC CID is 59 characters, so a 40-byte window falls inside the
        // `prev` value and never reaches `type` at all — which is how this
        // assertion silently stopped comparing anything once the fixtures
        // started minting real CIDs.
        let all = String::from_utf8_lossy(&a).to_string();
        let (prev_at, type_at) = (all.find("prev"), all.find("type"));
        assert!(prev_at.is_some() && type_at.is_some(), "both keys present");
        assert!(
            prev_at < type_at,
            "map keys are length-first then bytewise ordered: {all:?}"
        );
        // The longer keys must all follow the two 4-byte ones.
        for longer in ["services", "alsoKnownAs", "rotationKeys"] {
            assert!(
                all.find(longer) > type_at,
                "{longer} sorts after the 4-byte keys"
            );
        }
    }

    #[test]
    fn the_signature_verifies_over_the_unsigned_bytes_and_is_low_s() {
        let key = generate_rotation_key(1);
        let unsigned = build_fork_op(&contestable_fork_point()).unwrap();
        let msg = unsigned_fork_cbor(&unsigned).unwrap();
        let signed = sign_fork_op(unsigned, &key.secret_scalar.to_array()).unwrap();

        let raw = B64URL.decode(signed["sig"].as_str().unwrap()).unwrap();
        assert_eq!(raw.len(), 64, "fixed-size r‖s, never DER");
        let sig = p256::ecdsa::Signature::from_slice(&raw).unwrap();
        assert!(
            sig.normalize_s().is_none(),
            "already low-S — the directory rejects high-S roughly half the time"
        );
        let (_, point) = fauna_protocol::atproto::decode_did_key(&key.pubkey_did_key).unwrap();
        let vk = VerifyingKey::from_sec1_bytes(&point).unwrap();
        vk.verify(&msg, &sig)
            .expect("verifies over the sig-less canonical bytes");
    }

    // ── the advisory timestamp parser ───────────────────────────────────────

    #[test]
    fn plc_timestamps_parse_to_unix_seconds() {
        assert_eq!(parse_plc_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_plc_timestamp("2026-08-02T00:00:00Z"), Some(T0));
        assert_eq!(
            parse_plc_timestamp("2026-08-02T00:00:00.123456Z"),
            Some(T0),
            "fractional seconds are dropped, not rejected"
        );
        assert_eq!(parse_plc_timestamp("2026-08-02T01:02:03Z"), Some(T0 + 3723));
    }

    #[test]
    fn unparseable_timestamps_answer_none_rather_than_guessing() {
        for bad in [
            "",
            "2026-08-02",
            "2026-08-02T00:00:00",       // no zone marker
            "2026-08-02T00:00:00+02:00", // a numeric offset we will not guess at
            "2026-13-02T00:00:00Z",      // month out of range
            "2026-08-02T24:00:00Z",      // hour out of range
            "not-a-timestamp",
        ] {
            assert_eq!(parse_plc_timestamp(bad), None, "{bad:?} must not parse");
        }
    }
}
