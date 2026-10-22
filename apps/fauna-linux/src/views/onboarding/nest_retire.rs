//! `nest_retire` — retiring an app-provisioned nest
//! (`docs/goal/behavior/nest-retirement.md`, the owner of every rule below).
//! The Linux twin of the tui reference (`apps/fauna-tui/src/wizard/nest_retire.rs`).
//!
//! A paint shell over the shared [`NestRetireMachine`], hosted in its own
//! window exactly as the append-mode wizard is (`build_onboarding_window_append`:
//! a window over whatever the app shows now, a live session included). The
//! machine is not an `OnboardingStep`, so the page does not live in the wizard's
//! `gtk::Stack`; it owns a stack of its own, one child per [`RetirePhase`]:
//! **credentials** (the `vps_config` provider row, credential form and verify
//! button, reused by id — no region or server-type controls) → **list**
//! (`retire-server-item`, its members scoped inside the row) → **confirm** (the
//! typed-name gate) → **running** (two `provisioning-step-row` rows: DNS, then
//! the server) → **done** (the by-hand list).
//!
//! Two entries (`nest-retirement.md` § Layout & flow): `launch-retire-button` on
//! the unreachable-nest surface (`views/launch.rs`) and `admin-nest-retire-button`
//! beside Factory reset (`views/admin.rs`). Back from the credentials state and
//! Done close the window, which puts the entry back as it was; Done after
//! retiring *this session's own box* from the admin entry drops to the launch
//! flow instead ([`crate::settings::escalate_to_launch`] — the session is torn
//! down keeping its credentials), because the nest the session was signed into no
//! longer exists.
//!
//! **Nothing here decides anything.** The typed-name gate and the binding of a
//! run to the confirmed server are the machine's (`confirm_enabled`,
//! `force_server_offered`); every sentence that names what dies or what is left
//! is a machine fold (`confirm_summary`, `leftover_lines`, `transfer_code_note`);
//! the credential fields and the verify gate are the machine's (`visible_fields`,
//! `can_verify`). The VPS token lives in the machine for the page's lifetime and
//! is dropped on exit (`cancel`) — the entries here are paint buffers only.
//!
//! The machine has no observer, so the page repaints on a short timer while its
//! window is up (a run's step states change under the page) and after every
//! async gesture; the list is rebuilt only when its snapshot changed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_client_mail_settings::local_domains::{LocalDomainAction, LocalDomainMachine};
use fauna_onboarding_machine::FieldTypePlain;
use fauna_onboarding_machine::retire::{
    HeldDnsCredential, ManagedServerRow, NestRetireMachine, RememberedDnsSlots, RetireInputs,
    RetirePhase, RetireSnapshot, StepState, TransferCodeState,
};
use fauna_provisioning::{Capability, PROVIDERS};
use fauna_sync_engine::preference_surfaces::{plane_failure, refused_as_not_ready};
use fauna_ui_ids as ids;
use gtk::{Button, Label, Orientation};

use crate::i18n::resolve_key as resolve;
use crate::i18n::strings::onboarding::retire as t;
use crate::testid::{set_test_attr, set_test_id, set_test_scope};

/// Which entry opened the page — where Done sends the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireEntry {
    /// `launch-retire-button` on the unreachable-nest surface.
    Launch,
    /// `admin-nest-retire-button`, over the live session.
    Admin,
}

/// What an entry hands the page.
pub struct RetireOpen {
    pub entry: RetireEntry,
    /// The saved nest's URL: its host is the attribution candidate and, once
    /// resolved, its IPv4 the *current* badge's address.
    pub nest_url: Option<String>,
    /// The admin entry's live local-domain read (`nest-retirement.md` § DNS
    /// cleanup → *Several domains* (1)); `None` pre-login.
    pub local_domains: Option<Arc<LocalDomainMachine>>,
}

/// How long the live input reads may hold verify before the page goes on with
/// what it opened with.
const APP_INPUTS_BOUND: std::time::Duration = std::time::Duration::from_secs(15);

/// How often the page repaints while its window is up.
const REPAINT_MS: u64 = 150;

/// The e2e `vps` and `doh` base-URL overrides (`set_provider_base_urls`). The
/// machine takes them at construction, and the bridge call arrives on the launch
/// surface and post-auth, where no `OnboardingMachine` is registered to hold
/// them — so the agent records them here too
/// ([`remember_e2e_provider_base_urls`]).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static E2E_BASE_URLS: std::sync::Mutex<(Option<String>, Option<String>)> =
    std::sync::Mutex::new((None, None));

/// Record the `vps` and `doh` entries of a `set_provider_base_urls` bridge call.
/// e2e only — compiled out of a release build with the rest of the automation
/// surface.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn remember_e2e_provider_base_urls(json_arg: &str) {
    let map = serde_json::from_str::<std::collections::HashMap<String, String>>(json_arg)
        .unwrap_or_default();
    *E2E_BASE_URLS.lock().unwrap_or_else(|e| e.into_inner()) =
        (map.get("vps").cloned(), map.get("doh").cloned());
}

