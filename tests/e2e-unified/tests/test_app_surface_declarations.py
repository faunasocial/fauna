"""Proofs for the unbuilt-vs-absent-vs-environment skip taxonomy.

testing.md § Cross-app e2e conventions, convention 7. The mechanism under test
is `helpers/app_surface.py` + `scripts/check_app_gate_ratchet.py`: together they
answer "does this app actually run this test, or does it skip quietly?".

Every proof here is red-on-regression against the specific way the thing it
covers has already broken once:

  * `app_name` — `app_capabilities.app_name()` matched substrings of the
    driver's class name, and no substring of "tuidriver" matched any arm, so tui
    resolved to "unknown" and every capability lookup for it answered False.
    Mutation-verified 2026-07-29: deleting the `("tui", "is_tui")` arm turns
    `test_app_name_resolves_every_app[tui]` AND
    `test_app_capabilities_resolves_tui_and_declares_its_sections` red. That
    check is the reason `_driver_for`'s stub class is named `OpaqueDriver` — the
    first version named it `TuiDriver`, so `app_name`'s class-name FALLBACK
    answered "tui" with the arm deleted and the mutation stayed green. A proof
    whose fixture can satisfy it through the fallback proves nothing about the
    predicate.
  * the strict/lenient split — the whole flag is worthless if a *declared*
    absence also fails, because then nobody can leave it on.
  * the static scan — a scan that misses a shape is exactly the failure the
    hand-keyed "there are exactly three of these gates" list already made. The
    fixture sources below are the four shapes found in the real tree.
"""

from __future__ import annotations

import sys
import textwrap
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

E2E_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(E2E_ROOT.parent.parent / "scripts"))

from helpers import app_surface  # noqa: E402
from helpers.app_surface import (  # noqa: E402
    app_name,
    declared_absence,
    skip_environment,
    skip_unbuilt,
)


# ── Stub drivers: one predicate answers True, exactly like the real ones ────
def _driver_for(app: str):
    """A stand-in that answers only its own `is_<app>()`, like the real drivers.

    Verified against the tree on 2026-07-29: each of the seven drivers in
    `drivers/` returns True for exactly one of these predicates.
    """
    names = ("tui", "web", "linux", "windows", "macos", "ios", "android")

    # The class name deliberately does NOT contain the app name. `app_name()`
    # falls back to the class name with a "driver" suffix stripped, so a stub
    # called `TuiDriver` would resolve to "tui" through the FALLBACK even with
    # the `is_tui` arm deleted — the mutation check caught exactly that, and the
    # proof was hollow until this name became neutral. Now only the predicate
    # can produce the right answer.
    class OpaqueDriver:
        pass

    stub = OpaqueDriver()
    for name in names:
        setattr(stub, f"is_{name}", (lambda n=name: n == app))
    return stub


@pytest.fixture(autouse=True)
def _lenient_and_clean():
    """Every test states its own strictness; none leaks a tally to the next —
    and, just as importantly, none EATS the run's real tally.

    The cases below assert `unbuilt_hits()` exactly, so each needs a clean
    slate; but the tally is accumulated for the whole run and read once at the
    end (`conftest.py::pytest_terminal_summary`). Clearing without restoring
    therefore deleted every genuinely-unbuilt surface recorded before this
    module ran, silently shortening the very count convention 7 ratchets. So:
    snapshot, clear, restore.
    """
    outer = app_surface.unbuilt_hits()
    app_surface.set_strict_app(False)
    app_surface.reset_unbuilt_hits()
    yield
    app_surface.set_strict_app(False)
    app_surface.restore_unbuilt_hits(outer)


# ── app_name ───────────────────────────────────────────────────────────────
@pytest.mark.parametrize(
    "app", ["tui", "web", "linux", "windows", "macos", "ios", "android"]
)
def test_app_name_resolves_every_app(app):
    """All seven resolve — tui included, which is the one that used to not."""
    assert app_name(_driver_for(app)) == app


def test_app_name_never_returns_unknown_for_a_named_driver():
    """A driver that answers no predicate is still NAMED, not swallowed.

    "unknown" in a skip reason is unattributable, which is how the tui hole
    stayed invisible: the message read "unknown has not implemented feed", so it
    looked like a generic harness limitation rather than a missing dict entry.
    """

    class SomeFutureDriver:
        pass

    assert app_name(SomeFutureDriver()) == "somefuture"


def test_app_name_survives_a_driver_mid_teardown():
    """A predicate that raises must not break skip reporting."""

    class Exploding:
        def is_tui(self):
            raise RuntimeError("bridge already closed")

        def is_web(self):
            return True

    assert app_name(Exploding()) == "web"


# ── skip_unbuilt: skips lenient, fails strict, always counted ──────────────
def test_skip_unbuilt_skips_when_lenient():
    with pytest.raises(pytest.skip.Exception) as excinfo:
        skip_unbuilt(_driver_for("tui"), surface="the admin-dns page")
    assert "tui has not built the admin-dns page" in str(excinfo.value)


def test_skip_unbuilt_fails_under_strict_app():
    """The core of the flag: unbuilt debt goes red so a claim is falsifiable."""
    app_surface.set_strict_app(True)
    with pytest.raises(Failed := pytest.fail.Exception) as excinfo:  # noqa: F841
        skip_unbuilt(_driver_for("tui"), surface="the admin-dns page")
    assert "--strict-app" in str(excinfo.value)
    assert "the admin-dns page" in str(excinfo.value)


