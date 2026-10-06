"""mac's host-side recipes must build into the SHARED host tree, not a private
`--target aarch64-apple-darwin` one.

macOS is the one machine whose cross-compile set contains its own triple, and
asking for it explicitly is not free: cargo hard-separates explicit-target units
from implicit-host ones **even when the triple is identical**, so the flag buys a
second artifact tree and nothing else. Measured 2026-08-22 with `cargo build -Z
unstable-options --unit-graph`, against `cargo build -p fauna-nest`'s graph:

    explicit --target aarch64-apple-darwin :  238/1042 units,  0/95 workspace-local
    implicit host                          :  818/1000 units, 55/95 workspace-local

Zero workspace-local units shared means every `libs/fauna-*` crate was compiled a
second time per mac checkout — and since per-checkout *divergence* is what caps
how many sessions fit on that box, and a `libs/` edit dirties exactly those
units, the second copy was paid on every shared-Rust edit.

The mechanism has two halves and this file pins both, because half of it is a
foot-gun: joining the shared tree puts the artifact in a `target/<profile>/` slot
OTHER builds also write (`mail-bridge-ffi`'s release `libfauna_ffi.{a,dylib}`
comes from `--no-default-features --features labeler`; a workspace-wide test
build writes default features). Sharing the *build* is the win; sharing the
*path* is the bluesky "whichever built last owns the file" trap. So each recipe
that dropped the triple must route its own artifact through `--artifact-dir`.

Structural/text analysis only — the real-build verification is `just
apple-ffi-host` + `just mac-debug` on a macOS box, which no other dev machine can
run (this file's assertions are therefore machine-independent by construction and
run everywhere, the same reasoning as `test_apple_ffi_host_warm_takes_no_slot`).
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

_HOST_TRIPLE_FLAG = "--target aarch64-apple-darwin"


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name arg *ARGS:` declaration alike. Duplicated from
    `test_apple_ffi_host_warm_takes_no_slot.py` per this suite's
    no-cross-import-between-test-files convention (small helpers get duplicated,
    not imported).

    A body whose LAST non-blank line is a bare delegation call — `[{{slot_build}}]
    just _<name>`, nothing else on that line — resolves through it: the
    slot-lock lease split moved `mac-app`'s own content into
    `_mac-app-impl`, so reading only the wrapper's literal text would see none
    of it. Any preceding lines are kept; the delegate's body is appended after
    them."""
    lines = text.splitlines()
    header = re.compile(rf"^{re.escape(name)}(\s+\S+)*:")
    for i, ln in enumerate(lines):
        if header.match(ln):
            body = []
            for bl in lines[i + 1:]:
                if bl and not bl[:1].isspace():
                    break
                body.append(bl)
            code_lines = [bl for bl in body if bl.strip()]
            if code_lines:
                delegate = re.match(
                    r"^\s*(?:\{\{slot_build\}\}|\{\{slot_e2e\}\})?\s*just\s+(_\S+)\s*$",
                    code_lines[-1],
                )
                if delegate:
                    prefix = "\n".join(code_lines[:-1])
                    return (prefix + "\n" if prefix else "") + _recipe_body(
                        text, delegate.group(1)
                    )
            return "\n".join(body)
    return ""


def _code(body: str) -> str:
    """Recipe body with comment lines dropped and whitespace collapsed — the
    comments *explain* the removed flag at length, so a naive substring search
    over the raw body can never go red."""
    code_lines = [ln for ln in body.splitlines() if not ln.lstrip().startswith("#")]
    return " ".join(" ".join(code_lines).split())


def _justfile() -> str:
    return _JUSTFILE.read_text(encoding="utf-8")


# ── the two host-side recipes that joined the shared tree ────────────────────

def _assert_joins_the_shared_host_tree(text: str, recipe: str, package: str) -> None:
    """`recipe` must build `package` implicit-host and isolate its artifact."""
    body = _recipe_body(text, recipe)
    assert body, f"{recipe} recipe not found"
    code = _code(body)
    assert f"cargo build" in code and package in code, (
        f"{recipe} is expected to build {package}"
    )
    assert _HOST_TRIPLE_FLAG not in code, (
        f"{recipe} must build IMPLICIT-host: `{_HOST_TRIPLE_FLAG}` names the "
        f"machine's own triple, so it produces identical code in a SEPARATE "
        f"artifact tree — measured at 0/95 workspace-local units shared with "
        f"the dev inner loop, i.e. every libs/fauna-* crate compiled twice per "
        f"mac checkout."
    )
    assert "--artifact-dir" in code, (
        f"{recipe} builds into the shared host tree, so its own artifact must "
        f"go to a private --artifact-dir: the plain `target/<profile>/` slot is "
        f"also written by other feature sets (mail-bridge-ffi's labeler build, "
        f"any workspace-wide test build), and whichever builds last would own "
        f"the file."
    )


def test_apple_ffi_host_flavor_builds_implicit_host():
    _assert_joins_the_shared_host_tree(
        _justfile(), "_apple-ffi-host-flavor", "-p fauna-ffi"
    )


def test_mac_app_sync_agent_builds_implicit_host():
    """`mac-app` bundles a `fauna-sync-agent` the tui/e2e prebuild also builds
    (`cargo build -p fauna-tui -p fauna-sync-agent`). Measured 2026-08-22:
    26.4% → 89.3% of units reused, 0/45 → 30/45 of them workspace-local."""
    _assert_joins_the_shared_host_tree(
        _justfile(), "mac-app", "-p fauna-sync-agent"
    )


def test_the_artifact_dirs_are_not_the_shared_profile_dir():
    """The isolation must be a real subdirectory. `--artifact-dir target/debug`
    would satisfy the flag check above while reintroducing exactly the collision
    it exists to prevent."""
    text = _justfile()
    for recipe in ("_apple-ffi-host-flavor", "mac-app"):
        code = _code(_recipe_body(text, recipe))
        for m in re.finditer(r'--artifact-dir "?([^" ]+)"?', code):
            path = m.group(1)
            assert not re.fullmatch(
                r'"?\$?\{?\w*TARGET_DIR\}?/?\$?\{?\w*(CONFIG|PROFILE_DIR)\}?"?',
                path,
            ), (
                f"{recipe}: --artifact-dir {path} IS the shared profile dir — "
                f"the artifact needs its own subdirectory under it"
            )


# ── the deliberate carve-out ─────────────────────────────────────────────────

def test_the_five_slice_recipe_keeps_its_explicit_darwin_slice():
    """`_apple-ffi-flavor` (the full 5-slice release xcframework) is
    deliberately NOT converted, and that is a decision rather than an oversight:
    its profile is release and its only same-profile neighbours build different
    feature sets, so it would trade real transition risk (the cross-checkout
    cache, the 5-slice assembly) for little sharing.

    This asserts the carve-out still exists so a future session reading the
    implicit-host rule does not "finish the job" without re-measuring — and so
    that, if it ever IS converted, this pin is the place that says why it was
    left alone. Deleting it then is correct; deleting it silently is not."""
    code = _code(_recipe_body(_justfile(), "_apple-ffi-flavor"))
    assert _HOST_TRIPLE_FLAG in code, (
        "the 5-slice apple-ffi recipe no longer builds an explicit darwin "
        "slice — if that was deliberate, re-measure the unit sharing and "
        "update the host-tree layout notes, "
        "then drop this pin in the same commit"
    )
