use crate::i18n::strings::{admin, common, devices, features, photo_backup, status};
use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::glib;

/// Widget handles for status view rows that need dynamic updates.
pub struct StatusHandles {
    pub actor_id_row: adw::ActionRow,
    pub handle_row: adw::ActionRow,
    pub node_url_row: adw::ActionRow,
    pub connection_row: adw::ActionRow,
    // Quota rows.
    pub quota_tier_row: adw::ActionRow,
    pub quota_inbox_row: adw::ActionRow,
    pub quota_storage_row: adw::ActionRow,
    pub quota_devices_row: adw::ActionRow,
    /// AT-SPI markers carrying the quota values as readable text, plus a
    /// data-gated section anchor. The section marker stays hidden until
    /// `update_quota` runs (mirroring web's `{#if quota}` conditional render),
    /// so `wait_for("quota-section")` blocks on the real `fauna.quota.get`
    /// reply rather than racing it. The value markers carry the formatted
    /// usage text so the e2e driver's `get_text` returns it (an adw::ActionRow
    /// subtitle isn't reliably AT-SPI-discoverable — see actor_id_marker).
    pub quota_section_marker: gtk::Label,
    pub quota_inbox_marker: gtk::Label,
    pub quota_storage_marker: gtk::Label,
    pub quota_devices_marker: gtk::Label,
    // Feature-limits group. `feature_limits_rows_box` — not the group
    // itself — is the rebuild target: see its construction site for why.
    pub feature_limits_group: adw::PreferencesGroup,
    pub feature_limits_rows_box: gtk::Box,
    /// Hidden until `update_features` runs (the `quota_section_marker`
    /// hydrate-gates-paint pattern — `settings.md` § Layout & flow item 2b:
    /// "renders only once the transparency read resolves").
    pub feature_limits_section_marker: gtk::Label,
    /// The region transparency section's rows (`settings-region-*`) — a plain
    /// box `crate::region::paint_settings` owns outright and clear-and-rebuilds
    /// (the `feature_limits_rows_box` shape, for the same reason).
    pub region_rows_box: gtk::Box,
    // Node-info rows.
    pub node_domain_row: adw::ActionRow,
    pub node_version_row: adw::ActionRow,
    // Sync-section rows.
    pub files_row: adw::ActionRow,
    pub last_sync_row: adw::ActionRow,
}

