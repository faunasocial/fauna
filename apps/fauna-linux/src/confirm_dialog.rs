//! One confirm dialog for every "are you sure?" gesture this app shows.
//!
//! Ten call sites used to hand-copy the same `adw::MessageDialog` shape —
//! cancel plus one named response, `set_default_response("cancel")`,
//! `set_close_response("cancel")`, a `connect_response` that runs the action on
//! the named response — and each diverged from the others on five independent
//! axes: whether the dialog root carried a test id, which response buttons were
//! tagged, whether the tagging ran on an idle tick or synchronously after
//! `present()`, where `declare_wire_kind` sat, and whether the body was a plain
//! string or a caller-built widget. Two of those five turned out to be drift
//! rather than design, and the drift was invisible precisely because there was
//! no one shape to diverge *from*.
//!
//! The three that are real variance are [`ConfirmSpec`] fields; the two that
//! were drift are settled here, once:
//!
//! - **Tagging runs on `glib::idle_add_local_once`, never synchronously after
//!   `present()`.** That is what [`crate::testid::tag_response_button`] has
//!   always documented ("call it from a `glib::idle_add_local_once` queued
//!   right before `present()`") — five sites simply did not follow their own
//!   helper's contract. Both timings were green in e2e, so the choice is made
//!   on which one is *safe by construction*: idle runs strictly later than the
//!   synchronous form, one main-loop turn after the same `present()`, so it
//!   cannot be too early. The synchronous form is only correct if libadwaita
//!   materializes response buttons inside `present()` — an implementation
//!   detail of a dependency we do not read and it does not promise. Both reach
//!   the widget tree long before an e2e poll, which is driven by the same main
//!   loop.
//! - **The cancel button gets a test id wherever `ui.yaml` names one.** Three
//!   sites tagged confirm and cancel, four tagged confirm only — and for two of
//!   those four `ui.yaml` already names the cancel id (`sign-out-cancel-button`,
//!   `admin-factory-reset-cancel-button`, both `optional_elements`) with a
//!   `fauna-ui-ids` constant that no linux source referenced. Implementing them
//!   is conforming to the spec, not deviating from it (priority #4 — resolve
//!   drift toward the richest pattern).
//!
//! What stays per-site, because each is a real product decision:
//!
//! - **`wire_kind` placement.** Some ceremonies declare on the *confirm*
//!   button, some on the *entry* button that opens the dialog. Both are right:
//!   a folder delete's confirm IS its write, whereas factory reset, mail-disable
//!   and snapshot-delete declare on the entry because "offering a ceremony that
//!   must dead-end at its confirm is worse than withholding it"
//!   (`views/admin.rs`). Entry-declaring sites simply pass no `wire_kind` here.
//!   A client-local gesture (sign-out) declares nothing at all.
//! - **The dialog root's own test id**, which only some pages' element scopes
//!   name.
//! - **Body shape** — see [`ConfirmBody`].
//! - **Appearance.** Not every confirm is destructive: the re-auth prompt in
//!   front of an account switch is a `Suggested` gate over a reversible action.
//!
//! Presents `adw::AlertDialog`, not the deprecated `adw::MessageDialog` — the
//! swap this module's single seam existed to make. `AlertDialog` is an `adw::Dialog`: it embeds in `parent`'s widget
//! tree rather than opening a separate transient toplevel, so `present_confirm`
//! takes the anchor **widget**, not a resolved `gtk::Window` — callers no
//! longer need to walk `.root()` themselves.

use adw::prelude::*;
use gtk::glib;

use crate::i18n::strings;

/// What fills the dialog's message area.
pub enum ConfirmBody<'a> {
    /// A plain localized string — `AlertDialog`'s own body label. What every
    /// site wants unless it needs more than one paragraph or a conditional one.
    Text(&'a str),
    /// A caller-built widget, set as the dialog's extra child. Used where the
    /// body is genuinely structured: the bridge service-user rotate shows a
    /// warning label plus a second DKIM-consequence label that renders only for
    /// an mta-role bridge, and both labels carry their own test ids — none of
    /// which a single string can express.
    Child(&'a gtk::Widget),
}

/// The per-site knobs [`present_confirm`] needs, grouped so the helper stays
/// under clippy's argument-count lint and so a reader sees one site's whole
/// variance in one place.
///
/// Built through [`ConfirmSpec::new`] plus the `with_*` setters: the five
/// constructor arguments are the ones every site genuinely has, and each setter
/// is exactly one of the axes that legitimately varies.
pub struct ConfirmSpec<'a> {
    title: &'a str,
    body: ConfirmBody<'a>,
    confirm_response: &'static str,
    confirm_label: &'a str,
    cancel_label: &'a str,
    appearance: adw::ResponseAppearance,
    dialog_test_id: Option<&'static str>,
    confirm_test_id: Option<&'static str>,
    cancel_test_id: Option<&'static str>,
    wire_kind: Option<&'static str>,
    on_dismissed: Option<Box<dyn Fn()>>,
}

impl<'a> ConfirmSpec<'a> {
    /// A destructive confirm — the common case. `confirm_response` is the
    /// response id `connect_response` matches on; it never reaches the user, so
    /// it stays a `&'static str` while the two labels are localized.
    pub fn new(
        title: &'a str,
        body: ConfirmBody<'a>,
        confirm_response: &'static str,
        confirm_label: &'a str,
        cancel_label: &'a str,
    ) -> Self {
        Self {
            title,
            body,
            confirm_response,
            confirm_label,
            cancel_label,
            appearance: adw::ResponseAppearance::Destructive,
            dialog_test_id: None,
            confirm_test_id: None,
            cancel_test_id: None,
            wire_kind: None,
            on_dismissed: None,
        }
    }

