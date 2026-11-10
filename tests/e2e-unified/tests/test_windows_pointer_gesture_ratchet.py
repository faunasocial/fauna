"""tier_1: every pointer-gesture handler FaunaApp attaches is one the bridge knows.

Pure text scan of ``apps/fauna-windows`` — no MSBuild, no driver.

**Why the inventory is load-bearing.** The FlaUI bridge refuses a click on a
text label "not actuable" when the label takes no focus, holds no children and
has no UIA activation or editable value on itself or any ancestor
(``flaui-bridge/InertGesture.cs``; ``e2e-conventions.md`` § convention 11, the
structural refusal). UIA cannot see a ``Tapped`` handler, so that verdict is
honest only while no pointer handler sits on a text element, or on a panel
around one, where UIA would not show a pattern. This file holds the app to
that: it lists every ``Tapped`` / ``DoubleTapped`` / ``RightTapped`` /
``Holding`` / ``PointerPressed`` / ``PointerReleased`` handler, by file and
event, and fails on any it does not know, or on a listed one that is gone.

**A new handler.** Prefer a UIA pattern: a clickable surface on windows gets a
``Button`` root (Invoke), which a keyboard or Narrator user can also reach. If
it must stay a pointer handler, list it below naming the element it rides; one
on a text element, or on a panel that contains text with no actuable control
between, would make the bridge refuse a click the app handles, so it needs the
bridge taught first.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_WINDOWS = os.path.join(_REPO, "apps", "fauna-windows")

_EVENTS = "Tapped|DoubleTapped|RightTapped|Holding|PointerPressed|PointerReleased"
# `x.Tapped += …`
_SUBSCRIBE = re.compile(rf"\.({_EVENTS})\s*\+=")
# `x.AddHandler(UIElement.TappedEvent, …)`, possibly across lines.
_ADD_HANDLER = re.compile(rf"AddHandler\(\s*(?:UIElement\.)?({_EVENTS})Event\b")
# `<Image Tapped="OnTapped" …>`
_XAML_ATTR = re.compile(rf"\s({_EVENTS})\s*=\s*\"")

# (repo-relative path, event) → the element it rides. Each rides a non-text leaf
# or an actuable/editable control, so the bridge's text-label refusal never
# meets it.
_KNOWN = {
    ("apps/fauna-windows/FaunaApp/FaunaApp/Controls/ImageGrid.xaml.cs", "Tapped"):
        "a feed Image (a leaf, no text) — opens the image viewer",
    ("apps/fauna-windows/FaunaApp/FaunaApp/Controls/DmComposeBar.Decoration.cs", "PointerReleased"):
        "the compose TextBox (editable) — reschedules the styling pass",
    ("apps/fauna-windows/FaunaApp/FaunaApp/Views/EventsPage.xaml.cs", "DoubleTapped"):
        "the month day-cell Button (Invoke) — opens the new-event form",
}


def _sources():
    for root, dirs, files in os.walk(_WINDOWS):
        dirs[:] = [d for d in dirs if d not in ("bin", "obj", "Generated")]
        for name in files:
            if name.endswith((".cs", ".xaml")):
                yield os.path.join(root, name)


def _strip_comments(text: str, xaml: bool) -> str:
    if xaml:
        return re.sub(r"<!--.*?-->", "", text, flags=re.S)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())


def _handler_sites() -> set[tuple[str, str]]:
    found = set()
    for path in _sources():
        xaml = path.endswith(".xaml")
        with open(path, encoding="utf-8") as f:
            code = _strip_comments(f.read(), xaml)
        rel = os.path.relpath(path, _REPO).replace(os.sep, "/")
        patterns = (_XAML_ATTR,) if xaml else (_SUBSCRIBE, _ADD_HANDLER)
        for pattern in patterns:
            for m in pattern.finditer(code):
                found.add((rel, m.group(1)))
    return found


def test_the_scan_sees_the_known_handlers():
    """A scan that matched nothing would pass the ratchet vacuously."""
    assert _handler_sites() & set(_KNOWN), (
        f"the scan found none of the known pointer handlers under {_WINDOWS} — "
        "the patterns or the path have drifted"
    )


def test_every_pointer_handler_is_one_the_bridge_knows():
    sites = _handler_sites()
    new = sorted(sites - set(_KNOWN))
    gone = sorted(set(_KNOWN) - sites)
    assert not new, (
        "new pointer-gesture handler(s) in FaunaApp — give the surface a UIA "
        "pattern (a Button root), or list it in _KNOWN naming the element it "
        "rides (see this file's docstring: one on text would make the bridge "
        "refuse a click the app handles):\n"
        + "\n".join(f"  {path}: {event}" for path, event in new)
    )
    assert not gone, (
        "listed pointer handler(s) no longer in the app — drop them from _KNOWN "
        "so the inventory stays the truth:\n"
        + "\n".join(f"  {path}: {event}" for path, event in gone)
    )
