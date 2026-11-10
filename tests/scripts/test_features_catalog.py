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
    # android's absence carries to its kids channel (§ Cell semantics).
    assert page.absences == {"android": "docs/goal/ui/feed.md § Platform notes",
                             "android-kids": "docs/goal/ui/feed.md § Platform notes"}


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


# ── the unbuilt state (§ Cell semantics, the *unbuilt* row) ─────────────────
OWN = "tests/e2e-unified/tests/test_feed.py::test_own_post_appears"
REFRESH = "tests/e2e-unified/tests/test_feed.py::test_refresh"
DELETE = "tests/e2e-unified/tests/api/test_delete.py::test_delete"


def _tui_cell(tmp_path, tests, nest_tests=None, nest_witness=False):
    """tui's cell over PAGE — outcome 3 witnessed by DELETE when `nest_witness`."""
    text = PAGE.replace("   - (none)", f"   - `{DELETE}`") if nest_witness else PAGE
    ledger = {"tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": tests}})}
    if nest_tests is not None:
        ledger["nest"] = ledger_file("nest", {"feed-read": {"cell": cell_entry(),
                                                            "tests": nest_tests}})
    features = write_catalog(tmp_path, {"feed-read": text}, ledger)
    return fc.compute_cell(fc.load_pages(features)[0], "tui", fc.load_ledger(features))


def test_an_unbuilt_witness_beside_a_pass_is_unbuilt_not_partial(tmp_path):
    cell = _tui_cell(tmp_path, {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")})
    assert cell.state == "unbuilt"
    assert cell.stamp == "0.1.2-dev+8213688b standalone"


def test_every_witness_unbuilt_is_unbuilt_not_failing(tmp_path):
    cell = _tui_cell(tmp_path, {OWN: rec("skipped", skip_class="unbuilt"),
                                REFRESH: rec("skipped", skip_class="unbuilt")})
    assert cell.state == "unbuilt"


def test_an_unrun_witness_beside_an_unbuilt_one_changes_nothing(tmp_path):
    assert _tui_cell(tmp_path, {OWN: rec("skipped", skip_class="unbuilt")}).state == "unbuilt"


def test_an_absence_skip_beside_an_unbuilt_witness_is_not_a_real_red(tmp_path):
    cell = _tui_cell(tmp_path, {OWN: rec("skipped", skip_class="unbuilt"),
                                REFRESH: rec("skipped", skip_class="absence")})
    assert cell.state == "unbuilt"


@pytest.mark.parametrize("outcome,skip_class", [
    ("failed", None), ("error", None), ("xfailed", None), ("skipped", None)])
def test_a_real_red_beside_an_unbuilt_witness_is_partial(tmp_path, outcome, skip_class):
    """The failure is the more urgent signal (§ Cell semantics)."""
    cell = _tui_cell(tmp_path, {OWN: rec("skipped", skip_class="unbuilt"),
                                REFRESH: rec(outcome, skip_class=skip_class)})
    assert cell.state == "partial"


def test_unbuilt_is_read_before_the_nest_conjunct(tmp_path):
    """A nest half with no record blanks a cell; an unbuilt app witness is the known
    fact, so the column reads unbuilt whatever the nest half says."""
    cell = _tui_cell(tmp_path, {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")},
                     nest_witness=True)
    assert cell.state == "unbuilt"


def test_a_real_red_on_the_nest_half_makes_an_unbuilt_column_partial(tmp_path):
    cell = _tui_cell(tmp_path, {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")},
                     nest_tests={DELETE: rec("failed")}, nest_witness=True)
    assert cell.state == "partial"


def test_an_unbuilt_nest_record_is_not_an_app_witness(tmp_path):
    """The state is held by an `[app]` witness; a nest record is no app's build gap."""
    cell = _tui_cell(tmp_path, {OWN: rec(), REFRESH: rec()},
                     nest_tests={DELETE: rec("skipped", skip_class="unbuilt")},
                     nest_witness=True)
    assert cell.state == "partial"


@pytest.mark.parametrize("first", ["linux", "windows"])
def test_a_real_red_on_another_platform_of_the_unbuilt_witness_is_partial(tmp_path, first):
    """The platform fold must not let the unbuilt record hide a sibling's failure,
    whichever platform the ledger happens to list first."""
    records = {"linux": rec("skipped", skip_class="unbuilt", platform="linux"),
               "windows": rec("failed", platform="windows")}
    second = "windows" if first == "linux" else "linux"
    cell = _tui_cell(tmp_path, {OWN: {**records[first], **records[second]}, REFRESH: rec()})
    assert cell.state == "partial"


def test_an_unbuilt_platform_beside_a_passing_one_is_unbuilt(tmp_path):
    cell = _tui_cell(tmp_path, {
        OWN: {**rec(platform="linux"), **rec("skipped", skip_class="unbuilt", platform="windows")},
        REFRESH: rec()})
    assert cell.state == "unbuilt"


def test_the_unbuilt_mark_is_its_own_glyph_and_never_a_coloured_circle():
    glyph = fc.MARKS["unbuilt"]
    assert glyph and glyph not in {m for s, m in fc.MARKS.items() if s != "unbuilt"}
    assert not any(0x1F534 <= ord(c) <= 0x1F535 or 0x1F7E0 <= ord(c) <= 0x1F7EB for c in glyph)


def test_matrix_json_carries_the_unbuilt_state_word(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")}}})})
    fc.write_all(features)
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    assert data["sections"][0]["features"][0]["cells"]["tui"]["state"] == "unbuilt"
    assert fc.MARKS["unbuilt"] in (features / "MATRIX.md").read_text(encoding="utf-8")


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


# § Render, the placeholder paragraph: a page whose words are still a draft reaches the
# matrix under the placeholder. The draft record is the public-texts inventory, which a
# tree without it (the public one) never has — so it renders every page verbatim.
_DRAFT_INVENTORY = """\
schema = 1

[[tree]]
id = "catalog-pages"
glob = "docs/features/*.md"
published = "the public repository's docs/features/"
status = "live"
approved = "unrecorded"
drafts = [{drafts}]
"""


def _render_matrices(tmp_path, drafts: str | None):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {
            OWN: rec(), REFRESH: rec()}}})})
    if drafts is not None:
        inventory = tmp_path / "docs" / "public-texts" / "inventory.toml"
        inventory.parent.mkdir(parents=True, exist_ok=True)
        inventory.write_text(_DRAFT_INVENTORY.format(drafts=drafts), encoding="utf-8")
    out = fc.render_all(features)
    return json.loads(out["matrix.json"]), out["MATRIX.md"], out["feed-read.md"]


