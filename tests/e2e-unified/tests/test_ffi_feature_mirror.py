"""tier_1: the thin `fauna-ffi` crate forwards every feature of the core
`fauna-ffi-core`, name for name, and nothing else — but the one addition the
build stamp needs.

Authority: build-target-layout-windows.md § UniFFI cdylib rebuild cost (win) →
*The rlib + thin-cdylib split*. Every recipe, release step and harness builder
keeps `-p fauna-ffi` — the thin package, the one that links the artifacts — and
passes its flavor flags there, while the code behind every flag is the core's.
So a core feature with no forward is unreachable from every artifact build
(cargo refuses the flag), a forward naming anything but its own core feature
builds a different flavor than the flag says, a thin-only feature is a flavor
the core does not know, and a different `default` ships a plain
`-p fauna-ffi` build with another face than `-p fauna-ffi-core` tests.

The one addition (the same section → *The stamp*): `status_text`, the one
UniFFI function that reads the build stamp, lives in the THIN crate behind its
own `cfg(feature = "value-format")`, so that cfg must be true exactly when the
core's is. The thin `value-format` therefore also enables the thin crate's
`fauna-client-status` dependency, and every thin forward whose core feature
implies `value-format` (the core's implication closure, computed here) names
the thin crate's own `"value-format"` as well. Anything beyond that is drift.

The split also rests on the two lib names: UniFFI names every exported symbol
after the CRATE (measured 2026-10-25), so both crates are lib `fauna_ffi` and
the thin crate reaches the core under the extern name `fauna_ffi_core` — and an
export in the thin crate joins the core's `fauna_ffi` namespace (measured
2026-10-26), which is what lets `status_text` move there.

tier_1: reads two manifests and the thin crate's `lib.rs`, runs no cargo.
"""

import copy
import tomllib
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_CORE = _REPO / "libs" / "fauna-ffi" / "Cargo.toml"
_THIN = _REPO / "libs" / "fauna-ffi-cdylib" / "Cargo.toml"
_CORE_PACKAGE = "fauna-ffi-core"
# The feature the thin crate's `status_text` module is compiled under, and the
# thin-only dependency that module needs.
_STAMP_FEATURE = "value-format"
_STAMP_DEP = "dep:fauna-client-status"


def _load(path: Path) -> dict:
    return tomllib.loads(path.read_text(encoding="utf-8"))


def _implied(core_features: dict, name: str) -> set[str]:
    """Every core feature `name` turns on, itself included — following only the
    entries that are themselves core features (never `dep:`/`crate/feat`)."""
    seen, stack = set(), [name]
    while stack:
        feature = stack.pop()
        if feature in seen:
            continue
        seen.add(feature)
        stack.extend(e for e in core_features.get(feature, []) if e in core_features)
    return seen


def _forward(core_features: dict, name: str) -> list[str]:
    """The exact thin entry for core feature `name`."""
    forward = [f"{_CORE_PACKAGE}/{name}"]
    if name == _STAMP_FEATURE:
        return forward + [_STAMP_DEP]
    if _STAMP_FEATURE in _implied(core_features, name):
        return forward + [_STAMP_FEATURE]
    return forward


def mirror_errors(core: dict, thin: dict) -> list[str]:
    """Every way the thin crate's `[features]` differs from a pure forward plus
    the stamp's one addition."""
    core_features = core.get("features", {})
    thin_features = thin.get("features", {})
    errors = []
    for name in sorted(n for n in core_features if n != "default"):
        got = thin_features.get(name)
        want = _forward(core_features, name)
        if got is None:
            errors.append(f"missing forward — add: {name} = {want}")
        elif got != want:
            errors.append(f"{name} forwards {got}, not {want}")
    for name in sorted(set(thin_features) - set(core_features)):
        errors.append(f"thin-only feature {name}: the core has no such feature")
    if thin_features.get("default") != core_features.get("default"):
        errors.append(
            f"default differs: thin {thin_features.get('default')}, "
            f"core {core_features.get('default')}"
        )
    return errors


