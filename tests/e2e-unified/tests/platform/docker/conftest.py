"""Pytest fixtures and CLI options for docker e2e tests."""

import os
import secrets
import subprocess
import sys

import pytest


def pytest_collection_modifyitems(items):
    """Raise the per-test timeout for tier_4 docker tests lacking an explicit mark.

    The harness-wide default (pytest.ini ``timeout = 900``) assumes locally-built
    binaries; a docker module's fixture also boots containers and, when the
    session prebuild below could not build, builds the image itself. Bounded is
    still the law — a wedged run dies loudly, never silently."""
    for item in items:
        if item.get_closest_marker("timeout") is None:
            item.add_marker(pytest.mark.timeout(3600))


def _modules_needing_the_image(session) -> list:
    """The selected modules whose fixtures boot the nest image (the
    ``self_contained_docker`` marker), each once, in selection order."""
    seen: dict = {}
    for item in session.items:
        if item.get_closest_marker("self_contained_docker") is not None:
            seen.setdefault(item.module.__name__, item.module)
    return list(seen.values())


@pytest.hookimpl(tryfirst=True)
def pytest_collection_finish(session):
    """Build the nest image, and any image a selected module layers on it, HERE —
    outside every per-test timeout and before the run takes its e2e lane.

    A fixture's build is charged to the first test's 3600 s setup, while its own
    bound is ``SLOT_WAIT_TIMEOUT + BUILD_TIMEOUT``: on a loaded box the test
    timeout killed a build that was only queueing, the bound inversion
    ``conftest.py::_prebuild_web_spa`` documents for the SPA. A warm image costs
    a tag lookup here. The fixtures still call ``docker_build``, which finds the
    tag; a refusal or a failed build is not this hook's to report, so it says one
    line and leaves the module's own fixture to fail with the full diagnosis,
    test by test, while the rest of the selection runs. The tester's and the
    preflight's modes build nothing here."""
    if session.config.option.collectonly or any(os.environ.get(env) for env in (
        "FAUNA_E2E_WARM_ONLY", "FAUNA_E2E_PREWARM_ONLY", "FAUNA_E2E_PLAN_ONLY",
    )):
        return
    modules = _modules_needing_the_image(session)
    if not modules:
        return
    from .helpers import docker_build, get_repo_root

    try:
        base = docker_build(get_repo_root())
        for module in modules:
            prebuild = getattr(module, "prebuild_images", None)
            if prebuild is not None:
                prebuild(base)
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        print(f"[docker-prebuild] not built up front, the fixtures report it: "
              f"{str(e).splitlines()[0][:200]}", file=sys.stderr, flush=True)


def pytest_addoption(parser):
    group = parser.getgroup("docker-mail")
    group.addoption(
        "--imap-handle",
        action="store",
        default=None,
        help="Local-part for the IMAP test user "
             "(default: $FAUNA_TEST_IMAP_HANDLE or 'mailtest').",
    )
    group.addoption(
        "--imap-password",
        action="store",
        default=None,
        help="IMAP/SMTP password for the test user "
             "(default: $FAUNA_TEST_IMAP_PASSWORD or random per-run).",
    )


@pytest.fixture(scope="session")
def mail_credentials(request) -> dict:
    """Resolve handle + password for the docker mail e2e test.

    Resolution order: pytest CLI flag → env var → default.
    Domain is fixed to 'localhost' (the domainless box's identity fallback for
    these tests — it boots domainless and the bare-handle admin claim resolves
    the identity to 'localhost').
    """
    handle = (
        request.config.getoption("--imap-handle")
        or os.environ.get("FAUNA_TEST_IMAP_HANDLE")
        or "mailtest"
    )
    password = (
        request.config.getoption("--imap-password")
        or os.environ.get("FAUNA_TEST_IMAP_PASSWORD")
        or f"tk_{secrets.token_hex(8)}"
    )
    domain = "localhost"
    return {
        "handle": handle,
        "password": password,
        "domain": domain,
        "address": f"{handle}@{domain}",
    }
