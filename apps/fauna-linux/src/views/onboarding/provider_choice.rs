//! Stage 4d of handle-first onboarding: choose providers
//! (`docs/goal/behavior/onboarding-provisioning.md` § 3d).
//!
//! ui.yaml `onboarding.provider_choice`: the goal lines
//! `provider-goal-row[domain|dns|vps]` inside the `provider-goal-row`
//! container (with `provider-goal-select[...]` only when more than one
//! connected provider can cover the goal), the provider rows
//! `provider-row[<provider-id>|manual]` inside `provider-row`,
//! `dns-buy-domain-checkbox` and `dns-no-provider-message` (moved from
//! `dns_config`), `provider-choice-status-text` and Back / Continue — and,
//! while a provider row is open, its `provider-dialog` (link, open-in-browser,
//! help, `provider-credentials-form-{field}`, Connect / Disconnect, the
//! registrar note or contact form, a status line, Close), painted under its
//! own row. tui led (`apps/fauna-tui/src/wizard/provider_choice.rs`); this is
//! the same element list in GTK's retained mode.
//!
//! **Every rule is the machine's** (§ 3d *Where logic lives*):
//!
//! | Question | Shared getter |
//! |---|---|
//! | Which goals, covered by whom, at what price? | `goal_rows()` |
//! | What can each provider do, is it connected, why can't it cover a goal? | `provider_rows()` |
//! | Offer the buy box? | `can_offer_buy_domain()` |
//! | Which fields does a dialog show? | `visible_provider_fields(id)` |
//! | Can Connect be pressed, and if not why? | `connect_provider_possibility(id)` |
//! | Registrar note / contact form? | `provider_registrar_notes_key(id)` / `provider_shows_contact_form(id)` |
//! | Why is Continue dead? | `providers_continue_blocked_reason()` |
//!
//! Which dialog is open is the one piece of state this page keeps: it is
//! presentation, like tui's `Wizard::provider_dialog`, and decides nothing
//! about coverage.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, CheckButton, Label, Orientation};

use crate::i18n::resolve_key as resolve;
use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::dns_config as dns_t;
use crate::i18n::strings::onboarding::provider_choice as t;
use crate::testid::{set_test_attr, set_test_id, set_test_option_keys};
use fauna_onboarding_machine::{
    CapabilityPlain, GoalCover, GoalRow, LocalizedText, OnboardingMachine, ProviderGoal,
    ProviderRow,
};
use fauna_provisioning::PROVIDERS;

use super::generic_provider_form::{self, CredTarget, HostedAuthHandle};

/// The manual row's key in `provider-row[...]` and `provider-goal-select[dns]`.
/// The shared machine's `MANUAL_ROW_ID`.
const MANUAL: &str = "manual";

fn localized(text: &LocalizedText) -> String {
    text.resolve(crate::i18n::strings::lookup)
}

/// A can-cover reason or the DNS goal's note. `resolve_nested`, not
/// `localized`: its `{provider}` argument is the provider's display-name key.
fn reason_text(text: &LocalizedText) -> String {
    text.resolve_nested(crate::i18n::strings::lookup)
}

/// A provider's display name, or the id for one the registry does not know.
fn provider_name(id: &str) -> String {
    if id == MANUAL {
        return t::MANUAL_ROW.to_string();
    }
    PROVIDERS
        .iter()
        .find(|p| p.id.as_str() == id)
        .map(|p| resolve(p.display_name_key))
        .unwrap_or_else(|| id.to_string())
}

fn goal_key(goal: ProviderGoal) -> &'static str {
    match goal {
        ProviderGoal::Domain => "domain",
        ProviderGoal::Dns => "dns",
        ProviderGoal::Vps => "vps",
    }
}

fn cover_key(cover: &GoalCover) -> String {
    match cover {
        GoalCover::Provider { provider_id } => provider_id.clone(),
        GoalCover::Manual => MANUAL.to_string(),
    }
}

