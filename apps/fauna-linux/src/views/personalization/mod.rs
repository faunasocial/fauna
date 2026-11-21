//! Settings → **Personalization** (the unified home) + Settings → **Community
//! labelers** (the catalog) — two settings sub-pages backed by **one** shared
//! `LabelerCatalogMachine` and **one** observer-driven render loop. Mirrors
//! `views/devices_folders/mod.rs` (two sub-pages, one shared machine).
//!
//! `docs/goal/architecture/content-moderation-and-ranking.md` § Composition +
//! § Tier-3 community models. The personalization home hubs three facets
//! WITHOUT rebuilding them: **Feeds** (a link out to the feed page's
//! create-feed dialog — the trainable factor-weight authoring lands once Slice 1 ships), **Muted words** (a link
//! re-homing the ALREADY-SHIPPED `settings::muted_words` sub-page — reused,
//! not rebuilt), and **Community labelers** (the caller's SUBSCRIBED tier-3
//! labelers rendered inline, each unsubscribable, with a link out to the full
//! **Community labelers** catalog sub-page for browse + inspect-before-
//! subscribe + subscribe).

pub(crate) mod inspect_panel;
mod publish_sheet;
mod trained_topics;

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_labeler_catalog_machine::{
    LabelerCatalogEntry, LabelerCatalogMachine, LabelerCatalogObserver,
};

use fauna_client_moderation::moderation::ModerationSignalShareStatusReply;
use fauna_client_personalization::engagement::{
    clear_engagement_possibility, share_signals_possibility,
};
use fauna_core::possibility::{Possibility, ReadState};

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings;
use crate::i18n::strings::{labeler_catalog, personalization};
use crate::testid::set_test_id;

/// Handles the two sub-pages hand back to the settings shell.
pub struct PersonalizationHandles {
    /// The shared-Rust `LabelerCatalogMachine`. Refreshed on auth + whenever
    /// either sub-page becomes visible.
    pub labeler_catalog_machine: Arc<LabelerCatalogMachine>,
    /// The Trained topics facet's own load — what the home page's map runs;
    /// the store-change notice re-drives it while the page is open
    /// (`crate::store_surfaces`).
    pub trained_topics_refresh: Rc<dyn Fn()>,
}

crate::async_helper::snapshot_wake_observer!(
    /// Bridges `LabelerCatalogMachine` notifications to the GTK main loop. Mirrors
    /// `GtkDevicesObserver` / `GtkMediaObserver`.
    GtkLabelerCatalogObserver,
    LabelerCatalogObserver
);

/// Build the shared machine **with its grant seams**: subscribing a `wasm`
/// mail labeler mints the per-labeler grant to this nest's mail service and
/// unsubscribing revokes it (`content-moderation-and-ranking.md` § Tier-3 →
/// *Subscribing = minting a capability*), which needs the actor's identity key
/// — tui's `LabelerCatalogState::build`, lifted. A `secret_hex` that will not
/// decode (the launch flow validated it to reach Online, so this is an
/// unrecoverable identity fault) falls back to the grant-less machine with a
/// warning, so the pages still browse, inspect and (un)subscribe — the Nests
/// page's own fallback (`mail_glue::build_linked_nests_machine_with_mail_relay_and_trust`).
pub(crate) fn build_machine(
    fauna_client: &FaunaClient,
    observer: Arc<dyn LabelerCatalogObserver>,
) -> Arc<LabelerCatalogMachine> {
    let nest = Arc::clone(fauna_client.nest_rpc());
    match fauna_core::identity::ActorKeypair::from_secret_hex(fauna_client.secret_hex()) {
        Ok(keypair) => fauna_labeler_catalog_machine::build_labeler_catalog_machine_with_grants(
            nest,
            keypair,
            crate::account_runtime::ledger_seam(),
            crate::account_runtime::mail_store(),
            observer,
        ),
        Err(e) => {
            tracing::warn!(
                "labeler catalog: decode secret_hex: {e}; building without grant seams — \
                 a mail labeler subscription will not mint its grant"
            );
            fauna_labeler_catalog_machine::build_labeler_catalog_machine(nest, observer)
        }
    }
}

fn page_heading(text: &str) -> gtk::Label {
    let heading = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .css_classes(["title-1"])
        .build();
    set_test_id(&heading, ids::PAGE_HEADING);
    heading
}

/// A control's reason line (`ui/README.md` § Offer only what is possible,
/// rule 2: the reason within eyeshot), hidden while the control is live. The
/// possibility seam (`crate::possible::paint`) owns its text and visibility.
fn reason_line() -> gtk::Label {
    gtk::Label::builder()
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build()
}

fn error_label() -> gtk::Label {
    let label = gtk::Label::builder().visible(false).build();
    label.add_css_class("error");
    set_test_id(&label, ids::ERROR_MESSAGE);
    label
}