def test_a_draft_page_reaches_the_matrix_as_its_slug_and_the_placeholder(tmp_path):
    public_texts = pytest.importorskip("_public_texts")  # unshipped, beside the render
    approved = _render_matrices(tmp_path / "approved", '')
    data, md, page = _render_matrices(tmp_path / "draft", '"docs/features/feed-read.md"')
    entry = data["sections"][0]["features"][0]
    assert entry["title"] == "feed-read"
    assert entry["what"] == public_texts.DRAFT_PLACEHOLDER_SENTENCE
    assert "| [feed-read](feed-read.md) |" in md
    for words in ("Read your feed", "Your feed shows the posts"):
        assert words not in json.dumps(data) and words not in md
    # Everything but the words renders as for an approved page — cells, stamps, the
    # page's own status block, which keeps its draft words for the reviewer.
    approved_entry = approved[0]["sections"][0]["features"][0]
    assert entry["cells"] == approved_entry["cells"]
    assert {k: v for k, v in entry.items() if k not in ("title", "what")} == {
        k: v for k, v in approved_entry.items() if k not in ("title", "what")}
    assert md.replace("[feed-read](", "[Read your feed](") == approved[1]
    assert page == approved[2] and "Read your feed" in page


def test_a_tree_with_no_inventory_renders_every_page_verbatim(tmp_path):
    data, md, _page = _render_matrices(tmp_path, None)
    entry = data["sections"][0]["features"][0]
    assert entry["title"] == "Read your feed"
    assert entry["what"] == "Your feed shows the posts of the people you follow, newest first."
    assert "| [Read your feed](feed-read.md) |" in md


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


def test_the_site_lists_each_released_version_once():
    # The release table holds one row per BUILD: a version's private verified
    # builds and its release-candidate build all carry its number, and the site's
    # Releases table printed them all — "0.1.5" twice, once per verified dev build,
    # before the release itself was even recorded. A version is released by its
    # candidate run (release-table.md § Tag gate and release table), so the site
    # lists those, one line per version.
    releases = [
        {"version": "0.1.5", "date": "2026-10-20", "verified": "2026-10-20", "promoted": False},
        {"version": "0.1.5", "date": "2026-10-21", "verified": "2026-10-21", "promoted": False},
        {"version": "0.1.5", "date": "2026-10-22", "candidate_run": "2026-10-22", "promoted": False},
        {"version": "0.1.6", "date": "2026-10-30", "verified": "2026-10-30", "promoted": False},
    ]
    assert fc.released_versions(releases) == [
        {"version": "0.1.5", "date": "2026-10-22", "promoted": False}]
    releases.append({"version": "0.1.5", "date": "2026-10-24", "candidate_run": "2026-10-24",
                     "promoted": True})
    assert fc.released_versions(releases) == [
        {"version": "0.1.5", "date": "2026-10-24", "promoted": True}]


