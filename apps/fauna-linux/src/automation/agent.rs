//! Element introspection + actuation ops, performed on the GTK main thread.
//!
//! The automation HTTP server ([`super::server`]) runs on a background thread
//! and forwards each request here as an [`ElementOp`] over an async channel; the
//! GTK main loop drains it, runs [`perform`] (which touches widgets and so MUST
//! be on the main thread), and replies over the op's one-shot channel. This is
//! how native `gtk::Switch`/`CheckButton`/`DropDown` become actuable — we set
//! their state directly rather than via an AT-SPI action they don't expose.
use super::find;
use adw::prelude::*;
use serde_json::{Value, json};

// Op/request types + the HTTP front-end live in the shared `fauna-e2e-agent`
// crate (both direct-Rust clients — linux and cli — host the same agent);
// this module keeps the GTK-specific `perform` half.
pub use fauna_e2e_agent::{ElementKind, ElementOp, ElementReq};

/// Run an op on the GTK main thread; returns the JSON body the server responds
/// with. Predicate reads (`Visible`, `Enabled`, `Count`) return safe defaults
/// when the element is absent (no `error` key → HTTP 200) — "missing" and
/// "present but false" read the same, matching the driver's `is_enabled`/
/// `is_visible` contract (`drivers/http_bridge.py`). `Text` reports
/// `{"error": "not found"}` on absence instead, like actuation, so "not on
/// screen" is distinguishable from "on screen but empty" — matching apple/android/windows, which already raise on a missing
/// text read (web deliberately still answers `""`; tui shares this same gap,
/// tracked separately). Actuation returns `{"error": …}` when absent so the
/// server can surface a 4xx, matching the AT-SPI bridge's behaviour.
pub fn perform(req: &ElementReq) -> Value {
    let scope = &req.scope;
    let id = &req.id;
    match req.kind {
        ElementKind::Count => json!({ "count": find::find_all_scoped(scope, id).len() }),
        ElementKind::Visible => json!({
            "visible": find::find_scoped(scope, id).map(|w| find::is_visible(&w)).unwrap_or(false)
        }),
        ElementKind::Enabled => json!({
            "enabled": find::find_scoped(scope, id).map(|w| find::is_enabled(&w)).unwrap_or(false)
        }),
        ElementKind::Text => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => json!({ "text": find::text_of(&w) }),
            None => json!({ "error": "not found" }),
        },
        ElementKind::Attr => json!({ "value": attr(scope, id, &req.arg, req.index) }),
        ElementKind::Click => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => gate(&w, "click", req).unwrap_or_else(|| actuate_click(&w)),
            None => json!({ "error": "not found" }),
        },
        ElementKind::DoubleClick => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => gate(&w, "double-click", req).unwrap_or_else(|| actuate_double_click(&w)),
            None => json!({ "error": "not found" }),
        },
        ElementKind::Type => match find::find_scoped(scope, id) {
            Some(w) => gate(&w, "type", req).unwrap_or_else(|| type_text(&w, &req.arg)),
            None => json!({ "error": "not found" }),
        },
        ElementKind::Clear => match find::find_scoped(scope, id) {
            Some(w) => gate(&w, "clear", req).unwrap_or_else(|| clear(&w)),
            None => json!({ "error": "not found" }),
        },
        ElementKind::Select => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => gate(&w, "select", req).unwrap_or_else(|| select(&w, &req.arg)),
            None => json!({ "error": "not found" }),
        },
        // Deliberately UNGATED: bringing a widget into its scroll viewport is
        // viewport positioning, not actuation — and a test that scrolls to a
        // disabled control precisely in order to assert that it *is* disabled
        // must keep working (the same reason the read routes above are ungated).
        ElementKind::ScrollIntoView => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => scroll_into_view(&w),
            None => json!({ "error": "not found" }),
        },
        ElementKind::Key => match find::find_indexed_scoped(scope, id, req.index) {
            Some(w) => gate(&w, "key", req).unwrap_or_else(|| press_key(&w, &req.arg)),
            None => json!({ "error": "not found" }),
        },
        ElementKind::WindowClose => match find::active_window() {
            // `close()` emits `close-request`, the same signal the X button
            // fires, so the close-to-tray handler runs for real. If it quits,
            // the GApplication loop is asked to exit *after* this returns, so the
            // reply still flushes; if it hides, the window stays alive but hidden.
            Some(w) => {
                w.close();
                json!({ "ok": true })
            }
            None => json!({ "error": "no active window" }),
        },
        // The whole frame, walked over the same showing tree `find` resolves
        // against (`fauna_e2e_agent::ElementKind::Registry`).
        ElementKind::Registry => super::registry::snapshot_json(),
        ElementKind::ClipboardText => clipboard_text(),
    }
}

/// The display clipboard's text, read from GDK's own clipboard object — what
/// `crate::clipboard::copy_text` set and what the X selection then serves —
/// never a record of what the app meant to copy. Synchronous by construction:
/// a clipboard this process owns answers from its local content provider, and
/// one another client took over answers `null` (it is no longer ours to read
/// without an async round trip the GTK-thread op cannot make).
fn clipboard_text() -> Value {
    let Some(display) = gtk::gdk::Display::default() else {
        return json!({ "error": "no display" });
    };
    let text = display
        .clipboard()
        .content()
        .and_then(|provider| provider.value(gtk::glib::Type::STRING).ok())
        .and_then(|value| value.get::<String>().ok());
    json!({ "text": text })
}

/// Is refusal linux's DEFAULT? **Yes, since 2026-09-10.**
///
/// It was staged first, in the order `e2e-conventions.md` § convention 11
/// prescribes: land the refusal behind a flag whose permissive mode still
/// drives but logs one greppable marker per violation, sweep the whole
/// `--app linux` suite in that mode so a single run enumerates every offending
/// call site with no new red, triage that list to empty, *"and only then make
/// refusal the default"*. The enumerating sweep ran 2026-09-10 (3137 passed,
/// the gate's own probe the only violating call, its marker present — so the
/// detector demonstrably armed); every earlier offender had been fixed.
///
/// **Keep the permissive mode** (`--permissive-actuation`, which sets
/// `FAUNA_E2E_PERMISSIVE_ACTUATION` and wins over this default): it is the
/// instrument the next broad change to this app re-measures itself with, not
/// staging scaffolding to delete. A strict sweep stops at the first offender
/// per test and hides the rest.
const LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT: bool = true;

/// Consult the widget's LIVE sensitivity before an actuation route drives it.
///
/// `Some(refusal)` → reply it verbatim (it carries its own 409); `None` → drive
/// the control. Linux needs no registration surface for this: GTK's
/// `is_sensitive()` is already the *effective*, ancestor-inclusive predicate
/// that apple had to assemble by hand (`AutomationRegistry.folding`), so a
/// control greyed only by an insensitive ancestor is refused here too.
fn gate(w: &gtk::Widget, route: &str, req: &ElementReq) -> Option<Value> {
    fauna_e2e_agent::gate_actuation(
        route,
        &req.id,
        req.index,
        find::is_enabled(w),
        LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT,
    )
}

