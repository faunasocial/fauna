"""tier_1: a release gate must not be able to pass by running NOTHING.

The defect this pins is the sibling of finding (closed
2026-08-13): that one was a gate that proved a *commit string* while the
pipeline promoted an *artifact*; this one is a gate that proves an *exit code*
while its suite never executed. `pytest -m cd_suite` exits 0 when every
selected test SKIPS — and every `cd_suite` test skips silently on a missing
`FAUNA_LIVE_*` env var, an unbuilt app binary, or an unreachable port. So the
promote-after-verify pipeline's future CD gate (`build-system.md` § Image tags & channels) could promote `:latest` on the
strength of a summary line that reads like success in exactly the way
convention 7 warns about ("a skip is not coverage"). Measured on the primary
dev VM with no `FAUNA_LIVE_*` set (2026-08-13): `10 skipped, 2971 deselected in
1392.13s`, **exit 0**.

`--fail-on-skip` is the fix: for a gate run, a skip IS a failure, because the
run's whole claim is "this suite executed green against the deployed artifact".
It is deliberately broader than `--strict-app`/`--strict-nest` (which each red
only their own helper's skips): a gate does not care *why* its suite failed to
run, only that it did.

What is pinned here, without spawning pytest or touching a box:

  1. Without the flag, skips are never a violation (the ordinary inner loop is
     untouched — this is opt-in gate machinery, not a new suite-wide law).
  2. With the flag and no skips, there is no violation.
  3. With the flag and skips, the message names EVERY skipped test *and* its
     reason — a gate that reds without naming what did not run just moves the
     mystery.
  4. The four curated `cd_suite` modules still carry the marker (drift guard,
     same shape as `test_live_box_opt_in_gate.py`).
  5. Which `cd_suite` tests need a real app binary is DECLARED, not discovered
     at the first live run. The self-hosted runner — the only one that can
     reach the IP-locked staging box — has docker + python and no rust/just/uv
     toolchain (`tier4-deploy-e2e.yml`'s own header), so a driver-dependent
     test cannot execute there. Today 3 of the 4 need one; that count is a
     ratchet, and
     the declaration is what keeps a future `cd_suite` marking from silently
     widening the gap.
"""

import ast
import re
from pathlib import Path

import pytest

import conftest

pytestmark = pytest.mark.tier_1

_TESTS_DIR = Path(__file__).parent

# The curated CD suite (pytest.ini § cd_suite). Value = whether the module's
# tests drive a real app binary, i.e. whether the CD gate can run it on the
# toolchain-light self-hosted runner.
_CD_SUITE = {
    "test_mail_enable_live_nest.py": True,
    "test_mail_zero_cheat_live.py": True,
    "test_activitypub_live.py": True,
    "test_mail_port25_inbound_live.py": False,
}


def _module_takes_app_fixture(path: Path) -> bool:
    """True when any module-level `test_*` takes the `app` fixture — the fixture
    that launches a real app binary (conftest `_resolve_linux_binary` and its
    siblings), which is what the self-hosted runner cannot provide."""
    tree = ast.parse(path.read_text())
    for node in tree.body:
        if not isinstance(node, ast.FunctionDef) or not node.name.startswith("test_"):
            continue
        names = {a.arg for a in node.args.args}
        if names & {"app", "logged_in_app", "admin_app"}:
            return True
    return False


def test_a_skip_is_not_a_violation_without_the_flag():
    """The ordinary inner loop is untouched: skips stay skips."""
    assert conftest._fail_on_skip_violation({"t::a": "no live creds"}, False) is None


def test_no_skips_is_no_violation_under_the_flag():
    assert conftest._fail_on_skip_violation({}, True) is None


def test_the_violation_names_every_skipped_test_and_its_reason():
    message = conftest._fail_on_skip_violation(
        {
            "tests/test_a.py::test_one": "believable live-nest mail test: set FAUNA_LIVE_NEST_URL",
            "tests/test_b.py::test_two": "fauna-desktop not built",
        },
        True,
    )
    assert message, "skips under --fail-on-skip must red the run"
    for nodeid, reason in (
        ("tests/test_a.py::test_one", "FAUNA_LIVE_NEST_URL"),
        ("tests/test_b.py::test_two", "fauna-desktop not built"),
    ):
        assert nodeid in message, f"{nodeid} missing from the gate failure"
        assert reason in message, f"the reason for {nodeid} missing from the gate failure"
    assert "--fail-on-skip" in message


def test_the_curated_cd_suite_modules_still_carry_the_marker():
    """Drift guard: if an edit drops `cd_suite`, the release gate silently
    stops covering that journey."""
    for name in _CD_SUITE:
        src = (_TESTS_DIR / name).read_text()
        assert re.search(r"pytest\.mark\.cd_suite", src), (
            f"{name} lost its `cd_suite` marker — the promote-after-verify gate "
            "would stop running it and nothing else would say so"
        )