def test_matrix_json_carries_the_released_versions(tmp_path):
    features = write_catalog(tmp_path, {})
    (features / "ledger" / "releases.json").write_text(json.dumps({"schema": 1, "releases": [
        {"version": "0.1.5", "date": "2026-10-21", "verified": "2026-10-21", "promoted": False},
        {"version": "0.1.5", "date": "2026-10-22", "candidate_run": "2026-10-22", "promoted": False},
    ]}), encoding="utf-8")
    fc.write_all(features)
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    assert data["versions"] == [{"version": "0.1.5", "date": "2026-10-22", "promoted": False}]


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
    both of the others — the catalog's seven app columns (`APP_COLUMNS`; a kids column
    is never a mark) and the marks the run itself deselects on (`conftest.
    _APP_MARKERS`, whose app-axis values add the kids channels from the catalog's own
    `COLUMN_PARENT`). Read out of conftest by `ast` rather than by import, because
    importing a pytest conftest outside its own session is not a thing this test may
    do."""
    import ast

    import features_scan

    assert features_scan.APP_MARKERS == set(fc.APP_COLUMNS)

    conftest = (Path(__file__).resolve().parent.parent
                / "e2e-unified" / "conftest.py")
    tree = ast.parse(conftest.read_text(encoding="utf-8"), filename=str(conftest))
    known = None
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign)
                and any(getattr(t, "id", None) == "_APP_MARKERS" for t in node.targets)):
            value = node.value
            if isinstance(value, ast.Call) and getattr(value.func, "id", "") == "frozenset":
                value = value.args[0]
            known = ast.literal_eval(value)
    assert known is not None, "conftest no longer defines _APP_MARKERS — repoint this pin"
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


def test_the_scanner_reads_requires_surface_wherever_pytest_would(tmp_path):
    """`TestFacts.surfaces` — the static reader of the `requires_surface` marker
    (e2e-conventions.md convention 7, *The static reader is the catalog's*), read
    at module, class and function level the way the app marks are."""
    import features_scan

    src = tmp_path / "test_surfaces.py"
    src.write_text(
        "import pytest\n\n"
        "pytestmark = pytest.mark.requires_surface('admin-audit-section')\n\n\n"
        "@pytest.mark.requires_surface('events-day-cell')\n"
        "def test_function_level():\n    pass\n\n\n"
        "@pytest.mark.requires_surface('unclean-kill')\n"
        "class TestKlass:\n"
        "    def test_class_level(self):\n        pass\n",
        encoding="utf-8")
    facts = {f.node_id.split("::", 1)[1]: f.surfaces
             for f in features_scan.scan_file(src, repo=tmp_path)}
    assert facts["test_function_level"] == ("admin-audit-section", "events-day-cell")
    assert facts["TestKlass::test_class_level"] == ("admin-audit-section", "unclean-kill")


def test_the_surface_registry_loads_with_its_four_fields(tmp_path):
    path = tmp_path / "surfaces.toml"
    path.write_text(
        '[events-day-cell]\nkind = "product"\nbuilt = ["tui", "web"]\n'
        'detail = "built on tui first"\ntracked = "ui/events.md § Status"\n'
        '\n[unclean-kill]\nkind = "harness"\nbuilt = []\n', encoding="utf-8")
    registry = fc.load_surface_registry(path)
    assert registry["events-day-cell"].built == frozenset({"tui", "web"})
    assert registry["events-day-cell"].tracked == "ui/events.md § Status"
    assert registry["unclean-kill"].kind == "harness" and registry["unclean-kill"].detail == ""
    assert fc.load_surface_registry(tmp_path / "absent.toml") == {}


@pytest.mark.parametrize("body, why", [
    ('[Events]\nkind = "product"\nbuilt = []\n', "kebab-case"),
    ('[x]\nkind = "app"\nbuilt = []\n', "kind must be"),
    ('[x]\nkind = "product"\nbuilt = "tui"\n', "built is a list"),
    ('[x]\nkind = "product"\nbuilt = ["tvos"]\n', "names no app"),
    ('[x]\nkind = "product"\nbuilt = []\nowner = "me"\n', "unknown field"),
    ('[x\n', "surfaces.toml"),
])
def test_a_malformed_surface_registry_is_a_hard_stop(tmp_path, body, why):
    path = tmp_path / "surfaces.toml"
    path.write_text(body, encoding="utf-8")
    with pytest.raises(fc.CatalogError, match=why):
        fc.load_surface_registry(path)


def test_the_real_surface_registry_reads():
    assert fc.load_surface_registry(), "tests/e2e-unified/surfaces.toml is seeded"


# ── the declared route (§ Cell semantics, *Not built is declared*) ──────────
NOT_ON_TUI = frozenset(a for a in fc.APPS if a != "tui")


def _declared(kind="product", built=NOT_ON_TUI, key="feed-refresh", tests=(REFRESH,)):
    """A registry of one surface and a map naming it for `tests`."""
    return fc.Declared(registry={key: fc.Surface(key=key, kind=kind, built=frozenset(built))},
                       surfaces={t: frozenset({key}) for t in tests})


def _declared_cell(tmp_path, tests, declared, nest_tests=None):
    ledger = {}
    if tests is not None:
        ledger["tui"] = ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": tests}})
    if nest_tests is not None:
        ledger["nest"] = ledger_file("nest", {"feed-read": {"cell": cell_entry(),
                                                            "tests": nest_tests}})
    # A fresh world per call: `write_catalog` never deletes a ledger file, so a test
    # comparing two ledgers in one `tmp_path` would read the first one's leftovers.
    root = tmp_path / f"world{len(list(tmp_path.iterdir()))}"
    features = write_catalog(root, {"feed-read": PAGE}, ledger)
    return fc.compute_cell(fc.load_pages(features)[0], "tui", fc.load_ledger(features),
                           None, declared)


def test_a_declared_unbuilt_witness_with_no_record_is_unbuilt(tmp_path):
    """The definition of success: a cell held only by a product surface the registry
    declares unbuilt on its column reads unbuilt with NO record for that column."""
    both = _declared(tests=(OWN, REFRESH))
    assert _declared_cell(tmp_path, None, both).state == "unbuilt"
    assert _declared_cell(tmp_path, {OWN: rec()}, _declared()).state == "unbuilt"


def test_without_the_registry_the_same_cell_is_blank(tmp_path):
    assert _declared_cell(tmp_path, None, None).state == "none"
    assert _declared_cell(tmp_path, None, fc.Declared()).state == "none"


def test_a_declaration_beside_a_real_red_is_partial(tmp_path):
    assert _declared_cell(tmp_path, {OWN: rec("failed")}, _declared()).state == "partial"


def test_a_declaration_beside_a_nest_red_is_partial(tmp_path):
    text = PAGE.replace("   - (none)", f"   - `{DELETE}`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(), "tests": {OWN: rec()}}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(),
                                                   "tests": {DELETE: rec("error")}}})})
    cell = fc.compute_cell(fc.load_pages(features)[0], "tui", fc.load_ledger(features),
                           None, _declared())
    assert cell.state == "partial"


def test_a_declaration_on_a_column_that_built_the_surface_says_nothing(tmp_path):
    built = _declared(built=fc.APPS)
    assert _declared_cell(tmp_path, {OWN: rec()}, built).state == "partial"  # unrun, as before


def test_a_harness_declaration_never_makes_a_cell_unbuilt(tmp_path):
    harness = _declared(kind="harness")
    assert _declared_cell(tmp_path, {OWN: rec()}, harness).state == "partial"
    assert _declared_cell(tmp_path, None, harness).state == "none"


def test_a_harness_held_unbuilt_record_reads_as_unrun_never_unbuilt(tmp_path):
    """The gate takes one unbuilt path for both kinds, so the RECORD of a harness
    surface is `skipped: unbuilt` too; the registry reads it as unrun."""
    held = {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")}
    held[REFRESH]["linux"]["surface"] = "feed-refresh"
    assert _declared_cell(tmp_path, held, _declared(kind="harness")).state == "partial"
    assert _declared_cell(tmp_path, held, _declared()).state == "unbuilt"


def test_a_stale_unbuilt_record_reads_as_unrun(tmp_path):
    """Its surface is built on the column now: neither unbuilt nor a pass."""
    stale = {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")}
    stale[REFRESH]["linux"]["surface"] = "feed-refresh"
    assert _declared_cell(tmp_path, stale, _declared(built=fc.APPS)).state == "partial"
    # A pre-registry record (no `surface`) keeps the recorded reading.
    legacy = {OWN: rec(), REFRESH: rec("skipped", skip_class="unbuilt")}
    assert _declared_cell(tmp_path, legacy, _declared(built=fc.APPS)).state == "unbuilt"


@pytest.mark.parametrize("outcome", ["failed", "error"])
def test_an_ungated_failure_reads_as_unbuilt_not_red(tmp_path, outcome):
    cell = _declared_cell(tmp_path, {OWN: rec(), REFRESH: rec(outcome)}, _declared())
    assert cell.state == "unbuilt"
    # Without the declaration the same record is the red it says it is.
    assert _declared_cell(tmp_path, {OWN: rec(), REFRESH: rec(outcome)}, None).state == "partial"


def test_an_xfail_under_a_declaration_stays_a_real_red(tmp_path):
    cell = _declared_cell(tmp_path, {OWN: rec(), REFRESH: rec("xfailed")}, _declared())
    assert cell.state == "partial"


def test_a_pass_refutes_a_declaration_rather_than_reading_through_it(tmp_path):
    """A passing record under a declaration is lint rule 7's stale declaration; the
    cell shows the pass it recorded."""
    assert _declared_cell(tmp_path, {OWN: rec(), REFRESH: rec()},
                          _declared()).state == "partial"  # nest outcome 3 is (none)


# ── the map read for one record: a gate the test stopped calling ───────────
GATE = "feed-refresh"
ONLY_LINUX = fc.Surface(key=GATE, kind="product", built=frozenset({"linux"}))


def _ledger(**by_app):
    """`{app: {node id: {platform: record}}}` → the loaded-ledger shape."""
    return {app: {"feed-read": {"tests": tests}} for app, tests in by_app.items()}


def _reached(outcome="passed", date="2026-10-30", surfaces=None, platform="linux"):
    r = rec(outcome, date=date, platform=platform)
    if surfaces:
        r[platform]["surfaces"] = list(surfaces)
    return r


def _declared_over(ledger, marks=None):
    return fc.build_declared({GATE: ONLY_LINUX}, marks or {}, ledger)


def test_a_removed_gate_stops_counting_for_a_record_that_did_not_reach_it():
    """The row's case: an older record on one platform reached the gate, a newer one on
    another did not because the test no longer calls it — the newer record is judged
    on what its own run met, so its failure is the real red it is."""
    older = _reached("passed", "2026-10-29", [GATE])
    newer = _reached("failed", "2026-10-30", platform="windows")
    d = _declared_over(_ledger(linux={REFRESH: older}, windows={REFRESH: newer}))
    assert d.surfaces[REFRESH] == {GATE} and d.borrowed[REFRESH] == {GATE}
    assert not d.ungated(REFRESH, newer["windows"], "windows")
    assert d.unbuilt(REFRESH, "windows", record=newer["windows"]) == ()
    # with no record of its own a column still meets what the other run taught it
    assert d.unbuilt(REFRESH, "windows") == (GATE,)


def test_a_record_that_cannot_say_what_it_reached_reads_the_whole_map():
    """Written before the writer listed reached gates: its silence is no evidence."""
    older = _reached("passed", "2026-10-29", [GATE])
    legacy = _reached("failed", "2026-10-20", platform="windows")
    d = _declared_over(_ledger(linux={REFRESH: older}, windows={REFRESH: legacy}))
    assert d.ungated(REFRESH, legacy["windows"], "windows")


def test_a_narrowed_gate_counts_only_where_the_run_reaches_it():
    """The gate moved behind a branch only linux takes: a newer linux record still
    reaches it, windows's does not — order of the runs does not matter."""
    linux = _reached("passed", "2026-10-31", [GATE])
    windows = _reached("failed", "2026-10-30", platform="windows")
    d = _declared_over(_ledger(linux={REFRESH: linux}, windows={REFRESH: windows}))
    assert not d.ungated(REFRESH, windows["windows"], "windows")
    assert d.requires(REFRESH, linux["linux"]) == {GATE}


def test_an_added_gate_reaches_a_record_once_its_run_meets_it():
    """A gate added after windows's last run: that record is judged on what it met (a
    real red until windows runs again); the run that meets it skips as unbuilt."""
    tui = _reached("passed", "2026-10-30", [GATE], platform="linux")
    before = _reached("failed", "2026-10-27", platform="windows")
    d = _declared_over(_ledger(tui={REFRESH: tui}, windows={REFRESH: before}))
    assert not d.ungated(REFRESH, before["windows"], "windows")
    after = _reached("failed", "2026-10-31", [GATE], platform="windows")
    assert d.ungated(REFRESH, after["windows"], "windows")


def test_a_marked_surface_is_the_tests_own_and_never_dropped():
    """A `requires_surface` marker is the test's current code, not a borrowed key."""
    newer = _reached("failed", "2026-10-30", platform="windows")
    d = _declared_over(_ledger(windows={REFRESH: newer}), marks={REFRESH: (GATE,)})
    assert d.borrowed == {}
    assert d.ungated(REFRESH, newer["windows"], "windows")


def test_a_removed_gate_leaves_the_column_with_its_real_red(tmp_path):
    """Through the cell: the failure reads as the red it is, not as unbuilt."""
    older = _reached("passed", "2026-10-29", [GATE])
    newer = _reached("failed", "2026-10-30", platform="linux")
    d = fc.build_declared({GATE: fc.Surface(key=GATE, kind="product",
                                            built=frozenset({"web"}))},
                          {}, _ledger(web={REFRESH: older}, tui={REFRESH: newer}))
    assert _declared_cell(tmp_path, {OWN: rec(), REFRESH: newer}, d).state == "partial"
    stale_reach = _reached("failed", "2026-10-20", platform="linux")
    assert _declared_cell(tmp_path, {OWN: rec(), REFRESH: stale_reach}, d).state == "unbuilt"


def test_the_map_is_the_union_of_markers_and_every_records_surfaces(tmp_path):
    other = "tests/e2e-unified/tests/test_other.py::test_x"
    ledger = {
        "web": {"feed-read": {"tests": {REFRESH: {"linux": {"outcome": "passed",
                                                            "surfaces": ["a-key"]}}}}},
        "tui": {"other": {"tests": {REFRESH: {"windows": {
            "outcome": "skipped", "skip_class": "unbuilt", "surface": "b-key",
            "surfaces": ["b-key"]}}}}},
    }
    scan = {REFRESH: ("m-key",), other: ("m-key",)}
    assert fc.test_surfaces(REFRESH, scan, ledger) == {"a-key", "b-key", "m-key"}
    assert fc.test_surfaces(other, scan, ledger) == {"m-key"}
    assert fc.test_surfaces(OWN, scan, ledger) == frozenset()
    assert fc.surface_map(scan, ledger) == {REFRESH: {"a-key", "b-key", "m-key"},
                                            other: {"m-key"}}


def test_the_render_reads_the_registry_and_markers_beside_the_catalog(tmp_path):
    """End to end through `render_all`: the fixture tree's own `surfaces.toml` and its
    test's `requires_surface` marker make tui's cell unbuilt with no record, the word
    reaches `matrix.json`, and the page's `## Status` names the key beside the
    witness."""
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "tui": ledger_file("tui", {"feed-read": {"cell": cell_entry(),
                                                 "tests": {OWN: rec()}}})})
    e2e = tmp_path / "tests" / "e2e-unified"
    (e2e / "tests").mkdir(parents=True)
    (e2e / "tests" / "test_feed.py").write_text(
        "import pytest\n\n\n"
        "@pytest.mark.feature('feed-read')\n"
        "def test_own_post_appears(app):\n    pass\n\n\n"
        "@pytest.mark.feature('feed-read')\n"
        "@pytest.mark.requires_surface('feed-refresh')\n"
        "def test_refresh(app):\n    pass\n", encoding="utf-8")
    (e2e / "surfaces.toml").write_text(
        '[feed-refresh]\nkind = "product"\nbuilt = ["web", "linux"]\n', encoding="utf-8")
    fc.write_all(features)
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    cells = data["sections"][0]["features"][0]["cells"]
    assert cells["tui"]["state"] == "unbuilt"
    assert cells["web"]["state"] == "none"
    status = (features / "feed-read.md").read_text(encoding="utf-8")
    assert ("`tests/e2e-unified/tests/test_feed.py::test_refresh` — requires "
            "`feed-refresh` (not built: windows, macos, ios, android, tui, android-kids, "
            "ios-kids)") in status


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