def test_skip_unbuilt_is_tallied_either_way():
    """The tally is what conftest prints — the ratchet number, every run."""
    with pytest.raises(pytest.skip.Exception):
        skip_unbuilt(_driver_for("tui"), surface="surface-a", detail="because")
    app_surface.set_strict_app(True)
    with pytest.raises(pytest.fail.Exception):
        skip_unbuilt(_driver_for("web"), surface="surface-b")
    assert app_surface.unbuilt_hits() == [
        ("tui", "surface-a", "because"),
        ("web", "surface-b", ""),
    ]


def test_skip_unbuilt_message_carries_detail_and_tracking():
    """A reader must learn what is missing AND where the work is captured."""
    with pytest.raises(pytest.skip.Exception) as excinfo:
        skip_unbuilt(
            _driver_for("tui"),
            surface="the Backups snapshot surface",
            detail="only the destination half is built",
            tracked="ui/backups.md § Status",
        )
    message = str(excinfo.value)
    assert "only the destination half is built" in message
    assert "tracked: ui/backups.md § Status" in message


# ── declared_absence: a legitimate absence stays a skip, even strict ───────
def test_declared_absence_skips_even_under_strict_app():
    """If a declared absence failed too, nobody could leave the flag on."""
    app_surface.set_strict_app(True)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        declared_absence(
            _driver_for("tui"),
            capability="inline AV playback",
            doc="apps/tui.md § Declared platform absences",
        )
    assert "declared absence" in str(excinfo.value)
    assert "apps/tui.md" in str(excinfo.value)


def test_declared_absence_is_never_tallied_as_unbuilt():
    app_surface.set_strict_app(True)
    with pytest.raises(pytest.skip.Exception):
        declared_absence(
            _driver_for("tui"), capability="camera/QR capture", doc="apps/tui.md § x"
        )
    assert app_surface.unbuilt_hits() == []


@pytest.mark.parametrize("doc", ["", "   "])
def test_declared_absence_demands_a_citation(doc):
    """An absence with no declaration is unbuilt debt wearing a better name."""
    with pytest.raises(ValueError, match="requires a `doc` citation"):
        declared_absence(_driver_for("tui"), capability="something", doc=doc)


# ── skip_environment: not an app property ─────────────────────────────────
def test_skip_environment_skips_under_strict_app():
    app_surface.set_strict_app(True)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        skip_environment("no HETZNER_API_TOKEN in env")
    assert "environment:" in str(excinfo.value)
    assert app_surface.unbuilt_hits() == []


# ── The static scan (scripts/check_app_gate_ratchet.py) ───────────────────
# Each source below is a shape that occurs in the real tree. A scan that misses
# one under-reports the debt, which is precisely how "there are exactly three"
# got written down while seven sites existed.
SCAN_CASES = {
    "direct_guard": (
        1,
        """
        def test_a(app):
            if app.driver.is_tui():
                pytest.skip("not built on tui")
        """,
    ),
    "negated_disjunction": (
        1,
        """
        def test_b(app):
            if not (app.driver.is_web() or app.driver.is_linux()):
                pytest.skip("only web + linux implement this")
        """,
    ),
    "guard_and_skip_lines_apart": (
        1,
        """
        def test_c(app):
            if app.driver.is_tui():
                reason = "unbuilt"
                extra = compute(reason)
                pytest.skip(
                    f"the surface is {reason} on tui: {extra}"
                )
        """,
    ),
    "nested_under_guard": (
        1,
        """
        def test_d(app):
            if app.driver.is_tui():
                if app.something:
                    pytest.skip("unbuilt on tui")
        """,
    ),
    # Must NOT count:
    "declared_via_helper": (
        0,
        """
        def test_e(app):
            if app.driver.is_tui():
                skip_unbuilt(app.driver, surface="x")
        """,
    ),
    "declared_absence_via_helper": (
        0,
        """
        def test_f(app):
            if app.driver.is_tui():
                declared_absence(app.driver, capability="x", doc="y")
        """,
    ),
    "environment_skip_is_not_an_app_gate": (
        0,
        """
        def test_g(app):
            if not os.environ.get("HETZNER_API_TOKEN"):
                pytest.skip("no token")
        """,
    ),
    "form_factor_branch_is_not_an_app_gate": (
        0,
        """
        def test_h(app):
            if app.driver.is_mobile():
                pytest.skip("desktop-only layout assertion")
        """,
    ),
    "app_branch_without_a_skip": (
        0,
        """
        def test_i(app):
            if app.driver.is_tui():
                app.driver.press_key("tab")
            else:
                app.driver.click("thing")
        """,
    ),
}


@pytest.mark.parametrize("case", sorted(SCAN_CASES))
def test_scan_classifies_every_real_shape(case, tmp_path):
    from check_app_gate_ratchet import undeclared_app_gates

    expected, source = SCAN_CASES[case]
    path = tmp_path / "sample.py"
    path.write_text(textwrap.dedent(source))
    found = undeclared_app_gates(path)
    assert len(found) == expected, f"{case}: expected {expected}, got {found}"


