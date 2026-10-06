"""tier_1: the Go bridge test suite's reach on Windows — two recipes, one split.

`just mail-bridge-test` runs on Linux and macOS only: it links the host
`fauna_ffi` cdylib through rpath/`LD_LIBRARY_PATH`, and Windows has neither. On win the same coverage
is two recipes that partition `bins/fauna-bridges` along ONE edge — whether a
package's test binary links `fauna-mail-go` (hence cgo + `fauna_ffi`):

- `mail-bridge-test-portable` — the cgo-free half: no `fauna-ffi` build, a plain
  recipe, so it also runs from a bare PowerShell prompt;
- `mail-bridge-test-win-cgo` — the cgo-linked half, against the gnullvm
  `fauna_ffi.dll` that `_windows-go-cgo-env` builds (Go's cgo cannot link the
  MSVC one; `_windows-go-cgo-build`'s header says why).

Three ways that split rots silently, each pinned below:

1. the classifier forks — two copies of the `go list -deps` filter drift apart
   and a package lands in neither half (or in both);
2. the cgo half LINKS but its test exes cannot LOAD — `go test` runs each exe
   from a temp `$WORK` dir, so Windows finds `fauna_ffi.dll` only through PATH;
3. a leg later added to `mail-bridge-test` (the tagged e2e-flavor run, the
   uncached Rust-wrapper pins, the vendored go-imap module) gets no win home —
   read from `_mail-bridge-go-test-legs`, the one home of those legs, which
   `mail-bridge-test` and the Linux merge-gate check both run.

Reads the justfile only, so it runs on every machine; the green `go test` run
itself is the recipe's own witness on Windows.
"""

from __future__ import annotations

import re
import shlex
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

_SUBSET = "_go-bridge-test-subset"
_PORTABLE = "mail-bridge-test-portable"
_WIN_CGO = "mail-bridge-test-win-cgo"
_LEGS = "_mail-bridge-go-test-legs"
_MODULE = "bins/fauna-bridges"


