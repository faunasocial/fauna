#!/usr/bin/env -S uv run --quiet --no-project
"""
Schema-evolution gate for libs/fauna-protocol/schemas/*.cddl.

Compares the PR HEAD's CDDL files against the merge base. Allowed
changes: new optional fields, new variants, new kinds. Blocked: removed
keys, renamed keys, type changes, required-tightened.

The one way past a blocked change is a USER-RATIFIED in-place break listed
in `libs/fauna-protocol/schemas/ratified-breaks.txt` (`cddl <Type> removed`
or `cddl <Type>.<field> <transition>`), each entry excusing only the
transition it names; its Rust analogue `tools/check-additive-evolution`
honours the same file's `rust` entries. The list only ever grows — a
removed name never comes back, and this gate refuses one that does — and a
line lands there only alongside a ratification recorded in
version-compatibility.md § Dimension 2. Grammar owner: transport.md § Schema
and forward-compat discipline.

This is a coarse v1 — line-level heuristics. A future revision will use
a CDDL parser. v1 catches common drift; non-trivial schema changes still
require human review.

Exit 0 = OK. Exit 1 = blocked drift detected.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


SCHEMAS_DIR = "libs/fauna-protocol/schemas"
RATIFIED_BREAKS_PATH = f"{SCHEMAS_DIR}/ratified-breaks.txt"


# The transitions a ratification can name — each excuses exactly the finding
# of the same kind on its key and nothing else. `retyped→<type>` excuses a
# retype TO that type and no other (the Rust twin's `Transition::Retyped`;
# first ratified 2026-09-30 for the baseline reset's credential secrets).
REMOVED = "removed"
TIGHTENED = "optional→required"
RETYPED_PREFIX = "retyped→"
TRANSITIONS = frozenset({REMOVED, TIGHTENED})


def is_transition(token: str) -> bool:
    return token in TRANSITIONS or (
        token.startswith(RETYPED_PREFIX) and len(token) > len(RETYPED_PREFIX)
    )


def parse_ratified_breaks(text: str) -> frozenset[tuple[str, str]]:
    """The `cddl` entries of ratified-breaks.txt as `(key, transition)` pairs.

    One entry per line, `<gate> <key> <transition> <ratified-on>
    <ratification…>`; `#` starts a comment; `rust` entries belong to the
    struct gate and are skipped here WITHOUT validation — its own parser
    (`tools/check-additive-evolution`) validates those. Any OTHER first
    token is malformed: an entry the gate cannot read must never silently
    excuse (or silently stop excusing) anything, so an unrecognized gate
    name raises exactly like a short or invalid line does. `key` is `Type`
    or `Type.field`; a bare `Type` is only ever `removed`.
    """
    entries = set()
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        gate = parts[0]
        if gate == "rust":
            continue
        if gate != "cddl":
            raise ValueError(f"line {n}: unknown gate {gate!r} — want `cddl` or `rust`: {raw!r}")
        if len(parts) < 5:
            raise ValueError(
                f"line {n}: want `cddl <key> <transition> <ratified-on> <ratification…>`: {raw!r}"
            )
        key, transition = parts[1], parts[2]
        if not is_transition(transition):
            raise ValueError(
                f"line {n}: transition {transition!r} is not one of "
                f"{sorted(TRANSITIONS)} or `{RETYPED_PREFIX}<type>`: {raw!r}"
            )
        if "." not in key and transition != REMOVED:
            raise ValueError(f"line {n}: a bare type entry is only ever `{REMOVED}`: {raw!r}")
        entries.add((key, transition))
    return frozenset(entries)


def git_show(ref: str, path: str) -> str | None:
    """Return file contents at ref, or None if file didn't exist there."""
    try:
        return subprocess.check_output(
            ["git", "show", f"{ref}:{path}"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8")
    except subprocess.CalledProcessError:
        return None


def merge_base() -> str:
    return subprocess.check_output(
        ["git", "merge-base", "HEAD", "origin/main"],
        stderr=subprocess.DEVNULL,
    ).decode("utf-8").strip()


def parse_fields(text: str, type_name: str) -> dict[str, tuple[bool, str]]:
    """Parse fields of `<type_name> = { ... }`. Returns {key: (optional, type)}."""
    pat = re.compile(
        rf"^{re.escape(type_name)}\s*=\s*\{{(.*?)^}}",
        re.MULTILINE | re.DOTALL,
    )
    m = pat.search(text)
    if not m:
        return {}
    body = m.group(1)
    fields = {}
    for line in body.splitlines():
        line = line.strip()
        if not line or line.startswith(";") or line.startswith("*"):
            continue
        if line.startswith("?"):
            optional = True
            line = line[1:].strip()
        else:
            optional = False
        # Match `key: type` (key is alphanumeric or quoted string)
        m2 = re.match(r'^"?([^"\s:]+)"?\s*:\s*(.+?)[,;]?\s*$', line)
        if m2:
            key = m2.group(1).strip()
            typ = m2.group(2).strip().rstrip(",").strip()
            fields[key] = (optional, typ)
    return fields


def find_types(text: str) -> set[str]:
    """Find top-level type definitions: `Foo = ...`."""
    return set(re.findall(r"^([A-Z][\w_]*)\s*=", text, re.MULTILINE))


def check_schema_pair(
    path: str,
    base: str | None,
    head: str,
    allow: frozenset[tuple[str, str]] = frozenset(),
) -> list[str]:
    """Blocked diffs between `base` and `head`, minus the ratified ones.

    `allow` holds ratified-breaks.txt's `cddl` entries as `(key,
    transition)`: an entry excuses only the finding of its own transition on
    its own key — `(Type, removed)` the type's removal, `(Type.field,
    removed)` the field's removal, `(Type.field, optional→required)` the
    tightening and never a later removal or retype, `(Type.field,
    retyped→T)` a retype to `T` and no other. And since a removed name never comes back, a head defining a
    type or field the list records as `removed` is itself a finding — which
    is what keeps a `removed` entry from excusing a same-named successor.
    """
    errors = []
    head_types = find_types(head)

    for t in sorted(head_types):
        if (t, REMOVED) in allow:
            errors.append(f"{path}: revives type `{t}`, a ratified removal (ratified-breaks.txt)")
        for f in sorted(parse_fields(head, t)):
            if (f"{t}.{f}", REMOVED) in allow:
                errors.append(
                    f"{path}: type `{t}`: revives field `{f}`, a ratified removal (ratified-breaks.txt)"
                )

    if base is None:
        # New file — only adds; nothing else to check.
        return errors

    def blocked(key: str, transition: str | None, message: str) -> None:
        if transition is not None and (key, transition) in allow:
            return
        errors.append(message)

    base_types = find_types(base)

    for t in sorted(base_types - head_types):
        blocked(t, REMOVED, f"{path}: removed type definition `{t}`")

    for t in sorted(base_types & head_types):
        base_fields = parse_fields(base, t)
        head_fields = parse_fields(head, t)

        for f in sorted(set(base_fields) - set(head_fields)):
            blocked(f"{t}.{f}", REMOVED, f"{path}: type `{t}`: removed field `{f}`")

        for f in sorted(set(base_fields) & set(head_fields)):
            b_opt, b_typ = base_fields[f]
            h_opt, h_typ = head_fields[f]
            if b_opt and not h_opt:
                blocked(
                    f"{t}.{f}",
                    TIGHTENED,
                    f"{path}: type `{t}`: field `{f}` was optional, now required",
                )
            if b_typ != h_typ:
                # Coarse type-equality. False-positives possible; reviewer
                # decides whether the type change is semantically equivalent.
                blocked(
                    f"{t}.{f}",
                    f"{RETYPED_PREFIX}{h_typ}",
                    f"{path}: type `{t}`: field `{f}` type changed: `{b_typ}` -> `{h_typ}`",
                )
    return errors


def main() -> int:
    base = merge_base()
    base_root = Path(SCHEMAS_DIR)
    if not base_root.exists():
        print(f"warn: {SCHEMAS_DIR} does not exist; skipping CDDL evolution check")
        return 0

    allow = frozenset()
    ratified = Path(RATIFIED_BREAKS_PATH)
    if ratified.exists():
        try:
            allow = parse_ratified_breaks(ratified.read_text(encoding="utf-8"))
        except ValueError as e:
            print(f"error: {RATIFIED_BREAKS_PATH}: {e}", file=sys.stderr)
            return 2

    # The list only grows: a head whose own-gate entries drop one the merge
    # base carried is refused here, independently of whether the dropped
    # name is also revived — closing the two-step attack (delete the entry,
    # then revive the name in the same change) the revival-only check below
    # cannot see on its own.
    base_allow = frozenset()
    base_ratified_text = git_show(base, RATIFIED_BREAKS_PATH)
    if base_ratified_text is not None:
        try:
            base_allow = parse_ratified_breaks(base_ratified_text)
        except ValueError as e:
            print(f"error: {RATIFIED_BREAKS_PATH} at merge base: {e}", file=sys.stderr)
            return 2

    all_errors = []
    for key, transition in sorted(base_allow - allow):
        all_errors.append(
            f"{RATIFIED_BREAKS_PATH}: entry `cddl {key} {transition}` at the merge base "
            "is missing from HEAD — the list only grows"
        )

    for cddl in base_root.glob("*.cddl"):
        # `git show <ref>:<path>` needs a /-separated pathspec — str(cddl) on
        # Windows renders `\`, which `git show` can't resolve, so base_text
        # silently comes back None and a MODIFIED file reads as brand-new
        # (nothing to check) instead of being diffed against its base.
        rel = cddl.as_posix()
        head_text = cddl.read_text()
        base_text = git_show(base, rel)
        all_errors.extend(check_schema_pair(rel, base_text, head_text, allow))

    if all_errors:
        print("Schema-evolution violations:")
        for e in all_errors:
            print(f"  {e}")
        print()
        print("Allowed diffs: add optional field, add variant, add kind.")
        print("Blocked: remove/rename/retype field, tighten optional -> required.")
        print("For breaking changes: introduce new kind alongside old; deprecate old.")
        print(
            f"A user-ratified in-place break is listed in {RATIFIED_BREAKS_PATH}"
            " (version-compatibility.md § Dimension 2) — never by a session's own call."
        )
        return 1
    print("CDDL evolution check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