def test_scan_ratchet_fails_on_a_rise_and_passes_at_baseline(tmp_path):
    """The gate itself: at baseline it is green, one added gate turns it red."""
    import json

    from check_app_gate_ratchet import run_gate

    scan_root = tmp_path / "e2e"
    (scan_root / "actions").mkdir(parents=True)
    target = scan_root / "actions" / "thing.py"
    target.write_text(
        textwrap.dedent(
            """
            def navigate(self):
                if self.driver.is_tui():
                    pytest.skip("unbuilt")
            """
        )
    )
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps({"actions/thing.py": 1}))
    assert run_gate(scan_root, baseline) == 0

    target.write_text(
        target.read_text()
        + textwrap.dedent(
            """
            def navigate_two(self):
                if self.driver.is_tui():
                    pytest.skip("also unbuilt")
            """
        )
    )
    assert run_gate(scan_root, baseline) == 1


def test_scan_starts_a_brand_new_file_at_zero(tmp_path):
    """A file with no baseline entry has an implicit baseline of 0.

    Only pre-existing debt is grandfathered; a new file may not add a gate.
    """
    from check_app_gate_ratchet import run_gate

    scan_root = tmp_path / "e2e"
    (scan_root / "tests").mkdir(parents=True)
    (scan_root / "tests" / "test_new.py").write_text(
        textwrap.dedent(
            """
            def test_x(app):
                if app.driver.is_tui():
                    pytest.skip("unbuilt")
            """
        )
    )
    empty = tmp_path / "baseline.json"
    empty.write_text("{}")
    assert run_gate(scan_root, empty) == 1


def test_update_baseline_refuses_to_launder_a_rise(tmp_path):
    """A down-only ratchet never writes a rise (mirrors the sleep ratchet)."""
    import json

    from check_app_gate_ratchet import update_baseline

    scan_root = tmp_path / "e2e"
    (scan_root / "actions").mkdir(parents=True)
    (scan_root / "actions" / "thing.py").write_text(
        textwrap.dedent(
            """
            def a(self):
                if self.driver.is_tui():
                    pytest.skip("one")
            def b(self):
                if self.driver.is_tui():
                    pytest.skip("two")
            """
        )
    )
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps({"actions/thing.py": 1}))
    assert update_baseline(scan_root, baseline) == 1
    assert json.loads(baseline.read_text()) == {"actions/thing.py": 1}


# ── Mode 5: driver-CAPABILITY predicates (found 2026-07-29 by an admin-shell
# session; closed 2026-07-30 by an e2e-parity session) ──────────────────────
# The gate names no app and no `is_X` call — the tell is a base-class default
# that silently answers for every subclass that forgot to override, not the
# skip site itself. This is what hid all 11 `test_crash_recovery_journeys.py`
# journeys on tui: `killable_app` skips on `driver.supports_unclean_kill()`,
# whose base default silently answered False for tui because the inherited
# `HttpBridgeDriver` implementation tested `self._app_proc`, a Popen tui has
# never had (it owns its app child as a pty session leader instead).


def test_capability_predicate_names_finds_real_predicates_excludes_identity_and_hooks(
    tmp_path,
):
    """The scan must catch a real capability predicate (`supports_unclean_
    kill`-shaped) while excluding: an identity predicate (`is_tui`-shaped,
    correct-by-design False for every app but one), an abstract method
    (fails loudly if unimplemented, not silently), and a bare `-> None`
    action hook (a side effect, not something callers branch on)."""
    from check_app_gate_ratchet import capability_predicate_names

    base = tmp_path / "base.py"
    base.write_text(
        textwrap.dedent(
            """
            from abc import abstractmethod

            class PlatformDriver:
                def is_tui(self) -> bool:
                    \"\"\"identity — excluded.\"\"\"
                    return False

                def supports_unclean_kill(self) -> bool:
                    \"\"\"a real capability predicate — included.\"\"\"
                    return False

                def enable_dns_fake_provider(self) -> None:
                    \"\"\"a documented no-op action hook — excluded.\"\"\"
                    return None

                @abstractmethod
                def some_abstract_predicate(self) -> bool:
                    return False
            """
        )
    )
    assert capability_predicate_names(base) == frozenset({"supports_unclean_kill"})


def test_capability_predicate_names_excludes_bare_none_action_hooks(tmp_path):
    """A documented intentional no-op default is not capability debt — this
    scan's own first false positive (`enable_dns_fake_provider`: every native
    app enables the behavior another way at launch; only web overrides),
    found and excluded 2026-07-30."""
    from check_app_gate_ratchet import capability_predicate_names

    base = tmp_path / "base.py"
    base.write_text(
        textwrap.dedent(
            """
            class PlatformDriver:
                def enable_dns_fake_provider(self) -> None:
                    \"\"\"No-op by default; only web overrides.\"\"\"
                    return None
            """
        )
    )
    assert capability_predicate_names(base) == frozenset()


def test_capability_predicate_guard_is_caught_as_an_app_gate(tmp_path):
    """The exact `killable_app` shape: not caught by the identity-only
    predicate set (matching the historical miss), but IS caught once
    capability predicates are unioned in (the fix)."""
    from check_app_gate_ratchet import undeclared_app_gates

    path = tmp_path / "sample.py"
    path.write_text(
        textwrap.dedent(
            """
            def killable_app(app):
                if not app.driver.supports_unclean_kill():
                    pytest.skip("no unclean-kill primitive")
            """
        )
    )
    assert undeclared_app_gates(path) == []
    found = undeclared_app_gates(path, frozenset({"supports_unclean_kill"}))
    assert len(found) == 1


def test_gate_predicates_includes_real_capability_predicates():
    """Ties the synthetic proofs above to the actual tree: the real
    `drivers/base.py` must contribute at least `supports_unclean_kill` to the
    live gate set, and the identity family must still be present alongside
    it (mode 5 is additive, not a replacement)."""
    from check_app_gate_ratchet import _gate_predicates

    predicates = _gate_predicates()
    assert "supports_unclean_kill" in predicates
    assert "is_tui" in predicates


