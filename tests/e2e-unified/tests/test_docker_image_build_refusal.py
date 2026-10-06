"""The tier_4 docker helpers must NEVER build the nest image on a dev machine.

`build-system.md` § Image tags & channels: building `ghcr.io/faunasocial/nest` is
the self-hosted runner's job, dispatched through `build-nest-image.yml`. The three
dev VMs are guests on one physical host, so an image build on any of them starves
every sibling session — the standing user stop of 2026-06-13, and the reason the
nest-mode axis's `_DockerProvider` refuses rather than falls back:

    "This provider never builds the image, by design… an absent one is a loud
     refusal naming both remedies rather than a 20-minute surprise build."

`tests/platform/docker/helpers.py::docker_build` did the exact opposite — it ran
`docker buildx build` of the whole image unless `FAUNA_REUSE_IMAGE=1` was already
set — so the tier_4 suite and the axis provider disagreed about the single most
expensive thing either of them can do. That is not theoretical: on 2026-08-28 a
feature-selected docker-mode sweep pulled in `tests/platform/docker/` modules
(they carry `@pytest.mark.feature`, so `--feature` selects them like any other
test) and silently started `docker buildx build -t fauna-nest-test:local` on the
primary dev VM, twice — it respawned when the first was killed — while sibling
sessions were queued for build slots on that same box.

These tests are tier_1: they must run everywhere, including machines with no
docker daemon, because what they pin is a refusal.
"""

import subprocess

import pytest

pytestmark = pytest.mark.tier_1


@pytest.fixture()
def helpers():
    return pytest.importorskip("tests.platform.docker.helpers")


def _spy(monkeypatch, module, *, image_present: bool):
    """Replace subprocess.run so a build attempt is recorded, never executed."""
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        if "inspect" in argv:
            return subprocess.CompletedProcess(
                argv, 0 if image_present else 1, b"", b"")
        raise AssertionError(
            "docker_build ran a real subprocess in a unit test: " + " ".join(argv))

    monkeypatch.setattr(module.subprocess, "run", fake_run)
    return calls


def test_it_refuses_to_build_instead_of_starting_one(helpers, monkeypatch, tmp_path):
    """No env set, no image present → a loud refusal, and NO buildx."""
    monkeypatch.delenv("FAUNA_REUSE_IMAGE", raising=False)
    monkeypatch.delenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", raising=False)
    calls = _spy(monkeypatch, helpers, image_present=False)

    with pytest.raises(RuntimeError) as exc:
        helpers.docker_build(tmp_path)

    message = str(exc.value)
    assert "docker pull" in message, "the refusal must name the pull remedy"
    assert "build-nest-image.yml" in message, "and the dispatch remedy"
    assert not any("buildx" in " ".join(c) for c in calls), (
        "a dev VM must never start a nest image build — that is the whole point")


def test_a_present_image_is_used_without_any_build(helpers, monkeypatch, tmp_path):
    """The common case on a box that already pulled or built one: just use it."""
    monkeypatch.delenv("FAUNA_REUSE_IMAGE", raising=False)
    calls = _spy(monkeypatch, helpers, image_present=True)

    assert helpers.docker_build(tmp_path) == helpers.IMAGE_TAG
    assert not any("buildx" in " ".join(c) for c in calls)


def test_the_sanctioned_builder_can_still_opt_in(helpers, monkeypatch, tmp_path):
    """The escape hatch exists for the machine whose JOB is building the image —
    the self-hosted runner — so this pins an opt-in, never a default."""
    monkeypatch.setenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", "1")
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        # image ABSENT, so the opt-in is what decides — otherwise the early
        # "already present" return would make this test pass for the wrong reason
        return subprocess.CompletedProcess(argv, 1 if "inspect" in argv else 0, "", "")

    monkeypatch.setattr(helpers.subprocess, "run", fake_run)
    assert helpers.docker_build(tmp_path) == helpers.IMAGE_TAG
    assert any("buildx" in " ".join(c) for c in calls), (
        "with the explicit opt-in the build must still be reachable")
