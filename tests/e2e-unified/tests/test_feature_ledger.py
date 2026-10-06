"""The feature catalog's runtime half: the marker, `--feature`, and the ledger writer.

`docs/goal/architecture/feature-catalog.md` § The marker and § The ledger. tier_1 by
the taxonomy's own decision tree — no nest binary, no driver, no external process: the
ledger writer is a pure function of "what the run observed", and the marker half is
exercised against the live conftest's own selection helpers rather than a copy of
them, so a change to the real hook that broke the rule would red these.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "helpers"))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from helpers import feature_ledger as fl  # noqa: E402


@pytest.fixture(autouse=True)
def _clean_ledger_state(monkeypatch):
    """A clean ledger for each case — by SWAPPING the module's session state, never
    clearing it: in a session with the ledger on, these hold the live run's own
    records, and emptying them would silently discard every sibling test's
    outcome. (`_fixture_stack` is left alone: conftest's `pytest_fixture_setup`
    wrapper is pushing and popping it around this very fixture.)"""
    for name in ("_records", "_collected", "_seen", "_refusals", "_artifacts_by_fixture"):
        monkeypatch.setattr(fl, name, type(getattr(fl, name))())
    fl.reset_test_state()
    yield
    fl.reset_test_state()


SURFACES = {"feed-read": {"app": {"t.py::a", "t.py::b"}, "nest": {"t.py::n"}}}


COMMIT = "abcdef12" + "0" * 32


def _record(app, slug, node_id, outcome="passed", stamp="0.1.2-dev+abcdef12 standalone",
            dirty=False, base=COMMIT):
    fl.record(app, slug, node_id, outcome=outcome, skip_class=None, version="0.1.2",
              commit=COMMIT, base=base, image_digest=None, nest_mode="standalone",
              record_stamp=stamp, dirty=dirty, today="2026-08-26")


# ── the stamp grammar ───────────────────────────────────────────────────────
def test_a_plain_run_stamps_the_dev_form():
    assert fl.stamp("0.1.2", "8213688bbc99", "standalone", release_candidate=False) == \
        "0.1.2-dev+8213688b standalone"


def test_a_release_candidate_run_stamps_the_bare_version():
    """A bare stamp is read, on its own, as "against the released artifact"."""
    assert fl.stamp("0.1.2", "8213688bbc99", "docker", release_candidate=True) == \
        "0.1.2 docker"


def test_the_version_comes_from_the_workspace_manifest():
    version = fl.workspace_version()
    assert version.count(".") == 2 and version[0].isdigit()


def test_the_platform_is_the_host_os_never_a_host_name():
    assert fl.host_platform() in {"linux", "macos", "windows"}


# ── recording ───────────────────────────────────────────────────────────────
def test_a_worse_outcome_wins_within_one_session(tmp_path):
    """Two parametrizations collapse to one line — a green one must not bury a red."""
    _record("tui", "feed-read", "t.py::a", outcome="passed")
    _record("tui", "feed-read", "t.py::a", outcome="failed")
    _record("tui", "feed-read", "t.py::a", outcome="passed")
    fl.write(tmp_path, surfaces=SURFACES)
    data = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))
    assert data["features"]["feed-read"]["tests"]["t.py::a"][fl.host_platform()]["outcome"] == "failed"


def test_the_writer_merges_and_never_replaces(tmp_path):
    (tmp_path).mkdir(parents=True, exist_ok=True)
    (tmp_path / "tui.json").write_text(json.dumps({
        "schema": 1, "app": "tui", "features": {"other-feature": {
            "cell": {"stamp": "0.1.1 docker", "date": "2026-08-01", "complete": True},
            "tests": {"t.py::old": {"outcome": "passed"}}}}}), encoding="utf-8")
    _record("tui", "feed-read", "t.py::a")
    fl.write(tmp_path, surfaces=SURFACES)
    data = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))
    assert "other-feature" in data["features"], "an untouched record still stands"
    assert "feed-read" in data["features"]


def test_one_file_per_app_so_three_machines_never_conflict(tmp_path):
    _record("tui", "feed-read", "t.py::a")
    _record("web", "feed-read", "t.py::a")
    _record("nest", "feed-read", "t.py::n")
    fl.write(tmp_path, surfaces=SURFACES)
    assert {p.name for p in tmp_path.glob("*.json")} == {"tui.json", "web.json", "nest.json"}


# ── platform keying (feature-catalog.md § The ledger, Platform keying) ──────
def test_two_platforms_of_one_test_coexist_in_the_ledger(tmp_path, monkeypatch):
    """A tui run on windows must not overwrite a tui run's linux record for the
    same test — the bug this key shape exists to fix: a tui/windows run once
    silently destroyed the only witness that
    `test_sign_out_erases_the_account_scoped_state_directory` had ever run on
    tui/linux, and a reader (including a reviewer) trusted the wrong platform."""
    monkeypatch.setattr(fl, "host_platform", lambda: "linux")
    _record("tui", "feed-read", "t.py::a", outcome="passed")
    fl.write(tmp_path, surfaces=SURFACES)
    fl.reset_session_state()
    monkeypatch.setattr(fl, "host_platform", lambda: "windows")
    _record("tui", "feed-read", "t.py::a", outcome="failed")
    fl.write(tmp_path, surfaces=SURFACES)
    record = json.loads((tmp_path / "tui.json").read_text(
        encoding="utf-8"))["features"]["feed-read"]["tests"]["t.py::a"]
    assert record["linux"]["outcome"] == "passed", "the earlier platform's record must survive"
    assert record["windows"]["outcome"] == "failed", "the later platform's record must be visible too"


def test_a_complete_run_on_one_platform_does_not_erase_another_platforms_complete_cell(tmp_path, monkeypatch):
    """The cell-stamp collision one level up: two platforms can each independently
    complete the feature's whole tagged set, and neither's stamp may erase the
    other's."""
    monkeypatch.setattr(fl, "host_platform", lambda: "linux")
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::b")
    fl.write(tmp_path, surfaces=SURFACES)
    fl.reset_session_state()
    monkeypatch.setattr(fl, "host_platform", lambda: "windows")
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::b")
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"]["platforms"]["linux"]["complete"] is True
    assert entry["cell"]["platforms"]["windows"]["complete"] is True


# ── the whole-set rule ──────────────────────────────────────────────────────
def test_a_complete_run_stamps_the_cell(tmp_path):
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::b")
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"]["complete"] is True
    assert entry["cell"]["stamp"] == "0.1.2-dev+abcdef12 standalone"


def test_a_subset_run_updates_lines_but_leaves_the_cell_alone(tmp_path):
    """`-k` picks one of the two app witnesses: per-test lines move, the cell does not."""
    fl.note_collected("feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::a")
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert "t.py::a" in entry["tests"]
    assert "cell" not in entry, "an incomplete observation may not stamp a cell"


def test_an_apps_cell_is_judged_against_the_app_witnesses_only(tmp_path):
    """The nest witness of the same feature is not part of an app column's set."""
    for node in ("t.py::a", "t.py::b", "t.py::n"):
        fl.note_collected("feed-read", node)
    _record("tui", "feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::b")
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"]["complete"] is True


# ── attribution + skip classes ──────────────────────────────────────────────
def test_app_attribution_accumulates_every_driver_the_test_took():
    fl.note_app("tui")
    fl.note_app("macos")
    fl.note_app("tui")
    assert fl.apps_this_test() == {"tui", "macos"}


def test_the_per_test_slots_reset_between_tests():
    fl.note_app("tui")
    fl.note_skip_class(fl.UNBUILT)
    fl.reset_test_state()
    assert fl.apps_this_test() == set()
    assert fl.skip_class_this_test() is None


def test_the_declaration_helpers_name_their_skip_class():
    """The writer must never have to parse a skip message to classify it."""
    from helpers import app_surface

    for helper, expected in (
        (lambda: app_surface.declared_absence("tui", capability="x", doc="d.md § s"),
         fl.ABSENCE),
        (lambda: app_surface.skip_environment("no credential"), fl.ENVIRONMENT),
        (lambda: app_surface.skip_unbuilt("tui", surface="x"), fl.UNBUILT),
    ):
        fl.reset_test_state()
        with pytest.raises(BaseException):
            helper()
        assert fl.skip_class_this_test() == expected


