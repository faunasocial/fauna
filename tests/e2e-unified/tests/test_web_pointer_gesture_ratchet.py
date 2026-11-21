"""tier_1: every pointer-gesture handler the web SPA attaches outside Svelte's
delegation is one the bridge knows.

Pure text scan of ``apps/fauna-web/src`` — no build, no browser.

**Why the inventory is load-bearing.** The web bridge refuses a click or
double-click "not actuable" when the pointer would land on nothing that takes a
gesture (``web-bridge/server.py::_INERT_GESTURE_JS``; ``e2e-conventions.md`` §
convention 11, the structural refusal). It sees a native control, an editable
region, an interactive role, a tab stop, Svelte 5's delegated ``__<event>``
properties (every ``onclick=`` in a component) and DOM0 ``on<event>``
properties — but NOT a listener attached with ``addEventListener``, which the
DOM does not expose. That verdict is honest only while every such listener
rides an element the predicate already calls live. This file holds the app to
that: it lists every ``addEventListener`` of a pointer event in the SPA's
source, by file and event, and fails on any it does not know, or on a listed
one that is gone.

**A new handler.** Prefer a component ``onclick=`` on a button (Svelte
delegates it, and a keyboard user reaches it too). If it must be an
``addEventListener``, list it below naming the element it rides; one on a bare
element with no role or tab stop would make the bridge refuse a click the app
handles, so give that element its role first.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_WEB = os.path.join(_REPO, "apps", "fauna-web", "src")

_EVENTS = (
    "click|dblclick|mousedown|mouseup|pointerdown|pointerup|touchstart|touchend|"
    "contextmenu"
)
_LISTEN = re.compile(rf"addEventListener\(\s*['\"`]({_EVENTS})['\"`]")
# Svelte 4's `on:click` directive attaches with addEventListener too.
_DIRECTIVE = re.compile(rf"\son:({_EVENTS})\b")

# (repo-relative path, event) → the element it rides. Each rides an element
# `_INERT_GESTURE_JS` already calls live, so the refusal never meets it.
_KNOWN = {
    ("apps/fauna-web/src/lib/notes-editor-cm.ts", "mousedown"):
        "the note checkbox widget's <button role=checkbox> — toggles the item",
}


def _sources():
    for root, dirs, files in os.walk(_WEB):
        dirs[:] = [d for d in dirs if d not in ("node_modules", "generated")]
        for name in files:
            if name.endswith((".ts", ".js", ".svelte")) and not name.endswith(".test.ts"):
                yield os.path.join(root, name)


def _handler_sites() -> set[tuple[str, str]]:
    found = set()
    for path in _sources():
        with open(path, encoding="utf-8") as f:
            code = f.read()
        rel = os.path.relpath(path, _REPO).replace(os.sep, "/")
        patterns = (_LISTEN, _DIRECTIVE) if path.endswith(".svelte") else (_LISTEN,)
        for pattern in patterns:
            for m in pattern.finditer(code):
                found.add((rel, m.group(1)))
    return found


def test_the_scan_sees_the_known_handlers():
    """A scan that matched nothing would pass the ratchet vacuously."""
    assert _handler_sites() & set(_KNOWN), (
        f"the scan found none of the known pointer handlers under {_WEB} — "
        "the patterns or the path have drifted"
    )


def test_every_pointer_listener_is_one_the_bridge_knows():
    sites = _handler_sites()
    new = sorted(sites - set(_KNOWN))
    gone = sorted(set(_KNOWN) - sites)
    assert not new, (
        "new addEventListener pointer handler(s) in the web SPA — use a "
        "component onclick= (Svelte delegates it), or list it in _KNOWN naming "
        "the element it rides (see this file's docstring: one on a bare element "
        "would make the bridge refuse a click the app handles):\n"
        + "\n".join(f"  {path}: {event}" for path, event in new)
    )
    assert not gone, (
        "listed pointer handler(s) no longer in the app — drop them from _KNOWN "
        "so the inventory stays the truth:\n"
        + "\n".join(f"  {path}: {event}" for path, event in gone)
    )
