//! The post-claim **serving-enablement** step — the one place every app's
//! authed post-onboarding launch glue applies the four machine-derived
//! enablement intents (`docs/goal/behavior/onboarding.md` § 3b *Mechanism*).
//!
//! The intents themselves are derived in `fauna-onboarding-machine`
//! (`email_enable_requested` / `caldav_enable_requested` /
//! `carddav_enable_requested` / `webdav_enable_requested`, claim-gated per § 3b
//! *Gating rule*). What used to be hand-written per app was the **firing**:
//! each app spawned up to six fire-and-forget dispatches with no completion
//! marker, so nothing outside the process could tell "the glue decided OFF and
//! is finished" from "the glue has not run yet". This module owns both halves:
//!
//! - [`plan`] — the ordered step list the four intents imply, including the
//!   one-MSEK-mint-path gate (`mail-credentials.md` § Trigger taxonomy, *The
//!   calendar-only credential*): the DAV mailbox mint fires only with a DAV
//!   intent on and email off, so exactly one mint path writes the mail
//!   custody at first setup.
//! - [`apply_serving_enablement`] — runs the plan until it completes, one step
//!   after another, each waiting for a live connection and tried again with
//!   backoff on a passing failure ([`StepOutcome`]; `onboarding.md` § 3b *The
//!   step completes; it is not fired once*), returns the refusals for good for
//!   the page's `error-message`, and records each run against its actor —
//!   `{actor_id, decided, completed, outcome}` — which
//!   [`serving_enablement_json`] renders for
//!   `fauna_e2e_agent::SERVING_ENABLEMENT_KEY` (convention 14's causal anchor —
//!   `docs/goal/architecture/e2e-latency-independent-assertions.md`).
//! - [`RpcServingEnablement`] — the production executor, generic over the
//!   transport so native (`rpc_glue::dispatch_post_claim_serving_enablement`)
//!   and wasm (`fauna-wasm`) run the identical bodies.
//!
//! The mail-provision step runs on EVERY `LoggedIn` handoff, whatever the
//! intents: on a non-admin (invite-redeem) first setup it is the new-user
//! mailbox auto-mint (`mail-credentials.md` § Auto-enable for new users), which
//! the email intent does not gate — on a deployment serving CalDAV or CardDAV
//! with mail off it mints the read recipe. A sign-in reaches the step too, and there it
//! is a no-op for an admin (email intent OFF by the claim gate) and an
//! idempotent no-op for a user who already has a mailbox.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};
use serde::Serialize;

use crate::bridge_approval::{BridgeApprovalAction, BridgeApprovalMachine};
use crate::machine::MailSettingsMachine;

/// The four claim-time enablement intents, read off the onboarding machine
/// *before* the wizard is torn down (every app's capture-before-adopt shape).
/// All four `false` is a real, complete decision — a sign-in, an invite
/// redemption, or a claim on a local handle / private-NAT box.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ServingEnablementIntents {
    pub email: bool,
    pub caldav: bool,
    pub carddav: bool,
    pub webdav: bool,
}

/// One dispatch the post-claim glue performs. Each is idempotent with the
/// matching settings-page path, so a step tried again after a passing failure
/// never doubles anything (`onboarding.md` § 3b *The step completes; it is not
/// fired once*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingEnablementStep {
    /// The first-setup mail provision: on the admin claim, mint the admin's
    /// mailbox and flip deployment mail on iff `admin_enable_email`; on a
    /// non-admin first setup, the policy-gated new-user auto-mint.
    ProvisionMail { admin_enable_email: bool },
    /// `fauna.bridges.set_caldav_enabled(true)`.
    SetCalDavEnabled,
    /// `fauna.bridges.set_carddav_enabled(true)`.
    SetCardDavEnabled,
    /// The admin's calendar-only credential: the DAV mailbox mint (a DAV
    /// intent on, email off).
    ProvisionDavMailbox,
    /// `fauna.bridges.set_webdav_enabled(true)`. WebDAV has no per-actor
    /// mailbox, so there is no companion mint.
    SetWebDavEnabled,
}