def test_leaf_app_drivers_finds_the_seven_by_their_own_is_x_override(tmp_path):
    """Leaf detection is STRUCTURAL (a class that directly defines its own
    `is_<app>()`), not a hand-maintained class-name list — the same "never
    hand-maintain a count of a code class" lesson the ratchet's own scan
    (above) already paid for once."""
    from check_app_gate_ratchet import _parse_driver_classes, leaf_app_drivers

    drivers = tmp_path / "drivers"
    drivers.mkdir()
    (drivers / "base.py").write_text(
        textwrap.dedent(
            """
            class PlatformDriver:
                def is_tui(self) -> bool:
                    return False
                def is_linux(self) -> bool:
                    return False
            """
        )
    )
    (drivers / "http_bridge.py").write_text(
        textwrap.dedent(
            """
            class HttpBridgeDriver(PlatformDriver):
                pass
            """
        )
    )
    (drivers / "tui.py").write_text(
        textwrap.dedent(
            """
            class TuiDriver(HttpBridgeDriver):
                def is_tui(self) -> bool:
                    return True
            """
        )
    )
    (drivers / "linux.py").write_text(
        textwrap.dedent(
            """
            class LinuxBridgeDriver(HttpBridgeDriver):
                def is_linux(self) -> bool:
                    return True
            """
        )
    )
    classes = _parse_driver_classes(drivers)
    assert leaf_app_drivers(classes) == {"TuiDriver": "tui", "LinuxBridgeDriver": "linux"}


def test_capability_debt_walks_the_mro_to_find_the_resolving_class(tmp_path):
    """The exact pre-fix `supports_unclean_kill` shape: tui overrides it
    directly, linux inherits it from the intermediate `HttpBridgeDriver` —
    both must be reported with the class that actually answers, not just a
    yes/no "is it overridden"."""
    from check_app_gate_ratchet import capability_debt

    drivers = tmp_path / "drivers"
    drivers.mkdir()
    base = drivers / "base.py"
    base.write_text(
        textwrap.dedent(
            """
            class PlatformDriver:
                def is_tui(self) -> bool:
                    return False
                def is_linux(self) -> bool:
                    return False
                def supports_unclean_kill(self) -> bool:
                    \"\"\"default False.\"\"\"
                    return False
            """
        )
    )
    (drivers / "http_bridge.py").write_text(
        textwrap.dedent(
            """
            class HttpBridgeDriver(PlatformDriver):
                def supports_unclean_kill(self) -> bool:
                    return getattr(self, "_app_proc", None) is not None
            """
        )
    )
    (drivers / "tui.py").write_text(
        textwrap.dedent(
            """
            class TuiDriver(HttpBridgeDriver):
                def is_tui(self) -> bool:
                    return True
                def supports_unclean_kill(self) -> bool:
                    return True
            """
        )
    )
    (drivers / "linux.py").write_text(
        textwrap.dedent(
            """
            class LinuxBridgeDriver(HttpBridgeDriver):
                def is_linux(self) -> bool:
                    return True
            """
        )
    )
    debt = capability_debt(drivers, base)
    assert debt["supports_unclean_kill"]["tui"] == "TuiDriver"
    assert debt["supports_unclean_kill"]["linux"] == "HttpBridgeDriver"


# ── The regression proof for the fourth hidden mode ───────────────────────
def test_app_capabilities_resolves_tui_and_declares_its_sections():
    """tui must be a REAL entry, not a fall-through to "unknown".

    Before this landed, `has_capability(tui_driver, "feed")` answered False for
    every section, so `check_state_section` skipped and the "declares a
    capability but returned null" assertion was unreachable for tui — while both
    honesty suites collected for it and appeared to pass.
    """
    import app_capabilities

    driver = _driver_for("tui")
    assert app_capabilities.app_name(driver) == "tui"
    assert "tui" in app_capabilities.CAPABILITIES
    # The four sections apps/fauna-tui/src/automation.rs serializes non-null.
    for section in ("feed", "contacts", "notifications", "events"):
        assert app_capabilities.has_capability(driver, section), section