/// Build the status view: a single `adw::PreferencesPage` with identity,
/// connection, quota, and node groups.
///
/// Returns the page and handles to rows that need dynamic updates.
pub fn build_status_view() -> (gtk::Box, StatusHandles) {
    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let page = adw::PreferencesPage::new();

    // Marker for E2E test discovery via AT-SPI. Empty label = zero height.
    let marker = gtk::Label::new(None);
    crate::testid::set_test_id(&marker, ids::SETTINGS_VIEW);
    wrapper.append(&marker);

    // Page heading marker for unified E2E navigation tests.
    // Use zero-height label (not invisible) so AT-SPI can discover it.
    let heading_marker = gtk::Label::new(Some(common::SETTINGS));
    heading_marker.set_height_request(0);
    heading_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&heading_marker, ids::PAGE_HEADING);
    wrapper.append(&heading_marker);

    // Zero-height marker so E2E tests can find the account settings area via
    // the cross-platform `account-settings-link` element ID.
    let account_marker = gtk::Label::new(None);
    account_marker.set_height_request(0);
    account_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&account_marker, ids::ACCOUNT_SETTINGS_LINK);
    wrapper.append(&account_marker);

    // --- Identity group ---
    let identity_group = adw::PreferencesGroup::new();
    identity_group.set_title(common::IDENTITY);

    let actor_id_row = adw::ActionRow::builder()
        .title(common::ACTOR_ID)
        .subtitle(status::identity::NOT_CONFIGURED)
        .subtitle_selectable(true)
        .build();
    // The actor id lives in the row subtitle (the value the copy button reads).
    // Tag the row itself `account-actor-id` so the automation agent reads that
    // subtitle via `find::text_of` (adw::ActionRow → subtitle) — there is no
    // shadow marker label any more. The former 1px marker actually rendered the
    // 64-char id a second time as a suffix, squeezing the subtitle into a narrow
    // column. Matches ui.yaml's `settings` page + web/windows, which expose the
    // actor id on the settings landing surface.
    crate::testid::set_test_id(&actor_id_row, ids::ACCOUNT_ACTOR_ID);
    identity_group.add(&actor_id_row);
    {
        let row_ref = actor_id_row.clone();
        let actor_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        actor_copy.add_css_class("flat");
        crate::testid::set_test_id(&actor_copy, ids::STATUS_ACTOR_ID_COPY_BTN);
        actor_copy.connect_clicked(move |btn| {
            let subtitle = row_ref
                .subtitle()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if subtitle != status::identity::NOT_CONFIGURED {
                crate::clipboard::copy_and_confirm(btn, &subtitle);
            }
        });
        actor_id_row.add_suffix(&actor_copy);
    }

    let handle_row = adw::ActionRow::builder()
        .title(common::HANDLE)
        .subtitle(status::identity::NOT_CONFIGURED)
        .build();
    identity_group.add(&handle_row);

    let node_url_row = adw::ActionRow::builder()
        .title(common::NODE_URL)
        .subtitle(status::identity::NOT_CONFIGURED)
        .subtitle_selectable(true)
        .build();
    identity_group.add(&node_url_row);
    {
        let row_ref = node_url_row.clone();
        let url_copy = gtk::Button::with_label(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD);
        url_copy.add_css_class("flat");
        crate::testid::set_test_id(&url_copy, ids::STATUS_NODE_URL_COPY_BTN);
        url_copy.connect_clicked(move |btn| {
            let subtitle = row_ref
                .subtitle()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if subtitle != status::identity::NOT_CONFIGURED {
                crate::clipboard::copy_and_confirm(btn, &subtitle);
            }
        });
        node_url_row.add_suffix(&url_copy);
    }

    page.add(&identity_group);

    // --- Connection group ---
    let connection_group = adw::PreferencesGroup::new();
    connection_group.set_title(devices::peers::CONNECTION);

    // The single connection row. WS-RPC over one WebSocket is the *only* nest
    // transport, so "Status: Connected (real-time)" already IS the WebSocket
    // state — `app.rs` keeps this row's subtitle in sync with the live link.
    // The former separate "WebSocket" row was never wired (no handle), so it sat
    // permanently at "Not connected", contradicting the live Status row. Removed.
    let connection_row = adw::ActionRow::builder()
        .title(common::STATUS)
        .subtitle(common::CONNECTING)
        .build();
    connection_group.add(&connection_row);

    page.add(&connection_group);

    // --- Sync group ---
    let sync_group = adw::PreferencesGroup::new();
    sync_group.set_title(common::SYNC);

    let files_row = adw::ActionRow::builder()
        .title(status::sync::FILES_SYNCED)
        .subtitle("0")
        .build();
    sync_group.add(&files_row);

    let last_sync_row = adw::ActionRow::builder()
        .title(photo_backup::LAST_SYNC)
        .subtitle(common::NEVER)
        .build();
    crate::testid::set_test_id(&last_sync_row, ids::STATUS_SYNC_LAST);
    sync_group.add(&last_sync_row);

    page.add(&sync_group);

    // --- P2P group ---
    let p2p_group = adw::PreferencesGroup::new();
    p2p_group.set_title(status::p2p::TITLE);

    let p2p_tunnel_row = adw::ActionRow::builder()
        .title(status::p2p::TUNNEL)
        .subtitle(common::INACTIVE)
        .build();
    p2p_group.add(&p2p_tunnel_row);

    let p2p_peers_row = adw::ActionRow::builder()
        .title(common::PEERS)
        .subtitle("0")
        .build();
    p2p_group.add(&p2p_peers_row);

    page.add(&p2p_group);

    // Poll P2P status every 3 seconds
    {
        let tunnel_row = p2p_tunnel_row.clone();
        let peers_row = p2p_peers_row.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
            let (active, node_id) = crate::settings::p2p_tab::get_tunnel_status_summary();
            tunnel_row.set_subtitle(&p2p_tunnel_detail(active, node_id.as_deref()));

            // Update peer count
            if let Some(p2p) = crate::settings::get_p2p() {
                match p2p.list_contacts() {
                    Ok(contacts) => {
                        peers_row.set_subtitle(&contacts.len().to_string());
                    }
                    Err(_) => {
                        peers_row.set_subtitle("?");
                    }
                }
            }

            glib::ControlFlow::Continue
        });
    }

    // --- Quota group ---
    // Surfaces the canonical `settings`-page quota IDs (ui.yaml: quota-section,
    // quota-inbox, quota-storage, quota-devices). These used to also live on
    // the account-settings tab, but that build-once tab reads cached state at
    // *build* time, which goes stale — so the usage & quota group moved here
    // entirely (settings/account.rs's own NOTE records the move). The status
    // view is the reachable, live-updated surface for this data.
    let quota_group = adw::PreferencesGroup::new();
    quota_group.set_title(status::quota::TITLE);

    // Section anchor — kept hidden until quota data arrives so `wait_for`
    // blocks on the real fetch (web renders the whole section under `{#if quota}`).
    let quota_section_marker = gtk::Label::new(None);
    quota_section_marker.set_height_request(1);
    quota_section_marker.set_overflow(gtk::Overflow::Hidden);
    quota_section_marker.set_visible(false);
    crate::testid::set_test_id(&quota_section_marker, ids::QUOTA_SECTION);
    quota_group.set_header_suffix(Some(&quota_section_marker));

    let quota_tier_row = adw::ActionRow::builder()
        .title(common::TIER)
        .subtitle("—")
        .build();
    quota_group.add(&quota_tier_row);

    let quota_inbox_row = adw::ActionRow::builder()
        .title(status::quota::INBOX_USAGE)
        .subtitle("—")
        .build();
    let quota_inbox_marker = gtk::Label::new(None);
    quota_inbox_marker.set_height_request(1);
    quota_inbox_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&quota_inbox_marker, ids::QUOTA_INBOX);
    quota_inbox_row.add_suffix(&quota_inbox_marker);
    quota_group.add(&quota_inbox_row);

    let quota_storage_row = adw::ActionRow::builder()
        .title(status::quota::STORAGE_USAGE)
        .subtitle("—")
        .build();
    let quota_storage_marker = gtk::Label::new(None);
    quota_storage_marker.set_height_request(1);
    quota_storage_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&quota_storage_marker, ids::QUOTA_STORAGE);
    quota_storage_row.add_suffix(&quota_storage_marker);
    quota_group.add(&quota_storage_row);

    let quota_devices_row = adw::ActionRow::builder()
        .title(common::DEVICES)
        .subtitle("—")
        .build();
    let quota_devices_marker = gtk::Label::new(None);
    quota_devices_marker.set_height_request(1);
    quota_devices_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&quota_devices_marker, ids::QUOTA_DEVICES);
    quota_devices_row.add_suffix(&quota_devices_marker);
    quota_group.add(&quota_devices_row);

    page.add(&quota_group);

    // --- Feature limits group ---
    // `feature-limits-section` — the gated-feature plane's transparency read
    // (`dynamic-features.md` § Transparency & auditability, boundary 4: "no
    // silent gates"). Placed directly after Quota as its sibling "what bounds
    // me" surface (`settings.md` § Layout & flow item 2b). tui shipped this
    // screen first (2026-08-11); every rendering decision below is the shared
    // `fauna_client_features::FeatureRow` — this function paints, it decides
    // nothing (mirrors tui's `feature_limits_elements`,
    // `apps/fauna-tui/src/settings/root.rs`).
    let feature_limits_group = adw::PreferencesGroup::new();
    feature_limits_group.set_title(features::SECTION_TITLE);

    // Section anchor — kept hidden until the feature-limits read resolves, so
    // `wait_for("feature-limits-section")` blocks on the real
    // `FeaturesClient::rows()` reply rather than racing it (the
    // `quota_section_marker` precedent above).
    let feature_limits_section_marker = gtk::Label::new(None);
    feature_limits_section_marker.set_height_request(1);
    feature_limits_section_marker.set_overflow(gtk::Overflow::Hidden);
    feature_limits_section_marker.set_visible(false);
    crate::testid::set_test_id(&feature_limits_section_marker, ids::FEATURE_LIMITS_SECTION);
    feature_limits_group.set_header_suffix(Some(&feature_limits_section_marker));

    // The rows are plain `gtk::Box`/`gtk::Label` content, not `adw::ActionRow`s
    // — `AdwPreferencesGroup::add()` on non-row content wraps everything into
    // ONE internal container, so `first_child()` on the group itself never
    // sees individual rows to clear (found 2026-08-15: rows silently
    // accumulated across refreshes, 3/6/9/…, each `update_features` call
    // adding a fresh generation into the same still-there wrapper). Adding
    // exactly ONE plain `gtk::Box` here — mirrors `settings/nostr_tab.rs`'s
    // `follow_list_container` — gives `update_features` a container it owns
    // outright and can safely blind-clear-and-rebuild each time, with the
    // group's own header/title furniture never touched.
    let feature_limits_rows_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    feature_limits_group.add(&feature_limits_rows_box);

    page.add(&feature_limits_group);

    // --- Region group ---
    // `settings-region-section` — the region content plane's transparency
    // surface (`region-blocking.md` § The blocked render and the transparency
    // surface), beside feature limits: the two "what bounds me, and who says
    // so" surfaces sit together, as on tui (`settings/root.rs`). Always
    // present; the rows are a paint of the shared `RegionPlane::view`.
    let region_group = adw::PreferencesGroup::new();
    region_group.set_title(crate::i18n::strings::region::SECTION_TITLE);
    let region_section_marker = gtk::Label::new(Some(crate::i18n::strings::region::SECTION_TITLE));
    region_section_marker.set_height_request(1);
    region_section_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&region_section_marker, ids::SETTINGS_REGION_SECTION);
    region_group.set_header_suffix(Some(&region_section_marker));
    let region_rows_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    region_group.add(&region_rows_box);
    crate::region::paint_settings(&region_rows_box);
    page.add(&region_group);

    // --- Node info group ---
    let node_group = adw::PreferencesGroup::new();
    node_group.set_title(status::node::TITLE);

    let node_domain_row = adw::ActionRow::builder()
        .title(common::DOMAIN)
        .subtitle("—")
        .subtitle_selectable(true)
        .build();
    crate::testid::set_test_id(&node_domain_row, ids::STATUS_NODE_DOMAIN);
    node_group.add(&node_domain_row);

    let node_version_row = adw::ActionRow::builder()
        .title(admin::dashboard::VERSION)
        .subtitle("—")
        .build();
    crate::testid::set_test_id(&node_version_row, ids::STATUS_NODE_VERSION);
    node_group.add(&node_version_row);

    page.add(&node_group);

    // NOTE: the privacy / mail / mail-aliases / mail-spam / mail-export /
    // mail-lists / mail-list-members / linked-nests pages used to be embedded
    // here (the "AT-SPI reason": the e2e state protocol can't open the old
    // `adw::PreferencesWindow` modal). That embed forced the window wide (the
    // mail page's 539-char keys-info marker) and stacked short sections into one
    // scroll. Both the modal AND this embed are gone: those pages are now
    // reachable sub-pages of the inline Settings sidebar-swap shell
    // (`views::settings_shell`), where this status view is itself the "status"
    // sub-page. See settings.md § Navigation model.

    let handles = StatusHandles {
        actor_id_row,
        handle_row,
        node_url_row,
        connection_row,
        quota_tier_row,
        quota_inbox_row,
        quota_storage_row,
        quota_devices_row,
        quota_section_marker,
        quota_inbox_marker,
        quota_storage_marker,
        quota_devices_marker,
        feature_limits_group,
        feature_limits_rows_box,
        feature_limits_section_marker,
        region_rows_box,
        node_domain_row,
        node_version_row,
        files_row,
        last_sync_row,
    };

    wrapper.append(&page);
    (wrapper, handles)
}

