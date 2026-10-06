#!/usr/bin/env -S uv run --quiet --no-project
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""Lint: WinUI bare layout containers that carry an AutomationProperties.AutomationId
must ALSO carry an AutomationProperties.Name — checked in two scopes: (1) a
DataTemplate root, (2) a page-level container whose id ui.yaml declares.

Why this exists (the recurring trap): a layout panel/decorator (Grid, StackPanel,
Border, …) has no default UIA Content-view AutomationPeer. When such an element is
pruned from the discoverable tree, the FlaUI driver's
`FindAllDescendants(ByAutomationId)` counts **0 even when the element exists**
(confirmed for search-result-item, then a sweep of 18 more DataTemplate roots, then
the page-level `atproto-depth-selector` StackPanel). Adding
`AutomationProperties.Name="{x:Bind <field>}"` (or any non-empty literal) gives
the element an accessible name, which un-prunes it.

Content-control roots (TextBlock, Button, CheckBox, RadioButton, ToggleSwitch, …)
have native peers and need no Name — they are intentionally NOT flagged. Custom
UserControls (prefixed tags like `controls:Foo`) carry their own peers too.

Scope 1 — DataTemplate root: the element list virtualization materializes and the
driver counts. Bare layout elements nested deeper are out of scope (the list-item
AutomationId belongs on the root, or on a content control with its own peer).

Scope 2 — page-level container: any bare layout element OUTSIDE a DataTemplate
whose AutomationId matches a ui.yaml-declared element id. The id-match requirement
is what keeps this from flagging every decorative panel — most page layout Grids
carry no AutomationId, or one ui.yaml never heard of. Elements nested inside a
DataTemplate are excluded from this scope (scope 1 already owns that case, and
"root only" is the same rule there).