/// The **Personalization** home sub-page static shell.
struct HomeShell {
    content: gtk::Box,
    feeds_link: gtk::Button,
    muted_words_link: gtk::Button,
    labelers_list: gtk::ListBox,
    labelers_empty: gtk::Label,
    browse_catalog_button: gtk::Button,
    clear_engagement_data_button: gtk::Button,
    /// `personalization-clear-engagement-data-button`'s reason line.
    clear_engagement_reason: gtk::Label,
    // Layer-B signal-sharing (engagement-cues.md § Layer B): the opt-in toggle +
    // the "what this nest publishes" transparency pane. Mirrors the report-share
    // pane in `settings/mail_spam.rs`, on the Personalization home instead.
    share_signals_toggle: gtk::Switch,
    /// `personalization-share-signals-toggle`'s reason line.
    share_signals_reason: gtk::Label,
    signal_published_group: adw::PreferencesGroup,
    signal_published_placeholder: adw::ActionRow,
    signal_published_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
    error_label: gtk::Label,
}

/// Returns the shell plus the Trained-topics facet's widget handles (already
/// appended into `content`; handed separately because `trained_topics::wire`
/// takes them by value while the shell moves into the render ctx).
fn build_home_shell() -> (HomeShell, trained_topics::TrainedTopicsWidgets) {
    let content = crate::views::layout::page_box(16);
    content.append(&page_heading(personalization::TITLE));
    let err = error_label();
    content.append(&err);

    let feeds_link = gtk::Button::with_label(personalization::FEEDS_LINK);
    set_test_id(&feeds_link, ids::PERSONALIZATION_FEEDS_LINK);
    content.append(&feeds_link);

    let muted_words_link = gtk::Button::with_label(personalization::MUTED_WORDS_LINK);
    set_test_id(&muted_words_link, ids::PERSONALIZATION_MUTED_WORDS_LINK);
    content.append(&muted_words_link);

    let labelers_group = adw::PreferencesGroup::builder()
        .title(labeler_catalog::TITLE)
        .build();
    let labelers_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(&labelers_list, ids::PERSONALIZATION_LABELERS_LIST);
    labelers_group.add(&labelers_list);
    content.append(&labelers_group);

    let labelers_empty = gtk::Label::new(Some(personalization::LABELERS_EMPTY));
    labelers_empty.add_css_class("dim-label");
    labelers_empty.set_halign(gtk::Align::Start);
    set_test_id(&labelers_empty, ids::PERSONALIZATION_LABELERS_EMPTY);
    content.append(&labelers_empty);

    let browse_catalog_button = gtk::Button::with_label(personalization::BROWSE_CATALOG);
    browse_catalog_button.set_halign(gtk::Align::Start);
    set_test_id(
        &browse_catalog_button,
        ids::PERSONALIZATION_BROWSE_CATALOG_BUTTON,
    );
    content.append(&browse_catalog_button);

    // Trained topics facet (topic-factors.md § Authoring surface) — the
    // trainable tier-1 factors' CRUD home, after the labelers facet per the
    // ui.yaml `personalization` page order.
    let trained = trained_topics::build_widgets();
    content.append(&trained.group);

    // "Clear activity data" (engagement-cues.md § At rest): the user-revocable
    // affordance for the sealed engagement-cue rollup — deletes cues:v1 from
    // the user's own nest and resets the live engine.
    let clear_engagement_data_button =
        gtk::Button::with_label(personalization::CLEAR_ENGAGEMENT_DATA);
    clear_engagement_data_button.set_halign(gtk::Align::Start);
    set_test_id(
        &clear_engagement_data_button,
        ids::PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON,
    );
    // Painted from its answer before the offline gate records the page's
    // intent (the web page's order): not live until the feed manager is up.
    let clear_engagement_reason = reason_line();
    crate::possible::paint(
        &clear_engagement_data_button,
        Some(&clear_engagement_reason),
        clear_engagement_possibility(manager_ready()),
    );
    crate::offline_gate::declare_wire_kind(
        &clear_engagement_data_button,
        "fauna.personalization.model.delete",
    );
    content.append(&clear_engagement_data_button);
    content.append(&clear_engagement_reason);

    // "Share anonymous signals" (engagement-cues.md § Layer B): the opt-in for
    // contributing k-anonymized per-item verdicts to the network — default off,
    // the sibling of mail-spam's report-sharing toggle. A false→true flip lets
    // this device's derived cue verdicts on PUBLIC posts join the shared
    // aggregate; true→false also withdraws every `signal:*` row this actor
    // contributed (the nest opt-out sweep), so the published list may shrink.
    let share_signals_group = adw::PreferencesGroup::builder()
        .title(personalization::SHARE_SIGNALS_TITLE)
        .build();
    let share_signals_toggle = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    set_test_id(
        &share_signals_toggle,
        ids::PERSONALIZATION_SHARE_SIGNALS_TOGGLE,
    );
    // Not live before the `signal_share.status` read answers: the switch's
    // pre-read `false` is the type's default, never the nest's word, and
    // flipping it would write a guess (`engagement-cues.md` § Layer B; rule 5).
    let share_signals_reason = reason_line();
    crate::possible::paint(
        &share_signals_toggle,
        Some(&share_signals_reason),
        share_signals_possibility(manager_ready(), ReadState::Pending),
    );
    crate::offline_gate::declare_wire_kind(
        &share_signals_toggle,
        "fauna.moderation.signal_share.set",
    );
    let share_signals_row = adw::ActionRow::builder()
        .title(personalization::SHARE_SIGNALS_LABEL)
        .subtitle(personalization::SHARE_SIGNALS_SUBTITLE)
        .activatable(false)
        .build();
    share_signals_row.add_suffix(&share_signals_toggle);
    share_signals_group.add(&share_signals_row);
    content.append(&share_signals_group);
    content.append(&share_signals_reason);

    // signal-share-published-list — the ≥k aggregates this nest exports to peers
    // (byte-identical to the federation export — the transparency guarantee;
    // nest-wide, so it carries `report:*` and `signal:*` alike). Rows
    // (signal-share-published-list-item*) are rebuilt from the status reply on
    // every render_signal_share(). Empty on a fresh nest.
    let signal_published_group = adw::PreferencesGroup::builder()
        .title(personalization::SIGNAL_PUBLISHED_TITLE)
        .description(personalization::SIGNAL_PUBLISHED_DESCRIPTION)
        .build();
    signal_published_group.set_header_suffix(Some(&crate::settings::marker(
        "signal-share-published-list",
    )));
    let signal_published_placeholder = adw::ActionRow::builder()
        .title(personalization::SIGNAL_PUBLISHED_EMPTY)
        .build();
    signal_published_group.add(&signal_published_placeholder);
    content.append(&signal_published_group);

    (
        HomeShell {
            content,
            feeds_link,
            muted_words_link,
            labelers_list,
            labelers_empty,
            browse_catalog_button,
            clear_engagement_data_button,
            clear_engagement_reason,
            share_signals_toggle,
            share_signals_reason,
            signal_published_group,
            signal_published_placeholder,
            signal_published_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
            error_label: err,
        },
        trained,
    )
}