/// `(vps, doh)` — both `None` in a release build.
fn provider_base_url_overrides() -> (Option<String>, Option<String>) {
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        E2E_BASE_URLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
    {
        (None, None)
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// Open the page in a window over `parent`.
///
/// The machine takes the e2e provider base-URL override at construction, the
/// saved nest's host as an attribution candidate, and — once resolved — that
/// host's IPv4 for the *current* badge (`nest-retirement.md` § Where logic
/// lives: "inputs the app passes at construction, all optional"). The rest of
/// what the app knows goes in too (§ Credential stance (b), § DNS cleanup →
/// *Several domains* (1)): every held DNS credential — the account's DNS record
/// (`fauna.state.dns`), read through the account runtime whenever it is up
/// (always on the admin entry; the launch entry, which runs before any session,
/// normally not) — and the remembered DNS slot names that ride in with the same
/// record, and, from `admin-nest`, the nest's active local domains, re-read live.
pub fn open(parent: &impl IsA<gtk::Window>, spec: RetireOpen) {
    let RetireOpen {
        entry,
        nest_url,
        local_domains,
    } = spec;
    let nest_host = nest_url
        .as_deref()
        .and_then(|url| reqwest::Url::parse(url).ok())
        .and_then(|u| {
            u.host_str()
                .map(|h| (h.to_string(), u.port_or_known_default().unwrap_or(443)))
        });
    let candidate_domains: Vec<String> = nest_host
        .as_ref()
        .filter(|(host, _)| host.parse::<std::net::IpAddr>().is_err())
        .map(|(host, _)| vec![host.clone()])
        .unwrap_or_default();
    let (provider_base_url, doh_base_url) = provider_base_url_overrides();
    let machine = NestRetireMachine::new_with_inputs(RetireInputs {
        candidate_domains,
        current_ipv4: None,
        held_dns: Vec::new(),
        provider_base_url,
        doh_base_url,
        ..RetireInputs::default()
    });

    let page = Page::build(Arc::clone(&machine), entry);
    page.window.set_transient_for(Some(parent));
    page.window.set_modal(true);
    if let Some(app) = parent.application() {
        page.window.set_application(Some(&app));
    }

    spawn_app_inputs(&machine, local_domains, {
        let weak = Rc::downgrade(&page);
        move || {
            if let Some(page) = weak.upgrade() {
                page.repaint();
            }
        }
    });
    if let Some((host, port)) = nest_host {
        spawn_resolve_current(&machine, host, port, {
            let weak = Rc::downgrade(&page);
            move || {
                if let Some(page) = weak.upgrade() {
                    page.repaint();
                }
            }
        });
    }

    page.repaint();
    page.window.present();
}

/// The live input reads, off the GTK thread: the nest's local-domain list
/// (admin entry) and the account's held DNS credentials (whenever the account
/// runtime is up), each handed to the machine as it lands. The machine holds
/// verify until both have landed (`expect_app_inputs`), because verify is where
/// it reads them. Best-effort and bounded: a failed or slow read keeps what the
/// page opened with.
fn spawn_app_inputs(
    machine: &Arc<NestRetireMachine>,
    domains: Option<Arc<LocalDomainMachine>>,
    on_done: impl FnOnce() + 'static,
) {
    let account_store = crate::account_runtime::handle();
    if domains.is_none() && account_store.is_none() {
        return;
    }
    machine.expect_app_inputs();
    let machine = Arc::clone(machine);
    crate::async_helper::run_on_tokio(
        async move {
            let read = async {
                if let Some(domains) = domains
                    && domains.dispatch(LocalDomainAction::Refresh).await.is_ok()
                {
                    // Soft-deleted domains no longer publish, so they are not
                    // candidates.
                    machine.add_candidate_domains(
                        domains
                            .snapshot()
                            .active
                            .iter()
                            .map(|d| d.domain.clone())
                            .collect(),
                    );
                }
                if let Some(store) = account_store {
                    match store.dns().await {
                        Ok(dns) => {
                            machine.set_held_dns(HeldDnsCredential::all_from(&dns));
                            // The remembered DKIM / `_atproto` / `_fauna` names
                            // ride in with the same record (read-only for the
                            // machine).
                            machine.set_remembered_slots(RememberedDnsSlots::from_config(&dns));
                        }
                        Err(e) => {
                            tracing::info!(
                                "nest_retire: DNS record read failed (best-effort): {e:#}"
                            );
                            // A first-listing refusal is not "the account holds
                            // none": say why the held credentials could not be
                            // read (`account-client-lifecycle.md` § The first
                            // listing).
                            let reason = plane_failure(e);
                            if refused_as_not_ready(&reason) {
                                machine.set_input_error(reason);
                            }
                        }
                    }
                }
            };
            if tokio::time::timeout(APP_INPUTS_BOUND, read).await.is_err() {
                tracing::info!("nest_retire: the live input reads timed out (best-effort)");
            }
            machine.app_inputs_landed();
        },
        move |()| on_done(),
    );
}

/// Resolve the saved nest's host off the GTK thread and hand its IPv4 to the
/// machine for the *current* badge. Best-effort: an unresolvable host (the
/// commonest reason to be on the launch retry surface) simply badges nothing.
fn spawn_resolve_current(
    machine: &Arc<NestRetireMachine>,
    host: String,
    port: u16,
    on_done: impl FnOnce() + 'static,
) {
    let machine = Arc::clone(machine);
    crate::async_helper::run_on_tokio(
        async move {
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
            }
        },
        move |()| on_done(),
    );
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// Stack child names — one per [`RetirePhase`] family.
const CHILD_CREDENTIALS: &str = "credentials";
const CHILD_LIST: &str = "list";
const CHILD_CONFIRM: &str = "confirm";
const CHILD_RUNNING: &str = "running";
const CHILD_DONE: &str = "done";

/// "A gesture changed the machine — repaint now", installed by the page. A
/// handler that mutates the machine fires it, so the controls it gates (verify,
/// the hosted sign-in, confirm) are right the instant the gesture returns
/// instead of at the next timer tick.
type Notify = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

fn fire(notify: &Notify) {
    let f = notify.borrow().clone();
    if let Some(f) = f {
        f();
    }
}

struct Page {
    machine: Arc<NestRetireMachine>,
    notify: Notify,
    entry: RetireEntry,
    window: adw::Window,
    stack: gtk::Stack,
    error_label: Label,
    closed: Cell<bool>,
    creds: Credentials,
    list: ListView,
    confirm: ConfirmView,
    running: RunningView,
    done: DoneView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseKind {
    /// Back from credentials, or from a run stopped on a failure.
    Leave,
    /// `retire-done-button`.
    Done,
}

impl Page {
    fn build(machine: Arc<NestRetireMachine>, entry: RetireEntry) -> Rc<Self> {
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.set_vexpand(true);

        let notify: Notify = Rc::new(RefCell::new(None));
        let creds = Credentials::build(&machine, &notify);
        let list = ListView::build();
        let confirm = ConfirmView::build();
        let running = RunningView::build();
        let done = DoneView::build();
        stack.add_named(&creds.root, Some(CHILD_CREDENTIALS));
        stack.add_named(&list.root, Some(CHILD_LIST));
        stack.add_named(&confirm.root, Some(CHILD_CONFIRM));
        stack.add_named(&running.root, Some(CHILD_RUNNING));
        stack.add_named(&done.root, Some(CHILD_DONE));
        stack.set_visible_child_name(CHILD_CREDENTIALS);

        let error_label = Label::new(None);
        error_label.set_halign(gtk::Align::Fill);
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.add_css_class("error-banner");
        error_label.set_visible(false);
        set_test_id(&error_label, ids::ERROR_MESSAGE);

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&Label::new(Some(t::TITLE))));
        let outer = gtk::Box::new(Orientation::Vertical, 0);
        outer.append(&header);
        outer.append(&error_label);
        outer.append(&stack);

        let window = adw::Window::builder()
            .title(t::TITLE)
            .default_width(600)
            .default_height(600)
            .content(&outer)
            .build();

        let page = Rc::new(Self {
            machine,
            notify,
            entry,
            window,
            stack,
            error_label,
            closed: Cell::new(false),
            creds,
            list,
            confirm,
            running,
            done,
        });
        page.wire();
        page
    }

    /// Run `f` on a click, while the page is still alive. Widgets own these
    /// closures and the page owns the widgets, so they hold the page weakly.
    fn on_click(self: &Rc<Self>, btn: &Button, f: impl Fn(&Rc<Page>) + 'static) {
        let weak = Rc::downgrade(self);
        btn.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                f(&page);
            }
        });
    }

    /// Hook every gesture to the machine, and start the repaint timer.
    fn wire(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            *self.notify.borrow_mut() = Some(Rc::new(move || {
                if let Some(page) = weak.upgrade() {
                    page.repaint();
                }
            }));
        }
        // The window's own close (X) is Back from wherever the page is: the
        // token and everything fetched with it are dropped either way.
        {
            let weak = Rc::downgrade(self);
            self.window.connect_close_request(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.machine.cancel();
                    page.closed.set(true);
                }
                gtk::glib::Propagation::Proceed
            });
        }

        // credentials — Back leaves the page, except while the listing is in
        // flight.
        self.on_click(&self.creds.back, |page| {
            if page.machine.snapshot().phase != RetirePhase::Listing {
                page.close(CloseKind::Leave);
            }
        });
        self.on_click(&self.creds.verify, |page| {
            page.creds.verify.set_sensitive(false);
            page.run_async(|m| async move { m.verify().await });
        });

        // list
        self.on_click(&self.list.back, |page| {
            page.machine.back();
            page.repaint();
        });
        self.on_click(&self.list.delete, |page| {
            page.confirm.name.set_text("");
            page.machine.begin_confirm();
            page.repaint();
        });

        // confirm
        {
            let weak = Rc::downgrade(self);
            self.confirm.name.connect_changed(move |e| {
                if let Some(page) = weak.upgrade() {
                    page.machine.set_confirm_name(e.text().to_string());
                    page.repaint();
                }
            });
        }
        self.on_click(&self.confirm.confirm, |page| {
            page.run_async(|m| async move { m.confirm().await })
        });
        self.on_click(&self.confirm.cancel, |page| {
            page.confirm.name.set_text("");
            page.machine.back();
            page.repaint();
        });

        // running
        self.on_click(&self.running.retry, |page| {
            page.run_async(|m| async move { m.retry().await })
        });
        self.on_click(&self.running.force, |page| {
            page.run_async(|m| async move { m.force_server().await })
        });
        // Leaving a run stopped on a failure disarms it (`cancel`), so nothing
        // re-targets the run. Never offered while a step is in flight —
        // `repaint` only shows it after a failure.
        self.on_click(&self.running.back, |page| page.close(CloseKind::Leave));

        // done
        self.on_click(&self.done.copy, |page| {
            crate::clipboard::copy_text(&leftover_text(&page.machine));
        });
        self.on_click(&self.done.done, |page| page.close(CloseKind::Done));

        // The repaint timer: the machine has no observer, and a run's step
        // states change under the page. It is also what keeps the page alive —
        // every widget closure holds the page weakly, so this strong reference
        // is the page's one owner, released when the window closes.
        let page = Rc::clone(self);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(REPAINT_MS), move || {
            if page.closed.get() {
                return gtk::glib::ControlFlow::Break;
            }
            page.repaint();
            gtk::glib::ControlFlow::Continue
        });
    }

    /// The machine's async half of a gesture, off the GTK thread, with a
    /// repaint when it lands.
    fn run_async<Fut>(self: &Rc<Self>, op: impl FnOnce(Arc<NestRetireMachine>) -> Fut)
    where
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let work = op(Arc::clone(&self.machine));
        let weak = Rc::downgrade(self);
        crate::async_helper::run_on_tokio(work, move |()| {
            if let Some(page) = weak.upgrade() {
                page.repaint();
            }
        });
    }

    /// Close the page: drop the token and everything fetched with it, then —
    /// after retiring this session's own box from the admin entry — begin
    /// launch routing again, since the nest the session was signed into no
    /// longer exists.
    fn close(self: &Rc<Self>, kind: CloseKind) {
        if self.closed.replace(true) {
            return;
        }
        let retired_current = self
            .machine
            .snapshot()
            .retired
            .is_some_and(|row| row.current);
        self.machine.cancel();
        self.window.destroy();
        if kind == CloseKind::Done && retired_current && self.entry == RetireEntry::Admin {
            crate::settings::escalate_to_launch("nest-retired");
        }
    }

    /// Paint the machine's snapshot: the error line, the stack child for the
    /// phase, and that child's widgets.
    fn repaint(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        let snap = self.machine.snapshot();

        let msg = snap.error.as_deref().filter(|e| !e.is_empty());
        crate::settings::render_error_label(&self.error_label, msg);

        let child = match snap.phase {
            RetirePhase::Credentials | RetirePhase::Listing => CHILD_CREDENTIALS,
            RetirePhase::List => CHILD_LIST,
            RetirePhase::Confirm => CHILD_CONFIRM,
            RetirePhase::Running => CHILD_RUNNING,
            RetirePhase::Done => CHILD_DONE,
        };
        if self.stack.visible_child_name().as_deref() != Some(child) {
            self.stack.set_visible_child_name(child);
        }

        match snap.phase {
            RetirePhase::Credentials | RetirePhase::Listing => {
                self.creds.repaint(&self.machine, &snap)
            }
            RetirePhase::List => self.list.repaint(self, &snap),
            RetirePhase::Confirm => self.confirm.repaint(&self.machine, &snap),
            RetirePhase::Running => self.running.repaint(&self.machine, &snap),
            RetirePhase::Done => self.done.repaint(&self.machine, &snap),
        }
    }
}

