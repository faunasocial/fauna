//! The onboarding wizard: a paint shell over the shared `OnboardingMachine`.
//!
//! `onboarding.md` § Goal — "the client is a thin observer-driven shell over
//! `OnboardingMachine`. The machine owns all decision-making; the client
//! renders snapshots and forwards user gestures." So this module holds no
//! per-stage view-model state and never inspects machine state to infer an
//! outcome (`onboarding.md:375`) — it reads snapshots, calls mutators, and
//! routes on `wizard_outcome()`.
//!
//! **The element list is the single source.** Ratatui is immediate-mode, so
//! unlike linux (a persistent GTK widget tree with a per-page refresh closure)
//! each page here declares its ui.yaml elements once, as an ordered
//! [`Element`] list rebuilt from the current snapshot. Paint, the automation
//! registry, and the keyboard focus ring all consume that one list, so a
//! painted element is automatable and focusable by construction — the "no
//! invisible shim elements" rule holds without a second table to keep in sync.

pub mod awaiting_manual_dns;
pub mod claim_code;
pub mod dns_config;
pub mod dns_post_instructions;
pub mod handle_entry;
pub mod identity_choice;
pub mod identity_created;
pub mod identity_import;
pub mod invite_request;
pub mod nat_mode_choice;
pub mod nest_provisioning;
pub mod nest_recovery;
pub mod nest_retire;
pub mod recover_selfhosted_instructions;
pub mod recovery_entry;
pub mod recovery_kit;
pub mod trust_prompt;
pub mod vps_config;

use std::collections::HashMap;
use std::sync::Arc;

use fauna_client_accounts::{AccountRegistry, SecretStore};
use fauna_credential_store::CredentialStore;
use fauna_i18n::strings::onboarding::done as t;
use fauna_onboarding_machine::{OnboardingMachine, OnboardingObserver, OnboardingStep};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{DataMessage, UiMessage};

// The painted-element model lives in `crate::element` — it serves the
// authenticated pages too, so `use crate::wizard::Element` from the feed page
// would be nonsense to a cold reader. Re-exported here so this module's 14 page
// files keep importing it through `use super::{…}`.
pub use crate::element::{Element, Field, SelectTarget};

/// One field of the WHOIS contact form (`onboarding.md:221`). The machine takes
/// the whole `ContactInfo` at once (`set_contact`), so an edit reads the current
/// contact, replaces one field, and writes it back — see [`Wizard::set_field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactField {
    FirstName,
    LastName,
    Email,
    Phone,
    Address1,
    City,
    State,
    PostalCode,
    Country,
}

impl ContactField {
    /// The nine fields in ui.yaml's declared order.
    pub const ALL: [ContactField; 9] = [
        ContactField::FirstName,
        ContactField::LastName,
        ContactField::Email,
        ContactField::Phone,
        ContactField::Address1,
        ContactField::City,
        ContactField::State,
        ContactField::PostalCode,
        ContactField::Country,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ContactField::FirstName => "dns-contact-first-name-input",
            ContactField::LastName => "dns-contact-last-name-input",
            ContactField::Email => "dns-contact-email-input",
            ContactField::Phone => "dns-contact-phone-input",
            ContactField::Address1 => "dns-contact-address1-input",
            ContactField::City => "dns-contact-city-input",
            ContactField::State => "dns-contact-state-input",
            ContactField::PostalCode => "dns-contact-postal-code-input",
            ContactField::Country => "dns-contact-country-input",
        }
    }

    fn get(self, c: &fauna_provisioning::registrar::ContactInfo) -> &str {
        match self {
            ContactField::FirstName => &c.first_name,
            ContactField::LastName => &c.last_name,
            ContactField::Email => &c.email,
            ContactField::Phone => &c.phone,
            ContactField::Address1 => &c.address1,
            ContactField::City => &c.city,
            ContactField::State => &c.state,
            ContactField::PostalCode => &c.postal_code,
            ContactField::Country => &c.country,
        }
    }

    fn set(self, c: &mut fauna_provisioning::registrar::ContactInfo, value: String) {
        match self {
            ContactField::FirstName => c.first_name = value,
            ContactField::LastName => c.last_name = value,
            ContactField::Email => c.email = value,
            ContactField::Phone => c.phone = value,
            ContactField::Address1 => c.address1 = value,
            ContactField::City => c.city = value,
            ContactField::State => c.state = value,
            ContactField::PostalCode => c.postal_code = value,
            ContactField::Country => c.country = value,
        }
    }
}

/// An editable field's identity — what `/element/type` and `/element/clear`
/// write, and what a keystroke into a focused [`Role::Input`] appends to.
///
/// The machine owns the handle text (`set_current_handle` / `current_handle()`,
/// exactly as linux mirrors it into its `gtk::Entry`); the rest are page-local
/// buffers the client holds until it hands them to a mutator.
///
/// The provider-credential variants carry the provider's own field id (from
/// `visible_dns_fields()` / `visible_vps_fields()`), so the set is per-provider
/// and cannot be a fixed enum. They keep a local buffer *and* push through to
/// the machine on every edit — the same shape as linux's
/// `entry.connect_changed → set_dns_cred`. The buffer is what the input paints,
/// which matters because the machine stores creds as `SecretString` and will
/// not hand a value back for display.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WizardField {
    Handle,
    ClaimCode,
    ImportSecret,
    InviteCode,
    /// `recovery-entry-phrase-field` — the pasted recovery kit (URI or 64-hex).
    RecoveryPhrase,
    /// `recovery-entry-account-field` — the account being recovered, when the
    /// payload names none.
    RecoveryAccount,
    DnsCred(String),
    VpsCred(String),
    Contact(ContactField),
    /// A `nest_retire` credential field (`vps-credentials-form-<id>`, reused
    /// from `vps_config`) — pushed to the retire machine, not the wizard's.
    RetireCred(String),
    /// `retire-confirm-name-input` — the typed-name gate.
    RetireConfirmName,
}

/// A wizard gesture. Every one maps 1:1 onto a machine mutator — the client
/// adds no routing rules of its own (`onboarding.md` § Architectural rules:
/// "per-stage code only calls machine mutators, never navigates directly").
#[derive(Debug, Clone)]
pub enum Action {
    BeginCreateIdentity,
    BeginImportIdentity,
    ConfirmGeneratedIdentity,
    ConfirmImportedIdentity,
    /// `recovery-kit-confirm-button` — the phrase is saved; the machine keeps
    /// the pending root for the signed-in handoff.
    ConfirmRecoveryKit,
    /// `recovery-kit-skip-button` — decline the kit; the root is dropped.
    SkipRecoveryKit,
    /// Client-local like [`Action::CopySecret`]: the kit phrase via OSC 52.
    CopyRecoveryKitSecret,
    /// `trust-box-grant-button` — one tap: trust this box with the default
    /// set. The machine latches the answer; the mint itself runs at the
    /// signed-in handoff (`onboarding.md` § 3b-ter).
    GrantDefaultTrust,
    /// `trust-box-skip-button` — decline; leaves everything as today.
    SkipTrustPrompt,
    /// `restore-from-recovery-kit-button` on `identity_choice` — the phrase-only
    /// IDENTITY restore. Distinct from [`Action::BeginRecoverLostBox`], which
    /// starts the total-box-loss NEST recovery.
    BeginRecoveryEntry,
    /// `recovery-entry-submit-button` — run the pre-identity escrow restore.
    SubmitRecoveryEntry,
    StartHandleCheck,
    SetControlCheckbox(bool),
    SubmitHandleCheckContinue,
    SubmitClaimCode,
    SubmitInviteRequest,
    RecheckInviteStatus,
    VerifyOobInviteCode,
    InviteContinue,
    CancelInviteOpAndBack,
    SelectNatMode(fauna_core::nat_mode::NodeMode),
    SubmitNatModeChoice,
    DeferNatModeChoice,
    Back,
    /// Client-local, not a machine mutator: copy the generated secret to the
    /// terminal's clipboard via OSC 52, which works over SSH — the deployment
    /// `tui.md` § Goal names first.
    CopySecret,
    /// Single-shot probe on the "Almost ready" surface: is the deferred-DNS nest
    /// reachable yet, and can it be claimed? The machine owns the probe; the
    /// client owns only the cadence.
    RecheckManualDns,
    /// Put the manual DNS records on the clipboard (OSC 52), so the user can
    /// paste them at their registrar.
    CopyDnsRecords,
    /// "Use a different nest" — the "Almost ready" surface's exit
    /// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*). The
    /// machine clears the durable slot and lands the wizard at `handle_entry`
    /// holding the same identity; the client only forwards the click.
    AbandonAwaitingManualDns,

