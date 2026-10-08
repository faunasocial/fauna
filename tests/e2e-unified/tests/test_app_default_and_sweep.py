"""Proofs for the rust-first default app set and the `--app sweep` token.

testing.md § Default app and nest mode. The flip (2026-08-01) made `[tui]` the
default app set on every dev machine and demoted the old per-machine sets to
**sweep** sets, reachable as the `sweep` token.

Each proof here is red-on-regression against a specific way this was, or would
have been, got wrong:

  * **`get_available_apps()` governs only the parametrization.** Both filtering
    hooks used to key on the *presence of the `--app` flag*, so a bare run did
    no app filtering whatsoever — flipping the default alone would have left
    every `@pytest.mark.ios` test collecting on a Linux dev machine, and every
    `[web]` item beside the `[tui]` one.
    `test_bare_run_filters_by_the_default_app_set` is
    the proof: restoring the old `if client_opt:` guard turns it red.
  * **The sweep token must expand in BOTH readers of the app list.** When only
    `get_available_apps()` knew it, the parametrization expanded correctly
    while the deselect hook compared each item against the literal string
    "sweep" — measured 2026-08-01 on a three-app dev machine: 731 of 3128
    items selected instead of 2767, i.e. `--app sweep` quietly ran *less*
    than `--app tui`.

These are pure functions over stubs: no nest, no driver, no binary.
"""

from __future__ import annotations

import sys
from types import SimpleNamespace

import pytest

import conftest
from helpers import app_surface

pytestmark = pytest.mark.tier_1


class _Mark:
    def __init__(self, name: str):
        self.name = name


class _Item:
    """Minimal stand-in for a collected pytest item.

    Only the surface `pytest_collection_modifyitems` actually touches: the
    node name (which carries the `[app]` parametrization), the marker set, and
    the file path `_is_client_independent` reads.
    """

    def __init__(
        self,
        name: str,
        markers: tuple[str, ...] = (),
        path=None,
        callspec_params=None,
        direct_params: tuple[str, ...] = (),
    ):
        self.name = name
        # Every stub carries a tier marker: the same hook enforces strict tier
        # tagging at the end, and a stub without one fails there for a reason
        # that has nothing to do with the app axis under test.
        self._markers = markers + ("tier_1",)
        self.nodeid = f"tests/test_stub.py::{name}"
        # A plain top-level test file: never client-independent, so these
        # stubs exercise the app axis and nothing else.
        self.path = path or (conftest._repo_root / "tests" / "e2e-unified" / "tests" / "test_stub.py")
        # Mirrors pytest's real `item.callspec.params` — a dict from fixture
        # name to its parametrized value. `_parametrized_clients` keys on this, never on `name`'s bracket suffix, so a
        # caller that does not pass `callspec_params` explicitly gets one
        # synthesized from the bracket here — a real fixture per known-app
        # token, matching how most of this file's stubs only care about the
        # app axis, not the direct-vs-real discriminator below. A caller that
        # DOES want to exercise that discriminator passes `callspec_params` +
        # `direct_params` itself.
        if callspec_params is None and "[" in name:
            bracket = name.split("[", 1)[1].rstrip("]")
            callspec_params = {
                tok: tok for tok in bracket.split("-") if tok in conftest._KNOWN_APPS
            }
        self.callspec = _CallSpec(callspec_params) if callspec_params else None
        if callspec_params:
            # `_is_real_fixture` (helpers/fixture_closure.py) discriminates a
            # directly-parametrized argname (pytest's own synthesized pseudo-
            # fixture, module `_pytest.python`) from a genuinely real one (any
            # other module) via `item._fixtureinfo.name2fixturedefs`. Keys
            # named in `direct_params` get the pseudo-fixture shape; every
            # other key defaults real, mirroring `is_real_fixture`'s own
            # conservative "unknown shape answers real" fallback.
            self._fixtureinfo = SimpleNamespace(name2fixturedefs={
                k: [SimpleNamespace(
                    func=SimpleNamespace(
                        __module__="_pytest.python" if k in direct_params else "conftest"
                    ),
                    baseid="",
                )]
                for k in callspec_params
            })

    def iter_markers(self, name=None):
        return iter([_Mark(m) for m in self._markers if name is None or m == name])

    def get_closest_marker(self, name: str):
        return _Mark(name) if name in self._markers else None

    def add_marker(self, marker):  # only reached by live_box items
        pass