fn page_box() -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 12);
    b.set_margin_top(24);
    b.set_margin_bottom(24);
    b.set_margin_start(32);
    b.set_margin_end(32);
    b
}

fn heading(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("title-2");
    l.set_halign(gtk::Align::Start);
    l
}

fn muted(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("fauna-muted");
    l.set_wrap(true);
    l.set_halign(gtk::Align::Start);
    l.set_xalign(0.0);
    l
}

fn back_button() -> Button {
    let b = Button::with_label(crate::i18n::strings::common::BACK);
    b.add_css_class("flat");
    b.set_halign(gtk::Align::Start);
    set_test_id(&b, ids::RETIRE_BACK_BUTTON);
    b
}

/// Resolve a machine fold — a `{key, args}` pair — through the shared lookup.
fn localized(text: &fauna_core::localized::LocalizedText) -> String {
    text.resolve(crate::i18n::strings::lookup)
}

fn leftover_text(m: &NestRetireMachine) -> String {
    m.leftover_lines()
        .iter()
        .map(localized)
        .collect::<Vec<_>>()
        .join("\n")
}

fn selected_row(snap: &RetireSnapshot) -> Option<&ManagedServerRow> {
    let id = snap.selected.as_deref()?;
    snap.servers.iter().find(|r| r.server_id == id)
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// The `vps_config` provider row, credential form and verify button, by the
/// same ids and in the same shapes that page paints them — no location or
/// server-type controls.
struct Credentials {
    root: gtk::Box,
    provider_buttons: Vec<(String, Button)>,
    section: gtk::Box,
    verify: Button,
    back: Button,
    /// The provider the section was last built for, so it is rebuilt only on a
    /// change (a rebuild every tick would destroy half-typed credentials).
    built_for: RefCell<Option<String>>,
    /// `hosted-auth` field buttons currently in the form, repainted every tick.
    hosted: RefCell<Vec<(String, Button)>>,
    notify: Notify,
}

impl Credentials {
    fn build(m: &Arc<NestRetireMachine>, notify: &Notify) -> Self {
        let root = page_box();
        root.append(&heading(t::TITLE));
        root.append(&muted(t::SUBTITLE));

        let provider_row = gtk::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        set_test_id(&provider_row, ids::VPS_PROVIDER_ROW);
        root.append(&provider_row);

        // Every VPS-capable provider — the listing primitive is on all six
        // adapters, and a box bought through a provider no longer on the curated
        // offer list must still be retirable.
        let mut provider_buttons = Vec::new();
        for p in PROVIDERS
            .iter()
            .filter(|p| p.capabilities.contains(&Capability::Vps))
        {
            let id = p.id.as_str().to_string();
            let btn = Button::with_label(&resolve(p.display_name_key));
            set_test_id(&btn, &format!("vps-provider-row[{id}]"));
            set_test_attr(&btn, "state", "off");
            {
                let m = Arc::clone(m);
                let id = id.clone();
                let notify = Rc::clone(notify);
                btn.connect_clicked(move |_| {
                    m.select_provider(id.clone());
                    fire(&notify);
                });
            }
            provider_row.append(&btn);
            provider_buttons.push((id, btn));
        }

        let section = gtk::Box::new(Orientation::Vertical, 8);
        root.append(&section);

        let verify = Button::with_label(&resolve("provisioning.verify_credentials"));
        verify.add_css_class("suggested-action");
        set_test_id(&verify, ids::VPS_VERIFY_BUTTON);
        verify.set_visible(false);
        root.append(&verify);

        let back = back_button();
        root.append(&back);

        Self {
            root,
            provider_buttons,
            section,
            verify,
            back,
            built_for: RefCell::new(None),
            hosted: RefCell::new(Vec::new()),
            notify: Rc::clone(notify),
        }
    }

    fn repaint(&self, m: &Arc<NestRetireMachine>, snap: &RetireSnapshot) {
        let selected = m.selected_provider();
        for (id, btn) in &self.provider_buttons {
            let on = selected.as_deref() == Some(id.as_str());
            set_test_attr(btn, "state", if on { "on" } else { "off" });
            if on {
                btn.add_css_class("suggested-action");
            } else {
                btn.remove_css_class("suggested-action");
            }
        }
        if *self.built_for.borrow() != selected {
            self.rebuild_section(m, selected.as_deref());
            *self.built_for.borrow_mut() = selected.clone();
        }
        for (field_id, button) in self.hosted.borrow().iter() {
            button.set_label(&m.hosted_auth_button_text(field_id.clone()));
            button.set_sensitive(m.hosted_auth_can_begin(field_id.clone()));
        }
        self.verify.set_visible(selected.is_some());
        self.verify
            .set_sensitive(m.can_verify() && snap.phase == RetirePhase::Credentials);
    }

    fn rebuild_section(&self, m: &Arc<NestRetireMachine>, selected: Option<&str>) {
        while let Some(child) = self.section.first_child() {
            self.section.remove(&child);
        }
        self.hosted.borrow_mut().clear();
        let Some(provider) = selected.and_then(|id| PROVIDERS.iter().find(|p| p.id.as_str() == id))
        else {
            return;
        };

        let link = gtk::LinkButton::with_label(provider.signup_url, provider.signup_url);
        set_test_id(&link, ids::VPS_PROVIDER_LINK);
        self.section.append(&link);

        let open = Button::with_label(&resolve("onboarding.dns_config.open_in_browser"));
        set_test_id(&open, ids::VPS_PROVIDER_OPEN_BROWSER_BUTTON);
        {
            let url = provider.signup_url.to_string();
            open.connect_clicked(move |_| crate::url_opener::open(&url));
        }
        self.section.append(&open);

        let help = muted(&resolve(provider.help_key));
        set_test_id(&help, ids::VPS_PROVIDER_HELP_TEXT);
        self.section.append(&help);

        let form = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        set_test_id(&form, ids::VPS_CREDENTIALS_FORM);
        for f in m.visible_fields() {
            let row = gtk::Box::builder()
                .orientation(Orientation::Vertical)
                .spacing(4)
                .accessible_role(gtk::AccessibleRole::Group)
                .build();
            let label = Label::new(Some(&resolve(&f.label_key)));
            label.set_halign(gtk::Align::Start);
            row.append(&label);
            let test_id = format!("vps-credentials-form-{}", f.id);
            if f.field_type == FieldTypePlain::HostedAuth {
                // A `hosted-auth` field is a button, not an input — the bundled
                // provider's hosted sign-in. Label and pressability are the
                // machine's, repainted every tick.
                let button = Button::new();
                set_test_id(&button, &test_id);
                {
                    let m = Arc::clone(m);
                    let field_id = f.id.clone();
                    button.connect_clicked(move |_| {
                        let m = Arc::clone(&m);
                        let field_id = field_id.clone();
                        crate::async_helper::run_on_tokio(
                            async move {
                                // A failure is already the field's `Failed`
                                // state, painted as the button label.
                                if let Some(prompt) = m.hosted_auth_begin(field_id.clone()).await {
                                    crate::url_opener::open(&prompt.verification_url);
                                    m.hosted_auth_wait(field_id).await;
                                }
                            },
                            |()| {},
                        );
                    });
                }
                row.append(&button);
                self.hosted.borrow_mut().push((f.id.clone(), button));
            } else {
                let entry = gtk::Entry::builder().hexpand(true).build();
                if f.field_type == FieldTypePlain::Secret {
                    entry.set_visibility(false);
                }
                set_test_id(&entry, &test_id);
                {
                    let m = Arc::clone(m);
                    let field_id = f.id.clone();
                    let notify = Rc::clone(&self.notify);
                    entry.connect_changed(move |e| {
                        m.set_credential_field(field_id.clone(), e.text().to_string());
                        fire(&notify);
                    });
                }
                row.append(&entry);
            }
            form.append(&row);
        }
        self.section.append(&form);
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/// What the rows were last built from: the rows and each row's transfer-code
/// note (a machine fold, so it rides in the signature). The selection is NOT in
/// it — a selection change re-syncs the built rows in place, because a rebuild
/// inside the very toggle that caused it would tear down the emitting widget.
type ListSignature = (Vec<ManagedServerRow>, Vec<Option<String>>);

/// One built row's selection-dependent parts.
struct BuiltRow {
    server_id: String,
    check: gtk::CheckButton,
    /// The transfer-code affordance — present in every row, shown only on the
    /// selected one.
    transfer: gtk::Box,
}

struct ListView {
    root: gtk::Box,
    empty_message: Label,
    list_label: Label,
    list: gtk::Box,
    delete: Button,
    back: Button,
    built_from: RefCell<Option<ListSignature>>,
    built_rows: RefCell<Vec<BuiltRow>>,
    /// Set while a repaint drives the checks, so their `toggled` handlers do not
    /// echo the sync back into the machine.
    syncing: Rc<Cell<bool>>,
}

impl ListView {
    fn build() -> Self {
        let root = page_box();
        root.append(&heading(t::TITLE));

        let empty_message = muted(t::EMPTY_MESSAGE);
        set_test_id(&empty_message, ids::RETIRE_SERVER_EMPTY_MESSAGE);
        root.append(&empty_message);

        let list_label = muted(t::LIST_LABEL);
        root.append(&list_label);

        let list = gtk::Box::new(Orientation::Vertical, 8);
        set_test_id(&list, ids::RETIRE_SERVER_LIST);
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        root.append(&scroll);

        let delete = Button::with_label(t::DELETE_BUTTON);
        delete.add_css_class("destructive-action");
        set_test_id(&delete, ids::RETIRE_DELETE_BUTTON);
        root.append(&delete);

        let back = back_button();
        root.append(&back);

        Self {
            root,
            empty_message,
            list_label,
            list,
            delete,
            back,
            built_from: RefCell::new(None),
            built_rows: RefCell::new(Vec::new()),
            syncing: Rc::new(Cell::new(false)),
        }
    }

    fn repaint(&self, page: &Rc<Page>, snap: &RetireSnapshot) {
        let m = &page.machine;
        let empty = snap.servers.is_empty();
        self.empty_message.set_visible(empty);
        self.list_label.set_visible(!empty);
        self.delete.set_sensitive(selected_row(snap).is_some());

        let notes: Vec<Option<String>> = snap
            .servers
            .iter()
            .map(|r| {
                m.transfer_code_note(r.server_id.clone())
                    .map(|n| localized(&n))
            })
            .collect();
        let signature = (snap.servers.clone(), notes);
        if self.built_from.borrow().as_ref() != Some(&signature) {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
            let mut built = Vec::new();
            for (row, note) in snap.servers.iter().zip(signature.1.iter()) {
                let (wrapper, built_row) = build_row(page, &self.syncing, row, note.as_deref());
                self.list.append(&wrapper);
                built.push(built_row);
            }
            *self.built_rows.borrow_mut() = built;
            *self.built_from.borrow_mut() = Some(signature);
        }

        // The selection, synced in place.
        self.syncing.set(true);
        for row in self.built_rows.borrow().iter() {
            let on = snap.selected.as_deref() == Some(row.server_id.as_str());
            if row.check.is_active() != on {
                row.check.set_active(on);
            }
            row.transfer.set_visible(on);
        }
        self.syncing.set(false);
    }
}

/// One listed server, its members scoped inside `retire-server-item[n]` (the
/// scoped-descendant idiom, e2e convention 1): the id sits on the selectable
/// check, the wrapper is declared its scope container.
fn build_row(
    page: &Rc<Page>,
    syncing: &Rc<Cell<bool>>,
    row: &ManagedServerRow,
    transfer_note: Option<&str>,
) -> (gtk::Box, BuiltRow) {
    let wrapper = gtk::Box::new(Orientation::Vertical, 4);
    wrapper.add_css_class("card");
    set_test_scope(&wrapper, ids::RETIRE_SERVER_ITEM);

    let select = gtk::CheckButton::with_label(&row.name);
    set_test_id(&select, ids::RETIRE_SERVER_ITEM);
    {
        let weak = Rc::downgrade(page);
        let syncing = Rc::clone(syncing);
        let server_id = row.server_id.clone();
        select.connect_toggled(move |c| {
            if syncing.get() || !c.is_active() {
                return;
            }
            if let Some(page) = weak.upgrade() {
                page.machine.select(server_id.clone());
                page.repaint();
            }
        });
    }
    wrapper.append(&select);

    let name = Label::new(Some(&row.name));
    name.set_halign(gtk::Align::Start);
    set_test_id(&name, ids::RETIRE_SERVER_NAME);
    wrapper.append(&name);

    let member = |id: &str, caption: Option<&str>, value: &str| -> gtk::Box {
        let line = gtk::Box::new(Orientation::Horizontal, 8);
        if let Some(caption) = caption {
            let c = Label::new(Some(caption));
            c.add_css_class("fauna-muted");
            line.append(&c);
        }
        let v = Label::new(Some(value));
        v.set_halign(gtk::Align::Start);
        v.set_selectable(true);
        set_test_id(&v, id);
        line.append(&v);
        line
    };
    if let Some(domain) = &row.domain {
        wrapper.append(&member(
            ids::RETIRE_SERVER_DOMAIN,
            Some(t::DOMAIN_LABEL),
            domain,
        ));
    }
    if !row.secondary_domains.is_empty() {
        wrapper.append(&member(
            ids::RETIRE_SERVER_SECONDARY_DOMAINS,
            Some(t::SECONDARY_DOMAINS_LABEL),
            &row.secondary_domains.join(", "),
        ));
    }
    if let Some(ipv4) = &row.ipv4 {
        wrapper.append(&member(
            ids::RETIRE_SERVER_ADDRESS,
            Some(t::ADDRESS_LABEL),
            ipv4,
        ));
    }
    if row.current {
        wrapper.append(&member(
            ids::RETIRE_SERVER_CURRENT_BADGE,
            None,
            t::CURRENT_BADGE,
        ));
    }
    if !row.marked {
        wrapper.append(&member(
            ids::RETIRE_SERVER_UNMARKED_NOTE,
            None,
            t::UNMARKED_NOTE,
        ));
    }

    // The transfer-code affordance sits on the selected row and is independent
    // of deleting. Built into every row, shown (by the list's selection sync)
    // only on the selected one.
    let transfer = gtk::Box::new(Orientation::Vertical, 4);
    transfer.set_visible(false);
    match &row.transfer_code {
        TransferCodeState::Idle => {
            let b = Button::with_label(t::TRANSFER_CODE_BUTTON);
            set_test_id(&b, ids::RETIRE_TRANSFER_CODE_BUTTON);
            page.on_click(&b, |page| {
                page.run_async(|m| async move { m.fetch_transfer_code().await })
            });
            transfer.append(&b);
        }
        TransferCodeState::Code { code } => {
            transfer.append(&member(
                ids::RETIRE_TRANSFER_CODE_VALUE,
                Some(t::TRANSFER_CODE_LABEL),
                code,
            ));
            let copy = Button::with_label(t::TRANSFER_CODE_COPY);
            set_test_id(&copy, ids::RETIRE_TRANSFER_CODE_COPY_BUTTON);
            let code = code.clone();
            copy.connect_clicked(move |_| crate::clipboard::copy_text(&code));
            transfer.append(&copy);
        }
        _ => {}
    }
    if let Some(note) = transfer_note {
        transfer.append(&member(ids::RETIRE_TRANSFER_CODE_STATUS, None, note));
    }
    wrapper.append(&transfer);

    let built = BuiltRow {
        server_id: row.server_id.clone(),
        check: select,
        transfer,
    };
    (wrapper, built)
}

// ---------------------------------------------------------------------------
// Confirm
// ---------------------------------------------------------------------------

/// The typed-name gate (`nest-retirement.md` § Confirm shape).
struct ConfirmView {
    root: gtk::Box,
    summary: Label,
    name: gtk::Entry,
    confirm: Button,
    cancel: Button,
}

impl ConfirmView {
    fn build() -> Self {
        let root = page_box();
        root.append(&heading(t::TITLE));

        let summary = Label::new(None);
        summary.set_wrap(true);
        summary.set_xalign(0.0);
        summary.set_halign(gtk::Align::Start);
        summary.set_selectable(true);
        set_test_id(&summary, ids::RETIRE_CONFIRM_SUMMARY);
        root.append(&summary);

        root.append(&muted(t::CONFIRM_NAME_LABEL));
        let name = gtk::Entry::builder().hexpand(true).build();
        set_test_id(&name, ids::RETIRE_CONFIRM_NAME_INPUT);
        root.append(&name);

        let confirm = Button::with_label(t::CONFIRM_BUTTON);
        confirm.add_css_class("destructive-action");
        set_test_id(&confirm, ids::RETIRE_CONFIRM_BUTTON);
        root.append(&confirm);

        let cancel = Button::with_label(t::CANCEL_BUTTON);
        set_test_id(&cancel, ids::RETIRE_CANCEL_BUTTON);
        root.append(&cancel);

        Self {
            root,
            summary,
            name,
            confirm,
            cancel,
        }
    }

    fn repaint(&self, m: &NestRetireMachine, snap: &RetireSnapshot) {
        // The provider rides as its display KEY, so it is resolved too.
        let text = m
            .confirm_summary()
            .iter()
            .map(|line| line.resolve_nested(crate::i18n::strings::lookup))
            .collect::<Vec<_>>()
            .join("\n");
        if self.summary.text() != text {
            self.summary.set_text(&text);
        }
        self.confirm.set_sensitive(snap.confirm_enabled);
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// The two step rows, reusing the `provisioning-progress` component's row and
/// child ids and its glyph vocabulary.
struct StepRows {
    root: gtk::Box,
    rows: Vec<StepRow>,
}

struct StepRow {
    glyph: Label,
    label: Label,
    substep: Label,
    error: Label,
}

impl StepRows {
    fn build() -> Self {
        let root = gtk::Box::new(Orientation::Vertical, 8);
        let mut rows = Vec::new();
        for _ in 0..2 {
            let row = gtk::Box::builder()
                .orientation(Orientation::Horizontal)
                .spacing(8)
                .accessible_role(gtk::AccessibleRole::Group)
                .build();
            set_test_id(&row, ids::PROVISIONING_STEP_ROW);
            let glyph = Label::new(None);
            set_test_id(&glyph, ids::PROVISIONING_STEP_CHECKBOX);
            row.append(&glyph);
            let label = Label::new(None);
            label.set_halign(gtk::Align::Start);
            set_test_id(&label, ids::PROVISIONING_STEP_LABEL);
            row.append(&label);
            let substep = Label::new(None);
            substep.add_css_class("fauna-muted");
            substep.set_visible(false);
            set_test_id(&substep, ids::PROVISIONING_SUBSTEP);
            row.append(&substep);
            let error = Label::new(None);
            error.add_css_class("error");
            error.set_wrap(true);
            error.set_visible(false);
            set_test_id(&error, ids::PROVISIONING_STEP_ERROR);
            row.append(&error);
            root.append(&row);
            rows.push(StepRow {
                glyph,
                label,
                substep,
                error,
            });
        }
        Self { root, rows }
    }

    fn repaint(&self, m: &NestRetireMachine, snap: &RetireSnapshot) {
        for (row, step) in self.rows.iter().zip(snap.steps.iter()) {
            row.glyph
                .set_text(&fauna_provisioning::progress::status_glyph(
                    step.state.progress_status(),
                ));
            row.label.set_text(&localized(&m.step_label(step.step)));
            match m.step_note(step.state.clone()) {
                Some(note) => {
                    row.substep.set_text(&localized(&note));
                    row.substep.set_visible(true);
                }
                None => row.substep.set_visible(false),
            }
            match &step.state {
                StepState::Failed { cause } => {
                    row.error.set_text(cause);
                    row.error.set_visible(true);
                }
                _ => row.error.set_visible(false),
            }
        }
    }
}

struct RunningView {
    root: gtk::Box,
    steps: StepRows,
    retry: Button,
    force_warning: Label,
    force: Button,
    back: Button,
}

impl RunningView {
    fn build() -> Self {
        let root = page_box();
        root.append(&heading(t::TITLE));
        let steps = StepRows::build();
        root.append(&steps.root);

        let retry = Button::with_label(t::RETRY_BUTTON);
        set_test_id(&retry, ids::RETIRE_RETRY_BUTTON);
        retry.set_visible(false);
        root.append(&retry);

        // The dangling-record warning, repeated where the choice is made.
        let force_warning = muted(t::FORCE_SERVER_WARNING);
        set_test_id(&force_warning, ids::RETIRE_FORCE_SERVER_WARNING);
        force_warning.set_visible(false);
        root.append(&force_warning);
        let force = Button::with_label(t::FORCE_SERVER_BUTTON);
        force.add_css_class("destructive-action");
        set_test_id(&force, ids::RETIRE_FORCE_SERVER_BUTTON);
        force.set_visible(false);
        root.append(&force);

        let back = back_button();
        back.set_visible(false);
        root.append(&back);

        Self {
            root,
            steps,
            retry,
            force_warning,
            force,
            back,
        }
    }

    fn repaint(&self, m: &NestRetireMachine, snap: &RetireSnapshot) {
        self.steps.repaint(m, snap);
        let failed = snap
            .steps
            .iter()
            .any(|s| matches!(s.state, StepState::Failed { .. }));
        self.retry.set_visible(failed);
        self.force_warning.set_visible(snap.force_server_offered);
        self.force.set_visible(snap.force_server_offered);
        // Leaving is offered only once the run has stopped on a failure — never
        // while a step is still in flight.
        self.back.set_visible(failed);
    }
}

// ---------------------------------------------------------------------------
// Done
// ---------------------------------------------------------------------------

struct DoneView {
    root: gtk::Box,
    outcome: Label,
    steps: StepRows,
    leftover: Label,
    copy: Button,
    done: Button,
}

impl DoneView {
    fn build() -> Self {
        let root = page_box();
        root.append(&heading(t::TITLE));
        let outcome = Label::new(None);
        outcome.set_halign(gtk::Align::Start);
        root.append(&outcome);
        let steps = StepRows::build();
        root.append(&steps.root);

        let leftover = Label::new(None);
        leftover.set_wrap(true);
        leftover.set_xalign(0.0);
        leftover.set_halign(gtk::Align::Start);
        leftover.set_selectable(true);
        set_test_id(&leftover, ids::RETIRE_DNS_LEFTOVER_TEXT);
        root.append(&leftover);

        let copy = Button::with_label(t::LEFTOVER_COPY);
        set_test_id(&copy, ids::RETIRE_DNS_LEFTOVER_COPY_BUTTON);
        root.append(&copy);

        let done = Button::with_label(t::DONE_BUTTON);
        done.add_css_class("suggested-action");
        set_test_id(&done, ids::RETIRE_DONE_BUTTON);
        root.append(&done);

        Self {
            root,
            outcome,
            steps,
            leftover,
            copy,
            done,
        }
    }

    fn repaint(&self, m: &NestRetireMachine, snap: &RetireSnapshot) {
        match &snap.retired {
            Some(row) => {
                self.outcome.set_text(&t::done_deleted(&row.name));
                self.outcome.set_visible(true);
            }
            None => self.outcome.set_visible(false),
        }
        self.steps.repaint(m, snap);
        let text = leftover_text(m);
        if self.leftover.text() != text {
            self.leftover.set_text(&text);
        }
    }
}
