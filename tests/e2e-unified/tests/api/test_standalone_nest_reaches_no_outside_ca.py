"""tier_3: a standalone-mode nest claimed onto a domain orders from no public CA —
``testing.md`` § Default app and nest mode (a test venue never depends on the
outside world) and ``nest/tls-certificates.md`` § ACME settings — constants, not
choices (the test build's dark default directory).

The standalone twin of ``platform/docker/test_docker_mode_reaches_no_outside_name.py``.
A claim domain starts the nest's HTTP-01 order at boot; left on the constant
public CA, every standalone box the suite claims onto ``fauna.test`` ordered a
certificate from Let's Encrypt production from the dev box itself. A docker run
goes dark through the run's record-less resolver; a standalone nest is a
``test-hooks`` build, which forwards ``fauna-acme-http01/e2e-dark-ca``, so with
no ``[acme].directory_url`` its order goes to a closed loopback port and fails
before a connection. The release check needs nothing new: a ``test-hooks`` nest
naming no ``FAUNA_E2E_RELEASE_FEED_URL`` runs no check at all
(``tests/api/test_release_check.py``), and the absence is read again here.

**What is observed, and from where.** The nest's own log: the order names its
directory (``ACME directory: …``), which must be the dark loopback one and never
the public CA's host, the order fails, no failure line carries a CA's answer,
and no release-check line appears.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

#: `fauna_acme_http01::E2E_DARK_DIRECTORY_URL`.
DARK_DIRECTORY = "https://127.0.0.1:9/directory"

#: The directory line every order logs (`fauna_acme_http01::obtain_certificate`).
DIRECTORY_LINE = "ACME directory: "

#: The constant public ACME CA's directory host and the release API's host.
OUTSIDE_HOSTS = ("acme-v02.api.letsencrypt.org", "api.github.com")

#: Text only a reached service puts in a log line.
ANSWER_MARKERS = ("urn:ietf:params:acme:error", "public suffix",
                  "GitHub API returned", "rate limit exceeded")

#: The two failure lines of the cert paths a claim domain starts.
ACME_ATTEMPT = "issuance failed"
RELEASE_CHECK = "update check failed"

#: A ceiling, not a target: the order starts at boot and fails in milliseconds.
BOOT_PATHS_BUDGET_S = 120


@pytest.fixture(scope="module")
def outside_dark_nest(request, nest_mode, tmp_path_factory):
    """A dedicated standalone TLS nest claimed onto `fauna.test`, as the suite's
    domain-bearing boxes are — TLS because a plain-HTTP nest has no certificate
    to order (the order seen in the wild ran on a TLS box claimed after boot)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "outside-dark",
        claim_domain="fauna.test", serve_tls=True,
    )
    try:
        yield nest
    finally:
        cleanup()


def _lines_with(path: str, needle: str) -> list[str]:
    text = Path(path).read_text(errors="replace") if Path(path).exists() else ""
    return [line for line in text.splitlines() if needle in line]


def test_a_standalone_nest_claimed_onto_a_domain_reaches_no_outside_host(outside_dark_nest):
    log_path = outside_dark_nest["log_path"]
    wait_until(
        lambda: (_lines_with(log_path, DIRECTORY_LINE)
                 and _lines_with(log_path, ACME_ATTEMPT)),
        BOOT_PATHS_BUDGET_S, interval=1,
        diagnose=lambda: (
            f"the nest's log ({log_path}) shows "
            f"{len(_lines_with(log_path, DIRECTORY_LINE))} directory line(s) and "
            f"{len(_lines_with(log_path, ACME_ATTEMPT))} ACME failure line(s); a "
            f"nest claimed onto fauna.test must have ordered for the absence "
            f"below to mean anything"))

    directories = {line.split(DIRECTORY_LINE, 1)[1].strip()
                   for line in _lines_with(log_path, DIRECTORY_LINE)}
    assert directories == {DARK_DIRECTORY}, (
        f"a test-hooks nest with no [acme].directory_url must order from the "
        f"dark loopback directory {DARK_DIRECTORY!r} alone; it ordered from "
        f"{sorted(directories)}")

    text = Path(log_path).read_text(errors="replace")
    named = [h for h in OUTSIDE_HOSTS if h in text]
    assert not named, f"the nest's log names outside host(s) {named}: {log_path}"

    attempts = _lines_with(log_path, ACME_ATTEMPT)
    answered = [line for line in attempts if any(m in line for m in ANSWER_MARKERS)]
    assert not answered, (
        "a public service answered the nest:\n" + "\n".join(answered[:5]))

    checks = _lines_with(log_path, RELEASE_CHECK)
    assert not checks, (
        f"a test-hooks nest naming no release feed must send no release check: "
        f"{checks[:3]}")