/// Update quota rows from a `fauna.quota.get` reply (serialised as JSON for the
/// linux UI's internal `DataMessage::QuotaLoaded`; the nest↔client wire itself
/// is DAG-CBOR). Shape: `{ tier, inbox:{used_bytes,max_bytes},
/// storage:{used_bytes,max_bytes}, devices:{used,max}, features }` —
/// `QuotaGetReply` in fauna-protocol. The earlier `inbox_usage`/`storage_usage`
/// keys never existed on this reply, so those rows silently stayed at "—".
pub fn update_quota(handles: &StatusHandles, quota: &serde_json::Value) {
    if let Some(tier) = quota.get("tier").and_then(|v| v.as_str()) {
        handles.quota_tier_row.set_subtitle(tier);
    }

    let inbox = format_usage_bytes(quota.get("inbox"));
    handles.quota_inbox_row.set_subtitle(&inbox);
    handles.quota_inbox_marker.set_text(&inbox);

    let storage = format_usage_bytes(quota.get("storage"));
    handles.quota_storage_row.set_subtitle(&storage);
    handles.quota_storage_marker.set_text(&storage);

    let devices = quota
        .get("devices")
        .and_then(|d| {
            let used = d.get("used")?.as_u64()?;
            let max = d.get("max")?.as_u64()?;
            Some(format!("{used} / {max}"))
        })
        .unwrap_or_default();
    if !devices.is_empty() {
        handles.quota_devices_row.set_subtitle(&devices);
        handles.quota_devices_marker.set_text(&devices);
    }

    // Reveal the section anchor now that the fetch has resolved (matches web's
    // `{#if quota}`), so `wait_for("quota-section")` waited for real data.
    handles.quota_section_marker.set_visible(true);
}