    // --- dns_config (§4) ---
    /// Explicit user override of the machine-derived `buy_domain` default
    /// (`onboarding.md:219` — the machine seeds it from the handle-check
    /// outcome; the client only forwards toggles).
    ToggleBuyDomain(bool),
    ToggleSameProviderForVps(bool),
    SelectDnsProvider(String),
    /// `dns-provider-open-browser-button` / `vps-provider-open-browser-button`
    /// — hand the selected provider's signup URL to the OS default handler.
    /// Client-local (no machine state changes), so it carries the URL directly.
    OpenSignupUrl(String),
    /// A `hosted-auth` field's button (`onboarding.md` § 4): the bundled
    /// provider's device-authorization sign-in. The machine owns the flow
    /// (`hosted_auth_begin` → `hosted_auth_wait`); this client only opens the
    /// URL it is handed, through the same opener as `OpenSignupUrl`.
    HostedAuth(fauna_onboarding_machine::CredentialForm, String),
    VerifyDns,
    ConfirmPrice,
    /// "Set up later" — the deferred-DNS path. The wizard skips DNS and, after
    /// provisioning, exits through `dns_post_instructions` (§7).
    DnsSetUpLater,
    ContinueFromDns,

    // --- vps_config (§5) ---
    SelectVpsProvider(String),
    SelectVpsServerType(String),
    /// `vps-location-picker` selects by the location's **display name**, not its
    /// id (the cross-app picker contract), so the name→id lookup happens
    /// against the machine's own verified location list.
    SelectVpsLocationByName(String),
    /// Mail-vs-social mode, decided here because cloud-init needs mail-intent
    /// *and* the RAM tier before the box boots (`onboarding.md:231`).
    SetProvisionMailMode(bool),
    /// Which builds the box's updater follows — a
    /// `vps-config-update-channel-row[<channel>]` pick, decided here for the
    /// same reason as the mail mode: it is written into the box's cloud-init.
    SetProvisionUpdateChannel(fauna_provisioning::cloud_init::UpdateChannel),
    VerifyVps,
    ContinueFromVps,

    // --- nest_provisioning (§6) ---
    StartProvisioning,
    CancelProvisioning,
    RetryProvisioning,
    ContinueFromProvisioning,

    // --- dns_post_instructions (§7) ---
    ContinueFromDnsPostInstructions,
    /// Copy the manual records the user must paste at their registrar. Distinct
    /// from [`Action::CopyDnsRecords`]: that one serves the "Almost ready"
    /// surface's own line-per-record text, this one the machine's pre-rendered
    /// markdown table (`dns_post_instructions()`).
    CopyDnsPostInstructions,

    // --- box recovery, step 4 (`box-recovery.md` § Recovery UI (step 4)) ---
    /// `recover-lost-box-button` on `identity_choice` — the fresh-client recovery
    /// entry. Routes through `identity_import` (the identity is what unseals
    /// the account plane) and then the normal nest-connect step (Q2-A), landing on
    /// `nest_recovery` once a surviving nest resolves as already-owned.
    BeginRecoverLostBox,
    /// Pick which custodied box to recover. Carries the box's public
    /// `nest_actor_id` (hex) — never its seed.
    SelectRecoveryBox(String),
    RecoverViaCloud,
    RecoverViaSelfhosted,
    /// Copy the installer `.env` line to the terminal's clipboard (OSC 52, so it
    /// works over SSH). The command is client-held (`Wizard::selfhosted_command`),
    /// so it reaches [`run_action`] as the prepared payload rather than off the
    /// machine.
    CopySelfhostedCommand,
    /// Leave the recovery flow (`recover-selfhosted-continue-button` /
    /// `recover-restore-cta`). The rebuilt box reconnects through the normal
    /// launch flow once the admin has run the installer and it is reachable.
    ExitRecovery,
}

/// Observer → redraw. The machine notifies on every mutation; the render loop
/// re-reads snapshots on the tick (the same coalescing `try_send` shape as
/// linux's `GtkObserver`, here over the `UiMessage` channel).
struct TuiObserver {
    tx: UnboundedSender<UiMessage>,
}

impl OnboardingObserver for TuiObserver {
    fn on_changed(&self) {
        // A closed channel means the app is shutting down — nothing to notify.
        let _ = self.tx.send(UiMessage::Data(DataMessage::WizardChanged));
    }
}

/// The wizard's client-side state: the shared machine plus the page-local
/// input buffers.
///
/// The focus ring is **not** here — it belongs to the unauthenticated *screen*,
/// which the launch surface may own instead of the wizard, so `App` holds it
/// (`App::focus`) over `App::screen_elements()`.
pub struct Wizard {
    pub machine: Arc<OnboardingMachine>,
    inputs: HashMap<WizardField, String>,
    /// A client-side error (today: an unparseable pasted secret). Distinct from
    /// `machine.error_message()`, which the machine owns; `error-message`
    /// renders whichever is set.
    pub error: Option<String>,
    /// The resolved `recover-selfhosted-command` for the selected box — the
    /// installer `.env` line carrying that box's custodied `FAUNA_DEPLOYMENT_SEED`
    /// (`box-recovery.md` § Recovery UI (step 4)).
    ///
    /// Client-held rather than machine-held because it is the one value the
    /// machine deliberately will **not** surface: the machine resolves seeds
    /// inside Rust and hands out only public `nest_actor_id`s. This page is the
    /// sanctioned exception (it *is* the installer input), so the read lands here
    /// via `app.rs`'s page-entry fetch. `None` = show the pending placeholder;
    /// never show another box's command.
    pub selfhosted_command: Option<String>,
    /// The `request_id` whose pending-invite slot this session already wrote.
    ///
    /// The write moment is the `wizard_submit_invite_request()` return
    /// (`onboarding.md` § 3 Persistence callouts — "the only write moment"),
    /// but tui reaches it from two input paths (agent + keyboard) that both
    /// funnel through the main loop's post-action check, so the check runs on
    /// every wake. This makes it write **once per request** instead of once per
    /// wake — the re-persist-on-every-UI-wake shape the old continue-exit had.
    pub pending_invite_persisted: Option<String>,
    /// The `nest_retire` page, while it is open (`nest_retire.rs`). Hosted here
    /// exactly as `nest_recovery` is — and, like it, over a live session: while
    /// `Some`, this page is what the wizard paints, whatever the onboarding
    /// machine's step.
    pub retire: Option<nest_retire::RetireView>,
}

impl Wizard {
    pub fn new(tx: &UnboundedSender<UiMessage>, credentials: &Arc<CredentialStore>) -> Self {
        let observer = Arc::new(TuiObserver { tx: tx.clone() });
        // `None` provider_base_urls in production; e2e overrides install at
        // runtime via the `set_provider_base_urls` bridge hook.
        //
        // `new_with_persistence`, never bare `new`: the wizard writes the
        // pending-provision slot before it builds a box, so a quit or crash
        // mid-run resumes from the app alone (`onboarding.md` § 6 *The
        // pending-provision slot*). The store is the same per-actor registry
        // view `session::registry` builds — one slot, one writer.
        let machine = OnboardingMachine::new_with_persistence(
            observer,
            Arc::new(
                AccountRegistry::new(Arc::clone(credentials) as Arc<dyn SecretStore>)
                    .pending_provision_store(),
            ),
        );
        // tui has the `recovery_kit` onboarding screen built (the first app
        // to — lead-app rule), so the machine routes identity creation
        // through it. The other six apps leave this undeclared until their
        // trickle-down lands (`onboarding.md` § 1 Identity).
        machine.set_renders_recovery_kit(true);
        // Same lead-app declaration for the `trust_prompt` interstitial
        // (`onboarding.md` § 3b-ter): tui renders it, so the machine routes the
        // NAT step's exits through the one-tap trust offer. The other six apps
        // leave it undeclared and keep exiting straight to `Done`.
        machine.set_renders_trust_prompt(true);
        Wizard {
            machine,
            inputs: HashMap::new(),
            error: None,
            selfhosted_command: None,
            pending_invite_persisted: None,
            retire: None,
        }
    }

    /// The text `error-message` renders, if any. Empty strings never register
    /// (apple's lesson: a reserved-but-blank line must read as invisible).
    pub fn error_text(&self) -> Option<String> {
        if let Some(view) = &self.retire {
            return nest_retire::error_text(view);
        }
        let text = self
            .error
            .clone()
            .or_else(|| self.machine.error_message())?;
        (!text.is_empty()).then_some(text)
    }

