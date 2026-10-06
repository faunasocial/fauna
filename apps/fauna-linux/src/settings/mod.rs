pub mod account;
pub mod admin_bool_toggle_page;
pub mod admin_calendar;
pub mod admin_contacts;
pub mod admin_files;
pub mod admin_mail;
pub mod admin_web;
pub mod atproto;
pub mod connected_apps;
pub mod email_filters;
pub mod encryption;
pub mod general;
pub mod identity_export;
pub mod linked_nests;
pub mod logs;
pub mod mail;
pub mod mail_aliases;
pub mod mail_export;
pub mod mail_import;
pub mod mail_list_members;
pub mod mail_lists;
pub mod mail_spam;
pub mod member_review;
pub mod muted_words;
pub mod nostr_tab;
pub mod p2p_tab;
pub mod pending_actions;
pub mod privacy;
pub mod recovery_kit;
pub mod subscriptions;
pub mod task_delegation;
pub mod web;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::client::FaunaClient;
use crate::testid::{set_test_attr, set_test_id};

// ---------------------------------------------------------------------------
// Arm-then-confirm destructive buttons — shared by every settings page with a
// hard-delete/undo action (mail_export, mail_spam, mail_aliases, mail_lists
// each hand-copied this before the lift; round 75 of the shared-Rust harvest
// sweep).
// ---------------------------------------------------------------------------

/// Arm-then-confirm a destructive button: the first click arms — relabels the
/// button to `armed_label` (`common::CONFIRM_Q`, or the feature's own
/// descriptive confirm where one exists — apps/common.md § Two-click confirm)
/// and runs `on_arm` (a caller-supplied chrome hook,
/// e.g. a CSS class swap) — and starts a 4s auto-disarm timer that relabels
/// back to `base_label` and runs `on_disarm` if no second click comes before
/// it fires. A second click while armed calls `on_confirm`; when
/// `reset_before_confirm` is set, the button is relabelled and `on_disarm` run
/// *before* `on_confirm`, so the widget is back in its resting state should
/// the destructive action fail and the row survive (mail_export/mail_spam's
/// original behaviour — the two call sites that resolve a widget-owning
/// `alias_id_hex`/`list_id_hex` inline into the delete instead rely on the
/// row disappearing on success, matching their pre-lift behaviour).
///
/// Each timer disarms only the arm that started it: a confirm followed by a
/// fresh arm inside 4s must not be cut short by the first arm's pending timer.
///
/// Arm, confirm and disarm each log a `[two-click] <test id>` line. The 4s
/// window is wall clock, so a main loop stalled across it expires the arm
/// before the second click is dispatched, and that click re-arms instead of
/// confirming. These lines are the only witness of whether the destructive
/// action actually went out.
pub(super) fn wire_two_click(
    button: &gtk::Button,
    base_label: &'static str,
    armed_label: &'static str,
    reset_before_confirm: bool,
    on_arm: impl Fn(&gtk::Button) + 'static,
    on_disarm: impl Fn(&gtk::Button) + 'static,
    on_confirm: impl Fn() + 'static,
) {
    let armed = Rc::new(Cell::new(false));
    let generation = Rc::new(Cell::new(0u64));
    let on_disarm = Rc::new(on_disarm);
    button.connect_clicked(move |btn| {
        if !armed.get() {
            armed.set(true);
            let arm_generation = generation.get().wrapping_add(1);
            generation.set(arm_generation);
            let armed_at = std::time::Instant::now();
            tracing::info!("[two-click] {}: armed", btn.widget_name());
            btn.set_label(armed_label);
            on_arm(btn);
            let armed2 = Rc::clone(&armed);
            let generation2 = Rc::clone(&generation);
            let btn_weak = btn.downgrade();
            let on_disarm2 = Rc::clone(&on_disarm);
            glib::timeout_add_local_once(Duration::from_secs(4), move || {
                if generation2.get() != arm_generation || !armed2.get() {
                    return;
                }
                armed2.set(false);
                if let Some(b) = btn_weak.upgrade() {
                    tracing::info!(
                        "[two-click] {}: disarmed {:.1}s after arming",
                        b.widget_name(),
                        armed_at.elapsed().as_secs_f64()
                    );
                    b.set_label(base_label);
                    on_disarm2(&b);
                }
            });
        } else {
            tracing::info!("[two-click] {}: confirmed", btn.widget_name());
            if reset_before_confirm {
                armed.set(false);
                btn.set_label(base_label);
                on_disarm(btn);
            }
            on_confirm();
        }
    });
}

// ---------------------------------------------------------------------------
// Page-level error label — every settings page's `error-message` element
// (Rule 2) hand-copied the same show/clear pair (round 166 of the
// shared-Rust harvest sweep).
// ---------------------------------------------------------------------------

/// Render a page's `error-message` label: `Some(msg)` shows it, `None` clears
/// the text and hides it.
pub(super) fn render_error_label(label: &gtk::Label, error: Option<&str>) {
    match error {
        Some(msg) => {
            label.set_text(msg);
            label.set_visible(true);
        }
        None => {
            label.set_text("");
            label.set_visible(false);
        }
    }
}

// ---------------------------------------------------------------------------
// AT-SPI marker labels — a 1px-tall label carrying a test ID, for tree spots
// (adw rows, transparency-list rows) that aren't themselves AT-SPI-readable.
// Sixteen settings pages plus personalization/mod.rs and logs_view.rs
// hand-copied the same pair; `value_marker`'s ellipsize + max-width-chars(1)
// guard (so a long value like a hex hash can't widen an embedded page) was
// already the documented "canonical" version by cross-reference comment in
// three of the copies, never centralized (round 168 of the shared-Rust
// harvest sweep).
// ---------------------------------------------------------------------------

/// A 1px-tall marker label carrying a test ID (adw rows aren't AT-SPI-readable).
pub(super) fn marker(id: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_height_request(1);
    label.set_overflow(gtk::Overflow::Hidden);
    set_test_id(&label, id);
    label
}

/// A 1px marker label carrying `id` and holding `value` as readable text.
/// Width-less so a long value (e.g. a hex hash) can't force an embedded page
/// wide — `overflow(Hidden)` clips painting but not the size request;
/// ellipsize + max-width-chars(1) drops the width request to ~0 while
/// `label.text()` (what AT-SPI / e2e reads) still returns the full string.
pub(super) fn value_marker(id: &str, value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_use_markup(false);
    label.set_height_request(1);
    label.set_overflow(gtk::Overflow::Hidden);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    set_test_id(&label, id);
    label
}

/// A 1px marker label carrying `id`, left blank at construction — the wizard
/// pages set its text later, at render time, once the value it reports (a
/// progress summary, an error log) becomes known. `mail_export.rs` and
/// `mail_import.rs` hand-copied this exact shape (round 169 of the
/// shared-Rust harvest sweep).
pub(super) fn blank_value_marker(id: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_use_markup(false);
    label.set_height_request(1);
    label.set_overflow(gtk::Overflow::Hidden);
    set_test_id(&label, id);
    label
}

// ---------------------------------------------------------------------------
// Admin bool-toggle switch row — every admin page with an on/off setting
// (the `admin_bool_toggle_page!` macro's two expansions, admin_calendar,
// admin_mail's 20+ toggles) hand-copied the same four-line `gtk::Switch` +
// `adw::ActionRow` skeleton; the macro's own copy meant the shape was
// physically duplicated on every expansion, invisible to a source-level
// scanner (round 188 of the shared-Rust harvest sweep).
// ---------------------------------------------------------------------------

/// Add a `gtk::Switch` row (title + subtitle) to `group`, ID on the switch.
pub(super) fn switch_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: &str,
    test_id: &str,
) -> gtk::Switch {
    let sw = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    set_test_id(&sw, test_id);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(false)
        .build();
    row.add_suffix(&sw);
    group.add(&row);
    sw
}

