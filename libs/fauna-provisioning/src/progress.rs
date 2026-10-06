//! Snapshot-pull progress model for the four-step provisioning flow.
//!
//! Design tracked internally.
//! UI logic is "render this struct" — the orchestrator updates the snapshot
//! and calls `notify()` on every meaningful change; clients re-read
//! `ProvisioningSnapshot` on each tick.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

use crate::error::ProvisionError;

/// User-visible step in the four-step model. Step boundaries chosen so each
/// step is contiguous in time and has a single failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ProvisionStep {
    /// Domain reservation: registrar.register (when buying) + dns.verify
    /// to confirm the zone exists in the chosen DNS provider.
    Domain,
    /// VPS reservation: DKIM keygen + cloud-init render + create_server.
    /// Returns when the provider hands back the IPv4; cloud-init boot
    /// continues asynchronously.
    Server,
    /// All DNS publishing: A, MX, SPF, DMARC, two DKIM TXT records, PTR.
    Dns,
    /// Wait for `GET https://{domain}/api/v1/health` to return 200.
    Online,
}

impl ProvisionStep {
    /// Per-step retry budget. Step 4 (`Online`) inherits the previous
    /// 60×5s health-poll loop; the other three retry transient failures
    /// up to 3 times with exponential backoff (1s → 2s → 4s).
    pub fn default_retry_policy(self) -> RetryPolicy {
        match self {
            ProvisionStep::Domain => RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1000,
                max_backoff_ms: 4000,
            },
            ProvisionStep::Server => RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1000,
                max_backoff_ms: 4000,
            },
            ProvisionStep::Dns => RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1000,
                max_backoff_ms: 4000,
            },
            // Ceiling only — the poll exits as soon as the nest serves /health
            // over its floor cert. On native provisioning the poll reaches the box
            // by its captured static IP via a temporary DNS override
            // (`run_all_steps` builds the liveness client with `.resolve(domain ->
            // <ip>:443)`), so "Online" needs only the nest serving — NOT public DNS
            // propagation. What stacks into the budget then is the cold-boot time:
            // a fresh box `docker pull`s the nest image (a mail box also pulls the
            // ~1.5 GB clamd + rspamd sidecars — ~4-8 min to serve). 15 min (the
            // prior 180×5s) once timed a healthy Hetzner box out, so the ceiling is
            // a generous 480×5s = 40 min; a fast box still exits early — this is a
            // ceiling, not a fixed wait.
            //
            // ⚠ This is an attempt COUNT, not a wall clock, and the "40 min"
            // above only holds while each attempt is short. It was not bounded at
            // all until 2026-08-31: the probe clients set no timeout, so a dial
            // into a silent socket hung indefinitely and ONE attempt swallowed the
            // whole step — measured live on windows against a real Hetzner box,
            // `attempt 2/480` after 1200s. Each dial now stops at
            // `PROBE_CONNECT_TIMEOUT`/`PROBE_REQUEST_TIMEOUT`
            // (`fauna-onboarding-machine::machine`), so a healthy-but-slow box
            // still polls at the ~5s cadence this 40-min figure describes, while a
            // box that never answers bounds at 480×(10s+5s) rather than forever.
            //
            // ⚠ This ceiling was ALSO sized (2026-07-04) on the belief that it
            // covered the no-override path (web/wasm, where the browser owns DNS)
            // waiting out Hetzner's ~30-min DNS publish. It does not, and cannot:
            // a domainless box serves only its loopback-SAN floor until the claim
            // names it, so a browser's poll fails on TLS no matter how long DNS is
            // given — both live web runs of 2026-08-29 exhausted all 480 attempts.
            // Web's fix is the nest's IP bridge cert + polling the box at its IP on
            // every app (docs/goal/behavior/onboarding.md § 6 "Reaching the box";
            // docs/goal/architecture/nest/tls-certificates.md § B-IP); once that
            // lands, this ceiling bounds box boot + the bridge cert's first
            // issuance on every target, and nothing here waits on DNS.
            ProvisionStep::Online => RetryPolicy {
                max_attempts: 480,
                initial_backoff_ms: 5000,
                max_backoff_ms: 5000,
            },
        }
    }
}

/// Per-step retry budget. The orchestrator's `run_step` loop reads these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_ms: u32,
    pub max_backoff_ms: u32,
}

/// Classify an error as transient (retry within the step's budget) or
/// terminal (fail the step immediately). Network/timeout/5xx/408/429 are
/// transient; 4xx parse-clean responses and "Cancelled" are terminal.
pub fn is_transient(err: &ProvisionError) -> bool {
    match err {
        // `reqwest::Error::is_connect()` isn't exposed on wasm32 (the
        // upstream gates it to non-wasm targets). On wasm we rely on
        // is_timeout + is_request to cover transport-level transients;
        // is_request matches roughly the same set of "low-level transport
        // failure" cases on wasm that is_connect catches on native.
        #[cfg(not(target_arch = "wasm32"))]
        ProvisionError::Http(e) => e.is_timeout() || e.is_connect() || e.is_request(),
        #[cfg(target_arch = "wasm32")]
        ProvisionError::Http(e) => e.is_timeout() || e.is_request(),
        ProvisionError::Provider { status, .. } => {
            *status == 408 || *status == 429 || (500..=599).contains(status)
        }
        _ => false,
    }
}

/// Overall provisioning state. The UI hides Cancel/Retry depending on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum OverallStatus {
    Idle,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl OverallStatus {
    /// No run has started yet — the Start CTA is shown.
    pub fn is_idle(&self) -> bool {
        matches!(self, OverallStatus::Idle)
    }

    /// Provisioning is actively running — the Cancel button is shown and the
    /// elapsed-time ticker advances.
    pub fn is_running(&self) -> bool {
        matches!(self, OverallStatus::Running)
    }

    /// The run can be retried — it `Failed`, or was soft-`Cancelled`. Both are
    /// stopped-with-resources-intact states: a cancel aborts at the next step
    /// boundary but leaves created VPS/DNS resources in place, so retry is the
    /// user's resume path out of either (idempotency short-circuits completed
    /// steps). Without the `Cancelled` arm a soft-cancel would strand the user
    /// with only Back. Canonical rule for `onboarding.md` §6's retry-button row.
    pub fn can_retry(&self) -> bool {
        matches!(self, OverallStatus::Failed | OverallStatus::Cancelled)
    }

    /// The run finished successfully — the wizard-exit Continue button enables.
    pub fn can_continue(&self) -> bool {
        matches!(self, OverallStatus::Succeeded)
    }

    /// Why [`Self::can_continue`] says no, or `None` when it says yes.
    ///
    /// The sibling-getter shape (`dns_provider_ineligible_reason` beside
    /// `dns_provider_eligible`): verdict and explanation read the same value,
    /// so they cannot drift apart. Every app greys
    /// `provisioning-continue-button` off `can_continue()`, which is false for
    /// this screen's whole life until the run succeeds — the four `○` step
    /// glyphs are a symbol, not a reason, and `ui/README.md` rule 5 asks for
    /// one within eyeshot.
    ///
    /// One line per blocked state rather than a single generic one, because
    /// the user's next act differs in each: start it, wait, or retry (rule 5's
    /// Q2 — the `error_no_set` / `error_no_sync_set` precedent).
    pub fn continue_blocked_reason(&self) -> Option<LocalizedText> {
        let key = match self {
            OverallStatus::Succeeded => return None,
            OverallStatus::Idle => "onboarding.nest_provisioning.continue_blocked_idle",
            OverallStatus::Running => "onboarding.nest_provisioning.continue_blocked_running",
            OverallStatus::Failed => "onboarding.nest_provisioning.continue_blocked_failed",
            OverallStatus::Cancelled => "onboarding.nest_provisioning.continue_blocked_cancelled",
        };
        Some(LocalizedText::key(key))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum StepStatus {
    Pending,
    Running,
    Skipped,
    Succeeded,
    Failed,
}

/// Typed sub-step keys; the UI maps each to the localized string from
/// `i18n/strings/en.yaml` under `onboarding.provision.substep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SubstepKey {
    DomainCheckingAvailability,
    DomainRegistering,
    DomainVerifyingZone,
    ServerGeneratingDkim,
    ServerCreating,
    DnsAddingDomainRecords,
    DnsAddingEmailRecords,
    DnsSettingReverseDns,
    OnlineWaiting,
    /// The claim that turns "built" into "built **and claimed**" — `Online`'s
    /// final substep on the standard path, run by the machine (not this crate's
    /// provider-facing orchestrator) the instant `/health` answers.
    /// `docs/goal/behavior/onboarding.md` § 6 *Provisioning = build + claim*.
    OnlineClaiming,
    StatusSkipped,
    StatusRetrying,
    StatusCancelling,
    StatusCancelled,
}

/// Why a step short-circuited via the pre-flight idempotency check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SkipReason {
    ZoneAlreadyVerified,
    ServerAlreadyExists,
    DnsRecordAlreadyExists,
    PtrAlreadySet,
    NestAlreadyOnline,
}