# ── the release-candidate gate ──────────────────────────────────────────────
class _Mode:
    def __init__(self, name, argument=None):
        self.name, self.argument = name, argument
        self.is_docker = name == "docker"

    def __str__(self):
        return self.name


def test_a_standalone_run_may_never_stamp_a_bare_version():
    assert "docker" in fl.release_candidate_refusal(_Mode("standalone"))


def test_a_docker_run_with_no_image_is_refused():
    assert "names no image" in fl.release_candidate_refusal(_Mode("docker"))


def test_a_locally_built_image_has_no_registry_digest():
    """`image_digest_for` is None outside docker mode — the cheap half of the same rule."""
    assert fl.image_digest_for(_Mode("standalone")) is None


class _FakeDockerProvider:
    """A fake mode AND provider — the pin's own success criterion — standing in for `conftest._DockerProvider` without shelling out
    to `docker`."""

    def __init__(self, ref):
        self._ref = ref

    def image_ref(self, mode):
        return self._ref


def _with_fake_docker_provider(monkeypatch, ref):
    from helpers import nest_mode as nest_mode_mod

    monkeypatch.setitem(nest_mode_mod._PROVIDERS, "docker", _FakeDockerProvider(ref))


def test_a_plain_docker_run_records_the_pulled_images_digest(monkeypatch):
    """Pin: a plain `--nest docker` run (`mode.argument`
    is `None`) must still record the digest of the image its PROVIDER actually
    resolved and boots — never `null` just because no `:ref` was typed.

    Red-verify: before this row's fix, `image_digest_for` read `mode.argument`
    directly and returned `None` here regardless of what `image_digest_of`
    answers — reverting `resolved_image_ref` to that shape reddens this test.
    """
    _with_fake_docker_provider(monkeypatch, "ghcr.io/faunasocial/nest:latest")
    monkeypatch.setattr(
        fl, "image_digest_of",
        lambda ref: "sha256:deadbeef" if ref == "ghcr.io/faunasocial/nest:latest" else None,
    )
    mode = _Mode("docker", argument=None)
    assert fl.image_digest_for(mode) == "sha256:deadbeef"


def test_a_locally_built_image_still_answers_none_through_the_provider(monkeypatch):
    """The companion pin: a genuinely built (never-pulled) image still answers
    `None` — `image_digest_of`'s own `RepoDigests` absence, not a new gap this
    row's fix could introduce. `resolved_image_ref` finding a ref is not the
    same as that ref having a digest."""
    _with_fake_docker_provider(monkeypatch, "fauna-nest-test:local")
    monkeypatch.setattr(fl, "image_digest_of", lambda ref: None)
    mode = _Mode("docker", argument=None)
    assert fl.image_digest_for(mode) is None


def test_an_explicit_docker_ref_argument_still_resolves(monkeypatch):
    """`--nest docker:<ref>` must still work exactly as before: the provider's
    `image_ref` returns the argument itself when one was given."""

    class _ArgumentEchoingProvider:
        def image_ref(self, mode):
            return mode.argument or "fauna-nest-test:local"

    from helpers import nest_mode as nest_mode_mod

    monkeypatch.setitem(nest_mode_mod._PROVIDERS, "docker", _ArgumentEchoingProvider())
    monkeypatch.setattr(
        fl, "image_digest_of",
        lambda ref: "sha256:cafef00d" if ref == "ghcr.io/faunasocial/nest:pinned" else None,
    )
    mode = _Mode("docker", argument="ghcr.io/faunasocial/nest:pinned")
    assert fl.image_digest_for(mode) == "sha256:cafef00d"


def test_the_shipped_image_stage_carries_the_commit_the_candidate_gate_reads():
    """`release_candidate_refusal_for_image` reads FAUNA_BUILD_COMMIT off `docker inspect`
    `.Config.Env` — which only sees an ENV set in the image's FINAL stage. Until
    2026-08-29 the Dockerfile set it in the rust-native build stage alone (compiled
    into the binary, invisible to inspect), so every image ever built — the pulled
    `:latest` included, checked on the primary dev VM — failed the gate as "carries
    no FAUNA_BUILD_COMMIT", and no candidate run could ever have started."""
    text = (fl.REPO / "Dockerfile").read_text(encoding="utf-8")
    final_stage = text[text.rindex("\nFROM "):]
    assert "ENV FAUNA_BUILD_COMMIT=${FAUNA_BUILD_COMMIT}" in final_stage, (
        "the shipped stage must repeat FAUNA_BUILD_COMMIT as image config, or the "
        "release-candidate gate refuses every image (feature-catalog.md "
        "§ Release-candidate run)")


# ── the release table the candidate run writes ──────────────────────────────
def _table(tmp_path):
    return json.loads((tmp_path / "releases.json").read_text(encoding="utf-8"))["releases"]


def test_a_candidate_run_records_its_row(tmp_path):
    fl.record_release(tmp_path, version="0.4.1", commit="a" * 40,
                      image_digest="sha256:" + "b" * 64,
                      image="ghcr.io/x/nest:sha-" + "a" * 40, today="2026-08-27")
    row, = _table(tmp_path)
    assert row["version"] == "0.4.1"
    assert row["commit"] == "a" * 40
    assert row["image_digest"] == "sha256:" + "b" * 64
    assert row["image"] == "ghcr.io/x/nest:sha-" + "a" * 40
    assert row["date"] == "2026-08-27"
    assert row["promoted"] is False   # promotion is a separate, later act


def test_re_running_the_same_artifact_updates_the_row_instead_of_duplicating(tmp_path):
    """A candidate run is re-runnable by design, and a repeat is one release seen twice.

    Without the upsert the tag gate's "exactly one image digest at HEAD's commit" would
    be satisfiable one moment and ambiguous the next, for no reason but a second run.
    """
    for day in ("2026-08-27", "2026-08-28"):
        fl.record_release(tmp_path, version="0.4.1", commit="a" * 40,
                          image_digest="sha256:" + "b" * 64, today=day)
    row, = _table(tmp_path)
    assert row["date"] == "2026-08-28"


def test_a_repeat_run_never_un_promotes_a_release(tmp_path):
    fl.record_release(tmp_path, version="0.4.1", commit="a" * 40,
                      image_digest="sha256:" + "b" * 64, today="2026-08-27")
    table = json.loads((tmp_path / "releases.json").read_text(encoding="utf-8"))
    table["releases"][0]["promoted"] = True
    (tmp_path / "releases.json").write_text(json.dumps(table), encoding="utf-8")

    fl.record_release(tmp_path, version="0.4.1", commit="a" * 40,
                      image_digest="sha256:" + "b" * 64, today="2026-08-29")
    row, = _table(tmp_path)
    assert row["promoted"] is True


def test_a_different_artifact_at_the_same_commit_is_a_second_row(tmp_path):
    """Two builds of one commit are two artifacts — the distinction the tag gate lives on."""
    for digest in ("sha256:" + "b" * 64, "sha256:" + "c" * 64):
        fl.record_release(tmp_path, version="0.4.1", commit="a" * 40,
                          image_digest=digest, today="2026-08-27")
    assert len(_table(tmp_path)) == 2


def test_a_malformed_release_table_raises_rather_than_being_overwritten(tmp_path):
    """Release history is the one record no re-run can regenerate, so it never truncates."""
    (tmp_path / "releases.json").write_text('{"schema": 1}', encoding="utf-8")
    with pytest.raises(ValueError, match="refusing to overwrite"):
        fl.record_release(tmp_path, version="0.4.1", commit="a" * 40, image_digest=None)


# ── the marker and the selector ─────────────────────────────────────────────
def _conftest():
    """The live e2e conftest as a module — the selector's own code, not a copy."""
    conftest_dir = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(conftest_dir))
    import conftest as real  # noqa: E402

    return real, conftest_dir


class _Item:
    def __init__(self, slugs, nodeid="tests/test_x.py::test_y"):
        self._slugs = slugs
        self.nodeid = nodeid

    def iter_markers(self, name=None):
        if not self._slugs or (name is not None and name != "feature"):
            return []
        return [pytest.mark.feature(*self._slugs).mark]


def test_the_marker_reader_finds_every_slug_a_test_names():
    real, _ = _conftest()
    assert real._item_features(_Item(["feed-read", "feed-compose"])) == \
        ["feed-read", "feed-compose"]
    assert real._item_features(_Item([])) == []


