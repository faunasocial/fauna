"""Tests for scripts/features_lint.py — the six rules of the catalog lint.

Each test builds a whole miniature world in `tmp_path`: a goal tree the citations can
resolve into, a features directory, and an e2e tree the marker scan reads. That is
what makes a rule's *failure* testable — the tree the repo actually ships is (by the
time step 3 lands) green by construction, and a lint nobody has seen red is a lint
nobody knows works.
"""
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))
import features_lint as fl
import features_catalog as fc


GOAL = """\
# Feed

## Reading

Text.

## Compose

Text.
"""

GUIDE = """\
# App tour

## Feed

Text.
"""


def build(tmp_path, *, contract, absences="", what="Your feed shows posts.",
          tests_py=None, slug="feed-read", section="everyday", extra_section=""):
    """A complete fixture world; returns (features dir, e2e dir, repo root)."""
    repo = tmp_path
    (repo / "docs" / "goal" / "ui").mkdir(parents=True, exist_ok=True)
    (repo / "docs" / "goal" / "ui" / "feed.md").write_text(GOAL, encoding="utf-8")
    (repo / "docs" / "guides").mkdir(parents=True, exist_ok=True)
    (repo / "docs" / "guides" / "app-tour.md").write_text(GUIDE, encoding="utf-8")

    features = repo / "docs" / "features"
    (features / "ledger").mkdir(parents=True, exist_ok=True)
    absence_block = f"absences:\n{absences}\n" if absences else ""
    (features / f"{slug}.md").write_text(
        "---\n"
        f"slug: {slug}\n"
        "title: Read your feed\n"
        f"section: {section}\n"
        "goal: docs/goal/ui/feed.md § Reading\n"
        "guide: docs/guides/app-tour.md § Feed\n"
        f"{absence_block}"
        "---\n\n"
        "## What a user gets\n\n"
        f"{what}\n\n"
        "## Coverage contract\n\n"
        "Stamped 2026-08-26 at 8213688bbc.\n\n"
        f"{contract}\n\n"
        f"{extra_section}"
        "## Status\n\n"
        f"{fc.BEGIN}\n{fc.END}\n",
        encoding="utf-8")

    e2e = repo / "tests" / "e2e-unified"
    (e2e / "tests").mkdir(parents=True, exist_ok=True)
    if tests_py is not None:
        (e2e / "tests" / "test_feed.py").write_text(tests_py, encoding="utf-8")
    return features, e2e, repo


def run(features, e2e, repo, *, render=True):
    if render:
        fc.write_all(features)
    return fl.lint(features, e2e, repo)


TAGGED = '''\
import pytest


@pytest.mark.feature("feed-read")
def test_own_post_appears(app):
    pass
'''

CONTRACT_ONE = (
    "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
    "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
)


def test_a_well_formed_catalog_is_green(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    assert run(features, e2e, repo) == []


# ── rule 1 ──────────────────────────────────────────────────────────────────
def test_rule_1_flags_a_citation_whose_heading_does_not_exist(tmp_path):
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Nowhere\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    problems = run(features, e2e, repo)
    assert any("no heading matching § Nowhere" in p for p in problems)


def test_rule_1_flags_a_citation_whose_doc_does_not_exist(tmp_path):
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/gone.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    assert any("gone.md does not exist" in p for p in run(features, e2e, repo))


def test_rule_1_accepts_a_citation_naming_the_head_of_a_longer_heading(tmp_path):
    (tmp_path / "docs" / "goal" / "ui").mkdir(parents=True, exist_ok=True)
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    (repo / "docs" / "goal" / "ui" / "feed.md").write_text(
        "# Feed\n\n## Reading — how the timeline is assembled\n\nText.\n", encoding="utf-8")
    assert run(features, e2e, repo) == []


def test_rule_1_flags_an_absence_that_is_not_a_citation(tmp_path):
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
        absences="  android: not a citation at all")
    assert any("absences[android]" in p for p in run(features, e2e, repo))


