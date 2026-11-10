"""tier_1: a windows picker item is named by the label it paints, never by its key.

Pure text scan of ``apps/fauna-windows`` — no MSBuild, no driver. The shape is
convention 11's (``docs/goal/architecture/e2e-conventions.md``, *What counts as
"offered"*): the suite drives keys, the apps paint labels, and on windows the two
ride on separate UIA properties of each ``ComboBoxItem`` — the UIA Name is the
painted label (what a screen reader announces, and what the FlaUI bridge's
``options`` attribute lists, so ``option_texts`` answers painted texts as on every
other app) and the stable key is the item's AutomationId and Tag. The one place
that builds a keyed item is ``FaunaApp/FaunaApp/Helpers/PickerOption.cs``; the
bridge's ``select`` matches an AutomationId before a Name and its ``get_text`` on a
ComboBox answers the selected item's AutomationId when it carries one, so
``driver.select(id, key)`` and the key read-back keep working.

The drift this pins is the retired convention of putting the key in the Name
(``AutomationProperties.SetName(item, opt.value)``, ``AutomationProperties.Name=
"SenderIs"``), which made ``option_texts`` answer keys on windows alone:

* a C# ``AutomationProperties.SetName(x, …)`` on a variable built by
  ``new ComboBoxItem`` must name it by the very expression its ``Content`` paints
  (a label-only picker's explicit Name), or it is a key — use ``PickerOption``;
* a XAML ``<ComboBoxItem>`` never sets ``AutomationProperties.Name``: its Name is
  its painted Content, and a key goes on ``AutomationProperties.AutomationId``.

It also pins the *Pickers* rule of ``docs/goal/architecture/apps/windows.md``
§ Views: a code-built picker is repainted only through ``PickerOption.Reconcile``,
which tears the items down only when the option set changed, so the one
``Items.Clear()`` in the app is the helper's own. An unconditional clear-and-refill
on a path that runs after the picker's own gesture or on a view-model tick destroys
the items while an automation client still holds providers on them, and the
process dies ``0xC000027B`` in native code; it is a race, so no e2e run can be
trusted to catch a new one, and this scan is what does.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_WINDOWS = os.path.join(_REPO, "apps", "fauna-windows")
_HELPER = os.path.join(
    _WINDOWS, "FaunaApp", "FaunaApp", "Helpers", "PickerOption.cs"
)

# `var item = new ComboBoxItem { Content = <expr>, … }` / `ComboBoxItem item = new …`.
# The Content capture stops at the initializer's first top-level comma or brace —
# good enough for the single-line initializers a label-only site writes; a
# multi-line or Content-less initializer captures nothing, so any Name set on it
# reads as a key.
_DECL = re.compile(r"\b(?:var|ComboBoxItem)\s+(\w+)\s*=\s*new\s+(\w+)\b")
_CONTENT = re.compile(r"\bContent\s*=\s*([^,{}]+?)\s*[,}]")
_SET_NAME = re.compile(r"AutomationProperties\.SetName\(\s*(\w+)\s*,\s*(.+?)\);")
_XAML_ITEM = re.compile(r"<ComboBoxItem\b[^>]*>", re.DOTALL)
_ITEMS_CLEAR = re.compile(r"\.Items\s*\.\s*Clear\s*\(\s*\)")

# A file allowed an ``Items.Clear()`` besides the helper, with the reason it cannot
# race a held provider (the control is created on the same call and never shown
# before it is filled, say). Repo-relative, forward slashes. Empty today: every
# picker that used to clear goes through ``PickerOption.Reconcile``, which costs
# nothing on a control filled once.
_CLEAR_ALLOWED: dict[str, str] = {}


def _walk(suffix):
    for root, dirs, files in os.walk(_WINDOWS):
        dirs[:] = [d for d in dirs if d not in ("bin", "obj", "Generated")]
        for name in files:
            if name.endswith(suffix):
                yield os.path.join(root, name)


def _norm(expr: str) -> str:
    return re.sub(r"\s+", "", expr)


def _keyed_lines(lines) -> list[tuple[int, str]]:
    """``(line number, line)`` of every hand-set Name on a ComboBoxItem that is
    not its painted Content, in one file's lines."""
    found = []
    # The most recent declaration of each variable name, in file order:
    # (type, Content expression or None).
    last: dict[str, tuple[str, str | None]] = {}
    for n, line in enumerate(lines, 1):
        code = line.split("//", 1)[0]
        for decl in _DECL.finditer(code):
            content = _CONTENT.search(code, decl.end())
            last[decl.group(1)] = (decl.group(2), content.group(1) if content else None)
        for call in _SET_NAME.finditer(code):
            kind, content = last.get(call.group(1), ("", None))
            if kind != "ComboBoxItem":
                continue
            if content is not None and _norm(content) == _norm(call.group(2)):
                continue
            found.append((n, line.strip()))
    return found


