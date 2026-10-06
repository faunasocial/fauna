//! **Media** — the cross-set, Windows-Explorer-style media browser
//! (`docs/goal/ui/media.md`). The 2026-06-28 sync/folder UI unification
//! (design tracked internally § 4)
//! retired the old per-set file browser: folders are the substrate, and Media
//! is the media-optimized **view** of them — a unified all-media browse across
//! every readable folder, with a per-set filter.
//!
//! The page is the **content plane** (`media.md` rule 4 — it reads folders,
//! never configures them; configuration lives in Settings → Folders). It
//! renders off the shared observer-driven `MediaMachine`
//! (`libs/fauna-media-machine`, the cross-set `fauna.media.list` pager + the
//! `media-sort-select` / `media-folder-filter` / `media-view-toggle` view
//! state) exactly as the Devices page renders off `DevicesMachine`: one
//! `async_channel` observer tick wakes a glib render loop that rewrites the
//! whole page off `snapshot()`. Sort/filter run in shared Rust
//! (`MediaSnapshot::view`); this module is the renderer.
//!
//! Upload (`file-upload` + `upload-button`) reads the picked file and uploads it
//! **into the selected folder** via the shared `MediaMachine::upload_selected`
//! gesture (seal under the owner `BackupKey` → POST the blob → record the
//! manifest member → refresh; `libs/fauna-media` `process_and_seal`). The "which
//! set" policy (the filter-selected set, or the first set with media in the
//! all-media view) lives in the shared machine, so every app targets the same
//! set (priority #1/#2 — no per-app upload pipeline; `media.md` § Where logic
//! lives / § Layout & flow). Reading the file + deriving the owner key and this
//! device's sync id is the only client glue.

pub mod detail;
pub mod item;
pub mod share;

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_core::crypto::BackupKey;
use fauna_core::localized::LocalizedText;
use fauna_media_machine::{
    FILTER_ALL_VALUE, MediaMachine, MediaObserver, build_media_machine_with_folder_keys,
};

use crate::async_helper;
use crate::client::FaunaClient;
use crate::i18n::strings;
use crate::i18n::strings::media as media_strings;
use crate::testid::set_test_id;

/// Handles the Media page hands back to `app.rs`.
pub struct MediaHandles {
    /// The shared-Rust `MediaMachine`. `app.rs` refreshes it on auth; the page
    /// also refreshes itself when it becomes visible (`connect_map`).
    pub media_machine: Arc<MediaMachine>,
    /// The page root, retained so `app.rs`'s `PushEvent::SyncChanged` arm can
    /// ask GTK whether Media is the page on screen before re-reading the
    /// cross-set aggregate — [`media_page_is_visible`].
    pub media_page: gtk::Box,
}

/// Bridges `MediaMachine` notifications to the GTK main loop: each `on_changed`
/// pushes a tick onto an `async-channel` the render loop drains (the
/// `DevicesMachine` / wizard pattern). `on_changed` may fire from a worker
/// thread (the async `refresh()` runs on the tokio runtime), so the thread-safe
/// `async_channel::Sender` is the hand-off.
struct GtkMediaObserver {
    tx: async_channel::Sender<()>,
}

impl MediaObserver for GtkMediaObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// The `page-heading` title label (ui.yaml global rule).
fn page_heading(text: &str) -> gtk::Label {
    let heading = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(["title-1"])
        .build();
    set_test_id(&heading, ids::PAGE_HEADING);
    heading
}

/// The page-level `error-message` label (e2e Rule 2), hidden until set.
fn error_label() -> gtk::Label {
    let label = gtk::Label::builder()
        .visible(false)
        .halign(gtk::Align::Start)
        .css_classes(["error"])
        .margin_start(12)
        .margin_end(12)
        .build();
    set_test_id(&label, ids::ERROR_MESSAGE);
    label
}

