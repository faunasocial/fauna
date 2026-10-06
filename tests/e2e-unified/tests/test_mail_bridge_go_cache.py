"""tier_1: the cgo-linked Go bridge test legs run uncached on Linux/macOS.

`_mail-bridge-go-test-legs` (the one home of the legs both the dev recipe
`mail-bridge-test` and the Linux merge-gate check's `mail-bridge-ffi-check`
run) and
the dev recipe `mail-bridge-test-race` each drive `go -C bins/fauna-bridges
test` legs that link `libfauna_ffi.so` at run time through
`CGO_LDFLAGS`/`LD_LIBRARY_PATH`. Go's test cache hashes the Go sources, build flags and CGO_* env — never the
bytes of a library resolved through the dynamic loader at run time — so a
Rust-only change that relinks the `.so` still leaves every cgo-linked package
reading `(cached)`, reporting the PREVIOUS `.so`'s results as current.

Measured 2026-09-15: built
`libfauna_ffi.so`, ran `go -C bins/fauna-bridges test ./...` twice (both
`(cached)`), then lowered `libs/fauna-mail/src/tokenizer.rs`'s minimum token
length from 2 chars to 1 — which flips
`mailfauna_tokenize_test.go::TestTokenizeDeduplicatesAndSorts`'s expected
output — rebuilt the `.so`, and re-ran the same `./...` command with no Go
file touched: `internal/mailfauna` still read `(cached)`, while a `-run
TestTokenizeDeduplicatesAndSorts -v` invocation (a different cache key, so
forced to execute) failed. Same class as `mail-bridge-test-win-cgo`'s
DLL-load finding on Windows (`test_win_cgo_half_runs_uncached`), fixed the same
way: `export GOFLAGS="… -count=1"` around the cgo-linked legs, so the
uncached confinement pin (already `-count=1` on its own flag) and the
cgo-free go-imap leg keep their cache.

Reads the justfile only, so it runs on every machine; the green `go test`
run itself is each recipe's own witness on Linux/macOS.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

_MODULE = "bins/fauna-bridges"


def _recipe_body(name: str) -> str:
    """The indented body of justfile recipe `name` (small helpers are duplicated
    per this suite's no-cross-import-between-test-files convention)."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(1)


def _code_lines(body: str) -> list[str]:
    # A leading `{{slot_build}}` is the line's build slot (nothing compiles
    # outside the count), not part of the command this pin reads.
    return [
        line.strip().removeprefix("{{slot_build}} ") for line in body.splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]


_TEST_PREFIX = f"go -C {_MODULE} test "


def _go_test_line_indices(lines: list[str], *, suffix: str) -> list[int]:
    """Indices of lines that run `go -C bins/fauna-bridges test ...<suffix>` —
    an exact prefix/suffix match rather than a substring search, so the
    vendored go-imap leg (`go -C bins/fauna-bridges/third_party/go-imap
    test ...`, a different module under a path that merely starts with the
    same prefix) and the confinement/tagged legs (different argv shapes)
    never cross-match."""
    return [i for i, l in enumerate(lines) if l.startswith(_TEST_PREFIX) and l.endswith(suffix)]


def _goflags_export_index(lines: list[str]) -> int | None:
    return next(
        (i for i, l in enumerate(lines) if l.startswith("export GOFLAGS=") and "-count=1" in l),
        None,
    )


def _goflags_unset_index(lines: list[str]) -> int | None:
    return next((i for i, l in enumerate(lines) if l == "unset GOFLAGS"), None)


@pytest.mark.parametrize(
    ("recipe", "suffix"),
    [
        ("_mail-bridge-go-test-legs", "./..."),
        ("_mail-bridge-go-test-legs", "./cmd/fauna-atproto-bridge/"),
        ("mail-bridge-test-race", "./..."),
    ],
)
def test_cgo_linked_go_test_leg_runs_uncached(recipe, suffix):
    lines = _code_lines(_recipe_body(recipe))
    targets = _go_test_line_indices(lines, suffix=suffix)
    assert targets, (
        f"{recipe} no longer runs a `{_TEST_PREFIX}…{suffix}` leg — this "
        "pin's matcher is stale, re-aim it rather than deleting it"
    )
    export_at = _goflags_export_index(lines)
    unset_at = _goflags_unset_index(lines)
    assert export_at is not None, (
        f"{recipe} must `export GOFLAGS=\"… -count=1\"` before its cgo-linked test "
        "leg(s) — libfauna_ffi.so loads through LD_LIBRARY_PATH at run time, which "
        "Go's test cache does not hash, so without it a Rust-only change that "
        "relinks the .so still reports the PREVIOUS .so's cached results as this "
        "tree's (measured 2026-09-15 on Linux)"
    )
    for i in targets:
        assert export_at < i, (
            f"{recipe}: line {lines[i]!r} runs before GOFLAGS=-count=1 is exported"
        )
        assert unset_at is None or i < unset_at, (
            f"{recipe}: line {lines[i]!r} runs after `unset GOFLAGS` — it would "
            "read cached results again"
        )


def test_go_imap_and_confinement_legs_keep_their_cache():
    """The cgo-free legs load no shared library, so uncaching them buys
    nothing — they should sit OUTSIDE the exported GOFLAGS scope. Confinement
    is the one exception: it already runs `-count=1` for an unrelated reason
    (a cross-language pin reading a Rust file outside the Go module), so it is
    excluded here rather than asserted cache-preserving."""
    recipe = "_mail-bridge-go-test-legs"
    lines = _code_lines(_recipe_body(recipe))
    export_at = _goflags_export_index(lines)
    unset_at = _goflags_unset_index(lines)
    go_imap = [
        i for i, l in enumerate(lines)
        if l.startswith("go ") and "third_party/go-imap" in l
    ]
    assert go_imap, f"{recipe} no longer runs the vendored go-imap module's tests"
    for i in go_imap:
        in_scope = export_at is not None and export_at < i and (unset_at is None or i < unset_at)
        assert not in_scope, (
            f"{recipe}: the go-imap leg (line {lines[i]!r}) links no cgo "
            "library — it should keep its cache, not sit inside the "
            "GOFLAGS=-count=1 scope meant for the fauna_ffi.so legs"
        )