    /// Factory-reset the wizard back to `IdentityChoice`, dropping typed input.
    pub fn reset(&mut self) {
        self.machine.reset();
        self.inputs.clear();
        self.error = None;
        self.selfhosted_command = None;
        // Dropping the page drops its token with it (the machine holds it in
        // memory only); `cancel` makes that explicit.
        if let Some(view) = self.retire.take() {
            view.machine.cancel();
        }
    }

    /// Read an editable field. `Handle` reads through to the machine, which
    /// owns it — so a bridge-driven `set_current_handle` shows up in the input.
    ///
    /// `ClaimCode` falls back to `claim_code_prefill()` while the local buffer
    /// is empty: after a factory reset the human never sees the code the nest
    /// returned, so an un-prefilled input strands the admin (`onboarding.md:89`).
    /// Doing it here — not in the page's element list — keeps the paint and the
    /// submit path reading the same value.
    pub fn field(&self, field: WizardField) -> String {
        match field {
            WizardField::Handle => self.machine.current_handle(),
            WizardField::ClaimCode => match self.inputs.get(&WizardField::ClaimCode) {
                Some(typed) if !typed.is_empty() => typed.clone(),
                _ => self.machine.claim_code_prefill().unwrap_or_default(),
            },
            // The contact form reads through to the machine, which owns the
            // `ContactInfo` — `verify_dns()` pre-fills it from the registrar's
            // `fetch_default_contact()` (`onboarding.md:221`), and those values
            // must appear in the inputs without the user having typed them.
            WizardField::Contact(which) => self
                .machine
                .dns_config()
                .contact
                .as_ref()
                .map(|c| which.get(c).to_string())
                .unwrap_or_default(),
            WizardField::ImportSecret
            | WizardField::InviteCode
            | WizardField::RecoveryPhrase
            | WizardField::RecoveryAccount => self.inputs.get(&field).cloned().unwrap_or_default(),
            WizardField::DnsCred(_) | WizardField::VpsCred(_) => {
                self.inputs.get(&field).cloned().unwrap_or_default()
            }
            WizardField::RetireCred(_) | WizardField::RetireConfirmName => self
                .retire
                .as_ref()
                .map(|view| nest_retire::field(view, &field))
                .unwrap_or_default(),
        }
    }

    /// Write an editable field, pushing it through to the machine the way
    /// linux's `connect_changed` does.
    pub fn set_field(&mut self, field: WizardField, value: String) {
        match field {
            WizardField::Handle => self.machine.set_current_handle(value),
            // Credentials keep a local buffer for *paint* (the machine stores
            // them as `SecretString` and hands nothing back) but the machine is
            // still the owner — push every keystroke through, exactly as linux's
            // `entry.connect_changed → set_dns_cred` does.
            WizardField::DnsCred(ref id) => {
                self.machine.set_dns_cred(id.clone(), value.clone());
                self.inputs.insert(field, value);
            }
            WizardField::VpsCred(ref id) => {
                self.machine.set_vps_cred(id.clone(), value.clone());
                self.inputs.insert(field, value);
            }
            // `set_contact` takes the whole struct, so read-modify-write. No
            // local buffer: `field()` reads it back off the machine.
            WizardField::Contact(which) => {
                let mut contact = self.machine.dns_config().contact.unwrap_or_default();
                which.set(&mut contact, value);
                self.machine.set_contact(contact);
            }
            WizardField::ClaimCode
            | WizardField::ImportSecret
            | WizardField::InviteCode
            | WizardField::RecoveryPhrase
            | WizardField::RecoveryAccount => {
                self.inputs.insert(field, value);
            }
            WizardField::RetireCred(_) | WizardField::RetireConfirmName => {
                if let Some(view) = self.retire.as_mut() {
                    nest_retire::set_field(view, &field, value);
                }
            }
        }
    }

    /// The current page's ordered element list — the one source paint, the
    /// automation registry, and the focus ring all read.
    ///
    /// Only genuine ui.yaml elements appear here. Titles and help text are
    /// unregistered chrome ([`title`]/[`description`]): ui.yaml's onboarding
    /// pages do not scope a `page-heading`, so registering one would be an
    /// invented app-specific ID.
    pub fn elements(&self) -> Vec<Element> {
        // The retire page, while open, is the whole surface — it is hosted
        // here but driven by its own machine (`nest_retire.rs`).
        if self.retire.is_some() {
            return nest_retire::elements(self);
        }
        // The "Almost ready" surface is not an `OnboardingStep` — it is keyed on
        // the wizard *outcome* at `Done`, so the same-session deferred-DNS exit
        // and the relaunch hydration paint the identical page
        // (`onboarding.md` § "Almost ready" surface).
        if self.is_awaiting_manual_dns() {
            return awaiting_manual_dns::elements(self);
        }
        match self.machine.step() {
            OnboardingStep::IdentityChoice => identity_choice::elements(self),
            OnboardingStep::IdentityCreated => identity_created::elements(self),
            OnboardingStep::IdentityImport => identity_import::elements(self),
            OnboardingStep::HandleEntry => handle_entry::elements(self),
            OnboardingStep::InviteRequest => invite_request::elements(self),
            OnboardingStep::ClaimCode => claim_code::elements(self),
            OnboardingStep::NatModeChoice => nat_mode_choice::elements(self),
            OnboardingStep::DnsConfig => dns_config::elements(self),
            OnboardingStep::VpsConfig => vps_config::elements(self),
            OnboardingStep::NestProvisioning => nest_provisioning::elements(self),
            OnboardingStep::DnsPostInstructions => dns_post_instructions::elements(self),
            OnboardingStep::NestRecovery => nest_recovery::elements(self),
            OnboardingStep::RecoverSelfhostedInstructions => {
                recover_selfhosted_instructions::elements(self)
            }
            OnboardingStep::RecoveryKit => recovery_kit::elements(self),
            OnboardingStep::TrustPrompt => trust_prompt::elements(self),
            OnboardingStep::RecoveryEntry => recovery_entry::elements(self),
            // `Done` has no page of its own — the wizard has exited, and the
            // surface is keyed on `wizard_outcome()` from here (the "Almost
            // ready" early-return above, or the authenticated shell).
            //
            // Both this match and `description`'s are deliberately **exhaustive**:
            // a new `OnboardingStep` should fail tui's build — forcing the page to
            // be rendered — rather than silently painting an empty surface that
            // claims no ui.yaml element exists.
            OnboardingStep::Done => Vec::new(),
        }
    }

    /// The page's title, rendered as the pane's border title (chrome, not an
    /// automatable element).
    /// Is the wizard resting on the "Almost ready" surface? True at `Done` with
    /// a deferred-DNS outcome, on both the same-session and hydrated paths.
    pub fn is_awaiting_manual_dns(&self) -> bool {
        matches!(
            self.machine.wizard_outcome(),
            Some(fauna_onboarding_machine::WizardOutcome::AwaitingManualDns { .. })
        )
    }

    /// Whether the `invite_request` page is sitting on a submitted request
    /// awaiting the admin — the state that polls (`onboarding.md` § The
    /// pending-invite surface).
    ///
    /// Read off the SNAPSHOT, not a wizard outcome: unlike awaiting-DNS, this
    /// journey has no terminal of its own — it waits *on the page*, which is
    /// exactly what makes the page the surface.
    pub fn is_pending_invite_review(&self) -> bool {
        matches!(
            self.machine.invite_request_snapshot().state,
            fauna_onboarding_machine::InviteRequestState::PendingReview { .. }
        )
    }

    pub fn title(&self) -> String {
        if self.retire.is_some() {
            return nest_retire::title();
        }
        if self.is_awaiting_manual_dns() {
            return awaiting_manual_dns::title();
        }
        match self.machine.step() {
            OnboardingStep::IdentityChoice => identity_choice::title(),
            OnboardingStep::IdentityCreated => identity_created::title(),
            OnboardingStep::IdentityImport => identity_import::title(),
            OnboardingStep::HandleEntry => handle_entry::title(),
            OnboardingStep::InviteRequest => invite_request::title(),
            OnboardingStep::ClaimCode => claim_code::title(),
            OnboardingStep::NatModeChoice => nat_mode_choice::title(),
            OnboardingStep::DnsConfig => dns_config::title(),
            OnboardingStep::VpsConfig => vps_config::title(),
            OnboardingStep::NestProvisioning => nest_provisioning::title(),
            OnboardingStep::DnsPostInstructions => dns_post_instructions::title(),
            OnboardingStep::NestRecovery => nest_recovery::title(),
            OnboardingStep::RecoverSelfhostedInstructions => {
                recover_selfhosted_instructions::title()
            }
            OnboardingStep::RecoveryKit => recovery_kit::title(),
            OnboardingStep::TrustPrompt => trust_prompt::title(),
            OnboardingStep::RecoveryEntry => recovery_entry::title(),
            other => format!("{other:?}"),
        }
    }

