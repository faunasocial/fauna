#!/usr/bin/env python3
"""Read `tests/e2e-unified/ui.yaml` one page, component or element at a time.

The UI spec is the canonical description of what all seven apps must render,
and the project's authority table names it beside `docs/goal/` as the answer to
*how should things be*. At 867,925 bytes it is **3.31x the 262,144-byte
Read-tool ceiling**, so no session can open it — which made the UI rules
("match ui.yaml exactly", "ask if ui.yaml looks inconsistent") unfollowable for
as long as the file has been this size.

A file answers the ceiling either by getting smaller **or** by growing a reader
that takes one row at a time. This is the second route, for two reasons:

  * splitting the YAML needs a merging loader at all six independent parse
    sites (`safe_load` has no `!include`), and the arithmetic does not even
    work: `elements:` alone is 1.57x the ceiling, so a split yields one
    readable file and one that needs splitting again;
  * a reader changes nothing downstream. Every existing loader keeps reading
    one file, and the spec becomes readable again.

Two properties carry the design, pinned in `tests/scripts/test_ui_show.py`:

  1. **Verbatim.** A slice is the file's own bytes, addressed by line range --
     never a re-serialization. Rule A is "match ui.yaml exactly", so a reader
     that round-tripped through a YAML dumper would drop the comments (which
     carry load-bearing scope notes), re-quote strings and re-wrap lines, and
     hand back something the spec does not say. It also means every line this
     prints can be cited as `ui.yaml:NNN`.

  2. **Total reachability.** Every addressable node either fits the output cap
     or renders as an index of its children, each with an address that
     resolves. A reader that merely moved the truncation would be no reader.

Usage:
    just ui-show                       # the table of contents
    just ui-show feed                  # a bare key, resolved across sections
    just ui-show pages.feed            # fully qualified
    just ui-show elements.error-message
    just ui-show pages.settings        # over the cap -> its child index
    just ui-show pages --children      # force the index instead of the body
    just ui-show --find recipient      # every key whose name matches
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UI_YAML = ROOT / "tests" / "e2e-unified" / "ui.yaml"

# The Read/Bash tool output cap is ~30,000 characters. Stay under it with room
# for the header line this script adds, so a rendered node is never the thing
# that gets truncated.
OUTPUT_CAP = 28_000

CEILING = 262_144

# Sections whose members are the natural addressing unit. Everything else is a
# short section rendered whole.
_TOP = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*):")
_CHILD = re.compile(r"^  ([A-Za-z_][A-Za-z0-9_.-]*):")


class NotFound(Exception):
    pass


class Ambiguous(Exception):
    def __init__(self, key, candidates):
        self.candidates = candidates
        super().__init__(
            f"{key!r} is ambiguous — {len(candidates)} matches: "
            + ", ".join(candidates[:12])
            + ("..." if len(candidates) > 12 else "")
        )


class Node:
    """One addressable region of the spec, as a line range into the file."""

    def __init__(self, address, start, end, lines):
        self.address = address
        self.start = start  # 1-based, inclusive
        self.end = end  # 1-based, inclusive
        self._lines = lines

    def text(self):
        return "".join(self._lines[self.start - 1 : self.end])

    def size(self):
        return len(self.text().encode("utf-8"))

    def __repr__(self):
        return f"<Node {self.address} {self.start}-{self.end}>"


def _lines():
    if not hasattr(_lines, "_cache"):
        _lines._cache = UI_YAML.read_text(encoding="utf-8").splitlines(keepends=True)
    return _lines._cache


def _blank_or_comment(line):
    s = line.strip()
    return not s or s.startswith("#")


def _extent(lines, start_idx, pattern):
    """End of the block opened at `start_idx`, by indentation.

    Trailing blank and comment lines belong to whatever comes next, not to the
    node that happens to precede them -- otherwise a node's slice ends with its
    successor's header comment.
    """
    i = start_idx + 1
    last = start_idx
    while i < len(lines):
        line = lines[i]
        if _blank_or_comment(line):
            i += 1
            continue
        if pattern.match(line) or _TOP.match(line):
            break
        last = i
        i += 1
    return last + 1  # 1-based inclusive


def sections():
    """The spec's top-level section names, in file order."""
    if not hasattr(sections, "_cache"):
        lines = _lines()
        found = []
        for i, line in enumerate(lines):
            m = _TOP.match(line)
            if m:
                found.append(m.group(1))
        sections._cache = found
    return sections._cache