# ── The conftest wiring ───────────────────────────────────────────────────
# The helpers' behaviour is proved above by setting the flag directly. These two
# prove the part that direct calls cannot: that `--strict-app` actually REACHES
# the helper, and that the tally actually reaches the terminal. Without them the
# flag could be inert and every proof above would still pass.
class _StubConfig:
    """A config answering every option `conftest.pytest_configure` reads.

    Only `strict_app` is this pin's SUBJECT. The rest are answered with
    deliberately inert values so the hook can run to completion without the
    stub perturbing global harness state for the tests that follow:
    `strict_nest` is always False (never `strict`, which would leave nest-strict
    latched on for the rest of the process), and `nest` is None, which
    `resolve_nest_mode` reads as "flag absent" → the standalone default.

    It still ASSERTS on an unknown name rather than returning a blanket
    default: a stub that silently answers anything would keep passing while
    `pytest_configure` grew a flag it never actually pushed, which is precisely
    the failure this pin exists to catch. So a new option here is a deliberate
    one-line decision about what the stub should say — not a silent pass.
    """

    def __init__(self, strict: bool):
        self._strict = strict

    def getoption(self, name):
        if name == "strict_app":
            return self._strict
        if name == "strict_nest":
            return False
        if name == "nest":
            return None
        if name == "macos_artifact":
            # Inert: this stub drives `pytest_configure`'s app-strictness push,
            # and the artifact opt-in (`--macos-artifact`, the tier_4 shipped
            # `Fauna.app`/DMG arm) is not the subject of any pin here. False is
            # what an ordinary run passes.
            return False
        if name == "fail_on_skip":
            # Inert: `--fail-on-skip` is release-gate machinery (e2e-conventions.md
            # convention 7, the gate-run half) read by the same terminal-summary
            # hook this pin drives. False is what every non-gate run passes, and
            # it keeps the hook's other axis silent so the unbuilt-surface tally
            # stays this pin's only subject.
            return False
        if name == "device_serial":
            # Inert: the android device axis (`--device-serial`) is not the
            # subject of any pin here. `None` is what `resolve_device_serial`
            # reads as "flag absent" — the same posture `nest` takes above.
            return None
        if name == "adb_server":
            # Inert: the adb server the android device hangs off (`--adb-server`)
            # is not the subject of any pin here. `None` is what
            # `resolve_adb_server` reads as "flag absent" — the same posture
            # `device_serial` takes above.
            return None
        if name == "live_box":
            # Inert: the live-box declaration (`--live-box shared|disposable`) is
            # not the subject of any pin here. `None` is what `declare_box` reads
            # as "flag absent" — the same posture `nest` takes above.
            return None
        raise AssertionError(
            f"_StubConfig was asked for an option it has no answer for: {name!r}. "
            f"`conftest.pytest_configure` grew a new option — decide what this "
            f"stub should say for it (an inert value unless it is the subject of "
            f"a pin), rather than making the stub answer anything."
        )


class _StubReporter:
    def __init__(self):
        self.lines: list[str] = []

    def write_sep(self, _sep, title, **_kwargs):
        self.lines.append(title)

    def write_line(self, line, **_kwargs):
        self.lines.append(line)


def _unbuilt_section(reporter):
    """The unbuilt-surface section's own lines, sliced out of a summary run.

    `conftest.pytest_terminal_summary` fans out to FOUR other axes before this
    one — nest-mode gates + the feature ledger, WS-RPC reconnects, frame
    invariants, and `--fail-on-skip` — and every one of them reads
    process-global state that any earlier test in the run legitimately fills.
    So an assertion on the WHOLE reporter is not an assertion about this
    section at all: it pins convention 7's tally to every other axis's data,
    and reds whenever a sibling axis has something true to say. It did, twice —
    the reconnect axis, then the frame-invariants axis — and each time the fix
    was to clear that one axis, which both misses the next axis and DESTROYS
    the run's real tally for the axis it clears (the exact sin
    `app_surface.reset_unbuilt_hits`' own docstring spells out).

    Slicing is exact instead, and needs no global state touched: this section is
    the hook's LAST writer and opens with its own `unbuilt app surfaces:`
    header, so everything from that header on is the section and nothing else
    can be. `test_unbuilt_section_is_independent_of_every_other_summary_axis`
    is what keeps that true.
    """
    for i, line in enumerate(reporter.lines):
        if line.startswith("unbuilt app surfaces:"):
            return reporter.lines[i:]
    return []


@pytest.mark.parametrize("strict", [True, False])
def test_pytest_configure_pushes_the_flag_into_app_surface(strict):
    import conftest

    app_surface.set_strict_app(not strict)  # start from the wrong value
    conftest.pytest_configure(_StubConfig(strict))
    assert app_surface.strict_app_enabled() is strict


def test_terminal_summary_reports_the_tally_and_names_the_surfaces():
    import conftest

    with pytest.raises(pytest.skip.Exception):
        skip_unbuilt(_driver_for("tui"), surface="the admin-dns page")
    with pytest.raises(pytest.skip.Exception):
        skip_unbuilt(_driver_for("tui"), surface="the admin-dns page")
    with pytest.raises(pytest.skip.Exception):
        skip_unbuilt(_driver_for("web"), surface="some-other-surface")

    reporter = _StubReporter()
    conftest.pytest_terminal_summary(reporter, 0, _StubConfig(False))
    blob = "\n".join(_unbuilt_section(reporter))
    assert "unbuilt app surfaces: 3 test(s) skipped" in blob
    assert "tui: 2" in blob
    assert "the admin-dns page (x2)" in blob
    assert "web: 1" in blob
    # The lenient run must say how to make them red, or the number is inert.
    assert "--strict-app" in blob


def test_terminal_summary_stays_silent_when_nothing_was_unbuilt():
    """A clean run must not print a header — an empty section reads as noise.

    Asserts the SECTION, not the reporter: the hook's four other axes are none
    of this pin's business, and clearing them to make room for it is how this
    pin twice ate another axis's real data. See `_unbuilt_section`.
    """
    import conftest

    reporter = _StubReporter()
    conftest.pytest_terminal_summary(reporter, 0, _StubConfig(False))
    assert _unbuilt_section(reporter) == []


