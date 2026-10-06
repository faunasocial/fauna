//! Handle-first onboarding orchestrator.
//!
//! State lives in `Arc<OnboardingMachine>` (from `fauna-onboarding-machine`).
//! Per-stage modules each expose a universal
//! `build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>)` constructor.
//! The orchestrator builds every page once, registers each refresh closure
//! in a `name -> Rc<dyn Fn()>` map, and listens to the machine observer via
//! an `async-channel` receiver attached to the GTK main loop. On every
//! notification it (a) swaps the visible stack child if `m.step()` changed,
//! then (b) calls the active page's refresh closure; on `OnboardingStep::Done`
//! it tears down the wizard and routes per `wizard_outcome()`.
//!
//! wizard_outcome routing (see `handle_wizard_done`):
//!   LoggedIn               → store credentials + launch authenticated UI
//!   (no pending-invite exit — that journey stays on the page and polls)
//!   AwaitingManualDns      → store awaiting-DNS slot; close wizard
//!
//! Per-stage code never calls `stack.set_visible_child_name` directly — it
//! only invokes machine mutators and lets the observer tick do the navigation.

mod claim_code;
mod dns_config;
mod dns_post_instructions;
mod generic_provider_form;
mod handle_entry;
mod identity_choice;
mod identity_created;
mod identity_import;
mod invite_request;
mod machine_glue;
mod nat_mode_choice;
mod nest_provisioning;
mod nest_recovery;
mod recover_selfhosted_instructions;
mod recovery_entry;
mod recovery_kit;
mod trust_prompt;
mod vps_config;
#[allow(dead_code)]
mod widgets;

pub mod awaiting_manual_dns;

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::i18n::strings::onboarding::session_error;
use adw::prelude::*;
use fauna_onboarding_machine::{OnboardingMachine, OnboardingStep, WizardOutcome};
use gtk::glib;

// ---------------------------------------------------------------------------
// Step name constants — used as `gtk::Stack` child names.
// ---------------------------------------------------------------------------

pub const STEP_IDENTITY_CHOICE: &str = "identity_choice";
pub const STEP_IDENTITY_CREATED: &str = "identity_created";
pub const STEP_IDENTITY_IMPORT: &str = "identity_import";
pub const STEP_HANDLE_ENTRY: &str = "handle_entry";
pub const STEP_DNS_CONFIG: &str = "dns_config";
pub const STEP_VPS_CONFIG: &str = "vps_config";
pub const STEP_DNS_POST_INSTRUCTIONS: &str = "dns_post_instructions";
pub const STEP_INVITE_REQUEST: &str = "invite_request";
pub const STEP_CLAIM_CODE: &str = "claim_code";
pub const STEP_NAT_MODE_CHOICE: &str = "nat_mode_choice";
pub const STEP_TRUST_PROMPT: &str = "trust_prompt";
pub const STEP_RECOVERY_KIT: &str = "recovery_kit";
pub const STEP_RECOVERY_ENTRY: &str = "recovery_entry";
pub const STEP_NEST_PROVISIONING: &str = "nest_provisioning";
pub const STEP_NEST_RECOVERY: &str = "nest_recovery";
pub const STEP_RECOVER_SELFHOSTED_INSTRUCTIONS: &str = "recover_selfhosted_instructions";

/// The "Almost ready" surface. Deliberately NOT a `STEP_*`: it is not an
/// `OnboardingStep` at all, but a stack child selected off `wizard_outcome()`
/// (`onboarding.md` § "Almost ready" surface). It still needs a stack name
/// because the wizard renders through one `gtk::Stack`.
pub const SURFACE_AWAITING_MANUAL_DNS: &str = "awaiting_manual_dns";

fn step_to_name(step: OnboardingStep) -> Option<&'static str> {
    use OnboardingStep::*;
    Some(match step {
        IdentityChoice => STEP_IDENTITY_CHOICE,
        IdentityCreated => STEP_IDENTITY_CREATED,
        IdentityImport => STEP_IDENTITY_IMPORT,
        HandleEntry => STEP_HANDLE_ENTRY,
        DnsConfig => STEP_DNS_CONFIG,
        VpsConfig => STEP_VPS_CONFIG,
        DnsPostInstructions => STEP_DNS_POST_INSTRUCTIONS,
        InviteRequest => STEP_INVITE_REQUEST,
        ClaimCode => STEP_CLAIM_CODE,
        NatModeChoice => STEP_NAT_MODE_CHOICE,
        NestProvisioning => STEP_NEST_PROVISIONING,
        // Box-recovery step 4 (box-recovery.md § Recovery UI): the box-selection
        // hub + the self-hosted install instructions.
        NestRecovery => STEP_NEST_RECOVERY,
        RecoverSelfhostedInstructions => STEP_RECOVER_SELFHOSTED_INSTRUCTIONS,
        // Identity-recovery steps (onboarding.md § 1 Identity, ratified
        // 2026-08-01) — built on linux 2026-09-26 after tui led: the kit offer
        // (reached because `make_machine` declares `set_renders_recovery_kit`)
        // and the phrase restore (`restore-from-recovery-kit-button`).
        RecoveryKit => STEP_RECOVERY_KIT,
        RecoveryEntry => STEP_RECOVERY_ENTRY,
        // The one-tap trust offer (onboarding.md § 3b-ter) — BUILT on linux
        // 2026-08-14 (tui led it the same day), so `make_machine` declares
        // `set_renders_trust_prompt(true)` and both NAT-step exits route here.
        TrustPrompt => STEP_TRUST_PROMPT,
        // The orchestrator handles `Done` separately — no stack child to swap to.
        Done => return None,
    })
}

/// Result from building the onboarding window. The error label is exposed
/// so `start_test_agent_if_enabled` (in main.rs) can mirror in-window error
/// state into its JSON payload.
pub struct OnboardingResult {
    pub window: adw::Window,
    pub error_label: gtk::Label,
    /// Handle to the wizard's state machine — exposed so main.rs's
    /// test-agent reset handler can call `m.reset()` instead of tearing
    /// down the window (which would trigger app.quit() via the
    /// connect_close_request handler and kill the bridge between tests).
    pub machine: Arc<OnboardingMachine>,
}

type RefreshMap = Rc<RefCell<HashMap<&'static str, Rc<dyn Fn()>>>>;

/// One page's name and its deferred (widget, refresh-closure) builder.
type PageEntry = (&'static str, Box<dyn FnOnce() -> (gtk::Box, Rc<dyn Fn()>)>);

/// Build every page once and stash its refresh closure in a name-keyed map.
///
/// `append` reaches the two identity-confirm pages (moment 1 of the
/// two-moment write contract, [`commit_confirmed_identity`]) and the
/// invite-request page (moment 1's pending-invite twin,
/// [`persist_pending_invite_slot`]) — every page whose submit return can write
/// the account registry.
fn pages(
    m: Arc<OnboardingMachine>,
    command: recover_selfhosted_instructions::CommandCell,
    append: bool,
) -> Vec<PageEntry> {
    vec![
        (
            STEP_IDENTITY_CHOICE,
            Box::new({
                let m = m.clone();
                move || identity_choice::build(m)
            }),
        ),
        (
            STEP_IDENTITY_CREATED,
            Box::new({
                let m = m.clone();
                move || identity_created::build(m, append)
            }),
        ),
        (
            STEP_IDENTITY_IMPORT,
            Box::new({
                let m = m.clone();
                move || identity_import::build(m, append)
            }),
        ),
        (
            STEP_RECOVERY_KIT,
            Box::new({
                let m = m.clone();
                move || recovery_kit::build(m)
            }),
        ),
        (
            STEP_RECOVERY_ENTRY,
            Box::new({
                let m = m.clone();
                move || recovery_entry::build(m, append)
            }),
        ),
        (
            STEP_HANDLE_ENTRY,
            Box::new({
                let m = m.clone();
                move || handle_entry::build(m)
            }),
        ),
        (
            STEP_DNS_CONFIG,
            Box::new({
                let m = m.clone();
                move || dns_config::build(m)
            }),
        ),
        (
            STEP_VPS_CONFIG,
            Box::new({
                let m = m.clone();
                move || vps_config::build(m)
            }),
        ),
        (
            STEP_DNS_POST_INSTRUCTIONS,
            Box::new({
                let m = m.clone();
                move || dns_post_instructions::build(m)
            }),
        ),
        (
            STEP_INVITE_REQUEST,
            Box::new({
                let m = m.clone();
                move || invite_request::build(m, append)
            }),
        ),
        (
            STEP_CLAIM_CODE,
            Box::new({
                let m = m.clone();
                move || claim_code::build(m)
            }),
        ),
        (
            STEP_NAT_MODE_CHOICE,
            Box::new({
                let m = m.clone();
                move || nat_mode_choice::build(m)
            }),
        ),
        (
            STEP_TRUST_PROMPT,
            Box::new({
                let m = m.clone();
                move || trust_prompt::build(m)
            }),
        ),
        (
            STEP_NEST_RECOVERY,
            Box::new({
                let m = m.clone();
                move || nest_recovery::build(m)
            }),
        ),
        (
            STEP_RECOVER_SELFHOSTED_INSTRUCTIONS,
            Box::new({
                let m = m.clone();
                move || recover_selfhosted_instructions::build(m, command)
            }),
        ),
        (
            SURFACE_AWAITING_MANUAL_DNS,
            Box::new({
                let m = m.clone();
                move || awaiting_manual_dns::build(m)
            }),
        ),
        (
            STEP_NEST_PROVISIONING,
            Box::new(move || nest_provisioning::build(m)),
        ),
    ]
}

// The per-actor wizard-resume slots (`long-term-store.md` § Multi-account
// evolution) go through `crate::account_registry()` — this module used to keep
// a second builder of its own, which would have carried the no-op mutation
// lock and silently unserialized every wizard write once the real lock landed.

/// True while the wizard sits at the deferred-DNS exit. The "Almost ready"
/// surface is keyed on the *outcome*, not a step, so the same-session exit and
/// the relaunch hydration render one page (`onboarding.md` § "Almost ready"
/// surface).
fn is_awaiting_manual_dns(m: &OnboardingMachine) -> bool {
    matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    )
}