// ---------------------------------------------------------------------------
// Bridge-settings commit — fire-and-forget `fauna.bridges.set_settings`,
// error-logged, never awaited by the caller. `views/bridges/detail.rs`'s
// single-key `commit_setting` and `nostr_tab.rs`'s relay-list save + inline
// multi-key save each hand-copied the same spawn/set_settings/log-on-err
// ceremony; `commit_setting`'s own doc comment already pointed at
// `nostr_tab.rs`'s matching comment, never centralized (round 196 of the
// shared-Rust harvest sweep).
// ---------------------------------------------------------------------------

/// Persist a `fauna.bridges.set_settings` patch for `bridge_id`. The nest
/// merges by key (each field its own `UPDATE ... WHERE` gated on `Some`), so
/// a partial map never clobbers a sibling setting — callers may send just the
/// keys that changed.
pub(super) fn commit_bridge_settings(
    client: &Rc<FaunaClient>,
    bridge_id: &str,
    settings: fauna_client::Value,
) {
    let bridges = fauna_client_bridges::BridgesClient::new(client.nest_rpc().clone());
    let handle = client.runtime_handle();
    let bridge_id = bridge_id.to_string();
    handle.spawn(async move {
        if let Err(e) = bridges.set_settings(bridge_id, settings).await {
            tracing::error!("[settings] bridge set_settings error: {e}");
        }
    });
}

// ---------------------------------------------------------------------------
// Share-status pane — an opt-in toggle plus a transparency list of published
// aggregates, hydrated from a `{share: bool, published: Vec<ReportShareEntry>}`
// reply. mail_spam's report-share pane and personalization/mod.rs's
// signal-share pane hand-copied the render + row-builder shape; the mirror was
// already named ("mirrors mail_spam's report-share toggle" / "mirrors web.rs's
// subdomain toggle") but never centralized (round 195 of the shared-Rust
// harvest sweep).
// ---------------------------------------------------------------------------

/// The widgets a share-status pane renders into — bundled so
/// `render_share_pane` stays under clippy's argument-count lint.
pub(super) struct SharePaneWidgets<'a> {
    pub toggle: &'a gtk::Switch,
    pub syncing: &'a Cell<bool>,
    pub error_label: &'a gtk::Label,
    pub published_group: &'a adw::PreferencesGroup,
    pub published_rows: &'a RefCell<Vec<adw::ActionRow>>,
    pub published_placeholder: &'a adw::ActionRow,
}

/// Render a share-status reply: reflect the opt-in toggle without echoing it
/// back (`syncing` suppresses the caller's own `active_notify` handler), carry
/// its on/off for the e2e driver via the `state` attr, and rebuild the
/// published-aggregates list (tear down, re-add) from the nest-confirmed
/// reply. An error surfaces via the page `error-message`.
pub(super) fn render_share_pane(
    w: SharePaneWidgets<'_>,
    testid_prefix: &str,
    count_label: &str,
    res: Result<
        (
            bool,
            Vec<fauna_client_moderation::moderation::ReportShareEntry>,
        ),
        String,
    >,
) {
    let (share, published) = match res {
        Ok(s) => s,
        Err(msg) => {
            render_error_label(w.error_label, Some(&msg));
            return;
        }
    };

    if w.toggle.is_active() != share {
        w.syncing.set(true);
        w.toggle.set_active(share);
        w.syncing.set(false);
    }
    set_test_attr(w.toggle, "state", if share { "on" } else { "off" });

    {
        let mut rows = w.published_rows.borrow_mut();
        for row in rows.drain(..) {
            w.published_group.remove(&row);
        }
        for entry in &published {
            let row = build_share_published_row(entry, testid_prefix, count_label);
            w.published_group.add(&row);
            rows.push(row);
        }
    }
    w.published_placeholder.set_visible(published.is_empty());
}

/// Build one `<prefix>-published-list-item` row from a published aggregate.
/// Pure transparency — no action; every field rides a 1px marker. The count
/// is always ≥ k (the nest publication gate), so `count_label` is always
/// plural.
pub(super) fn build_share_published_row(
    entry: &fauna_client_moderation::moderation::ReportShareEntry,
    testid_prefix: &str,
    count_label: &str,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(format!("{} {}", entry.count, count_label))
        .subtitle(&entry.factor)
        .build();
    row.add_prefix(&marker(&format!("{testid_prefix}-published-list-item")));
    row.add_suffix(&value_marker(
        &format!("{testid_prefix}-published-list-item-hash"),
        &entry.content_hash,
    ));
    row.add_suffix(&value_marker(
        &format!("{testid_prefix}-published-list-item-factor"),
        &entry.factor,
    ));
    row.add_suffix(&value_marker(
        &format!("{testid_prefix}-published-list-item-count"),
        &entry.count.to_string(),
    ));
    row
}

// ---------------------------------------------------------------------------
// Global settings client — a thread-local Weak<FaunaClient> so we don't
// keep the client alive artificially. Set once during startup from main.rs.
// All access happens on the GTK main thread, so thread_local! is correct.
// ---------------------------------------------------------------------------

type SignOutHandler = Rc<dyn Fn()>;
/// Re-onboard seed handed to the factory-reset handler: the post-reset
/// `(claim_code, nest_url, secret_hex, handle)`. The handler tears down the
/// session (keeping local creds) and re-seeds onboarding at claim-code with the
/// code pre-filled.
type FactoryResetHandler = Rc<dyn Fn(String, String, String, String)>;
/// Account-switch handler: given the target account's actor id (hex), make it
/// the active account and rebuild the authenticated session for it (a live
/// in-session re-auth — no relaunch). Registered once from main.rs with access
/// to the `adw::Application`; invoked by the account-switcher rows.
type SwitchAccountHandler = Rc<dyn Fn(String, bool)>;
/// Post-auth terminal-verdict handler (`security.md` § Post-auth surfacing):
/// tear the authenticated session down and re-enter the launch flow, which
/// re-runs the challenge and lands on the same blocking surface the launch path
/// renders for that verdict. Registered once from main.rs with access to the
/// `adw::Application`, like the three above.
///
/// Deliberately **verdict-agnostic** — it serves both escalating verdicts
/// (nest identity changed, identity superseded) because the teardown is
/// identical and the launch flow re-derives which surface to render. The
/// verdict travels as a log label only; a handler per verdict would be two
/// copies of one teardown drifting apart.
type LaunchEscalationHandler = Rc<dyn Fn()>;
/// Repaint hook fired after every `fauna.bridges.list` reply lands in the
/// app-wide bridges snapshot. Registered by a settings page that embeds a
/// bridge surface (today: the AT Protocol page's Linked-account panel) so a
/// link/unlink completed elsewhere repaints it without a re-navigation.
type BridgesChangedHandler = Rc<dyn Fn()>;
/// Repaint hook fired when a `fauna.bridges.list_follows` reply lands for a
/// bridge a settings page embeds (today: the AT Protocol page's Linked-account
/// panel — `bridges::mod.rs`'s Bridges-page detail pane repaints directly via
/// its own tracked handle, `app.rs`'s `bridges_open_detail`, and needs no
/// hook). The handler decides for itself whether `bridge_id` is one it cares
/// about, mirroring `InboxModeLoadedHandler`'s shape.
type BridgeFollowsLoadedHandler =
    Rc<dyn Fn(&str, &[fauna_client_bridges::bridges_ui::BridgeFollow])>;
/// Nudge hook fired when a `fauna.atproto.consent_requested` push arrives
/// (F4 rung 2 — an external ATProto app asking to sign in). Registered by the
/// AT Protocol page so a live consent card appears without the user having to
/// leave and re-enter the page; the push carries no payload this side reads
/// — it is purely a "go re-list" signal, so a push and a stale poll can never
/// disagree (mirrors tui's `list_pending_consents`-is-the-one-source stance).
type AtprotoRehydrateHandler = Rc<dyn Fn()>;
/// Repaint hook fired when the account's real inbox mode lands from
/// `fauna.inbox.mode.get`. Registered by the Privacy page so the radio group
/// marks the stored mode instead of a guess; the argument is the wire value
/// (`"open"`, `"allow_knock"`, `"contacts_only"`, `"closed"`).
type InboxModeLoadedHandler = Rc<dyn Fn(&str)>;