def test_the_repo_relative_node_id_drops_the_parametrization():
    """Contract ids and ledger keys must be the same strings, repo-relative."""
    real, conftest_dir = _conftest()

    class _Config:
        rootpath = conftest_dir

    got = real._repo_node_id(_Config(), "tests/test_feed.py::test_x[tui-standalone]")
    assert got == "tests/e2e-unified/tests/test_feed.py::test_x"


def test_an_unknown_slug_is_a_collection_error_not_an_empty_run():
    """A typo must not silently mint a feature nobody has a page for."""
    real, _ = _conftest()

    class _Config:
        rootpath = Path(__file__).resolve().parent.parent

        def getoption(self, name, default=None):
            return {"feature": [], "no_feature_ledger": False,
                    "release_candidate": False}.get(name, default)

    fl.reset_session_state()
    assert fl.known_slugs() is not None, "the catalog directory must be readable"
    items = [_Item(["definitely-not-a-feature"])]
    with pytest.raises(pytest.UsageError, match="no page in docs/features"):
        real._apply_feature_axis(_Config(), items)


def test_known_slugs_is_the_pages_and_costs_no_tree_scan():
    """`known_slugs` reads the PAGES; `contract_surfaces` also AST-walks the tree.

    They must keep agreeing — the vocabulary is the page set either way — but the
    cheap one is on the inner loop and the expensive one must not creep back onto
    it. Until 2026-09-05 `known_slugs` went through `contract_surfaces`, so every
    pytest collection anywhere under `tests/e2e-unified/` paid an `ast.parse` +
    `ast.walk` of ~757 test modules to produce a per-column witness split it then
    discarded with `set(...)`: 1.7 s of the 3.4 s `payments-excision-spine-check`
    run on Windows, and the same toll on every other pytest gate in the tree
    (`merge-gates.md` § Local-merge gates -> The 10-second budget).

    The `scan_app_marks` assertion is the load-bearing half: equality alone would
    stay green if the cheap path quietly became the expensive one again.
    """
    assert fl.known_slugs() == set(fl.contract_surfaces())

    calls = []
    fc, _pages = fl._catalog_pages()
    assert fc is not None, "the catalog must be importable"
    real_scan = fc.scan_app_marks
    fc.scan_app_marks = lambda *a, **k: (calls.append(1), real_scan(*a, **k))[1]
    try:
        fl.known_slugs()
        assert calls == [], "known_slugs must not scan the test tree"
        fl.contract_surfaces()
        assert calls == [1], "contract_surfaces still needs the per-column split"
    finally:
        fc.scan_app_marks = real_scan


def test_known_slugs_reports_an_unreadable_catalog_as_None_not_as_empty(tmp_path):
    """`None` = "cannot be read"; an empty set = "no page exists yet", which is a
    real answer that makes every marker a collection error. Conflating them would
    red a run for a reason that has nothing to do with the run — so the cheap path
    must keep BOTH answers the expensive one gave.

    ⚠ Measured, not assumed: a MISSING tree yields the empty set, not `None`,
    because `features_catalog.load_pages` globs a nonexistent directory to `[]`
    rather than raising. `contract_surfaces` answered the same way before this
    split (its dict comprehension over `[]` is `{}`, and `set({})` is `set()`), so
    this is the behaviour preserved, not a behaviour introduced — but both
    docstrings say "a missing tree" among the `None` cases and only an unreadable
    PAGE actually reaches it. Left as-is deliberately: `docs/features/` is a
    committed directory, so the case is unreachable in the tree, and narrowing the
    guard would be a behaviour change dressed as a comment fix.
    """
    assert fl.known_slugs(repo=tmp_path / "no-such-repo") == set()
    assert fl.contract_surfaces(repo=tmp_path / "no-such-repo") == {}

    unreadable = tmp_path / "unreadable-repo" / "docs" / "features"
    unreadable.mkdir(parents=True)
    # A page whose front matter cannot be parsed — the case the `None` guard is for.
    (unreadable / "broken.md").write_text("not a page at all\n", encoding="utf-8")
    repo = tmp_path / "unreadable-repo"
    assert fl.known_slugs(repo=repo) is fl.contract_surfaces(repo=repo) is None


# ── The own-artifact class: a test that boots its OWN nest image ───────────
# Found 2026-08-29 by the docker-mode ledger track and given
# per-record provenance the same day. The premise § The ledger
# rests on ("first-party — written by the run that observed the outcome, on the
# commit it names, AGAINST THE ARTIFACT IT NAMES", feature-catalog.md § The ledger)
# cannot be met with the RUN's provenance by a `tests/platform/docker/` test: it
# boots `fauna-nest-test:local`, whatever that happens to be on the box. So such a
# record carries the booted container's own commit, digest and mode instead, and
# is declined — tallied — only where no single artifact can be named.

IMAGE = "sha256:" + "1" * 64
IMAGE_COMMIT = "c" * 40
IMAGE_DIGEST = "sha256:" + "d" * 64


def _boot(fixture: str = "relay_nest", *, commit: str = IMAGE_COMMIT,
          image_id: str = IMAGE, digest: str | None = IMAGE_DIGEST, version: str = "0.1.1",
          base: str | None = None):
    """What `wait_for_health` does inside a container fixture's setup: note the
    artifact that answered, attached to the fixture being set up."""
    fl.enter_fixture_setup(fixture)
    try:
        fl.note_artifact(image_id=image_id, image_digest=digest, version=version,
                         commit=commit, base=base)
    finally:
        fl.leave_fixture_setup(fixture)


def _own_artifact_test(closure=("relay_nest", "docker_image")):
    """A test's setup as conftest performs it for an own-image item."""
    fl.reset_test_state()
    fl.note_closure(closure)
    fl.note_own_artifact("boots fauna-nest-test:local itself, via docker_image, relay_nest")


def test_an_own_artifact_test_resolves_to_the_images_provenance():
    _boot()
    _own_artifact_test()
    provenance, why = fl.own_artifact_resolution()
    assert why == "" and provenance == {
        "image_id": IMAGE, "image_digest": IMAGE_DIGEST, "version": "0.1.1",
        "commit": IMAGE_COMMIT, "base": None}


def test_a_module_scoped_fixtures_artifact_reaches_every_test_in_the_module():
    """The container fixture sets up once, during the FIRST test's setup; the
    second test's per-test slots are reset, and it must still find the artifact
    through its closure — otherwise only the first test of each module records."""
    _boot()
    _own_artifact_test()
    fl.reset_test_state()          # the next test begins
    _own_artifact_test()
    provenance, _ = fl.own_artifact_resolution()
    assert provenance["commit"] == IMAGE_COMMIT


def test_a_fixture_setting_up_again_forgets_the_artifact_it_booted_before():
    """The next module's same-named fixture boots a fresh container, possibly of a
    different image — its tests must not inherit the previous module's artifact."""
    _boot(commit="a" * 40, image_id="sha256:" + "a" * 64)
    fl.enter_fixture_setup("relay_nest")   # re-executes for the next module...
    fl.leave_fixture_setup("relay_nest")   # ...and this time never reaches health
    _own_artifact_test()
    provenance, why = fl.own_artifact_resolution()
    assert provenance is None and "no container" in why


def test_an_artifact_observed_by_the_test_body_counts_too():
    """A restart flow's `wait_for_health` runs in the body, outside any fixture setup."""
    _own_artifact_test(closure=("docker_image",))
    fl.note_artifact(image_id=IMAGE, image_digest=None, version="0.1.1", commit=IMAGE_COMMIT)
    provenance, _ = fl.own_artifact_resolution()
    assert provenance["image_id"] == IMAGE and provenance["image_digest"] is None


def test_two_distinct_artifacts_are_declined_because_a_record_names_one():
    _boot()
    _boot("second_nest", commit="e" * 40, image_id="sha256:" + "e" * 64)
    _own_artifact_test(closure=("relay_nest", "second_nest"))
    provenance, why = fl.own_artifact_resolution()
    assert provenance is None and "2 distinct artifacts" in why


def test_two_containers_of_one_image_are_one_artifact():
    """A two-box topology or a restart is one image observed twice."""
    _boot()
    _boot("second_nest")
    _own_artifact_test(closure=("relay_nest", "second_nest"))
    provenance, why = fl.own_artifact_resolution()
    assert why == "" and provenance["image_id"] == IMAGE


