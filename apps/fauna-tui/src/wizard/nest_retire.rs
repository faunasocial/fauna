//! `nest_retire` — retiring an app-provisioned nest
//! (`docs/goal/behavior/nest-retirement.md`, the owner of every rule below).
//!
//! A paint shell over the shared [`NestRetireMachine`], hosted by the wizard
//! host exactly as `nest_recovery` is — including over a live session (the
//! append-mode precedent): while [`Wizard::retire`] is `Some`, the wizard's
//! element list is this page, whatever the onboarding machine's step.
//!
//! ui.yaml `onboarding.nest_retire`. States, each the machine's
//! ([`RetirePhase`]): **credentials** (the `vps_config` provider row, credential
//! form and verify button, reused by id — no region or server-type controls) →
//! **list** (`retire-server-item[n]`, its members scoped inside the row) →
//! **confirm** (the typed-name gate) → **running** (two `provisioning-step-row`
//! rows: DNS, then the server) → **done** (the by-hand list).
//!
//! Two entries (`nest-retirement.md` § Layout & flow): `launch-retire-button`
//! on the unreachable-nest surface, and `admin-nest-retire-button` beside
//! Factory reset. Back from the credentials state — and Done — return to the
//! entry that opened the page; Done after retiring *this session's own box*
//! from the admin entry drops to the launch flow instead, because the nest the
//! session was signed into no longer exists.
//!
//! **Nothing here decides anything.** The typed-name gate and the binding of a
//! run to the confirmed server are the machine's (`confirm_enabled`,
//! `force_server_offered`, `armed`); every sentence that names what dies or
//! what is left is a machine fold (`confirm_summary`, `leftover_lines`,
//! `transfer_code_note`); the credential fields and the verify gate are the
//! machine's (`visible_fields`, `can_verify`). The VPS token lives in the
//! machine for the page's lifetime and is dropped on exit (`cancel`) — this
//! shell keeps only a paint buffer for what was typed.

use std::collections::HashMap;
use std::sync::Arc;

use fauna_client_mail_settings::local_domains::{
    LocalDomainAction, LocalDomainMachine, LocalDomainsSnapshot,
};
use fauna_core::data::DnsConfig;
use fauna_i18n::strings::onboarding::retire as t;
use fauna_onboarding_machine::FieldTypePlain;
use fauna_onboarding_machine::retire::{
    HeldDnsCredential, ManagedServerRow, NestRetireMachine, RetireInputs, RetirePhase,
    RetireSnapshot, StepState, TransferCodeState,
};
use fauna_provisioning::providers_generated::{Capability, PROVIDERS};
use fauna_ui_ids as ids;
use tokio::sync::mpsc::UnboundedSender;

use super::{Wizard, WizardField, key, localized};
use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture};
use crate::launch::LaunchSurface;

/// Which entry opened the page — where Back and Done return to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireEntry {
    /// `launch-retire-button` on the unreachable-nest surface.
    Launch,
    /// `admin-nest-retire-button`, over the live session.
    Admin,
}

/// One visit to the page: the machine plus what this shell needs to paint it
/// and to leave it.
pub struct RetireView {
    pub machine: Arc<NestRetireMachine>,
    pub entry: RetireEntry,
    /// The launch surface to put back when the page closes.
    return_to: LaunchSurface,
    /// Paint buffers for the typed credential fields — the machine holds them
    /// as secrets and hands nothing back.
    creds: HashMap<String, String>,
    /// What has been typed into `retire-confirm-name-input`.
    confirm_typed: String,
}

/// A gesture on the page, or on one of its two entries.
#[derive(Debug, Clone)]
pub enum RetireAction {
    OpenFromLaunch,
    OpenFromAdmin,
    SelectProvider(String),
    /// A `hosted-auth` credential field's sign-in button.
    HostedAuth(String),
    Verify,
    /// A row, by the provider's server id.
    Select(String),
    FetchTransferCode,
    CopyTransferCode,
    /// `retire-delete-button` → the confirm.
    Delete,
    Confirm,
    CancelConfirm,
    Retry,
    ForceServer,
    Back,
    CopyLeftovers,
    Done,
}