thread_local! {
    static SETTINGS_CLIENT: RefCell<Option<Weak<FaunaClient>>> = const { RefCell::new(None) };
    /// The app-wide `fauna.bridges.list` snapshot (`AppWidgets::bridges_snapshot`),
    /// shared by reference — never a second copy. Kept fresh by the
    /// `BridgesLoaded` handler, which re-fetches on every link/unlink.
    static SETTINGS_BRIDGES: RefCell<Option<crate::views::bridges::BridgesSnapshot>> =
        const { RefCell::new(None) };
    static BRIDGES_CHANGED_HANDLER: RefCell<Option<BridgesChangedHandler>> =
        const { RefCell::new(None) };
    static BRIDGE_FOLLOWS_LOADED_HANDLER: RefCell<Option<BridgeFollowsLoadedHandler>> =
        const { RefCell::new(None) };
    static ATPROTO_REHYDRATE_HANDLER: RefCell<Option<AtprotoRehydrateHandler>> =
        const { RefCell::new(None) };
    static CONNECTED_APPS_REHYDRATE_HANDLER: RefCell<Option<AtprotoRehydrateHandler>> =
        const { RefCell::new(None) };
    /// The session's mail-settings machine, published by the Mail & Calendar
    /// page when it builds it. Its app passwords are rows of the Connected apps
    /// roster, which reads them through this same machine.
    static SETTINGS_MAIL_MACHINE: RefCell<Option<std::sync::Arc<fauna_client_mail_settings::MailSettingsMachine>>> =
        const { RefCell::new(None) };
    static SETTINGS_HANDLE: RefCell<Option<String>> = const { RefCell::new(None) };
    static SETTINGS_P2P: RefCell<Option<std::sync::Arc<crate::p2p::P2pService>>> = const { RefCell::new(None) };
    /// The account's inbox mode as this process currently knows it. `None` =
    /// **not yet known** — no fetch has landed and the user has not chosen one.
    /// Deliberately not defaulted to `"open"`: a guess here is indistinguishable
    /// from a fact everywhere downstream, and "open" is the most permissive of
    /// the four, so guessing it is the worst direction to be wrong in.
    static INBOX_MODE: RefCell<Option<String>> = const { RefCell::new(None) };
    static INBOX_MODE_LOADED_HANDLER: RefCell<Option<InboxModeLoadedHandler>> =
        const { RefCell::new(None) };
    static SIGN_OUT_HANDLER: RefCell<Option<SignOutHandler>> = const { RefCell::new(None) };
    static FACTORY_RESET_HANDLER: RefCell<Option<FactoryResetHandler>> = const { RefCell::new(None) };
    static SWITCH_ACCOUNT_HANDLER: RefCell<Option<SwitchAccountHandler>> = const { RefCell::new(None) };
    static LAUNCH_ESCALATION_HANDLER: RefCell<Option<LaunchEscalationHandler>> =
        const { RefCell::new(None) };
    /// Shutdown flag for the authenticated UI message pump. Sign-out flips
    /// this so the 50ms `glib::timeout_add_local` returns `ControlFlow::Break`
    /// and drops its strong refs to the FaunaClient.
    static PUMP_SHUTDOWN: RefCell<Option<Rc<std::cell::Cell<bool>>>> = const { RefCell::new(None) };
}

/// Register the `FaunaClient` with the settings subsystem.
/// Call this once after creating the client in main.rs.
pub fn set_client(client: &Rc<FaunaClient>) {
    SETTINGS_CLIENT.with(|cell| {
        *cell.borrow_mut() = Some(Rc::downgrade(client));
    });
}

/// Retrieve a live `Rc<FaunaClient>` if one has been registered and not yet
/// dropped. Returns `None` if the client was never set or has been dropped.
pub fn get_client() -> Option<Rc<FaunaClient>> {
    SETTINGS_CLIENT.with(|cell| cell.borrow().as_ref().and_then(|w| w.upgrade()))
}

// ---------------------------------------------------------------------------
// Global bridges snapshot — the same `Rc` the Bridges view holds, registered
// once from app.rs. A settings page that embeds a bridge surface reads the
// provider's `BridgeStatus` from here instead of issuing its own
// `fauna.bridges.list`: one fetch, one snapshot, and the link/unlink re-fetch
// (`ActionResult` "bridge_linked"/"bridge_unlinked" → `fetch_bridges`) keeps
// every consumer live. Today's consumer is the AT Protocol page's Linked-account
// panel (docs/goal/ui/atproto.md § Layout & flow).
// ---------------------------------------------------------------------------

/// Register the app-wide bridges snapshot with the settings subsystem.
pub fn set_bridges_snapshot(snapshot: &crate::views::bridges::BridgesSnapshot) {
    SETTINGS_BRIDGES.with(|cell| {
        *cell.borrow_mut() = Some(Rc::clone(snapshot));
    });
}

/// The last-known `BridgeStatus` for `bridge_id`, if a `fauna.bridges.list`
/// reply has landed and carried that provider. `None` covers both "not fetched
/// yet" and "this nest has no such provider" — a caller renders the unlinked
/// surface in either case.
pub fn get_bridge_status(
    bridge_id: &str,
) -> Option<fauna_client_bridges::bridges_ui::BridgeStatus> {
    SETTINGS_BRIDGES.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|snap| snap.borrow().get(bridge_id).cloned())
    })
}

/// Register the repaint hook fired after each bridges snapshot update.
pub fn set_bridges_changed_handler(handler: BridgesChangedHandler) {
    BRIDGES_CHANGED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Fire the repaint hook, if one is registered. Called by the `BridgesLoaded`
/// handler in app.rs *after* it has refreshed the snapshot.
pub fn notify_bridges_changed() {
    let handler = BRIDGES_CHANGED_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler();
    }
}

/// Register the repaint hook fired when a `fauna.bridges.list_follows` reply
/// lands.
pub fn set_bridge_follows_loaded_handler(handler: BridgeFollowsLoadedHandler) {
    BRIDGE_FOLLOWS_LOADED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Fire the follows-loaded repaint hook, if one is registered. Called by the
/// `BridgeFollowsLoaded` handler in app.rs for every reply, regardless of
/// `bridge_id` — the handler itself decides whether it applies.
pub fn notify_bridge_follows_loaded(
    bridge_id: &str,
    follows: &[fauna_client_bridges::bridges_ui::BridgeFollow],
) {
    let handler = BRIDGE_FOLLOWS_LOADED_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(bridge_id, follows);
    }
}

/// Register the nudge hook fired on `fauna.atproto.consent_requested`.
pub fn set_atproto_rehydrate_handler(handler: AtprotoRehydrateHandler) {
    ATPROTO_REHYDRATE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Register the Connected apps page's nudge hook, fired with the AT Protocol
/// one on `fauna.atproto.consent_requested` — the consent card lives there now.
pub fn set_connected_apps_rehydrate_handler(handler: AtprotoRehydrateHandler) {
    CONNECTED_APPS_REHYDRATE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Fire the nudge hooks, if registered. Called by the
/// `AtprotoConsentRequested` push handler in app.rs.
pub fn notify_atproto_rehydrate() {
    let handler = ATPROTO_REHYDRATE_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler();
    }
    let handler = CONNECTED_APPS_REHYDRATE_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler();
    }
}

/// Publish the session's mail-settings machine (the Mail & Calendar page).
pub(crate) fn set_mail_machine(
    machine: &std::sync::Arc<fauna_client_mail_settings::MailSettingsMachine>,
) {
    SETTINGS_MAIL_MACHINE.with(|cell| {
        *cell.borrow_mut() = Some(std::sync::Arc::clone(machine));
    });
}

/// The session's mail-settings machine, once the Mail & Calendar page has built
/// it — `None` before that, or when its build failed.
pub(crate) fn mail_machine()
-> Option<std::sync::Arc<fauna_client_mail_settings::MailSettingsMachine>> {
    SETTINGS_MAIL_MACHINE.with(|cell| cell.borrow().clone())
}

