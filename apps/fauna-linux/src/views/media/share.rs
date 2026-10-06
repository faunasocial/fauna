//! Share links on the Media page (`docs/goal/behavior/share-links.md` § Flows):
//! the `share-link-create-modal` a detail's `share-link-button` opens, the
//! page-level `share-link-list` with its per-row copy / revoke, and the single
//! `share-link-revoke-confirm-modal`.
//!
//! Every piece of state — the create step, the URL revealed only after the
//! registration succeeded, the list's `loaded` bit and row states, the armed
//! revoke — lives in the shared `MediaMachine`; this module paints it and
//! forwards gestures (`share-links.md` § Where logic lives). tui's
//! `share_create_elements` / `share_list_elements` are the reference.
//!
//! The three windows are **render-driven**: the page's render loop calls
//! [`ShareSurfaces::render`] on every observer tick, and each window is
//! presented while its half of the snapshot is open and closed once it is
//! not — so a window never outlives, nor runs ahead of, the machine state it
//! shows. A user-initiated close (Escape, the title-bar X, the sign-out sweep)
//! forwards the machine's own close gesture; a close the render performs does
//! not (it takes the window out of its slot first, which is how the
//! close-request handler tells the two apart). That distinction is
//! load-bearing for the revoke confirm: its confirm button leaves the window
//! for the render to close, so the armed token is consumed by
//! `confirm_share_revoke`, never cleared by a `cancel_share_revoke`.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_core::localized::LocalizedText;
use fauna_media_machine::{MediaMachine, MediaPageSnapshot, ShareCreateSnapshot, ShareLinkSummary};

use crate::i18n::strings;
use crate::i18n::strings::share_link as sl;
use crate::testid::{set_test_attr, set_test_id};

thread_local! {
    /// The window the next create surface is presented over — the
    /// `media-item-detail` whose `share-link-button` was pressed. A detail is
    /// opened from the page AND from Search (`views::search`), neither of which
    /// holds the page's render context, so the button records its window here
    /// and the render that sees the machine's create surface open picks it up.
    /// Weak: a detail closed first simply falls back to the page's window.
    static CREATE_PARENT: RefCell<Option<glib::WeakRef<gtk::Window>>> =
        const { RefCell::new(None) };
}

/// `share-link-button` — open the create surface on the file at `path` of
/// `folder`, over `parent`. The machine refuses an ineligible file (no-op), so
/// no window follows for it.
pub fn open_create(
    parent: Option<&gtk::Window>,
    machine: &MediaMachine,
    folder: String,
    path: String,
) {
    CREATE_PARENT.with(|p| *p.borrow_mut() = parent.map(|w| w.downgrade()));
    machine.open_share_create(folder, path);
}

/// The `share-link-button` for a detail surface, or `None` when the file is not
/// eligible — the control is ABSENT then, never painted-but-inert
/// (`share-links.md` § Which files can be linked; the verdict is the machine's
/// `MediaItemSummary::share_link_eligible`, which is already `false` for a
/// followed browse scope's items).
pub fn detail_button(
    eligible: bool,
    machine: &Arc<MediaMachine>,
    folder: &str,
    path: &str,
) -> Option<gtk::Button> {
    if !eligible {
        return None;
    }
    let button = gtk::Button::with_label(sl::BUTTON);
    set_test_id(&button, ids::SHARE_LINK_BUTTON);
    let machine = Arc::clone(machine);
    let folder = folder.to_string();
    let path = path.to_string();
    button.connect_clicked(move |btn| {
        let parent = btn.root().and_downcast::<gtk::Window>();
        open_create(parent.as_ref(), &machine, folder.clone(), path.clone());
    });
    Some(button)
}

/// The page-level `share-link-list-button` — open + load the caller's links
/// (`fauna.share.list`, a read).
pub fn list_button(machine: &Arc<MediaMachine>, runtime: &tokio::runtime::Handle) -> gtk::Button {
    let button = gtk::Button::with_label(sl::LIST_BUTTON);
    set_test_id(&button, ids::SHARE_LINK_LIST_BUTTON);
    crate::offline_gate::declare_wire_kind(&button, "fauna.share.list");
    let machine = Arc::clone(machine);
    let runtime = runtime.clone();
    button.connect_clicked(move |_| {
        let machine = Arc::clone(&machine);
        runtime.spawn(async move { machine.open_share_links().await });
    });
    button
}