/// `/element/attr`: `disabled` ← `!is_sensitive`; any other `name` is read from
/// the `test-attr-{name}-{value}` CSS class written by `testid::set_test_attr`
/// (the carrier the AT-SPI bridge encoded in the accessible Description, which
/// GTK exposes no getter for — e.g. `recipient-resolve-status`'s `state`).
/// Returns a JSON value (string or null when absent). Deliberately NOT given
/// `Text`'s not-found-vs-empty split: `get_attr`
/// already collapses a missing widget and a present-but-unset attribute to
/// the same `None` driver-side (`drivers/http_bridge.py::get_attr`), so
/// distinguishing them here would be invisible to every caller.
/// Read one attribute off `id`'s `index`-th match.
///
/// ⚠ `index` is load-bearing and was DROPPED here until 2026-08-31: this route
/// resolved with `find_scoped`, so every `get_attr(id, …, index=N)` answered
/// from the FIRST match, whatever N was. Silent by construction — an indexed
/// row's neighbour usually holds the same value, so the wrong answer is
/// normally the right one, and the bug only surfaces once two rows differ
/// (found by `test_mail_import.py`'s "deselect one mailbox, read that mailbox
/// back" walk, which read INBOX's `state` while asserting about Archive's).
/// `find_indexed_scoped` at index 0 IS `find_scoped`, so this is a strict
/// superset of the old behaviour — no index-0 caller changes.
fn attr(scope: &[find::ScopeStep], id: &str, name: &str, index: usize) -> Value {
    let Some(w) = find::find_indexed_scoped(scope, id, index) else {
        return Value::Null;
    };
    match name {
        "disabled" => json!(if find::is_enabled(&w) {
            "false"
        } else {
            "true"
        }),
        // `state`: an explicit `test-attr-state-{value}` marker class ALWAYS
        // wins (e.g. the SwitchRow `mail-settings-serve-here-toggle` and the
        // CheckButton `admin-…-auto-renew`, both of which encode their own
        // `on`/`off`). Only when no such marker is present do we fall back to
        // reading a toggleable widget's live checked/on state as a
        // `"true"`/`"false"` string (the same convention `disabled` uses above)
        // — the uniform `driver.get_attr(id, "state")` idiom for any
        // unmarked CheckButton/Switch/SwitchRow.
        //
        // An unmarked IMAGE element answers its paint the same way, off the
        // live picture rather than a marker the paint site would have to keep
        // in step: `painted` once decoded bytes are its paintable,
        // `placeholder` while it has none. That is the one headless witness of
        // fetch → open → decode → paint (`post-image`; `ui/media.md`
        // § Encryption at rest), since a sealed item handed to the decoder
        // unopened leaves the picture empty without an error anywhere.
        "state" => {
            let marker = w
                .css_classes()
                .iter()
                .find_map(|c| c.strip_prefix("test-attr-state-").map(|v| json!(v)));
            marker.unwrap_or_else(|| {
                if let Some(c) = w.downcast_ref::<gtk::CheckButton>() {
                    json!(if c.is_active() { "true" } else { "false" })
                } else if let Some(s) = w.downcast_ref::<gtk::Switch>() {
                    json!(if s.is_active() { "true" } else { "false" })
                } else if let Some(r) = w.downcast_ref::<adw::SwitchRow>() {
                    json!(if r.is_active() { "true" } else { "false" })
                } else if let Some(picture) = picture_of(&w) {
                    json!(if picture.paintable().is_some() {
                        "painted"
                    } else {
                        "placeholder"
                    })
                } else if let Some(image) = w.downcast_ref::<gtk::Image>() {
                    // A `gtk::Image` starts on an icon and is painted by
                    // `set_paintable` (`media-thumbnail`, `views/media/item.rs
                    // ::paint_thumbnail`): an icon-backed image has no
                    // paintable, so the same read tells the picture from the
                    // placeholder.
                    json!(if image.storage_type() == gtk::ImageType::Paintable {
                        "painted"
                    } else {
                        "placeholder"
                    })
                } else {
                    Value::Null
                }
            })
        }
        // `visible`: the compose field's RENDERED text — the native twin of
        // windows' `AutomationProperties.HelpText` and apple's
        // `Entry.visibleText` (`compose_visible_text()`,
        // `tests/e2e-unified/actions/conversations.py`). `text_of()`
        // (`find.rs`) — what plain `get_text` reads — deliberately always
        // includes hidden chars (the draft round-trip needs the full
        // markdown SOURCE, not the marker-concealed display), so it cannot
        // answer "what does the user see"; this reads the SAME buffer with
        // `include_hidden_chars: false` instead, honouring the
        // `md-hidden` tag `compose_decoration.rs` applies over concealed
        // markers. `Value::Null` for any non-`TextView` widget — no other
        // element publishes a distinct rendered-vs-source text today.
        // `in-viewport`: whether the widget's vertical centre lies inside the
        // visible band of its nearest `gtk::ScrolledWindow` — the observable
        // for "brought into view" (`conversations.md` § The selected message).
        // GTK realizes every row of a `gtk::Box`, so presence in the registry
        // says nothing about scroll position; a widget with no scrollable
        // ancestor cannot be scrolled out of view and answers by whether it is
        // mapped. `Value::Null` only when its bounds cannot be computed.
        "in-viewport" => in_viewport(&w),
        "visible" => {
            if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
                let buffer = tv.buffer();
                json!(
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string()
                )
            } else {
                Value::Null
            }
        }
        // `text-runs`: the text field's STYLING as its buffer holds it — the
        // observable for "formatting shows as you type" (`conversations.md`
        // § Compose-field inline markdown styling), which neither `get_text`
        // (the source) nor `visible` (the concealed display) can answer. A
        // JSON-encoded list, one record per run of characters sharing a tag set:
        // its source text (hidden chars included) and each applied tag's name
        // plus the look it sets. Read off the live `gtk::TextBuffer`, never
        // recomputed from the shared decoration plan: the question is whether
        // the app APPLIED the styling, and the plan is only what it was told to
        // apply. `Value::Null` for any non-`TextView` widget.
        "text-runs" => w
            .downcast_ref::<gtk::TextView>()
            .map_or(Value::Null, |tv| json!(text_runs(&tv.buffer()).to_string())),
        // `checked`: a toggle's live on/off as `"true"`/`"false"` — the name
        // tui publishes for every checkbox and web answers off `data-checked`,
        // so one cross-app `get_attr(id, "checked")` reads it everywhere. An
        // explicit `test-attr-checked-{value}` marker still wins, as for
        // `state` above.
        "checked" => w
            .css_classes()
            .iter()
            .find_map(|c| c.strip_prefix("test-attr-checked-").map(|v| json!(v)))
            .unwrap_or_else(|| {
                let active = if let Some(c) = w.downcast_ref::<gtk::CheckButton>() {
                    Some(c.is_active())
                } else if let Some(s) = w.downcast_ref::<gtk::Switch>() {
                    Some(s.is_active())
                } else {
                    w.downcast_ref::<adw::SwitchRow>().map(|r| r.is_active())
                };
                active.map_or(Value::Null, |on| json!(if on { "true" } else { "false" }))
            }),
        // `options`: every option a picker PAINTS, JSON-encoded in model order —
        // the contract `drivers/base.py::option_texts` reads, which tui and web
        // already serve. The selected-value read (`get_text`) cannot see an
        // option's absence, and a legal-set picker (`participants.md` § The
        // assignment picker) is asserted precisely by what it does NOT offer.
        // Read off the same `StringObject` model `select` matches against, so
        // the list reported is the list a select would search. `Value::Null`
        // for a non-picker, never `"[]"`: zero options is a distinct fact.
        "options" => {
            let model = if let Some(d) = w.downcast_ref::<gtk::DropDown>() {
                Some(d.model())
            } else {
                w.downcast_ref::<adw::ComboRow>().map(|c| c.model())
            };
            model.map_or(Value::Null, |model| {
                let options: Vec<String> = model_strings(model.as_ref())
                    .into_iter()
                    .map(|(_, s)| s)
                    .collect();
                json!(serde_json::to_string(&options).unwrap_or_default())
            })
        }
        _ => {
            let prefix = format!("test-attr-{name}-");
            w.css_classes()
                .iter()
                .find_map(|c| c.strip_prefix(&prefix).map(|v| json!(v)))
                .unwrap_or(Value::Null)
        }
    }
}

/// `text-runs` (see [`attr`]): split the buffer at every tag toggle and describe
/// each run's tags.
fn text_runs(buffer: &gtk::TextBuffer) -> Value {
    let end = buffer.end_iter();
    let mut at = buffer.start_iter();
    let mut runs = Vec::new();
    while at.offset() < end.offset() {
        let mut next = at;
        if !next.forward_to_tag_toggle(None::<&gtk::TextTag>) || next.offset() <= at.offset() {
            next = end;
        }
        runs.push(json!({
            "text": buffer.text(&at, &next, true).to_string(),
            "tags": at.tags().iter().map(tag_look).collect::<Vec<_>>(),
        }));
        at = next;
    }
    Value::Array(runs)
}