/// Store the authenticated user's handle (result of GET /api/v1/account).
pub fn set_handle(handle: Option<String>) {
    SETTINGS_HANDLE.with(|cell| {
        *cell.borrow_mut() = handle;
    });
}

/// Retrieve the last-known handle, if any.
pub fn get_handle() -> Option<String> {
    SETTINGS_HANDLE.with(|cell| cell.borrow().clone())
}

// ---------------------------------------------------------------------------
// (The global MLS-manager thread-local `SETTINGS_MLS` + its `set_mls`/`get_mls`
// accessors were removed: the encryption settings page no longer mints key
// packages itself — its "refresh keys" button now drives the durable
// `conversations::conv_backend::replenish_key_packages` surface, and the app
// state already owns the `MlsManager` for the engine handoff, so no settings-side
// copy is needed.)

// ---------------------------------------------------------------------------
// Global P2P service — set on startup, read by P2P settings page
// ---------------------------------------------------------------------------

/// Store the P2P service so the P2P settings page can manage the tunnel.
pub fn set_p2p(p2p: std::sync::Arc<crate::p2p::P2pService>) {
    SETTINGS_P2P.with(|cell| {
        *cell.borrow_mut() = Some(p2p);
    });
}

/// Retrieve the P2P service, if one has been initialised.
pub fn get_p2p() -> Option<std::sync::Arc<crate::p2p::P2pService>> {
    SETTINGS_P2P.with(|cell| cell.borrow().clone())
}

// (The live sync-engine command channel is gone with the in-process
// `SyncDriver` — folder-binding edits route to the external `fauna-sync-agent`
// via `crate::sync_agent` now.)

// ---------------------------------------------------------------------------
// Inbox mode tracking — exposed in the test agent state JSON so E2E tests
// can read the currently selected mode without AT-SPI state introspection.
// ---------------------------------------------------------------------------

/// Store the inbox mode the **user just chose** (e.g. "open", "contacts_only").
pub fn set_inbox_mode(mode: &str) {
    INBOX_MODE.with(|cell| {
        *cell.borrow_mut() = Some(mode.to_string());
    });
}

/// Retrieve the currently known inbox mode, or `""` when it is not known yet.
///
/// The empty string is the same "unknown" spelling tui's state JSON uses
/// (`settings/mod.rs`'s `unwrap_or_default()`), so the shared e2e action layer
/// reads one contract on both: a value means the app really learned the mode,
/// never that it painted a default.
pub fn get_inbox_mode() -> String {
    INBOX_MODE.with(|cell| cell.borrow().clone().unwrap_or_default())
}

/// Register the Privacy page's repaint hook for a landed inbox mode.
pub fn set_inbox_mode_loaded_handler(handler: InboxModeLoadedHandler) {
    INBOX_MODE_LOADED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Record the account's real inbox mode and repaint the Privacy page.
///
/// The single entry point for a **nest-sourced** mode, called by the
/// `InboxModeLoaded` handler in `app.rs`. Before this existed the reply was
/// logged and dropped while the radio group painted a hard-coded "open", so
/// Settings → Privacy reported the same answer for every account whatever the
/// nest actually held. Storing without repainting
/// would only move the lie into the test agent's state JSON, so the store and
/// the repaint are deliberately one call.
pub fn apply_loaded_inbox_mode(mode: &str) {
    set_inbox_mode(mode);
    let handler = INBOX_MODE_LOADED_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(mode);
    }
}

// ---------------------------------------------------------------------------
// Recovery kit — repaint hooks for the Settings/Account section
// (`settings.md` § Recovery kit). Registered by `settings::recovery_kit` when
// it builds the section; invoked from the `RecoveryStatusLoaded` /
// `RecoveryKitMinted` handlers in `app.rs`. No cache is kept here (unlike
// inbox mode): the section's own view state lives in the page closure, and a
// status read is never trusted stale, so there is nothing to re-derive from
// on a page re-open — the page re-fetches instead.
// ---------------------------------------------------------------------------

type RecoveryStatusHandler = Rc<dyn Fn(Result<fauna_client_recovery::RecoveryKitStatus, String>)>;
type RecoveryKitMintedHandler =
    Rc<dyn Fn(Result<(String, fauna_client_recovery::RecoveryKitStatus), String>)>;
/// A kit-in-hand repair (veto, escrow re-seal) came back: the fresh status, or
/// the already-localized failure sentence. Nothing is minted, so there is no
/// display trio — the re-read status is the whole receipt.
type RecoveryRepairedHandler = Rc<dyn Fn(Result<fauna_client_recovery::RecoveryKitStatus, String>)>;
/// A succession that did NOT carry through: the sentence to show, and the busy
/// flag to clear. There is deliberately no success arm — a landed succession
/// tears this whole section down a moment later (the account switch), so the
/// only thing a repaint hook could usefully do with one is nothing.
type RecoveryStolenFailedHandler = Rc<dyn Fn(String)>;
/// A press of `recovery-kit-sweep-retry-button` came back: `Some(sentence)` on
/// the three arms whose answer IS the whole gesture, `None` when the sweep ran
/// and the freshly parked report is the answer.
type SweepRetriedHandler = Rc<dyn Fn(Option<String>)>;
/// A leg of the post-succession aftermath reported. No payload: the report is
/// already folded into the section's session cell (`recovery_kit::fold_aftermath`)
/// by the time the hook runs, and the hook repaints from that.
type AftermathHandler = Rc<dyn Fn()>;
/// The aftermath's mail burn settled. No payload: the Mail page re-reads the
/// account's mail custody through its own machine (`mail::wire_machine`).
type MailBurnSettledHandler = Rc<dyn Fn()>;