Usage: python3 scripts/lint_winui_datatemplate_names.py
Exit 0 = clean, 1 = violations found.
"""
import re
import sys
from pathlib import Path
from typing import NamedTuple

import yaml

ROOT = Path(__file__).resolve().parent.parent
WINUI_DIR = ROOT / "apps" / "fauna-windows"
UI_YAML = ROOT / "tests" / "e2e-unified" / "ui.yaml"

# ui.yaml is ~9k lines; PyYAML's pure-Python SafeLoader takes ~11s on it (measured
# 2026-07-31) vs ~0.5s for the libyaml-backed CSafeLoader — the gap between "cheap
# merge-gate tier" and not. Fall back to SafeLoader only if libyaml truly isn't
# available (CSafeLoader is present whenever yaml.__with_libyaml__ is True, which
# holds on every dev machine this has been checked on).
_YAML_LOADER = getattr(yaml, "CSafeLoader", yaml.SafeLoader)

# WinUI layout/decorator elements with no default Content-view AutomationPeer.
# A DataTemplate root of one of these types + an AutomationId but no Name is the trap.
LAYOUT_ROOT_TYPES = frozenset({
    "Grid", "StackPanel", "RelativePanel", "Canvas", "Border", "Viewbox",
    "WrapPanel", "VariableSizedWrapGrid", "ItemsStackPanel", "ItemsWrapGrid",
    # ItemsControl/ItemsRepeater added 2026-08-04 because this lint MISSED
    # the fifth recurrence of the very trap it exists to stop: `critical-alerts`
    # (MainPage.xaml) is a page-level ItemsControl carrying only an AutomationId,
    # and UIA pruned it while its rows resolved normally — so the custody alarm
    # read as "never fires" for two sessions when it had been firing all along.
    # A bare ItemsControl gets no ControlTemplate and therefore no Content-view
    # peer, exactly like the panels above; the empirical proof is that adding a
    # Name is what put the container back in the tree.
    "ItemsControl", "ItemsRepeater",
})

_DATATEMPLATE = "<DataTemplate"
_AUTO_ID_RE = re.compile(r"""AutomationProperties\.AutomationId\s*=\s*(["'])(.*?)\1""", re.DOTALL)
_NAME_RE = re.compile(r"""AutomationProperties\.Name\s*=\s*(["'])(.*?)\1""", re.DOTALL)
_X_NAME_RE = re.compile(r"""x:Name\s*=\s*(["'])(.*?)\1""", re.DOTALL)
_TAG_NAME_RE = re.compile(r"<\s*([A-Za-z_][\w:\-]*)")
# Includes '.' so property-element syntax (<Grid.RowDefinitions>, <DataTemplate.Resources>)
# is captured WHOLE rather than truncated at the dot — truncating would make it
# indistinguishable from a real <Grid>/<DataTemplate> instance.
_FULL_TAG_NAME_RE = re.compile(r"<\s*/?\s*([A-Za-z_][\w:\-.]*)")


class Violation(NamedTuple):
    source: str
    line: int
    element: str
    automation_id: str
    context: str = "DataTemplate root"


def _line_of(text: str, idx: int) -> int:
    return text.count("\n", 0, idx) + 1


def _opening_tag(text: str, start: int):
    """From the '<' at `start`, return (opening_tag_text, end_idx) spanning to the
    matching top-level '>' (inclusive of '>'), respecting quoted attribute values.
    Returns None if no closing '>' is found."""
    i = start
    n = len(text)
    quote = None
    while i < n:
        c = text[i]
        if quote:
            if c == quote:
                quote = None
        elif c in ("'", '"'):
            quote = c
        elif c == ">":
            return text[start:i + 1], i + 1
        i += 1
    return None


def _next_element_start(text: str, idx: int):
    """Skip whitespace and XML comments from `idx`; return the index of the next
    element-opening '<', or None if a non-element character or EOF is hit first."""
    n = len(text)
    while idx < n:
        while idx < n and text[idx].isspace():
            idx += 1
        if idx >= n:
            return None
        if text.startswith("<!--", idx):
            end = text.find("-->", idx)
            if end == -1:
                return None
            idx = end + 3
            continue
        return idx if text[idx] == "<" else None
    return None


def _tag_name(opening_tag_text: str) -> str:
    m = _TAG_NAME_RE.match(opening_tag_text)
    return m.group(1) if m else ""


def _full_tag_name(tag_text: str) -> str:
    m = _FULL_TAG_NAME_RE.match(tag_text)
    return m.group(1) if m else ""


def _iter_tags(text: str):
    """Yield (kind, name, tag_text, start_idx) for every element tag in `text`, in
    document order — kind is 'open', 'close', or 'selfclose'. Comments, the `<?xml
    ?>` declaration, and CDATA sections are silently skipped. `name` is the FULL
    tag identifier (dots included), so a property element (`<Grid.RowDefinitions>`)
    is never confusable with a real `<Grid>` instance. Reuses `_opening_tag`'s
    quote-aware scan, so a `>` inside a quoted attribute value doesn't end the tag
    early."""
    i = 0
    n = len(text)
    while i < n:
        lt = text.find("<", i)
        if lt == -1:
            return
        if text.startswith("<!--", lt):
            end = text.find("-->", lt)
            if end == -1:
                return
            i = end + 3
            continue
        if text.startswith("<?", lt):
            end = text.find("?>", lt)
            if end == -1:
                return
            i = end + 2
            continue
        if text.startswith("<![CDATA[", lt):
            end = text.find("]]>", lt)
            if end == -1:
                return
            i = end + 3
            continue
        opened = _opening_tag(text, lt)
        if opened is None:
            return
        tag_text, end_idx = opened
        inner = tag_text[1:-1].strip()
        if inner.startswith("/"):
            yield ("close", _full_tag_name(tag_text), tag_text, lt)
        elif inner.endswith("/"):
            yield ("selfclose", _full_tag_name(tag_text), tag_text, lt)
        else:
            yield ("open", _full_tag_name(tag_text), tag_text, lt)
        i = end_idx


def find_datatemplate_root_violations(xaml_text: str, *, source: str = "<text>") -> list[Violation]:
    """Return one Violation per DataTemplate whose root is a bare layout container
    carrying AutomationProperties.AutomationId but missing (or empty)
    AutomationProperties.Name."""
    violations: list[Violation] = []
    pos = 0
    while True:
        dt = xaml_text.find(_DATATEMPLATE, pos)
        if dt == -1:
            break
        # Match the element exactly: next char must end the token (e.g. not
        # <DataTemplateSelector>).
        after = xaml_text[dt + len(_DATATEMPLATE): dt + len(_DATATEMPLATE) + 1]
        dt_open = _opening_tag(xaml_text, dt)
        if dt_open is None:
            break
        dt_tag_text, root_search_start = dt_open
        pos = root_search_start
        if after not in (" ", "\t", "\r", "\n", ">", "/"):
            continue
        if dt_tag_text.rstrip().endswith("/>"):
            continue  # self-closing <DataTemplate/> has no root

        root_start = _next_element_start(xaml_text, root_search_start)
        if root_start is None:
            continue
        root_open = _opening_tag(xaml_text, root_start)
        if root_open is None:
            continue
        root_tag_text, _ = root_open
        if _tag_name(root_tag_text) not in LAYOUT_ROOT_TYPES:
            continue
        m_id = _AUTO_ID_RE.search(root_tag_text)
        if not m_id:
            continue  # no list-item id on the root → not the trap
        m_name = _NAME_RE.search(root_tag_text)
        if m_name is None or len(m_name.group(2)) == 0:
            violations.append(Violation(
                source=source,
                line=_line_of(xaml_text, root_start),
                element=_tag_name(root_tag_text),
                automation_id=m_id.group(2),
            ))
    return violations


def iter_xaml_files(root: Path = WINUI_DIR):
    for p in sorted(root.rglob("*.xaml")):
        parts = set(p.parts)
        if "obj" in parts or "bin" in parts:
            continue  # build outputs are regenerated copies
        yield p


def scan_repo(root: Path = WINUI_DIR) -> list[Violation]:
    out: list[Violation] = []
    for p in iter_xaml_files(root):
        try:
            rel = str(p.relative_to(ROOT))
        except ValueError:
            rel = str(p)
        out.extend(find_datatemplate_root_violations(p.read_text(encoding="utf-8"), source=rel))
    return out


def _has_code_behind_set_name(code_behind_text: str, x_name: str) -> bool:
    """True iff `code_behind_text` contains an `AutomationProperties.SetName(<x_name>, ...)`
    call targeting the given x:Name — this codebase's established alternative to an
    inline XAML Name for imperatively-rendered content (e.g. EventsPage.xaml.cs's
    per-cell/event SetName calls, DmMessageBubble.xaml.cs's RenderLinkPreview /
    RenderQuote / RenderContentLabel). Required precisely because scope 2 would
    otherwise false-positive on every one of those: the element's own XAML tag has
    no inline Name by design. Matches both the fully-qualified call and a
    `using`-shortened `AutomationProperties.SetName(...)` — both end in
    `SetName(<x_name>` — at the cost of also matching an unrelated same-named bare
    `SetName(` call; harmless here since it can only suppress a would-be violation,
    never manufacture one, and no such collision exists in the tree today."""
    if not x_name:
        return False
    pattern = re.compile(r"\bSetName\s*\(\s*" + re.escape(x_name) + r"\s*[,)]")
    return bool(pattern.search(code_behind_text))


def find_page_level_container_violations(
    xaml_text: str, ui_yaml_ids: frozenset, *, source: str = "<text>", code_behind_text: str = ""
) -> list[Violation]:
    """Return one Violation per bare layout element OUTSIDE any DataTemplate that
    carries an AutomationProperties.AutomationId ui.yaml declares but has no Name —
    neither inline (AutomationProperties.Name) nor set imperatively in the sibling
    .xaml.cs (AutomationProperties.SetName, keyed off x:Name). Page-level twin of
    `find_datatemplate_root_violations` — see module docstring § Scope 2."""
    violations: list[Violation] = []
    dt_depth = 0
    for kind, name, tag_text, start in _iter_tags(xaml_text):
        if name == "DataTemplate":
            if kind == "open":
                dt_depth += 1
            elif kind == "close":
                dt_depth = max(0, dt_depth - 1)
            continue  # DataTemplate itself is never a LAYOUT_ROOT_TYPES match
        if kind == "close":
            continue
        if dt_depth > 0:
            continue  # inside a DataTemplate — scope 1's job, not this one's
        if name not in LAYOUT_ROOT_TYPES:
            continue
        m_id = _AUTO_ID_RE.search(tag_text)
        if not m_id:
            continue
        automation_id = m_id.group(2)
        if automation_id not in ui_yaml_ids:
            continue  # not a ui.yaml id → decorative panel, not the trap
        m_name = _NAME_RE.search(tag_text)
        if m_name is not None and len(m_name.group(2)) > 0:
            continue
        m_x_name = _X_NAME_RE.search(tag_text)
        if m_x_name and _has_code_behind_set_name(code_behind_text, m_x_name.group(2)):
            continue
        violations.append(Violation(
            source=source,
            line=_line_of(xaml_text, start),
            element=name,
            automation_id=automation_id,
            context="page-level container",
        ))
    return violations


def load_ui_yaml_ids(path: Path = UI_YAML) -> frozenset:
    """Every element id ui.yaml declares anywhere — global elements, each page's
    `elements`/`optional_elements`/`platform_elements`, and component elements —
    collected by walking the whole parsed structure rather than hand-tracking each
    nesting shape, so a new page/component doesn't silently fall outside scope.
    Also includes the flat `elements:` registry's own keys (a few ids — e.g.
    `atproto-depth-selector` at the time this was written — live only in a page's
    `elements:` list and are missing from the registry; that gap is a separate,
    pre-existing ui.yaml drift, not something to special-case here)."""
    spec = yaml.load(path.read_text(encoding="utf-8"), Loader=_YAML_LOADER)
    ids: set[str] = set()

    def walk(node) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                if key in ("elements", "optional_elements") and isinstance(value, list):
                    ids.update(v for v in value if isinstance(v, str))
                elif key == "platform_elements" and isinstance(value, dict):
                    for plist in value.values():
                        if isinstance(plist, list):
                            ids.update(v for v in plist if isinstance(v, str))
                walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(spec)
    ids.update(spec.get("elements", {}).keys())
    return frozenset(ids)


def scan_repo_page_level(ui_yaml_ids: frozenset, root: Path = WINUI_DIR) -> list[Violation]:
    out: list[Violation] = []
    for p in iter_xaml_files(root):
        try:
            rel = str(p.relative_to(ROOT))
        except ValueError:
            rel = str(p)
        code_behind = p.with_name(p.name + ".cs")
        code_behind_text = code_behind.read_text(encoding="utf-8") if code_behind.exists() else ""
        out.extend(find_page_level_container_violations(
            p.read_text(encoding="utf-8"), ui_yaml_ids, source=rel, code_behind_text=code_behind_text
        ))
    return out


def main(argv=None) -> int:
    violations = scan_repo()
    violations += scan_repo_page_level(load_ui_yaml_ids())
    if not violations:
        print("WinUI AutomationProperties.Name lint: OK (no bare layout containers "
              "missing AutomationProperties.Name).")
        return 0
    print("WinUI AutomationProperties.Name lint — FAILURES:\n")
    for v in violations:
        print(f'  {v.source}:{v.line}  <{v.element} AutomationId="{v.automation_id}"> '
              f"is a bare layout {v.context} with no AutomationProperties.Name.")
    print(f"\n{len(violations)} violation(s). A bare layout container "
          "(Grid/StackPanel/Border/…) — a DataTemplate row root, or a page-level "
          "container whose AutomationId ui.yaml declares — must also carry "
          "AutomationProperties.Name, or UIA prunes it and FlaUI counts 0 / can't "
          'find it even though it exists. Add AutomationProperties.Name="{x:Bind '
          '<field>}" (or a literal).')
    return 1


if __name__ == "__main__":
    sys.exit(main())