/// Fire one single-shot `recheck_manual_dns()` off the GTK thread. The machine
/// notifies on completion, so the observer tick repaints the surface — nothing
/// here has to touch a widget. Spawned rather than awaited so a probe against a
/// nest whose DNS has not propagated cannot freeze the UI.
fn spawn_recheck_manual_dns(m: Arc<OnboardingMachine>) {
    crate::async_helper::run_on_tokio(
        async move {
            m.recheck_manual_dns().await;
        },
        |()| {},
    );
}

/// Persist the deferred-DNS resume record into the per-actor registry slot.
///
/// Deliberately writes **no `nest_url`** on the identity: the nest is not
/// claimed yet, so a silent challenge against it could only fail — and the
/// awaiting-DNS row outranks the silent-challenge row anyway. The record
/// carries its own `nest_url` for the reseed (`onboarding.md` § Long-term store
/// contract). Builds the record, then defers the registry write to the shared
/// `fauna_client_accounts::persist_awaiting_dns` (tui consumes the same helper).
fn persist_awaiting_dns(
    m: &OnboardingMachine,
    nest_url: &str,
    dns_records_json: &str,
    claim_code: &str,
) {
    // The identity comes from the MACHINE, never the store: it is the identity
    // this wizard just provisioned with, in append mode it exists nowhere else
    // yet, and reading the active account's slot here would file the deferred
    // nest under whoever is signed in (`onboarding.md` § Long-term store
    // contract — the terminal reads the secret from the machine).
    let Some(secret_hex) = m.effective_secret() else {
        tracing::error!("[onboarding] awaiting-DNS exit with no identity in the machine");
        return;
    };
    let record = fauna_launch_machine::AwaitingDnsRecord {
        nest_url: nest_url.to_string(),
        // Not in the `AwaitingManualDns` outcome payload, but the slot requires
        // it (the eventual `LoggedIn` carries it and it is not derivable from
        // `nest_url`). The machine is where the wizard put it.
        handle: m.current_handle(),
        dns_records_json: dns_records_json.to_string(),
        claim_code: claim_code.to_string(),
        // Completed, not replaced — the shared writer keeps the reach address
        // and the built-with identity the pending-provision write already
        // stored.
        reach_ipv4: None,
        nest_actor_id: None,
    };
    // The shared writer (add_account + set_active + set_awaiting_dns_json) — one
    // implementation across all apps (priority #2).
    if let Err(e) = fauna_client_accounts::persist_awaiting_dns(
        &crate::account_registry(),
        &secret_hex,
        &record,
    ) {
        tracing::error!("[onboarding] persisting the awaiting-DNS account failed: {e:#}");
        m.set_error_message(session_error::persist_account(&e.to_string()));
    }
}

/// Commit a confirmed identity — moment 1 of the two-moment write contract
/// (`long-term-store.md`: *"1. Confirm-identity (generated or imported): write
/// `secret_key`"*). Called from both confirm arms, generated and imported.
///
/// One shared call in both modes, `fauna_client_accounts::persist_confirmed_identity`
/// (priority #2), which owns the first-run/add-account split:
///
/// - **First run** (`append == false`) registers and activates the identity,
///   with the helper's **read-back** — `SecretStore::set` is infallible by
///   signature, so a raw write reports success on a keyring that silently kept
///   nothing, at the one write whose loss destroys an account outright (a
///   freshly generated secret exists nowhere else).
/// - **Append** (`append == true`) writes NOTHING: the appended identity stays
///   in the wizard machine (`effective_secret()`) until `handle_change`'s
///   append arm registers it and switches, so an abandoned append can neither
///   leave a half-account nor shadow the live one — the shape tui always had,
///   and the reason the pre-registry single slot (which this arm used to
///   overwrite, for a boot re-mirror to heal) could retire.
///
/// Log-only on failure, matching every other app's confirm-identity write (tui
/// `tracing::error!`, web's swallowing commit, apple `try?`): the machine has
/// already advanced, so failing loudly here would only desync the UI from
/// wizard state. The read-back is what makes the log line fire at all.
pub(super) fn commit_confirmed_identity(secret_hex: &str, append: bool) {
    if let Err(e) = fauna_client_accounts::persist_confirmed_identity(
        &crate::account_registry(),
        secret_hex,
        append,
    ) {
        tracing::error!("[onboarding] committing the confirmed identity failed: {e:#}");
    }
}

/// Write the pending-invite resume slot at the submit return.
///
/// linux's half of the 2026-08-12 retirement of `WizardOutcome::InviteSubmitted`:
/// the journey no longer exits the wizard, so [`handle_wizard_done`] never sees
/// it and the write moved to the one moment `onboarding.md` § 3 Persistence
/// callouts sanctions — this return. A no-op unless the machine is actually in
/// `PendingReview`, so a failed or refused submit writes nothing.
///
/// **In append mode ("Add account") the write is the adoption**
/// (`onboarding.md` § Multi-account: "the append glue adopts on the submit
/// return — register the append identity …, write its per-actor pending-invite
/// slot, switch to it"): the registry write below is the register + slot half,
/// and `settings::trigger_switch_account` with the actor it returns is the
/// switch — the same trigger the `LoggedIn` terminal's append arm pulls, and
/// tui's `adopt_appended_pending_invite`. The switch tears the running session
/// down (this wizard included) and lands the new account on its own launch
/// surface — the wizard at `invite_request`, what a relaunch would show — via
/// the shared launch routing (`main.rs` `classify_launch`), not an
/// authenticated window over a nest it has no `nest_url` for. Outside append
/// mode there is no live session to switch: the wizard already IS the surface.
pub(super) fn persist_pending_invite_slot(m: &OnboardingMachine, append: bool) {
    let Some(slot) = m.pending_invite_slot() else {
        return;
    };
    let Some(secret_hex) = m.effective_secret() else {
        // Loud: a silently unwritten slot is exactly the class this retirement
        // closed (the deletion compiles fine and just stops persisting).
        tracing::error!("[onboarding] pending-invite slot with no effective_secret");
        return;
    };
    // No nest_url on the *account* — `onboarding.md` § App-launch routing keys
    // the pending-invite row on identity + nest_url absent + pending_invite; a
    // saved nest_url would divert the relaunch onto the silent-challenge row.
    // The record carries its own nest_url for seeding.
    let record = fauna_launch_machine::PendingInviteRecord {
        nest_url: slot.nest_url,
        handle: slot.handle,
        request_id: slot.request_id,
        status_json: slot.status_json,
    };
    // The shared writer (add_account + set_active + set_pending_invite_json) —
    // one implementation across all apps (priority #2).
    match fauna_client_accounts::persist_pending_invite(
        &crate::account_registry(),
        &secret_hex,
        &record,
    ) {
        // `confirmed = false`: no re-auth prompt ran, and a freshly-added
        // account is unflagged, so plain `set_active` takes it (the `LoggedIn`
        // append arm's reasoning).
        Ok(actor_id) if append => crate::settings::trigger_switch_account(actor_id, false),
        Ok(_) => {}
        Err(e) => {
            tracing::error!("[onboarding] pending-invite add_account failed: {e:#}");
            m.set_error_message(session_error::persist_account(&e.to_string()));
        }
    }
}

