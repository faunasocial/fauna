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
import features_scan as fs


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
          tests_py=None, slug="feed-read", section="everyday", extra_section="", ui_pages=""):
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
        f"{'ui-pages: ' + ui_pages + chr(10) if ui_pages else ''}"
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


def test_rule_1_accepts_ui_pages_naming_real_ui_yaml_pages(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
                                ui_pages="feed, compose")
    (e2e / "ui.yaml").write_text("pages:\n  feed:\n    a: 1\n  compose:\n    b: 2\n",
                                 encoding="utf-8")
    assert run(features, e2e, repo) == []


def test_rule_1_flags_a_ui_page_key_ui_yaml_does_not_have(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
                                ui_pages="feed, nowhere")
    (e2e / "ui.yaml").write_text("pages:\n  feed:\n    a: 1\nelements:\n  nowhere:\n    b: 2\n",
                                 encoding="utf-8")
    problems = run(features, e2e, repo)
    assert any("ui-pages: `nowhere`" in p for p in problems)
    assert not any("`feed`" in p for p in problems)


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


# ── rule 7 — the surface registry (e2e-conventions.md convention 7) ─────────
SURFACES_TOML = '[events-day-cell]\nkind = "product"\nbuilt = ["tui"]\n'


def test_rule_7_accepts_a_marker_naming_a_declared_surface(tmp_path):
    tagged = TAGGED.replace('@pytest.mark.feature("feed-read")',
                            '@pytest.mark.feature("feed-read")\n'
                            '@pytest.mark.requires_surface("events-day-cell")')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=tagged)
    (e2e / "surfaces.toml").write_text(SURFACES_TOML, encoding="utf-8")
    assert run(features, e2e, repo) == []


def test_rule_7_flags_a_marker_naming_an_undeclared_surface(tmp_path):
    tagged = TAGGED.replace('@pytest.mark.feature("feed-read")',
                            '@pytest.mark.feature("feed-read")\n'
                            '@pytest.mark.requires_surface("events-day-cel")')
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=tagged)
    (e2e / "surfaces.toml").write_text(SURFACES_TOML, encoding="utf-8")
    assert any('requires_surface("events-day-cel") names no table' in p
               for p in run(features, e2e, repo))


def test_rule_7_flags_a_malformed_registry(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    (e2e / "surfaces.toml").write_text(
        '[events-day-cell]\nkind = "product"\nbuilt = ["tvos"]\n', encoding="utf-8")
    # The render reads the registry too (the declared route), so it refuses the same
    # file — a hard stop, never a partial read; the lint reports it either way.
    with pytest.raises(fc.CatalogError, match="built names no app tvos"):
        fc.write_all(features)
    assert any("built names no app tvos" in p for p in run(features, e2e, repo, render=False))


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


# ── rule 6: stale ledger records (§ The ledger, *Stale records*) ─────────────
def _rec(outcome):
    return {"linux": {
        "outcome": outcome, "skip_class": None, "stamp": "0.1.2-dev+abc standalone",
        "version": "0.1.2", "commit": "abc", "image_digest": None,
        "nest_mode": "standalone", "date": "2026-08-26", "platform": "linux"}}


def _tui_ledger(features, feats):
    (features / "ledger" / "tui.json").write_text(
        json.dumps({"schema": 2, "app": "tui", "features": feats}, indent=2, sort_keys=True)
        + "\n", encoding="utf-8")


MAPPED = "tests/e2e-unified/tests/test_feed.py::test_own_post_appears"
MOVED = "tests/e2e-unified/tests/test_feed.py::test_a_test_re_marked_elsewhere"


def _stale_world(tmp_path):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    _tui_ledger(features, {
        "feed-read": {"tests": {MAPPED: _rec("passed"), MOVED: _rec("failed")}},
        "gone-feature": {"cell": {"stamp": "x", "date": "2026-08-26", "complete": True},
                         "tests": {MAPPED: _rec("passed")}},
    })
    return features, e2e, repo


def test_rule_6_flags_a_record_under_a_feature_that_no_longer_maps_its_test(tmp_path):
    features, e2e, repo = _stale_world(tmp_path)
    problems = run(features, e2e, repo)
    assert any("ledger/tui.json: 1 record(s) under `feed-read`" in p and MOVED in p
               for p in problems)
    # a feature whose page is gone is stale too
    assert any("under `gone-feature`" in p for p in problems)
    # the mapped record is not reported
    assert not any(f"`{MAPPED}`" in p and "feed-read" in p for p in problems)


def test_prune_removes_exactly_the_stale_records_and_the_lint_goes_green(tmp_path):
    features, e2e, repo = _stale_world(tmp_path)
    removed = fc.prune_ledger(features)
    assert removed == [("tui", "feed-read", MOVED, None), ("tui", "gone-feature", MAPPED, None)]
    left = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))
    assert list(left["features"]) == ["feed-read"]
    assert list(left["features"]["feed-read"]["tests"]) == [MAPPED]
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []  # idempotent


