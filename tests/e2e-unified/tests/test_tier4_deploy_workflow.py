"""tier_1: the self-hosted tier_4 workflow opts its test step into building the
nest test image.

Pure YAML analysis of ``.github/workflows/tier4-deploy-e2e.yml``. No runner,
no docker, no network.

``docker_build`` (``tests/platform/docker/helpers.py``) builds the image keyed
on the checkout's sources only under ``FAUNA_ALLOW_NEST_IMAGE_BUILD=1`` — a
build is a heavy act no sweep starts by surprise. The workflow exists to run
the tier_4 docker suite on the self-hosted runner from a fresh checkout, so its test step
must carry that opt-in: without it every dispatch whose keyed image is absent
on the runner refuses at the first fixture, and the workflow can only ever
test an image somebody happened to leave there (the state it sat in from the
2026-08-28 build refusal until this pin). An image only for tests is built
locally, never dispatched (``docs/goal/architecture/image-pipeline.md``
§ Image tags & channels).

The retired ``reuse_image`` input and its ``FAUNA_REUSE_IMAGE`` variable stay
gone: reuse of the keyed tag is the helper's default, so either would be a
knob that does nothing.
"""

from __future__ import annotations

import os

import pytest
import yaml

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_WORKFLOW = os.path.join(_REPO, ".github", "workflows", "tier4-deploy-e2e.yml")
_OPT_IN = "FAUNA_ALLOW_NEST_IMAGE_BUILD"


def _load():
    with open(_WORKFLOW, encoding="utf-8") as handle:
        text = handle.read()
    return yaml.safe_load(text), text


def _runs_pytest(script: str) -> bool:
    """A line of the step's script whose command IS pytest (``.venv/bin/pytest
    …``, ``pytest …``) — not one that merely names the package, like the venv
    step's ``uv pip install … pytest …``."""
    for line in script.splitlines():
        words = line.split()
        if words and os.path.basename(words[0]) == "pytest":
            return True
    return False


def _pytest_steps(wf):
    """Every step whose ``run`` invokes pytest, across every job."""
    return [
        step
        for job in (wf.get("jobs") or {}).values()
        for step in job.get("steps") or []
        if _runs_pytest(str(step.get("run", "")))
    ]


def test_the_pytest_step_opts_into_the_image_build():
    wf, _ = _load()
    steps = _pytest_steps(wf)
    assert steps, f"{_WORKFLOW} has no step that runs pytest"
    for step in steps:
        value = (step.get("env") or {}).get(_OPT_IN)
        assert str(value) == "1", (
            f"step {step.get('name')!r} runs pytest without {_OPT_IN}: '1' "
            f"(found {value!r}); docker_build refuses to build the keyed nest "
            f"test image without it, so a dispatch whose image is absent "
            f"fails at the first fixture instead of building it")


def test_no_reuse_image_knob_remains():
    wf, text = _load()
    # PyYAML reads the bare key `on` as boolean True.
    triggers = wf.get("on", wf.get(True)) or {}
    inputs = ((triggers.get("workflow_dispatch") or {}).get("inputs")) or {}
    assert "reuse_image" not in inputs, (
        "the reuse_image input is retired: docker_build reuses the keyed tag "
        "whenever it is present, so the input would do nothing")
    assert "FAUNA_REUSE_IMAGE" not in text, (
        "FAUNA_REUSE_IMAGE is retired: docker_build never reads it")


def test_the_venv_installs_pytest_timeout():
    # conftest.py refuses to load without pytest-timeout (the harness-wide
    # per-test bound, testing.md § Cross-app e2e conventions, point 9): a venv
    # without it fails every dispatch with a usage error before collection.
    _, text = _load()
    assert "pytest-timeout" in text.split(), (
        "the venv step must install pytest-timeout: conftest.py refuses to load "
        "without it")


_DOCKER_DIR = os.path.join(_HERE, "platform", "docker")
#: Markers that scope a test to a client app; conftest builds that app's
#: artifacts at collection for a run whose app set holds it.
_CLIENT_MARKERS = ("web", "linux", "tui", "windows", "macos", "ios", "android")


def test_self_contained_docker_files_carry_no_client_marker():
    # `self_contained_docker` means the image is the ONLY out-of-process
    # dependency (pytest.ini) — the workflow's runner has no just/cargo/deno.
    # A client marker on such a file made the runner's run (app set: web, the
    # `github` platform's sweep) build the web SPA at collection and die there
    # before any test ran.
    offenders = []
    for name in sorted(os.listdir(_DOCKER_DIR)):
        if not name.endswith(".py"):
            continue
        with open(os.path.join(_DOCKER_DIR, name), encoding="utf-8") as handle:
            text = handle.read()
        if "pytest.mark.self_contained_docker" not in text:
            continue
        offenders += [f"{name}: pytest.mark.{m}" for m in _CLIENT_MARKERS
                      if f"pytest.mark.{m}," in text or f"pytest.mark.{m}]" in text
                      or f"pytest.mark.{m}\n" in text]
    assert not offenders, (
        "a self_contained_docker test drives no client, so it carries no client "
        f"marker: {offenders}")
