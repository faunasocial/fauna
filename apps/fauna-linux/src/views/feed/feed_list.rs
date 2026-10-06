use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tokio::runtime::Handle;

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use crate::i18n::strings::{common, feed};
use fauna_client_labelers::LabelersClient;
use fauna_feed::{
    AvailableBridge, BridgeFeedView, FactorWeightInput, FeedSummaryView, FilterRuleInput,
};

// The rule-builder's type catalog, its input-kind classification, the added-rule
// chip text, and the required/excluded toggle label all come from the shared
// `fauna_client_feed` (feed.md § Where logic lives) — the local `RULE_VARIANTS`
// table and the `is_label`-only visibility rule are gone. Don't re-add either:
// the catalog derives beside `build_filter_rule`, so the form can't offer an
// input the encoder ignores.

/// Handles returned from `build_feed_list_with_boxes` for runtime population.
pub struct FeedListHandles {
    /// The single-row list holding the built-in **Trending** virtual-feed entry
    /// (`feed-trending-item`, above the user's own feeds — `trending.md` § The
    /// Trending feed). Its own `ListBox` (not a row of `feeds_list`) so the
    /// virtual feed never mixes into the custom-feed rows (no delete button, no
    /// "no feeds configured" placeholder, no feed-id index mapping); mutual
    /// selection with `feeds_list` is coordinated in `build_feed_view`.
    pub trending_list: gtk::ListBox,
    pub feeds_list: gtk::ListBox,
    pub bridge_list: gtk::ListBox,
    /// The "subscribe to bridge feed" button — shown only when the nest supports
    /// at least one bridge (`snapshot.available_bridges` non-empty; Dim 3 gating).
    pub subscribe_btn: gtk::Button,
}

