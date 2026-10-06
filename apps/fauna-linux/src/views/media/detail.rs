//! The `media-item-detail` surface + its `file-version-history` component
//! (ui.yaml `media` page, approved 2026-07-09) — opened by `media-item`
//! tap/open (`docs/goal/ui/media.md` § User actions / § Element IDs).
//!
//! Renders the per-file version history off the shared
//! `MediaMachine::file_versions` (oldest→newest; every recorded change IS a
//! version — `docs/goal/behavior/file-sync.md` § File Versions) and drives
//! restore through the shared `MediaMachine::restore_version` gesture behind
//! the lightweight `file-version-restore-confirm-modal` (restore is
//! reversible — it appends a new version — so no irreversible-action ceremony;
//! file-sync.md § Restore). All semantics live in shared Rust; this file is
//! pure renderer + the device-id glue, mirroring the page (`media.md` rule 2).
//!
//! `media-item-detail-download-button` (approved 2026-09-25, `media.md`
//! § Element IDs) runs the shared `MediaMachine::download_file` walk keyed by
//! the latest version row — a shared set opens under the content keys the
//! machine's `NestFolderKeyResolver` reads from this actor's custody, an
//! owner-only set under the owner `BackupKey` — or, in a followed browse scope,
//! the keyless `download_followed`. The only client glue is the save path: the
//! native `gtk::FileDialog`, bypassed under e2e into `FAUNA_E2E_DOWNLOAD_DIR`
//! exactly as the backups single-file download is (`views/backups/file_list.rs`).
//! A followed item's detail offers download alone — the public plane is
//! head-only, so no version history, restore or delete (`media.md` § Followed
//! public folders).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;

use fauna_media_machine::{FileVersionSummary, MediaItemSummary, MediaMachine};

use crate::i18n::strings;
use crate::i18n::strings::media as media_strings;
use crate::testid::set_test_id;

/// Everything the detail window needs to (re)load and restore versions.
struct DetailCtx {
    machine: Arc<MediaMachine>,
    runtime: tokio::runtime::Handle,
    folder: String,
    path: String,
    list: gtk::Box,
    status: gtk::Label,
    /// The display name — the save path's file name.
    name: String,
    /// The owner 32-byte `BackupKey` the download walk takes for an owner-only
    /// set (a shared set's chunks open under content keys from custody).
    backup_key: Vec<u8>,
    /// The active followed browse scope's value, read once at open: `Some`
    /// routes the download to the keyless `download_followed` and withholds
    /// every version/restore/delete affordance.
    followed_scope: Option<String>,
    /// `media-item-detail-download-button` — painted once it is actionable (a
    /// manifest known from the version rows, or at once in a followed scope),
    /// never inert.
    download: gtk::Button,
    /// The newest version row (rows are oldest→newest, so the last one is the
    /// current file) — the manifest the download walk is keyed by.
    latest: RefCell<Option<FileVersionSummary>>,
    /// The `file-version-show-pruned-toggle` recovery browse
    /// (`file-versions.md` § Retention (3), apps row 323): ON re-lists with
    /// `include_pruned`, so soft-pruned rows appear with their badge +
    /// undelete button.
    show_pruned: Cell<bool>,
}