def test_a_test_that_is_only_its_second_app_seat_narrows_to_the_seat(tmp_path):
    """A `SECOND_APP_FIXTURES` seat beside the run's own `app` leaves the column the
    run's — but a test requesting NO other app fixture IS its seat: conftest
    deselects it unless every seat value was selected, so it speaks only for the
    seats' apps. The scan read it unrestricted, and `--runnable` listed it on columns
    whose run collects `0 of N` (the cross-nest foreign-row witness's old five-app
    shape was the live case). Two seats are one restriction, their union (conftest needs every seat selected, then matches any-of); a mark intersects it."""
    facts = _scan_apps(tmp_path, (
        "import pytest\n\n\n"
        "@pytest.mark.parametrize(\"folder_share_owner_app\", [\"tui\", \"macos\"], indirect=True)\n"
        "def test_seat_only(folder_share_owner_app):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"folder_share_owner_app\", [\"tui\", \"macos\"], indirect=True)\n"
        "@pytest.mark.parametrize(\"folder_share_recipient_app\", [\"macos\", \"windows\"], indirect=True)\n"
        "def test_two_seats(folder_share_owner_app, folder_share_recipient_app):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"folder_share_owner_app\", [\"tui\"], indirect=True)\n"
        "def test_seat_beside_app(app, folder_share_owner_app):\n    pass\n"))
    assert facts == {"test_seat_only": ("macos", "tui"), "test_two_seats": ("macos", "tui", "windows"),
                     "test_seat_beside_app": ()}


