"""tier_1: the release's own app — which private commit an organisation drive
against a kept box builds its client from (`helpers/release_app.py`).

Owner: `docs/goal/architecture/org-nests.md` § fauna.build → *Disposable*, the
2026-11-01 ruling, rule (1). Every re-creation of fauna.build from 2026-10-29
died after buying its server because the drive ran the TIP's app against the
0.1.5 `:test` nest; the rule makes it run the release's own. These pin the
resolution over fixture strings — the registry label read, the pairing of a
public build to its private source (private, public-paired, unpaired, mirror
missing, a release table that disagrees), the box's health commit taken as
untrusted data, the drive's pre-buy check and the conftest marker's selection
(the reports pass, a maintainer tool, is pinned beside it in a private suite). The drives themselves are
tier_4 live.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import conftest  # noqa: E402
from helpers import release_app as ra  # noqa: E402

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]
PUBLIC = "ab" * 20   # a public commit (synthetic)
IMAGE = "ghcr.io/faunasocial/nest:test"


def _head() -> str:
    return subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True).stdout.strip()


def _inspect(labels_per_platform, rc=0, stderr=""):
    """A fake `docker buildx imagetools inspect --format '{{json .Image}}'`."""
    body = {plat: {"config": {"Labels": labels}} for plat, labels in labels_per_platform.items()}
    calls = []

    def run(argv):
        calls.append(argv)
        return SimpleNamespace(returncode=rc, stdout=json.dumps(body), stderr=stderr)

    run.calls = calls
    return run


def _labels(revision=PUBLIC, version="0.1.5"):
    out = {}
    if revision is not None:
        out[ra.REVISION_LABEL] = revision
    if version is not None:
        out[ra.VERSION_LABEL] = version
    return out


# ── the registry label ──────────────────────────────────────────────────────

def test_the_revision_and_version_are_read_off_the_registry():
    run = _inspect({"linux/amd64": _labels(), "linux/arm64": _labels()})
    assert ra.image_commit(IMAGE, run=run) == (PUBLIC, "0.1.5")
    assert run.calls[0][:5] == ["docker", "buildx", "imagetools", "inspect", IMAGE]


def test_a_single_platform_image_answers_with_its_own_config():
    def run(argv):
        return SimpleNamespace(returncode=0, stderr="",
                               stdout=json.dumps({"config": {"Labels": _labels()}}))
    assert ra.image_commit(IMAGE, run=run) == (PUBLIC, "0.1.5")


@pytest.mark.parametrize("platforms, needle", [
    ({"linux/amd64": _labels(revision=None)}, "no org.opencontainers.image.revision"),
    ({"linux/amd64": _labels(), "linux/arm64": _labels(revision="cd" * 20)}, "disagree"),
])
def test_a_missing_or_split_revision_is_refused(platforms, needle):
    with pytest.raises(ra.ReleaseAppRefused, match=needle):
        ra.image_commit(IMAGE, run=_inspect(platforms))


def test_an_image_the_registry_does_not_have_is_refused():
    run = _inspect({}, rc=1, stderr="ERROR: not found")
    with pytest.raises(ra.ReleaseAppRefused, match="cannot read"):
        ra.image_commit(IMAGE, run=run)


# ── the pairing ─────────────────────────────────────────────────────────────

def _no_sync_point(*_a, **_k):
    pytest.fail("a private revision needs no Sync-Point read")


def test_a_private_revision_is_taken_as_is():
    head = _head()
    assert ra.private_commit_for(head[:12], sync_point_of=_no_sync_point, releases=[]) == \
        (head, "private")


def test_a_public_revision_pairs_through_its_sync_point_and_the_release_table():
    head = _head()
    sync = lambda rev, public_clone=None, repo=None: (head[:10], "")  # noqa: E731
    rows = [{"version": "0.1.5", "commit": head, "public_commit": PUBLIC}]
    assert ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=rows) == \
        (head, "sync-point")
    # No release row (a :dev public build, say) — the trailer alone pairs it.
    assert ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=[]) == \
        (head, "sync-point")


def test_a_release_table_that_pairs_the_public_commit_elsewhere_is_refused():
    head = _head()
    sync = lambda rev, public_clone=None, repo=None: (head[:10], "")  # noqa: E731
    rows = [{"version": "0.1.5", "commit": "c" * 40, "public_commit": PUBLIC}]
    with pytest.raises(ra.ReleaseAppRefused, match="refusing to choose"):
        ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=rows)


@pytest.mark.parametrize("why", [
    "no public clone at /nowhere to read the Sync-Point trailer from",   # mirror missing
    "public commit abababababab carries no Sync-Point trailer",          # unpaired
])
def test_an_unpairable_public_revision_is_refused_with_the_reason(why):
    sync = lambda rev, public_clone=None, repo=None: (None, why)  # noqa: E731
    with pytest.raises(ra.ReleaseAppRefused, match="no readable pairing") as exc:
        ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=[])
    assert why in str(exc.value)


def test_a_sync_point_this_repository_lacks_is_refused():
    sync = lambda rev, public_clone=None, repo=None: ("0" * 10, "")  # noqa: E731
    with pytest.raises(ra.ReleaseAppRefused, match="not a commit in this repository"):
        ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=[])


def test_resolve_image_chains_the_label_and_the_pairing():
    head = _head()
    sync = lambda rev, public_clone=None, repo=None: (head[:10], "")  # noqa: E731
    res = ra.resolve_image(IMAGE, run=_inspect({"linux/amd64": _labels()}),
                           sync_point_of=sync, releases=[])
    assert (res.commit, res.revision, res.version, res.via) == \
        (head, PUBLIC, "0.1.5", "sync-point")


# ── the box's health commit: untrusted data ────────────────────────────────

@pytest.mark.parametrize("body", [
    '{"status":"ok","commit":"dev"}',
    '{"status":"ok"}',
    '{"commit":"abab; rm -rf /"}',
    "<html>not json</html>",
])
def test_a_health_reply_without_a_hex_commit_is_refused(body):
    with pytest.raises(ra.ReleaseAppRefused):
        ra.health_commit("test.fauna.build", fetch=lambda url: body)


def test_the_health_commit_is_read_from_https():
    seen = []
    got = ra.health_commit("test.fauna.build",
                           fetch=lambda url: seen.append(url) or f'{{"commit":"{PUBLIC.upper()}"}}')
    assert got == PUBLIC and seen == ["https://test.fauna.build/api/v1/health"]


@pytest.mark.parametrize("served, revision, ok", [
    (PUBLIC, PUBLIC, True), (PUBLIC[:12], PUBLIC, True), (PUBLIC, PUBLIC[:8], True),
    (PUBLIC, "cd" * 20, False), ("", PUBLIC, False),
])
def test_commit_matches_compares_prefixes(served, revision, ok):
    assert ra.commit_matches(served, revision) is ok


# ── the drive's pre-buy check ───────────────────────────────────────────────

@pytest.fixture
def launched(monkeypatch):
    monkeypatch.setattr(ra, "_LAUNCHED", {})
    return ra._LAUNCHED


def test_prebuy_passes_when_the_launched_app_is_the_images_commit(launched, tmp_path):
    exe = tmp_path / "fauna-tui"
    exe.write_bytes(b"...stamp:bbbbbbbb...")
    ra.note_launched("tui", "b" * 40, PUBLIC, str(exe))
    expected = ra.Resolution(commit="b" * 40, revision=PUBLIC, version="0.1.5", via="sync-point")
    assert ra.prebuy_refusal(expected) is None
    assert ra.binary_stamp(exe, "b" * 40)


def test_prebuy_refuses_a_tag_that_moved_since_the_app_launched(launched):
    ra.note_launched("tui", "b" * 40, PUBLIC, "/nowhere")
    moved = ra.Resolution(commit="c" * 40, revision="d" * 40, version="0.1.6", via="sync-point")
    assert "image now names" in ra.prebuy_refusal(moved)


def test_prebuy_refuses_when_no_release_build_was_launched(launched):
    expected = ra.Resolution(commit="b" * 40, revision=PUBLIC, version=None, via="private")
    assert "marker did not reach" in ra.prebuy_refusal(expected)


# ── the conftest marker's selection ─────────────────────────────────────────

def _item(**kwargs):
    marker = SimpleNamespace(kwargs=kwargs) if kwargs is not None else None
    return SimpleNamespace(get_closest_marker=lambda name: marker if name == "release_app" else None)


def _plain():
    return SimpleNamespace(get_closest_marker=lambda name: None)


def test_no_marked_item_names_no_target():
    assert conftest._release_app_target([_plain()]) is None


def test_a_marker_without_a_target_names_nothing():
    assert conftest._release_app_target([_item(image=None)]) is None


def test_marked_items_naming_one_target_select_it():
    items = [_item(box="test.fauna.build"), _item(box="test.fauna.build"), _plain()]
    assert conftest._release_app_target(items) == (None, "test.fauna.build")


def test_two_targets_in_one_run_are_refused():
    with pytest.raises(RuntimeError, match="its own invocation"):
        conftest._release_app_target([_item(image=IMAGE), _item(box="dev.fauna.build")])


def test_the_override_refuses_an_app_it_cannot_build(monkeypatch):
    request = SimpleNamespace(session=SimpleNamespace(items=[_item(image=IMAGE)]))
    with pytest.raises(RuntimeError, match="cargo clients only"):
        conftest._release_app_override(request, "windows")


def test_the_override_is_silent_without_the_marker():
    request = SimpleNamespace(session=SimpleNamespace(items=[_plain()]))
    assert conftest._release_app_override(request, "tui") is None


# ── the launched app's own strings ──────────────────────────────────────────
# The harness's `S` catalog is the tip's; a release build renders its own
# commit's wording (0.1.5's device-cap notice said "plan", the tip's says
# "tier"), so a classifier keyed on an app-rendered string reads it there.

_EN_AT_RELEASE = """\
error:
  sync:
    device_limit_exceeded: This account already has as many devices as its plan allows.