thread_local! {
    static AFTERMATH_HANDLER: RefCell<Option<AftermathHandler>> = const { RefCell::new(None) };
    static MAIL_BURN_SETTLED_HANDLER: RefCell<Option<MailBurnSettledHandler>> =
        const { RefCell::new(None) };
    static RECOVERY_STATUS_HANDLER: RefCell<Option<RecoveryStatusHandler>> =
        const { RefCell::new(None) };
    static RECOVERY_KIT_MINTED_HANDLER: RefCell<Option<RecoveryKitMintedHandler>> =
        const { RefCell::new(None) };
    static RECOVERY_REPAIRED_HANDLER: RefCell<Option<RecoveryRepairedHandler>> =
        const { RefCell::new(None) };
    static RECOVERY_STOLEN_FAILED_HANDLER: RefCell<Option<RecoveryStolenFailedHandler>> =
        const { RefCell::new(None) };
    static SWEEP_RETRIED_HANDLER: RefCell<Option<SweepRetriedHandler>> =
        const { RefCell::new(None) };
    /// A stolen-identity ceremony's persist-failure message — the one message
    /// in this section that embeds the successor's identity secret, the only
    /// copy of the key the account now belongs to
    /// (`apply_recovery_succeeded`'s persist-failure arm; `settings.md` §
    /// Recovery kit; ). Stored independent of
    /// `RECOVERY_STOLEN_FAILED_HANDLER`'s registration lifecycle for the same
    /// reason [`INBOX_MODE`] is stored independent of its own handler: a
    /// message that lands with no section built yet, or one that is currently
    /// off-screen, must still be readable once a section is on screen to show
    /// it — there is no "ask the nest again" for a seed that lives nowhere
    /// else. See [`park_stolen_failed_message`] / [`show_stolen_failed_message`].
    ///
    /// ⚠ **Deliberately NOT cleared once shown** (fixed ,
    /// `settings.md` § Recovery kit → *The persist-failure message survives
    /// the page*): it stays the source of truth on the Account page's shared
    /// `error-message` label until [`acknowledge_stolen_failed_message`]
    /// discharges it, so no other writer of that label — a click handler or
    /// an async repaint hook reacting to something unrelated — can silently
    /// wipe the only surviving copy of the successor's key. Every such writer
    /// MUST go through [`render_account_error_label`], never
    /// `render_error_label`/`Label::set_visible` directly.
    static PENDING_STOLEN_FAILED_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Register the Recovery kit section's repaint hook for a landed status read.
pub fn set_recovery_status_handler(handler: RecoveryStatusHandler) {
    RECOVERY_STATUS_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Register the Recovery kit section's repaint hook for a landed ceremony.
pub fn set_recovery_kit_minted_handler(handler: RecoveryKitMintedHandler) {
    RECOVERY_KIT_MINTED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Register the Recovery kit section's repaint hook for a landed kit-in-hand
/// repair (veto, escrow re-seal).
pub fn set_recovery_repaired_handler(handler: RecoveryRepairedHandler) {
    RECOVERY_REPAIRED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Repaint the Recovery kit section after a veto or an escrow re-seal.
pub fn apply_recovery_repaired(result: Result<fauna_client_recovery::RecoveryKitStatus, String>) {
    let handler = RECOVERY_REPAIRED_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(result);
    }
}

/// Repaint the Recovery kit section's status line + button enablement.
pub fn apply_recovery_status(result: Result<fauna_client_recovery::RecoveryKitStatus, String>) {
    let handler = RECOVERY_STATUS_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(result);
    }
}

/// Repaint the Recovery kit section with a just-minted kit (or the ceremony's
/// failure) — the display trio, the fresh status, and busy clearing.
pub fn apply_recovery_kit_minted(
    result: Result<(String, fauna_client_recovery::RecoveryKitStatus), String>,
) {
    let handler = RECOVERY_KIT_MINTED_HANDLER.with(|cell| cell.borrow().clone());
    // **"Minted" and "shown" are different events, and only the second one
    // discharges a succession's owed kit** (`identity-succession.md` § The
    // RecoveryKey → *At succession*; apple's `rearmUnshownKit`, iOS 2026-08-26).
    // A failed mint is the common case — the ceremony revokes every session of
    // the account inside the nest's own transaction, so the successor's first
    // mint races its own reconnect — and a mint that lands with no section
    // registered puts a kit on the nest and in nobody's hands. Both re-arm.
    // Read before the handler consumes `result`.
    let shown = handler.is_some() && result.is_ok();
    if let Some(handler) = handler {
        handler(result);
    }
    recovery_kit::settle_kit_discharge(shown);
}

/// Register the Recovery kit section's hook for a succession that did not carry
/// through.
pub fn set_recovery_stolen_failed_handler(handler: RecoveryStolenFailedHandler) {
    RECOVERY_STOLEN_FAILED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
    // A message may have landed before this section was (re)built — the same
    // "paint from stored state on construction" duty `get_inbox_mode()` names
    // at its own call site.
    show_stolen_failed_message();
}

/// Record a stolen-identity ceremony's persist-failure message
/// ([`PENDING_STOLEN_FAILED_MESSAGE`]) and try to show it right away.
///
/// Deliberately not routed through `say` (`apply_recovery_succeeded`'s local
/// closure): `say`'s no-handler arm logs its whole argument via
/// `tracing::error!`, and this is the one caller whose argument carries the
/// successor's identity secret — logging it would break `fauna-log`'s
/// crate-level redaction rule (`libs/fauna-log/src/lib.rs` — "NEVER log …
/// secret material"). The caller logs its own fixed, seed-free line instead,
/// unconditionally, and hands the seed-bearing message here.
fn park_stolen_failed_message(message: String) {
    PENDING_STOLEN_FAILED_MESSAGE.with(|cell| {
        *cell.borrow_mut() = Some(message);
    });
    show_stolen_failed_message();
}

/// Paint the parked persist-failure message on the currently registered
/// section, if any. A no-op — leaving the message parked for the next call —
/// when no section is registered right now, or nothing is pending.
///
/// Called from three places, covering the three times a section could become
/// able to show it: immediately after [`park_stolen_failed_message`] parks
/// one (the common case — the user is watching), from
/// `set_recovery_stolen_failed_handler` (a section built or rebuilt after the
/// fold ran), and from `recovery_kit::build_recovery_kit_group`'s `refresh`
/// closure (the user navigates back to a section that was off-screen when
/// the fold ran). This is the state-then-paint split tui's twin uses —
/// `apps/fauna-tui/src/settings/mod.rs:11592-11602` parks the same message in
/// `app.errors`, page-keyed state a render reads, rather than pushing it into
/// a widget reference captured at fold time.
///
/// ⚠ Deliberately does NOT clear [`PENDING_STOLEN_FAILED_MESSAGE`] — see that
/// static's doc comment. Only [`acknowledge_stolen_failed_message`] does.
pub fn show_stolen_failed_message() {
    let message = PENDING_STOLEN_FAILED_MESSAGE.with(|cell| cell.borrow().clone());
    let Some(message) = message else {
        return;
    };
    let handler = RECOVERY_STOLEN_FAILED_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(message);
    }
}

/// Whether a stolen-ceremony persist-failure message is currently parked and
/// therefore still owed its screen time on the Account page's shared
/// `error-message` label (`settings.md` § Recovery kit → *The persist-failure
/// message survives the page*; ).
fn has_pending_stolen_failed_message() -> bool {
    PENDING_STOLEN_FAILED_MESSAGE.with(|cell| cell.borrow().is_some())
}

/// Render the Account page's shared `error-message` label — UNLESS a
/// stolen-ceremony persist-failure message is still pending acknowledgment on
/// it, in which case this call is dropped rather than clobbering it. That
/// message is the ONLY surviving copy of a succession's new key, and it wins
/// over any other content on the label until the user leaves the Account
/// page (`acknowledge_stolen_failed_message`).
///
/// Every writer of that shared label — every button click handler and every
/// async repaint hook in `settings::account` / `settings::recovery_kit` —
/// MUST call this instead of `render_error_label` / `Label::set_visible`
/// directly. The one exception is the write that shows the pending message
/// itself, inside `set_recovery_stolen_failed_handler`'s registered closure:
/// it calls `render_error_label` unguarded, because at that instant the
/// message IS pending and this guard would block its own display.
pub(super) fn render_account_error_label(label: &gtk::Label, error: Option<&str>) {
    if has_pending_stolen_failed_message() {
        return;
    }
    render_error_label(label, error);
}

/// Discharge a still-pending persist-failure message: the user has navigated
/// away from the Account sub-page, having had the whole visit to read or copy
/// it (`settings.md` § Recovery kit → *The persist-failure message survives
/// the page* — the one acknowledgment gesture this fix defines, chosen
/// because it needs no new `ui.yaml` element). A no-op when nothing is
/// pending. Called from the settings shell's nav-edge-away-from-`account`
/// hook (`views/settings_shell.rs`).
pub fn acknowledge_stolen_failed_message() {
    PENDING_STOLEN_FAILED_MESSAGE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Register the Recovery kit section's repaint hook for an aftermath report.
pub(crate) fn set_aftermath_handler(handler: AftermathHandler) {
    AFTERMATH_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Register the Mail page's re-read hook for a settled mail burn — the burn
/// marks rows on the account's mail custody that the page's machine hydrated
/// before it ran, and the settings sub-stack is built once at launch, so
/// without the hook an open page paints the pre-burn rows until it is shown
/// again (tui's `refold_mail_snapshot`, web's re-hydrate on
/// `configStageSettled`).
pub(crate) fn set_mail_burn_settled_handler(handler: MailBurnSettledHandler) {
    MAIL_BURN_SETTLED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Fold one aftermath report (`crate::succession_aftermath`) and repaint the
/// Recovery kit section. The fold happens whether or not a section is built —
/// a section built later seeds from it — so a report can never be dropped for
/// arriving at the wrong moment.
pub(crate) fn apply_aftermath_progress(update: recovery_kit::AftermathUpdate) {
    let mail_burn_settled = matches!(
        update,
        recovery_kit::AftermathUpdate::MailBurn(fauna_core::progress::Passage::Settled(_))
    );
    recovery_kit::fold_aftermath(update);
    let handler = AFTERMATH_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler();
    }
    if mail_burn_settled {
        let handler = MAIL_BURN_SETTLED_HANDLER.with(|cell| cell.borrow().clone());
        if let Some(handler) = handler {
            handler();
        }
    }
}

/// Register the Recovery kit section's hook for a returned sweep retry.
pub fn set_sweep_retried_handler(handler: SweepRetriedHandler) {
    SWEEP_RETRIED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Land a succession: park its sweep, verify the successor seed is really on
/// this device, and adopt the successor through the app's own account switch.
///
/// ⚠ **The load-bearing half is HERE, not in the section's repaint hook.** The
/// ceremony can only be started from the section, so the hook is registered in
/// practice — but a landed succession has already re-pointed the account on the
/// nest, and routing the adoption through a hook would make "the app switched to
/// the identity it now belongs to" conditional on a repaint closure being
/// installed. The hook is used only for the arms that have something to SAY.
pub fn apply_recovery_succeeded(outcome: &fauna_client_recovery::ceremony::StolenOutcome) {
    let say = |message: String| {
        let handler = RECOVERY_STOLEN_FAILED_HANDLER.with(|cell| cell.borrow().clone());
        match handler {
            Some(handler) => handler(message),
            // A refusal the user must see cannot be allowed to vanish
            // (convention 11): loud in the log rather than silent.
            None => tracing::error!(
                "[settings/recovery] the succession reported \"{message}\" with no section \
                 registered to show it"
            ),
        }
    };
    let landed = match outcome {
        fauna_client_recovery::ceremony::StolenOutcome::Landed(landed) => landed,
        unlanded => return dispatch_unlanded(unlanded, say),
    };

    // Park the sweep BEFORE the switch: the switch destroys every window and
    // rebuilds this section as the successor, and the sweep's lines render
    // afterwards (`settings.md` § Recovery kit → *The sweep's own lines*).
    recovery_kit::park_succession_sweep(landed.sweep.clone());

    let new_actor_hex = landed.new_actor_id.to_hex();

    // The ceremony's LAST step, owed to the successor and performable only by
    // it: the mint authenticates as the new identity, which does not exist as a
    // session until the switch below completes (`identity-succession.md` § The
    // RecoveryKey → *At succession*). Recorded here for the same reason the
    // sweep is one line up — this is the last moment before a teardown that
    // wipes everything actor-scoped, and the cell is declared to survive it.
    //
    // **Before the persist check below, deliberately.** The succession has
    // already landed on the nest either way, so the account is kitless and
    // escrowless from this instant; a fold that reached the failure arm without
    // recording the debt would leave nothing anywhere remembering it is owed,
    // and a user who recovers by importing the secret from that error would come
    // back up with no kit and no record that one is due.
    recovery_kit::owe_succession_kit(new_actor_hex.clone());

    // The persist READ-BACK, not the `add_account` return code: `SecretStore::set`
    // is infallible by signature, so a clean return proves the call was made and
    // never that anything landed. If the seed is not here, the switch below would
    // find no secret and bail into a log line — and the user would be left signed
    // in as an identity the account no longer belongs to, with the only copy of
    // the new key gone. So it goes on the screen instead, which is the only way
    // back.
    //
    // The boolean is computed HERE, against the one real `account_registry()`
    // this crate's census test permits, and handed to `dispatch_persist_outcome`
    // below — which carries the actual branch and is therefore the pinnable
    // seam, since `apply_recovery_succeeded` itself has no test caller
    // (`account_registry()` cannot be minted in a unit test).
    let persisted = crate::account_registry().secrets(&new_actor_hex).is_some();
    dispatch_persist_outcome(persisted, landed.successor_secret_hex.as_str(), || {
        // `confirmed = false`, the same value tui's `adopt_successor` passes: the bit
        // reports that the in-app Stage-2 re-auth prompt *just succeeded*, and it
        // did not — nothing about a RecoveryKey ceremony is that prompt. Plain
        // `set_active` refuses only a **flagged** account, and this row was minted
        // seconds ago by `add_account` with no flag on it, so the honest value is
        // also the safe one; passing `true` would quietly hand every future
        // successor a re-auth bypass to keep this one path from being refused.
        trigger_switch_account(new_actor_hex, false);
    });
}

/// `apply_recovery_succeeded`'s persist-check dispatch, pulled out so it is
/// pinnable without a real `account_registry()` — `supervision_snapshot.rs`'s
/// census test forbids a second unit-testable call site that mints one, so
/// this takes the already-computed `persisted` bool rather than reading the
/// registry itself. `switch` runs on a successful persist; a parked,
/// seed-bearing message runs otherwise — NEVER `say`, which logs its whole
/// argument and would put the successor's identity secret into `fauna-log`
/// (; `settings.md` § Recovery kit). 's pin
/// (`recovery_stolen_failed_message_tests::a_persist_failure_parks_the_message_never_the_switch`
/// / `::a_persist_success_switches_and_never_parks`) reds if this routing
/// ever reverts.
fn dispatch_persist_outcome(persisted: bool, secret_hex: &str, switch: impl FnOnce()) {
    if persisted {
        switch();
        return;
    }
    // NOT `say`: its no-handler arm logs its whole argument, and this
    // message carries the successor's identity secret. Log a fixed,
    // seed-free line unconditionally, and park the seed-bearing message
    // as state `show_stolen_failed_message` reads — never a direct write
    // into whichever widget happens to be registered at this instant
    // (; see `park_stolen_failed_message`'s doc comment).
    tracing::error!(
        "[settings/recovery] the succession landed but the successor secret \
         failed to persist locally"
    );
    park_stolen_failed_message(
        crate::i18n::strings::settings::recovery_kit::stolen_persist_failed(secret_hex),
    );
}

/// Every arm of the ceremony but `Landed`: the shared sentence, painted verbatim
/// and wrapped in nothing (`settings.md` § Recovery kit → *The ceremony's outcome
/// is headlined by its arm*) — only nothing-moved reads as a failure, and its
/// sentence already says so. The undecided arm whose persist was not verified
/// carries the seed's only copy, so it is PARKED exactly as
/// `dispatch_persist_outcome` parks the persist-failure message — never `say`,
/// whose no-handler arm logs its whole argument.
fn dispatch_unlanded(
    outcome: &fauna_client_recovery::ceremony::StolenOutcome,
    say: impl FnOnce(String),
) {
    let message = outcome
        .message()
        .map(|m| m.resolve(crate::i18n::strings::lookup))
        .unwrap_or_default();
    if outcome.carries_the_only_seed() {
        tracing::error!(
            "[settings/recovery] the succession's outcome is unknown and the successor secret \
             did not read back locally"
        );
        park_stolen_failed_message(message);
    } else {
        say(message);
    }
}

/// Fold a returned sweep retry: the fresher report replaces the parked one, or
/// the shared projection's sentence goes on `error-message`.
pub fn apply_sweep_retried(
    result: &Result<Box<fauna_client_recovery::ceremony::SweepStatus>, String>,
) {
    let answer = match result {
        Ok(status) => {
            recovery_kit::park_succession_sweep((**status).clone());
            None
        }
        Err(message) => Some(message.clone()),
    };
    let handler = SWEEP_RETRIED_HANDLER.with(|cell| cell.borrow().clone());
    match handler {
        Some(handler) => handler(answer),
        None => tracing::error!(
            "[settings/recovery] a sweep retry came back with no section registered to show it"
        ),
    }
}

// ---------------------------------------------------------------------------
// Pending actions — repaint hook for the Settings/Account section
// (`settings.md` § Pending actions). Registered by `settings::pending_actions`
// when it builds the section; invoked from the `PendingActionsLoaded` handler
// in `app.rs`. No cache is kept here, same reasoning as recovery kit above:
// the section's own view state lives in the page closure, and a list read is
// never trusted stale.
// ---------------------------------------------------------------------------

type PendingActionsHandler =
    Rc<dyn Fn(Result<Vec<fauna_protocol::pending_actions::PendingActionSummary>, String>)>;

thread_local! {
    static PENDING_ACTIONS_HANDLER: RefCell<Option<PendingActionsHandler>> =
        const { RefCell::new(None) };
}

/// Register the Pending actions section's repaint hook for a landed list read.
pub fn set_pending_actions_handler(handler: PendingActionsHandler) {
    PENDING_ACTIONS_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(handler);
    });
}

/// Repaint the Pending actions section from a fresh list read.
pub fn apply_pending_actions(
    result: Result<Vec<fauna_protocol::pending_actions::PendingActionSummary>, String>,
) {
    let handler = PENDING_ACTIONS_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(handler) = handler {
        handler(result);
    }
}

// ---------------------------------------------------------------------------
// Sign-out handler — registered once from main.rs with access to the main
// window state. Invoked by the sign-out button in the account settings page
// and by the test-agent "reset"/"logout" actions.
// ---------------------------------------------------------------------------

/// Register the sign-out handler. The handler must clear credentials, drop
/// the authenticated session, close the main window, and present onboarding.
pub fn set_sign_out_handler(handler: impl Fn() + 'static) {
    SIGN_OUT_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Rc::new(handler));
    });
}