def test_an_app_set_imported_from_a_helper_is_read_in_the_helpers_scope(tmp_path):
    """`SKEW_CLIENTS` / `AT_REST_CLIENTS` live in `helpers/skew_client.py`, the second a
    filtering `tuple(...)` over the first that the importing module never sees. The
    scan resolved only names bound in the test module, so an imported tuple read as
    'no restriction' and the version-skew suite counted on tui and web."""
    helpers = tmp_path / "helpers"
    helpers.mkdir()
    (helpers / "skew_client.py").write_text(
        "SKEW_CLIENTS = (\"linux\", \"macos\", \"windows\")\n"
        "AT_REST_CLIENTS = tuple(c for c in SKEW_CLIENTS if supports(c))\n"
        "OTHER = get_available_apps()\n", encoding="utf-8")
    facts = _scan_apps(tmp_path, (
        "import pytest\n"
        "from helpers.skew_client import AT_REST_CLIENTS, SKEW_CLIENTS, OTHER\n\n\n"
        "@pytest.fixture(params=SKEW_CLIENTS)\n"
        "def skew(request):\n    yield request.param\n\n\n"
        "@pytest.fixture(params=AT_REST_CLIENTS)\n"
        "def at_rest(request):\n    yield request.param\n\n\n"
        "@pytest.fixture(params=OTHER)\n"
        "def unnameable(request):\n    yield request.param\n\n\n"
        "def test_skew(skew):\n    pass\n\n\n"
        "def test_at_rest(at_rest):\n    pass\n\n\n"
        "def test_unnameable(unnameable):\n    pass\n"))
    assert facts == {"test_skew": ("linux", "macos", "windows"),
                     "test_at_rest": ("linux", "macos", "windows"),
                     "test_unnameable": ()}


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
    # `AUTOSTART_APPS` grew from windows alone; the test name keeps its first
    # spelling so the last-green ledger key stays stable.
    assert case_j.apps == ("linux", "macos", "windows")
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
    assert page.completable() == [a for a in fc.APPS if fc.parent_app(a) != "android"], (
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
    windows, so it is short on five other columns (android is declared absent, and its
    kids channel with it; ios's kids channel is short beside ios) — and that is ONE gap
    with a reach of five, never five separate pieces of work."""
    features = _per_app_catalog(tmp_path, [])
    gaps = fc.gap_reach(fc.load_pages(features), MARKS_PER_APP)
    assert list(gaps) == [("feed-read", 1)]
    gap = gaps[("feed-read", 1)]
    assert gap.witnesses == ("windows", "tui")
    assert gap.short == ("web", "linux", "macos", "ios", "ios-kids")
    assert gap.reach == 5


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
    is at most the cap. The fixture's one gap reaches five columns, so a cap of 5 hands
    it to linux's pass with its reference columns named, and a cap of 4 leaves it to a
    wider row — the same gap is never in both."""
    features = _per_app_catalog(tmp_path, [])
    pages = fc.load_pages(features)
    at_five = fc.column_trickle_down(pages, "linux", 5, MARKS_PER_APP)
    assert [(g.page.slug, g.outcome.number, g.witnesses) for g in at_five] == [
        ("feed-read", 1, ("windows", "tui"))]
    assert fc.column_trickle_down(pages, "linux", 4, MARKS_PER_APP) == []
    # A witnessing column owes nothing, and neither does a declared-absent one.
    assert fc.column_trickle_down(pages, "tui", 5, MARKS_PER_APP) == []
    assert fc.column_trickle_down(pages, "android", 5, MARKS_PER_APP) == []


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
    assert page.absences == {"android": "docs/goal/ui/feed.md § Platform notes",
                             "android-kids": "docs/goal/ui/feed.md § Platform notes"}, (
        "the page-level map keeps its meaning: `—` on the whole column (and its kids "
        "channel's)")
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
    assert gap.short == ("linux", "windows", "macos", "ios", "ios-kids"), (
        "web is excused from the outcome, android (and its kids channel) from the page")


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
    assert entry["absences"] == {"android": "docs/goal/ui/feed.md § Platform notes",
                                 "android-kids": "docs/goal/ui/feed.md § Platform notes"}


# ── the last-green reader (delayed-testing.md § The last-green directory) ───
def _git(repo, *args):
    import os
    import subprocess
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@e",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@e")
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True,
                          text=True, env=env).stdout.strip()