/// The machine's async half of a gesture, run spawned (keyboard) or awaited
/// (the e2e agent) — the same duality every other surface uses.
pub struct RetireWork {
    machine: Arc<NestRetireMachine>,
    op: RetireOp,
    tx: UnboundedSender<UiMessage>,
}

enum RetireOp {
    Verify,
    HostedAuth(String),
    FetchTransferCode,
    Confirm,
    Retry,
    ForceServer,
}

impl RetireWork {
    pub async fn run(self) {
        let redraw = |tx: &UnboundedSender<UiMessage>| {
            let _ = tx.send(UiMessage::Data(DataMessage::WizardChanged));
        };
        let m = &self.machine;
        match self.op {
            RetireOp::Verify => m.verify().await,
            RetireOp::HostedAuth(field_id) => {
                // A failure is already the field's `Failed` state, painted as
                // the button label — nothing to re-derive here.
                if let Some(prompt) = m.hosted_auth_begin(field_id.clone()).await {
                    super::open_in_browser(&prompt.verification_url);
                    // Paint the pending code while the person signs in.
                    redraw(&self.tx);
                    m.hosted_auth_wait(field_id).await;
                }
            }
            RetireOp::FetchTransferCode => m.fetch_transfer_code().await,
            RetireOp::Confirm => m.confirm().await,
            RetireOp::Retry => m.retry().await,
            RetireOp::ForceServer => m.force_server().await,
        }
        redraw(&self.tx);
    }
}

// ---------------------------------------------------------------------------
// Entries and exits
// ---------------------------------------------------------------------------

/// Open the page over whatever the screen shows now.
///
/// The machine takes the e2e provider base-URL override at construction (the
/// same `vps` key the wizard's `set_provider_base_urls` installs), the saved
/// nest's host as an attribution candidate, and — once resolved — that host's
/// IPv4 for the *current* badge (`nest-retirement.md` § Where logic lives:
/// "inputs the app passes at construction, all optional").
///
/// The rest of what the app knows goes in too (§ Credential stance (b),
/// § DNS cleanup → *Several domains* (1)): every held DNS credential — the
/// account's DNS record (`fauna.state.dns`), read through the account runtime,
/// so only from a signed-in session whose runtime is up (the admin entry
/// always; the launch entry, which runs before any session, normally not) —
/// and, from `admin-nest`, the nest's active local domains, re-read live
/// ([`spawn_app_inputs`]).
pub fn open(app: &mut App, entry: RetireEntry) {
    let account = crate::session::stored_account(app);
    let nest_host = account
        .as_ref()
        .and_then(|(nest_url, _, _)| nest_url.clone())
        .and_then(|url| reqwest::Url::parse(&url).ok())
        .and_then(|u| {
            u.host_str()
                .map(|h| (h.to_string(), u.port_or_known_default()))
        });
    let mut candidate_domains: Vec<String> = nest_host
        .as_ref()
        .filter(|(host, _)| host.parse::<std::net::IpAddr>().is_err())
        .map(|(host, _)| vec![host.clone()])
        .unwrap_or_default();
    let admin_domains = match entry {
        RetireEntry::Admin => app.admin.local_domains_snapshot.as_ref(),
        RetireEntry::Launch => None,
    };
    let (local_domains, held_dns) = retire_inputs_from(admin_domains, None);
    candidate_domains.extend(local_domains);
    let machine = NestRetireMachine::new_with_inputs(RetireInputs {
        candidate_domains,
        current_ipv4: None,
        held_dns,
        provider_base_url: app.wizard.machine.provider_base_url("vps".into()),
        doh_base_url: None,
    });
    let domains = match entry {
        RetireEntry::Admin => crate::admin::retire_local_domains(&app.admin),
        RetireEntry::Launch => None,
    };
    let account_store = app.settings.account_store.clone();
    if domains.is_some() || account_store.is_some() {
        spawn_app_inputs(Arc::clone(&machine), domains, account_store, app.tx.clone());
    }
    if let Some((host, port)) = nest_host {
        spawn_resolve_current(
            Arc::clone(&machine),
            host,
            port.unwrap_or(443),
            app.tx.clone(),
        );
    }
    let return_to = std::mem::replace(&mut app.launch, LaunchSurface::Wizard);
    app.wizard.retire = Some(RetireView {
        machine,
        entry,
        return_to,
        creds: HashMap::new(),
        confirm_typed: String::new(),
    });
    app.focus = 0;
}