# ── rule 2 ──────────────────────────────────────────────────────────────────
def test_rule_2_flags_a_mapped_test_that_does_not_exist(tmp_path):
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_absent`"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    assert any("does not exist in the tree" in p for p in run(features, e2e, repo))


def test_rule_2_flags_a_mapped_test_without_the_marker(tmp_path):
    untagged = "def test_own_post_appears(app):\n    pass\n"
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=untagged)
    assert any("does not carry" in p for p in run(features, e2e, repo))


def test_rule_2_accepts_a_module_level_pytestmark(tmp_path):
    module_mark = (
        'import pytest\n\n'
        'pytestmark = pytest.mark.feature("feed-read")\n\n\n'
        'def test_own_post_appears(app):\n    pass\n'
    )
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=module_mark)
    assert run(features, e2e, repo) == []


# ── rule 3 ──────────────────────────────────────────────────────────────────
def test_rule_3_flags_a_marker_no_contract_asked_for(tmp_path):
    extra = TAGGED + '\n\n@pytest.mark.feature("feed-read")\ndef test_stray(app):\n    pass\n'
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=extra)
    assert any("a witness nobody asked for" in p for p in run(features, e2e, repo))


def test_rule_3_flags_a_marker_naming_no_page(tmp_path):
    extra = TAGGED + '\n\n@pytest.mark.feature("no-such-feature")\ndef test_x(app):\n    pass\n'
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=extra)
    assert any("names no page in docs/features/" in p for p in run(features, e2e, repo))


# ── rule 4 ──────────────────────────────────────────────────────────────────
def test_rule_4_flags_a_nest_outcome_whose_test_drives_an_app(tmp_path):
    contract = (
        "1. [nest] A deleted post stops being served — `docs/goal/ui/feed.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
    )
    features, e2e, repo = build(tmp_path, contract=contract,
                                what=f"Nothing here. There is {fl.NO_CHOICE_PHRASE}.",
                                tests_py=TAGGED)
    assert any("launches an app driver" in p for p in run(features, e2e, repo))


