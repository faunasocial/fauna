"""The 7-day minimum release age for new dependency versions stays committed.

`release-integrity.md` § Dependency verification → *Minimum release age for new
dependency versions* (owner-ruled 2026-10-03) says Cargo's resolver refuses any
registry version published less than 7 days ago. The whole control is three
lines of `.cargo/config.toml`, and it fails silently in both directions:

  * drop the `[registry]` value and every resolution goes back to "newest",
    with no error anywhere;
  * drop the `[unstable]` gate and the pinned Cargo (1.99 nightly) prints one
    warning line and ignores the value — measured 2026-10-03;
  * commit the resolver's own override (`incompatible-publish-age`, or its
    environment form in a recipe or workflow) and the value is still there to
    read while nothing enforces it.

So the gate is that all three hold. The age is the owner's: changing the number
here without a new ruling in the goal doc is the drift this test exists to stop.

The same age is committed for Deno's npm and JSR resolution, as the
`minimumDependencyAge` key of every tracked `deno.json` (the web app and the
project site). Measured on Deno 2.7.14, 2026-10-03: the key takes an ISO-8601
duration (`"7 days"` is rejected), an already-locked version is kept whatever
its age, and it fails silently the same two ways — a removed key resolves
"newest" with no error (Deno does not warn on a key it does not know either),
and the command-line flag of the same name overrides the key for one
invocation (`=0` switches it off).

tier_1: reads tracked files, runs no cargo and no deno.
"""

import json
import subprocess
import tomllib
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_CONFIG = _REPO / ".cargo" / "config.toml"

#: The owner's ruling. Not a tunable: see the module docstring.
_AGE = "7 days"

#: The resolver's per-invocation override, as an environment variable. Built
#: from parts so this file's own walk does not find it here.
_OVERRIDE_ENV = "CARGO_RESOLVER_" + "INCOMPATIBLE_PUBLISH_AGE"

#: The same ruling in the form Deno takes (an ISO-8601 duration).
_DENO_AGE = "P7D"

#: Deno's per-invocation override, as a command-line flag. Built from parts for
#: the same reason.
_DENO_OVERRIDE_FLAG = "--minimum-" + "dependency-age"

_SCANNED_SUFFIXES = (".py", ".sh", ".yml", ".yaml", ".cmd", ".ps1", ".toml")


def _config() -> dict:
    return tomllib.loads(_CONFIG.read_text(encoding="utf-8"))


def test_the_seven_day_age_is_committed():
    value = _config().get("registry", {}).get("global-min-publish-age")
    assert value == _AGE, (
        f"{_CONFIG.relative_to(_REPO)}: [registry] global-min-publish-age is "
        f"{value!r}, the owner's ruling is {_AGE!r} (release-integrity.md "
        "§ Dependency verification → Minimum release age for new dependency "
        "versions). A different number needs a new ruling there first."
    )


def test_the_unstable_gate_is_on():
    """Without the gate the pinned Cargo warns and ignores the age.

    At the pin bump to a Cargo that has stabilised the setting (1.100), this
    assertion and the `[unstable]` line go together — but only after checking
    that Cargo still honours the `[registry]` value without it.
    """
    gate = _config().get("unstable", {}).get("min-publish-age")
    assert gate is True, (
        f"{_CONFIG.relative_to(_REPO)}: [unstable] min-publish-age is {gate!r}; "
        "the pinned Cargo ignores [registry] global-min-publish-age without it."
    )


def test_the_override_is_not_committed_in_config():
    value = _config().get("resolver", {}).get("incompatible-publish-age")
    assert value is None, (
        f"{_CONFIG.relative_to(_REPO)}: [resolver] incompatible-publish-age = "
        f"{value!r} switches the minimum release age off for every resolution."
    )


def _scanned_files() -> list[Path]:
    """Everything tracked that could run a cargo resolution.

    A walk, not a hand-listed set: the override would be added by whoever is
    blocked by the age, wherever they happen to be working.
    """
    out: list[Path] = [_REPO / "justfile"]
    out += sorted(_REPO.glob("**/Dockerfile*"))
    for root in (_REPO / "scripts", _REPO / ".github", _REPO / "tests" / "e2e-unified", _REPO / "tools", _REPO / ".cargo"):
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*")):
            if path.is_file() and path.suffix in _SCANNED_SUFFIXES:
                out.append(path)
    return [p for p in out if p.is_file() and "node_modules" not in p.parts and "target" not in p.parts]


def test_the_override_is_not_committed_in_any_recipe_or_workflow():
    hits = []
    for path in _scanned_files():
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            if _OVERRIDE_ENV in line:
                hits.append(f"{path.relative_to(_REPO)}:{number}: {line.strip()[:160]}")
    assert not hits, (
        f"{_OVERRIDE_ENV} is set in a tracked file — it switches the minimum "
        "release age off for whatever that file runs:\n" + "\n".join(hits)
    )


def _deno_configs() -> list[Path]:
    """Every tracked Deno config — asked of git, so a new Deno project is held
    from its first commit and no untracked directory is ever walked."""
    listed = subprocess.run(
        ["git", "-C", str(_REPO), "ls-files", "--", "deno.json", "*/deno.json", "deno.jsonc", "*/deno.jsonc"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split()
    return [_REPO / name for name in sorted(listed)]


def test_deno_configs_are_found():
    names = {p.relative_to(_REPO).as_posix() for p in _deno_configs()}
    assert {"apps/fauna-web/deno.json", "sites/fauna-social/deno.json"} <= names, names


@pytest.mark.parametrize("config", _deno_configs(), ids=lambda p: str(p.relative_to(_REPO)))
def test_the_seven_day_age_is_committed_for_deno(config: Path):
    """Exactly the duration string: the key's object form carries an `exclude`
    list, which is an override by another name."""
    value = json.loads(config.read_text(encoding="utf-8")).get("minimumDependencyAge")
    assert value == _DENO_AGE, (
        f"{config.relative_to(_REPO)}: minimumDependencyAge is {value!r}, the "
        f"owner's ruling is {_DENO_AGE!r} (release-integrity.md § Dependency "
        "verification → Minimum release age for new dependency versions)."
    )


def test_the_deno_override_is_not_committed_in_any_recipe_or_workflow():
    hits = []
    for path in _scanned_files():
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            if _DENO_OVERRIDE_FLAG in line:
                hits.append(f"{path.relative_to(_REPO)}:{number}: {line.strip()[:160]}")
    assert not hits, (
        f"{_DENO_OVERRIDE_FLAG} is passed in a tracked file — it overrides the "
        "committed minimum release age for whatever that file runs:\n" + "\n".join(hits)
    )