/// What the page takes from local state: the active local domains (soft-deleted
/// ones no longer publish, so they are not candidates) and every held DNS
/// credential, projected by the machine crate's shared
/// [`HeldDnsCredential::all_from`].
fn retire_inputs_from(
    local_domains: Option<&LocalDomainsSnapshot>,
    dns: Option<&DnsConfig>,
) -> (Vec<String>, Vec<HeldDnsCredential>) {
    let domains = local_domains
        .map(|s| s.active.iter().map(|d| d.domain.clone()).collect())
        .unwrap_or_default();
    let held = dns.map(HeldDnsCredential::all_from).unwrap_or_default();
    (domains, held)
}

/// How long the live input reads may hold verify before the page goes on
/// with what it opened with.
const ADMIN_REFRESH_BOUND: std::time::Duration = std::time::Duration::from_secs(15);

/// The live input reads, off the render loop: the nest's local-domain list
/// (admin entry) and the account's held DNS credentials (whenever the account
/// runtime is up), each handed to the machine as it lands. The machine holds
/// verify until both have landed (`expect_app_inputs`), because verify is
/// where it reads them. Best-effort and bounded: a failed or slow read keeps
/// what the page opened with.
fn spawn_app_inputs(
    machine: Arc<NestRetireMachine>,
    domains: Option<Arc<LocalDomainMachine>>,
    account_store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
    tx: UnboundedSender<UiMessage>,
) {
    machine.expect_app_inputs();
    tokio::spawn(async move {
        let read = async {
            if let Some(domains) = domains
                && domains.dispatch(LocalDomainAction::Refresh).await.is_ok()
            {
                let (active, _) = retire_inputs_from(Some(&domains.snapshot()), None);
                machine.add_candidate_domains(active);
            }
            if let Some(store) = account_store {
                match store.dns().await {
                    Ok(dns) => machine.set_held_dns(retire_inputs_from(None, Some(&dns)).1),
                    Err(e) => {
                        tracing::info!("nest_retire: DNS record read failed (best-effort): {e:#}")
                    }
                }
            }
        };
        if tokio::time::timeout(ADMIN_REFRESH_BOUND, read)
            .await
            .is_err()
        {
            tracing::info!("nest_retire: the live input reads timed out (best-effort)");
        }
        machine.app_inputs_landed();
        let _ = tx.send(UiMessage::Data(DataMessage::WizardChanged));
    });
}

/// Resolve the saved nest's host off the render loop and hand its IPv4 to the
/// machine for the *current* badge. Best-effort: an unresolvable host (the
/// commonest reason to be on the launch retry surface) simply badges nothing.
fn spawn_resolve_current(
    machine: Arc<NestRetireMachine>,
    host: String,
    port: u16,
    tx: UnboundedSender<UiMessage>,
) {
    tokio::spawn(async move {
        let lookup = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await;
        let ipv4 = match lookup {
            Ok(Ok(mut addrs)) => addrs.find(|a| a.is_ipv4()).map(|a| a.ip().to_string()),
            _ => None,
        };
        if ipv4.is_some() {
            machine.set_current_ipv4(ipv4);
            let _ = tx.send(UiMessage::Data(DataMessage::WizardChanged));
        }
    });
}

/// Close the page: drop the token and everything fetched with it, then put
/// back what the entry showed — or, after retiring this session's own box
/// from the admin entry, begin launch routing again, since the nest the
/// session was signed into no longer exists.
fn close(app: &mut App) {
    let Some(view) = app.wizard.retire.take() else {
        return;
    };
    let retired_current = view
        .machine
        .snapshot()
        .retired
        .is_some_and(|row| row.current);
    view.machine.cancel();
    app.launch = view.return_to;
    app.focus = 0;
    if retired_current && view.entry == RetireEntry::Admin {
        crate::session::sign_out(app, fauna_client_account_runtime::StopReason::AccountSwitch);
        app.after_stops(|app| {
            let tx = app.tx.clone();
            crate::launch::start(app, &tx);
        });
    }
}

