//! Client-side genesis-seniority verification (S4-C) — the check that
//! tombstones the TOFU caveat of `atproto-pds-bridge.md` § State & data shape.
//!
//! After a hosted mint, the client fetches the DID's operation log directly
//! from the public PLC directory — its own HTTPS connection, **no nest
//! dependency**, so the box cannot sit in the middle of its own audit — and
//! asserts that **every standing operation** carries one of the user's held
//! rotation keys ([`crate::rotation_key`]) at `rotationKeys[0]`. Membership
//! in the held ring, not equality with a single key: the ring can hold a
//! retired identity's burned key beside the live identity's fresh one
//! (fresh-key-per-mint), and every ring entry is user-custodied — keys enter
//! the account plane's custody only through the client-side sole writer, so the box
//! cannot smuggle a key into the comparison set.
//!
//! Every standing op, not just the head: PLC allows a senior rotation key to
//! fork and nullify later operations within a 72 h window, so a log whose head
//! looks correct but whose genesis had a box key senior is still stealable
//! during that window. Seniority must hold across the whole standing chain.
//!
//! Verdict philosophy: **"couldn't read" is quiet, "read it and it's wrong"
//! alarms.** Network errors, HTTP failures, and unparseable bodies are
//! [`VerifyFailure`] — retried on a later check, never an alarm (unreachable ≠
//! compromised, and a false alarm trains the user to ignore the real one).
//! A log we *can* read that shows a different senior key — or a legacy
//! non-`plc_operation` entry, which our mint flow never produces — is a
//! [`SeniorityVerdict::Mismatch`]. That last point deliberately diverges from
//! the Go bridge's `FetchLastOp` (which refuses legacy ops neutrally): a
//! neutral skip here would let a hostile box dodge detection by publishing in
//! the legacy format.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The production PLC directory. Hard-coded on purpose: which directory a
/// deployment trusts is not a user/admin choice (product invariant — no
/// admin config surface). Mirrors the Go bridge's `DefaultPLCDirectoryURL`.
pub const DEFAULT_PLC_DIRECTORY_URL: &str = "https://plc.directory";

/// TEST-ONLY env seam, same name the Go bridge honours: the e2e harness points
/// the client at `FakePlcDirectory`. Test-harness IPC, never operator
/// configuration. Native only — wasm has no env, see [`WASM_DIRECTORY_OVERRIDE`].
const PLC_DIRECTORY_URL_ENV: &str = "FAUNA_ATPROTO_PLC_DIRECTORY_URL";

/// wasm twin of [`PLC_DIRECTORY_URL_ENV`]: a browser has no process
/// environment, so the e2e harness points wasm at `FakePlcDirectory` through
/// this `thread_local` instead (wasm32 is single-threaded, so this is
/// effectively one flag per loaded module instance — the same lifetime a
/// native process's env var has). Set by [`enable_fake_plc_directory_for_test`],
/// exposed to JS by `fauna-wasm-atproto-settings`'s
/// `enableFakePlcDirectoryForTest` and called only from the Playwright e2e
/// bridge; never in production.
#[cfg(target_arch = "wasm32")]
thread_local! {
    static WASM_DIRECTORY_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: point wasm's genesis-seniority check at a fake PLC directory for
/// the rest of this module instance's lifetime.
///
/// **Compiled out of release artifacts** (convention 15 rule (a),
/// `e2e-automation-surface-gating.md` § The convention), the same `any(...)`
/// [`plc_directory_base_url`]'s native arm uses — `target_arch = "wasm32"`
/// alone (the only gate this carried before) restricts to wasm, not to a
/// test-capable build.
#[cfg(all(target_arch = "wasm32", any(debug_assertions, feature = "e2e-agent")))]
pub fn enable_fake_plc_directory_for_test(url: String) {
    WASM_DIRECTORY_OVERRIDE.with(|c| *c.borrow_mut() = Some(url));
}

/// Resolve the directory base URL (default unless the test seam overrides it).
///
/// ⚠ **The override is an automation surface, so it is compiled OUT of release
/// artifacts** (`testing.md` convention 15). Until
/// 2026-08-02 the native arm honoured the env var unconditionally while the
/// wasm twin was properly `cfg`-scoped — which made "anyone who can set an env
/// var on the app process" a member of the attacker set for every check that
/// reads this log, the identity-contest signing path included. The runtime env
/// var remains the inner switch *within* a test-capable build; the compile-time
/// gate is the boundary.
pub fn plc_directory_base_url() -> String {
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(debug_assertions, feature = "e2e-agent")
    ))]
    if let Ok(v) = std::env::var(PLC_DIRECTORY_URL_ENV) {
        let v = v.trim().trim_end_matches('/');
        if !v.is_empty() {
            return v.to_string();
        }
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(v) = WASM_DIRECTORY_OVERRIDE.with(|c| c.borrow().clone()) {
        let v = v.trim().trim_end_matches('/');
        if !v.is_empty() {
            return v.to_string();
        }
    }
    DEFAULT_PLC_DIRECTORY_URL.to_string()
}

