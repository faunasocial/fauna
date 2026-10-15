//! Multi-rail recipient picker: chip list + input + suggestions + status.
//!
//! Used in two places (per spec section 3):
//! - **New-thread compose** (detail pane shows recipient_picker above the
//!   compose bar when `snapshot.new_thread_compose.is_some()`).
//! - **Add-participant overlay** (modal dialog reuses the same widget).
//!
//! Mirror of `apps/fauna-windows/.../Controls/RecipientPicker.xaml{,.cs}`.
//! State lives in `ComposeState.recipient_picker` (shared crate); this
//! widget renders that state and emits callbacks the parent wires to
//! `manager.{set_new_thread_recipient_input, accept_new_thread_chip,
//! cancel_new_conversation, …}`.
//!
//! Indexed list IDs: `recipient-picker-chip[i]` sits on the chip's `gtk::Button`
//! (its × gesture — `remove_recipient_chip(i)`). `recipient-picker-suggestion[i]`
//! sits on the `gtk::ListBoxRow`, because it is *clicked*: the row is what
//! carries the activation, and a `gtk::Label` is inert — see `build_suggestion`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use fauna_conversations::compose::RecipientPickerState;
use fauna_conversations::snapshot::BridgeIdentitySnapshot;

/// Owns the GTK widgets for one picker instance plus the callbacks the
/// parent wired in.
pub struct RecipientPicker {
    pub root: gtk::Box,
    pub input: gtk::Entry,
    /// "Also reaches people on: …" — the serving bridges' declared labels.
    bridges_label: gtk::Label,
    chips_box: gtk::Box,
    suggestions_list: gtk::ListBox,
    status_label: gtk::Label,
    class_label: gtk::Label,
    /// `recipient-picker-home-nest-toggle` — whether this nest joins the room
    /// being created (`ConversationsManager::set_new_thread_home_nest`). Painted
    /// whether or not a chip is committed: it is a property of the room, chosen
    /// before its first message.
    home_nest_toggle: gtk::ToggleButton,
    /// Set true while the parent's `Refresh()` is updating widget contents,
    /// so the entry's `connect_changed` doesn't loop into another mutator.
    refreshing: Rc<RefCell<bool>>,
    /// `accept_suggestion(i)` — wired by [`Self::connect_suggestion_accepted`];
    /// unwired, a suggestion click falls back to `on_accept`.
    on_suggestion: IndexCallback,
    /// `remove_recipient_chip(i)` — wired by [`Self::connect_chip_removed`].
    on_remove_chip: IndexCallback,
}

/// A late-bound `Fn(index)` callback slot shared with the widgets `render` rebuilds.
type IndexCallback = Rc<RefCell<Option<Rc<dyn Fn(u32)>>>>;

