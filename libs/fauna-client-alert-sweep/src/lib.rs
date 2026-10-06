//! The session-start critical-alert sweep — the one call every app makes from
//! its universal post-auth hook.
//!
//! # Why this crate exists
//!
//! `critical-alerts.md` § Goal: the product's pages are set-and-forget by
//! design, so a condition detected only *while its own page is open* is
//! structurally unseeable. The banner solved half of that — an alert, once
//! posted, shows on every page. This crate solves the other half: **something
//! has to run the detector for a user who never opens the page.**
//!
//! Feeder #1 (ATProto genesis-seniority custody) also runs from the settings
//! machine's status convergence, which covers the TOFU-critical mint instant —
//! but only for a user who opens that page, so it runs from here too. Its
//! two-consecutive-convergence debounce is **re-expressed, not copied**, for a
//! one-shot sweep; the reasoning is on [`run_genesis_custody_feeder`].
//! Feeder #2 (the pending-RecoveryKey-replacement window) had, until this
//! crate, **no caller at all**: its projection was built and tested and no app
//! ever polled it, so a 30-day window authorized by the identity seed alone was
//! loud precisely nowhere — the failure mode `critical-alerts.md`:38 and
//! `identity-succession.md`:20 both recorded as the open gap.
//!
//! # Why one shared entry point rather than a per-app poll
//!
//! Priority #1 (minimize per-app divergence) and #2 (maximize shared Rust): a
//! feeder joining the sweep must not mean seven app changes. Apps call
//! [`run_session_start_sweep`] exactly once and never change again — which
//! feeders exist, in what order they run, how a failure is handled, and what
//! gets logged are all decided here, once, for all seven.
//!
//! The *trigger* is deliberately **not** invented here: every app already has a
//! universal post-auth hook hosting best-effort session-start convergences of
//! exactly this shape (tui `session::establish` — `spawn_refresh_mail_epoch_schedule`,
//! the deployment-seed custody leg; linux's own equivalents). The sweep is one more call
//! there, which is what makes it uniform rather than seven bespoke designs.
//!
//! # Contract
//!
//! * **Best-effort, never fatal.** A sweep runs for its side effects on the
//!   registry; the caller logs the report and moves on. Sign-in must not fail
//!   because a feeder's plane was unreachable.
//! * **Every feeder runs, independently.** One feeder's transport failure never
//!   skips a later feeder — a nest that cannot answer the recovery plane must
//!   not be able to silence a different feeder's alarm.
//! * **A failure never clears.** Each feeder's own `sync_*` call owns that rule
//!   (unreachable is not resolved); the sweep preserves it by never
//!   substituting a default outcome for a failed read.
//! * **Identity-scoped.** Alert keys carry the actor, and the caller sweeps with
//!   the actor whose session just came up — an account switch clears the
//!   outgoing account's alerts (`CriticalAlerts::clear_all`, per
//!   `critical-alerts.md` § Mechanism → *Lifetime*) and then sweeps afresh.

use std::sync::Arc;

pub mod domain_expiry;
pub use domain_expiry::{
    DOMAIN_EXPIRY_ALERT_KEY, domain_expiry_alert_lines, sync_domain_expiry_alert,
};

use fauna_client_alerts::CriticalAlerts;
use fauna_client_atproto::identity_store::AtprotoIdentityStore;
use fauna_core::identity::ActorId;
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// Which feeder an outcome belongs to. A stable `&'static str` rather than an
/// enum variant name so a log line and a test assertion read the same word.
pub mod feeders {
    /// The pending-RecoveryKey-replacement window
    /// (`identity-succession.md` § The RecoveryKey → *Replacement*).
    pub const PENDING_REPLACEMENT: &str = "pending-replacement";
    /// The published ATProto handle binding
    /// (`atproto-pds-bridge.md` § State & data shape).
    pub const HANDLE_BINDING: &str = "atproto-handle-binding";
    /// The genesis-seniority custody check
    /// (`atproto-pds-bridge.md` § State & data shape).
    pub const GENESIS_CUSTODY: &str = "atproto-genesis-custody";
    /// The deployment's primary-domain registration
    /// (`domains-and-tls-bootstrap.md` § Domain loss → *Detection*).
    pub const DOMAIN_EXPIRY: &str = "domain-expiry";
}

/// One feeder that could not be checked this sweep.
///
/// The error is flattened to a string on purpose: a caller's only honest action
/// is to log it and let the next sweep retry, and keeping the feeders' distinct
/// error types would put a type parameter on the report for no gained decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeederFailure {
    /// One of [`feeders`].
    pub feeder: &'static str,
    /// The feeder's own error, rendered.
    pub error: String,
}

/// Why a feeder had nothing to do this sweep. Stable tokens, not prose: a log
/// line, a test assertion and a support read should all say the same word.
pub mod skip_reasons {
    /// The nest reported no ATProto identity at all.
    pub const NO_IDENTITY: &str = "nest-reports-no-identity";
    /// The identity is not a `did:plc` one (a `did:web` has no PLC log).
    pub const NOT_PLC: &str = "identity-is-not-did-plc";
    /// A mint is pending — an identity with no DID yet.
    pub const NO_DID: &str = "identity-has-no-did-yet";
    /// This client holds no rotation keys, so there is nothing to compare a
    /// published one against.
    pub const EMPTY_RING: &str = "no-held-rotation-keys";
    /// The nest reported no handle domain to audit the binding against.
    pub const NO_HANDLE_DOMAIN: &str = "nest-reports-no-handle-domain";
    /// The deployment has no primary domain (a domainless box) — same
    /// re-export reasoning as [`RDAP_UNSERVED_TLD`], the nest's other
    /// domain-expiry skip token this module's callers match against.
    pub use fauna_protocol::domain_expiry::skip_reasons::NO_PRIMARY_DOMAIN;
    /// The deployment's TLD is not served by the RDAP bootstrap (many ccTLDs).
    /// A stable fact, not a transient one — absence of data must not alarm.
    /// The nest emits this exact token (`bins/fauna-nest/src/domain_expiry.rs`)
    /// straight from `fauna_protocol::domain_expiry::skip_reasons::UNSERVED_TLD`
    /// — re-exported under this module's own name rather than a second literal,
    /// so a wire-format change can't silently desync the two.
    pub use fauna_protocol::domain_expiry::skip_reasons::UNSERVED_TLD as RDAP_UNSERVED_TLD;
    /// The nest's domain-expiry watch has not completed a single attempt yet —
    /// a box in its first minutes.
    pub const WATCH_NOT_YET_RUN: &str = "domain-watch-not-yet-run";
    /// The nest recorded a skip whose reason token this build does not know —
    /// a newer nest. Kept as a distinct token rather than dropped: a skip must
    /// never become a silence indistinguishable from health
    /// (`critical-alerts.md` § Feeders).
    pub const UNKNOWN_NEST_SKIP: &str = "nest-reports-unknown-skip";
}

/// One feeder that had nothing to check — the third outcome, beside "checked"
/// and "could not reach".
///
/// It exists because the first two **cannot express "not applicable"**, and that
/// gap was a security finding (2026-08-02): a nest answering `identity: null`
/// made both directory feeders return silently while the report still said
/// every feeder was reached, so a box could decline to be audited and the logs
/// could not tell that from health. `critical-alerts.md` § Feeders states the
/// three-outcome rule this landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeederSkip {
    /// One of [`feeders`].
    pub feeder: &'static str,
    /// One of [`skip_reasons`].
    pub reason: &'static str,
}

/// What one sweep did — for logging and for tests, never for control flow in
/// the app (the sweep's real output is the registry's contents).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Feeders whose check completed (posted *or* cleared).
    pub checked: Vec<&'static str>,
    /// Feeders that could not be reached; their previously-posted alerts (if
    /// any) deliberately still stand.
    pub failures: Vec<FeederFailure>,
    /// Feeders that had nothing to check, each with its [`skip_reasons`] token.
    /// A skip is legitimate (a user with no ATProto identity) *or* the visible
    /// shadow of a nest declining to be audited — the report does not judge
    /// which, it only refuses to stay silent.
    pub skipped: Vec<FeederSkip>,
    /// The custody door refused the held-ring read, so feeder #1 did not run
    /// (it is also in `failures`). The one fact of the report the loop acts
    /// on: its next wait also ends at the door's readiness edge
    /// ([`run_alert_sweep_loop_with`]).
    pub ring_unread: bool,
}

impl SweepReport {
    // ── There is deliberately NO `is_complete()` ────────────────────────────────────────────────────────
    //
    // It existed to say "every feeder actually ran", and a later fix made it honest by folding `skipped` into it. But it never
    // acquired a production consumer: all three app-facing callers branch on
    // `failures.is_empty()` and each carries a comment explaining why it must
    // NOT use completeness —
    // because a healthy deployment with no ATProto integration is legitimately
    // *incomplete*, and warning on that every session is the crying-wolf
    // failure `critical-alerts.md` § Severity bar exists to prevent.
    //
    // The question it left open — *should a skipped feeder #1 on an opted-in
    // account be user-visible?* — is answered **no**, and the audit floor is
    // what earns that answer. The one skip that was security-relevant (a box
    // answering `identity: null` to decline being audited) is no longer a skip
    // at all: `nest_named_dids` floors it into a real audit that either passes
    // or alarms. What remains skippable is transient or by-design — an unsynced
    // ring, a did:web identity, a mint still in flight — none of which clears
    // the severity bar, and each of which would accuse an innocent deployment.
    //
    // `skipped` itself stays and is the ratified requirement (`critical-alerts.md`
    // § Feeders: "silence is what makes the gate exploitable, and the skipped
    // bucket is the minimum"). Assert on it directly — it names *which* feeder
    // skipped and *why*, which a conflating boolean never could.

    /// Record that `feeder` had nothing to check.
    fn skip(&mut self, feeder: &'static str, reason: &'static str) {
        self.skipped.push(FeederSkip { feeder, reason });
    }

    /// Record that `feeder`'s check completed. Idempotent: feeder #1 can run
    /// over **several** DIDs in one sweep (the audit floor —
    /// [`plan_directory_audit`]), and "custody was checked" is one fact however
    /// many identities it covered.
    fn check(&mut self, feeder: &'static str) {
        if !self.checked.contains(&feeder) {
            self.checked.push(feeder);
        }
    }
}

/// How long a live session waits between re-sweeps
/// ([`run_alert_sweep_loop`]) — 6 hours.
///
/// Sized against **PLC's 72-hour contest window**, which is the deadline that
/// actually binds: feeders #1 and #3 read the public PLC directory, whose
/// changes the nest never observes and therefore can never announce, so the
/// only thing that can notice a hostile rotation mid-session is this clock. Six
/// hours puts detection at most 6 h after the op and leaves the user ≥66 h to
/// contest it; 24 h would leave a first look as late as the deadline itself.
/// The other direction is bounded by politeness to a public directory, and 4
/// fetches/day/session is nothing — the AT Protocol settings page already fetches
/// once per status convergence.
///
/// Feeder #2's 30-day replacement window is covered many times over by the same
/// clock; it does **not** get its own faster trigger (see
/// [`run_alert_sweep_loop`]).
pub const RE_SWEEP_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// The cadence above is a decision, not an accident: it must leave real room
/// inside PLC's 72 h contest window after the worst-case detection delay.
/// Compile-time, so widening the interval past that is a build failure someone
/// has to answer rather than a silently weakened guarantee.
const _: () = {
    const PLC_CONTEST_WINDOW_SECS: u64 = 72 * 60 * 60;
    assert!(
        RE_SWEEP_INTERVAL_SECS * 4 <= PLC_CONTEST_WINDOW_SECS,
        "a hostile PLC op must be detectable with most of the 72 h contest \
         window still left to contest it"
    );
};

/// Run every session-start feeder against `rpc` for `actor_id`, reading the
/// clock for the time-dependent copy.
///
/// One shot. App shells call [`run_alert_sweep_loop`] instead, which opens with
/// exactly this sweep and then repeats it for as long as the session lives. See
/// [`run_session_start_sweep_at`] for the clock-injected form the tests drive.
pub async fn run_session_start_sweep<R>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
) -> SweepReport
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    run_session_start_sweep_at(
        rpc,
        custody,
        alerts,
        actor_id,
        fauna_core::data::Timestamp::now_secs(),
    )
    .await
}

/// [`run_session_start_sweep`] with the time source injected.
///
/// `now` is unix seconds; it reaches the alert copy (the replacement window's
/// whole-days countdown), so a test can assert the rendered countdown without a
/// real clock — the same discipline `fauna_client_recovery::alerts` keeps.
///
/// `rpc` is taken by value and cloned per feeder: each feeder wraps the
/// transport in its own typed client (`RecoveryClient`, and the bluesky
/// verifier when it joins), and every real transport is already an `Arc`
/// (`impl RpcRequester for Arc<T>`), so the clone is a refcount bump.
pub async fn run_session_start_sweep_at<R>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
    now: i64,
) -> SweepReport
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let mut report = SweepReport::default();

    // This pass has begun — published before the first feeder reads anything,
    // and paired with the completion bump at the bottom of this function. The
    // pair is the sweep's causal barrier for negative asserts, and the ordering
    // *is* the mechanism: `CriticalAlerts::sweep_passes_started` states what a
    // reader may conclude from it and why one counter would not do.
    alerts.note_sweep_started();

    // Per-feeder progress. The sweep's only exit lines were the per-feeder
    // *outcomes*, so a pass that stalled INSIDE a feeder logged nothing at all
    // and was indistinguishable from one that never started. Entry
    // lines make the stall localisable from the log alone, on every app.
    tracing::debug!(stage = "sweep-begin", "session-start sweep: pass begins");

    // ── Feeder: pending RecoveryKey replacement ──────────────────────────────
    // `refresh_*` posts or clears from the freshly-read status and *returns* a
    // transport failure rather than clearing, which is the fail-safe direction
    // for this severity class: a nest that merely went offline must not be able
    // to silence a live compromise warning.
    let recovery = fauna_client_recovery::nest::RecoveryClient::new(rpc.clone());
    tracing::debug!(
        stage = "pending-replacement-begin",
        "session-start sweep: reading the pending-replacement window"
    );
    match fauna_client_recovery::alerts::refresh_pending_replacement_alert(
        &recovery, alerts, actor_id, now,
    )
    .await
    {
        Ok(pending) => {
            tracing::debug!(
                pending = pending.is_some(),
                "session-start sweep: pending-replacement checked"
            );
            report.checked.push(feeders::PENDING_REPLACEMENT);
        }
        Err(e) => {
            // Not an error the user sees: the next sweep retries, and any alert
            // this feeder posted earlier still stands in the meantime.
            tracing::warn!(
                error = %e,
                "session-start sweep: pending-replacement unreachable; \
                 any standing alert is left as-is and the next sweep retries"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::PENDING_REPLACEMENT,
                error: e.to_string(),
            });
        }
    }
    // The same feeder's second source: the windows the runtime's secondary
    // leg last read at each linked nest, posted beside the bound nest's
    // reading (`fauna_client_core::recovery_pending`). Each nest's reading is
    // its own, so the bound nest reading nothing never clears a window a
    // linked nest holds — a seed thief can open one at a nest no device is
    // bound to (`identity-succession.md` § Enforcement on the home nest →
    // *Every nest the identity is linked to*, clause (c)).
    fauna_client_recovery::alerts::sync_linked_readings(alerts, actor_id, now);

    // ── Feeders reading the DID's published PLC log ──────────────────────────
    // Genesis-seniority custody (#1) and the published handle binding (#3) ask
    // two different questions of the *same* log, so they share one nest read and
    // one directory read — and can never answer from two fetches of a log that
    // changed in between.
    tracing::debug!(
        stage = "directory-begin",
        "session-start sweep: directory feeders begin"
    );
    run_directory_feeders(rpc.clone(), custody, alerts, &mut report).await;
    tracing::debug!(
        stage = "directory-end",
        "session-start sweep: directory feeders returned"
    );

    // ── Feeder: the deployment's primary-domain registration ─────────────────
    // Deployment-scoped rather than identity-scoped — the first of its kind
    // (`domain_expiry`'s module note). Runs last only because it is newest;
    // nothing orders it against the others, and the every-feeder-runs-
    // independently contract means an unreachable directory above cannot skip
    // it.
    tracing::debug!(
        stage = "domain-expiry-begin",
        "session-start sweep: domain-expiry feeder begins"
    );
    domain_expiry::run_domain_expiry_feeder(rpc, alerts, &mut report, now).await;
    tracing::debug!(
        stage = "domain-expiry-end",
        "session-start sweep: domain-expiry feeder returned"
    );

    // Every feeder above has posted or cleared, so the registry now reflects
    // this pass's verdict — only *here* may the pass count itself finished. A
    // bump any earlier would let a waiting test read the banner before the
    // alarm it is asking about had landed, which is the exact false-pass the
    // settle-sleep this barrier replaces used to give.
    alerts.note_sweep_completed();

    tracing::debug!(
        stage = "sweep-end",
        checked = ?report.checked,
        failures = ?report.failures,
        skipped = ?report.skipped,
        "session-start sweep: pass complete"
    );

    report
}

