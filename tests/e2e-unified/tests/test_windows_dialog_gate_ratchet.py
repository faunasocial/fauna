"""tier_1: every windows ContentDialog is shown through the one shell gate.

Pure text scan of ``apps/fauna-windows`` — no MSBuild, no driver. The gate is
``FaunaApp/FaunaApp/Controls/Dialogs.cs``: WinUI allows ONE ContentDialog per
XamlRoot and throws on a second ``ShowAsync()``, which, from an ``async void``
show handler, kills the process. The gate refuses the second open legibly and
owns the registry the e2e ``reset`` force-closes. Both properties hold only
while the gate is the ONLY path, so this pins it:

* no ``.ShowAsync(`` on a dialog anywhere but ``Dialogs.cs`` — a bypass is the
  crash back, and a dialog the reset cannot see;
* no ``ShownDialog`` — the retired per-page registry the gate absorbed; a page
  re-growing its own registry is the drift this consolidation removed.

The behavioral half is ``test_windows_dialog_gate.py`` (tier_3).
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_WINDOWS = os.path.join(_REPO, "apps", "fauna-windows")
_GATE = os.path.join(_WINDOWS, "FaunaApp", "FaunaApp", "Controls", "Dialogs.cs")

# `ShowAsync(` on a ContentDialog. The one other ShowAsync shape in the app is
# a picker/launcher — none today; if one lands, name it here explicitly rather
# than widening the pattern.
_SHOW = re.compile(r"(?<!\bDialogs)\.ShowAsync\s*\(")
_REGISTRY = re.compile(r"\bShownDialog\b")


def _sources():
    for root, dirs, files in os.walk(_WINDOWS):
        dirs[:] = [d for d in dirs if d not in ("bin", "obj", "Generated")]
        for name in files:
            if name.endswith(".cs"):
                yield os.path.join(root, name)


def _hits(pattern):
    found = []
    for path in _sources():
        if os.path.normcase(path) == os.path.normcase(_GATE):
            continue
        with open(path, encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                code = line.split("//", 1)[0]
                if pattern.search(code):
                    found.append(f"{os.path.relpath(path, _REPO)}:{n}: {line.strip()}")
    return found


def test_the_gate_exists():
    with open(_GATE, encoding="utf-8") as f:
        assert _SHOW.search(f.read()), "Dialogs.cs must hold the app's one ShowAsync"


def test_no_dialog_is_shown_outside_the_gate():
    hits = _hits(_SHOW)
    assert not hits, (
        "route these through FaunaApp.Controls.Dialogs.ShowAsync "
        "(a second open would otherwise crash the app):\n" + "\n".join(hits)
    )


def test_no_page_keeps_its_own_open_dialog_registry():
    hits = _hits(_REGISTRY)
    assert not hits, (
        "App.ShownDialog is retired; the gate's registry is the only one:\n"
        + "\n".join(hits)
    )