/// One goal pick, from the manual row or a goal's select. The machine
/// refuses only a cover that cannot cover the goal, which neither surface
/// offers; a refusal that slips through still reaches `error-message`
/// (`docs/goal/ui/README.md` § Offer only what is possible, rule 7).
fn pick_cover(m: &OnboardingMachine, goal: ProviderGoal, key: &str) {
    m.surface(m.select_goal_cover(goal, cover_of(key)));
}

fn cover_of(key: &str) -> GoalCover {
    if key == MANUAL {
        GoalCover::Manual
    } else {
        GoalCover::Provider {
            provider_id: key.to_string(),
        }
    }
}

fn goal_line(domain: &str, row: &GoalRow) -> String {
    let name = match row.goal {
        ProviderGoal::Domain => t::goal_domain(domain),
        ProviderGoal::Dns => t::goal_dns(domain),
        ProviderGoal::Vps => t::GOAL_VPS.to_string(),
    };
    let status = match &row.cover {
        Some(c) => t::goal_covered_by(&provider_name(&cover_key(c))),
        None => t::GOAL_OPEN.to_string(),
    };
    let mut line = format!("{name} — {status}");
    if let Some(price) = row.price_cents {
        let cur = row.currency.clone().unwrap_or_else(|| "USD".to_string());
        let first = fauna_onboarding_machine::format_price(price, cur.clone());
        let priced = match row.renewal_cents {
            Some(r) => t::goal_price(&first, &fauna_onboarding_machine::format_price(r, cur)),
            None => t::goal_price_first_year(&first),
        };
        line.push_str(" · ");
        line.push_str(&priced);
    }
    line
}

/// What a capability lets the provider do, in the row's words. `Zone` is a
/// field kind only, never a provider capability, so it names nothing.
fn capability_label(c: &CapabilityPlain) -> Option<&'static str> {
    match c {
        CapabilityPlain::Registrar => Some(t::CAPABILITY_REGISTRAR),
        CapabilityPlain::Dns => Some(t::CAPABILITY_DNS),
        CapabilityPlain::Vps => Some(t::CAPABILITY_VPS),
        CapabilityPlain::Zone => None,
    }
}

/// A provider row's text: its name, what it can do, whether it is connected.
fn row_text(row: &ProviderRow) -> String {
    if row.provider_id == MANUAL {
        return format!("{} — {}", t::MANUAL_ROW, t::MANUAL_ROW_HELP);
    }
    let caps: Vec<&str> = row
        .capabilities
        .iter()
        .filter_map(capability_label)
        .collect();
    let state = if row.connecting {
        t::STATE_CONNECTING
    } else if row.connected {
        t::STATE_CONNECTED
    } else {
        t::STATE_AVAILABLE
    };
    format!(
        "{} — {} · {state}",
        provider_name(&row.provider_id),
        caps.join(", ")
    )
}

/// The un-id'd line beneath a row: why it cannot cover the goals it cannot
/// (rule 5), from the one can-cover computation, a reason repeated across
/// goals said once. The manual row's server and registration reasons restate
/// what the row is (DNS only); only its DNS reason explains anything.
fn reason_line(row: &ProviderRow) -> Option<String> {
    if row.provider_id == MANUAL {
        return row.dns_reason.as_ref().map(reason_text);
    }
    let mut seen: Vec<String> = Vec::new();
    for r in [&row.domain_reason, &row.dns_reason, &row.vps_reason]
        .into_iter()
        .flatten()
    {
        let text = reason_text(r);
        if !seen.contains(&text) {
            seen.push(text);
        }
    }
    (!seen.is_empty()).then(|| seen.join(" "))
}

/// A vertical `Group`-role box: AT-SPI on Linux can omit a plain `Generic`
/// box from the tree, which hides every id inside it from the bridge.
fn group_box(spacing: i32) -> gtk::Box {
    gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(spacing)
        .accessible_role(gtk::AccessibleRole::Group)
        .build()
}

