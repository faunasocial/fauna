//! The user-facing "Spam" preferences page (linux; the mail-UX seed's lead
//! app).
//!
//! Where a person manages **their own** per-account spam classifier: reset the
//! per-user Bayesian model, opt in/out of the deployment-baseline contribution,
//! and review + undo individual training events. Target behavior:
//! `docs/goal/behavior/mail-spam.md` § Reset / § Cold start Path 2 /
//! § Training-sample retention + § Undo. UX/IDs: `tests/e2e-unified/ui.yaml`
//! `mail-spam` page + `mail-spam-training-history-list` component.
//!
//! Per `mail-spam.md` § Where logic lives this layer holds **no** business
//! logic — it is a dumb renderer of [`MailSpamSnapshot`] + dispatcher of
//! [`MailSpamAction`]; the projection + action sequencing live in the shared
//! `fauna_client_mail_settings::spam` machine (priority #2/#4), the prior art the
//! other five apps lift. Direct sibling: `settings/mail_aliases.rs`.
//!
//! # UI precedes backend (surfaced, never faked)
//!
//! The per-user Bayesian feedback loop is **not built yet** (`mail-spam.md`
//! § Implementation status today): the shared seam returns an honest
//! `unimplemented` rejection for every RPC, so on a real nest this page renders
//! its controls + an `error-message` explaining the feature isn't available yet
//! — it never fabricates training rows. The training-history rows
//! (`mail-spam-training-history-list-item*`) are therefore render-time only (no
//! rows until the backend lands), exactly like `mail-aliases`' indexed rows.
//!
//! # AT-SPI discoverability
//!
//! Same idiom as `mail_aliases.rs`: read-only fields carry a 1px marker
//! `gtk::Label`; directly-actuable controls (`gtk::Button`, `gtk::Switch`) carry
//! the ID on the widget itself. The page is embedded in the status view
//! (`views/status.rs`) so its IDs are reachable through the state protocol (the
//! separate `adw::PreferencesWindow` modal isn't).

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::async_helper::spawn_with_snapshot;

use fauna_client_mail_settings::{
    MailSpamAction, MailSpamMachine, MailSpamSnapshot, SpamTrainingView,
};

use fauna_client::NestClient;
use fauna_client_bridges::MailAccountClient;
use fauna_client_moderation::ModerationClient;
use fauna_client_moderation::moderation::ModerationReportShareStatusReply;

use crate::i18n::strings::mail_spam as S;
use crate::testid::set_test_id;

/// Handles to the widgets the snapshot renders into. GTK objects are
/// reference-counted, so cloning this is cheap and shares the same widgets.
#[derive(Clone)]
struct SpamWidgets {
    error_label: gtk::Label,
    reset_button: gtk::Button,
    contribute_toggle: gtk::Switch,
    // Distributed report-sharing opt-in + "what this nest publishes" list
    // (report-sharing.md § Client wire + transparency surface).
    share_reports_toggle: gtk::Switch,
    // Per-account spam-folder threshold override — empty follows the admin default,
    // "0" is a real setting (turns auto-Junk filing off). Commits on Enter,
    // no separate save button (tui's `Element::input_commit` shape).
    threshold_input: gtk::Entry,
    published_group: adw::PreferencesGroup,
    published_placeholder: adw::ActionRow,
    published_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,

    // Training-history list group + dynamic rows.
    list_group: adw::PreferencesGroup,
    list_placeholder: adw::ActionRow,
    rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct SpamCtx {
    machine: Arc<MailSpamMachine>,
    /// The WS-RPC handle the report-share flow builds a [`ModerationClient`] over
    /// (`fauna.moderation.report_share.{set,status}`). The transparency surface
    /// is a plain read/write of the shared manager — no state machine, unlike the
    /// per-user classifier the `MailSpamMachine` drives.
    nest: Arc<NestClient>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates the contribute toggle, so
    /// its `active_notify` handler doesn't echo the change back as a dispatch.
    syncing: Cell<bool>,
    /// The same echo-suppression flag for the report-share toggle.
    rs_syncing: Cell<bool>,
    w: SpamWidgets,
}

/// Build the "Spam" preferences page.
pub fn build_mail_spam_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("mail-mark-junk-symbolic")
        .build();