/// The live create surface's widgets.
struct CreateUi {
    window: adw::Window,
    expiry_row: gtk::Box,
    expiry: gtk::DropDown,
    create: gtk::Button,
    cancel: gtk::Button,
    url: gtk::Label,
    copy: gtk::Button,
    status: gtk::Label,
}

/// The live list surface's widgets.
struct ListUi {
    window: adw::Window,
    loading: gtk::Label,
    empty: gtk::Label,
    rows: gtk::Box,
    status: gtk::Label,
}

/// The share surfaces one Media page owns — held by the page's render context.
pub struct ShareSurfaces {
    machine: Arc<MediaMachine>,
    runtime: tokio::runtime::Handle,
    /// The page root, whose window a surface is presented over when it has no
    /// better parent.
    page: gtk::Box,
    /// Guards the render's programmatic `set_selected` from re-entering the
    /// expiry gesture.
    updating: Cell<bool>,
    create: RefCell<Option<CreateUi>>,
    list: RefCell<Option<ListUi>>,
    revoke: RefCell<Option<adw::Window>>,
}

impl ShareSurfaces {
    pub fn new(
        machine: &Arc<MediaMachine>,
        runtime: &tokio::runtime::Handle,
        page: &gtk::Box,
    ) -> Rc<Self> {
        Rc::new(Self {
            machine: Arc::clone(machine),
            runtime: runtime.clone(),
            page: page.clone(),
            updating: Cell::new(false),
            create: RefCell::new(None),
            list: RefCell::new(None),
            revoke: RefCell::new(None),
        })
    }

    /// Bring every surface in line with `snap` — called by the page render loop
    /// on each observer tick.
    pub fn render(self: &Rc<Self>, snap: &MediaPageSnapshot) {
        self.render_create(snap);
        // The confirm closes before the list it sits over, and opens after it.
        if snap.share_links.revoke_confirm.is_none() || !snap.share_links.open {
            close_slot(&self.revoke);
        }
        self.render_list(snap);
        if snap.share_links.open
            && let Some(armed) = &snap.share_links.revoke_confirm
            && self.revoke.borrow().is_none()
        {
            let window = self.build_revoke(snap, armed);
            *self.revoke.borrow_mut() = Some(window);
        }
    }

    fn page_window(&self) -> Option<gtk::Window> {
        self.page.root().and_downcast::<gtk::Window>()
    }

    // ── Create ─────────────────────────────────────────────────────────────

    fn render_create(self: &Rc<Self>, snap: &MediaPageSnapshot) {
        let Some(create) = &snap.share_create else {
            close_slot(&self.create);
            return;
        };
        if self.create.borrow().is_none() {
            let ui = self.build_create(snap, create);
            *self.create.borrow_mut() = Some(ui);
        }
        let slot = self.create.borrow();
        let Some(ui) = slot.as_ref() else { return };

        self.updating.set(true);
        super::select_dropdown_value(&ui.expiry, &create.expiry);
        self.updating.set(false);

        // Step 4 of the create flow: the URL (and its Copy) exist only once the
        // registration succeeded; until then the expiry and Create show.
        let revealed = create.url.as_deref();
        ui.expiry_row.set_visible(revealed.is_none());
        ui.create.set_visible(revealed.is_none());
        ui.create.set_sensitive(!create.busy);
        ui.create.set_label(if create.busy {
            sl::CREATING
        } else {
            sl::CREATE
        });
        ui.url.set_text(revealed.unwrap_or_default());
        ui.url.set_visible(revealed.is_some());
        ui.copy.set_visible(revealed.is_some());
        ui.cancel.set_label(if revealed.is_some() {
            sl::CLOSE
        } else {
            sl::CANCEL
        });
        paint_status(&ui.status, own_error(snap, &["share_link.error_create"]));
    }

    fn build_create(
        self: &Rc<Self>,
        snap: &MediaPageSnapshot,
        create: &ShareCreateSnapshot,
    ) -> CreateUi {
        let parent = CREATE_PARENT
            .with(|p| p.borrow_mut().take())
            .and_then(|w| w.upgrade())
            .or_else(|| self.page_window());
        let window = modal_window(parent.as_ref(), sl::BUTTON);

        let outer = modal_body();
        set_test_id(&outer, ids::SHARE_LINK_CREATE_MODAL);
        outer.append(&heading(&sl::create_title(&create.name)));
        outer.append(&wrapped(sl::CREATE_BODY));

        let expiry_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        expiry_row.append(
            &gtk::Label::builder()
                .label(sl::EXPIRY_LABEL)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .build(),
        );
        let expiry = expiry_dropdown(&snap.share_expiry_options);
        expiry_row.append(&expiry);
        outer.append(&expiry_row);

        let url = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Char)
            .visible(false)
            .build();
        set_test_id(&url, ids::SHARE_LINK_URL);
        outer.append(&url);

