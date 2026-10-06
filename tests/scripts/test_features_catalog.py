"""Tests for scripts/features_catalog.py — the feature catalog's parse/compute/render core.

The catalog is empty in the tree until step 3 writes its pages
(`docs/goal/architecture/feature-catalog.md` § Implementation status today), so every
test here builds a fixture catalog in `tmp_path`. That is also the honest shape: the
library's inputs are a directory of pages and a directory of ledger files, never the
repo it happens to live in.
"""
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))
import features_catalog as fc


# ── fixture catalog ─────────────────────────────────────────────────────────
PAGE = """\
---
slug: feed-read
title: Read your feed
section: everyday
goal: docs/goal/ui/feed.md § Reading
guide: docs/guides/app-tour.md § Feed
absences:
  android: docs/goal/ui/feed.md § Platform notes
---

## What a user gets

Your feed shows the posts of the people you follow, newest first.

## Coverage contract

Stamped 2026-08-26 at 8213688bbc.

1. [app] A signed-in person sees their own posts in the feed — `docs/goal/ui/feed.md` § Reading
   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`
2. [app] Pulling to refresh brings in new posts — `docs/goal/ui/feed.md` § Reading
   - `tests/e2e-unified/tests/test_feed.py::test_refresh`
3. [nest] A deleted post stops being served — `docs/goal/behavior/delete.md` § Posts
   - (none)

## Status

<!-- features-render:begin -->
<!-- features-render:end -->
"""


def write_catalog(root: Path, pages: dict, ledger: dict | None = None):
    features = root / "docs" / "features"
    (features / "ledger").mkdir(parents=True, exist_ok=True)
    for stem, text in pages.items():
        (features / (stem + ".md")).write_text(text, encoding="utf-8")
    for app, payload in (ledger or {}).items():
        (features / "ledger" / (app + ".json")).write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return features


def ledger_file(app: str, features: dict) -> dict:
    return {"schema": 1, "app": app, "features": features}


def rec(outcome="passed", stamp="0.1.2-dev+8213688b standalone", date="2026-08-26",
        skip_class=None, nest_mode="standalone", platform="linux"):
    """A test's ledger entry: `{platform: record}` (§ The ledger, *Platform
    keying*) — every fixture in this file observes on one platform unless a test
    passes a second `platform=` explicitly and merges the two dicts itself."""
    return {platform: {
        "outcome": outcome, "skip_class": skip_class, "stamp": stamp,
        "version": "0.1.2", "commit": "8213688bbc" + "0" * 30,
        "image_digest": None, "nest_mode": nest_mode, "date": date, "platform": platform,
    }}


def cell_entry(stamp="0.1.2-dev+8213688b standalone", date="2026-08-26", complete=True):
    return {"stamp": stamp, "date": date, "complete": complete}


# ── front-matter + contract parsing ─────────────────────────────────────────
def test_parse_page_front_matter(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    page = fc.parse_page(features / "feed-read.md")
    assert page.slug == "feed-read"
    assert page.title == "Read your feed"
    assert page.section == "everyday"
    assert page.goal == "docs/goal/ui/feed.md § Reading"
    assert page.guide == "docs/guides/app-tour.md § Feed"
    assert page.absences == {"android": "docs/goal/ui/feed.md § Platform notes"}


def test_parse_page_contract(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    page = fc.parse_page(features / "feed-read.md")
    assert [o.number for o in page.outcomes] == [1, 2, 3]
    assert [o.surface for o in page.outcomes] == ["app", "app", "nest"]
    assert list(page.outcomes[0].tests) == ["tests/e2e-unified/tests/test_feed.py::test_own_post_appears"]
    assert page.outcomes[2].tests == ()
    assert page.outcomes[0].citation == "docs/goal/ui/feed.md § Reading"
    assert page.stamp_commit == "8213688bbc"
    assert page.stamp_date == "2026-08-26"


def test_contract_with_an_unmapped_outcome_is_partial(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    assert fc.parse_page(features / "feed-read.md").full is False


def test_contract_with_every_outcome_mapped_is_full(tmp_path):
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text})
    assert fc.parse_page(features / "feed-read.md").full is True


def test_slug_must_equal_the_stem(tmp_path):
    features = write_catalog(tmp_path, {"other-name": PAGE})
    with pytest.raises(fc.CatalogError, match="slug"):
        fc.parse_page(features / "other-name.md")


def test_outcome_without_a_citation_is_refused(tmp_path):
    text = PAGE.replace(
        "1. [app] A signed-in person sees their own posts in the feed — `docs/goal/ui/feed.md` § Reading",
        "1. [app] A signed-in person sees their own posts in the feed")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="citation"):
        fc.parse_page(features / "feed-read.md")


def test_outcome_line_outside_the_contract_block_is_refused(tmp_path):
    """A page carrying an extra section (e.g. `## Workstream`) between the contract
    and `## Status` must not silently swallow an outcome line placed there — it
    parses as nothing today and every gate stays green."""
    text = PAGE.replace(
        "## Status",
        "## Workstream\n\n"
        "9. [app] x — `docs/goal/ui/feed.md` § Reading\n"
        "   - (none)\n\n"
        "## Status")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="Workstream"):
        fc.parse_page(features / "feed-read.md")


def test_witness_line_outside_the_contract_block_is_refused(tmp_path):
    """Same hole, minus the outcome header: a bare witness bullet left in another
    section also parses as nothing and must be refused."""
    text = PAGE.replace(
        "## Status",
        "## Workstream\n\n"
        "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`\n\n"
        "## Status")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="Workstream"):
        fc.parse_page(features / "feed-read.md")


# ── cell semantics ──────────────────────────────────────────────────────────
def _catalog(tmp_path, ledger):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, ledger)
    return fc.load_pages(features), fc.load_ledger(features)