/// Update the `feature-limits-section` group from a
/// `FeaturesClient::rows()` reply — the whole feature-limits surface, ready to
/// render. **This function paints; it decides nothing** — every judgement
/// (which cells survived the tier meet, `remaining = limit − observed`, which
/// tier bound each cell, whether a member is available/restricted/hidden, and
/// the words for all of it) is `fauna_client_features`' output, so the answer
/// to "why can't I do this" cannot differ across the 7 apps (priority #1) and
/// is written once (priority #2). Mirrors tui's `feature_limits_elements`
/// (`apps/fauna-tui/src/settings/root.rs`) field-for-field.
pub fn update_features(handles: &StatusHandles, rows: &[fauna_client_features::FeatureRow]) {
    // Rebuild: clear `feature_limits_rows_box`'s own children — a plain
    // `gtk::Box` this function owns outright, safe to blind-clear-and-rebuild.
    // NEVER clear `feature_limits_group` itself: `AdwPreferencesGroup::add()`
    // on non-`ActionRow` content (this function's `gtk::Box`/`gtk::Label`
    // rows) wraps everything into ONE internal container, so the group's own
    // `first_child()` chain never exposes individual rows to remove — found
    // 2026-08-15 chasing rows that silently accumulated across refreshes
    // (3, 6, 9, … — a fresh generation added into the still-there wrapper on
    // every call) and, before that fix, a blind clear-and-remove of the
    // group's OWN `first_child()` chain hit its title/header furniture too
    // and corrupted the widget outright (wedged every automation command on
    // the app — see the construction site's comment).
    while let Some(child) = handles.feature_limits_rows_box.first_child() {
        handles.feature_limits_rows_box.remove(&child);
    }

    // A `hidden` row means this nest build does not carry the feature at all
    // (its capability token is absent) — filtered here, not in the shared
    // crate, for the same reason tui's `root.rs` filters it client-side: the
    // crate's job is to decide, not to choose which decisions a surface shows.
    let visible: Vec<_> = rows.iter().filter(|r| r.affordance != "hidden").collect();

    if visible.is_empty() {
        let empty = gtk::Label::new(Some(features::EMPTY));
        empty.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&empty, ids::FEATURE_LIMITS_EMPTY);
        handles.feature_limits_rows_box.append(&empty);
    } else {
        for row in &visible {
            let row_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
            row_box.set_margin_top(6);
            row_box.set_margin_bottom(6);
            crate::testid::set_test_id(&row_box, ids::FEATURE_LIMITS_ROW);

            let name = gtk::Label::new(Some(&row.name.resolve(crate::i18n::strings::lookup)));
            name.set_halign(gtk::Align::Start);
            name.add_css_class("heading");
            crate::testid::set_test_id(&name, ids::FEATURE_LIMITS_NAME);
            row_box.append(&name);

            let status = gtk::Label::new(Some(&row.status.resolve(crate::i18n::strings::lookup)));
            status.set_halign(gtk::Align::Start);
            crate::testid::set_test_id(&status, ids::FEATURE_LIMITS_STATUS);
            row_box.append(&status);

            // Only when something actually blocks — boundary 4's "no silent
            // gates" half. `resolve_nested`, not `resolve`: the sentence's
            // `{window}` is itself an i18n key.
            if let Some(reason) = &row.restriction {
                let restriction =
                    gtk::Label::new(Some(&reason.resolve_nested(crate::i18n::strings::lookup)));
                restriction.set_halign(gtk::Align::Start);
                crate::testid::set_test_id(&restriction, ids::FEATURE_LIMITS_RESTRICTION);
                row_box.append(&restriction);
            }

            for cell in &row.cells {
                let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                crate::testid::set_test_id(&cell_box, ids::FEATURE_LIMITS_QUOTA);

                let label = gtk::Label::new(Some(
                    &cell.label.resolve_nested(crate::i18n::strings::lookup),
                ));
                crate::testid::set_test_id(&label, ids::FEATURE_LIMITS_QUOTA_LABEL);
                cell_box.append(&label);

                // Per CELL, not per row: the meet takes the MIN per
                // (dimension, window), so two bounds on one feature can come
                // from different tiers.
                let value = gtk::Label::new(Some(&fauna_client_features::cell_value_text(
                    cell,
                    crate::i18n::strings::lookup,
                )));
                crate::testid::set_test_id(&value, ids::FEATURE_LIMITS_QUOTA_VALUE);
                cell_box.append(&value);

                let tier =
                    gtk::Label::new(Some(&cell.tier_label.resolve(crate::i18n::strings::lookup)));
                crate::testid::set_test_id(&tier, ids::FEATURE_LIMITS_QUOTA_TIER);
                cell_box.append(&tier);

                row_box.append(&cell_box);
            }

            handles.feature_limits_rows_box.append(&row_box);
        }
    }

    // Reveal the section anchor now that the read has resolved.
    handles.feature_limits_section_marker.set_visible(true);
}

