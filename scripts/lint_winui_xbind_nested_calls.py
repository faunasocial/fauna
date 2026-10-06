#!/usr/bin/env -S uv run --quiet --no-project
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Lint: a WinUI `{x:Bind ...}` expression must NOT nest one function call inside
another's argument list — `{x:Bind Foo(Bar(x))}`.

Why this exists (the recurring trap): the `x:Bind` compiler cannot compile a
function-call argument that is itself a function call. On ARM64 it crashes the
out-of-process XamlCompiler.exe with no usable diagnostic
(microsoft/microsoft-ui-xaml#8871 — the broader ARM64 XAML-toolchain breakage),
so a session that writes a nested call hits an opaque MSBuild failure rather than
a clear "you nested a call" message. The fix is to extract a helper method and
bind to that.

What is FINE and must NOT be flagged:
  - property paths:            {x:Bind ViewModel.IsReady}
  - a single call:             {x:Bind local:Foo.ToVisibility(IsReady)}
  - multi-arg single call:     {x:Bind local:Foo.FormatTag(Tags, 0)}
  - zero-arg call:             {x:Bind local:Foo.Loader()}
  - dotted property-path args: {x:Bind local:Foo.ToVis(ViewModel.X), Mode=OneWay}
  - attached-property parens:  {x:Bind (Grid.Row)}    (a path step, not a call)
  - binding settings / nested markup extensions after the positional expression
    ({x:Bind Foo(X), Converter={StaticResource c}}) — they carry no call-parens.

Only {x:Bind} is checked: {Binding} is runtime-evaluated and never sees the XAML
compiler, so it does not hit this trap.

Detection: scan each {x:Bind ...} body tracking paren depth (ignoring quoted
string-literal args). A "call-open" '(' is one immediately preceded by an
identifier character (the method name); a grouping/attached-property '(' is not.
A call-open found while already inside another call's argument list (depth >= 1)
is a nested call.

Usage: python3 scripts/lint_winui_xbind_nested_calls.py
Exit 0 = clean, 1 = violations found.
"""
import sys
from pathlib import Path
from typing import NamedTuple, Optional

ROOT = Path(__file__).resolve().parent.parent
WINUI_DIR = ROOT / "apps" / "fauna-windows"

_XBIND = "{x:Bind"


class Violation(NamedTuple):
    source: str
    line: int
    expression: str  # the offending x:Bind body, whitespace-normalized


def _line_of(text: str, idx: int) -> int:
    return text.count("\n", 0, idx) + 1


def _extract_markup(text: str, start: int) -> Optional[tuple[str, int]]:
    """`start` is the index of the '{' opening a markup extension. Return
    (body, end_idx) where `body` is the text strictly between the outer braces
    and `end_idx` is the index just past the closing '}'. Respects nested braces
    (a `Converter={StaticResource x}` setting) and quoted strings. None if the
    extension is unterminated."""
    i = start + 1
    n = len(text)
    depth = 1
    quote = None
    while i < n:
        c = text[i]
        if quote:
            if c == quote:
                quote = None
        elif c in ("'", '"'):
            quote = c
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1:i], i + 1
        i += 1
    return None


def _has_nested_call(body: str) -> bool:
    """True if `body` (an x:Bind expression) nests one function call inside
    another's argument list. A call-open '(' is preceded by an identifier
    character; an attached-property/grouping '(' is not. A call-open seen at
    paren-depth >= 1 sits inside another call's args → nested."""
    depth = 0
    quote = None
    prev = ""  # previous non-whitespace char (to classify the next '(')
    for c in body:
        if quote:
            if c == quote:
                quote = None
            # stay inside the string literal; do not update `prev`/depth
            continue
        if c in ("'", '"'):
            quote = c
            prev = c
            continue
        if c == "(":
            is_call_open = prev.isalnum() or prev == "_"
            if is_call_open and depth >= 1:
                return True
            depth += 1
        elif c == ")":
            if depth > 0:
                depth -= 1
        if not c.isspace():
            prev = c
    return False


def find_nested_xbind_violations(xaml_text: str, *, source: str = "<text>") -> list[Violation]:
    """Return one Violation per {x:Bind ...} expression that nests a function
    call inside another call's argument list."""
    violations: list[Violation] = []
    pos = 0
    while True:
        at = xaml_text.find(_XBIND, pos)
        if at == -1:
            break
        # Match the markup-extension name exactly: next char must end the token
        # (whitespace or the closing brace), not e.g. "{x:BindHelper".
        after = xaml_text[at + len(_XBIND): at + len(_XBIND) + 1]
        if after not in (" ", "\t", "\r", "\n", "}", ""):
            pos = at + len(_XBIND)
            continue
        extracted = _extract_markup(xaml_text, at)
        if extracted is None:
            break
        body, end = extracted
        pos = end
        if _has_nested_call(body):
            violations.append(Violation(
                source=source,
                line=_line_of(xaml_text, at),
                expression="{x:Bind " + " ".join(body[len("x:Bind"):].split()) + "}",
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
        out.extend(find_nested_xbind_violations(p.read_text(encoding="utf-8"), source=rel))
    return out


def main(argv=None) -> int:
    violations = scan_repo()
    if not violations:
        print("WinUI x:Bind nested-call lint: OK (no nested function calls in x:Bind expressions).")
        return 0
    print("WinUI x:Bind nested-call lint — FAILURES:\n")
    for v in violations:
        print(f"  {v.source}:{v.line}  {v.expression}")
    print(f"\n{len(violations)} violation(s). The x:Bind compiler cannot compile a function "
          "call whose argument is itself a function call (on ARM64 it crashes XamlCompiler.exe "
          "opaquely — microsoft/microsoft-ui-xaml#8871). Extract a helper method and bind to "
          "that instead.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