/// One applied tag: its name and the properties it SETS (an unset property is
/// `null`, so a reader never mistakes a default for a style).
fn tag_look(tag: &gtk::TextTag) -> Value {
    let set = |name: &str| tag.property::<bool>(&format!("{name}-set"));
    json!({
        "name": tag.property::<Option<String>>("name"),
        "weight": set("weight").then(|| tag.property::<i32>("weight")),
        "family": set("family").then(|| tag.property::<Option<String>>("family")).flatten(),
        "scale": set("scale").then(|| tag.property::<f64>("scale")),
        "left_margin": set("left-margin").then(|| tag.property::<i32>("left-margin")),
        "invisible": set("invisible") && tag.property::<bool>("invisible"),
    })
}

/// `/element/key`: one named key in a text field, driven through the widget's
/// own `move-cursor` keybinding signal — the signal GTK's ArrowLeft/ArrowRight/
/// Home/End bindings emit — so the buffer's `cursor-position` notify fires and
/// every caret-move handler runs exactly as for a real key (the compose field's
/// caret-edge marker reveal among them). Only the caret keys have a consumer;
/// any other key or widget is refused, never acked (point 11).
///
/// Escape is the one key bound by a shortcut rather than a widget signal (a
/// dialog's "close me"), so it runs [`press_escape`] instead.
fn press_key(w: &gtk::Widget, key: &str) -> Value {
    if key == "Escape" {
        return press_escape(w);
    }
    // Enter on a focused control is its keyboard activation — the same effect
    // a click has (a row's `row-activated`, a button's `clicked`), and it
    // returns at once while whatever the activation started runs on. A widget
    // a click would refuse is refused here too, naming why (convention 11).
    if key == "Enter" && !w.is::<gtk::TextView>() {
        if !super::registry::is_actuable(w) {
            return json!({
                "error": format!(
                    "press_key \"Enter\": element is not actuable ({})",
                    w.type_().name()
                )
            });
        }
        return actuate_click(w);
    }
    let Some(tv) = w.downcast_ref::<gtk::TextView>() else {
        return json!({ "error": format!("press_key {key:?}: only a text view takes a named key on linux") });
    };
    let (step, count) = match key {
        "ArrowLeft" => (gtk::MovementStep::VisualPositions, -1),
        "ArrowRight" => (gtk::MovementStep::VisualPositions, 1),
        "Home" => (gtk::MovementStep::DisplayLineEnds, -1),
        "End" => (gtk::MovementStep::DisplayLineEnds, 1),
        other => return json!({ "error": format!("press_key {other:?} is not driven on linux") }),
    };
    tv.emit_move_cursor(step, count, false);
    json!({ "ok": true })
}

/// Escape pressed with focus on `w`: the innermost Escape shortcut on `w`'s own
/// path to its window activates, as GTK's bubble phase would pick it for a
/// real key — so the handler under test is the product's own binding (the
/// event form's close, `views/events/event_form.rs`), and a path that binds no
/// Escape is refused rather than acked (point 11). Walking `w`'s ancestry, not
/// the active window's, keeps a background window's Escape (the main window's
/// minimize-to-tray) out of reach.
fn press_escape(w: &gtk::Widget) -> Value {
    let escape = gtk::ShortcutTrigger::parse_string("Escape").expect("valid shortcut trigger");
    let mut at = Some(w.clone());
    while let Some(widget) = at {
        let controllers = widget.observe_controllers();
        for i in 0..controllers.n_items() {
            let Some(shortcuts) = controllers
                .item(i)
                .and_downcast::<gtk::ShortcutController>()
            else {
                continue;
            };
            for j in 0..shortcuts.n_items() {
                let Some(shortcut) = shortcuts.item(j).and_downcast::<gtk::Shortcut>() else {
                    continue;
                };
                let bound = shortcut.trigger().is_some_and(|t| t.equal(&escape));
                if let (true, Some(action)) = (bound, shortcut.action())
                    && action.activate(gtk::ShortcutActionFlags::empty(), &widget, None)
                {
                    return json!({ "ok": true });
                }
            }
        }
        at = widget.parent();
    }
    json!({ "error": format!("press_key \"Escape\": nothing on the path from {} binds Escape", w.type_().name()) })
}

/// The `gtk::Picture` an image element paints into: the tagged widget itself,
/// or the picture a tagged button wraps — the shape `build_post_image` gives
/// `post-image`, whose id sits on the button a click opens the lightbox from.
fn picture_of(w: &gtk::Widget) -> Option<gtk::Picture> {
    if let Some(picture) = w.downcast_ref::<gtk::Picture>() {
        return Some(picture.clone());
    }
    w.downcast_ref::<gtk::Button>()?
        .child()?
        .downcast::<gtk::Picture>()
        .ok()
}

/// Targeted scroll-into-view: center the widget in its nearest
/// `ScrolledWindow` ancestor's viewport by driving the real vadjustment — the
/// same adjustment the engagement-cue viewport observer samples, so an e2e
/// dwell driven through this is an honest exposure, not an injected one.
fn in_viewport(w: &gtk::Widget) -> Value {
    let Some(scrolled) = nearest_scrolled_window(w) else {
        return json!(if w.is_mapped() { "true" } else { "false" });
    };
    let Some(bounds) = w.compute_bounds(&scrolled) else {
        return Value::Null;
    };
    let centre = f64::from(bounds.y()) + f64::from(bounds.height()) / 2.0;
    let inside = w.is_mapped() && centre >= 0.0 && centre < f64::from(scrolled.height());
    json!(if inside { "true" } else { "false" })
}

fn nearest_scrolled_window(w: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    let mut ancestor = w.parent();
    while let Some(a) = ancestor {
        match a.downcast::<gtk::ScrolledWindow>() {
            Ok(s) => return Some(s),
            Err(a) => ancestor = a.parent(),
        }
    }
    None
}

fn scroll_into_view(w: &gtk::Widget) -> Value {
    let Some(scrolled) = nearest_scrolled_window(w) else {
        return json!({ "error": "no scrollable ancestor" });
    };
    // The centering math is the production `conversations::detail`
    // implementation — this always-dev-only module depends on it, not the
    // other way, since `detail.rs` still has to compile in release builds.
    if crate::views::conversations::detail::scroll_widget_into_view(&scrolled, w) {
        json!({ "found": true })
    } else {
        json!({ "error": "could not compute bounds" })
    }
}