def test_unbuilt_section_is_independent_of_every_other_summary_axis(monkeypatch):
    """The pin above must stay silent while every SIBLING axis is loud.

    This is the proof the two single-axis fixes could not be. A run of this
    file alone never exercises the collision at all — `_report_frame_invariants`
    early-returns while `conftest._frame_tally` is None, and nothing in a tier_1
    selection observes a frame — so the pin passed in isolation on every box and
    reds only from inside a bigger run, ORDER-dependent, which convention 14
    forbids outright. Seeding all four axes makes the independence a property of
    the code rather than of what happened to run first.

    The axes are stubbed at their READERS rather than filled through their real
    tallies, deliberately: those tallies are the run's own data, read once at
    session end, and a self-test that writes them is the truncation hazard
    `app_surface.reset_unbuilt_hits` documents at length.
    """
    import conftest

    monkeypatch.setattr(
        conftest, "_report_nest_mode_gates",
        lambda reporter, config: reporter.write_line("nest axis spoke"),
    )
    monkeypatch.setattr(
        conftest, "_report_ws_rpc_reconnects",
        lambda reporter: reporter.write_line("reconnect axis spoke"),
    )
    monkeypatch.setattr(
        conftest, "_report_frame_invariants",
        lambda reporter: reporter.write_line("frame axis spoke"),
    )
    monkeypatch.setattr(
        conftest, "_fail_on_skip_violation",
        lambda reasons, flag: "skip axis spoke",
    )

    reporter = _StubReporter()
    conftest.pytest_terminal_summary(reporter, 0, _StubConfig(False))
    # The seeding has to have REACHED the reporter, or this proves nothing: a
    # stub that silently never ran would leave the section trivially empty.
    spoke = "\n".join(reporter.lines)
    for axis in (
        "nest axis spoke", "reconnect axis spoke", "frame axis spoke",
        "skip axis spoke",
    ):
        assert axis in spoke, axis
    assert _unbuilt_section(reporter) == []

    # ...and with a hit, the section is the hit and nothing else: no sibling
    # axis's line leaks in, which is the half that proves the slice is a slice.
    with pytest.raises(pytest.skip.Exception):
        skip_unbuilt(_driver_for("web"), surface="some-surface")
    reporter = _StubReporter()
    conftest.pytest_terminal_summary(reporter, 0, _StubConfig(False))
    section = "\n".join(_unbuilt_section(reporter))
    assert "unbuilt app surfaces: 1 test(s) skipped" in section
    assert "web: 1" in section
    assert "axis spoke" not in section


# ── conftest's own app-not-built skips ──
# Six fixtures resolve a built app binary/path for a DIRECT (non-cached)
# driver launch. Before this fix each used a bare `pytest.skip(...)`, which
# `check_app_gate_ratchet.py`'s scan cannot see (there is no `is_<app>()`
# guard to key on — the app identity is implicit in which fixture ran) and
# `--strict-app` cannot turn red — exactly the shape that let
# `test_offline_share_two_seat.py` report "4 skipped" as a clean run when
# `debug/fauna-tui` simply did not exist.
#
# ⚠ Five of these six fixtures BUILD before they resolve (adopted 2026-08-27;
# `tui_app_path`'s docstring carries the measured incident), so every proof that
# reaches one of the five stubs `_ensure_app_built` — all of them but android's,
# whose fixture deliberately does not build. The SUBJECT here is the routing
# decision the fixture makes AFTER the build, never the build. Left unstubbed, a
# proof becomes a property of the box: four of them ran a real `just linux-debug`
# / `just tui-debug` on every machine, and on macOS — which has no GTK — the
# linux pair died `RuntimeError: building the 'linux' app failed on BOTH the original
# attempt and the retry` while the tui pair charged a machine-wide build-slot
# wait to a tier_1 proof the owning doc records as "50 passed in 0.13 s". That
# is `e2e-conventions.md` convention 7's own Mode 7 — the harness putting a
# build in front of THIS file — recurring one layer in, and a proof tier that
# reds for reasons no app change can fix is how a box learns to ignore an
# unbuilt-surface red.
#
# The stub is the fix. `skip_environment` would be wrong (nothing about the
# routing decision is a box property, so the declaration would leave this
# convention's own proof unrun on macOS), and taking the build out of the
# fixture would be worse still. `test_app_path_fixture_builds_before_it_resolves`
# below is what keeps the stubbed-away build honest.
def test_linux_app_path_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_resolve_linux_binary", lambda: None)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.linux_app_path.__wrapped__()
    assert "linux has not built the fauna-desktop binary" in str(excinfo.value)
    assert "just linux-debug" in str(excinfo.value)


def test_tui_app_path_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_resolve_cli_binary", lambda: None)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.tui_app_path.__wrapped__()
    assert "tui has not built the fauna-tui binary" in str(excinfo.value)
    assert "cargo build -p fauna-tui" in str(excinfo.value)


def test_android_app_path_routes_through_skip_unbuilt(monkeypatch, tmp_path):
    import conftest

    monkeypatch.setattr(conftest, "_repo_root", tmp_path)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.android_app_path.__wrapped__()
    assert "android has not built the Android debug APK" in str(excinfo.value)
    assert "just android-debug" in str(excinfo.value)


def test_macos_app_path_routes_through_skip_unbuilt(monkeypatch, tmp_path):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_repo_root", tmp_path)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.macos_app_path.__wrapped__()
    assert "macos has not built the macOS app binary" in str(excinfo.value)
    assert "just mac-debug" in str(excinfo.value)


def test_windows_app_path_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_resolve_windows_app", lambda: None)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.windows_app_path.__wrapped__()
    assert "windows has not built the Windows app binary" in str(excinfo.value)
    assert "just windows-debug" in str(excinfo.value)


def test_ios_setup_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_get_ios_setup", lambda: None)
    with pytest.raises(pytest.skip.Exception) as excinfo:
        conftest.ios_setup.__wrapped__()
    assert "ios has not built the iOS simulator/app" in str(excinfo.value)
    assert "just apple-ffi-test" in str(excinfo.value)


