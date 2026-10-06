//! The profile **edit form** (SELF view) per `profile.md` § Where logic lives
//! → *Profile publish/edit*. The user edits `display_name` / `bio` / a
//! repeatable list of links, plus the `avatar`/`banner` pictures; Save runs
//! the shared `fauna-client-profile::build_edited_profile_with_images` (sign)
//! → `ProfileClient::profile_set`.
//!
//! Read-modify-write: opening the form fetches the current `Profile` (via
//! `fauna.profile.get`, through the shared read-prove-record) so a re-edit
//! *preserves* the non-display fields the user doesn't touch here (`inbox_mode` / `nests` / `admin_nests` / `load_hint` /
//! `recovery_head`); a first publish (`not_found`) starts from defaults
//! (publish-on-first-edit). Observer-free: Save calls `on_saved` so the caller
//! re-renders the header (no client-side caching — `feed.md` § Architectural
//! rules).
//!
//! `profile-edit-avatar` / `profile-edit-banner` are real `gtk::FileDialog`
//! triggers (the same idiom as feed's `compose-file` attach button —
//! `views/feed/post_list.rs`), not a typed-path field (tui's shape — tui has
//! no OS file-chooser, `tui.md` § Declared platform absences 4). No e2e harness
//! can drive a real OS dialog, so the picked path is staged as the button's own
//! **label text** — which is exactly what `avatar_path_text()`/
//! `banner_path_text()` reads back — and the test agent's `compose.file` bypass
//! (`main.rs`) sets that same label directly via `automation::find`, mirroring
//! the real dialog callback. A pending removal (`profile-edit-*-remove-button`)
//! is a separate flag (`avatar_clear`/`banner_clear`): resolved at Save time, a
//! freshly staged label always wins over it (picking overrides removing).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::prelude::FileExt;

use fauna_client_profile::{
    ProfileClient, ProfileDisplay, ProfileImageEdit, build_edited_profile_with_images,
    decode_profile_display,
};
use fauna_core::data::ProfileLink;
use fauna_core::identity::ActorKeypair;

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::profile as p;
use crate::testid::set_test_id;

/// One repeatable link row: a label + url entry. The row's container is removed
/// from `link_list` on delete; collection skips rows whose container has been
/// detached (`parent().is_none()`), so no widget-equality bookkeeping is needed.
struct LinkRow {
    container: gtk::Box,
    label: gtk::Entry,
    url: gtk::Entry,
}

struct EditWidgets {
    form: gtk::Box,
    display_name: gtk::Entry,
    bio: gtk::Entry,
    link_list: gtk::Box,
    link_rows: RefCell<Vec<LinkRow>>,
    /// `profile-edit-avatar` — opens a real `gtk::FileDialog`; its own label
    /// doubles as the staged path once a file is picked (`avatar_path_text()`'s
    /// read target). Starts (and resets, on open/Save) at the placeholder text.
    avatar_btn: gtk::Button,
    /// `profile-edit-avatar-remove-button` was tapped — a fresh pick (the label
    /// differing from the placeholder) overrides this at Save (`pending_image`).
    /// `Rc`-wrapped so the file-dialog callback can hold it independent of `ctx`.
    avatar_clear: Rc<Cell<bool>>,
    /// `profile-edit-banner` — same shape as `avatar_btn`.
    banner_btn: gtk::Button,
    /// `profile-edit-banner-remove-button` was tapped.
    banner_clear: Rc<Cell<bool>>,
    /// The page-level `error-message` label, shared from the profile view.
    error_label: gtk::Label,
}

struct EditCtx {
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    w: EditWidgets,
    /// The current profile's raw signed bytes (read-modify-write base) — `None`
    /// until `open` fetches it, or when the actor has never published (first
    /// edit). Kept as raw bytes (not a decoded `Profile`) so `submit` can hand
    /// it straight to `build_edited_profile_with_images`'s read-modify-write.
    base: RefCell<Option<Vec<u8>>>,
    /// Called after a successful publish so the caller refreshes the header.
    on_saved: Rc<dyn Fn()>,
}