/// The `media-sort-select` dropdown: a value-keyed `StringList`
/// (`"name"`/`"size"`/`"date"` — the ui.yaml option values the e2e `select`
/// matches) with a localized display label, so the cross-app
/// `select(id, "<value>")` / `get_text` contract holds on the stable key (the
/// Slice-2 `folder-conflict-policy-select` value/label split). The label
/// itself is the shared [`fauna_core::format::media_sort_label`] decision
/// (priority #2) — not a per-app match.
fn build_sort_dropdown() -> gtk::DropDown {
    let model = gtk::StringList::new(&["name", "size", "date"]);
    let dropdown = gtk::DropDown::builder().model(&model).build();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        glib::closure!(|item: gtk::StringObject| {
            crate::i18n::media_sort_label(item.string().as_str())
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    set_test_id(&dropdown, ids::MEDIA_SORT_SELECT);
    dropdown
}

/// Display labels for filter values whose value is NOT its own label.
///
/// A followed browse scope's option carries a machine-minted **opaque** value
/// (`ui/media.md` § Followed public folders — hand it back, never parse it) and a
/// separate display label that already includes the owner disambiguator. The
/// GTK `StringList` model holds the VALUES, because that is what
/// `set_filter`/`select_followed_scope` take, so the label expression needs a
/// side map to render them. Rebuilt from `snapshot().followed` on every render,
/// before the model is swapped.
type FilterLabels = Rc<RefCell<std::collections::HashMap<String, String>>>;

/// What one `media-folder-filter` option should DISPLAY, given its value.
///
/// Split out of the label expression so the three arms are testable without a
/// GTK window (the `upload_path_of` habit). The interesting one is the middle:
/// a followed scope's value is opaque and would be unreadable on screen, so it
/// must resolve through the map — and a followed value MISSING from the map must
/// still not render raw, because that would leak the address shape into the UI.
fn filter_display_label(value: &str, labels: &std::collections::HashMap<String, String>) -> String {
    if value == FILTER_ALL_VALUE {
        return media_strings::FILTER_ALL.to_string();
    }
    if let Some(label) = labels.get(value) {
        // A followed scope: show its shared-Rust-minted label, never its value.
        return label.clone();
    }
    // An own set: the value IS the name (the stable e2e key).
    value.to_string()
}

/// The `media-folder-filter` dropdown shell — the model is rebuilt per render
/// from the snapshot's readable sets (the all-media sentinel first), then the
/// followed browse scopes. The display expression maps the sentinel to the
/// localized "All media" label, a followed scope to its shared-Rust-minted
/// label (via `labels`), and a real set to its own name (the stable e2e key).
fn build_filter_dropdown(labels: FilterLabels) -> gtk::DropDown {
    let dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&[FILTER_ALL_VALUE]))
        .build();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        glib::closure_local!(move |item: gtk::StringObject| {
            filter_display_label(item.string().as_str(), &labels.borrow())
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    set_test_id(&dropdown, ids::MEDIA_FOLDER_FILTER);
    dropdown
}

/// The file path an Upload press should act on, or the message to show instead.
///
/// Split out of the click handler purely so the precondition is testable without
/// a GTK window: the interesting case is the *empty* one, which used to `return`
/// silently and so presented as a dead button. `media.md` § Where logic lives leaves
/// the guard to app glue (the shared `MediaMachine::upload_selected` takes bytes,
/// never a path), which is why tui carries the same check in its own arm.
fn upload_path_of(typed: &str) -> Result<&str, &'static str> {
    let picked = typed.trim();
    if picked.is_empty() {
        // The picker-flavoured wording: linux, unlike tui, can open one.
        return Err(media_strings::FILE_REQUIRED);
    }
    Ok(picked)
}

/// Surface a client-side upload-glue failure (file read / device-id) on the page
/// `error-message`, formatted as the localized `media.error_upload` banner so it
/// reads like a machine-surfaced upload error. A successful upload's refresh
/// clears it (the render loop sets the label from `snapshot().error`).
fn show_upload_glue_error(error_label: &gtk::Label, detail: &str) {
    let err = LocalizedText::key_arg("media.error_upload", "message", detail.to_string());
    crate::settings::render_error_label(error_label, Some(&err.resolve(strings::lookup)));
}

/// Read a dropdown's selected string value (its stable model key).
pub(super) fn dropdown_value(dd: &gtk::DropDown) -> Option<String> {
    dd.model()?
        .item(dd.selected())
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
}

/// Select the dropdown row whose model string equals `value` (no-op if absent).
pub(super) fn select_dropdown_value(dd: &gtk::DropDown, value: &str) {
    let Some(model) = dd.model() else { return };
    for i in 0..model.n_items() {
        if let Some(s) = model
            .item(i)
            .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            && s.string() == value
        {
            dd.set_selected(i);
            return;
        }
    }
}

/// Widgets + state the render loop rewrites on every observer tick.
struct RenderCtx {
    view_toggle: gtk::ToggleButton,
    sort_dropdown: gtk::DropDown,
    filter_dropdown: gtk::DropDown,
    /// Value → display label for filter options whose value is not its own label
    /// (today: the followed browse scopes, whose value is opaque). Shared with
    /// `build_filter_dropdown`'s label expression; rebuilt each render.
    filter_labels: FilterLabels,
    /// `file-upload` + browse + `upload-button`, hidden as a unit while a
    /// followed browse scope is active — that scope is structurally read-only.
    upload_row: gtk::Box,
    items_flow: gtk::FlowBox,
    items_scroll: gtk::ScrolledWindow,
    empty_status: adw::StatusPage,
    error_label: gtk::Label,
    /// The share-link create / list / revoke-confirm windows
    /// (`share-links.md` § Flows), presented and closed off the snapshot.
    share: Rc<share::ShareSurfaces>,
    /// Guards the render loop's programmatic widget updates from re-entering the
    /// gesture handlers (`set_selected` / `set_active` fire their notify signals).
    updating: Rc<Cell<bool>>,
    /// The owner 32-byte `BackupKey` (derived once at build time from the actor
    /// secret) each `media-item` card's thumbnail fetch decrypts under, plus the
    /// tokio handle it runs the shared `MediaMachine::fetch_thumbnail` on.
    backup_key: Vec<u8>,
    runtime: tokio::runtime::Handle,
}

/// Build the Media explorer page. Returns the page widget (added to the main
/// stack as `"media"`) + handles (the `MediaMachine` for app.rs's auth refresh).
pub fn build_media_view(fauna_client: &Rc<FaunaClient>) -> (gtk::Box, MediaHandles) {
    let updating = Rc::new(Cell::new(false));

    // ── Header row: heading + the explorer controls ─────────────────────
    let heading = page_heading(media_strings::TITLE);

    let view_toggle = gtk::ToggleButton::builder()
        .label(media_strings::VIEW_LIST)
        .build();
    set_test_id(&view_toggle, ids::MEDIA_VIEW_TOGGLE);

    let sort_dropdown = build_sort_dropdown();
    let filter_labels: FilterLabels = Rc::new(RefCell::new(std::collections::HashMap::new()));
    let filter_dropdown = build_filter_dropdown(Rc::clone(&filter_labels));

    // Upload affordance — `file-upload` is the drivable source of truth and
    // `media.md` § Where logic lives calls for a **native picker** beside it; the
    // browse button merely *fills* the entry, exactly as the sync-folder row
    // does (`views/devices_folders/location_binding.rs`) and as windows already
    // pairs its `UploadPathBox` with a Browse button. `upload-button` seals +
    // uploads into the selected set via the shared gesture (handler wired
    // below, after `ctx`).
    //
    // The browse button carries no test ID, matching windows: the native dialog
    // is not e2e-driveable, so an id would only find a control no driver can
    // follow through. Every e2e upload types the path into `file-upload`.
    let file_entry = gtk::Entry::builder()
        .placeholder_text(media_strings::CHOOSE_FILE)
        .width_chars(18)
        .build();
    set_test_id(&file_entry, ids::FILE_UPLOAD);
    let browse_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(media_strings::CHOOSE_FILE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let upload_btn = gtk::Button::with_label(media_strings::UPLOAD);
    upload_btn.add_css_class("suggested-action");
    set_test_id(&upload_btn, ids::UPLOAD_BUTTON);
    crate::offline_gate::declare_wire_kind(&upload_btn, "fauna.sync.changes.record");

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    // The three upload widgets live in their own box so a followed browse scope
    // can withdraw the whole affordance at once — hiding the container takes
    // them out of the mapped tree (and so out of AT-SPI), which is what makes
    // the read-only scope's absence real rather than merely insensitive
    // (`ui/media.md` § Followed public folders).
    let upload_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    upload_row.append(&file_entry);
    upload_row.append(&browse_btn);
    upload_row.append(&upload_btn);

    header.append(&heading);
    header.append(&view_toggle);
    header.append(&sort_dropdown);
    header.append(&filter_dropdown);
    header.append(&upload_row);

    // Browse → open the native picker and fill the typed-path entry. Disabled
    // while open so a second click cannot spawn a second dialog (the
    // `location_binding.rs` idiom).
    {
        let file_entry = file_entry.clone();
        browse_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let dialog = gtk::FileDialog::builder()
                .title(media_strings::CHOOSE_FILE)
                .build();
            let file_entry = file_entry.clone();
            let browse_btn = btn.clone();
            browse_btn.set_sensitive(false);
            dialog.open(
                parent.as_ref(),
                gtk::gio::Cancellable::NONE,
                move |result| {
                    browse_btn.set_sensitive(true);
                    if let Ok(file) = result
                        && let Some(path) = file.path()
                    {
                        file_entry.set_text(&path.display().to_string());
                    }
                },
            );
        });
    }

    let err = error_label();

    // ── Items area: a FlowBox (1 col = list, wrapped = grid) + empty state ──
    let items_flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .row_spacing(4)
        .column_spacing(4)
        .min_children_per_line(1)
        .max_children_per_line(1)
        .build();
    let items_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&items_flow)
        .build();
    let empty_status = adw::StatusPage::builder()
        .title(media_strings::NO_MEDIA_YET)
        .icon_name("image-x-generic-symbolic")
        .vexpand(true)
        .build();
    set_test_id(&empty_status, ids::MEDIA_EMPTY_STATE);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&err);
    content.append(&empty_status);
    content.append(&items_scroll);

    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn MediaObserver> = Arc::new(GtkMediaObserver { tx });
    // The shared-folder content-key resolver (tui parity —
    // `apps/fauna-tui/src/media/mod.rs::init`). Without it a `download_file` of a
    // SHARED set's content opens under this actor's own `BackupKey`, which cannot
    // decrypt what the owner sealed under the set's M2 content key — so a member
    // (writer or reader, same-nest or cross-nest) could not read shared content
    // through the Media external-open gesture at all. The resolver reads the keys
    // from this actor's own custody, the same shared `NestFolderKeyResolver` the
    // FFI and tui glue construct.
    let resolver: Arc<dyn fauna_media_machine::FolderKeyResolver> =
        Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
            Arc::clone(fauna_client.nest_rpc()),
            crate::account_runtime::folder_key_store(),
        ));
    let machine = build_media_machine_with_folder_keys(
        Arc::clone(fauna_client.nest_rpc()),
        observer,
        Some(resolver),
    );
    // Write-side label custody for the delete/restore gestures (S8 D2): the
    // same per-actor owner key the upload gesture seals with, injected once so
    // those records seal instead of resting plaintext-only.
    machine.set_owner_backup_key(
        BackupKey::derive(&fauna_client.secret_bytes())
            .to_bytes()
            .to_vec(),
    );
    // READ-side custody for a successor: the media corpus a succession
    // re-pointed is still sealed under the identities it succeeded from
    // (`succession-aftermath.md` § Re-key scope — *media*, folders, backups).
    // Deliberately a second injection rather than an extra key on the line
    // above: that one is the delete/restore **seal** root, and a retired key
    // must never reach it. Mirrors tui's `apps/fauna-tui/src/media/mod.rs`.
    let predecessor_backup_keys = fauna_client.predecessor_backup_keys();
    if !predecessor_backup_keys.is_empty() {
        machine.set_predecessor_backup_keys(
            predecessor_backup_keys
                .iter()
                .map(|k| k.to_bytes().to_vec())
                .collect(),
        );
    }
    // …and the READER's half of the same walk: the attested predecessor ids,
    // so the listing's judge reads a row a retired identity signed as this
    // account's own (`mls-group-key-material.md` § M2 → *Writer-signed change
    // records*, ruling (8)(b)). Mirrors tui.
    let attested_predecessors = fauna_client.attested_predecessors();
    if !attested_predecessors.is_empty() {
        machine.set_predecessor_actor_ids(
            attested_predecessors
                .actor_ids()
                .iter()
                .map(|id| id.0.to_vec())
                .collect(),
        );
    }
    // …and the keys PAIRED with those identities, replacing the bare keys
    // above: a row signed as a predecessor opens only under that identity's
    // root and its predecessors' (ruling (8)(c)). Mirrors tui.
    let predecessor_chain = fauna_client.predecessor_chain();
    if !predecessor_chain.is_empty() {
        let (ids, keys) = predecessor_chain
            .iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .unzip();
        machine.set_predecessor_chain(ids, keys);
    }
    // The followed browse scopes (`ui/media.md` § Followed public folders) — the
    // SAME account-store-backed source type the Folders page wires, so a browse
    // fetch and the availability probe share one cached verdict rather than
    // racing two. Unwired, `snapshot().followed` stays empty and the filter
    // offers no scope: the correct render for a page that has not built the
    // surface.
    machine.set_followed_media_source(Arc::new(
        fauna_devices_machine::StoreFollowedFoldersSource::new(
            Arc::clone(fauna_client.nest_rpc()),
            crate::account_runtime::follows_seam(),
        ),
    ));
    // The share-link author (`share-links.md` § Where logic lives): the
    // session's identity signs the token and seals its filename, and the links
    // point at this session's nest. After the predecessors, which it reads —
    // tui's `apps/fauna-tui/src/media/mod.rs::init` order.
    machine.set_share_author(
        fauna_client.secret_bytes().to_vec(),
        fauna_client.nest_rpc().nest_url(),
    );
    // "Shared links" — the page-level entry to the caller's share links
    // (`share-links.md` § Flows → List), after the upload affordance in the
    // ui.yaml page order.
    header.append(&share::list_button(
        &machine,
        &fauna_client.runtime_handle(),
    ));

    // Gestures → the machine's view-state setters (skip the programmatic
    // render-loop updates via the `updating` guard).
    {
        let machine = Arc::clone(&machine);
        let updating = Rc::clone(&updating);
        view_toggle.connect_toggled(move |btn| {
            if updating.get() {
                return;
            }
            machine.set_view_grid(btn.is_active());
        });
    }
    {
        let machine = Arc::clone(&machine);
        let updating = Rc::clone(&updating);
        sort_dropdown.connect_selected_notify(move |dd| {
            if updating.get() {
                return;
            }
            if let Some(value) = dropdown_value(dd) {
                machine.set_sort(value);
            }
        });
    }
    {
        let machine = Arc::clone(&machine);
        let updating = Rc::clone(&updating);
        filter_dropdown.connect_selected_notify(move |dd| {
            if updating.get() {
                return;
            }
            // A followed scope's minted value routes to the async on-demand
            // listing fetch; a set name routes to the ordinary filter. The
            // MACHINE says which values are followed — asked, never parsed,
            // because the value is opaque by contract (`ui/media.md`
            // § Followed public folders). Selecting is what FETCHES, once.
            if let Some(value) = dropdown_value(dd)
                && machine.snapshot().followed.iter().any(|f| f.value == value)
            {
                let machine = Arc::clone(&machine);
                async_helper::run_on_tokio(
                    async move { machine.select_followed_scope(value).await },
                    |_| {},
                );
                return;
            }
            let filter = match dropdown_value(dd) {
                Some(v) if v == FILTER_ALL_VALUE => None,
                other => other,
            };
            machine.set_filter(filter);
        });
    }
    // ── Render loop ─────────────────────────────────────────────────────
    let ctx = Rc::new(RenderCtx {
        view_toggle,
        sort_dropdown,
        filter_dropdown,
        filter_labels,
        upload_row,
        items_flow,
        items_scroll,
        empty_status,
        error_label: err,
        share: share::ShareSurfaces::new(&machine, &fauna_client.runtime_handle(), &content),
        updating,
        // Owner backup key + runtime for the per-item `media-thumbnail` fetch
        // (`item::build_media_item`) — derived once here (per-actor, immutable),
        // same key the upload gesture seals with.
        backup_key: BackupKey::derive(&fauna_client.secret_bytes())
            .to_bytes()
            .to_vec(),
        runtime: fauna_client.runtime_handle(),
    });
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        crate::async_helper::spawn_wake_loop(rx, move || {
            render_page(&machine, &ctx);
            glib::ControlFlow::Continue
        });
    }

    // Upload: read the picked file (`file-upload` acting as a file picker) and
    // hand the bytes to the shared `MediaMachine::upload_selected` gesture, which
    // seals + POSTs + records into the selected set, then refreshes (the observer
    // repaints the list). File IO + the owner-key / device-id derivation is the
    // only client glue (the gesture takes bytes, not a path) — done here on the
    // main thread so a read / device-id failure surfaces at once on
    // `error-message`; a successful upload's refresh then clears it.
    {
        let machine = Arc::clone(&machine);
        let handle = fauna_client.runtime_handle();
        let ctx = Rc::clone(&ctx);
        let file_entry = file_entry.clone();
        let secret = fauna_client.secret_bytes();
        upload_btn.connect_clicked(move |_| {
            let typed = file_entry.text().to_string();
            let picked = match upload_path_of(&typed) {
                Ok(p) => p,
                Err(message) => {
                    // Never silence: pressing Upload with an empty box used to
                    // do nothing at all, which reads as a broken button (the
                    // live-user report that produced tui's identical fix).
                    // Surfaced BARE, not through `show_upload_glue_error` — no
                    // upload was ever attempted, so wrapping this in the
                    // `media.error_upload` "Failed to upload:" banner would
                    // misstate what happened (tui and windows both surface it
                    // bare too; row 29).
                    crate::settings::render_error_label(&ctx.error_label, Some(message));
                    return;
                }
            };
            let raw_bytes = match std::fs::read(picked) {
                Ok(b) => b,
                Err(e) => {
                    show_upload_glue_error(&ctx.error_label, &e.to_string());
                    return;
                }
            };
            let device_id = match crate::sync::device_id() {
                Ok(id) => fauna_core::hex32::encode(&id),
                Err(e) => {
                    show_upload_glue_error(&ctx.error_label, &e.to_string());
                    return;
                }
            };
            // The member path within the set is the picked file's name.
            let member_path = std::path::Path::new(picked)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| picked.to_string());
            let backup_key = BackupKey::derive(&secret).to_bytes().to_vec();
            let machine = Arc::clone(&machine);
            handle.spawn(async move {
                machine
                    .upload_selected(device_id, member_path, raw_bytes, backup_key)
                    .await;
            });
        });
    }

    // Refresh off WS-RPC whenever the page becomes visible (`connect_map` fires
    // on a real nav to Media — Feed is the stack's initial child, so this never
    // fires spuriously at construction).
    {
        let machine = Arc::clone(&machine);
        let handle = fauna_client.runtime_handle();
        let ctx = Rc::clone(&ctx);
        content.connect_map(move |_| {
            let machine = Arc::clone(&machine);
            let backup_key = ctx.backup_key.clone();
            handle.spawn(async move { machine.refresh(Some(backup_key)).await });
        });
    }

    let handles = MediaHandles {
        media_machine: Arc::clone(&machine),
        media_page: content.clone(),
    };
    (content, handles)
}