impl ServingEnablementStep {
    /// The step's stable name in [`serving_enablement_json`]'s `refused` list.
    pub fn name(self) -> &'static str {
        match self {
            Self::ProvisionMail { .. } => "provision_mail",
            Self::SetCalDavEnabled => "set_caldav_enabled",
            Self::SetCardDavEnabled => "set_carddav_enabled",
            Self::ProvisionDavMailbox => "provision_dav_mailbox",
            Self::SetWebDavEnabled => "set_webdav_enabled",
        }
    }
}

/// The ordered steps `intents` imply. Pure; the single home of the ordering and
/// of the one-MSEK-mint-path gates.
pub fn plan(intents: ServingEnablementIntents) -> Vec<ServingEnablementStep> {
    use ServingEnablementStep::*;
    let mut steps = vec![ProvisionMail {
        admin_enable_email: intents.email,
    }];
    if intents.caldav {
        steps.push(SetCalDavEnabled);
    }
    if intents.carddav {
        steps.push(SetCardDavEnabled);
    }
    if (intents.caldav || intents.carddav) && !intents.email {
        steps.push(ProvisionDavMailbox);
    }
    if intents.webdav {
        steps.push(SetWebDavEnabled);
    }
    steps
}

/// How one attempt at a [`ServingEnablementStep`] ended — classified from the
/// error TYPES the step met, never from their message text
/// ([`StepOutcome::of_dispatch`], [`StepOutcome::of_transport`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step did what it was for (an idempotent no-op included).
    Done,
    /// Refused for a passing reason — the account store not ready yet, the
    /// connection lost, a request timeout: tried again after a backoff. The
    /// text is for the log only.
    TryAgain(String),
    /// Refused for good — the sentence the page's `error-message` shows.
    RefusedForGood(String),
}

impl StepOutcome {
    /// Classify a mail-settings dispatch failure: the account store's typed
    /// not-ready refusal ([`crate::error::StoreError::is_not_ready`]) and the
    /// nest seam's `Transient` class (a dropped connection, a timeout, a rate
    /// limit, a 5xx) pass; everything else is final. A nest refusal is said as
    /// the nest's own sentence, bare (the way the settings pages say it).
    pub fn of_dispatch(e: crate::error::DispatchError) -> Self {
        use crate::error::{DispatchError, NestError};
        match e {
            DispatchError::Store(s) if s.is_not_ready() => Self::TryAgain(s.to_string()),
            DispatchError::Nest(NestError::Transient(m)) => Self::TryAgain(m),
            DispatchError::Nest(NestError::Rejected(sentence)) => Self::RefusedForGood(sentence),
            other => Self::RefusedForGood(other.to_string()),
        }
    }

    /// Classify a bare transport failure through the shared two-class
    /// classifier ([`crate::error::nest_error`]).
    pub fn of_transport<E>(e: E) -> Self
    where
        E: fauna_protocol::RpcErrorClass + core::fmt::Display,
    {
        Self::of_dispatch(crate::error::nest_error(e).into())
    }
}

/// A step the nest refused for good, as the run returns and records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepRefusal {
    pub step: &'static str,
    pub message: String,
}

/// What [`apply_serving_enablement`] hands back to the app's glue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServingEnablementReport {
    /// The steps refused for good, in plan order. Empty on success.
    pub refusals: Vec<StepRefusal>,
    /// The session ended (signed out, its connection stopped for good) before
    /// every step was answered; the run stays not-completed and the
    /// remaining enables are left to the settings pages.
    pub abandoned: bool,
}