/// What one image field's edit-form widgets resolve to at Save — a fresh pick
/// (the button's label differs from its placeholder) wins over a pending
/// remove, which wins over untouched (`pending_image`).
enum PendingImage {
    Keep,
    Clear,
    Upload(String),
}

/// Resolve `btn`'s current label + `clear`'s flag into a [`PendingImage`]
/// (mirrors tui's `staged_image` priority: a typed/picked path always beats a
/// pending clear, which beats leaving the field untouched).
fn pending_image(btn: &gtk::Button, placeholder: &str, clear: &Cell<bool>) -> PendingImage {
    let label = btn.label().map(|s| s.to_string()).unwrap_or_default();
    if label != placeholder {
        PendingImage::Upload(label)
    } else if clear.get() {
        PendingImage::Clear
    } else {
        PendingImage::Keep
    }
}

/// Reset one image field to its untouched state — on open (fresh
/// read-modify-write base) and after a successful Save.
fn reset_image_field(btn: &gtk::Button, clear: &Cell<bool>, placeholder: &str) {
    clear.set(false);
    btn.set_label(placeholder);
}

/// Stage an explicit removal (`profile-edit-*-remove-button`) — back to the
/// placeholder label, but flagged so `pending_image` resolves to `Clear`
/// rather than `Keep` (a fresh pick still overrides this at Save).
fn clear_image_field(btn: &gtk::Button, clear: &Cell<bool>, placeholder: &str) {
    clear.set(true);
    btn.set_label(placeholder);
}

/// Wire `btn` to open a real `gtk::FileDialog` filtered to images; picking a
/// file stages its absolute path as the button's own label (overriding any
/// pending clear) — the same upload-at-Save deferral `compose-file` uses
/// (`views/feed/post_list.rs`).
fn wire_image_picker(btn: &gtk::Button, title: &'static str, clear: Rc<Cell<bool>>) {
    btn.connect_clicked(move |button| {
        let dialog = gtk::FileDialog::builder().title(title).build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images (*.png, *.jpg, *.webp, *.gif)"));
        filter.add_mime_type("image/png");
        filter.add_mime_type("image/jpeg");
        filter.add_mime_type("image/webp");
        filter.add_mime_type("image/gif");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));

        let win = button.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let button = button.clone();
        let clear = Rc::clone(&clear);
        dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
            if let Ok(file) = result
                && let Some(path) = file.path()
            {
                clear.set(false);
                button.set_label(&path.to_string_lossy());
            }
        });
    });
}

/// Handle the profile view retains to open the form (from `profile-edit-button`).
pub struct EditFormHandle {
    pub open: Rc<dyn Fn()>,
}

fn entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build()
}

fn show_error(w: &EditWidgets, msg: &str) {
    crate::settings::render_error_label(&w.error_label, Some(msg));
}

fn clear_error(w: &EditWidgets) {
    crate::settings::render_error_label(&w.error_label, None);
}