def _platform_rec(outcome, platform, date="2026-08-26", stamp="0.1.2-dev+abc standalone"):
    return {**_rec(outcome)["linux"], "platform": platform, "date": date, "stamp": stamp}


def _cell(date, stamp):
    return {"stamp": stamp, "date": date, "complete": True, "dirty": False, "base": None}


def _unrunnable_world(tmp_path):
    """web on windows is a pair no machine can run any more (build-system.md § The Deno
    build sandbox); tui on windows is one it still can."""
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED)
    linux_cell, windows_cell = (_cell("2026-10-20", "0.1.5-dev+bbb standalone"),
                                _cell("2026-09-14", "0.1.2-dev+abc standalone"))
    both = {"linux": _platform_rec("passed", "linux", "2026-10-20", linux_cell["stamp"]),
            "windows": _platform_rec("error", "windows", "2026-09-14")}
    for app in ("web", "tui"):
        (features / "ledger" / f"{app}.json").write_text(json.dumps({
            "schema": 2, "app": app, "features": {"feed-read": {
                "cell": {**windows_cell, "platforms": {"linux": linux_cell,
                                                       "windows": windows_cell}},
                "tests": {MAPPED: dict(both)}}}}, indent=2, sort_keys=True) + "\n",
            encoding="utf-8")
    return features, e2e, repo


def test_rule_6_flags_a_record_on_a_platform_its_column_can_no_longer_run_on(tmp_path):
    features, e2e, repo = _unrunnable_world(tmp_path)
    problems = run(features, e2e, repo)
    assert any("ledger/web.json" in p and "`windows`" in p and "feed-read" in p
               for p in problems)
    # tui still runs on windows: its windows record is a live observation
    assert not any("ledger/tui.json" in p for p in problems)


def test_prune_drops_an_unrunnable_platform_record_and_keeps_a_runnable_one(tmp_path):
    features, e2e, repo = _unrunnable_world(tmp_path)
    removed = fc.prune_ledger(features)
    assert removed == [("web", "feed-read", None, "windows"),
                       ("web", "feed-read", MAPPED, "windows")]
    web = json.loads((features / "ledger" / "web.json").read_text(encoding="utf-8"))
    entry = web["features"]["feed-read"]
    assert list(entry["tests"][MAPPED]) == ["linux"]
    # the cell re-folds over the platforms left: the windows stamp no longer holds it
    assert list(entry["cell"]["platforms"]) == ["linux"]
    assert entry["cell"]["stamp"] == "0.1.5-dev+bbb standalone"
    tui = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))
    assert sorted(tui["features"]["feed-read"]["tests"][MAPPED]) == ["linux", "windows"]
    assert sorted(tui["features"]["feed-read"]["cell"]["platforms"]) == ["linux", "windows"]
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []  # idempotent


# ── ruling (g): the nest file is pruned by the venues its set holds ──────────
NEST_WITNESS = "tests/e2e-unified/tests/api/test_feed_api.py::test_feed_over_the_wire"
DOCKER_WITNESS = ("tests/e2e-unified/tests/platform/docker/test_feed_image.py"
                  "::test_feed_served_by_the_image")
CONTRACT_NEST = (
    CONTRACT_ONE + "\n"
    "2. [nest] The nest serves the feed over the wire — `docs/goal/ui/feed.md` § Reading\n"
    f"   - `{NEST_WITNESS}`"
)
CONTRACT_NEST_DOCKER = CONTRACT_NEST + f"\n   - `{DOCKER_WITNESS}`"
# A linux-agent and a windows-agent witness of one outcome, each a `skipif` on the
# other OS — the `notifications` outcome-8 shape (ruling (h)).
LINUX_WITNESS = ("tests/e2e-unified/tests/platform/linux/test_agent_push_arm.py"
                 "::test_a_frame_is_posted_once_the_app_closes")
WINDOWS_WITNESS = ("tests/e2e-unified/tests/platform/windows/test_agent_push_arm.py"
                   "::test_a_frame_is_toasted_once_the_app_closes")
CONTRACT_NEST_LEGS = CONTRACT_NEST + f"\n   - `{LINUX_WITNESS}`\n   - `{WINDOWS_WITNESS}`"
NEST_TAGGED = '''\
import pytest


@pytest.mark.feature("feed-read")
def {name}():
    pass
'''