def test_cell_is_empty_without_a_ledger_entry(tmp_path):
    pages, ledger = _catalog(tmp_path, {})
    cell = fc.compute_cell(pages[0], "tui", ledger)
    assert cell.state == "none"
    assert cell.stamp == ""


def test_declared_absence_renders_a_dash(tmp_path):
    pages, ledger = _catalog(tmp_path, {})
    assert fc.compute_cell(pages[0], "android", ledger).state == "absent"


def test_cell_is_partial_when_the_contract_is_partial(tmp_path):
    """Outcome 3 is `(none)`, so even an all-green app half can only be partial."""
    pages, ledger = _catalog(tmp_path, {"tui": ledger_file("tui", {"feed-read": {
        "cell": cell_entry(),
        "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}})})
    cell = fc.compute_cell(pages[0], "tui", ledger)
    assert cell.state == "partial"
    assert cell.stamp == "0.1.2-dev+8213688b standalone"


def test_cell_is_full_when_the_contract_is_full_and_every_test_passed(tmp_path):
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec(),
        }}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger).state == "full"


def test_cell_is_failing_when_no_mapped_test_passed(tmp_path):
    pages, ledger = _catalog(tmp_path, {"tui": ledger_file("tui", {"feed-read": {
        "cell": cell_entry(),
        "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec("failed"),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec("failed"),
        }}})})
    assert fc.compute_cell(pages[0], "tui", ledger).state == "failing"


def test_a_missing_nest_half_empties_a_green_app_column(tmp_path):
    """The nest half is a conjunct on every column (§ Cell semantics)."""
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger).state == "none"


def test_a_cell_shows_the_older_of_its_two_stamps(tmp_path):
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "tui": ledger_file("tui", {"feed-read": {
            "cell": cell_entry(stamp="0.1.2 docker", date="2026-08-26"), "tests": {
                "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
                "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
            }}}),
        "nest": ledger_file("nest", {"feed-read": {
            "cell": cell_entry(stamp="0.1.1-dev+deadbeef standalone", date="2026-08-20"),
            "tests": {
                "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec(),
            }}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger).stamp == "0.1.1-dev+deadbeef standalone"


def test_nest_only_feature_spans_every_column(tmp_path):
    text = """\
---
slug: dkim-signing
title: Your outgoing mail is signed
section: mail, calendar and contacts
goal: docs/goal/behavior/admin.md § 6. Mail
guide: docs/guides/admin-tour.md § Mail
---

## What a user gets

Mail you send is signed so other servers trust it. There is nothing to choose.

## Coverage contract

Stamped 2026-08-26 at 8213688bbc.

1. [nest] A receiving server sees a valid DKIM signature — `docs/goal/behavior/admin.md` § 6. Mail
   - `tests/e2e-unified/tests/platform/docker/test_dkim.py::test_signed`

## Status

<!-- features-render:begin -->
<!-- features-render:end -->
"""
    features = write_catalog(tmp_path, {"dkim-signing": text}, {
        "nest": ledger_file("nest", {"dkim-signing": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/platform/docker/test_dkim.py::test_signed": rec(),
        }}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    page = pages[0]
    assert page.nest_only is True
    assert {fc.compute_cell(page, app, ledger).state for app in fc.APPS} == {"full"}


def test_skip_unbuilt_is_partial_not_failing(tmp_path):
    pages, ledger = _catalog(tmp_path, {"tui": ledger_file("tui", {"feed-read": {
        "cell": cell_entry(),
        "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec("skipped", skip_class="unbuilt"),
        }}})})
    assert fc.compute_cell(pages[0], "tui", ledger).state == "partial"


# ── platform keying (§ The ledger, Platform keying) ─────────────────────────
def test_a_test_failing_on_one_platform_is_mixed_even_if_another_platform_passed(tmp_path):
    """A folded pass on linux must not hide a fail on windows — cross-platform
    disagreement is exactly the "mixed" outcome § Cell semantics already defines,
    never a ✅ (the bug: a ledger that could say "tui passed" or "tui failed" but
    never both, so a windows regression a linux pass had already recorded stayed
    invisible)."""
    pages, ledger = _catalog(tmp_path, {"tui": ledger_file("tui", {"feed-read": {
        "cell": cell_entry(),
        "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": {
                **rec(platform="linux"), **rec(outcome="failed", platform="windows")},
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}})})
    assert fc.compute_cell(pages[0], "tui", ledger).state == "partial"


def test_a_test_passing_on_every_platform_that_recorded_it_still_folds_to_full(tmp_path):
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": {
                **rec(platform="linux"), **rec(platform="windows")},
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec(),
        }}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger).state == "full"


def test_status_block_names_the_platform_beside_each_app(tmp_path):
    """The page's own `## Status` block is where a cross-platform disagreement the
    folded matrix cell papers over becomes visible."""
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": {
                **rec(platform="linux"), **rec(outcome="failed", platform="windows")},
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}})})
    fc.write_all(features)
    body = (features / "feed-read.md").read_text(encoding="utf-8")
    assert "tui (linux): passed" in body
    assert "tui (windows): failed" in body


# ── render ──────────────────────────────────────────────────────────────────
def test_render_is_idempotent(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    first = fc.render_all(features)
    second = fc.render_all(features)
    assert first == second


def test_render_writes_matrix_status_and_json(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    changed = fc.write_all(features)
    assert (features / "MATRIX.md").exists()
    assert (features / "matrix.json").exists()
    assert changed  # the first render always writes
    body = (features / "feed-read.md").read_text(encoding="utf-8")
    assert "<!-- features-render:begin -->" in body
    assert "Read your feed" in (features / "MATRIX.md").read_text(encoding="utf-8")
    # A second write is a no-op — the check gate depends on it.
    assert fc.write_all(features) == []


def test_render_uses_the_ascii_marks_never_coloured_circles(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}})})
    fc.write_all(features)
    matrix = (features / "MATRIX.md").read_text(encoding="utf-8")
    assert fc.MARKS["partial"] in matrix
    for circle in ("\U0001f7e2", "\U0001f7e1", "\U0001f534"):
        assert circle not in matrix