def test_app_path_fixture_fails_under_strict_app(monkeypatch):
    """The core of row 335: a missing binary must be able to go RED, not just
    print a bigger number — mirrors `test_skip_unbuilt_fails_under_strict_app`
    but through the real fixture rather than a direct `skip_unbuilt` call."""
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_resolve_linux_binary", lambda: None)
    app_surface.set_strict_app(True)
    with pytest.raises(pytest.fail.Exception) as excinfo:
        conftest.linux_app_path.__wrapped__()
    assert "--strict-app" in str(excinfo.value)
    assert "linux has not built the fauna-desktop binary" in str(excinfo.value)


def test_app_path_fixture_is_tallied_as_unbuilt(monkeypatch):
    """The scan-blind-spot half of row 335: these six sites never reached
    `_UNBUILT_HITS` before, so the terminal summary's tally under-counted."""
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_resolve_cli_binary", lambda: None)
    with pytest.raises(pytest.skip.Exception):
        conftest.tui_app_path.__wrapped__()
    assert app_surface.unbuilt_hits() == [
        ("tui", "the fauna-tui binary", "run 'cargo build -p fauna-tui' first")
    ]


# The five direct-launch fixtures that BUILD before they resolve, and how to make
# each one resolve nothing so it reaches its skip. `android_app_path` is
# deliberately absent — it is the one that does NOT build (its own docstring owns
# the reason), pinned by `test_android_app_path_does_not_build` below.
_BUILDING_APP_PATH_FIXTURES = {
    "ios": (
        "ios_setup",
        lambda mp, ct, tmp: mp.setattr(ct, "_get_ios_setup", lambda: None),
    ),
    "linux": (
        "linux_app_path",
        lambda mp, ct, tmp: mp.setattr(ct, "_resolve_linux_binary", lambda: None),
    ),
    "macos": (
        "macos_app_path",
        lambda mp, ct, tmp: mp.setattr(ct, "_repo_root", tmp),
    ),
    "tui": (
        "tui_app_path",
        lambda mp, ct, tmp: mp.setattr(ct, "_resolve_cli_binary", lambda: None),
    ),
    "windows": (
        "windows_app_path",
        lambda mp, ct, tmp: mp.setattr(ct, "_resolve_windows_app", lambda: None),
    ),
}


@pytest.mark.parametrize("app", sorted(_BUILDING_APP_PATH_FIXTURES))
def test_app_path_fixture_builds_before_it_resolves(app, monkeypatch, tmp_path):
    """Every proof in the group above stubs `_ensure_app_built` — so SOMETHING
    has to prove the call is still there.

    Without this pair the stubs would quietly turn the build into an unproven
    claim: delete `_ensure_app_built(...)` from all five fixtures and every other
    proof in this file stays green, while the direct-launch suites go back to
    driving whatever binary was last built. Not hypothetical —
    `tui_app_path`'s docstring records the measured cost: a "green" journey run
    drove a 55-minute-old binary and failed on the pre-fix shape,
    indistinguishable from the fix not working. Asserting the app NAME rather
    than merely that something was built is what catches a copy-paste that
    builds the wrong app.
    """
    import conftest

    built: list[str] = []
    monkeypatch.setattr(conftest, "_ensure_app_built", lambda name: built.append(name))
    fixture_name, make_unresolvable = _BUILDING_APP_PATH_FIXTURES[app]
    make_unresolvable(monkeypatch, conftest, tmp_path)

    with pytest.raises(pytest.skip.Exception):
        getattr(conftest, fixture_name).__wrapped__()
    assert built == [app]


def test_android_app_path_does_not_build(monkeypatch, tmp_path):
    """The one deliberate exception, pinned so it stays deliberate.

    `just android-debug` is FFI plus two Gradle assembles, and android e2e
    additionally needs a connected `adb` device that no build step can provide —
    so this fixture skips rather than builds. Drift either way is what the pair
    catches: android growing a build it cannot satisfy, or one of the five above
    losing theirs.
    """
    import conftest

    built: list[str] = []
    monkeypatch.setattr(conftest, "_ensure_app_built", lambda name: built.append(name))
    monkeypatch.setattr(conftest, "_repo_root", tmp_path)

    with pytest.raises(pytest.skip.Exception):
        conftest.android_app_path.__wrapped__()
    assert built == []


# ── `_build_app_config`'s own ios/macos skips (the remaining half) ──
# The direct-launch fixtures above are one of TWO places the fix
# landed. `_build_app_config` is the path the CACHED `app` fixture uses for
# nearly every macos/ios e2e test — its own ios branch and its two macos
# branches (bare binary + `--macos-artifact` bundle) had the same bare-skip
# shape, fixed the same way, but the landed commit exercised them only
# through the mechanism-level `skip_unbuilt` proofs, not through the real
# function. These three close that gap.
def test_build_app_config_ios_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app_name: None)
    monkeypatch.setattr(conftest, "_get_ios_setup", lambda: None)

    app_surface.set_strict_app(True)
    with pytest.raises(pytest.fail.Exception) as excinfo:
        conftest._build_app_config("ios", {"url": "http://unused"}, None)
    assert "--strict-app" in str(excinfo.value)
    assert "ios has not built the iOS simulator/app" in str(excinfo.value)


