"""AT-SPI helper for driving GTK4 desktop app UIs in e2e tests."""

import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi


def wait_for_app(app_name: str, timeout: float = 15.0) -> Atspi.Accessible:
    """Wait for an application to appear in the AT-SPI tree.
    Dumps available apps on timeout for debugging."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        desktop = Atspi.get_desktop(0)
        for i in range(desktop.get_child_count()):
            app = desktop.get_child_at_index(i)
            if app and app.get_name() == app_name:
                return app
        time.sleep(0.3)

    # Dump available apps for debugging.
    desktop = Atspi.get_desktop(0)
    apps = []
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app:
            apps.append(app.get_name())
    raise TimeoutError(
        f"App '{app_name}' not found in AT-SPI tree within {timeout}s. "
        f"Available apps: {apps}"
    )


def find_by_role_and_name(root, role, name):
    """Recursively find a widget matching the given role and name."""
    if root.get_role() == role and root.get_name() == name:
        return root
    for i in range(root.get_child_count()):
        child = root.get_child_at_index(i)
        if child is None:
            continue
        result = find_by_role_and_name(child, role, name)
        if result is not None:
            return result
    return None


def find_button(root, name):
    """Find a push button by its label text."""
    return find_by_role_and_name(root, Atspi.Role.PUSH_BUTTON, name)


def find_entry_by_label(root, label):
    """Find a text entry by its accessible label/name."""
    return find_by_role_and_name(root, Atspi.Role.TEXT, label)


def find_all_by_role(root, role):
    """Find all widgets with the given role."""
    results = []
    if root.get_role() == role:
        results.append(root)
    for i in range(root.get_child_count()):
        child = root.get_child_at_index(i)
        if child is None:
            continue
        results.extend(find_all_by_role(child, role))
    return results


def click(widget):
    """Perform the default action (click) on a widget."""
    action = widget.get_action_iface()
    if action is None:
        raise RuntimeError(f"Widget '{widget.get_name()}' has no action interface")
    action.do_action(0)


def type_text(widget, text):
    """Type text into a widget.

    Tries two strategies:
    1. AT-SPI EditableText.insert_text (fast, but may not update GTK widget)
    2. Keyboard event simulation via Atspi.generate_keyboard_event (slower, reliable)

    Uses strategy 1, then verifies the text was set. If not, falls back to strategy 2.
    """
    # Strategy 1: EditableText interface.
    try:
        count = Atspi.Text.get_character_count(widget)
        if count > 0:
            Atspi.EditableText.delete_text(widget, 0, count)
        Atspi.EditableText.insert_text(widget, 0, text, len(text))
        time.sleep(0.1)
        # Verify it took.
        actual = Atspi.Text.get_text(widget, 0, Atspi.Text.get_character_count(widget))
        if actual == text:
            return
    except Exception:
        pass

    # Strategy 2: focus + keyboard events.
    try:
        widget.grab_focus()
    except Exception:
        action = widget.get_action_iface()
        if action:
            action.do_action(0)
    time.sleep(0.1)

    for char in text:
        Atspi.generate_keyboard_event(0, char, Atspi.KeySynthType.STRING)
        time.sleep(0.02)
    time.sleep(0.1)


def get_text(widget):
    """Get the text content of a widget via the AT-SPI Text interface."""
    count = Atspi.Text.get_character_count(widget)
    return Atspi.Text.get_text(widget, 0, count)


def wait_for_widget(root, role, name, timeout=10.0):
    """Wait for a widget to appear in the tree."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        widget = find_by_role_and_name(root, role, name)
        if widget is not None:
            return widget
        time.sleep(0.3)
    raise TimeoutError(
        f"Widget role={role} name='{name}' not found within {timeout}s"
    )


def dump_tree(root, indent=0, max_depth=5):
    """Debug helper: print the AT-SPI tree."""
    if indent > max_depth:
        return
    role_name = root.get_role_name()
    name = root.get_name()
    print(f"{'  ' * indent}[{role_name}] '{name}'")
    for i in range(root.get_child_count()):
        child = root.get_child_at_index(i)
        if child:
            dump_tree(child, indent + 1, max_depth)