    /// Help text painted above the elements (chrome, not automatable).
    pub fn description(&self) -> Vec<String> {
        if self.retire.is_some() {
            return nest_retire::description();
        }
        if self.is_awaiting_manual_dns() {
            return awaiting_manual_dns::description();
        }
        match self.machine.step() {
            OnboardingStep::IdentityChoice => identity_choice::description(),
            OnboardingStep::IdentityCreated => identity_created::description(),
            OnboardingStep::IdentityImport => identity_import::description(),
            OnboardingStep::HandleEntry => handle_entry::description(),
            OnboardingStep::InviteRequest => invite_request::description(),
            OnboardingStep::ClaimCode => claim_code::description(),
            OnboardingStep::NatModeChoice => nat_mode_choice::description(),
            OnboardingStep::DnsConfig => dns_config::description(),
            OnboardingStep::VpsConfig => vps_config::description(),
            OnboardingStep::NestProvisioning => nest_provisioning::description(self),
            OnboardingStep::DnsPostInstructions => dns_post_instructions::description(),
            OnboardingStep::NestRecovery => nest_recovery::description(),
            OnboardingStep::RecoverSelfhostedInstructions => {
                recover_selfhosted_instructions::description()
            }
            OnboardingStep::RecoveryKit => recovery_kit::description(),
            OnboardingStep::TrustPrompt => trust_prompt::description(),
            OnboardingStep::RecoveryEntry => recovery_entry::description(),
            OnboardingStep::Done => done_description(self),
        }
    }

    /// Client-side validation, run before a gesture reaches the machine.
    ///
    /// Returns the payload string the action carries, or `Err(message)` to
    /// abort and show `message` in `error-message`. Only `identity_import`
    /// validates: a bad paste "surfaces the localized `invalid_secret` without
    /// touching the machine" (`onboarding.md:25`). Everything else forwards
    /// its field verbatim — the machine owns every other rule.
    pub fn prepare(&mut self, action: &Action) -> Result<String, String> {
        match action {
            Action::ConfirmImportedIdentity => {
                let pasted = self.field(WizardField::ImportSecret);
                let parsed = fauna_core::identity_qr::parse_import_input(&pasted)
                    .ok_or_else(identity_import::invalid_secret_message)?;
                // A QR payload may carry the handle; pre-fill step 2 with it.
                if let Some(handle) = parsed.handle {
                    self.machine.set_current_handle(handle);
                }
                Ok(parsed.secret)
            }
            // The phrase rides as the payload; the account field rides on the
            // machine's own handle — the wizard's one account field, and the
            // one `handle_entry` asks for next, so a successful restore
            // pre-fills it exactly as an `identity_import` QR's handle does.
            // Same stash-then-forward shape `ConfirmImportedIdentity` uses.
            Action::SubmitRecoveryEntry => {
                // ALWAYS forwarded, empty included: what the field shows is
                // what is sent — never a handle an earlier flow left on the
                // machine, which would outrank the payload's own `handle=`.
                let typed = self.field(WizardField::RecoveryAccount);
                self.machine.set_current_handle(typed.trim().to_string());
                Ok(self.field(WizardField::RecoveryPhrase))
            }
            Action::SubmitClaimCode => Ok(self.field(WizardField::ClaimCode)),
            Action::VerifyOobInviteCode => Ok(self.field(WizardField::InviteCode)),
            // The installer command is client-held (the machine never surfaces a
            // seed), so it rides to `run_action` as the payload. The button is
            // disabled until it resolves, so an empty payload is unreachable —
            // but never copy the placeholder if it somehow is.
            Action::CopySelfhostedCommand => {
                Ok(self.selfhosted_command.clone().unwrap_or_default())
            }
            _ => Ok(String::new()),
        }
    }
}

/// Resolve a shared `LocalizedText` against the shared string table. Clients
/// render the machine's `{key, args}` pair; they never recompute the text
/// (`onboarding.md` § Architectural rules).
pub fn localized(text: &fauna_core::localized::LocalizedText) -> String {
    text.resolve(fauna_i18n::strings::lookup)
}

/// Resolve a bare i18n key that the *data* carries rather than the code — a
/// provider's `display_name_key` / `help_key` / `registrar_notes_key`, a
/// credential field's `label_key`. Falls back to the key itself so a missing
/// string is visible rather than blank. Never hardcode a provider's string:
/// the providers table is generated, and the key is the only stable handle.
pub fn key(k: &str) -> String {
    fauna_i18n::strings::lookup(k)
        .map(str::to_string)
        .unwrap_or_else(|| k.to_string())
}

/// Hand a URL to the OS default handler — `tui.md` § External media handoff:
/// "the OS already owns 'which program plays video'", and the same holds for
/// 'which program opens a URL', so fauna adds no program-picker knob. Failure
/// (headless box, no handler) is silent by design: the URL is also painted in
/// `dns-provider-link` / `vps-provider-link`, so the user can always reach it.
///
/// The shared [`crate::os_open`] resolution (this fn's former inline copy
/// lacked the Windows `cmd /C start` arm, so these links never opened there)
/// also honours the `FAUNA_TUI_MEDIA_OPENER` e2e override — deliberate: it
/// makes wizard browser-opens fakeable in a test the same way media opens are.
fn open_in_browser(url: &str) {
    crate::os_open::open(url);
}

/// The `hosted-auth` field's button, shared by the DNS and VPS credential
/// forms (`onboarding.md` § 4): it carries the field's own derived element id
/// (no new ui.yaml element), its label is the machine's
/// `hosted_auth_state` — Idle → "Sign in at the provider…", Pending → the
/// browser + code line, Connected, Failed — and it is pressable exactly when
/// the machine's `hosted_auth_can_begin` says so (the `base-url` is typed and
/// no attempt is mid-flight). Nothing here is re-derived: label states and
/// pressability are both the machine's.
pub(crate) fn hosted_auth_button(
    m: &OnboardingMachine,
    form: fauna_onboarding_machine::CredentialForm,
    element_id: String,
    field_id: &str,
) -> Element {
    let text = m.hosted_auth_button_text(form, field_id.to_string());
    Element::button(
        element_id,
        text,
        m.hosted_auth_can_begin(form, field_id.to_string()),
        Action::HostedAuth(form, field_id.to_string()),
    )
}