// ---------------------------------------------------------------------------
// Gestures
// ---------------------------------------------------------------------------

/// Apply a gesture's local half; hand back the machine call still to run.
pub fn gesture(app: &mut App, action: RetireAction) -> Option<RetireWork> {
    let tx = app.tx.clone();
    match action {
        RetireAction::OpenFromLaunch => {
            open(app, RetireEntry::Launch);
            return None;
        }
        RetireAction::OpenFromAdmin => {
            open(app, RetireEntry::Admin);
            return None;
        }
        _ => {}
    }
    let view = app.wizard.retire.as_mut()?;
    let m = Arc::clone(&view.machine);
    let work = |op| {
        Some(RetireWork {
            machine: Arc::clone(&m),
            op,
            tx: tx.clone(),
        })
    };
    match action {
        RetireAction::OpenFromLaunch | RetireAction::OpenFromAdmin => None,
        RetireAction::SelectProvider(id) => {
            if m.selected_provider().as_deref() != Some(id.as_str()) {
                view.creds.clear();
            }
            m.select_provider(id);
            None
        }
        RetireAction::HostedAuth(field_id) => work(RetireOp::HostedAuth(field_id)),
        RetireAction::Verify => work(RetireOp::Verify),
        RetireAction::Select(server_id) => {
            m.select(server_id);
            None
        }
        RetireAction::FetchTransferCode => work(RetireOp::FetchTransferCode),
        RetireAction::CopyTransferCode => {
            let snap = m.snapshot();
            if let Some(TransferCodeState::Code { code }) =
                selected_row(&snap).map(|r| &r.transfer_code)
            {
                super::copy_to_clipboard(code);
            }
            None
        }
        RetireAction::Delete => {
            view.confirm_typed.clear();
            m.begin_confirm();
            None
        }
        RetireAction::Confirm => work(RetireOp::Confirm),
        RetireAction::CancelConfirm => {
            view.confirm_typed.clear();
            m.back();
            None
        }
        RetireAction::Retry => work(RetireOp::Retry),
        RetireAction::ForceServer => work(RetireOp::ForceServer),
        RetireAction::Back => {
            match m.snapshot().phase {
                // Credentials — and a run that stopped on a failure — leave the
                // page; `cancel` disarms it, so nothing re-targets the run.
                RetirePhase::Credentials | RetirePhase::Running | RetirePhase::Done => close(app),
                RetirePhase::List | RetirePhase::Confirm => m.back(),
                RetirePhase::Listing => {}
            }
            None
        }
        RetireAction::CopyLeftovers => {
            super::copy_to_clipboard(&leftover_text(&m));
            None
        }
        RetireAction::Done => {
            close(app);
            None
        }
    }
}

/// A typed field on the page: the paint buffer, pushed through to the machine
/// on every edit, as the wizard's credential fields are.
pub fn set_field(view: &mut RetireView, field: &WizardField, value: String) {
    match field {
        WizardField::RetireCred(id) => {
            view.machine.set_credential_field(id.clone(), value.clone());
            view.creds.insert(id.clone(), value);
        }
        WizardField::RetireConfirmName => {
            view.machine.set_confirm_name(value.clone());
            view.confirm_typed = value;
        }
        _ => {}
    }
}

