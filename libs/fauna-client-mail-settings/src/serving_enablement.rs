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
//!   one-MSEK-mint-path gates (`caldav-server.md` / `carddav-server.md`
//!   § Independent enablement): the CalDAV-only mailbox mint fires only with
//!   email off, the CardDAV-only one only with email and CalDAV both off, so
//!   exactly one mint path writes the mail custody at first setup.
//! - [`apply_serving_enablement`] — runs the plan to the end, one step after
//!   another, and records each run against its actor — `{actor_id, decided,
//!   completed}` — which [`serving_enablement_json`] renders for
//!   `fauna_e2e_agent::SERVING_ENABLEMENT_KEY` (convention 14's causal anchor —
//!   `docs/goal/architecture/e2e-latency-independent-assertions.md`).
//! - [`RpcServingEnablement`] — the production executor, generic over the
//!   transport so native (`rpc_glue::dispatch_post_claim_serving_enablement`)
//!   and wasm (`fauna-wasm`) run the identical bodies.
//!
//! The mail-provision step runs on EVERY `LoggedIn` handoff, whatever the
//! intents: on a non-admin (invite-redeem) first setup it is the new-user
//! mailbox auto-mint (`mail-credentials.md` § Auto-enable for new users), which
//! the email intent does not gate. A sign-in reaches the step too, and there it
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
/// matching settings-page path and log-only on failure — onboarding has already
/// succeeded, and every one of these can be redone from the app's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingEnablementStep {
    /// The first-setup mail provision: on the admin claim, mint the admin's
    /// mailbox and flip deployment mail on iff `admin_enable_email`; on a
    /// non-admin first setup, the policy-gated new-user auto-mint.
    ProvisionMail { admin_enable_email: bool },
    /// `fauna.bridges.set_caldav_enabled(true)`.
    SetCalDavEnabled,
    /// The CalDAV-only admin mailbox mint (email off).
    ProvisionCalDavMailbox,
    /// `fauna.bridges.set_carddav_enabled(true)`.
    SetCardDavEnabled,
    /// The CardDAV-only admin mailbox mint (email and CalDAV off).
    ProvisionCardDavMailbox,
    /// `fauna.bridges.set_webdav_enabled(true)`. WebDAV has no per-actor
    /// mailbox, so there is no companion mint.
    SetWebDavEnabled,
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
        if !intents.email {
            steps.push(ProvisionCalDavMailbox);
        }
    }
    if intents.carddav {
        steps.push(SetCardDavEnabled);
        if !intents.email && !intents.caldav {
            steps.push(ProvisionCardDavMailbox);
        }
    }
    if intents.webdav {
        steps.push(SetWebDavEnabled);
    }
    steps
}

/// Performs one [`ServingEnablementStep`]. Infallible by signature: every step
/// logs its own failure, and a failed step must not stop the ones after it
/// (the four protocols gate independently).
#[allow(async_fn_in_trait)] // static dispatch only; Send-ness leaks per concrete impl
pub trait ServingEnablementExecutor {
    async fn run(&self, step: ServingEnablementStep);
}

/// Run the post-claim serving enablement to the end, for the actor
/// `actor_id_hex` (the identity whose `LoggedIn` handoff this is).
///
/// The run is recorded — `{actor_id, decided, completed}` — at the first
/// statement, and marked `completed` once every step the plan holds has been
/// answered, **including the decide-nothing case**, whose plan still carries the
/// mail-provision step. That is what gives a "stays OFF" read its causal anchor:
/// once this actor's run is `completed`, no late enable can arrive from this
/// glue. The mark is set by a drop guard, so a step that panics still lands its
/// run. Steps run sequentially: each is bounded by its transport's request
/// timeout, and one-at-a-time keeps the mint paths and the deployment toggles
/// from racing one another.
pub async fn apply_serving_enablement<E: ServingEnablementExecutor>(
    actor_id_hex: String,
    intents: ServingEnablementIntents,
    executor: &E,
) {
    let _run = runs::begin(actor_id_hex, intents);
    for step in plan(intents) {
        executor.run(step).await;
    }
}