/// Is the Media page the one currently on screen?
///
/// Reads GTK's own map state rather than any bookkeeping of ours — `is_mapped()`
/// is precisely the condition `content.connect_map` above fires on, so the
/// push-driven refresh and the become-visible refresh are gated on one fact
/// instead of two that could drift. The `expanded_folder_row_where` shape
/// (`views::devices_folders::folders`), applied to a page instead of a row.
pub fn media_page_is_visible(media_page: &gtk::Box) -> bool {
    use gtk::prelude::WidgetExt;
    media_page.is_mapped()
}

/// Re-render the whole page off a fresh `MediaPageSnapshot`.
fn render_page(machine: &Arc<MediaMachine>, ctx: &Rc<RenderCtx>) {
    let snap = machine.snapshot();

    ctx.updating.set(true);

    // View toggle: active = grid; label reflects the current mode.
    ctx.view_toggle.set_active(snap.view_grid);
    ctx.view_toggle.set_label(if snap.view_grid {
        media_strings::VIEW_GRID
    } else {
        media_strings::VIEW_LIST
    });

    // Sort select: reflect the active key.
    select_dropdown_value(&ctx.sort_dropdown, &snap.sort);

    // Filter select: rebuild options (all-media sentinel + the readable sets
    // the client knows, empty ones included), then reflect the active filter.
    let mut filter_values: Vec<&str> = vec![FILTER_ALL_VALUE];
    filter_values.extend(snap.folders.iter().map(String::as_str));
    // The followed browse scopes ride AFTER the own-set options
    // (`ui/media.md` § Followed public folders). Both halves are shared-Rust
    // minted: the value is opaque and disjoint from every set name, the label
    // already carries the owner disambiguator. Refresh the label side-map
    // BEFORE swapping the model, so the expression can resolve the new values
    // the moment GTK asks.
    {
        let mut labels = ctx.filter_labels.borrow_mut();
        labels.clear();
        for f in &snap.followed {
            labels.insert(f.value.clone(), f.label.clone());
        }
    }
    filter_values.extend(snap.followed.iter().map(|f| f.value.as_str()));
    ctx.filter_dropdown
        .set_model(Some(&gtk::StringList::new(&filter_values)));
    let active_filter = snap.filter.as_deref().unwrap_or(FILTER_ALL_VALUE);
    select_dropdown_value(&ctx.filter_dropdown, active_filter);

    // A followed browse scope is READ-ONLY, structurally: the upload affordance
    // is ABSENT rather than painted-but-inert, because a follow never enters
    // `known_folders` and so can never be an upload target (`ui/media.md`
    // § Followed public folders — the same gating tui applies to its own upload
    // gesture).
    let followed_scope_active = snap.followed_scope.is_some();
    ctx.upload_row.set_visible(!followed_scope_active);

    ctx.updating.set(false);

    // Items + empty state.
    item::update_media_list(
        &ctx.items_flow,
        &snap.items,
        snap.view_grid,
        machine,
        &ctx.backup_key,
        &ctx.runtime,
    );
    let has_items = !snap.items.is_empty();
    ctx.items_scroll.set_visible(has_items);
    // `media-empty-state` paints only once a refresh has actually RETURNED —
    // `items` alone can't tell "genuinely empty" from "still loading" (both
    // start empty). See `MediaPageSnapshot::loaded`'s doc comment for the
    // three-state table.
    ctx.empty_status.set_visible(snap.loaded && !has_items);

    // Page error.
    let text = snap.error.as_ref().map(|err| err.resolve(strings::lookup));
    crate::settings::render_error_label(&ctx.error_label, text.as_deref());

    // The share surfaces follow their halves of the snapshot.
    ctx.share.render(&snap);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// Pressing Upload with an empty path box must SAY so, not no-op.
    ///
    /// The linux peer of tui's `an_upload_with_an_empty_path_says_so_instead_of_
    /// no_opping`. A live user pressed tui's Upload, saw nothing happen, and
    /// reported the feature as missing entirely; linux had the identical silent
    /// `return`. Whitespace counts as empty — it is what a stray space in the
    /// box looks like to a user who thinks they typed a path.
    #[test]
    fn an_upload_with_an_empty_path_says_so_instead_of_no_opping() {
        assert_eq!(
            upload_path_of("   "),
            Err(media_strings::FILE_REQUIRED),
            "an all-whitespace box must answer with the picker-flavoured prompt"
        );
        assert_eq!(upload_path_of(""), Err(media_strings::FILE_REQUIRED));
        assert_eq!(
            upload_path_of("  /tmp/photo.png  "),
            Ok("/tmp/photo.png"),
            "a real path is trimmed and passed through"
        );
    }

    /// Row 29: the empty-path guard's message must reach `error-message` BARE,
    /// never through `show_upload_glue_error`'s `media.error_upload` "Failed to
    /// upload:" wrapping — no upload was ever attempted, so that prefix
    /// misstates what happened (tui and windows both surface it bare).
    /// Doesn't simulate the click itself (that needs a live `MediaMachine` +
    /// `fauna_client`, not cheap to stand up for a 3-line control-flow fix) —
    /// pins the two candidate renderings' shapes are distinct instead, so a
    /// regression that re-routes the empty-path arm through the wrapper
    /// changes what this test would have to assert against.
    #[test]
    fn empty_path_message_is_never_the_upload_failed_wrapping() {
        let bare = upload_path_of("").expect_err("empty path must be an Err");
        assert!(
            !bare.starts_with("Failed to upload:"),
            "the bare guard message must not carry the upload-failed prefix: {bare:?}"
        );
        let wrapped = LocalizedText::key_arg("media.error_upload", "message", bare.to_string())
            .resolve(strings::lookup);
        assert!(
            wrapped.starts_with("Failed to upload:"),
            "sanity: show_upload_glue_error's own wrapping must still say \
             'Failed to upload:' for OTHER (real) upload errors — this proves \
             the two renderings are genuinely different shapes, not that the \
             wrapper broke: {wrapped:?}"
        );
        assert_ne!(
            bare, wrapped,
            "the empty-path message must not equal the wrapped form"
        );
    }

    /// A followed browse scope's option must DISPLAY its label and never its
    /// value (`ui/media.md` § Followed public folders: the value is opaque, the
    /// label already carries the owner disambiguator that tells a follow apart
    /// from a same-named set of the user's own).
    ///
    /// The third case is the one a leg gets wrong: an own set's value IS its
    /// name, so it must pass through untouched — a map lookup that fell back to
    /// the sentinel label, or that rendered every option through the map, would
    /// blank the ordinary browse.
    #[test]
    fn a_followed_scopes_option_shows_its_label_not_its_opaque_value() {
        let mut labels = std::collections::HashMap::new();
        labels.insert(
            "followed:7@".to_string(),
            "holiday-pics (alice)".to_string(),
        );

        assert_eq!(
            filter_display_label("followed:7@", &labels),
            "holiday-pics (alice)",
            "a followed scope renders its minted label",
        );
        assert_eq!(
            filter_display_label(FILTER_ALL_VALUE, &labels),
            media_strings::FILTER_ALL,
            "the all-media sentinel keeps its localized label",
        );
        assert_eq!(
            filter_display_label("my-photos", &labels),
            "my-photos",
            "an own set's value IS its name and passes through",
        );
    }

    /// The Media explorer page exposes its always-present ui.yaml `media` IDs:
    /// the chrome (`page-heading`, `error-message`, `media-view-toggle`,
    /// `media-sort-select`, `media-folder-filter`) + the upload affordance
    /// (`file-upload`, `upload-button`). The dynamic indexed `media-item` rows +
    /// AT-SPI discoverability are exercised by the cross-app E2E suite
    /// (`test_media.py`).
    #[test]
    fn media_shell_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            // Build just the static chrome (no machine/runtime needed) so the test
            // stays in-process: mirror build_media_view's header construction.
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            header.append(&page_heading(media_strings::TITLE));
            let vt = gtk::ToggleButton::builder()
                .label(media_strings::VIEW_LIST)
                .build();
            set_test_id(&vt, ids::MEDIA_VIEW_TOGGLE);
            header.append(&vt);
            header.append(&build_sort_dropdown());
            header.append(&build_filter_dropdown(Rc::new(RefCell::new(
                std::collections::HashMap::new(),
            ))));
            let entry = gtk::Entry::new();
            set_test_id(&entry, ids::FILE_UPLOAD);
            header.append(&entry);
            let btn = gtk::Button::new();
            set_test_id(&btn, ids::UPLOAD_BUTTON);
            header.append(&btn);
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&header);
            root.append(&error_label());

            let names = widget_names(&root);
            for id in [
                "page-heading",
                "error-message",
                "media-view-toggle",
                "media-sort-select",
                "media-folder-filter",
                "file-upload",
                "upload-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