def _nest_world(tmp_path, *, contract, platforms, tests):
    """A page with a `[nest]` outcome witnessed by an ordinary `tests/api/` test and —
    under `CONTRACT_NEST_DOCKER` — a `tests/platform/docker/` one, which runs only
    where a docker daemon is (`fc.NEST_VENUE_PLATFORMS`) — or, under
    `CONTRACT_NEST_LEGS`, a linux-only and a windows-only one (ruling (h)). The
    top-level cell is the OLDEST platform's, as the writer folded before the legs."""
    features, e2e, repo = build(tmp_path, contract=contract, tests_py=TAGGED)
    for node_id in (NEST_WITNESS, DOCKER_WITNESS, LINUX_WITNESS, WINDOWS_WITNESS):
        if node_id not in contract:
            continue  # a tagged test no page cites is a rule-3 finding of its own
        rel, name = node_id.split("::")
        path = repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(NEST_TAGGED.format(name=name), encoding="utf-8")
    cell = dict(min(platforms.values(), key=lambda c: c["date"]))
    (features / "ledger" / "nest.json").write_text(json.dumps({
        "schema": 2, "app": "nest", "features": {"feed-read": {
            "cell": {**cell, "platforms": platforms}, "tests": tests}}},
        indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return features, e2e, repo


MACOS_CELL = _cell("2026-09-20", "0.1.2-dev+abc standalone")
LINUX_CELL = _cell("2026-10-20", "0.1.5-dev+bbb standalone")


def test_venue_platforms_narrow_a_docker_witness_to_where_a_daemon_is():
    assert fc.venue_platforms(DOCKER_WITNESS) == frozenset({"linux"})
    assert fc.venue_platforms(NEST_WITNESS) == fc.PLATFORMS
    assert fc.set_platforms(None) == fc.PLATFORMS


def test_rule_6_flags_a_nest_stamp_on_a_platform_that_cannot_complete_a_docker_bearing_set(tmp_path):
    """The macos stamp was a complete observation of the set BEFORE the docker witness
    joined it; no macos run can ever see the whole set again (the module skips through
    a plain `skipif`, which the writer records as nothing), so the stamp is stale."""
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_DOCKER,
        platforms={"macos": MACOS_CELL, "linux": LINUX_CELL},
        tests={NEST_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                              "macos": _platform_rec("passed", "macos", "2026-09-20")},
               DOCKER_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20")}})
    problems = run(features, e2e, repo)
    assert any("ledger/nest.json" in p and "`macos`" in p and "feed-read" in p
               and "cannot complete" in p for p in problems), problems
    assert not any("`linux`" in p for p in problems)


def test_rule_6_keeps_a_nest_stamp_from_any_platform_when_the_set_holds_no_docker_witness(tmp_path):
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST,
        platforms={"macos": MACOS_CELL},
        tests={NEST_WITNESS: {"macos": _platform_rec("passed", "macos", "2026-09-20")}})
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []


def test_prune_drops_the_unreachable_nest_stamp_and_refolds_over_the_platforms_left(tmp_path):
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_DOCKER,
        platforms={"macos": MACOS_CELL, "linux": LINUX_CELL},
        tests={NEST_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                              "macos": _platform_rec("passed", "macos", "2026-09-20")},
               DOCKER_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20")}})
    assert fc.prune_ledger(features) == [("nest", "feed-read", None, "macos")]
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert list(entry["cell"]["platforms"]) == ["linux"]
    assert entry["cell"]["stamp"] == LINUX_CELL["stamp"]
    # the ordinary witness's macos record is a live observation: it runs there
    assert sorted(entry["tests"][NEST_WITNESS]) == ["linux", "macos"]
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []  # idempotent


def test_prune_removes_the_nest_cell_outright_when_no_platform_that_can_complete_the_set_stamped_it(tmp_path):
    """The measured case (2026-10-30, `admin-forwarders` / `admin-mail-policy`): macos
    the ONLY stamp. Unstamped is the honest state — records exist, no complete
    observation of the current set does — until a linux run collects the whole set."""
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_DOCKER,
        platforms={"macos": MACOS_CELL},
        tests={NEST_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                              "macos": _platform_rec("passed", "macos", "2026-09-20")},
               DOCKER_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20")}})
    assert fc.prune_ledger(features) == [("nest", "feed-read", None, "macos")]
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert "cell" not in entry
    assert sorted(entry["tests"]) == [NEST_WITNESS, DOCKER_WITNESS]
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []


def test_prune_drops_a_docker_witness_record_keyed_by_a_platform_its_venue_cannot_run_on(tmp_path):
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_DOCKER,
        platforms={"linux": LINUX_CELL},
        tests={NEST_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                              "macos": _platform_rec("passed", "macos", "2026-09-20")},
               DOCKER_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                                "macos": _platform_rec("failed", "macos", "2026-09-20")}})
    assert fc.prune_ledger(features) == [("nest", "feed-read", DOCKER_WITNESS, "macos")]
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert list(entry["tests"][DOCKER_WITNESS]) == ["linux"]
    assert sorted(entry["tests"][NEST_WITNESS]) == ["linux", "macos"]
    assert run(features, e2e, repo) == []