def _index():
    """{section: {key: Node}} plus a Node per section, built by one scan."""
    if hasattr(_index, "_cache"):
        return _index._cache
    lines = _lines()
    by_section = {}
    section_nodes = {}
    current = None
    starts = []  # (section, key, idx)
    for i, line in enumerate(lines):
        top = _TOP.match(line)
        if top:
            current = top.group(1)
            by_section.setdefault(current, {})
            section_nodes[current] = i
            continue
        if current is None:
            continue
        child = _CHILD.match(line)
        if child:
            starts.append((current, child.group(1), i))
    for section, key, idx in starts:
        end = _extent(lines, idx, _CHILD)
        by_section[section][key] = Node(f"{section}.{key}", idx + 1, end, lines)
    ordered = sections()
    for n, section in enumerate(ordered):
        start = section_nodes[section]
        end = (section_nodes[ordered[n + 1]] - 1) if n + 1 < len(ordered) else len(lines)
        while end > start and _blank_or_comment(lines[end - 1]):
            end -= 1
        section_nodes[section] = Node(section, start + 1, end, lines)
    _index._cache = (by_section, section_nodes)
    return _index._cache


def resolve(address):
    """Address -> Node.

    A bare name that IS a section resolves to that section outright: sections
    are the top of the namespace, and both `pages` and `elements` also occur as
    member keys further down (`global.elements`), which would otherwise make
    the two largest sections unaddressable. Any other bare name must be unique
    across sections -- 117 keys are not, so the collision is reported with its
    candidates rather than guessed at. Qualified addresses nest arbitrarily
    deep, which is what makes a child index's own addresses resolve.
    """
    by_section, section_nodes = _index()
    if "." not in address:
        if address in section_nodes:
            return section_nodes[address]
        hits = [f"{s}.{address}" for s in sections() if address in by_section[s]]
        if not hits:
            raise NotFound(f"no {address!r} — try `just ui-show --find {address}`")
        if len(hits) > 1:
            raise Ambiguous(address, hits)
        return resolve(hits[0])

    section, _, rest = address.partition(".")
    if section not in by_section:
        raise NotFound(f"no section {section!r}; sections are: " + ", ".join(sections()))
    head, _, tail = rest.partition(".")
    node = by_section[section].get(head)
    if node is None:
        raise NotFound(f"no {section}.{head!r} — try `just ui-show --find {head}`")
    while tail:
        head, _, tail = tail.partition(".")
        kids = {k.address.rsplit(".", 1)[1]: k for k in children(node.address)}
        if head not in kids:
            known = ", ".join(sorted(kids)) or "none"
            raise NotFound(f"no {node.address}.{head!r} — children are: {known}")
        node = kids[head]
    return node


def children(address):
    """The addressable children of any node, at whatever depth it sits.

    The child indent is the node's own plus two, so this works the same for a
    section member and for a child of one -- which is what lets a child index
    print addresses that resolve.
    """
    by_section, _ = _index()
    if address in by_section:
        return list(by_section[address].values())
    node = resolve(address)
    lines = _lines()
    own = len(lines[node.start - 1]) - len(lines[node.start - 1].lstrip())
    pattern = re.compile(r"^ {%d}([A-Za-z_][A-Za-z0-9_.-]*):" % (own + 2))
    out = []
    for i in range(node.start, node.end):
        m = pattern.match(lines[i])
        if m:
            end = min(_extent(lines, i, pattern), node.end)
            out.append(Node(f"{node.address}.{m.group(1)}", i + 1, end, lines))
    return out