/// Bound on the audit-log body. A DID's log is a handful of small ops;
/// anything vastly larger is a misbehaving directory (mirrors Go's
/// `maxAuditLogBytes`).
pub(crate) const MAX_AUDIT_LOG_BYTES: usize = 1 << 20;

/// One row of `GET /{did}/log/audit`. Only the fields this crate reads are
/// modelled; unknown fields are ignored.
///
/// Shared with [`crate::tombstone`], which needs the same rows for a different
/// question (*which op does a tombstone chain to?*). One parser, so the two
/// readings of the directory's log can never drift apart.
#[derive(Deserialize)]
pub(crate) struct AuditEntry {
    /// The op's CID — the value a following op's `prev` chains on. `#[serde(default)]`
    /// because the seniority check never reads it; the tombstone path, which
    /// does, refuses an entry that omits it rather than chaining to nothing.
    #[serde(default)]
    pub(crate) cid: String,
    #[serde(default)]
    pub(crate) nullified: bool,
    /// The directory's own timestamp for this op (RFC 3339). Read only by
    /// [`crate::recovery_fork`], for the **advisory** 72 h window countdown —
    /// whether a contest is still in time is ultimately the directory's own
    /// ruling at submit, so this drives what the ceremony *shows*, never a
    /// claim the client makes about acceptance. `#[serde(default)]`: a
    /// directory that omits it leaves the window unknown rather than
    /// unparseable.
    #[serde(default, rename = "createdAt")]
    pub(crate) created_at: Option<String>,
    pub(crate) operation: PlcOpKeys,
}

/// The slice of a PLC operation this crate reads.
#[derive(Deserialize)]
pub(crate) struct PlcOpKeys {
    #[serde(rename = "type")]
    pub(crate) op_type: String,
    /// The op this one chains to (`null` on a genesis). Read only for
    /// tombstone attribution: the signed bytes of a `plc_tombstone` are the
    /// canonical dag-cbor of `{prev, type}`, so re-verifying its signature
    /// needs the `prev` it was signed over.
    #[serde(default)]
    pub(crate) prev: Option<String>,
    /// The op's signature (base64url-no-pad `r‖s`). Read only for tombstone
    /// attribution — see
    /// [`crate::tombstone::tombstone_signed_by_held_key`].
    #[serde(default)]
    pub(crate) sig: String,
    #[serde(default, rename = "rotationKeys")]
    pub(crate) rotation_keys: Vec<String>,
    /// The op's priority-ordered handle list (`at://alice.example.com`) — the
    /// handle the *published* identity claims. Read only by
    /// [`crate::handle_binding`], for the same reason the seniority check reads
    /// `rotationKeys`: it is the directory's record of what the box actually
    /// published, which the box cannot retroactively rewrite.
    #[serde(default, rename = "alsoKnownAs")]
    pub(crate) also_known_as: Vec<String>,
    /// The op's service entries, keyed by service id. Only `atproto_pds` is
    /// ever read, and only by [`crate::tombstone`]'s sweep probe, which needs
    /// somewhere to *ask* whether the repo is gone. Read from the published log
    /// rather than from anything the box says, for the same reason the
    /// seniority check reads the log: a hostile box must not be able to answer
    /// a question about its own conduct.
    #[serde(default)]
    pub(crate) services: BTreeMap<String, PlcService>,
}