/// One step's slot in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StepSnapshot {
    pub kind: ProvisionStep,
    pub status: StepStatus,
    pub substep: Option<SubstepKey>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
    pub skip_reason: Option<SkipReason>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// Display projection: whether the `provisioning-substep` text row should
    /// render. Whether the `provisioning-step-error` row should render. Whether
    /// the `(attempt N of M)` suffix should be appended.
    ///
    /// These three are the *single source* of the per-step visibility rule that
    /// every app used to re-derive (it had drifted — Android most of all).
    /// They are populated by `ProvisioningSnapshot::enrich_display`, called by
    /// the machine's `provisioning_snapshot()` getter on its outgoing clone, so
    /// every app — including those across the uniffi/wasm boundary that
    /// cannot call methods on a `Record` — reads identical booleans instead of
    /// re-deriving the rule. **Do not read these off a snapshot obtained outside
    /// that getter** (live mutated state leaves them stale; see
    /// `recompute_display`). The canonical rule lives in `recompute_display`.
    ///
    /// `#[serde(default)]`: a producer that hasn't run `enrich_display` — only a
    /// hand-built test fixture (`set_provisioning_snapshot_for_test`), since every
    /// production snapshot is born through the enriching getter — still
    /// deserializes (defaulting to `false`); the reader's getter recomputes them.
    #[serde(default)]
    pub shows_substep: bool,
    #[serde(default)]
    pub shows_error: bool,
    #[serde(default)]
    pub shows_attempt_suffix: bool,
}

impl StepSnapshot {
    fn fresh(kind: ProvisionStep) -> Self {
        Self {
            kind,
            status: StepStatus::Pending,
            substep: None,
            attempt: 0,
            max_attempts: kind.default_retry_policy().max_attempts,
            last_error: None,
            skip_reason: None,
            started_at_ms: None,
            finished_at_ms: None,
            // Display projections — filled by `recompute_display` at getter time.
            shows_substep: false,
            shows_error: false,
            shows_attempt_suffix: false,
        }
    }

    /// Recompute the display-projection fields (`shows_substep`, `shows_error`,
    /// `shows_attempt_suffix`) from this step's `status` / `substep` / `attempt` /
    /// `last_error`. This is the **canonical** per-step visibility rule — the one
    /// place it lives; clients read the resulting fields and never re-derive it.
    /// Driven by `ProvisioningSnapshot::enrich_display` → the `provisioning_snapshot()`
    /// getter, never stored through a mutation (the `set_cancelled` path mutates
    /// step status outside `with_step`, so a stored-and-recomputed-on-mutate field
    /// would go stale there).
    ///
    /// - **Substep row** — shown on `Skipped` (the "already configured" placeholder
    ///   text), and on `Running`/`Failed` when there is a substep label *or* an
    ///   attempt suffix to show. That is exactly when the client's resolved text is
    ///   non-empty, relying on the i18n invariant that every `SubstepKey` label and
    ///   the `status_skipped` / attempt-suffix strings are non-empty. Hidden on
    ///   `Pending`/`Succeeded`.
    /// - **Error row** — only on `Failed` with a non-empty `last_error`. A `Running`
    ///   row mid-retry can still carry a stale `last_error` from the prior attempt,
    ///   which must not surface — hence the `Failed` gate (not "when populated").
    /// - **Attempt suffix** — `max_attempts > 1 && attempt > 1`.
    pub fn recompute_display(&mut self) {
        self.shows_attempt_suffix = self.max_attempts > 1 && self.attempt > 1;
        self.shows_substep = match self.status {
            StepStatus::Skipped => true,
            StepStatus::Running | StepStatus::Failed => {
                self.substep.is_some() || self.shows_attempt_suffix
            }
            StepStatus::Pending | StepStatus::Succeeded => false,
        };
        self.shows_error = matches!(self.status, StepStatus::Failed)
            && self.last_error.as_deref().is_some_and(|m| !m.is_empty());
    }
}

/// The full snapshot. Always carries exactly four entries in fixed order:
/// Domain, Server, Dns, Online.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProvisioningSnapshot {
    pub overall: OverallStatus,
    pub steps: Vec<StepSnapshot>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// Set when overall == Succeeded. Mirrors the previous flow's
    /// `ProvisionResult` return value.
    pub result: Option<ProvisionResultPlain>,
    /// Set when overall == Failed or Cancelled.
    pub final_error: Option<String>,
}

impl ProvisioningSnapshot {
    /// Fill every step's display-projection fields (`shows_substep` etc.). The
    /// machine's `provisioning_snapshot()` getter calls this on the outgoing
    /// clone so all apps — native (uniffi Record fields), web (the JSON-parsed
    /// snapshot), and linux (the same getter) — read identical booleans rather
    /// than re-deriving the per-step visibility rule. See `StepSnapshot::recompute_display`.
    pub fn enrich_display(&mut self) {
        for step in self.steps.iter_mut() {
            step.recompute_display();
        }
    }

    pub fn idle() -> Self {
        Self {
            overall: OverallStatus::Idle,
            steps: vec![
                StepSnapshot::fresh(ProvisionStep::Domain),
                StepSnapshot::fresh(ProvisionStep::Server),
                StepSnapshot::fresh(ProvisionStep::Dns),
                StepSnapshot::fresh(ProvisionStep::Online),
            ],
            started_at_ms: None,
            finished_at_ms: None,
            result: None,
            final_error: None,
        }
    }
}

impl Default for ProvisioningSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

