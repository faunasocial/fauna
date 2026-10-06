#!/usr/bin/env -S uv run --quiet --no-project
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Lint: an `{x:Bind ...}` expression inside a WinUI `<DataTemplate>` that declares
no `x:DataType` has no binding context — it silently resolves to nothing (or, when
it is the template's ONLY `x:Bind`, crashes the generated `SetDataRoot` with an
`ArgumentException` the moment the template is realized, since that binding was the
whole reason the XAML compiler emitted a binding class for the template at all).

Why this exists (a recurring trap, not a one-off): a bulk change mechanically
converted 1410 literal `AutomationProperties.AutomationId="..."` sites to
`{x:Bind ids:Ids...}` across 98 files. Neither MSBuild, the 1669+ C# unit tests,
nor `ui-registry-lint` (which greps the *source text* for the id — now the
constant's NAME, still present) ever see the resulting empty binding; only a
running e2e does. Measured twice before this lint existed —
`Controls/RecipientPicker.xaml` (silently dropped ids) and
`Views/NotificationsPage.xaml` (crashed the app outright) — plus 70
further latent sites swept in one pass. A nested
`<DataTemplate>` does NOT inherit its parent's `x:DataType`: each is its own
binding scope and is checked independently.

The fix, in order of preference: (1) restore the binding to a literal value (most
DataTemplates here have no single bound type — several are filled by a generic
helper, e.g. `SetChips<T>` — so this is usually the only option); (2) give the
DataTemplate an `x:DataType` if it genuinely has one bound type throughout.

Usage: python3 scripts/lint_winui_xbind_datatemplate_scope.py
Exit 0 = clean, 1 = violations found.
"""
import re
import sys
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parent.parent
WINUI_DIR = ROOT / "apps" / "fauna-windows"

_FULL_TAG_NAME_RE = re.compile(r"<\s*/?\s*([A-Za-z_][\w:\-.]*)")
_DATATYPE_RE = re.compile(r"x:DataType\s*=")
_XBIND_ATTR_RE = re.compile(r'([\w:.]+)\s*=\s*(["\'])(\{x:Bind\b.*?)\2', re.DOTALL)


class Violation(NamedTuple):
    source: str
    line: int
    element: str
    attribute: str
    expression: str


def _line_of(text: str, idx: int) -> int:
    return text.count("\n", 0, idx) + 1


def _opening_tag(text: str, start: int):
    """From the '<' at `start`, return (tag_text, end_idx) spanning to the matching
    top-level '>' (inclusive), respecting quoted attribute values. None if
    unterminated."""
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


def _full_tag_name(tag_text: str) -> str:
    m = _FULL_TAG_NAME_RE.match(tag_text)
    return m.group(1) if m else ""


def _iter_tags(text: str):
    """Yield (kind, name, tag_text, start_idx) for every element tag in `text`, in
    document order — kind is 'open', 'close', or 'selfclose'. `name` is the FULL
    tag identifier including any dot (so a property element like
    `<DataTemplate.Resources>` is never confusable with a real `<DataTemplate>`).
    Comments, `<?xml ?>`, and CDATA are skipped."""
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


def find_unscoped_xbind_violations(xaml_text: str, *, source: str = "<text>") -> list[Violation]:
    """Return one Violation per `{x:Bind ...}` attribute whose nearest enclosing
    `<DataTemplate>` declares no `x:DataType`."""
    violations: list[Violation] = []
    dt_stack: list[bool] = []  # one bool (has x:DataType) per open <DataTemplate>
    for kind, name, tag_text, start in _iter_tags(xaml_text):
        if name == "DataTemplate":
            if kind == "open":
                dt_stack.append(bool(_DATATYPE_RE.search(tag_text)))
            elif kind == "close":
                if dt_stack:
                    dt_stack.pop()
            # selfclose: an empty <DataTemplate/> has no children to scan.

        if kind in ("open", "selfclose") and dt_stack and not dt_stack[-1]:
            for m in _XBIND_ATTR_RE.finditer(tag_text):
                attribute, _quote, expr = m.group(1), m.group(2), m.group(3)
                violations.append(Violation(
                    source=source,
                    line=_line_of(xaml_text, start),
                    element=name,
                    attribute=attribute,
                    expression=expr + "}",
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
        out.extend(find_unscoped_xbind_violations(p.read_text(encoding="utf-8"), source=rel))
    return out


def main(argv=None) -> int:
    violations = scan_repo()
    if not violations:
        print("WinUI x:Bind DataTemplate-scope lint: OK (no unscoped x:Bind inside a DataTemplate).")
        return 0
    print("WinUI x:Bind DataTemplate-scope lint — FAILURES:\n")
    for v in violations:
        print(f"  {v.source}:{v.line}  <{v.element} {v.attribute}=\"{v.expression}\">")
    print(f"\n{len(violations)} violation(s). Each DataTemplate above declares no x:DataType, "
          "so its {x:Bind} has no binding context: it silently resolves to nothing, or — if it "
          "is the template's only x:Bind — crashes the generated SetDataRoot the moment the "
          "template is realized (see Controls/RecipientPicker.xaml for "
          "the worked fix). Restore the binding to a literal value, or add x:DataType if the "
          "template genuinely has one bound type.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