def test_the_thin_crate_forwards_every_core_feature_and_nothing_else():
    errors = mirror_errors(_load(_CORE), _load(_THIN))
    assert not errors, (
        "libs/fauna-ffi-cdylib/Cargo.toml's [features] must mirror "
        "libs/fauna-ffi/Cargo.toml's, each forwarding to the core:\n  "
        + "\n  ".join(errors)
    )


def test_the_thin_crate_reaches_the_core_with_its_defaults_off():
    dep = _load(_THIN)["dependencies"][_CORE_PACKAGE]
    # `package =` makes the extern name `fauna_ffi_core` rather than the thin
    # crate's own name; defaults off so a flavor's flags reach the core exactly.
    assert dep.get("package") == _CORE_PACKAGE
    assert dep.get("path") == "../fauna-ffi"
    assert dep.get("default-features") is False


def test_both_crates_keep_the_lib_name_every_uniffi_symbol_carries():
    core, thin = _load(_CORE), _load(_THIN)
    assert core["package"]["name"] == _CORE_PACKAGE
    assert thin["package"]["name"] == "fauna-ffi"
    assert core["lib"] == {"name": "fauna_ffi", "crate-type": ["lib"]}
    assert thin["lib"] == {"name": "fauna_ffi", "crate-type": ["staticlib", "cdylib"]}


def test_the_thin_source_is_the_reexport_plus_the_stamped_status_text():
    """The thin crate carries no FFI code but the one function that reads the
    build stamp; anything else belongs in the core, where the tests live."""
    source = (_THIN.parent / "src" / "lib.rs").read_text(encoding="utf-8")
    code = [
        line.strip()
        for line in source.splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    assert code == [
        "pub use fauna_ffi_core::*;",
        f'#[cfg(feature = "{_STAMP_FEATURE}")]',
        "mod status_text;",
    ], code


def test_the_stamp_addition_reaches_exactly_the_value_format_closure():
    """Pins the closure against today's manifest, so a reader sees which thin
    forwards carry the addition and why."""
    core_features = _load(_CORE)["features"]
    carrying = sorted(
        n
        for n in core_features
        if n not in ("default", _STAMP_FEATURE)
        and _STAMP_FEATURE in _implied(core_features, n)
    )
    for name in ("kids-safe", "deployment-seed", "contact-overlays", "store-safe",
                 "backup-destinations", "backup-audit"):
        assert name in carrying, f"{name} no longer implies {_STAMP_FEATURE}: {carrying}"
    assert "payments" not in carrying


@pytest.mark.parametrize(
    "mutation",
    ["drop", "retarget", "extra", "default", "unstamped", "overstamped", "stampless-dep"],
)
def test_the_mirror_check_reds_on_each_drift(mutation):
    core, thin = _load(_CORE), copy.deepcopy(_load(_THIN))
    features = core["features"]
    victim = sorted(n for n in features if n != "default")[0]
    implying = sorted(
        n for n in features
        if n not in ("default", _STAMP_FEATURE) and _STAMP_FEATURE in _implied(features, n)
    )[0]
    plain = next(
        n for n in sorted(features)
        if n != "default" and _STAMP_FEATURE not in _implied(features, n)
    )
    if mutation == "drop":
        del thin["features"][victim]
    elif mutation == "retarget":
        thin["features"][victim] = [f"{_CORE_PACKAGE}/default"]
    elif mutation == "extra":
        thin["features"]["thin-only"] = []
    elif mutation == "default":
        thin["features"]["default"] = thin["features"]["default"][1:]
    elif mutation == "unstamped":
        # The core turns on value-format but the thin cfg stays false: the
        # artifact silently loses `status_text`.
        thin["features"][implying] = [f"{_CORE_PACKAGE}/{implying}"]
    elif mutation == "overstamped":
        thin["features"][plain] = [f"{_CORE_PACKAGE}/{plain}", _STAMP_FEATURE]
    else:
        thin["features"][_STAMP_FEATURE] = [f"{_CORE_PACKAGE}/{_STAMP_FEATURE}"]
    assert mirror_errors(core, thin), f"a {mutation} of the mirror went unnoticed"
