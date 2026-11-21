//! Read-only text can be selected and copied — the linux leg of
//! `docs/goal/ui/README.md` § Text the person reads can be copied.
//!
//! The rule covers every piece of text the app *shows* rather than *acts on*:
//! messages, errors, status and outcome lines, explanatory copy, values and
//! user content. The app builds its labels at over a thousand sites, so the
//! rule is not a per-site `set_selectable(true)` (which the next new page
//! would forget) but ONE decision made where every label passes: when a
//! `gtk::Label` is mapped, [`install`]'s emission hook asks [`is_read_text`]
//! and, on yes, turns on gtk's own selection (rule 1, the platform's own
//! selection, nothing bespoke).
//!
//! **What is a control, and so stays as it is (rule 3, selecting is not
//! acting).** A label is a control's caption when it — or a widget between it
//! and its window — is something a click acts on: a button of any kind, a
//! switch, a drop-down, an expander's title, a list row whose click selects
//! or activates it, a list-view cell, or any widget the app gave its own
//! click gesture (a clickable chip, a post's author line, an event block). A
//! selectable label there would take the press the control is waiting for,
//! so it is left alone.
//!
//! **Focus (rule 1's corollary).** A selectable gtk label is focusable, and
//! gtk's `gtk-label-select-on-focus` default selects a label's whole text when
//! it takes focus — so a window opening on a label would paint it selected.
//! [`install`] turns that setting off once for the display.
//!
//! **A repaint keeps a selection (rule 2).** gtk's `Label::set_text` with the
//! text the label already holds changes nothing — the selection included —
//! which `a_repaint_with_the_same_text_keeps_the_selection` pins; so a page
//! that repaints its labels on every observer tick keeps a selection being
//! made, and only a changed value resets it.

use gtk::glib;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;

/// True when `widget` carries a click gesture the app gave it.
fn has_click_gesture(widget: &gtk::Widget) -> bool {
    let controllers = widget.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .any(|c| c.is::<gtk::GestureClick>())
}

/// gtk widget types that hold click gestures of their own as part of what
/// they are (a list box's row press, a scrolled window's kinetic drag), so a
/// gesture found on one of them does not make it a control.
pub(crate) fn gesture_is_internal(widget: &gtk::Widget) -> bool {
    widget.is::<gtk::ListBox>()
        || widget.is::<gtk::ScrolledWindow>()
        || widget.is::<gtk::Window>()
        || widget.is::<gtk::Popover>()
        || widget.is::<gtk::Text>()
        || widget.is::<gtk::TextView>()
        || widget.is::<gtk::Paned>()
}

fn row_is_a_control(row: &gtk::ListBoxRow) -> bool {
    let list = row.parent().and_then(|p| p.downcast::<gtk::ListBox>().ok());
    let selects = row.is_selectable()
        && list
            .as_ref()
            .is_some_and(|l| l.selection_mode() != gtk::SelectionMode::None);
    if selects {
        return true;
    }
    if !row.is_activatable() {
        return false;
    }
    // A libadwaita row that is activatable activates through its own class
    // handler (its activatable widget, an expander's toggle) — a control.
    if row.is::<adw::PreferencesRow>() {
        return true;
    }
    // A plain row is a control only when something listens for its click.
    let listened = |obj: &glib::Object, signal: &str| {
        glib::subclass::signal::SignalId::lookup(signal, obj.type_()).is_some_and(|id| {
            // SAFETY: a live object and a signal id looked up on its own type.
            unsafe {
                glib::gobject_ffi::g_signal_has_handler_pending(
                    obj.as_ptr() as *mut _,
                    id.into_glib(),
                    0,
                    glib::ffi::GTRUE,
                ) != glib::ffi::GFALSE
            }
        })
    };
    listened(row.upcast_ref(), "activate")
        || list
            .as_ref()
            .is_some_and(|l| listened(l.upcast_ref(), "row-activated"))
}

/// True when `widget` is something a click acts on.
fn is_control(widget: &gtk::Widget) -> bool {
    if widget.is::<gtk::Button>()
        || widget.is::<gtk::CheckButton>()
        || widget.is::<gtk::MenuButton>()
        || widget.is::<gtk::Switch>()
        || widget.is::<gtk::DropDown>()
        || widget.is::<gtk::Expander>()
        || widget.is::<gtk::StackSwitcher>()
        || widget.is::<gtk::Notebook>()
        || widget.is::<adw::ViewSwitcher>()
    {
        return true;
    }
    // A list-view, grid-view or column-view cell: its click selects it. The
    // cell widgets are gtk-private types, so they are named.
    let type_name = widget.type_().name();
    if matches!(
        type_name,
        "GtkListItemWidget" | "GtkColumnViewRowWidget" | "GtkColumnViewCell" | "GtkModelButton"
    ) {
        return true;
    }
    if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
        && row_is_a_control(row)
    {
        return true;
    }
    !gesture_is_internal(widget) && has_click_gesture(widget)
}

/// The one decision: is this label read-only text the person may select?
pub(crate) fn is_read_text(label: &gtk::Label) -> bool {
    // The label's own click gesture (a post's author line) makes it a control.
    if has_click_gesture(label.upcast_ref()) {
        return false;
    }
    let mut at = label.parent();
    while let Some(widget) = at {
        if widget.is::<gtk::Root>() {
            break;
        }
        if is_control(&widget) {
            return false;
        }
        at = widget.parent();
    }
    true
}

/// Apply the decision to one label: read-only text becomes selectable. A label
/// that is a control's caption is left exactly as it was built.
pub(crate) fn apply(label: &gtk::Label) {
    if !label.is_selectable() && is_read_text(label) {
        label.set_selectable(true);
    }
}