/// Format a `{used_bytes, max_bytes}` usage object as "used / max"; empty when
/// the object is absent so the e2e `has_quota` check isn't satisfied by a stub.
fn format_usage_bytes(usage: Option<&serde_json::Value>) -> String {
    let Some(obj) = usage else {
        return String::new();
    };
    let used = obj.get("used_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    let max = obj.get("max_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    format!(
        "{} / {}",
        crate::i18n::byte_size(used),
        crate::i18n::byte_size(max)
    )
}

/// Update node-info rows from a JSON node-info response.
pub fn update_nest_info(handles: &StatusHandles, info: &serde_json::Value) {
    if let Some(domain) = info.get("domain").and_then(|v| v.as_str()) {
        handles.node_domain_row.set_subtitle(domain);
    }
    if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
        handles.node_version_row.set_subtitle(version);
    }
}

/// Decide the "Last Sync" row text: `None`/`0` → [`common::NEVER`] ("no sync
/// recorded yet" — the same convention as
/// `fauna_core::format::backup_last_upload_label`); otherwise the shared
/// relative-time display for `last_sync_at` (epoch **seconds**). `now_ms` is a
/// parameter (not read internally) so this stays a pure, GTK-free unit.
fn last_sync_text(last_sync_at: Option<i64>, now_ms: i64) -> String {
    match last_sync_at {
        Some(secs) if secs > 0 => crate::i18n::relative_time(secs.saturating_mul(1000), now_ms),
        _ => common::NEVER.to_string(),
    }
}

