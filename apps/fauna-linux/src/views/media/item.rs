//! The indexed `media-item` component — one row (list view) or tile (grid view)
//! in the cross-set Media explorer, plus the `update_media_list` re-render that
//! rewrites the whole `gtk::FlowBox` off a fresh `MediaPageSnapshot`.
//!
//! Each card is the direct FlowBox child and carries the `media-item` test id
//! (no index suffix — the e2e driver selects by index when several match); its
//! children carry `media-item-name` / `media-item-size` / `media-item-date` /
//! `media-thumbnail` / `media-source-status` (ui.yaml `media-item` component).
//! Cross-set aggregation + sort + filter already ran in shared Rust
//! (`MediaSnapshot::view`); this file is pure renderer (`media.md` rule 2).

use fauna_ui_ids as ids;
use std::sync::Arc;

use adw::prelude::*;

use fauna_media_machine::{MediaItemSummary, MediaMachine};

use crate::i18n::strings::media as media_strings;
use crate::testid::set_test_id;

/// Build one `media-item` card. `grid` picks the tile (vertical, thumbnail-led)
/// vs. list (horizontal row) layout; both expose the same child ids.
///
/// The `media-thumbnail` starts as a placeholder icon; when the item carries a
/// `thumbnail_hash`, the real thumbnail is fetched + decrypted through the
/// shared `MediaMachine::fetch_thumbnail` and painted over the placeholder
/// (`machine` / `backup_key` / `runtime` are the client glue that seam needs).
/// A `None` hash or any fetch/decode error keeps the placeholder.
pub fn build_media_item(
    item: &MediaItemSummary,
    grid: bool,
    machine: &Arc<MediaMachine>,
    backup_key: &[u8],
    runtime: &tokio::runtime::Handle,
) -> gtk::Button {
    let thumbnail = gtk::Image::from_icon_name("image-x-generic-symbolic");
    if grid {
        thumbnail.set_pixel_size(96);
    } else {
        thumbnail.set_pixel_size(32);
    }
    set_test_id(&thumbnail, ids::MEDIA_THUMBNAIL);

    // Real thumbnail: when the item carries a `thumbnail_hash`, fetch + decrypt
    // it through the shared `MediaMachine::fetch_thumbnail` (GET direct-by-hash,
    // content-address verify, owner-`BackupKey` decrypt — all shared Rust,
    // priority #2) on the tokio runtime, hand the decoded bytes back to the GTK
    // main loop, and paint them over the placeholder icon (the feed
    // `build_post_image` idiom). Per-item + per-render (the FlowBox rebuilds each
    // tick); a `None` hash or any fetch/decode error keeps the placeholder — one
    // unreadable thumbnail must never blank the tile (`media.md` § Thumbnails).
    if let Some(hash) = item.thumbnail_hash.clone() {
        let (tx, rx) = async_channel::bounded(1);
        let machine = Arc::clone(machine);
        let backup_key = backup_key.to_vec();
        runtime.spawn(async move {
            let _ = tx
                .send(machine.fetch_thumbnail(hash, backup_key).await)
                .await;
        });
        let thumb = thumbnail.clone();
        gtk::glib::spawn_future_local(async move {
            if let Ok(Ok(bytes)) = rx.recv().await {
                paint_thumbnail(&thumb, &bytes);
            }
        });
    }

    let name = gtk::Label::builder()
        .label(&item.name)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["heading"])
        .build();
    set_test_id(&name, ids::MEDIA_ITEM_NAME);

    let size = gtk::Label::builder()
        .label(crate::i18n::byte_size(item.size_bytes.max(0) as u64))
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&size, ids::MEDIA_ITEM_SIZE);

    // `updated_at` is unix **seconds**; `format_epoch_us` takes microseconds.
    let date = gtk::Label::builder()
        .label(crate::client::format_epoch_us(
            item.updated_at.saturating_mul(1_000_000),
        ))
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(&date, ids::MEDIA_ITEM_DATE);

    // Source folder online/offline liveness (distinct from a file's sync-state
    // badge — `media.md` § Source status vs. sync state).
    let (status_text, status_css) = if item.source_online {
        (media_strings::SOURCE_ONLINE, "success")
    } else {
        (media_strings::SOURCE_OFFLINE, "error")
    };
    let source_status = gtk::Label::builder()
        .label(status_text)
        .halign(gtk::Align::Start)
        .css_classes(["caption", status_css])
        .build();
    set_test_id(&source_status, ids::MEDIA_SOURCE_STATUS);

    // This file's own presence — `file-sync.md` § Per-file sync-status display.
    // linux's media page is a control-plane surface (`fauna.sync.files` carries
    // no per-file status; `client.rs::fetch_sync_files` stamps every file
    // `synced`), so it renders only the `Synced` state — the class split the
    // goal doc sanctions for clients with no local sync engine. The label comes
    // from the shared `sync_display_state_label`, never a hand-written string.
    let sync_label =
        fauna_core::format::sync_display_state_label(fauna_core::format::SyncDisplayState::Synced)
            .resolve(crate::i18n::strings::lookup);
    let sync_badge = gtk::Label::builder()
        .label(&sync_label)
        .halign(gtk::Align::Start)
        .css_classes(["caption", "success"])
        .build();
    set_test_id(&sync_badge, ids::SYNC_STATE_BADGE);

    let inner = if grid {
        let v = gtk::Box::new(gtk::Orientation::Vertical, 4);
        v.set_width_request(160);
        v.append(&thumbnail);
        v.append(&name);
        v.append(&size);
        v.append(&date);
        v.append(&source_status);
        v.append(&sync_badge);
        v
    } else {
        let h = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        name.set_hexpand(true);
        h.append(&thumbnail);
        h.append(&name);
        h.append(&size);
        h.append(&date);
        h.append(&source_status);
        h.append(&sync_badge);
        h
    };
    // The card is an activatable flat Button (not a plain Box): media-item
    // tap/open opens the `media-item-detail` surface (`media.md` § User
    // actions), and the automation agent actuates clicks via
    // `widget.activate()`, which a Box would ignore.
    let card = gtk::Button::builder().child(&inner).build();
    card.add_css_class("flat");
    card.set_margin_top(8);
    card.set_margin_bottom(8);
    card.set_margin_start(12);
    card.set_margin_end(12);
    set_test_id(&card, ids::MEDIA_ITEM);
    crate::offline_gate::declare_wire_kind(&card, "fauna.files.versions.list");
    {
        let item = item.clone();
        let machine = Arc::clone(machine);
        let backup_key = backup_key.to_vec();
        let runtime = runtime.clone();
        card.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            super::detail::open_item_detail(
                parent.as_ref(),
                &item,
                &machine,
                &backup_key,
                &runtime,
            );
        });
    }
    card
}