@pytest.mark.parametrize("commit", ["dev", "unknown", ""])
def test_a_hand_built_image_that_names_no_commit_is_declined(commit):
    """`build_identity::commit()` answers `dev` (or `unknown`, from a tree without
    `.git`) for a build nothing stamped: a record of it could not say what was built."""
    _boot(commit=commit, digest=None)
    _own_artifact_test()
    provenance, why = fl.own_artifact_resolution()
    assert provenance is None and "names no commit" in why


def test_a_short_stamped_commit_is_a_commit():
    """`just docker-push-dev` stamps the short 8; the pipeline the full 40."""
    _boot(commit="abcdef12", digest=None)
    _own_artifact_test()
    provenance, why = fl.own_artifact_resolution()
    assert why == "" and provenance["commit"] == "abcdef12"


class _FakeReport:
    """The two attributes the ledger's per-report hook reads."""

    def __init__(self, nodeid):
        self.nodeid = nodeid


RUN_COMMIT = "f" * 40
#: The run's HEAD is one unlanded commit past `origin/main` — the ordinary shape.
RUN_BASE = "b" * 40


def _run_context(monkeypatch, nodeid, *, dirty=False, base=RUN_BASE):
    """A minimal live `_feature_run`, as `_apply_feature_axis` would leave it — the
    RUN's provenance, which an own-artifact record must not carry."""
    import conftest

    monkeypatch.setitem(conftest._feature_run, "features",
                        {nodeid: ["nest-relays-peer-traffic"]})
    monkeypatch.setitem(conftest._feature_run, "node_ids", {nodeid: nodeid})
    monkeypatch.setitem(conftest._feature_run, "version", "0.1.2")
    monkeypatch.setitem(conftest._feature_run, "commit", RUN_COMMIT)
    monkeypatch.setitem(conftest._feature_run, "base", base)
    monkeypatch.setitem(conftest._feature_run, "image_digest", "sha256:" + "a" * 64)
    monkeypatch.setitem(conftest._feature_run, "nest_mode", "standalone")
    monkeypatch.setitem(conftest._feature_run, "dirty", dirty)
    monkeypatch.setitem(conftest._feature_run, "dirt", [" M tests/test_feed.py"] if dirty else [])
    monkeypatch.setitem(conftest._feature_run, "stamp",
                        "0.1.2-dev+ffffffff.dirty standalone" if dirty
                        else "0.1.2-dev+ffffffff standalone")
    return conftest


NODE = "tests/platform/docker/test_iroh_relay.py::test_relay_reachable_via_sni_router_at_443"


def test_the_hook_writes_an_own_artifact_outcome_with_the_images_provenance(monkeypatch):
    """The record names the image's commit, digest and `docker`, whatever the run's
    mode and HEAD were — the proof that the fields are not silently the run's."""
    conftest = _run_context(monkeypatch, NODE)
    _boot()
    _own_artifact_test()

    conftest._feature_ledger_note(_FakeReport(NODE), "passed")

    (record,) = fl._records.values()
    assert record["nest_mode"] == "docker"
    assert record["commit"] == IMAGE_COMMIT != conftest._feature_run["commit"]
    assert record["image_digest"] == IMAGE_DIGEST != conftest._feature_run["image_digest"]
    assert record["version"] == "0.1.1"
    assert record["stamp"] == "0.1.1-dev+cccccccc docker", "never the bare form (§ The ledger)"
    assert fl.refusals() == []


def test_the_hook_declines_an_own_artifact_outcome_it_cannot_name(monkeypatch):
    """Driven at the hook rather than through a container because the decision is
    made before any of them: the flag is read off the fixture closure at setup,
    so a self-contained module that ERRORS before health is un-recordable — an
    error is the shape that would otherwise write a ❌ accusing working product
    code, stamped with an artifact nobody can name."""
    conftest = _run_context(monkeypatch, NODE)
    _own_artifact_test()   # the fixture never reached health: nothing noted

    conftest._feature_ledger_note(_FakeReport(NODE), "error")

    assert fl._records == {}
    refusals = fl.refusals()
    assert len(refusals) == 1 and "would name no artifact" in refusals[0][1], (
        "the refusal must be TALLIED, not silent — a silent decline is the "
        "hand-maintained-table failure this whole axis exists to prevent")


def test_an_ordinary_outcome_still_carries_the_runs_provenance(monkeypatch):
    """The control: without the flag the same call records the run's identity, even
    while some docker container fixture happens to be alive in the session."""
    nodeid = "tests/test_feed.py::test_own_post_appears"
    conftest = _run_context(monkeypatch, nodeid)
    _boot()
    fl.reset_test_state()
    fl.note_closure({"relay_nest"})

    conftest._feature_ledger_note(_FakeReport(nodeid), "passed")

    (record,) = fl._records.values()
    assert record["commit"] == "f" * 40 and record["nest_mode"] == "standalone"


def test_the_refusal_is_tallied_once_per_TEST_not_once_per_REPORT(monkeypatch):
    """One test reaches the hook up to three times (setup / call / teardown).

    The tally is a count of tests excluded — conftest prints it as `(xN)` beside
    a reason — so a test that fails in `call` and again in `teardown` must not
    read as two.
    """
    conftest = _run_context(monkeypatch, NODE)
    _own_artifact_test()
    conftest._feature_ledger_note(_FakeReport(NODE), "failed")
    conftest._feature_ledger_note(_FakeReport(NODE), "error")
    assert fl._records == {} and len(fl.refusals()) == 1

    # ...and the next test starts owing its own tally again.
    _own_artifact_test()
    conftest._feature_ledger_note(_FakeReport(NODE), "failed")
    assert len(fl.refusals()) == 2


# ── the cell stamp names ONE artifact ───────────────────────────────────────
DOCKER_SURFACES = {"relay": {"app": set(), "nest": {"d.py::a", "d.py::b"}},
                   "mixed": {"app": set(), "nest": {"api.py::x", "d.py::a"}}}


def _nest_record(slug, node, *, commit, stamp, nest_mode="docker"):
    fl.record("nest", slug, node, outcome="passed", skip_class=None, version="0.1.2",
              commit=commit, base=commit, image_digest=None, nest_mode=nest_mode,
              record_stamp=stamp, dirty=False,
              today="2026-08-29")


def _cell(tmp_path, slug):
    return json.loads((tmp_path / "nest.json").read_text(encoding="utf-8"))["features"][slug]


def test_a_feature_witnessed_wholly_by_own_image_tests_takes_the_images_stamp(tmp_path):
    """Not the run's — the run never went against a standalone nest for it at all."""
    for node in ("d.py::a", "d.py::b"):
        fl.note_collected("relay", node)
        _nest_record("relay", node, commit=IMAGE_COMMIT, stamp="0.1.1-dev+cccccccc docker")
    # `today=` fixes `write()`'s own "date" stamp the same way `_nest_record` already
    # fixes each record's: assert deterministic state, never wall-clock time. Without
    # it this test went wall-clock-dependent and failed whenever a run crossed
    # midnight.
    fl.write(tmp_path, surfaces=DOCKER_SURFACES, today="2026-08-29")
    platform_cell = {"stamp": "0.1.1-dev+cccccccc docker", "date": "2026-08-29",
                      "complete": True, "dirty": False, "base": IMAGE_COMMIT}
    assert _cell(tmp_path, "relay")["cell"] == {
        **platform_cell, "platforms": {fl.host_platform(): platform_cell}}


def _head_and_parent():
    import subprocess

    out = subprocess.run(["git", "-C", str(fl.REPO), "rev-parse", "HEAD", "HEAD~1"],
                         capture_output=True, text=True, check=True).stdout.split()
    return out[0], out[1]


def test_a_mixed_complete_observation_stamps_the_cell_with_the_OLDEST_artifact(tmp_path):
    """The `tests/api/` witness ran against HEAD; the own-image witness against an
    image built at HEAD's parent. The cell shows the older — the weakest claim
    the observation supports, the same rule the app/nest halves follow."""
    head, parent = _head_and_parent()
    fl.note_collected("mixed", "api.py::x")
    fl.note_collected("mixed", "d.py::a")
    _nest_record("mixed", "api.py::x", commit=head, stamp=f"0.1.2-dev+{head[:8]} standalone",
                 nest_mode="standalone")
    _nest_record("mixed", "d.py::a", commit=parent, stamp=f"0.1.2-dev+{parent[:8]} docker")
    fl.write(tmp_path, surfaces=DOCKER_SURFACES)
    assert _cell(tmp_path, "mixed")["cell"]["stamp"] == f"0.1.2-dev+{parent[:8]} docker"