pub fn field(view: &RetireView, field: &WizardField) -> String {
    match field {
        WizardField::RetireCred(id) => view.creds.get(id).cloned().unwrap_or_default(),
        WizardField::RetireConfirmName => view.confirm_typed.clone(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SUBTITLE.to_string()]
}

/// `error-message`'s text while the page is up — the machine's.
pub fn error_text(view: &RetireView) -> Option<String> {
    view.machine.snapshot().error.filter(|e| !e.is_empty())
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let Some(view) = w.retire.as_ref() else {
        return Vec::new();
    };
    let snap = view.machine.snapshot();
    let mut out = Vec::new();
    match snap.phase {
        RetirePhase::Credentials | RetirePhase::Listing => credentials(view, &snap, &mut out),
        RetirePhase::List => list(view, &snap, &mut out),
        RetirePhase::Confirm => confirm(view, &snap, &mut out),
        RetirePhase::Running => running(view, &snap, &mut out),
        RetirePhase::Done => done(view, &snap, &mut out),
    }
    out
}

fn gesture_button(
    id: &str,
    text: impl Into<String>,
    enabled: bool,
    action: RetireAction,
) -> Element {
    Element::gesture_button(id, text.into(), enabled, Gesture::Retire(action))
}

fn back_button(out: &mut Vec<Element>) {
    out.push(gesture_button(
        ids::RETIRE_BACK_BUTTON,
        fauna_i18n::strings::common::BACK,
        true,
        RetireAction::Back,
    ));
}

/// The `vps_config` provider row, credential form and verify button, by the
/// same ids and in the same shapes that page paints them — no location or
/// server-type controls.
fn credentials(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    let m = &view.machine;
    let selected = m.selected_provider();
    out.push(Element::label(ids::VPS_PROVIDER_ROW, ""));
    // Every VPS-capable provider — the listing primitive is on all six
    // adapters, and a box bought through a provider no longer on the curated
    // offer list must still be retirable.
    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Vps))
    {
        let id = p.id.as_str();
        let is_selected = selected.as_deref() == Some(id);
        out.push(
            Element::radio_gesture(
                format!("vps-provider-row[{id}]"),
                key(p.display_name_key),
                is_selected,
                Gesture::Retire(RetireAction::SelectProvider(id.to_string())),
            )
            .attr("state", if is_selected { "on" } else { "off" })
            .within(ids::VPS_PROVIDER_ROW, 0),
        );
    }
    if let Some(p) = selected
        .as_deref()
        .and_then(|id| PROVIDERS.iter().find(|p| p.id.as_str() == id))
    {
        out.push(Element::label(ids::VPS_PROVIDER_LINK, p.signup_url));
        out.push(Element::button(
            ids::VPS_PROVIDER_OPEN_BROWSER_BUTTON,
            fauna_i18n::strings::onboarding::dns_config::OPEN_IN_BROWSER,
            true,
            super::Action::OpenSignupUrl(p.signup_url.to_string()),
        ));
        out.push(Element::label(ids::VPS_PROVIDER_HELP_TEXT, key(p.help_key)));
        out.push(Element::label(ids::VPS_CREDENTIALS_FORM, ""));
        for f in m.visible_fields() {
            let element_id = format!("vps-credentials-form-{}", f.id);
            if f.field_type == FieldTypePlain::HostedAuth {
                out.push(
                    Element::gesture_button(
                        element_id,
                        m.hosted_auth_button_text(f.id.clone()),
                        m.hosted_auth_can_begin(f.id.clone()),
                        Gesture::Retire(RetireAction::HostedAuth(f.id.clone())),
                    )
                    .labelled(key(&f.label_key))
                    .within(ids::VPS_CREDENTIALS_FORM, 0),
                );
                continue;
            }
            let wf = WizardField::RetireCred(f.id.clone());
            out.push(
                Element::input(element_id, field(view, &wf), Field::Wizard(wf))
                    .labelled(key(&f.label_key))
                    .within(ids::VPS_CREDENTIALS_FORM, 0),
            );
        }
        out.push(gesture_button(
            ids::VPS_VERIFY_BUTTON,
            fauna_i18n::strings::provisioning::VERIFY_CREDENTIALS,
            m.can_verify() && snap.phase == RetirePhase::Credentials,
            RetireAction::Verify,
        ));
    }
    back_button(out);
}

fn selected_row(snap: &RetireSnapshot) -> Option<&ManagedServerRow> {
    let id = snap.selected.as_deref()?;
    snap.servers.iter().find(|r| r.server_id == id)
}