class _CallSpec:
    def __init__(self, params: dict):
        self.params = params


class _Config:
    def __init__(self, app=None, include_independent=False, tier=None):
        self._opts = {
            "--app": app,
            "--client": None,
            "--include-independent": include_independent,
            "--tier": tier,
        }
        self.hook = self
        self.deselected: list[_Item] = []

    def getoption(self, name):
        return self._opts.get(name)

    def pytest_deselected(self, items):
        self.deselected.extend(items)


def _bare_argv(monkeypatch):
    """A run with no `--app` flag and no env override — the default path."""
    monkeypatch.setattr(sys, "argv", ["pytest"])
    monkeypatch.delenv("E2E_APPS", raising=False)
    monkeypatch.delenv("E2E_CLIENTS", raising=False)


def test_default_app_set_is_tui_on_every_dev_machine(monkeypatch):
    """The flip itself — and it must hold for all three dev platforms, not
    just whichever box happens to be running this test."""
    _bare_argv(monkeypatch)
    for plat in ("ubuntu", "macos", "windows"):
        monkeypatch.setattr(conftest, "detect_platform", lambda _p=plat: _p)
        assert conftest.get_available_apps() == ["tui"], plat


def test_bare_run_filters_by_the_default_app_set(monkeypatch):
    """A run with no `--app` still applies the app axis.

    This is the half the flip could most easily have missed: the default set
    has to *filter*, not merely parametrize.
    """
    _bare_argv(monkeypatch)
    items = [
        _Item("test_a[tui]"),
        _Item("test_a[web]"),
        _Item("test_ios_only", markers=("ios",)),
    ]
    config = _Config(app=None)
    conftest.pytest_collection_modifyitems(config, items)
    assert [i.name for i in items] == ["test_a[tui]"]
    assert {i.name for i in config.deselected} == {"test_a[web]", "test_ios_only"}


def test_sweep_token_expands_in_the_deselect_hook(monkeypatch):
    """`--app sweep` must select the whole sweep set, not the literal token."""
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "sweep"])
    sweep = conftest.sweep_apps()
    assert sweep, "every dev platform declares a non-empty sweep set"
    outsider = next(c for c in sorted(conftest._KNOWN_APPS) if c not in sweep)

    items = [_Item(f"test_a[{app}]") for app in sweep] + [_Item(f"test_a[{outsider}]")]
    config = _Config(app="sweep")
    conftest.pytest_collection_modifyitems(config, items)

    assert [i.name for i in items] == [f"test_a[{app}]" for app in sweep]
    assert [i.name for i in config.deselected] == [f"test_a[{outsider}]"]


def test_sweep_is_a_superset_of_the_default(monkeypatch):
    """The sweep set is exactly the pre-flip default, so `--app sweep`
    restores precisely the coverage the flip moved off the default path."""
    _bare_argv(monkeypatch)
    assert set(conftest.get_available_apps()) <= set(conftest.sweep_apps())


def test_last_app_flag_wins_matching_pytest_getoption(monkeypatch):
    """A `just` recipe supplies `--app sweep`; the caller appends `--app tui`.

    pytest's own `config.getoption("--app")` returns the LAST occurrence and
    the deselect hook reads it from there, so this sys.argv scan has to agree.
    A first-wins scan would build the parametrization for one app set while the
    filter selected another — the recipe defaults added for
    `e2e-version-skew-test` / `e2e-crash-recovery-test` / `e2e-reclaim-cycle-test`
    make that combination routine rather than exotic.
    """
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "sweep", "--app", "tui"])
    assert conftest.get_available_apps() == ["tui"]
    # `android`, not `web`: web is refused off Linux, and this proof is about
    # which occurrence wins, which any app token shows on every dev machine.
    monkeypatch.setattr(sys, "argv", ["pytest", "--app=sweep", "--app=android"])
    assert conftest.get_available_apps() == ["android"]


def test_resolve_app_tokens_dedups_and_preserves_order():
    # `android`, not `web`: web is refused off Linux, order/dedup is app-agnostic.
    assert conftest._resolve_app_tokens(" tui , android ,tui") == ["tui", "android"]
    assert conftest._resolve_app_tokens("") == []
    assert conftest._resolve_app_tokens("sweep") == conftest.sweep_apps()