/// Actuate a "click". Native `Switch`/`SwitchRow` flip their state directly
/// (they expose no click action); a `gtk::Button` (and its subclasses) emits
/// `clicked`; everything else (CheckButton incl. radios, ListBoxRow, …) goes
/// through `activate()`, which is the widget's own activation entry point —
/// equivalent to a user click.
pub(crate) fn actuate_click(w: &gtk::Widget) -> Value {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        // Not `activate()`: on a button that animates a press-then-release and
        // emits `clicked` only when the animation's timeout fires, and an
        // unrealize in between abandons it silently. A list that rebuilds its
        // rows on a snapshot tick (the conversation list during a re-drain)
        // unrealizes the clicked card inside that window, so the agent replied
        // `ok` for a click that never ran. `clicked`
        // is what a real pointer release emits, synchronously; emitting it here
        // lands the click before the reply (convention 11), and it still runs
        // the button's action-name, as a pointer release does.
        b.emit_clicked();
    } else if let Some(s) = w.downcast_ref::<gtk::Switch>() {
        s.set_active(!s.is_active());
    } else if let Some(r) = w.downcast_ref::<adw::SwitchRow>() {
        r.set_active(!r.is_active());
    } else if let Some(e) = w.downcast_ref::<adw::ExpanderRow>() {
        // Toggle the disclosure — the revealed child rows (e.g. a folder's
        // member roster / path editors / delete button) are not child-visible
        // while collapsed, so the test must expand the row to reach them.
        e.set_expanded(!e.is_expanded());
    } else if let Some(row) = w.downcast_ref::<gtk::ListBoxRow>() {
        // A `gtk::ListBoxRow` has no activate signal of its own, so
        // `gtk_widget_activate` is a no-op on it — the `row-activated` handler
        // (e.g. the contacts list → profile tap-through, `app.rs`) would never
        // fire. A real pointer release on the row is the parent `GtkListBox`
        // emitting `row-activated`, so emit it directly here. The agent resolves
        // a row by its widget name (the contact row carries the peer actor id),
        // so a test can drive a list tap-through by clicking that id.
        if let Some(list) = row.parent().and_downcast::<gtk::ListBox>() {
            // What a real pointer release (or Enter) on a row does is GTK's
            // select-and-activate: the row is selected — where the list selects
            // at all — and THEN `row-activated` fires. Emitting the signal alone
            // left every list that acts on SELECTION unmoved: the main sidebar
            // navigates on `row-selected`, so a driven `feed-tab`/`settings-tab`
            // press changed no page while its reply said it had (convention 11),
            // and the feed page carried its own `row-activated` → `select_row`
            // bridge to paper over the same gap.
            let selects = row.is_selectable() && list.selection_mode() != gtk::SelectionMode::None;
            if selects && !row.is_selected() {
                list.select_row(Some(row));
            }
            // …and `row-activated` fires only for an ACTIVATABLE row: GTK never
            // emits it for a pointer release on a row built `activatable(false)`.
            // Emitting it anyway handed the list's dispatch a click no user can
            // make, and where that dispatch has no arm for the row (the Account
            // page's served-account row) the click ran nothing while the reply
            // said `ok`. A row a real click would
            // neither select nor activate is refused loudly instead (convention
            // 11), at the 409 that says "found it, it cannot be actuated".
            if row.is_activatable() {
                list.emit_by_name::<()>("row-activated", &[&row]);
            } else if !selects {
                return json!({
                    "error": format!(
                        "row is not activatable and its list does not select ({})",
                        w.type_().name()
                    ),
                    "status": 409,
                });
            }
        } else {
            w.activate();
        }
    } else if w.downcast_ref::<gtk::Entry>().is_some() {
        // Enter-commit emulation: in the automation vocabulary a "click" on an
        // entry means "commit its text" (the `folder-member-cap-input` idiom —
        // its handler rides `Entry::activate`). Emit the action signal
        // directly: `gtk_widget_activate` routes through the class's registered
        // activate signal, which is GtkText's, not the wrapping GtkEntry's, so
        // a plain `activate()` never reaches an `Entry::connect_activate`
        // handler here.
        w.emit_by_name::<()>("activate", &[]);
    } else if !w.activate() && !press_gesture(w, 1) {
        // `gtk_widget_activate` is a no-op on widgets with no activate signal —
        // a plain `gtk::Box` or `gtk::Label` that carries its behaviour on a
        // `GestureClick` (the month grid's "+N more" overflow label), so try the
        // gesture next. When *neither* lands, nothing ran: replying `ok` would
        // be a silent drop, and `testing.md` point 11 forbids exactly that — the
        // driver would see a delivered click and the test would fail on some
        // later read, indistinguishable from a real product bug.
        //
        // The GType is in the message so the failure diagnoses itself
        // (convention 6): `GtkLabel` here means an inert marker widget is
        // wearing an id that ui.yaml declares as a real control.
        return json!({
            "error": format!(
                "element is not activatable and has no click gesture ({})",
                w.type_().name()
            )
        });
    }
    json!({ "ok": true })
}

/// Actuate a "double click": the widget's normal activation (the first press's
/// effect), then the `n_press = 2` arm of its own `GestureClick` — the same two
/// effects, in the same order, that a real pointer double-press delivers to a
/// GTK widget. Faithfulness matters here: the month day cell drills into Day
/// view on the first press *before* the second opens the compose, so a
/// double-click that skipped the drill would let a broken drill test green.
fn actuate_double_click(w: &gtk::Widget) -> Value {
    actuate_click(w);
    if press_gesture(w, 2) {
        json!({ "ok": true })
    } else {
        json!({ "error": "element has no click gesture to double-press" })
    }
}

/// Emit `pressed`/`released` with `n_press` on the widget's own `GestureClick`,
/// as a real pointer press would. Returns false when the widget carries none.
///
/// Emitting the signals directly (rather than synthesising `GdkEvent`s) is what
/// keeps this headless: the handlers under test are the very ones a real press
/// runs, and no display-server input injection is involved.
fn press_gesture(w: &gtk::Widget, n_press: i32) -> bool {
    let controllers = w.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(gesture) = controllers.item(i).and_downcast::<gtk::GestureClick>() else {
            continue;
        };
        gesture.emit_by_name::<()>("pressed", &[&n_press, &0.0f64, &0.0f64]);
        gesture.emit_by_name::<()>("released", &[&n_press, &0.0f64, &0.0f64]);
        return true;
    }
    false
}

fn editable_of(w: &gtk::Widget) -> Option<gtk::Editable> {
    if let Some(e) = w.downcast_ref::<gtk::Entry>() {
        return Some(e.clone().upcast());
    }
    if let Some(e) = w.downcast_ref::<gtk::Text>() {
        return Some(e.clone().upcast());
    }
    if let Some(e) = w.downcast_ref::<gtk::PasswordEntry>() {
        return Some(e.clone().upcast());
    }
    if let Some(e) = w.downcast_ref::<gtk::SearchEntry>() {
        return Some(e.clone().upcast());
    }
    // adw::EntryRow / PasswordEntryRow implement GtkEditable by delegating to their
    // inner GtkText. The test id is set on the *row* (the widget the agent resolves),
    // so resolve the row itself to its `Editable` — otherwise typing an EntryRow input
    // (e.g. `folder-location-path-input`, the handle field) fails "not editable". This is
    // the headless peer of the AT-SPI bridge reaching the row's inner EditableText.
    if let Some(e) = w.downcast_ref::<adw::EntryRow>() {
        return Some(e.clone().upcast());
    }
    if let Some(e) = w.downcast_ref::<adw::PasswordEntryRow>() {
        return Some(e.clone().upcast());
    }
    None
}

fn type_text(w: &gtk::Widget, text: &str) -> Value {
    if let Some(e) = editable_of(w) {
        let mut pos = e.text().chars().count() as i32;
        e.insert_text(text, &mut pos);
        return json!({ "ok": true });
    }
    // gtk::TextView is multi-line and backed by a TextBuffer, not gtk::Editable
    // (the compose-text-field). The AT-SPI bridge typed via its EditableText
    // interface; here we append at the buffer end, mirroring the Editable arm's
    // insert-at-end semantics so clear_and_type then type the target text.
    if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
        let buffer = tv.buffer();
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, text);
        return json!({ "ok": true });
    }
    // gtk::Scale and other ranges aren't editable — the e2e layer "types" a
    // number to set their value (the AT-SPI bridge does this via the Value
    // interface; here we set it directly).
    if let Some(r) = w.downcast_ref::<gtk::Range>()
        && let Ok(v) = text.trim().parse::<f64>()
    {
        r.set_value(v);
        return json!({ "ok": true });
    }
    // gtk::SpinButton (e.g. the folder wizard's retention controls). Its inner
    // Editable doesn't downcast as a bare Entry, and inserting text wouldn't
    // commit the value until focus-out; `set_value` commits + fires
    // `value-changed` immediately, mirroring the Range arm.
    if let Some(sb) = w.downcast_ref::<gtk::SpinButton>()
        && let Ok(v) = text.trim().parse::<f64>()
    {
        sb.set_value(v);
        return json!({ "ok": true });
    }
    // A button that owns a `GtkEmojiChooser` — the fuller reaction picker
    // (`dm-reaction-more-button`), the one sanctioned per-app widget of the
    // actions menu. Its cells are GTK internals with no test id, so typing an
    // emoji at the button picks it through the chooser's own `emoji-picked`,
    // the signal a human pick raises, and closes the chooser as a pick does.
    // The chooser must already be open: the click that opens it stays part of
    // the journey, exactly as for a human.
    if let Some(chooser) = emoji_chooser_of(w) {
        if !chooser.is_visible() {
            return json!({ "error": "the emoji chooser is not open — click its button first" });
        }
        chooser.emit_by_name::<()>("emoji-picked", &[&text]);
        chooser.popdown();
        return json!({ "ok": true });
    }
    json!({ "error": "not editable" })
}