/// One row per listed server, its members scoped inside
/// `retire-server-item[n]` (the scoped-descendant idiom, e2e convention 1).
fn list(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    let m = &view.machine;
    if snap.servers.is_empty() {
        out.push(Element::label(
            ids::RETIRE_SERVER_EMPTY_MESSAGE,
            t::EMPTY_MESSAGE,
        ));
    } else {
        out.push(Element::label(ids::RETIRE_SERVER_LIST, t::LIST_LABEL));
    }
    for (i, row) in snap.servers.iter().enumerate() {
        let is_selected = snap.selected.as_deref() == Some(row.server_id.as_str());
        let item = ids::RETIRE_SERVER_ITEM;
        let member = |el: Element| el.within(item, i).within(ids::RETIRE_SERVER_LIST, 0);
        out.push(
            Element::checkbox_gesture(
                item,
                row.name.clone(),
                is_selected,
                Gesture::Retire(RetireAction::Select(row.server_id.clone())),
            )
            .within(ids::RETIRE_SERVER_LIST, 0),
        );
        out.push(member(Element::label(
            ids::RETIRE_SERVER_NAME,
            row.name.clone(),
        )));
        if let Some(domain) = &row.domain {
            out.push(member(
                Element::label(ids::RETIRE_SERVER_DOMAIN, domain.clone()).labelled(t::DOMAIN_LABEL),
            ));
        }
        if !row.secondary_domains.is_empty() {
            out.push(member(
                Element::label(
                    ids::RETIRE_SERVER_SECONDARY_DOMAINS,
                    row.secondary_domains.join(", "),
                )
                .labelled(t::SECONDARY_DOMAINS_LABEL),
            ));
        }
        if let Some(ipv4) = &row.ipv4 {
            out.push(member(
                Element::label(ids::RETIRE_SERVER_ADDRESS, ipv4.clone()).labelled(t::ADDRESS_LABEL),
            ));
        }
        if row.current {
            out.push(member(Element::label(
                ids::RETIRE_SERVER_CURRENT_BADGE,
                t::CURRENT_BADGE,
            )));
        }
        if !row.marked {
            out.push(member(Element::label(
                ids::RETIRE_SERVER_UNMARKED_NOTE,
                t::UNMARKED_NOTE,
            )));
        }
        // The transfer-code affordance sits on the selected row and is
        // independent of deleting.
        if is_selected {
            match &row.transfer_code {
                TransferCodeState::Idle => out.push(member(gesture_button(
                    ids::RETIRE_TRANSFER_CODE_BUTTON,
                    t::TRANSFER_CODE_BUTTON,
                    true,
                    RetireAction::FetchTransferCode,
                ))),
                TransferCodeState::Code { code } => {
                    out.push(member(
                        Element::label(ids::RETIRE_TRANSFER_CODE_VALUE, code.clone())
                            .labelled(t::TRANSFER_CODE_LABEL),
                    ));
                    out.push(member(gesture_button(
                        ids::RETIRE_TRANSFER_CODE_COPY_BUTTON,
                        t::TRANSFER_CODE_COPY,
                        true,
                        RetireAction::CopyTransferCode,
                    )));
                }
                _ => {}
            }
            if let Some(note) = m.transfer_code_note(row.server_id.clone()) {
                out.push(member(Element::label(
                    ids::RETIRE_TRANSFER_CODE_STATUS,
                    localized(&note),
                )));
            }
        }
    }
    out.push(gesture_button(
        ids::RETIRE_DELETE_BUTTON,
        t::DELETE_BUTTON,
        selected_row(snap).is_some(),
        RetireAction::Delete,
    ));
    back_button(out);
}

/// The typed-name gate (`nest-retirement.md` § Confirm shape).
fn confirm(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    let summary: Vec<String> = view
        .machine
        .confirm_summary()
        .iter()
        // The provider rides as its display KEY, so it is resolved too.
        .map(|line| line.resolve_nested(fauna_i18n::strings::lookup))
        .collect();
    out.push(Element::label(
        ids::RETIRE_CONFIRM_SUMMARY,
        summary.join("\n"),
    ));
    let wf = WizardField::RetireConfirmName;
    out.push(
        Element::input(
            ids::RETIRE_CONFIRM_NAME_INPUT,
            field(view, &wf),
            Field::Wizard(wf),
        )
        .labelled(t::CONFIRM_NAME_LABEL),
    );
    out.push(gesture_button(
        ids::RETIRE_CONFIRM_BUTTON,
        t::CONFIRM_BUTTON,
        snap.confirm_enabled,
        RetireAction::Confirm,
    ));
    out.push(gesture_button(
        ids::RETIRE_CANCEL_BUTTON,
        t::CANCEL_BUTTON,
        true,
        RetireAction::CancelConfirm,
    ));
}

