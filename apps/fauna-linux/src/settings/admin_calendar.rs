//! The flat **`admin-calendar`** page (linux; the calendar-enable seed's lead
//! app).
//!
//! Where a nest admin flips the deployment-wide **CalDAV-enable** toggle — the
//! sibling of `admin-mail`'s mail-enable toggle. Email and calendar are two
//! independently enableable features of one MDA bridge (`caldav-server.md`
//! § Independent enablement): the MDA runs iff `mail_enabled || caldav_enabled`,
//! so this toggle (or the Mail page's `admin-mail-enabled-toggle`) is what brings
//! the one bridge up/down — "enabled if either". Target behavior + the page §:
//! `docs/goal/behavior/admin.md` § 8 Calendar. UX/IDs: `tests/e2e-unified/ui.yaml`
//! `admin-calendar`.
//!
//! Like `admin_mail.rs` this layer holds **no** business logic — it is a dumb
//! renderer of [`CaldavPolicySnapshot`] + dispatcher of [`CaldavPolicyAction`];
//! the hydrate (`get_mail_config` → `caldav_enabled`) + the `set_caldav_enabled`
//! write live in the shared `fauna_client_mail_settings::caldav_policy` machine
//! (priority #2/#4), the prior art the other five apps lift over the
//! `build_caldav_policy_machine` UniFFI/wasm export.
//!
//! **No nest work** — `set_caldav_enabled` already exists + gates the MDA. Read +
//! write are both LIVE; a nest rejection surfaces via `CaldavPolicySnapshot::error`,
//! never faked green. The page is registered as an admin-shell `gtk::Stack`
//! sub-page (child `admin-calendar`), reached through the state protocol.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{CaldavPolicyAction, CaldavPolicyMachine, CaldavPolicySnapshot};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::admin::calendar_page as S;
use crate::testid::set_test_id;

/// Handles to the widgets the snapshot renders into. GTK objects are
/// reference-counted, so cloning this is cheap and shares widgets.
#[derive(Clone)]
struct CaldavWidgets {
    error_label: gtk::Label,
    enabled_toggle: gtk::Switch,
    /// The admin-set CalDAV listener port (`admin-calendar-caldav-port-input`).
    /// Holds the in-progress edit; re-seeded from `caldav_port` on every render.
    port_entry: gtk::Entry,
}

/// Per-page context threaded through hydrate / dispatch / render.
struct CaldavCtx {
    machine: Arc<CaldavPolicyMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates the toggle, so its
    /// `active-notify` handler doesn't echo the change back as an action.
    syncing: Cell<bool>,
    w: CaldavWidgets,
}

/// Add a `gtk::Entry` row (title + subtitle) to `group`, ID on the entry
/// (mirrors `admin_mail.rs::entry_row` — the `admin-mail-*-input` idiom).
fn entry_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: &str,
    test_id: &str,
) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .width_chars(12)
        .build();
    set_test_id(&entry, test_id);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(false)
        .build();
    row.add_suffix(&entry);
    group.add(&row);
    entry
}

/// Build the flat `admin-calendar` page.
pub fn build_admin_calendar_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("x-office-calendar-symbolic")
        .build();

    // --- Top group: heading + page-level error + the CalDAV-enable toggle.
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("admin-calendar-heading")));

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    // admin-calendar-enabled-toggle — caldav.enabled (set_caldav_enabled);
    // dispatches immediately on change (re-reads persisted state).
    let enabled_toggle = super::switch_row(
        &top_group,
        S::ENABLED_LABEL,
        S::ENABLED_SUBTITLE,
        "admin-calendar-enabled-toggle",
    );
    // tui's `admin::Action::ToggleCaldavEnabled` (`account-data-plane.md` §
    // The offline-mutation contract — admin/provisioning is OnlineOnly).
    crate::offline_gate::declare_wire_kind(&enabled_toggle, "fauna.bridges.set_caldav_enabled");

    // admin-calendar-caldav-port-input — the admin-set CalDAV listener port
    // (set_caldav_port). A text_input + save button (the admin-mail-*-input
    // pattern): the entry holds the in-progress edit, the save button validates a
    // u16 in [1, 65535] then dispatches. Governs only the router-less direct
    // listener (desktop / bare-IP / domainless box); inert on a domain deployment
    // where CalDAV serves at mail.<domain>:443 via the SNI router.
    let port_entry = entry_row(
        &top_group,
        S::CALDAV_PORT_LABEL,
        S::CALDAV_PORT_DESC,
        "admin-calendar-caldav-port-input",
    );
    let port_save = gtk::Button::builder()
        .label(S::CALDAV_PORT_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&port_save, ids::ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON);
    // tui's `admin::Action::SaveCaldavPort`.
    crate::offline_gate::declare_wire_kind(&port_save, "fauna.bridges.set_caldav_port");
    let port_save_row = adw::ActionRow::builder().activatable(false).build();
    port_save_row.add_suffix(&port_save);
    top_group.add(&port_save_row);

    page.add(&top_group);

    wire_machine(
        CaldavWidgets {
            error_label,
            enabled_toggle,
            port_entry,
        },
        port_save,
    );

    page
}