/// True while the wizard sits on `invite_request` in `PendingReview` — the
/// "no nests, 1 pending invite" surface. Keyed on the *state*, not an outcome:
/// that journey has no wizard exit (`onboarding.md` § The pending-invite
/// surface), so the same-session wait and the relaunch hydration are one page.
fn is_pending_invite_review(m: &OnboardingMachine) -> bool {
    matches!(
        m.invite_request_snapshot().state,
        fauna_onboarding_machine::InviteRequestState::PendingReview { .. }
    )
}

/// Fire one single-shot `recheck_invite_status()` off the GTK thread, the twin
/// of [`spawn_recheck_manual_dns`]. The recheck resolves an approval into
/// `LoggedIn` by itself (the registered-probe), so the observer tick that
/// follows lands the user in the app with no user action at all.
fn spawn_recheck_invite_status(m: Arc<OnboardingMachine>) {
    crate::async_helper::run_on_tokio(
        async move {
            m.recheck_invite_status().await;
        },
        |()| {},
    );
}

/// Arm the pending-invite poll — the same self-terminating shape as
/// [`start_awaiting_dns_poll`], and for the same structural reason: approval
/// reaches an *unregistered* actor through no push channel (every notification
/// plane is keyed on a bearer-proven `actor_id` the requester does not have
/// yet), so the client asks. `onboarding.md` § The pending-invite surface —
/// "Poll is the channel — structurally, not provisionally."
///
/// Armed **once per entry to the `invite_request` page**, not per transition
/// into `PendingReview`. That distinction matters: the state is reached two
/// ways — the same-session submit (page already entered, so no entry fires) and
/// the relaunch hydration (entered already in `PendingReview`) — and arming on
/// both would leave two timers running against one page. So the lifetime is the
/// PAGE and the guard is the STATE: it ticks only while `PendingReview`, and
/// breaks when the wizard leaves the page (an approval routes to `Done`, Back
/// routes to `handle_entry`) or when the machine is dropped, which is what a
/// dismissed append-mode wizard does.
fn start_pending_invite_poll(m: &Arc<OnboardingMachine>) {
    let weak = Arc::downgrade(m);
    glib::timeout_add_local(
        std::time::Duration::from_millis(fauna_onboarding_machine::INVITE_RECHECK_POLL_MS),
        move || {
            let Some(m) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if m.step() != OnboardingStep::InviteRequest {
                return glib::ControlFlow::Break;
            }
            if is_pending_invite_review(&m) {
                spawn_recheck_invite_status(m);
            }
            glib::ControlFlow::Continue
        },
    );
}

/// Arm the surface's poll loop. Self-terminating on both axes: it breaks when
/// the wizard leaves the deferred-DNS outcome (a successful claim routes the
/// machine onward), and when the machine itself is dropped — in append mode the
/// user can simply dismiss the wizard window, and a strong ref here would leave
/// a probe loop hammering a detached machine forever.
fn start_awaiting_dns_poll(m: &Arc<OnboardingMachine>) {
    let weak = Arc::downgrade(m);
    glib::timeout_add_local(awaiting_manual_dns::POLL_INTERVAL, move || {
        let Some(m) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !is_awaiting_manual_dns(&m) {
            return glib::ControlFlow::Break;
        }
        spawn_recheck_manual_dns(m);
        glib::ControlFlow::Continue
    });
}

/// Clear the pending-factory-reset resume slot at a claim terminal (gap CR-1,
/// `common.md` § Client-state recoverability).
///
/// Addressed by the *active* account — the same way
/// `RegistryLaunchPersistence::load_pending_factory_reset` reads it — so the
/// row is written (pre-dispatch), read (at launch), and cleared (here) against
/// one notion of whose slot it is. (The awaiting-DNS slot needs no such helper
/// any more: `persist_logged_in` spends it by actor id.)
fn clear_pending_factory_reset_slot(registry: &fauna_client_accounts::AccountRegistry) {
    if let Some(actor_id) = registry.active() {
        registry.clear_pending_factory_reset(&actor_id);
    }
}

/// The `LoggedIn` terminal's writes — moment 4 of the two-moment contract, then
/// claim terminal #3.
///
/// **Append mode writes nothing here** (`onboarding.md` § Long-term store
/// contract → *Append mode is exempt*): activating now would move `active` off
/// the live account before `handle_change`'s append arm registers the new
/// identity and switches to it — and the live account's factory-reset slot is
/// not this wizard's to spend. tui's `handle_wizard_done` short-circuits its
/// append mode the same way.
fn persist_logged_in_terminal(
    registry: &fauna_client_accounts::AccountRegistry,
    secret_hex: Option<&str>,
    nest_url: &str,
    append: bool,
) {
    if append {
        return;
    }
    if let Some(secret_hex) = secret_hex {
        // Moment 4, through the shared helper — this sequence (register
        // the home nest per-actor, activate, spend the invite slot) was
        // linux's inline original; it is shared now because writing only
        // the legacy slot here is a silent loss on any app whose index is
        // already materialized (`onboarding.md` § Long-term store
        // contract — the `LoggedIn` terminal). Claim terminal #1 is in
        // there too: the helper spends the awaiting-DNS slot, whose row
        // is evaluated *before* the silent challenge and would otherwise
        // pin every later launch on "Almost ready" — at this terminal
        // and nowhere earlier (ratified 2026-09-21).
        let _ = fauna_client_accounts::persist_logged_in(
            registry, secret_hex, nest_url, None,
            // No reach hint on this path yet: linux reads the wizard's
            // captured address at its own `LoggedIn` terminal only once
            // its leg lands (`onboarding.md` § Reach hint). `None` is
            // the honest value and behaves exactly as today.
            None,
        );
    }
    // Claim terminal #3 (CR-1). The re-claim after a factory reset has
    // landed, so the pre-dispatch slot is spent. Leaving it set would pin
    // every future launch to the pre-filled claim surface for a box the
    // admin has already re-claimed — that row is evaluated before *all*
    // the others.
    clear_pending_factory_reset_slot(registry);
}

/// Route a Done step to the right post-wizard persistence action, keyed on
/// `wizard_outcome()`:
///   LoggedIn               → save nest_url; clear the pending-invite slot
///                            (first run only — [`persist_logged_in_terminal`])
///   (the pending-invite slot is written at the SUBMIT return instead —
///    `persist_pending_invite_slot`; that journey has no wizard exit)
///   AwaitingManualDns      → save the awaiting-manual-DNS slot
/// Called only from `handle_change` — the one place that knows the wizard's
/// mode — on the observer tick that sees `Done` (which then also performs the
/// window teardown / main-app launch or the append switch). Pages never call
/// it: they invoke machine mutators and let the observer route.
///
/// Returns the outcome so the caller can choose additional actions (e.g.
/// transition to the main app window).
fn handle_wizard_done(m: &Arc<OnboardingMachine>, append: bool) -> Option<WizardOutcome> {
    let outcome = m.wizard_outcome()?;
    match &outcome {
        WizardOutcome::LoggedIn {
            nest_url,
            handle: _handle,
        } => {
            // The home nest is recorded per-actor by moment 4 and nowhere else
            // (there is no single slot beside the registry any more). Callers
            // needing the secret at this terminal take it from
            // `effective_secret()` — see the `LoggedIn` arm in `handle_change`.
            persist_logged_in_terminal(
                &crate::account_registry(),
                m.effective_secret().as_deref(),
                nest_url.as_str(),
                append,
            );
        }
        WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        } => {
            // The per-actor registry slot the shared `LaunchMachine` reads
            // (`fauna/{actor_id}/awaiting_dns`) — NOT a client-local slot. This
            // is what lets `LaunchMachine::start()` route the relaunch to
            // `WizardAt{AwaitingManualDns}` on its own, with no pre-machine
            // branch here (`onboarding.md` § Long-term store contract,
            // *Mechanism*). The records are opaque JSON to both the registry and
            // the launch machine; only the wizard's seeder parses them back.
            let dns_records_json =
                serde_json::to_string(dns_records).unwrap_or_else(|_| "[]".into());
            persist_awaiting_dns(m, nest_url.as_str(), &dns_records_json, claim_code.as_str());
        }
    }
    Some(outcome)
}

