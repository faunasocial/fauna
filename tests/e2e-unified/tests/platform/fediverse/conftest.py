"""Fixtures for the real-fediverse interop harness.

The subject under test is a *real third-party server's* acceptance of our
ActivityPub output — JSON-LD, addressing, HTTP signatures — which our own
in-test fediverse server (`tests/api/test_activitypub_federation.py`) accepts by
construction and so cannot prove. Here a pinned official peer image (docker
compose) federates with a locally-built `fauna-nest` binary across a real TLS
boundary, driven entirely black-box through the peer's REST API. See the design
plan and `docs/goal/behavior/activitypub.md` § Implementation status today, gap 4.

**Two peers, one assertion set.** Mastodon is the mainstream, lenient peer;
GoToSocial is a different implementation in a different language that requires
signed inbound fetches unconditionally. The F1-F9 flows are written once against
the `peer` fixture (`helpers/fediverse_peer.py` holds the contract), because a
second copy of the assertions would drift, and a drifted assertion that passes
proves nothing.

Topology (design D3), identical for both peers: one nginx terminates TLS for two
SNI vhosts on the compose network — the peer's vhost (loopback-published, so the
nest's resolve-override reaches it) and `nest.test` (→ the host nest via
host-gateway). A per-run ephemeral CA signs both leaf certs; the nest trusts it
via the D4 `FAUNA_TEST_AP_EXTRA_CA_PEM` hook, the peer via its own CA mount.

Opt-in like tier_4 (design D6): booting a third-party server is not inner-loop,
so the `peer` fixture skips unless `FAUNA_E2E_FEDIVERSE=1`; a general `--tier 3`
sweep never brings a stack up. The `just e2e-*` recipes set it.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path

import pytest

from drivers.port_util import find_free_port
from helpers.ap_nest import build_ap_nest_binary, start_ap_nest
from helpers.ephemeral_ca import EphemeralCA
from helpers.fediverse_peer import GoToSocialPeer, MastodonPeer

# Each peer's compose stack + nginx template live in a directory beside this file.
_FEDIVERSE_DIR = Path(__file__).parent

# The nest's AP identity domain — the vhost the peer dials to reach it. Shared by
# both stacks: the nest half of the topology is peer-independent.
NEST_AP_DOMAIN = "nest.test"

# Cross-boundary federation is worker-driven (seconds), but a cold first job can
# lag; bound generously but hard, same posture as the federation suite.
FED_TIMEOUT_S = 60


class _PeerSpec:
    """Everything that differs between one peer stack and another."""

    def __init__(self, *, name, cls, domain, service, health_path, ready_timeout_s,
                 always_authorized_fetch=False):
        self.name = name
        self.cls = cls
        self.domain = domain
        #: The compose service whose healthcheck gates bring-up.
        self.service = service
        #: An unauthenticated path the TLS front answers once the peer is live.
        self.health_path = health_path
        #: Outer ceiling on the whole bring-up (cold boot + migrations).
        self.ready_timeout_s = ready_timeout_s
        #: True when the peer refuses unsigned fetches with no way to turn that
        #: off, so every run of it is a secure-mode run.
        self.always_authorized_fetch = always_authorized_fetch

    @property
    def dir(self):
        return _FEDIVERSE_DIR / self.name


_PEERS = {
    # Rails cold boot = db:prepare migrations + puma warmup. The web
    # healthcheck's own start_period is 90s; 420s is the outer ceiling.
    "mastodon": _PeerSpec(
        name="mastodon", cls=MastodonPeer, domain="mastodon.test",
        service="web", health_path="/health", ready_timeout_s=420,
    ),
    # One Go binary on SQLite: seconds, not minutes. Still bounded.
    "gotosocial": _PeerSpec(
        name="gotosocial", cls=GoToSocialPeer, domain="gts.test",
        service="gts", health_path="/readyz", ready_timeout_s=120,
        always_authorized_fetch=True,
    ),
}


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    """On failure, attach BOTH servers' logs — the exchange has two witnesses.

    A federation exchange fails inside one server or the other, and each answers
    the peer rather than the harness: an activity we send that the peer refuses,
    and an activity it declines to send us, are both invisible to every
    client-side observation the test can make. So "never delivered" and
    "delivered and rejected" look identical unless the report carries the log of
    the side that made the decision. Convention 6 (failures diagnose themselves)
    plus the lesson that the nest's log is a first-class test surface —
    here generalised to the peer's, which owns the outbound half.
    """
    outcome = yield
    report = outcome.get_result()
    if report.when != "call" or not report.failed:
        return
    peer_obj = (getattr(item, "funcargs", None) or {}).get("peer")
    if peer_obj is None:
        return
    from helpers import ap_nest

    for title, text in (
        (f"{peer_obj.name} server log (tail)", peer_obj.peer_log()),
        ("nest log (tail)", ap_nest.nest_log(peer_obj.nest)),
    ):
        tail = "\n".join((text or "").splitlines()[-120:])
        report.sections.append((f"federation — {title}", tail or "<empty>"))


def _skip_unless_opted_in() -> None:
    if not os.environ.get("FAUNA_E2E_FEDIVERSE"):
        pytest.skip(
            "the real-fediverse interop harness is opt-in (boots a third-party "
            "server); set FAUNA_E2E_FEDIVERSE=1 or run `just e2e-fediverse-test` "
            "/ `just e2e-gotosocial-test`"
        )


def _selected_peer() -> _PeerSpec:
    """Which peer does THIS run federate against?

    The peer is a property of the *run*, not of the individual test — the same
    reasoning that makes strict mode a run-level choice below. One stack is alive
    per run, and the whole F1-F9 matrix re-runs against whichever it is.
    """
    name = os.environ.get("FAUNA_E2E_FEDIVERSE_PEER", "mastodon")
    if name not in _PEERS:
        raise pytest.UsageError(
            f"unknown FAUNA_E2E_FEDIVERSE_PEER={name!r}; "
            f"expected one of {sorted(_PEERS)}"
        )
    return _PEERS[name]


def _strict_mode_requested() -> bool:
    """Does THIS run want a secure-mode stack?

    Set by `just e2e-fediverse-strict-test`. Only meaningful for a peer where
    secure mode is a *choice*: GoToSocial always refuses unsigned fetches, so its
    spec carries `always_authorized_fetch` and this is moot. Keeping the mode at
    run level means the whole matrix can be re-run against a secure-mode peer
    while still booting exactly ONE stack per run.
    """
    return os.environ.get("FAUNA_E2E_MASTODON_STRICT") == "1"


def _compose(spec, project, env, *args, timeout=600):
    return subprocess.run(
        ["docker", "compose", "-p", project, "-f", str(spec.dir / "docker-compose.yml"),
         *args],
        cwd=str(spec.dir), env=env, capture_output=True, text=True, timeout=timeout,
    )


def _wait_service_healthy(spec, project, env, timeout_s):
    """Poll the peer container's healthcheck until healthy; fail loud on timeout."""
    container = f"{project}-{spec.service}-1"
    deadline = time.time() + timeout_s
    last = "unknown"
    while time.time() < deadline:
        r = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Health.Status}}", container],
            capture_output=True, text=True,
        )
        last = (r.stdout or r.stderr).strip()
        if last == "healthy":
            return
        if last == "unhealthy":
            break
        time.sleep(3)
    # Dump what compose saw, so a boot failure diagnoses itself.
    logs = _compose(spec, project, env, "ps").stdout
    raise RuntimeError(
        f"{spec.name} {spec.service} never became healthy within {timeout_s}s "
        f"(last: {last!r}).\n{logs}"
    )