"""


def test_an_app_string_is_read_at_the_launched_commit(launched):
    ra.note_launched("tui", "b" * 40, PUBLIC, "/nowhere")
    asked = []

    def show(commit, path):
        asked.append((commit, path))
        return _EN_AT_RELEASE

    got = ra.app_string("error.sync.device_limit_exceeded", show=show)
    assert got == "This account already has as many devices as its plan allows."
    assert asked == [("b" * 40, "i18n/strings/en.yaml")]


def test_no_launched_release_build_reads_no_app_string(launched):
    assert ra.app_string("error.sync.device_limit_exceeded",
                         show=lambda *_: pytest.fail("read with nothing launched")) is None


def test_a_key_the_release_lacks_is_none(launched):
    ra.note_launched("tui", "b" * 40, PUBLIC, "/nowhere")
    assert ra.app_string("error.sync.nope", show=lambda *_: _EN_AT_RELEASE) is None


def test_the_device_cap_reads_the_launched_apps_wording(launched, monkeypatch):
    from helpers import org_account_session as oas
    monkeypatch.setattr(ra, "app_string", lambda key, **_: "plan wording"
                        if key == "error.sync.device_limit_exceeded" else None)
    assert oas.device_limit_text() == "plan wording"
    monkeypatch.setattr(ra, "app_string", lambda key, **_: None)
    assert oas.device_limit_text() == oas.S.error.sync.device_limit_exceeded