/// Run one wizard gesture to completion.
///
/// The agent's click path `await`s this before replying, so the driver's next
/// element read already observes the new step — the TUI analogue of linux's
/// "the observer tick swapped the page before the reply left the main loop".
/// The keyboard path spawns it instead ([`spawn_action`]) so a slow network
/// probe never freezes the render loop.
///
/// `confirm_sink` is where a confirmed identity secret is committed — moment 1
/// of the two-moment write contract, carrying the append flag the shared
/// moment branches on (it writes nothing in append mode). Built by
/// [`crate::session::confirm_identity_sink`], which owns why.
pub async fn run_action(
    machine: Arc<OnboardingMachine>,
    confirm_sink: crate::session::ConfirmIdentitySink,
    action: Action,
    code: String,
) {
    use Action::*;
    // The confirm arms below are the *only* readers; every other action leaves
    // the sink untouched.
    let commit = |secret: &str| {
        crate::session::persist_confirmed_identity(&confirm_sink, secret);
    };
    match action {
        BeginCreateIdentity => machine.begin_create_identity(),
        BeginImportIdentity => machine.begin_import_identity(),
        ConfirmGeneratedIdentity => match machine.confirm_generated_identity() {
            // The durable commit point (`long-term-store.md` moment 1,
            // `onboarding.md` § 1 Identity): written HERE, not carried to
            // `LoggedIn`. A generated secret that has not been committed exists
            // nowhere else, so deferring it makes a force-quit destroy the
            // identity — and it deletes the launch router's case 2
            // (`secret_key` present, `node_url` empty → resume at
            // `HandleEntry`), which the whole partial-state design exists for.
            Ok(secret) => commit(&secret),
            Err(e) => tracing::warn!("confirm_generated_identity: {e}"),
        },
        ConfirmImportedIdentity => match machine.confirm_imported_identity(code) {
            Ok(secret) => commit(&secret),
            Err(e) => tracing::warn!("confirm_imported_identity: {e}"),
        },
        BeginRecoveryEntry => machine.begin_recovery_entry(),
        SubmitRecoveryEntry => run_recovery_entry(&machine, code).await,
        StartHandleCheck => machine.start_handle_check(machine.current_handle()).await,
        SetControlCheckbox(checked) => machine.set_control_checkbox(checked),
        SubmitHandleCheckContinue => {
            machine.submit_handle_check_continue().await;
        }
        SubmitClaimCode => {
            machine.wizard_submit_claim_code(code).await;
        }
        SubmitInviteRequest => {
            machine.wizard_submit_invite_request().await;
        }
        RecheckInviteStatus => {
            machine.recheck_invite_status().await;
        }
        VerifyOobInviteCode => machine.verify_oob_invite_code(code).await,
        InviteContinue => {
            // `onboarding.md` § 3 — Continue is the out-of-band code's redeem
            // and nothing else. The PendingReview branch retired 2026-08-12
            // with the continue-exit: that journey advances by polling
            // (§ The pending-invite surface), so pressing Continue there does
            // nothing, exactly as `continue_enabled` reports.
            machine.redeem_invite().await;
        }
        CancelInviteOpAndBack => {
            machine.cancel_invite_op();
            machine.back();
        }
        SelectNatMode(mode) => machine.select_nat_mode(mode),
        SubmitNatModeChoice => {
            // Async: commits over the mutable `fauna.setup.nat_mode` kind. On
            // success the machine exits to `Done` with `LoggedIn`.
            machine.submit_nat_mode_choice().await;
        }
        DeferNatModeChoice => {
            // Keeps the seeded mode — a working default the admin can change
            // later from the admin-nest page — and exits the same way.
            machine.defer_nat_mode_choice();
        }
        Back => machine.back(),
        CopySecret => {
            if let Some(secret) = machine.generated_secret() {
                copy_to_clipboard(&secret);
            }
        }
        ConfirmRecoveryKit => machine.confirm_recovery_kit(),
        SkipRecoveryKit => machine.skip_recovery_kit(),
        // Both exits conclude the wizard; the grant arm additionally latches
        // the answer, which `handle_wizard_done` consumes to run the mint on
        // the freshly authenticated session (`onboarding.md` § 3b-ter).
        GrantDefaultTrust => {
            machine.grant_default_trust();
        }
        SkipTrustPrompt => {
            machine.skip_trust_prompt();
        }
        // Fire-and-forget and deliberately local, like `CopySecret`: the
        // phrase is already on screen, so this adds no exposure.
        //
        // Copies the `fauna://recovery` URI — the same payload the QR beside it
        // encodes, not the bare hex the screen displays. See
        // `recovery_kit::kit_uri` for why the two encodings were unified.
        CopyRecoveryKitSecret => {
            if let Some(uri) = recovery_kit::kit_uri(&machine) {
                copy_to_clipboard(&uri);
            }
        }
        RecheckManualDns => {
            // Single-shot by contract — the machine runs one `fauna.setup.status`
            // probe (and a claim, if the nest is up and unclaimed) and returns.
            // On a successful claim it routes itself to `NatModeChoice` (or, on
            // the already-claimed edge, to the trust offer); the observer tick
            // repaints, and the launch slot is cleared only at `LoggedIn`, inside
            // the shared `persist_logged_in` moment `session::adopt` runs
            // (`onboarding.md` § Long-term store contract) — never here.
            machine.recheck_manual_dns().await;
        }
        CopyDnsRecords => {
            // Same shared formatter the label renders — they cannot drift apart.
            copy_to_clipboard(&machine.awaiting_dns_records_text());
        }
        // One call: the machine clears the identity's awaiting slot BEFORE it moves
        // its own state (a crash between the two relaunches onto the surface, a
        // recoverable place) and lands `handle_entry` holding the identity — the
        // landing `LaunchAction::Fallthrough` makes with `seed_identity`, minus
        // the slot. No registry call here: the machine's injected store owns it.
        AbandonAwaitingManualDns => machine.abandon_awaiting_manual_dns(),

        // --- dns_config (§4) ---
        ToggleBuyDomain(on) => machine.toggle_buy_domain(on),
        ToggleSameProviderForVps(on) => machine.toggle_same_provider_for_vps(on),
        SelectDnsProvider(id) => machine.select_dns_provider(id),
        OpenSignupUrl(url) => open_in_browser(&url),
        HostedAuth(form, field_id) => {
            // Errors are already in the field's `HostedAuthState::Failed`
            // (painted as the button label) — nothing to re-derive here.
            if let Ok(prompt) = machine.hosted_auth_begin(form, field_id.clone()).await {
                open_in_browser(&prompt.verification_url);
                let _ = machine.hosted_auth_wait(form, field_id).await;
            }
        }
        VerifyDns => {
            // The Err is already surfaced by the machine (`set_error`), which
            // `error-message` renders — so there is nothing for the client to do
            // with it. Never re-derive an error string here.
            let _ = machine.verify_dns().await;
        }
        ConfirmPrice => machine.confirm_price(),
        DnsSetUpLater => machine.dns_set_up_later(),
        ContinueFromDns => {
            let _ = machine.continue_from_dns();
        }

        // --- vps_config (§5) ---
        SelectVpsProvider(id) => machine.select_vps_provider(id),
        SelectVpsServerType(id) => machine.select_vps_server_type(id),
        SelectVpsLocationByName(name) => {
            if let Some(loc) = machine
                .vps_config()
                .locations
                .iter()
                .find(|l| l.name == name)
            {
                machine.select_vps_location(loc.id.clone());
            }
        }
        SetProvisionMailMode(on) => machine.set_provision_mail_mode(on),
        SetProvisionUpdateChannel(channel) => machine.set_provision_update_channel(channel),
        VerifyVps => {
            let _ = machine.verify_vps().await;
        }
        ContinueFromVps => {
            let _ = machine.continue_from_vps().await;
        }

        // --- nest_provisioning (§6) ---
        // `start_provisioning` spawns the orchestrator and returns at once — it
        // runs for minutes, so the click must not await it. The observer ticks
        // drive the re-render, and the page reads `provisioning_snapshot()` on
        // each. tui's main loop is tokio, so the machine's own `tokio::spawn`
        // lands on an ambient runtime; no worker-thread dance (linux's GTK
        // thread has no reactor, which is why it drives `run_provisioning`
        // itself).
        StartProvisioning => machine.start_provisioning(),
        CancelProvisioning => machine.cancel_provisioning(),
        RetryProvisioning => machine.retry_provisioning(),
        ContinueFromProvisioning => {
            machine.continue_from_provisioning();
        }

        // --- dns_post_instructions (§7) ---
        ContinueFromDnsPostInstructions => {
            machine.continue_from_dns_post_instructions();
        }
        CopyDnsPostInstructions => {
            // The machine pre-renders the markdown record table; copying
            // anything else would be a second formatter that could drift from
            // what the page shows.
            if let Some(text) = machine.dns_post_instructions() {
                copy_to_clipboard(&text);
            }
        }

        // --- box recovery, step 4 ---
        BeginRecoverLostBox => machine.begin_recover_lost_box(),
        SelectRecoveryBox(id) => machine.select_recovery_box(id),
        // Both methods are guarded machine-side by `require_selected_recovery_box`;
        // the client already gates the buttons on a selection, so an Err here is
        // a state the user cannot reach. The machine surfaces it through
        // `error_message()` either way — never re-derive an error string.
        RecoverViaCloud => {
            let _ = machine.recover_via_cloud();
        }
        RecoverViaSelfhosted => {
            let _ = machine.recover_via_selfhosted();
        }
        CopySelfhostedCommand => {
            // `code` is the resolved installer line, prepared from
            // `Wizard::selfhosted_command` (the machine holds no seed to give).
            if !code.is_empty() {
                copy_to_clipboard(&code);
            }
        }
        ExitRecovery => machine.reset(),
    }
}

/// Put `text` on the terminal's clipboard with OSC 52. The sequence is
/// forwarded by the terminal emulator, so it reaches the *user's* clipboard
/// even when `fauna-tui` runs on a remote box over SSH. `pub(crate)`: the
/// contacts page's `contact-actor-id-copy-btn` copies through this same door.
pub(crate) fn copy_to_clipboard(text: &str) {
    use base64::Engine as _;
    use std::io::Write as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    // A terminal that ignores OSC 52 (or a closed stdout) simply doesn't copy;
    // the secret is on screen either way, so this is never load-bearing.
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = out.flush();
}

