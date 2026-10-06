"""A per-root `cargo tree` query must opt out of workspace feature unification.

`.cargo/config.toml` § feature unification turns the dev inner loop's resolve
into "as if the whole workspace were selected", and states guard 2 in prose:
every explicit-`--target` build, artifact/flavor recipe and feature-matrix gate
sets `CARGO_RESOLVER_FEATURE_UNIFICATION=selected` to revert that one
invocation. The justfile, the Dockerfile and the feature-matrix scripts carried
it from day one. **Nothing enforced it**, and the class it misses is the one
that fails silently: a *resolve-only* query — `cargo tree` — which compiles
nothing, so nobody notices it has started answering a different question.

Measured on 2026-08-22, the day `[resolver] feature-unification = "workspace"`
landed, all four in a single afternoon:

  * the protocol transport-agnosticism CHECK-tier merge gate died on
    `xrealloc: cannot allocate 18446744071562067968 bytes` — the unified
    resolve times `--no-dedupe`.
  * the MLS production-features gate (standing between the
    `verify()`-bypassing `SealedRecordBytes` constructor and the shipped nest
    image) hit a cargo ICE, `missing dep graph connection for CLI feature`,
    behind a `2>/dev/null`.
  * the supply-chain surface `release-integrity.md` § Surface today cites
    over-counted `fauna-nest`'s shipped closure 5015 lines against 2495.
  * `test_no_desktop_keyring_on_phone_targets.py` reported `secret-service`
    present in the iOS graph — via `fauna-linux`, a desktop app no phone ever
    builds — and went ten-of-twelve red on a graph no artifact has.

That gate's own header names the shape: "it rested on an unenforced build-config
invariant". So does this one, until here. A `--workspace` query is exempt by
construction — unification is exactly the question it asks.

tier_1: reads source files, runs no cargo.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]

_OVERRIDE = "CARGO_RESOLVER_FEATURE_UNIFICATION"

#: Shell: a non-comment line running `cargo tree`.
_SHELL_CALL = re.compile(r"\bcargo\s+tree\b")
#: Python: the list form, `["cargo", "tree", ...]`, however it is spaced.
_PY_CALL = re.compile(r"""["']cargo["']\s*,\s*["']tree["']""")


def _scanned_files() -> list[Path]:
    """Every script and test that could run a cargo query.

    Deliberately a WALK rather than a hand-listed set: the four sites measured
    above were four different authors in four different directories, and a
    hand-listed set is the second copy that goes stale the first time someone
    adds a fifth.
    """
    roots = [_REPO / "scripts", _REPO / "tests" / "e2e-unified", _REPO / "tools"]
    out: list[Path] = []
    for root in roots:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*")):
            if path.is_file() and path.suffix in (".py", ".sh"):
                out.append(path)
    return out


def _cargo_tree_lines(path: Path) -> list[str]:
    """Lines in `path` that actually invoke `cargo tree` (comments excluded)."""
    try:
        text = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return []
    hits: list[str] = []
    for raw in text.splitlines():
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        if path.suffix == ".sh":
            if _SHELL_CALL.search(stripped):
                hits.append(stripped)
        # Python: the LIST form only. Matching the bare words `cargo tree` here
        # would flag prose — a docstring explaining the query, an assertion
        # message quoting it — and a pin whose reds are mostly its own comments
        # is one people learn to skip past. Every python caller in this tree
        # builds an argv list; the positive control above is what keeps that
        # assumption honest.
        elif _PY_CALL.search(stripped):
            hits.append(stripped)
    return hits


def _callers() -> list[tuple[Path, list[str]]]:
    found = []
    for path in _scanned_files():
        if path.name == Path(__file__).name:
            continue
        lines = _cargo_tree_lines(path)
        if lines:
            found.append((path, lines))
    return found


def test_the_scan_finds_the_known_callers():
    """Positive control. Without it this file passes vacuously the day the walk
    breaks, the suffixes change, or the call form stops matching — the "a parse
    that yields nothing must not read as all clear" discipline, applied to this
    pin's own query."""
    names = {p.name for p, _ in _callers()}
    for expected in (
        "check-protocol-deps.sh",
        "check-mls-production-features.sh",
        "dep-inventory.py",
        "test_no_desktop_keyring_on_phone_targets.py",
    ):
        assert expected in names, (
            f"{expected} runs a `cargo tree` query but this pin's scan no longer "
            f"sees it, so every assertion below is guarding nothing. Found: "
            f"{sorted(names)}"
        )


@pytest.mark.parametrize(
    "rel",
    [
        pytest.param(str(p.relative_to(_REPO)), id=p.name.replace(".", "_"))
        for p, _ in _callers()
    ],
)
def test_a_cargo_tree_caller_opts_out_or_asks_the_workspace(rel):
    path = _REPO / rel
    text = path.read_text(encoding="utf-8")
    lines = _cargo_tree_lines(path)
    if _OVERRIDE in text:
        return
    unscoped = [ln for ln in lines if "--workspace" not in ln]
    assert not unscoped, (
        f"{rel} runs a per-root `cargo tree` query without setting "
        f"`{_OVERRIDE}=selected`.\n\n"
        f"Under `.cargo/config.toml`'s `[resolver] feature-unification = "
        f'"workspace"` the resolve is "as if the whole workspace were selected", '
        f"so a `-p <crate>` or `--target <triple>` query silently answers a "
        f"WIDER question than it asked — and resolve-only queries compile "
        f"nothing, so nothing else will tell you. Export the override at the top "
        f"of the script (shell) or pass it in the subprocess env (python); a "
        f"`--workspace` query needs neither.\n\n"
        f"Offending line(s):\n  " + "\n  ".join(unscoped)
    )