# ── ruling (h): a nest set with platform-exclusive witnesses is observed in legs ──
WINDOWS_CELL = _cell("2026-10-09", "0.1.3-dev+ccc standalone")


def _legs_tests(*, macos=False):
    tests = {NEST_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                            "windows": _platform_rec("passed", "windows", "2026-10-09")},
             LINUX_WITNESS: {"linux": _platform_rec("passed", "linux", "2026-10-20")},
             WINDOWS_WITNESS: {"windows": _platform_rec("passed", "windows", "2026-10-09")}}
    if macos:
        tests[NEST_WITNESS]["macos"] = _platform_rec("passed", "macos", "2026-09-20")
    return tests


def test_a_platform_exclusive_venue_runs_on_its_own_os_alone():
    assert fc.venue_platforms(LINUX_WITNESS) == frozenset({"linux"})
    assert fc.venue_platforms(WINDOWS_WITNESS) == frozenset({"windows"})
    assert fc.venue_platforms("tests/e2e-unified/tests/platform/ios/test_x.py::test_y") == frozenset({"macos"})
    assert fc.venue_platforms("tests/e2e-unified/tests/platform/macos/test_x.py::test_y") == frozenset({"macos"})


def test_nest_legs_follow_the_venue_table(tmp_path):
    """One leg any platform can run for an ordinary set; one `linux` leg for a set with a
    docker witness (ruling (g)'s reading, kept); a `linux` and a `windows` leg for a set
    with a witness exclusive to each, `macos` redundant under both."""
    for contract, legs, may_stamp in (
            (CONTRACT_NEST, ({"linux", "macos", "windows"},), fc.PLATFORMS),
            (CONTRACT_NEST_DOCKER, ({"linux"},), {"linux"}),
            (CONTRACT_NEST_LEGS, ({"linux"}, {"windows"}), {"linux", "windows"})):
        features, _e2e, _repo = _nest_world(tmp_path / contract[-20:].replace("`", "").replace("/", "_"),
                                            contract=contract, platforms={"linux": LINUX_CELL}, tests={})
        page = fc.load_pages(features)[0]
        assert fc.legs(page) == tuple(frozenset(l) for l in legs), contract
        assert fc.set_platforms(page) == frozenset(may_stamp), contract
        assert set(fc.scope_by_platform(page)) == set(may_stamp), contract
    assert fc.legs(None) == () and fc.set_platforms(None) == fc.PLATFORMS


def test_rule_6_refolds_a_nest_cell_whose_top_level_claims_a_leg_it_lacks(tmp_path):
    """The measured case (2026-10-31, `notifications`): a linux stamp alone, the
    windows leg never stamped — yet the top level read complete, holding every column
    at the linux stamp's date. The honest top level has no stamp until the windows
    leg lands; the linux stamp waits underneath."""
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_LEGS,
        platforms={"linux": LINUX_CELL}, tests=_legs_tests())
    problems = run(features, e2e, repo)
    assert any("ledger/nest.json" in p and "feed-read" in p and "not the fold" in p
               and "rulings (h) and (i)" in p for p in problems), problems
    assert fc.prune_ledger(features) == []  # nothing dropped: the stamp is live
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"] == {"complete": False, "platforms": {"linux": LINUX_CELL}}
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []  # idempotent


def test_a_stamp_on_every_leg_folds_the_nest_cell_complete_at_the_oldest(tmp_path):
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_LEGS,
        platforms={"linux": LINUX_CELL, "windows": WINDOWS_CELL}, tests=_legs_tests())
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"]["complete"] is True
    assert entry["cell"]["stamp"] == WINDOWS_CELL["stamp"]  # the older leg shows