def test_matrix_json_carries_the_state_as_a_word(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
        }}})})
    fc.write_all(features)
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    feature = data["sections"][0]["features"][0]
    assert feature["slug"] == "feed-read"
    assert feature["cells"]["tui"]["state"] == "partial"
    assert feature["cells"]["android"]["state"] == "absent"


def test_render_of_an_empty_catalog_is_legal(tmp_path):
    features = write_catalog(tmp_path, {})
    fc.write_all(features)
    assert (features / "MATRIX.md").exists()
    assert json.loads((features / "matrix.json").read_text(encoding="utf-8"))["sections"] == []


# ── the column is its own scope (§ Cell semantics, "for that app") ───────────
# A contract names one witness per app for the same outcome — the shape most of the
# catalog is written in. Read per PAGE, such a page can never stamp any column on any
# machine: the Windows, Linux and Apple apps are each drivable only on their own
# development machine, so a full sweep selects one machine's apps and no run anywhere
# collects all three witnesses. Read per COLUMN, each column is judged on the
# witnesses that can speak for it — what the doc says, and what makes a cell
# reachable at all.
PER_APP_PAGE = PAGE.replace(
    "   - `tests/e2e-unified/tests/test_feed.py::test_own_post_appears`",
    "   - `tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears`\n"
    "   - `tests/e2e-unified/tests/test_feed.py::test_windows_own_post_appears`",
).replace(
    "   - `tests/e2e-unified/tests/test_feed.py::test_refresh`",
    "   - `tests/e2e-unified/tests/test_feed.py::test_refresh`",
).replace("   - (none)",
          "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")

MARKS_PER_APP = {
    "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears": ("tui",),
    "tests/e2e-unified/tests/test_feed.py::test_windows_own_post_appears": ("windows",),
}


def _per_app_catalog(tmp_path, observed):
    """`observed` are the node ids the tui run recorded an outcome for."""
    return write_catalog(tmp_path, {"feed-read": PER_APP_PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {
            "cell": cell_entry(), "tests": {t: rec() for t in observed}}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec()}}}),
    })


def test_a_columns_scope_excludes_another_apps_witness(tmp_path):
    """The whole finding: tui ran every witness that can speak for tui, and the
    windows witness — which no run on this machine can even collect — must not hold
    tui's cell blank forever."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears",
        "tests/e2e-unified/tests/test_feed.py::test_refresh",
    ])
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger, MARKS_PER_APP).state == "full"


def test_the_pages_status_block_reads_each_column_in_its_own_scope(tmp_path):
    """A page's `## Status` row and its matrix cell are ONE computation, so they
    must agree. The status block used to drop the app marks it was handed and judge
    every column on all seven apps' witnesses together. A windows-only witness
    carrying a tui record (a mis-marked run, since fixed) then held the page at
    ⚠ while the matrix read ✅ for the same column."""
    features = write_catalog(tmp_path, {"feed-read": PER_APP_PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_refresh": rec(),
            "tests/e2e-unified/tests/test_feed.py::test_windows_own_post_appears":
                rec(outcome="failed"),
        }}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec()}}}),
    })
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    cell = fc.compute_cell(pages[0], "tui", ledger, MARKS_PER_APP)
    assert cell.state == "full", "the matrix's own reading of this column"
    block = fc._status_block(pages[0], ledger, MARKS_PER_APP)
    assert f"| tui | {cell.mark} {cell.state} |" in block, block


def test_a_column_missing_its_own_witness_is_partial_never_full(tmp_path):
    """The other half, and the reason the two changes had to land together: narrowing
    the scope without narrowing FULLNESS would let a column claim ✅ for an outcome no
    test of its own ever witnessed.

    Here outcome 2's only witness is windows-marked, so tui has run every witness that
    can speak for it — and must still not read ✅, because outcome 2 is `(none)` for
    tui exactly as if the contract had spelled it that way.
    """
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears"])
    marks = dict(MARKS_PER_APP)
    marks["tests/e2e-unified/tests/test_feed.py::test_refresh"] = ("windows",)
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "tui", ledger, marks).state == "partial"


def test_an_unmarked_witness_speaks_for_every_column(tmp_path):
    """The identity case, and by far the common one: 3683 of the tree's 4277 tests
    carry no app marker, because the `app` fixture parametrizes them over whatever the
    run selected."""
    assert fc.applicable("t.py::a", "tui", {}) is True
    assert fc.applicable("t.py::a", "tui", {"t.py::a": ()}) is True
    assert fc.applicable("t.py::a", "tui", {"t.py::a": ("windows",)}) is False
    assert fc.applicable("t.py::a", "tui", {"t.py::a": ("tui", "web")}) is True


def test_a_nest_outcome_is_never_narrowed_by_the_column(tmp_path):
    """A nest record counts for every column (§ The two surfaces), so a nest witness
    that only runs on one machine still witnesses every column — narrowing it would
    make every nest-only page partial on six of seven columns."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears",
        "tests/e2e-unified/tests/test_feed.py::test_refresh",
    ])
    pages, _ = fc.load_pages(features), None
    marks = dict(MARKS_PER_APP)
    marks["tests/e2e-unified/tests/api/test_delete.py::test_delete"] = ("macos",)
    assert pages[0].full_for("tui", marks) is True


