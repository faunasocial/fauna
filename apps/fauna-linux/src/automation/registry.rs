//! `GET /registry` — the whole current frame as records, linux's leg of
//! `fauna_e2e_agent::ElementKind::Registry` (field contract:
//! `tests/e2e-unified/drivers/base.py::registry_snapshot`; the tui, apple,
//! windows and web surfaces serve the same shape).
//!
//! **One walk, the driver's own addressing.** The frame is every showing
//! widget that carries a test id, walked from [`super::find`]'s search roots
//! with its [`super::find::is_showing`] pruning — the exact tree `find`/`count`
//! resolve against — so a record's `index` is the index a driver re-drives it
//! with (`find_all(id).nth(index)`), and a surface this frame lists is one the
//! driver can reach. `scope` is the chain of id-carrying showing ancestors in
//! the same `id[index]` DSL.
//!
//! **`actuable` is what [`super::agent::actuate_click`] would drive**, decided
//! by the same widget-kind ladder, never a guess from the id: a label with no
//! click gesture is not actuable (the critical-alert row), a button is. The
//! one narrowing: the door's last rung presses ANY click gesture, gtk's own
//! included, while a record counts only one the app gave (see [`is_actuable`]),
//! so a read label's selection gesture or a window's never reads as a control.
use super::find;
use adw::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;

/// A GObject type name — what `widget_name()` answers when no test id was set.
pub(crate) fn is_test_id(name: &str) -> bool {
    !name.is_empty()
        && !(name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('-'))
}

/// The frame, as `{"elements": [...], "reads_owed": n}`.
///
/// `reads_owed` is the count of one-shot reads still in flight when the frame
/// was read (`crate::async_helper::reads_owed`), taken BEFORE every message
/// already delivered is applied and the frame walked — so a zero says the
/// frame holds every read that had finished; a page whose first paint is
/// still owed reads above zero (`tests/e2e-unified/drivers/base.py::
/// registry_frame`).
pub fn snapshot_json() -> Value {
    let reads_owed = crate::async_helper::settle_for_frame_read();
    // While any read is owed, the source lines that started them, so a read
    // that never returns names itself in the sweep's report (convention 6).
    let owed_by: serde_json::Map<String, Value> = if reads_owed == 0 {
        serde_json::Map::new()
    } else {
        crate::async_helper::reads_owed_by()
            .into_iter()
            .map(|(at, n)| (at, json!(n)))
            .collect()
    };
    json!({ "elements": frame(), "reads_owed": reads_owed, "reads_owed_by": owed_by })
}

fn frame() -> Vec<Value> {
    struct Walk {
        counters: HashMap<String, usize>,
        out: Vec<Value>,
    }
    fn visit(w: &gtk::Widget, root: &gtk::Widget, scope: &mut Vec<String>, walk: &mut Walk) {
        let name = w.widget_name();
        let pushed = if is_test_id(&name) {
            let id = name.to_string();
            let index = {
                let n = walk.counters.entry(id.clone()).or_insert(0);
                let i = *n;
                *n += 1;
                i
            };
            walk.out.push(record(w, root, &id, index, scope));
            scope.push(format!("{id}[{index}]"));
            true
        } else {
            false
        };
        let mut child = w.first_child();
        while let Some(c) = child {
            if find::is_showing(&c) {
                visit(&c, root, scope, walk);
            }
            child = c.next_sibling();
        }
        if pushed {
            scope.pop();
        }
    }
    let mut walk = Walk {
        counters: HashMap::new(),
        out: Vec::new(),
    };
    for root in find::search_roots() {
        visit(&root, &root, &mut Vec::new(), &mut walk);
    }
    walk.out
}

fn record(w: &gtk::Widget, root: &gtk::Widget, id: &str, index: usize, scope: &[String]) -> Value {
    let frame = w
        .compute_bounds(root)
        .map(|b| {
            format!(
                "{},{},{},{}",
                b.x().round() as i64,
                b.y().round() as i64,
                b.width().round() as i64,
                b.height().round() as i64
            )
        })
        .unwrap_or_default();
    json!({
        "id": id,
        "index": index,
        "enabled": find::is_enabled(w),
        "declares_enabled": crate::offline_gate::is_declared(w),
        "actuable": is_actuable(w),
        "editable": is_editable(w),
        "scope": scope.join("/"),
        "frame": frame,
        "text": find::text_of(w),
        "attrs": attrs(w),
    })
}