fn wrapped_label(css: Option<&str>) -> Label {
    let l = Label::new(None);
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.set_halign(gtk::Align::Start);
    if let Some(css) = css {
        l.add_css_class(css);
    }
    l
}

/// One provider row's widgets, built once — the registry's rows and the
/// manual row are a fixed list.
struct RowWidgets {
    id: String,
    button: Button,
    held: Label,
    reason: Label,
    /// Where this row's dialog is painted, directly under it.
    dialog_slot: gtk::Box,
}

/// The open dialog's widgets and the field set its form was built from.
struct Dialog {
    id: String,
    root: gtk::Box,
    notes: Label,
    form: gtk::Box,
    field_ids: Vec<String>,
    hosted_auth: Vec<HostedAuthHandle>,
    connect: Button,
    disconnect: Button,
    contact_refresh: Rc<dyn Fn()>,
    status: Label,
}

/// The page's repaint, reachable from a press that changes only this page's
/// own state (opening or closing a dialog) — held weakly, so the widgets the
/// repaint captures do not keep themselves alive through their own handlers.
type RepaintCell = Rc<RefCell<Weak<dyn Fn()>>>;

fn repaint(cell: &RepaintCell) {
    if let Some(f) = cell.borrow().upgrade() {
        f();
    }
}

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    let title = Label::new(Some(t::TITLE));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    let description = wrapped_label(Some("fauna-muted"));
    description.set_text(t::DESCRIPTION);
    root.append(&description);

    let error_label = wrapped_label(Some("error"));
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    let unset: Weak<dyn Fn()> = Weak::<fn()>::new();
    let repaint_cell: RepaintCell = Rc::new(RefCell::new(unset));
    let open: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // -- The goals at the top, in page order: rebuilt only when what they say
    //    changes, so a goal's select is never torn down under the pointer. --
    let goals_box = group_box(4);
    set_test_id(&goals_box, ids::PROVIDER_GOAL_ROW);
    root.append(&goals_box);

    // -- The buy box (moved from dns_config), offered only for a domain that
    //    can be bought; a repaint must not read as a press. --
    let buy_check = CheckButton::with_label(dns_t::BUY_DOMAIN_CHECKBOX);
    set_test_id(&buy_check, ids::DNS_BUY_DOMAIN_CHECKBOX);
    buy_check.set_visible(false);
    let buy_syncing = Rc::new(Cell::new(false));
    {
        let m = m.clone();
        let buy_syncing = buy_syncing.clone();
        buy_check.connect_toggled(move |b| {
            if !buy_syncing.get() {
                m.toggle_buy_domain(b.is_active());
            }
        });
    }
    root.append(&buy_check);

    let no_provider = wrapped_label(None);
    set_test_id(&no_provider, ids::DNS_NO_PROVIDER_MESSAGE);
    no_provider.set_visible(false);
    root.append(&no_provider);

    // -- The providers, then the manual row. --
    let providers_box = group_box(8);
    set_test_id(&providers_box, ids::PROVIDER_ROW);
    let heading = Label::new(Some(t::PROVIDERS_HEADING));
    heading.add_css_class("heading");
    heading.set_halign(gtk::Align::Start);
    providers_box.append(&heading);
    let mut rows: Vec<RowWidgets> = Vec::new();
    for row in m.provider_rows() {
        let column = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();
        let id = row.provider_id.clone();
        let button = Button::with_label(&row_text(&row));
        set_test_id(&button, &format!("provider-row[{id}]"));
        {
            let m = m.clone();
            let id = id.clone();
            let open = open.clone();
            let repaint_cell = repaint_cell.clone();
            button.connect_clicked(move |_| {
                if id == MANUAL {
                    // The manual row covers DNS only.
                    pick_cover(&m, ProviderGoal::Dns, MANUAL);
                } else {
                    *open.borrow_mut() = Some(id.clone());
                    repaint(&repaint_cell);
                }
            });
        }
        column.append(&button);
        let held = wrapped_label(Some("dim-label"));
        held.set_text(t::HELD_CREDENTIAL);
        held.set_visible(false);
        column.append(&held);
        let reason = wrapped_label(Some("dim-label"));
        reason.set_visible(false);
        column.append(&reason);
        let dialog_slot = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .build();
        column.append(&dialog_slot);
        providers_box.append(&column);
        rows.push(RowWidgets {
            id,
            button,
            held,
            reason,
            dialog_slot,
        });
    }
    root.append(&providers_box);

    // -- Status line, then Back / Continue. --
    let status = wrapped_label(None);
    set_test_id(&status, ids::PROVIDER_CHOICE_STATUS_TEXT);
    root.append(&status);

    let nav = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    nav.set_margin_top(12);
    let back_btn = Button::with_label(common::BACK);
    set_test_id(&back_btn, ids::PROVIDER_CHOICE_BACK_BUTTON);
    {
        let m = m.clone();
        let open = open.clone();
        back_btn.connect_clicked(move |_| {
            *open.borrow_mut() = None;
            m.back();
        });
    }
    nav.append(&back_btn);
    let continue_btn = Button::with_label(common::CONTINUE);
    continue_btn.add_css_class("suggested-action");
    set_test_id(&continue_btn, ids::PROVIDER_CHOICE_CONTINUE_BUTTON);
    {
        let m = m.clone();
        let open = open.clone();
        let error = error_label.clone();
        continue_btn.connect_clicked(move |_| {
            match m.continue_from_providers() {
                // The machine advances the step → the orchestrator swaps page.
                Ok(()) => *open.borrow_mut() = None,
                Err(e) => crate::settings::render_error_label(&error, Some(&format!("{e}"))),
            }
        });
    }
    nav.append(&continue_btn);
    root.append(&nav);

    let last_goals: RefCell<Option<(String, Vec<GoalRow>)>> = RefCell::new(None);
    let dialog: RefCell<Option<Dialog>> = RefCell::new(None);

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let repaint_cell = repaint_cell.clone();
        move || {
            // Goals: rebuilt only when the domain or a goal line changes.
            let domain = m.provider_goal_domain();
            let goals = m.goal_rows();
            let key = (domain.clone(), goals.clone());
            if last_goals.borrow().as_ref() != Some(&key) {
                rebuild_goals(&goals_box, &m, &domain, &goals);
                *last_goals.borrow_mut() = Some(key);
            }

            let offer_buy = m.can_offer_buy_domain();
            buy_check.set_visible(offer_buy);
            buy_syncing.set(true);
            let buying = m.dns_config().buy_domain;
            if buy_check.is_active() != buying {
                buy_check.set_active(buying);
            }
            buy_syncing.set(false);
            if m.should_show_no_provider_message() {
                let tld =
                    fauna_onboarding_machine::handle_tld(m.current_handle()).unwrap_or_default();
                no_provider.set_text(&dns_t::no_provider_carries_tld(&tld));
                no_provider.set_visible(true);
            } else {
                no_provider.set_visible(false);
            }

            // Provider rows, in place.
            let dns_cover = goals
                .iter()
                .find(|g| g.goal == ProviderGoal::Dns)
                .and_then(|g| g.cover.clone());
            let provider_rows = m.provider_rows();
            for w in &rows {
                let Some(row) = provider_rows.iter().find(|r| r.provider_id == w.id) else {
                    continue;
                };
                let text = row_text(row);
                if w.button.label().as_deref() != Some(text.as_str()) {
                    w.button.set_label(&text);
                }
                let connected = if w.id == MANUAL {
                    dns_cover == Some(GoalCover::Manual)
                } else {
                    row.connected
                };
                set_test_attr(
                    &w.button,
                    "state",
                    if connected { "connected" } else { "available" },
                );
                // The manual row covers DNS only; its pick's answer is the
                // machine's (`ui/README.md` § Offer only what is possible,
                // rule 6), the one `select_goal_cover` asks first, and its
                // reason stays on the row's line beneath.
                if w.id == MANUAL {
                    crate::possible::paint(
                        &w.button,
                        None,
                        m.goal_cover_possibility(ProviderGoal::Dns, GoalCover::Manual),
                    );
                }
                w.held.set_visible(row.held);
                match reason_line(row) {
                    Some(text) => {
                        w.reason.set_text(&text);
                        w.reason.set_visible(true);
                    }
                    None => w.reason.set_visible(false),
                }
            }

            // The dialog: built when a row opens, torn down when it closes.
            let want = open.borrow().clone();
            let have = dialog.borrow().as_ref().map(|d| d.id.clone());
            if want != have {
                if let Some(old) = dialog.borrow_mut().take()
                    && let Some(w) = rows.iter().find(|w| w.id == old.id)
                {
                    w.dialog_slot.remove(&old.root);
                }
                if let Some(id) = want.as_deref()
                    && let Some(w) = rows.iter().find(|w| w.id == id)
                {
                    let d = build_dialog(&m, id, &open, &repaint_cell);
                    w.dialog_slot.append(&d.root);
                    *dialog.borrow_mut() = Some(d);
                }
            }
            if let Some(d) = dialog.borrow_mut().as_mut()
                && let Some(row) = provider_rows.iter().find(|r| r.provider_id == d.id)
            {
                refresh_dialog(d, &m, row);
            }

            // Status line and Continue, from the one answer.
            // Nested args: `status_contact_missing`'s `{provider}` is the
            // registrar's display-name key (onboarding-provisioning.md § 3d).
            let answer = m
                .providers_continue_possibility()
                .resolving_nested_args(crate::i18n::strings::lookup);
            status.set_text(&answer.reason().map(localized).unwrap_or_default());
            crate::possible::paint(&continue_btn, None, answer);

            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });
    *repaint_cell.borrow_mut() = Rc::downgrade(&refresh);

    (root, refresh)
}

