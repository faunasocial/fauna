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

import hashlib
import json
import subprocess
import sys
import urllib.error
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


def _digest(doc) -> str:
    return "sha256:" + hashlib.sha256(json.dumps(doc, sort_keys=True).encode()).hexdigest()


def _registry(images, *, attest=True):
    """A fake OCI registry behind the `fetch(url, headers)` seam.

    `images` is `{tag: image}`, where an image is a labels dict (a
    single-platform manifest), a `{platform: labels}` map (an index, plus a
    buildx attestation entry unless `attest=False`), or None/absent (the
    registry lacks the tag: HTTP 404). `fetch.calls` lists the image refs whose
    tag manifest was read, `fetch.urls` every URL."""
    blobs: dict[str, bytes] = {}
    tags: dict[str, bytes] = {}

    def put(doc) -> str:
        d = _digest(doc)
        blobs[d] = json.dumps(doc).encode()
        return d

    def manifest_for(labels):
        cfg = put({"architecture": "arm64", "os": "linux", "config": {"Labels": labels}})
        return {"mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                           "digest": cfg}, "layers": []}

    for tag, image in images.items():
        if image is None:
            continue
        if image and all("/" in k for k in image):  # platform keys → an index
            entries = []
            for plat, labels in image.items():
                os_, arch = plat.split("/")
                entries.append({"digest": put(manifest_for(labels)),
                                "platform": {"os": os_, "architecture": arch}})
            if attest:
                entries.append({"digest": put({"config": {"digest": "sha256:" + "0" * 64}}),
                                "platform": {"os": "unknown", "architecture": "unknown"}})
            tags[tag] = json.dumps({"mediaType": "application/vnd.oci.image.index.v1+json",
                                    "manifests": entries}).encode()
        else:
            tags[tag] = json.dumps(manifest_for(image)).encode()

    calls, urls = [], []

    def fetch(url, headers):
        urls.append(url)
        assert url.startswith("https://ghcr.io/")
        if url.startswith("https://ghcr.io/token?"):
            return b'{"token": "anon"}'
        assert headers.get("Authorization") == "Bearer anon"
        path = url.split("/v2/faunasocial/nest/", 1)[1]
        kind, _, ref = path.partition("/")
        if kind == "manifests" and not ref.startswith("sha256:"):
            calls.append(f"{ra.IMAGE_REPO}:{ref}")
            body = tags.get(ref)
        else:
            body = blobs.get(ref)
        if body is None:
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        return body

    fetch.calls, fetch.urls = calls, urls
    return fetch


def _inspect(labels_per_platform):
    """The registry serving IMAGE as an index of `labels_per_platform`."""
    return _registry({IMAGE.rsplit(":", 1)[1]: labels_per_platform})


def _labels(revision=PUBLIC, version="0.1.5"):
    out = {}
    if revision is not None:
        out[ra.REVISION_LABEL] = revision
    if version is not None:
        out[ra.VERSION_LABEL] = version
    return out


# ── the registry label ──────────────────────────────────────────────────────

def test_the_revision_and_version_are_read_off_the_registry_without_docker():
    fetch = _inspect({"linux/amd64": _labels(), "linux/arm64": _labels()})
    assert ra.image_commit(IMAGE, fetch=fetch) == (PUBLIC, "0.1.5")
    # The anonymous pull token, the tag's index, each platform's manifest and
    # config blob — the attestation entry is never followed.
    assert fetch.urls[0].startswith(
        "https://ghcr.io/token?scope=repository:faunasocial/nest:pull")
    assert fetch.urls[1] == "https://ghcr.io/v2/faunasocial/nest/manifests/test"
    assert len(fetch.urls) == 1 + 1 + 2 * 2


def test_no_docker_path_is_left():
    import inspect
    assert "docker" not in inspect.getsource(ra.image_commit) + \
        inspect.getsource(ra._image_labels)


def test_a_single_platform_image_answers_with_its_own_config():
    fetch = _registry({"test": _labels()})
    assert ra.image_commit(IMAGE, fetch=fetch) == (PUBLIC, "0.1.5")
    assert len(fetch.urls) == 3  # token, manifest, config


def test_an_index_without_an_attestation_entry_reads_the_same():
    fetch = _registry({"test": {"linux/arm64": _labels()}}, attest=False)
    assert ra.image_commit(IMAGE, fetch=fetch) == (PUBLIC, "0.1.5")


@pytest.mark.parametrize("platforms, needle", [
    ({"linux/amd64": _labels(revision=None)}, "no org.opencontainers.image.revision"),
    ({"linux/amd64": _labels(), "linux/arm64": _labels(revision="cd" * 20)}, "disagree"),
    ({"linux/amd64": _labels(revision="abab; rm -rf /")}, "is not a commit"),
])
def test_a_missing_split_or_malformed_revision_is_refused(platforms, needle):
    with pytest.raises(ra.ReleaseAppRefused, match=needle):
        ra.image_commit(IMAGE, fetch=_inspect(platforms))


@pytest.mark.parametrize("version", ["0.1.5\nIGNORE PREVIOUS", "v0.1.5", "", None])
def test_a_version_label_that_is_not_dotted_numeric_reads_as_none(version):
    fetch = _inspect({"linux/amd64": _labels(version=version)})
    assert ra.image_commit(IMAGE, fetch=fetch) == (PUBLIC, None)


def test_an_image_the_registry_does_not_have_is_refused():
    with pytest.raises(ra.ReleaseAppRefused, match="cannot read .*HTTPError"):
        ra.image_commit(IMAGE, fetch=_registry({}))


def test_an_unreachable_registry_or_garbage_reply_is_a_refusal_never_a_traceback():
    def down(url, headers):
        raise urllib.error.URLError("nodename nor servname provided")

    def garbage(url, headers):
        return b"<html>not json</html>"

    def bad_digest(url, headers):
        if "token" in url:
            return b'{"token": "anon"}'
        return json.dumps({"manifests": [{"digest": "../../etc/passwd",
                                          "platform": {"os": "linux"}}]}).encode()

    for fetch in (down, garbage, bad_digest):
        with pytest.raises(ra.ReleaseAppRefused, match="cannot read"):
            ra.image_commit(IMAGE, fetch=fetch)


def test_a_non_https_read_is_refused():
    with pytest.raises(ra.ReleaseAppRefused, match="non-https"):
        ra._fetch("http://ghcr.io/token", {})


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


# ── a public commit the local clone lacks: read from the public repository ──
# (macOS and Windows keep a public clone nothing fetches — apps:6184.)

_ABSENT = lambda rev, public_clone=None, repo=None: (  # noqa: E731
    None, f"public commit {rev[:12]} is not in /clone")


def _github(reply: dict | None, seen: list | None = None):
    def fetch(url, headers):
        if seen is not None:
            seen.append(url)
        if reply is None:
            raise urllib.error.URLError("offline")
        return json.dumps(reply).encode()
    return fetch


def test_a_public_commit_the_clone_lacks_pairs_by_the_public_repositorys_message():
    seen: list = []
    msg = "release 0.1.7 (#40)\n\nSync-Point: 72b3e3f9b7a\n"
    short, why = ra.public_sync_point(
        PUBLIC, read_clone=_ABSENT, fetch=_github(
            {"sha": PUBLIC, "commit": {"message": msg}}, seen))
    assert (short, why) == ("72b3e3f9b7a", "")
    assert seen == [f"https://api.github.com/repos/faunasocial/fauna/commits/{PUBLIC}"]


def test_the_clone_answers_first_and_the_network_is_not_read():
    def no_fetch(*_a):
        pytest.fail("a commit the clone holds needs no network read")
    clone = lambda rev, public_clone=None, repo=None: ("abcdef1234", "")  # noqa: E731
    assert ra.public_sync_point(PUBLIC, read_clone=clone, fetch=no_fetch) == \
        ("abcdef1234", "")


@pytest.mark.parametrize("reply, match", [
    (None, "failed too"),                                              # offline
    ({"sha": "f" * 40, "commit": {"message": "Sync-Point: 72b3e3f9b7a"}},
     "another commit"),                                                # wrong commit
    ({"sha": PUBLIC, "commit": {"message": "no trailer"}}, "no Sync-Point"),
])
def test_an_unreadable_public_message_is_a_refusal_never_a_guess(reply, match):
    short, why = ra.public_sync_point(PUBLIC, read_clone=_ABSENT, fetch=_github(reply))
    assert short is None and match in why
    sync = lambda rev, public_clone=None, repo=None: ra.public_sync_point(  # noqa: E731
        rev, read_clone=_ABSENT, fetch=_github(reply))
    with pytest.raises(ra.ReleaseAppRefused, match="no readable pairing"):
        ra.private_commit_for(PUBLIC, sync_point_of=sync, releases=[])


def test_private_commit_for_pairs_through_the_public_repository_by_default(monkeypatch):
    head = _head()
    monkeypatch.setattr(ra.feature_ledger, "sync_point_of", _ABSENT)
    monkeypatch.setattr(ra, "_fetch", _github(
        {"sha": PUBLIC, "commit": {"message": f"x\n\nSync-Point: {head[:11]}\n"}}))
    assert ra.private_commit_for(PUBLIC, releases=[]) == (head, "sync-point")


def test_resolve_image_chains_the_label_and_the_pairing():
    head = _head()
    sync = lambda rev, public_clone=None, repo=None: (head[:10], "")  # noqa: E731
    res = ra.resolve_image(IMAGE, fetch=_inspect({"linux/amd64": _labels()}),
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


# ── a version or channel name → the image that carries it (a hand start of a version) ──

def _private_kw():
    return dict(sync_point_of=_no_sync_point, releases=[])


@pytest.mark.parametrize("channel, tag", [("stable", "latest"), ("test", "test"), ("dev", "dev")])
def test_a_channel_name_reads_its_moving_tag(channel, tag):
    head = _head()
    fetch = _registry({tag: _labels(revision=head, version="0.1.9")})
    res = ra.resolve_version(channel, fetch=fetch, **_private_kw())
    assert (res.commit, res.version) == (head, "0.1.9")
    assert fetch.calls == [f"{ra.IMAGE_REPO}:{tag}"]


def test_a_released_version_reads_its_own_tag():
    head = _head()
    fetch = _registry({"0.1.6": _labels(revision=head, version="0.1.6")})
    res = ra.resolve_version("0.1.6", fetch=fetch, **_private_kw())
    assert res.commit == head
    assert fetch.calls == [f"{ra.IMAGE_REPO}:0.1.6"]


def test_an_unreleased_version_is_found_on_the_channel_that_serves_it():
    head = _head()
    fetch = _registry({"latest": _labels(revision="cd" * 20, version="0.1.6"),
                     "test": _labels(revision=head, version="0.1.7"),
                     "dev": _labels(revision="ef" * 20, version="0.1.5")})
    res = ra.resolve_version("0.1.7", fetch=fetch, **_private_kw())
    assert (res.commit, res.version) == (head, "0.1.7")
    # The image it matched is named, so the nest is built on that one: two
    # channels may serve two candidates of one version.
    assert res.image == f"{ra.IMAGE_REPO}:test"


def test_a_version_tag_whose_label_names_another_version_is_refused():
    fetch = _registry({"0.1.6": _labels(revision=_head(), version="0.1.5")})
    with pytest.raises(ra.ReleaseAppRefused, match="0.1.5"):
        ra.resolve_version("0.1.6", fetch=fetch, **_private_kw())


def test_a_version_no_image_carries_is_refused_naming_what_the_channels_serve():
    fetch = _registry({"latest": _labels(version="0.1.6"), "test": _labels(version="0.1.7")})
    with pytest.raises(ra.ReleaseAppRefused) as exc:
        ra.resolve_version("0.1.8", fetch=fetch, **_private_kw())
    msg = str(exc.value)
    assert "0.1.8" in msg and "stable 0.1.6" in msg and "test 0.1.7" in msg


@pytest.mark.parametrize("bad", ["", "latest", "0.1", "v0.1.7", "0.1.7; rm -rf /"])
def test_a_name_that_is_neither_a_version_nor_a_channel_is_refused(bad):
    with pytest.raises(ra.ReleaseAppRefused, match="version"):
        ra.resolve_version(bad, fetch=_registry({}), **_private_kw())