        let status = status_label();
        outer.append(&status);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label(sl::CANCEL);
        set_test_id(&cancel, ids::SHARE_LINK_CANCEL_BUTTON);
        let copy = gtk::Button::with_label(sl::COPY);
        copy.set_visible(false);
        set_test_id(&copy, ids::SHARE_LINK_COPY_BUTTON);
        let create_btn = gtk::Button::with_label(sl::CREATE);
        create_btn.add_css_class("suggested-action");
        set_test_id(&create_btn, ids::SHARE_LINK_CREATE_BUTTON);
        crate::offline_gate::declare_wire_kind(&create_btn, "fauna.share.create");
        buttons.append(&cancel);
        buttons.append(&copy);
        buttons.append(&create_btn);
        outer.append(&buttons);

        {
            let this = Rc::downgrade(self);
            expiry.connect_selected_notify(move |dd| {
                let Some(this) = this.upgrade() else { return };
                if this.updating.get() {
                    return;
                }
                if let Some(value) = super::dropdown_value(dd) {
                    this.machine.set_share_expiry(value);
                }
            });
        }
        {
            let machine = Arc::clone(&self.machine);
            let runtime = self.runtime.clone();
            create_btn.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.create_share_link().await });
            });
        }
        {
            let machine = Arc::clone(&self.machine);
            cancel.connect_clicked(move |_| machine.close_share_create());
        }
        {
            let machine = Arc::clone(&self.machine);
            copy.connect_clicked(move |_| {
                if let Some(url) = machine.snapshot().share_create.and_then(|c| c.url) {
                    crate::clipboard::copy_text(&url);
                }
            });
        }
        {
            let this = Rc::downgrade(self);
            window.connect_close_request(move |_| {
                forward_user_close(&this, |s| &s.create, |m| m.close_share_create());
                glib::Propagation::Proceed
            });
        }

        window.set_content(Some(&outer));
        window.present();
        CreateUi {
            window,
            expiry_row,
            expiry,
            create: create_btn,
            cancel,
            url,
            copy,
            status,
        }
    }

    // ── List ───────────────────────────────────────────────────────────────

    fn render_list(self: &Rc<Self>, snap: &MediaPageSnapshot) {
        let list = &snap.share_links;
        if !list.open {
            close_slot(&self.list);
            return;
        }
        if self.list.borrow().is_none() {
            let ui = self.build_list();
            *self.list.borrow_mut() = Some(ui);
        }
        let slot = self.list.borrow();
        let Some(ui) = slot.as_ref() else { return };

        // Three states off one `loaded` bit (`ui/README.md` § List pages:
        // loading is not empty): rows, the empty state, or neither.
        ui.loading.set_visible(!list.loaded);
        ui.empty.set_visible(list.loaded && list.rows.is_empty());
        while let Some(child) = ui.rows.first_child() {
            ui.rows.remove(&child);
        }
        for row in &list.rows {
            ui.rows.append(&self.build_row(row));
        }
        paint_status(
            &ui.status,
            own_error(snap, &["share_link.error_list", "share_link.error_revoke"]),
        );
    }

    fn build_list(self: &Rc<Self>) -> ListUi {
        let window = modal_window(self.page_window().as_ref(), sl::LIST_TITLE);
        window.set_default_size(560, 420);

        let outer = modal_body();
        set_test_id(&outer, ids::SHARE_LINK_LIST);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = heading(sl::LIST_TITLE);
        title.set_hexpand(true);
        header.append(&title);
        let close = gtk::Button::with_label(sl::CLOSE);
        set_test_id(&close, ids::SHARE_LINK_LIST_CLOSE_BUTTON);
        {
            let machine = Arc::clone(&self.machine);
            close.connect_clicked(move |_| machine.close_share_links());
        }
        header.append(&close);
        outer.append(&header);

        let loading = gtk::Label::builder()
            .label(sl::LIST_LOADING)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label"])
            .build();
        outer.append(&loading);
        let empty = gtk::Label::builder()
            .label(sl::EMPTY)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        set_test_id(&empty, ids::SHARE_LINK_EMPTY_STATE);
        outer.append(&empty);

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
        outer.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .vexpand(true)
                .child(&rows)
                .build(),
        );
        let status = status_label();
        outer.append(&status);

        {
            let this = Rc::downgrade(self);
            window.connect_close_request(move |_| {
                forward_user_close(&this, |s| &s.list, |m| m.close_share_links());
                glib::Propagation::Proceed
            });
        }

        window.set_content(Some(&outer));
        window.present();
        ListUi {
            window,
            loading,
            empty,
            rows,
            status,
        }
    }

    /// One `share-link-item` row: name, expiry, state — plus Copy where the
    /// shared re-derivation verified the URL, and Revoke on an Active row.
    fn build_row(&self, row: &ShareLinkSummary) -> gtk::Box {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        item.set_accessible_role(gtk::AccessibleRole::Group);
        set_test_id(&item, ids::SHARE_LINK_ITEM);

        let name = gtk::Label::builder()
            .label(&row.name)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        set_test_id(&name, ids::SHARE_LINK_ITEM_NAME);
        item.append(&name);

        let expires = gtk::Label::builder()
            .label(sl::expires(&fauna_core::format::format_unix_local_date(
                row.expires_at,
            )))
            .css_classes(["dim-label", "caption"])
            .build();
        set_test_id(&expires, ids::SHARE_LINK_ITEM_EXPIRES);
        item.append(&expires);

        // The label is paint-only; the `state` attribute carries the stable
        // value (`active` / `expired` / `revoked`) a test asserts on.
        let state = gtk::Label::builder()
            .label(state_label(&row.state))
            .css_classes(["caption"])
            .build();
        set_test_attr(&state, "state", &row.state);
        set_test_id(&state, ids::SHARE_LINK_ITEM_STATE);
        item.append(&state);

        // Copy only where the URL re-derived and verified; absent otherwise,
        // never a wrong link (`share-links.md` § Flows → List).
        if let Some(url) = row.url.clone() {
            let copy = gtk::Button::with_label(sl::COPY);
            set_test_id(&copy, ids::SHARE_LINK_ITEM_COPY_BUTTON);
            copy.connect_clicked(move |_| crate::clipboard::copy_text(&url));
            item.append(&copy);
        }
        if row.state == "active" {
            let revoke = gtk::Button::with_label(sl::REVOKE);
            revoke.add_css_class("destructive-action");
            set_test_id(&revoke, ids::SHARE_LINK_REVOKE_BUTTON);
            let machine = Arc::clone(&self.machine);
            let token_id = row.token_id.clone();
            revoke.connect_clicked(move |_| machine.arm_share_revoke(token_id.clone()));
            item.append(&revoke);
        }
        item
    }

    // ── Revoke confirm ─────────────────────────────────────────────────────

    /// The single `share-link-revoke-confirm-modal` (the file-delete confirm
    /// is the precedent; a revoke destroys nothing but the link).
    fn build_revoke(self: &Rc<Self>, snap: &MediaPageSnapshot, armed: &str) -> adw::Window {
        let parent = self
            .list
            .borrow()
            .as_ref()
            .map(|ui| ui.window.clone().upcast::<gtk::Window>())
            .or_else(|| self.page_window());
        let window = modal_window(parent.as_ref(), sl::REVOKE_CONFIRM_TITLE);

        let name = snap
            .share_links
            .rows
            .iter()
            .find(|r| r.token_id == armed)
            .map(|r| r.name.as_str())
            .unwrap_or_default();
        let outer = modal_body();
        set_test_id(&outer, ids::SHARE_LINK_REVOKE_CONFIRM_MODAL);
        outer.append(&heading(sl::REVOKE_CONFIRM_TITLE));
        outer.append(&wrapped(&sl::revoke_confirm_body(name)));

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label(strings::common::CANCEL);
        set_test_id(&cancel, ids::SHARE_LINK_REVOKE_CANCEL_BUTTON);
        {
            let machine = Arc::clone(&self.machine);
            cancel.connect_clicked(move |_| machine.cancel_share_revoke());
        }
        let confirm = gtk::Button::with_label(sl::REVOKE_CONFIRM);
        confirm.add_css_class("destructive-action");
        set_test_id(&confirm, ids::SHARE_LINK_REVOKE_CONFIRM_BUTTON);
        crate::offline_gate::declare_wire_kind(&confirm, "fauna.share.revoke");
        {
            // Leaves the window open: the gesture takes the armed token first
            // thing, and the next render closes the confirm (module doc).
            let machine = Arc::clone(&self.machine);
            let runtime = self.runtime.clone();
            confirm.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.confirm_share_revoke().await });
            });
        }
        buttons.append(&cancel);
        buttons.append(&confirm);
        outer.append(&buttons);

        {
            let this = Rc::downgrade(self);
            window.connect_close_request(move |_| {
                forward_user_close(&this, |s| &s.revoke, |m| m.cancel_share_revoke());
                glib::Propagation::Proceed
            });
        }

        window.set_content(Some(&outer));
        window.present();
        window
    }
}