/// One `services` entry of a PLC operation.
#[derive(Deserialize)]
pub(crate) struct PlcService {
    #[serde(default)]
    pub(crate) endpoint: String,
}

/// Size-check and parse a raw `/{did}/log/audit` body. The one parser both the
/// seniority check and the tombstone's head lookup go through.
pub(crate) fn parse_audit_log(audit_json: &[u8]) -> Result<Vec<AuditEntry>, VerifyFailure> {
    if audit_json.len() > MAX_AUDIT_LOG_BYTES {
        return Err(VerifyFailure::TooLarge);
    }
    Ok(serde_json::from_slice(audit_json)?)
}

/// Parse the log into the typed rows **and** each row's untouched `operation`
/// JSON, from ONE parse of the bytes.
///
/// The recovery fork carries the fork-point op forward *verbatim* — every
/// field, including ones no PLC version we know about defines — and
/// [`PlcOpKeys`] deliberately models only the slice this crate reads
/// (`services` keeps just `endpoint`, so re-serializing the typed view would
/// silently drop fields). Hence the raw copy.
///
/// One parse, not two: the typed rows are converted **from the same
/// `Value`s**, so the raw op a fork is built from and the typed op the
/// eligibility check ruled on can never come from two different reads of a log
/// that changed in between — the same one-fetch discipline
/// [`fetch_audit_log`] exists for, one level down.
pub(crate) fn parse_audit_log_with_raw(
    audit_json: &[u8],
) -> Result<Vec<(AuditEntry, serde_json::Value)>, VerifyFailure> {
    if audit_json.len() > MAX_AUDIT_LOG_BYTES {
        return Err(VerifyFailure::TooLarge);
    }
    let rows: Vec<serde_json::Value> = serde_json::from_slice(audit_json)?;
    rows.into_iter()
        .map(|row| {
            let raw_op = row
                .get("operation")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let entry: AuditEntry = serde_json::from_value(row)?;
            Ok((entry, raw_op))
        })
        .collect()
}

/// The `type` of every op our mint flow builds (Go `OpTypeOperation`).
pub(crate) const OP_TYPE_OPERATION: &str = "plc_operation";

/// Outcome of a successfully *read* log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeniorityVerdict {
    /// Every standing op carries one of the client's held keys at
    /// `rotationKeys[0]` — user custody holds across the whole standing
    /// chain. Membership in the held ring, not equality with one key: the
    /// ring can legitimately hold several user-custodied keys (a retired
    /// identity's burned key beside the live identity's fresh one), and
    /// which of them a given DID publishes senior is that DID's own log's
    /// fact, not a list position.
    Verified {
        /// Number of standing (non-nullified) ops checked. Never 0.
        standing_ops: usize,
        /// The held keys observed at `rotationKeys[0]` across the standing
        /// ops, deduped in first-seen order — what the caller's binding
        /// converge records against the DID ([`AtprotoRotationKey::
        /// published_for_dids`](fauna_core::data::AtprotoRotationKey)).
        observed_seniors: Vec<String>,
    },
    /// The published log contradicts this client's key material — the alarm
    /// case (possible genesis-time custody compromise).
    Mismatch(MismatchReason),
}