def test_rule_6_flags_a_nest_stamp_on_a_platform_on_no_leg(tmp_path):
    """`macos` can run the ordinary witness alone — a set `linux`'s and `windows`'
    both contain outright — so its stamp claims nothing a sibling does not, and
    would hold the top level at its own older date for good."""
    features, e2e, repo = _nest_world(
        tmp_path, contract=CONTRACT_NEST_LEGS,
        platforms={"linux": LINUX_CELL, "windows": WINDOWS_CELL, "macos": MACOS_CELL},
        tests=_legs_tests(macos=True))
    problems = run(features, e2e, repo)
    assert any("ledger/nest.json" in p and "`macos`" in p and "cannot complete" in p for p in problems), problems
    assert fc.prune_ledger(features) == [("nest", "feed-read", None, "macos")]
    entry = json.loads((features / "ledger" / "nest.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert sorted(entry["cell"]["platforms"]) == ["linux", "windows"]
    assert entry["cell"]["stamp"] == WINDOWS_CELL["stamp"]
    # the ordinary witness's macos record is a live observation: it runs there
    assert sorted(entry["tests"][NEST_WITNESS]) == ["linux", "macos", "windows"]
    assert run(features, e2e, repo) == []


def test_the_real_notifications_page_is_observed_in_a_linux_and_a_windows_leg():
    """Pinned against the live catalog: outcome 8's two agent witnesses."""
    pages = {p.slug: p for p in fc.load_pages(fc.FEATURES)}
    if "notifications" not in pages:
        pytest.skip("no notifications page in this tree")
    assert fc.legs(pages["notifications"]) == (frozenset({"linux"}), frozenset({"windows"}))


def test_rule_6_absence_ignores_a_passing_record_for_an_unmapped_test(tmp_path):
    features, e2e, repo = build(
        tmp_path, contract=CONTRACT_ONE, tests_py=TAGGED,
        absences="  tui: docs/goal/ui/feed.md § Compose")
    _tui_ledger(features, {"feed-read": {"tests": {MOVED: _rec("passed")}}})
    problems = run(features, e2e, repo)
    assert not any("the declaration is stale" in p for p in problems)
    assert any("no longer maps" in p for p in problems)


# ── the surface registry's declarations (rules 6 and 7, § Cell semantics) ────
NOT_ON_TUI = '[feed-own]\nkind = "product"\nbuilt = ["web", "linux"]\n'
ON_TUI = '[feed-own]\nkind = "product"\nbuilt = ["web", "linux", "tui"]\n'
MARKED = TAGGED.replace('def test_own_post_appears',
                        '@pytest.mark.requires_surface("feed-own")\ndef test_own_post_appears')


def _registry_world(tmp_path, registry, *, tests_py=TAGGED, records=None):
    features, e2e, repo = build(tmp_path, contract=CONTRACT_ONE, tests_py=tests_py)
    (e2e / "surfaces.toml").write_text(registry, encoding="utf-8")
    if records is not None:
        _tui_ledger(features, {"feed-read": {"tests": {MAPPED: records}}})
    return features, e2e, repo


def _unbuilt_rec(surface="feed-own"):
    rec = _rec("skipped")
    rec["linux"].update(skip_class="unbuilt", surface=surface, surfaces=[surface])
    return rec


def test_rule_6_flags_a_stale_unbuilt_record_by_name(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, ON_TUI, records=_unbuilt_rec())
    problems = run(features, e2e, repo)
    assert any("stale `skipped: unbuilt` record" in p and MAPPED in p and "`feed-own`" in p
               for p in problems)


def test_an_unbuilt_record_on_a_column_still_unbuilt_is_not_stale(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI, records=_unbuilt_rec())
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []


def test_a_pre_registry_unbuilt_record_is_left_for_the_migrations_last_step(tmp_path):
    legacy = _rec("skipped")
    legacy["linux"]["skip_class"] = "unbuilt"
    features, e2e, repo = _registry_world(tmp_path, ON_TUI, records=legacy)
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []


def test_unbuilt_without_surface_prune_drops_only_the_surfaceless_unbuilt_records(tmp_path):
    """The migration's one-time last step (`--unbuilt-without-surface`): the
    pre-registry record goes, a record that names its surface stays, and it is
    idempotent."""
    legacy = _rec("skipped")
    legacy["linux"]["skip_class"] = "unbuilt"
    keyed = _unbuilt_rec()["linux"]
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI, records=legacy)
    _tui_ledger(features, {"feed-read": {"tests": {MAPPED: {
        "linux": legacy["linux"], "windows": keyed}}}})
    assert fc.prune_ledger(features) == []  # the default prune leaves them
    assert fc.prune_ledger(features, unbuilt_without_surface=True) == [
        ("tui", "feed-read", MAPPED, "linux")]
    left = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))
    assert list(left["features"]["feed-read"]["tests"][MAPPED]) == ["windows"]
    assert fc.prune_ledger(features, unbuilt_without_surface=True) == []  # idempotent