def test_the_app_marker_vocabulary_is_the_columns_and_the_runs_alike():
    """Three lists of the seven apps would drift; this is one, pinned BY EQUALITY to
    both of the others — the render's columns and the authority the run itself
    deselects on (`conftest._KNOWN_APPS`). Read out of conftest by `ast` rather than
    by import, because importing a pytest conftest outside its own session is not a
    thing this test may do."""
    import ast

    import features_scan

    assert features_scan.APP_MARKERS == set(fc.APPS)

    conftest = (Path(__file__).resolve().parent.parent
                / "e2e-unified" / "conftest.py")
    tree = ast.parse(conftest.read_text(encoding="utf-8"), filename=str(conftest))
    known = None
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign)
                and any(getattr(t, "id", None) == "_KNOWN_APPS" for t in node.targets)):
            known = ast.literal_eval(node.value)
    assert known is not None, "conftest no longer defines _KNOWN_APPS — repoint this pin"
    assert features_scan.APP_MARKERS == set(known)


def test_the_scanner_reads_an_app_marker_wherever_pytest_would(tmp_path):
    """Module `pytestmark`, a class decorator and a function decorator all reach the
    test — and the class case is not hypothetical: `@pytest.mark.windows` on
    `class TestFullJourneyInstalledApp` is the only app marker the whole windows
    installer journey carries, and a scan that read only module and function level
    would call those tests app-agnostic and hand them to every column."""
    import features_scan

    src = tmp_path / "test_marks.py"
    src.write_text(
        "import pytest\n\n"
        "pytestmark = [pytest.mark.tier_3, pytest.mark.linux]\n\n\n"
        "def test_module_level():\n    pass\n\n\n"
        "@pytest.mark.web\n"
        "def test_function_level():\n    pass\n\n\n"
        "@pytest.mark.windows\n"
        "class TestKlass:\n"
        "    def test_class_level(self):\n        pass\n",
        encoding="utf-8")
    facts = {f.node_id.split("::", 1)[1]: f.apps
             for f in features_scan.scan_file(src, repo=tmp_path)}
    assert facts["test_module_level"] == ("linux",)
    assert facts["test_function_level"] == ("linux", "web")
    assert facts["TestKlass::test_class_level"] == ("linux", "windows")


def test_an_unmarked_test_is_scanned_as_speaking_for_every_column(tmp_path):
    import features_scan

    src = tmp_path / "test_plain.py"
    src.write_text("def test_plain(app):\n    pass\n", encoding="utf-8")
    assert features_scan.scan_file(src, repo=tmp_path)[0].apps == ()


def _scan_apps(tmp_path, source: str) -> dict:
    import features_scan

    src = tmp_path / "test_params.py"
    src.write_text(source, encoding="utf-8")
    return {f.node_id.split("::", 1)[1]: f.apps
            for f in features_scan.scan_file(src, repo=tmp_path)}


def test_a_client_set_parametrization_narrows_the_columns_like_a_mark(tmp_path):
    """A test restricted to a client set by PARAMETRIZATION rather than by a mark
    runs only on that set — conftest keeps a client-parametrized item only when its
    client was selected (`_parametrized_clients`), and on a column outside the set
    pytest collects a lone `[NOTSET]` placeholder that skips and records nothing. Read
    as unmarked, such a witness made its cell `unrun` — *a run can close this* — on
    columns no run could ever stamp. The live instance was the launch-routing smoke's
    case J, `_clients(*AUTOSTART_APPS)` with `AUTOSTART_APPS = ("windows",)`, which
    read `unrun` on macos when the honest cause is `short`.

    Each idiom the tree spells is read: the filter helper over a starred constant, a
    literal list, and a zero-argument helper returning a filtered comprehension —
    and a mark, where both are present, intersects (conftest's `params &
    marker_platforms`)."""
    facts = _scan_apps(tmp_path, (
        "import pytest\n"
        "from drivers import get_available_apps\n\n"
        "AUTOSTART_APPS = (\"windows\",)\n"
        "_SUPPORTED = (\"linux\", \"tui\", \"macos\")\n\n\n"
        "def _clients(*want):\n"
        "    available = get_available_apps()\n"
        "    return [c for c in want if c in available]\n\n\n"
        "def _supported():\n"
        "    available = get_available_apps()\n"
        "    return [c for c in _SUPPORTED if c in available]\n\n\n"
        "@pytest.fixture\n"
        "def launch_harness(request):\n    yield request.param\n\n\n"
        "@pytest.fixture(params=_supported())\n"
        "def sync_app(request):\n    yield request.param\n\n\n"
        "@pytest.fixture\n"
        "def seeded(sync_app):\n    yield sync_app\n\n\n"
        "@pytest.mark.parametrize(\"launch_harness\", _clients(*AUTOSTART_APPS), indirect=True)\n"
        "def test_starred_constant(launch_harness):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"launch_harness\", [\"tui\", \"web\"], indirect=True)\n"
        "def test_literal(launch_harness):\n    pass\n\n\n"
        "@pytest.mark.linux\n"
        "@pytest.mark.parametrize(\"launch_harness\", _clients(\"linux\", \"tui\"), indirect=True)\n"
        "def test_marked_and_parametrized(launch_harness):\n    pass\n\n\n"
        "def test_fixture_params(sync_app):\n    pass\n\n\n"
        "def test_fixture_params_through_a_fixture(seeded):\n    pass\n\n\n"
        "@pytest.mark.macos\n"
        "@pytest.mark.parametrize(\"sync_app\", [\"macos\"], indirect=True)\n"
        "def test_the_test_overrides_the_fixtures_params(sync_app):\n    pass\n"))
    assert facts["test_starred_constant"] == ("windows",)
    assert facts["test_literal"] == ("tui", "web")
    assert facts["test_marked_and_parametrized"] == ("linux",)
    assert facts["test_fixture_params"] == ("linux", "macos", "tui")
    assert facts["test_fixture_params_through_a_fixture"] == ("linux", "macos", "tui")
    # A test's own parametrization of the fixture replaces the fixture's `params=`.
    assert facts["test_the_test_overrides_the_fixtures_params"] == ("macos",)