/// The two step rows, reusing the `provisioning-progress` component's row and
/// child ids and its glyph vocabulary.
fn step_rows(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    let row = "provisioning-step-row";
    for (i, step) in snap.steps.iter().enumerate() {
        out.push(Element::label(row, "").within(row, i));
        out.push(
            Element::label(
                ids::PROVISIONING_STEP_CHECKBOX,
                fauna_provisioning::progress::status_glyph(step.state.progress_status()),
            )
            .within(row, i),
        );
        out.push(
            Element::label(
                ids::PROVISIONING_STEP_LABEL,
                localized(&view.machine.step_label(step.step)),
            )
            .within(row, i),
        );
        if let Some(note) = view.machine.step_note(step.state.clone()) {
            out.push(Element::label(ids::PROVISIONING_SUBSTEP, localized(&note)).within(row, i));
        }
        if let StepState::Failed { cause } = &step.state {
            out.push(Element::label(ids::PROVISIONING_STEP_ERROR, cause.clone()).within(row, i));
        }
    }
}

fn running(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    step_rows(view, snap, out);
    let failed = snap
        .steps
        .iter()
        .any(|s| matches!(s.state, StepState::Failed { .. }));
    if failed {
        out.push(gesture_button(
            ids::RETIRE_RETRY_BUTTON,
            t::RETRY_BUTTON,
            true,
            RetireAction::Retry,
        ));
    }
    if snap.force_server_offered {
        // The dangling-record warning, repeated where the choice is made.
        out.push(Element::chrome(t::FORCE_SERVER_WARNING));
        out.push(gesture_button(
            ids::RETIRE_FORCE_SERVER_BUTTON,
            t::FORCE_SERVER_BUTTON,
            true,
            RetireAction::ForceServer,
        ));
    }
    // Leaving is offered only once the run has stopped on a failure — never
    // while a step is still in flight.
    if failed {
        back_button(out);
    }
}

fn leftover_text(m: &NestRetireMachine) -> String {
    m.leftover_lines()
        .iter()
        .map(localized)
        .collect::<Vec<_>>()
        .join("\n")
}