def _wait_tls_front_ready(spec, nginx_port, ca_path, timeout_s=90):
    """Poll the peer's TLS front until it answers on its health path.

    nginx starts only after the peer is healthy (compose `depends_on`), so this
    doubles as the nginx-ready gate — it needs a moment to bind :443 and fork its
    workers, and a lone curl here would hit nginx mid-startup. Bounded (e2e
    conventions point 9); on timeout it fails loud with the last curl's output.
    """
    deadline = time.time() + timeout_s
    last = None
    while time.time() < deadline:
        last = subprocess.run(
            ["curl", "-sf", "--max-time", "10",
             "--resolve", f"{spec.domain}:{nginx_port}:127.0.0.1",
             "--cacert", ca_path,
             f"https://{spec.domain}:{nginx_port}{spec.health_path}"],
            capture_output=True, text=True,
        )
        if last.returncode == 0:
            return
        time.sleep(2)
    raise RuntimeError(
        f"{spec.name} TLS front not reachable via nginx within {timeout_s}s "
        f"(last: rc={last.returncode} {last.stdout!r} {last.stderr!r})"
    )


@pytest.fixture(scope="session")
def peer(tmp_path_factory):
    """Bring up the selected fediverse peer + a host nest that federates with it.

    Opt-in (see module docstring): skips unless `FAUNA_E2E_FEDIVERSE=1`, so the
    expensive stack never boots on a general `--tier 3` run. Session-scoped — the
    peer boots once — and self-terminating: `compose down -v` and the nest kill
    always run, bounded waits throughout.
    """
    yield from _bring_up_stack(tmp_path_factory)