/// The **Community labelers** catalog sub-page static shell.
struct CatalogShell {
    content: gtk::Box,
    catalog_list: gtk::ListBox,
    catalog_empty: gtk::Label,
    /// The inspect panel — the one widget the Community labelers page and a
    /// room's labeler-set editor both paint (`inspect_panel.rs`).
    inspect: inspect_panel::InspectPanel,
    error_label: gtk::Label,
}

fn build_catalog_shell() -> CatalogShell {
    let content = crate::views::layout::page_box(16);
    content.append(&page_heading(labeler_catalog::TITLE));
    let err = error_label();
    content.append(&err);

    let inspect = inspect_panel::InspectPanel::new();
    content.append(&inspect.root);

    let catalog_group = adw::PreferencesGroup::builder()
        .title(labeler_catalog::TITLE)
        .build();
    let catalog_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(&catalog_list, ids::LABELER_CATALOG_LIST);
    catalog_group.add(&catalog_list);
    content.append(&catalog_group);

    let catalog_empty = gtk::Label::new(Some(labeler_catalog::EMPTY));
    catalog_empty.add_css_class("dim-label");
    catalog_empty.set_halign(gtk::Align::Start);
    set_test_id(&catalog_empty, ids::LABELER_CATALOG_EMPTY);
    content.append(&catalog_empty);

    CatalogShell {
        content,
        catalog_list,
        catalog_empty,
        inspect,
        error_label: err,
    }
}