/// Build the feed list pane with live handles and wired buttons (New Feed /
/// Subscribe). The feed *rows* are populated by `render_feeds` /
/// `render_bridge_feeds` from the snapshot.
pub fn build_feed_list_with_boxes(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) -> (gtk::Box, FeedListHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Header bar.
    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(feed::list::TITLE));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));

    let new_feed_btn = gtk::Button::with_label(feed::create::TITLE);
    new_feed_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&new_feed_btn, ids::FEED_CREATE_FEED_BUTTON);
    header.pack_end(&new_feed_btn);

    outer.append(&header);

    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt = rt.clone();
        new_feed_btn.connect_clicked(move |btn| {
            let dialog = build_new_feed_dialog(&m, &c, &rt);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    let scroll_content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // --- Trending (built-in virtual feed, above the user's own feeds) ---
    // trending.md § The Trending feed: a pseudo-entry driven through the shared
    // FeedManager (`select_trending_feed` → `fauna.feed.trending.posts`). Its own
    // single-row ListBox so it never mixes into the custom-feed rows.
    let trending_list = gtk::ListBox::new();
    trending_list.set_selection_mode(gtk::SelectionMode::Single);
    trending_list.add_css_class("boxed-list");
    trending_list.set_margin_start(8);
    trending_list.set_margin_end(8);
    trending_list.set_margin_top(12);
    trending_list.append(&build_trending_row());
    scroll_content.append(&trending_list);

    // --- Feeds section ---
    let feeds_label = gtk::Label::new(Some(feed::list::TITLE));
    feeds_label.set_halign(gtk::Align::Start);
    feeds_label.add_css_class("heading");
    feeds_label.set_margin_start(12);
    feeds_label.set_margin_top(12);
    feeds_label.set_margin_bottom(4);
    scroll_content.append(&feeds_label);

    let feeds_list = gtk::ListBox::new();
    feeds_list.set_selection_mode(gtk::SelectionMode::Single);
    feeds_list.add_css_class("boxed-list");
    feeds_list.set_margin_start(8);
    feeds_list.set_margin_end(8);

    let feeds_placeholder = gtk::Label::new(Some(feed::post::NO_FEEDS_CONFIGURED));
    feeds_placeholder.add_css_class("dim-label");
    feeds_placeholder.set_margin_top(8);
    feeds_placeholder.set_margin_bottom(8);
    feeds_list.set_placeholder(Some(&feeds_placeholder));

    scroll_content.append(&feeds_list);

    // --- Bridge Feeds section ---
    let bridge_label = gtk::Label::new(Some(feed::list::BRIDGE_FEEDS));
    bridge_label.set_halign(gtk::Align::Start);
    bridge_label.add_css_class("heading");
    bridge_label.set_margin_start(12);
    bridge_label.set_margin_top(16);
    bridge_label.set_margin_bottom(4);
    scroll_content.append(&bridge_label);

    let bridge_list = gtk::ListBox::new();
    bridge_list.set_selection_mode(gtk::SelectionMode::Single);
    bridge_list.add_css_class("boxed-list");
    bridge_list.set_margin_start(8);
    bridge_list.set_margin_end(8);
    bridge_list.set_margin_bottom(8);

    let bridge_placeholder = gtk::Label::new(Some(feed::post::NO_BRIDGE_FEEDS));
    bridge_placeholder.add_css_class("dim-label");
    bridge_placeholder.set_margin_top(8);
    bridge_placeholder.set_margin_bottom(8);
    bridge_list.set_placeholder(Some(&bridge_placeholder));

    scroll_content.append(&bridge_list);

    // Subscribe button at bottom of bridge section.
    let subscribe_btn = gtk::Button::with_label(feed::list::SUBSCRIBE_BRIDGE);
    crate::testid::set_test_id(&subscribe_btn, ids::BRIDGE_FEED_SUBSCRIBE_TOGGLE);
    subscribe_btn.set_margin_start(8);
    subscribe_btn.set_margin_end(8);
    subscribe_btn.set_margin_bottom(8);

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        subscribe_btn.connect_clicked(move |btn| {
            let dialog = build_subscribe_bridge_dialog(&m, &rt);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    scroll_content.append(&subscribe_btn);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_content)
        .build();

    outer.append(&scrolled);

    let handles = FeedListHandles {
        trending_list,
        feeds_list,
        bridge_list,
        subscribe_btn,
    };
    (outer, handles)
}

/// Build the built-in **Trending** selector row (`feed-trending-item`,
/// `trending.md` § The Trending feed) — a label-only pseudo-entry (no delete
/// button; the virtual feed has no feed row). Selection drives
/// `FeedManager::select_trending_feed` in `build_feed_view`.
fn build_trending_row() -> gtk::ListBoxRow {
    let name_label = gtk::Label::new(Some(feed::list::TRENDING));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_hexpand(true);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&name_label);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    crate::testid::set_test_id(&row, ids::FEED_TRENDING_ITEM);
    row
}

/// Re-render the feed selector rows from `snapshot.feeds`.
pub fn render_feeds(
    feeds_list: &gtk::ListBox,
    feeds: &[FeedSummaryView],
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
) {
    while let Some(child) = feeds_list.first_child() {
        feeds_list.remove(&child);
    }
    for feed in feeds {
        feeds_list.append(&build_feed_row(&feed.feed_id, &feed.name, manager, rt));
    }
}

/// Re-render the subscribed bridge-feed rows from `snapshot.bridge_feeds`.
pub fn render_bridge_feeds(
    bridge_list: &gtk::ListBox,
    bridge_feeds: &[BridgeFeedView],
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
) {
    while let Some(child) = bridge_list.first_child() {
        bridge_list.remove(&child);
    }
    for sub in bridge_feeds {
        bridge_list.append(&build_bridge_feed_row(sub.id, &sub.name, manager, rt));
    }
}

/// Build a feed row (`feed-item`) with a Delete button → `delete_feed`.
fn build_feed_row(
    feed_id: &str,
    name: &str,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
) -> gtk::ListBoxRow {
    let name_label = gtk::Label::new(Some(name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_hexpand(true);

    let delete_btn = gtk::Button::with_label(common::DELETE);
    delete_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&delete_btn, ids::FEED_DELETE_BUTTON);
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.feed.delete");

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&name_label);
    hbox.append(&delete_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    // `set_test_id` owns the row's widget_name ("feed-item" — the finder
    // matches widget_name only); selection resolves the feed id by row index
    // into `snapshot.feeds` (views/feed/mod.rs), never a name read. A prior
    // `set_widget_name(feed_id)` here was dead the moment set_test_id ran.
    crate::testid::set_test_id(&row, ids::FEED_ITEM);

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        let fid = feed_id.to_string();
        delete_btn.connect_clicked(move |_| {
            let m = Arc::clone(&m);
            let fid = fid.clone();
            rt.spawn(async move {
                let _ = m.delete_feed(fid).await;
            });
        });
    }

    row
}

/// Build a bridge feed row with an Unsubscribe button → `unsubscribe_bridge`.
fn build_bridge_feed_row(
    id: i64,
    name: &str,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
) -> gtk::ListBoxRow {
    let name_label = gtk::Label::new(Some(name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_hexpand(true);

    let unsub_btn = gtk::Button::with_label(common::UNFOLLOW);
    unsub_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&unsub_btn, ids::BRIDGE_FEED_UNSUBSCRIBE_BUTTON);
    crate::offline_gate::declare_wire_kind(&unsub_btn, "fauna.bridges.feeds.delete");

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&name_label);
    hbox.append(&unsub_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_widget_name(&id.to_string());

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        unsub_btn.connect_clicked(move |_| {
            let m = Arc::clone(&m);
            rt.spawn(async move {
                let _ = m.unsubscribe_bridge(id).await;
            });
        });
    }

    row
}

/// The two `feed-combination-select` wire values — what `create_feed` carries
/// and what the driver selects by (`feed.md` § create_feed).
const COMBINATION_ALL: &str = "all";
const COMBINATION_ANY: &str = "any";

/// Build `feed-rule-type-select`: the model holds the **wire values** (the
/// `FilterRule` variant names from the shared catalog) while a display
/// `ClosureExpression` renders each one's localized label — the
/// stable-key-vs-display split, mirroring web's
/// `<option value="LabelBelow">Label Below (exclude spam)</option>` and tui's
/// `SelectTarget::RuleType`. `feed.md` § Where logic lives states the contract:
/// *"The select's value stays the `FilterRule` variant name … only the label is
/// localized."*
///
/// **A label-only model silently breaks `select()` for three of the 11 types.**
/// The agent resolves a driver's value through
/// `fauna_e2e_agent::select_match`, which matches exactly, then on lowercased
/// alphanumerics of both sides — so a label can only ever be found when it is
/// the variant name respaced. `"Body contains"` qualifies; `"Protocol Source"`,
/// `"Label Below (exclude spam)"` and `"Label Above (show only)"` cannot, and
/// refused `409` (`test_create_feed_label_below_rule[linux]` standing red;
/// `Source` was silently exposed too, having no test that drives it). No
/// loosening of the matcher could fix this — nothing derives `Source` from
/// `Protocol Source` — which is exactly why the value must ride the model.
/// Pinned by `every_rule_type_value_resolves_through_the_agent_lookup`.
///
/// The model order stays the catalog's, so every `selected()`-indexed lookup
/// against `rule_types` below is unaffected.
fn build_rule_type_dropdown(options: &[fauna_client_feed::RuleTypeOption]) -> gtk::DropDown {
    let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
    let dropdown = gtk::DropDown::new(Some(gtk::StringList::new(&values)), gtk::Expression::NONE);
    let labels: HashMap<String, String> = options
        .iter()
        .map(|o| {
            (
                o.value.clone(),
                o.label.resolve(crate::i18n::strings::lookup),
            )
        })
        .collect();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        glib::closure_local!(move |item: gtk::StringObject| {
            let value = item.string().to_string();
            labels.get(&value).cloned().unwrap_or(value)
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    dropdown
}

/// Build `feed-combination-select` on the same split: the model holds the two
/// **wire values** `create_feed` encodes (`"all"` / `"any"` — what
/// `actions/feed.py` drives and what tui offers), painted as the localized
/// "All match" / "Any match". The label-only model this replaced resolved
/// *neither* value (`"all"` vs `allmatch`), so a non-default combination was
/// unselectable on linux — latent only because no test drives one yet.
/// Pinned by `both_combination_values_resolve_through_the_agent_lookup`.
fn build_combination_dropdown() -> gtk::DropDown {
    let dropdown = gtk::DropDown::new(
        Some(gtk::StringList::new(&[COMBINATION_ALL, COMBINATION_ANY])),
        gtk::Expression::NONE,
    );
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        glib::closure_local!(move |item: gtk::StringObject| {
            match item.string().as_str() {
                COMBINATION_ANY => feed::create::MODE_ANY.to_string(),
                _ => feed::create::MODE_ALL.to_string(),
            }
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    dropdown
}

/// Build a "New Feed" dialog with the canonical rule builder: name, a
/// combination mode, and a repeatable rule row (type + value + required toggle +
/// add). Accumulated rules ride `FeedManager::create_feed` as `FilterRuleInput`s
/// (the manager encodes them via the shared `encode_filter_rule`). Also hosts
/// the factor-weight editor (`feed-factor-*`, content-moderation-and-
/// ranking.md § Composition; UI approved 2026-07-08) — a second repeatable
/// row (factor + decimal weight + apply-to-all-feeds toggle + add) whose
/// accumulated `FactorWeightInput`s ride the same `create_feed` call.
fn build_new_feed_dialog(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(feed::create::TITLE)
        .modal(true)
        .default_width(400)
        .build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    // ── Feed name ────────────────────────────────────────────────────────
    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some(feed::create::NAME_PLACEHOLDER));
    crate::testid::set_test_id(&name_entry, ids::FEED_CREATE_FEED_NAME);

    // ── Combination mode (All match / Any match) ─────────────────────────
    let combo = build_combination_dropdown();
    crate::testid::set_test_id(&combo, ids::FEED_COMBINATION_SELECT);

    // ── Rule builder ─────────────────────────────────────────────────────
    // The shared catalog: wire value + localized label + input kind, per type.
    let rule_types = Rc::new(fauna_client_feed::rule_type_options());
    let rule_type_dropdown = build_rule_type_dropdown(&rule_types);
    crate::testid::set_test_id(&rule_type_dropdown, ids::FEED_RULE_TYPE_SELECT);

    let rule_value_entry = gtk::Entry::new();
    rule_value_entry.set_placeholder_text(Some(feed::create::RULE_VALUE_PLACEHOLDER));
    rule_value_entry.set_hexpand(true);
    crate::testid::set_test_id(&rule_value_entry, ids::FEED_RULE_VALUE_INPUT);

    // Threshold input for the label rules (LabelBelow / LabelAbove): a 0–10
    // confidence packed with the category into the value as `"category:threshold"`
    // on Add. Defaults to the "5" midpoint.
    let rule_threshold_entry = gtk::Entry::new();
    rule_threshold_entry.set_placeholder_text(Some(feed::create::RULE_THRESHOLD));
    rule_threshold_entry.set_text(fauna_client_feed::DEFAULT_RULE_THRESHOLD);
    crate::testid::set_test_id(&rule_threshold_entry, ids::FEED_RULE_THRESHOLD_INPUT);

    // Required/excluded checkbox for the boolean rule types (HasMedia, IsReply).
    // The label flips with the state: `required:false` is a real exclusion on the
    // nest ("must NOT have media"), so a static "Required" would read as the
    // opposite of the rule being built (ui.yaml — "Required/excluded toggle").
    let rule_required_toggle = gtk::CheckButton::with_label(
        &fauna_client_feed::rule_required_label(true).resolve(crate::i18n::strings::lookup),
    );
    rule_required_toggle.set_active(true);
    crate::testid::set_test_id(&rule_required_toggle, ids::FEED_RULE_REQUIRED_TOGGLE);
    rule_required_toggle.connect_toggled(|t| {
        t.set_label(Some(
            &fauna_client_feed::rule_required_label(t.is_active())
                .resolve(crate::i18n::strings::lookup),
        ));
    });

    let add_rule_btn = gtk::Button::with_label(feed::create::ADD_RULE);
    crate::testid::set_test_id(&add_rule_btn, ids::FEED_ADD_RULE_BUTTON);

    let rule_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rule_row.append(&rule_type_dropdown);
    rule_row.append(&rule_value_entry);
    rule_row.append(&rule_threshold_entry);
    rule_row.append(&rule_required_toggle);
    rule_row.append(&add_rule_btn);

    // Show only the inputs the selected type's encoder arm actually reads
    // (`RuleInputKind`): the toggles ignore `value`, and only the label rules
    // take a threshold. Previously every type showed the value entry AND the
    // required checkbox, so e.g. "Min Replies" offered a "Required" box the
    // encoder discards.
    let apply_input_kind = {
        let rule_types = Rc::clone(&rule_types);
        let value = rule_value_entry.clone();
        let threshold = rule_threshold_entry.clone();
        let toggle = rule_required_toggle.clone();
        move |selected: usize| {
            let kind = rule_types
                .get(selected)
                .map(|o| o.input_kind)
                .unwrap_or(fauna_client_feed::RuleInputKind::Text);
            value.set_visible(kind != fauna_client_feed::RuleInputKind::Toggle);
            threshold.set_visible(kind == fauna_client_feed::RuleInputKind::TextAndNumber);
            toggle.set_visible(kind == fauna_client_feed::RuleInputKind::Toggle);
        }
    };
    apply_input_kind(rule_type_dropdown.selected() as usize);
    rule_type_dropdown.connect_selected_notify(move |dd| apply_input_kind(dd.selected() as usize));

    // `feed-add-rule-button` gates on `fauna_client_feed::can_add_rule` — apple's
    // `FeedCreateForm.canAddRule`, lifted (feed.md § Add-rule gating). linux
    // rendered this button permanently sensitive until this fix, so a user could
    // stage an empty/unparseable rule that `build_filter_rule` then silently
    // papered over. Recomputed on every keystroke and on every type switch.
    let update_sensitivity = {
        let rule_types = Rc::clone(&rule_types);
        let dropdown = rule_type_dropdown.clone();
        let value = rule_value_entry.clone();
        let threshold = rule_threshold_entry.clone();
        let btn = add_rule_btn.clone();
        move || {
            let kind = rule_types
                .get(dropdown.selected() as usize)
                .map(|o| o.input_kind)
                .unwrap_or(fauna_client_feed::RuleInputKind::Text);
            btn.set_sensitive(fauna_client_feed::can_add_rule(
                kind,
                &value.text(),
                &threshold.text(),
            ));
        }
    };
    update_sensitivity();
    rule_type_dropdown.connect_selected_notify({
        let update_sensitivity = update_sensitivity.clone();
        move |_| update_sensitivity()
    });
    rule_value_entry.connect_changed({
        let update_sensitivity = update_sensitivity.clone();
        move |_| update_sensitivity()
    });
    rule_threshold_entry.connect_changed(move |_| update_sensitivity());

    // Accumulated rules (`FilterRuleInput`) + a running summary.
    let rules: Rc<RefCell<Vec<FilterRuleInput>>> = Rc::new(RefCell::new(Vec::new()));
    let rules_summary = gtk::Label::new(None);
    rules_summary.set_halign(gtk::Align::Start);
    rules_summary.set_wrap(true);
    rules_summary.add_css_class("dim-label");
    rules_summary.add_css_class("caption");

    {
        let rules = Rc::clone(&rules);
        let rule_types = Rc::clone(&rule_types);
        let dropdown = rule_type_dropdown.clone();
        let value = rule_value_entry.clone();
        let threshold = rule_threshold_entry.clone();
        let toggle = rule_required_toggle.clone();
        let summary = rules_summary.clone();
        add_rule_btn.connect_clicked(move |_| {
            let idx = dropdown.selected() as usize;
            let selected = rule_types.get(idx);
            let variant = selected
                .map(|o| o.value.as_str())
                .unwrap_or("HasHashtag")
                .to_string();
            // The label rules pack two inputs (category + threshold) into the
            // single value string the shared encoder splits on `:`.
            let raw = if selected.map(|o| o.input_kind)
                == Some(fauna_client_feed::RuleInputKind::TextAndNumber)
            {
                format!("{}:{}", value.text(), threshold.text())
            } else {
                value.text().to_string()
            };
            rules.borrow_mut().push(FilterRuleInput {
                rule_type: variant,
                value: raw,
                required: toggle.is_active(),
            });
            value.set_text("");
            threshold.set_text(fauna_client_feed::DEFAULT_RULE_THRESHOLD);
            // Each chip is the shared `rule_summary_label` — feed.md's
            // Example-display prose (`#rust, #fauna`, `media: yes`), not the raw
            // `rule_type` wire key this line used to join.
            let added: Vec<String> = rules
                .borrow()
                .iter()
                .map(|r| {
                    fauna_client_feed::rule_summary_label(&r.rule_type, &r.value, r.required)
                        .resolve(crate::i18n::strings::lookup)
                })
                .collect();
            summary.set_text(&format!(
                "{}: {}",
                feed::create::FILTER_RULES,
                added.join(", ")
            ));
        });
    }

    // ── Factor-weight editor (content-moderation-and-ranking.md §
    // Composition) ─────────────────────────────────────────────────────────
    // The picker starts with the shared built-ins
    // (`fauna_client_feed::builtin_factor_options` — engagement, trending;
    // feed.md § Where logic lives → Feed factor-picker built-ins) and grows once
    // the caller's subscribed-labeler fetch + sealed trained-factor registry
    // resolve. The model holds STABLE KEYS ("engagement" / "trending" /
    // "labeler:<hex>" / "topic:<hex>") — the cross-app `select(id, value)`
    // contract — while the closure expression renders the display label (the
    // sync-default-conflict-policy-select precedent): the localized label for a
    // built-in, the user's chosen name for a trained topic
    // (topic-factors.md § Authoring surface — the stable-key-vs-display split),
    // and the raw factor id for labelers (no display name exists anywhere).
    // `factor_keys` mirrors the model order 1:1 so the selected index maps
    // back to the factor string on Add.
    let builtins = fauna_client_feed::builtin_factor_options();
    let builtin_keys: Vec<String> = builtins.iter().map(|o| o.value.clone()).collect();
    let factor_names: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(
        builtins
            .into_iter()
            .map(|o| (o.value, o.label.resolve(crate::i18n::strings::lookup)))
            .collect(),
    ));
    let factor_model =
        gtk::StringList::new(&builtin_keys.iter().map(String::as_str).collect::<Vec<_>>());
    let factor_keys: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(builtin_keys));
    let factor_dropdown = gtk::DropDown::new(Some(factor_model.clone()), gtk::Expression::NONE);
    {
        let factor_names = Rc::clone(&factor_names);
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            glib::closure_local!(move |item: gtk::StringObject| {
                let key = item.string().to_string();
                factor_names.borrow().get(&key).cloned().unwrap_or(key)
            }),
        );
        factor_dropdown.set_expression(Some(&label_expr));
    }
    crate::testid::set_test_id(&factor_dropdown, ids::FEED_FACTOR_SELECT);

    let factor_weight_entry = gtk::Entry::new();
    factor_weight_entry.set_placeholder_text(Some(feed::create::FACTOR_WEIGHT_PLACEHOLDER));
    factor_weight_entry.set_text("1.0");
    crate::testid::set_test_id(&factor_weight_entry, ids::FEED_FACTOR_WEIGHT_INPUT);

    let factor_global_toggle = gtk::CheckButton::with_label(feed::create::FACTOR_GLOBAL_TOGGLE);
    crate::testid::set_test_id(&factor_global_toggle, ids::FEED_FACTOR_GLOBAL_TOGGLE);

    let add_factor_btn = gtk::Button::with_label(feed::create::ADD_FACTOR);
    crate::testid::set_test_id(&add_factor_btn, ids::FEED_ADD_FACTOR_BUTTON);

    let factor_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    factor_row.append(&factor_dropdown);
    factor_row.append(&factor_weight_entry);
    factor_row.append(&factor_global_toggle);
    factor_row.append(&add_factor_btn);

    {
        let nest = Arc::clone(client.nest_rpc());
        let rt = rt.clone();
        let factor_keys = Rc::clone(&factor_keys);
        let factor_names = Rc::clone(&factor_names);
        let factor_model = factor_model.clone();
        // Wire reads run on the tokio runtime and the results land on the GTK
        // thread (spawn_with_snapshot): `NestClient`'s reply timeout is a
        // tokio timer, so awaiting these from the glib context panics with
        // "no reactor running" — it killed the app the first time the dialog
        // opened with a sealed-registry read on this path.
        crate::async_helper::spawn_with_snapshot(
            &rt,
            move || async move {
                let labelers: Vec<String> = LabelersClient::new(Arc::clone(&nest))
                    .list()
                    .await
                    .map(|reply| {
                        reply
                            .labelers
                            .iter()
                            .filter(|l| l.subscribed)
                            .map(|l| l.factor.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                // Trained topic factors from the account's registry — the
                // picker's third source (topic-factors.md § Authoring surface
                // & picker): key `topic:<hex>`, display = the user's name.
                let topics: Vec<(String, String)> =
                    match fauna_sync_engine::preference_surfaces::load_personalization(
                        &crate::account_runtime::handle_source(),
                    )
                    .await
                    {
                        Ok(registry) => registry
                            .trained_factors
                            .iter()
                            .filter_map(|m| m.factor_key().map(|k| (k, m.name.clone())))
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                (labelers, topics)
            },
            move |(labelers, topics): (Vec<String>, Vec<(String, String)>)| {
                for factor in labelers {
                    factor_model.append(&factor);
                    factor_keys.borrow_mut().push(factor);
                }
                // Names land in the display map BEFORE the keys append, so an
                // option never renders its raw key first.
                for (key, name) in topics {
                    factor_names.borrow_mut().insert(key.clone(), name);
                    factor_model.append(&key);
                    factor_keys.borrow_mut().push(key);
                }
            },
        );
    }

    // Accumulated factor-weight entries (`FactorWeightInput`) + a running
    // summary — mirrors the rule builder above exactly.
    let factors: Rc<RefCell<Vec<FactorWeightInput>>> = Rc::new(RefCell::new(Vec::new()));
    let factors_summary = gtk::Label::new(None);
    factors_summary.set_halign(gtk::Align::Start);
    factors_summary.set_wrap(true);
    factors_summary.add_css_class("dim-label");
    factors_summary.add_css_class("caption");

    {
        let factors = Rc::clone(&factors);
        let factor_keys = Rc::clone(&factor_keys);
        let dropdown = factor_dropdown.clone();
        let weight_entry = factor_weight_entry.clone();
        let global_toggle = factor_global_toggle.clone();
        let summary = factors_summary.clone();
        add_factor_btn.connect_clicked(move |_| {
            let idx = dropdown.selected() as usize;
            let Some(factor) = factor_keys.borrow().get(idx).cloned() else {
                return;
            };
            let weight_permille = fauna_core::format::parse_weight_permille(&weight_entry.text());
            let global = global_toggle.is_active();
            factors.borrow_mut().push(FactorWeightInput {
                factor,
                weight_permille,
                global,
            });
            weight_entry.set_text("1.0");
            global_toggle.set_active(false);
            let added: Vec<String> = factors.borrow().iter().map(|f| f.factor.clone()).collect();
            summary.set_text(&format!("{}: {}", feed::create::FACTORS, added.join(", ")));
        });
    }

    // ── Action buttons ───────────────────────────────────────────────────
    let create_btn = gtk::Button::with_label(common::CREATE);
    create_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&create_btn, ids::CREATE_FEED);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.feed.create");

    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    crate::testid::set_test_id(&cancel_btn, ids::FEED_CREATE_CANCEL);

    let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_row.set_halign(gtk::Align::End);
    button_row.append(&cancel_btn);
    button_row.append(&create_btn);

    vbox.append(&gtk::Label::new(Some(feed::create::FEED_NAME)));
    vbox.append(&name_entry);
    vbox.append(&gtk::Label::new(Some(feed::create::COMBINATION)));
    vbox.append(&combo);
    vbox.append(&gtk::Label::new(Some(feed::create::FILTER_RULES)));
    vbox.append(&rule_row);
    vbox.append(&rules_summary);
    vbox.append(&gtk::Label::new(Some(feed::create::FACTORS)));
    vbox.append(&factor_row);
    vbox.append(&factors_summary);
    vbox.append(&button_row);

    dialog.set_content(Some(&vbox));

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        let d = dialog.clone();
        let name_e = name_entry.clone();
        let combo_r = combo.clone();
        let rules = Rc::clone(&rules);
        let factors = Rc::clone(&factors);
        create_btn.connect_clicked(move |_| {
            let name = name_e.text().to_string();
            if name.is_empty() {
                return;
            }
            // The model carries the wire values, so read the selection back
            // rather than re-deriving it from a magic index — the same
            // `dropdown_wire_value` shape `views/admin.rs` uses. This is what
            // keeps the value/label split honest: were the model ever swapped
            // back to painted labels, the feed would be created with the
            // *label* as its combination, not merely fail a test.
            let combination = combo_r
                .selected_item()
                .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                .map(|s| s.string().to_string())
                .unwrap_or_else(|| COMBINATION_ALL.to_string());
            let rule_inputs = rules.borrow().clone();
            let factor_inputs = factors.borrow().clone();
            let m = Arc::clone(&m);
            rt.spawn(async move {
                let _ = m
                    .create_feed(name, rule_inputs, combination, None, None, factor_inputs)
                    .await;
            });
            d.close();
        });
    }

    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            d.close();
        });
    }

    dialog
}