/// Repaint the goal lines from `goal_rows()`: each line keyed by its goal,
/// carrying `state` (covered / open) and the covering `provider`; the goal's
/// note beneath it; the choice only where there is one to make.
fn rebuild_goals(
    container: &gtk::Box,
    m: &Arc<OnboardingMachine>,
    domain: &str,
    goals: &[GoalRow],
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for row in goals {
        let g = goal_key(row.goal);
        let line = wrapped_label(None);
        line.set_text(&goal_line(domain, row));
        set_test_id(&line, &format!("provider-goal-row[{g}]"));
        set_test_attr(
            &line,
            "state",
            if row.cover.is_some() {
                "covered"
            } else {
                "open"
            },
        );
        if let Some(c) = &row.cover {
            set_test_attr(&line, "provider", &cover_key(c));
        }
        container.append(&line);
        // DNS covered at a provider whose zone the domain does not point at
        // yet (§ 3d, *A zone the domain does not point at*).
        if let Some(note) = &row.note {
            let note_label = wrapped_label(Some("dim-label"));
            note_label.set_text(&reason_text(note));
            container.append(&note_label);
        }
        if row.candidates.len() > 1 {
            let names: Vec<String> = row.candidates.iter().map(|c| provider_name(c)).collect();
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let select = gtk::DropDown::from_strings(&name_refs);
            set_test_id(&select, &format!("provider-goal-select[{g}]"));
            let keys: Vec<&str> = row.candidates.iter().map(String::as_str).collect();
            set_test_option_keys(&select, &keys);
            if let Some(i) = row
                .cover
                .as_ref()
                .and_then(|c| row.candidates.iter().position(|k| *k == cover_key(c)))
            {
                select.set_selected(i as u32);
            }
            {
                let m = m.clone();
                let goal = row.goal;
                let candidates = row.candidates.clone();
                select.connect_selected_notify(move |d| {
                    if let Some(k) = candidates.get(d.selected() as usize) {
                        pick_cover(&m, goal, k);
                    }
                });
            }
            let picker_row = gtk::Box::builder()
                .orientation(Orientation::Horizontal)
                .spacing(8)
                .build();
            let label = Label::new(Some(t::GOAL_SELECT));
            picker_row.append(&label);
            picker_row.append(&select);
            container.append(&picker_row);
        }
    }
}