/// Build one `labeler-catalog-item` row. `show_inspect_subscribe` gates the
/// inspect + subscribe affordances (labeler-catalog page only — ui.yaml
/// `labeler-catalog-item`: "labeler-catalog-item-subscribe-button …
/// labeler-catalog only"); unsubscribe is always present so the
/// personalization home's subscribed-labelers facet can un-subscribe inline.
/// `error_label` is the page's `error-message`, where a refused press says why.
fn build_labeler_row(
    machine: &Arc<LabelerCatalogMachine>,
    runtime: &tokio::runtime::Handle,
    entry: &LabelerCatalogEntry,
    index: u32,
    show_inspect_subscribe: bool,
    error_label: &gtk::Label,
) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::LABELER_CATALOG_ITEM);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();

    let publisher = gtk::Label::new(Some(&entry.publisher_actor));
    publisher.set_halign(gtk::Align::Start);
    set_test_id(&publisher, ids::LABELER_CATALOG_ITEM_PUBLISHER);
    info.append(&publisher);

    // The artifact kind (`list` | `wasm` | `text-model`, normalized by the
    // machine) — a curated List is distinguishable from an executable module
    // BEFORE inspect (frame § Tier-3 artifact kinds). Rendered verbatim,
    // EXCEPT the one case a subscribed `text-model` whose tokenizer contract
    // this build does not implement: the compose seam leaves that factor
    // inert, and this row is where the user learns why. The predicate and
    // wording are both shared (`text_model_needs_newer_app`, over the same
    // `scoring::text_model_version_supported` the seam's inert branch
    // reads), so this badge cannot disagree with the scorer.
    let kind_text =
        crate::i18n::text_model_needs_newer_app(&entry.artifact_kind, entry.artifact_version)
            .unwrap_or_else(|| entry.artifact_kind.clone());
    let kind = gtk::Label::new(Some(&kind_text));
    kind.set_halign(gtk::Align::Start);
    set_test_id(&kind, ids::LABELER_CATALOG_ITEM_KIND);
    info.append(&kind);

    let content_kind = gtk::Label::new(Some(&entry.content_kind));
    content_kind.set_halign(gtk::Align::Start);
    set_test_id(&content_kind, ids::LABELER_CATALOG_ITEM_CONTENT_KIND);
    info.append(&content_kind);

    let version = gtk::Label::new(Some(&entry.version.to_string()));
    version.set_halign(gtk::Align::Start);
    set_test_id(&version, ids::LABELER_CATALOG_ITEM_VERSION);
    info.append(&version);

    let factor = gtk::Label::new(Some(&entry.factor));
    factor.set_halign(gtk::Align::Start);
    set_test_id(&factor, ids::LABELER_CATALOG_ITEM_FACTOR);
    info.append(&factor);

    row.append(&info);

    if show_inspect_subscribe {
        let inspect = gtk::Button::with_label(labeler_catalog::INSPECT);
        set_test_id(&inspect, ids::LABELER_CATALOG_ITEM_INSPECT_BUTTON);
        crate::offline_gate::declare_wire_kind(&inspect, "fauna.labelers.inspect");
        {
            let machine = Arc::clone(machine);
            let runtime = runtime.clone();
            inspect.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.inspect(index).await });
            });
        }
        row.append(&inspect);

        if !entry.subscribed {
            let subscribe = gtk::Button::with_label(labeler_catalog::SUBSCRIBE);
            subscribe.add_css_class("suggested-action");
            set_test_id(&subscribe, ids::LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON);
            // Disabled with the refusal's reason while the open inspect view
            // is THIS row's labeler and fails the shared `subscribable` gate —
            // the machine's one read (`subscribe_possibility`), painted before
            // the offline gate records the page's own sensitivity.
            crate::form_gate::paint_submit(&subscribe, &machine.subscribe_possibility(index), None);
            crate::offline_gate::declare_wire_kind(&subscribe, "fauna.labelers.subscribe");
            {
                let machine = Arc::clone(machine);
                let runtime = runtime.clone();
                let error_label = error_label.clone();
                subscribe.connect_clicked(move |_| {
                    // The row's answer, asked again: a labeler whose inspect
                    // already failed refuses with that reason (rule 6).
                    if crate::form_gate::refuse(&machine.subscribe_possibility(index), &error_label)
                    {
                        return;
                    }
                    let machine = Arc::clone(&machine);
                    runtime.spawn(async move { machine.subscribe(index).await });
                });
            }
            row.append(&subscribe);
        }
    }

    if entry.subscribed {
        let unsubscribe = gtk::Button::with_label(labeler_catalog::UNSUBSCRIBE);
        set_test_id(&unsubscribe, ids::LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON);
        crate::offline_gate::declare_wire_kind(&unsubscribe, "fauna.labelers.unsubscribe");
        {
            let machine = Arc::clone(machine);
            let runtime = runtime.clone();
            unsubscribe.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.unsubscribe(index).await });
            });
        }
        row.append(&unsubscribe);
    }

    row
}

/// Rebuild a `ListBox`'s rows from a filtered, indexed view of the catalog.
/// `indices` pairs each shown entry with its ORIGINAL snapshot index (the
/// machine's `inspect`/`subscribe`/`unsubscribe` gestures are index-addressed
/// into the full, unfiltered `entries`), so the personalization home's
/// subscribed-only filter doesn't scramble which row a gesture targets.
fn rebuild_rows(
    list: &gtk::ListBox,
    machine: &Arc<LabelerCatalogMachine>,
    runtime: &tokio::runtime::Handle,
    indices: &[(u32, &LabelerCatalogEntry)],
    show_inspect_subscribe: bool,
    error_label: &gtk::Label,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (index, entry) in indices {
        let row = build_labeler_row(
            machine,
            runtime,
            entry,
            *index,
            show_inspect_subscribe,
            error_label,
        );
        list.append(&row);
    }
}