/// Build the onboarding wizard window with the user's existing identity
/// pre-seeded (loaded from libsecret at app launch). Skips identity-creation
/// and lands the wizard on `HandleEntry`. See
/// `OnboardingMachine::seed_identity` for back-navigation semantics.
pub fn build_onboarding_window_with_seed(
    app: &adw::Application,
    secret_hex: &str,
) -> OnboardingResult {
    build_onboarding_window_inner(app, Some(secret_hex), false, None)
}

/// Build the wizard already parked on the "Almost ready" surface — the
/// deferred-DNS launch row (`onboarding.md` § App-launch routing).
///
/// The record is seeded **before the first render**, not after the window is
/// shown, and that ordering is load-bearing: seeding afterwards would render
/// `handle_entry` (where `seed_identity` leaves the machine) and then *slide
/// away from it*, flashing a page the user never visited — and leaving it
/// mapped for the length of the transition, since a `GtkStack` maps both
/// children while it animates.
pub fn build_onboarding_window_awaiting_dns(
    app: &adw::Application,
    secret_hex: &str,
    record: fauna_launch_machine::AwaitingDnsRecord,
) -> OnboardingResult {
    build_onboarding_window_inner(app, Some(secret_hex), false, Some(record))
}

/// Build the onboarding wizard window.
pub fn build_onboarding_window(app: &adw::Application) -> OnboardingResult {
    build_onboarding_window_inner(app, None, false, None)
}

/// Build the onboarding wizard in **append** ("Add account") mode: the same
/// create-or-import → handle → connect flow, but launched from a *running*
/// authenticated session to add another identity. Two behavioural differences
/// (both keyed off `append`): closing the wizard (X) must NOT quit the app (the
/// current session stays live), and on success the new identity is added to the
/// `AccountRegistry` and *switched to* (via `settings::trigger_switch_account`)
/// rather than booting a fresh single-identity main app (Staged plan,
/// Stage 1; tracked internally).
pub fn build_onboarding_window_append(app: &adw::Application) -> OnboardingResult {
    build_onboarding_window_inner(app, None, true, None)
}

/// `sync-agent.md` § Credential model → *The signed-out reconcile*, shape
/// (a). Best-effort and guarded — see
/// [`fauna_client_sync::agent::signed_out_onboarding_reconcile`] for the
/// actor-matched licensing that keeps a co-resident sibling account safe.
/// Pre-client / onboarding, so it runs on [`crate::async_helper::run_on_tokio`]'s
/// worker-thread runtime rather than a shared client handle that does not
/// exist yet.
fn spawn_signed_out_onboarding_reconcile() {
    let Ok(endpoint) = fauna_ipc::endpoint::AgentEndpoint::default_for_user() else {
        return;
    };
    crate::async_helper::run_on_tokio(
        async move {
            fauna_client_sync::agent::signed_out_onboarding_reconcile(&endpoint).await;
        },
        |()| {},
    );
}