/// Build the (initially hidden) edit form + the `open` entry point. `error_label`
/// is the profile page's shared `error-message`; `on_saved` re-renders the header.
pub fn build_edit_form(
    client: &Rc<FaunaClient>,
    error_label: gtk::Label,
    on_saved: Rc<dyn Fn()>,
) -> (gtk::Box, EditFormHandle) {
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&form, ids::PROFILE_EDIT_FORM);

    let display_name = entry(p::EDIT_DISPLAY_NAME);
    set_test_id(&display_name, ids::PROFILE_EDIT_DISPLAY_NAME);
    form.append(&display_name);

    let bio = entry(p::EDIT_BIO);
    set_test_id(&bio, ids::PROFILE_EDIT_BIO);
    form.append(&bio);

    let link_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    set_test_id(&link_list, ids::PROFILE_EDIT_LINK_LIST);
    form.append(&link_list);

    let add_link = gtk::Button::with_label(p::EDIT_ADD_LINK);
    add_link.set_halign(gtk::Align::Start);
    set_test_id(&add_link, ids::PROFILE_EDIT_LINK_ADD_BUTTON);
    form.append(&add_link);

    // Avatar / banner (`profile.md` § Field ownership — the same shared
    // `build_edited_profile_with_images` path every app rides). Each is a
    // single button: its label is the placeholder until a file is picked, then
    // the picked absolute path (the literal text `avatar_path_text()`/
    // `banner_path_text()` read back), paired with a remove button.
    let avatar_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let avatar_btn = gtk::Button::with_label(p::EDIT_AVATAR);
    set_test_id(&avatar_btn, ids::PROFILE_EDIT_AVATAR);
    let avatar_remove = gtk::Button::with_label(p::EDIT_REMOVE_AVATAR);
    set_test_id(&avatar_remove, ids::PROFILE_EDIT_AVATAR_REMOVE_BUTTON);
    avatar_row.append(&avatar_btn);
    avatar_row.append(&avatar_remove);
    form.append(&avatar_row);

    let banner_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let banner_btn = gtk::Button::with_label(p::EDIT_BANNER);
    set_test_id(&banner_btn, ids::PROFILE_EDIT_BANNER);
    let banner_remove = gtk::Button::with_label(p::EDIT_REMOVE_BANNER);
    set_test_id(&banner_remove, ids::PROFILE_EDIT_BANNER_REMOVE_BUTTON);
    banner_row.append(&banner_btn);
    banner_row.append(&banner_remove);
    form.append(&banner_row);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(p::EDIT_CANCEL);
    set_test_id(&cancel, ids::PROFILE_EDIT_CANCEL_BUTTON);
    let save = gtk::Button::with_label(p::EDIT_SAVE);
    save.add_css_class("suggested-action");
    set_test_id(&save, ids::PROFILE_EDIT_SAVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&save, "fauna.profile.set");
    buttons.append(&cancel);
    buttons.append(&save);
    form.append(&buttons);

    let avatar_clear = Rc::new(Cell::new(false));
    let banner_clear = Rc::new(Cell::new(false));
    wire_image_picker(&avatar_btn, p::EDIT_AVATAR, Rc::clone(&avatar_clear));
    wire_image_picker(&banner_btn, p::EDIT_BANNER, Rc::clone(&banner_clear));

    let ctx = Rc::new(EditCtx {
        client: Rc::clone(client),
        rt: client.runtime_handle(),
        w: EditWidgets {
            form: form.clone(),
            display_name,
            bio,
            link_list,
            link_rows: RefCell::new(Vec::new()),
            avatar_btn,
            avatar_clear,
            banner_btn,
            banner_clear,
            error_label,
        },
        base: RefCell::new(None),
        on_saved,
    });

    {
        let ctx = Rc::clone(&ctx);
        avatar_remove.connect_clicked(move |_| {
            clear_image_field(&ctx.w.avatar_btn, &ctx.w.avatar_clear, p::EDIT_AVATAR);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        banner_remove.connect_clicked(move |_| {
            clear_image_field(&ctx.w.banner_btn, &ctx.w.banner_clear, p::EDIT_BANNER);
        });
    }

    {
        let ctx = Rc::clone(&ctx);
        add_link.connect_clicked(move |_| add_link_row(&ctx, "", ""));
    }
    {
        let ctx = Rc::clone(&ctx);
        cancel.connect_clicked(move |_| {
            clear_error(&ctx.w);
            ctx.w.form.set_visible(false);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        save.connect_clicked(move |_| submit(&ctx));
    }

    let open: Rc<dyn Fn()> = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || open_form(&ctx))
    };

    (form, EditFormHandle { open })
}

/// Append a link row (prefilled with `label_val`/`url_val`, both empty for a
/// fresh row). Remove detaches the row container; `collect_links` then skips it.
fn add_link_row(ctx: &Rc<EditCtx>, label_val: &str, url_val: &str) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let label = entry(p::EDIT_LINK_LABEL);
    label.set_text(label_val);
    set_test_id(&label, ids::PROFILE_EDIT_LINK_LABEL);
    let url = entry(p::EDIT_LINK_URL);
    url.set_text(url_val);
    set_test_id(&url, ids::PROFILE_EDIT_LINK_URL);
    let remove = gtk::Button::with_label(p::EDIT_REMOVE_LINK);
    set_test_id(&remove, ids::PROFILE_EDIT_LINK_REMOVE_BUTTON);

    row.append(&label);
    row.append(&url);
    row.append(&remove);

    {
        let link_list = ctx.w.link_list.clone();
        let row = row.clone();
        remove.connect_clicked(move |_| link_list.remove(&row));
    }

    ctx.w.link_list.append(&row);
    ctx.w.link_rows.borrow_mut().push(LinkRow {
        container: row,
        label,
        url,
    });
}