    // --- Top group: heading + page-level error + reset + contribute toggle ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    // mail-spam-reset-model-button — destructive; two-click inline confirm.
    let reset_button = gtk::Button::builder()
        .label(S::RESET_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&reset_button, ids::MAIL_SPAM_RESET_MODEL_BUTTON);
    crate::offline_gate::declare_wire_kind(&reset_button, "fauna.bridges.reset_spam_model");
    let reset_row = adw::ActionRow::builder()
        .title(S::RESET_BUTTON)
        .subtitle(S::RESET_SUBTITLE)
        .activatable(false)
        .build();
    reset_row.add_suffix(&reset_button);
    top_group.add(&reset_row);

    // mail-spam-contribute-baseline-toggle — opt in/out of the deployment
    // baseline. Default off (user-controls-their-data).
    let contribute_toggle = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    set_test_id(
        &contribute_toggle,
        ids::MAIL_SPAM_CONTRIBUTE_BASELINE_TOGGLE,
    );
    crate::offline_gate::declare_wire_kind(
        &contribute_toggle,
        "fauna.bridges.set_baseline_contribution",
    );
    let contribute_row = adw::ActionRow::builder()
        .title(S::CONTRIBUTE_BASELINE_LABEL)
        .subtitle(S::CONTRIBUTE_BASELINE_SUBTITLE)
        .activatable(false)
        .build();
    contribute_row.add_suffix(&contribute_toggle);
    top_group.add(&contribute_row);

    // mail-spam-share-reports-toggle — opt in/out of distributed, k-anonymized
    // report sharing (report-sharing.md § Client wire). Default off
    // (user-controls-their-data). Sibling of the baseline toggle.
    let share_reports_toggle = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    set_test_id(&share_reports_toggle, ids::MAIL_SPAM_SHARE_REPORTS_TOGGLE);
    // NOT a `MailSpamMachine`/bridge call — a plain write on the shared
    // moderation manager (`ModerationClient::report_share_set`), per tui's
    // `MailSpamToggleShareReports` ruling.
    crate::offline_gate::declare_wire_kind(
        &share_reports_toggle,
        "fauna.moderation.report_share.set",
    );
    let share_reports_row = adw::ActionRow::builder()
        .title(S::SHARE_REPORTS_LABEL)
        .subtitle(S::SHARE_REPORTS_SUBTITLE)
        .activatable(false)
        .build();
    share_reports_row.add_suffix(&share_reports_toggle);
    top_group.add(&share_reports_row);

    // mail-spam-threshold-override-input — per-account spam-folder threshold
    // override (mail-policy-config.md § Tier 3). Empty follows the admin
    // default; "0" is a real setting, never collapsed into empty. Commits on
    // Enter — no separate save button, the same shape as the report-share
    // toggle's own direct-RPC pattern (not the MailSpamMachine).
    let threshold_input = gtk::Entry::builder().build();
    set_test_id(&threshold_input, ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT);
    // NOT a `MailSpamMachine` action either — a plain write on
    // `MailAccountClient` (`mail-policy-config.md` § Tier 3), per tui's
    // `MailSpamCommitThreshold` ruling.
    crate::offline_gate::declare_wire_kind(
        &threshold_input,
        "fauna.bridges.set_spam_threshold_override",
    );
    let threshold_row = adw::ActionRow::builder()
        .title(S::THRESHOLD_OVERRIDE_LABEL)
        .subtitle(S::THRESHOLD_OVERRIDE_SUBTITLE)
        .activatable(false)
        .build();
    threshold_row.add_suffix(&threshold_input);
    top_group.add(&threshold_row);
    page.add(&top_group);