fn build_onboarding_window_inner(
    app: &adw::Application,
    seed_secret: Option<&str>,
    append: bool,
    awaiting_dns: Option<fauna_launch_machine::AwaitingDnsRecord>,
) -> OnboardingResult {
    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let m = machine_glue::make_machine(tx);
    if let Some(secret) = seed_secret {
        m.seed_identity(secret.to_string());
    }
    // Resume state is seeded here, before the stack picks its first visible
    // child — see `build_onboarding_window_awaiting_dns` for why the ordering
    // matters. The records are opaque JSON in the slot; only this seeder parses
    // them back (a corrupt slot degrades to an empty record list, never a panic).
    if let Some(rec) = awaiting_dns {
        m.seed_awaiting_manual_dns_record(rec);
    }
    // `sync-agent.md` § Credential model → *The signed-out reconcile*, shape
    // (a): NOT for `append` — that mode launches from a *running*
    // authenticated session adding a second identity, and the agent may well
    // be serving that first, still-live account. Every other route here means
    // this app has no account, so nudge a reachable agent to drop its
    // capability now rather than wait for its own renewal-loop cadence.
    if !append {
        spawn_signed_out_onboarding_reconcile();
    }

    // ── Stack + per-page registry ───────────────────────────────────────
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    stack.set_transition_duration(200);

    // The resolved `recover-selfhosted-command`, shared between the page that
    // paints it and the page-entry read that fills it (the machine surfaces no
    // seed, so it is carried beside the machine — see the page's module doc).
    let recovery_command: recover_selfhosted_instructions::CommandCell =
        Rc::new(RefCell::new(None));

    let refreshers: RefreshMap = Rc::new(RefCell::new(HashMap::new()));
    for (name, build_fn) in pages(m.clone(), recovery_command.clone(), append) {
        let (page, refresh) = build_fn();
        stack.add_named(&page, Some(name));
        refreshers.borrow_mut().insert(name, refresh);
    }

    // Park the stack on its first page while nothing is mapped yet. A resumed
    // launch lands *directly* on its page: no animation away from a page the
    // user never visited, and no window in which the outgoing page is still
    // mapped. `handle_change`'s `entered` guard then sees the surface is already
    // showing and does not re-run the entry work.
    if is_awaiting_manual_dns(&m) {
        stack.set_visible_child_name(SURFACE_AWAITING_MANUAL_DNS);
        if let Some(refresh) = refreshers
            .borrow()
            .get(SURFACE_AWAITING_MANUAL_DNS)
            .cloned()
        {
            refresh();
        }
        // The relaunch path never enters `handle_change`'s surface branch (the
        // stack is already there), so the poll has to be armed here.
        start_awaiting_dns_poll(&m);
    } else if let Some(name) = step_to_name(m.step()) {
        stack.set_visible_child_name(name);
    }

    // ── Error banner (orchestrator-level, populated from m.error_message()) ──
    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Fill);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    error_label.add_css_class("error-banner");
    error_label.set_visible(false);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);

    // ── Layout ──────────────────────────────────────────────────────────
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let header_title = gtk::Label::new(Some(
        crate::i18n::strings::onboarding::identity_choice::TITLE,
    ));
    header.set_title_widget(Some(&header_title));
    header.set_show_end_title_buttons(true);
    header.set_show_start_title_buttons(true);

    outer.append(&header);
    outer.append(&error_label);
    outer.append(&stack);

    let window = adw::Window::builder()
        .title(crate::i18n::strings::onboarding::identity_choice::TITLE)
        .default_width(600)
        .default_height(550)
        .resizable(false)
        .content(&outer)
        .build();
    window.set_application(Some(app));

    // The app is permanently held alive by `app.hold()` in main.rs so
    // close-to-tray works on the authenticated main window. That same
    // hold means destroying the onboarding window would leave a headless
    // zombie process — there's no session to keep alive during
    // onboarding. Force a real quit when the user clicks X.
    //
    // Append ("Add account") mode is the exception: there IS a live
    // authenticated session behind this wizard, so closing the wizard (cancel)
    // must merely dismiss it, never quit the app. Just Proceed (destroy).
    {
        let app_for_quit = app.clone();
        window.connect_close_request(move |_| {
            if !append {
                app_for_quit.quit();
            }
            glib::Propagation::Proceed
        });
    }

    // One-shot: set when the wizard routes its terminal `Done` outcome, so the
    // route runs exactly once and the observer loop below stops instead of
    // spinning on a machine that stays `Done`. Shared by
    // the loop and the initial kick, which is why it is an `Rc<Cell<_>>` rather
    // than a local.
    let handed_off = Rc::new(std::cell::Cell::new(false));

    // ── Observer-driven navigation + refresh ────────────────────────────
    {
        let m = m.clone();
        let stack = stack.clone();
        let refreshers = refreshers.clone();
        let recovery_command = recovery_command.clone();
        let app = app.clone();
        let window = window.clone();
        let error_label = error_label.clone();
        let handed_off = Rc::clone(&handed_off);
        // A closed channel (sender dropped) shouldn't happen — the loop's exit
        // on it is exactly the "wizard tearing down" exit.
        crate::async_helper::spawn_wake_loop(rx, move || {
            handle_change(
                &m,
                &stack,
                &refreshers,
                &recovery_command,
                &error_label,
                &app,
                &window,
                append,
                &handed_off,
            );
            // The wizard has routed its terminal outcome and torn
            // itself down; there is nothing left to render. Stopping
            // here — rather than spinning on a `Done` machine that
            // keeps notifying — is what keeps the GTK main thread
            // free.
            if handed_off.get() {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    // Kick the first refresh so the initial step renders correctly.
    handle_change(
        &m,
        &stack,
        &refreshers,
        &recovery_command,
        &error_label,
        app,
        &window,
        append,
        &handed_off,
    );

    OnboardingResult {
        window,
        error_label,
        machine: m,
    }
}

/// Populate the `nest_recovery` box list on entering the page — the linux twin
/// of the web reference's `$effect` on `nest_recovery`
/// (`apps/fauna-web/src/routes/onboarding/+page.svelte`), per `box-recovery.md`
/// § Recovery UI (step 4).
///
/// One read, the shared pre-login resolver (`box-recovery.md` § The plane-era
/// recovery floor → *(b) The reads*): this device's own account store joined
/// with a cold read from the resolved nest when there is one — never either-or
/// on whether a nest URL is stored, since a surviving device's saved nest is,
/// in the case recovery exists for, the dead box. Async, off the GTK thread.
///
/// Best-effort. The list is only pushed into the machine when the read returns
/// **≥1 box**, so an empty or failed read never clobbers a list already on
/// screen (the launch entry's instant push, or the tier_2 `set_recovery_boxes`
/// injection). No identity yet ⇒ nothing to read with ⇒ no read.
fn fetch_recovery_boxes(m: &Arc<OnboardingMachine>) {
    let Some(secret) = m.effective_secret() else {
        return;
    };
    let node_url = Some(m.nest_url()).filter(|u| !u.is_empty());

    let m = m.clone();
    crate::async_helper::run_on_tokio(
        async move { crate::client::load_recoverable_boxes(node_url.as_deref(), &secret).await },
        move |boxes| {
            if !boxes.is_empty() {
                m.set_recovery_boxes(boxes);
            }
        },
    );
}

/// Resolve the selected box's `recover-selfhosted-command` on entry to
/// `recover_selfhosted_instructions`, through the same pre-login resolver as
/// [`fetch_recovery_boxes`] (this device's store joined with the resolved nest).
///
/// The command is the installer input the admin pastes, so it carries the box's
/// custodied **seed** — the one value the machine deliberately does not surface.
/// It therefore lands in the page's [`recover_selfhosted_instructions::CommandCell`]
/// rather than in machine state, and the page's own refresh paints it (the read
/// completes off-tick, so it must ask for that repaint itself).
///
/// Best-effort: no source custodying a seed for this box leaves the cell `None`
/// → the page keeps its pending placeholder. It must never show a command carrying the **wrong** box's seed — that rebuilds the box under
/// a different `nest_actor_id`, the exact trust break recovery exists to prevent.
fn fetch_selfhosted_command(
    m: &Arc<OnboardingMachine>,
    command: recover_selfhosted_instructions::CommandCell,
    refresh: Option<Rc<dyn Fn()>>,
) {
    let Some(secret) = m.effective_secret() else {
        return;
    };
    let Some(box_id) = m.recovery_selected_nest_id() else {
        return; // Unreachable: the method buttons are gated on a selection.
    };
    let node_url = Some(m.nest_url()).filter(|u| !u.is_empty());

    crate::async_helper::run_on_tokio(
        async move {
            crate::client::load_selfhosted_recovery_command(node_url.as_deref(), &secret, &box_id)
                .await
        },
        move |resolved| {
            if let Some(text) = resolved {
                *command.borrow_mut() = Some(text);
                if let Some(refresh) = refresh {
                    refresh();
                }
            }
        },
    );
}

/// One observer tick: read `m.step()`, swap stack page if changed, invoke
/// the active page's refresh, and mirror `m.error_message()` into the
/// orchestrator's banner. On `OnboardingStep::Done`, tear the wizard down.
#[allow(clippy::too_many_arguments)]
fn handle_change(
    m: &Arc<OnboardingMachine>,
    stack: &gtk::Stack,
    refreshers: &RefreshMap,
    recovery_command: &recover_selfhosted_instructions::CommandCell,
    error_label: &gtk::Label,
    app: &adw::Application,
    window: &adw::Window,
    append: bool,
    handed_off: &Rc<std::cell::Cell<bool>>,
) {
    // ── The hand-off latch ──
    // `OnboardingStep::Done` is a TERMINAL route, not a page: it launches the
    // main app (or switches accounts, or dismisses the wizard) and destroys the
    // wizard window. But the machine stays `Done` and the observer keeps
    // notifying, so without this latch every subsequent tick re-ran the whole
    // terminal route — measured at ~840 ticks/s, each one re-writing the
    // credential trio and then bailing out of `launch_main_app_after_signin` at
    // its `onboarding_window.application()` guard (63,613 entries, exactly 1 of
    // which got as far as starting the LaunchMachine).
    //
    // That busy loop is what pegged the GTK main thread at ~100% of a core and
    // starved the e2e element drain, surfacing as `agent timeout — the UI thread
    // did not reply within 25s` on every claim journey — and it is the
    // same "dismisses the wizard, in a hot loop" that row 23 measured on the
    // append branch. Re-running terminal routing after the app has already
    // launched is wrong on its own terms too: it re-persists credentials and
    // re-adds the account on every tick.
    //
    // The latch is the fix; the loop in `build_onboarding_window_with_seed` also
    // breaks on it so the observer stops entirely. (The pre-existing
    // consume-once guard on `take_trust_prompt_granted` — "so a handoff that
    // runs twice mints once" — shows the re-entry was known; it defended one
    // symptom rather than the routing.)
    if handed_off.get() {
        return;
    }

    // ── Loop witness ─────────────────────────────
    // This function is the wizard's render pump, driven from the machine's
    // `on_changed` observer through the `spawn_local` loop in
    // `build_onboarding_window_with_seed`. A render that mutates the machine
    // re-notifies, so a runaway re-entry here spins the GTK main thread — and a
    // spinning main thread starves the e2e element drain, surfacing as
    // `agent timeout — the UI thread did not reply within 25s` (a product-bug
    // costume, e2e-conventions.md § point 13).
    //
    // Measured 2026-08-17: on the claim journey the main thread goes state=R
    // and burns CPU continuously from the moment the wizard opens, never
    // recovering. A counter is the only thing that can tell the two mechanisms
    // apart — one call that never returns (no matching exit) versus thousands of
    // fast calls (a re-notify ping-pong) — so it is deliberately permanent.
    // Rate-limited so a tight loop cannot flood the log it is diagnosing.
    static HANDLE_CHANGE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let call = HANDLE_CHANGE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let loud = call < 20 || call.is_multiple_of(1000);
    if loud {
        tracing::info!(call, step = ?m.step(), "[onboarding] handle_change enter");
    }
    // Exit via a guard so the early returns below are covered too: an `enter`
    // with no `exit` is the "one call never returns" verdict.
    struct ExitWitness(u64, bool);
    impl Drop for ExitWitness {
        fn drop(&mut self) {
            if self.1 {
                tracing::info!(call = self.0, "[onboarding] handle_change exit");
            }
        }
    }
    let _exit_witness = ExitWitness(call, loud);

    // ── Banner: surface error_message() on every tick. ─────────────────
    // Nothing else rides this banner: the sign-out residue, which once did (as
    // a "sticky notice" re-applied here, because a line painted once was wiped
    // by this very render on the next tick), has its own `sign-out-residue`
    // view on `identity_choice` since 2026-09-25, painted from state the
    // wizard does not own (`account_scope::sign_out_residue`).
    let msg = m.error_message().filter(|msg| !msg.is_empty());
    crate::settings::render_error_label(error_label, msg.as_deref());

    // ── The "Almost ready" surface ──────────────────────────────────────
    // Keyed on `wizard_outcome()`, not on `m.step()` (which is `Done` here),
    // and checked *ahead of* the step match so that the same-session exit and
    // the relaunch hydration paint the identical page (`onboarding.md`
    // § "Almost ready" surface). Without this branch `Done` would fall into the
    // teardown arm below — which is what used to quit the app outright on the
    // deferred-DNS exit.
    if is_awaiting_manual_dns(m) {
        let entered = stack.visible_child_name().as_deref() != Some(SURFACE_AWAITING_MANUAL_DNS);
        if entered {
            // Same-session exit: this is what writes the resume slot. On the
            // relaunch-hydration path the record written back is the one we
            // were just seeded from, so the write is a self-healing no-op.
            handle_wizard_done(m, append);
            stack.set_visible_child_name(SURFACE_AWAITING_MANUAL_DNS);
            start_awaiting_dns_poll(m);
        }
        if let Some(refresh) = refreshers
            .borrow()
            .get(SURFACE_AWAITING_MANUAL_DNS)
            .cloned()
        {
            refresh();
        }
        return;
    }

    // ── Step routing ────────────────────────────────────────────────────
    match step_to_name(m.step()) {
        Some(name) => {
            // Entry into a page, as opposed to a same-page observer tick (the
            // machine notifies on every mutation). The box-list fetch below is
            // keyed off this so it fires once per visit, not once per tick —
            // and so `set_recovery_boxes`'s own notification can't re-enter it.
            let entered = stack.visible_child_name().as_deref() != Some(name);
            if entered {
                stack.set_visible_child_name(name);
            }
            if let Some(refresh) = refreshers.borrow().get(name).cloned() {
                refresh();
            }
            if entered && name == STEP_INVITE_REQUEST {
                // One timer per page entry — covers BOTH ways `PendingReview` is
                // reached (the relaunch hydration lands here already in it; the
                // same-session submit transitions into it with the page already
                // entered). The timer itself guards on the state.
                start_pending_invite_poll(m);
            }
            if entered && name == STEP_NEST_RECOVERY {
                fetch_recovery_boxes(m);
            }
            if entered && name == STEP_RECOVER_SELFHOSTED_INSTRUCTIONS {
                let refresh = refreshers.borrow().get(name).cloned();
                fetch_selfhosted_command(m, recovery_command.clone(), refresh);
            }
        }
        None => {
            // OnboardingStep::Done — wizard finished.
            // Persist credentials / pending-invite slot per the outcome,
            // then tear the window down or launch the main app.
            //
            // Latch BEFORE routing, not after: every arm below is terminal (it
            // launches, switches, closes or quits), and `handle_wizard_done`
            // itself persists credentials and mutates the account registry, so a
            // re-entry must be refused even if an arm re-enters this function
            // synchronously. See the latch's note at the top of `handle_change`.
            // `AwaitingManualDns` never reaches here — it returns from the
            // outcome-keyed branch above, which is why that surface keeps polling.
            handed_off.set(true);
            // Breadcrumb, same contract as the `[launch]` trail in
            // `launch_main_app_after_signin`: this route is terminal and two of
            // its arms below quit the app outright, so a run that dies here must
            // say so.
            tracing::info!("[onboarding] wizard reached Done; routing terminal outcome");
            match handle_wizard_done(m, append) {
                Some(WizardOutcome::LoggedIn {
                    nest_url,
                    handle: _,
                }) => {
                    // Outside append mode `handle_wizard_done` has run moment 4
                    // (`persist_logged_in`, per-actor: the home nest, activation,
                    // the spent resume slots); in append mode it wrote nothing and
                    // the append arm below registers and switches. The secret
                    // below is read off the machine, not the store — see the note
                    // at `device_id` further down.
                    // Slice 4 (dns-management.md § Where the credential lives /
                    // onboarding.md §4): capture the onboarding-verified
                    // DNS-provider credential *before* the machine is torn down,
                    // so the launched client can seal it into DnsConfig. This
                    // is `Some` only on the managed-publish path where the DNS step
                    // verified a provider credential; `None` on manual /
                    // set-up-later / returning-user paths.
                    let captured_dns = m.captured_dns_credential();
                    // T0: capture the onboarding-enable-email intent before the
                    // machine is torn down. The launched (authed) client fires
                    // set_mail_enabled(true) — see launch_main_app_after_signin.
                    let enable_email = m.email_enable_requested();
                    // Feature D: same for the sibling enable-caldav intent. CalDAV
                    // gates independently of email, so this is captured + fired
                    // separately (set_caldav_enabled(true)).
                    let enable_caldav = m.caldav_enable_requested();
                    // Contacts sibling: the enable-carddav intent, captured +
                    // fired separately (set_carddav_enabled(true)) — CardDAV
                    // gates independently of both email and calendar.
                    let enable_carddav = m.carddav_enable_requested();
                    // Files sibling: the enable-webdav intent, captured +
                    // fired separately (set_webdav_enabled(true)) — WebDAV
                    // gates independently of email, calendar, and contacts.
                    let enable_webdav = m.webdav_enable_requested();
                    // Same capture-before-adopt shape for the `trust_prompt`
                    // answer (`onboarding.md` § 3b-ter). Consume-once, so a
                    // handoff that runs twice mints once; `false` when the user
                    // skipped or was never asked.
                    let trust_granted = m.take_trust_prompt_granted();
                    // Same capture-before-teardown shape for the kit the user
                    // confirmed on the `recovery_kit` screen — minted there but
                    // deliberately unregistered until a signed-in connection
                    // exists (`identity-succession.md` § The RecoveryKey →
                    // *Creation UX*). Consume-once; `None` if skipped.
                    let pending_kit = m.take_pending_recovery_secret();
                    // And the predecessor seeds a phrase-only restore recovered
                    // from the escrow blob's additive section — empty on every
                    // ordinary onboarding, the only copies left anywhere when
                    // not (`identity-succession.md` § Seed escrow).
                    let restored_predecessors = m.restored_predecessors();
                    // The secret comes from the MACHINE, not the store: at
                    // this moment the wizard has just authenticated with it, so
                    // `effective_secret()` always has it, while the store
                    // has it only if moment 1's write landed. It can fail to
                    // land — `commit_confirmed_identity` is log-only over a
                    // `SecretStore::set` that is infallible *by signature*, so a
                    // keyring that silently kept nothing arrives here with a live
                    // authenticated session and an empty slot. Reading the store
                    // first made that case tear the wizard down, and on the
                    // non-append path `connect_close_request` turns that into a
                    // process quit: the user watches the app vanish at the exact
                    // moment onboarding succeeded. tui
                    // (`wizard/mod.rs::handle_wizard_done`) and android
                    // (`OnboardingHost.handleWizardExit`) already read the
                    // machine here; taking the store instead is what made linux
                    // the only app of seven whose `LoggedIn` terminal could kill
                    // the app — the per-app-divergence shape `onboarding.md`
                    // § Wizard exit handling deleted for `InviteSubmitted`.
                    //
                    // `device_id` still comes from the store — this identity's
                    // own per-actor slot, which `handle_wizard_done`'s moment 4
                    // has just registered (in append mode nothing has yet, so the
                    // slot is absent) — and EMPTY IS THE ORDINARY CASE here,
                    // not a fault: moment 1 is `persist_confirmed_identity`,
                    // which calls `add_account(secret, None, None)` and writes no
                    // device id at all (linux's sync device id lives in
                    // `sync::device_id`'s file store, not this slot).
                    let device_id = m
                        .effective_secret()
                        .and_then(|secret| {
                            fauna_core::identity::ActorKeypair::from_secret_hex(&secret).ok()
                        })
                        .and_then(|kp| crate::account_registry().secrets(&kp.actor_id_hex()))
                        .and_then(|stored| stored.device_id)
                        .unwrap_or_default();
                    if let Some(secret_hex) = m.effective_secret() {
                        if append {
                            // Append ("Add account") mode: a live session is
                            // already running. Register the new identity in the
                            // shared AccountRegistry and switch to it — the switch
                            // handler tears down the current window + this wizard
                            // (destroy(), so no app.quit()) and rebuilds the
                            // authenticated session for the new active account.
                            // This reuses the *whole* switch path (design Stage 1);
                            // no second single-identity boot. The onboarding-only
                            // provisioning glue (DNS / enable-email / deployment
                            // seed) does not apply to adding a returning identity.
                            let registry = crate::account_registry();
                            match registry.add_account(
                                &secret_hex,
                                Some(nest_url.as_str()),
                                Some(device_id.as_str()),
                            ) {
                                Ok(new_actor) => {
                                    // Before the switch, and linked to the
                                    // added identity by name — it is not
                                    // active until the switch lands.
                                    persist_restored_predecessors(
                                        Some(&new_actor),
                                        &restored_predecessors,
                                    );
                                    if pending_kit.is_some() {
                                        // tui's append handoff drops it the
                                        // same way: the kit registers only on
                                        // the first-sign-in launch below, and
                                        // Settings says never-created.
                                        tracing::warn!(
                                            "[onboarding] add-account: the confirmed \
                                             recovery kit is not registered on this path"
                                        );
                                    }
                                    // `confirmed = false`: this path ran no re-auth
                                    // prompt, so it must not assert one. A
                                    // freshly-added account is unflagged by default,
                                    // so plain `set_active` takes it; were it somehow
                                    // flagged, the registry refusing is exactly the
                                    // intended fail-closed outcome (Stage 2,
                                    // long-term-store.md § Multi-account evolution).
                                    crate::settings::trigger_switch_account(new_actor, false);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "[onboarding] add_account failed: {e:#}; dismissing wizard"
                                    );
                                    window.close();
                                }
                            }
                        } else {
                            // Moment 4 has added and activated the restored
                            // identity, so the links name it.
                            let restored_actor =
                                fauna_core::identity::ActorKeypair::from_secret_hex(&secret_hex)
                                    .ok()
                                    .map(|kp| kp.actor_id_hex());
                            persist_restored_predecessors(
                                restored_actor.as_deref(),
                                &restored_predecessors,
                            );
                            launch_main_app_after_signin(
                                window,
                                &nest_url,
                                &secret_hex,
                                captured_dns,
                                enable_email,
                                enable_caldav,
                                enable_carddav,
                                enable_webdav,
                                trust_granted,
                                pending_kit,
                            );
                        }
                    } else {
                        // Unreachable by construction: `LoggedIn` is produced
                        // only after an identity authenticated, so the machine
                        // holds `imported_secret` or `generated_secret` (see
                        // `OnboardingMachine::effective_secret`). Kept as a loud
                        // arm rather than an `expect` because the teardown is
                        // destructive — for a non-append run `window.close()`
                        // alone quits the app through `connect_close_request` —
                        // so a future outcome that reached `Done` with no
                        // identity must announce itself here, not panic on a
                        // user's box.
                        tracing::error!(
                            "[onboarding] wizard done with LoggedIn but the machine has no \
                             effective secret — tearing the wizard down (append={append})"
                        );
                        window.close();
                        if !append {
                            app.quit();
                        }
                    }
                }
                // ⚠ There is deliberately no pending-invite arm here. Until
                // 2026-08-12 an `InviteSubmitted` exit landed in this branch and
                // **quit the app** — one of the five divergent per-app behaviors
                // `onboarding.md` § Wizard exit handling deletes. That journey
                // now has no exit at all: the wizard stays on `invite_request`
                // and polls, so it never reaches this teardown.
                //
                // `AwaitingManualDns` is likewise NOT here: it is not a teardown
                // either. It renders the "Almost ready" surface and keeps
                // polling, handled by the outcome-keyed branch above.
                Some(WizardOutcome::AwaitingManualDns { .. }) => {
                    // Unreachable: the outcome-keyed branch at the top of
                    // `handle_change` returns before the step match. Spelled out
                    // as its own arm rather than folded into a catch-all so that
                    // deleting the surface is a loud failure here, not a silent
                    // return to quitting the app on the deferred-DNS exit.
                    tracing::error!(
                        "[onboarding] AwaitingManualDns reached the teardown arm — \
                         the 'Almost ready' surface branch did not run"
                    );
                }
                None => {
                    // No outcome — should not happen; close defensively (keep the
                    // running app alive in append mode). Loud for the same reason
                    // as the no-credentials arm above: a non-append run quits here.
                    tracing::error!(
                        "[onboarding] wizard reached Done with NO outcome — \
                         tearing the wizard down (append={append})"
                    );
                    window.close();
                    if !append {
                        app.quit();
                    }
                }
            }
        }
    }
}

/// Persist the predecessor seeds a phrase-only restore recovered, linked to the
/// identity they are predecessors of — the registry's own shared body
/// (`AccountRegistry::persist_restored_predecessors`, tui's handoff too).
fn persist_restored_predecessors(
    restored_actor: Option<&str>,
    predecessors: &[fauna_onboarding_machine::nest_api::RestoredPredecessorSeed],
) {
    if predecessors.is_empty() {
        return;
    }
    crate::account_registry().persist_restored_predecessors(
        restored_actor,
        predecessors
            .iter()
            .map(|p| (p.seed_hex.as_str(), p.actor_id_hex.as_str())),
    );
}

// ---------------------------------------------------------------------------
// Persisted-credential launch path — called by handle_change() when the
// wizard reaches Done with WizardOutcome::LoggedIn.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn launch_main_app_after_signin(
    onboarding_window: &adw::Window,
    nest_url: &str,
    secret_hex: &str,
    captured_dns: Option<fauna_onboarding_machine::CapturedDnsCredential>,
    enable_email: bool,
    enable_caldav: bool,
    enable_carddav: bool,
    enable_webdav: bool,
    trust_granted: bool,
    pending_kit: Option<fauna_core::secret::SecretString>,
) {
    // ⚠ The breadcrumbs through this function and `finish_launch_after_signin`
    // are load-bearing, not debug leftovers. Until 2026-08-17 this entire
    // wizard→main-app transition emitted **not one** `tracing` event: the prescribed next step was "raise the app's log
    // level and find the last thing the main thread does", and there was
    // nothing to raise. A transition that can wedge the GTK main loop must say
    // where it got to. Keep them, and add one beside any new step.
    tracing::info!("[launch] post-wizard transition: credentials registered by moment 4");

    let app = match onboarding_window
        .application()
        .and_then(|a| a.downcast::<adw::Application>().ok())
    {
        Some(a) => a,
        None => return,
    };

    onboarding_window.destroy();

    // LaunchMachine: construct fresh, then drive `start()` to Online before
    // FaunaClient consumes it for token refresh. The wizard just authenticated
    // via OnboardingMachine; the LaunchMachine re-runs silent challenge against
    // the just-registered credentials — wasteful but correct, and the
    // post-wizard transition is rare. `handle_wizard_done`'s moment 4
    // (`persist_logged_in`) registered the identity per-actor and made it
    // active, so this launch routes on that account.
    // See docs/goal/architecture/long-term-store.md § Shared seam.
    //
    // **`start()` runs OFF the GTK main thread** (`run_on_tokio`), and the rest
    // of the launch is the continuation. It used to be a `block_on` on a
    // runtime built and dropped *on the main thread* — a real hazard, since the
    // e2e agent drains its commands from a `glib::timeout_add_local` tick on
    // that same thread (`main.rs` `start_test_agent_if_enabled`), and
    // `Runtime::drop` joins the blocking pool (tokio resolves DNS on
    // `spawn_blocking(getaddrinfo)`), so the drop half has no bound. This now
    // matches the cold-launch path in `main.rs`, which has always driven
    // `LaunchMachine::start` this way. The ordering the old code depended on is
    // preserved exactly: `store_credentials` still runs first, and the machine
    // is still Online before `FaunaClient::new` receives it — only the
    // *waiting* moved.
    //
    // ⚠ **This did NOT fix the standing hang** — do not re-derive that guess.
    // It named this `block_on` as its located hypothesis for the standing
    // `agent timeout — the UI thread did not reply within 25s` on every
    // claim→app journey. Measured 2026-08-16: with all three such sites moved
    // off the main thread, the failing set was byte-for-byte unchanged (3
    // `test_mail_enable_at_admin_claim` + 2 `test_trust_prompt`), and the
    // drop-duration detector added to `async_helper` the same day never fired,
    // so the unbounded-drop half was never happening either. The blocking work
    // is somewhere else and is still unlocated. Keep this shape — it is correct
    // and uniform — but look elsewhere for it.
    //
    // The app survives the gap with no window: `app.hold()` in `main.rs` holds
    // the GApplication alive for the process's lifetime.
    let machine = fauna_launch_machine::LaunchMachine::new(
        std::sync::Arc::new(fauna_launch_machine::NullObserver),
        std::sync::Arc::new(crate::account_registry().launch_persistence()),
    );
    let machine_for_start = std::sync::Arc::clone(&machine);
    let nest_url = nest_url.to_string();
    let secret_hex = secret_hex.to_string();
    tracing::info!("[launch] driving LaunchMachine::start off the GTK thread");
    crate::async_helper::run_on_tokio(async move { machine_for_start.start().await }, move |()| {
        tracing::info!(
            "[launch] LaunchMachine::start returned; entering finish_launch_after_signin"
        );
        finish_launch_after_signin(
            app,
            machine,
            &nest_url,
            &secret_hex,
            captured_dns,
            enable_email,
            enable_caldav,
            enable_carddav,
            enable_webdav,
            trust_granted,
            pending_kit,
        );
    });
}

/// The post-`start()` half of [`launch_main_app_after_signin`] — everything that
/// needs the `LaunchMachine` already Online. Runs on the GTK main thread, as the
/// continuation of the off-thread silent challenge.
#[allow(clippy::too_many_arguments)]
fn finish_launch_after_signin(
    app: adw::Application,
    machine: std::sync::Arc<fauna_launch_machine::LaunchMachine>,
    nest_url: &str,
    secret_hex: &str,
    captured_dns: Option<fauna_onboarding_machine::CapturedDnsCredential>,
    enable_email: bool,
    enable_caldav: bool,
    enable_carddav: bool,
    enable_webdav: bool,
    trust_granted: bool,
    pending_kit: Option<fauna_core::secret::SecretString>,
) {
    // One funnel for every authenticated launch: `crate::launch_authenticated`
    // builds the client, runs the post-auth hooks (autostart, `authenticate`, and
    // the silent sign-in that fills the account cache and so gives the send rail
    // its `<handle>@<domain>`), builds the window and pumps its messages. The
    // first-setup glue below rides its `first_setup` hook — on the authenticated
    // client, before the window is built, the order it has always run in.
    //
    // ⚠ This transition used to build its own client, window and pump, and so
    // skipped the silent sign-in: an account signed in through the wizard (every
    // UI claim included) refused every send with `no_handle` until the app was
    // relaunched, and never registered autostart, tracked window focus, or wired
    // close-to-tray.
    let result =
        crate::launch_authenticated(&app, nest_url, secret_hex, machine, move |fauna_client| {
            tracing::info!("[launch] FaunaClient built; authenticate() dispatched");

            // Slice 4 (onboarding launch glue): seal the onboarding-captured DNS
            // credential into the admin's DNS record (`fauna.state.dns`) now that we
            // have an authenticated client — the store waits for the account runtime
            // this sign-in starts. `dns_put_credentials` re-runs `verify()` to
            // (re)derive the covered zones — so there is one store, one writer (the
            // `DnsManagementMachine`), no second config-put path in onboarding (priority
            // #2). See `docs/goal/behavior/dns-management.md` § Where the credential lives.
            //
            // This call site keeps linux's own `dns_put_credentials` rather than the
            // shared `fauna_client_dns::dispatch_seal_captured_dns_credential` tui uses:
            // the live `FaunaClient` already holds a built DNS machine, so going through
            // it reuses that one instead of standing a second up, and it keeps the
            // trailing `VerifyRecords` refresh the page wants. The rule the two share —
            // map the captured fields, pass the machine's own label through — is the
            // shared helper's; `cred.label` is that label, and passing it was the fix:
            // `dns_put_credentials` used to substitute the bare provider id, so a linux
            // admin saw "cloudflare" where every other app shows "cloudflare
            // (example.com)".
            if let Some(cred) = captured_dns {
                let fields = cred
                    .fields
                    .into_iter()
                    .map(|(id, value)| fauna_client_dns::DnsCredentialField { id, value })
                    .collect();
                fauna_client.dns_put_credentials(cred.provider_id, fields, cred.label);
            }

            // onboarding.md § 3b-ter: the one-tap trust answer latched on the
            // `trust_prompt` page. The page only asks — minting needs this
            // authenticated session and the nest's own roster — so the mint runs here.
            // Best-effort and log-only by design: a failure must not paint an error over a completed
            // onboarding, and the same trust is grantable any time from Settings →
            // Nests. WHICH grants is not decided here either: `MintDefaultSet` mints
            // exactly what the shared catalog derives, so this glue holds no policy
            // that could drift from the Nests page's own picker (and an empty set on a
            // box with nothing enrolled yet is an honest no-op, not an error).
            if trust_granted {
                fauna_client.mint_default_trust_set();
            }

            // The kit confirmed on the `recovery_kit` screen, registered now
            // that this session is authenticated — the same deferral as the
            // two above, through the shared body tui's handoff calls.
            if let Some(kit_hex) = pending_kit {
                fauna_client.register_deferred_recovery_kit(kit_hex);
            }

            // The post-claim serving enablement — ONE shared step for every
            // `LoggedIn` route (`onboarding.md` § 3b *Mechanism*). ⚠ "First setup"
            // is carried by the four `*_enable_requested()` intents, NOT by this
            // call site: this whole function runs at `WizardOutcome::LoggedIn`,
            // which a returning admin's plain **sign-in** reaches too (a relaunch
            // skips the wizard entirely, but a sign-in does not). The machine's
            // § 3b claim axis is what makes the intents false on that route —
            // `onboarding.md` § 3b *Gating rule*. The shared step still runs the
            // first-setup mail provision on every route, which is the new-user
            // mailbox auto-mint on invite redemption (`mail-credentials.md`
            // § Auto-enable for new users), and publishes its completion anchor
            // (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`) whatever it decided.
            tracing::info!(
                enable_email,
                enable_caldav,
                enable_carddav,
                enable_webdav,
                "[launch] post-claim serving enablement dispatched; building the main window"
            );
            fauna_client.apply_post_claim_serving_enablement(
                fauna_client_mail_settings::serving_enablement::ServingEnablementIntents {
                    email: enable_email,
                    caldav: enable_caldav,
                    carddav: enable_carddav,
                    webdav: enable_webdav,
                },
            );
        });
    tracing::info!("[launch] main window built");

    // E2E: re-point the test-command drain's cells at this post-onboarding
    // window so state-protocol nav (`navigate_to`) drives this window's stack.
    // Without it the believable onboarding flow reaches the feed but can't
    // navigate to mail-settings / conversations. No-op for real users (the
    // registry is only populated when the e2e agent/bridge is wired).
    crate::rebind_active_window(
        result.stack.clone(),
        result.state.clone(),
        result.error_label.clone(),
        result.warning_label.clone(),
        result.info_label.clone(),
        std::rc::Rc::clone(&result.client),
        std::rc::Rc::clone(&result.open_profile),
    );
    // Shown the way the account-switch launch shows its window: the user is
    // driving this session, so it is never an autostart-hidden launch.
    crate::show_window(&result.window);
    tracing::info!("[launch] post-wizard transition complete — main window presented");
}

#[cfg(test)]
mod tests {
    use super::persist_logged_in_terminal;

    const LIVE_SECRET: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const ADDED_SECRET: &str = "0202020202020202020202020202020202020202020202020202020202020202";
    const NEST: &str = "https://nest.example";

    /// A registry over a temp dir holding one live, active account.
    fn registry_with_live_account(
        tmp: &tempfile::TempDir,
    ) -> (fauna_client_accounts::AccountRegistry, String) {
        let registry = crate::account_registry_in(tmp.path().join("credentials"));
        let live = registry
            .add_account(LIVE_SECRET, Some(NEST), None)
            .expect("live account");
        registry.set_active(&live).expect("activate live account");
        (registry, live)
    }

    /// `onboarding.md` § Long-term store contract → *Append mode is exempt*:
    /// the "Add account" wizard's `LoggedIn` terminal must not move `active`
    /// off the live account — its own add + switch does the adoption, after.
    #[test]
    fn append_logged_in_terminal_never_moves_active() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, live) = registry_with_live_account(&tmp);
        registry.set_pending_factory_reset_json(&live, "{}");

        persist_logged_in_terminal(&registry, Some(ADDED_SECRET), NEST, true);

        assert_eq!(registry.active().as_deref(), Some(live.as_str()));
        assert_eq!(registry.list().len(), 1, "nothing registered yet");
        assert!(
            registry.pending_factory_reset_json(&live).is_some(),
            "the live account's slot is not this wizard's to spend"
        );
    }

    #[test]
    fn first_run_logged_in_terminal_registers_and_activates() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, _live) = registry_with_live_account(&tmp);

        persist_logged_in_terminal(&registry, Some(ADDED_SECRET), NEST, false);

        let added = fauna_core::identity::ActorKeypair::from_secret_hex(ADDED_SECRET)
            .unwrap()
            .actor_id_hex();
        assert_eq!(registry.active().as_deref(), Some(added.as_str()));
    }
}