/// Record a success/notice line the UI is showing ("Copied!") in the log ring
/// at `info` — `observability.md` § What must be logged, category 1: the widget
/// covers the moment, the Logs page is the durable record. Pass the displayed
/// text only, never the value it concerns (the copied id, a secret) — § 2's
/// redaction rule.
pub(crate) fn log_displayed_notice(text: &str) {
    tracing::info!(target: "fauna_tui::notice", "{text}");
}

/// Fire-and-forget an action from the keyboard path. The observer tick redraws
/// when the machine lands, so a multi-second probe leaves the TUI responsive.
/// Resolve a [`RecoveryEntryOutcome`] into what the user sees.
///
/// The machine returns the typed outcome and writes no message of its own — it
/// holds no string table — so this is the single owner of what lands on
/// `error-message` for this screen. Every refusal keeps the user on
/// `recovery_entry` to act on it; `Superseded` is the one that routes, and it
/// routes to the identity import with the same claim-free wording the launch
/// flow's superseded refusal uses (the successor arrives unverified, so naming
/// it would be trusting the nest as an authorizer rather than verifying it —
/// `identity-succession.md` § Propagation).
async fn run_recovery_entry(machine: &Arc<OnboardingMachine>, phrase: String) {
    use fauna_i18n::strings::onboarding::recovery_entry as t;
    use fauna_onboarding_machine::RecoveryEntryOutcome as O;

    let outcome = machine.submit_recovery_entry(phrase).await;
    // `Superseded` routes instead of speaking — uniform with the launch flow's
    // refusal. Every other arm's words are the shared table
    // (`RecoveryEntryOutcome::message`), including the one success that must
    // still speak: `RestoredPredecessorsLost`, where the account is back but a
    // corpus sealed under an older identity just became unopenable
    // (`identity-succession.md` § Seed escrow).
    if outcome == O::Superseded {
        machine.begin_import_identity_with_reason(t::SUPERSEDED.to_string());
        return;
    }
    if let Some(message) = outcome.message() {
        machine.set_error_message(localized(&message));
    }
}

pub fn spawn_action(
    machine: Arc<OnboardingMachine>,
    confirm_sink: crate::session::ConfirmIdentitySink,
    action: Action,
    code: String,
) {
    tokio::spawn(run_action(machine, confirm_sink, action, code));
}

/// The machine reached `Done`: route on `wizard_outcome()` per
/// `onboarding.md` § Wizard exit handling. Never infer the outcome from state.
///
/// `LoggedIn` navigates to the authenticated UI. There is no unresolved-mode
/// outcome any more — the storage-mode axis is retired (`storage-modes.md`), so
/// the claim's only terminal is `LoggedIn` (via the `nat_mode_choice` confirm or
/// defer). The provisioning-branch outcomes arrive with their pages in a later
/// milestone (tracked internally).
/// The deferred half of the `recovery_kit` screen's ceremony: register the
/// root the user confirmed there, now that a signed-in connection exists
/// (`identity-succession.md` § The RecoveryKey → *Creation UX*, ratified
/// 2026-08-01 — the screen mints and displays only). Fire-and-forget like the
/// first-setup mail glue: on any failure the chain simply has no registration,
/// which Settings' `recovery-kit-status` never-created warning surfaces
/// honestly — the screen promised activation, never protection. A failed
/// escrow half is likewise the Settings status line's job
/// (`RegisteredNoEscrow` on the next read); the kit is registered and valid.
fn register_deferred_recovery_kit(
    nest: Arc<fauna_client::NestClient>,
    secret_hex: &str,
    kit_hex: fauna_core::secret::SecretString,
) {
    let identity = match fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) {
        Ok(kp) => kp,
        Err(e) => {
            tracing::error!("deferred recovery kit: identity secret unreadable: {e}");
            return;
        }
    };
    // Registration, profile-head mirror and every logged arm are the shared
    // body linux's handoff calls too.
    tokio::spawn(async move {
        fauna_client_recovery::ceremony::register_deferred_kit(nest, &identity, &kit_hex).await;
    });
}

/// Mint the default grant set the user asked for on the `trust_prompt` screen
/// (`onboarding.md` § 3b-ter).
///
/// Deferred to this signed-in handoff for the same reason the recovery kit's
/// registration is: the wizard holds no authenticated session, and minting
/// needs one twice over — to read the nest's content-processor roster and to
/// deposit each sealed blob at `fauna.capabilities.mint`. What crossed the
/// handoff is the user's *answer*, never a capability.
///
/// Best-effort and log-only, like every other post-`adopt` provisioning call
/// beside it: the account is fully usable without the grants (they are an
/// optimization — `encryption-at-rest.md` § Capability tiering: "the fallback
/// is always present, so server-side is an optimization"), and the user can
/// grant the same trust any time from Settings → Nests. A failure here must
/// not paint an error over a completed onboarding.
///
/// **Which grants** is not decided here: `MintDefaultSet` mints exactly what
/// the shared mint catalog derives, so this glue holds no policy that could
/// drift from the Nests page's own picker.
///
/// `runtime` is the settings-owned account-store slot: the tap's blessing waits for
/// the account runtime this sign-in is still assembling
/// (`fauna_account_seams::blessed_nests`).
fn mint_default_trust_set(
    nest: Arc<fauna_client::NestClient>,
    secret_hex: &str,
    runtime: crate::settings::AccountRuntimeSlot,
    mail: Arc<dyn fauna_client_config::MailStore>,
) {
    let machine = match crate::mail_glue::build_linked_nests_machine_with_trust(
        nest,
        secret_hex,
        crate::settings::nests::ledger_door(runtime.clone()),
        crate::settings::nests::backup_door(runtime.clone()),
        crate::settings::nests::blessing_door(runtime.clone()),
        crate::settings::period_key_door(runtime.clone()),
        mail,
        crate::settings::folder_key_door(runtime),
    ) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("one-tap trust: {e}; nothing minted");
            return;
        }
    };
    tokio::spawn(fauna_client_pair::dispatch_mint_default_trust_set(machine));
}

/// Seal the DNS-provider credential the wizard captured at the `dns_config`
/// step into the admin's `fauna.state.dns`
/// (`onboarding-provisioning.md` § 4. DNS configuration).
///
/// Deferred to this signed-in handoff for the same reason the recovery kit's
/// registration and the trust mint are: the wizard holds no authenticated
/// session, and the credential store is `BackupKey`-sealed under the admin's
/// own key. What crossed the handoff is the *credential*, never a capability
/// — and the nest can neither receive nor decrypt it.
///
/// The rule itself lives in `fauna_client_dns` so tui is a call site rather
/// than a fourth dialect of it (priority #2): every app was writing the same
/// build-machine → map-fields → dispatch body, and one of them had already
/// drifted on the label.
///
/// Best-effort and log-only, like every other post-`adopt` provisioning call
/// beside it: onboarding is complete and the account fully usable, and the
/// same credential is re-enterable any time from the admin DNS page — which
/// is also where a failure surfaces.
fn seal_captured_dns_credential(
    nest: Arc<fauna_client::NestClient>,
    secret_hex: &str,
    account: fauna_client_dns::AccountHandleSource,
    cred: fauna_onboarding_machine::CapturedDnsCredential,
) {
    // The credentialed DNS machine needs the admin's own keypair: the
    // account store is sealed under the BackupKey derived from it. Same
    // stance as the `admin-dns` page's own derive — `secret_hex` already passed
    // launch validation, so a failure here is practically impossible and is
    // logged rather than allowed to panic a completed onboarding.
    let keypair = match fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(
                "onboarding DNS glue: derive keypair: {e}; the captured provider \
                 credential was not sealed — re-enter it from the admin DNS page"
            );
            return;
        }
    };
    tokio::spawn(fauna_client_dns::dispatch_seal_captured_dns_credential(
        nest,
        keypair,
        account,
        cred.provider_id,
        cred.fields,
        cred.label,
    ));
}