/// Paint decoded thumbnail `bytes` (the JPEG/PNG the shared
/// `MediaMachine::fetch_thumbnail` returns) over the placeholder icon in
/// `image`, replacing it with the real texture. Returns `false` — leaving the
/// placeholder untouched — when the bytes don't decode to a paintable, the
/// per-item degrade `media.md` § Thumbnails requires (a bad thumbnail must not
/// blank the tile). Shared with the unit test.
pub(crate) fn paint_thumbnail(image: &gtk::Image, bytes: &[u8]) -> bool {
    match gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(bytes)) {
        Ok(texture) => {
            image.set_paintable(Some(&texture));
            true
        }
        Err(_) => false,
    }
}

/// Rewrite the whole item `FlowBox` off the snapshot's already-filtered+sorted
/// items. `grid` toggles the layout: list = one item per line (a detail list),
/// grid = wrapped tiles (`media-view-toggle`). `machine` / `backup_key` /
/// `runtime` thread through to each card's lazy `media-thumbnail` fetch.
pub fn update_media_list(
    flow: &gtk::FlowBox,
    items: &[MediaItemSummary],
    grid: bool,
    machine: &Arc<MediaMachine>,
    backup_key: &[u8],
    runtime: &tokio::runtime::Handle,
) {
    while let Some(child) = flow.first_child() {
        flow.remove(&child);
    }

    if grid {
        flow.set_min_children_per_line(2);
        flow.set_max_children_per_line(8);
    } else {
        flow.set_min_children_per_line(1);
        flow.set_max_children_per_line(1);
    }

    for item in items {
        flow.append(&build_media_item(item, grid, machine, backup_key, runtime));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid minimal 1×1 grayscale PNG — stands in for a decoded thumbnail
    /// blob the shared `MediaMachine::fetch_thumbnail` hands back.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x3a,
        0x7e, 0x9b, 0x55, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0x0f, 0x00, 0x01, 0x01, 0x01, 0x00, 0xb1, 0x38, 0xf6, 0x14, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// `paint_thumbnail` swaps the placeholder icon for a real texture when
    /// handed decodable image bytes (returns `true`, the image now backed by a
    /// paintable), and leaves the placeholder intact on undecodable bytes
    /// (returns `false`, no paintable) — so one bad thumbnail never blanks the
    /// tile (`media.md` § Thumbnails per-item degrade).
    #[test]
    fn paint_thumbnail_swaps_valid_bytes_and_rejects_garbage() {
        crate::testid::run_on_gtk_thread(|| {
            let image = gtk::Image::from_icon_name("image-x-generic-symbolic");
            // Placeholder icon: no paintable backing yet.
            assert!(image.paintable().is_none());

            // Garbage bytes: rejected, placeholder retained.
            assert!(!paint_thumbnail(&image, b"not an image"));
            assert!(image.paintable().is_none());

            // Valid PNG: painted, now backed by a texture paintable.
            assert!(paint_thumbnail(&image, TINY_PNG));
            assert!(image.paintable().is_some());
        });
    }
}