/// Sweep now, then keep sweeping every [`RE_SWEEP_INTERVAL_SECS`] until the
/// identity goes away. **This is the call app shells make** — it opens with the
/// session-start sweep, so it replaces a bare `run_session_start_sweep` rather
/// than sitting beside one.
///
/// # A first pass that ran before the account runtime
///
/// Every app fires this at its post-auth hook, which is before the seat's
/// account runtime assembles (sign-in waits for neither), and feeder #1 reads
/// the rotation keyring through that runtime — so the first pass of a fresh
/// sign-in commonly finds the custody door refusing
/// ([`SweepReport::ring_unread`]). The wait after such a pass also ends at
/// [`AtprotoIdentityStore::until_readable`], the moment the runtime is there and its first catch-up pass has settled,
/// and the pass re-runs then instead of leaving feeder #1 unrun until the next
/// [`RE_SWEEP_INTERVAL_SECS`]. **Once per streak**: a pass that fails the ring
/// read again waits out the ordinary clock — a door that is readable yet
/// failing (a store fault) must not spin the loop — and the next pass that
/// reads the ring re-arms the edge. Living here, it needs no per-app wiring:
/// the edge is the seam's own, over the handle source every app already
/// passes.
///
/// # Why a clock rather than an event (ratified 2026-08-02)
///
/// A sweep at session establishment only is half the set-and-forget story
/// `critical-alerts.md` § Goal tells: a desktop app left open for a week checks
/// once, at the start of that week. Feeders #1 and #3 read the **public PLC
/// directory**, which the nest never observes, so no server-pushed event for
/// them can exist in principle — a clock is not the cheap option here, it is the
/// only one.
///
/// Feeder #2 *does* have a server-side event (the nest's `SecurityNotifier`
/// posts a `SecurityNotice` to the durable inbox when a replacement window
/// opens), and driving a re-check off the shared inbox drain's
/// `apply_security_notice` hook was considered and **deliberately rejected**.
/// Two reasons, worth keeping so it is not rebuilt as if load-bearing:
///
/// 1. It buys no coverage. This clock re-checks feeder #2 ~120 times inside its
///    30-day window; shaving hours off that is not what the window needs.
/// 2. `SecurityNoticeInbox` carries only prose (`subject`/`body`) — there is no
///    machine-readable event kind. A client would therefore either re-sweep on
///    *every* notice, or ask the nest which of its own notices deserve a
///    re-check. The second hands the audited party a mute button, which is
///    exactly the trust inversion the review names: the accused box must
///    not decide whether it is checked.
///
/// (A third reason — that returning `Ok` from the hook would consume a notice
/// no app renders — retired 2026-08-10: the nest now writes the rendered
/// `notifications` row and both client faces ack the redundant inbox copy,
/// `notifications.md` § Security notices. The rejection stands on 1 + 2.)
///
/// `identity-succession.md` § The RecoveryKey → *Replacement* keeps the two
/// halves separate for the same reason: the `SecurityNotifier` alarm is the
/// one-shot notification, the banner is the standing client-side read.
///
/// # Stopping
///
/// The loop watches [`CriticalAlerts::teardown_epoch`], which every app already
/// bumps by calling `clear_all` at sign-out / account switch / factory reset
/// (§ Mechanism → *Lifetime* requires that call). So it stops on the first wake
/// after the identity goes away with **no per-app liveness plumbing at all** —
/// and, because the check sits between the sleep and the sweep, a departed
/// identity is never swept for.
///
/// One residual, unchanged from the one-shot sweep it replaces: a teardown
/// landing *during* a sweep can still race an alert into the registry after
/// `clear_all` ran. The window is one sweep, and the next establish's own
/// `clear_all` closes it.
pub async fn run_alert_sweep_loop<R>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
) where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    run_alert_sweep_loop_with(rpc, custody, alerts, actor_id, || async {
        cross_platform_sleep_secs(RE_SWEEP_INTERVAL_SECS).await;
        true
    })
    .await;
}

/// [`run_alert_sweep_loop`] with the wait injected, returning how many sweeps
/// ran.
///
/// `wait` is awaited between sweeps and returns `false` to end the loop, which
/// is what lets the tests drive N iterations with **no wall clock at all**
/// (`testing.md` § point 14: assert latency-independent state, never timing).
/// Production passes a real sleep that always returns `true`.
pub async fn run_alert_sweep_loop_with<R, W, F>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
    mut wait: W,
) -> u32
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    W: FnMut() -> F,
    F: core::future::Future<Output = bool>,
{
    let epoch = alerts.teardown_epoch();
    let mut sweeps = 0u32;
    // Whether a refused ring read may still end the next wait at the door's
    // readiness edge — spent by one such wait, re-armed by a pass that read
    // the ring (the rustdoc above: *A first pass that ran before the account
    // runtime*).
    let mut readable_edge_armed = true;
    loop {
        let report = run_session_start_sweep(rpc.clone(), custody, alerts, actor_id).await;
        sweeps += 1;
        let await_readable = report.ring_unread && readable_edge_armed;
        readable_edge_armed = !report.ring_unread;
        // Failures and skips are logged at different levels on purpose. A
        // failure is a transport fault worth a warn; a skip is usually just a
        // deployment with no ATProto identity, and warning about that every
        // wake would train the reader to ignore the line. Both are always
        // *printed* — a support read must be able to tell "no alert because
        // nothing is wrong" from "no alert because nothing ever ran".
        if !report.failures.is_empty() {
            // A standing alert (if any) is left alone and the next wake retries.
            tracing::warn!(
                sweep = sweeps,
                checked = ?report.checked,
                failures = ?report.failures,
                skipped = ?report.skipped,
                "critical-alert sweep: some feeders were unreachable"
            );
        } else {
            tracing::debug!(
                sweep = sweeps,
                checked = ?report.checked,
                skipped = ?report.skipped,
                "critical-alert sweep: every reachable feeder ran"
            );
        }
        let go_on = if await_readable {
            tracing::debug!(
                sweep = sweeps,
                "critical-alert sweep: the rotation keyring was unreadable; the \
                 next pass also runs once the account runtime is readable"
            );
            readable_or_wait(custody.until_readable(), wait()).await
        } else {
            wait().await
        };
        if !go_on {
            return sweeps;
        }
        if alerts.teardown_epoch() != epoch {
            tracing::debug!(
                sweeps,
                "critical-alert sweep loop: identity torn down, stopping"
            );
            return sweeps;
        }
    }
}

/// The loop's wait after a refused ring read: `true` (sweep again) the moment
/// the custody door is `readable`, else whatever `wait` answers. `readable` is
/// polled first, so an edge already there never drives the clock at all.
async fn readable_or_wait<A, B>(readable: A, wait: B) -> bool
where
    A: core::future::Future<Output = ()>,
    B: core::future::Future<Output = bool>,
{
    let mut readable = core::pin::pin!(readable);
    let mut wait = core::pin::pin!(wait);
    core::future::poll_fn(|cx| {
        if readable.as_mut().poll(cx).is_ready() {
            return core::task::Poll::Ready(true);
        }
        wait.as_mut().poll(cx)
    })
    .await
}

/// [`run_alert_sweep_loop`] whose wait between passes also ends when the future
/// `wake` hands back resolves — the e2e seam that lets a test drive the loop's
/// OWN re-sweep instead of waiting out [`RE_SWEEP_INTERVAL_SECS`]
/// (convention 14: never wait on a clock a test can poke).
///
/// Everything but the wait is the production loop — the same
/// [`run_alert_sweep_loop_with`] body and the same teardown-epoch stop, with
/// the production clock still running underneath: a wake ends one wait early,
/// nothing more. That is what makes a witness of "a condition that arises while
/// the app is open is announced without a restart" (`critical-alerts.md`
/// § Mechanism → *How often the detector runs*) a witness of THIS loop, where
/// re-establishing the session would only prove the one-shot sweep a converge
/// runs.
///
/// `wake` is called once per wait, so each call hands back a fresh future (a
/// tokio `Notify::notified()`, a JS promise, …); the race itself is
/// runtime-free, so this crate still needs no runtime of its own. An app should
/// mint a fresh wake source per identity, so a departed identity's loop — which
/// lingers until its next wake reads the teardown — is never the one woken.
///
/// Compiled out of release artifacts (convention 15 rule (a)): debug builds
/// reach it through `debug_assertions`, and a release-profile e2e build or a
/// `wasm-pack` consumer (always `--release`) through `test-helpers`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub async fn run_alert_sweep_loop_wakeable<R, W, F>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    actor_id: &ActorId,
    mut wake: W,
) -> u32
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    W: FnMut() -> F,
    F: core::future::Future<Output = ()>,
{
    run_alert_sweep_loop_with(rpc, custody, alerts, actor_id, || {
        let woken = wake();
        async move {
            first_of(woken, cross_platform_sleep_secs(RE_SWEEP_INTERVAL_SECS)).await;
            true
        }
    })
    .await
}

/// Resolve as soon as either future does. `first` is polled first, so a future
/// that is already ready ends the race without the other ever being polled —
/// which is why the wakeable loop passes the wake first: the clock's sleep then
/// never has to be driven at all on a woken wait.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
async fn first_of<A, B>(first: A, second: B)
where
    A: core::future::Future<Output = ()>,
    B: core::future::Future<Output = ()>,
{
    let mut first = core::pin::pin!(first);
    let mut second = core::pin::pin!(second);
    core::future::poll_fn(|cx| {
        if first.as_mut().poll(cx).is_ready() || second.as_mut().poll(cx).is_ready() {
            core::task::Poll::Ready(())
        } else {
            core::task::Poll::Pending
        }
    })
    .await
}

/// Seconds-shaped call-site adapter over the shared cross-target sleep. The
/// `#[cfg]` split this used to spell out — and which its own doc flagged as a
/// shared-Rust lift somebody should do — now lives once, in `fauna-sleep`.
async fn cross_platform_sleep_secs(secs: u64) {
    fauna_sleep::sleep(std::time::Duration::from_secs(secs)).await;
}

/// Test-only passthrough to [`fauna_client_atproto::genesis_verify`]'s wasm
/// directory override, for the wasm chunk that hosts THIS crate's
/// [`run_session_start_sweep`] — a separately-compiled wasm binary from the
/// chunk `fauna-wasm-atproto-settings` hosts, with its own copy of that
/// crate's `thread_local` override (the same module-boundary shape
/// `critical-alerts.md` § Mechanism already documents for the alert
/// registry itself). Without this, `enable_fake_plc_directory_for_test`
/// called only in the atproto-settings chunk leaves this chunk's copy of
/// `plc_directory_base_url()` pointed at the real `https://plc.directory`,
/// so a sweep-driven feeder (#1 or #3) run from THIS chunk 404s against the
/// real directory and reports "unreadable, inconclusive" instead of reading
/// the test's `FakePlcDirectory` — found via
/// `test_alert_sweep_directory_feeders_e2e.py --app web`.
///
/// **Compiled out of release artifacts** (convention 15 rule (a),
/// `e2e-automation-surface-gating.md` § The convention): `target_arch =
/// "wasm32"` alone (the only gate this carried before) restricts to wasm,
/// not to a test-capable build. `debug_assertions` alone is not enough
/// either — `wasm-pack build` is `--release` internally regardless of
/// dev/prod flavor (rule (b)), so `fauna-wasm`'s `test-helpers` caller can
/// only reach this through the feature; this crate's own `test-helpers`
/// forwards to `fauna-client-atproto/e2e-agent`, the inner fn's own
/// gate.
#[cfg(all(
    target_arch = "wasm32",
    any(debug_assertions, feature = "test-helpers")
))]
pub fn enable_fake_plc_directory_for_test(url: String) {
    fauna_client_atproto::genesis_verify::enable_fake_plc_directory_for_test(url);
}

/// Feeder #3 — is the identity this client protects still published under a
/// handle at the user's own deployment?
///
/// Full reasoning (including what this deliberately does **not** claim to
/// catch) is in [`fauna_client_atproto::handle_binding`]. The parts that belong
/// here, because they are the sweep's decisions rather than the check's:
///
/// * **Nothing to check is silent, never an alarm.** No hosted identity, no DID
///   yet (a pending mint), a did:web identity (its custody *is* domain custody —
///   there is no directory log to audit and the comparison would be
///   tautological), or a nest that reports no handle domain: the feeder does not
///   run, and is not counted as a failure. Only a *definite* contradiction in a
///   log we could actually read raises the banner.
/// * **The nest read and the directory read fail differently.** An unreachable
///   nest means we never learned which DID to audit — a failure. An unreachable
///   *directory* is the check's own quiet-retry class, and is likewise recorded
///   as a failure so the report stays honest about what was not verified. In
///   both cases any standing alert deliberately stands: unreachable is not
///   resolved.
async fn run_directory_feeders<R>(
    rpc: R,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    report: &mut SweepReport,
) where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let atproto = fauna_client_bridges::AtprotoSettingsClient::new(rpc);
    tracing::debug!(
        stage = "directory-status-begin",
        "session-start sweep: reading atproto integration status"
    );
    let status = match atproto.get_integration_status().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "session-start sweep: atproto integration status unreachable; \
                 neither directory check ran and any standing alert is left as-is"
            );
            // Both feeders needed this one read to learn *which* DID to audit,
            // so both are honestly unverified — not silently passed over.
            //
            // The audit floor does not extend here on purpose: an unreachable
            // nest is the *control* the finding leaned on, and it is
            // already right — a failure means retry, clears nothing, and leaves
            // every standing alert up, so it cannot compose with the settings
            // machine's departed-DID clear the way a nest that *answers, with
            // nothing* could. Auditing ring DIDs from this arm would put one
            // feeder in two buckets for no reachable gain.
            for feeder in [feeders::GENESIS_CUSTODY, feeders::HANDLE_BINDING] {
                report.failures.push(FeederFailure {
                    feeder,
                    error: e.to_string(),
                });
            }
            return;
        }
    };

    // Freeze what the nest just claimed, BEFORE the ring is read, so this very
    // sweep's floor already carries it. A box that mints under
    // a hostile key and then answers `identity: null` on the next sweep meets a
    // floor it can no longer shrink.
    //
    // This writes `nest_named_dids` only — never `rotation_keys` — so the
    // log-before-ring ordering that replaced feeder #1's debounce is untouched:
    // the ring that DECIDES a verdict is re-read after each log below, and its
    // senior-key half cannot be moved by this call.
    if let Some(did) = status
        .identity
        .as_ref()
        .and_then(|id| id.did.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        && let Err(e) = {
            tracing::debug!(
                stage = "directory-freeze-begin",
                %did,
                "session-start sweep: freezing the nest-named DID into the audit floor"
            );
            fauna_client_atproto::rotation_key::record_nest_named_did(&**custody, did).await
        }
    {
        // Quiet: the next sweep re-records, and a floor that failed to grow is
        // the pre-fix behaviour rather than a new failure mode.
        tracing::warn!(%did, %e, "could not freeze the nest-named DID for the audit floor; will re-record");
    }

    // The gate read of the held ring. It is only the *gate* — the ring that
    // DECIDES feeder #1's verdict is re-read after each log below, which is the
    // whole of the debounce's replacement (see `run_genesis_custody_feeder`).
    // What it additionally supplies here is the audit **floor**: the DIDs this
    // client independently knows it protects.
    tracing::debug!(
        stage = "directory-gate-ring-begin",
        "session-start sweep: reading the held rotation keyring (gate + floor)"
    );
    let ring = match read_held_ring(custody).await {
        Ok(ring) => Some(ring),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "session-start sweep: could not read the rotation keyring; the custody \
                 check did not run and any standing alert is left as-is"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::GENESIS_CUSTODY,
                error: e,
            });
            report.ring_unread = true;
            // Feeder #3 does not need the ring, so it still runs off whatever
            // the nest named. `None` (not an empty ring) so the plan records no
            // *skip* for custody on top of the failure just pushed: a feeder
            // belongs in one bucket per sweep, and "unreachable" is the one
            // that means retry.
            None
        }
    };

    let plan = plan_directory_audit(&status, ring.as_ref(), report);
    let directory = fauna_client_atproto::genesis_verify::plc_directory_base_url();
    tracing::debug!(
        stage = "directory-plan",
        targets = plan.targets.len(),
        custody_runs = plan.custody_runs,
        binding_runs = plan.binding_runs,
        "session-start sweep: directory audit plan decided"
    );
    execute_audit_plan(&plan, custody, alerts, report, |did| {
        let directory = directory.clone();
        async move { fauna_client_atproto::genesis_verify::fetch_audit_log(&directory, &did).await }
    })
    .await;
}