def _keyed_names() -> list[str]:
    found = []
    for path in _walk(".cs"):
        if os.path.normcase(path) == os.path.normcase(_HELPER):
            continue
        with open(path, encoding="utf-8") as f:
            for n, line in _keyed_lines(f.read().splitlines()):
                found.append(f"{os.path.relpath(path, _REPO)}:{n}: {line}")
    return found


def _clear_lines(lines) -> list[tuple[int, str]]:
    """``(line number, line)`` of every ``Items.Clear()`` in code (not in a
    comment) in one file's lines."""
    return [
        (n, line.strip())
        for n, line in enumerate(lines, 1)
        if _ITEMS_CLEAR.search(line.split("//", 1)[0])
    ]


def _cleared_pickers() -> list[str]:
    found = []
    for path in _walk(".cs"):
        if os.path.normcase(path) == os.path.normcase(_HELPER):
            continue
        rel = os.path.relpath(path, _REPO).replace(os.sep, "/")
        if rel in _CLEAR_ALLOWED:
            continue
        with open(path, encoding="utf-8") as f:
            for n, line in _clear_lines(f.read().splitlines()):
                found.append(f"{rel}:{n}: {line}")
    return found


def _xaml_named_items() -> list[str]:
    found = []
    for path in _walk(".xaml"):
        with open(path, encoding="utf-8") as f:
            text = f.read()
        for item in _XAML_ITEM.finditer(text):
            if "AutomationProperties.Name=" in item.group(0):
                n = text.count("\n", 0, item.start()) + 1
                found.append(f"{os.path.relpath(path, _REPO)}:{n}: {item.group(0).strip()}")
    return found


def test_the_helper_names_by_label_and_keys_by_automation_id():
    with open(_HELPER, encoding="utf-8") as f:
        src = f.read()
    assert "AutomationProperties.SetName(item, label)" in src, (
        "PickerOption must name its item by the label it paints"
    )
    assert "AutomationProperties.SetAutomationId(item, key)" in src, (
        "PickerOption must carry the stable key on the item's AutomationId"
    )


def test_no_code_built_picker_item_is_named_by_its_key():
    hits = _keyed_names()
    assert not hits, (
        "a ComboBoxItem's UIA Name is set to something other than the label it "
        "paints — build a keyed option with FaunaApp.Helpers.PickerOption.Create("
        "label, key) instead (convention 11, What counts as offered):\n  "
        + "\n  ".join(hits)
    )


def test_no_xaml_picker_item_is_named_by_its_key():
    hits = _xaml_named_items()
    assert not hits, (
        "a XAML ComboBoxItem sets AutomationProperties.Name — its Name is its "
        "painted Content; put the key on AutomationProperties.AutomationId:\n  "
        + "\n  ".join(hits)
    )


def test_the_helper_holds_the_one_items_clear():
    with open(_HELPER, encoding="utf-8") as f:
        lines = f.read().splitlines()
    assert len(_clear_lines(lines)) == 1, (
        "PickerOption.cs should hold exactly one Items.Clear(), inside its "
        "change-only ReconcileItems"
    )


def test_no_picker_is_cleared_outside_reconcile():
    hits = _cleared_pickers()
    assert not hits, (
        "an Items.Clear() outside FaunaApp.Helpers.PickerOption: repaint the picker "
        "with PickerOption.Reconcile (or ReconcileAtIndex / ReconcileCatalog), which "
        "rebuilds only when the option set changed — an unconditional clear on a "
        "repaint path kills the app 0xC000027B under an automation client "
        "(docs/goal/architecture/apps/windows.md § Views, Pickers). A control filled "
        "once at its creation needs no clear at all; a site that truly cannot race "
        "goes in _CLEAR_ALLOWED with its reason:\n  " + "\n  ".join(hits)
    )


def test_the_clear_scan_reads_code_not_comments():
    """The clear scan is not vacuous: a planted clear reads as a hit, a mention in
    a comment does not."""
    lines = [
        "/// <c>Items.Clear()</c>-and-rebuild there tears down the item",
        "        // FilterCombo.Items.Clear(); retired",
        "        FilterCombo.Items.Clear();",
        "        combo.Items .Clear( );",
        "        FilterCombo.Children.Clear();",
    ]
    assert [n for n, _ in _clear_lines(lines)] == [3, 4]


def test_the_scan_tells_a_key_from_a_label():
    """The scan is not vacuous: the retired shape reads as a hit, a label-only
    item's explicit Name does not, and a same-named variable of another type is
    not mistaken for a picker item."""
    keyed = [
        "var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };",
        "AutomationProperties.SetName(item, opt.value);",
    ]
    labelled = [
        "var item = new ComboBoxItem { Content = label };",
        "AutomationProperties.SetName(item, label);",
    ]
    other = [
        "var item = new ComboBoxItem { Content = label };",
        "var item = new Border();",
        'AutomationProperties.SetName(item, "a-row");',
    ]
    assert [n for n, _ in _keyed_lines(keyed)] == [2]
    assert _keyed_lines(labelled) == []
    assert _keyed_lines(other) == []