/// Open the modal `media-item-detail` window for `item`. `parent` is the main
/// window (transient-for + the sign-out close-all sweep, the lightbox idiom).
pub fn open_item_detail(
    parent: Option<&gtk::Window>,
    item: &MediaItemSummary,
    machine: &Arc<MediaMachine>,
    backup_key: &[u8],
    runtime: &tokio::runtime::Handle,
) {
    // The machine says whether this item belongs to a followed browse scope —
    // asked at open, never parsed (tui's `followed_scope_value` idiom).
    let followed_scope = machine.snapshot().followed_scope.map(|s| s.value);
    let followed = followed_scope.is_some();

    let window = adw::Window::builder()
        .title(&item.name)
        .modal(true)
        .default_width(480)
        .default_height(420)
        .build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
        if let Some(app) = p.application() {
            window.set_application(Some(&app));
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.set_margin_top(12);
    outer.set_margin_bottom(12);
    outer.set_margin_start(12);
    outer.set_margin_end(12);
    set_test_id(&outer, ids::MEDIA_ITEM_DETAIL);

    let name = gtk::Label::builder()
        .label(&item.name)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["title-2"])
        .build();
    set_test_id(&name, ids::MEDIA_ITEM_DETAIL_NAME);
    outer.append(&name);

    let versions_heading = gtk::Label::builder()
        .label(media_strings::VERSIONS_TITLE)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    outer.append(&versions_heading);

    // The recovery browse switch (`file-versions.md` § Retention (3)) — ON
    // re-lists with `include_pruned`, so soft-pruned rows appear below with
    // their badge + undelete button.
    let show_pruned_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let show_pruned_label = gtk::Label::builder()
        .label(media_strings::VERSIONS_SHOW_PRUNED)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    let show_pruned_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    set_test_id(&show_pruned_switch, ids::FILE_VERSION_SHOW_PRUNED_TOGGLE);
    crate::offline_gate::declare_wire_kind(&show_pruned_switch, "fauna.files.versions.list");
    show_pruned_row.append(&show_pruned_label);
    show_pruned_row.append(&show_pruned_switch);
    outer.append(&show_pruned_row);

    // The version rows container (`file-version-list`) + a status line for the
    // loading / load-error states (per-surface, never the page banner).
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    set_test_id(&list, ids::FILE_VERSION_LIST);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    outer.append(&scroll);

    let status = gtk::Label::builder()
        .label(media_strings::VERSIONS_LOADING)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    outer.append(&status);

    // Delete (start) + Close (end). `destructive-action` is the Adwaita idiom for
    // an affordance that removes user content — the visual counterpart to the
    // confirm modal the click opens.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    // "Share a link" — present ONLY on an eligible file (the machine's
    // `share_link_eligible` verdict), absent elsewhere; its create surface is a
    // window over this one (`super::share`, `share-links.md` § Flows → Create).
    if let Some(share) =
        super::share::detail_button(item.share_link_eligible, machine, &item.folder, &item.path)
    {
        actions.append(&share);
    }
    let delete = gtk::Button::with_label(media_strings::file_detail::DELETE_FILE);
    delete.add_css_class("destructive-action");
    delete.set_halign(gtk::Align::Start);
    delete.set_hexpand(true);
    set_test_id(&delete, ids::MEDIA_DELETE_BUTTON);
    actions.append(&delete);

    let download = gtk::Button::with_label(media_strings::DOWNLOAD);
    download.add_css_class("suggested-action");
    download.set_halign(gtk::Align::End);
    set_test_id(&download, ids::MEDIA_ITEM_DETAIL_DOWNLOAD_BUTTON);
    download.set_visible(followed);
    actions.append(&download);

    let close = gtk::Button::with_label(media_strings::DETAIL_CLOSE);
    close.set_halign(gtk::Align::End);
    set_test_id(&close, ids::MEDIA_ITEM_DETAIL_CLOSE_BUTTON);
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }
    actions.append(&close);
    outer.append(&actions);

    // A followed item is head-only: no version history, no recovery browse,
    // no delete — the detail offers download alone. Hidden widgets leave the
    // mapped tree, so they are absent to AT-SPI too (the upload row's idiom).
    if followed {
        for widget in [
            versions_heading.upcast_ref::<gtk::Widget>(),
            show_pruned_row.upcast_ref(),
            scroll.upcast_ref(),
            delete.upcast_ref(),
            status.upcast_ref(),
        ] {
            widget.set_visible(false);
        }
        // Download takes the row's start edge Delete would have held.
        download.set_hexpand(true);
    }

    window.set_content(Some(&outer));
    window.present();

    let ctx = Rc::new(DetailCtx {
        machine: Arc::clone(machine),
        runtime: runtime.clone(),
        folder: item.folder.clone(),
        path: item.path.clone(),
        list,
        status,
        show_pruned: Cell::new(false),
        name: item.name.clone(),
        backup_key: backup_key.to_vec(),
        followed_scope,
        download: download.clone(),
        latest: RefCell::new(None),
    });
    {
        let ctx = Rc::clone(&ctx);
        download.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            start_download(parent.as_ref(), &ctx);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        let name = item.name.clone();
        let detail_window = window.clone();
        delete.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            open_delete_confirm(parent.as_ref(), &ctx, &name, &detail_window);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        show_pruned_switch.connect_active_notify(move |sw| {
            ctx.show_pruned.set(sw.is_active());
            load_versions(&ctx);
        });
    }
    // The public plane has no version rows to read.
    if !followed {
        load_versions(&ctx);
    }
}