def test_two_real_app_fixture_needs_both_seats_selected(monkeypatch):
    """A test parametrizing BOTH `folder_share_owner_app` and
    `folder_share_recipient_app` (item name e.g. `[tui-macos]`) launches TWO
    real GUI processes — it must be deselected unless every seat's app is
    drivable HERE, not merely one of them.

    Regression for the 2026-08-17 finding: `--app sweep` on a Linux dev
    machine (sweep set has `tui`, not `macos`) selected
    `test_macos_writer_member_decrypts_owner_upload[tui-macos]` and burned two
    full failed cross-compile builds (`E0463: can't find crate for std`,
    `aarch64-apple-darwin` absent) before the in-body skip ever ran — the old
    check kept the item because `tui` alone (the FIRST seat) intersected
    `selected`, never checking the second.
    """
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "sweep"])
    sweep = conftest.sweep_apps()
    if "tui" not in sweep or "macos" in sweep:
        # The finding it replays is a two-seat item whose SECOND seat this box
        # cannot drive, so the proof needs a sweep set holding one seat's app and
        # not the other's — true on a Linux or Windows dev machine, false on an
        # Apple one, whose sweep set holds both. A box property, never an app's
        # (convention 7), so it declares `skip_environment` rather than failing:
        # an assert here reds every tier_1 run on such a box, for a reason no
        # change to any app could ever fix.
        app_surface.skip_environment(
            f"this proof needs a sweep set with tui but not macos; this box's is {sweep}"
        )

    # The kept item's second seat is drawn from THIS box's sweep set, not
    # hard-coded: `linux` is a sweep app on the Linux dev machine only (the
    # Windows one sweeps web, windows, tui), and a literal `[tui-linux]` is
    # correctly deselected on Windows — the
    # proof is "an item whose every seat is drivable here is kept", which any
    # sweep app other than tui witnesses on every box.
    here = next(a for a in sweep if a != "tui")
    both_seats_here = _Item(
        f"test_owner_writer[tui-{here}]",
        callspec_params={
            "folder_share_owner_app": "tui", "folder_share_recipient_app": here,
        },
    )
    second_seat_absent = _Item(
        "test_macos_writer_member_decrypts_owner_upload[tui-macos]",
        callspec_params={
            "folder_share_owner_app": "tui", "folder_share_recipient_app": "macos",
        },
    )
    items = [both_seats_here, second_seat_absent]
    config = _Config(app="sweep")
    conftest.pytest_collection_modifyitems(config, items)

    assert [i.name for i in items] == [f"test_owner_writer[tui-{here}]"]
    assert [i.name for i in config.deselected] == [
        "test_macos_writer_member_decrypts_owner_upload[tui-macos]"
    ]


def test_driver_kind_data_param_is_not_mistaken_for_a_second_app(monkeypatch):
    """A `driver_kind`-style data parameter that merely shares a known app's
    NAME (simulating another client's rendered outcome through the ONE real
    `app` under test) must NOT be treated as a second real app requirement —
    only `_REAL_SECOND_APP_FIXTURES` fixtures carry that meaning. Regression
    guard: requiring every app-shaped token in an item's name would wrongly
    deselect e.g.
    `test_handle_entry_outcomes.py::test_format_invalid_disables_continue[tui-macos]`
    (`def test_format_invalid_disables_continue(app, driver_kind)`) on a Linux
    dev machine, whose "macos" never launches a second app.
    """
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "sweep"])
    sweep = conftest.sweep_apps()
    if "tui" not in sweep or "macos" in sweep:
        # The same box property its sibling above declares, for the same reason:
        # the finding needs a sweep set holding one of the item's two app-shaped
        # tokens and not the other, which an Apple dev machine's does not give.
        app_surface.skip_environment(
            f"this proof needs a sweep set with tui but not macos; this box's is {sweep}"
        )

    # `app` is the real fixture (its value, "tui", is what selection keys
    # on); `driver_kind` is a direct `@pytest.mark.parametrize` on the test
    # whose value happens to equal a known app name.
    item = _Item(
        "test_format_invalid_disables_continue[tui-macos]",
        callspec_params={"app": "tui", "driver_kind": "macos"},
        direct_params=("driver_kind",),
    )
    items = [item]
    config = _Config(app="sweep")
    conftest.pytest_collection_modifyitems(config, items)

    assert [i.name for i in items] == ["test_format_invalid_disables_continue[tui-macos]"]
    assert config.deselected == []