/// The record's `attrs` — the state a whole-frame comparison must see when the
/// text stays put: every `test-attr-{name}-{value}` marker class
/// ([`crate::testid::set_test_attr`]; split at the first `-` after the prefix,
/// exact for the one-word names `state`/`checked`/`selected`/`copied`), an
/// unmarked toggle's live `checked` and `state` (the agent's `attr` fallback),
/// and `selected` for a selected list row or a pressed toggle button — the
/// current choice of a one-of-N control. Free-text attributes
/// ([`crate::testid::set_test_attr_text`]) are keyed object data with no
/// enumeration, so they are not listed.
fn attrs(w: &gtk::Widget) -> Value {
    let mut out = serde_json::Map::new();
    for class in w.css_classes() {
        if let Some((name, value)) = class
            .strip_prefix("test-attr-")
            .and_then(|rest| rest.split_once('-'))
        {
            out.insert(name.to_owned(), json!(value));
        }
    }
    let active = if let Some(c) = w.downcast_ref::<gtk::CheckButton>() {
        Some(c.is_active())
    } else if let Some(s) = w.downcast_ref::<gtk::Switch>() {
        Some(s.is_active())
    } else {
        w.downcast_ref::<adw::SwitchRow>().map(|r| r.is_active())
    };
    if let Some(on) = active {
        let on = json!(if on { "true" } else { "false" });
        out.entry("checked").or_insert_with(|| on.clone());
        out.entry("state").or_insert(on);
    }
    let selected = w
        .downcast_ref::<gtk::ListBoxRow>()
        .is_some_and(|r| r.is_selected())
        || w.downcast_ref::<gtk::ToggleButton>()
            .is_some_and(|t| t.is_active());
    if selected {
        out.entry("selected").or_insert(json!("true"));
    }
    Value::Object(out)
}

/// Whether [`super::agent::actuate_click`] would drive `w` rather than refuse
/// it — the same kind ladder, in the same order. A field is filled, never
/// clicked; a row the door answers 409 for (not activatable, in a list that
/// does not select) is not actuable; and a click gesture counts only where the
/// app gave it — not gtk's own on a window, a scrolled view or a text view
/// ([`crate::read_text::gesture_is_internal`]), nor the selection gesture of a
/// label the read-text rule made selectable (selecting is not acting; that
/// rule never makes a label carrying an app gesture selectable).
pub(crate) fn is_actuable(w: &gtk::Widget) -> bool {
    if w.is::<gtk::Button>()
        || w.is::<gtk::Switch>()
        || w.is::<adw::SwitchRow>()
        || w.is::<adw::ExpanderRow>()
        || w.is::<gtk::CheckButton>()
        || w.is::<gtk::MenuButton>()
    {
        return true;
    }
    if let Some(row) = w.downcast_ref::<gtk::ListBoxRow>() {
        let Some(list) = row.parent().and_downcast::<gtk::ListBox>() else {
            return row.is_activatable();
        };
        let selects = row.is_selectable() && list.selection_mode() != gtk::SelectionMode::None;
        return selects || row.is_activatable();
    }
    if w.is::<gtk::Editable>() || w.is::<gtk::Entry>() || w.is::<gtk::TextView>() {
        return false;
    }
    if w.downcast_ref::<gtk::Label>()
        .is_some_and(|l| l.is_selectable())
    {
        return false;
    }
    !crate::read_text::gesture_is_internal(w) && has_click_gesture(w)
}

fn has_click_gesture(w: &gtk::Widget) -> bool {
    let controllers = w.observe_controllers();
    (0..controllers.n_items()).any(|i| {
        controllers
            .item(i)
            .and_downcast::<gtk::GestureClick>()
            .is_some()
    })
}

/// Whether the driver can type into (or select on) `w`.
fn is_editable(w: &gtk::Widget) -> bool {
    w.is::<gtk::Editable>()
        || w.is::<gtk::Entry>()
        || w.is::<gtk::TextView>()
        || w.is::<gtk::DropDown>()
        || w.is::<adw::ComboRow>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    #[test]
    fn read_text_windows_fields_and_inert_rows_are_not_actuable() {
        run_on_gtk_thread(|| {
            // A label the read-text rule made selectable carries gtk's own
            // selection gesture; selecting text is not acting on it.
            let read = gtk::Label::new(Some("Quota: 5 GB"));
            read.set_selectable(true);
            assert!(!is_actuable(read.upcast_ref()), "a selectable read label");
            assert!(!is_actuable(gtk::Window::new().upcast_ref()), "a window");
            assert!(
                !is_actuable(gtk::ScrolledWindow::new().upcast_ref()),
                "a scrolled view"
            );
            assert!(
                !is_actuable(gtk::SearchEntry::new().upcast_ref()),
                "a search field"
            );
            assert!(
                !is_actuable(gtk::TextView::new().upcast_ref()),
                "a text view"
            );
            // A row the click door answers 409 for: not activatable, and its
            // list does not select.
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            let row = adw::ActionRow::new();
            row.set_activatable(false);
            list.append(&row);
            assert!(!is_actuable(row.upcast_ref()), "an inert row");
        });
    }

    #[test]
    fn buttons_and_app_click_gestures_stay_actuable() {
        run_on_gtk_thread(|| {
            assert!(is_actuable(gtk::Button::with_label("Save").upcast_ref()));
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            chip.add_controller(gtk::GestureClick::new());
            assert!(is_actuable(chip.upcast_ref()), "a box the app gave a click");
            let more = gtk::Label::new(Some("+3 more"));
            more.add_controller(gtk::GestureClick::new());
            assert!(
                is_actuable(more.upcast_ref()),
                "a label the app gave a click"
            );
            let list = gtk::ListBox::new();
            let row = gtk::ListBoxRow::new();
            list.append(&row);
            assert!(is_actuable(row.upcast_ref()), "an activatable row");
        });
    }
}