/// FFI-friendly mirror of the orchestrator's `ProvisionResult`. The original
/// has the same shape but lives in `orchestrator.rs`; this duplicate exists
/// so `ProvisioningSnapshot` can derive `uniffi::Record` without pulling
/// uniffi into the orchestrator types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProvisionResultPlain {
    pub server_id: String,
    pub ipv4: String,
    pub domain: String,
    pub claim_code: String,
}

/// What `run_step`'s `work` closure returns on success. The orchestrator
/// distinguishes "the step did real work" from "the step pre-flight matched
/// already-desired state." Both transition the step to its terminal status,
/// but the snapshot reports `Skipped` so the user sees the difference.
#[derive(Debug, Clone)]
pub enum StepOutcome {
    Succeeded,
    Skipped(SkipReason),
}

/// Soft-cancel flag. Set by `cancel_provisioning`; checked by `run_step`
/// at every step boundary and retry iteration. Already-created VPS/DNS
/// resources stay; a subsequent retry picks them up via idempotency.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn raise(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
    pub fn is_raised(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// Convenience used by `run_step` to short-circuit with `Cancelled`.
    pub fn check(&self) -> Result<(), ProvisionError> {
        if self.is_raised() {
            Err(ProvisionError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Best-effort current-time-ms. Returns 0 on platforms where the clock
/// isn't accessible (the snapshot times are decorative; the loop logic
/// doesn't depend on them).
pub fn now_ms() -> u64 {
    // Delegate to the shared wasm-safe clock. A hand-rolled
    // `SystemTime::now()` panics on wasm32 (the platform has no system
    // clock), which aborts the spawned provisioning task on its very
    // first timestamp and poisons the snapshot mutex — provisioning was
    // entirely broken on web until this delegation. `Timestamp::now_millis`
    // uses `js_sys::Date::now()` under `wasm32 + js` and `SystemTime`
    // elsewhere; the `js` plumbing rides this crate's `js` feature
    // (→ `fauna-core/js`).
    fauna_core::data::Timestamp::now_millis()
}

/// Wall-clock elapsed for the `provisioning-elapsed` ticker, as the shared
/// i18n string (`onboarding.nest_provisioning.elapsed_template` → "{seconds}s
/// elapsed"). Returns `None` until the run has started (the row stays hidden);
/// once `finished_at_ms` is set it freezes there (the ticker stops on
/// Succeeded/Failed/Cancelled), otherwise it ticks against the client's live
/// `now_ms`. Saturating: a backwards clock yields 0, never an underflow.
///
/// The five apps that render this row previously hand-rolled the same
/// subtraction + i18n glue — and web bypassed i18n entirely with hardcoded
/// `m`/`s` literals. Per priority #2 and `docs/goal/behavior/value-formatting.md`
/// (§ "Where logic lives") the decision lives in shared Rust once and is
/// returned as an i18n key + args via [`LocalizedText`], which each app
/// resolves through its own localization pipeline. A free function (not a
/// `Record` method) because the snapshot crosses the uniffi/wasm boundary as a
/// `Record` and clients hold `started_at_ms`/`finished_at_ms` already; `now_ms`
/// is the client's live tick.
pub fn elapsed_display(
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    now_ms: u64,
) -> Option<LocalizedText> {
    let start = started_at_ms?;
    let end = finished_at_ms.unwrap_or(now_ms);
    let secs = end.saturating_sub(start) / 1000;
    Some(LocalizedText::key_arg(
        "onboarding.nest_provisioning.elapsed_template",
        "seconds",
        secs.to_string(),
    ))
}

/// The status-column glyph for a provisioning step — canonical across all six
/// apps. Resolves the prior glyph drift (linux/apple `…`/`—`/`✗` vs
/// windows/web `⟳`/`−`/`✕`) to the linux/apple set: `…`/`—`/`✗` render and read
/// more reliably across platform fonts and assistive tech than the
/// gapped-circle-arrow / minus-sign / multiplication-x, and em-dash + ballot-X
/// are the more conventional "skipped"/"failed" marks (`○`/`✓` already agreed
/// everywhere). A plain `String`, **not** a [`LocalizedText`]: the symbol is
/// locale-invariant — nothing to translate (same rationale as
/// `fauna_core::format::short_id`). The apps that render the
/// `provisioning-step-checkbox` column consume this instead of re-deriving it.
pub fn status_glyph(status: StepStatus) -> String {
    match status {
        StepStatus::Pending => "○",
        StepStatus::Running => "…",
        StepStatus::Skipped => "—",
        StepStatus::Succeeded => "✓",
        StepStatus::Failed => "✗",
    }
    .to_string()
}

/// The i18n key for a step's user-visible name, as a [`LocalizedText`] the client
/// resolves through its own pipeline (same carrier as [`elapsed_display`]). The
/// canonical key family is `onboarding.provision.step.*` — the cohesive home
/// alongside `substep.*` / `step_failed` / `step_attempt_template`; the
/// identical-valued `onboarding.nest_provisioning.step.*` entries were a stray
/// duplicate, removed 2026-06-14 once android (the last remaining consumer)
/// swapped onto this fn. No args.
pub fn step_label(kind: ProvisionStep) -> LocalizedText {
    let key = match kind {
        ProvisionStep::Domain => "onboarding.provision.step.domain",
        ProvisionStep::Server => "onboarding.provision.step.server",
        ProvisionStep::Dns => "onboarding.provision.step.dns",
        ProvisionStep::Online => "onboarding.provision.step.online",
    };
    LocalizedText::key(key)
}

/// The i18n key for a sub-step's text, as a [`LocalizedText`]. For
/// [`SubstepKey::StatusRetrying`] — whose string is `"Retrying after error:
/// {cause}"` — the `{cause}` arg is filled from `cause` (the step's
/// `last_error`); every other key ignores it. Carrying the arg unifies the
/// apps upward to web's behavior: linux/apple/windows resolved the bare
/// `status_retrying` key and showed the user a literal `{cause}`.
pub fn substep_label(key: SubstepKey, cause: Option<String>) -> LocalizedText {
    let k = match key {
        SubstepKey::DomainCheckingAvailability => {
            "onboarding.provision.substep.domain_checking_availability"
        }
        SubstepKey::DomainRegistering => "onboarding.provision.substep.domain_registering",
        SubstepKey::DomainVerifyingZone => "onboarding.provision.substep.domain_verifying_zone",
        SubstepKey::ServerGeneratingDkim => "onboarding.provision.substep.server_generating_dkim",
        SubstepKey::ServerCreating => "onboarding.provision.substep.server_creating",
        SubstepKey::DnsAddingDomainRecords => {
            "onboarding.provision.substep.dns_adding_domain_records"
        }
        SubstepKey::DnsAddingEmailRecords => {
            "onboarding.provision.substep.dns_adding_email_records"
        }
        SubstepKey::DnsSettingReverseDns => "onboarding.provision.substep.dns_setting_reverse_dns",
        SubstepKey::OnlineWaiting => "onboarding.provision.substep.online_waiting",
        SubstepKey::OnlineClaiming => "onboarding.provision.substep.online_claiming",
        SubstepKey::StatusSkipped => "onboarding.provision.substep.status_skipped",
        SubstepKey::StatusRetrying => "onboarding.provision.substep.status_retrying",
        SubstepKey::StatusCancelling => "onboarding.provision.substep.status_cancelling",
        SubstepKey::StatusCancelled => "onboarding.provision.substep.status_cancelled",
    };
    match key {
        SubstepKey::StatusRetrying => LocalizedText::key_arg(k, "cause", cause.unwrap_or_default()),
        _ => LocalizedText::key(k),
    }
}

fn step_index(step: ProvisionStep) -> usize {
    match step {
        ProvisionStep::Domain => 0,
        ProvisionStep::Server => 1,
        ProvisionStep::Dns => 2,
        ProvisionStep::Online => 3,
    }
}

fn with_step<F>(state: &Mutex<ProvisioningSnapshot>, step: ProvisionStep, f: F)
where
    F: FnOnce(&mut StepSnapshot),
{
    let mut guard = state.lock().unwrap();
    let idx = step_index(step);
    f(&mut guard.steps[idx]);
}

/// Mutate the running step's slot to reflect a new attempt.
pub fn set_running(
    state: &Mutex<ProvisioningSnapshot>,
    step: ProvisionStep,
    attempt: u32,
    max_attempts: u32,
    substep: Option<SubstepKey>,
) {
    {
        let mut guard = state.lock().unwrap();
        if matches!(guard.overall, OverallStatus::Idle) {
            guard.overall = OverallStatus::Running;
            guard.started_at_ms = Some(now_ms());
        }
    }
    with_step(state, step, |s| {
        s.status = StepStatus::Running;
        s.attempt = attempt;
        s.max_attempts = max_attempts;
        if s.started_at_ms.is_none() {
            s.started_at_ms = Some(now_ms());
        }
        if let Some(k) = substep {
            s.substep = Some(k);
        }
    });
}

/// Record a transient failure mid-step; the loop will sleep and retry.
pub fn set_retrying(
    state: &Mutex<ProvisioningSnapshot>,
    step: ProvisionStep,
    attempt: u32,
    err: &ProvisionError,
) {
    with_step(state, step, |s| {
        s.attempt = attempt;
        s.last_error = Some(err.to_string());
        s.substep = Some(SubstepKey::StatusRetrying);
    });
}

/// Mark the step terminal (Succeeded or Skipped).
pub fn set_finished(
    state: &Mutex<ProvisioningSnapshot>,
    step: ProvisionStep,
    outcome: &StepOutcome,
) {
    with_step(state, step, |s| {
        s.finished_at_ms = Some(now_ms());
        match outcome {
            StepOutcome::Succeeded => {
                s.status = StepStatus::Succeeded;
                s.last_error = None;
                s.skip_reason = None;
            }
            StepOutcome::Skipped(reason) => {
                s.status = StepStatus::Skipped;
                s.skip_reason = Some(*reason);
                s.substep = Some(SubstepKey::StatusSkipped);
                s.last_error = None;
            }
        }
    });
}

/// Mark the step Failed and copy the cause into `last_error`. Caller
/// also sets `overall = Failed` and `final_error`.
pub fn set_failed(
    state: &Mutex<ProvisioningSnapshot>,
    step: ProvisionStep,
    attempt: u32,
    err: &ProvisionError,
) {
    {
        let mut guard = state.lock().unwrap();
        guard.overall = OverallStatus::Failed;
        guard.finished_at_ms = Some(now_ms());
        guard.final_error = Some(err.to_string());
    }
    with_step(state, step, |s| {
        s.status = StepStatus::Failed;
        s.attempt = attempt;
        s.last_error = Some(err.to_string());
        s.finished_at_ms = Some(now_ms());
    });
}

/// Mark the run cancelled. Already-completed steps keep their statuses; the
/// in-flight step (if any) is marked Failed with a "cancelled" message.
pub fn set_cancelled(state: &Mutex<ProvisioningSnapshot>) {
    let mut guard = state.lock().unwrap();
    guard.overall = OverallStatus::Cancelled;
    guard.finished_at_ms = Some(now_ms());
    guard.final_error = Some("Cancelled".to_string());
    for s in guard.steps.iter_mut() {
        if matches!(s.status, StepStatus::Running) {
            s.status = StepStatus::Failed;
            s.substep = Some(SubstepKey::StatusCancelled);
            s.last_error = Some("Cancelled".to_string());
            s.finished_at_ms = Some(now_ms());
        }
    }
}

/// Set a running step's substep text, leaving every other field alone — the
/// progress seam for work a *caller* runs inside a step (today: the machine's
/// claim, `Online`'s final substep).
pub fn set_substep(state: &Mutex<ProvisioningSnapshot>, step: ProvisionStep, key: SubstepKey) {
    with_step(state, step, |s| s.substep = Some(key));
}

/// Re-open the finished `Online` step for its **claiming** substep, and put the
/// run back to `Running`.
///
/// The claim is `Online`'s last substep on the standard path but it is run by the
/// machine, not by this crate's provider-facing orchestrator
/// (`docs/goal/behavior/onboarding.md` § 6 *Provisioning = build + claim*), so it
/// lands after the orchestrator has already marked the run succeeded. Reopening —
/// rather than leaving `Succeeded` up while the claim flies — is what keeps
/// `can_continue` false through it: `Succeeded` on this path must mean *built and
/// claimed*, and a Continue answered in between would leave the box unclaimed.
/// Pair with [`set_claim_succeeded`] (or `set_failed` on a refused claim).
pub fn set_claiming(state: &Mutex<ProvisioningSnapshot>) {
    {
        let mut guard = state.lock().unwrap();
        guard.overall = OverallStatus::Running;
        guard.finished_at_ms = None;
        guard.final_error = None;
    }
    with_step(state, ProvisionStep::Online, |s| {
        s.status = StepStatus::Running;
        s.substep = Some(SubstepKey::OnlineClaiming);
        s.last_error = None;
        s.finished_at_ms = None;
    });
}

/// Close the claiming substep [`set_claiming`] opened: `Online` is finished and
/// the run is `Succeeded` again — now meaning built **and claimed**. The stashed
/// result is untouched.
pub fn set_claim_succeeded(state: &Mutex<ProvisioningSnapshot>) {
    with_step(state, ProvisionStep::Online, |s| {
        s.status = StepStatus::Succeeded;
        s.substep = None;
        s.last_error = None;
        s.finished_at_ms = Some(now_ms());
    });
    let mut guard = state.lock().unwrap();
    guard.overall = OverallStatus::Succeeded;
    guard.finished_at_ms = Some(now_ms());
    guard.final_error = None;
}

/// Mark overall = Succeeded and stash the result.
pub fn set_run_succeeded(state: &Mutex<ProvisioningSnapshot>, result: ProvisionResultPlain) {
    let mut guard = state.lock().unwrap();
    guard.overall = OverallStatus::Succeeded;
    guard.finished_at_ms = Some(now_ms());
    guard.result = Some(result);
    guard.final_error = None;
}

/// The generic step driver. Handles pre-flight via `work`'s `Skipped`
/// return, retry-on-transient with exponential backoff, and snapshot
/// transitions. Caller passes a `work(attempt)` closure that does the
/// idempotency check first, then the actual mutation; if the world is
/// already in the desired state it returns `Ok(StepOutcome::Skipped(...))`
/// and the loop exits with attempt=1.
///
/// `notify` is called after each snapshot transition. `sleep_fn` is the
/// platform-appropriate sleep — `tokio::time::sleep` on native, a JS
/// timer-based future on wasm.
#[allow(clippy::too_many_arguments)]
pub async fn run_step<F, Fut, N, S, SFut>(
    state: &Mutex<ProvisioningSnapshot>,
    notify: &N,
    cancel: &CancelFlag,
    step: ProvisionStep,
    policy: RetryPolicy,
    initial_substep: Option<SubstepKey>,
    work: F,
    sleep_fn: &S,
) -> Result<StepOutcome, ProvisionError>
where
    F: Fn(u32) -> Fut,
    Fut: std::future::Future<Output = Result<StepOutcome, ProvisionError>>,
    N: Fn() + Send + Sync,
    S: Fn(u64) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let mut backoff = policy.initial_backoff_ms;
    for attempt in 1..=policy.max_attempts {
        cancel.check()?;
        set_running(state, step, attempt, policy.max_attempts, initial_substep);
        notify();

        // Load-bearing line: a live run that freezes here is a silent hang
        // otherwise — this is the only line that says which attempt (of how
        // many) was in flight when it stalled. Measured live on windows
        // against a real Hetzner box: a 27-minute captured app log held
        // exactly one line total because this loop emitted nothing.
        tracing::info!(
            target: "fauna_provisioning",
            "provisioning step {step:?}: attempt {attempt}/{} starting",
            policy.max_attempts,
        );

        let outcome = work(attempt).await;
        match outcome {
            Ok(out) => {
                tracing::info!(
                    target: "fauna_provisioning",
                    "provisioning step {step:?}: attempt {attempt} finished ({out:?})",
                );
                set_finished(state, step, &out);
                notify();
                return Ok(out);
            }
            Err(e) if attempt < policy.max_attempts && is_transient(&e) => {
                tracing::warn!(
                    target: "fauna_provisioning",
                    "provisioning step {step:?}: attempt {attempt}/{} failed transiently ({e}); retrying in {backoff}ms",
                    policy.max_attempts,
                );
                set_retrying(state, step, attempt, &e);
                notify();
                cancel.check()?;
                sleep_fn(backoff as u64).await;
                backoff = (backoff.saturating_mul(2)).min(policy.max_backoff_ms);
            }
            Err(e) => {
                tracing::error!(
                    target: "fauna_provisioning",
                    "provisioning step {step:?}: failed terminally after {attempt} attempt(s): {e}",
                );
                set_failed(state, step, attempt, &e);
                notify();
                return Err(ProvisionError::StepFailed {
                    step,
                    attempts: attempt,
                    cause: Box::new(e),
                });
            }
        }
    }
    unreachable!("the inner Err branch returns when attempt == max_attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(snap: &ProvisioningSnapshot, kind: ProvisionStep) -> &StepSnapshot {
        &snap.steps[step_index(kind)]
    }

    #[test]
    fn elapsed_display_computes_freezes_and_guards() {
        // Not started → no row.
        assert_eq!(elapsed_display(None, None, 5_000), None);
        assert_eq!(elapsed_display(None, Some(9_000), 5_000), None);

        // Running: ticks against now_ms, ms→secs (floored).
        let lt = elapsed_display(Some(1_000), None, 6_500).unwrap();
        assert_eq!(lt.key, "onboarding.nest_provisioning.elapsed_template");
        assert_eq!(lt.args.get("seconds").map(String::as_str), Some("5"));

        // Finished: freezes at finished_at_ms, ignoring a later now_ms.
        let lt = elapsed_display(Some(1_000), Some(4_000), 60_000).unwrap();
        assert_eq!(lt.args.get("seconds").map(String::as_str), Some("3"));

        // Backwards clock (now < start): saturating → 0, no underflow.
        let lt = elapsed_display(Some(10_000), None, 5_000).unwrap();
        assert_eq!(lt.args.get("seconds").map(String::as_str), Some("0"));
    }

    #[test]
    fn fresh_snapshot_has_four_pending_steps() {
        let s = ProvisioningSnapshot::idle();
        assert_eq!(s.overall, OverallStatus::Idle);
        assert_eq!(s.steps.len(), 4);
        for st in &s.steps {
            assert_eq!(st.status, StepStatus::Pending);
        }
    }

    #[test]
    fn overall_status_affordance_predicates() {
        use OverallStatus::*;

        // Start button: only while no run has begun.
        assert!(Idle.is_idle());
        for s in [Running, Succeeded, Failed, Cancelled] {
            assert!(!s.is_idle());
        }

        // Cancel button + elapsed ticker: only while actively running.
        assert!(Running.is_running());
        for s in [Idle, Succeeded, Failed, Cancelled] {
            assert!(!s.is_running());
        }

        // Retry button: Failed OR soft-Cancelled (both stopped-with-resources
        // states; retry resumes via idempotency).
        assert!(Failed.can_retry());
        assert!(Cancelled.can_retry());
        for s in [Idle, Running, Succeeded] {
            assert!(!s.can_retry());
        }

        // Continue (wizard exit): only on success.
        assert!(Succeeded.can_continue());
        for s in [Idle, Running, Failed, Cancelled] {
            assert!(!s.can_continue());
        }
    }

    /// The reason getter is bound to its verdict, not maintained beside it:
    /// exactly the states that block Continue carry a line, and the one that
    /// allows it carries none. Stated as the biconditional so a later arm
    /// added to `OverallStatus` cannot land silently reason-less and leave a
    /// DIM Continue unexplained (`ui/README.md` rule 5).
    #[test]
    fn every_state_that_blocks_continue_says_why_and_the_one_that_allows_it_does_not() {
        use OverallStatus::*;
        for s in [Idle, Running, Succeeded, Failed, Cancelled] {
            assert_eq!(
                s.continue_blocked_reason().is_none(),
                s.can_continue(),
                "{s:?}: a reason must be present exactly when Continue is dead"
            );
        }
        // The four blocked states say four different things — the user's next
        // act is start / wait / retry / retry, and one generic line would lose
        // that (rule 5 Q2).
        let keys: Vec<String> = [Idle, Running, Failed, Cancelled]
            .iter()
            .map(|s| s.continue_blocked_reason().unwrap().key)
            .collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "blocked states share a line: {keys:?}"
        );
    }

    // Build a step in `status` with optional substep / attempt / last_error, then
    // recompute its display projection. Mirrors how the getter enriches a clone.
    fn display(
        status: StepStatus,
        substep: Option<SubstepKey>,
        attempt: u32,
        max_attempts: u32,
        last_error: Option<&str>,
    ) -> StepSnapshot {
        let mut s = StepSnapshot::fresh(ProvisionStep::Server);
        s.status = status;
        s.substep = substep;
        s.attempt = attempt;
        s.max_attempts = max_attempts;
        s.last_error = last_error.map(str::to_string);
        s.recompute_display();
        s
    }

    #[test]
    fn step_snapshot_display_predicates() {
        use StepStatus::*;

        // Pending / Succeeded: nothing renders.
        for st in [Pending, Succeeded] {
            let s = display(st, None, 1, 1, None);
            assert!(!s.shows_substep, "{st:?} substep");
            assert!(!s.shows_error, "{st:?} error");
            assert!(!s.shows_attempt_suffix, "{st:?} suffix");
        }

        // Skipped: substep row always shows (the "already configured" placeholder),
        // never an error row.
        let skipped = display(Skipped, Some(SubstepKey::StatusSkipped), 1, 1, None);
        assert!(skipped.shows_substep);
        assert!(!skipped.shows_error);

        // Running with a substep: substep shows; no error; no suffix at attempt 1.
        let running = display(Running, Some(SubstepKey::ServerCreating), 1, 3, None);
        assert!(running.shows_substep);
        assert!(!running.shows_error);
        assert!(!running.shows_attempt_suffix);

        // Running with no substep and not retried: text would be empty → hidden.
        let running_bare = display(Running, None, 1, 3, None);
        assert!(!running_bare.shows_substep);

        // Running with no substep but retried: the attempt suffix alone keeps the
        // row non-empty → shown.
        let running_retry = display(Running, None, 2, 3, None);
        assert!(running_retry.shows_substep);
        assert!(running_retry.shows_attempt_suffix);

        // Failed with substep + last_error: both rows show.
        let failed = display(Failed, Some(SubstepKey::ServerCreating), 3, 3, Some("boom"));
        assert!(failed.shows_substep);
        assert!(failed.shows_error);

        // Failed, no substep, not retried, no error: both rows hidden (the
        // Failed-no-substep case where linux/web previously diverged).
        let failed_bare = display(Failed, None, 1, 3, None);
        assert!(!failed_bare.shows_substep);
        assert!(!failed_bare.shows_error);

        // Failed, no substep, retried: suffix keeps the substep row (this is the
        // case web used to hide — the drift this lift resolves).
        let failed_retry = display(Failed, None, 2, 3, None);
        assert!(failed_retry.shows_substep);
        assert!(failed_retry.shows_attempt_suffix);

        // Failed with an empty last_error string: no error row (empty ≠ populated).
        let failed_empty = display(Failed, None, 1, 1, Some(""));
        assert!(!failed_empty.shows_error);

        // last_error set on a non-Failed (Running mid-retry) step never surfaces.
        let running_stale_err = display(
            Running,
            Some(SubstepKey::ServerCreating),
            2,
            3,
            Some("prior"),
        );
        assert!(!running_stale_err.shows_error);
    }

    #[test]
    fn enrich_display_fills_every_step() {
        let mut snap = ProvisioningSnapshot::idle();
        snap.steps[step_index(ProvisionStep::Dns)].status = StepStatus::Failed;
        snap.steps[step_index(ProvisionStep::Dns)].last_error = Some("dns boom".into());
        snap.enrich_display();
        assert!(step(&snap, ProvisionStep::Dns).shows_error);
        // Untouched Pending steps stay clean.
        assert!(!step(&snap, ProvisionStep::Online).shows_error);
        assert!(!step(&snap, ProvisionStep::Online).shows_substep);
    }

    #[test]
    fn snapshot_without_display_projections_deserializes_then_enriches() {
        // The e2e `set_provisioning_snapshot_for_test` fixture builds a raw
        // snapshot with no `shows_*` projection fields (it never runs
        // `enrich_display`). `#[serde(default)]` on those fields lets it parse;
        // the reader's getter then recomputes the projections. Regression guard
        // for the strict-deserialize break introduced when the fields landed.
        let mut built = ProvisioningSnapshot::idle();
        built.steps[step_index(ProvisionStep::Server)].status = StepStatus::Running;
        built.steps[step_index(ProvisionStep::Server)].substep = Some(SubstepKey::ServerCreating);
        let mut value = serde_json::to_value(&built).unwrap();
        for s in value["steps"].as_array_mut().unwrap() {
            let obj = s.as_object_mut().unwrap();
            obj.remove("shows_substep");
            obj.remove("shows_error");
            obj.remove("shows_attempt_suffix");
        }
        // Parses despite the absent projection fields (defaulting to false)...
        let mut parsed: ProvisioningSnapshot = serde_json::from_value(value).unwrap();
        assert!(!step(&parsed, ProvisionStep::Server).shows_substep);
        // ...and the getter's enrich recomputes the canonical projection.
        parsed.enrich_display();
        assert!(step(&parsed, ProvisionStep::Server).shows_substep);
    }

    #[test]
    fn is_transient_classifies_provider_errors() {
        assert!(is_transient(&ProvisionError::Provider {
            status: 503,
            body: String::new()
        }));
        assert!(is_transient(&ProvisionError::Provider {
            status: 429,
            body: String::new()
        }));
        assert!(is_transient(&ProvisionError::Provider {
            status: 408,
            body: String::new()
        }));
        assert!(!is_transient(&ProvisionError::Provider {
            status: 400,
            body: String::new()
        }));
        assert!(!is_transient(&ProvisionError::Provider {
            status: 401,
            body: String::new()
        }));
        assert!(!is_transient(&ProvisionError::Other("nope".into())));
        assert!(!is_transient(&ProvisionError::Cancelled));
    }

    #[test]
    fn default_retry_policies() {
        let p = ProvisionStep::Online.default_retry_policy();
        assert_eq!(p.max_attempts, 480); // 40-min fallback ceiling (reach-by-IP exits early)
        assert_eq!(p.initial_backoff_ms, 5000);
        let p = ProvisionStep::Domain.default_retry_policy();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.initial_backoff_ms, 1000);
    }

    #[test]
    fn cancel_flag_round_trip() {
        let f = CancelFlag::new();
        assert!(!f.is_raised());
        f.raise();
        assert!(f.is_raised());
        assert!(matches!(f.check(), Err(ProvisionError::Cancelled)));
        f.reset();
        assert!(!f.is_raised());
    }

    #[tokio::test]
    async fn run_step_marks_succeeded_after_one_attempt() {
        // This test asserts nothing about narration, but it drives `run_step`
        // and so touches the callsites the capturing test reads — see
        // `install_narration_capture`, whose whole point is that first touch
        // decides the callsite's fate for the entire process.
        install_narration_capture();
        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        let notify = || {};
        let sleep_fn = |_ms: u64| async move {};
        let result = run_step(
            &state,
            &notify,
            &cancel,
            ProvisionStep::Domain,
            ProvisionStep::Domain.default_retry_policy(),
            Some(SubstepKey::DomainVerifyingZone),
            |_attempt| async move { Ok(StepOutcome::Succeeded) },
            &sleep_fn,
        )
        .await
        .expect("run_step should succeed");
        assert!(matches!(result, StepOutcome::Succeeded));
        let snap = state.lock().unwrap().clone();
        assert_eq!(
            step(&snap, ProvisionStep::Domain).status,
            StepStatus::Succeeded
        );
        assert_eq!(step(&snap, ProvisionStep::Domain).attempt, 1);
        assert_eq!(snap.overall, OverallStatus::Running);
    }

    #[tokio::test]
    async fn run_step_marks_skipped_when_preflight_matches() {
        // Drives `run_step`; see `install_narration_capture`.
        install_narration_capture();
        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        let result = run_step(
            &state,
            &|| {},
            &cancel,
            ProvisionStep::Server,
            ProvisionStep::Server.default_retry_policy(),
            Some(SubstepKey::ServerCreating),
            |_| async move { Ok(StepOutcome::Skipped(SkipReason::ServerAlreadyExists)) },
            &|_ms: u64| async move {},
        )
        .await
        .expect("run_step should succeed");
        assert!(matches!(result, StepOutcome::Skipped(_)));
        let snap = state.lock().unwrap().clone();
        let st = step(&snap, ProvisionStep::Server);
        assert_eq!(st.status, StepStatus::Skipped);
        assert_eq!(st.skip_reason, Some(SkipReason::ServerAlreadyExists));
    }

    #[tokio::test]
    async fn run_step_retries_transient_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        // The one sibling that reaches the retry `warn!` callsite, and so the
        // one whose first touch used to cost the capturing test that line —
        // see `install_narration_capture`.
        install_narration_capture();
        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        let calls = AtomicU32::new(0);
        let result = run_step(
            &state,
            &|| {},
            &cancel,
            ProvisionStep::Dns,
            RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
            },
            Some(SubstepKey::DnsAddingDomainRecords),
            |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(ProvisionError::Provider {
                            status: 503,
                            body: String::new(),
                        })
                    } else {
                        Ok(StepOutcome::Succeeded)
                    }
                }
            },
            &|_ms: u64| async move {},
        )
        .await
        .expect("run_step should eventually succeed");
        assert!(matches!(result, StepOutcome::Succeeded));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let snap = state.lock().unwrap().clone();
        assert_eq!(
            step(&snap, ProvisionStep::Dns).status,
            StepStatus::Succeeded
        );
        assert_eq!(step(&snap, ProvisionStep::Dns).attempt, 3);
    }

    #[tokio::test]
    async fn run_step_terminal_error_fails_immediately() {
        // Drives `run_step`; see `install_narration_capture`.
        install_narration_capture();
        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        let result = run_step(
            &state,
            &|| {},
            &cancel,
            ProvisionStep::Domain,
            ProvisionStep::Domain.default_retry_policy(),
            None,
            |_| async move {
                Err(ProvisionError::Provider {
                    status: 400,
                    body: "bad".into(),
                })
            },
            &|_ms: u64| async move {},
        )
        .await;
        assert!(matches!(
            result,
            Err(ProvisionError::StepFailed {
                step: ProvisionStep::Domain,
                attempts: 1,
                ..
            })
        ));
        let snap = state.lock().unwrap().clone();
        assert_eq!(
            step(&snap, ProvisionStep::Domain).status,
            StepStatus::Failed
        );
        assert_eq!(snap.overall, OverallStatus::Failed);
    }

    #[tokio::test]
    async fn run_step_observes_cancel_at_iteration_boundary() {
        // Drives `run_step`; see `install_narration_capture`.
        install_narration_capture();
        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        cancel.raise();
        let result = run_step(
            &state,
            &|| {},
            &cancel,
            ProvisionStep::Online,
            ProvisionStep::Online.default_retry_policy(),
            None,
            |_| async move { Ok(StepOutcome::Succeeded) },
            &|_ms: u64| async move {},
        )
        .await;
        assert!(matches!(result, Err(ProvisionError::Cancelled)));
    }

    /// Every narration line this module captures, tagged with the thread that
    /// emitted it. Written by [`CaptureLayer`] under the one process-global
    /// subscriber [`install_narration_capture`] installs; read back per-thread
    /// by [`captured_lines_for_this_thread`].
    static CAPTURED: Mutex<Vec<(std::thread::ThreadId, String)>> = Mutex::new(Vec::new());

    /// Minimal capturing `tracing::Layer` — copies each event's `{message}`
    /// field, level, and target into [`CAPTURED`], tagged with the emitting
    /// thread.
    struct CaptureLayer;

    /// Install the narration capture for the whole test binary, once.
    ///
    /// ⚠ This is deliberately a **process-global** subscriber, not the
    /// thread-local `tracing::subscriber::set_default` that stood here before,
    /// and the difference is load-bearing rather than stylistic. `tracing`
    /// caches each callsite's interest process-globally on first use, so
    /// whichever thread reaches a callsite first decides whether that callsite
    /// is enabled for *everyone*. Six tests in this binary drive `run_step`,
    /// and five of them install no subscriber; libtest sorts by name, so the
    /// capturing test wins every callsite when the tests run sequentially — and
    /// loses them whenever a subscriber-less sibling gets there first, which
    /// only concurrency allows. What decides it is contention for a core, not
    /// the thread count: the failure survived re-runs at `--test-threads`
    /// 2/8/16/32 on an idle box and was written off as not reproducible, yet
    /// reddened `workspace-test-check` on a loaded one.
    ///
    /// Measured with convention 14's contention instrument
    /// (`taskset -c 0 <bin> --test-threads=8 run_step`, 300 runs each): the
    /// unfiltered thread-local layer failed 2/300, losing only the `warn!` line;
    /// adding an `EnvFilter` to it failed 4/300, losing *every* line — the
    /// gate's own empty-capture signature. A thread-local subscriber cannot fix
    /// a process-global cache; installing one subscriber for the binary can —
    /// this shape is 0/600 under that same contention.
    ///
    /// The filter is `EnvFilter::new("info")` — the ring's real default, the
    /// same shape the sibling captures use (`fauna_client_mls_sync::test_tracing`,
    /// `fauna_client_conversations::test_tracing`,
    /// `fauna_onboarding_machine::machine`) — so a line captured here is a line
    /// that reaches the ring in production.
    fn install_narration_capture() {
        use tracing_subscriber::prelude::*;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let subscriber = tracing_subscriber::registry()
                .with(tracing_subscriber::EnvFilter::new("info"))
                .with(CaptureLayer);
            // Nothing else in this binary sets a global default; if that ever
            // changes, the capturing test fails with an empty capture rather
            // than silently asserting against someone else's subscriber.
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// The lines emitted by the calling thread. Sibling tests share the global
    /// subscriber and emit the very same callsites, so filtering by thread is
    /// what keeps one test's narration out of another's assertions — an
    /// existence assert over a shared buffer would otherwise pass on a
    /// sibling's line even if `run_step` itself had gone silent.
    fn captured_lines_for_this_thread() -> Vec<String> {
        let me = std::thread::current().id();
        CAPTURED
            .lock()
            .unwrap()
            .iter()
            .filter(|(thread, _)| *thread == me)
            .map(|(_, line)| line.clone())
            .collect()
    }

    #[derive(Default)]
    struct MessageOnly(String);

    impl tracing::field::Visit for MessageOnly {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = MessageOnly::default();
            event.record(&mut visitor);
            let line = format!(
                "[{}][{}] {}",
                event.metadata().level(),
                event.metadata().target(),
                visitor.0
            );
            CAPTURED
                .lock()
                .unwrap()
                .push((std::thread::current().id(), line));
        }
    }

    /// Regression guard for the live-run freeze this instrumentation exists to
    /// diagnose: a 27-minute captured app log for a real `Online`-step run
    /// contained exactly one line total, because `run_step` — the retry loop
    /// driving every provisioning step — emitted nothing. Drives a transient
    /// failure then a success through two attempts and asserts the narration a
    /// human reading a stalled live log needs: an attempt-1 start (so "attempt
    /// N/M" is visible even before it stalls), the transient-retry warning, the
    /// attempt-2 start, and the success line.
    #[tokio::test]
    async fn run_step_narrates_every_attempt_so_a_stalled_dial_is_visible() {
        use std::sync::atomic::{AtomicU32, Ordering};

        install_narration_capture();

        let state = Mutex::new(ProvisioningSnapshot::idle());
        let cancel = CancelFlag::new();
        let calls = AtomicU32::new(0);

        let result = run_step(
            &state,
            &|| {},
            &cancel,
            ProvisionStep::Dns,
            RetryPolicy {
                max_attempts: 2,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
            },
            Some(SubstepKey::DnsAddingDomainRecords),
            |_attempt| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n == 1 {
                        Err(ProvisionError::Provider {
                            status: 503,
                            body: String::new(),
                        })
                    } else {
                        Ok(StepOutcome::Succeeded)
                    }
                }
            },
            &|_ms: u64| async move {},
        )
        .await;
        assert!(result.is_ok(), "expected eventual success, got {result:?}");

        let lines = captured_lines_for_this_thread();
        let joined = lines.join("\n");

        assert!(
            lines.iter().any(|l| l.contains("[INFO]")
                && l.contains("Dns")
                && l.contains("attempt 1/2")
                && l.contains("starting")),
            "expected an attempt-1/2 start line, got:\n{joined}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[WARN]") && l.contains("attempt 1/2") && l.contains("retry")),
            "expected a transient-retry warning naming the attempt, got:\n{joined}"
        );
        assert!(
            lines.iter().any(|l| l.contains("[INFO]")
                && l.contains("attempt 2/2")
                && l.contains("starting")),
            "expected an attempt-2/2 start line, got:\n{joined}"
        );
        assert!(
            lines.iter().any(|l| l.contains("[INFO]")
                && l.contains("attempt 2")
                && l.contains("Succeeded")),
            "expected a success line naming the outcome, got:\n{joined}"
        );
    }

    #[test]
    fn status_glyph_is_canonical_across_clients() {
        // The unified glyph set — resolves the prior linux/apple `… — ✗` vs
        // windows/web `⟳ − ✕` drift to the linux/apple set (Pending/Succeeded
        // already agreed). Plain `String`, locale-invariant (nothing to translate).
        assert_eq!(status_glyph(StepStatus::Pending), "○");
        assert_eq!(status_glyph(StepStatus::Running), "…");
        assert_eq!(status_glyph(StepStatus::Skipped), "—");
        assert_eq!(status_glyph(StepStatus::Succeeded), "✓");
        assert_eq!(status_glyph(StepStatus::Failed), "✗");
    }

    #[test]
    fn step_label_maps_to_canonical_provision_keys() {
        // Canonical key family is `onboarding.provision.step.*` (not the
        // identical-valued duplicate `onboarding.nest_provisioning.step.*`). No args.
        for (kind, key) in [
            (ProvisionStep::Domain, "onboarding.provision.step.domain"),
            (ProvisionStep::Server, "onboarding.provision.step.server"),
            (ProvisionStep::Dns, "onboarding.provision.step.dns"),
            (ProvisionStep::Online, "onboarding.provision.step.online"),
        ] {
            let lt = step_label(kind);
            assert_eq!(lt.key, key);
            assert!(lt.args.is_empty(), "{kind:?} should carry no args");
        }
    }

    #[test]
    fn substep_label_maps_every_key() {
        // The 12 keys with no `{cause}` placeholder → bare key, empty args.
        // `StatusRetrying` is covered separately (it carries `{cause}`).
        for (k, key) in [
            (
                SubstepKey::DomainCheckingAvailability,
                "onboarding.provision.substep.domain_checking_availability",
            ),
            (
                SubstepKey::DomainRegistering,
                "onboarding.provision.substep.domain_registering",
            ),
            (
                SubstepKey::DomainVerifyingZone,
                "onboarding.provision.substep.domain_verifying_zone",
            ),
            (
                SubstepKey::ServerGeneratingDkim,
                "onboarding.provision.substep.server_generating_dkim",
            ),
            (
                SubstepKey::ServerCreating,
                "onboarding.provision.substep.server_creating",
            ),
            (
                SubstepKey::DnsAddingDomainRecords,
                "onboarding.provision.substep.dns_adding_domain_records",
            ),
            (
                SubstepKey::DnsAddingEmailRecords,
                "onboarding.provision.substep.dns_adding_email_records",
            ),
            (
                SubstepKey::DnsSettingReverseDns,
                "onboarding.provision.substep.dns_setting_reverse_dns",
            ),
            (
                SubstepKey::OnlineWaiting,
                "onboarding.provision.substep.online_waiting",
            ),
            (
                SubstepKey::StatusSkipped,
                "onboarding.provision.substep.status_skipped",
            ),
            (
                SubstepKey::StatusCancelling,
                "onboarding.provision.substep.status_cancelling",
            ),
            (
                SubstepKey::StatusCancelled,
                "onboarding.provision.substep.status_cancelled",
            ),
        ] {
            let lt = substep_label(k, None);
            assert_eq!(lt.key, key);
            assert!(lt.args.is_empty(), "{k:?} should carry no args");
        }
    }

    #[test]
    fn substep_label_retrying_carries_cause() {
        // `status_retrying: 'Retrying after error: {cause}'` — the cause arg is
        // filled from the step's `last_error` so every app substitutes it
        // (web's behavior; fixes the linux/apple/windows literal-`{cause}` bug).
        let lt = substep_label(SubstepKey::StatusRetrying, Some("boom".to_string()));
        assert_eq!(lt.key, "onboarding.provision.substep.status_retrying");
        assert_eq!(lt.args.get("cause").map(String::as_str), Some("boom"));

        // No cause → the placeholder resolves to empty (matches android's `""`).
        let lt = substep_label(SubstepKey::StatusRetrying, None);
        assert_eq!(lt.args.get("cause").map(String::as_str), Some(""));
    }

    /// The **second** sink for the same string, and the one the finding filed
    /// as un-assessed rather than cleared: `set_retrying` / `set_failed` copy
    /// `err.to_string()` into the snapshot the UI reads, independently of the
    /// `warn!`. Both render `ProvisionError`'s `Display`, so redacting at the
    /// error boundary closes both at once — pinned here rather than asserted in
    /// prose, because "the other sink renders the same type" is exactly the
    /// kind of reasoning that stops being true when someone adds a field.
    #[tokio::test]
    async fn the_progress_snapshot_carries_no_credential_from_a_transport_failure() {
        const FAKE_KEY: &str = "nckey-SUPERSECRET-do-not-log-me";

        let raw = reqwest::Client::new()
            .get("http://127.0.0.1:1/xml.response")
            .query(&[("ApiKey", FAKE_KEY)])
            .send()
            .await
            .expect_err("a closed port cannot answer");
        let err: ProvisionError = raw.into();

        let state = Mutex::new(ProvisioningSnapshot::idle());
        set_retrying(&state, ProvisionStep::Domain, 2, &err);
        set_failed(&state, ProvisionStep::Domain, 3, &err);

        let rendered = format!("{:?}", state.lock().unwrap());
        assert!(
            !rendered.contains(FAKE_KEY),
            "the API key must not reach the UI snapshot: {rendered}"
        );
        assert!(
            rendered.contains("HTTP request failed"),
            "the failure is still reported, just without the credential: {rendered}"
        );
    }
}