impl RecipientPicker {
    /// Build a picker. `on_input` is called for every keystroke (the
    /// parent forwards to `manager.set_*_recipient_input`). `on_accept` is
    /// called when the user presses Enter; the manager decides which picker
    /// is active and whether the current text parses — the widget no longer
    /// parses addresses itself.
    pub fn new<F, G>(on_input: F, on_accept: G) -> Self
    where
        F: Fn(String) + 'static,
        G: Fn() + 'static,
    {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.set_margin_top(8);
        root.set_margin_bottom(4);
        root.set_margin_start(8);
        root.set_margin_end(8);

        // Chips row — flow horizontally; per-chip ID lives on inner label.
        let chips_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        chips_box.set_halign(gtk::Align::Start);
        root.append(&chips_box);

        // Input field.
        let input = gtk::Entry::new();
        input.set_placeholder_text(Some(
            crate::i18n::strings::conversations::unified::RECIPIENT_PICKER_PLACEHOLDER,
        ));
        crate::testid::set_test_id(&input, ids::RECIPIENT_PICKER_INPUT);
        root.append(&input);

        // The bridges serving the account, each by the label it declared —
        // which far networks a typed address may reach. Only a list: the nest
        // matches an address to its bridge (`conversations.md` § Where logic
        // lives → *The `Bridged` adapter*, ruling 2 (d)). Chrome: ui.yaml gives
        // it no id.
        let bridges_label = gtk::Label::new(None);
        bridges_label.set_halign(gtk::Align::Start);
        bridges_label.add_css_class("dim-label");
        bridges_label.add_css_class("caption");
        bridges_label.set_visible(false);
        root.append(&bridges_label);

        // Suggestions list (collapsed when empty).
        let suggestions_list = gtk::ListBox::new();
        suggestions_list.set_selection_mode(gtk::SelectionMode::None);
        suggestions_list.add_css_class("boxed-list");
        suggestions_list.set_visible(false);
        root.append(&suggestions_list);

        // Resolve-status label. The state is communicated to the e2e
        // bridge via the AT-SPI accessible-description property (the
        // bridge reads it through `get_attr` per the windows pattern of
        // mirroring Tag → HelpText). The user-visible text mirrors the
        // i18n string for the current state.
        let status_label = gtk::Label::new(None);
        status_label.set_halign(gtk::Align::Start);
        status_label.add_css_class("dim-label");
        status_label.add_css_class("caption");
        crate::testid::set_test_id(&status_label, ids::RECIPIENT_RESOLVE_STATUS);
        // GtkLabel derives its AT-SPI accessible Name from label text and
        // `render` overrides Description with the resolve-state name, so
        // neither default channel keeps the test id findable. Pin
        // Property::Label = test id so `Atspi.Accessible.get_name()`
        // matches in the bridge's `_find_by_test_id` even when text is
        // empty (idle) and Description carries state.
        status_label
            .update_property(&[gtk::accessible::Property::Label("recipient-resolve-status")]);
        root.append(&status_label);

        // `recipient-picker-class` — the class statement of the room about to
        // be created (`ui/conversations.md` § Element IDs). Built
        // unconditionally, hidden until a chip is committed.
        let class_label = gtk::Label::new(None);
        class_label.set_halign(gtk::Align::Start);
        class_label.add_css_class("dim-label");
        class_label.add_css_class("caption");
        crate::testid::set_test_id(&class_label, ids::RECIPIENT_PICKER_CLASS);
        class_label.set_visible(false);
        root.append(&class_label);

        // `recipient-picker-home-nest-toggle` — built hidden and un-id'd:
        // only the new-thread composer turns it on
        // ([`Self::enable_home_nest_toggle`]); the add-participant dialog and
        // the folder share dialog reuse this widget and own no room choice.
        let home_nest_toggle = gtk::ToggleButton::new();
        home_nest_toggle.set_halign(gtk::Align::Start);
        home_nest_toggle.set_visible(false);
        root.append(&home_nest_toggle);

        let refreshing = Rc::new(RefCell::new(false));

        // Wire input → on_input callback (gated by refreshing flag to
        // avoid feedback loops during `set_state`).
        {
            let refreshing = refreshing.clone();
            input.connect_changed(move |entry| {
                if *refreshing.borrow() {
                    return;
                }
                on_input(entry.text().to_string());
            });
        }

        // Wire Enter → on_accept. The manager decides which picker is
        // active and whether the current text parses; the widget no
        // longer parses addresses itself.
        let on_accept: Rc<dyn Fn()> = Rc::new(on_accept);
        {
            let on_accept = Rc::clone(&on_accept);
            input.connect_activate(move |_| on_accept());
        }

        // Wire suggestion click → `accept_suggestion(i)` once the parent wires
        // it (`conversations.md` § Where logic lives → *Recipient suggestions*:
        // the Enter path run over *that row's* spelling). Unwired — the folder
        // share dialog, which owns no suggestion source — the click falls back to
        // `on_accept`, the typed text's own path.
        let on_suggestion: IndexCallback = Rc::new(RefCell::new(None));
        {
            let on_accept = Rc::clone(&on_accept);
            let on_suggestion = Rc::clone(&on_suggestion);
            suggestions_list.connect_row_activated(move |_, row| {
                let callback = on_suggestion.borrow().clone();
                match callback {
                    Some(cb) => cb(row.index().max(0) as u32),
                    None => on_accept(),
                }
            });
        }
        let on_remove_chip: IndexCallback = Rc::new(RefCell::new(None));

        Self {
            root,
            input,
            bridges_label,
            chips_box,
            suggestions_list,
            status_label,
            class_label,
            home_nest_toggle,
            refreshing,
            on_suggestion,
            on_remove_chip,
        }
    }