impl SeniorityVerdict {
    /// Does this verdict mean the DID's published log has been **terminally
    /// retired by the user's own act** — a `plc_tombstone` standing over the
    /// chain, whose signature verifies against a key this client holds —
    /// rather than compromised?
    ///
    /// The distinction is invisible in the verdict's own name on purpose:
    /// [`verify_audit_log`] asks one question ("is a key I hold senior over
    /// every standing op?") and a tombstone answers it *no*, because a
    /// tombstone carries no `rotationKeys` at all. Whether that `no` is an
    /// **alarm** or the **expected end state** depends entirely on the caller's
    /// context, and only two callers have that context:
    ///
    /// * The identity the nest names as *live*: a tombstone contradicts that
    ///   claim outright — either the nest is lying or someone destroyed the
    ///   identity — so it stays the alarm it is today.
    /// * A DID known only from the held ring
    ///   ([`AtprotoRotationKey::published_for_dids`](fauna_core::data::AtprotoRotationKey)),
    ///   or one the nest has *stopped* naming: the client published a key for
    ///   it at some point and the burn is permanent, so a retirement is the
    ///   ordinary end of that entry's life. Silent — and the one thing that
    ///   legitimately takes a standing alarm for it down
    ///   (`critical-alerts.md` § Mechanism → *Lifetime*: re-checked and found
    ///   resolved, never merely un-mentioned).
    ///
    /// Doubly narrow (the second bound RE-TAKEN 2026-08-02):
    /// **only** a tombstone reads as retirement — any other foreign op type is
    /// a log this client's key was never senior over, the alarm case in every
    /// caller's context — and only a tombstone **this client's own ring
    /// signed** ([`MismatchReason::OwnRetirement`]). A tombstone is the one op
    /// any *listed* key may sign, and the bridge's junior key is listed for
    /// the identity's whole life — so "a tombstone is in the log" is evidence
    /// of a retirement, not of the *user's* retirement, and reading it as
    /// consent would let the party that destroyed the identity also silence
    /// the alarm for it ([`MismatchReason::RetirementByUnheldKey`] stays
    /// `false` here, i.e. loud).
    pub fn is_terminal_retirement(&self) -> bool {
        matches!(self, Self::Mismatch(MismatchReason::OwnRetirement { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MismatchReason {
    /// A standing op's senior slot holds a different key (or none at all).
    SeniorKeyDiffers {
        /// Index into the standing-op sequence (0 = genesis).
        standing_index: usize,
        /// What the directory published at `rotationKeys[0]`, if anything.
        found: Option<String>,
    },
    /// A standing op is neither a `plc_operation` nor a `plc_tombstone` — a
    /// shape our mint flow never produces, so the published identity is not
    /// one this client's key can be verified senior over.
    NotAPlcOperation {
        standing_index: usize,
        op_type: String,
    },
    /// A `plc_tombstone` stands over the chain and its signature verifies
    /// against a key this client holds: the user's own retirement (this
    /// device or a sibling — the ring syncs). The one Mismatch that reads as
    /// the expected end state ([`SeniorityVerdict::is_terminal_retirement`]).
    OwnRetirement { standing_index: usize },
    /// A `plc_tombstone` stands over the chain and its signature does **not**
    /// verify against any held key: someone else's key — the bridge's listed
    /// junior key foremost — destroyed this identity. Always the alarm case:
    /// this is the box-destroys-the-identity capability the 2026-08-02
    /// finding (c) names, caught in the act.
    RetirementByUnheldKey { standing_index: usize },
}

/// "Couldn't verify" — quiet-retry class, never an alarm.
#[derive(Debug, thiserror::Error)]
pub enum VerifyFailure {
    #[error("audit log did not parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("audit log has no standing operation")]
    NoStandingOps,
    #[error("audit log exceeds {MAX_AUDIT_LOG_BYTES} bytes")]
    TooLarge,
    #[error("directory fetch failed: {0}")]
    Fetch(String),
    #[error("directory returned HTTP {0}")]
    Status(u16),
}

/// Verify a raw `/{did}/log/audit` body against the client's held rotation
/// keyring. Pure — no I/O; the unit under test.
///
/// The assertion is **membership**: every standing op's `rotationKeys[0]`
/// must be one of `held_did_keys` (all of which are user-custodied — keys
/// enter the account plane's custody only through the client-side sole writer, so a
/// hostile box cannot place one in the set). An empty ring never verifies:
/// with nothing held, no published key can be the user's.
pub fn verify_audit_log(
    audit_json: &[u8],
    held_did_keys: &[String],
) -> Result<SeniorityVerdict, VerifyFailure> {
    let entries = parse_audit_log(audit_json)?;
    let mut standing = 0usize;
    let mut observed_seniors: Vec<String> = Vec::new();
    for entry in entries.iter().filter(|e| !e.nullified) {
        let op = &entry.operation;
        if op.op_type == crate::tombstone::OP_TYPE_TOMBSTONE {
            // A tombstone ends the chain, but WHOSE act it was decides
            // everything downstream: any listed key may sign one, and the
            // bridge's junior key is listed for life — so only a signature
            // from the client's own ring reads as the user's retirement.
            // Anything unverifiable is loud (RetirementByUnheldKey), never
            // quietly absorbed as "retired".
            let ours = crate::tombstone::tombstone_signed_by_held_key(
                op.prev.as_deref().unwrap_or(""),
                &op.sig,
                held_did_keys,
            );
            return Ok(SeniorityVerdict::Mismatch(if ours {
                MismatchReason::OwnRetirement {
                    standing_index: standing,
                }
            } else {
                MismatchReason::RetirementByUnheldKey {
                    standing_index: standing,
                }
            }));
        }
        if op.op_type != OP_TYPE_OPERATION {
            return Ok(SeniorityVerdict::Mismatch(
                MismatchReason::NotAPlcOperation {
                    standing_index: standing,
                    op_type: op.op_type.clone(),
                },
            ));
        }
        match op.rotation_keys.first() {
            Some(senior) if held_did_keys.contains(senior) => {
                if !observed_seniors.contains(senior) {
                    observed_seniors.push(senior.clone());
                }
            }
            found => {
                return Ok(SeniorityVerdict::Mismatch(
                    MismatchReason::SeniorKeyDiffers {
                        standing_index: standing,
                        found: found.cloned(),
                    },
                ));
            }
        }
        standing += 1;
    }
    if standing == 0 {
        return Err(VerifyFailure::NoStandingOps);
    }
    Ok(SeniorityVerdict::Verified {
        standing_ops: standing,
        observed_seniors,
    })
}

/// Fetch the DID's audit log from the directory and verify it. The one
/// network call of the check; errors are all quiet-retry class.
pub async fn fetch_and_verify(
    directory_base_url: &str,
    did: &str,
    held_did_keys: &[String],
) -> Result<SeniorityVerdict, VerifyFailure> {
    let body = fetch_audit_log(directory_base_url, did).await?;
    verify_audit_log(&body, held_did_keys)
}

/// Fetch a DID's raw `/{did}/log/audit` body from the directory.
///
/// Split out of [`fetch_and_verify`] because two checks now read the *same*
/// log for different questions — seniority ([`verify_audit_log`]) and the
/// published handle ([`crate::handle_binding::verify_handle_binding`]). One
/// fetch feeds both, so joining the second check costs no extra round trip and
/// the two verdicts can never be computed from two different reads of a log
/// that changed in between.
///
/// The connection is the client's own, direct to the directory: no nest in the
/// path, so the box cannot sit in the middle of its own audit.
pub async fn fetch_audit_log(
    directory_base_url: &str,
    did: &str,
) -> Result<Vec<u8>, VerifyFailure> {
    let url = format!(
        "{}/{did}/log/audit",
        directory_base_url.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let req = client.get(&url);
    // reqwest's wasm backend (browser fetch) has no per-request timeout; the
    // browser's own fetch limits apply there.
    #[cfg(not(target_arch = "wasm32"))]
    let req = req.timeout(std::time::Duration::from_secs(30));
    let resp = req
        .send()
        .await
        .map_err(|e| VerifyFailure::Fetch(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(VerifyFailure::Status(status.as_u16()));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| VerifyFailure::Fetch(e.to_string()))?;
    if body.len() > MAX_AUDIT_LOG_BYTES {
        return Err(VerifyFailure::TooLarge);
    }
    Ok(body.to_vec())
}

/// Is this DID one the public PLC directory can be asked about?
///
/// **The single predicate both production resolvers use** — the session-start
/// sweep's `plan_directory_audit` and the settings machine's `check_custody`.
/// They answered this question differently until finding (2026-08-02):
/// the sweep gated on the *nest's* `method` label while the machine gated on
/// the DID's own prefix, so a `did:plc:` reported under `method: "web"` was
/// audited on the settings page and **skipped by the sweep** — the sweep being
/// the one that matters, because it runs without the user opening a page.
///
/// The gate is anchored on the **data, not on the nest's word for it**: the DID
/// string is the thing the directory is keyed by, and a box that mislabels its
/// own method does not thereby earn an exemption from the audit. did:web is
/// genuinely not auditable here — its custody *is* domain custody, with no
/// directory log to read (`atproto-pds-bridge.md` § Identity).
pub fn is_auditable_did(did: &str) -> bool {
    did.starts_with("did:plc:")
}

/// i18n key for the genesis-seniority custody alarm (arg: the handle) — a
/// CRITICAL alert (`critical-alerts.md` § Severity bar).
pub const CUSTODY_ALARM_KEY: &str = "critical_alerts.atproto_custody_mismatch";

/// The registry key this feeder's alarm is posted under, identity-scoped so a
/// re-check updates its own alert rather than stacking duplicates
/// (`critical-alerts.md` § Mechanism → *Keys*).
pub fn alert_key(did: &str) -> String {
    format!("atproto-custody:{did}")
}

/// The alarm's lines for a confirmed custody mismatch. Pure, so the copy is
/// testable on every platform.
pub fn custody_mismatch_alert_lines(handle: &str) -> Vec<fauna_core::localized::LocalizedText> {
    vec![fauna_core::localized::LocalizedText::key_arg(
        CUSTODY_ALARM_KEY,
        "handle",
        handle.to_string(),
    )]
}

/// Post or clear this feeder's alert from a *decided* verdict — the one call
/// both callers make, so the settings machine's convergence and the
/// session-start sweep can never drift on the key or the copy.
///
/// `Verified` clears: the published log just showed user custody holding, which
/// is the only thing that legitimately takes this banner down (it is
/// non-dismissable — `critical-alerts.md` § Goal). A [`VerifyFailure`] must
/// never reach here: unreachable is not resolved, and clearing on a failed read
/// would let a directory that merely went offline silence a live compromise
/// warning.
pub fn sync_custody_alert(
    alerts: &fauna_client_alerts::CriticalAlerts,
    did: &str,
    handle: &str,
    verdict: &SeniorityVerdict,
) {
    let key = alert_key(did);
    match verdict {
        SeniorityVerdict::Mismatch(reason) => {
            tracing::error!(
                ?reason,
                did,
                "ATProto genesis-seniority MISMATCH: the published senior rotation key is not this client's"
            );
            alerts.post(key, custody_mismatch_alert_lines(handle));
        }
        SeniorityVerdict::Verified { .. } => alerts.clear(&key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_KEY: &str = "did:key:zDnaeUserSeniorKey";
    const BOX_KEY: &str = "did:key:zQ3shBoxJuniorKey"; // gitleaks:allow

    fn entry(rotation_keys: &[&str], nullified: bool) -> serde_json::Value {
        crate::test_fixtures::plc_operation_entry_json(
            "bafyfake",
            "plc_operation",
            rotation_keys,
            nullified,
            "2026-07-23T00:00:00Z",
        )
    }

    fn log(entries: &[serde_json::Value]) -> Vec<u8> {
        serde_json::to_vec(&entries).unwrap()
    }

    fn held(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// A standing op of some other `type`.
    fn foreign_entry(op_type: &str) -> serde_json::Value {
        serde_json::json!({
            "cid": "bafyfake",
            "nullified": false,
            "createdAt": "2026-08-02T00:00:00Z",
            "operation": {"type": op_type, "prev": "bafyprev", "sig": "fakesig"}
        })
    }

    /// A standing `plc_tombstone` entry chained to `prev`, genuinely signed
    /// with `key` through the same [`crate::tombstone::sign_tombstone`] every
    /// legitimate retirement in this system uses.
    fn tombstone_entry(
        prev: &str,
        key: &fauna_core::data::AtprotoRotationKey,
    ) -> serde_json::Value {
        let op = crate::tombstone::sign_tombstone(prev, &key.secret_scalar.to_array()).unwrap();
        serde_json::json!({
            "cid": "bafytomb",
            "nullified": false,
            "createdAt": "2026-08-02T00:00:00Z",
            "operation": {"type": op.op_type, "prev": op.prev, "sig": op.sig}
        })
    }

    /// A retired identity's log is a `Mismatch` because a tombstone carries no
    /// `rotationKeys` to be senior in — but when the tombstone's signature
    /// verifies against the client's own ring it is the *expected* end state,
    /// not a compromise, and callers that know the DID is no longer claimed
    /// live need to tell the two apart. Without this the audit floor would
    /// alarm forever on every identity the user ever retired:
    /// `published_for_dids` keeps a retired DID permanently (the retirement
    /// path burns the key that signed the tombstone — which is also why the
    /// signing key is always still held here).
    #[test]
    fn an_own_signed_tombstone_reads_as_terminal_retirement_not_compromise() {
        let key = crate::rotation_key::generate_rotation_key(1);
        let body = log(&[
            entry(&[key.pubkey_did_key.as_str(), BOX_KEY], false),
            tombstone_entry("bafyprev", &key),
        ]);
        let verdict = verify_audit_log(&body, &held(&[key.pubkey_did_key.as_str()])).unwrap();
        assert!(
            matches!(
                verdict,
                SeniorityVerdict::Mismatch(MismatchReason::OwnRetirement { .. })
            ),
            "the seniority question is still answered no, attributably: {verdict:?}"
        );
        assert!(verdict.is_terminal_retirement(), "{verdict:?}");
    }

    /// The RE-TAKEN half: a tombstone is the one op any *listed* key may sign, and
    /// the bridge's junior key is listed for the identity's whole life — so a
    /// tombstone whose signature does NOT verify against the held ring is
    /// someone else's destruction of an identity this client protects, and it
    /// must stay LOUD. Before this split, the box that destroyed the identity
    /// also silenced (and cleared) the alarm for it.
    #[test]
    fn a_tombstone_signed_by_an_unheld_key_is_not_a_retirement() {
        let ours = crate::rotation_key::generate_rotation_key(1);
        let theirs = crate::rotation_key::generate_rotation_key(2);
        let body = log(&[
            entry(&[ours.pubkey_did_key.as_str(), BOX_KEY], false),
            tombstone_entry("bafyprev", &theirs),
        ]);
        let verdict = verify_audit_log(&body, &held(&[ours.pubkey_did_key.as_str()])).unwrap();
        assert!(
            matches!(
                verdict,
                SeniorityVerdict::Mismatch(MismatchReason::RetirementByUnheldKey { .. })
            ),
            "{verdict:?}"
        );
        assert!(!verdict.is_terminal_retirement(), "{verdict:?}");

        // An unverifiable signature is the same loud answer — a garbage sig
        // must never read quieter than a wrong one.
        let unverifiable = log(&[
            entry(&[ours.pubkey_did_key.as_str(), BOX_KEY], false),
            foreign_entry(crate::tombstone::OP_TYPE_TOMBSTONE),
        ]);
        assert!(
            !verify_audit_log(&unverifiable, &held(&[ours.pubkey_did_key.as_str()]))
                .unwrap()
                .is_terminal_retirement()
        );
    }

    /// Deliberately narrow. A hostile or unknown op type is a log this
    /// client's key was never senior over — the alarm case in every caller's
    /// context — so only `plc_tombstone` may read as retirement.
    #[test]
    fn only_a_tombstone_reads_as_retirement() {
        let body = log(&[foreign_entry("plc_something_else")]);
        let verdict = verify_audit_log(&body, &held(&[USER_KEY])).unwrap();
        assert!(!verdict.is_terminal_retirement(), "{verdict:?}");

        // Nor does the ordinary alarm — a live op whose senior slot is the
        // box's key, which is the finding this whole feeder exists for.
        let live = log(&[entry(&[BOX_KEY], false)]);
        assert!(
            !verify_audit_log(&live, &held(&[USER_KEY]))
                .unwrap()
                .is_terminal_retirement()
        );
        // Nor a passing verdict.
        let ok = log(&[entry(&[USER_KEY], false)]);
        assert!(
            !verify_audit_log(&ok, &held(&[USER_KEY]))
                .unwrap()
                .is_terminal_retirement()
        );
    }

    #[test]
    fn genesis_with_user_senior_verifies() {
        let body = log(&[entry(&[USER_KEY, BOX_KEY], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Verified {
                standing_ops: 1,
                observed_seniors: vec![USER_KEY.to_string()],
            }
        );
    }

    /// Fresh-key-per-mint means the ring can hold a retired identity's
    /// burned key beside the live one — a log naming ANY held key at
    /// `rotationKeys[0]` is user custody and must verify, and the observed
    /// senior is reported for the caller's binding converge.
    #[test]
    fn any_held_key_senior_verifies() {
        const FRESH_KEY: &str = "did:key:zDnaeFreshReMintKey";
        let body = log(&[entry(&[FRESH_KEY, BOX_KEY], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY, FRESH_KEY])).unwrap(),
            SeniorityVerdict::Verified {
                standing_ops: 1,
                observed_seniors: vec![FRESH_KEY.to_string()],
            }
        );
    }

    /// An unheld key at `rotationKeys[0]` is still the alarm case, however
    /// many keys the ring holds — membership widens the set to every
    /// user-custodied key, never beyond it.
    #[test]
    fn unheld_senior_is_mismatch_with_multiple_held() {
        const OTHER: &str = "did:key:zDnaeSomebodyElse";
        let body = log(&[entry(&[OTHER, BOX_KEY], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY, "did:key:zDnaeFresh"])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some(OTHER.to_string()),
            })
        );
    }

    /// An empty ring can never verify: with nothing held, no published key
    /// is the user's. (The machine returns before calling in that case; this
    /// pins the pure function's own behavior.)
    #[test]
    fn empty_ring_never_verifies() {
        let body = log(&[entry(&[USER_KEY, BOX_KEY], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some(USER_KEY.to_string()),
            })
        );
    }

    #[test]
    fn genesis_with_box_senior_is_mismatch() {
        let body = log(&[entry(&[BOX_KEY, USER_KEY], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some(BOX_KEY.to_string()),
            })
        );
    }

    #[test]
    fn absent_rotation_keys_is_mismatch() {
        let body = log(&[entry(&[], false)]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: None,
            })
        );
    }

    /// The 72 h fork window: a later standing op demoting the user's key must
    /// alarm even though the genesis (and possibly the head) look correct.
    #[test]
    fn later_op_demoting_user_is_mismatch() {
        let body = log(&[
            entry(&[USER_KEY, BOX_KEY], false),
            entry(&[BOX_KEY], false),
            entry(&[USER_KEY, BOX_KEY], false),
        ]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::SeniorKeyDiffers {
                standing_index: 1,
                found: Some(BOX_KEY.to_string()),
            })
        );
    }

    /// A nullified hostile op no longer stands (the user's senior key
    /// contested it) — the surviving chain verifies.
    #[test]
    fn nullified_ops_are_skipped() {
        let body = log(&[
            entry(&[USER_KEY, BOX_KEY], false),
            entry(&[BOX_KEY], true),
            entry(&[USER_KEY], false),
        ]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Verified {
                standing_ops: 2,
                observed_seniors: vec![USER_KEY.to_string()],
            }
        );
    }

    /// A legacy v0 `create` op is a shape our mint never produces — treating
    /// it as quiet would let a hostile box dodge the check, so it alarms.
    #[test]
    fn legacy_create_op_is_mismatch() {
        let mut e = entry(&[USER_KEY], false);
        e["operation"]["type"] = "create".into();
        let body = log(&[e]);
        assert_eq!(
            verify_audit_log(&body, &held(&[USER_KEY])).unwrap(),
            SeniorityVerdict::Mismatch(MismatchReason::NotAPlcOperation {
                standing_index: 0,
                op_type: "create".to_string(),
            })
        );
    }

    #[test]
    fn empty_or_fully_nullified_log_is_quiet_failure() {
        assert!(matches!(
            verify_audit_log(b"[]", &held(&[USER_KEY])),
            Err(VerifyFailure::NoStandingOps)
        ));
        let body = log(&[entry(&[BOX_KEY], true)]);
        assert!(matches!(
            verify_audit_log(&body, &held(&[USER_KEY])),
            Err(VerifyFailure::NoStandingOps)
        ));
    }

    #[test]
    fn garbage_body_is_quiet_failure() {
        assert!(matches!(
            verify_audit_log(b"not json", &held(&[USER_KEY])),
            Err(VerifyFailure::Parse(_))
        ));
    }

    #[test]
    fn oversized_body_is_quiet_failure() {
        let body = vec![b' '; MAX_AUDIT_LOG_BYTES + 1];
        assert!(matches!(
            verify_audit_log(&body, &held(&[USER_KEY])),
            Err(VerifyFailure::TooLarge)
        ));
    }

    #[test]
    fn base_url_default_stands() {
        // The env seam is exercised by the tier_3 harness; here only the
        // default path (no env set in unit tests).
        assert_eq!(DEFAULT_PLC_DIRECTORY_URL, "https://plc.directory");
    }
}