def _commit(repo, rel, message):
    path = repo / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(message + "\n", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "--quiet", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


def _fixture_repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet", "--initial-branch=main")
    return repo


def test_load_last_green_reads_one_platforms_snapshot(tmp_path):
    features = tmp_path / "features"
    (features / "last-green").mkdir(parents=True)
    record = {"collected": "2026-10-20", "last_green": {"base": "a" * 40}}
    (features / "last-green" / "linux.json").write_text(
        '{"platform": "linux", "schema": 1, "tests": {\n'
        f'"t.py::test_x[tui]": {json.dumps(record)}\n' + "}}\n", encoding="utf-8")
    assert fc.load_last_green("linux", features) == {"t.py::test_x[tui]": record}
    assert fc.load_last_green("windows", features) == {}, "a platform that never published"


def test_behind_commits_counts_only_the_tests_trees(tmp_path):
    """A quiet weekend ages nothing and a `libs/` sweep ages everything: the count is
    `base..ref` restricted to the shared trees plus the test's own app's."""
    repo = _fixture_repo(tmp_path)
    base = _commit(repo, "README", "start")
    _commit(repo, "libs/fauna-core/x.rs", "shared code")        # counts for every app
    _commit(repo, "apps/fauna-tui/y.rs", "the tui's tree")      # counts for tui only
    _commit(repo, "apps/fauna-apple/z.swift", "the apple tree")  # counts for macos and ios
    _commit(repo, "docs/goal/q.md", "a doc")                    # counts for nobody
    record = {"last_green": {"base": base}}
    assert fc.behind_commits(record, "tui", repo, ref="main") == 2
    assert fc.behind_commits(record, "macos", repo, ref="main") == 2
    assert fc.behind_commits(record, "ios", repo, ref="main") == 2
    assert fc.behind_commits(record, "linux", repo, ref="main") == 1
    assert fc.behind_commits(record, "nest", repo, ref="main") == 1


def test_behind_commits_is_unknown_without_a_green_or_a_resolvable_base(tmp_path):
    repo = _fixture_repo(tmp_path)
    _commit(repo, "README", "start")
    assert fc.behind_commits({"last_green": None}, "tui", repo, ref="main") is None
    assert fc.behind_commits({"last_green": {"base": None}}, "tui", repo, ref="main") is None
    assert fc.behind_commits({"last_green": {"base": "f" * 40}}, "tui", repo, ref="main") is None


def test_a_seat_list_spelled_through_a_module_constant_still_carries_its_marks(tmp_path):
    """`pytest.param("android", marks=pytest.mark.android)` inside a module constant
    (`_MEMBER_SEATS`) that a `parametrize` names is collected with the mark at run
    time — pytest adds a param's marks to its item — so the item is selected on that
    app's run. The scan walked only the decorator's own nodes, so a constant's marks
    were invisible and a seat arm that ran on android credited the column to nobody
    (the share-a-folder member witnesses, whose android and web/ios arms were real)."""
    facts = _scan_apps(tmp_path, (
        "import pytest\n\n"
        "_SEATS = [\n"
        "    pytest.param(\"tui\", marks=pytest.mark.tui),\n"
        "    pytest.param(\"android\", marks=pytest.mark.android),\n"
        "]\n\n\n"
        "@pytest.mark.parametrize(\"media_member\", _SEATS, indirect=True)\n"
        "def test_member(media_member):\n    pass\n\n\n"
        "@pytest.mark.parametrize(\"media_member\", [\"tui\"], indirect=True)\n"
        "def test_literal(media_member):\n    pass\n"))
    assert facts["test_member"] == ("android", "tui")


# ── two sources: the ledger and the published last-green snapshot ───────────
# feature-catalog.md § Cell semantics, *Two sources*: each record is the newer of the
# ledger's and the snapshot's for its key; the snapshot in git, never the box's store.
OWN = "tests/e2e-unified/tests/test_feed.py::test_own_post_appears"
REFRESH_ID = "tests/e2e-unified/tests/test_feed.py::test_refresh"
LEDGER_BASE = "a" * 40
LATER_BASE = "b" * 40


def _ledger_rec(outcome="failed", date="2026-10-31", skip_class=None):
    record = rec(outcome, date=date, skip_class=skip_class)
    record["linux"]["base"] = LEDGER_BASE
    return record


def _dir_rec(outcome="green", time="2026-11-01T10:00:00+00:00", base=LATER_BASE,
             dirty=False):
    green = ({"base": base, "commit": base, "dirty": dirty, "duration": 1.0,
              "origin": "session", "time": time} if outcome == "green" else None)
    return {"collected": time[:10], "flaky": {"count": 0, "last": None},
            "last": {"commit": base, "outcome": outcome, "time": time},
            "last_green": green, "red_since": None}


def _two_source_catalog(tmp_path, own_ledger, snapshot):
    """A full-contract page whose two app witnesses the linux ledger holds; the
    snapshot is `docs/features/last-green/linux.json`'s `tests` map."""
    text = PAGE.replace("   - (none)",
                        "   - `tests/e2e-unified/tests/api/test_delete.py::test_delete`")
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "linux": ledger_file("linux", {"feed-read": {"cell": cell_entry(), "tests": {
            OWN: own_ledger, REFRESH_ID: rec(),
        }}}),
        "nest": ledger_file("nest", {"feed-read": {"cell": cell_entry(), "tests": {
            "tests/e2e-unified/tests/api/test_delete.py::test_delete": rec(),
        }}}),
    })
    (features / "last-green").mkdir()
    (features / "last-green" / "linux.json").write_text(json.dumps(
        {"platform": "linux", "schema": 1, "tests": snapshot}), encoding="utf-8")
    return features


def _linux_cell(features, ancestor=lambda older, newer: False):
    ledger = fc.load_ledger(features, last_green=False)
    fc.overlay_last_green(ledger, features, ancestor=ancestor)
    return fc.compute_cell(fc.load_pages(features)[0], "linux", ledger), ledger


def test_a_newer_directory_green_supersedes_a_ledger_red(tmp_path):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[linux]": _dir_rec("green")})
    cell, ledger = _linux_cell(features)
    assert cell.state == "full"
    record = ledger["linux"]["feed-read"]["tests"][OWN]["linux"]
    assert (record["outcome"], record["date"], record["source"]) == \
        ("passed", "2026-11-01", "last-green")
    # The files as written are untouched: only a file can be pruned.
    raw = fc.load_ledger(features, last_green=False)
    assert raw["linux"]["feed-read"]["tests"][OWN]["linux"]["outcome"] == "failed"


def test_the_default_load_reads_both_sources(tmp_path):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[linux]": _dir_rec("green")})
    assert fc.load_ledger(features)["linux"]["feed-read"]["tests"][OWN]["linux"][
        "outcome"] == "passed"


def test_an_older_directory_record_leaves_the_ledger_standing(tmp_path):
    features = _two_source_catalog(
        tmp_path, _ledger_rec("failed"),
        {OWN + "[linux]": _dir_rec("green", time="2026-10-30T10:00:00+00:00")})
    cell, _ = _linux_cell(features)
    assert cell.state == "partial"


def test_a_newer_directory_unbuilt_skip_reads_unbuilt(tmp_path):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[linux]": _dir_rec("skip:unbuilt")})
    cell, _ = _linux_cell(features)
    assert cell.state == "unbuilt"


@pytest.mark.parametrize("outcome", ["red", "error"])
def test_a_directory_red_never_supersedes_the_ledger(tmp_path, outcome):
    """The directory notes reds the ledger declines (a setup-time NestModeError, an
    unnameable own artifact, a strict refusal), so a red cannot say the ledger would
    have recorded it."""
    features = _two_source_catalog(tmp_path, _ledger_rec("passed"),
                                   {OWN + "[linux]": _dir_rec(outcome)})
    cell, _ = _linux_cell(features)
    assert cell.state == "full"


@pytest.mark.parametrize("descends, state", [(True, "full"), (False, "partial")])
def test_on_the_same_day_the_merge_bases_decide(tmp_path, descends, state):
    features = _two_source_catalog(
        tmp_path, _ledger_rec("failed"),
        {OWN + "[linux]": _dir_rec("green", time="2026-10-31T21:00:00+00:00")})
    asked = []

    def ancestor(older, newer):
        asked.append((older, newer))
        return descends
    cell, _ = _linux_cell(features, ancestor)
    assert cell.state == state
    assert asked == [(LEDGER_BASE, LATER_BASE)]