/// Widgets + state the render loop rewrites on every observer tick.
struct RenderCtx {
    home: HomeShell,
    catalog: CatalogShell,
}

/// Everything the Layer-B signal-sharing pane's handlers + render need. Holds
/// cloned widget handles (GTK objects are refcounted, so this shares the same
/// widgets) plus the echo-suppression flag, so it lives independently of the
/// `RenderCtx` the labeler loop owns.
struct SignalShareCtx {
    toggle: gtk::Switch,
    toggle_reason: gtk::Label,
    clear_button: gtk::Button,
    clear_reason: gtk::Label,
    published_group: adw::PreferencesGroup,
    published_placeholder: adw::ActionRow,
    published_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
    error_label: gtk::Label,
    /// Set while `render_signal_share` programmatically updates the toggle, so its
    /// `active_notify` handler doesn't echo the change back as a `set`.
    syncing: Cell<bool>,
    /// Where the `signal_share.status` read stands — the toggle's answer waits
    /// on it (rule 5).
    share_read: Cell<ReadState>,
    /// The opt-in the nest last confirmed. The toggle writes its flip, never
    /// the switch's own (possibly guessed) position.
    confirmed: Cell<bool>,
    rt: tokio::runtime::Handle,
}

/// Whether the live feed manager both engagement-cue controls act through is
/// up.
fn manager_ready() -> bool {
    crate::feed::host::manager().is_some()
}

/// Repaint both engagement-cue controls from their shared answers.
fn paint_controls(ctx: &SignalShareCtx) {
    let ready = manager_ready();
    crate::possible::paint(
        &ctx.toggle,
        Some(&ctx.toggle_reason),
        share_signals_possibility(ready, ctx.share_read.get()),
    );
    crate::possible::paint(
        &ctx.clear_button,
        Some(&ctx.clear_reason),
        clear_engagement_possibility(ready),
    );
}

/// The toggle press: asks the answer the switch painted from again. Possible →
/// `Some(the flip of the nest-confirmed value)`, the one value it may write.
/// Otherwise the reason goes on the page's `error-message` and `None` comes
/// back — no guessed write, no silent return.
fn share_toggle_press(
    answer: &Possibility,
    confirmed: bool,
    error_label: &gtk::Label,
) -> Option<bool> {
    if crate::possible::refused_at_press(answer, error_label) {
        return None;
    }
    Some(!confirmed)
}

/// Put the switch back on the nest-confirmed value without echoing it as a
/// gesture.
fn resync_toggle(ctx: &SignalShareCtx) {
    let confirmed = ctx.confirmed.get();
    if ctx.toggle.is_active() != confirmed {
        ctx.syncing.set(true);
        ctx.toggle.set_active(confirmed);
        ctx.syncing.set(false);
    }
}

/// Wire the signal-sharing opt-in toggle + transparency pane over the shared
/// `FeedManager` (engagement-cues.md § Layer B). Mirrors the report-share pane in
/// `settings/mail_spam.rs`: hydrate on build + on page-visible, flip on toggle
/// (echo-suppressed), rebuild the published list from the nest-confirmed reply.
fn wire_signal_sharing(
    home: &HomeShell,
    fauna_client: &Rc<FaunaClient>,
    home_page: &gtk::ScrolledWindow,
) {
    let ctx = Rc::new(SignalShareCtx {
        toggle: home.share_signals_toggle.clone(),
        toggle_reason: home.share_signals_reason.clone(),
        clear_button: home.clear_engagement_data_button.clone(),
        clear_reason: home.clear_engagement_reason.clone(),
        published_group: home.signal_published_group.clone(),
        published_placeholder: home.signal_published_placeholder.clone(),
        published_rows: Rc::clone(&home.signal_published_rows),
        error_label: home.error_label.clone(),
        syncing: Cell::new(false),
        share_read: Cell::new(ReadState::Pending),
        confirmed: Cell::new(false),
        rt: fauna_client.runtime_handle(),
    });

    hydrate_signal_share(&ctx);

    // Toggle → FeedManager::set_signal_sharing (skip the echo from
    // render_signal_share). A false→true flip opts in; true→false ALSO withdraws
    // every signal this actor contributed (the nest opt-out sweep), so the
    // published list may shrink on the re-read.
    {
        let ctx = Rc::clone(&ctx);
        home.share_signals_toggle.connect_active_notify(move |_| {
            if ctx.syncing.get() {
                return;
            }
            // The answer the switch painted from, asked again: no manager, or
            // an unread opt-in, refuses on `error-message` and puts the switch
            // back; a possible press writes the flip of the confirmed value.
            let answer = share_signals_possibility(manager_ready(), ctx.share_read.get());
            match share_toggle_press(&answer, ctx.confirmed.get(), &ctx.error_label) {
                Some(share) => set_signal_share(&ctx, share),
                None => {
                    resync_toggle(&ctx);
                    paint_controls(&ctx);
                }
            }
        });
    }

    // Refresh on page-visible: the opt-in + published list change out-of-band
    // (another user on this nest crossing k; this actor opting out on another
    // device), and the manager may not exist yet at build time. Mirrors the
    // labeler/trained refresh-on-map.
    {
        let ctx = Rc::clone(&ctx);
        home_page.connect_map(move |_| {
            // The manager may have come up since the last paint.
            paint_controls(&ctx);
            hydrate_signal_share(&ctx);
        });
    }
}