def test_build_app_config_macos_bare_binary_routes_through_skip_unbuilt(monkeypatch):
    import conftest

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app_name: None)
    monkeypatch.setattr(conftest, "_MACOS_ARTIFACT_MODE", False)
    monkeypatch.setitem(conftest.APP_PATHS, "macos", "nonexistent-test-macos-binary")

    app_surface.set_strict_app(True)
    with pytest.raises(pytest.fail.Exception) as excinfo:
        conftest._build_app_config("macos", {"url": "http://unused"}, None)
    assert "--strict-app" in str(excinfo.value)
    assert "macos has not built the macOS app binary" in str(excinfo.value)


def test_build_app_config_macos_artifact_routes_through_skip_unbuilt(monkeypatch):
    import conftest
    import helpers.macos_artifact as macos_artifact

    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app_name: None)
    monkeypatch.setattr(conftest, "_MACOS_ARTIFACT_MODE", True)
    monkeypatch.setattr(
        macos_artifact, "ARTIFACT_APP_RELPATH", "nonexistent-test-bundle.app"
    )

    app_surface.set_strict_app(True)
    with pytest.raises(pytest.fail.Exception) as excinfo:
        conftest._build_app_config("macos", {"url": "http://unused"}, None)
    assert "--strict-app" in str(excinfo.value)
    assert "macos has not built the macOS app bundle" in str(excinfo.value)


def test_declared_capability_returning_null_is_an_error_not_a_skip():
    """The point of declaring: null becomes a failure instead of a shrug."""
    import app_capabilities

    driver = _driver_for("tui")
    with pytest.raises(AssertionError, match="declares 'feed' capability"):
        app_capabilities.check_state_section(driver, {"data": {"feed": None}}, "feed")


# ── the local-arm content classes cannot reach web ───
#
# `Contact` and `File` search hits exist only in backend 2, the app's sealed
# LOCAL search index. Web registers no local arm and never will (no Tantivy on
# wasm — structural, not parity debt), and the nest arm's `content_fts` has
# exactly two writers, neither of which emits a contact- or file-class row. So
# on web those result sets are empty by construction and the two navigation
# tests polled until their 90 s budget expired — a deterministic red that read
# like a bug in seeding or in the search UI.
#
# The gate is `declared_absence`, so `--strict-app` must NOT red it: there is no
# surface to build. These two cases pin the logic deterministically; that web
# actually collects and runs those tests is already measured (it is how the
# timeout was found), so nothing here needs a browser.
def test_local_arm_content_classes_are_a_declared_absence_on_web():
    from helpers.app_surface import skip_if_no_local_search_arm

    for content_class in ("Contact", "File"):
        with pytest.raises(pytest.skip.Exception) as excinfo:
            skip_if_no_local_search_arm(
                _driver_for("web"), content_class=content_class
            )
        message = str(excinfo.value)
        assert "web" in message
        assert content_class in message
        # A declared absence with no declaration is unbuilt debt wearing a
        # better name — `declared_absence` enforces the citation, and this pins
        # that the citation we pass is the one that actually declares it.
        assert "search.md" in message


def test_local_arm_content_classes_still_run_on_an_app_that_has_the_arm():
    """The gate must not quietly disable the legs it exists to keep honest."""
    from helpers.app_surface import skip_if_no_local_search_arm

    for app in ("tui", "linux"):
        # No raise: these apps register a local arm, so the class can reach them
        # and the assertion below the gate is a real one.
        skip_if_no_local_search_arm(_driver_for(app), content_class="Contact")


def test_local_arm_absence_survives_strict_app():
    """`--strict-app` reds unbuilt debt; a declared absence must still skip.

    Without this the fix would trade a 90 s timeout for a strict-mode failure —
    the same red, one flag away.
    """
    from helpers.app_surface import skip_if_no_local_search_arm

    app_surface.set_strict_app(True)
    try:
        with pytest.raises(pytest.skip.Exception):
            skip_if_no_local_search_arm(
                _driver_for("web"), content_class="Contact"
            )
    finally:
        app_surface.set_strict_app(False)


# ── a phone seat builds no index of its own ─────────────────────────────────
#
# iOS and Android register the local search ARM but not the BUILDER
# (`CLIENT_BUILDS_INDEX` is false there — `libs/fauna-ffi/src/index_launch.rs`),
# so a single-seat journey that needs content THIS seat seeded to be found by
# its own local index can never pass on a phone: nobody indexes it. That is the
# ratified build-vs-query split (`content-index.md` § Build vs. query), not
# parity debt — so it must survive `--strict-app` — and it is worded apart from
# web's missing arm.
def test_a_phone_seat_declares_it_builds_no_index():
    from helpers.app_surface import skip_if_seat_builds_no_index

    for app in ("ios", "android"):
        app_surface.set_strict_app(True)
        try:
            with pytest.raises(pytest.skip.Exception) as excinfo:
                skip_if_seat_builds_no_index(
                    _driver_for(app), what="a draft this seat wrote"
                )
        finally:
            app_surface.set_strict_app(False)
        message = str(excinfo.value)
        assert app in message
        assert "a draft this seat wrote" in message
        assert "content-index.md § Build vs. query" in message


def test_a_builder_seat_runs_the_journey():
    """The gate must not disable the legs it exists to keep honest — every
    desktop seat builds, and web's missing ARM is a different declaration."""
    from helpers.app_surface import skip_if_seat_builds_no_index

    for app in ("tui", "linux", "macos", "windows", "web"):
        skip_if_seat_builds_no_index(_driver_for(app), what="a draft")