impl ServingEnablementReport {
    /// The text the page's `error-message` shows, or `None` when nothing was
    /// refused for good. Each refusal is the nest's own sentence.
    pub fn error_message(&self) -> Option<String> {
        if self.refusals.is_empty() {
            return None;
        }
        Some(
            self.refusals
                .iter()
                .map(|r| r.message.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

/// The first wait after a passing failure; each further try doubles it, up to
/// [`STEP_BACKOFF_MAX`].
pub const STEP_BACKOFF_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
/// The longest wait between two tries of one step.
pub const STEP_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// The wait before try `attempt + 2` of a step (`attempt` counts the passing
/// failures so far, from 0).
pub fn step_backoff(attempt: u32) -> std::time::Duration {
    STEP_BACKOFF_FIRST
        .saturating_mul(1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX))
        .min(STEP_BACKOFF_MAX)
}

/// Performs one [`ServingEnablementStep`] and says how it ended, and waits for
/// the step's precondition — a live nest connection.
#[allow(async_fn_in_trait)] // static dispatch only; Send-ness leaks per concrete impl
pub trait ServingEnablementExecutor {
    /// Resolve once the nest connection is live; `false` once it can never be
    /// (the session signed out, its connection stopped for good).
    async fn wait_until_live(&self) -> bool;
    /// Run one attempt at `step`.
    async fn run(&self, step: ServingEnablementStep) -> StepOutcome;
    /// Wait out a backoff. The production default sleeps; a scripted test
    /// executor returns at once.
    async fn back_off(&self, delay: std::time::Duration) {
        fauna_sleep::sleep(delay).await;
    }
}

/// The precondition half of the production executor — a transport's own
/// connection-state signal (`rpc_glue`'s native and wasm builders).
#[allow(async_fn_in_trait)] // static dispatch only
pub trait ConnectionLive {
    /// See [`ServingEnablementExecutor::wait_until_live`].
    async fn wait_until_live(&self) -> bool;
}

/// Run the post-claim serving enablement until it completes, for the actor
/// `actor_id_hex` (the identity whose `LoggedIn` handoff this is) —
/// `onboarding.md` § 3b *The step completes; it is not fired once*.
///
/// Each planned step, in order, waits for a live connection and is tried until
/// it is [`StepOutcome::Done`] or [`StepOutcome::RefusedForGood`]; a
/// [`StepOutcome::TryAgain`] waits [`step_backoff`] and tries the same step
/// again, the later steps waiting behind it. A refusal for good ends that step
/// only — the four protocols gate independently — and is returned so the app
/// says it on its page's `error-message`.
///
/// The run is recorded — `{actor_id, decided, completed, outcome}` — at the
/// first statement, and marked `completed` only once every step has succeeded
/// or been refused for good, **including the decide-nothing case**, whose plan
/// still carries the mail-provision step. That is what gives a "stays OFF" read
/// its causal anchor, and an "is ON" read its: once this actor's run is
/// `completed` with `outcome: succeeded`, every enable it planned has landed. A
/// run that ends any other way (the session gone, a step that panicked, the
/// future dropped) reads `outcome: abandoned` and is never `completed`. Nothing
/// is persisted: an app that dies mid-run leaves the enables to the settings
/// pages.
pub async fn apply_serving_enablement<E: ServingEnablementExecutor>(
    actor_id_hex: String,
    intents: ServingEnablementIntents,
    executor: &E,
) -> ServingEnablementReport {
    let run = runs::begin(actor_id_hex, intents);
    let mut report = ServingEnablementReport::default();
    for step in plan(intents) {
        let mut attempt = 0u32;
        loop {
            if !executor.wait_until_live().await {
                tracing::warn!(
                    "post-claim serving enablement: the session ended before {} was \
                     answered; the remaining enables are left to the settings pages",
                    step.name()
                );
                report.abandoned = true;
                return report;
            }
            match executor.run(step).await {
                StepOutcome::Done => break,
                StepOutcome::RefusedForGood(message) => {
                    tracing::error!(
                        "post-claim serving enablement: {} refused: {message}",
                        step.name()
                    );
                    report.refusals.push(StepRefusal {
                        step: step.name(),
                        message,
                    });
                    break;
                }
                StepOutcome::TryAgain(why) => {
                    let delay = step_backoff(attempt);
                    tracing::warn!(
                        "post-claim serving enablement: {} not done yet ({why}); \
                         trying again in {delay:?}",
                        step.name()
                    );
                    executor.back_off(delay).await;
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }
    run.finish(&report.refusals);
    report
}

/// Build `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`'s value —
/// `{"started": N, "completed": M, "runs": [{"actor_id", "decided": {"email",
/// "caldav", "carddav", "webdav"}, "completed", "outcome", "refused": [{"step",
/// "message"}]}]}`, `runs` in start order for this process. `outcome` is
/// `running` (a step still being tried), `succeeded`, `refused` (every step
/// answered, at least one refused for good — `refused` lists them) or
/// `abandoned` (the session ended first); `completed` is `true` exactly for
/// `succeeded` and `refused`. A consumer finds ITS run by the actor it
/// onboarded, so the answer needs no baseline and survives the app relaunching
/// before the handoff. The empty shape is the legitimate "the glue has not run
/// in this process"; an app without the leg publishes nothing.
pub fn serving_enablement_json() -> serde_json::Value {
    let (started, completed, records) = runs::read();
    let runs: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "actor_id": r.actor_id,
                "decided": r.decided,
                "completed": r.outcome.completed(),
                "outcome": r.outcome.label(),
                "refused": r.outcome.refusals(),
            })
        })
        .collect();
    serde_json::json!({
        "started": started,
        "completed": completed,
        "runs": runs,
    })
}

/// [`serving_enablement_json`] as JSON text — the face for the apps whose state
/// builder is not Rust (web via `fauna-wasm`; the FFI apps at their trickle-down).
pub fn serving_enablement_json_text() -> String {
    serving_enablement_json().to_string()
}

mod runs {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) enum RunOutcome {
        Running,
        Succeeded,
        Refused(Vec<StepRefusal>),
        Abandoned,
    }

    impl RunOutcome {
        pub(super) fn completed(&self) -> bool {
            matches!(self, Self::Succeeded | Self::Refused(_))
        }
        pub(super) fn label(&self) -> &'static str {
            match self {
                Self::Running => "running",
                Self::Succeeded => "succeeded",
                Self::Refused(_) => "refused",
                Self::Abandoned => "abandoned",
            }
        }
        pub(super) fn refusals(&self) -> &[StepRefusal] {
            match self {
                Self::Refused(r) => r,
                _ => &[],
            }
        }
    }