    // --- "What this nest publishes" transparency list ---
    // report-share-published-list — the ≥k aggregates this nest exports to peers,
    // byte-identical to the federation export (the transparency guarantee). Rows
    // (report-share-published-list-item*) are rebuilt from the status reply on
    // every render_report_share(). Empty on a fresh nest.
    let published_group = adw::PreferencesGroup::builder()
        .title(S::PUBLISHED_TITLE)
        .description(S::PUBLISHED_DESCRIPTION)
        .build();
    published_group.set_header_suffix(Some(&super::marker("report-share-published-list")));
    let published_placeholder = adw::ActionRow::builder().title(S::PUBLISHED_EMPTY).build();
    published_group.add(&published_placeholder);
    page.add(&published_group);

    // --- Training-history list group ---
    // mail-spam-training-history-list — the group container. Per-event rows
    // (mail-spam-training-history-list-item*) are rebuilt from
    // MailSpamSnapshot.events on every render().
    let list_group = adw::PreferencesGroup::builder()
        .title(S::HISTORY_TITLE)
        .build();
    list_group.set_header_suffix(Some(&super::marker("mail-spam-training-history-list")));
    let list_placeholder = adw::ActionRow::builder().title(S::EMPTY).build();
    list_group.add(&list_placeholder);
    page.add(&list_group);

    let widgets = SpamWidgets {
        error_label,
        reset_button,
        contribute_toggle,
        share_reports_toggle,
        threshold_input,
        published_group,
        published_placeholder,
        published_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        list_group,
        list_placeholder,
        rows: Rc::new(std::cell::RefCell::new(Vec::new())),
    };
    // Re-read the training history every time the page is shown. It changes
    // *outside* the app — a third-party MUA setting the IMAP `\Junk` flag (or a
    // "Mark as spam" elsewhere) trains the model and appends history — so the page
    // must refresh on becoming visible, mirroring the data-driven views'
    // refresh-on-visible (app.rs `connect_visible_child_name_notify`; the feed's
    // `connect_map`). The settings shell builds every sub-page once at app init,
    // so without this the list would only ever show its build-time hydrate
    // (empty at a fresh start) until a reset/undo/toggle dispatch.
    if let Some(refresh) = wire_machine(widgets) {
        page.connect_map(move |_| refresh());
    }

    page
}

/// Connect the page to the shared `MailSpamMachine`, hydrate on mount, and wire
/// every interaction. No-op (page stays at static placeholders) when no client
/// is available — e.g. the unit test, which has no registered client.
fn wire_machine(widgets: SpamWidgets) -> Option<Rc<dyn Fn()>> {
    let client = crate::settings::get_client()?;
    // `Err` only on an unrecoverable identity fault (launch has validated the
    // secret to reach Online); the page stays at static placeholders if so.
    let machine = Arc::new(crate::mail_glue::build_mail_spam_machine(&client).ok()?);

    let ctx = Rc::new(SpamCtx {
        machine,
        nest: client.nest_rpc().clone(),
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        rs_syncing: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);
    hydrate_report_share(&ctx);
    hydrate_threshold_override(&ctx);

    // Reset button → two-click inline confirm → ResetModel.
    {
        let ctx = Rc::clone(&ctx);
        super::wire_two_click(
            &ctx.w.reset_button.clone(),
            S::RESET_BUTTON,
            S::RESET_CONFIRM,
            true,
            |_| {},
            |_| {},
            move || dispatch_action(&ctx, MailSpamAction::ResetModel),
        );
    }

    // Contribute toggle → SetContributeBaseline (skip the echo from render()).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .contribute_toggle
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    MailSpamAction::SetContributeBaseline {
                        contribute: sw.is_active(),
                    },
                );
            });
    }

    // Share-reports toggle → fauna.moderation.report_share.set (skip the echo
    // from render_report_share()). A false→true toggle just flips the opt-in;
    // true→false ALSO withdraws every report this actor contributed (the nest
    // opt-out sweep), so the published list may shrink on the re-read.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .share_reports_toggle
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.rs_syncing.get() {
                    return;
                }
                set_report_share(&ctx, sw.is_active());
            });
    }

    // Threshold input → commit on Enter (no separate save button, tui's
    // `Element::input_commit` shape).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.threshold_input.clone().connect_activate(move |_| {
            commit_threshold_override(&ctx);
        });
    }

    // Refresh-on-visible handle: re-read the training history + contribution flag,
    // the report-share opt-in + published list, AND the threshold override (all
    // change out-of-band — a third-party MUA's `\Junk`, another user on this nest
    // crossing k, another device editing the override).
    let refresh_ctx = Rc::clone(&ctx);
    Some(Rc::new(move || {
        hydrate_and_render(&refresh_ctx);
        hydrate_report_share(&refresh_ctx);
        hydrate_threshold_override(&refresh_ctx);
    }))
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<SpamCtx>) {
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
fn dispatch_action(ctx: &Rc<SpamCtx>, action: MailSpamAction) {
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

/// Read the report-share opt-in + published list over the shared
/// [`ModerationClient`] (`fauna.moderation.report_share.status`) and render it.
/// A single NestClient RPC — the transport already parks it while the socket
/// comes up (transport.md § Request lifecycle step 3).
fn hydrate_report_share(ctx: &Rc<SpamCtx>) {
    let nest = Arc::clone(&ctx.nest);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let client = ModerationClient::new(nest);
            client
                .report_share_status()
                .await
                .map_err(|e| e.to_string())
        },
        move |res| render_report_share(&ctx_render, res),
    );
}

