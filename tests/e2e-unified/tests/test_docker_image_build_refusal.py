"""The tier_4 docker helpers build the nest test image only on an explicit opt-in.

`image-pipeline.md` § Image tags & channels (owner ruling 2026-10-19): an image only
for tests is built locally, inside a build slot, never dispatched — a dispatch adds
`:sha-<sha>` to the public package, and the self-hosted CI runner shares the dev VMs' host, so a local
build costs the host what a dispatch does. But a build is still a heavy act no sweep
may start by surprise. That is not theoretical: on 2026-08-28 a feature-selected
docker-mode sweep pulled in `tests/platform/docker/` modules (they carry
`@pytest.mark.feature`, so `--feature` selects them like any other test) and
silently started `docker buildx build` on the primary dev VM, twice — it respawned
when the first was killed — while sibling sessions were queued for build slots on
that same box. So an absent image is a loud refusal naming both remedies, and
`FAUNA_ALLOW_NEST_IMAGE_BUILD=1` is the opt-in. The keyed tag, the lock and GC are
pinned in `test_nest_test_image.py`.

These tests are tier_1: they must run everywhere, including machines with no
docker daemon, because what they pin is a refusal.
"""

import subprocess

import pytest

from helpers import nest_test_image

pytestmark = pytest.mark.tier_1

TAG = "fauna-nest-test:src-0123456789abcdef"


@pytest.fixture()
def helpers(monkeypatch, tmp_path):
    dh = pytest.importorskip("tests.platform.docker.helpers")
    monkeypatch.setattr(nest_test_image, "local_image_tag", lambda root: TAG)
    monkeypatch.setattr(nest_test_image, "collect_old_tags", lambda keep, **kw: [])
    monkeypatch.setattr(nest_test_image, "LOCK_DIR", tmp_path / "locks")
    monkeypatch.delenv(nest_test_image.OVERRIDE_ENV, raising=False)
    monkeypatch.delenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", raising=False)
    return dh


def _spy(monkeypatch, module, *, image_present: bool):
    """Replace subprocess.run so a build attempt is recorded, never executed."""
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        if "inspect" in argv:
            return subprocess.CompletedProcess(
                argv, 0 if image_present else 1, "", "")
        raise AssertionError(
            "docker_build ran a real subprocess in a unit test: " + " ".join(argv))

    monkeypatch.setattr(module.subprocess, "run", fake_run)
    return calls


def test_it_refuses_to_build_instead_of_starting_one(helpers, monkeypatch, tmp_path):
    """No opt-in, no image present → a loud refusal, and NO buildx."""
    calls = _spy(monkeypatch, helpers, image_present=False)

    with pytest.raises(RuntimeError) as exc:
        helpers.docker_build(tmp_path)

    message = str(exc.value)
    assert TAG in message, "the refusal must name the keyed tag it looked for"
    assert "FAUNA_ALLOW_NEST_IMAGE_BUILD=1" in message, "the local-build remedy"
    assert "docker pull" in message and nest_test_image.OVERRIDE_ENV in message, (
        "and the pulled-image remedy, named rather than retagged")
    assert "never a build-nest-image.yml dispatch" in message
    assert not any("buildx" in " ".join(c) for c in calls), (
        "no sweep may start a nest image build without the opt-in")


def test_a_present_image_is_used_without_any_build(helpers, monkeypatch, tmp_path):
    """The common case on a box where this tree's image is already built."""
    calls = _spy(monkeypatch, helpers, image_present=True)

    assert helpers.docker_build(tmp_path) == TAG
    assert not any("buildx" in " ".join(c) for c in calls)


def test_the_opt_in_builds_the_keyed_tag(helpers, monkeypatch, tmp_path):
    monkeypatch.setenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", "1")
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        # image ABSENT, so the opt-in is what decides — otherwise the early
        # "already present" return would make this test pass for the wrong reason
        return subprocess.CompletedProcess(argv, 1 if "inspect" in argv else 0, "", "")

    monkeypatch.setattr(helpers.subprocess, "run", fake_run)
    assert helpers.docker_build(tmp_path) == TAG
    build = next(c for c in calls if "buildx" in c)
    assert build[build.index("-t") + 1] == TAG


def test_an_unkeyable_tree_is_refused_not_built(helpers, monkeypatch, tmp_path):
    monkeypatch.setenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", "1")
    monkeypatch.setattr(nest_test_image, "local_image_tag",
                        lambda root: nest_test_image.UNKEYED_TAG)
    calls = _spy(monkeypatch, helpers, image_present=False)
    with pytest.raises(RuntimeError, match="cannot key"):
        helpers.docker_build(tmp_path)
    assert calls == []