def test_a_parametrization_the_run_does_not_deselect_on_does_not_narrow(tmp_path):
    """Only what conftest's app axis narrows on narrows the scan — never more. A
    DIRECT parametrization (no `indirect`) is a plain value however app-like its
    strings (`is_real_fixture` excludes pytest's pseudo-fixture); a second-app seat
    (`folder_share_owner_app`) launches beside the run's own app, which still speaks
    for its column; and a set the parse cannot name (`get_available_apps()`) is every
    column. Each stays unmarked — the status quo, never a guess."""
    facts = _scan_apps(tmp_path, (
        "import pytest\n"
        "from drivers import get_available_apps\n\n\n"
        "@pytest.fixture\n"
        "def launch_harness(request):\n    yield request.param\n\n\n"
        "@pytest.mark.parametrize(\"client\", [\"linux\", \"tui\"])\n"
        "def test_direct(app, client):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"folder_share_owner_app\", [\"tui\"], indirect=True)\n"
        "def test_second_seat(app, folder_share_owner_app):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"launch_harness\", get_available_apps(), indirect=True)\n"
        "def test_unnameable(launch_harness):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"launch_harness\", [\"tui\", \"darwin\"], indirect=True)\n"
        "def test_not_an_app_set(launch_harness):\n    pass\n"))
    assert facts == {"test_direct": (), "test_second_seat": (),
                     "test_unnameable": (), "test_not_an_app_set": ()}


def test_the_second_app_seats_are_the_runs_own_list():
    """`SECOND_APP_FIXTURES` is the scan's copy of conftest's `_REAL_SECOND_APP_FIXTURES`
    — the seats whose parametrization launches a second app beside the run's, and so
    never narrows the column. Pinned by equality, read by `ast` as the vocabulary pin
    above is."""
    import ast

    import features_scan

    conftest = (Path(__file__).resolve().parent.parent
                / "e2e-unified" / "conftest.py")
    tree = ast.parse(conftest.read_text(encoding="utf-8"), filename=str(conftest))
    seats = None
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign)
                and any(getattr(t, "id", None) == "_REAL_SECOND_APP_FIXTURES"
                        for t in node.targets)):
            seats = ast.literal_eval(node.value)
    assert seats is not None, "conftest no longer defines _REAL_SECOND_APP_FIXTURES — repoint this pin"
    assert features_scan.SECOND_APP_FIXTURES == set(seats)


def test_the_launch_routing_smokes_autostart_case_speaks_for_windows_alone():
    """The live instance, read off the real tree: case J is parametrized over
    `AUTOSTART_APPS`, so it is a witness for those columns only — and every other
    case of the module is narrowed to its own client set, not handed to all seven."""
    import features_scan

    scanned = features_scan.scan_tree()
    module = "tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::"
    case_j = scanned[module + "test_smoke_j_autostart_stays_hidden_only_when_it_lands_in_the_main_app"]
    assert case_j.apps == ("windows",)
    assert all(f.apps for nid, f in scanned.items()
               if nid.startswith(module) and f.features), (
        "a launch-routing case read as unmarked — every case there is client-restricted")


def test_no_witness_narrows_to_no_column_at_all():
    """A mark and a parametrization that share no app leave a test no run can ever
    select — `apps` cannot say *no column* (empty means every column), so the scan
    keeps the marks and this pin makes the contradiction loud instead."""
    import features_scan

    empty = [nid for nid, f in features_scan.scan_tree().items()
             if f.features and f.contradictory]
    assert empty == [], empty


def test_blocking_names_the_outcomes_that_keep_a_column_from_full(tmp_path):
    """`full_for` answers yes/no; the reader's next question is always *which
    outcome*, and deriving that by hand from a page and a marker table is what
    several triage passes did one page at a time.

    The per-app fixture's outcome 1 cites a tui witness and a windows one, so those
    two columns are full and the other five are blocked — on that outcome alone,
    which is exactly what the reader needs told."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears"])
    page = fc.load_pages(features)[0]

    assert page.blocking("tui", MARKS_PER_APP) == []
    assert page.blocking("windows", MARKS_PER_APP) == []
    for column in ("android", "ios", "linux", "macos", "web"):
        assert [o.number for o in page.blocking(column, MARKS_PER_APP)] == [1], column


def test_blocking_is_empty_exactly_when_the_column_is_full(tmp_path):
    """The invariant that keeps the two from drifting apart, asserted over every
    column of a catalog full on two of them and partial on the other five."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears"])
    page = fc.load_pages(features)[0]
    for app in ("android", "ios", "linux", "macos", "tui", "web", "windows"):
        assert bool(page.blocking(app, MARKS_PER_APP)) != page.full_for(app, MARKS_PER_APP)


def test_a_nest_outcome_blocks_every_column_alike(tmp_path):
    """A `[nest]` outcome is never narrowed by the column (§ The two surfaces), so
    when it is the blocker it blocks all seven alike. That shape is the signal a
    page's blankness is NOT a parity question: no app's test can ever close it.

    The base fixture's outcome 3 is the honest `(none)` mapping, and its outcomes 1
    and 2 are unmarked, so the nest outcome is the only blocker on every column."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    page = fc.load_pages(features)[0]
    assert [o.surface for o in page.blocking("tui")] == ["nest"]
    blocked = {a: tuple(o.number for o in page.blocking(a))
               for a in ("android", "ios", "linux", "macos", "tui", "web", "windows")}
    assert set(blocked.values()) == {(3,)}, blocked


def test_completable_is_the_parse_only_half_of_the_cell_question(tmp_path):
    """What a run could EVER record, against `compute_cell`'s what one HAS. A page
    completable on no column is blank for a contract reason, not a coverage one — so
    running more tests cannot move it, which is the distinction that turns a page
    list into work."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears"])
    page = fc.load_pages(features)[0]
    assert page.completable(MARKS_PER_APP) == ["windows", "tui"]
    assert page.completable() == [a for a in fc.APPS if a != "android"], (
        "unmarked witnesses speak for every column the page has")