@pytest.fixture(scope="session")
def strict_peer(peer):
    """The peer, asserted to refuse UNSIGNED fetches of its own objects.

    Not a second stack: it is `peer` plus a guard, so a secure-mode test can
    never silently run against a permissive peer and report a meaningless pass.
    Satisfied either by booting Mastodon in secure mode
    (`just e2e-fediverse-strict-test`) or by selecting GoToSocial, which has no
    permissive mode to fall into.
    """
    assert peer.authorized_fetch, (
        f"this test requires a peer that refuses unsigned fetches, but "
        f"{peer.name} booted permissive. Run `just e2e-fediverse-strict-test` "
        f"(secure-mode Mastodon) or `just e2e-gotosocial-test` (GoToSocial, "
        f"which is always strict) rather than selecting the marker by hand."
    )
    return peer


def _bring_up_stack(tmp_path_factory):
    """Shared bring-up for every peer and mode — one implementation, one teardown."""
    _skip_unless_opted_in()

    spec = _selected_peer()
    authorized_fetch = spec.always_authorized_fetch or _strict_mode_requested()

    binary = build_ap_nest_binary()

    run_dir = tmp_path_factory.mktemp(f"fediverse_{spec.name}")
    cert_dir = run_dir / "certs"
    cert_dir.mkdir()

    # Per-run ephemeral CA + the two leaf certs the nginx vhosts serve.
    ca = EphemeralCA()
    ca_path = ca.write_cert(str(cert_dir), "ca")
    ca.issue(spec.domain).write(str(cert_dir))     # <peer domain>.crt / .key
    ca.issue(NEST_AP_DOMAIN).write(str(cert_dir))  # nest.test.crt / .key

    nest_port = find_free_port()
    nginx_port = find_free_port()
    # Peer + mode are part of the project name: a shared name would make a second
    # `compose up` adopt (and then `down -v`) the first's containers.
    mode = "strict" if authorized_fetch else "permissive"
    project = f"fauna-{spec.name}-interop-{mode}-{os.getpid()}"

    # Render the nginx conf (only __NEST_PORT__ is dynamic).
    nginx_conf = run_dir / "nginx.conf"
    nginx_conf.write_text(
        (spec.dir / "nginx.conf.template").read_text().replace(
            "__NEST_PORT__", str(nest_port)
        )
    )

    compose_env = {
        **os.environ,
        "FAUNA_TEST_CERT_DIR": str(cert_dir),
        "FAUNA_TEST_NGINX_CONF": str(nginx_conf),
        "FAUNA_TEST_NGINX_PORT": str(nginx_port),
        # Consumed by Mastodon's web/sidekiq AUTHORIZED_FETCH default; GoToSocial
        # has no such knob (it is always strict) and ignores this.
        "FAUNA_TEST_AUTHORIZED_FETCH": "true" if authorized_fetch else "false",
    }

    # Start the host nest first (fast; it just listens). Bind 0.0.0.0 so the
    # container reaches it via host-gateway; serve real HTTPS (its self-signed
    # floor cert) because `nest.test` is a public AP domain the nest refuses to
    # serve in cleartext (`serve_tls`); trust the CA + resolve the peer domain to
    # the loopback-published nginx port (both D4 test-hooks envs).
    nest = start_ap_nest(
        binary, run_dir / "nest", nest_port, NEST_AP_DOMAIN,
        bind_host="0.0.0.0",
        serve_tls=True,
        extra_env={
            "FAUNA_TEST_AP_RESOLVE_JSON": json.dumps(
                {spec.domain: f"127.0.0.1:{nginx_port}"}
            ),
            "FAUNA_TEST_AP_EXTRA_CA_PEM": ca_path,
        },
    )

    try:
        up = _compose(spec, project, compose_env, "up", "-d")
        if up.returncode != 0:
            raise RuntimeError(f"docker compose up failed:\n{up.stderr}\n{up.stdout}")
        _wait_service_healthy(spec, project, compose_env, spec.ready_timeout_s)

        # Confirm the TLS front + resolve path both work end-to-end before any
        # test runs, rather than letting the first test absorb a topology failure.
        _wait_tls_front_ready(spec, nginx_port, ca_path)

        yield spec.cls(
            project=project, compose_dir=spec.dir, compose_env=compose_env,
            nginx_port=nginx_port, ca_path=ca_path, nest=nest,
            domain=spec.domain, authorized_fetch=authorized_fetch,
        )
    finally:
        nest["proc"].kill()
        nest["proc"].wait()
        _compose(spec, project, compose_env, "down", "-v", "--remove-orphans")