/// The save file name for `name`: its last path component, so a name that
/// carries a separator can never steer the write outside the chosen directory.
fn save_file_name(name: &str) -> String {
    std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("download")
        .to_string()
}

/// `media-item-detail-download-button`: pick where the file goes, then download
/// into it. Under e2e automation the native picker is bypassed and the file lands
/// in the harness-provided directory — the backups single-file download's seam
/// (`views/backups/file_list.rs`), behind the same two gates: the `#[cfg]` is
/// convention 15's outer boundary and `e2e_mode_enabled()` the inner switch.
fn start_download(parent: Option<&gtk::Window>, ctx: &Rc<DetailCtx>) {
    let file_name = save_file_name(&ctx.name);

    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if crate::e2e_mode_enabled()
        && let Some(dir) = std::env::var_os("FAUNA_E2E_DOWNLOAD_DIR").filter(|v| !v.is_empty())
    {
        download_to(ctx, std::path::Path::new(&dir).join(&file_name));
        return;
    }

    let dialog = gtk::FileDialog::builder()
        .title(media_strings::DOWNLOAD)
        .initial_name(file_name.as_str())
        .build();
    let ctx = Rc::clone(ctx);
    dialog.save(parent, gio::Cancellable::NONE, move |result| {
        if let Ok(gfile) = result
            && let Some(path) = gfile.path()
        {
            download_to(&ctx, path);
        }
    });
}

/// Run the shared download query and write the plaintext to `target`. A failure
/// lands on the detail's own status line (`media.error_download`) — the page
/// banner sits behind this modal, where the user who asked could not read it
/// (the delete failure's reasoning).
fn download_to(ctx: &Rc<DetailCtx>, target: PathBuf) {
    let latest = ctx.latest.borrow().clone();
    if ctx.followed_scope.is_none() && latest.is_none() {
        return; // the button paints only once a manifest is known
    }
    ctx.download.set_sensitive(false);

    let (tx, rx) = async_channel::bounded(1);
    {
        let machine = Arc::clone(&ctx.machine);
        let followed_scope = ctx.followed_scope.clone();
        let folder = ctx.folder.clone();
        let path = ctx.path.clone();
        let backup_key = ctx.backup_key.clone();
        ctx.runtime.spawn(async move {
            let bytes = match (followed_scope, latest) {
                (Some(value), _) => machine.download_followed(value, path).await,
                (None, Some(version)) => {
                    machine
                        .download_file(
                            version.manifest_hash,
                            version.content_key_version,
                            folder,
                            path,
                            backup_key,
                        )
                        .await
                }
                (None, None) => unreachable!("guarded before the spawn"),
            };
            let result = match bytes {
                Ok(bytes) => tokio::task::spawn_blocking(move || std::fs::write(&target, bytes))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|written| written.map_err(|e| e.to_string())),
                Err(e) => Err(e.detail().to_string()),
            };
            let _ = tx.send(result).await;
        });
    }
    let ctx = Rc::clone(ctx);
    gtk::glib::spawn_future_local(async move {
        let Ok(result) = rx.recv().await else {
            return; // sender dropped (window closed mid-flight)
        };
        ctx.download.set_sensitive(true);
        if let Err(message) = result {
            tracing::warn!("media download failed: {message}");
            let err = fauna_core::localized::LocalizedText::key_arg(
                "media.error_download",
                "message",
                message,
            );
            ctx.status.set_text(&err.resolve(strings::lookup));
            ctx.status.set_visible(true);
        }
    });
}

/// (Re)load the version history off the shared machine and rebuild the rows.
fn load_versions(ctx: &Rc<DetailCtx>) {
    ctx.status.set_text(media_strings::VERSIONS_LOADING);
    ctx.status.set_visible(true);

    let (tx, rx) = async_channel::bounded(1);
    {
        let machine = Arc::clone(&ctx.machine);
        let folder = ctx.folder.clone();
        let path = ctx.path.clone();
        let include_pruned = ctx.show_pruned.get();
        ctx.runtime.spawn(async move {
            let _ = tx
                .send(machine.file_versions(folder, path, include_pruned).await)
                .await;
        });
    }
    let ctx = Rc::clone(ctx);
    gtk::glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(versions)) => {
                ctx.status.set_visible(false);
                render_versions(&ctx, &versions);
            }
            Ok(Err(e)) => {
                let err = fauna_core::localized::LocalizedText::key_arg(
                    "media.versions_error",
                    "message",
                    e.detail().to_string(),
                );
                ctx.status.set_text(&err.resolve(strings::lookup));
                ctx.status.set_visible(true);
            }
            Err(_) => {} // sender dropped (window closed mid-flight)
        }
    });
}