fn done(view: &RetireView, snap: &RetireSnapshot, out: &mut Vec<Element>) {
    if let Some(row) = &snap.retired {
        out.push(Element::chrome(t::done_deleted(&row.name)));
    }
    step_rows(view, snap, out);
    out.push(Element::label(
        ids::RETIRE_DNS_LEFTOVER_TEXT,
        leftover_text(&view.machine),
    ));
    out.push(gesture_button(
        ids::RETIRE_DNS_LEFTOVER_COPY_BUTTON,
        t::LEFTOVER_COPY,
        true,
        RetireAction::CopyLeftovers,
    ));
    out.push(gesture_button(
        ids::RETIRE_DONE_BUTTON,
        t::DONE_BUTTON,
        true,
        RetireAction::Done,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;

    fn ids(app: &App) -> Vec<String> {
        app.page_elements().into_iter().map(|e| e.id).collect()
    }

    fn transient(app: &mut App) {
        app.launch = LaunchSurface::TransientRetry {
            error: "connection refused".to_string(),
            recover_boxes: Vec::new(),
        };
    }

    fn enabled(app: &App, id: &str) -> bool {
        app.page_elements()
            .into_iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id} is not painted"))
            .enabled
    }

    /// `launch-retire-button` opens the page on its credentials state — the
    /// `vps_config` controls reused by id, no location or server-type ones —
    /// and Back puts the retry surface back exactly as it was.
    #[tokio::test]
    async fn the_launch_entry_opens_the_credentials_state_and_back_returns_to_it() {
        let mut app = test_app();
        transient(&mut app);
        assert!(gesture(&mut app, RetireAction::OpenFromLaunch).is_none());

        let painted = ids(&app);
        assert!(
            painted.contains(&"vps-provider-row[hetzner]".to_string()),
            "{painted:?}"
        );
        assert!(painted.contains(&"retire-back-button".to_string()));
        assert!(!painted.contains(&"launch-retry-button".to_string()));
        assert!(!painted.iter().any(|id| id.starts_with("vps-location")
            || id.starts_with("vps-server-type")
            || id == "vps-config-continue-button"));

        gesture(&mut app, RetireAction::Back);
        assert!(app.wizard.retire.is_none());
        assert!(matches!(app.launch, LaunchSurface::TransientRetry { .. }));
    }

    /// Verify is dead until the provider's required field is typed — the
    /// machine's gate, painted — and the typed token lands in the machine.
    #[tokio::test]
    async fn verify_waits_for_the_token() {
        let mut app = test_app();
        transient(&mut app);
        gesture(&mut app, RetireAction::OpenFromLaunch);
        gesture(&mut app, RetireAction::SelectProvider("hetzner".into()));
        assert!(ids(&app).contains(&"vps-credentials-form-api-token".to_string()));
        assert!(!enabled(&app, "vps-verify-button"));

        app.wizard
            .set_field(WizardField::RetireCred("api-token".into()), "tok".into());
        assert!(enabled(&app, "vps-verify-button"));
        assert_eq!(
            app.wizard
                .field(WizardField::RetireCred("api-token".into())),
            "tok"
        );
    }

    /// The bundled provider's `hosted-auth` field is a button, exactly as on
    /// `vps_config` — dead until its address is typed.
    #[tokio::test]
    async fn the_hosted_sign_in_is_a_button_gated_on_the_address() {
        let mut app = test_app();
        transient(&mut app);
        gesture(&mut app, RetireAction::OpenFromLaunch);
        gesture(&mut app, RetireAction::SelectProvider("bundled".into()));
        assert!(!enabled(&app, "vps-credentials-form-api-token"));
        app.wizard.set_field(
            WizardField::RetireCred("base-url".into()),
            "https://bundle.example".into(),
        );
        assert!(enabled(&app, "vps-credentials-form-api-token"));
    }

    /// From `admin-nest` the page is hosted over the LIVE session: the sidebar
    /// steps aside while it is up, and Back returns to the authenticated shell
    /// with the session untouched.
    #[tokio::test]
    async fn the_admin_entry_hosts_the_page_over_the_live_session() {
        let mut app = test_app();
        app.session = Some(crate::app::tests::test_session());
        assert!(!app.showing_launch_surface());

        gesture(&mut app, RetireAction::OpenFromAdmin);
        assert!(app.showing_launch_surface());
        assert!(
            app.sidebar_elements().is_empty(),
            "the page owns the screen"
        );
        assert!(ids(&app).contains(&"retire-back-button".to_string()));

        gesture(&mut app, RetireAction::Back);
        assert!(app.authenticated(), "backing out never touches the session");
        assert!(!app.showing_launch_surface());
    }

    /// What the admin entry passes the machine from a seeded session
    /// (`nest-retirement.md` § Credential stance (b), § DNS cleanup →
    /// *Several domains* (1)): every active local domain — a soft-deleted one
    /// publishes nothing, so it is no candidate — and every held
    /// DNS credential, in the record's order.
    #[test]
    fn the_admin_inputs_are_the_active_domains_and_every_held_credential() {
        use fauna_core::data::{DnsConfig, DnsProviderCredential};
        use fauna_core::secret::SecretString;

        let domains = crate::admin::domains_snapshot(
            vec![
                crate::admin::domain_row("example.test", true),
                crate::admin::domain_row("second.test", false),
            ],
            vec![crate::admin::domain_row("gone.test", false)],
        );
        let mut dns = DnsConfig::default();
        for (id, tok) in [("hetzner", "h"), ("porkbun", "p")] {
            dns.credentials.push(DnsProviderCredential {
                provider_id: id.into(),
                fields: vec![("api-token".into(), SecretString::from(tok.to_string()))],
                zones: vec![],
                label: String::new(),
                created_at: 0,
            });
        }

        let (candidates, held) = retire_inputs_from(Some(&domains), Some(&dns));
        assert_eq!(candidates, ["example.test", "second.test"]);
        assert_eq!(
            held.iter()
                .map(|h| h.provider_id.as_str())
                .collect::<Vec<_>>(),
            ["hetzner", "porkbun"]
        );
        assert_eq!(held[1].fields["api-token"].as_str(), "p");

        let (none, no_held) = retire_inputs_from(None, None);
        assert!(
            none.is_empty() && no_held.is_empty(),
            "a cold launch passes nothing"
        );
    }
}