    #[derive(Clone)]
    pub(super) struct RunRecord {
        pub(super) actor_id: String,
        pub(super) decided: ServingEnablementIntents,
        pub(super) outcome: RunOutcome,
    }

    static STARTED: AtomicU64 = AtomicU64::new(0);
    static COMPLETED: AtomicU64 = AtomicU64::new(0);
    // One entry per `LoggedIn` handoff this process ran — a handful at most
    // (an add-account or a re-onboarding appends one), so never pruned.
    static RUNS: Mutex<Vec<RunRecord>> = Mutex::new(Vec::new());

    /// The run's record handle. [`Self::finish`] completes it; dropped any
    /// other way (the session ended, a step panicked, the future was dropped)
    /// it marks the run `abandoned`, never `completed`.
    pub(super) struct RunGuard(usize);

    impl RunGuard {
        pub(super) fn finish(self, refusals: &[StepRefusal]) {
            let outcome = if refusals.is_empty() {
                RunOutcome::Succeeded
            } else {
                RunOutcome::Refused(refusals.to_vec())
            };
            set(self.0, outcome);
            COMPLETED.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for RunGuard {
        fn drop(&mut self) {
            let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = runs.get_mut(self.0)
                && r.outcome == RunOutcome::Running
            {
                r.outcome = RunOutcome::Abandoned;
            }
        }
    }

    fn set(index: usize, outcome: RunOutcome) {
        if let Some(r) = RUNS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(index)
        {
            r.outcome = outcome;
        }
    }

    pub(super) fn begin(actor_id: String, decided: ServingEnablementIntents) -> RunGuard {
        let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        runs.push(RunRecord {
            actor_id,
            decided,
            outcome: RunOutcome::Running,
        });
        STARTED.fetch_add(1, Ordering::SeqCst);
        RunGuard(runs.len() - 1)
    }

    pub(super) fn read() -> (u64, u64, Vec<RunRecord>) {
        // `completed` first: a reader must never see completed > started.
        let completed = COMPLETED.load(Ordering::SeqCst);
        let started = STARTED.load(Ordering::SeqCst);
        let runs = RUNS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (started, completed, runs)
    }
}

/// Whether the signed-in actor is the nest's admin, or how the read failed.
async fn am_i_admin<R>(nest: R) -> Result<bool, StepOutcome>
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    fauna_client_account::AccountClient::new(nest)
        .am_i_admin()
        .await
        .map(|r| r.admin)
        .map_err(StepOutcome::of_transport)
}

/// The admin's calendar-only credential at claim: on a box serving CalDAV or
/// CardDAV with mail off, mint the admin's shared MSEK + `default` read
/// credential ([`MailSettingsMachine::enable_dav_mailbox_with_generated_password`]).
/// **Admin-only** — every other user's comes from the new-user arm of
/// [`provision_mail_at_first_setup`]. Callers gate the call on being the only
/// mint path live ([`plan`]).
pub async fn provision_dav_mailbox_at_first_setup<R>(
    nest: R,
    machine: MailSettingsMachine,
) -> StepOutcome
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    match am_i_admin(nest).await {
        Ok(true) => {}
        Ok(false) => return StepOutcome::Done,
        Err(outcome) => return outcome,
    }
    match machine
        .enable_dav_mailbox_with_generated_password("Default".to_string())
        .await
    {
        Ok(_password) => {
            tracing::info!(
                "onboarding: calendar-only admin credential auto-minted (shared MSEK; \
                 generated password on the mail-settings page)"
            );
            StepOutcome::Done
        }
        Err(e) => StepOutcome::of_dispatch(e),
    }
}