def test_artifacts_git_cannot_order_leave_the_cell_alone(tmp_path):
    """Two artifacts at one commit (the image IS HEAD, under two modes), or commits
    this checkout cannot see: the per-test lines carry their stamps, the cell waits."""
    fl.note_collected("mixed", "api.py::x")
    fl.note_collected("mixed", "d.py::a")
    _nest_record("mixed", "api.py::x", commit="a" * 40, stamp="0.1.2-dev+aaaaaaaa standalone",
                 nest_mode="standalone")
    _nest_record("mixed", "d.py::a", commit="a" * 40, stamp="0.1.2-dev+aaaaaaaa docker")
    fl.write(tmp_path, surfaces=DOCKER_SURFACES)
    entry = _cell(tmp_path, "mixed")
    assert "cell" not in entry and len(entry["tests"]) == 2


def test_the_by_image_digest_read_is_none_for_an_image_docker_does_not_know():
    assert fl.image_digest_of("fauna-nest-test:definitely-not-a-tag-anyone-made") is None


# ── the whole-set rule is read PER COLUMN ───────────────────────────────────
def _catalog_apps():
    """The render's column vocabulary, from the module that owns it."""
    import features_catalog
    return features_catalog.APPS


#: One outcome, one witness per app — the shape most of the catalog is written in.
#: The union of these can be collected by no machine: the Windows, Linux and Apple
#: apps are each drivable only on their own development machine, and a full sweep
#: selects one machine's apps.
PER_COLUMN = {"feed-read": {
    "app": {"t.py::tui_a", "t.py::win_a", "t.py::plain_b"},
    "app_by_column": {
        "tui": {"t.py::tui_a", "t.py::plain_b"},
        "windows": {"t.py::win_a", "t.py::plain_b"},
        "web": {"t.py::plain_b"},
    },
    "nest": set(),
}}


def test_a_column_is_complete_on_its_own_witnesses(tmp_path):
    """The tui run collected and observed every witness that can speak for tui. The
    windows witness is not tui's to run, and must not hold tui's cell blank forever."""
    for node in ("t.py::tui_a", "t.py::plain_b"):
        fl.note_collected("feed-read", node)
        _record("tui", "feed-read", node)
    fl.write(tmp_path, surfaces=PER_COLUMN)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["cell"]["complete"] is True


def test_another_columns_witness_is_not_this_columns_subset(tmp_path):
    """The rule still bites where it should: missing one of tui's OWN witnesses is
    the `-k` subset case, and leaves the cell alone."""
    fl.note_collected("feed-read", "t.py::tui_a")
    _record("tui", "feed-read", "t.py::tui_a")
    fl.write(tmp_path, surfaces=PER_COLUMN)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert "cell" not in entry


def test_the_real_contract_surfaces_carry_the_per_column_split():
    """The split is what `cell_complete` judges against, and the flat fallback is for
    a hand-built payload only — so the REAL producer must never take it. Pinned
    against the live catalog rather than a fixture, because a `contract_surfaces` that
    silently stopped splitting would be invisible to every fixture test here."""
    surfaces = fl.contract_surfaces()
    if not surfaces:            # the catalog is unreadable on this box; nothing to pin
        pytest.skip("no catalog in this tree")
    assert all("app_by_column" in entry for entry in surfaces.values())
    split = next(e for e in surfaces.values() if e["app"])
    assert set(split["app_by_column"]) == set(_catalog_apps())
    assert all(scope <= split["app"] for scope in split["app_by_column"].values()), \
        "a column's scope is a subset of the contract's app witnesses, never wider"


def test_a_real_per_app_page_becomes_stampable_by_one_machines_run(tmp_path):
    """End-to-end against the REAL catalog, not a fixture: the union-reading made
    `multiple-accounts` unstampable on every column, and the per-column reading makes
    it stampable by a run that drives only the app it is about.

    This is the whole defect in one assertion, on real data. The page's contract
    witnesses one outcome once per app (a Linux test, a Windows test, an Apple test, a
    terminal-UI test), and those apps are each drivable only on their own development
    machine — so the union is collectable nowhere and the old reading left every
    column of this page blank forever, in every nest mode.
    """
    surfaces = fl.contract_surfaces()
    if not surfaces or "multiple-accounts" not in surfaces:
        pytest.skip("no catalog in this tree")
    entry = surfaces["multiple-accounts"]
    tui_scope = entry["app_by_column"]["tui"]

    assert tui_scope, "the terminal UI has witnesses on this page"
    assert tui_scope < entry["app"], (
        "the union is strictly wider than any one column's scope — if this ever "
        "becomes equal, the narrowing has stopped happening and every such page is "
        "silently unstampable again")

    # A run that drove exactly the terminal UI's witnesses, and nothing else.
    for node in tui_scope:
        fl.note_collected("multiple-accounts", node)
        _record("tui", "multiple-accounts", node)
    fl.write(tmp_path, surfaces=surfaces)

    written = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))
    cell = written["features"]["multiple-accounts"].get("cell")
    assert cell is not None and cell["complete"] is True, (
        "one machine's honest run must be able to stamp this cell")


# ── dirty trees: the commit is a lower bound, and the record says so ────────
# Found 2026-09-02: a tier_4 run launched from the ordinary
# inner-loop tree (code written, run started, commit made while it built) stamped
# nine cells with the CLAIM commit — at which the tests it recorded could not even
# have been collected in that mode. Nothing on the ordinary path had asked whether
# the tree was clean; the release-candidate gate did, alone. The ruling
# (feature-catalog.md § The ledger, *Dirty trees*): never refuse the record — a run
# that observed something and did not record it is the failure the catalog exists
# to prevent — but MARK it, in the data (`dirty`) and in the stamp (`.dirty`, a
# second semver build-metadata identifier), so a reader can tell the two apart.
# The run's own output, `docs/features/ledger/`, is the one path that is not dirt.


def _git_repo(tmp_path):
    """A one-commit repository, clean, in a temp dir."""
    repo = tmp_path / "repo"
    repo.mkdir()
    git = ["git", "-c", "user.name=ledger-test", "-c",
           "user.email=ledger-test@example.invalid", "-c", "commit.gpgsign=false",
           "-C", str(repo)]
    subprocess.run(["git", "init", "-q", str(repo)], check=True, capture_output=True)
    (repo / "a.txt").write_text("a\n", encoding="utf-8")
    subprocess.run(git + ["add", "."], check=True, capture_output=True)
    subprocess.run(git + ["commit", "-qm", "init"], check=True, capture_output=True)
    return repo


def test_a_dirty_tree_stamps_a_second_build_metadata_identifier():
    assert fl.stamp("0.1.2", "8213688bbc99", "docker", release_candidate=False,
                    dirty=True) == "0.1.2-dev+8213688b.dirty docker"


def test_the_dirty_stamp_is_still_legal_semver_build_metadata():
    """`+<sha>.dirty` — dot-separated identifiers. A second `+` would not be semver,
    and the tag gate's "ignore build metadata" reading rests on the stamp being it."""
    stamped = fl.stamp("0.1.2", "8213688bbc99", "docker", release_candidate=False,
                       dirty=True)
    version, _, _mode = stamped.partition(" ")
    assert re.fullmatch(r"\d+\.\d+\.\d+-dev\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*", version), version


def test_a_bare_stamp_can_never_be_dirty():
    """The candidate gate refuses a dirty tree before any stamp is made; the grammar
    refuses too, so the half-state is unrepresentable rather than merely guarded."""
    with pytest.raises(ValueError):
        fl.stamp("0.1.2", "8213688bbc99", "docker", release_candidate=True, dirty=True)


def test_a_dirty_tree_with_no_commit_is_marked_all_the_same():
    assert fl.stamp("0.1.2", "", "standalone", release_candidate=False, dirty=True) == \
        "0.1.2-dev+dirty standalone"


def test_a_clean_tree_has_no_dirt(tmp_path):
    assert fl.tree_dirt(_git_repo(tmp_path)) == []


def test_a_modified_tracked_file_is_dirt(tmp_path):
    repo = _git_repo(tmp_path)
    (repo / "a.txt").write_text("b\n", encoding="utf-8")
    assert fl.tree_dirt(repo) == [" M a.txt"]