/// Read the signal opt-in + published list over the shared `FeedManager`
/// (`crate::feed::host::manager()` — the same accessor the clear-activity-data
/// button uses) and render it. `None` (pre-auth: the page mounts at app init)
/// is a no-op — the page-visible refresh re-hydrates once the manager exists.
/// `FeedManager::signal_share_status` is a single NestClient RPC (plus a
/// trivial local cache write) — the transport already parks it while the
/// socket comes up (transport.md § Request lifecycle step 3).
fn hydrate_signal_share(ctx: &Rc<SignalShareCtx>) {
    let Some(manager) = crate::feed::host::manager() else {
        return;
    };
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { manager.signal_share_status().await },
        move |res| render_signal_share(&ctx_render, res),
    );
}

/// Set the opt-in (`FeedManager::set_signal_sharing` → `signal_share.set`), then
/// re-read status so the toggle + published list reflect the persisted value
/// (opting out withdraws this actor's signals, which may shrink the list).
fn set_signal_share(ctx: &Rc<SignalShareCtx>, share: bool) {
    // The press asked the answer; the manager can only have gone since then,
    // which is the same refusal.
    let Some(manager) = crate::feed::host::manager() else {
        let answer = share_signals_possibility(false, ctx.share_read.get());
        crate::possible::refused_at_press(&answer, &ctx.error_label);
        resync_toggle(ctx);
        paint_controls(ctx);
        return;
    };
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { manager.set_signal_sharing(share).await },
        move |res| render_signal_share(&ctx_render, res),
    );
}

/// Render a signal-share status reply: reflect the opt-in toggle (echo-suppressed)
/// and rebuild the published list. An error surfaces via the page `error-message`.
/// Delegates to the shared `render_share_pane` (round 195 of the shared-Rust
/// harvest sweep).
fn render_signal_share(
    ctx: &Rc<SignalShareCtx>,
    res: Result<ModerationSignalShareStatusReply, String>,
) {
    ctx.share_read.set(ctx.share_read.get().after(res.is_ok()));
    if let Ok(reply) = &res {
        ctx.confirmed.set(reply.share);
    }
    crate::settings::render_share_pane(
        crate::settings::SharePaneWidgets {
            toggle: &ctx.toggle,
            syncing: &ctx.syncing,
            error_label: &ctx.error_label,
            published_group: &ctx.published_group,
            published_rows: &ctx.published_rows,
            published_placeholder: &ctx.published_placeholder,
        },
        "signal-share",
        personalization::SIGNAL_PUBLISHED_CONTRIBUTORS,
        res.map(|s| (s.share, s.published)),
    );
    // A failed write leaves the switch where the press put it; it shows the
    // confirmed value instead (a success already synced it).
    resync_toggle(ctx);
    paint_controls(ctx);
}