    /// Route a suggestion row's click to `accept_suggestion(index)`.
    pub fn connect_suggestion_accepted<F: Fn(u32) + 'static>(&self, f: F) {
        *self.on_suggestion.borrow_mut() = Some(Rc::new(f));
    }

    /// Route a chip's × to `remove_recipient_chip(index)`.
    pub fn connect_chip_removed<F: Fn(u32) + 'static>(&self, f: F) {
        *self.on_remove_chip.borrow_mut() = Some(Rc::new(f));
    }

    /// Re-render the picker from the snapshot's `RecipientPickerState`.
    /// Cheap full rebuild of chips + suggestions (small lists). Sets the
    /// `refreshing` flag so the entry's `changed` signal doesn't loop.
    pub fn render(&self, state: &RecipientPickerState, bridges: &[BridgeIdentitySnapshot]) {
        *self.refreshing.borrow_mut() = true;

        if bridges.is_empty() {
            self.bridges_label.set_visible(false);
        } else {
            let labels: Vec<&str> = bridges.iter().map(|b| b.label.as_str()).collect();
            self.bridges_label.set_text(
                &crate::i18n::strings::conversations::unified::recipient_picker_bridges(
                    &labels.join(", "),
                ),
            );
            self.bridges_label.set_visible(true);
        }

        // Chips.
        while let Some(child) = self.chips_box.first_child() {
            self.chips_box.remove(&child);
        }
        for (i, chip_addr) in state.chips.iter().enumerate() {
            let chip = build_chip(
                &chip_addr.display_with_bridges(bridges),
                i as u32,
                Rc::clone(&self.on_remove_chip),
            );
            self.chips_box.append(&chip);
        }
        self.chips_box.set_visible(!state.chips.is_empty());

        // Input text — only update if it diverges (avoid cursor jump).
        if self.input.text().as_str() != state.raw_input {
            self.input.set_text(&state.raw_input);
        }

        // Suggestions.
        crate::views::layout::clear_list_box_rows(&self.suggestions_list);
        for (i, sug) in state.suggestions.iter().enumerate() {
            // The finished label the manager derived, with the address beside it
            // when the two differ (`conversations.md` § Where logic lives →
            // *Recipient suggestions*) — the shape tui paints.
            let address = sug.display_with_bridges(bridges);
            let text = match state.suggestion_displays.get(i) {
                Some(label) if !label.is_empty() && *label != address => {
                    format!("{label} · {address}")
                }
                _ => address,
            };
            let row = build_suggestion(&text);
            self.suggestions_list.append(&row);
        }
        self.suggestions_list
            .set_visible(!state.suggestions.is_empty());

        // Status — text + AT-SPI Description property carries the state name.
        // The state→(token, label) decision lives in shared Rust
        // (`fauna_conversations::compose::recipient_resolve_status`); the AT-SPI
        // / test-attr plumbing below stays an idiomatic per-app render.
        let status = fauna_conversations::compose::recipient_resolve_status(state.resolve_state);
        let state_name = status.token.as_str();
        let text = status
            .label
            .as_ref()
            .map(|l| l.resolve(crate::i18n::strings::lookup))
            .unwrap_or_default();
        self.status_label.set_text(&text);
        // The AT-SPI bridge reads the state via accessible-description; testid.rs
        // already sets Description=ID, so we override here with the state.
        // Re-pin Property::Label so the test id stays on accessible name
        // even after `set_text` re-derives name from the visible text.
        self.status_label.update_property(&[
            gtk::accessible::Property::Label("recipient-resolve-status"),
            gtk::accessible::Property::Description(state_name),
        ]);
        // The in-process agent can't read the accessible Description, so also
        // carry the state as a readable `test-attr-state-*` CSS class
        // (`get_attr("recipient-resolve-status", "state")`).
        crate::testid::set_test_attr(&self.status_label, "state", state_name);

        // The class of the room about to be created, stated once a chip is
        // committed and before the first message goes out
        // (`conversation-rooms.md` § The three classes — "the recipient picker
        // says which class the new room will be"). Derived in shared Rust from
        // the committed chips and the home-nest choice; this only paints it.
        match fauna_conversations::prospective_room_class(&state.chips, state.include_home_nest) {
            Some(class) => {
                self.class_label.set_text(class.label());
                crate::testid::set_test_attr(&self.class_label, "class", class.attr_token());
                self.class_label.set_visible(true);
            }
            None => self.class_label.set_visible(false),
        }

        // The home-nest choice, straight off the shared state.
        let included = state.include_home_nest;
        self.home_nest_toggle.set_active(included);
        self.home_nest_toggle.set_label(if included {
            crate::i18n::strings::conversations::unified::ROOM_HOME_NEST_YES
        } else {
            crate::i18n::strings::conversations::unified::ROOM_HOME_NEST_NO
        });
        crate::testid::set_test_attr(
            &self.home_nest_toggle,
            "checked",
            if included { "true" } else { "false" },
        );

        *self.refreshing.borrow_mut() = false;
    }

    /// Turn on `recipient-picker-home-nest-toggle` — the new-thread composer
    /// only. `on_toggle` receives the value the click asks for; the painted
    /// `checked` state and label come back from the snapshot in [`Self::render`],
    /// so a refused or no-op toggle never leaves the button lying.
    pub fn enable_home_nest_toggle<H: Fn(bool) + 'static>(&self, on_toggle: H) {
        crate::testid::set_test_id(
            &self.home_nest_toggle,
            ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE,
        );
        crate::testid::set_test_attr(&self.home_nest_toggle, "checked", "false");
        self.home_nest_toggle
            .set_label(crate::i18n::strings::conversations::unified::ROOM_HOME_NEST_NO);
        self.home_nest_toggle.set_visible(true);
        let refreshing = self.refreshing.clone();
        self.home_nest_toggle.connect_toggled(move |btn| {
            if *refreshing.borrow() {
                return;
            }
            on_toggle(btn.is_active());
        });
    }

    /// Move keyboard focus to the input field.
    pub fn focus_input(&self) {
        self.input.grab_focus();
    }
}