def test_an_untracked_file_is_dirt_too(tmp_path):
    """A new test file the run collected and nobody committed is exactly the case."""
    repo = _git_repo(tmp_path)
    (repo / "test_new.py").write_text("", encoding="utf-8")
    assert fl.tree_dirt(repo) == ["?? test_new.py"]


def test_the_ledgers_own_output_is_not_dirt(tmp_path):
    """A run WRITES `docs/features/ledger/`; the next run must not read that as a
    modification of what it observed, or every second run would be dirty. The
    exemption is that directory alone: a page holds the contract the run judges
    completeness against, so a modified page is dirt like any other file."""
    repo = _git_repo(tmp_path)
    ledger = repo / "docs" / "features" / "ledger"
    ledger.mkdir(parents=True)
    (ledger / "tui.json").write_text("{}\n", encoding="utf-8")
    assert fl.tree_dirt(repo) == []
    (repo / "docs" / "features" / "feed-read.md").write_text("", encoding="utf-8")
    assert fl.tree_dirt(repo) == ["?? docs/features/feed-read.md"]


def test_a_tree_git_cannot_read_is_unknown(tmp_path):
    assert fl.tree_dirt(tmp_path / "not-a-repo") is None


def test_unknown_is_dirty_because_clean_is_the_claim_that_needs_proof():
    assert fl.is_dirty([]) is False
    assert fl.is_dirty([" M a.txt"]) is True
    assert fl.is_dirty(None) is True