/// One DID this sweep audits, and **where the sweep learned of it** — which is
/// the whole of the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditTarget {
    did: String,
    /// The name the alarm calls this identity by. The nest's reported handle
    /// when the nest named it; otherwise the DID itself — for a DID the nest
    /// declines to mention there is no handle to report, and the DID is the
    /// only truthful identifier the user can act on.
    handle: String,
    /// `true` when the nest named this DID as the account's **live** ATProto
    /// identity.
    ///
    /// It decides one thing: how a terminal retirement in the published log
    /// reads ([`SeniorityVerdict::is_terminal_retirement`]). A DID the nest
    /// claims is live must not be tombstoned — that contradiction is an alarm.
    /// A DID known only from the held ring carries no such claim: the ring
    /// remembers every DID a held key was ever published for, permanently (the
    /// burn is what stops a re-mint reusing the key), so retirement is the
    /// ordinary end of that entry's life. Without this distinction the floor
    /// would alarm forever on every identity the user ever retired.
    nest_claims_live: bool,
}

/// What one sweep will audit — decided from the nest's answer **and** the
/// client's own ring, before any directory is touched.
///
/// Pure by construction so the decision the finding is about ("who chooses
/// whether this box is examined?") is a value a test can assert on, rather than
/// a control-flow shape only a live directory could exercise.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AuditPlan {
    /// Every DID to fetch, nest-named first, then ring-derived in the ring's
    /// own sorted order.
    targets: Vec<AuditTarget>,
    /// Whether feeder #1 has anything to compare a published log against.
    custody_runs: bool,
    /// Whether feeder #3 has a comparison domain. It audits **only** the
    /// nest-named target: its comparison basis (`handle_domain`) is itself nest
    /// testimony, so a floor over it would not be independent of the party it
    /// audits — and a retired DID publishes no handle, so the floor would turn
    /// every past retirement into an unbound-handle alarm.
    binding_runs: bool,
    /// `handle_domain`, trimmed. Empty iff `binding_runs` is false.
    expected_domain: String,
}

/// Decide the audit set, recording a skip for every feeder that will not run.
///
/// # The ring is a floor, not a gate
///
/// Every input that used to decide *whether to audit* came from one
/// nest-computed reply — `identity`, `identity.method`, `identity.did` — while
/// the evidence itself is fetched independently from the public directory. So
/// **the accused party chose whether it was examined**: a nest answering
/// `identity: null` made both directory feeders return without posting,
/// clearing, or recording anything.
///
/// The client already holds an anchor the nest cannot author:
/// [`AtprotoRotationKey::published_for_dids`](fauna_core::data::AtprotoRotationKey)
/// is written only from the directory's own log or from having just signed for
/// it — *never from nest testimony* (`atproto-pds-bridge.md` § State & data
/// shape, decisions (a) and (b)). Those DIDs are therefore unioned into the
/// audit set here, **after** the nest's own contribution and independent of it.
///
/// The ring only ever *adds* targets: an empty or not-yet-synced ring leaves
/// the plan byte-identical to what the nest alone would have produced, which is
/// why this cannot re-introduce the muting feeder #3 deliberately avoided.
/// `ring` is `None` when the keyring could not be read — the caller has already
/// recorded that as feeder #1's *failure*, so nothing here adds a skip on top
/// of it. There is then no floor, which is exactly the pre-fix behaviour.
fn plan_directory_audit(
    status: &fauna_protocol::atproto_pds::GetIntegrationStatusReply,
    ring: Option<&HeldRing>,
    report: &mut SweepReport,
) -> AuditPlan {
    // ── What the NEST contributes: at most one DID, past the same three gates
    // that used to end the whole sweep.
    // The DID's own prefix decides auditability, NOT `identity.method` — the
    // nest's *label* for its own identity is the audited party's word, and
    // gating on it let a `did:plc:` reported as `method: "web"` skip the sweep
    // entirely while the settings page audited it. One
    // shared predicate now answers this for both resolvers.
    let (nest_target, nest_gate) = match status.identity.as_ref() {
        None => (None, Some(skip_reasons::NO_IDENTITY)),
        Some(id) => match id.did.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            None => (None, Some(skip_reasons::NO_DID)),
            Some(did) if !fauna_client_atproto::genesis_verify::is_auditable_did(did) => {
                (None, Some(skip_reasons::NOT_PLC))
            }
            Some(did) => (
                Some(AuditTarget {
                    did: did.to_string(),
                    handle: id.handle.clone(),
                    nest_claims_live: true,
                }),
                None,
            ),
        },
    };

    // ── What the CLIENT contributes, from evidence the nest cannot author.
    let mut targets: Vec<AuditTarget> = nest_target.into_iter().collect();
    for did in ring.iter().flat_map(|r| r.protected_dids.iter()) {
        if !targets.iter().any(|t| &t.did == did) {
            targets.push(AuditTarget {
                did: did.clone(),
                handle: did.clone(),
                nest_claims_live: false,
            });
        }
    }

    // ── Feeder #1: needs something to compare, and something to compare it to.
    let Some(ring) = ring else {
        // Unreadable keyring — already on the record as this feeder's failure.
        return AuditPlan::binding_only(status, nest_gate, targets, report);
    };
    let custody_runs = if targets.is_empty() {
        // Nothing named a DID at all. `targets` can only be empty when the nest
        // gated its answer *and* the ring named nothing, so the nest's own
        // reason is the one on the record — it is the security-relevant half.
        if let Some(reason) = nest_gate {
            report.skip(feeders::GENESIS_CUSTODY, reason);
        }
        false
    } else if ring.senior_keys.is_empty() {
        // With nothing held, no published key could be the user's: there is
        // nothing to compare, and alarming would accuse every device whose
        // `fauna.state.atproto-identity` has not synced yet.
        report.skip(feeders::GENESIS_CUSTODY, skip_reasons::EMPTY_RING);
        false
    } else {
        true
    };

    let (binding_runs, expected_domain) = plan_handle_binding(status, nest_gate, report);

    AuditPlan {
        // Neither check can say anything ⇒ no directory is touched at all.
        targets: if custody_runs || binding_runs {
            targets
        } else {
            Vec::new()
        },
        custody_runs,
        binding_runs,
        expected_domain,
    }
}

/// Feeder #3's half of the plan — the nest-named identity only (see
/// [`AuditPlan::binding_runs`]). Returns whether it runs and the domain it
/// compares against.
fn plan_handle_binding(
    status: &fauna_protocol::atproto_pds::GetIntegrationStatusReply,
    nest_gate: Option<&'static str>,
    report: &mut SweepReport,
) -> (bool, String) {
    let expected_domain = status.handle_domain.trim().to_string();
    let runs = match (nest_gate, expected_domain.is_empty()) {
        (Some(reason), _) => {
            report.skip(feeders::HANDLE_BINDING, reason);
            false
        }
        (None, true) => {
            report.skip(feeders::HANDLE_BINDING, skip_reasons::NO_HANDLE_DOMAIN);
            false
        }
        (None, false) => true,
    };
    (runs, expected_domain)
}

impl AuditPlan {
    /// The plan when feeder #1 cannot run at all because the keyring was
    /// unreadable — already on the record as *its failure*, so no skip is added
    /// on top. Feeder #3 needs no ring and still runs off whatever the nest
    /// named; with no ring there is no floor, which is the pre-fix behaviour.
    fn binding_only(
        status: &fauna_protocol::atproto_pds::GetIntegrationStatusReply,
        nest_gate: Option<&'static str>,
        targets: Vec<AuditTarget>,
        report: &mut SweepReport,
    ) -> Self {
        let (binding_runs, expected_domain) = plan_handle_binding(status, nest_gate, report);
        Self {
            targets: if binding_runs { targets } else { Vec::new() },
            custody_runs: false,
            binding_runs,
            expected_domain,
        }
    }
}

/// Fetch each target's published log once and answer both feeders from it.
///
/// `fetch` is injected so the whole per-target loop — the multi-DID floor, the
/// retired-identity silence, the per-target failure arms — is exercised
/// headlessly. Production passes the real directory read.
async fn execute_audit_plan<F, Fut>(
    plan: &AuditPlan,
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    report: &mut SweepReport,
    fetch: F,
) where
    F: Fn(String) -> Fut,
    Fut: core::future::Future<
            Output = Result<Vec<u8>, fauna_client_atproto::genesis_verify::VerifyFailure>,
        >,
{
    for target in &plan.targets {
        let binding_here = plan.binding_runs && target.nest_claims_live;
        if !plan.custody_runs && !binding_here {
            continue;
        }

        // ── The one directory read both checks answer from ───────────────────
        tracing::debug!(
            stage = "directory-fetch-begin",
            did = %target.did,
            "session-start sweep: fetching the published PLC audit log"
        );
        let body = match fetch(target.did.clone()).await {
            Ok(body) => body,
            Err(e) => {
                tracing::warn!(
                    did = %target.did,
                    error = %e,
                    "session-start sweep: PLC directory unreadable; the directory checks \
                     are inconclusive for this identity and any standing alert is left as-is"
                );
                let error = format!("{}: {e}", target.did);
                if plan.custody_runs {
                    report.failures.push(FeederFailure {
                        feeder: feeders::GENESIS_CUSTODY,
                        error: error.clone(),
                    });
                }
                if binding_here {
                    report.failures.push(FeederFailure {
                        feeder: feeders::HANDLE_BINDING,
                        error,
                    });
                }
                continue;
            }
        };

        tracing::debug!(
            stage = "directory-fetch-end",
            did = %target.did,
            bytes = body.len(),
            "session-start sweep: PLC audit log fetched"
        );

        // Custody first: feeder #3 suppresses its own alarm while a custody
        // alarm stands for this DID, and running #1 first makes that read the
        // *current* sweep's verdict rather than the previous one's.
        if plan.custody_runs {
            run_genesis_custody_feeder(&body, custody, alerts, target, report).await;
        }
        if binding_here {
            run_handle_binding_feeder(&body, alerts, target, &plan.expected_domain, report);
        }
    }
}

/// Feeder #3's half of one target's log — see [`AuditPlan::binding_runs`] for
/// why it only ever runs on the nest-named identity.
fn run_handle_binding_feeder(
    body: &[u8],
    alerts: &CriticalAlerts,
    target: &AuditTarget,
    expected_domain: &str,
    report: &mut SweepReport,
) {
    let custody_key = fauna_client_atproto::genesis_verify::alert_key(&target.did);
    let custody_alarm_standing = alerts.active().iter().any(|row| row.key == custody_key);
    match fauna_client_atproto::handle_binding::verify_handle_binding(body, expected_domain) {
        Ok(v) => {
            fauna_client_atproto::handle_binding::sync_handle_binding_alert(
                alerts,
                &target.did,
                expected_domain,
                &v,
                custody_alarm_standing,
            );
            tracing::debug!(?v, "session-start sweep: handle-binding checked");
            report.check(feeders::HANDLE_BINDING);
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "session-start sweep: the published log did not yield a handle verdict; \
                 any standing alert is left as-is"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::HANDLE_BINDING,
                error: e.to_string(),
            });
        }
    }
}