/// The `GtkEmojiChooser` a button pops up, parented on the button itself
/// (`views/conversations/message_bubble.rs`, the "more reactions" button).
fn emoji_chooser_of(w: &gtk::Widget) -> Option<gtk::EmojiChooser> {
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Ok(chooser) = c.clone().downcast::<gtk::EmojiChooser>() {
            return Some(chooser);
        }
        child = c.next_sibling();
    }
    None
}

fn clear(w: &gtk::Widget) -> Value {
    if let Some(e) = editable_of(w) {
        e.set_text("");
        return json!({ "ok": true });
    }
    // gtk::TextView (compose-text-field): empty its TextBuffer.
    if let Some(tv) = w.downcast_ref::<gtk::TextView>() {
        tv.buffer().set_text("");
        return json!({ "ok": true });
    }
    // Clearing a range resets it to its lower bound (clear_and_type then sets
    // the target value).
    if let Some(r) = w.downcast_ref::<gtk::Range>() {
        r.set_value(r.adjustment().lower());
        return json!({ "ok": true });
    }
    // gtk::SpinButton: reset to its lower bound (clear_and_type then sets the
    // target value via the SpinButton arm of `type_text`).
    if let Some(sb) = w.downcast_ref::<gtk::SpinButton>() {
        sb.set_value(sb.adjustment().lower());
        return json!({ "ok": true });
    }
    json!({ "error": "not editable" })
}

/// Set a `DropDown`/`ComboRow` to the option whose visible string equals
/// `value` — the headless-safe replacement for the AT-SPI `select()`, which is
/// a no-op under headless, coordinate-degraded automation.
fn select(w: &gtk::Widget, value: &str) -> Value {
    if let Some(d) = w.downcast_ref::<gtk::DropDown>() {
        return match string_model_index(d.model().as_ref(), value) {
            Some(i) => {
                d.set_selected(i);
                json!({ "ok": true })
            }
            None => not_offered(d.model().as_ref(), value),
        };
    }
    if let Some(c) = w.downcast_ref::<adw::ComboRow>() {
        return match string_model_index(c.model().as_ref(), value) {
            Some(i) => {
                c.set_selected(i);
                json!({ "ok": true })
            }
            None => not_offered(c.model().as_ref(), value),
        };
    }
    json!({ "error": "not a selector" })
}

