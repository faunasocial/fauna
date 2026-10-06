//! The profile's **private section** — the viewer's own nickname, notes and
//! labels on the person being viewed (`docs/goal/ui/profile.md` § The private
//! section owns this edit surface; `docs/goal/ui/contacts.md` § The private
//! overlay owns the record, the merge and where the nickname paints).
//!
//! OTHER only. Glue over shared Rust: the staging rule (lazy, diffed from the
//! form as the user opened it), the changed-register diff, the bounds
//! validation and its refusals are `fauna_core::contact_overlay`
//! (`OverlayEditor`); the Save is
//! `fauna_client_account_runtime::contact_overlays::save`, the one door every
//! app writes through. This file owns the widgets and nothing else.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fauna_core::contact_overlay::{OverlayEditor, OverlayForm};
use fauna_sync_engine::contact_overlay_rows::{OverlayWrite, OverlayWriteOutcome};
use fauna_ui_ids as ids;

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::conversations::overlays;
use crate::i18n::strings::profile as p;
use crate::testid::set_test_id;

/// The section's widgets and its staging — one per profile open.
struct Section {
    actor_id: String,
    editor: RefCell<OverlayEditor>,
    /// Set while [`Section::paint`] writes the fields, so its own writes are
    /// not read back as the user's edits.
    painting: Cell<bool>,
    nickname: gtk::Entry,
    notes: gtk::TextView,
    labels: gtk::Box,
    label_input: gtk::Entry,
    error_label: gtk::Label,
}

impl Section {
    /// The overlay form as the projection holds it right now.
    fn live(&self) -> OverlayForm {
        overlays::projection().form(&self.actor_id)
    }

    /// Paint the fields from the staged edits once editing began, else from
    /// the live projection — so a sibling device's edit re-paints an untouched
    /// section.
    fn paint(self: &Rc<Self>) {
        let form = self.editor.borrow().form(&self.live());
        self.painting.set(true);
        if self.nickname.text() != form.nickname {
            self.nickname.set_text(&form.nickname);
        }
        let buffer = self.notes.buffer();
        if buffer.text(&buffer.start_iter(), &buffer.end_iter(), true) != form.notes {
            buffer.set_text(&form.notes);
        }
        self.painting.set(false);

        while let Some(child) = self.labels.first_child() {
            self.labels.remove(&child);
        }
        for (i, label) in form.labels.iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let chip = gtk::Label::new(Some(label));
            chip.set_halign(gtk::Align::Start);
            chip.set_hexpand(true);
            set_test_id(&chip, ids::PROFILE_LABEL_CHIP);
            row.append(&chip);
            let remove = gtk::Button::with_label(p::PRIVATE_LABEL_REMOVE);
            remove.add_css_class("flat");
            set_test_id(&remove, ids::PROFILE_LABEL_REMOVE_BUTTON);
            let section = Rc::clone(self);
            remove.connect_clicked(move |_| {
                let live = section.live();
                section.editor.borrow_mut().remove_label(&live, i);
                section.paint();
            });
            row.append(&remove);
            self.labels.append(&row);
        }
    }

    fn show_error(&self, message: Option<&str>) {
        crate::settings::render_error_label(&self.error_label, message);
    }

    fn add_label(self: &Rc<Self>) {
        let live = self.live();
        let added = self
            .editor
            .borrow_mut()
            .add_label(&live, &self.label_input.text());
        match added {
            Err(refusal) => {
                self.show_error(Some(&refusal.resolve(crate::i18n::strings::lookup)));
            }
            Ok(()) => {
                self.label_input.set_text("");
                self.show_error(None);
                self.paint();
            }
        }
    }

    /// One Save for everything staged: only the registers this user changed
    /// are written (`profile.md` § The private section). A refusal keeps the
    /// staged edits on screen.
    fn save(self: &Rc<Self>, client: &Rc<FaunaClient>) {
        let changes = match self.editor.borrow().changes() {
            Ok(Some(changes)) => changes,
            // Nothing staged, or staged back to where it started.
            Ok(None) => {
                self.editor.borrow_mut().reset();
                self.paint();
                return;
            }
            Err(refusal) => {
                self.show_error(Some(&refusal.resolve(crate::i18n::strings::lookup)));
                return;
            }
        };
        // The account store assembles off the login path; until it has, there
        // is nowhere to write — the staged edits stay on screen.
        let Some(store) = crate::account_runtime::handle() else {
            self.show_error(Some(crate::i18n::strings::common::STILL_LOADING));
            return;
        };
        let manager = crate::conversations::manager();
        let actor = self.actor_id.clone();
        let section = Rc::clone(self);
        spawn_with_snapshot(
            &client.runtime_handle(),
            move || async move {
                fauna_client_account_runtime::contact_overlays::save(
                    &manager,
                    &store,
                    &actor,
                    OverlayWrite::Changes(changes),
                )
                .await
                .map_err(|e| {
                    (
                        fauna_client_account_runtime::contact_overlays::not_ready_reason(&e),
                        format!("{e:#}"),
                    )
                })
            },
            move |outcome| match outcome {
                // The projection already reloaded: drop the staging and let
                // the fields read live again.
                Ok(OverlayWriteOutcome::Written(_) | OverlayWriteOutcome::Unchanged(_)) => {
                    section.editor.borrow_mut().reset();
                    section.show_error(None);
                    section.paint();
                }
                Ok(OverlayWriteOutcome::Refused(refusal)) => {
                    section.show_error(Some(&refusal.resolve(crate::i18n::strings::lookup)));
                }
                // A replica that is not ready — it has never listed the
                // account's records, or holds them under a generation it may
                // still be keyed for — refuses the save at the read gate; it
                // can be retried once what it waits for has come.
                Err((Some(not_ready), _)) => {
                    section.show_error(Some(not_ready));
                }
                // The plane refuses to seal while no generation tip resolves
                // for this device (`account-data-taxonomy.md` → *The
                // contact-overlay rung*).
                Err((None, reason)) => {
                    section.show_error(Some(&p::PRIVATE_SAVE_FAILED.replace("{reason}", &reason)));
                }
            },
        );
    }
}