/// Fetch the current profile (read-modify-write base), populate the fields, show
/// the form. A `not_found`/transport error starts from a blank first-publish.
///
/// The base loads through the shared read-prove-record
/// (`fauna_client_recovery::ceremony::load_profile_edit_base`, tui's door), so
/// a succession link the base needs is proven and recorded **before** the save
/// reads `predecessors_of` — a linkless successor's first edit never waits on
/// the spawned per-sign-in hop (`profile.md` § After an identity succession).
fn open_form(ctx: &Rc<EditCtx>) {
    let nest = ctx.client.nest_rpc().clone();
    let kp = ActorKeypair::from_secret(ctx.client.secret_bytes());
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            match fauna_client_recovery::ceremony::load_profile_edit_base(
                nest,
                &crate::account_registry(),
                &kp,
            )
            .await
            {
                Ok(Some(body)) => decode_profile_display(&body).ok().map(|d| (body, d)),
                Ok(None) | Err(_) => None,
            }
        },
        move |result| {
            populate(&ctx_render, result);
            ctx_render.w.form.set_visible(true);
        },
    );
}

fn populate(ctx: &Rc<EditCtx>, result: Option<(Vec<u8>, ProfileDisplay)>) {
    let (base_body, dn, bio, links) = match result {
        Some((body, d)) => (
            Some(body),
            d.display_name.unwrap_or_default(),
            d.bio.unwrap_or_default(),
            d.links,
        ),
        None => (None, String::new(), String::new(), Vec::new()),
    };
    ctx.w.display_name.set_text(&dn);
    ctx.w.bio.set_text(&bio);
    for row in ctx.w.link_rows.borrow_mut().drain(..) {
        ctx.w.link_list.remove(&row.container);
    }
    for link in &links {
        add_link_row(ctx, &link.label, &link.uri);
    }
    *ctx.base.borrow_mut() = base_body;
    reset_image_field(&ctx.w.avatar_btn, &ctx.w.avatar_clear, p::EDIT_AVATAR);
    reset_image_field(&ctx.w.banner_btn, &ctx.w.banner_clear, p::EDIT_BANNER);
    clear_error(&ctx.w);
}