/// One committed chip: a `gtk::Button` reading `"{address} ×"` whose click is the
/// chip's one gesture, `remove_recipient_chip(index)`. The id rides the button —
/// the element with the activation and an a11y peer (a `gtk::Label` is inert).
fn build_chip(text: &str, index: u32, on_remove: IndexCallback) -> gtk::Button {
    let chip = gtk::Button::with_label(&format!("{text} ×"));
    chip.add_css_class("pill");
    chip.set_margin_top(2);
    chip.set_margin_bottom(2);
    crate::testid::set_test_id(&chip, ids::RECIPIENT_PICKER_CHIP);
    chip.connect_clicked(move |_| {
        let callback = on_remove.borrow().clone();
        if let Some(cb) = callback {
            cb(index);
        }
    });
    chip
}

fn build_suggestion(text: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label.set_margin_start(8);
    label.set_margin_end(8);
    row.set_child(Some(&label));
    // The id rides the *row*, not the inner label: the row is what carries the
    // activation (the agent's `ListBoxRow` arm emits `row-activated` on the
    // parent list, which is the real click path), and a `gtk::Label` is inert —
    // tagging it made `click("recipient-picker-suggestion")` a silent no-op.
    // `find::text_of` falls through to joining descendant labels, so the row
    // still reads back as the suggestion's display text.
    crate::testid::set_test_id(&row, ids::RECIPIENT_PICKER_SUGGESTION);
    row
}