def test_prune_drops_a_stale_unbuilt_record_and_the_lint_goes_green(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, ON_TUI, records=_unbuilt_rec())
    assert fc.prune_ledger(features) == [("tui", "feed-read", MAPPED, "linux")]
    left = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))
    assert left["features"]["feed-read"]["tests"] == {}
    assert run(features, e2e, repo) == []
    assert fc.prune_ledger(features) == []  # idempotent


def test_rule_7_flags_a_declaration_a_passing_record_refutes(tmp_path):
    """Lint (a): the map names the surface (here through the marker) and the test
    PASSED on an app the registry says has not built it."""
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI, tests_py=MARKED,
                                          records=_rec("passed"))
    problems = run(features, e2e, repo)
    assert any("[feed-own]: declared not built on tui" in p and "PASSED there" in p
               for p in problems)


def test_rule_7_reads_the_map_from_records_too(tmp_path):
    passed = _rec("passed")
    passed["linux"]["surfaces"] = ["feed-own"]
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI, records=passed)
    assert any("[feed-own]: declared not built on tui" in p for p in run(features, e2e, repo))


def test_rule_7_is_quiet_when_the_declaration_holds(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, ON_TUI, tests_py=MARKED,
                                          records=_rec("passed"))
    assert run(features, e2e, repo) == []
    features, e2e, repo = _registry_world(tmp_path / "b", NOT_ON_TUI, tests_py=MARKED,
                                          records=_rec("failed"))
    assert not any("feed-own" in p for p in run(features, e2e, repo))