/// Collect the non-empty link rows still attached to the list (removed rows are
/// detached, so `parent()` is `None`).
fn collect_links(ctx: &Rc<EditCtx>) -> Vec<ProfileLink> {
    ctx.w
        .link_rows
        .borrow()
        .iter()
        .filter(|r| r.container.parent().is_some())
        .filter_map(|r| {
            let label = r.label.text().trim().to_string();
            let uri = r.url.text().trim().to_string();
            if label.is_empty() && uri.is_empty() {
                None
            } else {
                Some(ProfileLink { label, uri })
            }
        })
        .collect()
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn submit(ctx: &Rc<EditCtx>) {
    let kp = ActorKeypair::from_secret(ctx.client.secret_bytes());
    // Whom this identity succeeded from, per this device's registry — the only
    // evidence that admits a base the nest serves signed by someone else
    // (`profile.md` § After an identity succession, the successor RE-PUBLISHES).
    let predecessors = fauna_client_profile::predecessors_from_hex(
        &crate::account_registry().predecessors_of(&kp.actor_id_hex()),
    );
    let display_name = non_empty(ctx.w.display_name.text().to_string());
    let bio = non_empty(ctx.w.bio.text().to_string());
    let links = collect_links(ctx);
    let avatar_pending = pending_image(&ctx.w.avatar_btn, p::EDIT_AVATAR, &ctx.w.avatar_clear);
    let banner_pending = pending_image(&ctx.w.banner_btn, p::EDIT_BANNER, &ctx.w.banner_clear);
    let base_body = ctx.base.borrow().clone();

    let content = ctx.client.content_api().clone();
    let nest = ctx.client.nest_rpc().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // Never drop a picture the user picked: an upload failure fails
            // the whole save rather than publishing text-only (mirrors tui's
            // `resolve_staged_image` rule).
            let avatar = resolve_pending_image(avatar_pending, &*content)
                .await
                .map_err(|e| format!("avatar: {e}"))?;
            let banner = resolve_pending_image(banner_pending, &*content)
                .await
                .map_err(|e| format!("banner: {e}"))?;
            let body = build_edited_profile_with_images(
                &kp,
                base_body.as_deref(),
                &predecessors,
                display_name,
                bio,
                links,
                avatar,
                banner,
            )
            .map_err(|e| e.to_string())?;
            let pc = ProfileClient::new(nest);
            pc.profile_set(body)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        move |res: Result<(), String>| match res {
            Ok(()) => {
                clear_error(&ctx_render.w);
                ctx_render.w.form.set_visible(false);
                (ctx_render.on_saved)();
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

/// Resolve one image field's [`PendingImage`] into the [`ProfileImageEdit`]
/// `build_edited_profile_with_images` needs. `Keep`/`Clear` need no HTTP plane;
/// `Upload` reads the picked file and uploads it via the shared public-post
/// blob path (`media.md` § Encryption at rest: avatar/banner are "the same
/// shape" as post attachments) — the identical mechanism feed's `compose-file`
/// rides (`client.rs::upload_staged_blob`).
async fn resolve_pending_image(
    pending: PendingImage,
    content: &dyn crate::nest_content_api::NestContentApi,
) -> Result<ProfileImageEdit, String> {
    match pending {
        PendingImage::Keep => Ok(ProfileImageEdit::Keep),
        PendingImage::Clear => Ok(ProfileImageEdit::Clear),
        PendingImage::Upload(path) => {
            let blob = fauna_client::upload_public_post_blob(content, &path)
                .await
                .map_err(|e| e.to_string())?;
            ProfileImageEdit::set_from_hex(&blob.blob_hash).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Save-time resolution priority: an untouched field is `Keep`, an
    /// explicit remove tap is `Clear`, and a freshly picked file always wins
    /// over a pending clear (mirrors tui's `staged_image_priority_is_path_
    /// then_clear_then_keep`).
    #[test]
    fn pending_image_priority_is_upload_then_clear_then_keep() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = gtk::Button::with_label("placeholder");
            let clear = Cell::new(false);
            assert!(matches!(
                pending_image(&btn, "placeholder", &clear),
                PendingImage::Keep
            ));

            clear.set(true);
            assert!(matches!(
                pending_image(&btn, "placeholder", &clear),
                PendingImage::Clear
            ));

            btn.set_label("/tmp/pic.png");
            assert!(matches!(
                pending_image(&btn, "placeholder", &clear),
                PendingImage::Upload(p) if p == "/tmp/pic.png"
            ));
        });
    }

    /// `clear_image_field` flags a pending removal (back to the placeholder
    /// label); `reset_image_field` returns to the fully untouched state — the
    /// two states `pending_image` must tell apart.
    #[test]
    fn clear_and_reset_image_field_set_the_expected_state() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = gtk::Button::with_label("placeholder");
            let clear = Cell::new(false);

            btn.set_label("/tmp/a.png");
            clear_image_field(&btn, &clear, "placeholder");
            assert_eq!(btn.label().as_deref(), Some("placeholder"));
            assert!(clear.get(), "remove tap stages a clear");

            btn.set_label("/tmp/b.png");
            reset_image_field(&btn, &clear, "placeholder");
            assert_eq!(btn.label().as_deref(), Some("placeholder"));
            assert!(!clear.get(), "open/save resets to fully untouched");
        });
    }
}