@pytest.mark.parametrize("name,needs_binary", sorted(_CD_SUITE.items()))
def test_cd_suite_app_binary_dependence_is_declared(name, needs_binary):
    """Whether each CD-suite test can run on the CI runner is a declared fact,
    not a discovery made during a live release."""
    actual = _module_takes_app_fixture(_TESTS_DIR / name)
    assert actual == needs_binary, (
        f"{name} now {'needs' if actual else 'does not need'} a real app binary, but "
        f"_CD_SUITE declares {'needs' if needs_binary else 'does not need'}. A "
        "driver-dependent CD test cannot run on the self-hosted runner (docker "
        "+ python only) — "
        "update the declaration and Phase 2 together, so the "
        "gap stays visible instead of surfacing at the first live release."
    )


# ── The gate must be able to RUN its destructive residents (2026-10-04) ──────
#
# `--nest live` carries class (3), the shared-box policy, which deselects the
# two mail residents at collection — a deselect, not a skip, so `--fail-on-skip`
# never sees it and the gate passes green having run neither (measured against
# the staging box 2026-10-04). The run declares the box disposable instead
# (`--live-box disposable`, `testing.md` § The shared-box rule → *The
# disposable-box declaration*), and the recipe is where that declaration lives.

_JUSTFILE = Path(__file__).resolve().parents[3] / "justfile"

#: The residents class (3) deselects on a shared box: the believable round
#: trip reaches `fauna.admin.factory_reset`; the UI-only receive approves
#: bridges on the admin Bridges page.
_DESTRUCTIVE_RESIDENTS = (
    "test_believable_live_mail_roundtrip",
    "test_live_mail_receive_ui_only",
)


class _Item:
    """What `nest_surface.classify` reads off a pytest item."""

    def __init__(self, name, fixturenames=("app",)):
        self.name = self.originalname = name
        self.fixturenames = list(fixturenames)

    def iter_markers(self):
        return iter(())


def _cd_gate_recipe_body() -> str:
    text = _JUSTFILE.read_text()
    match = re.search(r"^e2e-cd-gate URL \*ARGS:\n((?:[ \t]+.*\n)+)", text, re.M)
    assert match, "the `e2e-cd-gate` recipe is gone from the justfile"
    return match.group(1)


def test_the_gate_recipe_carries_every_load_bearing_part():
    """Four parts, each the fix for a way the gate passed green having proved
    nothing: the live URL (no collection-time nest build), the disposable
    declaration (class (3) off, so the residents are selected), the marker
    (the curated suite) and `--fail-on-skip` (a skip is a failure)."""
    body = _cd_gate_recipe_body()
    for part in ("-m cd_suite", "--nest live:{{URL}}", "--live-box disposable", "--fail-on-skip"):
        assert part in body, f"`e2e-cd-gate` lost `{part}`:\n{body}"


@pytest.mark.parametrize("name", _DESTRUCTIVE_RESIDENTS)
def test_a_destructive_resident_is_selected_only_under_the_declaration(name):
    """The real classifier over the real test, as the recipe's invocation and
    a bare `--nest live` would classify it: deselected as class (3) on a shared
    box, eligible once the run declares the box disposable."""
    from helpers import nest_mode as nm, nest_surface as ns

    item = _Item(name)
    shared = nm.parse_nest_mode("live:https://dev.example.com")
    verdict = ns.classify(item, shared)
    assert verdict is not None and verdict.rule == ns.RULE_GLOBAL_ADMIN, (
        f"{name} is no longer class (3) on a shared box — if it stopped being "
        f"destructive, drop it from _DESTRUCTIVE_RESIDENTS; got {verdict}"
    )
    disposable = nm.declare_box(shared, nm.BOX_DISPOSABLE)
    assert ns.classify(item, disposable) is None, (
        f"{name} is still deselected on a declared-disposable box — the CD gate "
        "would pass green without running it"
    )


def test_the_residents_are_cd_suite_members():
    """The two names above are the gate's own: each is a test function in a
    module `_CD_SUITE` lists, so a rename reds here rather than silently
    pinning a test the gate never runs."""
    found = set()
    for module in _CD_SUITE:
        tree = ast.parse((_TESTS_DIR / module).read_text())
        found |= {n.name for n in tree.body if isinstance(n, ast.FunctionDef)}
    missing = set(_DESTRUCTIVE_RESIDENTS) - found
    assert not missing, f"{sorted(missing)} are not test functions of any cd_suite module"