# ── a direct-parametrize value is not app selection  ──
#
# `_parametrized_clients` used to pattern-match `item.name`'s bracket suffix
# for hyphen tokens equal to a known app name — so ANY parametrization whose
# value happened to collide with an app name, real fixture or not, was read
# as "this test exercises that app". Found 2026-08-25 via
# `test_merge_gate_check.py::test_gate_recipes_are_locked`'s `gate`
# parametrization (full merge-gate names like `android-unit-test-compile-check`
# — never a real app fixture): 4 of its ~28 cases were silently deselected
# under the default bare run, and no single `--app` value collected all of
# them (each value deselects a *different* subset of the false positives).
# The same defect independently affects ~20 more non-app parametrizations
# across ≥5 other tier_1 files (`test_build_slot.py`'s `platform`,
# `test_multiseat_config.py`'s `platform`/`seat`, …) — the two tests below
# cover both shapes the fix must handle: a value with NO exact match to a
# known app name at all, and a value that IS an exact match but comes from a
# direct parametrize rather than a real fixture.


def test_gate_name_parametrize_value_is_not_app_selection(monkeypatch):
    """`test_gate_recipes_are_locked[android-unit-test-compile-check]`'s
    `gate` value never equals a bare `_KNOWN_APPS` member exactly, so this
    case is excluded by value-matching alone — a regression pin for the
    literal bug found, independent of the `_is_real_fixture` discriminator
    the next test covers."""
    monkeypatch.setattr(sys, "argv", ["pytest"])  # bare run, default app set
    item = _Item(
        "test_gate_recipes_are_locked[android-unit-test-compile-check]",
        callspec_params={"gate": "android-unit-test-compile-check"},
        direct_params=("gate",),
    )
    items = [item]
    config = _Config(app=None)
    conftest.pytest_collection_modifyitems(config, items)
    assert items == [item]
    assert config.deselected == []


def test_direct_parametrize_value_exactly_matching_an_app_name_is_not_app_selection(monkeypatch):
    """`test_seat_for_platform_maps_the_three_dev_machines[linux-linux]`
    (`@pytest.mark.parametrize("platform,seat", [("linux", "linux"), ...])`)
    — here BOTH values exactly equal a known app name, so only
    `_is_real_fixture` (neither `platform` nor `seat` is a real fixture)
    excludes it; value-matching alone would not."""
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "tui"])
    item = _Item(
        "test_seat_for_platform_maps_the_three_dev_machines[linux-linux]",
        callspec_params={"platform": "linux", "seat": "linux"},
        direct_params=("platform", "seat"),
    )
    items = [item]
    config = _Config(app="tui")
    conftest.pytest_collection_modifyitems(config, items)
    assert items == [item]
    assert config.deselected == []


def test_real_app_fixture_value_still_filters_correctly(monkeypatch):
    """The corrected discriminator must not become a blanket bypass — a test
    genuinely driven by the real `app` fixture still deselects when its app
    is not selected. (`callspec_params={"app": ...}` with no `direct_params`
    — `app` defaults real, matching production.)"""
    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "tui"])
    kept_item = _Item("test_something[tui]", callspec_params={"app": "tui"})
    deselected_item = _Item("test_something[web]", callspec_params={"app": "web"})
    items = [kept_item, deselected_item]
    config = _Config(app="tui")
    conftest.pytest_collection_modifyitems(config, items)
    assert items == [kept_item]
    assert [i.name for i in config.deselected] == ["test_something[web]"]


def test_an_empty_client_set_placeholder_is_deselected_not_skipped(monkeypatch):
    """A test restricted to clients this run did not select — the launch-routing
    smoke's `_clients(*AUTOSTART_APPS)` on a macos run — is parametrized over an
    empty set, and pytest collects a lone `[NOTSET]` placeholder that skips and
    records nothing. It sat in every `--feature` selection as a phantom witness; the
    app axis deselects it instead, as it does any item none of whose clients is
    selected. The sentinel is pytest's real one, so a pytest that changes its shape
    reds here rather than silently re-admitting the phantom."""
    from _pytest.compat import NOTSET

    monkeypatch.setattr(sys, "argv", ["pytest", "--app", "tui"])
    phantom = _Item("test_smoke_j[NOTSET]", callspec_params={"launch_harness": NOTSET})
    real = _Item("test_smoke_m[tui]", callspec_params={"launch_harness": "tui"})
    items = [phantom, real]
    config = _Config(app="tui")
    conftest.pytest_collection_modifyitems(config, items)
    assert items == [real]
    assert [i.name for i in config.deselected] == ["test_smoke_j[NOTSET]"]