/// Build a "Subscribe to Bridge Feed" dialog → `FeedManager::subscribe_bridge`.
fn build_subscribe_bridge_dialog(manager: &Arc<LinuxFeedManager>, rt: &Handle) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(feed::list::SUBSCRIBE_BRIDGE)
        .modal(true)
        .default_width(360)
        .build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    // The selectable bridges = the ones the nest can actually serve
    // (`snapshot.available_bridges`, from `fauna.bridges.list`, build+runtime
    // gated) — a dropdown, never a free-text "type any protocol" entry, so the
    // client never offers a protocol the nest can't serve (`version-compatibility.md`
    // § Dim 3 — capability consumption). Read at dialog-build time.
    let available: Vec<AvailableBridge> = manager.snapshot().available_bridges;
    let bridge_names: Vec<&str> = available.iter().map(|b| b.name.as_str()).collect();
    let bridge_model = gtk::StringList::new(&bridge_names);
    let bridge_dropdown = gtk::DropDown::builder().model(&bridge_model).build();
    crate::testid::set_test_id(&bridge_dropdown, ids::BRIDGE_FORM_BRIDGE_SELECT);

    let uri_entry = gtk::Entry::new();
    uri_entry.set_placeholder_text(Some(feed::bridge_form::URI));
    crate::testid::set_test_id(&uri_entry, ids::BRIDGE_FORM_URI_INPUT);

    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some(feed::bridge_form::NAME));
    crate::testid::set_test_id(&name_entry, ids::BRIDGE_FORM_NAME_INPUT);

    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    crate::testid::set_test_id(&cancel_btn, ids::BRIDGE_FORM_CANCEL_BUTTON);

    let subscribe_btn = gtk::Button::with_label(feed::list::SUBSCRIBE_BRIDGE);
    subscribe_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&subscribe_btn, ids::BRIDGE_FORM_SUBSCRIBE_BUTTON);
    crate::offline_gate::declare_wire_kind(&subscribe_btn, "fauna.bridges.feeds.create");

    let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_row.set_halign(gtk::Align::End);
    button_row.append(&cancel_btn);
    button_row.append(&subscribe_btn);

    vbox.append(&gtk::Label::new(Some(&format!(
        "{}:",
        feed::bridge_form::KIND
    ))));
    vbox.append(&bridge_dropdown);
    vbox.append(&gtk::Label::new(Some(&format!(
        "{}:",
        feed::bridge_form::URI
    ))));
    vbox.append(&uri_entry);
    vbox.append(&gtk::Label::new(Some(&format!("{}:", common::NAME))));
    vbox.append(&name_entry);
    vbox.append(&button_row);

    dialog.set_content(Some(&vbox));

    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            d.close();
        });
    }

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        let d = dialog.clone();
        let dd = bridge_dropdown.clone();
        let available = available.clone();
        let ue = uri_entry.clone();
        let ne = name_entry.clone();
        subscribe_btn.connect_clicked(move |_| {
            // The dropdown index maps to the available-bridge id sent as
            // `CreateFeedRequest.bridge` — never a free-typed protocol string.
            let bridge = match available.get(dd.selected() as usize) {
                Some(b) => b.id.clone(),
                None => return,
            };
            let uri = ue.text().to_string();
            let name = ne.text().to_string();
            if bridge.is_empty() || uri.is_empty() {
                return;
            }
            let display_name = if name.is_empty() { uri.clone() } else { name };
            let m = Arc::clone(&m);
            rt.spawn(async move {
                let _ = m.subscribe_bridge(bridge, uri, display_name).await;
            });
            d.close();
        });
    }

    dialog
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::agent::string_model_index;
    use crate::testid::run_on_gtk_thread;

    /// **Every** value the shared catalog offers must resolve through the very
    /// lookup `/element/select` uses — not the three the suite happens to drive
    /// today (`HasMedia`, `MinReplies`, `BodyContains`).
    ///
    /// `feed.md` § Where logic lives is explicit: *"The select's value stays the
    /// `FilterRule` variant name (ui.yaml's domain — the e2e action layer drives
    /// it); only the label is localized."* A label-only model breaks that
    /// contract for every type whose label carries text beyond the variant name,
    /// because `select_match`'s normalization fallback (lowercased alphanumerics)
    /// can only bridge a label that IS the key respaced — `"Body contains"` works,
    /// `"Protocol Source"` and `"Label Below (exclude spam)"` never can.
    ///
    /// Asserted as the catalog-wide invariant rather than a fixed value list
    /// (e2e-conventions.md point 17) so a future rule type whose label gains
    /// descriptive text fails here, at tier_1, instead of in a sweep months later
    /// — which is exactly how `Source`/`LabelBelow`/`LabelAbove` got in.
    #[test]
    fn every_rule_type_value_resolves_through_the_agent_lookup() {
        run_on_gtk_thread(|| {
            let options = fauna_client_feed::rule_type_options();
            let dropdown = build_rule_type_dropdown(&options);
            let model = dropdown.model();
            let painted: Vec<String> = (0..model.as_ref().map_or(0, |m| m.n_items()))
                .filter_map(|j| {
                    model
                        .as_ref()
                        .and_then(|m| m.item(j))
                        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                        .map(|s| s.string().to_string())
                })
                .collect();
            for (i, opt) in options.iter().enumerate() {
                assert_eq!(
                    string_model_index(model.as_ref(), &opt.value),
                    Some(i as u32),
                    "select(\"feed-rule-type-select\", {:?}) must resolve to row {i}; \
                     this render offered [{}] — the model must carry the WIRE VALUES, \
                     with the localized label painted by the display expression",
                    opt.value,
                    painted.join(", "),
                );
            }
        });
    }

    /// The same contract on `feed-combination-select`, whose two wire values are
    /// what `actions/feed.py` drives (`select(id, "any")`) and what `create_feed`
    /// encodes. tui offers exactly `["all", "any"]`
    /// (`apps/fauna-tui/src/feed/mod.rs`); linux painted `"All match"` /
    /// `"Any match"`, which normalize to `allmatch`/`anymatch` — so *neither*
    /// value resolved. Latent rather than red only because no test drives a
    /// non-default combination yet.
    #[test]
    fn both_combination_values_resolve_through_the_agent_lookup() {
        run_on_gtk_thread(|| {
            let dropdown = build_combination_dropdown();
            let model = dropdown.model();
            assert_eq!(string_model_index(model.as_ref(), "all"), Some(0));
            assert_eq!(string_model_index(model.as_ref(), "any"), Some(1));
        });
    }

    /// The other half of the split: the model carries machine values, so the
    /// *painted* string must still be the localized label. Without this, making
    /// the tests above pass by simply swapping labels for values would silently
    /// ship raw `LabelBelow` text to the user.
    #[test]
    fn the_rule_type_dropdown_still_paints_localized_labels() {
        run_on_gtk_thread(|| {
            let options = fauna_client_feed::rule_type_options();
            let dropdown = build_rule_type_dropdown(&options);
            let expression = dropdown
                .expression()
                .expect("the rule-type dropdown must paint labels via a display expression");
            let model = dropdown.model().expect("model");
            for (i, opt) in options.iter().enumerate() {
                let item = model.item(i as u32).expect("item");
                let painted: String = expression
                    .evaluate(Some(&item))
                    .expect("the display expression must evaluate for every row")
                    .get()
                    .expect("the display expression must yield a string");
                assert_eq!(
                    painted,
                    opt.label.resolve(crate::i18n::strings::lookup),
                    "row {i} ({}) must paint its localized catalog label",
                    opt.value,
                );
            }
        });
    }
}
