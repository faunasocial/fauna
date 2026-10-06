#!/usr/bin/env python3
"""Generate abstract FakeBase classes for windows C# test fakes.

Reads every generated UniFFI C# interface in
`apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/*.cs` and emits one
abstract `<X>FakeBase : I<X>` per interface into
`apps/fauna-windows/FaunaApp/FaunaApp.Tests/Generated/FakeBases.g.cs` (gitignored).
Every member is `virtual` and throws `NotSupportedException` by default, so a
hand-written test fake only needs to `override` the members it actually
implements — a Rust-side interface growth then compiles clean instead of the
recurring CS0535 ("member not implemented") break.

Run as the last step of `_windows-ffi-bindgen` in the justfile — never invoke
directly except to hand-verify the growth-simulation property: append a member
to a faked interface's generated `.cs`, rerun this script alone (a full
`just windows-ffi` would overwrite the edit), then `dotnet test` — it must
compile and pass with the new member auto-throwing.

Zero config: every `(public|internal) interface I<Name> { ... }` found in the
source directory gets a fake base, so a brand-new UniFFI object needs no
generator change, only a fake that inherits `<Name>FakeBase` when a test wants
one.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCE_DIR = REPO_ROOT / "apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi"
OUTPUT_FILE = REPO_ROOT / "apps/fauna-windows/FaunaApp/FaunaApp.Tests/Generated/FakeBases.g.cs"

_NAMESPACE_RE = re.compile(r"^\s*namespace\s+([\w.]+)\s*;", re.MULTILINE)
# Matches both import form (`using uniffi.fauna_core;`) and alias form
# (`using ThreadId = String;`, `using FfiConverterTypeActorId = FfiConverterByteArray;`).
# Alias directives are FILE-SCOPED in C# — reproducing them verbatim per source
# file (never merged into one global list) is required: two source files can
# alias the same name to different-looking RHS text for the same underlying
# type (e.g. `= FfiConverterString` inside fauna_core.cs itself vs. the
# fully-qualified `= uniffi.fauna_core.FfiConverterString` from every other
# crate's file), which would collide (CS7007) if hoisted into one shared list.
_USING_RE = re.compile(r"^\s*using\s+.+;\s*$", re.MULTILINE)
_INTERFACE_RE = re.compile(
    r"(?:public|internal)\s+interface\s+(\w+)\s*(?::\s*([\w,\s<>]+))?\s*\{"
)
_METHOD_RE = re.compile(r"^(?P<ret>.+?)\s+(?P<name>\w+)\s*\((?P<params>.*)\)\s*$", re.DOTALL)
_PROPERTY_RE = re.compile(
    r"^(?P<type>.+?)\s+(?P<name>\w+)\s*\{\s*get\s*;\s*(?P<set>set\s*;\s*)?\}\s*$", re.DOTALL
)


class ParseError(RuntimeError):
    pass


class Member:
    def __init__(self, kind, ret_or_type, name, params="", has_setter=False):
        self.kind = kind  # "method" | "property"
        self.ret_or_type = ret_or_type
        self.name = name
        self.params = params
        self.has_setter = has_setter


def _find_matching_brace(text: str, open_idx: int) -> int:
    """Index of the `}` matching the `{` at open_idx (text[open_idx] must be '{')."""
    depth = 0
    for i in range(open_idx, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return i
    raise ParseError(f"unbalanced braces starting at index {open_idx}")


def _split_members(body: str) -> list[str]:
    """Split an interface body into member statements. A method member ends at
    a top-level `;`; a property member is a balanced `{ ... }` block (so the
    `;` separating `get;`/`set;` inside it is not mistaken for a terminator)."""
    members = []
    current: list[str] = []
    depth = 0
    for ch in body:
        current.append(ch)
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                members.append("".join(current))
                current = []
        elif ch == ";" and depth == 0:
            members.append("".join(current))
            current = []
    tail = "".join(current).strip()
    if tail:
        raise ParseError(f"trailing content after last interface member: {tail!r}")
    return members


def _strip_comments(member: str) -> str:
    lines = [ln for ln in member.splitlines() if not ln.strip().startswith("//")]
    return "\n".join(lines).strip()


def _parse_member(raw: str) -> Member | None:
    text = _strip_comments(raw).strip()
    if not text:
        return None
    text = text.rstrip(";").strip()
    if text.endswith("}"):
        m = _PROPERTY_RE.match(text)
        if not m:
            raise ParseError(f"could not parse interface property: {text!r}")
        return Member(
            "property", m.group("type").strip(), m.group("name"), has_setter=bool(m.group("set"))
        )
    m = _METHOD_RE.match(text)
    if not m:
        raise ParseError(f"could not parse interface method: {text!r}")
    return Member("method", m.group("ret").strip(), m.group("name"), params=m.group("params").strip())


def _emit_member(m: Member, interface_name: str) -> str:
    full_name = f"{interface_name}.{m.name}"
    msg = f"{full_name}: outside this fake's implemented surface"
    if m.name == "Dispose" and m.kind == "method" and m.ret_or_type == "void" and not m.params:
        return "    public virtual void Dispose() { }\n"
    if m.kind == "property":
        getter = f'get => throw new NotSupportedException(\n                "{msg}");'
        if m.has_setter:
            setter = f'set => throw new NotSupportedException(\n                "{msg}");'
            return (
                f"    public virtual {m.ret_or_type} {m.name}\n    {{\n"
                f"        {getter}\n        {setter}\n    }}\n"
            )
        return f"    public virtual {m.ret_or_type} {m.name}\n    {{\n        {getter}\n    }}\n"
    return (
        f"    public virtual {m.ret_or_type} {m.name}({m.params}) =>\n"
        f'        throw new NotSupportedException(\n            "{msg}");\n'
    )


def _generate(source_dir: Path) -> tuple[str, int]:
    if not source_dir.is_dir():
        raise ParseError(f"source dir not found: {source_dir} (run `just windows-ffi` first)")

    blocks: list[tuple[str, list[str], list[str]]] = []
    total_interfaces = 0

    for cs_file in sorted(source_dir.glob("*.cs")):
        text = cs_file.read_text(encoding="utf-8")
        ns_match = _NAMESPACE_RE.search(text)
        if not ns_match:
            continue
        namespace = ns_match.group(1)

        interface_matches = list(_INTERFACE_RE.finditer(text))
        if not interface_matches:
            continue

        # This file's own usings, scoped to its own namespace block below —
        # never merged across files (see _USING_RE's docstring).
        file_usings = _USING_RE.findall(text)

        classes = []
        for im in interface_matches:
            iface_name = im.group(1)
            bases = [b.strip() for b in (im.group(2) or "").split(",") if b.strip()]
            open_brace = im.end() - 1
            close_brace = _find_matching_brace(text, open_brace)
            # Strip comments BEFORE brace/semicolon splitting: an XML doc line can
            # contain a literal `;` (English prose) or a markdown code-span `{...}`
            # (e.g. `` `Foo.{a, b}` ``), either of which would otherwise be mistaken
            # for a real member boundary and corrupt the split.
            body = _strip_comments(text[open_brace + 1 : close_brace])

            base_name = iface_name[1:] if iface_name.startswith("I") else iface_name
            class_name = f"{base_name}FakeBase"

            try:
                members = [m for m in (_parse_member(raw) for raw in _split_members(body)) if m]
            except ParseError as e:
                raise ParseError(f"{cs_file.name}: interface {iface_name}: {e}") from e

            lines = [f"internal abstract class {class_name} : {iface_name}", "{"]
            for m in members:
                lines.append(_emit_member(m, iface_name))
            member_names = {m.name for m in members}
            if "IDisposable" in bases and "Dispose" not in member_names:
                lines.append("    public virtual void Dispose() { }\n")
            lines.append("}")
            classes.append("\n".join(lines))
            total_interfaces += 1

        blocks.append((namespace, file_usings, classes))

    header = (
        "// <auto-generated>\n"
        "//     Generated by scripts/generate-windows-fake-bases.py — DO NOT EDIT.\n"
        "//     One abstract <X>FakeBase : I<X> per generated UniFFI interface; every\n"
        "//     member is virtual and throws NotSupportedException, so a hand-written\n"
        "//     fake only needs to override the members it actually implements.\n"
        "//     See docs/goal/architecture/merge-gate-check.md § Merge-gate check (win).\n"
        "// </auto-generated>\n\n"
        "#nullable enable\n\n"
    )
    parts = [header]
    for namespace, file_usings, classes in blocks:
        parts.append(f"namespace {namespace}\n{{\n")
        parts.extend(f"    {u}\n" for u in file_usings)
        parts.append("\n")
        parts.append("\n\n".join(classes))
        parts.append("\n\n}\n\n")

    return "".join(parts), total_interfaces


def main() -> int:
    try:
        content, total_interfaces = _generate(SOURCE_DIR)
    except ParseError as e:
        print(f"generate-windows-fake-bases: {e}", file=sys.stderr)
        return 1

    OUTPUT_FILE.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_FILE.write_text(content, encoding="utf-8")
    print(f"generate-windows-fake-bases: {total_interfaces} fake base(s) -> {OUTPUT_FILE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