/// Build `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`'s value —
/// `{"started": N, "completed": M, "runs": [{"actor_id", "decided": {"email",
/// "caldav", "carddav", "webdav"}, "completed"}]}`, `runs` in start order for
/// this process. A consumer finds ITS run by the actor it onboarded, so the
/// answer needs no baseline and survives the app relaunching before the
/// handoff. The empty shape is the legitimate "the glue has not run in this
/// process"; an app without the leg publishes nothing.
pub fn serving_enablement_json() -> serde_json::Value {
    let (started, completed, records) = runs::read();
    let runs: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "actor_id": r.actor_id,
                "decided": r.decided,
                "completed": r.completed,
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

    #[derive(Clone)]
    pub(super) struct RunRecord {
        pub(super) actor_id: String,
        pub(super) decided: ServingEnablementIntents,
        pub(super) completed: bool,
    }

    static STARTED: AtomicU64 = AtomicU64::new(0);
    static COMPLETED: AtomicU64 = AtomicU64::new(0);
    // One entry per `LoggedIn` handoff this process ran — a handful at most
    // (an add-account or a re-onboarding appends one), so never pruned.
    static RUNS: Mutex<Vec<RunRecord>> = Mutex::new(Vec::new());

    pub(super) struct RunGuard(usize);

    impl Drop for RunGuard {
        fn drop(&mut self) {
            if let Some(r) = RUNS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(self.0)
            {
                r.completed = true;
            }
            COMPLETED.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub(super) fn begin(actor_id: String, decided: ServingEnablementIntents) -> RunGuard {
        let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        runs.push(RunRecord {
            actor_id,
            decided,
            completed: false,
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

/// Which first-setup mailbox mint [`provision_mailbox_at_first_setup`] runs.
/// The CalDAV and CardDAV arms differ only in the enable method and the label.
pub enum FirstSetupMailboxKind {
    CalDav,
    CardDav,
}

impl FirstSetupMailboxKind {
    fn label(&self) -> &'static str {
        match self {
            Self::CalDav => "CalDAV",
            Self::CardDav => "CardDAV",
        }
    }
}

/// First-setup CalDAV/CardDAV admin mailbox auto-mint. On the **admin claim**,
/// mint the admin's shared MSEK + `default` credential so a
/// calendar/contacts-only deployment has the per-actor key material its store
/// seals under. **Admin-only** — a non-admin auto-mint policy is unbuilt
/// (`SetupStatusReply` carries no `{caldav,carddav}_enabled` /
/// `auto_enable_*_for_new_users` fields today), so a non-admin first setup
/// no-ops. Callers gate the call on being the only mint path live ([`plan`]).
pub async fn provision_mailbox_at_first_setup<R: RpcRequester>(
    nest: R,
    machine: MailSettingsMachine,
    kind: FirstSetupMailboxKind,
) {
    let account = fauna_client_account::AccountClient::new(nest);
    let is_admin = account.am_i_admin().await.map(|r| r.admin).unwrap_or(false);
    if !is_admin {
        return;
    }
    let result = match kind {
        FirstSetupMailboxKind::CalDav => machine
            .enable_caldav_mailbox_with_generated_password("Default".to_string())
            .await
            .map(drop),
        FirstSetupMailboxKind::CardDav => machine
            .enable_carddav_mailbox_with_generated_password("Default".to_string())
            .await
            .map(drop),
    };
    match result {
        Ok(()) => tracing::info!(
            "onboarding: {}-only admin mailbox auto-minted (shared MSEK; \
             generated password on the mail-settings page)",
            kind.label()
        ),
        Err(e) => tracing::error!(
            "provision_{}_mailbox_at_first_setup(admin): {e:?}",
            kind.label().to_lowercase()
        ),
    }
}

/// First-authenticated-setup mail provision. Discriminates on `am_i_admin`:
///
/// - **Admin claim**: iff `admin_enable_email`, mint the admin's own mailbox via
///   [`MailSettingsMachine::enable_mail_with_generated_password`], whose final
///   step fires `set_mail_enabled(true)`.
/// - **New (non-admin) user**: auto-mint the user's own mailbox iff the
///   deployment policy allows — `fauna.setup.status`'s `email_enabled &&
///   auto_enable_mail_for_new_users`, no mailbox yet
///   ([`MailSettingsMachine::auto_enable_mail_for_new_user`]).
pub async fn provision_mail_at_first_setup<R: RpcRequester + Clone>(
    nest: R,
    machine: MailSettingsMachine,
    admin_enable_email: bool,
) {
    let account = fauna_client_account::AccountClient::new(nest.clone());
    let is_admin = account.am_i_admin().await.map(|r| r.admin).unwrap_or(false);

    if is_admin {
        if admin_enable_email {
            match machine
                .enable_mail_with_generated_password("Default".to_string())
                .await
            {
                Ok(_password) => tracing::info!(
                    "onboarding: deployment mail auto-enabled + admin mailbox \
                     auto-minted (generated password on the mail-settings page)"
                ),
                Err(e) => tracing::error!("provision_mail_at_first_setup(admin): {e:?}"),
            }
        }
    } else {
        let status: Result<SetupStatusReply, _> = nest
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await;
        let Ok(status) = status else {
            tracing::error!("provision_mail_at_first_setup(new-user): setup.status fetch failed");
            return;
        };
        match machine
            .auto_enable_mail_for_new_user(
                status.email_enabled,
                status.auto_enable_mail_for_new_users,
                "Default".to_string(),
            )
            .await
        {
            Ok(Some(_password)) => tracing::info!(
                "first setup: mailbox auto-minted for new user \
                 (generated password on the mail-settings page)"
            ),
            Ok(None) => {}
            Err(e) => tracing::error!("provision_mail_at_first_setup(new-user): {e:?}"),
        }
    }
}

/// The production [`ServingEnablementExecutor`]: the transport handle, a
/// factory for this actor's [`MailSettingsMachine`] (each mint step consumes
/// one), and the deployment-toggle machine. Generic over the transport so the
/// native and wasm builders in `rpc_glue` construct the same executor.
pub struct RpcServingEnablement<R, F> {
    pub nest: R,
    pub build_mail_machine: F,
    pub bridges: BridgeApprovalMachine,
}

impl<R, F> ServingEnablementExecutor for RpcServingEnablement<R, F>
where
    R: RpcRequester + Clone,
    F: Fn() -> MailSettingsMachine,
{
    async fn run(&self, step: ServingEnablementStep) {
        use ServingEnablementStep::*;
        let toggle = |action: BridgeApprovalAction| async move {
            let label = format!("{action:?}");
            if let Err(e) = self.bridges.dispatch(action).await {
                tracing::error!("{label}: {e:?}");
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
            ProvisionCalDavMailbox => {
                provision_mailbox_at_first_setup(
                    self.nest.clone(),
                    (self.build_mail_machine)(),
                    FirstSetupMailboxKind::CalDav,
                )
                .await
            }
            SetCardDavEnabled => {
                toggle(BridgeApprovalAction::SetCardDavEnabled { enabled: true }).await
            }
            ProvisionCardDavMailbox => {
                provision_mailbox_at_first_setup(
                    self.nest.clone(),
                    (self.build_mail_machine)(),
                    FirstSetupMailboxKind::CardDav,
                )
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

    /// The companion mints fire exactly when no earlier protocol minted the
    /// shared MSEK — over every combination, at most one mint step.
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
                        } | ProvisionCalDavMailbox
                            | ProvisionCardDavMailbox
                    )
                })
                .count();
            let expected = usize::from(i.email || i.caldav || i.carddav);
            assert_eq!(mints, expected, "{i:?} → {steps:?}");
            assert_eq!(steps.contains(&SetCalDavEnabled), i.caldav, "{i:?}");
            assert_eq!(steps.contains(&SetCardDavEnabled), i.carddav, "{i:?}");
            assert_eq!(steps.contains(&SetWebDavEnabled), i.webdav, "{i:?}");
        }
    }

    struct Recorder(Mutex<Vec<ServingEnablementStep>>);

    impl ServingEnablementExecutor for Recorder {
        async fn run(&self, step: ServingEnablementStep) {
            self.0.lock().unwrap().push(step);
        }
    }

    /// The anchor contract: a run is recorded against its actor at the first
    /// statement and marked completed at the end — the decide-nothing run
    /// included — and the executor saw exactly the plan. Each test keys its
    /// own actor, so tests running in parallel never read each other's run.
    #[tokio::test]
    async fn a_run_lands_its_completion_even_when_it_decides_nothing() {
        let rec = Recorder(Mutex::new(Vec::new()));
        apply_serving_enablement("aa".into(), ServingEnablementIntents::default(), &rec).await;
        let (_, _, r1) = runs::read();
        let last = r1.iter().find(|r| r.actor_id == "aa").unwrap();
        assert_eq!(
            (last.actor_id.as_str(), last.decided, last.completed),
            ("aa", ServingEnablementIntents::default(), true)
        );
        assert_eq!(
            *rec.0.lock().unwrap(),
            plan(ServingEnablementIntents::default())
        );

        let on = intents(false, true, false, true);
        let rec = Recorder(Mutex::new(Vec::new()));
        apply_serving_enablement("bb".into(), on, &rec).await;
        assert_eq!(*rec.0.lock().unwrap(), plan(on));

        let json = serving_enablement_json();
        let runs = json["runs"].as_array().unwrap();
        let bb = runs.iter().find(|r| r["actor_id"] == "bb").unwrap();
        assert_eq!(bb["completed"], true);
        assert_eq!(bb["decided"]["caldav"], true);
        assert_eq!(bb["decided"]["email"], false);
        assert!(runs.iter().any(|r| r["actor_id"] == "aa"));
    }

    /// A run still in flight reads `completed: false` — the consumer's wait
    /// cannot release on a run that has not finished.
    #[test]
    fn a_run_in_flight_is_not_completed() {
        struct Parked;
        impl ServingEnablementExecutor for Parked {
            async fn run(&self, _step: ServingEnablementStep) {
                std::future::pending::<()>().await
            }
        }
        let fut =
            apply_serving_enablement("cc".into(), ServingEnablementIntents::default(), &Parked);
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        let (_, _, runs) = runs::read();
        let cc = runs.iter().find(|r| r.actor_id == "cc").unwrap();
        assert!(!cc.completed);
    }
}