def _recipe_body(name: str) -> str:
    """The indented body of justfile recipe `name` (small helpers are duplicated
    per this suite's no-cross-import-between-test-files convention)."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(1)


def _code_lines(body: str) -> list[str]:
    return [
        line.strip() for line in body.splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]


def _subset_calls(body: str) -> list[tuple[str, str, frozenset[str]]]:
    """Every `just _go-bridge-test-subset <subset> <tags> <patterns...>` call."""
    calls = []
    for line in _code_lines(body):
        argv = shlex.split(line)
        if argv[:2] == ["just", _SUBSET]:
            subset, tags, *patterns = argv[2:]
            calls.append((subset, tags, frozenset(patterns)))
    return calls


def test_both_win_halves_classify_through_one_helper():
    helper = _recipe_body(_SUBSET)
    assert "fauna-mail-go" in helper and "-test -deps" in helper, (
        f"{_SUBSET} must classify on the `fauna-mail-go` edge of each package's "
        "TEST binary (`go list -test -deps`): a `_test.go` importing the cgo tree "
        "links fauna_ffi exactly as a production import does"
    )
    assert not _code_lines(helper)[0].startswith("#!"), (
        f"{_SUBSET} must stay a plain (non-shebang) recipe — {_PORTABLE} calls it "
        "and keeps its bare-PowerShell reach only while every recipe it runs does "
        "(a shebang recipe needs Git Bash's `cygpath` to resolve its interpreter)"
    )
    for recipe, subset in ((_PORTABLE, "portable"), (_WIN_CGO, "cgo")):
        body = _recipe_body(recipe)
        assert "fauna-mail-go" not in body and "list -deps" not in body, (
            f"{recipe} carries its own copy of the package classifier — the two "
            f"halves must derive through {_SUBSET} or they drift apart"
        )
        subsets = {call[0] for call in _subset_calls(body)}
        assert subsets == {subset}, (
            f"{recipe} must run only the {subset!r} half through {_SUBSET}, "
            f"found {sorted(subsets) or 'no call'}"
        )


def test_win_cgo_test_exes_find_the_gnullvm_dll_on_path():
    lines = _code_lines(_recipe_body(_WIN_CGO))
    env = next((i for i, l in enumerate(lines) if "just _windows-go-cgo-env" in l), None)
    path = next(
        (i for i, l in enumerate(lines) if l == 'export PATH="$(cygpath -u "$REL"):$PATH"'),
        None,
    )
    first_test = next((i for i, l in enumerate(lines) if f"just {_SUBSET}" in l), None)
    assert env is not None, (
        f"{_WIN_CGO} must take its cargo step and cgo env from _windows-go-cgo-env, "
        "the one home of the gnullvm fauna-ffi feature set"
    )
    assert path is not None and first_test is not None and env < path < first_test, (
        f'{_WIN_CGO} must `export PATH="$(cygpath -u "$REL"):$PATH"` after sourcing '
        "the cgo env and before running any test: go test executes each test exe "
        "from a temp dir, so without it fauna_ffi.dll does not load "
        "(0xc0000135, STATUS_DLL_NOT_FOUND) even though the link succeeded. The "
        "cygpath is load-bearing too: REL is the mixed `D:/…` form, and a bare "
        "`$REL:` splits at the drive colon into two useless entries (measured "
        "2026-09-14)"
    )


def test_win_cgo_half_runs_uncached():
    """Every cgo test exe loads fauna_ffi.dll through PATH at run time, which Go's
    test cache does not hash: a Rust-only change rebuilds the DLL yet leaves each
    test's cached result standing, so a cached `go test` reports the PREVIOUS DLL's
    results as current. Measured 2026-09-14 — every cgo package read `(cached)`
    straight after a 7m51s gnullvm `fauna-ffi` rebuild. The win merge-gate check
    runs this recipe (gate 9), so a cached half there is a verdict about a DLL the
    tip no longer builds. The portable half loads no DLL; its cache stays sound."""
    lines = _code_lines(_recipe_body(_WIN_CGO))
    flags = next(
        (i for i, l in enumerate(lines) if l.startswith("export GOFLAGS=") and "-count=1" in l),
        None,
    )
    first_test = next((i for i, l in enumerate(lines) if f"just {_SUBSET}" in l), None)
    assert flags is not None and first_test is not None and flags < first_test, (
        f"{_WIN_CGO} must `export GOFLAGS=\"… -count=1\"` before its first test run — "
        "without it a DLL-only change is tested against cached results"
    )


def test_every_mail_bridge_test_leg_has_a_win_home():
    # The legs `mail-bridge-test` runs live in `_LEGS`, which the Linux merge-gate
    # check runs too.
    assert re.search(rf"^\s*just {re.escape(_LEGS)}\s*$", _recipe_body("mail-bridge-test"), re.M), (
        f"mail-bridge-test no longer runs `just {_LEGS}` — this pin reads the legs "
        "from the wrong recipe; re-aim it"
    )
    classified: set[tuple[str, frozenset[str]]] = set()
    verbatim: list[str] = []
    for line in _code_lines(_recipe_body(_LEGS)):
        argv = shlex.split(line)
        if argv[:1] != ["go"] or "test" not in argv:
            continue
        rest = argv[argv.index("test") + 1:]
        tags = ""
        if rest[:1] == ["-tags"]:
            tags, rest = rest[1], rest[2:]
        if argv[1:3] == ["-C", _MODULE] and rest and all(a.startswith("./") for a in rest):
            classified.add((tags, frozenset(rest)))
        else:
            verbatim.append(line)
    assert ("", frozenset({"./..."})) in classified, (
        f"{_LEGS} no longer runs `go test ./...` — this pin's parser is stale"
    )

    for recipe, subset in ((_PORTABLE, "portable"), (_WIN_CGO, "cgo")):
        calls = {(tags, patterns) for s, tags, patterns in _subset_calls(_recipe_body(recipe)) if s == subset}
        missing = sorted((t, sorted(p)) for t, p in classified - calls)
        assert not missing, (
            f"{recipe} has no {subset!r}-half counterpart for {_LEGS}'s "
            f"leg(s) (tags, packages) {missing} — on Windows that coverage is unrun"
        )

    portable = _code_lines(_recipe_body(_PORTABLE))
    for line in verbatim:
        assert line in portable, (
            f"{_LEGS}'s cgo-free leg `{line}` must be mirrored verbatim in "
            f"{_PORTABLE}, its only win home"
        )