/// Rebuild the `file-version-list` rows, oldest→newest — one `file-version-item`
/// per version with its timestamp / size / restore affordance.
fn render_versions(ctx: &Rc<DetailCtx>, versions: &[FileVersionSummary]) {
    while let Some(child) = ctx.list.first_child() {
        ctx.list.remove(&child);
    }
    for version in versions {
        ctx.list.append(&build_version_row(ctx, version));
    }
    // The newest row is the current file; the download paints once it exists.
    let latest = versions.last().cloned();
    ctx.download.set_visible(latest.is_some());
    *ctx.latest.borrow_mut() = latest;
}

/// One `file-version-item` row: `file-version-timestamp` + `file-version-size`
/// + `file-version-author` + `file-version-restore-button`.
fn build_version_row(ctx: &Rc<DetailCtx>, version: &FileVersionSummary) -> gtk::Box {
    // `created_at` is epoch **millis** (file-sync.md § File Versions);
    // `format_epoch_us` takes microseconds.
    let timestamp = gtk::Label::builder()
        .label(crate::client::format_epoch_us(
            version.created_at.saturating_mul(1_000),
        ))
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    set_test_id(&timestamp, ids::FILE_VERSION_TIMESTAMP);

    let size = gtk::Label::builder()
        .label(crate::i18n::byte_size(version.size_bytes.max(0) as u64))
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&size, ids::FILE_VERSION_SIZE);

    let author_label = gtk::Label::builder()
        .label(media_strings::version_author(&version.author_display))
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&author_label, ids::FILE_VERSION_AUTHOR);

    // A soft-pruned row (only an `include_pruned` listing carries one) says so
    // and offers its recovery verb — badge + undelete, present ONLY on pruned
    // rows.
    let pruned_widgets = version.pruned.then(|| {
        let badge = gtk::Label::builder()
            .label(media_strings::VERSION_PRUNED_BADGE)
            .css_classes(["dim-label", "caption"])
            .build();
        set_test_id(&badge, ids::FILE_VERSION_PRUNED_BADGE);

        let undelete = gtk::Button::with_label(media_strings::VERSION_UNDELETE);
        set_test_id(&undelete, ids::FILE_VERSION_UNDELETE_BUTTON);
        crate::offline_gate::declare_wire_kind(&undelete, "fauna.files.versions.undelete");
        {
            let ctx = Rc::clone(ctx);
            let version_num = version.version_num;
            undelete.connect_clicked(move |_| {
                let (tx, rx) = async_channel::bounded(1);
                {
                    let machine = Arc::clone(&ctx.machine);
                    let path = ctx.path.clone();
                    ctx.runtime.spawn(async move {
                        let _ = tx
                            .send(machine.undelete_version(path, version_num).await)
                            .await;
                    });
                }
                let ctx = Rc::clone(&ctx);
                gtk::glib::spawn_future_local(async move {
                    match rx.recv().await {
                        Ok(Ok(())) => load_versions(&ctx),
                        Ok(Err(e)) => {
                            let err = fauna_core::localized::LocalizedText::key_arg(
                                "media.error_undelete",
                                "message",
                                e.detail().to_string(),
                            );
                            ctx.status.set_text(&err.resolve(strings::lookup));
                            ctx.status.set_visible(true);
                        }
                        Err(_) => {} // sender dropped (window closed mid-flight)
                    }
                });
            });
        }
        (badge, undelete)
    });

    let restore = gtk::Button::with_label(media_strings::VERSION_RESTORE);
    set_test_id(&restore, ids::FILE_VERSION_RESTORE_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let version = version.clone();
        restore.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            open_restore_confirm(parent.as_ref(), &ctx, &version);
        });
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&timestamp);
    row.append(&size);
    row.append(&author_label);
    if let Some((badge, undelete)) = &pruned_widgets {
        row.append(badge);
        row.append(undelete);
    }
    row.append(&restore);
    row.set_accessible_role(gtk::AccessibleRole::Group);
    set_test_id(&row, ids::FILE_VERSION_ITEM);
    row
}