/// Set the report-share opt-in (`fauna.moderation.report_share.set`), then
/// re-read status so the toggle + published list reflect the persisted value
/// (opting out withdraws this actor's reports, which may shrink the list).
/// Owned by `ModerationClient::report_share_set_and_reload`.
fn set_report_share(ctx: &Rc<SpamCtx>, share: bool) {
    let nest = Arc::clone(&ctx.nest);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            ModerationClient::new(nest)
                .report_share_set_and_reload(share)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| render_report_share(&ctx_render, res),
    );
}

/// Read `fauna.bridges.get_spam_threshold_override` and render it. A single
/// NestClient RPC — the transport already parks it while the socket comes up
/// (transport.md § Request lifecycle step 3).
fn hydrate_threshold_override(ctx: &Rc<SpamCtx>) {
    let nest = Arc::clone(&ctx.nest);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let client = MailAccountClient::new(nest);
            client
                .get_spam_threshold_override()
                .await
                .map_err(|e| e.to_string())
        },
        move |res| render_threshold_override(&ctx_render, res),
    );
}

/// Parse the entry's current text (empty → `None`, `fauna_core::format::parse_count`
/// — the same shared validator the alias add-sheet's own `spam_threshold_override`
/// field uses), set it (`fauna.bridges.set_spam_threshold_override`), then
/// re-read so the input reflects the **persisted** value, never the local
/// keystroke — the `set_report_share` shape. `Some(0)` is a real setting, never
/// collapsed into "unset".
fn commit_threshold_override(ctx: &Rc<SpamCtx>) {
    let nest = Arc::clone(&ctx.nest);
    let ctx_render = Rc::clone(ctx);
    let value = fauna_core::format::parse_count(&ctx.w.threshold_input.text());
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            MailAccountClient::new(nest)
                .set_spam_threshold_override_and_reload(value)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| render_threshold_override(&ctx_render, res),
    );
}

/// Render a `get_spam_threshold_override` result into the input's text.
/// `None` renders empty (follows the admin default); `Some(0)` renders "0",
/// never blank — an error surfaces via the page `error-message`.
fn render_threshold_override(ctx: &Rc<SpamCtx>, res: Result<Option<u32>, String>) {
    let w = &ctx.w;
    match res {
        Ok(value) => {
            w.threshold_input
                .set_text(&value.map(|v| v.to_string()).unwrap_or_default());
        }
        Err(msg) => super::render_error_label(&w.error_label, Some(&msg)),
    }
}