unsafe extern "C" fn on_map(
    _hint: *mut glib::gobject_ffi::GSignalInvocationHint,
    n_params: std::os::raw::c_uint,
    params: *const glib::gobject_ffi::GValue,
    _data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if n_params >= 1 && !params.is_null() {
        // SAFETY: "map"'s first param value is its instance, a GObject.
        let instance = unsafe { glib::gobject_ffi::g_value_get_object(params) };
        if !instance.is_null() {
            // SAFETY: a live GObject the emission holds for the hook's span.
            let object: glib::Object = unsafe { glib::translate::from_glib_none(instance) };
            if let Some(label) = object.downcast_ref::<gtk::Label>() {
                apply(label);
            }
        }
    }
    // Stay installed.
    glib::ffi::GTRUE
}

thread_local! {
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install the rule for the process: every label mapped from now on is asked
/// [`is_read_text`], and gtk no longer selects a label's whole text when it
/// takes focus. Idempotent; call on the GTK thread after `gtk::init`.
pub(crate) fn install() {
    if INSTALLED.with(|i| i.replace(true)) {
        return;
    }
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_label_select_on_focus(false);
    }
    // A signal exists only once its class is initialised, and at startup no
    // widget has been built yet — so take a (never released) reference on
    // the widget class first, or the lookup below finds nothing.
    // SAFETY: a registered type; the class reference is held for the process.
    unsafe { glib::gobject_ffi::g_type_class_ref(gtk::Widget::static_type().into_glib()) };
    let map = glib::subclass::signal::SignalId::lookup("map", gtk::Widget::static_type())
        .expect("GtkWidget has a map signal once its class is initialised");
    // SAFETY: `on_map` matches GSignalEmissionHook, takes no data and never
    // unhooks; "map" carries its instance as the first param value.
    unsafe {
        glib::gobject_ffi::g_signal_add_emission_hook(
            map.into_glib(),
            0,
            Some(on_map),
            std::ptr::null_mut(),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    /// Present `child` in a window so its labels map, then return the window.
    fn present(child: &impl IsA<gtk::Widget>) -> gtk::Window {
        install();
        let window = gtk::Window::new();
        window.set_child(Some(child));
        window.present();
        let ctx = glib::MainContext::default();
        while ctx.iteration(false) {}
        window
    }

    #[test]
    fn a_shown_message_is_selectable_and_a_button_caption_is_not() {
        run_on_gtk_thread(|| {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let message = gtk::Label::new(Some("That handle is taken."));
            let button = gtk::Button::with_label("Continue");
            page.append(&message);
            page.append(&button);
            let window = present(&page);
            assert!(message.is_selectable(), "a status line is read-only text");
            let caption = button.child().and_downcast::<gtk::Label>().unwrap();
            assert!(!caption.is_selectable(), "a button's caption is a control");
            window.destroy();
        });
    }

    #[test]
    fn a_label_under_an_app_click_gesture_or_a_selecting_row_is_left_alone() {
        run_on_gtk_thread(|| {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            // A chip the app made clickable.
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let chip_label = gtk::Label::new(Some("alice"));
            chip.append(&chip_label);
            chip.add_controller(gtk::GestureClick::new());
            page.append(&chip);
            // A row whose click selects it.
            let selecting = gtk::ListBox::new();
            let picked = gtk::Label::new(Some("a post"));
            selecting.append(&picked);
            page.append(&selecting);
            // A list of read-only rows: no selection, nobody listens.
            let info = gtk::ListBox::new();
            info.set_selection_mode(gtk::SelectionMode::None);
            let value = gtk::Label::new(Some("fingerprint 12:34"));
            info.append(&value);
            page.append(&info);
            // A row in a no-selection list that something activates.
            let acting = gtk::ListBox::new();
            acting.set_selection_mode(gtk::SelectionMode::None);
            acting.connect_row_activated(|_, _| {});
            let nav = gtk::Label::new(Some("Mail"));
            acting.append(&nav);
            page.append(&acting);

            let window = present(&page);
            assert!(!chip_label.is_selectable(), "a clickable chip is a control");
            assert!(
                !picked.is_selectable(),
                "a row whose click selects it is a control"
            );
            assert!(value.is_selectable(), "a value in a read-only list is text");
            assert!(!nav.is_selectable(), "an activated row is a control");
            window.destroy();
        });
    }

    #[test]
    fn a_label_added_after_the_window_opened_is_selectable_too() {
        run_on_gtk_thread(|| {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let window = present(&page);
            let late = gtk::Label::new(Some("Saved."));
            page.append(&late);
            let ctx = glib::MainContext::default();
            while ctx.iteration(false) {}
            assert!(
                late.is_selectable(),
                "a status line painted later is text too"
            );
            window.destroy();
        });
    }

    #[test]
    fn a_repaint_with_the_same_text_keeps_the_selection() {
        run_on_gtk_thread(|| {
            let label = gtk::Label::new(Some("error: no route to nest"));
            let window = present(&label);
            assert!(label.is_selectable());
            label.select_region(0, 5);
            assert_eq!(label.selection_bounds(), Some((0, 5)));
            label.set_text("error: no route to nest");
            assert_eq!(
                label.selection_bounds(),
                Some((0, 5)),
                "a tick that repaints the same text must keep the selection"
            );
            window.destroy();
        });
    }

    #[test]
    fn opening_a_window_does_not_select_a_labels_text() {
        run_on_gtk_thread(|| {
            let label = gtk::Label::new(Some("Your recovery words"));
            let window = present(&label);
            label.grab_focus();
            let ctx = glib::MainContext::default();
            while ctx.iteration(false) {}
            assert_eq!(
                label.selection_bounds(),
                None,
                "focus must not select a label's whole text"
            );
            window.destroy();
        });
    }
}