/// Write the pending-invite resume slot once the wizard is in `PendingReview`.
///
/// This is tui's half of the 2026-08-12 retirement of
/// `WizardOutcome::InviteSubmitted`. That journey no longer *exits* the wizard
/// — it stays on `invite_request` and polls (`onboarding.md` § The pending-invite
/// surface) — so there is no `Done` outcome to hang the write on and
/// [`handle_wizard_done`] never sees it. The slot is written here instead, from
/// the machine's assembled [`OnboardingMachine::pending_invite_slot`], which is
/// the submit return's observable effect.
///
/// Idempotent per request: the main loop calls this on every wake, and without
/// the `pending_invite_persisted` guard that would rewrite the slot continuously
/// — the exact shape the old exit had.
///
/// In **append mode** the same return additionally adopts — register the append
/// identity, write its slot, switch to it — per `onboarding.md` § Multi-account
/// ("the append glue adopts on the submit return"). That is the same work the
/// retired `InviteSubmitted` terminal did; only its trigger moved.
pub fn persist_pending_invite_slot(app: &mut crate::app::App) {
    let Some(slot) = app.wizard.machine.pending_invite_slot() else {
        return;
    };
    if app.wizard.pending_invite_persisted.as_deref() == Some(slot.request_id.as_str()) {
        return;
    }
    let Some(secret) = app.wizard.machine.effective_secret() else {
        // The secret is the durable commit point; without it there is nothing to
        // key the slot on. Loud, because a silent miss here is exactly the class
        // this retirement closed — a resume slot that is simply never written.
        tracing::error!("[onboarding] pending-invite slot with no effective_secret");
        return;
    };
    // Mark before acting: both arms below are one-shot per request, and a failed
    // attempt must not re-fire on every wake of the main loop.
    app.wizard.pending_invite_persisted = Some(slot.request_id.clone());

    if app.adding_account {
        if let Err(e) = crate::session::adopt_appended_pending_invite(app, &slot, &secret) {
            tracing::error!("[onboarding] appending the pending invite: {e}");
            app.wizard.error = Some(e);
        }
        return;
    }
    let record = fauna_launch_machine::PendingInviteRecord {
        nest_url: slot.nest_url,
        handle: slot.handle,
        request_id: slot.request_id,
        status_json: slot.status_json,
    };
    if let Err(e) = crate::session::persist_pending_invite(app, &secret, &record) {
        tracing::error!("[onboarding] persisting the pending-invite slot: {e}");
        app.wizard.error = Some(e);
    }
}

/// The URL to **dial** for the nest `nest_url` names.
///
/// A thin alias for the shared seam, kept only so this module's call sites read
/// in wizard vocabulary. The `cfg`-split that used to live here — a production
/// twin plus a machine-reading twin — moved into
/// `fauna_launch_machine::dial::resolved_dial_url`, where every app's *store*
/// read reaches it too: tui was the only app that could resolve through the
/// onboarding machine, because it is the only one whose `LoggedIn` glue takes
/// the dial URL as a parameter. Keeping a second copy here would have been the
/// first of seven dialects of one rule (priority #1).
fn nest_dial_url(_machine: &fauna_onboarding_machine::OnboardingMachine, nest_url: &str) -> String {
    fauna_launch_machine::resolved_dial_url(nest_url)
}

pub fn handle_wizard_done(app: &mut crate::app::App, tx: &UnboundedSender<UiMessage>) {
    use fauna_onboarding_machine::WizardOutcome;
    let Some(outcome) = app.wizard.machine.wizard_outcome() else {
        return;
    };
    let Some(secret) = app.wizard.machine.effective_secret() else {
        tracing::error!("wizard reached Done without a secret");
        return;
    };
    // Append mode ("Add account" over a live session): every terminal persists the
    // NEW identity + switches the client to it (`session::adopt_appended` — the
    // add_account-then-switch path), instead of the no-session arms below that
    // `establish`/resume directly on the current surface. A failed append surfaces
    // on the wizard's own error line and leaves the live session running.
    if app.adding_account {
        if let Err(e) = crate::session::adopt_appended(app, &outcome, &secret) {
            tracing::error!("wizard append: {e}");
            app.wizard.error = Some(e);
        }
        return;
    }
    match outcome {
        WizardOutcome::LoggedIn { nest_url, handle } => {
            // First-setup mail/CalDAV glue (Feature D): capture the machine-derived
            // enablement intent *before* `adopt` replaces `app.wizard` (mirrors
            // linux's `launch_main_app_after_signin` call site, which reads these
            // off the machine before it is torn down). `email_enable_requested`/
            // `caldav_enable_requested` are ON iff the handle targets a real
            // registerable domain — there is no user-facing checkbox to read
            // (Phase-4 S8.7 deleted the onboarding service checkboxes; the intent
            // now derives, `onboarding.md` § 3b).
            let enable_email = app.wizard.machine.email_enable_requested();
            let enable_caldav = app.wizard.machine.caldav_enable_requested();
            let enable_carddav = app.wizard.machine.carddav_enable_requested();
            let enable_webdav = app.wizard.machine.webdav_enable_requested();
            // The recovery kit the user confirmed on the `recovery_kit` screen,
            // minted there but deliberately unregistered until now — no nest
            // existed at that screen's position, and the root is never
            // persisted, so this signed-in handoff is the one point custody
            // permits the registration (`identity-succession.md` § The
            // RecoveryKey → *Creation UX*). Consume-once; `None` if skipped.
            let pending_kit = app.wizard.machine.take_pending_recovery_secret();
            // Same capture-before-adopt shape: the `trust_prompt` answer
            // (`onboarding.md` § 3b-ter). Consume-once, so a handoff that runs
            // twice mints once; `false` when the user skipped or was never
            // asked.
            let trust_granted = app.wizard.machine.take_trust_prompt_granted();
            // Same capture-before-adopt shape: the DNS-provider credential the
            // user verified back on the `dns_config` step. The machine only
            // *captures* it — it has no `fauna.account.state.put` capability, and on
            // the fresh-provision path the nest did not yet exist at that step
            // — so the seal is this signed-in handoff's job
            // (`onboarding-provisioning.md` § 4. DNS configuration, *Capture at
            // onboarding, seal via the launched app*). `None` on the manual /
            // set-up-later / unverified paths, which is every run that did not
            // provision a box through a supported provider.
            let captured_dns = app.wizard.machine.captured_dns_credential();
            // Same capture-before-adopt shape: predecessor seeds a phrase-only
            // restore recovered from the escrow blob's additive section. Empty
            // on every ordinary onboarding; non-empty only when the restored
            // account is mid corpus re-seal after a succession, and then they
            // are the only copies left anywhere (`identity-succession.md`
            // § Seed escrow). `adopt` persists them after the restored identity
            // (so it is the one that lands active) and before the session is
            // built (so the session's read custody holds them).
            let restored_predecessors = app.wizard.machine.restored_predecessors();
            // The socket to open, which is the typed URL itself in every
            // production build — see `nest_dial_url` below.
            let dial_url = nest_dial_url(&app.wizard.machine, &nest_url);
            // Same capture-before-adopt shape as the trust answer:
            // the box's reach address, which outlives the wizard as the account's
            // reach hint so the first main-app session opens connected while the
            // domain is still propagating (`onboarding.md` § Reach hint). `None`
            // on every path that did not provision a box.
            let reach_ipv4 = app.wizard.machine.provision_reach_ipv4();
            if let Err(e) = crate::session::adopt(
                app,
                tx,
                &nest_url,
                &dial_url,
                &secret,
                handle,
                reach_ipv4.as_deref(),
                &restored_predecessors,
            ) {
                tracing::error!("wizard LoggedIn: {e}");
                app.wizard.error = Some(e);
            } else if let Some(session) = app.session.as_ref() {
                // The post-claim serving enablement (`onboarding.md` § 3b): the
                // four intents captured above, fired by the ONE shared step every
                // app calls — reached only here, on a wizard `LoggedIn`; a
                // returning-user relaunch never calls `adopt`.
                let client = Arc::clone(&session.client);
                crate::mail_glue::apply_post_claim_serving_enablement(
                    Arc::clone(&client),
                    &secret,
                    app.settings.mail_store(),
                    &nest_url,
                    crate::session::ledger_seam(app),
                    fauna_client_mail_settings::serving_enablement::ServingEnablementIntents {
                        email: enable_email,
                        caldav: enable_caldav,
                        carddav: enable_carddav,
                        webdav: enable_webdav,
                    },
                );
                // No claim-time seed capture: the deployment-seed custody leg
                // captures this box's seed once the account runtime is up
                // (`box-recovery.md` § The plane-era recovery floor, (c) The
                // writes; `crate::recovery::spawn_custody_leg`).
                if let Some(kit_hex) = pending_kit {
                    register_deferred_recovery_kit(Arc::clone(&client), &secret, kit_hex);
                }
                if trust_granted {
                    mint_default_trust_set(
                        Arc::clone(&client),
                        &secret,
                        app.settings.account_runtime.clone(),
                        app.settings.mail_store(),
                    );
                }
                if let Some(cred) = captured_dns {
                    // The record lives on the account plane, whose runtime
                    // this `adopt` has only just started: the store waits
                    // for it (`fauna_client_dns::AccountDnsStore`).
                    seal_captured_dns_credential(
                        Arc::clone(&client),
                        &secret,
                        app.settings.account_runtime_source(),
                        cred,
                    );
                }
            }
        }
        WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        } => {
            // `handle` is not in the outcome payload but the slot requires it —
            // the eventual `LoggedIn` carries it and it is not derivable from
            // `nest_url` (`onboarding.md` § Long-term store contract). Read it
            // off the machine, which is where the wizard put it.
            let handle = app.wizard.machine.current_handle();
            // Opaque-JSON contract, as for the invite slot: serialize verbatim
            // and never validate on the way back in.
            let dns_records_json = serde_json::to_string(&dns_records).unwrap_or_default();
            let record = fauna_launch_machine::AwaitingDnsRecord {
                nest_url,
                handle,
                dns_records_json,
                claim_code,
                // Completed, not replaced — see the shared writer.
                reach_ipv4: None,
                nest_actor_id: None,
            };
            if let Err(e) = crate::session::persist_awaiting_dns(app, &secret, &record) {
                tracing::error!("wizard AwaitingManualDns: {e}");
                app.wizard.error = Some(e);
            }
        }
    }
}

