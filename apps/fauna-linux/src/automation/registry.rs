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
//! click gesture is not actuable (the critical-alert row), a button is.
use super::find;
use adw::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;

/// A GObject type name — what `widget_name()` answers when no test id was set.
fn is_test_id(name: &str) -> bool {
    !name.is_empty()
        && !(name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('-'))
}

/// The frame, as `{"elements": [...]}`.
pub fn snapshot_json() -> Value {
    json!({ "elements": frame() })
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
    })
}

/// Whether [`super::agent::actuate_click`] would drive `w` rather than refuse
/// it — the same kind ladder, in the same order.
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
        return row.parent().is_some_and(|p| p.is::<gtk::ListBox>()) || row.is_activatable();
    }
    if w.is::<gtk::Entry>() {
        return false;
    }
    has_click_gesture(w)
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