/// One provider's dialog — the sign-in pop-up, painted under its row.
fn build_dialog(
    m: &Arc<OnboardingMachine>,
    id: &str,
    open: &Rc<RefCell<Option<String>>>,
    repaint_cell: &RepaintCell,
) -> Dialog {
    let root = group_box(8);
    root.set_margin_start(16);
    root.set_margin_top(4);
    root.set_margin_bottom(8);
    set_test_id(&root, ids::PROVIDER_DIALOG);

    let name = Label::new(Some(&provider_name(id)));
    name.add_css_class("heading");
    name.set_halign(gtk::Align::Start);
    root.append(&name);

    let meta = PROVIDERS.iter().find(|p| p.id.as_str() == id);
    if let Some(p) = meta {
        let link = gtk::LinkButton::with_label(p.signup_url, p.signup_url);
        set_test_id(&link, ids::PROVIDER_DIALOG_LINK);
        root.append(&link);

        let open_browser = Button::with_label(dns_t::OPEN_IN_BROWSER);
        set_test_id(&open_browser, ids::PROVIDER_DIALOG_OPEN_BROWSER_BUTTON);
        let url = p.signup_url.to_string();
        open_browser.connect_clicked(move |_| crate::url_opener::open(&url));
        root.append(&open_browser);

        let help = wrapped_label(None);
        help.set_text(&resolve(p.help_key));
        set_test_id(&help, ids::PROVIDER_DIALOG_HELP_TEXT);
        root.append(&help);
    }

    let notes = wrapped_label(Some("fauna-muted"));
    set_test_id(&notes, ids::DNS_REGISTRAR_NOTES_TEXT);
    notes.set_visible(false);
    root.append(&notes);

    let form = group_box(8);
    set_test_id(&form, ids::PROVIDER_CREDENTIALS_FORM);
    root.append(&form);

    let connect = Button::with_label(t::CONNECT);
    connect.add_css_class("suggested-action");
    set_test_id(&connect, ids::PROVIDER_CONNECT_BUTTON);
    {
        let m = m.clone();
        let id = id.to_string();
        connect.connect_clicked(move |btn| {
            // Dead while the checks run; the next tick repaints it.
            btn.set_sensitive(false);
            let m = m.clone();
            let id = id.clone();
            // A failed check is the row's own error, painted on the
            // dialog's status line by the next tick.
            crate::async_helper::run_on_tokio(async move { m.connect_provider(id).await }, |_| {});
        });
    }
    root.append(&connect);

    let disconnect = Button::with_label(t::DISCONNECT);
    set_test_id(&disconnect, ids::PROVIDER_DISCONNECT_BUTTON);
    disconnect.set_visible(false);
    {
        let m = m.clone();
        let id = id.to_string();
        disconnect.connect_clicked(move |_| m.disconnect_provider(id.clone()));
    }
    root.append(&disconnect);

    // The WHOIS contact form of a registrar that requires one — `dns_config`'s
    // form, its rules unchanged, in its new place (§ 3d).
    let (contact_form, contact_refresh) = super::dns_config::build_contact_form(
        {
            let m = m.clone();
            let id = id.to_string();
            Rc::new(move |c| m.set_provider_contact(id.clone(), c))
        },
        {
            let m = m.clone();
            let id = id.to_string();
            Rc::new(move || m.provider_shows_contact_form(id.clone()))
        },
        {
            let m = m.clone();
            let id = id.to_string();
            Rc::new(move || m.provider_contact(id.clone()))
        },
    );
    root.append(&contact_form);

    let status = wrapped_label(None);
    set_test_id(&status, ids::PROVIDER_DIALOG_STATUS_TEXT);
    root.append(&status);

    let close = Button::with_label(t::CLOSE);
    set_test_id(&close, ids::PROVIDER_DIALOG_CLOSE_BUTTON);
    {
        let open = open.clone();
        let repaint_cell = repaint_cell.clone();
        close.connect_clicked(move |_| {
            *open.borrow_mut() = None;
            repaint(&repaint_cell);
        });
    }
    root.append(&close);

    Dialog {
        id: id.to_string(),
        root,
        notes,
        form,
        field_ids: Vec::new(),
        hosted_auth: Vec::new(),
        connect,
        disconnect,
        contact_refresh,
        status,
    }
}

