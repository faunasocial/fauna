//! The room's labeler set — `room-labeler-toggle[i]` + `room-labeler-inspect-button[i]`
//! in the room policy editor (`ui/conversations.md` § Element IDs → *The room's
//! labeler set*; behavior owner `community-rooms.md` § The three classes,
//! purpose 2).
//!
//! **Every decision is shared Rust.** Which catalog kinds a room may name
//! (`room_may_name_labeler_kind`), the `checked` mark (`labeler_staged`) and
//! whether a row may act (`labeler_toggle_live`: a full set greys only the rows
//! it does not name) are `fauna_conversations`'; the catalog and its inspect view
//! are the Community labelers page's own `LabelerCatalogMachine`. This paints
//! them and adds the one capability term (`can_set_policy`).
//!
//! The inspect view opens **in place**, inside the editor, so the staged draft
//! survives; its own Close returns to the rows. The section is absent where the
//! draft has no set to stage (an end-to-end room, or a set that did not verify).

use fauna_ui_ids as ids;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fauna_conversations::{RoomSettingsDraft, room_may_name_labeler_kind};
use fauna_labeler_catalog_machine::{LabelerCatalogEntry, LabelerCatalogMachine};

use crate::async_helper::spawn_with_snapshot;
use crate::views::personalization::inspect_panel::InspectPanel;

/// The catalog seam the editor needs: the machine the section reads, and the
/// runtime its round trips run on.
pub struct LabelerSeams {
    pub machine: Arc<LabelerCatalogMachine>,
    pub rt: tokio::runtime::Handle,
}

pub struct LabelerSection {
    pub root: gtk::Box,
    rows: gtk::Box,
    inspect: InspectPanel,
    draft: Rc<RefCell<RoomSettingsDraft>>,
    seams: LabelerSeams,
    can_set_policy: bool,
    entries: RefCell<Vec<LabelerCatalogEntry>>,
}

impl LabelerSection {
    /// Build the section and read the catalog once, best-effort (opening the
    /// editor re-reads it; a failed read leaves the rows as they were).
    pub fn new(
        draft: &Rc<RefCell<RoomSettingsDraft>>,
        seams: LabelerSeams,
        can_set_policy: bool,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let inspect = InspectPanel::new();
        let entries = seams.machine.snapshot().entries;

        let section = Rc::new(Self {
            root,
            rows,
            inspect,
            draft: Rc::clone(draft),
            seams,
            can_set_policy,
            entries: RefCell::new(entries),
        });

        // Absent where the draft has no set to stage.
        if section.draft.borrow().labelers.is_none() {
            section.root.set_visible(false);
            return section;
        }

        let heading = gtk::Label::new(Some(
            crate::i18n::strings::conversations::unified::ROOM_LABELERS_LABEL,
        ));
        heading.set_halign(gtk::Align::Start);
        heading.add_css_class("heading");
        section.root.append(&heading);
        section.root.append(&section.rows);
        section.root.append(&section.inspect.root);

        // The inspect view's own Close: back to the rows, the draft untouched.
        {
            let weak = Rc::downgrade(&section);
            section.inspect.close_button.connect_clicked(move |_| {
                if let Some(section) = weak.upgrade() {
                    section.seams.machine.close_inspect();
                    section.inspect.render(None);
                }
            });
        }

        section.paint_rows();

        // Re-read the catalog on open; only the async completion repaints.
        {
            let machine = Arc::clone(&section.seams.machine);
            let weak = Rc::downgrade(&section);
            spawn_with_snapshot(
                &section.seams.rt,
                move || async move {
                    machine.refresh().await;
                    machine.snapshot().entries
                },
                move |entries| {
                    if let Some(section) = weak.upgrade() {
                        *section.entries.borrow_mut() = entries;
                        section.paint_rows();
                    }
                },
            );
        }
        section
    }

    /// The editor's window closed: nothing may keep an inspect view open on the
    /// shared machine.
    pub fn close(&self) {
        self.seams.machine.close_inspect();
    }

    /// Rebuild the rows off the draft and the cached catalog — the staged
    /// marks and the full-set greying are always read straight off the shared
    /// draft, never tracked per widget.
    fn paint_rows(self: &Rc<Self>) {
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        for (index, entry) in self.entries.borrow().iter().enumerate() {
            if !room_may_name_labeler_kind(entry.artifact_kind.clone()) {
                continue;
            }
            let id = entry.labeler_id.clone();
            let (staged, live) = {
                let d = self.draft.borrow();
                (d.labeler_staged(&id), d.labeler_toggle_live(&id))
            };
            let mark = if staged {
                crate::i18n::strings::conversations::unified::ROOM_LABELER_ON
            } else {
                crate::i18n::strings::conversations::unified::ROOM_LABELER_OFF
            };
            // The kind and a prefix of the id: the catalog row carries no name,
            // and the full id sits one gesture away in the inspect view.
            let short_id: String = id.chars().take(16).collect();

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

            let toggle = gtk::ToggleButton::with_label(&format!(
                "{} · {short_id}… · {mark}",
                entry.artifact_kind
            ));
            toggle.set_hexpand(true);
            toggle.set_active(staged);
            toggle.set_sensitive(self.can_set_policy && live);
            crate::testid::set_test_id(&toggle, ids::ROOM_LABELER_TOGGLE);
            crate::testid::set_test_attr(&toggle, "checked", if staged { "true" } else { "false" });
            {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                toggle.connect_clicked(move |_| {
                    if let Some(section) = weak.upgrade() {
                        section.draft.borrow_mut().toggle_labeler(&id);
                        section.paint_rows();
                    }
                });
            }
            row.append(&toggle);

            // Inspecting is a read any member may make.
            let inspect = gtk::Button::with_label(crate::i18n::strings::labeler_catalog::INSPECT);
            crate::testid::set_test_id(&inspect, ids::ROOM_LABELER_INSPECT_BUTTON);
            crate::offline_gate::declare_wire_kind(&inspect, "fauna.labelers.inspect");
            {
                let weak = Rc::downgrade(self);
                inspect.connect_clicked(move |_| {
                    let Some(section) = weak.upgrade() else {
                        return;
                    };
                    let machine = Arc::clone(&section.seams.machine);
                    let weak = Rc::downgrade(&section);
                    spawn_with_snapshot(
                        &section.seams.rt,
                        move || async move {
                            machine.inspect(index as u32).await;
                            machine.snapshot().inspecting
                        },
                        move |inspecting| {
                            if let Some(section) = weak.upgrade() {
                                section.inspect.render(inspecting.as_ref());
                            }
                        },
                    );
                });
            }
            row.append(&inspect);

            self.rows.append(&row);
        }
    }
}