def test_a_declared_absent_column_is_not_completable_and_not_short(tmp_path):
    """An `absences:` column renders `—` — the feature is absent there by design — so
    no run completes it and nothing is missing from it. Counting it among a page's
    completable columns (or, in the parity view, its short ones) reads a designed
    absence as coverage backlog. The fixture declares android absent."""
    features = _per_app_catalog(tmp_path, [
        "tests/e2e-unified/tests/test_feed.py::test_tui_own_post_appears"])
    page = fc.load_pages(features)[0]
    assert "android" not in page.completable()
    assert "android" not in page.completable(MARKS_PER_APP)


def test_a_page_completable_on_no_column_is_a_contract_fact(tmp_path):
    """The sharpest verdict the parse gives: the base fixture's `(none)` nest outcome
    blocks all seven alike, so no machine and no nest mode can ever stamp it."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    assert fc.load_pages(features)[0].completable() == []


# ── the backlog, transposed to the column (§ Cell semantics, the parity view) ─
# `Page.blocking` answers "which outcomes keep THIS PAGE short of column a", which is
# what a reader of one page wants. Routing the backlog asks the transpose — "which
# outcomes keep COLUMN a short, across every page" — because a trickle-down pass and
# the queue that receives its capture are both per app, never per feature.


def test_column_backlog_names_the_sibling_columns_a_gap_can_be_lifted_from(tmp_path):
    """The routing fact, and the reason the transpose is worth computing: outcome 1 is
    witnessed on tui and windows only, so every other column is short of it — and the
    repair there is a LIFT, with `witnesses` naming what to lift from."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    blockers = fc.column_backlog(pages, "linux", MARKS_PER_APP)
    assert [(b.page.slug, b.outcome.number) for b in blockers] == [("feed-read", 1)]
    # Canonical column order (`APPS`), the same order `completable` and `blocking`
    # iterate — not alphabetical, so a witness list reads like a matrix row.
    assert blockers[0].witnesses == ("windows", "tui")
    assert blockers[0].owed_here is True


def test_a_literal_none_outcome_is_owed_by_nobody(tmp_path):
    """The split that makes routing honest. The base fixture's outcome 3 is `(none)`:
    no column witnesses it, so it is an unwritten test owned by the feature's area —
    NOT a parity gap any app queue can close by lifting a sibling's implementation."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    pages = fc.load_pages(features)
    for column in ("tui", "linux", "web"):
        none_blockers = [b for b in fc.column_backlog(pages, column) if not b.owed_here]
        assert [b.outcome.number for b in none_blockers] == [3], column
        assert none_blockers[0].witnesses == ()


def test_a_declared_absent_column_has_no_backlog(tmp_path):
    """Same rule `completable` follows: a `—` column is neither completable nor short,
    so listing its outcomes as work to do reads a designed absence as coverage debt.
    The fixture declares android absent, and its outcome 1 blocks every other column."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    assert fc.column_backlog(pages, "android", MARKS_PER_APP) == []
    assert [b.outcome.number for b in fc.column_backlog(pages, "web", MARKS_PER_APP)] == [1]


def test_column_backlog_is_empty_exactly_when_the_column_is_full(tmp_path):
    """One definition, checked from the other side — the same invariant `blocking` and
    `full_for` already hold, now across pages rather than within one."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    for app in fc.APPS:
        if app in pages[0].absences:
            continue
        empty = not fc.column_backlog(pages, app, MARKS_PER_APP)
        assert empty is pages[0].full_for(app, MARKS_PER_APP), app


def test_a_nest_blocker_is_never_owed_by_one_column(tmp_path):
    """A nest record counts for every column (§ The two surfaces), so an unwitnessed
    `[nest]` outcome blocks all seven and no column can lift it from a sibling — it is
    never `owed_here`, however many columns are short of it."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    pages = fc.load_pages(features)
    nest = [b for b in fc.column_backlog(pages, "tui") if b.outcome.surface == fc.NEST]
    assert [b.outcome.number for b in nest] == [3]
    assert nest[0].owed_here is False


# ── a gap's reach: how many columns one missing witness blocks ───────────────
# `column_backlog` is per column, so one outcome witnessed on a single app appears in
# six columns' backlogs. Routing each of those to its app queue would mint the same
# work six times — the per-feature × 6 shape `testing.md` § Default app and nest mode
# refuses. Reach tells the two apart, and it belongs to the GAP, not to any column.


def test_gap_reach_is_one_entry_per_outcome_however_many_columns_are_short(tmp_path):
    """The whole point of the transpose's transpose. Outcome 1 is witnessed on tui and
    windows, so it is short on four other columns (android is declared absent) — and
    that is ONE gap with a reach of four, never four separate pieces of work."""
    features = _per_app_catalog(tmp_path, [])
    gaps = fc.gap_reach(fc.load_pages(features), MARKS_PER_APP)
    assert list(gaps) == [("feed-read", 1)]
    gap = gaps[("feed-read", 1)]
    assert gap.witnesses == ("windows", "tui")
    assert gap.short == ("web", "linux", "macos", "ios")
    assert gap.reach == 4