/// The placeholder text painted once the wizard is `Done` but the app is still
/// unauthenticated. A terminal placeholder is a line of text; it carries no
/// ui.yaml element because the doc names none.
///
/// ⚠ The pending-invite journey no longer reaches here at all (2026-08-12).
/// It used to render a two-line "request sent / we'll notify you" screen —
/// the dead end `onboarding.md` § Wizard exit handling deletes, whose second
/// line promised a push channel an unregistered actor cannot have. That journey
/// now stays on `invite_request` and polls, so the wizard never goes `Done` for
/// it.
fn done_description(_w: &Wizard) -> Vec<String> {
    vec![t::FINISHED.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use fauna_launch_machine::LaunchPersistence;

    const SECRET: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn bytes(secret_hex: &str) -> Vec<u8> {
        fauna_core::hex32::decode(secret_hex).unwrap().to_vec()
    }

    /// Drive one wizard gesture exactly as the app does — through the same
    /// door and the same sink the keyboard and agent paths both compute.
    ///
    /// ⚠ Every step below goes through here, including the ones that only set
    /// up state. Calling a machine mutator directly instead would test a door
    /// the app never uses: a mutation committing the secret inside
    /// `BeginCreateIdentity` survived precisely that shortcut.
    async fn drive(app: &crate::app::App, action: Action, code: &str) {
        run_action(
            Arc::clone(&app.wizard.machine),
            crate::session::confirm_identity_sink(app),
            action,
            code.to_string(),
        )
        .await;
    }

    // ── Moment 1: confirm-identity is the durable commit point ───────────
    //
    // `long-term-store.md` § "Long-term state is only ever written at two
    // specific moments" (1. confirm-identity writes `secret_key`) and
    // `onboarding.md` § 1 Identity ("the app writes the secret to its
    // long-term store immediately on return — this is the durable commit
    // point"). Asserted through `LaunchPersistence`, because the reason the
    // contract exists is the three-case launch routing's case 2.

    /// A generated secret exists nowhere but this app until it is written.
    /// Force-quitting between here and complete-login must resume the wizard
    /// at `HandleEntry`, not destroy the identity.
    #[tokio::test]
    async fn confirming_a_generated_identity_commits_the_secret_at_once() {
        let app = test_app();
        drive(&app, Action::BeginCreateIdentity, "").await;
        let secret = app
            .wizard
            .machine
            .generated_secret()
            .expect("begin_create_identity generates one");

        drive(&app, Action::ConfirmGeneratedIdentity, "").await;

        let persistence = crate::session::launch_persistence(&app);
        assert_eq!(
            persistence.load_identity(),
            Some(bytes(&secret)),
            "the confirmed secret must be durable before the wizard advances"
        );
        assert_eq!(
            persistence.load_nest_url(),
            None,
            "moment 2 has not happened — case 2 routing needs node_url empty"
        );
    }

    /// The import arm carries the same contract. Less catastrophic (the user
    /// pasted the secret from somewhere), but the routing case is identical
    /// and a divergence between the two arms is its own bug.
    #[tokio::test]
    async fn confirming_an_imported_identity_commits_the_secret_at_once() {
        let app = test_app();
        drive(&app, Action::BeginImportIdentity, "").await;

        drive(&app, Action::ConfirmImportedIdentity, SECRET).await;

        assert_eq!(
            crate::session::launch_persistence(&app).load_identity(),
            Some(bytes(SECRET))
        );
    }

    /// The write is the *confirm*, not the *generate*. `identity_created`
    /// shows the secret and the user may still go Back and import instead —
    /// committing at display time would persist an identity they rejected.
    #[tokio::test]
    async fn showing_a_generated_secret_commits_nothing() {
        let app = test_app();
        drive(&app, Action::BeginCreateIdentity, "").await;

        assert!(
            app.wizard.machine.generated_secret().is_some(),
            "the secret is on screen"
        );
        assert_eq!(
            crate::session::launch_persistence(&app).load_identity(),
            None,
            "but nothing is committed until the user confirms"
        );
    }

    /// Append mode keeps its structural non-pollution (`apps/tui.md`
    /// § Append-mode "Add account"): a second-account wizard run over a live
    /// session writes at its own terminal, so abandoning it leaves the store
    /// exactly as it was. Moment 1 is the first-run wizard's contract, and
    /// widening it here would trade a real property for nothing — the append
    /// user's identity is already durable under the account they are adding
    /// *from*.
    #[tokio::test]
    async fn an_abandoned_append_still_commits_nothing() {
        let mut app = test_app();
        app.adding_account = true;
        drive(&app, Action::BeginCreateIdentity, "").await;

        drive(&app, Action::ConfirmGeneratedIdentity, "").await;

        assert_eq!(
            crate::session::launch_persistence(&app).load_identity(),
            None,
            "an append that the user walks away from must not pollute the store"
        );
    }

    /// The other half of moment 1's contract, on the lead app: what the wizard
    /// **retracts**. Create an identity, go Back, onboard a *different* one —
    /// the abandoned first must not survive as a second switcher row.
    ///
    /// The second identity arrives by **import**, which is what makes the two
    /// distinct: `begin_create_identity` deliberately re-offers the secret it
    /// already generated (`machine.rs`, the `generated_secret.is_none()`
    /// guard), so create → Back → create is the same identity twice — already
    /// covered, and no ghost. Windows' e2e twin takes the same import arm for
    /// the same reason.
    ///
    /// Moment 1 writes a real, activated registry row at Continue, so nothing
    /// on the Back path can retract it (`OnboardingMachine::back` is in-memory
    /// step routing, and quit/crash/kill do not run it at all); the retraction
    /// lives in the next commit instead
    /// (`fauna_client_accounts::AccountRegistry::retire_superseded_provisionals`).
    /// This is the shared-Rust twin of windows'
    /// `test_windows_abandoned_create_identity_does_not_ghost_the_switcher` —
    /// the root cause is in `fauna-client-accounts`, which all seven apps
    /// consume, so it is pinned on the lead app rather than on the one
    /// platform that happened to catch it.
    #[tokio::test]
    async fn abandoning_a_created_identity_leaves_no_second_switcher_row() {
        let app = test_app();
        drive(&app, Action::BeginCreateIdentity, "").await;
        drive(&app, Action::ConfirmGeneratedIdentity, "").await;
        let abandoned = crate::session::registry(&app)
            .active()
            .expect("moment 1 registered and activated the first identity");

        // Back out of handle entry and onboard a *different* identity instead.
        drive(&app, Action::Back, "").await;
        drive(&app, Action::BeginImportIdentity, "").await;
        drive(&app, Action::ConfirmImportedIdentity, SECRET).await;

        let registry = crate::session::registry(&app);
        let rows: Vec<String> = registry.list().into_iter().map(|e| e.actor_id).collect();
        assert_eq!(
            rows.len(),
            1,
            "the abandoned identity is still listed as an account — the ghost row \
             `long-term-store.md` § Eager vs. lazy migration at native boot forbids; \
             rows were {rows:?}"
        );
        assert_ne!(
            rows[0], abandoned,
            "the surviving row must be the identity the user actually confirmed"
        );
        assert_eq!(
            registry.active().as_deref(),
            Some(rows[0].as_str()),
            "and it is the one the next launch resumes"
        );
    }
}