/// The lightweight `file-version-restore-confirm-modal`: restore propagates to
/// every device but is reversible (the pre-restore head stays restorable), so a
/// single confirm — not the backups immediate-delete ceremony (media.md
/// § Element IDs).
fn open_restore_confirm(
    parent: Option<&gtk::Window>,
    ctx: &Rc<DetailCtx>,
    version: &FileVersionSummary,
) {
    let modal = adw::Window::builder()
        .title(media_strings::RESTORE_CONFIRM_TITLE)
        .modal(true)
        .default_width(360)
        .build();
    if let Some(p) = parent {
        modal.set_transient_for(Some(p));
        if let Some(app) = p.application() {
            modal.set_application(Some(&app));
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.set_margin_top(12);
    outer.set_margin_bottom(12);
    outer.set_margin_start(12);
    outer.set_margin_end(12);
    set_test_id(&outer, ids::FILE_VERSION_RESTORE_CONFIRM_MODAL);

    let title = gtk::Label::builder()
        .label(media_strings::RESTORE_CONFIRM_TITLE)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    outer.append(&title);
    let body = gtk::Label::builder()
        .label(media_strings::RESTORE_CONFIRM_BODY)
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    outer.append(&body);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label(media_strings::RESTORE_CANCEL);
    set_test_id(&cancel, ids::FILE_VERSION_RESTORE_CANCEL_BUTTON);
    {
        let modal = modal.clone();
        cancel.connect_clicked(move |_| modal.close());
    }
    let confirm = gtk::Button::with_label(media_strings::RESTORE_CONFIRM);
    confirm.add_css_class("suggested-action");
    set_test_id(&confirm, ids::FILE_VERSION_RESTORE_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(&confirm, "fauna.sync.changes.record");
    {
        let ctx = Rc::clone(ctx);
        let version = version.clone();
        let modal = modal.clone();
        confirm.connect_clicked(move |_| {
            modal.close();
            // The device-id derivation is the only client glue (the upload
            // gesture's idiom); a failure surfaces on the detail status line.
            let device_id = match crate::sync::device_id() {
                Ok(id) => fauna_core::hex32::encode(&id),
                Err(e) => {
                    let err = fauna_core::localized::LocalizedText::key_arg(
                        "media.error_restore",
                        "message",
                        e.to_string(),
                    );
                    ctx.status.set_text(&err.resolve(strings::lookup));
                    ctx.status.set_visible(true);
                    return;
                }
            };
            let (tx, rx) = async_channel::bounded(1);
            {
                let machine = Arc::clone(&ctx.machine);
                let folder = ctx.folder.clone();
                let path = ctx.path.clone();
                let version = version.clone();
                ctx.runtime.spawn(async move {
                    machine
                        .restore_version(folder, device_id, path, version)
                        .await;
                    let _ = tx.send(()).await;
                });
            }
            // When the restore lands (machine.refresh repaints the page), the
            // open detail re-loads its rows — the restore appears as the new
            // head version (a restore failure set the page error; the reload
            // then just re-renders the unchanged history).
            let ctx = Rc::clone(&ctx);
            gtk::glib::spawn_future_local(async move {
                if rx.recv().await.is_ok() {
                    load_versions(&ctx);
                }
            });
        });
    }
    buttons.append(&cancel);
    buttons.append(&confirm);
    outer.append(&buttons);

    modal.set_content(Some(&outer));
    modal.present();
}

/// The lightweight `media-delete-confirm-modal`: a delete propagates to every
/// device, so it is confirmed — but with a **single confirm**, deliberately not
/// the backups typed-id immediate-delete ceremony (ui.yaml `media-item-detail`,
/// user-approved 2026-07-16). A media delete records a *tombstone* and leaves the
/// historical version rows, and backup destinations do not forward deletes at
/// all (`file-sync.md` § File Versions / § Destination modes), so the heavier
/// ceremony would be miscalibrated to the risk. Mirrors `open_restore_confirm`.
fn open_delete_confirm(
    parent: Option<&gtk::Window>,
    ctx: &Rc<DetailCtx>,
    name: &str,
    detail_window: &adw::Window,
) {
    let modal = adw::Window::builder()
        .title(media_strings::file_detail::DELETE_CONFIRM_TITLE)
        .modal(true)
        .default_width(360)
        .build();
    if let Some(p) = parent {
        modal.set_transient_for(Some(p));
        if let Some(app) = p.application() {
            modal.set_application(Some(&app));
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.set_margin_top(12);
    outer.set_margin_bottom(12);
    outer.set_margin_start(12);
    outer.set_margin_end(12);
    set_test_id(&outer, ids::MEDIA_DELETE_CONFIRM_MODAL);

    let title = gtk::Label::builder()
        .label(media_strings::file_detail::DELETE_CONFIRM_TITLE)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    outer.append(&title);
    // The body names the file — the shared `{name}`-arg string, never a
    // hand-assembled sentence (i18n placeholders are named, not positional).
    let body = gtk::Label::builder()
        .label(media_strings::file_detail::delete_confirm(name))
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    outer.append(&body);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    // Cancel is a PURE no-op — close the modal, touch nothing (the contract the
    // sibling restore/re-auth confirm modals hold; pinned by
    // `test_media_delete_cancel_is_a_no_op`).
    let cancel = gtk::Button::with_label(strings::common::CANCEL);
    set_test_id(&cancel, ids::MEDIA_DELETE_CANCEL_BUTTON);
    {
        let modal = modal.clone();
        cancel.connect_clicked(move |_| modal.close());
    }
    let confirm = gtk::Button::with_label(media_strings::file_detail::DELETE_CONFIRM_BUTTON);
    confirm.add_css_class("destructive-action");
    set_test_id(&confirm, ids::MEDIA_DELETE_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(&confirm, "fauna.sync.changes.record");
    {
        let ctx = Rc::clone(ctx);
        let modal = modal.clone();
        let detail_window = detail_window.clone();
        confirm.connect_clicked(move |_| {
            modal.close();
            // The device-id derivation is the only client glue (the restore
            // gesture's idiom); a failure surfaces on the detail status line
            // rather than silently dropping the gesture.
            let device_id = match crate::sync::device_id() {
                Ok(id) => fauna_core::hex32::encode(&id),
                Err(e) => {
                    let err = fauna_core::localized::LocalizedText::key_arg(
                        "media.error_delete",
                        "message",
                        e.to_string(),
                    );
                    ctx.status.set_text(&err.resolve(strings::lookup));
                    ctx.status.set_visible(true);
                    return;
                }
            };
            let (tx, rx) = async_channel::bounded(1);
            {
                let machine = Arc::clone(&ctx.machine);
                let folder = ctx.folder.clone();
                let path = ctx.path.clone();
                ctx.runtime.spawn(async move {
                    // The shared gesture records the tombstone and refreshes.
                    // `delete` swallows its error into the machine snapshot
                    // rather than returning it, so the snapshot IS the success
                    // signal: the success arm's `refresh()` clears `error`, the
                    // failure arm sets `media.error_delete`.
                    machine.delete(folder, device_id, path).await;
                    let _ = tx.send(machine.snapshot().error.clone()).await;
                });
            }
            let detail_window = detail_window.clone();
            let ctx = Rc::clone(&ctx);
            gtk::glib::spawn_future_local(async move {
                match rx.recv().await {
                    // Deleted: close the surface — its subject is gone, so a
                    // version history for it would be a dangling view. (The
                    // restore path instead reloads: its file still exists.)
                    Ok(None) => detail_window.close(),
                    // Failed: the file is still there, so KEEP the surface open
                    // and show the error on its own status line. The page banner
                    // also carries it, but this window is modal — a banner behind
                    // a modal is a banner the user cannot read, so closing on
                    // failure would be the only way they'd see it, at the cost of
                    // yanking away the context they acted in.
                    Ok(Some(err)) => {
                        ctx.status.set_text(&err.resolve(strings::lookup));
                        ctx.status.set_visible(true);
                    }
                    Err(_) => {} // sender dropped (window closed mid-flight)
                }
            });
        });
    }
    buttons.append(&cancel);
    buttons.append(&confirm);
    outer.append(&buttons);

    modal.set_content(Some(&outer));
    modal.present();
}