def test_a_declared_absent_column_is_neither_witness_nor_short(tmp_path):
    """Same rule the rest of the module follows: `—` is not coverage debt. The fixture
    declares android absent, so it never appears in a gap's reach."""
    features = _per_app_catalog(tmp_path, [])
    gap = fc.gap_reach(fc.load_pages(features), MARKS_PER_APP)[("feed-read", 1)]
    assert "android" not in gap.short
    assert "android" not in gap.witnesses


def test_an_outcome_nobody_witnesses_is_not_a_gap(tmp_path):
    """Reach ranks LIFTABLE debt, so a literal `(none)` — which no column can lift
    from any other — is excluded rather than ranked as reaching all seven. Including
    it would put the widest-reaching unwritten test at the top of a routing list that
    only app queues read, which is exactly where it does not belong."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    assert fc.gap_reach(fc.load_pages(features)) == {}


def test_reach_agrees_with_every_columns_backlog(tmp_path):
    """One definition, checked against the other: a gap reaches column `a` exactly when
    `a`'s own backlog owes it."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    gaps = fc.gap_reach(pages, MARKS_PER_APP)
    for app in fc.APPS:
        owed = {(b.page.slug, b.outcome.number)
                for b in fc.column_backlog(pages, app, MARKS_PER_APP) if b.owed_here}
        assert owed == {k for k, g in gaps.items() if app in g.short}, app


def test_a_columns_trickle_down_is_its_owed_gaps_under_a_reach_cap(tmp_path):
    """What ONE app queue's batched pass receives: the gaps this column owes whose reach
    is at most the cap. The fixture's one gap reaches four columns, so a cap of 4 hands
    it to linux's pass with its reference columns named, and a cap of 3 leaves it to a
    wider row — the same gap is never in both."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    at_four = fc.column_trickle_down(pages, "linux", 4, MARKS_PER_APP)
    assert [(g.page.slug, g.outcome.number, g.witnesses) for g in at_four] == [
        ("feed-read", 1, ("windows", "tui"))]
    assert fc.column_trickle_down(pages, "linux", 3, MARKS_PER_APP) == []
    # A witnessing column owes nothing, and neither does a declared-absent one.
    assert fc.column_trickle_down(pages, "tui", 4, MARKS_PER_APP) == []
    assert fc.column_trickle_down(pages, "android", 4, MARKS_PER_APP) == []


def test_a_trickle_down_never_carries_an_outcome_nobody_witnesses(tmp_path):
    """A literal `(none)` is the feature area's unwritten test, never an app queue's
    lift, so no reach cap — however generous — puts it in a column's pass."""
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    assert fc.column_trickle_down(fc.load_pages(features), "tui", len(fc.APPS)) == []


# ── maintainer-only witnesses (§ The coverage contract, 2026-09-09) ─────────
MO_ID = "tests/e2e-unified/tests/test_feed.py::test_refresh"


def test_a_maintainer_only_witness_parses_to_its_id_and_is_remembered(tmp_path):
    text = PAGE.replace(f"   - `{MO_ID}`", f"   - {fc.maintainer_only_witness(MO_ID)}")
    features = write_catalog(tmp_path, {"feed-read": text})
    page = fc.parse_page(features / "feed-read.md")
    outcome = page.outcomes[1]
    assert list(outcome.tests) == [MO_ID]
    assert outcome.maintainer_only == frozenset({MO_ID})
    assert page.outcomes[0].maintainer_only == frozenset()


def test_the_shipped_maintainer_only_spelling_parses_as_a_line_naming_no_id(tmp_path):
    """What a public clone reads after the transform excised the span."""
    text = PAGE.replace(f"   - `{MO_ID}`", "   - (maintainer-only)")
    features = write_catalog(tmp_path, {"feed-read": text})
    page = fc.parse_page(features / "feed-read.md")
    assert page.outcomes[1].tests == ()
    assert page.outcomes[1].maintainer_only == frozenset()


def test_the_status_table_spells_a_maintainer_only_witness_the_same_way(tmp_path):
    text = PAGE.replace(f"   - `{MO_ID}`", f"   - {fc.maintainer_only_witness(MO_ID)}")
    features = write_catalog(tmp_path, {"feed-read": text})
    fc.write_all(features)
    rendered = (features / "feed-read.md").read_text(encoding="utf-8")
    status = rendered.split(fc.BEGIN, 1)[1]
    assert f"| 2 | app | {fc.maintainer_only_witness(MO_ID)} |" in status
    # The span holds the id and nothing outside it names the test: excising the
    # span leaves the shipped table with only the mark.
    assert MO_ID not in status.replace(fc.maintainer_only_witness(MO_ID), "")


def test_a_missing_ledger_directory_is_a_hard_stop(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE})
    (features / "ledger").rmdir()
    with pytest.raises(fc.CatalogError, match="does not ship"):
        fc.load_ledger(features)


# ── per-outcome absences (§ The page, `absences`; ruled 2026-09-26) ──────────
# A column a goal doc excuses from SOME outcomes while it owes the rest: an
# `absences` entry keyed `<app> (outcome N)` / `<app> (outcomes N, M)` takes those
# outcomes off that column's contract and nothing else. The column is still a
# column — it renders a real cell, judged on the outcomes it owes.
OUTCOME_ABSENT_PAGE = PAGE.replace(
    "  android: docs/goal/ui/feed.md § Platform notes\n",
    "  android: docs/goal/ui/feed.md § Platform notes\n"
    "  web (outcome 1): docs/goal/ui/feed.md § Platform notes\n",
).replace("   - (none)", "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")

OWN_POST = "tests/e2e-unified/tests/test_feed.py::test_own_post_appears"
REFRESH = "tests/e2e-unified/tests/test_feed.py::test_refresh"
DELETE = "tests/e2e-unified/tests/api/test_delete.py::test_delete"