/// A window the render wants gone: take it out of its slot FIRST (so its
/// close-request handler knows the close is not the user's), then close it.
fn close_slot<T: AsWindow>(slot: &RefCell<Option<T>>) {
    let taken = slot.borrow_mut().take();
    if let Some(surface) = taken {
        surface.window().close();
    }
}

/// A close-request on a surface: if it is still in its slot the render did
/// not ask for it, so the user did — forward the machine's close gesture.
fn forward_user_close<T>(
    this: &Weak<ShareSurfaces>,
    slot: impl Fn(&ShareSurfaces) -> &RefCell<Option<T>>,
    gesture: impl Fn(&MediaMachine),
) {
    let Some(this) = this.upgrade() else { return };
    let was_open = slot(&this).borrow_mut().take().is_some();
    if was_open {
        gesture(&this.machine);
    }
}

/// What a slot holds that [`close_slot`] can close.
trait AsWindow {
    fn window(&self) -> &adw::Window;
}

impl AsWindow for CreateUi {
    fn window(&self) -> &adw::Window {
        &self.window
    }
}

impl AsWindow for ListUi {
    fn window(&self) -> &adw::Window {
        &self.window
    }
}

impl AsWindow for adw::Window {
    fn window(&self) -> &adw::Window {
        self
    }
}

