"""tier_4: a docker-mode nest resolves no name outside its run network —
``testing.md`` § Default app and nest mode (a test venue never depends on the
outside world).

Left on docker's default upstream, the release image reached two public services
from every test run: it ordered its web cert from the constant public ACME CA
once a claim domain was set (the CA answered each order for ``*.fauna.test``
with a refusal), and its daily release check polled the release host at every
boot until the anonymous budget answered 403. Both go by hostname, so the docker
provider runs every nest through the run's record-less resolver
(``_DockerProvider._ensure_run_resolver``), and the image itself stays exactly as
shipped — no mounted ``nest.toml``, no new entrypoint env.

**What is observed, and from where.** Inside the nest container, glibc's own
lookup (``getent hosts``, the resolver path the nest's HTTP clients use) finds
nothing for the ACME directory's host or the release API's host, while the run
resolver's own container name still resolves — so the outside went dark and the
run network did not. The nest's log, after its first ACME attempt and its first
release check, carries no answer from either service: each fails before a
connection, never with a CA's problem document or an HTTP status.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from helpers.waiting import wait_until

from .helpers import docker_build, get_repo_root

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
]

#: The constant public ACME CA's directory host (`nest/tls-certificates.md`
#: § ACME settings — constants, not choices) and the release API's host
#: (`fauna_core::version::GITHUB_API_ORIGIN`).
OUTSIDE_HOSTS = ("acme-v02.api.letsencrypt.org", "api.github.com")

#: Text only a reached service puts in a log line: the CA's problem documents and
#: the release API's status answers.
ANSWER_MARKERS = ("urn:ietf:params:acme:error", "public suffix",
                  "GitHub API returned", "rate limit exceeded")

#: The two lines that prove both paths ran.
ACME_ATTEMPT = "issuance failed"
RELEASE_CHECK = "update check failed"

#: A ceiling, not a target: both paths start at boot and fail in seconds.
BOOT_PATHS_BUDGET_S = 240


@pytest.fixture(scope="module")
def docker_image():
    return docker_build(get_repo_root())


@pytest.fixture()
def provider_nest(docker_image, tmp_path_factory):
    import conftest

    handle, cleanup = conftest._DockerProvider(docker_image).start(
        None, tmp_path_factory, "outside-dark", claim_domain="fauna.test")
    try:
        yield conftest, handle
    finally:
        cleanup()


def _resolves(container: str, host: str) -> bool:
    r = subprocess.run(["docker", "exec", container, "getent", "hosts", host],
                       capture_output=True, text=True, timeout=30)
    return r.returncode == 0 and bool(r.stdout.strip())


def _log_lines_with(path: str, needle: str) -> list[str]:
    text = Path(path).read_text(errors="replace") if Path(path).exists() else ""
    return [line for line in text.splitlines() if needle in line]


def test_a_docker_mode_nest_reaches_no_outside_name(provider_nest):
    conftest, handle = provider_nest
    name = handle["container_name"]

    resolver = conftest._DockerProvider._run_resolver_name()
    assert _resolves(name, resolver), (
        f"the run resolver's own container name {resolver!r} does not resolve "
        f"inside {name}: the run network went dark too, so a negative below "
        f"would prove nothing")
    reached = [h for h in OUTSIDE_HOSTS if _resolves(name, h)]
    assert not reached, (
        f"{name} resolves {reached}: a docker-mode nest can still reach the "
        f"outside world by name")

    # Both paths start at boot (the release check's first tick is immediate; a
    # claim domain starts the cert order), so wait for one line of each, then
    # read what they say.
    log_path = handle["log_path"]
    wait_until(
        lambda: (_log_lines_with(log_path, ACME_ATTEMPT)
                 and _log_lines_with(log_path, RELEASE_CHECK)),
        BOOT_PATHS_BUDGET_S, interval=2,
        diagnose=lambda: (
            f"the nest's log ({log_path}) shows "
            f"{len(_log_lines_with(log_path, ACME_ATTEMPT))} ACME attempt "
            f"line(s) and {len(_log_lines_with(log_path, RELEASE_CHECK))} "
            f"release-check line(s); both paths must have run for the absence "
            f"below to mean anything"))
    attempts = _log_lines_with(log_path, ACME_ATTEMPT)
    checks = _log_lines_with(log_path, RELEASE_CHECK)

    answered = [line for line in attempts + checks
                if any(m in line for m in ANSWER_MARKERS)]
    assert not answered, (
        "a public service answered the nest:\n" + "\n".join(answered[:5]))