def _outcome_absent(tmp_path, text=OUTCOME_ABSENT_PAGE, web_tests=(REFRESH,)):
    return write_catalog(tmp_path, {"feed-read": text}, {
        "web": ledger_file("web", {"feed-read": {
            "cell": cell_entry(), "tests": {t: rec() for t in web_tests}}}),
        "nest": ledger_file("nest", {"feed-read": {
            "cell": cell_entry(), "tests": {DELETE: rec()}}}),
    })


def test_an_outcome_absence_parses_beside_the_page_level_map(tmp_path):
    page = fc.parse_page(_outcome_absent(tmp_path) / "feed-read.md")
    assert page.absences == {"android": "docs/goal/ui/feed.md § Platform notes"}, (
        "the page-level map keeps its meaning: `—` on the whole column")
    assert page.outcome_absences == {"web": {1: "docs/goal/ui/feed.md § Platform notes"}}
    assert page.absent_from("web", page.outcomes[0])
    assert not page.absent_from("web", page.outcomes[1])
    assert not page.absent_from("linux", page.outcomes[0])


def test_the_plural_form_names_several_outcomes_under_one_citation(tmp_path):
    text = OUTCOME_ABSENT_PAGE.replace("web (outcome 1):", "web (outcomes 1, 2):")
    page = fc.parse_page(_outcome_absent(tmp_path, text) / "feed-read.md")
    assert sorted(page.outcome_absences["web"]) == [1, 2]


@pytest.mark.parametrize("key, why", [
    ("web (outcome 9)", "no outcome 9"),
    ("web (outcome 3)", r"\[nest\] outcome"),
    ("android (outcome 1)", "already declared absent"),
    ("wear (outcome 1)", "unknown app"),
    ("web (outcome one)", "unreadable `absences` key"),
])
def test_an_outcome_absence_the_contract_cannot_honour_is_refused(tmp_path, key, why):
    text = OUTCOME_ABSENT_PAGE.replace("web (outcome 1):", f"{key}:")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match=why):
        fc.parse_page(features / "feed-read.md")


def test_an_outcome_absence_named_twice_is_refused(tmp_path):
    text = OUTCOME_ABSENT_PAGE.replace(
        "  web (outcome 1): docs/goal/ui/feed.md § Platform notes\n",
        "  web (outcome 1): docs/goal/ui/feed.md § Platform notes\n"
        "  web (outcomes 1, 2): docs/goal/ui/feed.md § Reading\n")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="more than once"):
        fc.parse_page(features / "feed-read.md")


def test_an_absence_from_every_outcome_is_the_page_level_form(tmp_path):
    """A column absent from every outcome has none of the feature: that is the
    page-level entry, spelled once, and the per-outcome form refuses to stand in
    for it (the list a reader audits stays the honest one)."""
    text = OUTCOME_ABSENT_PAGE.replace(
        "3. [nest] A deleted post stops being served — `docs/goal/behavior/delete.md` § Posts\n"
        f"   - `{DELETE}`\n", "").replace("web (outcome 1):", "web (outcomes 1, 2):")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="every outcome"):
        fc.parse_page(features / "feed-read.md")


def test_an_absent_outcome_blocks_only_the_columns_that_owe_it(tmp_path):
    page = fc.load_pages(_outcome_absent(tmp_path))[0]
    assert page.blocking("web") == []
    assert page.blocking("linux") == []  # an unmarked witness speaks for every column
    marks = {OWN_POST: ("tui",)}
    assert page.blocking("web", marks) == [], "outcome 1 is not web's to owe"
    assert [o.number for o in page.blocking("linux", marks)] == [1]
    assert "web" in page.completable(marks)
    assert "linux" not in page.completable(marks)


def test_an_absent_outcome_is_nobodys_backlog_on_that_column(tmp_path):
    marks = {OWN_POST: ("tui",)}
    pages = fc.load_pages(_outcome_absent(tmp_path))
    assert fc.column_backlog(pages, "web", marks) == []
    gap = fc.gap_reach(pages, marks)[("feed-read", 1)]
    assert gap.short == ("linux", "windows", "macos", "ios"), (
        "web is excused from the outcome, android from the page")


def test_the_columns_cell_is_judged_without_the_absent_outcomes_witnesses(tmp_path):
    """Web never ran outcome 1's (unmarked) witness. Without the absence that missing
    record holds web's cell partial for ever; with it, the witness is not part of
    web's set and the cell is full on what web owes."""
    features = _outcome_absent(tmp_path)
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "web", ledger).state == "full"
    assert OWN_POST not in pages[0].surface_tests("app", "web")

    plain = _outcome_absent(tmp_path / "plain", OUTCOME_ABSENT_PAGE.replace(
        "  web (outcome 1): docs/goal/ui/feed.md § Platform notes\n", ""))
    pages, ledger = fc.load_pages(plain), fc.load_ledger(plain)
    assert fc.compute_cell(pages[0], "web", ledger).state == "partial"


def test_the_status_table_and_the_json_say_where_an_outcome_is_absent(tmp_path):
    features = _outcome_absent(tmp_path)
    fc.write_all(features)
    status = (features / "feed-read.md").read_text(encoding="utf-8").split(fc.BEGIN, 1)[1]
    rows = [line for line in status.splitlines() if line.startswith("| 1 |")]
    assert rows[-1] == "| 1 | app | absent by design on web | — |", (
        "the absence sits under its outcome's witnesses, not at the table's end")
    assert status.index(rows[-1]) < status.index("| 2 |")
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    entry = data["sections"][0]["features"][0]
    assert entry["outcome_absences"] == {"web": {"1": "docs/goal/ui/feed.md § Platform notes"}}
    assert entry["absences"] == {"android": "docs/goal/ui/feed.md § Platform notes"}
