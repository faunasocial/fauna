"""Pytest fixtures and CLI options for docker e2e tests."""

import os
import secrets

import pytest


def pytest_collection_modifyitems(items):
    """Raise the per-test timeout for tier_4 docker tests lacking an explicit mark.

    The harness-wide default (pytest.ini ``timeout = 900``) assumes locally-built
    binaries; here a *cold* ``just docker-build`` runs inside the first test's
    fixture-setup phase and can exceed it. Bounded is still the law — 60 min covers
    a cold multi-stage image build; a wedged run dies loudly, never silently."""
    for item in items:
        if item.get_closest_marker("timeout") is None:
            item.add_marker(pytest.mark.timeout(3600))


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