def test_the_columns_own_key_answers_and_its_mode_narrows(tmp_path):
    """`[tui]` is another column's run and `[linux-docker]` another mode's."""
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"), {
        OWN + "[tui]": _dir_rec("green"),
        OWN + "[linux-docker]": _dir_rec("green"),
    })
    cell, _ = _linux_cell(features)
    assert cell.state == "partial"


@pytest.mark.parametrize("second, state", [("green", "full"), ("red", "partial")])
def test_every_variant_the_ledger_collapsed_must_agree(tmp_path, second, state):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"), {
        OWN + "[linux-eml]": _dir_rec("green"),
        OWN + "[linux-mbox]": _dir_rec(second),
    })
    cell, _ = _linux_cell(features)
    assert cell.state == state


def test_a_dirty_directory_green_marks_the_cell_dirty(tmp_path):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[linux]": _dir_rec("green", dirty=True)})
    cell, ledger = _linux_cell(features)
    assert cell.state == "full"
    assert cell.dirty
    assert ledger["linux"]["feed-read"]["tests"][OWN]["linux"]["stamp"] == \
        "0.1.2-dev+bbbbbbbb.dirty standalone"


# A convention-16 seat set is keyed by seat MODES (`[2seat-native+native]`), never by
# an app; the catalog resolves each mode to the app it launches on the snapshot's
# platform through conftest's own table (`tests/e2e-unified/helpers/seat_modes.py`).
def _seat_candidates(*params):
    return [(fc._split_directory_key(OWN + f"[{p}]")[1], {"key": p}) for p in params]


def _answering(candidates, app, platform, nest_mode=None):
    return [r["key"] for r in fc.directory_records_for(candidates, app, nest_mode, platform)]


def test_a_native_seat_key_answers_the_platforms_own_app(tmp_path):
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[2seat-native+native]": _dir_rec("green")})
    cell, ledger = _linux_cell(features)
    assert cell.state == "full"
    assert ledger["linux"]["feed-read"]["tests"][OWN]["linux"]["source"] == "last-green"


def test_a_seat_key_of_another_app_never_falls_back_to_answer(tmp_path):
    """The no-app-token fallback is for a test with no app axis; a `tui+tui` seat
    key names tui through its modes and must not answer a linux record."""
    features = _two_source_catalog(tmp_path, _ledger_rec("failed"),
                                   {OWN + "[2seat-tui+tui]": _dir_rec("green")})
    cell, _ = _linux_cell(features)
    assert cell.state == "partial"


@pytest.mark.parametrize("platform, app", [("linux", "linux"), ("macos", "macos"),
                                           ("windows", "windows")])
def test_native_resolves_per_platform_and_never_answers_tui(platform, app):
    candidates = _seat_candidates("2seat-native+native", "2seat-tui+tui")
    assert _answering(candidates, app, platform) == ["2seat-native+native"]
    assert _answering(candidates, "tui", platform) == ["2seat-tui+tui"]


def test_the_snapshot_platform_maps_to_sys_platform():
    assert fc.seat_key_apps(("native", "native"), "windows") == {"windows"}
    assert fc.seat_key_apps(("native", "native"), "macos") == {"macos"}
    assert fc.seat_key_apps(("phone", "tui"), "macos") == {"ios"}
    assert fc.seat_key_apps(("native", "native"), "plan9") == frozenset()
    assert fc.seat_key_apps(("native", "native"), None) == frozenset()


def test_a_device_only_seat_is_the_only_app_credited():
    candidates = _seat_candidates("2seat-tui+phone", "2seat-web+tui")
    assert _answering(candidates, "android", "linux") == ["2seat-tui+phone"]
    assert _answering(candidates, "web", "linux") == ["2seat-web+tui"]
    assert _answering(candidates, "tui", "linux") == []


def test_an_unknown_mode_answers_no_app_and_not_the_nest():
    candidates = _seat_candidates("2seat-engine+engine")
    for app in ("linux", "tui"):
        assert _answering(candidates, app, "linux") == []
    assert _answering(candidates, fc.NEST, "linux") == []


def test_a_docker_seat_key_answers_only_the_docker_record():
    candidates = _seat_candidates("2seat-native+native", "docker-2seat-native+native")
    assert _answering(candidates, "linux", "linux", "docker") == ["docker-2seat-native+native"]
    assert _answering(candidates, "linux", "linux") == ["2seat-native+native"]


def test_no_snapshot_reads_the_ledger_alone(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": PAGE}, {
        "linux": ledger_file("linux", {"feed-read": {"cell": cell_entry(), "tests": {
            OWN: _ledger_rec("failed")}}})})
    assert fc.load_ledger(features) == fc.load_ledger(features, last_green=False)


# ── the kids channels as columns (feature-catalog.md § Cell semantics, *A second store
# identity of an app … is a COLUMN*) ──────────────────────────────────────────
def test_the_kids_channels_are_columns_after_the_seven_each_with_its_parent():
    assert fc.APPS == fc.APP_COLUMNS + ("android-kids", "ios-kids")
    assert len(fc.APPS) == 9
    assert fc.COLUMN_PARENT == {"android-kids": "android", "ios-kids": "ios"}
    assert fc.parent_app("android-kids") == "android" and fc.parent_app("tui") == "tui"
    # It runs where its parent runs, from its parent's tree.
    assert fc.COLUMN_PLATFORMS["android-kids"] == fc.COLUMN_PLATFORMS["android"]
    assert fc.COLUMN_PLATFORMS["ios-kids"] == fc.COLUMN_PLATFORMS["ios"]
    assert fc.app_tree("ios-kids") == fc.app_tree("ios") == "apps/fauna-apple/"


def test_a_kids_column_can_have_its_parents_marked_witnesses_and_the_unmarked():
    marks = {"a": ("android",), "b": ("ios",), "c": ("tui", "android")}
    assert fc.applicable("a", "android-kids", marks)
    assert not fc.applicable("b", "android-kids", marks)
    assert fc.applicable("c", "android-kids", marks)
    assert fc.applicable("unmarked", "ios-kids", marks)
    assert fc.applicable("b", "ios-kids", marks)


KIDS_ABSENT_PAGE = PAGE.replace(
    "  android: docs/goal/ui/feed.md § Platform notes\n",
    "  android: docs/goal/ui/feed.md § Platform notes\n"
    "  ios-kids: docs/goal/behavior/account-age-band.md § The account age band\n")