def find(needle):
    """Every addressable node whose key contains `needle` (case-insensitive)."""
    by_section, section_nodes = _index()
    needle = needle.lower()
    hits = [n for s in sections() for k, n in by_section[s].items() if needle in k.lower()]
    hits += [n for s, n in section_nodes.items() if needle in s.lower()]
    return sorted(hits, key=lambda n: n.start)


def _index_body(nodes, note):
    rows = [f"  {n.address:<52} {n.start:>6}-{n.end:<6} {n.size():>8,} B" for n in nodes]
    body = "\n".join(rows)
    if len(body) > OUTPUT_CAP - 600:
        keep = []
        used = 0
        for row in rows:
            if used + len(row) > OUTPUT_CAP - 900:
                break
            keep.append(row)
            used += len(row) + 1
        body = "\n".join(keep) + (
            f"\n  ... {len(rows) - len(keep)} more of {len(rows)} not shown — "
            "narrow with `just ui-show --find <substring>`"
        )
    return f"{note}\n\n{body}\n"


def render(node):
    """A node's verbatim text, or its child index when that would overflow."""
    header = f"# {UI_YAML.relative_to(ROOT).as_posix()}:{node.start}-{node.end}  ({node.size():,} B)"
    if node.size() <= OUTPUT_CAP - 400:
        return f"{header}\n\n{node.text()}"
    kids = children(node.address)
    if not kids:
        return (
            f"{header}\n\n{node.address} is {node.size():,} B, over the {OUTPUT_CAP:,}-char "
            "output cap, and has no addressable children.\n"
            f"Read it directly with an offset: ui.yaml lines {node.start}-{node.end}.\n"
        )
    note = (
        f"{header}\n\n"
        f"{node.address} is {node.size():,} B — over the {OUTPUT_CAP:,}-char output cap, so "
        f"here are its {len(kids)} children instead.\nAddress any of them directly, "
        "e.g. `just ui-show " + kids[0].address + "`."
    )
    return _index_body(kids, note)


def render_find(needle):
    hits = find(needle)
    if not hits:
        return f"no key contains {needle!r}.\n"
    return _index_body(hits, f"{len(hits)} key(s) containing {needle!r}:")


def render_toc():
    by_section, section_nodes = _index()
    total = UI_YAML.stat().st_size
    out = [
        f"{UI_YAML.relative_to(ROOT).as_posix()} — {total:,} B, "
        f"{total / CEILING:.2f}x the {CEILING:,} B Read-tool ceiling.",
        "No session can open it whole; address one node at a time.",
        "",
        f"{'section':<24} {'members':>8} {'lines':>14} {'bytes':>11}",
    ]
    for section in sections():
        node = section_nodes[section]
        out.append(
            f"{section:<24} {len(by_section[section]):>8} "
            f"{str(node.start) + '-' + str(node.end):>14} {node.size():>9,} B"
        )
    out += [
        "",
        "  just ui-show <section>            # the section's members, or its text if small",
        "  just ui-show pages.feed           # one page, verbatim, with its line range",
        "  just ui-show feed                 # a bare key, when it is unambiguous",
        "  just ui-show --find recipient     # every key whose name matches",
    ]
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("address", nargs="?", help="section, section.key, or a unique bare key")
    ap.add_argument("--find", metavar="SUBSTR", help="list every key whose name matches")
    ap.add_argument(
        "--children", action="store_true", help="print the child index, not the body"
    )
    args = ap.parse_args()

    if args.find:
        sys.stdout.write(render_find(args.find))
        return 0
    if not args.address:
        sys.stdout.write(render_toc())
        return 0
    try:
        node = resolve(args.address)
    except (NotFound, Ambiguous) as e:
        print(f"ui-show: {e}", file=sys.stderr)
        return 1
    if args.children:
        kids = children(node.address)
        if not kids:
            print(f"ui-show: {node.address} has no addressable children", file=sys.stderr)
            return 1
        sys.stdout.write(_index_body(kids, f"{node.address} — {len(kids)} children:"))
        return 0
    sys.stdout.write(render(node))
    return 0


if __name__ == "__main__":
    sys.exit(main())