/// Build the **Personalization** home + **Community labelers** catalog
/// sub-pages over one shared `LabelerCatalogMachine`. Returns
/// `(home_page, catalog_page, handles)` — each page is a `gtk::ScrolledWindow`
/// the settings shell adds to its sub-stack. `on_navigate_to_feed` exits
/// Settings to the top-level Feed page (the "Feeds" facet link);
/// `on_navigate_to_muted_words` / `on_navigate_to_catalog` switch the
/// SETTINGS sub-stack to the already-shipped `muted-words` sub-page / this
/// module's own `labeler-catalog` sub-page (both same-stack, wired by the
/// caller since only it holds the settings sub-stack at construction time).
#[allow(clippy::type_complexity)]
pub fn build_personalization_and_catalog_pages(
    fauna_client: &Rc<FaunaClient>,
    on_navigate_to_feed: Rc<dyn Fn()>,
    on_navigate_to_muted_words: Rc<dyn Fn()>,
    on_navigate_to_catalog: Rc<dyn Fn()>,
) -> (
    gtk::ScrolledWindow,
    gtk::ScrolledWindow,
    PersonalizationHandles,
) {
    let (home, trained_widgets) = build_home_shell();
    let catalog = build_catalog_shell();

    let home_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&home.content)
        .build();
    let catalog_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&catalog.content)
        .build();

    home.feeds_link
        .connect_clicked(move |_| on_navigate_to_feed());
    home.muted_words_link
        .connect_clicked(move |_| on_navigate_to_muted_words());
    home.browse_catalog_button
        .connect_clicked(move |_| on_navigate_to_catalog());

    // Clear activity data → FeedManager::delete_cue_rollup (model.delete on
    // cues:v1 + live-engine reset). The press asks the button's answer again:
    // with no live manager it refuses with the feed-not-ready reason on the
    // page's error element, never a bare return.
    {
        let error_label = home.error_label.clone();
        let runtime = fauna_client.runtime_handle();
        home.clear_engagement_data_button.connect_clicked(move |_| {
            let manager = crate::feed::host::manager();
            let answer = clear_engagement_possibility(manager.is_some());
            if crate::possible::refused_at_press(&answer, &error_label) {
                return;
            }
            let Some(manager) = manager else {
                return;
            };
            let error_label = error_label.clone();
            crate::async_helper::spawn_with_snapshot(
                &runtime,
                move || async move { manager.delete_cue_rollup().await },
                move |result: Result<(), String>| {
                    crate::settings::render_error_label(&error_label, result.err().as_deref());
                },
            );
        });
    }

    // Layer-B signal-sharing pane: hydrate the opt-in + published list, flip on
    // toggle, refresh on page-visible — all over the shared FeedManager (the same
    // accessor the clear button uses).
    wire_signal_sharing(&home, fauna_client, &home_page);

    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn LabelerCatalogObserver> = Arc::new(GtkLabelerCatalogObserver { tx });
    let machine = build_machine(fauna_client, observer);

    {
        let machine = Arc::clone(&machine);
        let runtime = fauna_client.runtime_handle();
        catalog.inspect.close_button.connect_clicked(move |_| {
            machine.close_inspect();
            let _ = &runtime;
        });
    }

    // ── Render loop ─────────────────────────────────────────────────────
    let ctx = Rc::new(RenderCtx { home, catalog });
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let runtime = fauna_client.runtime_handle();
        crate::async_helper::spawn_wake_loop(rx, move || {
            render_pages(&machine, &ctx, &runtime);
            glib::ControlFlow::Continue
        });
    }

    // Refresh the machine off WS-RPC whenever either sub-page becomes visible
    // (mirrors `devices_folders`'s `connect_map` refresh).
    for page in [&home_page, &catalog_page] {
        let machine = Arc::clone(&machine);
        let handle = fauna_client.runtime_handle();
        page.connect_map(move |_| {
            let machine = Arc::clone(&machine);
            handle.spawn(async move { machine.refresh().await });
        });
    }

    // Trained topics facet: sequenced account-store/PersonalizationClient calls
    // (no machine — the muted-words shape), refreshed on the same
    // visible-trigger so the registry + example counts stay current.
    let trained = trained_topics::wire(fauna_client, trained_widgets, ctx.home.error_label.clone());
    let trained_topics_refresh: Rc<dyn Fn()> = Rc::new(move || trained.refresh());
    {
        let refresh = Rc::clone(&trained_topics_refresh);
        home_page.connect_map(move |_| refresh());
    }

    let handles = PersonalizationHandles {
        labeler_catalog_machine: Arc::clone(&machine),
        trained_topics_refresh,
    };
    (home_page, catalog_page, handles)
}