def test_a_page_declaring_a_kids_column_absent_parses_and_renders_a_dash(tmp_path):
    features = write_catalog(tmp_path, {"feed-read": KIDS_ABSENT_PAGE})
    page = fc.parse_page(features / "feed-read.md")
    assert page.absences["ios-kids"] == \
        "docs/goal/behavior/account-age-band.md § The account age band"
    assert "ios-kids" not in page.inherited_absences
    assert fc.compute_cell(page, "ios-kids", {}).state == "absent"
    fc.write_all(features)
    matrix = (features / "MATRIX.md").read_text(encoding="utf-8")
    header = next(line for line in matrix.splitlines() if line.startswith("| Feature |"))
    assert header == "| Feature | " + " | ".join(fc.APPS) + " |"
    row = next(line for line in matrix.splitlines() if line.startswith("| [Read your feed]"))
    cells = [c.strip() for c in row.strip("|").split("|")][1:]
    assert dict(zip(fc.APPS, cells))["ios-kids"] == fc.MARKS["absent"]
    data = json.loads((features / "matrix.json").read_text(encoding="utf-8"))
    assert data["apps"] == list(fc.APPS)
    assert data["parents"] == fc.COLUMN_PARENT
    status = (features / "feed-read.md").read_text(encoding="utf-8")
    assert "| ios-kids | — absent |" in status


def test_a_parents_absence_carries_to_its_kids_column(tmp_path):
    page = fc.parse_page(write_catalog(tmp_path, {"feed-read": PAGE}) / "feed-read.md")
    assert page.absences["android-kids"] == page.absences["android"]
    assert page.inherited_absences == frozenset({"android-kids"})
    assert fc.compute_cell(page, "android-kids", {}).state == "absent"
    assert "ios-kids" not in page.absences


def test_a_parents_outcome_absence_excuses_its_kids_column_from_that_outcome(tmp_path):
    text = OUTCOME_ABSENT_PAGE.replace("web (outcome 1):", "ios (outcome 1):")
    page = fc.parse_page(write_catalog(tmp_path, {"feed-read": text}) / "feed-read.md")
    assert page.absent_from("ios-kids", page.outcomes[0])
    assert not page.absent_from("ios-kids", page.outcomes[1])
    assert page.inherited_absences == frozenset({"android-kids", "ios-kids (outcome 1)"})


def test_kids_owed_keeps_a_kids_column_owed_beside_an_absent_parent(tmp_path):
    # The `kids-app` page: absent on android and ios, owed on their kids columns.
    text = PAGE.replace("absences:\n", "kids-owed: android-kids\nabsences:\n")
    page = fc.parse_page(write_catalog(tmp_path, {"feed-read": text}) / "feed-read.md")
    assert "android-kids" not in page.absences and "android-kids" not in page.inherited_absences
    assert fc.compute_cell(page, "android-kids", {}).state != "absent"
    assert fc.compute_cell(page, "android", {}).state == "absent"


def test_kids_owed_names_only_kids_columns_and_never_one_it_also_excuses(tmp_path):
    bad = PAGE.replace("absences:\n", "kids-owed: android\nabsences:\n")
    with pytest.raises(fc.CatalogError, match="not kids columns"):
        fc.parse_page(write_catalog(tmp_path, {"feed-read": bad}) / "feed-read.md")
    both = KIDS_ABSENT_PAGE.replace("absences:\n", "kids-owed: ios-kids\nabsences:\n")
    with pytest.raises(fc.CatalogError, match="also excuses"):
        fc.parse_page(write_catalog(tmp_path, {"feed-read": both}) / "feed-read.md")


def test_a_kids_outcome_absence_on_a_page_its_parent_is_absent_from_is_refused(tmp_path):
    text = PAGE.replace(
        "  android: docs/goal/ui/feed.md § Platform notes\n",
        "  android: docs/goal/ui/feed.md § Platform notes\n"
        "  android-kids (outcome 1): docs/goal/ui/feed.md § Platform notes\n")
    features = write_catalog(tmp_path, {"feed-read": text})
    with pytest.raises(fc.CatalogError, match="absent from the whole page"):
        fc.parse_page(features / "feed-read.md")


def test_a_kids_column_reads_its_own_ledger_file_never_its_parents(tmp_path):
    text = PAGE.replace("absences:\n  android: docs/goal/ui/feed.md § Platform notes\n",
                        "").replace("   - (none)", f"   - `{DELETE}`")
    green = {"cell": cell_entry(), "tests": {OWN: rec(), REFRESH: rec()}}
    nest = {"cell": cell_entry(), "tests": {DELETE: rec()}}
    features = write_catalog(tmp_path, {"feed-read": text}, {
        "android": ledger_file("android", {"feed-read": green}),
        "nest": ledger_file("nest", {"feed-read": nest})})
    pages, ledger = fc.load_pages(features), fc.load_ledger(features)
    assert fc.compute_cell(pages[0], "android", ledger).state == "full"
    assert fc.compute_cell(pages[0], "android-kids", ledger).state == "none"


def test_a_kids_directory_key_answers_the_kids_column_and_never_its_parent():
    candidates = _seat_candidates("android-kids")
    assert fc.token_columns(candidates[0][0]) == {"android-kids"}
    assert _answering(candidates, "android-kids", "linux") == ["android-kids"]
    assert _answering(candidates, "android", "linux") == []
    assert _answering(candidates, fc.NEST, "linux") == []


def test_the_surface_registry_accepts_the_kids_columns_in_built(tmp_path):
    path = tmp_path / "surfaces.toml"
    path.write_text('[events-day-cell]\nkind = "product"\n'
                    'built = ["ios", "ios-kids"]\n', encoding="utf-8")
    entry = fc.load_surface_registry(path)["events-day-cell"]
    assert entry.built == frozenset({"ios", "ios-kids"})
    path.write_text('[events-day-cell]\nkind = "product"\nbuilt = ["wear-kids"]\n',
                    encoding="utf-8")
    with pytest.raises(fc.CatalogError, match="names no app"):
        fc.load_surface_registry(path)


def test_a_kids_column_has_its_parents_driver_but_not_its_parents_product_surfaces(tmp_path):
    """A harness surface is the e2e driver's, and the kids build is driven by its
    parent's driver — so the reader gives the kids column the parent's answer. A product
    surface is the app's, which the kids build may excise: only a hand listing counts."""
    path = tmp_path / "surfaces.toml"
    path.write_text('[app-log-reader]\nkind = "harness"\nbuilt = ["android", "tui"]\n\n'
                    '[events-day-cell]\nkind = "product"\nbuilt = ["android", "ios"]\n',
                    encoding="utf-8")
    registry = fc.load_surface_registry(path)
    assert registry["app-log-reader"].built == frozenset({"android", "android-kids", "tui"})
    assert registry["events-day-cell"].built == frozenset({"android", "ios"})