    /// Present the confirm as `Suggested` rather than `Destructive` — a gate in
    /// front of a reversible action, not a warning about an irreversible one.
    pub fn suggested(mut self) -> Self {
        self.appearance = adw::ResponseAppearance::Suggested;
        self
    }

    /// Tag the dialog root itself, for the pages whose element scope names it
    /// (e2e discovery/scoping).
    pub fn with_dialog_id(mut self, id: &'static str) -> Self {
        self.dialog_test_id = Some(id);
        self
    }

    /// Tag the confirm button. Required by [`Self::with_wire_kind`], which
    /// finds that button by this id.
    pub fn with_confirm_id(mut self, id: &'static str) -> Self {
        self.confirm_test_id = Some(id);
        self
    }

    /// Tag the cancel button. Pass it wherever `ui.yaml` names a cancel id.
    pub fn with_cancel_id(mut self, id: &'static str) -> Self {
        self.cancel_test_id = Some(id);
        self
    }

    /// Declare this ceremony's wire kind on the **confirm** button for the
    /// offline gate. Only for sites where the confirm gesture IS the write; a
    /// site that declares on its entry button instead (see the module docs)
    /// passes nothing here.
    pub fn with_wire_kind(mut self, kind: &'static str) -> Self {
        self.wire_kind = Some(kind);
        self
    }

    /// Run `f` when the dialog is answered **however** it is answered —
    /// confirm, cancel, Escape, close — before `on_confirm`.
    ///
    /// For the one thing a confirm-only callback structurally cannot express: a
    /// site holding a re-entrancy guard ("a prompt is already open") must clear
    /// it on the decline paths too, or declining once wedges the gesture for
    /// the rest of the session. Only the account re-auth prompt needs it, and
    /// it is a builder axis rather than a second entry point so that the site
    /// which needs it says so in its own call.
    pub fn with_dismiss(mut self, f: impl Fn() + 'static) -> Self {
        self.on_dismissed = Some(Box::new(f));
        self
    }
}

/// Present a confirm dialog — cancel plus one named response — running
/// `on_confirm` if the user picks the named one. Any other outcome (cancel,
/// Escape, the close button) is a pure no-op.
///
/// `parent` is the anchor widget the dialog embeds over (any widget already in
/// the window's tree works — `AlertDialog::present` resolves the toplevel
/// itself), not a resolved `gtk::Window`.
pub fn present_confirm(
    parent: &impl IsA<gtk::Widget>,
    spec: ConfirmSpec<'_>,
    on_confirm: impl Fn() + 'static,
) {
    let ConfirmSpec {
        title,
        body,
        confirm_response,
        confirm_label,
        cancel_label,
        appearance,
        dialog_test_id,
        confirm_test_id,
        cancel_test_id,
        wire_kind,
        on_dismissed,
    } = spec;

    debug_assert!(
        wire_kind.is_none() || confirm_test_id.is_some(),
        "a wire kind is declared on the confirm button, which is found by its \
         test id — passing one without the other declares nothing, silently"
    );

    let text_body = match body {
        ConfirmBody::Text(t) => Some(t),
        ConfirmBody::Child(_) => None,
    };
    let dialog = adw::AlertDialog::new(Some(title), text_body);
    if let ConfirmBody::Child(child) = body {
        dialog.set_extra_child(Some(child));
    }
    if let Some(id) = dialog_test_id {
        crate::testid::set_test_id(&dialog, id);
    }

    dialog.add_response("cancel", cancel_label);
    dialog.add_response(confirm_response, confirm_label);
    dialog.set_response_appearance(confirm_response, appearance);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_dialog, response| {
        if let Some(dismissed) = on_dismissed.as_ref() {
            dismissed();
        }
        if response == confirm_response {
            on_confirm();
        }
    });

    // The response buttons materialize at present-time, so the tagging is
    // queued rather than run inline — see the module docs for why this is the
    // one timing rather than one of two.
    let dialog_for_id = dialog.clone();
    let confirm_label = confirm_label.to_string();
    let cancel_label = cancel_label.to_string();
    glib::idle_add_local_once(move || {
        let root = dialog_for_id.upcast_ref::<gtk::Widget>();
        if let Some(id) = confirm_test_id {
            crate::testid::tag_response_button(root, &confirm_label, id);
            if let Some(kind) = wire_kind
                && let Some(confirm) = crate::testid::find_by_test_id(root, id)
                    .and_then(|w| w.downcast::<gtk::Button>().ok())
            {
                crate::offline_gate::declare_wire_kind(&confirm, kind);
            }
        }
        if let Some(id) = cancel_test_id {
            crate::testid::tag_response_button(root, &cancel_label, id);
        }
    });

    dialog.present(Some(parent));
}

/// The localized "Cancel" every site that has no page-specific cancel string
/// uses. Named here so a call site cannot reach for a bare `"Cancel"` literal —
/// which `views/backups/snapshot_list.rs` did, the one untranslated response
/// label in the set.
pub const CANCEL: &str = strings::common::CANCEL;