/// A modal window over `parent`, joined to its application so the sign-out
/// close-all sweep reaches it (the detail window's idiom).
fn modal_window(parent: Option<&gtk::Window>, title: &str) -> adw::Window {
    let window = adw::Window::builder()
        .title(title)
        .modal(true)
        .default_width(420)
        .build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
        if let Some(app) = p.application() {
            window.set_application(Some(&app));
        }
    }
    window
}

fn modal_body() -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.set_margin_top(12);
    outer.set_margin_bottom(12);
    outer.set_margin_start(12);
    outer.set_margin_end(12);
    outer
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["heading"])
        .build()
}

fn wrapped(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .wrap(true)
        .build()
}

/// A surface's own error line. The share errors land in the page's
/// `error-message` (the machine's contract), but that label sits behind these
/// modal windows where it cannot be read — the delete confirm's reasoning — so
/// each surface repeats ITS errors here, untagged (the id stays the page's).
fn status_label() -> gtk::Label {
    gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .visible(false)
        .css_classes(["error"])
        .build()
}

fn paint_status(label: &gtk::Label, text: Option<String>) {
    label.set_text(text.as_deref().unwrap_or_default());
    label.set_visible(text.is_some());
}

/// The page error, resolved — but only when it is one of `keys`, so a surface
/// never repeats an unrelated page error it did not cause.
fn own_error(snap: &MediaPageSnapshot, keys: &[&str]) -> Option<String> {
    snap.error
        .as_ref()
        .filter(|e: &&LocalizedText| keys.contains(&e.key.as_str()))
        .map(|e| e.resolve(strings::lookup))
}

/// The `share-link-expiry-select` dropdown over the machine's option values,
/// each labelled through the shared `share_link_expiry_label` map — the value
/// stays the model key so the cross-app `select(id, "<value>")` holds.
fn expiry_dropdown(options: &[String]) -> gtk::DropDown {
    let values: Vec<&str> = options.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&values))
        .build();
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        glib::closure!(|item: gtk::StringObject| expiry_label(item.string().as_str())),
    );
    dropdown.set_expression(Some(&label_expr));
    set_test_id(&dropdown, ids::SHARE_LINK_EXPIRY_SELECT);
    dropdown
}

fn expiry_label(value: &str) -> String {
    fauna_core::format::share_link_expiry_label(value)
        .map_or_else(|| value.to_string(), |t| t.resolve(strings::lookup))
}

fn state_label(state: &str) -> String {
    fauna_core::format::share_link_state_label(state)
        .map_or_else(|| state.to_string(), |t| t.resolve(strings::lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row labels come from the shared maps and fall back to the raw value.
    #[test]
    fn labels_resolve_through_the_shared_maps() {
        assert_eq!(expiry_label("7d"), sl::EXPIRY_7D);
        assert_eq!(expiry_label("2w"), "2w");
        assert_eq!(state_label("revoked"), sl::STATE_REVOKED);
        assert_eq!(state_label("paused"), "paused");
    }
}