/// Connect the page to the shared `CaldavPolicyMachine`, hydrate on mount, and
/// wire the toggle + the port save button. No-op (page stays at static
/// placeholders) when no client is available — e.g. the unit test, which has no
/// registered client.
fn wire_machine(widgets: CaldavWidgets, port_save: gtk::Button) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };
    let machine = Arc::new(crate::mail_glue::build_caldav_policy_machine(&client));

    let ctx = Rc::new(CaldavCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // CalDAV-enable toggle → SetCaldavEnabled (skip the echo from render()).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .enabled_toggle
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    CaldavPolicyAction::SetCaldavEnabled {
                        enabled: sw.is_active(),
                    },
                );
            });
    }

    // CalDAV-port save → validate via fauna_core::format::parse_port (a u16 in
    // [1, 65535], invalid → error-message, no dispatch), else SetCaldavPort
    // (mirrors web/android).
    {
        let ctx = Rc::clone(&ctx);
        port_save.connect_clicked(move |_| {
            match fauna_core::format::parse_port(&ctx.w.port_entry.text()) {
                Some(port) => {
                    dispatch_action(&ctx, CaldavPolicyAction::SetCaldavPort { port });
                }
                None => {
                    // Surface the invalid-port message directly (no dispatch fires,
                    // so render() won't clear it; a later valid save re-renders).
                    super::render_error_label(&ctx.w.error_label, Some(S::CALDAV_PORT_INVALID));
                }
            }
        });
    }
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<CaldavCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Dispatch a fire-and-render action on the tokio runtime, then render the
/// resulting snapshot.
fn dispatch_action(ctx: &Rc<CaldavCtx>, action: CaldavPolicyAction) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Render a `CaldavPolicySnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<CaldavCtx>, snap: &CaldavPolicySnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // CalDAV-enable toggle — reflect persisted state without echoing a dispatch.
    if w.enabled_toggle.is_active() != snap.caldav_enabled {
        ctx.syncing.set(true);
        w.enabled_toggle.set_active(snap.caldav_enabled);
        ctx.syncing.set(false);
    }

    // CalDAV-port entry — re-seed from the persisted port (the entry doesn't
    // dispatch on change, so this is a plain text set; renders only fire on
    // hydrate / after a save, never mid-edit).
    let port_text = snap.caldav_port.to_string();
    if w.port_entry.text() != port_text {
        w.port_entry.set_text(&port_text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The admin-calendar page exposes every static ui.yaml ID for the page
    /// (the `admin-nav-back` button lives in the shared admin-shell header,
    /// not in this page's subtree, so it is not asserted here). Pins
    /// `switch_row`'s shared shape (round 188 of the shared-Rust harvest
    /// sweep) still renders the enable
    /// toggle with its ID intact.
    #[test]
    fn admin_calendar_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_admin_calendar_page();
            let names = widget_names(&page);
            for id in [
                "admin-calendar-heading",
                "error-message",
                "admin-calendar-enabled-toggle",
                "admin-calendar-caldav-port-input",
                ids::ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON,
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