def test_a_dirty_run_marks_every_record_and_the_cell_it_stamps(tmp_path):
    """A dirty run still stamps the cell — it IS a complete first-party observation —
    and the mark travels with it, in the data and in the stamp."""
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    for node in ("t.py::a", "t.py::b"):
        _record("tui", "feed-read", node, stamp="0.1.2-dev+abcdef12.dirty standalone",
                dirty=True)
    fl.write(tmp_path, today="2026-09-02", surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["tests"]["t.py::a"][fl.host_platform()]["dirty"] is True
    platform_cell = {"stamp": "0.1.2-dev+abcdef12.dirty standalone",
                      "date": "2026-09-02", "complete": True, "dirty": True,
                      "base": COMMIT}
    assert entry["cell"] == {**platform_cell, "platforms": {fl.host_platform(): platform_cell}}


def test_a_clean_record_and_cell_say_so_in_the_data_not_only_by_omission(tmp_path):
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a")
    _record("tui", "feed-read", "t.py::b")
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["tests"]["t.py::a"][fl.host_platform()]["dirty"] is False
    assert entry["cell"]["dirty"] is False


def test_the_hook_carries_the_runs_dirtiness_onto_an_ordinary_record(monkeypatch):
    nodeid = "tests/test_feed.py::test_own_post_appears"
    conftest = _run_context(monkeypatch, nodeid, dirty=True)
    fl.reset_test_state()

    conftest._feature_ledger_note(_FakeReport(nodeid), "passed")

    (record,) = fl._records.values()
    assert record["dirty"] is True
    assert record["stamp"] == "0.1.2-dev+ffffffff.dirty standalone"


def test_an_own_artifact_record_is_never_dirty(monkeypatch):
    """The image is a built artifact: a dirty checkout on the box that ran the test
    says nothing about what the container ran, so the record says `false`."""
    conftest = _run_context(monkeypatch, NODE, dirty=True)
    _boot()
    _own_artifact_test()

    conftest._feature_ledger_note(_FakeReport(NODE), "passed")

    (record,) = fl._records.values()
    assert record["dirty"] is False
    assert record["stamp"] == "0.1.1-dev+cccccccc docker"


def test_collection_reads_the_trees_dirt_once_and_stamps_the_run_with_it(monkeypatch):
    """`_apply_feature_axis` resolves the run's context ONCE, and the dirt check rides
    beside the commit there — the wiring the measured defect was missing: `head_commit`
    was asked, the tree never was."""
    real, _ = _conftest()

    class _Config:
        rootpath = Path(__file__).resolve().parent.parent

        def getoption(self, name, default=None):
            return {"feature": [], "no_feature_ledger": False,
                    "release_candidate": False}.get(name, default)

    # A fresh run context and selection set, so the LIVE session's own are untouched.
    monkeypatch.setattr(real, "_feature_run", {})
    monkeypatch.setattr(real, "_selected_repo_ids", set())
    monkeypatch.setattr(fl, "tree_dirt", lambda repo=None: [" M tests/e2e-unified/conftest.py"])

    real._apply_feature_axis(_Config(), [_Item(["feed-read"])])

    assert real._feature_run["dirty"] is True
    assert real._feature_run["dirt"] == [" M tests/e2e-unified/conftest.py"]
    assert real._feature_run["stamp"].partition(" ")[0].endswith(".dirty"), \
        real._feature_run["stamp"]


def test_the_session_summary_says_the_tree_was_dirty(monkeypatch, capsys):
    """The harness says a word — the measured defect was caught by eye, re-reading a
    diff, because nothing at the end of the run had mentioned the tree."""
    conftest = _run_context(monkeypatch, "tests/test_feed.py::test_x", dirty=True)
    monkeypatch.setitem(conftest._feature_run, "dirt",
                        [" M tests/test_feed.py", "?? scratch.py"])
    monkeypatch.setattr(fl, "write", lambda: [Path("tui.json")])

    conftest._write_feature_ledger(None)

    out = capsys.readouterr().out
    assert "DIRTY" in out and " M tests/test_feed.py" in out and "?? scratch.py" in out, out


def test_the_candidate_gate_refuses_a_dirty_tree(tmp_path):
    repo = _git_repo(tmp_path)
    (repo / "a.txt").write_text("b\n", encoding="utf-8")
    assert fl.release_candidate_refusal_for_image("img:tag", repo=repo) == \
        "the working tree is not clean"


def test_the_candidate_gate_shares_the_one_definition_of_clean(tmp_path):
    """Ledger output alone does not refuse a candidate run either: one `tree_dirt`,
    two callers, so the gate and the mark can never disagree on what dirt is. Past
    the tree check the gate reaches `docker inspect`, whose failure is the proof."""
    repo = _git_repo(tmp_path)
    ledger = repo / "docs" / "features" / "ledger"
    ledger.mkdir(parents=True)
    (ledger / "nest.json").write_text("{}\n", encoding="utf-8")
    refusal = fl.release_candidate_refusal_for_image(
        "fauna-nest-test:no-such-image-ledger-test", repo=repo)
    assert refusal and "docker inspect" in refusal and "not clean" not in refusal, refusal


# ── rebased commits: `commit` names what ran; `base` names what will still resolve ──
# Found 2026-09-05: a record's `commit` is the run's own HEAD,
# and every landing path rebases onto `origin/main` before it fast-forward-pushes,
# so whenever main moves during a run — the NORMAL case for a run long enough to
# matter — the record lands naming a commit that resolves nowhere in main's
# history: 49 of the 212 distinct commits across the seven ledger files were
# already orphans the day it was ruled. The ruling (feature-catalog.md § The
# ledger, *Rebased commits*): `commit` keeps naming what ran and is never rewritten
# (the post-rebase sha names a tree that never ran), and every record ALSO carries
# `base` — the merge-base with `origin/main` at collection, the newest main commit
# in the run's own history — which always resolves, and means what `dirty` makes
# `commit` mean, one level up: a lower bound on the code the run saw. Unknown is
# `null`, never a guess; the render marks nothing, because reachability is a fact
# about the renderer's checkout and its output must agree across machines.


def _git(repo):
    return ["git", "-c", "user.name=ledger-test", "-c",
            "user.email=ledger-test@example.invalid", "-c", "commit.gpgsign=false",
            "-C", str(repo)]


def _rev_parse(repo, ref="HEAD"):
    return subprocess.run(_git(repo) + ["rev-parse", ref], check=True, capture_output=True,
                          text=True).stdout.strip()


def _repo_with_origin_main(tmp_path):
    """`_git_repo` whose one commit is also what the checkout knows as `origin/main`."""
    repo = _git_repo(tmp_path)
    head = _rev_parse(repo)
    subprocess.run(_git(repo) + ["update-ref", "refs/remotes/origin/main", head],
                   check=True, capture_output=True)
    return repo, head


def _commit(repo, name):
    (repo / name).write_text(f"{name}\n", encoding="utf-8")
    subprocess.run(_git(repo) + ["add", "."], check=True, capture_output=True)
    subprocess.run(_git(repo) + ["commit", "-qm", name], check=True, capture_output=True)
    return _rev_parse(repo)


def test_a_commit_already_on_main_is_its_own_base(tmp_path):
    repo, main = _repo_with_origin_main(tmp_path)
    assert fl.main_base(main, repo) == main


def test_an_unlanded_commits_base_is_the_main_commit_beneath_it(tmp_path):
    """The measured shape: the run's HEAD carries this checkout's own commits on top
    of main. The rebase will replace HEAD; the commit beneath it stays."""
    repo, main = _repo_with_origin_main(tmp_path)
    head = _commit(repo, "unlanded.txt")
    assert head != main
    assert fl.main_base(head, repo) == main


def test_a_stale_origin_main_still_yields_a_resolvable_base(tmp_path):
    """A session that never fetched knows an OLDER main than the real one. The base
    it records is older than it need be — still an ancestor of the real main (main
    only ever fast-forwards), still a lower bound — never wrong."""
    repo, old_main = _repo_with_origin_main(tmp_path)
    landed = _commit(repo, "landed-but-not-fetched.txt")   # on the real main, unknown here
    head = _commit(repo, "unlanded.txt")
    assert fl.main_base(head, repo) == old_main != landed


def test_a_checkout_that_knows_no_origin_main_has_no_base(tmp_path):
    """No `origin/main` ref at all (a bare clone of the public mirror, a repo with no
    remote): the answer is unknown, and unknown is `None`, never HEAD by default."""
    repo = _git_repo(tmp_path)
    assert fl.main_base(_rev_parse(repo), repo) is None


def test_a_tree_git_cannot_read_has_no_base(tmp_path):
    assert fl.main_base("f" * 40, tmp_path / "not-a-repo") is None


def test_no_commit_has_no_base(tmp_path):
    assert fl.main_base("", _git_repo(tmp_path)) is None


def test_every_record_and_the_cell_it_stamps_carry_the_base(tmp_path):
    """The base travels exactly as `dirty` does: on every record, and on the cell as
    the base of the records whose stamp the cell carries."""
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a", base=RUN_BASE)
    _record("tui", "feed-read", "t.py::b", base=RUN_BASE)
    fl.write(tmp_path, today="2026-09-06", surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["tests"]["t.py::a"][fl.host_platform()]["base"] == RUN_BASE
    assert entry["tests"]["t.py::a"][fl.host_platform()]["commit"] == COMMIT, \
        "`commit` still names what ran"
    platform_cell = {"stamp": "0.1.2-dev+abcdef12 standalone", "date": "2026-09-06",
                      "complete": True, "dirty": False, "base": RUN_BASE}
    assert entry["cell"] == {**platform_cell, "platforms": {fl.host_platform(): platform_cell}}


def test_an_unknown_base_is_null_in_the_data_never_a_guess(tmp_path):
    fl.note_collected("feed-read", "t.py::a")
    fl.note_collected("feed-read", "t.py::b")
    _record("tui", "feed-read", "t.py::a", base=None)
    _record("tui", "feed-read", "t.py::b", base=None)
    fl.write(tmp_path, surfaces=SURFACES)
    entry = json.loads((tmp_path / "tui.json").read_text(encoding="utf-8"))["features"]["feed-read"]
    assert entry["tests"]["t.py::a"][fl.host_platform()]["base"] is None
    assert entry["cell"]["base"] is None


def test_the_hook_carries_the_runs_base_onto_an_ordinary_record(monkeypatch):
    nodeid = "tests/test_feed.py::test_own_post_appears"
    conftest = _run_context(monkeypatch, nodeid)
    fl.reset_test_state()

    conftest._feature_ledger_note(_FakeReport(nodeid), "passed")

    (record,) = fl._records.values()
    assert record["commit"] == RUN_COMMIT
    assert record["base"] == RUN_BASE


def test_an_own_artifact_record_takes_its_images_base_not_the_runs(monkeypatch):
    """Provenance is per record: the image's commit has its own place in main's
    history, and the checkout's HEAD says nothing about it."""
    conftest = _run_context(monkeypatch, NODE)
    _boot(base="1" * 40)
    _own_artifact_test()

    conftest._feature_ledger_note(_FakeReport(NODE), "passed")

    (record,) = fl._records.values()
    assert record["commit"] == IMAGE_COMMIT
    assert record["base"] == "1" * 40 != conftest._feature_run["base"]


def test_a_real_container_asks_git_for_its_images_base(monkeypatch):
    """`note_container` is the one door a booted image's identity comes through, so
    the base is asked there — once per container at setup, never on the per-report
    path, which stays a dict write."""
    asked = []
    monkeypatch.setattr(fl, "_container_image_id", lambda name: IMAGE)
    monkeypatch.setattr(fl, "image_digest_of", lambda image_id: IMAGE_DIGEST)
    monkeypatch.setattr(fl, "main_base", lambda commit, repo=None: asked.append(commit) or "2" * 40)
    fl.enter_fixture_setup("relay_nest")
    try:
        provenance = fl.note_container("fauna-nest-ledger-test",
                                       {"version": "0.1.1", "commit": IMAGE_COMMIT})
    finally:
        fl.leave_fixture_setup("relay_nest")
    assert asked == [IMAGE_COMMIT]
    assert provenance["base"] == "2" * 40


def test_collection_reads_the_base_once_beside_the_commit(monkeypatch):
    """`_apply_feature_axis` resolves the run's context ONCE; the base rides beside
    the commit there, from the same HEAD."""
    real, _ = _conftest()

    class _Config:
        rootpath = Path(__file__).resolve().parent.parent

        def getoption(self, name, default=None):
            return {"feature": [], "no_feature_ledger": False,
                    "release_candidate": False}.get(name, default)

    asked = []
    monkeypatch.setattr(real, "_feature_run", {})
    monkeypatch.setattr(real, "_selected_repo_ids", set())
    monkeypatch.setattr(fl, "head_commit", lambda repo=None: RUN_COMMIT)
    monkeypatch.setattr(fl, "tree_dirt", lambda repo=None: [])
    monkeypatch.setattr(fl, "main_base",
                        lambda commit, repo=None: asked.append(commit) or RUN_BASE)

    real._apply_feature_axis(_Config(), [_Item(["feed-read"])])

    assert asked == [RUN_COMMIT]
    assert real._feature_run["commit"] == RUN_COMMIT
    assert real._feature_run["base"] == RUN_BASE


def test_the_session_summary_says_when_head_is_ahead_of_main(monkeypatch, capsys):
    """The harness says a word at collection's counterpart, the summary: HEAD being
    ahead of main is the precondition of the orphan, and it IS knowable at the run."""
    conftest = _run_context(monkeypatch, "tests/test_feed.py::test_x")
    monkeypatch.setattr(fl, "write", lambda: [Path("tui.json")])

    conftest._write_feature_ledger(None)

    out = capsys.readouterr().out
    assert "ahead of origin/main" in out and RUN_BASE[:12] in out, out


def test_the_session_summary_is_silent_when_head_is_on_main(monkeypatch, capsys):
    conftest = _run_context(monkeypatch, "tests/test_feed.py::test_x", base=RUN_COMMIT)
    monkeypatch.setattr(fl, "write", lambda: [Path("tui.json")])

    conftest._write_feature_ledger(None)

    out = capsys.readouterr().out
    assert "ahead of origin/main" not in out, out


def test_the_session_summary_says_when_the_base_is_unknown(monkeypatch, capsys):
    """Unknown is not silence: a checkout with no `origin/main` writes `null`, and
    the summary says so, because a null nobody mentioned reads as a bug later."""
    conftest = _run_context(monkeypatch, "tests/test_feed.py::test_x", base=None)
    monkeypatch.setattr(fl, "write", lambda: [Path("tui.json")])

    conftest._write_feature_ledger(None)

    out = capsys.readouterr().out
    assert "origin/main" in out and "unknown" in out, out


# ── the live box door: an unreachable box is environment, never a record ─────
# `feature-catalog.md` § Cell semantics: "`skip_environment` outcomes are never
# recorded. A missing credential or an unreachable port says nothing about the
# feature." A `--nest live` run whose box was down wrote 185 setup `error`s into
# tui's ledger on 2026-09-14 because the session provisioning fixtures raised
# the transport failure raw; these pin the door that declares it instead.

import socket  # noqa: E402
import ssl  # noqa: E402

import websocket  # noqa: E402

from clients._ws_rpc_core import RpcCallError, WsLinkDied  # noqa: E402
from helpers import live_box_door  # noqa: E402
from helpers import nest_mode as nest_mode_mod  # noqa: E402


@pytest.fixture
def _live_run(monkeypatch):
    monkeypatch.setattr(nest_mode_mod, "_RUN_MODE", nest_mode_mod.NestMode(nest_mode_mod.LIVE))


@pytest.mark.parametrize("exc", [
    ConnectionRefusedError(111, "Connection refused"),
    ConnectionResetError(104, "Connection reset by peer"),
    socket.gaierror(-2, "Name or service not known"),
    ssl.SSLCertVerificationError(1, "certificate has expired"),
    websocket.WebSocketBadStatusException("Handshake status %d", 502),
    websocket.WebSocketBadStatusException("Handshake status %d", 503),
    websocket.WebSocketBadStatusException("Handshake status %d", 504),
], ids=repr)
def test_failing_to_reach_the_box_is_unreachable(exc):
    assert live_box_door.is_unreachable(exc)


@pytest.mark.parametrize("exc", [
    # The box answered: a typed refusal is the product speaking.
    RpcCallError("fauna.admin.forbidden", "errors.forbidden", {}, None),
    websocket.WebSocketBadStatusException("Handshake status %d", 403),
    websocket.WebSocketBadStatusException("Handshake status %d", 500),
    # A link that died with a request in flight is a product signal by ruling
    # (`_ws_rpc_core.WsLinkDied`), and a reply timeout cannot be told from a hang.
    WsLinkDied("link died"),
    TimeoutError("no reply to fauna.admin.users.create"),
    RuntimeError("fauna.auth.verify returned no token"),
], ids=repr)
def test_an_answer_from_the_box_is_not_unreachable(exc):
    assert not live_box_door.is_unreachable(exc)


def test_a_wrapped_transport_failure_is_still_unreachable():
    try:
        try:
            raise ConnectionRefusedError(111, "Connection refused")
        except ConnectionRefusedError as inner:
            raise RuntimeError("could not provision the test user") from inner
    except RuntimeError as outer:
        assert live_box_door.is_unreachable(outer)


def test_on_live_an_unreachable_box_skips_as_environment(_live_run):
    with pytest.raises(pytest.skip.Exception, match="environment: .*registering the test user"):
        with live_box_door.reaching_the_live_box("registering the test user"):
            raise ConnectionRefusedError(111, "Connection refused")
    assert fl.skip_class_this_test() == fl.ENVIRONMENT


def test_on_live_a_product_refusal_still_raises(_live_run):
    with pytest.raises(RpcCallError):
        with live_box_door.reaching_the_live_box("registering the test user"):
            raise RpcCallError("fauna.admin.forbidden", "errors.forbidden", {}, None)
    assert fl.skip_class_this_test() is None


def test_off_live_the_door_is_transparent():
    """Standalone and docker nests are the harness's own: a refused port there is
    a harness or product failure, and must stay a recorded error."""
    assert not nest_mode_mod.run_mode().is_live
    with pytest.raises(ConnectionRefusedError):
        with live_box_door.reaching_the_live_box("registering the test user"):
            raise ConnectionRefusedError(111, "Connection refused")


def test_the_hook_records_nothing_for_a_test_the_door_skipped(monkeypatch, _live_run):
    """End to end at the writer: the door's skip reaches `_feature_ledger_note` as
    an environment skip, and the previous ledger line stands."""
    nodeid = "tests/test_feed.py::test_own_post_appears"
    conftest = _run_context(monkeypatch, nodeid)
    with pytest.raises(pytest.skip.Exception):
        with live_box_door.reaching_the_live_box("registering the test user"):
            raise ConnectionRefusedError(111, "Connection refused")

    conftest._feature_ledger_note(_FakeReport(nodeid), "skipped")

    assert fl._records == {}


def test_the_live_provisioning_doors_are_wired():
    """The two places a live run first touches the box: the provider's
    reachability probe and the shared account provisioning. Pinned by source so
    a refactor that drops the door reds here rather than in the next outage."""
    import inspect

    import conftest

    for fn in (conftest._LiveProvider._admin, conftest.test_user.__wrapped__, conftest._make_user):
        assert "reaching_the_live_box(" in inspect.getsource(fn), fn.__qualname__


# ── The live-box PREFLIGHT: a box that answered "not mine" is setup, not product ──
# 2026-10-04: a `--nest live` run against a reset example.com reached the admin
# probe, and the box ANSWERED `fauna.auth.not_registered` for the admin seed.
# The door above leaves every typed answer a product signal, so all 3 tui tests
# died in setup as recorded `error`s and the AP serving test wrote a `failed`
# cell against working code. The admin probe is the one place that answer is a
# fact about the BOX (unclaimed, or the wrong seed), so it alone declares it.


def test_a_box_that_does_not_know_the_admin_seed_is_not_ready():
    exc = RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, "user not registered")
    assert live_box_door.box_not_ready(exc)


def test_a_wrapped_not_registered_is_still_not_ready():
    try:
        try:
            raise RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, None)
        except RpcCallError as inner:
            raise RuntimeError("could not mint a token") from inner
    except RuntimeError as outer:
        assert live_box_door.box_not_ready(outer)