// Ensure glib is available for any future async hooks; harmless dead.
#[allow(dead_code)]
fn _glib_pin() {
    let _ = glib::user_data_dir();
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::address::TypedAddress;

    /// The suggestion row must be *clickable by the agent's real click path*,
    /// not merely findable. Before this was wired, the id sat on the inner
    /// `gtk::Label` and `suggestions_list` had no `row-activated` handler at
    /// all, so `click("recipient-picker-suggestion")` reached an inert widget
    /// and the agent answered `{"ok": true}` — a silent drop (`testing.md`
    /// point 11) that also meant a *real user's* click did nothing.
    #[test]
    fn clicking_a_suggestion_row_accepts_the_chip() {
        crate::testid::run_on_gtk_thread(|| {
            let accepted = Rc::new(std::cell::Cell::new(0u32));
            let picker = {
                let accepted = Rc::clone(&accepted);
                RecipientPicker::new(|_| {}, move || accepted.set(accepted.get() + 1))
            };

            let state = RecipientPickerState {
                raw_input: "alice@example.test".to_string(),
                suggestions: vec![TypedAddress::Email {
                    email_address: "alice@example.test".to_string(),
                }],
                ..Default::default()
            };
            picker.render(&state, &[]);

            let root: gtk::Widget = picker.root.clone().upcast();
            let found = crate::automation::find::find_in(&root, "recipient-picker-suggestion")
                .expect("the suggestion is findable by its ui.yaml id");
            assert!(
                found.downcast_ref::<gtk::ListBoxRow>().is_some(),
                "the id must ride the activatable row, not the inert inner label \
             (got {})",
                found.type_().name()
            );
            // Reading it back still yields the suggestion text (`text_of` joins
            // descendant labels), so moving the id off the label costs no coverage.
            assert!(
                crate::automation::find::text_of(&found).contains("alice@example.test"),
                "the row must still read back as the suggestion's display text"
            );

            let reply = crate::automation::agent::actuate_click(&found);
            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "the click must be delivered, not refused: {reply:?}"
            );
            assert_eq!(
                accepted.get(),
                1,
                "an unwired picker (the folder share dialog) falls back to the typed-text \
             accept callback"
            );
        });
    }
    /// Every descendant carrying `id`, in document order (the picker paints one
    /// widget per indexed row/chip).
    fn all_with_id(root: &gtk::Widget, id: &str, out: &mut Vec<gtk::Widget>) {
        if root.widget_name() == id {
            out.push(root.clone());
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            all_with_id(&c, id, out);
            child = c.next_sibling();
        }
    }

    /// Wired, a suggestion click is `accept_suggestion(i)` with the clicked row's
    /// own index (never the typed-text `on_accept`), the row paints the
    /// manager-derived label with the address beside it, and a chip's × is
    /// `remove_recipient_chip(i)`.
    #[test]
    fn suggestion_click_and_chip_remove_carry_their_index() {
        crate::testid::run_on_gtk_thread(|| {
            let typed_accepts = Rc::new(std::cell::Cell::new(0u32));
            let picker = {
                let typed_accepts = Rc::clone(&typed_accepts);
                RecipientPicker::new(|_| {}, move || typed_accepts.set(typed_accepts.get() + 1))
            };
            let accepted = Rc::new(RefCell::new(Vec::new()));
            let removed = Rc::new(RefCell::new(Vec::new()));
            {
                let accepted = Rc::clone(&accepted);
                picker.connect_suggestion_accepted(move |i| accepted.borrow_mut().push(i));
                let removed = Rc::clone(&removed);
                picker.connect_chip_removed(move |i| removed.borrow_mut().push(i));
            }

            let email = |a: &str| TypedAddress::Email {
                email_address: a.to_string(),
            };
            let state = RecipientPickerState {
                chips: vec![email("kept@example.test"), email("gone@example.test")],
                suggestions: vec![email("alice@example.test"), email("bob@example.test")],
                suggestion_displays: vec!["Alice".to_string(), "bob@example.test".to_string()],
                ..Default::default()
            };
            picker.render(&state, &[]);

            let root: gtk::Widget = picker.root.clone().upcast();
            let mut rows = Vec::new();
            all_with_id(&root, "recipient-picker-suggestion", &mut rows);
            assert_eq!(rows.len(), 2);
            assert_eq!(
                crate::automation::find::text_of(&rows[0]),
                "Alice · alice@example.test",
                "label, then the address beside it when they differ"
            );
            assert_eq!(
                crate::automation::find::text_of(&rows[1]),
                "bob@example.test",
                "a label equal to the address paints once"
            );
            crate::automation::agent::actuate_click(&rows[1]);
            assert_eq!(*accepted.borrow(), vec![1]);
            assert_eq!(typed_accepts.get(), 0, "never the typed-text path");

            let mut chips = Vec::new();
            all_with_id(&root, "recipient-picker-chip", &mut chips);
            assert_eq!(chips.len(), 2);
            assert!(crate::automation::find::text_of(&chips[1]).ends_with('×'));
            crate::automation::agent::actuate_click(&chips[1]);
            assert_eq!(*removed.borrow(), vec![1]);
        });
    }

    /// The home-nest toggle is the new-thread composer's alone, mirrors the
    /// shared `include_home_nest` in its `checked` attribute, and a click asks
    /// the manager for the NEW value without the render echoing it back.
    #[test]
    fn the_home_nest_toggle_follows_the_shared_state_and_reports_clicks() {
        crate::testid::run_on_gtk_thread(|| {
            let plain = RecipientPicker::new(|_| {}, || {});
            let root: gtk::Widget = plain.root.clone().upcast();
            assert!(
                crate::testid::find_by_test_id(&root, ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE)
                    .is_none(),
                "the add-participant and share dialogs reuse the widget without the toggle"
            );

            let asked = Rc::new(std::cell::RefCell::new(Vec::new()));
            let picker = RecipientPicker::new(|_| {}, || {});
            {
                let asked = Rc::clone(&asked);
                picker.enable_home_nest_toggle(move |v| asked.borrow_mut().push(v));
            }
            let root: gtk::Widget = picker.root.clone().upcast();
            let toggle =
                crate::testid::find_by_test_id(&root, ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE)
                    .expect("the composer paints the toggle");

            let mut state = RecipientPickerState {
                include_home_nest: true,
                ..Default::default()
            };
            picker.render(&state, &[]);
            assert!(
                toggle
                    .css_classes()
                    .iter()
                    .any(|c| c == "test-attr-checked-true")
            );
            assert!(
                asked.borrow().is_empty(),
                "a render must not report itself as a click"
            );

            state.include_home_nest = false;
            picker.render(&state, &[]);
            assert!(
                toggle
                    .css_classes()
                    .iter()
                    .any(|c| c == "test-attr-checked-false")
            );

            let reply = crate::automation::agent::actuate_click(&toggle);
            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "{reply:?}"
            );
            assert_eq!(*asked.borrow(), vec![true]);
        });
    }
}