/// Feeder #1 — does the published log still show a rotation key **this user
/// holds** as senior over every standing operation?
///
/// The check itself is [`fauna_client_atproto::genesis_verify::verify_audit_log`],
/// unchanged and shared with the settings machine. What belongs here is the one
/// thing a *session-start* run has to decide differently.
///
/// # Why this needs no debounce, and why the read order is the whole answer
///
/// On the settings page the check runs on every status convergence, and a
/// contradiction must survive **two consecutive** convergences before alarming
/// (`atproto-pds-bridge.md` § State & data shape). That debounce buys exactly
/// one thing: it excludes the single legitimate cause of a contradiction — a
/// sibling device re-minted with a fresh senior key (fresh-key-per-mint) and
/// this device read the new DID's log before its custody rows held that
/// key. A sweep is **one shot**, so a copied debounce would never reach its
/// second look and feeder #1 could never alarm from here at all.
///
/// The rule is re-expressed rather than copied, and it becomes a **causal**
/// barrier instead of a temporal one: *read the log first, then read the ring.*
/// That excludes the legitimate cause by construction, because the mint path
/// writes the fresh key to `fauna.state.atproto-identity` **before** it can be
/// published — `mint_rotation_key`'s `merge_atproto_identity` join completes
/// (and aborts the mint if it fails)
/// before `set_integration_level` is called, which is what causes the genesis
/// operation to appear in the directory at all. So every op present in a log we
/// fetched at T already had its senior key stored at the nest before T, and a
/// ring read *after* T must observe it. A ring read strictly newer than the log
/// read cannot be missing a legitimately published key — no waiting required.
///
/// This is strictly stronger than two-consecutive-convergences (it excludes the
/// race rather than out-waiting it) and strictly faster (the alarm lands on the
/// first sweep, inside PLC's 72 h contest window rather than a session start
/// later). The alarm's meaning is unchanged: only a definite contradiction in a
/// log we could actually read raises it.
///
/// The ring read below is therefore deliberately a *second* read, not a reuse of
/// the gate read the caller already made: only a read taken after the log can
/// carry the ordering guarantee, and reusing the earlier value would silently
/// reintroduce exactly the race the debounce existed for.
async fn run_genesis_custody_feeder(
    body: &[u8],
    custody: &Arc<dyn AtprotoIdentityStore>,
    alerts: &CriticalAlerts,
    target: &AuditTarget,
    report: &mut SweepReport,
) {
    use fauna_client_atproto::genesis_verify::{SeniorityVerdict, verify_audit_log};
    let did = target.did.as_str();

    // The deciding ring read — strictly AFTER the log fetch (see the doc
    // comment: that ordering is the debounce's replacement, not an accident).
    tracing::debug!(
        stage = "custody-deciding-ring-begin",
        %did,
        "session-start sweep: re-reading the held rotation keyring for the custody verdict"
    );
    let ring = match read_held_ring(custody).await {
        Ok(ring) => ring.senior_keys,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "session-start sweep: could not re-read the rotation keyring after the \
                 log; the custody verdict is inconclusive and any standing alert stands"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::GENESIS_CUSTODY,
                error: e,
            });
            return;
        }
    };
    // The ring emptied between the gate read and this one — nothing to compare
    // against, which is silence rather than an accusation.
    if ring.is_empty() {
        return;
    }

    let verdict = match verify_audit_log(body, &ring) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "session-start sweep: the published log did not yield a custody verdict; \
                 any standing alert is left as-is"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::GENESIS_CUSTODY,
                error: e.to_string(),
            });
            return;
        }
    };

    // A DID the nest does not claim is live, whose log is terminally retired
    // **by the user's own act**, is an entry reaching the ordinary end of its
    // life — the burn in `published_for_dids` is permanent, so the floor meets
    // every identity the user ever retired on every sweep. Silent, and it
    // *clears*: a retirement in the public log is the condition being
    // re-checked and found resolved (`critical-alerts.md` § Mechanism →
    // *Lifetime*), which is the same client-side evidence the settings
    // machine's departed-DID clear now requires. Since 2026-08-02 `is_terminal_retirement` is true only when the tombstone's
    // signature verifies against the held ring — a tombstone anyone's *other*
    // listed key signed (the bridge's junior key foremost) falls through to
    // the alarm below instead of being absorbed as "retired". See
    // [`AuditTarget::nest_claims_live`] for why a DID the nest *does* claim is
    // live stays an alarm either way.
    if !target.nest_claims_live && verdict.is_terminal_retirement() {
        tracing::debug!(%did, "session-start sweep: a ring-held DID is terminally retired; silent");
        alerts.clear(&fauna_client_atproto::genesis_verify::alert_key(did));
        report.check(feeders::GENESIS_CUSTODY);
        return;
    }

    fauna_client_atproto::genesis_verify::sync_custody_alert(alerts, did, &target.handle, &verdict);
    // Converge the published-for burn on the passing arm, the same fact the
    // settings machine records: the directory's own log just showed these held
    // keys senior for this DID, which is what stops `mint_rotation_key` ever
    // reusing them. Idempotent; a failure is quiet (the next pass re-records).
    if let SeniorityVerdict::Verified {
        observed_seniors, ..
    } = &verdict
    {
        tracing::debug!(
            stage = "custody-burn-begin",
            %did,
            seniors = observed_seniors.len(),
            "session-start sweep: converging the published-for burn (one join per key)"
        );
        for senior in observed_seniors {
            if let Err(e) = fauna_client_atproto::rotation_key::record_published_binding(
                &**custody, did, senior,
            )
            .await
            {
                tracing::warn!(%did, %e, "could not record the rotation-key publication; will re-derive");
            }
        }
    }
    tracing::debug!(
        stage = "custody-end",
        ?verdict,
        "session-start sweep: genesis-custody checked"
    );
    report.check(feeders::GENESIS_CUSTODY);
}

/// The client's held rotation keyring, as the sweep reads it.
///
/// Two facts, and the difference between them is the fix: the keys are
/// what a published log is *compared against*, while the DIDs are what the
/// sweep independently knows to *look at* — the audit floor
/// ([`plan_directory_audit`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HeldRing {
    /// Every held key's `did:key:` public half, as `verify_audit_log` compares
    /// against.
    senior_keys: Vec<String>,
    /// Every DID this client independently knows to look at, deduped and
    /// sorted — the audit floor, from its **two** sources:
    ///
    /// 1. `published_for_dids` — directory-derived, never nest testimony
    ///    (`atproto-pds-bridge.md` § State & data shape, decision (b)).
    /// 2. `nest_named_dids` — nest testimony the client **froze**, so the box
    ///    cannot retract it.
    ///
    /// They are merged here because the plan only asks one question of them:
    /// *which DIDs get fetched?* Source 1 alone left the genesis-time
    /// compromise fully nest-gated — it is written only on a **passing**
    /// custody verdict, and the compromised case never passes. Source 2 is
    /// weaker evidence by construction and carries no custody claim; it is
    /// sufficient here only because a floor can add targets and never mute one.
    ///
    /// ⚠ Do **not** reuse this merged view where the two provenances differ —
    /// the settings machine's departed-alarm clear reads them separately, and
    /// must.
    protected_dids: Vec<String>,
}