def _web_ledger(features, records):
    (features / "ledger" / "web.json").write_text(
        json.dumps({"schema": 2, "app": "web",
                    "features": {"feed-read": {"tests": {MAPPED: records}}}},
                   indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _dated(outcome, date, surfaces=None):
    r = _rec(outcome)
    r["linux"]["date"] = date
    if surfaces:
        r["linux"]["surfaces"] = list(surfaces)
    return r


def test_rule_7_ignores_a_removed_gate_a_newer_pass_did_not_reach(tmp_path):
    """The row's case: an older record on another app still lists the gate the test
    dropped; the newer pass on tui reached no gate, so it refutes nothing."""
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI,
                                          records=_dated("passed", "2026-10-30"))
    _web_ledger(features, _dated("passed", "2026-10-29", ["feed-own"]))
    assert not any("feed-own" in p for p in run(features, e2e, repo))


def test_rule_7_still_reads_a_pass_that_cannot_say_what_it_reached(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI,
                                          records=_dated("passed", "2026-10-20"))
    _web_ledger(features, _dated("passed", "2026-10-29", ["feed-own"]))
    assert any("[feed-own]: declared not built on tui" in p
               for p in run(features, e2e, repo))


def test_rule_7_keeps_a_marked_surface_whatever_the_pass_reached(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI, tests_py=MARKED,
                                          records=_dated("passed", "2026-10-30"))
    assert any("[feed-own]: declared not built on tui" in p
               for p in run(features, e2e, repo))


UI_YAML = "pages:\n  feed:\n    elements:\n      - feed-own   # text\n"
UI_ACTUAL_HAS = "pages:\n  feed:\n    elements:\n      - feed-own   # built 2026-10-01\n"
UI_ACTUAL_MISSING = ("pages:\n  feed:\n    elements:\n      - page-heading\n"
                     "    missing:\n      - feed-own\n")


def test_rule_7_flags_a_ui_yaml_id_declared_unbuilt_where_ui_actual_records_it(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI)
    (e2e / "ui.yaml").write_text(UI_YAML, encoding="utf-8")
    (e2e / "ui-actual-tui.yaml").write_text(UI_ACTUAL_HAS, encoding="utf-8")
    problems = run(features, e2e, repo)
    assert [p for p in problems if "ui-actual" in p] == [
        "tests/e2e-unified/surfaces.toml [feed-own]: declared not built on tui, but "
        "tests/e2e-unified/ui-actual-tui.yaml records the ui.yaml id `feed-own` present "
        "there — declaring unbuilt what exists is a never-move (feature-catalog.md § "
        "Closing a gap); widen `built` or correct the snapshot"]


def test_rule_7_reads_a_missing_list_as_absent_and_a_harness_key_not_at_all(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI)
    (e2e / "ui.yaml").write_text(UI_YAML, encoding="utf-8")
    (e2e / "ui-actual-tui.yaml").write_text(UI_ACTUAL_MISSING, encoding="utf-8")
    assert run(features, e2e, repo) == []
    (e2e / "surfaces.toml").write_text(NOT_ON_TUI.replace("product", "harness"),
                                       encoding="utf-8")
    (e2e / "ui-actual-tui.yaml").write_text(UI_ACTUAL_HAS, encoding="utf-8")
    assert run(features, e2e, repo) == []


def test_rule_7_ignores_a_key_that_is_not_a_ui_yaml_id(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, NOT_ON_TUI)
    (e2e / "ui.yaml").write_text("pages:\n  feed:\n    elements:\n      - other\n",
                                 encoding="utf-8")
    (e2e / "ui-actual-tui.yaml").write_text(UI_ACTUAL_HAS, encoding="utf-8")
    assert run(features, e2e, repo) == []


ALL_SEVEN = ('[feed-own]\nkind = "product"\n'
             'built = ["web", "linux", "windows", "macos", "ios", "android", "tui"]\n')


def test_rule_7_calls_a_product_entry_built_everywhere_finished(tmp_path):
    features, e2e, repo = _registry_world(tmp_path, ALL_SEVEN)
    assert any("[feed-own]: built on all seven apps — finished" in p
               for p in run(features, e2e, repo))
    # a harness entry built everywhere is still a live description of the driver
    features, e2e, repo = _registry_world(tmp_path / "b", ALL_SEVEN.replace("product", "harness"))
    assert run(features, e2e, repo) == []


def test_yaml_list_ids_reads_block_lists_under_the_named_keys():
    text = ("pages:\n  feed:\n    state_fields:\n      - data.feed.posts[].body\n"
            "    elements:\n      - page-heading          # text\n      - feed-view\n"
            "    optional_elements:\n      - feed-extra\n    missing:\n      - feed-gone\n"
            "  other:\n    elements:\n    - compact-item\n")
    present = fl.yaml_list_ids(text, fl.UI_ACTUAL_PRESENT)
    assert present == {"page-heading", "feed-view", "feed-extra", "compact-item"}
    assert fl.yaml_list_ids(text) == present | {"feed-gone"}


# ── ruling (i): an app column's set is observed in legs, by host_platform ────
APP_EVERY = MAPPED
APP_LINUX = "tests/e2e-unified/tests/test_feed_keyring.py::test_the_feed_key_rests_in_the_keyring"
APP_WINDOWS = "tests/e2e-unified/tests/test_feed_credman.py::test_the_feed_key_rests_in_credman"
CONTRACT_APP_LEGS = CONTRACT_ONE + f"\n   - `{APP_LINUX}`\n   - `{APP_WINDOWS}`"
HOST_TAGGED = '''\
import pytest

pytestmark = [pytest.mark.tui, pytest.mark.host_platform({module})]


@pytest.mark.feature("feed-read")
@pytest.mark.host_platform({function})
def {name}(app):
    pass
'''
LINUX_COLUMN_MARKED_WINDOWS = '''\
import pytest


@pytest.mark.linux
@pytest.mark.feature("feed-read")
@pytest.mark.host_platform("windows")
def test_own_post_appears(app):
    pass
'''


def _app_world(tmp_path, *, platforms=None, linux="\"linux\"", windows="\"windows\"",
               tests=None):
    """A page whose `[app]` outcome is witnessed by an ordinary test any platform runs,
    a tui test marked `host_platform("linux")` (module and function marks, folded by
    intersection) and one marked `host_platform("windows")` — the
    identity-protected-on-this-device shape (ruling (i))."""
    features, e2e, repo = build(tmp_path, contract=CONTRACT_APP_LEGS, tests_py=TAGGED)
    for node_id, mark in ((APP_LINUX, linux), (APP_WINDOWS, windows)):
        rel, name = node_id.split("::")
        (repo / rel).write_text(HOST_TAGGED.format(
            module=f"{mark}, \"macos\"", function=mark, name=name), encoding="utf-8")
    if platforms is not None:
        cell = dict(min(platforms.values(), key=lambda c: c["date"]))
        _tui_ledger(features, {"feed-read": {"cell": {**cell, "platforms": platforms},
                                             "tests": tests or {}}})
    return features, e2e, repo


def test_the_host_platform_marks_fold_by_intersection(tmp_path):
    _features, e2e, repo = _app_world(tmp_path)
    facts = fs.scan_tree(e2e, repo=repo)
    assert facts[APP_LINUX].platforms == ("linux",)  # module {linux, macos} ∩ function {linux}
    assert facts[APP_WINDOWS].platforms == ("windows",)
    assert facts[APP_EVERY].platforms is None
    assert fc.scan_marks(repo)[2] == {APP_LINUX: ("linux",), APP_WINDOWS: ("windows",)}


def test_app_legs_follow_the_host_platform_marker(tmp_path):
    """tui: a `linux` and a `windows` leg, `macos` redundant under both; every
    single-platform column one leg of its one platform, as before."""
    features, _e2e, repo = _app_world(tmp_path)
    page = fc.load_pages(features)[0]
    app_marks, _s, platform_marks = fc.scan_marks(repo)
    assert fc.witness_platforms(APP_LINUX, platform_marks) == frozenset({"linux"})
    assert fc.witness_platforms(APP_EVERY, platform_marks) == fc.PLATFORMS
    assert fc.legs(page, "tui", app_marks, platform_marks) == (
        frozenset({"linux"}), frozenset({"windows"}))
    scope = fc.scope_by_platform(page, "tui", app_marks, platform_marks)
    assert set(scope) == {"linux", "windows"}
    assert scope["windows"] == {APP_EVERY, APP_WINDOWS}
    assert fc.legs(page, "linux", app_marks, platform_marks) == (frozenset({"linux"}),)
    assert fc.legs(page, "android", app_marks, platform_marks) == (frozenset({"linux", "macos"}),)
    # without the markers the column is one leg, exactly the pre-(i) fold
    assert fc.legs(page, "tui", app_marks) == (frozenset({"linux", "macos", "windows"}),)


def test_rule_6_refolds_an_app_cell_whose_top_level_claims_a_leg_it_lacks(tmp_path):
    """The measured case (2026-11-02, identity-protected-on-this-device): a linux tui
    stamp alone read complete; the windows leg was never stamped."""
    features, e2e, repo = _app_world(tmp_path, platforms={"linux": LINUX_CELL})
    problems = run(features, e2e, repo)
    assert any("ledger/tui.json" in p and "feed-read" in p and "not the fold" in p
               for p in problems), problems
    assert fc.prune_ledger(features) == []
    entry = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"] == {"complete": False, "platforms": {"linux": LINUX_CELL}}
    assert run(features, e2e, repo) == []


def test_a_tui_stamp_on_a_redundant_platform_is_stale(tmp_path):
    features, e2e, repo = _app_world(
        tmp_path, platforms={"linux": LINUX_CELL, "windows": WINDOWS_CELL, "macos": MACOS_CELL},
        tests={APP_LINUX: {"linux": _platform_rec("passed", "linux", "2026-10-20"),
                           "macos": _platform_rec("passed", "macos", "2026-09-20")}})
    problems = run(features, e2e, repo)
    assert any("ledger/tui.json" in p and "`macos`" in p and "ruling" in p for p in problems), problems
    assert fc.prune_ledger(features) == [("tui", "feed-read", None, "macos"),
                                         ("tui", "feed-read", APP_LINUX, "macos")]
    entry = json.loads((features / "ledger" / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert sorted(entry["cell"]["platforms"]) == ["linux", "windows"]
    assert entry["cell"]["complete"] is True and entry["cell"]["stamp"] == WINDOWS_CELL["stamp"]
    assert list(entry["tests"][APP_LINUX]) == ["linux"]
    assert run(features, e2e, repo) == []


def test_rule_6_refuses_a_host_platform_outside_its_vocabulary_venue_or_columns(tmp_path):
    features, e2e, repo = _app_world(tmp_path, linux="\"darwin\"")
    assert any("darwin" in p and "vocabulary" in p for p in run(features, e2e, repo))
    # a linux-column witness marked windows: no column it speaks for runs there
    features, e2e, repo = build(tmp_path / "cols", contract=CONTRACT_ONE,
                                tests_py=LINUX_COLUMN_MARKED_WINDOWS)
    assert any("windows" in p and "none of its columns" in p for p in run(features, e2e, repo))
    # a venue module naming a platform outside its venue
    features, e2e, repo = _nest_world(tmp_path / "venue", contract=CONTRACT_NEST_LEGS,
                                      platforms={"linux": LINUX_CELL}, tests={})
    rel, name = LINUX_WITNESS.split("::")
    (repo / rel).write_text(NEST_TAGGED.replace(
        "def {name}", "@pytest.mark.host_platform(\"windows\")\ndef {name}").format(name=name),
        encoding="utf-8")
    assert any("outside its venue" in p for p in run(features, e2e, repo))


def test_the_real_identity_page_has_tui_legs_linux_and_windows():
    """Pinned against the live catalog: the Credential Manager witness is the windows
    leg's, the Secret-Service and session-bus ones the linux leg's."""
    pages = {p.slug: p for p in fc.load_pages(fc.FEATURES)}
    if "identity-protected-on-this-device" not in pages:
        pytest.skip("no identity-protected-on-this-device page in this tree")
    app_marks, _s, platform_marks = fc.scan_marks()
    assert fc.legs(pages["identity-protected-on-this-device"], "tui", app_marks,
                   platform_marks) == (frozenset({"linux"}), frozenset({"windows"}))