/// Per-tick repaint of the open dialog from the machine.
fn refresh_dialog(d: &mut Dialog, m: &Arc<OnboardingMachine>, row: &ProviderRow) {
    let target = CredTarget::Provider(d.id.clone());

    // The fields: rebuilt only when the set the machine shows changes, so
    // half-typed values survive every other tick.
    let fields = m.visible_provider_fields(d.id.clone());
    let field_ids: Vec<String> = fields.iter().map(|f| f.id.clone()).collect();
    if field_ids != d.field_ids {
        while let Some(child) = d.form.first_child() {
            d.form.remove(&child);
        }
        d.hosted_auth =
            generic_provider_form::populate_creds_into(&d.form, &fields, m.clone(), &target);
        d.field_ids = field_ids;
    }
    generic_provider_form::refresh_hosted_auth_buttons(&d.hosted_auth, m, &target);

    match m.provider_registrar_notes_key(d.id.clone()) {
        Some(key) => {
            d.notes.set_text(&resolve(&key));
            d.notes.set_visible(true);
        }
        None => d.notes.set_visible(false),
    }

    // The shared answer covers the connect in flight and the empty required
    // field; a connected provider shows Disconnect instead.
    if row.connected {
        d.connect.set_visible(false);
    } else {
        crate::possible::paint(
            &d.connect,
            None,
            m.connect_provider_possibility(d.id.clone()),
        );
    }
    d.disconnect.set_visible(row.connected);
    (d.contact_refresh)();

    let status = if row.connecting {
        t::DIALOG_CONNECTING.to_string()
    } else if let Some(e) = &row.error {
        e.clone()
    } else if row.connected {
        t::DIALOG_CONNECTED.to_string()
    } else {
        String::new()
    };
    d.status.set_text(&status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::observer::NullObserver;
    use fauna_onboarding_machine::{OnboardingStep, ProviderConnection};

    fn find(root: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
        if root.widget_name() == id {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            if let Some(hit) = find(&c, id) {
                return Some(hit);
            }
            child = c.next_sibling();
        }
        None
    }

    fn attr(w: &gtk::Widget, name: &str) -> Option<String> {
        let prefix = format!("test-attr-{name}-");
        w.css_classes()
            .iter()
            .find_map(|c| c.strip_prefix(&prefix).map(str::to_string))
    }

    fn page() -> (Arc<OnboardingMachine>, gtk::Widget, Rc<dyn Fn()>) {
        let m = OnboardingMachine::new(Arc::new(NullObserver));
        m.set_current_handle("alice@example.com".into());
        m.set_step_for_test(OnboardingStep::ProviderChoice);
        let (root, refresh) = build(m.clone());
        refresh();
        (m, root.upcast(), refresh)
    }

    /// At entry nothing is connected: the goal lines read open, every
    /// registry provider has a row plus the manual row, and the dead Continue
    /// says which goal is still open (rule 5).
    #[test]
    fn at_entry_the_goals_are_open_and_continue_says_why() {
        crate::testid::run_on_gtk_thread(|| {
            let (_m, root, _refresh) = page();
            for g in ["dns", "vps"] {
                let row = find(&root, &format!("provider-goal-row[{g}]")).expect(g);
                assert_eq!(attr(&row, "state").as_deref(), Some("open"));
            }
            assert!(
                find(&root, "provider-goal-row[domain]").is_none(),
                "registration is a goal only while a domain is bought"
            );
            assert!(find(&root, "provider-row[hetzner]").is_some());
            assert!(find(&root, "provider-row[manual]").is_some());
            let cont = find(&root, ids::PROVIDER_CHOICE_CONTINUE_BUTTON).unwrap();
            assert!(!cont.is_sensitive());
            let status = find(&root, ids::PROVIDER_CHOICE_STATUS_TEXT)
                .and_then(|w| w.downcast::<Label>().ok())
                .unwrap();
            assert!(!status.text().is_empty(), "a dead Continue owes a reason");
            assert!(
                find(&root, ids::PROVIDER_DIALOG).is_none(),
                "no dialog is open"
            );
        });
    }

    /// One connected provider that runs DNS for the domain and rents servers
    /// covers both goals, and Continue goes live.
    #[test]
    fn one_provider_covering_both_goals_makes_continue_live() {
        crate::testid::run_on_gtk_thread(|| {
            let (m, root, refresh) = page();
            m.set_provider_connection_for_test(
                "hetzner".into(),
                ProviderConnection {
                    connected: true,
                    server_checked: true,
                    zones: vec![fauna_provisioning::dns::DnsZone {
                        id: "z1".into(),
                        name: "example.com".into(),
                        nameservers: vec![],
                    }],
                    ..Default::default()
                },
            );
            refresh();
            for g in ["dns", "vps"] {
                let row = find(&root, &format!("provider-goal-row[{g}]")).expect(g);
                assert_eq!(attr(&row, "provider").as_deref(), Some("hetzner"), "{g}");
            }
            let hetzner = find(&root, "provider-row[hetzner]").unwrap();
            assert_eq!(attr(&hetzner, "state").as_deref(), Some("connected"));
            assert!(
                find(&root, ids::PROVIDER_CHOICE_CONTINUE_BUTTON)
                    .unwrap()
                    .is_sensitive()
            );
        });
    }

    /// A refused goal pick is never silent (rule 7): its reason is on the
    /// page's `error-message` at the next paint. Registration is no goal
    /// while no domain is bought, so picking a provider for it is refused.
    #[test]
    fn a_refused_goal_pick_reads_its_reason_from_error_message() {
        crate::testid::run_on_gtk_thread(|| {
            let (m, root, refresh) = page();
            pick_cover(&m, ProviderGoal::Domain, "hetzner");
            refresh();
            let reason = m.error_message().expect("the refusal is on the error line");
            assert!(!reason.is_empty());
            let label = find(&root, ids::ERROR_MESSAGE)
                .and_then(|w| w.downcast::<Label>().ok())
                .expect("error-message");
            assert!(label.is_visible(), "the refusal is painted");
            assert_eq!(label.text().as_str(), reason);
        });
    }

    /// Pressing a provider row opens its dialog under it, with Connect dead
    /// until the required fields are typed; Close shuts it.
    #[test]
    fn a_row_opens_its_dialog_and_close_shuts_it() {
        crate::testid::run_on_gtk_thread(|| {
            let (_m, root, _refresh) = page();
            find(&root, "provider-row[hetzner]")
                .and_then(|w| w.downcast::<Button>().ok())
                .unwrap()
                .emit_clicked();
            assert!(find(&root, ids::PROVIDER_DIALOG).is_some());
            let connect = find(&root, ids::PROVIDER_CONNECT_BUTTON).expect("connect");
            assert!(!connect.is_sensitive(), "nothing typed yet");
            assert!(
                find(&root, "provider-credentials-form-api-token").is_some(),
                "the dialog shows the provider's fields"
            );
            find(&root, ids::PROVIDER_DIALOG_CLOSE_BUTTON)
                .and_then(|w| w.downcast::<Button>().ok())
                .unwrap()
                .emit_clicked();
            assert!(find(&root, ids::PROVIDER_DIALOG).is_none());
        });
    }
}