@pytest.mark.parametrize("exc", [
    RpcCallError("fauna.admin.forbidden", "errors.forbidden", {}, None),
    RpcCallError("fauna.auth.signature_failed", "errors.sig", {}, None),
    ConnectionRefusedError(111, "Connection refused"),
], ids=repr)
def test_other_answers_are_not_a_box_not_ready(exc):
    assert not live_box_door.box_not_ready(exc)


def test_on_live_the_admin_probe_declares_an_unregistered_seed_as_environment(_live_run):
    with pytest.raises(pytest.skip.Exception, match="environment: .*probing .*fauna.auth.not_registered"):
        with live_box_door.reaching_the_live_box("probing https://box", admin_probe=True):
            raise RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, "user not registered")
    assert fl.skip_class_this_test() == fl.ENVIRONMENT


def test_not_registered_outside_the_admin_probe_is_still_the_product_speaking(_live_run):
    """A later `not_registered` (a test user, a peer) says nothing about the box's
    claim state — only the admin probe may turn it into environment."""
    with pytest.raises(RpcCallError):
        with live_box_door.reaching_the_live_box("registering a test user"):
            raise RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, None)
    assert fl.skip_class_this_test() is None


def test_the_admin_probe_still_raises_any_other_typed_refusal(_live_run):
    with pytest.raises(RpcCallError):
        with live_box_door.reaching_the_live_box("probing https://box", admin_probe=True):
            raise RpcCallError("fauna.admin.forbidden", "errors.forbidden", {}, None)


def test_off_live_the_admin_probe_is_transparent():
    assert not nest_mode_mod.run_mode().is_live
    with pytest.raises(RpcCallError):
        with live_box_door.reaching_the_live_box("probing https://box", admin_probe=True):
            raise RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, None)


def test_the_preflight_is_wired_where_a_live_run_first_signs_in():
    import inspect

    import conftest
    from tests import test_activitypub_live

    assert "admin_probe=True" in inspect.getsource(conftest._LiveProvider._admin)
    assert "preflight_admin(" in inspect.getsource(
        test_activitypub_live.test_activitypub_live_serving_surface
    )


def test_the_admin_probe_skip_names_the_seed_source_it_was_refused_with(_live_run):
    """A `not_registered` to the admin seed is only diagnosable when the skip says
    WHICH seed was refused — example.com's ~/.fauna-id offered to dev.example.com read
    as a reset box for a whole session."""
    what = live_box_door.admin_probe_what("https://box", "~/.fauna-id")
    with pytest.raises(pytest.skip.Exception, match=r"admin seed from ~/\.fauna-id.*not_registered"):
        with live_box_door.reaching_the_live_box(what, admin_probe=True):
            raise RpcCallError("fauna.auth.not_registered", "error.auth.not_registered", {}, None)


def test_the_live_provider_probes_with_the_seed_resolved_for_its_url():
    import inspect

    import conftest

    assert "resolve_secret(url)" in inspect.getsource(conftest._LiveProvider._admin)
    assert "admin_probe_what(" in inspect.getsource(conftest._LiveProvider._admin)
