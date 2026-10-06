"""What the Go bridge tests need from OUTSIDE their own Go module.

Two classes, one derivation discipline: repo FILES they read
(`reads_outside_the_module`) and external EXECUTABLES they refuse to run
without (`hard_required_tools`). Both are inputs no `go -C <dir>` leg names,
and both go stale the moment a gate keeps its own copy of the list.

`bins/fauna-bridges` tests pin Go against the Rust side by reading Rust
sources and shared fixtures directly: the confinement pins read
`bins/fauna-sandbox/src/main.rs`, the dagcbor and DAV/IMAP tests read
fixtures under `libs/`. Such a file is a test INPUT that no `go -C <dir>` leg
names, so a gate's path-scope derived from its legs alone misses it: a change
there fires no Go gate, and the drift the pin exists to catch lands unrun.
Both Go merge-gate checks' path-scope pins (Linux and Windows) add this set to
what their scope must cover. It is derived from the test
sources, never hand-listed.

Three spellings are recognized — the three the tree uses today:

- a relative string literal climbing out of the package dir
  (`"../../../../libs/fauna-cbor/tests/fixtures/cross-language"`);
- `filepath.Join("..", …, "libs", "fauna-protocol", …)`, all-literal from a
  `..` head up to the first non-literal argument;
- `filepath.Join(root, "bins", "fauna-sandbox", …)`, where
  `root := filepath.Clean(filepath.Join(filepath.Dir(thisFile), "..", …))`
  names the climb.

A fourth spelling would go unseen, which is why each caller also asserts the
confinement pin's own input is found: if a rewrite of those tests moves it
out of these shapes, the pin reds and gets re-aimed rather than going blind.
The executables are derived the same way, from the `exec.LookPath` guards
in those same test sources: a guard that calls `t.Fatal*` names a hard
prerequisite a merge-gate check's toolchain preflight must check for, and
one that calls `t.Skip*` does not.

`third_party/` is skipped: it holds the vendored go-imap fork, a separate
module whose source this suite never reads.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
MODULE = "bins/fauna-bridges"

# The input the confinement pins read. Callers assert it is found.
SANDBOX_MAIN = "bins/fauna-sandbox/src/main.rs"

_ROOT_CLIMB = re.compile(
    r"(\w+)\s*:?=\s*filepath\.Clean\(filepath\.Join\(filepath\.Dir\(\w+\),((?:\s*\"\.\.\",?)+)\)\)"
)
_JOIN = re.compile(r"filepath\.Join\(\s*(\w+|\"[^\"]*\")((?:\s*,\s*\"[^\"]*\")*)")
_RELATIVE = re.compile(r"\"((?:\.\./)+[^\"]*)\"")


def reads_outside_the_module() -> set[str]:
    """Repo-relative paths (file or dir) a `_test.go` under the module reads
    from outside it."""
    found: set[str] = set()
    for dirpath, dirs, files in os.walk(REPO / MODULE):
        dirs[:] = sorted(d for d in dirs if d != "third_party")
        here = Path(dirpath).relative_to(REPO).as_posix()
        for name in files:
            if not name.endswith("_test.go"):
                continue
            src = (Path(dirpath) / name).read_text(encoding="utf-8")
            roots = {
                m.group(1): here + "/.." * m.group(2).count('".."')
                for m in _ROOT_CLIMB.finditer(src)
            }
            candidates = [f"{here}/{m.group(1)}" for m in _RELATIVE.finditer(src)]
            for m in _JOIN.finditer(src):
                head, rest = m.group(1), re.findall(r"\"([^\"]*)\"", m.group(2))
                if head.startswith('"'):
                    candidates.append("/".join([here, head.strip('"'), *rest]))
                elif head in roots:
                    candidates.append("/".join([roots[head], *rest]))
            for candidate in candidates:
                path = os.path.normpath(candidate).replace(os.sep, "/")
                if path.startswith("..") or path == MODULE or path.startswith(MODULE + "/"):
                    continue
                found.add(path)
    return found


# ── External executables, the second class of outside-the-module input ───────

# How many lines after the `exec.LookPath` line the guard's verdict is looked
# for. The shape in the tree is a three-line `if _, err := …; err != nil { …
# t.Fatalf(…) }`; the window leaves room for the comment the ffmpeg guard
# carries between the two.
_GUARD_WINDOW = 8

_LOOKPATH = re.compile(r'exec\.LookPath\(\s*"([^"]+)"\s*\)')


def hard_required_tools() -> set[str]:
    """Executables a `_test.go` under the module refuses to run WITHOUT — the
    ones whose `exec.LookPath` failure branch calls `t.Fatal*` rather than
    `t.Skip*`.

    A hard requirement is deliberate (`internal/atprotorepo`'s video tests:
    "ffmpeg ships in the runtime image and the projection cannot publish video
    without it, so its absence is a broken environment, not a reason to report
    success"), and it is exactly what a merge-gate check's toolchain preflight
    must name. A box missing one of these tools compiles and tests nothing
    useful, so its gate has to publish INFRA — never a code red naming the
    range's innocent commits. Derived rather than hand-listed for the reason
    `reads_outside_the_module` is: a preflight holding its own copy of the list
    goes quietly stale the day a test starts requiring a fourth tool, and the
    first symptom is that code red.

    A `t.Skip` branch means the tool is optional, so it is NOT returned: a box
    without it still runs the rest of the package honestly.
    """
    found: set[str] = set()
    for dirpath, dirs, files in os.walk(REPO / MODULE):
        dirs[:] = sorted(d for d in dirs if d != "third_party")
        for name in files:
            if not name.endswith("_test.go"):
                continue
            lines = (Path(dirpath) / name).read_text(encoding="utf-8").splitlines()
            for i, line in enumerate(lines):
                m = _LOOKPATH.search(line)
                if not m:
                    continue
                window = "\n".join(lines[i : i + _GUARD_WINDOW])
                if re.search(r"\bt\.Skip", window):
                    continue
                if re.search(r"\bt\.Fatal", window):
                    found.add(m.group(1))
    return found