/// Render a report-share status reply: reflect the opt-in toggle
/// (echo-suppressed) and rebuild the published list. An error surfaces via the
/// page `error-message`. Delegates to the shared `render_share_pane` (round
/// 195 of the shared-Rust harvest sweep).
fn render_report_share(ctx: &Rc<SpamCtx>, res: Result<ModerationReportShareStatusReply, String>) {
    let w = &ctx.w;
    super::render_share_pane(
        super::SharePaneWidgets {
            toggle: &w.share_reports_toggle,
            syncing: &ctx.rs_syncing,
            error_label: &w.error_label,
            published_group: &w.published_group,
            published_rows: &w.published_rows,
            published_placeholder: &w.published_placeholder,
        },
        "report-share",
        S::PUBLISHED_REPORTERS,
        res.map(|s| (s.share, s.published)),
    );
}

/// Render a `MailSpamSnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<SpamCtx>, snap: &MailSpamSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Reflect the persisted contribution flag without echoing it back as a
    // dispatch (the active_notify handler checks ctx.syncing).
    if w.contribute_toggle.is_active() != snap.contribute_baseline {
        ctx.syncing.set(true);
        w.contribute_toggle.set_active(snap.contribute_baseline);
        ctx.syncing.set(false);
    }

    // Training-history list: tear down the previous rows, rebuild from the
    // snapshot.
    {
        let mut rows = w.rows.borrow_mut();
        for row in rows.drain(..) {
            w.list_group.remove(&row);
        }
        for view in &snap.events {
            let row = build_history_row(ctx, view);
            w.list_group.add(&row);
            rows.push(row);
        }
    }
    w.list_placeholder.set_visible(snap.events.is_empty());
}

/// Build one `mail-spam-training-history-list-item` row from a
/// [`SpamTrainingView`]. Read-only fields carry 1px marker labels; the undo is a
/// single-click dispatch (it's reversible-by-retrain, so no confirm).
fn build_history_row(ctx: &Rc<SpamCtx>, view: &SpamTrainingView) -> adw::ActionRow {
    let label_text = fauna_client_mail_settings::training_label_badge(view.label)
        .resolve(crate::i18n::strings::lookup);
    let source_text = fauna_client_mail_settings::training_source_badge(view.source)
        .resolve(crate::i18n::strings::lookup);
    let row = adw::ActionRow::builder()
        .title(&view.message)
        .subtitle(format!("{label_text} · {source_text}"))
        .build();
    // mail-spam-training-history-list-item — indexed row container marker.
    row.add_prefix(&super::marker("mail-spam-training-history-list-item"));

    row.add_suffix(&super::value_marker(
        "mail-spam-training-history-list-item-message",
        &view.message,
    ));
    row.add_suffix(&super::value_marker(
        "mail-spam-training-history-list-item-label",
        &label_text,
    ));
    row.add_suffix(&super::value_marker(
        "mail-spam-training-history-list-item-source",
        &source_text,
    ));
    row.add_suffix(&super::value_marker(
        "mail-spam-training-history-list-item-created-at",
        &crate::i18n::local_date(view.created_at_ms),
    ));

    // mail-spam-training-history-list-item-undo-button — apply the inverse delta.
    let undo_button = gtk::Button::builder()
        .label(S::UNDO)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(
        &undo_button,
        ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_UNDO_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(&undo_button, "fauna.bridges.put_spam_model");
    {
        let ctx = Rc::clone(ctx);
        let history_id_hex = view.history_id_hex.clone();
        undo_button.connect_clicked(move |_| {
            dispatch_action(
                &ctx,
                MailSpamAction::UndoTraining {
                    history_id_hex: history_id_hex.clone(),
                },
            );
        });
    }
    row.add_suffix(&undo_button);

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The Spam page exposes every static ui.yaml ID for the `mail-spam` page.
    /// The indexed `mail-spam-training-history-list-item*` rows are added from
    /// the snapshot at render time (no registered client + an unbuilt backend in
    /// this test), so they're not asserted here.
    #[test]
    fn spam_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_spam_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-spam-reset-model-button",
                "mail-spam-contribute-baseline-toggle",
                "mail-spam-share-reports-toggle",
                "mail-spam-threshold-override-input",
                "report-share-published-list",
                "mail-spam-training-history-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