/// Build the section for the viewed `actor_id` — below the relationship
/// actions, above the tab strip. `error_label` is the page's `error-message`.
/// Returns the section and its repaint, which the page runs when the overlay
/// projection moves.
pub fn build(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    error_label: gtk::Label,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&root, ids::PROFILE_PRIVATE_SECTION);

    let title = gtk::Label::new(Some(p::PRIVATE_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    root.append(&title);

    let nickname = gtk::Entry::builder()
        .placeholder_text(p::PRIVATE_NICKNAME)
        .build();
    set_test_id(&nickname, ids::PROFILE_NICKNAME_FIELD);
    root.append(&nickname);

    let notes_caption = gtk::Label::new(Some(p::PRIVATE_NOTES));
    notes_caption.set_halign(gtk::Align::Start);
    notes_caption.add_css_class("caption");
    notes_caption.add_css_class("dim-label");
    root.append(&notes_caption);
    let notes = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .build();
    set_test_id(&notes, ids::PROFILE_NOTES_FIELD);
    let notes_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(72)
        .child(&notes)
        .build();
    notes_scroll.add_css_class("card");
    root.append(&notes_scroll);

    let labels_caption = gtk::Label::new(Some(p::PRIVATE_LABELS));
    labels_caption.set_halign(gtk::Align::Start);
    labels_caption.add_css_class("caption");
    labels_caption.add_css_class("dim-label");
    root.append(&labels_caption);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    set_test_id(&labels, ids::PROFILE_LABEL_LIST);
    root.append(&labels);

    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label_input = gtk::Entry::builder()
        .placeholder_text(p::PRIVATE_LABEL_ADD)
        .hexpand(true)
        .build();
    set_test_id(&label_input, ids::PROFILE_LABEL_FIELD);
    add_row.append(&label_input);
    let add_btn = gtk::Button::with_label(p::PRIVATE_LABEL_ADD);
    set_test_id(&add_btn, ids::PROFILE_LABEL_ADD_BUTTON);
    add_row.append(&add_btn);
    root.append(&add_row);

    let save_btn = gtk::Button::with_label(p::PRIVATE_SAVE);
    save_btn.add_css_class("suggested-action");
    save_btn.set_halign(gtk::Align::Start);
    set_test_id(&save_btn, ids::PROFILE_PRIVATE_SAVE_BUTTON);
    root.append(&save_btn);

    let section = Rc::new(Section {
        actor_id: actor_id.to_string(),
        editor: RefCell::new(OverlayEditor::default()),
        painting: Cell::new(false),
        nickname,
        notes,
        labels,
        label_input,
        error_label,
    });

    {
        let s = Rc::clone(&section);
        section.nickname.connect_changed(move |entry| {
            if !s.painting.get() {
                let live = s.live();
                s.editor
                    .borrow_mut()
                    .set_nickname(&live, entry.text().to_string());
            }
        });
    }
    {
        let s = Rc::clone(&section);
        section.notes.buffer().connect_changed(move |buffer| {
            if !s.painting.get() {
                let live = s.live();
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
                s.editor.borrow_mut().set_notes(&live, text.to_string());
            }
        });
    }
    {
        let s = Rc::clone(&section);
        add_btn.connect_clicked(move |_| s.add_label());
    }
    {
        let s = Rc::clone(&section);
        section.label_input.connect_activate(move |_| s.add_label());
    }
    {
        let s = Rc::clone(&section);
        let client = Rc::clone(client);
        save_btn.connect_clicked(move |_| s.save(&client));
    }

    section.paint();
    let repaint: Rc<dyn Fn()> = {
        let s = Rc::clone(&section);
        Rc::new(move || s.paint())
    };
    (root, repaint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::find;

    fn texts(root: &gtk::Widget, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        find::collect_in(root, id, &mut out);
        out.iter().map(find::text_of).collect()
    }

    fn click(root: &gtk::Widget, id: &str) {
        find::find_in(root, id)
            .and_then(|w| w.downcast::<gtk::Button>().ok())
            .unwrap_or_else(|| panic!("{id} is a button"))
            .emit_clicked();
    }

    /// The section paints every approved element (`profile.md` § The private
    /// section → Element IDs), stages a label as a chip with its own remove
    /// button, and surfaces a label refusal on the page's `error-message`
    /// without staging anything.
    #[test]
    fn the_section_stages_labels_as_chips_and_refuses_an_empty_one() {
        crate::testid::run_on_gtk_thread(|| {
            // A client whose nest is a closed port — nothing here dials one
            // (`walk.rs`'s fixture idiom); `_rx` keeps the UI channel open.
            let (tx, _rx) = crate::client::ui_channel();
            let machine = fauna_launch_machine::LaunchMachine::new(
                std::sync::Arc::new(fauna_launch_machine::NullObserver),
                std::sync::Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
            );
            let client = Rc::new(FaunaClient::new(
                "http://127.0.0.1:1".to_string(),
                "11".repeat(32),
                tx,
                machine,
            ));
            let error_label = gtk::Label::builder().visible(false).build();
            let (section, _repaint) = build(&client, &"ab".repeat(32), error_label.clone());
            let root: gtk::Widget = section.upcast();
            for id in [
                ids::PROFILE_PRIVATE_SECTION,
                ids::PROFILE_NICKNAME_FIELD,
                ids::PROFILE_NOTES_FIELD,
                ids::PROFILE_LABEL_LIST,
                ids::PROFILE_LABEL_FIELD,
                ids::PROFILE_LABEL_ADD_BUTTON,
                ids::PROFILE_PRIVATE_SAVE_BUTTON,
            ] {
                assert!(
                    find::find_in(&root, id).is_some(),
                    "the section paints {id}"
                );
            }

            click(&root, ids::PROFILE_LABEL_ADD_BUTTON);
            assert!(
                error_label.is_visible(),
                "an empty label is refused out loud"
            );
            assert!(texts(&root, ids::PROFILE_LABEL_CHIP).is_empty());

            let field = find::find_in(&root, ids::PROFILE_LABEL_FIELD)
                .and_then(|w| w.downcast::<gtk::Entry>().ok())
                .expect("profile-label-field is an entry");
            for label in [" Book   club ", "Family"] {
                field.set_text(label);
                click(&root, ids::PROFILE_LABEL_ADD_BUTTON);
            }
            assert!(
                !error_label.is_visible(),
                "a staged label clears the refusal"
            );
            assert_eq!(field.text(), "", "the add field empties once staged");
            assert_eq!(
                texts(&root, ids::PROFILE_LABEL_CHIP),
                ["Book club", "Family"]
            );

            click(&root, ids::PROFILE_LABEL_REMOVE_BUTTON);
            assert_eq!(texts(&root, ids::PROFILE_LABEL_CHIP), ["Family"]);
        });
    }
}