/// The refusal for a value this render never offered — convention 11's twin
/// rule (`e2e-conventions.md`: a picker refuses a value the frame did not
/// offer). Two things the bare `{"error": "value not found"}` this replaced got
/// wrong, both caught by `test_select_refuses_unoffered_option.py`:
///
/// * **It named nothing.** "value not found" cannot be told apart from a
///   transport fault, let alone tell the reader what the widget *did* offer
///   (convention 6 — failures diagnose themselves).
/// * **It rode the default 404,** so the driver's `_post_with_scroll` scrolled
///   three times hunting a widget already on screen and then raised
///   `LookupError` — i.e. reported "not rendered yet" for an element it had
///   found. `409` says "found it; you asked for something impossible", which is
///   what `drivers/http_bridge.py::SelectOptionNotOffered` keys on.
fn not_offered(model: Option<&gio::ListModel>, value: &str) -> Value {
    json!({
        "error": format!(
            "select target {:?} is not offered — this render painted [{}]",
            value,
            model_strings(model)
                .into_iter()
                .map(|(_, s)| s)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        "status": 409,
    })
}

/// Model index of the option `value` selects, via the shared
/// [`fauna_e2e_agent::select_match`] — which owns the exact-then-normalized
/// rule (the suite drives stable keys like `"BodyContains"`; a `StringList`
/// DropDown paints `"Body contains"`). The matching lived here until
/// 2026-08-03, which made linux quietly more permissive than every other
/// app; it is shared now so "which values does a picker accept" has one
/// answer everywhere.
pub(crate) fn string_model_index(model: Option<&gio::ListModel>, value: &str) -> Option<u32> {
    let strings = model_strings(model);
    let options: Vec<String> = strings.iter().map(|(_, s)| s.clone()).collect();
    fauna_e2e_agent::select_match(&options, value).map(|i| strings[i].0)
}

/// The model's `StringObject` options as `(index, string)`, in model order.
/// Shared by the lookup and the refusal so a picker can never report options it
/// did not actually search — the list the user is told about is the list that
/// was matched against.
fn model_strings(model: Option<&gio::ListModel>) -> Vec<(u32, String)> {
    let Some(model) = model else {
        return Vec::new();
    };
    (0..model.n_items())
        .filter_map(|i| {
            model
                .item(i)
                .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                .map(|s| (i, s.string().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pump the GTK main loop until `ready()` holds, failing after a
    /// generous ceiling — a layout pass has no fixed duration (convention 14).
    fn pump_until(what: &str, mut ready: impl FnMut() -> bool) {
        const BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
        let ctx = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + BUDGET;
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} within {BUDGET:?}"
            );
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// `in-viewport` answers the scroll position, not presence: a `gtk::Box`
    /// realizes and maps every row, so the last row of a tall list is present
    /// (and reads visible) while scrolled out of view. It must read out of
    /// view until the list is scrolled to it, and the first row the reverse.
    #[test]
    fn in_viewport_follows_the_scroll_position_not_realization() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            window.set_default_size(300, 200);
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let rows: Vec<gtk::Widget> = (0..60)
                .map(|i| {
                    let row = gtk::Label::new(Some(&format!("row {i}")));
                    row.set_size_request(-1, 40);
                    list.append(&row);
                    row.upcast()
                })
                .collect();
            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_child(Some(&list));
            window.set_child(Some(&scrolled));
            window.present();

            let (first, last) = (&rows[0], &rows[59]);
            let adj = scrolled.vadjustment();
            pump_until("the list never laid out taller than its viewport", || {
                last.is_mapped() && adj.page_size() > 0.0 && adj.upper() > adj.page_size()
            });
            assert_eq!(
                in_viewport(first),
                json!("true"),
                "the top row starts in view"
            );
            assert_eq!(
                in_viewport(last),
                json!("false"),
                "the bottom row is mapped but scrolled out of view"
            );

            adj.set_value(adj.upper() - adj.page_size());
            pump_until("the scroll to the end never reached the last row", || {
                in_viewport(last) == json!("true")
            });
            assert_eq!(
                in_viewport(first),
                json!("false"),
                "the top row leaves the view once scrolled to the end"
            );
            window.close();
        });
    }

    /// `/element/text` on a widget that is not on screen must report an error,
    /// not the empty string — otherwise "not found" and "found but empty" are
    /// the same reply and every failure downstream reads as a content bug
    /// (convention 6). Matches the `Click`/`Type`/…
    /// routes' existing not-found shape, and the apple/android/windows
    /// precedent, which already raise on a missing text read.
    #[test]
    fn a_text_read_on_a_not_found_widget_reports_an_error_not_empty_text() {
        crate::testid::run_on_gtk_thread(|| {
            let req = ElementReq {
                kind: ElementKind::Text,
                id: "definitely-nonexistent-test-id-row-40".to_string(),
                arg: String::new(),
                index: 0,
                scope: Vec::new(),
            };
            let reply = perform(&req);
            assert_eq!(
                reply.get("error").and_then(|e| e.as_str()),
                Some("not found"),
                "a missing widget must not reply `{{\"text\": \"\"}}` — that is \
                 indistinguishable from a found-but-empty widget: {reply:?}"
            );
            assert!(
                reply.get("text").is_none(),
                "a not-found reply must not also carry a text field: {reply:?}"
            );
        });
    }

    /// The counterpart: a widget that IS found but genuinely carries no text
    /// (an untouched entry) still replies the plain empty string through the
    /// same `perform` dispatch, not an error — the flip narrows only the
    /// missing-widget case.
    #[test]
    fn a_text_read_on_a_found_but_empty_widget_still_replies_empty_text_via_perform() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let entry = gtk::Entry::new();
            crate::testid::set_test_id(&entry, "row-40-empty-entry");
            window.set_child(Some(&entry));
            window.present();

            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            let ctx = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + MAP_BUDGET;
            while !entry.is_mapped() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the test entry never mapped within {MAP_BUDGET:?}"
                );
                if !ctx.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            let req = ElementReq {
                kind: ElementKind::Text,
                id: "row-40-empty-entry".to_string(),
                arg: String::new(),
                index: 0,
                scope: Vec::new(),
            };
            let reply = perform(&req);
            assert_eq!(
                reply,
                json!({ "text": "" }),
                "a found-but-empty widget must still 200 with an empty string, \
                 not be swept into the not-found error: {reply:?}"
            );

            window.destroy();
        });
    }

    /// `/element/attr` honours `index` — the bug that made an indexed row's
    /// `state` read answer from its FIRST sibling, whatever index was asked for.
    ///
    /// Silent by construction, which is why it lived so long: rows of one id
    /// usually agree, so reading the wrong one usually gives the right answer.
    /// It surfaces only where two differ — `test_mail_import.py` deselects one
    /// source mailbox and reads *that* mailbox back, and got the still-selected
    /// neighbour's `on` instead. Every other indexed route (text/click/select/
    /// scroll) already resolved through `find_indexed_scoped`; only this one
    /// called `find_scoped` and threw the index away.
    #[test]
    fn an_attr_read_honours_the_index_rather_than_answering_from_the_first_match() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            // Two same-id rows that DISAGREE — the only shape that can catch this.
            for (i, state) in ["on", "off"].iter().enumerate() {
                let cb = gtk::CheckButton::with_label(&format!("row-{i}"));
                crate::testid::set_test_id(&cb, "row-470-indexed-state");
                cb.add_css_class(&format!("test-attr-state-{state}"));
                row.append(&cb);
            }
            window.set_child(Some(&row));
            window.present();

            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            let ctx = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + MAP_BUDGET;
            while !row.is_mapped() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the test rows never mapped within {MAP_BUDGET:?}"
                );
                if !ctx.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            let read = |index: usize| {
                perform(&ElementReq {
                    kind: ElementKind::Attr,
                    id: "row-470-indexed-state".to_string(),
                    arg: "state".to_string(),
                    index,
                    scope: Vec::new(),
                })
            };
            assert_eq!(read(0), json!({ "value": "on" }), "index 0 is unchanged");
            assert_eq!(
                read(1),
                json!({ "value": "off" }),
                "index 1 must read the SECOND row; answering `on` here is the \
                 dropped-index bug — every indexed state assertion in the suite \
                 silently reads row 0 again"
            );

            window.destroy();
        });
    }

    /// An image element's `state` is its PAINT, read off the live picture:
    /// `painted` once decoded bytes are its paintable, `placeholder` while it
    /// has none. It is the one headless witness that the fetch → open → decode
    /// chain ended in a picture — a sealed item handed to the decoder unopened
    /// fails `Texture::from_bytes` and leaves the picture empty with no error
    /// anywhere (`ui/media.md` § Encryption at rest → *Rendering a sealed
    /// attachment*). Both shapes an image element takes here: the flat button
    /// `build_post_image` wraps its picture in, and a bare tagged picture.
    #[test]
    fn an_image_elements_state_reads_painted_only_once_its_picture_holds_a_paintable() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let decoded = || {
                let pixel = gtk::glib::Bytes::from_static(&[0xff, 0x00, 0x00, 0xff]);
                gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &pixel, 4)
            };
            for paint in [true, false] {
                let picture = gtk::Picture::new();
                if paint {
                    picture.set_paintable(Some(&decoded()));
                }
                let button = gtk::Button::new();
                crate::testid::set_test_id(&button, "paint-state-wrapped-image");
                button.set_child(Some(&picture));
                column.append(&button);
            }
            let bare = gtk::Picture::new();
            bare.set_paintable(Some(&decoded()));
            crate::testid::set_test_id(&bare, "paint-state-bare-image");
            column.append(&bare);
            window.set_child(Some(&column));
            window.present();

            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            let ctx = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + MAP_BUDGET;
            while !column.is_mapped() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the test images never mapped within {MAP_BUDGET:?}"
                );
                if !ctx.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            let read = |id: &str, index: usize| {
                perform(&ElementReq {
                    kind: ElementKind::Attr,
                    id: id.to_string(),
                    arg: "state".to_string(),
                    index,
                    scope: Vec::new(),
                })
            };
            assert_eq!(
                read("paint-state-wrapped-image", 0),
                json!({ "value": "painted" }),
                "a button whose picture holds decoded bytes has painted"
            );
            assert_eq!(
                read("paint-state-wrapped-image", 1),
                json!({ "value": "placeholder" }),
                "a button whose picture holds nothing is still the placeholder — \
                 answering null here leaves the e2e unable to tell an unopened \
                 sealed photo from a painted one"
            );
            assert_eq!(
                read("paint-state-bare-image", 0),
                json!({ "value": "painted" }),
                "a tagged picture reads its own paintable"
            );

            window.destroy();
        });
    }

    /// Convention 11's twin rule: a picker refuses a value the frame never
    /// offered — and refuses *readably*. Before 2026-08-03 this replied a bare
    /// `{"error": "value not found"}` at the default 404, which the driver
    /// scroll-retried three times and then raised as `LookupError`, i.e.
    /// "element not rendered yet" for a widget it had just found.
    #[test]
    fn a_select_of_an_unoffered_value_refuses_readably_and_changes_nothing() {
        crate::testid::run_on_gtk_thread(|| {
            let model = gtk::StringList::new(&["alpha", "beta"]);
            let drop = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
            drop.set_selected(1);

            let reply = select(drop.upcast_ref(), "never-painted");
            assert_eq!(
                reply.get("status").and_then(|s| s.as_u64()),
                Some(409),
                "the widget WAS found — 404 would send the driver scroll-retrying: {reply:?}"
            );
            let err = reply
                .get("error")
                .and_then(|e| e.as_str())
                .expect("a refusal must name itself");
            assert!(
                err.contains("never-painted"),
                "the refusal must quote the rejected value, or it reads as a \
                 transport fault: {err:?}"
            );
            assert!(
                err.contains("alpha") && err.contains("beta"),
                "and must list what the render DID paint (convention 6): {err:?}"
            );
            assert_eq!(
                drop.selected(),
                1,
                "a refused select must not have moved the widget"
            );

            // The guard is targeted, not a wedge: an offered value still lands.
            let reply = select(drop.upcast_ref(), "alpha");
            assert_eq!(reply, json!({ "ok": true }));
            assert_eq!(drop.selected(), 0);
        });
    }

    /// `options`: a picker answers with EVERY option it paints, JSON-encoded in
    /// model order — the cross-app `driver.option_texts` contract tui and web
    /// already serve. A selected-value read cannot assert an option's ABSENCE,
    /// which is the whole point of the Task-delegation picker's legal set
    /// (`participants.md` § The assignment picker). A non-picker answers
    /// `Null`, never `"[]"`: a picker with zero options is a different fact.
    #[test]
    fn a_pickers_options_attr_lists_every_painted_option_and_a_non_picker_answers_null() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let drop = gtk::DropDown::new(
                Some(gtk::StringList::new(&["Automatic", "This device"])),
                gtk::Expression::NONE,
            );
            drop.set_selected(1);
            crate::testid::set_test_id(&drop, "options-attr-dropdown");
            column.append(&drop);
            let combo = adw::ComboRow::new();
            combo.set_model(Some(&gtk::StringList::new(&["Automatic"])));
            crate::testid::set_test_id(&combo, "options-attr-combo");
            column.append(&combo);
            let label = gtk::Label::new(Some("not a picker"));
            crate::testid::set_test_id(&label, "options-attr-label");
            column.append(&label);
            window.set_child(Some(&column));
            window.present();

            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            let ctx = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + MAP_BUDGET;
            while !column.is_mapped() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the test pickers never mapped within {MAP_BUDGET:?}"
                );
                if !ctx.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            let read = |id: &str| {
                perform(&ElementReq {
                    kind: ElementKind::Attr,
                    id: id.to_string(),
                    arg: "options".to_string(),
                    index: 0,
                    scope: Vec::new(),
                })
            };
            assert_eq!(
                read("options-attr-dropdown"),
                json!({ "value": r#"["Automatic","This device"]"# }),
                "a DropDown lists its whole model, not just the selected option"
            );
            assert_eq!(
                read("options-attr-combo"),
                json!({ "value": r#"["Automatic"]"# }),
                "a ComboRow answers the same way"
            );
            assert_eq!(
                read("options-attr-label"),
                json!({ "value": null }),
                "a non-picker has no option set to report"
            );

            window.destroy();
        });
    }

    /// `testing.md` point 11: a click the agent cannot deliver must fail loudly.
    /// A bare `gtk::Label` has no activate signal and no `GestureClick`, so
    /// nothing runs — the reply must be an `error`, never `{"ok": true}`, and it
    /// must name the GType so the failure diagnoses itself (convention 6).
    #[test]
    fn a_click_on_an_inert_widget_reports_an_error() {
        crate::testid::run_on_gtk_thread(|| {
            let label = gtk::Label::new(Some("marker"));
            let reply = actuate_click(label.upcast_ref());
            let err = reply
                .get("error")
                .and_then(|e| e.as_str())
                .expect("an inert widget must reply with an error, not ok");
            assert!(
                err.contains("GtkLabel"),
                "the error must name the widget type so the failure self-diagnoses, got {err:?}"
            );
            assert!(
                reply.get("ok").is_none(),
                "an undelivered click must not also claim ok: {reply:?}"
            );
        });
    }

    /// The counterpart: a widget that *does* activate still replies `ok`, and
    /// the activation really ran (the flip must not turn working clicks into
    /// errors).
    ///
    /// The observable is a `gtk::CheckButton`'s `active` flip, not a
    /// `gtk::Button`'s `clicked` handler: `gtk_widget_activate` on a plain
    /// button completes through the main loop, which a unit test has none of,
    /// so a `clicked` assertion here would be testing the harness rather than
    /// the agent. A CheckButton's activation flips state synchronously and goes
    /// down the identical `else`-branch this flip guards. The plain-button case
    /// below still pins the half that matters for the flip — that it is not
    /// refused.
    #[test]
    fn a_click_on_an_activatable_widget_runs_it_and_replies_ok() {
        crate::testid::run_on_gtk_thread(|| {
            let check = gtk::CheckButton::with_label("Toggle me");
            assert!(!check.is_active());
            let reply = actuate_click(check.upcast_ref());
            assert_eq!(reply.get("ok").and_then(|v| v.as_bool()), Some(true));
            assert!(
                check.is_active(),
                "activation must really run, not just report ok"
            );

            let button = gtk::Button::with_label("Go");
            let reply = actuate_click(button.upcast_ref());
            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "an activatable button must not be refused by the inert-click flip"
            );
        });
    }

    /// A click on a list row is what a pointer release on it does, and no more:
    /// an activatable row fires `row-activated`; a non-activatable row in a list
    /// that does not select (an `AdwPreferencesGroup`'s, where the Account page's
    /// served-account row lives) runs nothing, so the agent refuses it with a
    /// 409 rather than answering `ok` for a click that ran nothing — the silent
    /// drop behind a switch-back that never switched.
    #[test]
    fn a_click_on_a_row_fires_row_activated_only_when_a_user_click_would() {
        crate::testid::run_on_gtk_thread(|| {
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            let live = gtk::ListBoxRow::new();
            let inert = gtk::ListBoxRow::new();
            inert.set_activatable(false);
            list.append(&live);
            list.append(&inert);
            let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::<i32>::new()));
            {
                let fired = std::rc::Rc::clone(&fired);
                list.connect_row_activated(move |_, r| fired.borrow_mut().push(r.index()));
            }

            assert_eq!(actuate_click(live.upcast_ref()), json!({ "ok": true }));
            let reply = actuate_click(inert.upcast_ref());
            assert_eq!(
                reply.get("status").and_then(|s| s.as_u64()),
                Some(409),
                "a row no click can actuate must be refused, not answered ok: {reply:?}"
            );
            assert_eq!(
                *fired.borrow(),
                vec![0],
                "only the activatable row may reach the list's row-activated dispatch"
            );

            // In a list that selects, the same inert row still takes the click's
            // other effect — selection — and so is delivered, not refused.
            list.set_selection_mode(gtk::SelectionMode::Single);
            assert_eq!(actuate_click(inert.upcast_ref()), json!({ "ok": true }));
            assert!(inert.is_selected(), "the click must have selected the row");
            assert_eq!(*fired.borrow(), vec![0], "and still not activated it");
        });
    }

    /// A gesture-only widget (a plain `gtk::Box` carrying a `GestureClick`, the
    /// month grid's "+N more" shape) is delivered via the gesture and stays
    /// `ok` — the flip narrows only the case where *neither* path exists.
    #[test]
    fn a_click_on_a_gesture_only_widget_stays_ok() {
        crate::testid::run_on_gtk_thread(|| {
            let boxed = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let gesture = gtk::GestureClick::new();
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let fired = std::rc::Rc::clone(&fired);
                gesture.connect_pressed(move |_, _, _, _| fired.set(true));
            }
            boxed.add_controller(gesture);
            let reply = actuate_click(boxed.upcast_ref());
            assert_eq!(reply.get("ok").and_then(|v| v.as_bool()), Some(true));
            assert!(fired.get(), "the gesture's pressed handler must have run");
        });
    }

    // --- the actuation gate (convention 11 one layer down) -----------------
    //
    // The refusal SHAPE (409, the message, permissive-still-drives) is pinned
    // once in `libs/fauna-e2e-agent`. What is linux-specific, and pinned here,
    // is the predicate the gate consults: GTK's `is_sensitive()`, read live.
    // The wiring of the five routes is pinned end-to-end by
    // `tests/e2e-unified/tests/test_linux_disabled_actuation.py`, which drives
    // the real HTTP surface with `FAUNA_E2E_STRICT_ACTUATION=1`.

    fn req_for(id: &str) -> ElementReq {
        ElementReq {
            kind: ElementKind::Click,
            id: id.to_string(),
            arg: String::new(),
            index: 0,
            scope: Vec::new(),
        }
    }

    /// **The bug, measured rather than asserted.** GTK does not stop
    /// `gtk_widget_activate` on an insensitive widget — sensitivity guards
    /// *event delivery*, and the agent synthesises no events. So without the
    /// gate the harness really can flip a control no user could reach, and the
    /// app's own handler really runs.
    ///
    /// This is the premise the whole track rests on, and it is also what makes
    /// permissive mode meaningful: the control is still driven, so a sweep
    /// enumerates offenders without changing any test's outcome. If GTK ever
    /// starts refusing here, this test fails and the staging plan needs
    /// rethinking — the marker would be recording a violation that could not
    /// happen.
    #[test]
    fn without_the_gate_gtk_activates_an_insensitive_control() {
        crate::testid::run_on_gtk_thread(|| {
            let check = gtk::CheckButton::with_label("Impossible");
            check.set_sensitive(false);
            assert!(!check.is_active());

            // Deliberately the raw actuator, bypassing `gate` — this measures
            // GTK, not our guard.
            let reply = actuate_click(check.upcast_ref());

            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "the harness reports success driving a disabled control: {reply:?}"
            );
            assert!(
                check.is_active(),
                "and the activation REALLY ran — this is the harness-only \
                 capability with no user analogue that convention 11 forbids"
            );
        });
    }

    /// An ancestor-disabled control must read DISABLED — the property that lets
    /// linux gate with no registration surface at all.
    ///
    /// apple had to build this by hand (`AutomationRegistry.folding`) after a
    /// control greyed only by an ancestor reported `enabled=true`, and the
    /// driver's answer disagreeing with the real UI is the direction that costs
    /// debugging: the test actuates it, nothing happens, and the failure
    /// presents as a product bug. GTK gives it for free — but "for free" is
    /// exactly the kind of claim that silently stops being true, so it is pinned.
    #[test]
    fn a_control_greyed_only_by_its_ancestor_reads_disabled() {
        crate::testid::run_on_gtk_thread(|| {
            let parent = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let button = gtk::Button::with_label("Restore");
            parent.append(&button);

            assert!(find::is_enabled(button.upcast_ref()));
            parent.set_sensitive(false);
            assert!(
                !find::is_enabled(button.upcast_ref()),
                "the button's OWN sensitivity is still true; only the effective, \
                 ancestor-inclusive read sees the grey-out"
            );
            assert!(
                button.is_sensitive() != button.get_sensitive(),
                "premise: GTK distinguishes the effective read from the own-flag read"
            );
        });
    }

    /// The predicate is read LIVE, per request — never captured. A snapshot
    /// would keep refusing a control that has since enabled, which breaks
    /// working tests rather than fake ones.
    #[test]
    fn the_gate_reads_sensitivity_live_not_once() {
        crate::testid::run_on_gtk_thread(|| {
            let button = gtk::Button::with_label("Confirm");
            let req = req_for("restore-confirm-button");

            button.set_sensitive(false);
            let disabled_verdict = fauna_e2e_agent::actuation_verdict(
                "click",
                &req.id,
                req.index,
                find::is_enabled(button.upcast_ref()),
                true,
            );
            assert!(disabled_verdict.is_some(), "a disabled control is refused");

            // The friction bar arms; the very next request must be honoured.
            button.set_sensitive(true);
            let enabled_verdict = fauna_e2e_agent::actuation_verdict(
                "click",
                &req.id,
                req.index,
                find::is_enabled(button.upcast_ref()),
                true,
            );
            assert_eq!(
                enabled_verdict, None,
                "the same control, now sensitive, must be driven"
            );
        });
    }

    /// An enabled control is untouched by the gate — the regression that would
    /// matter most, since `gate` now sits on the hot path of every click, type,
    /// clear and select the linux harness issues.
    #[test]
    fn the_gate_lets_every_enabled_control_through() {
        crate::testid::run_on_gtk_thread(|| {
            let button = gtk::Button::with_label("Go");
            let entry = gtk::Entry::new();
            for (w, route) in [
                (button.clone().upcast::<gtk::Widget>(), "click"),
                (entry.clone().upcast::<gtk::Widget>(), "type"),
                (entry.clone().upcast::<gtk::Widget>(), "clear"),
            ] {
                assert_eq!(
                    gate(&w, route, &req_for("some-id")),
                    None,
                    "{route} on an enabled widget must not be gated"
                );
            }
        });
    }

    /// **The flipped contract, pinned deliberately (2026-09-10).** Refusal is
    /// linux's default, so a disabled control is REFUSED with a 409 that names
    /// it — and the permissive mode still drives it, because that mode outlives
    /// the flip as the measuring instrument: a strict sweep stops at the first
    /// offender per test and hides the rest.
    #[test]
    // The constant is the stance under test — its assertion's job is to fail
    // loudly if the default is ever quietly reverted, not to hold at compile
    // time.
    #[allow(clippy::assertions_on_constants)]
    fn linux_refuses_a_disabled_control_by_default_and_permissive_mode_still_drives_it() {
        assert!(
            LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT,
            "refusal is linux's default since the 2026-09-10 sweep; reverting it \
             re-opens convention 11's hole"
        );
        crate::testid::run_on_gtk_thread(|| {
            let button = gtk::Button::with_label("Restore");
            button.set_sensitive(false);
            let enabled = find::is_enabled(button.upcast_ref());
            let refusal = fauna_e2e_agent::actuation_verdict(
                "click",
                "restore-confirm-button",
                0,
                enabled,
                LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT,
            )
            .expect("the default must refuse a disabled control");
            assert_eq!(refusal["status"], 409, "{refusal}");
            assert!(
                refusal["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("restore-confirm-button")),
                "the refusal must name the element: {refusal}"
            );
            assert_eq!(
                fauna_e2e_agent::actuation_verdict(
                    "click",
                    "restore-confirm-button",
                    0,
                    enabled,
                    false,
                ),
                None,
                "permissive mode must still drive, or one sweep cannot enumerate"
            );
        });
    }

    /// A click on a `gtk::Button` lands its `clicked` BEFORE the reply, not on
    /// a timer. `gtk_widget_activate` on a button animates a press-then-release
    /// and emits `clicked` only when that animation's timeout fires — and an
    /// unrealize in between (the conversation list rebuilding every row on a
    /// snapshot tick, a re-drain after a relaunch) abandons it without ever
    /// emitting. That is the missed `conversation-item` click: the agent replied `ok`, the card was replaced, and
    /// `select_thread` never ran. Asserted with no main-loop iteration between
    /// the click and the read, so it holds whatever the machine's load.
    #[test]
    fn a_button_click_emits_clicked_synchronously_and_survives_an_immediate_detach() {
        crate::testid::run_on_gtk_thread(|| {
            use std::cell::Cell;
            use std::rc::Rc;
            let window = gtk::Window::new();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let button = gtk::Button::with_label("row 0");
            let toggle = gtk::ToggleButton::with_label("toggle");
            column.append(&button);
            column.append(&toggle);
            window.set_child(Some(&column));
            window.present();
            pump_until("the test buttons never mapped", || {
                button.is_mapped() && toggle.is_mapped()
            });

            let clicks = Rc::new(Cell::new(0u32));
            let seen = clicks.clone();
            button.connect_clicked(move |_| seen.set(seen.get() + 1));

            assert_eq!(actuate_click(button.upcast_ref()), json!({ "ok": true }));
            assert_eq!(
                clicks.get(),
                1,
                "`clicked` must have fired by the time the agent replies ok"
            );
            // The re-render: the row is detached (unrealized) right after the
            // click. The click already landed; nothing may fire it again.
            column.remove(&button);
            while gtk::glib::MainContext::default().iteration(false) {}
            assert_eq!(clicks.get(), 1, "the detach neither cancels nor repeats it");

            // A `ToggleButton` is a `Button` subclass: `clicked` is what flips it.
            assert!(!toggle.is_active());
            assert_eq!(actuate_click(toggle.upcast_ref()), json!({ "ok": true }));
            assert!(toggle.is_active(), "the toggle flips before the reply");
            window.close();
        });
    }

    /// Typing an emoji at a button that owns a `GtkEmojiChooser` (the fuller
    /// reaction picker, `dm-reaction-more-button`) picks it through the
    /// chooser's own `emoji-picked` — the signal a human pick raises — and
    /// closes the chooser as a pick does. A chooser that is not open is
    /// refused: no human picks from a closed picker, so the click that opens
    /// it stays part of the journey.
    #[test]
    fn typing_at_an_emoji_chooser_button_picks_through_the_open_chooser() {
        crate::testid::run_on_gtk_thread(|| {
            use std::cell::RefCell;
            use std::rc::Rc;
            let window = gtk::Window::new();
            let more = gtk::Button::with_label("More reactions");
            let chooser = gtk::EmojiChooser::new();
            chooser.set_parent(&more);
            window.set_child(Some(&more));
            window.present();
            pump_until("the more button never mapped", || more.is_mapped());

            let picked = Rc::new(RefCell::new(Vec::<String>::new()));
            let seen = picked.clone();
            chooser.connect_emoji_picked(move |_, text| seen.borrow_mut().push(text.to_owned()));

            let closed = type_text(more.upcast_ref(), "🦊");
            assert!(
                closed.get("error").is_some(),
                "a closed chooser is refused, not picked from: {closed}"
            );
            assert!(picked.borrow().is_empty(), "nothing picked while closed");

            chooser.popup();
            pump_until("the emoji chooser never opened", || chooser.is_visible());
            assert_eq!(type_text(more.upcast_ref(), "🦊"), json!({ "ok": true }));
            assert_eq!(
                *picked.borrow(),
                vec!["🦊".to_owned()],
                "picked once, verbatim"
            );
            pump_until("the pick never closed the chooser", || {
                !chooser.is_visible()
            });
            chooser.unparent();
            window.close();
        });
    }
}