/// First-authenticated-setup mail provision. First, a custody that already
/// holds a mailbox is restored, never minted over
/// ([`MailSettingsMachine::restore_mailbox_at_claim`]). Otherwise it
/// discriminates on `am_i_admin`:
///
/// - **Admin claim**: iff `admin_enable_email`, mint the admin's own mailbox via
///   [`MailSettingsMachine::enable_mail_with_generated_password`], whose final
///   step fires `set_mail_enabled(true)`.
/// - **New (non-admin) user**: auto-mint the user's own credential iff the
///   deployment policy allows — `fauna.setup.status`'s
///   `auto_enable_mail_for_new_users`, no MSEK yet — the full mailbox when
///   `email_enabled`, the read recipe when only `caldav_enabled` /
///   `carddav_enabled` ([`MailSettingsMachine::auto_enable_mail_for_new_user`]).
///
/// A failed `am_i_admin` read is a failed step (classified like any other),
/// never a guess: reading it as "not the admin" would run the new-user arm on
/// the admin's own claim.
pub async fn provision_mail_at_first_setup<R>(
    nest: R,
    machine: MailSettingsMachine,
    admin_enable_email: bool,
) -> StepOutcome
where
    R: RpcRequester + Clone,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let is_admin = match am_i_admin(nest.clone()).await {
        Ok(is_admin) => is_admin,
        Err(outcome) => return outcome,
    };
    // A custody that already holds a mailbox — the same identity reclaiming a
    // factory-reset box, or a rebuilt one — is restored onto this nest, never
    // minted over; a sentinel standing is resumed here (`mail-credentials.md`
    // § A nest that lost the mailbox, rule 3). Only the admin claim's
    // enable-email choice turns the deployment subsystem on.
    match machine
        .restore_mailbox_at_claim(is_admin && admin_enable_email)
        .await
    {
        Ok(true) => {
            tracing::info!("first setup: the account's mailbox was restored from its custody");
            return StepOutcome::Done;
        }
        Ok(false) => {}
        Err(e) => return StepOutcome::of_dispatch(e),
    }

    if is_admin {
        if !admin_enable_email {
            return StepOutcome::Done;
        }
        match machine
            .enable_mail_with_generated_password("Default".to_string())
            .await
        {
            Ok(_password) => {
                tracing::info!(
                    "onboarding: deployment mail auto-enabled + admin mailbox \
                     auto-minted (generated password on the mail-settings page)"
                );
                StepOutcome::Done
            }
            Err(e) => StepOutcome::of_dispatch(e),
        }
    } else {
        let status: SetupStatusReply = match nest
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await
        {
            Ok(status) => status,
            Err(e) => return StepOutcome::of_transport(e),
        };
        match machine
            .auto_enable_mail_for_new_user(
                status.email_enabled,
                status.caldav_enabled || status.carddav_enabled,
                status.auto_enable_mail_for_new_users,
                "Default".to_string(),
            )
            .await
        {
            Ok(Some(_password)) => {
                tracing::info!(
                    "first setup: bridge credential auto-minted for new user \
                     (generated password on the mail-settings page)"
                );
                StepOutcome::Done
            }
            Ok(None) => StepOutcome::Done,
            Err(e) => StepOutcome::of_dispatch(e),
        }
    }
}