/// Re-render both sub-pages off a fresh `LabelerCatalogSnapshot`.
fn render_pages(
    machine: &Arc<LabelerCatalogMachine>,
    ctx: &Rc<RenderCtx>,
    runtime: &tokio::runtime::Handle,
) {
    let snap = machine.snapshot();

    // Personalization home: only the caller's SUBSCRIBED labelers, indices
    // preserved into the full `entries` so gestures target the right row.
    let subscribed: Vec<(u32, &LabelerCatalogEntry)> = snap
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.subscribed)
        .map(|(i, e)| (i as u32, e))
        .collect();
    rebuild_rows(
        &ctx.home.labelers_list,
        machine,
        runtime,
        &subscribed,
        false,
        &ctx.home.error_label,
    );
    // `personalization-labelers-empty` paints only once a refresh has
    // actually RETURNED — an empty `subscribed` list is indistinguishable
    // from "still loading" without `loaded` (`LabelerCatalogSnapshot::loaded`).
    ctx.home
        .labelers_empty
        .set_visible(snap.loaded && subscribed.is_empty());

    // Catalog page: every published labeler.
    let all: Vec<(u32, &LabelerCatalogEntry)> = snap
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (i as u32, e))
        .collect();
    rebuild_rows(
        &ctx.catalog.catalog_list,
        machine,
        runtime,
        &all,
        true,
        &ctx.catalog.error_label,
    );
    // Same `loaded` gate as the home page's empty state above.
    ctx.catalog
        .catalog_empty
        .set_visible(snap.loaded && all.is_empty());

    ctx.catalog.inspect.render(snap.inspecting.as_ref());

    let text = snap.error.as_ref().map(|err| err.resolve(strings::lookup));
    crate::settings::render_error_label(&ctx.home.error_label, text.as_deref());
    crate::settings::render_error_label(&ctx.catalog.error_label, text.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    #[test]
    fn personalization_home_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (shell, _trained) = build_home_shell();
            let names = widget_names(&shell.content);
            for id in [
                "page-heading",
                "error-message",
                "personalization-feeds-link",
                "personalization-muted-words-link",
                "personalization-labelers-list",
                "personalization-labelers-empty",
                "personalization-browse-catalog-button",
                "personalization-trained-factor-list",
                "personalization-trained-factor-create-button",
                "personalization-trained-factor-name-input",
                "personalization-clear-engagement-data-button",
                "personalization-share-signals-toggle",
                "signal-share-published-list",
                // The publish review-prune sheet's static family (the per-row
                // publish-button and the per-exemplar rows are indexed, built per
                // render, so they are not static-tree IDs).
                "personalization-trained-factor-publish-sheet",
                "personalization-trained-factor-publish-name-input",
                "personalization-trained-factor-publish-limitation-note",
                "personalization-trained-factor-publish-exemplar-list",
                "personalization-trained-factor-publish-exemplar-empty",
                "personalization-trained-factor-publish-submit-button",
                "personalization-trained-factor-publish-cancel-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }

    /// Rules 5 and 7 on the engagement-cue controls (`engagement-cues.md`
    /// § Layer B): with no live feed manager both are painted disabled with
    /// the feed-not-ready reason beside them, and a press refuses on
    /// `error-message` instead of writing a guess or returning without a word.
    #[test]
    fn engagement_controls_wait_for_the_manager_and_the_read() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let feed_not_ready = strings::errors::FEED_NOT_READY;
            // No manager in a unit test: both controls paint blocked.
            assert!(crate::feed::host::manager().is_none());
            let (shell, _trained) = build_home_shell();
            for (widget, line) in [
                (
                    shell.share_signals_toggle.upcast_ref::<gtk::Widget>(),
                    &shell.share_signals_reason,
                ),
                (
                    shell
                        .clear_engagement_data_button
                        .upcast_ref::<gtk::Widget>(),
                    &shell.clear_engagement_reason,
                ),
            ] {
                assert!(widget.is_visible() && !widget.is_sensitive());
                assert!(line.is_visible());
                assert_eq!(line.text().as_str(), feed_not_ready);
            }

            let error = gtk::Label::new(None);
            // The toggle press, manager down: refused, said.
            let answer = share_signals_possibility(false, ReadState::Resolved);
            assert_eq!(share_toggle_press(&answer, false, &error), None);
            assert!(error.is_visible());
            assert_eq!(error.text().as_str(), feed_not_ready);
            // Manager up, opt-in unread: refused as still loading.
            let answer = share_signals_possibility(true, ReadState::Pending);
            assert_eq!(share_toggle_press(&answer, false, &error), None);
            assert_eq!(error.text().as_str(), strings::common::STILL_LOADING);
            // Read: the press writes the flip of the CONFIRMED value.
            let answer = share_signals_possibility(true, ReadState::Resolved);
            assert_eq!(share_toggle_press(&answer, true, &error), Some(false));
            assert_eq!(share_toggle_press(&answer, false, &error), Some(true));

            // The clear press, manager down: refused with feed-not-ready.
            let error = gtk::Label::new(None);
            assert!(crate::possible::refused_at_press(
                &clear_engagement_possibility(false),
                &error
            ));
            assert_eq!(error.text().as_str(), feed_not_ready);
            assert!(!crate::possible::refused_at_press(
                &clear_engagement_possibility(true),
                &error
            ));
        });
    }

    #[test]
    fn labeler_catalog_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let shell = build_catalog_shell();
            let names = widget_names(&shell.content);
            for id in [
                "page-heading",
                "error-message",
                "labeler-catalog-list",
                "labeler-catalog-empty",
                "labeler-inspect-panel",
                "labeler-inspect-metadata",
                "labeler-inspect-close-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