/// Invoke the registered sign-out handler, if any. No-op before the main
/// shell is built (i.e. during onboarding).
pub fn trigger_sign_out() {
    let handler = SIGN_OUT_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(h) = handler {
        h();
    }
}

/// Register the post-auth launch-escalation handler (`security.md` § Post-auth
/// surfacing). Registered once from main.rs.
pub fn set_launch_escalation_handler(handler: impl Fn() + 'static) {
    LAUNCH_ESCALATION_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Rc::new(handler));
    });
}

/// Escalate a terminal post-auth verdict to the launch surface: invoke the
/// registered handler, if any. `verdict` is a diagnostic label only
/// (`nest-identity-changed`, `identity-superseded`) — the handler's teardown is
/// the same either way, and the launch flow re-derives which surface to render.
///
/// The no-handler case is a genuine no-op rather than a fallback: before the
/// authenticated shell exists the *launch* flow already owns these verdicts and
/// renders them directly, so there is nothing to escalate — and inventing a
/// fallback surface here is precisely the "second, softer per-app shape"
/// the goal doc forbids.
pub fn escalate_to_launch(verdict: &str) {
    let handler = LAUNCH_ESCALATION_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(h) = handler {
        tracing::info!("[identity] escalating to the launch surface: {verdict}");
        h();
    } else {
        tracing::warn!(
            "[identity] {verdict} with no authenticated shell to block — \
             the launch flow owns this verdict"
        );
    }
}