/// Read [`HeldRing`] from the account plane's custody
/// (`fauna.state.atproto-identity`). A live store read every call — never a
/// cache — which is what makes "read the ring after the log" a real ordering
/// guarantee.
async fn read_held_ring(custody: &Arc<dyn AtprotoIdentityStore>) -> Result<HeldRing, String> {
    let cfg = custody.atproto_identity().await?;
    let mut protected_dids: Vec<String> = cfg
        .rotation_keys
        .iter()
        .flat_map(|k| k.published_for_dids.iter().cloned())
        .chain(cfg.nest_named_dids.iter().cloned())
        .collect();
    protected_dids.sort();
    protected_dids.dedup();
    Ok(HeldRing {
        senior_keys: cfg
            .rotation_keys
            .iter()
            .map(|k| k.pubkey_did_key.clone())
            .collect(),
        protected_dids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use fauna_client_alerts::CriticalAlertsObserver;
    use fauna_protocol::recovery as wire;
    use fauna_protocol::{ByteBuf, RpcError};

    /// A transport that answers the sweep's kinds — enough to prove its
    /// contract without re-implementing a nest.
    ///
    /// **Kind-routed, deliberately.** It answered every kind with one reply
    /// until feeder #3 joined; a fake that ignores the kind silently hands the
    /// wrong body to the next feeder added, and the failure surfaces as a decode
    /// panic three crates away from the mistake.
    #[derive(Clone)]
    struct FakeRpc {
        inner: std::sync::Arc<FakeState>,
    }

    struct FakeState {
        /// What `replacement.status` answers, or `None` to fail transport-wise.
        pending: Mutex<Option<Option<wire::ReplacementPendingInfo>>>,
        /// What `atproto.get_integration_status` answers. The default is the
        /// off, no-identity deployment — the state in which feeder #3 has
        /// nothing to check and must therefore make **no network call**, which
        /// is what keeps this unit-test suite offline.
        atproto: Mutex<fauna_protocol::atproto_pds::GetIntegrationStatusReply>,
        /// What `domain.expiry.get` answers. The default is `record: None` — a
        /// nest whose watch has not completed an attempt yet, which is the
        /// honest state of a freshly-booted fake and makes no network claim.
        domain: Mutex<fauna_protocol::domain_expiry::DomainExpiryReply>,
        /// `(kind, error code)` to reject with, leaving every other kind alone.
        reject_kind: Mutex<Option<(&'static str, &'static str)>>,
        calls: Mutex<Vec<&'static str>>,
    }

    #[derive(Debug)]
    struct FakeError(RpcError);

    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }

    impl RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            // A transport fault, not a nest refusal — the arm that must NOT
            // clear a standing alert.
            false
        }

        /// Exposes the wire code, so a test can script a rejection by its
        /// *code*.
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl FakeRpc {
        fn answering(pending: Option<wire::ReplacementPendingInfo>) -> Self {
            Self {
                inner: std::sync::Arc::new(FakeState {
                    pending: Mutex::new(Some(pending)),
                    atproto: Mutex::new(Default::default()),
                    domain: Mutex::new(Default::default()),
                    reject_kind: Mutex::new(None),
                    calls: Mutex::new(Vec::new()),
                }),
            }
        }

        fn unreachable() -> Self {
            Self {
                inner: std::sync::Arc::new(FakeState {
                    pending: Mutex::new(None),
                    atproto: Mutex::new(Default::default()),
                    domain: Mutex::new(Default::default()),
                    reject_kind: Mutex::new(None),
                    calls: Mutex::new(Vec::new()),
                }),
            }
        }

        /// Report a hosted identity of `method` with `did`, at `domain`.
        fn with_identity(self, method: &str, did: Option<&str>, domain: &str) -> Self {
            *self.inner.atproto.lock().unwrap() =
                fauna_protocol::atproto_pds::GetIntegrationStatusReply {
                    level: "hosted_full".into(),
                    hosted_allowed: true,
                    handle_domain: domain.into(),
                    handle_preview: format!("alice.{domain}"),
                    identity: Some(fauna_protocol::atproto_pds::AtprotoIdentitySummary {
                        handle: "alice".into(),
                        method: method.into(),
                        status: "active".into(),
                        did: did.map(str::to_string),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
            self
        }

        /// Reject exactly `kind` with `code`, answering every other kind
        /// normally — the shape of an older nest that lacks one handler.
        fn rejecting_kind(self, kind: &'static str, code: &'static str) -> Self {
            *self.inner.reject_kind.lock().unwrap() = Some((kind, code));
            self
        }

        /// Report a domain-expiry record from the nest's watch.
        fn with_domain_expiry(
            self,
            record: fauna_protocol::domain_expiry::DomainExpiryRecord,
            admin: bool,
        ) -> Self {
            *self.inner.domain.lock().unwrap() = fauna_protocol::domain_expiry::DomainExpiryReply {
                record: Some(record),
                admin,
                ..Default::default()
            };
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.inner.calls.lock().unwrap().clone()
        }
    }

    impl RpcRequester for FakeRpc {
        type Error = FakeError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.inner.calls.lock().unwrap().push(kind);
            if let Some((rejected, code)) = *self.inner.reject_kind.lock().unwrap()
                && rejected == kind
            {
                return Err(FakeError(RpcError::new(code, "scripted rejection")));
            }
            // `pending: None` stands for "the nest went away" for BOTH kinds:
            // an unreachable nest is unreachable for every feeder, which is the
            // state the sweep's every-feeder-runs-independently contract is
            // about.
            let answer = self.inner.pending.lock().unwrap().clone();
            let Some(pending) = answer else {
                return Err(FakeError(RpcError::new(
                    "transport.disconnected",
                    "the nest went away mid-sweep",
                )));
            };
            let bytes = if kind == "fauna.bridges.atproto.get_integration_status" {
                fauna_core::encoding::canonical_encode(&*self.inner.atproto.lock().unwrap())
                    .expect("encode reply")
            } else if kind == "fauna.domain.expiry.get" {
                fauna_core::encoding::canonical_encode(&*self.inner.domain.lock().unwrap())
                    .expect("encode reply")
            } else {
                fauna_core::encoding::canonical_encode(&wire::ReplacementStatusReply {
                    pending,
                    ..Default::default()
                })
                .expect("encode reply")
            };
            Ok(fauna_core::encoding::canonical_decode(&bytes).expect("decode reply"))
        }
    }

    fn actor(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    // ── Feeder #1's fixtures ─────────────────────────────────────────────────

    const USER_KEY: &str = "did:key:zDnaeUserSeniorKey";
    const BOX_KEY: &str = "did:key:zQ3shBoxJuniorKey"; // gitleaks:allow
    const DID: &str = "did:plc:abc123";
    const HANDLE: &str = "alice";

    /// A one-entry audit log whose genesis lists `rotation_keys` in order.
    fn audit_log(rotation_keys: &[&str]) -> Vec<u8> {
        let entry = fauna_client_atproto::test_fixtures::plc_operation_entry_json(
            "bafyfake",
            "plc_operation",
            rotation_keys,
            false,
            "2026-08-02T00:00:00Z",
        );
        serde_json::to_vec(&[entry]).expect("encode audit log")
    }

    /// A genesis by `rotation_keys`, then a `plc_tombstone` whose signature
    /// does NOT verify against any held key — what the directory serves after
    /// someone *else's* listed key (the bridge's junior one foremost)
    /// destroyed the identity. For the user's own retirement use
    /// [`own_tombstoned_log`]; since the 2026-08-02 re-take the two read differently on purpose.
    fn tombstoned_log(rotation_keys: &[&str]) -> Vec<u8> {
        let mut entries: Vec<serde_json::Value> =
            serde_json::from_slice(&audit_log(rotation_keys)).expect("decode genesis");
        entries.push(serde_json::json!({
            "cid": "bafytomb",
            "nullified": false,
            "createdAt": "2026-08-02T01:00:00Z",
            "operation": {
                "type": fauna_client_atproto::tombstone::OP_TYPE_TOMBSTONE,
                "prev": "bafyfake",
                "sig": "fakesig"
            }
        }));
        serde_json::to_vec(&entries).expect("encode tombstoned log")
    }

    /// A genesis by `rotation_keys`, then the `plc_tombstone` that terminally
    /// retires it, genuinely signed by `key` — what the directory serves for
    /// an identity the USER retired (this device or a sibling; the ring holds
    /// the signing key forever, since the retirement path burns it).
    fn own_tombstoned_log(
        rotation_keys: &[&str],
        key: &fauna_core::data::AtprotoRotationKey,
    ) -> Vec<u8> {
        let op = fauna_client_atproto::tombstone::sign_tombstone(
            "bafyfake",
            &key.secret_scalar.to_array(),
        )
        .expect("sign fixture tombstone");
        let mut entries: Vec<serde_json::Value> =
            serde_json::from_slice(&audit_log(rotation_keys)).expect("decode genesis");
        entries.push(serde_json::json!({
            "cid": "bafytomb",
            "nullified": false,
            "createdAt": "2026-08-02T01:00:00Z",
            "operation": {
                "type": op.op_type,
                "prev": op.prev,
                "sig": op.sig
            }
        }));
        serde_json::to_vec(&entries).expect("encode tombstoned log")
    }

    /// An `AtprotoIdentityStore` whose successive reads can return **different**
    /// rings — the only way to drive the read-ordering rule this feeder rests
    /// on. Each entry is one ring; the last repeats for every further read.
    struct FakeIdentityStore {
        rings: Mutex<std::collections::VecDeque<Vec<String>>>,
        /// Attached to every key of every ring — the directory-derived
        /// provenance the audit floor reads.
        published_for: Vec<String>,
        /// The audit floor's SECOND source: DIDs the nest once named and this
        /// client froze. Carries no custody claim.
        nest_named: Vec<String>,
        fail: bool,
        loads: Mutex<usize>,
    }

    impl FakeIdentityStore {
        fn with_rings(rings: &[&[&str]]) -> Arc<dyn AtprotoIdentityStore> {
            Arc::new(Self {
                rings: Mutex::new(
                    rings
                        .iter()
                        .map(|r| r.iter().map(|k| k.to_string()).collect())
                        .collect(),
                ),
                published_for: Vec::new(),
                nest_named: Vec::new(),
                fail: false,
                loads: Mutex::new(0),
            })
        }

        fn unreadable() -> Arc<dyn AtprotoIdentityStore> {
            Arc::new(Self {
                rings: Mutex::new(Default::default()),
                published_for: Vec::new(),
                nest_named: Vec::new(),
                fail: true,
                loads: Mutex::new(0),
            })
        }

        fn custody(&self) -> fauna_core::data::AtprotoIdentityConfig {
            let mut q = self.rings.lock().unwrap();
            *self.loads.lock().unwrap() += 1;
            let ring = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().unwrap_or_default()
            };
            fauna_core::data::AtprotoIdentityConfig {
                rotation_keys: ring
                    .iter()
                    .map(|k| fauna_core::data::AtprotoRotationKey {
                        pubkey_did_key: k.clone(),
                        published_for_dids: self.published_for.clone(),
                        ..fauna_client_atproto::rotation_key::generate_rotation_key(0)
                    })
                    .collect(),
                nest_named_dids: self.nest_named.clone(),
                ..Default::default()
            }
        }
    }

    /// A store holding `ring`, with **no** directory-derived bindings, whose
    /// client froze `named` when the nest once claimed them.
    fn store_nest_named(ring: &[&str], named: &[&str]) -> Arc<dyn AtprotoIdentityStore> {
        Arc::new(FakeIdentityStore {
            rings: Mutex::new(
                [ring.iter().map(|k| k.to_string()).collect::<Vec<_>>()]
                    .into_iter()
                    .collect(),
            ),
            published_for: Vec::new(),
            nest_named: named.iter().map(|d| d.to_string()).collect(),
            fail: false,
            loads: Mutex::new(0),
        })
    }

    /// A store whose held keys were observed published for `dids` — the
    /// directory-derived anchor the audit floor stands on.
    fn store_published_for(ring: &[&str], dids: &[&str]) -> Arc<dyn AtprotoIdentityStore> {
        Arc::new(FakeIdentityStore {
            rings: Mutex::new(
                [ring.iter().map(|k| k.to_string()).collect::<Vec<_>>()]
                    .into_iter()
                    .collect(),
            ),
            published_for: dids.iter().map(|d| d.to_string()).collect(),
            nest_named: Vec::new(),
            fail: false,
            loads: Mutex::new(0),
        })
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl AtprotoIdentityStore for FakeIdentityStore {
        async fn atproto_identity(
            &self,
        ) -> Result<fauna_core::data::AtprotoIdentityConfig, String> {
            if self.fail {
                return Err("fake load failure".into());
            }
            Ok(self.custody())
        }
        async fn merge_atproto_identity(
            &self,
            replica: fauna_core::data::AtprotoIdentityConfig,
        ) -> Result<fauna_core::data::AtprotoIdentityConfig, String> {
            // Joined against the ring as it now reads, never stored: the
            // rings the test scripted stay the rings every read answers.
            Ok(self.custody().merge(&replica))
        }
    }

    /// A store whose ring never changes — the ordinary case.
    fn store(ring: &[&str]) -> Arc<dyn AtprotoIdentityStore> {
        FakeIdentityStore::with_rings(&[ring])
    }

    /// Drive feeder #1 the way `run_directory_feeders` does: the cheap **gate**
    /// read first, then the log, then the feeder's own **deciding** read. The
    /// two reads are what the ordering rule is about, so a helper that made only
    /// one would test a sequence production never runs.
    fn custody_feeder(
        body: &[u8],
        custody: &Arc<dyn AtprotoIdentityStore>,
        alerts: &CriticalAlerts,
    ) -> SweepReport {
        custody_feeder_for(body, custody, alerts, &nest_named(DID))
    }

    /// The nest-named identity — what every pre-floor test models.
    fn nest_named(did: &str) -> AuditTarget {
        AuditTarget {
            did: did.into(),
            handle: HANDLE.into(),
            nest_claims_live: true,
        }
    }

    /// A DID known only from the held ring's `published_for_dids`.
    fn ring_derived(did: &str) -> AuditTarget {
        AuditTarget {
            did: did.into(),
            handle: did.into(),
            nest_claims_live: false,
        }
    }

    fn custody_feeder_for(
        body: &[u8],
        custody: &Arc<dyn AtprotoIdentityStore>,
        alerts: &CriticalAlerts,
        target: &AuditTarget,
    ) -> SweepReport {
        let mut report = SweepReport::default();
        block_on(async {
            // The gate read (its value only decides whether the feeder runs at
            // all; an unreadable store still reaches the feeder, which records
            // the failure).
            let _gate = read_held_ring(custody).await;
            run_genesis_custody_feeder(body, custody, alerts, target, &mut report).await;
        });
        report
    }

    /// **The claim the whole track rests on.** On the settings page a
    /// contradiction must survive two consecutive convergences; a sweep is one
    /// shot, so a copied debounce would mean feeder #1 could never alarm from
    /// here. Reading the log before the ring excludes the one legitimate cause
    /// by ordering instead, so a real contradiction is loud after ONE sweep.
    #[test]
    fn a_custody_mismatch_alarms_on_a_single_sweep() {
        let alerts = CriticalAlerts::new();
        let report = custody_feeder(&audit_log(&[BOX_KEY]), &store(&[USER_KEY]), &alerts);

        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
        let active = alerts.active();
        assert_eq!(active.len(), 1, "one sweep must be enough to alarm");
        assert_eq!(
            active[0].key,
            fauna_client_atproto::genesis_verify::alert_key(DID)
        );
    }

    /// The legitimate race the debounce existed for: a sibling device re-minted
    /// with a fresh senior key, and this device's ring had not caught up when
    /// the log was read. The confirming re-read — strictly newer than the log —
    /// sees the key and the alarm is correctly withheld.
    #[test]
    fn a_sibling_re_mint_that_lands_between_reads_does_not_alarm() {
        let alerts = CriticalAlerts::new();
        // First ring read: stale (missing the sibling's fresh key) but NOT empty,
        // so the empty-ring gate is not what withholds the alarm. Second read:
        // the key has landed — exactly what the mint's write ordering promises.
        let custody = FakeIdentityStore::with_rings(&[&[BOX_KEY], &[USER_KEY]]);
        let report = custody_feeder(&audit_log(&[USER_KEY]), &custody, &alerts);

        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
        assert!(
            alerts.active().is_empty(),
            "a key that landed between the log read and the confirming ring read \
             is the sibling re-mint case, not a compromise"
        );
    }

    /// The passing arm clears — the only thing that legitimately takes this
    /// non-dismissable banner down.
    #[test]
    fn a_verified_log_clears_a_standing_custody_alarm() {
        let alerts = CriticalAlerts::new();
        custody_feeder(&audit_log(&[BOX_KEY]), &store(&[USER_KEY]), &alerts);
        assert_eq!(alerts.active().len(), 1);

        custody_feeder(&audit_log(&[USER_KEY]), &store(&[USER_KEY]), &alerts);
        assert!(alerts.active().is_empty());
    }

    /// An unreadable ring is "could not check", never "resolved": the standing
    /// alarm survives and the report says so.
    #[test]
    fn an_unreadable_ring_leaves_a_standing_custody_alarm_alone() {
        let alerts = CriticalAlerts::new();
        custody_feeder(&audit_log(&[BOX_KEY]), &store(&[USER_KEY]), &alerts);
        assert_eq!(alerts.active().len(), 1);

        let report = custody_feeder(
            &audit_log(&[USER_KEY]),
            &FakeIdentityStore::unreadable(),
            &alerts,
        );
        assert_eq!(report.failures[0].feeder, feeders::GENESIS_CUSTODY);
        assert!(report.checked.is_empty());
        assert_eq!(
            alerts.active().len(),
            1,
            "a failed read is not a resolution"
        );
    }

    /// With nothing held, no published key could be the user's — alarming would
    /// accuse every device whose `fauna.state.atproto-identity` has not synced yet.
    #[test]
    fn an_empty_ring_is_silent() {
        let alerts = CriticalAlerts::new();
        let report = custody_feeder(&audit_log(&[BOX_KEY]), &store(&[]), &alerts);
        assert!(report.checked.is_empty() && report.failures.is_empty());
        assert!(alerts.active().is_empty());
    }

    fn window(lands_at: i64) -> wire::ReplacementPendingInfo {
        wire::ReplacementPendingInfo {
            new_recovery_pubkey: ByteBuf::from(vec![0xab; 32]),
            requested_at: 0,
            lands_at,
            ..Default::default()
        }
    }

    fn block_on<F: core::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    /// The gap this crate closes: a pending window is loud after a sweep, with
    /// no page ever visited.
    #[test]
    fn a_sweep_raises_the_pending_replacement_window() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);
        let rpc = FakeRpc::answering(Some(window(90_000)));

        let report = block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            &alerts,
            &id,
            0,
        ));

        assert!(report.failures.is_empty(), "{report:?}");
        assert_eq!(report.checked, vec![feeders::PENDING_REPLACEMENT]);
        // Not `is_complete()`: this fake reports no ATProto identity, so both
        // directory feeders honestly had nothing to check — and since 2026-08-02
        // that is *recorded* rather than passed over in silence.
        assert_eq!(
            report.skipped,
            vec![
                FeederSkip {
                    feeder: feeders::GENESIS_CUSTODY,
                    reason: skip_reasons::NO_IDENTITY
                },
                FeederSkip {
                    feeder: feeders::HANDLE_BINDING,
                    reason: skip_reasons::NO_IDENTITY
                },
                // Feeder #4: this fake's watch has not completed an attempt, so
                // it honestly has nothing to report — recorded, never silent.
                FeederSkip {
                    feeder: feeders::DOMAIN_EXPIRY,
                    reason: skip_reasons::WATCH_NOT_YET_RUN
                },
            ]
        );
        let active = alerts.active();
        assert_eq!(active.len(), 1, "the window must be loud after one sweep");
        assert_eq!(
            active[0].key,
            fauna_client_recovery::alerts::alert_key(&id),
            "identity-scoped key"
        );
    }

    /// The other half of the same call: a window that was vetoed or landed
    /// takes its non-dismissable banner with it on the next sweep.
    #[test]
    fn a_sweep_clears_a_window_that_no_longer_pends() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));
        assert_eq!(alerts.active().len(), 1);

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(None),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));
        assert!(
            alerts.active().is_empty(),
            "nothing else would ever remove it — the alert is non-dismissable"
        );
    }

    /// The fail-safe direction: an unreachable nest must not be able to silence
    /// a standing compromise warning.
    #[test]
    fn an_unreachable_feeder_leaves_a_standing_alert_alone() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));

        let report = block_on(run_session_start_sweep_at(
            FakeRpc::unreachable(),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));

        assert!(!report.failures.is_empty(), "{report:?}");
        assert_eq!(report.failures[0].feeder, feeders::PENDING_REPLACEMENT);
        assert_eq!(
            alerts.active().len(),
            1,
            "a transport failure is not a resolution"
        );
    }

    /// A failing sweep is still a completed call — the app's sign-in path never
    /// depends on the result.
    #[test]
    fn a_failed_sweep_reports_rather_than_panics() {
        let alerts = CriticalAlerts::new();
        let rpc = FakeRpc::unreachable();
        let report = block_on(run_session_start_sweep_at(
            rpc.clone(),
            &store(&[]),
            &alerts,
            &actor(3),
            0,
        ));
        assert!(report.checked.is_empty());
        assert!(
            alerts.active().is_empty(),
            "a failed read posts nothing it did not learn"
        );
        // Every feeder runs and fails on its own: one feeder's dead transport
        // must never skip a later feeder, or a nest that cannot answer the
        // recovery plane could silence a different feeder's alarm.
        assert_eq!(
            report.failures.iter().map(|f| f.feeder).collect::<Vec<_>>(),
            vec![
                feeders::PENDING_REPLACEMENT,
                feeders::GENESIS_CUSTODY,
                feeders::HANDLE_BINDING,
                feeders::DOMAIN_EXPIRY
            ],
        );
        assert_eq!(
            rpc.calls(),
            vec![
                fauna_client_recovery::nest::kinds::REPLACEMENT_STATUS,
                "fauna.bridges.atproto.get_integration_status",
                "fauna.domain.expiry.get",
            ],
            "the sweep drives each feeder's standing read, not a ceremony"
        );
    }

    /// A deployment with no hosted ATProto identity has nothing to check — and
    /// crucially reaches **no** directory. Silence here is the crying-wolf bar
    /// (`critical-alerts.md` § Goal) doing its job: "nothing published" is the
    /// common honest state, not a finding.
    #[test]
    fn no_hosted_identity_checks_nothing_and_touches_no_directory() {
        let alerts = CriticalAlerts::new();
        let rpc = FakeRpc::answering(None);
        let report = block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            &alerts,
            &actor(4),
            0,
        ));
        assert!(
            !report.checked.contains(&feeders::HANDLE_BINDING)
                && !report
                    .failures
                    .iter()
                    .any(|f| f.feeder == feeders::HANDLE_BINDING),
            "nothing to check is neither a pass nor a failure"
        );
        assert!(alerts.active().is_empty());
    }

    /// did:web has no directory log to audit — its custody **is** domain
    /// custody, so the comparison would be tautological
    /// (`atproto-pds-bridge.md` § State & data shape). Silent, and again no
    /// network call.
    #[test]
    fn a_did_web_identity_is_silent() {
        let alerts = CriticalAlerts::new();
        let rpc = FakeRpc::answering(None).with_identity(
            "web",
            Some("did:web:alice.example.com"),
            "example.com",
        );
        let report = block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            &alerts,
            &actor(5),
            0,
        ));
        assert!(!report.checked.contains(&feeders::HANDLE_BINDING));
        assert!(alerts.active().is_empty());
    }

    /// A pending mint has no DID yet — nothing to resolve, so nothing to say.
    #[test]
    fn a_pending_mint_is_silent() {
        let alerts = CriticalAlerts::new();
        let rpc = FakeRpc::answering(None).with_identity("plc", None, "example.com");
        let report = block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            &alerts,
            &actor(6),
            0,
        ));
        assert!(!report.checked.contains(&feeders::HANDLE_BINDING));
        assert!(alerts.active().is_empty());
    }

    /// No handle domain means no comparison basis. Alarming with nothing to
    /// compare against would accuse every deployment that has not published a
    /// domain yet.
    #[test]
    fn no_handle_domain_is_silent() {
        let alerts = CriticalAlerts::new();
        let rpc = FakeRpc::answering(None).with_identity("plc", Some("did:plc:abc"), "");
        let report = block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            &alerts,
            &actor(7),
            0,
        ));
        assert!(!report.checked.contains(&feeders::HANDLE_BINDING));
        assert!(alerts.active().is_empty());
    }

    /// Two accounts' sweeps never collide — the switch case the Lifetime rule
    /// covers with `clear_all` is a *different* mechanism from key collision.
    #[test]
    fn sweeps_for_two_actors_hold_independent_alerts() {
        let alerts = CriticalAlerts::new();
        let (a, b) = (actor(1), actor(2));

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &a,
            0,
        ));
        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &b,
            0,
        ));
        assert_eq!(alerts.active().len(), 2);
    }

    /// The clock reaches the copy: the countdown a user reads comes from the
    /// sweep's `now`, so a long-lived session's later sweep shows fewer days.
    #[test]
    fn the_sweeps_clock_reaches_the_rendered_countdown() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);
        let lands_at = 10 * 86_400;

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(lands_at))),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));
        assert_eq!(alerts.active()[0].lines[1].args["days"], "10");

        block_on(run_session_start_sweep_at(
            FakeRpc::answering(Some(window(lands_at))),
            &store(&[]),
            &alerts,
            &id,
            9 * 86_400,
        ));
        assert_eq!(alerts.active()[0].lines[1].args["days"], "1");
    }

    // ── The skipped bucket ─────────────────────────

    /// **The finding itself.** A nest that answers `identity: null` used to make
    /// both directory feeders return without posting, clearing, *or recording*
    /// anything, while the report still said every feeder was reached — so a box
    /// could decline to be audited and no log could tell that from health. The
    /// skip is now on the record.
    #[test]
    fn a_nest_denied_identity_is_recorded_not_passed_over_in_silence() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        let report = block_on(run_session_start_sweep_at(
            FakeRpc::answering(None),   // default atproto status: no identity
            &store(&["did:key:zHeld"]), // ...and this client DOES hold a key
            &alerts,
            &id,
            0,
        ));

        assert!(
            report.failures.is_empty(),
            "a denied identity is not a transport failure: {report:?}"
        );
        assert!(
            !report.skipped.is_empty(),
            "a sweep in which two feeders never ran must record why"
        );
        assert_eq!(
            report.skipped,
            vec![
                FeederSkip {
                    feeder: feeders::GENESIS_CUSTODY,
                    reason: skip_reasons::NO_IDENTITY
                },
                FeederSkip {
                    feeder: feeders::HANDLE_BINDING,
                    reason: skip_reasons::NO_IDENTITY
                },
                // Feeder #4: this fake's watch has not completed an attempt, so
                // it honestly has nothing to report — recorded, never silent.
                FeederSkip {
                    feeder: feeders::DOMAIN_EXPIRY,
                    reason: skip_reasons::WATCH_NOT_YET_RUN
                },
            ]
        );
    }

    /// The control the finding leaned on: an *unreachable* nest was always
    /// handled right (both feeders recorded as failures). It must stay that way
    /// — a failure is not a skip, and only the failure arm means "retry".
    #[test]
    fn an_unreachable_nest_still_fails_rather_than_skips() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        let report = block_on(run_session_start_sweep_at(
            FakeRpc::unreachable(),
            &store(&[]),
            &alerts,
            &id,
            0,
        ));

        let failed: Vec<_> = report.failures.iter().map(|f| f.feeder).collect();
        assert!(failed.contains(&feeders::GENESIS_CUSTODY), "{report:?}");
        assert!(failed.contains(&feeders::HANDLE_BINDING), "{report:?}");
        assert!(
            !report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::HANDLE_BINDING),
            "unreachable must not be reported as nothing-to-check: {report:?}"
        );
    }

    /// Each nothing-to-check state keeps its own token, so a support read says
    /// *why* rather than just "skipped".
    #[test]
    fn every_nothing_to_check_state_names_its_own_reason() {
        for (rpc, ring, expected) in [
            (
                FakeRpc::answering(None).with_identity("web", Some("did:web:x.test"), "x.test"),
                &["did:key:zHeld"][..],
                skip_reasons::NOT_PLC,
            ),
            (
                FakeRpc::answering(None).with_identity("plc", None, "x.test"),
                &["did:key:zHeld"][..],
                skip_reasons::NO_DID,
            ),
        ] {
            let alerts = CriticalAlerts::new();
            let report = block_on(run_session_start_sweep_at(
                rpc,
                &store(ring),
                &alerts,
                &actor(7),
                0,
            ));
            // Scoped to the DIRECTORY feeders: this test is about *their*
            // per-state tokens. Feeder #4 skips here too (this fake's watch has
            // not run), for an unrelated reason of its own — which is the point
            // of per-feeder tokens rather than one shared "skipped" flag.
            let directory: Vec<_> = report
                .skipped
                .iter()
                .filter(|s| {
                    s.feeder == feeders::GENESIS_CUSTODY || s.feeder == feeders::HANDLE_BINDING
                })
                .collect();
            assert!(
                directory.iter().all(|s| s.reason == expected),
                "expected every directory skip to read {expected}: {report:?}"
            );
            assert_eq!(directory.len(), 2, "both directory feeders: {report:?}");
        }
    }

    /// An empty ring is feeder #1's own gate, not a shared one — feeder #3 has a
    /// handle domain and runs, so only custody is skipped, with its own reason.
    #[test]
    fn an_empty_ring_skips_only_the_custody_feeder() {
        let alerts = CriticalAlerts::new();
        let report = block_on(run_session_start_sweep_at(
            FakeRpc::answering(None).with_identity("plc", Some("did:plc:abc"), ""),
            &store(&[]),
            &alerts,
            &actor(7),
            0,
        ));

        assert_eq!(
            report.skipped,
            vec![
                FeederSkip {
                    feeder: feeders::GENESIS_CUSTODY,
                    reason: skip_reasons::EMPTY_RING
                },
                FeederSkip {
                    feeder: feeders::HANDLE_BINDING,
                    reason: skip_reasons::NO_HANDLE_DOMAIN
                },
                FeederSkip {
                    feeder: feeders::DOMAIN_EXPIRY,
                    reason: skip_reasons::WATCH_NOT_YET_RUN
                },
            ],
            "{report:?}"
        );
    }

    // ── The audit floor ───────────────────────────
    //
    // The claim lives on the *plan*, not on a live directory: "who decides
    // whether this box is examined?" is a planning question, and
    // `plan_directory_audit` is pure, so every case below is a value assertion
    // with no network and no clock. The per-target execution loop is driven
    // through `execute_audit_plan`'s injected fetch, so the fetch/verdict half
    // is exercised headlessly too — nothing here is left to an e2e.

    fn status_reply(
        identity: Option<(&str, Option<&str>)>,
        domain: &str,
    ) -> fauna_protocol::atproto_pds::GetIntegrationStatusReply {
        fauna_protocol::atproto_pds::GetIntegrationStatusReply {
            level: "hosted_full".into(),
            hosted_allowed: true,
            handle_domain: domain.into(),
            handle_preview: format!("alice.{domain}"),
            identity: identity.map(|(method, did)| {
                fauna_protocol::atproto_pds::AtprotoIdentitySummary {
                    handle: HANDLE.into(),
                    method: method.into(),
                    status: "active".into(),
                    did: did.map(str::to_string),
                    ..Default::default()
                }
            }),
            ..Default::default()
        }
    }

    fn plan_for(
        identity: Option<(&str, Option<&str>)>,
        domain: &str,
        ring: &HeldRing,
    ) -> (AuditPlan, SweepReport) {
        let mut report = SweepReport::default();
        let plan = plan_directory_audit(&status_reply(identity, domain), Some(ring), &mut report);
        (plan, report)
    }

    fn ring(senior_keys: &[&str], protected: &[&str]) -> HeldRing {
        HeldRing {
            senior_keys: senior_keys.iter().map(|k| k.to_string()).collect(),
            protected_dids: protected.iter().map(|d| d.to_string()).collect(),
        }
    }

    const VICTIM: &str = "did:plc:victimidentity";

    /// **Item 1's headline — the finding's leg A.** A nest that answers
    /// `identity: null` no longer decides whether it is audited: the client's
    /// own ring names a DID it independently observed one of its keys published
    /// senior for, and that DID is audited whatever the nest says.
    ///
    /// Mutation check: delete the `ring.protected_dids` union in
    /// `plan_directory_audit` and this goes red on the first assertion.
    #[test]
    fn a_nest_denied_identity_still_audits_the_dids_the_ring_protects() {
        let (plan, report) = plan_for(None, "", &ring(&[USER_KEY], &[VICTIM]));

        assert_eq!(
            plan.targets,
            vec![ring_derived(VICTIM)],
            "the client's own anchor must survive the nest's silence"
        );
        assert!(plan.custody_runs, "and feeder #1 must actually run on it");
        assert!(
            !report.skipped.is_empty(),
            "the nest still declined to name an identity, and that stays on the \
             record even though the floor rescued the audit: {report:?}"
        );
        assert_eq!(
            report.skipped,
            vec![FeederSkip {
                feeder: feeders::HANDLE_BINDING,
                reason: skip_reasons::NO_IDENTITY,
            }],
            "only feeder #3 is passed over — its comparison basis is nest \
             testimony, so the floor cannot stand in for it: {report:?}"
        );
    }

    /// The link the plan tests above take as given: `read_held_ring` must
    /// actually project `published_for_dids` out of the stored config, deduped
    /// across keys and sorted. This is the line the finding named — the anchor
    /// was being *thrown away* eight lines below the gate that needed it.
    #[test]
    fn the_held_ring_read_carries_the_published_for_anchor() {
        let held = block_on(read_held_ring(&store_published_for(
            &[USER_KEY, "did:key:zSecond"],
            &[VICTIM, "did:plc:aaa", VICTIM],
        )))
        .expect("readable");

        assert_eq!(
            held.senior_keys,
            vec![USER_KEY.to_string(), "did:key:zSecond".to_string()]
        );
        assert_eq!(
            held.protected_dids,
            vec!["did:plc:aaa".to_string(), VICTIM.to_string()],
            "sorted and deduped across every held key"
        );
    }

    // ── the floor was EMPTY in exactly the case feeder #1
    // exists for ────────────────────────────────────────────────────────────
    //
    // `published_for_dids` is written only on a **passing** custody verdict, so
    // a genesis-time compromise — where the verdict never passes — never
    // entered the floor. The audit set was therefore still entirely the accused
    // box's to choose in the one case that matters most. `nest_named_dids` is
    // the second source: nest testimony the client FROZE, so the claim cannot
    // be retracted. Weaker evidence, deliberately — sufficient only because a
    // floor adds targets and can never mute one.

    /// Item 2's link: the floor must actually read its second source out of the
    /// stored config, merged with the first.
    ///
    /// Mutation check: drop the `nest_named_dids` chain in `read_held_ring` and
    /// this goes red.
    #[test]
    fn the_held_ring_read_carries_the_frozen_nest_claim_too() {
        let held = block_on(read_held_ring(&store_nest_named(
            &[USER_KEY],
            &[VICTIM, "did:plc:aaa", VICTIM],
        )))
        .expect("readable");

        assert_eq!(
            held.protected_dids,
            vec!["did:plc:aaa".to_string(), VICTIM.to_string()],
            "a frozen nest claim floors the audit even with nothing ever verified: {held:?}"
        );
    }

    /// Plan a sweep whose client has **never had a passing verdict** — the
    /// never-verified ring the finding is about — but did freeze `named`.
    fn plan_never_verified(
        identity: Option<(&str, Option<&str>)>,
        named: &[&str],
    ) -> (AuditPlan, SweepReport) {
        let ring =
            block_on(read_held_ring(&store_nest_named(&[USER_KEY], named))).expect("readable ring");
        assert!(
            ring.protected_dids.iter().all(|d| named.contains(&&**d)),
            "the fixture must carry NO directory-derived binding, or it proves the wrong thing"
        );
        let mut report = SweepReport::default();
        let plan = plan_directory_audit(&status_reply(identity, ""), Some(&ring), &mut report);
        (plan, report)
    }

    /// **Hostile shape 1 of 3 — `identity: None`.** The box mints under its own
    /// key, then simply declines to name the identity. Before the freeze the
    /// floor was empty (nothing ever verified), so this was silent.
    #[test]
    fn a_denied_identity_is_still_audited_against_a_never_verified_ring() {
        let (plan, _) = plan_never_verified(None, &[VICTIM]);

        assert_eq!(
            plan.targets,
            vec![ring_derived(VICTIM)],
            "the frozen claim must survive the nest's silence"
        );
        assert!(
            plan.custody_runs,
            "and feeder #1 must actually run on it — an audited-but-unrun target is the \
             same silence in a different shape"
        );
    }

    /// **Hostile shape 2 of 3 — a real `did:plc:` mislabelled `method: "web"`.**
    /// The finding's leg 2: the sweep gated on the nest's *label* while
    /// `check_custody` gated on the DID's own prefix, so this was audited on the
    /// settings page and skipped by the sweep — the sweep being the one that
    /// runs without the user opening a page.
    ///
    /// Note it needs **no** floor at all: the DID is right there in the nest's
    /// own answer. Item 1 alone closes it.
    #[test]
    fn a_did_plc_mislabelled_as_web_is_audited_anyway() {
        let (plan, report) = plan_never_verified(Some(("web", Some(VICTIM))), &[]);

        assert_eq!(
            plan.targets,
            vec![nest_named(VICTIM)],
            "the DID's own prefix decides auditability, never the box's label for it"
        );
        assert!(plan.custody_runs, "{report:?}");
        assert!(
            !report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::GENESIS_CUSTODY),
            "a mislabelled method must not buy an exemption from the audit: {report:?}"
        );
    }

    /// **Hostile shape 3 of 3 — `did: None`.** The identity is admitted but
    /// unnamed, so the nest's answer yields no target at all; only the frozen
    /// claim does.
    #[test]
    fn an_unnamed_identity_is_still_audited_against_a_never_verified_ring() {
        let (plan, report) = plan_never_verified(Some(("plc", None)), &[VICTIM]);

        assert_eq!(plan.targets, vec![ring_derived(VICTIM)]);
        assert!(plan.custody_runs);
        assert_eq!(
            report.skipped,
            vec![FeederSkip {
                feeder: feeders::HANDLE_BINDING,
                reason: skip_reasons::NO_DID,
            }],
            "feeder #3 still has nothing — its basis is nest testimony, so the floor \
             cannot stand in for it: {report:?}"
        );
    }

    /// The converse of shape 2, and the reason the two resolvers must share one
    /// predicate: a `did:web:` reported as `method: "plc"` has no directory log
    /// to read, and must skip rather than 404 the real directory forever.
    #[test]
    fn a_did_web_mislabelled_as_plc_still_skips() {
        let (plan, report) = plan_never_verified(Some(("plc", Some("did:web:example.com"))), &[]);

        assert!(plan.targets.is_empty(), "nothing to fetch: {plan:?}");
        assert!(!plan.custody_runs);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::GENESIS_CUSTODY && s.reason == skip_reasons::NOT_PLC),
            "and the reason on the record is the DID's method, not the label: {report:?}"
        );
    }

    /// The floor's bounding clause, restated for the second source: it only ever
    /// **adds**. Nothing frozen and nothing published ⇒ the plan is exactly what
    /// the nest alone would have produced.
    #[test]
    fn a_client_that_froze_nothing_plans_exactly_as_before() {
        assert_eq!(plan_never_verified(None, &[]).0, AuditPlan::default());
    }

    /// The floor only ever **adds**. With nothing published on record — a fresh
    /// or not-yet-synced ring — the plan is exactly what the nest alone would
    /// have produced, which is what keeps this from re-introducing the muting
    /// feeder #3 deliberately avoided.
    #[test]
    fn an_unsynced_ring_leaves_the_plan_byte_identical() {
        let with_floor = plan_for(None, "", &ring(&[USER_KEY], &[])).0;
        assert_eq!(with_floor, AuditPlan::default());

        let (nest_only, _) = plan_for(
            Some(("plc", Some(DID))),
            "example.com",
            &ring(&[USER_KEY], &[]),
        );
        assert_eq!(nest_only.targets, vec![nest_named(DID)]);
        assert!(nest_only.custody_runs && nest_only.binding_runs);
    }

    /// The nest-named DID leads, then the ring's — and a DID both name is
    /// audited **once**, as the nest-claimed-live one (a re-mint leaves the
    /// retired DID beside the live one in `published_for_dids`).
    #[test]
    fn the_nest_named_did_leads_and_is_never_audited_twice() {
        let (plan, _) = plan_for(
            Some(("plc", Some(DID))),
            "example.com",
            &ring(&[USER_KEY], &["did:plc:retiredold", DID]),
        );
        assert_eq!(
            plan.targets,
            vec![nest_named(DID), ring_derived("did:plc:retiredold")]
        );
    }

    /// Feeder #3 audits the nest-named identity only. Extending the floor to it
    /// would be no more independent (its `handle_domain` is nest testimony too)
    /// and would turn every past retirement into an unbound-handle alarm.
    #[test]
    fn the_floor_does_not_extend_to_the_handle_binding_feeder() {
        let (plan, _) = plan_for(None, "example.com", &ring(&[USER_KEY], &[VICTIM]));
        assert_eq!(plan.targets, vec![ring_derived(VICTIM)]);
        assert!(
            !plan.binding_runs,
            "a nest that named no identity named no handle to bind either"
        );
    }

    /// A feeder belongs in **one bucket per sweep**. An unreadable keyring is
    /// already feeder #1's *failure* (the arm that means retry), so the plan
    /// must not also record it as nothing-to-check — and with no ring there is
    /// no floor, which is the pre-fix behaviour rather than a new gap.
    #[test]
    fn an_unreadable_keyring_is_a_failure_and_never_also_a_skip() {
        let mut report = SweepReport::default();
        let plan = plan_directory_audit(
            &status_reply(Some(("plc", Some(DID))), "example.com"),
            None,
            &mut report,
        );

        assert!(!plan.custody_runs);
        assert!(
            !report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::GENESIS_CUSTODY),
            "no skip on top of the failure the caller already recorded: {report:?}"
        );
        assert!(
            plan.binding_runs && plan.targets == vec![nest_named(DID)],
            "feeder #3 needs no ring and still runs: {plan:?}"
        );
    }

    /// An empty ring is still feeder #1's own gate: the floor names DIDs, but
    /// with no held key there is nothing to compare a published log against.
    #[test]
    fn the_floor_never_runs_custody_without_a_key_to_compare() {
        let (plan, report) = plan_for(
            Some(("plc", Some(DID))),
            "example.com",
            &ring(&[], &[VICTIM]),
        );
        assert!(!plan.custody_runs);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::GENESIS_CUSTODY
                    && s.reason == skip_reasons::EMPTY_RING),
            "{report:?}"
        );
    }

    /// **The crying-wolf trap the floor would otherwise walk into.**
    /// `published_for_dids` keeps a retired DID forever — the retirement path
    /// burns the key that signed the tombstone — so the floor meets every
    /// identity the user ever retired on every sweep. An OWN-signed tombstoned
    /// log is the ordinary end of that entry's life: silent, and it takes any
    /// standing alarm for that DID down, because a retirement in the public
    /// log IS the condition re-checked and found resolved. (The burn is also
    /// why the attribution can always succeed here: the key that signed is
    /// still in the ring.)
    #[test]
    fn a_retired_ring_held_did_is_silent_and_clears_its_alarm() {
        let signer = fauna_client_atproto::rotation_key::generate_rotation_key(1);
        let alerts = CriticalAlerts::new();
        let key = fauna_client_atproto::genesis_verify::alert_key(VICTIM);
        alerts.post(
            key.clone(),
            fauna_client_atproto::genesis_verify::custody_mismatch_alert_lines(HANDLE),
        );

        let report = custody_feeder_for(
            &own_tombstoned_log(&[signer.pubkey_did_key.as_str()], &signer),
            &store(&[signer.pubkey_did_key.as_str()]),
            &alerts,
            &ring_derived(VICTIM),
        );

        assert!(alerts.active().is_empty(), "{:?}", alerts.active());
        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
        assert!(report.failures.is_empty(), "{report:?}");
    }

    /// RE-TAKEN 2026-08-02 (composing with (c)): the
    /// silence above is earned by the tombstone's own signature, never by the
    /// tombstone's existence. A tombstone the held ring did NOT sign, on a DID
    /// the nest has stopped naming, is precisely the composed attack — the box
    /// destroys the identity with its listed junior key, stops naming the DID,
    /// and lands in what used to be the silent-and-clearing branch. It now
    /// alarms.
    #[test]
    fn a_foreign_signed_tombstone_on_a_ring_held_did_alarms() {
        let alerts = CriticalAlerts::new();
        let report = custody_feeder_for(
            &tombstoned_log(&[USER_KEY]),
            &store(&[USER_KEY]),
            &alerts,
            &ring_derived(VICTIM),
        );

        assert_eq!(alerts.active().len(), 1, "{:?}", alerts.active());
        assert_eq!(
            alerts.active()[0].key,
            fauna_client_atproto::genesis_verify::alert_key(VICTIM)
        );
        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
    }

    /// The other side of that asymmetry, and it must not soften: a DID the nest
    /// claims is the account's **live** identity has no business being
    /// tombstoned. Either the nest is lying or someone destroyed the identity —
    /// both are the alarm this feeder exists for.
    #[test]
    fn a_tombstoned_log_still_alarms_when_the_nest_claims_the_did_is_live() {
        let alerts = CriticalAlerts::new();
        let report = custody_feeder_for(
            &tombstoned_log(&[USER_KEY]),
            &store(&[USER_KEY]),
            &alerts,
            &nest_named(DID),
        );

        assert_eq!(alerts.active().len(), 1, "{:?}", alerts.active());
        assert_eq!(
            alerts.active()[0].key,
            fauna_client_atproto::genesis_verify::alert_key(DID)
        );
        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
    }

    /// A ring-held DID that is *alive and compromised* is exactly what the
    /// floor exists to catch — the retirement silence above must not swallow
    /// it.
    #[test]
    fn a_live_ring_held_did_with_a_box_senior_key_still_alarms() {
        let alerts = CriticalAlerts::new();
        let report = custody_feeder_for(
            &audit_log(&[BOX_KEY]),
            &store(&[USER_KEY]),
            &alerts,
            &ring_derived(VICTIM),
        );

        assert_eq!(alerts.active().len(), 1, "{:?}", alerts.active());
        assert_eq!(
            alerts.active()[0].key,
            fauna_client_atproto::genesis_verify::alert_key(VICTIM)
        );
        assert_eq!(report.checked, vec![feeders::GENESIS_CUSTODY]);
    }

    /// End to end over the plan: two targets, one fetch each, one feeder token
    /// in `checked` however many identities it covered — and an unreadable
    /// directory for one target fails *that* target without stopping the next.
    #[test]
    fn the_executor_audits_every_target_and_one_failure_never_skips_the_rest() {
        let alerts = CriticalAlerts::new();
        let plan = AuditPlan {
            targets: vec![ring_derived("did:plc:unreadable"), ring_derived(VICTIM)],
            custody_runs: true,
            binding_runs: false,
            expected_domain: String::new(),
        };
        let mut report = SweepReport::default();
        let custody = store(&[USER_KEY]);
        let fetched = Mutex::new(Vec::new());

        block_on(execute_audit_plan(
            &plan,
            &custody,
            &alerts,
            &mut report,
            |did| {
                fetched.lock().unwrap().push(did.clone());
                async move {
                    if did == VICTIM {
                        Ok(audit_log(&[BOX_KEY]))
                    } else {
                        Err(fauna_client_atproto::genesis_verify::VerifyFailure::Fetch(
                            "directory said no".into(),
                        ))
                    }
                }
            },
        ));

        assert_eq!(
            *fetched.lock().unwrap(),
            vec!["did:plc:unreadable".to_string(), VICTIM.to_string()],
            "every target gets its own read"
        );
        assert_eq!(
            report.checked,
            vec![feeders::GENESIS_CUSTODY],
            "one feeder token however many DIDs it covered: {report:?}"
        );
        assert_eq!(report.failures.len(), 1, "{report:?}");
        assert!(
            report.failures[0].error.starts_with("did:plc:unreadable:"),
            "a per-target failure names its target: {report:?}"
        );
        assert_eq!(
            alerts.active().len(),
            1,
            "the reachable target still alarmed"
        );
    }

    // ── The re-sweep loop ────────────────────────────────────────────────────
    //
    // Every test here drives `run_alert_sweep_loop_with`'s injected wait, so
    // none of them sleeps or reads a clock: they assert *how many sweeps ran*
    // and *what the registry holds*, both latency-independent
    // (`testing.md` § point 14).

    /// A counted wait: yields `true` `n` times, then `false` to end the loop.
    fn wake_times(n: u32) -> impl FnMut() -> core::future::Ready<bool> {
        let mut left = n;
        move || {
            let more = left > 0;
            left = left.saturating_sub(1);
            core::future::ready(more)
        }
    }

    /// The loop's whole point: the *first* sweep is the session-start sweep, and
    /// each wake runs another. A session-start-only client got exactly one.
    #[test]
    fn the_loop_sweeps_once_at_start_and_once_per_wake() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None),
            &store(&[]),
            &alerts,
            &id,
            wake_times(3),
        ));

        assert_eq!(sweeps, 4, "one at start plus one per wake");
    }

    /// The condition arising *mid-session* is the gap this loop closes: nothing
    /// pends at session start, a window opens later, and the banner comes up
    /// without the user restarting the app.
    #[test]
    fn a_window_that_opens_mid_session_becomes_loud_without_a_restart() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);
        // Answers "nothing pending" first, then "a window is open" — the state
        // change a long-lived session must not miss.
        let rpc = FakeRpc::answering(None);
        let state = Arc::clone(&rpc.inner);

        let sweeps = block_on(run_alert_sweep_loop_with(
            rpc.clone(),
            &store(&[]),
            &alerts,
            &id,
            {
                let mut woke = false;
                move || {
                    let first = !woke;
                    woke = true;
                    if first {
                        *state.pending.lock().unwrap() = Some(Some(window(90_000)));
                    }
                    core::future::ready(first)
                }
            },
        ));

        assert_eq!(sweeps, 2);
        let active = alerts.active();
        assert_eq!(
            active.len(),
            1,
            "the second sweep must raise the window the first did not see"
        );
        assert_eq!(active[0].key, fauna_client_recovery::alerts::alert_key(&id));
    }

    /// Sign-out / account switch / factory reset all call `clear_all`, and that
    /// is the only stop signal the loop needs — no per-app liveness plumbing.
    ///
    /// The wait is bounded so deleting the teardown check *fails* this test
    /// (3 sweeps) rather than hanging it.
    #[test]
    fn a_teardown_stops_the_loop_before_it_sweeps_again() {
        let alerts = Arc::new(CriticalAlerts::new());
        let id = actor(7);

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &id,
            {
                let alerts = Arc::clone(&alerts);
                let mut wake = wake_times(2);
                move || {
                    alerts.clear_all();
                    wake()
                }
            },
        ));

        assert_eq!(
            sweeps, 1,
            "the wake after a teardown must not sweep for the departed identity"
        );
        assert!(alerts.active().is_empty());
    }

    /// The teardown check must survive a teardown that cleared *nothing* — an
    /// account with no active alerts still signs out, and the epoch bump is
    /// deliberately unconditional for exactly this case.
    #[test]
    fn a_teardown_with_no_active_alerts_still_stops_the_loop() {
        let alerts = Arc::new(CriticalAlerts::new());
        let id = actor(7);

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None), // nothing pending ⇒ nothing to clear
            &store(&[]),
            &alerts,
            &id,
            {
                let alerts = Arc::clone(&alerts);
                let mut wake = wake_times(2);
                move || {
                    assert!(alerts.active().is_empty());
                    alerts.clear_all();
                    wake()
                }
            },
        ));

        assert_eq!(sweeps, 1);
    }

    /// A custody door modelling a seat whose account runtime assembles after
    /// sign-in: every read is refused as the plane door refuses one, until
    /// the readiness edge fires — which, here, is the runtime assembling.
    /// `edges` bounds how many times the edge fires at all; past that it
    /// never resolves, as a door with no runtime to wait for.
    struct AssemblingStore {
        assembled: Mutex<bool>,
        /// Refuse reads even once assembled — a store fault on a readable door.
        faulty: bool,
        edges: Mutex<u32>,
        ring_reads: Mutex<u32>,
    }

    impl AssemblingStore {
        fn new(faulty: bool, edges: u32) -> Arc<Self> {
            Arc::new(Self {
                assembled: Mutex::new(false),
                faulty,
                edges: Mutex::new(edges),
                ring_reads: Mutex::new(0),
            })
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl AtprotoIdentityStore for AssemblingStore {
        async fn atproto_identity(
            &self,
        ) -> Result<fauna_core::data::AtprotoIdentityConfig, String> {
            if self.faulty || !*self.assembled.lock().unwrap() {
                return Err("the account runtime is not running".into());
            }
            *self.ring_reads.lock().unwrap() += 1;
            Ok(Default::default())
        }
        async fn merge_atproto_identity(
            &self,
            replica: fauna_core::data::AtprotoIdentityConfig,
        ) -> Result<fauna_core::data::AtprotoIdentityConfig, String> {
            Ok(replica)
        }
        async fn until_readable(&self) {
            let fire = {
                let mut left = self.edges.lock().unwrap();
                let fire = *left > 0;
                *left = left.saturating_sub(1);
                fire
            };
            if fire {
                *self.assembled.lock().unwrap() = true;
            } else {
                core::future::pending::<()>().await;
            }
        }
    }

    /// The ordinary clock, which in these tests ends the loop — after one
    /// yield, so whichever of the edge and the clock the loop polls first,
    /// an edge that is there wins.
    fn clock_ends_the_loop()
    -> impl FnMut() -> core::pin::Pin<Box<dyn core::future::Future<Output = bool>>> {
        || {
            Box::pin(async {
                tokio::task::yield_now().await;
                false
            })
        }
    }

    /// The regression the plane cut opened: the first pass of a fresh sign-in
    /// runs before the account runtime assembles, so feeder #1's ring read is
    /// refused — and nothing re-ran it until the 6 h re-sweep. The pass must
    /// re-run at the door's readiness edge, reading the ring then.
    #[test]
    fn a_ring_refused_before_the_runtime_assembles_is_read_at_the_readable_edge() {
        let alerts = CriticalAlerts::new();
        let door = AssemblingStore::new(false, 1);
        let custody: Arc<dyn AtprotoIdentityStore> = door.clone();

        let first = block_on(run_session_start_sweep_at(
            FakeRpc::answering(None),
            &custody,
            &alerts,
            &actor(7),
            0,
        ));
        assert!(first.ring_unread, "the precondition: the door refused");

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None),
            &custody,
            &alerts,
            &actor(7),
            clock_ends_the_loop(),
        ));

        assert_eq!(sweeps, 2, "one refused pass, one at the readable edge");
        assert_eq!(
            *door.ring_reads.lock().unwrap(),
            1,
            "the pass at the edge read the ring — feeder #1 ran"
        );
    }

    /// Once per streak: a door that is readable yet keeps failing (a store
    /// fault) gets ONE re-run at the edge, then the ordinary clock — never a
    /// loop spinning on an edge that is always there.
    #[test]
    fn a_door_readable_but_failing_is_re_run_once_per_streak() {
        let alerts = CriticalAlerts::new();
        let door = AssemblingStore::new(true, 3);
        let custody: Arc<dyn AtprotoIdentityStore> = door.clone();

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None),
            &custody,
            &alerts,
            &actor(7),
            clock_ends_the_loop(),
        ));

        assert_eq!(sweeps, 2, "the edge re-ran the pass once, then the clock");
    }

    /// A pass that reads the ring takes no edge: the ordinary deployment's
    /// loop is the clock alone, whatever the door could answer.
    #[test]
    fn a_ring_read_takes_the_clock_not_the_edge() {
        let alerts = CriticalAlerts::new();
        let door = AssemblingStore::new(false, 1);
        *door.assembled.lock().unwrap() = true;
        let custody: Arc<dyn AtprotoIdentityStore> = door.clone();

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None),
            &custody,
            &alerts,
            &actor(7),
            clock_ends_the_loop(),
        ));

        assert_eq!(sweeps, 1);
        assert_eq!(*door.edges.lock().unwrap(), 1, "the edge was never awaited");
    }

    /// The e2e seam's contract: a wake ends ONE wait early and the loop sweeps
    /// again, running the production loop body — a condition that arose since
    /// the last pass is raised, and the teardown stop still ends the loop.
    ///
    /// Bounded both ways: the second wake tears the identity down, so a loop
    /// that ignored the stop would call the wake a third time (`unreachable!`),
    /// and one that ignored the wake would poll the production clock, which
    /// this runtime has no timer for.
    #[test]
    fn a_wake_ends_the_wait_and_the_loop_sweeps_again() {
        let alerts = Arc::new(CriticalAlerts::new());
        let id = actor(7);
        let rpc = FakeRpc::answering(None);
        let state = Arc::clone(&rpc.inner);

        let sweeps = block_on(run_alert_sweep_loop_wakeable(
            rpc.clone(),
            &store(&[]),
            &alerts,
            &id,
            {
                let alerts = Arc::clone(&alerts);
                let mut wakes = 0u32;
                move || {
                    wakes += 1;
                    match wakes {
                        // The condition arises mid-session, after the first pass.
                        1 => *state.pending.lock().unwrap() = Some(Some(window(90_000))),
                        2 => {
                            assert_eq!(
                                alerts.active().len(),
                                1,
                                "the woken pass must raise the window the first pass did not see"
                            );
                            alerts.clear_all();
                        }
                        n => unreachable!("wake {n}: the loop outlived its identity's teardown"),
                    }
                    core::future::ready(())
                }
            },
        ));

        assert_eq!(
            sweeps, 2,
            "one at start plus one per wake, until the teardown"
        );
    }

    // ── The pass counters: the negative-assert barrier ───────────────────────

    /// Records what the sweep's own counters read at the instant a feeder posts
    /// — the only vantage point from which the *ordering* below is observable,
    /// and it uses the production observer seam rather than a test-only hook.
    struct PostTimeCounters {
        alerts: Mutex<Option<Arc<CriticalAlerts>>>,
        seen: Mutex<Vec<(u64, u64)>>,
    }

    impl CriticalAlertsObserver for PostTimeCounters {
        fn on_changed(&self) {
            let guard = self.alerts.lock().unwrap();
            if let Some(alerts) = guard.as_ref() {
                self.seen.lock().unwrap().push((
                    alerts.sweep_passes_started(),
                    alerts.sweep_passes_completed(),
                ));
            }
        }
    }

    /// **The barrier's load-bearing ordering** (`e2e-conventions.md`
    /// § convention 14, mechanism 2): a pass bumps `started` before its first
    /// feeder runs and `completed` only after the last one has posted, so a test
    /// that waits for `completed` to pass the `started` it read at plant time is
    /// reading a registry whose alarm decision has already landed.
    ///
    /// Asserted from *inside* a post, because that is where the claim is
    /// falsifiable: bump `completed` too early — before the feeders — and the
    /// pair read here becomes `(1, 1)`, which is precisely the false-pass a
    /// settle-sleep gives (test reads the banner before the alarm arrives).
    #[test]
    fn a_pass_counts_its_start_before_its_feeders_and_its_completion_after() {
        let alerts = Arc::new(CriticalAlerts::new());
        let id = actor(7);
        let probe = Arc::new(PostTimeCounters {
            alerts: Mutex::new(Some(Arc::clone(&alerts))),
            seen: Mutex::new(Vec::new()),
        });
        alerts.subscribe(Arc::clone(&probe) as Arc<dyn CriticalAlertsObserver>);

        assert_eq!(
            (
                alerts.sweep_passes_started(),
                alerts.sweep_passes_completed()
            ),
            (0, 0),
            "a registry no sweep has touched has run no passes"
        );

        // A pending replacement window: a feeder that actually posts, so the
        // observer above fires inside the pass.
        block_on(run_session_start_sweep(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &id,
        ));

        let seen = probe.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![(1, 0)],
            "the alarm posts inside a pass that has started and NOT yet completed"
        );
        assert_eq!(
            (
                alerts.sweep_passes_started(),
                alerts.sweep_passes_completed()
            ),
            (1, 1),
            "and the pass counts itself finished once its feeders are done"
        );
    }

    /// A subscribed observer that dies must not be able to end a sweep pass
    /// between its feeders.
    /// `CriticalAlerts::notify` contains the panic, so the pass runs its
    /// remaining feeders, the alarm stands, and the completion bump — the
    /// convention-14 barrier — still releases.
    #[test]
    fn a_panicking_observer_cannot_wedge_a_pass_mid_flight() {
        struct Panicking;
        impl CriticalAlertsObserver for Panicking {
            fn on_changed(&self) {
                panic!("observer bug (deliberate — this test pins containment)");
            }
        }

        let alerts = CriticalAlerts::new();
        alerts.subscribe(Arc::new(Panicking));
        let id = actor(7);

        // A pending replacement window: the first feeder posts, so the
        // panicking observer fires inside the pass with feeders still queued
        // behind it — the exact mid-flight shape.
        block_on(run_session_start_sweep(
            FakeRpc::answering(Some(window(90_000))),
            &store(&[]),
            &alerts,
            &id,
        ));

        assert_eq!(
            (
                alerts.sweep_passes_started(),
                alerts.sweep_passes_completed()
            ),
            (1, 1),
            "the pass must run its remaining feeders and count itself finished \
             despite the observer dying inside the first feeder's post"
        );
        assert_eq!(
            alerts.active().len(),
            1,
            "and the alarm the observer died on still stands in the registry"
        );
        assert_eq!(
            alerts.observer_faults(),
            1,
            "the panicking hand-off is COUNTED, not silently discarded — one \
             notify() this pass (the pending-replacement post), one fault; \
             without this counter the pass is indistinguishable from a clean \
             one by anything readable"
        );
    }

    /// The counter measures *passes*, not sessions — so an e2e barrier keeps
    /// working across the re-sweeps a long-lived session runs.
    #[test]
    fn every_pass_of_the_loop_counts_itself() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::answering(None),
            &store(&[]),
            &alerts,
            &id,
            wake_times(3),
        ));

        assert_eq!(
            (
                u32::try_from(alerts.sweep_passes_started()).unwrap(),
                u32::try_from(alerts.sweep_passes_completed()).unwrap()
            ),
            (sweeps, sweeps),
            "every pass the loop ran counted its start and its completion"
        );
    }

    /// A pass whose feeders all fail still counts — the barrier must release on
    /// an unreachable nest too, or a test waiting on it hangs for its whole
    /// budget and reds as a timeout instead of asserting what it came to assert.
    #[test]
    fn an_unreachable_pass_still_counts_itself_finished() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        block_on(run_session_start_sweep(
            FakeRpc::unreachable(),
            &store(&[]),
            &alerts,
            &id,
        ));

        assert_eq!(
            (
                alerts.sweep_passes_started(),
                alerts.sweep_passes_completed()
            ),
            (1, 1),
            "a failed read is still a completed pass"
        );
    }

    /// An unreachable nest must not end the loop — a session that loses its
    /// network for an afternoon has to resume checking, not go quiet for good.
    #[test]
    fn an_unreachable_sweep_does_not_end_the_loop() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        let sweeps = block_on(run_alert_sweep_loop_with(
            FakeRpc::unreachable(),
            &store(&[]),
            &alerts,
            &id,
            wake_times(2),
        ));

        assert_eq!(sweeps, 3, "failures are retried, never fatal");
    }

    // ── Feeder #4: the domain-expiry watch ───────────────────────────────────

    use fauna_protocol::domain_expiry::{
        DomainExpiryRecord, outcomes as wire_outcomes, skip_reasons as wire_skips,
    };

    const DAY: i64 = 24 * 60 * 60;
    const NOW: i64 = 1_800_000_000;

    fn expiry_record(
        expires_at: Option<i64>,
        statuses: &[&str],
        outcome: &str,
        detail: Option<&str>,
    ) -> DomainExpiryRecord {
        DomainExpiryRecord {
            domain: "example.org".into(),
            expires_at,
            statuses: statuses.iter().map(|s| (*s).to_string()).collect(),
            fetched_at: NOW,
            outcome: outcome.into(),
            detail: detail.map(str::to_string),
            extra: Default::default(),
        }
    }

    fn sweep_with_domain(rpc: FakeRpc, alerts: &CriticalAlerts) -> SweepReport {
        block_on(run_session_start_sweep_at(
            rpc,
            &store(&[]),
            alerts,
            &actor(9),
            NOW,
        ))
    }

    /// The alarm is deployment-scoped: one bare key, not one per identity.
    #[test]
    fn a_lapsing_domain_alarms_on_the_bare_deployment_key() {
        let alerts = CriticalAlerts::new();
        let report = sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(
                    Some(NOW + 2 * DAY),
                    &["active"],
                    wire_outcomes::CHECKED,
                    None,
                ),
                true,
            ),
            &alerts,
        );

        assert!(
            report.checked.contains(&feeders::DOMAIN_EXPIRY),
            "{report:?}"
        );
        let active = alerts.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key, DOMAIN_EXPIRY_ALERT_KEY);
        assert_eq!(active[0].key, "domain-expiry", "no actor suffix");
    }

    /// The ratified audience rule, at the only place it is observable: the same
    /// finding must reach an admin and a resident as **different sentences**.
    /// A single line with a role caveat would not translate, and telling a
    /// resident to go renew a domain they cannot reach is a dead end.
    #[test]
    fn admin_and_resident_get_different_lines_for_the_same_finding() {
        let record = expiry_record(
            Some(NOW + 2 * DAY),
            &["active"],
            wire_outcomes::CHECKED,
            None,
        );

        let admin_alerts = CriticalAlerts::new();
        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(record.clone(), true),
            &admin_alerts,
        );
        let resident_alerts = CriticalAlerts::new();
        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(record, false),
            &resident_alerts,
        );

        let admin_line = admin_alerts.active()[0].lines[0].clone();
        let resident_line = resident_alerts.active()[0].lines[0].clone();
        assert_eq!(admin_line.key, "critical_alerts.domain_expiring_admin");
        assert_eq!(
            resident_line.key,
            "critical_alerts.domain_expiring_resident"
        );
        assert_ne!(admin_line.key, resident_line.key);
        // Both carry the same substituted facts — only the sentence differs.
        for line in [&admin_line, &resident_line] {
            assert_eq!(
                line.args.get("domain").map(String::as_str),
                Some("example.org")
            );
            assert_eq!(line.args.get("days").map(String::as_str), Some("2"));
        }
    }

    /// The status arm end-to-end: a future-dated registration in redemption is
    /// the case the whole second arm exists for, and it must reach the banner
    /// with the status named.
    #[test]
    fn a_future_dated_registration_in_redemption_alarms_through_the_sweep() {
        let alerts = CriticalAlerts::new();
        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(
                    Some(NOW + 365 * DAY),
                    &["redemption period"],
                    wire_outcomes::CHECKED,
                    None,
                ),
                true,
            ),
            &alerts,
        );

        let line = alerts.active()[0].lines[0].clone();
        assert_eq!(line.key, "critical_alerts.domain_lapsing_admin");
        assert_eq!(
            line.args.get("status").map(String::as_str),
            Some("redemption period")
        );
    }

    /// A renewed domain must take its banner with it — the only thing that
    /// legitimately clears this non-dismissable alert.
    #[test]
    fn a_renewed_domain_clears_a_standing_alarm() {
        let alerts = CriticalAlerts::new();
        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(Some(NOW + DAY), &["active"], wire_outcomes::CHECKED, None),
                true,
            ),
            &alerts,
        );
        assert_eq!(alerts.active().len(), 1);

        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(
                    Some(NOW + 400 * DAY),
                    &["active"],
                    wire_outcomes::CHECKED,
                    None,
                ),
                true,
            ),
            &alerts,
        );
        assert!(alerts.active().is_empty(), "a renewal clears the banner");
    }

    /// **The fail-safe direction.** The nest could not reach RDAP, so it knows
    /// nothing new — and "nothing new" must never read as "resolved". The
    /// standing alarm survives and the report says which feeder could not check.
    #[test]
    fn an_rdap_failure_leaves_a_standing_alarm_alone() {
        let alerts = CriticalAlerts::new();
        sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(Some(NOW + DAY), &["active"], wire_outcomes::CHECKED, None),
                true,
            ),
            &alerts,
        );
        assert_eq!(alerts.active().len(), 1);

        let report = sweep_with_domain(
            FakeRpc::answering(None).with_domain_expiry(
                expiry_record(None, &[], wire_outcomes::FAILED, Some("503 from rdap")),
                true,
            ),
            &alerts,
        );

        assert_eq!(
            alerts.active().len(),
            1,
            "unreachable is not resolved — the alarm must stand"
        );
        let failed: Vec<_> = report.failures.iter().map(|f| f.feeder).collect();
        assert!(failed.contains(&feeders::DOMAIN_EXPIRY), "{report:?}");
        assert!(!report.checked.contains(&feeders::DOMAIN_EXPIRY));
    }

    /// A skip is silent on the banner and never silent in the report — and, like
    /// a failure, it does not clear: a box that just went domainless has not
    /// told us the old name is safe.
    #[test]
    fn skips_are_recorded_with_their_reason_and_never_clear() {
        for (detail, expected) in [
            (
                wire_skips::NO_PRIMARY_DOMAIN,
                skip_reasons::NO_PRIMARY_DOMAIN,
            ),
            (wire_skips::UNSERVED_TLD, skip_reasons::RDAP_UNSERVED_TLD),
            // A newer nest naming a reason this build has never heard of must
            // still surface AS A SKIP rather than vanishing into a silence
            // indistinguishable from health.
            ("some-future-reason", skip_reasons::UNKNOWN_NEST_SKIP),
        ] {
            let alerts = CriticalAlerts::new();
            sweep_with_domain(
                FakeRpc::answering(None).with_domain_expiry(
                    expiry_record(Some(NOW + DAY), &["active"], wire_outcomes::CHECKED, None),
                    true,
                ),
                &alerts,
            );
            assert_eq!(alerts.active().len(), 1);

            let report = sweep_with_domain(
                FakeRpc::answering(None).with_domain_expiry(
                    expiry_record(None, &[], wire_outcomes::SKIPPED, Some(detail)),
                    true,
                ),
                &alerts,
            );

            assert!(
                report.skipped.contains(&FeederSkip {
                    feeder: feeders::DOMAIN_EXPIRY,
                    reason: expected
                }),
                "{detail}: {report:?}"
            );
            assert_eq!(alerts.active().len(), 1, "{detail}: a skip must not clear");
        }
    }

    /// A nest whose watch has not run yet is a skip, not health and not a
    /// failure — the box is minutes old and simply knows nothing.
    #[test]
    fn a_watch_that_has_not_run_yet_is_a_recorded_skip() {
        let alerts = CriticalAlerts::new();
        // The fake's default reply is `record: None`.
        let report = sweep_with_domain(FakeRpc::answering(None), &alerts);

        assert!(
            report.skipped.contains(&FeederSkip {
                feeder: feeders::DOMAIN_EXPIRY,
                reason: skip_reasons::WATCH_NOT_YET_RUN
            }),
            "{report:?}"
        );
        assert!(alerts.active().is_empty());
    }

    /// A nest that answers `unknown_kind` is reported as a feeder failure like
    /// any other refusal — the "older nest ⇒ skip" arm left with the
    /// compat-remnant sweep (`version-compatibility.md` § Dimension 2). The
    /// OTHER feeders are untouched.
    #[test]
    fn an_unknown_kind_refusal_is_a_failure_not_a_skip() {
        let alerts = CriticalAlerts::new();
        let report = sweep_with_domain(
            FakeRpc::answering(None)
                .rejecting_kind("fauna.domain.expiry.get", "fauna.protocol.unknown_kind"),
            &alerts,
        );

        assert!(
            report
                .failures
                .iter()
                .any(|f| f.feeder == feeders::DOMAIN_EXPIRY),
            "{report:?}"
        );
        assert!(alerts.active().is_empty());
        assert!(
            report.checked.contains(&feeders::PENDING_REPLACEMENT),
            "{report:?}"
        );
    }

    /// The code, not the message, is what decides — and any *other* rejection
    /// stays a failure. A nest that refuses the read for a real reason has not
    /// told us the domain is fine.
    #[test]
    fn any_other_rejection_is_still_a_failure() {
        let alerts = CriticalAlerts::new();
        let report = sweep_with_domain(
            FakeRpc::answering(None)
                .rejecting_kind("fauna.domain.expiry.get", "fauna.domain.permission_denied"),
            &alerts,
        );

        assert!(
            report
                .failures
                .iter()
                .any(|f| f.feeder == feeders::DOMAIN_EXPIRY),
            "{report:?}"
        );
        assert!(
            !report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::DOMAIN_EXPIRY),
            "{report:?}"
        );
    }

    /// The every-feeder-runs-independently contract, from the newest feeder's
    /// side: an unreachable nest fails this one too rather than skipping it.
    #[test]
    fn an_unreachable_nest_fails_the_domain_feeder_rather_than_skipping_it() {
        let alerts = CriticalAlerts::new();
        let report = sweep_with_domain(FakeRpc::unreachable(), &alerts);

        let failed: Vec<_> = report.failures.iter().map(|f| f.feeder).collect();
        assert!(failed.contains(&feeders::DOMAIN_EXPIRY), "{report:?}");
        assert!(
            !report
                .skipped
                .iter()
                .any(|s| s.feeder == feeders::DOMAIN_EXPIRY),
            "unreachable is a failure, never a skip — only a failure retries"
        );
    }
}
