"""macOS Accessibility helper for driving SwiftUI apps in e2e tests.

Uses the AXUIElement API via pyobjc to find and interact with UI elements
by their accessibility identifiers — the macOS equivalent of pywinauto on
Windows or AT-SPI on Linux.

Requires: pip install pyobjc-framework-ApplicationServices
"""

import subprocess
import time

from ApplicationServices import (
    AXIsProcessTrusted,
    AXUIElementCreateApplication,
    AXUIElementCopyAttributeValue,
    AXUIElementCopyAttributeNames,
    AXUIElementSetAttributeValue,
    AXUIElementPerformAction,
    kAXErrorSuccess,
)
from CoreFoundation import CFArrayGetCount, CFArrayGetValueAtIndex


def _ax_attr(element, attr):
    """Get an AX attribute value, returning None on failure."""
    err, value = AXUIElementCopyAttributeValue(element, attr, None)
    if err == kAXErrorSuccess:
        return value
    return None


def _ax_children(element):
    """Get the children of an AX element as a Python list."""
    children = _ax_attr(element, "AXChildren")
    if children is None:
        return []
    return list(children)


def check_accessibility_trusted():
    """Check that this process has Accessibility permissions.
    If not, tests cannot drive the UI."""
    if not AXIsProcessTrusted():
        raise RuntimeError(
            "Accessibility access not granted. Go to System Settings > "
            "Privacy & Security > Accessibility and add Terminal / your IDE."
        )


def app_element(pid: int):
    """Create an AXUIElement for the application with the given PID."""
    return AXUIElementCreateApplication(pid)


def find_by_identifier(root, identifier: str):
    """Recursively find the first element with the given AXIdentifier."""
    ident = _ax_attr(root, "AXIdentifier")
    if ident == identifier:
        return root
    for child in _ax_children(root):
        result = find_by_identifier(child, identifier)
        if result is not None:
            return result
    return None


def find_by_role_and_title(root, role: str, title: str):
    """Recursively find the first element matching role and title."""
    elem_role = _ax_attr(root, "AXRole")
    elem_title = _ax_attr(root, "AXTitle") or _ax_attr(root, "AXValue") or ""
    if elem_role == role and title in str(elem_title):
        return root
    # Also check AXDescription
    desc = _ax_attr(root, "AXDescription") or ""
    if elem_role == role and title in str(desc):
        return root
    for child in _ax_children(root):
        result = find_by_role_and_title(child, role, title)
        if result is not None:
            return result
    return None


def find_by_title(root, title: str):
    """Recursively find the first element whose title/value contains the string."""
    elem_title = _ax_attr(root, "AXTitle") or ""
    elem_value = _ax_attr(root, "AXValue") or ""
    elem_desc = _ax_attr(root, "AXDescription") or ""
    if title in str(elem_title) or title in str(elem_value) or title in str(elem_desc):
        return root
    for child in _ax_children(root):
        result = find_by_title(child, title)
        if result is not None:
            return result
    return None


def wait_for_identifier(root, identifier: str, timeout: float = 15.0):
    """Wait until an element with the given AXIdentifier appears."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        elem = find_by_identifier(root, identifier)
        if elem is not None:
            return elem
        time.sleep(0.3)
    raise TimeoutError(
        f"Element with identifier '{identifier}' not found within {timeout}s"
    )


def wait_for_title(root, title: str, timeout: float = 15.0):
    """Wait until an element whose title contains the string appears."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        elem = find_by_title(root, title)
        if elem is not None:
            return elem
        time.sleep(0.3)
    raise TimeoutError(
        f"Element with title containing '{title}' not found within {timeout}s"
    )


def click(element):
    """Press/click an element via AXPress action."""
    err = AXUIElementPerformAction(element, "AXPress")
    if err != kAXErrorSuccess:
        raise RuntimeError(f"AXPress failed with error {err}")
    time.sleep(0.1)


def set_text(element, text: str):
    """Set the value of a text field."""
    # Focus the element first
    AXUIElementSetAttributeValue(element, "AXFocused", True)
    time.sleep(0.1)
    err = AXUIElementSetAttributeValue(element, "AXValue", text)
    if err != kAXErrorSuccess:
        raise RuntimeError(f"Setting AXValue failed with error {err}")
    time.sleep(0.1)


def get_value(element) -> str:
    """Get the AXValue of an element."""
    return str(_ax_attr(element, "AXValue") or "")


def get_title(element) -> str:
    """Get the AXTitle of an element."""
    return str(_ax_attr(element, "AXTitle") or "")


def get_role(element) -> str:
    """Get the AXRole of an element."""
    return str(_ax_attr(element, "AXRole") or "")


def dump_tree(root, indent: int = 0, max_depth: int = 5):
    """Debug helper: print the accessibility tree."""
    if indent > max_depth:
        return
    role = _ax_attr(root, "AXRole") or "?"
    title = _ax_attr(root, "AXTitle") or ""
    ident = _ax_attr(root, "AXIdentifier") or ""
    value = _ax_attr(root, "AXValue") or ""
    desc = _ax_attr(root, "AXDescription") or ""
    label = title or value or desc
    suffix = f" id='{ident}'" if ident else ""
    print(f"{'  ' * indent}[{role}] '{label}'{suffix}")
    for child in _ax_children(root):
        dump_tree(child, indent + 1, max_depth)