/// The production [`ServingEnablementExecutor`]: the transport handle, a
/// factory for this actor's [`MailSettingsMachine`] (each mint step consumes
/// one), the deployment-toggle machine, and the transport's connection-state
/// signal. Generic over the transport so the native and wasm builders in
/// `rpc_glue` construct the same executor.
pub struct RpcServingEnablement<R, F, L> {
    pub nest: R,
    pub build_mail_machine: F,
    pub bridges: BridgeApprovalMachine,
    pub live: L,
}

impl<R, F, L> ServingEnablementExecutor for RpcServingEnablement<R, F, L>
where
    R: RpcRequester + Clone,
    R::Error: fauna_protocol::RpcErrorClass,
    F: Fn() -> MailSettingsMachine,
    L: ConnectionLive,
{
    async fn wait_until_live(&self) -> bool {
        self.live.wait_until_live().await
    }

    async fn run(&self, step: ServingEnablementStep) -> StepOutcome {
        use ServingEnablementStep::*;
        let toggle = |action: BridgeApprovalAction| async move {
            match self.bridges.dispatch(action).await {
                Ok(_) => StepOutcome::Done,
                Err(e) => StepOutcome::of_dispatch(e),
            }
        };
        match step {
            ProvisionMail { admin_enable_email } => {
                provision_mail_at_first_setup(
                    self.nest.clone(),
                    (self.build_mail_machine)(),
                    admin_enable_email,
                )
                .await
            }
            SetCalDavEnabled => {
                toggle(BridgeApprovalAction::SetCalDavEnabled { enabled: true }).await
            }
            SetCardDavEnabled => {
                toggle(BridgeApprovalAction::SetCardDavEnabled { enabled: true }).await
            }
            ProvisionDavMailbox => {
                provision_dav_mailbox_at_first_setup(self.nest.clone(), (self.build_mail_machine)())
                    .await
            }
            SetWebDavEnabled => {
                toggle(BridgeApprovalAction::SetWebDavEnabled { enabled: true }).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ServingEnablementStep::*;
    use super::*;

    fn intents(email: bool, caldav: bool, carddav: bool, webdav: bool) -> ServingEnablementIntents {
        ServingEnablementIntents {
            email,
            caldav,
            carddav,
            webdav,
        }
    }

    /// Decide-nothing (sign-in, invite, local handle) still plans the mail
    /// provision — the new-user auto-mint — and nothing else.
    #[test]
    fn all_off_plans_only_the_mail_provision() {
        assert_eq!(
            plan(ServingEnablementIntents::default()),
            vec![ProvisionMail {
                admin_enable_email: false
            }]
        );
    }

    /// A real-domain claim: everything on, and email's mint is the only one.
    #[test]
    fn all_on_plans_one_mint_path() {
        assert_eq!(
            plan(intents(true, true, true, true)),
            vec![
                ProvisionMail {
                    admin_enable_email: true
                },
                SetCalDavEnabled,
                SetCardDavEnabled,
                SetWebDavEnabled,
            ]
        );
    }

    /// One DAV mint, fired exactly when a DAV intent is on and email off —
    /// over every combination, at most one mint step.
    #[test]
    fn exactly_one_mint_path_whenever_any_mailbox_protocol_is_on() {
        for bits in 0..16u8 {
            let i = intents(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
            let steps = plan(i);
            let mints = steps
                .iter()
                .filter(|s| {
                    matches!(
                        s,
                        ProvisionMail {
                            admin_enable_email: true
                        } | ProvisionDavMailbox
                    )
                })
                .count();
            let expected = usize::from(i.email || i.caldav || i.carddav);
            assert_eq!(mints, expected, "{i:?} → {steps:?}");
            assert_eq!(
                steps.contains(&ProvisionDavMailbox),
                (i.caldav || i.carddav) && !i.email,
                "{i:?}"
            );
            assert_eq!(steps.contains(&SetCalDavEnabled), i.caldav, "{i:?}");
            assert_eq!(steps.contains(&SetCardDavEnabled), i.carddav, "{i:?}");
            assert_eq!(steps.contains(&SetWebDavEnabled), i.webdav, "{i:?}");
        }
    }

    /// A scripted executor: each step answers from its own queue of outcomes
    /// (`Done` once the queue is empty), every attempt and backoff is
    /// recorded, and the connection is live unless `gone_after` attempts have
    /// run.
    #[derive(Default)]
    struct Scripted {
        script: Mutex<Vec<(ServingEnablementStep, Vec<StepOutcome>)>>,
        attempts: Mutex<Vec<ServingEnablementStep>>,
        backoffs: Mutex<Vec<std::time::Duration>>,
        gone_after: Option<usize>,
    }

    impl Scripted {
        fn with(script: Vec<(ServingEnablementStep, Vec<StepOutcome>)>) -> Self {
            Self {
                script: Mutex::new(script),
                ..Self::default()
            }
        }
        fn attempts(&self) -> Vec<ServingEnablementStep> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl ServingEnablementExecutor for Scripted {
        async fn wait_until_live(&self) -> bool {
            self.gone_after
                .is_none_or(|n| self.attempts.lock().unwrap().len() < n)
        }
        async fn run(&self, step: ServingEnablementStep) -> StepOutcome {
            self.attempts.lock().unwrap().push(step);
            let mut script = self.script.lock().unwrap();
            match script.iter_mut().find(|(s, q)| *s == step && !q.is_empty()) {
                Some((_, q)) => q.remove(0),
                None => StepOutcome::Done,
            }
        }
        async fn back_off(&self, delay: std::time::Duration) {
            self.backoffs.lock().unwrap().push(delay);
        }
    }

    fn run_of(actor: &str) -> serde_json::Value {
        serving_enablement_json()["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["actor_id"] == actor)
            .cloned()
            .unwrap()
    }

    /// The anchor contract: a run is recorded against its actor at the first
    /// statement and marked completed once every step succeeded — the
    /// decide-nothing run included — and the executor saw exactly the plan.
    /// Each test keys its own actor, so tests running in parallel never read
    /// each other's run.
    #[tokio::test]
    async fn a_run_lands_its_completion_even_when_it_decides_nothing() {
        let rec = Scripted::default();
        let report =
            apply_serving_enablement("aa".into(), ServingEnablementIntents::default(), &rec).await;
        assert_eq!(report, ServingEnablementReport::default());
        let aa = run_of("aa");
        assert_eq!(aa["completed"], true);
        assert_eq!(aa["outcome"], "succeeded");
        assert_eq!(aa["refused"], serde_json::json!([]));
        assert_eq!(rec.attempts(), plan(ServingEnablementIntents::default()));

        let on = intents(false, true, false, true);
        let rec = Scripted::default();
        apply_serving_enablement("bb".into(), on, &rec).await;
        assert_eq!(rec.attempts(), plan(on));
        let bb = run_of("bb");
        assert_eq!(bb["completed"], true);
        assert_eq!(bb["decided"]["caldav"], true);
        assert_eq!(bb["decided"]["email"], false);
    }

    /// The 0.1.7 walk: the mail provision meets a store not ready yet, then a
    /// dropped connection, then succeeds — it is tried until it does, with a
    /// growing backoff, and the later steps wait behind it in plan order.
    #[tokio::test]
    async fn a_passing_failure_is_tried_again_and_the_order_is_kept() {
        let on = intents(true, true, true, false);
        let mail = ProvisionMail {
            admin_enable_email: true,
        };
        let exec = Scripted::with(vec![
            (
                mail,
                vec![
                    StepOutcome::TryAgain("the account store is not ready yet".into()),
                    StepOutcome::TryAgain("The connection to the nest was lost.".into()),
                ],
            ),
            (
                SetCalDavEnabled,
                vec![StepOutcome::TryAgain("timeout".into())],
            ),
        ]);
        let report = apply_serving_enablement("dd".into(), on, &exec).await;
        assert_eq!(report, ServingEnablementReport::default());
        assert_eq!(
            exec.attempts(),
            vec![
                mail,
                mail,
                mail,
                SetCalDavEnabled,
                SetCalDavEnabled,
                SetCardDavEnabled
            ]
        );
        assert_eq!(
            *exec.backoffs.lock().unwrap(),
            vec![step_backoff(0), step_backoff(1), step_backoff(0)]
        );
        assert_eq!(run_of("dd")["outcome"], "succeeded");
    }

    /// A refusal for good ends its step only: the later steps still run, the
    /// run completes as `refused`, and the refusal is returned for the page.
    #[tokio::test]
    async fn a_refusal_for_good_is_returned_and_completes_the_run() {
        let on = intents(false, true, false, true);
        let exec = Scripted::with(vec![(
            SetCalDavEnabled,
            vec![StepOutcome::RefusedForGood(
                "CalDAV is not available here.".into(),
            )],
        )]);
        let report = apply_serving_enablement("ee".into(), on, &exec).await;
        assert_eq!(
            report.refusals,
            vec![StepRefusal {
                step: "set_caldav_enabled",
                message: "CalDAV is not available here.".into()
            }]
        );
        assert_eq!(
            report.error_message().as_deref(),
            Some("CalDAV is not available here.")
        );
        assert_eq!(exec.attempts(), plan(on));
        let ee = run_of("ee");
        assert_eq!(ee["completed"], true);
        assert_eq!(ee["outcome"], "refused");
        assert_eq!(ee["refused"][0]["step"], "set_caldav_enabled");
        assert_eq!(ee["refused"][0]["message"], "CalDAV is not available here.");
    }

    /// A session that ends mid-run abandons it: never `completed`, the
    /// remaining steps not tried.
    #[tokio::test]
    async fn a_session_that_ends_abandons_the_run() {
        let on = intents(false, true, true, false);
        let exec = Scripted {
            gone_after: Some(1),
            ..Scripted::default()
        };
        let report = apply_serving_enablement("ff".into(), on, &exec).await;
        assert!(report.abandoned);
        assert_eq!(exec.attempts().len(), 1);
        let ff = run_of("ff");
        assert_eq!(ff["completed"], false);
        assert_eq!(ff["outcome"], "abandoned");
    }

    /// A run still trying a step reads `completed: false`, `outcome: running`
    /// — the consumer's wait cannot release on a run that has not finished.
    #[test]
    fn a_run_in_flight_is_not_completed() {
        struct Parked;
        impl ServingEnablementExecutor for Parked {
            async fn wait_until_live(&self) -> bool {
                true
            }
            async fn run(&self, _step: ServingEnablementStep) -> StepOutcome {
                std::future::pending::<StepOutcome>().await
            }
        }
        let fut =
            apply_serving_enablement("cc".into(), ServingEnablementIntents::default(), &Parked);
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        let cc = run_of("cc");
        assert_eq!(cc["completed"], false);
        assert_eq!(cc["outcome"], "running");
        drop(fut);
        assert_eq!(run_of("cc")["outcome"], "abandoned");
    }

    /// The classifier reads the error TYPES: the store's typed not-ready
    /// refusal and the seam's `Transient` class pass, a nest refusal is final
    /// and said as the nest's own sentence.
    #[test]
    fn errors_are_classified_by_type() {
        use crate::error::{DispatchError, NestError, StoreError};
        let not_ready = DispatchError::Store(StoreError::Load(
            fauna_client_config::LEDGER_NOT_READY.into(),
        ));
        assert!(matches!(
            StepOutcome::of_dispatch(not_ready),
            StepOutcome::TryAgain(_)
        ));
        assert!(matches!(
            StepOutcome::of_dispatch(DispatchError::Nest(NestError::Transient("x".into()))),
            StepOutcome::TryAgain(_)
        ));
        assert_eq!(
            StepOutcome::of_dispatch(DispatchError::Nest(NestError::Rejected(
                "Not allowed.".into()
            ))),
            StepOutcome::RefusedForGood("Not allowed.".into())
        );
        // A store that answered and failed is final.
        assert!(matches!(
            StepOutcome::of_dispatch(DispatchError::Store(StoreError::Save("disk full".into()))),
            StepOutcome::RefusedForGood(_)
        ));
    }

    #[test]
    fn the_backoff_doubles_up_to_its_cap() {
        assert_eq!(step_backoff(0), STEP_BACKOFF_FIRST);
        assert_eq!(step_backoff(1), STEP_BACKOFF_FIRST * 2);
        assert_eq!(step_backoff(10), STEP_BACKOFF_MAX);
        assert_eq!(step_backoff(u32::MAX), STEP_BACKOFF_MAX);
    }
}