/// Update the Sync section from a `fetch_sync_status_summary` aggregate
/// (`status.md` footnote 4 — the section used to render a permanent "0"/
/// "Never" placeholder, never wired to live data after initial paint).
pub fn update_sync_status(handles: &StatusHandles, files_synced: u64, last_sync_at: Option<i64>) {
    handles.files_row.set_subtitle(&files_synced.to_string());
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    handles
        .last_sync_row
        .set_subtitle(&last_sync_text(last_sync_at, now_ms));
}

/// Decide the P2P tunnel row's subtitle from a `get_tunnel_status_summary()`
/// pair — no hardcoded English (previously `"Active".to_string()` /
/// `format!("Active ({ip}:{p})")` inline in the poll closure).
///
/// The address shown is the node id in the canonical [`short_id`] form: it is a
/// 64-char hex string (the actor's Ed25519 public key, which *is* the iroh
/// dialable address) and a status row is one line. The full value is on the P2P
/// settings page behind `p2p-node-id-copy-btn`.
fn p2p_tunnel_detail(active: bool, node_id: Option<&str>) -> String {
    if !active {
        return common::INACTIVE.to_string();
    }
    match node_id {
        Some(id) => status::p2p::tunnel_active_with_address(&fauna_core::format::short_id(id)),
        None => common::ACTIVE.to_string(),
    }
}