/// Register the account-switch handler. The handler makes `actor_id` the active
/// account and rebuilds the authenticated session for it (teardown + reconnect,
/// no relaunch). Registered once from main.rs.
///
/// `confirmed` is the Stage-2 re-auth bit (`long-term-store.md` § Multi-account
/// evolution → Per-account re-auth): true iff the user has *just* completed the
/// re-auth confirmation for this activation, which routes the handler through
/// `set_active_confirmed` instead of plain `set_active`. Plain `set_active`
/// refuses a flagged account with `ConfirmationRequired`, so a path that skipped
/// the prompt fails loudly here rather than silently waving the gate through.
pub fn set_switch_account_handler(handler: impl Fn(String, bool) + 'static) {
    SWITCH_ACCOUNT_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Rc::new(handler));
    });
}

/// Invoke the registered account-switch handler for `actor_id`, if any. No-op
/// before the main shell is built. Called by the account-switcher rows — via
/// `account::request_switch_account`, which resolves the Stage-2 re-auth gate
/// first and is the only thing that may pass `confirmed = true`.
pub fn trigger_switch_account(actor_id: String, confirmed: bool) {
    let handler = SWITCH_ACCOUNT_HANDLER.with(|cell| cell.borrow().clone());
    match handler {
        Some(h) => h(actor_id, confirmed),
        // A switch that silently does nothing is a convention-11 violation in
        // production code: the caller (an account-switcher row, or onboarding's
        // append branch) has already made the account active in the registry,
        // so a dropped trigger leaves the live session belonging to the OUTGOING
        // identity with nothing anywhere saying so. Registration happens in
        // `build_ui` *after* an early return, so any path that re-enters with a
        // window already present can reach here unset. Loud, not silent.
        None => tracing::warn!(
            actor_id = %actor_id,
            confirmed,
            "[account-switch] no switch handler registered — the switch was DROPPED; \
             the registry says this account is active but the live session is not"
        ),
    }
}

/// Register the factory-reset handler. Like the sign-out handler, but instead
/// of clearing credentials it keeps them (the identity is still valid for the
/// re-claim) and re-seeds onboarding at the claim-code step with the returned
/// code pre-filled. Registered once from main.rs.
pub fn set_factory_reset_handler(handler: impl Fn(String, String, String, String) + 'static) {
    FACTORY_RESET_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Rc::new(handler));
    });
}

/// Invoke the registered factory-reset handler with the re-onboard seed
/// `(claim_code, nest_url, secret_hex, handle)`. No-op before the main shell is
/// built. Called from the `FactoryResetComplete` message handler in app.rs.
pub fn trigger_factory_reset(
    claim_code: String,
    nest_url: String,
    secret_hex: String,
    handle: String,
) {
    let handler = FACTORY_RESET_HANDLER.with(|cell| cell.borrow().clone());
    if let Some(h) = handler {
        h(claim_code, nest_url, secret_hex, handle);
    }
}

/// Register the authenticated UI message pump's shutdown flag. The pump
/// reads this flag each tick and breaks when it becomes `true`. Replaces
/// any previously registered flag.
pub fn register_pump_shutdown(flag: Rc<std::cell::Cell<bool>>) {
    PUMP_SHUTDOWN.with(|cell| {
        *cell.borrow_mut() = Some(flag);
    });
}

/// Signal the authenticated UI message pump to shut down on its next tick.
/// Called by the sign-out handler so the pump drops its FaunaClient ref
/// and stops driving messages into the now-detached widget tree.
pub fn trigger_pump_shutdown() {
    let flag = PUMP_SHUTDOWN.with(|cell| cell.borrow().clone());
    if let Some(f) = flag {
        f.set(true);
    }
    PUMP_SHUTDOWN.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

// ---------------------------------------------------------------------------
// Settings pages
// ---------------------------------------------------------------------------
//
// The former `build_preferences_window` (an `adw::PreferencesWindow` modal with
// all settings tabs, opened by the header cogwheel) has been REMOVED. Settings
// is now an inline vertical sidebar-swap shell — `views::settings_shell` — that
// assembles these same per-page builders as rail sub-pages reached from the
// left menu (settings.md § Navigation model). Each `build_*_page()` above stays
// the single per-page source the shell consumes.

#[cfg(test)]
mod inbox_mode_tests {
    //! The unknown-until-fetched contract behind a confirmed defect. These run without GTK: the widget half is pinned by the
    //! e2e `test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch`,
    //! which is the only place a real process boundary exists.
    //!
    //! Each `#[test]` gets its own thread, hence its own `thread_local!`, so
    //! "fresh process" is faithfully modelled and the tests cannot race.

    /// The whole defect in one assertion: a client that has not asked the nest
    /// anything must not claim to know the answer. The old code seeded this
    /// with `"open"` — the most permissive of the four modes — so every account
    /// was reported as fully open until it happened to be re-set.
    #[test]
    fn a_mode_nobody_fetched_or_chose_reads_as_unknown_not_open() {
        assert_eq!(
            super::get_inbox_mode(),
            "",
            "an un-fetched inbox mode must read as unknown, never as a default \
             the user never chose"
        );
    }

    /// `apply_loaded_inbox_mode` must do BOTH halves. Storing without notifying
    /// would leave the radio group painting nothing while the state JSON claimed
    /// the mode; notifying without storing would repaint a page that the test
    /// agent still reports as unknown. Either half alone re-opens the defect in
    /// a new place, so the pair is pinned together.
    #[test]
    fn a_loaded_mode_is_both_stored_and_announced() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let seen = seen.clone();
            super::set_inbox_mode_loaded_handler(std::rc::Rc::new(move |mode: &str| {
                seen.borrow_mut().push(mode.to_string());
            }));
        }

        super::apply_loaded_inbox_mode("contacts_only");

        assert_eq!(super::get_inbox_mode(), "contacts_only", "stored half");
        assert_eq!(
            seen.borrow().as_slice(),
            ["contacts_only"],
            "announced half — the Privacy page repaints off this hook"
        );
    }

    /// A mode landing before any page is built must still be readable when one
    /// is: the Privacy page paints from `get_inbox_mode()` on construction
    /// precisely so a rebuild after the reply is not left blank forever.
    #[test]
    fn a_mode_that_lands_with_no_handler_registered_is_still_stored() {
        super::apply_loaded_inbox_mode("closed");
        assert_eq!(super::get_inbox_mode(), "closed");
    }
}