def test_rule_4_flags_an_app_outcome_whose_test_drives_nothing(tmp_path):
    driverless = ('import pytest\n\n\n@pytest.mark.feature("feed-read")\n'
                  'def test_own_post_appears(nest_instance):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=driverless)
    assert any("launches no app driver" in p for p in run(features, e2e, repo))


def test_rule_4_counts_a_playwright_module_as_app_driving(tmp_path):
    browser = ('import pytest\nfrom playwright.sync_api import sync_playwright\n\n\n'
               '@pytest.mark.feature("feed-read")\n'
               'def test_own_post_appears(nest_instance):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=browser)
    assert run(features, e2e, repo) == []


def test_rule_4_counts_a_conftest_fixture_derived_from_app_as_app_driving(tmp_path):
    """`ungranted_app(request, app, nest_instance)` and its ~25 siblings in the real
    conftest launch a driver by requesting `app`; a test requesting THEM drives an
    app too. Found by transitive closure over the conftest, never by a hand list —
    two hops deep here, so a one-level lookup would still fail."""
    derived = ('import pytest\n\n\n@pytest.mark.feature("feed-read")\n'
               'def test_own_post_appears(seeded_app):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=derived)
    (e2e / "conftest.py").write_text(
        'import pytest\n\n\n@pytest.fixture\ndef granted_app(request, app, nest_instance):\n'
        '    return app\n\n\n@pytest.fixture(scope="function")\ndef seeded_app(granted_app):\n'
        '    return granted_app\n', encoding="utf-8")
    assert run(features, e2e, repo) == []


def test_rule_4_counts_a_module_local_fixture_derived_from_app_as_app_driving(tmp_path):
    """A test module's own `@pytest.fixture def two_account_app(app)` is the other
    place a derived door lives (the account-switcher suites are built this way)."""
    local = ('import pytest\n\n\n@pytest.fixture\ndef two_account_app(app):\n    return app\n\n\n'
             '@pytest.mark.feature("feed-read")\n'
             'def test_own_post_appears(two_account_app):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=local)
    assert run(features, e2e, repo) == []


def test_rule_4_counts_a_module_importing_the_driver_factory_as_app_driving(tmp_path):
    """`from drivers import create_driver` is how the bespoke harnesses (`pin_env`,
    `launch_harness`, `tray_app`, the account-switcher suites) obtain a driver
    without ever requesting `app` — module-wide, like the playwright rule."""
    factory = ('import pytest\nfrom drivers import create_driver\n\n\n'
               '@pytest.fixture\ndef pin_env(request, tmp_path):\n'
               '    return create_driver("linux")\n\n\n'
               '@pytest.mark.feature("feed-read")\n'
               'def test_own_post_appears(pin_env):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=factory)
    assert run(features, e2e, repo) == []


def test_rule_4_counts_a_class_autouse_fixture_requesting_a_door_as_app_driving(tmp_path):
    """`class TestDeviceCards: @pytest.fixture(autouse=True) def setup(self,
    logged_in_app, …)` drives an app in every method, though each requests only
    `self`."""
    klass = ('import pytest\n\n\nclass TestCards:\n'
             '    @pytest.fixture(autouse=True)\n'
             '    def setup(self, logged_in_app):\n        self.app = logged_in_app\n\n'
             '    @pytest.mark.feature("feed-read")\n'
             '    def test_own_post_appears(self):\n        pass\n')
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::TestCards::test_own_post_appears`"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=klass)
    assert run(features, e2e, repo) == []


def test_rule_4_does_not_count_a_fixture_that_never_reaches_a_door(tmp_path):
    """The closure must stop at real doors: a fixture chain that never requests one
    (a nest-side helper) still reads as nest-side, so rule 4 keeps its teeth."""
    local = ('import pytest\n\n\n@pytest.fixture\ndef seeded_nest(nest_instance):\n'
             '    return nest_instance\n\n\n@pytest.mark.feature("feed-read")\n'
             'def test_own_post_appears(seeded_nest):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=local)
    assert any("launches no app driver" in p for p in run(features, e2e, repo))


# ── rule 5 ──────────────────────────────────────────────────────────────────
def test_rule_5_flags_an_unrendered_catalog(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    problems = run(features, e2e, repo, render=False)
    assert any("MATRIX.md is stale" in p for p in problems)
    assert any("matrix.json is stale" in p for p in problems)


def test_rule_5_flags_a_hand_edited_status_block(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    fc.write_all(features)
    page = features / "feed-read.md"
    page.write_text(page.read_text(encoding="utf-8").replace(
        fc.BEGIN, fc.BEGIN + "\n| hand | edited |"), encoding="utf-8")
    assert any("feed-read.md is stale" in p for p in fl.lint(features, e2e, repo))


# ── rule 6 ──────────────────────────────────────────────────────────────────
def test_rule_6_flags_a_stale_absence(tmp_path):
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
        absences="  android: docs/goal/ui/feed.md § Compose")
    (features / "ledger" / "android.json").write_text(json.dumps({
        "schema": 1, "app": "android", "features": {"feed-read": {
            "cell": {"stamp": "0.1.2-dev+abc standalone", "date": "2026-08-26",
                     "complete": True},
            "tests": {"tests/e2e-unified/tests/test_feed.py::test_own_post_appears": {
                "linux": {
                    "outcome": "passed", "skip_class": None,
                    "stamp": "0.1.2-dev+abc standalone", "version": "0.1.2",
                    "commit": "abc", "image_digest": None, "nest_mode": "standalone",
                    "date": "2026-08-26", "platform": "linux"}}}}}}), encoding="utf-8")
    assert any("the declaration is stale" in p for p in run(features, e2e, repo))


CONTRACT_TWO = CONTRACT_ONE + (
    "\n2. [app] Composing shows the thumbnail — `docs/goal/ui/feed.md` § Compose\n"
    "   - `tests/e2e-unified/tests/test_feed.py::test_compose`")

TAGGED_TWO = TAGGED + '''

@pytest.mark.feature("feed-read")
def test_compose(app):
    pass
'''


def _android_passed(features, test):
    (features / "ledger" / "android.json").write_text(json.dumps({
        "schema": 1, "app": "android", "features": {"feed-read": {
            "cell": {"stamp": "0.1.2-dev+abc standalone", "date": "2026-08-26",
                     "complete": True},
            "tests": {f"tests/e2e-unified/tests/test_feed.py::{test}": {
                "linux": {
                    "outcome": "passed", "skip_class": None,
                    "stamp": "0.1.2-dev+abc standalone", "version": "0.1.2",
                    "commit": "abc", "image_digest": None, "nest_mode": "standalone",
                    "date": "2026-08-26", "platform": "linux"}}}}}}), encoding="utf-8")


def test_rule_1_flags_an_outcome_absence_that_is_not_a_citation(tmp_path):
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_TWO, tests_py=TAGGED_TWO,
        absences="  android (outcome 2): not a citation at all")
    assert any("absences[android (outcome 2)]" in p for p in run(features, e2e, repo))


def test_rule_6_flags_a_stale_outcome_absence(tmp_path):
    """The per-outcome form is stale on the same evidence as the page-level one: a
    witness of THAT outcome passed on the column declared absent from it."""
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_TWO, tests_py=TAGGED_TWO,
        absences="  android (outcome 2): docs/goal/ui/feed.md § Compose")
    _android_passed(features, "test_compose")
    assert any("from outcome 2" in p and "the declaration is stale" in p
               for p in run(features, e2e, repo))


def test_rule_6_accepts_an_outcome_absence_beside_a_pass_on_an_owed_outcome(tmp_path):
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_TWO, tests_py=TAGGED_TWO,
        absences="  android (outcome 2): docs/goal/ui/feed.md § Compose")
    _android_passed(features, "test_own_post_appears")
    assert not any("stale" in p for p in run(features, e2e, repo))


def test_rule_6_flags_a_nest_only_page_that_does_not_say_there_is_no_choice(tmp_path):
    contract = (
        "1. [nest] A receiving server sees a signature — `docs/goal/ui/feed.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
    )
    driverless = ('import pytest\n\n\n@pytest.mark.feature("feed-read")\n'
                  'def test_own_post_appears(nest_instance):\n    pass\n')
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=driverless,
                                what="Mail you send is signed.")
    assert any("What a user gets" in p for p in run(features, e2e, repo))


def test_rule_6_accepts_a_nest_only_page_that_says_so(tmp_path):
    contract = (
        "1. [nest] A receiving server sees a signature — `docs/goal/ui/feed.md` § Reading\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`"
    )
    driverless = ('import pytest\n\n\n@pytest.mark.feature("feed-read")\n'
                  'def test_own_post_appears(nest_instance):\n    pass\n')
    features, e2e, repo = build(
        tmp_path, contract=contract, tests_py=driverless,
        what=f"Mail you send is signed. There is {fl.NO_CHOICE_PHRASE}.")
    assert run(features, e2e, repo) == []


# ── rule 2, the shipping halves ─────────────────────────────────────────────
# (The real `docs/features/` being green lives in test_features_repo_catalog.py:
# it reads this checkout's ledger, which does not ship, so it stays out of the
# fixture-world file that does.)
def _ships(dropped: str):
    return lambda rel: rel != dropped


def test_rule_2_flags_an_unmarked_witness_whose_file_does_not_ship(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    fc.write_all(features)
    problems = fl.lint(features, e2e, repo, ships=_ships("tests/e2e-unified/tests/test_feed.py"))
    assert any("does not ship" in p and fc.MAINTAINER_ONLY in p for p in problems), problems


def test_rule_2_accepts_a_maintainer_only_witness_whose_file_does_not_ship(tmp_path):
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
        f"   - {fc.maintainer_only_witness('tests/e2e-unified/tests/test_feed.py::test_own_post_appears')}"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    fc.write_all(features)
    assert fl.lint(features, e2e, repo, ships=_ships("tests/e2e-unified/tests/test_feed.py")) == []


def test_rule_2_flags_a_maintainer_only_mark_whose_file_ships(tmp_path):
    """A mark that outlived its reason hides real, public evidence from the reader."""
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
        f"   - {fc.maintainer_only_witness('tests/e2e-unified/tests/test_feed.py::test_own_post_appears')}"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    fc.write_all(features)
    problems = fl.lint(features, e2e, repo, ships=lambda rel: True)
    assert any("outlived its reason" in p for p in problems), problems


def test_rule_2_marked_witness_still_needs_to_exist_and_carry_the_marker(tmp_path):
    """The mark changes how a citation is SPELLED where it ships, nothing else:
    the internal tree still proves the test exists and is tagged."""
    contract = (
        "1. [app] A person sees their own posts — `docs/goal/ui/feed.md` § Reading\n"
        f"   - {fc.maintainer_only_witness('tests/e2e-unified/tests/test_feed.py::test_absent')}"
    )
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    fc.write_all(features)
    problems = fl.lint(features, e2e, repo, ships=_ships("tests/e2e-unified/tests/test_feed.py"))
    assert any("does not exist in the tree" in p for p in problems), problems


def test_without_a_shipping_predicate_the_halves_are_not_evaluated(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    assert run(features, e2e, repo) == []


def test_a_stray_outcome_line_outside_the_contract_is_refused(tmp_path):
    """A numbered outcome or witness line sitting in another section (a page-added
    `## Workstream`, say) parses as nothing today, so every gate — including this
    one — must refuse the page rather than stay green."""
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
        extra_section=(
            "## Workstream\n\n"
            "9. [app] x — `docs/goal/ui/feed.md` § Reading\n"
            "   - (none)\n\n"))
    problems = run(features, e2e, repo, render=False)
    assert len(problems) == 1 and "feed-read.md:" in problems[0] and \
        "Workstream" in problems[0], problems


def test_a_missing_ledger_is_one_precise_refusal(tmp_path):
    """A public clone (the ledger does not ship) gets one finding naming the
    cause, never a hundred "stale render" ones — and never a blanking render."""
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    fc.write_all(features)
    (features / "ledger").rmdir()
    problems = fl.lint(features, e2e, repo)
    assert len(problems) == 1 and "does not ship" in problems[0], problems
    with pytest.raises(fc.CatalogError, match="does not ship"):
        fc.write_all(features)


# ── the scan memo ───────────────────────────────────────────────────────────
def test_a_rewritten_test_file_is_scanned_again(tmp_path):
    """`features_scan.scan_tree` memoizes, and its key is a content stamp.

    The memo is why the gate walks the tree once instead of twice: `lint()` asks
    for the whole tree, and so does `features_catalog.scan_app_marks()` through
    rule 5's render — the same question, previously parsed and walked 757 test
    modules apart (5.77 s -> 2.15 s on Windows, 2026-09-05, with the fused walk).

    The hazard a memo introduces is answering a later question about the same
    PATH with an earlier tree's facts, which is precisely what a fixture world
    under `tmp_path` does. So the key carries each file's mtime and size — and
    that is what this pins, because a path-only key passes every other test
    here, each of which builds its world once.
    """
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    assert run(features, e2e, repo) == []

    # Same path, new content: the marker the contract needs is gone, so rule 2
    # must see the CURRENT tree and complain.
    (e2e / "tests" / "test_feed.py").write_text(
        "import pytest\n\n\ndef test_own_post_appears(app):\n    pass\n",
        encoding="utf-8")
    problems = run(features, e2e, repo, render=False)
    assert any("test_own_post_appears" in p for p in problems), problems


def test_a_driver_factory_import_inside_a_fixture_still_drives_an_app(tmp_path):
    """The three per-module questions are answered by ONE `ast.walk` now
    (`features_scan._module_facts`), so the pass has to keep reaching the places
    the three separate walks reached: an import nested inside a function body,
    not just at module top level. Rule 4 is the observer — an [app] outcome whose
    test launches no driver is its red — so a module that drives an app only
    through a nested `create_driver` import must stay green.
    """
    nested = (
        "import pytest\n\n\n"
        "@pytest.fixture\n"
        "def browser_app():\n"
        "    from drivers import create_driver\n"
        "    return create_driver('web')\n\n\n"
        '@pytest.mark.feature("feed-read")\n'
        "def test_own_post_appears():\n"
        "    pass\n"
    )
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=nested)
    assert run(features, e2e, repo) == []


def test_rule_4_counts_the_headless_store_launcher_as_app_driving(tmp_path):
    """`from helpers import tui_headless_store as hs` then `hs.headless_launch(...)`
    — the tui re-key journey's door (`test_tui_credential_store_rekey.py`). Until
    2026-09-26 it borrowed the launcher from a SIBLING TEST MODULE, a door the
    closure (conftest + the module under scan) cannot see, so the journey read as
    launching no app and no page could cite it; the launcher is a shared helper
    now, registered like the seat factories, and the aliased module form must
    read app-driving exactly like the name import."""
    aliased = ('import pytest\nfrom helpers import tui_headless_store as hs\n\n\n'
               '@pytest.mark.feature("feed-read")\n'
               'def test_own_post_appears(request):\n'
               '    with hs.headless_launch("tui", {}, "/tmp", request):\n'
               '        pass\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=aliased)
    assert run(features, e2e, repo) == []


def test_rule_4_counts_a_module_import_of_a_seat_factory_module_as_app_driving(tmp_path):
    """`from helpers import sync_seats` then `sync_seats.start_seats(...)` is the
    module form of the `("helpers.sync_seats", "start_seats")` pair — the seat
    rounds (`test_filesync_seats.py`) launch tui and native seats through it, so
    it must read app-driving exactly like the name import."""
    module_form = ('import pytest\nfrom helpers import sync_seats\n\n\n'
                   '@pytest.mark.feature("feed-read")\n'
                   'def test_own_post_appears():\n'
                   '    sync_seats.start_seats(["tui"])\n')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=module_form)
    assert run(features, e2e, repo) == []