#[cfg(test)]
mod p2p_tunnel_tests {
    use super::*;

    /// A 64-hex node id — the shape `tunnel_info()` actually returns (the
    /// actor's Ed25519 public key). The WG-era fixture here was a `10.x`
    /// tunnel IP; there is no tunnel IP any more.
    const NODE_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    #[test]
    fn inactive_reads_common_inactive() {
        assert_eq!(p2p_tunnel_detail(false, Some(NODE_ID)), common::INACTIVE);
        assert_eq!(p2p_tunnel_detail(false, None), common::INACTIVE);
    }

    #[test]
    fn active_with_no_known_endpoint_reads_common_active() {
        assert_eq!(p2p_tunnel_detail(true, None), common::ACTIVE);
    }

    #[test]
    fn active_with_endpoint_renders_the_address_qualified_string() {
        assert!(
            crate::i18n::strings::lookup("status.p2p.tunnel_active_with_address").is_some(),
            "a subtitle rendered off a missing key is hard-coded English wearing an i18n costume"
        );
        assert_eq!(
            p2p_tunnel_detail(true, Some(NODE_ID)),
            status::p2p::tunnel_active_with_address("a1b2c3d4e5f6…")
        );
        // Guards against a regression back to raw English: the qualified
        // string must actually differ from the bare "Active" label.
        assert_ne!(p2p_tunnel_detail(true, Some(NODE_ID)), common::ACTIVE);
    }

    /// The status row must NOT dump the full 64-char node id: it is a
    /// one-line subtitle, and the canonical short form is `short_id`
    /// (`value-formatting.md` § Short id).
    #[test]
    fn active_abbreviates_the_node_id_rather_than_dumping_it() {
        let rendered = p2p_tunnel_detail(true, Some(NODE_ID));
        assert!(
            !rendered.contains(NODE_ID),
            "status row dumped the full node id: {rendered}"
        );
        assert!(
            rendered.contains(&fauna_core::format::short_id(NODE_ID)),
            "status row dropped the short id entirely: {rendered}"
        );
    }
}

#[cfg(test)]
mod sync_status_tests {
    use super::*;

    const NOW_MS: i64 = 1_700_000_000_000;

    #[test]
    fn no_recorded_sync_reads_never() {
        assert_eq!(last_sync_text(None, NOW_MS), common::NEVER);
        // The `0` sentinel (never-synced epoch) is also "Never", matching
        // `backup_last_upload_label`'s `secs > 0` guard.
        assert_eq!(last_sync_text(Some(0), NOW_MS), common::NEVER);
    }

    #[test]
    fn recent_sync_renders_relative_time_not_never() {
        let just_now_secs = NOW_MS / 1000;
        let text = last_sync_text(Some(just_now_secs), NOW_MS);
        assert_ne!(text, common::NEVER);
        assert_eq!(
            text,
            crate::i18n::relative_time(just_now_secs * 1000, NOW_MS)
        );
    }
}