#[cfg(test)]
mod recovery_stolen_failed_message_tests {
    //! `apply_recovery_succeeded`'s persist-failure arm cannot be driven
    //! end-to-end here: it reads the real `account_registry()`, and this
    //! crate deliberately has no unit test that mints one — see
    //! `supervision_snapshot.rs`'s "No unit tests here" note and
    //! `account_registry_census_test`, which fails the build if a second
    //! call site tries. So these pin the pure part instead: the seed-bearing
    //! message never routes through `park_stolen_failed_message` without
    //! surviving a missing-handler window, and never through it into
    //! `tracing` at all (`settings.md` § Recovery kit; ).
    //!
    //! Each `#[test]` gets its own thread, hence its own `thread_local!` (the
    //! same isolation `inbox_mode_tests` relies on) and its own
    //! `tracing::subscriber::with_default` scope.

    /// The security pin this closes: parking a
    /// message — the only path a seed-bearing recovery message can take — must never
    /// itself put anything on `tracing`, whether or not a section is
    /// registered to show it. Before this fix, `say`'s no-handler arm logged
    /// the raw message straight into `fauna-log`'s ring + rolling file
    /// (`libs/fauna-log/src/lib.rs:17`'s crate rule broken); this pin fails
    /// loudly if that shape ever comes back — temporarily restoring the old
    /// `tracing::error!("...{message}...")` fallback in
    /// `park_stolen_failed_message` reds it for exactly that reason.
    #[test]
    fn parking_a_stolen_failed_message_never_itself_touches_tracing() {
        struct CaptureSubscriber {
            events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl tracing::Subscriber for CaptureSubscriber {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, event: &tracing::Event<'_>) {
                self.events
                    .lock()
                    .unwrap()
                    .push(fauna_log::record_event(event).message);
            }
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = CaptureSubscriber {
            events: events.clone(),
        };

        // The shape of a successor identity secret — not a real one.
        let seed = "f".repeat(64);
        tracing::subscriber::with_default(subscriber, || {
            super::park_stolen_failed_message(format!("import this recovery phrase: {seed}"));
        });

        let captured = events.lock().unwrap();
        assert!(
            captured.is_empty(),
            "parking a stolen-failed message must never itself emit a tracing \
             event — it may carry the successor's identity secret, the only \
             copy of the key the account now belongs to; got {captured:?}"
        );
    }

    /// The other half of criterion 2: a message parked before any section is
    /// built (or while one is off-screen) must still reach the screen once a
    /// section registers — page-keyed state a later paint reads, never a
    /// one-shot write into a widget reference captured at fold time. Mirrors
    /// `inbox_mode_tests::a_mode_that_lands_with_no_handler_registered_is_still_stored`.
    #[test]
    fn a_parked_message_is_shown_the_moment_a_section_registers() {
        super::park_stolen_failed_message("import this recovery phrase: SEED".to_string());

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let seen = seen.clone();
            super::set_recovery_stolen_failed_handler(std::rc::Rc::new(move |message: String| {
                seen.borrow_mut().push(message);
            }));
        }

        assert_eq!(
            seen.borrow().as_slice(),
            ["import this recovery phrase: SEED"],
            "a message parked before any section existed must reach the first \
             section that ever registers, not be lost"
        );
    }

    /// The ceremony's non-landed arms paint the shared sentence verbatim — no
    /// failure headline on the arms that are not failures — and the
    /// undecided-unsaved arm, which carries the seed's only copy, is PARKED
    /// rather than handed to `say` (whose no-handler arm would log the seed).
    /// Red-verify by routing that arm through `say`: the `said` assertion fails.
    #[test]
    fn the_unlanded_arms_paint_their_own_sentence_and_the_unsaved_seed_is_parked() {
        use crate::i18n::strings::settings::recovery_kit as rk;
        use fauna_client_recovery::ceremony::StolenOutcome;

        let said = std::cell::RefCell::new(Vec::<String>::new());
        super::dispatch_unlanded(&StolenOutcome::not_landed("nest refused"), |m| {
            said.borrow_mut().push(m)
        });
        let actor = fauna_core::identity::ActorId([9u8; 32]);
        super::dispatch_unlanded(
            &StolenOutcome::LandedForAnother {
                new_actor_id: actor,
            },
            |m| said.borrow_mut().push(m),
        );
        assert_eq!(
            said.borrow().as_slice(),
            [
                rk::stolen_ceremony_failed("nest refused"),
                rk::stolen_landed_for_another(&actor.to_hex()),
            ],
            "each arm's own sentence, and the failure headline on nothing-moved alone"
        );

        let seed = "f".repeat(64);
        let reported = fauna_client_recovery::RecoveryError::Transport("gone".to_string());
        let unsaved = StolenOutcome::undecided("cause".to_string(), false, &seed, &reported);
        let mut said_unsaved = false;
        super::dispatch_unlanded(&unsaved, |_| said_unsaved = true);
        assert!(
            !said_unsaved,
            "the seed-bearing arm must never route through `say`"
        );
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let seen = seen.clone();
            super::set_recovery_stolen_failed_handler(std::rc::Rc::new(move |message: String| {
                seen.borrow_mut().push(message);
            }));
        }
        assert_eq!(
            seen.borrow().len(),
            1,
            "the unsaved arm parks exactly one message"
        );
        assert!(seen.borrow()[0].contains(&seed), "the seed is the way back");
    }

    /// The regression pin a bounded recovery-kit fix asks for: a red-first check AT THE CALL SITE, not only
    /// on `park_stolen_failed_message`'s own body. `apply_recovery_succeeded`
    /// itself has no test caller (it needs a real `account_registry()`,
    /// which this crate's census test forbids minting in a unit test), so
    /// this drives the seam that carries its persist check's actual branch
    /// instead. Temporarily reverting `dispatch_persist_outcome`'s failure
    /// arm to route through something other than `park_stolen_failed_message`
    /// (e.g. a bare `tracing::error!` of the message, mirroring the old
    /// pre-fix `say` fallback) reds this.
    #[test]
    fn a_persist_failure_parks_the_message_never_the_switch() {
        let switched = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let switched = switched.clone();
            super::dispatch_persist_outcome(false, &"f".repeat(64), move || switched.set(true));
        }
        assert!(
            !switched.get(),
            "a failed persist must never proceed to the account switch"
        );

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let seen = seen.clone();
            super::set_recovery_stolen_failed_handler(std::rc::Rc::new(move |message: String| {
                seen.borrow_mut().push(message);
            }));
        }
        assert_eq!(
            seen.borrow().len(),
            1,
            "a failed persist must park exactly one seed-bearing message for \
             the section to show"
        );
    }

    /// The mirror image: a successful persist must switch and must NEVER also
    /// park a message — the two outcomes are exclusive.
    #[test]
    fn a_persist_success_switches_and_never_parks() {
        let switched = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let switched = switched.clone();
            super::dispatch_persist_outcome(true, "irrelevant", move || switched.set(true));
        }
        assert!(
            switched.get(),
            "a successful persist must proceed to the account switch"
        );

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let seen = seen.clone();
            super::set_recovery_stolen_failed_handler(std::rc::Rc::new(move |message: String| {
                seen.borrow_mut().push(message);
            }));
        }
        assert!(
            seen.borrow().is_empty(),
            "a successful persist must never park a persist-failure message"
        );
    }
}
