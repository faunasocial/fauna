"""tier_4 e2e: nest's per-(bridge,actor,credential) ``bridge_rate_limit`` and the
MDA's CalDAV auth-material cache under a real request BURST (not single requests).

Gap 2 of ``docs/goal/architecture/testing.md`` (the real mail-flow perimeter),
track **2e** (tracked internally). Two bugs in this
class reproduced only against example.com and cost live debugging:

  - The CalDAV auth-material cache ("MDA caches CalDAV auth material to stay under nest's
    bridge_rate_limit"): CalDAV is stateless HTTP Basic auth, so without a cache
    EVERY request re-fetches the wrapped MLS blob from nest. A MUA sync burst (two
    clients plus the IMAP poller sharing the actor's ``default`` credential bucket)
    then blows past nest's ``bridge_rate_limit`` — 30 events / 60 s, keyed on
    (bridge, target_actor, credential); ``bins/fauna-nest/src/bridge_rate_limit.rs``
    — nest returns ``fauna.bridges.rate_limited`` on ``fetch_wrapped_mls_blob`` and
    the MDA 401s a *legitimate* request.
  - The 2c reconnect-loop: a 1/s IMAP reconnect-poll exhausted the same
    30/60 s blob-fetch budget — each fresh IMAP session re-fetches (IMAP auth is
    session-scoped, with NO cross-session cache), so a reconnect STORM trips the
    limit where a steady single session never would.

The two tests are complementary halves of one invariant and make each other
non-vacuous — Test 1 alone could pass because the limit is toothless; Test 2
proves it is genuinely enforced on the mail path:

  Test 1 (``test_caldav_burst_stays_under_rate_limit_via_cache``) — the
    cache regression lock. ``BURST_REQUESTS`` (> 30, so it WOULD exceed the
    30/60 s budget uncached) authenticated CalDAV PROPFINDs on ONE credential,
    fired through the real ``fauna-sni-router`` :443 → MDA CalDAV path (the
    production network shape — implicit TLS, SNI ``mail.<domain>``, PROXY-v2 IP),
    all inside one 30 s cache window. EVERY request must auth-resolve (status
    ``!= 401``; the AUTH middleware runs before path routing, so a path may 404
    after a *successful* auth — "not 401" is the auth signal, per the bare-username
    matrix test). The auth-material cache collapses the N per-request
    ``fetch_wrapped_mls_blob`` calls to ~1, keeping the burst under the budget;
    WITHOUT the cache request ~31 onward 401s.

  Test 2 (``test_imap_reconnect_storm_trips_rate_limit``) — proves the limit is
    genuinely ENFORCED on the mail path, so Test 1's all-pass means "the cache
    worked", not "the limit is toothless". IMAP auth has no cross-session cache
    (``internal/mda/imap/auth.go`` fetches the wrapped blob per login), so a
    reconnect STORM (``IMAP_STORM`` fresh connect+AUTH on ONE credential, fired
    concurrently within the 60 s window) drives ``IMAP_STORM`` fetches and trips
    the limit: at least one login must FAIL (nest returns
    ``fauna.bridges.rate_limited`` → the MDA AUTH fails) while at least one
    SUCCEEDS (the credential is valid). This is the 2c over-poll class. The
    failure count (~``IMAP_STORM`` − 30) stays below the *separate* 30-failures/min
    per-IP AUTH lockout, so the lockout never masks the successes.

Each test provisions its OWN mail user → independent (actor, credential)
rate-limit buckets → no cross-test budget bleed. tier_4 (real image + s6 router +
MDA listeners); ~8-15 min. Run from ``tests/e2e-unified`` with ``TMPDIR=/work/tmp``.
"""

import base64
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor

import pytest


from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    provision_mail_user,
    register_primary_domain,
    remove_container,
    sni_https_request,
    start_container_with_ports,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"
CLAIM_CODE = "RLBRST"

# > 30 so an UNCACHED CalDAV burst (Test 1) would exceed the 30/60 s budget, and
# an IMAP reconnect storm (Test 2, no cache) actually trips it. 40 leaves Test 2
# with ~10 rate-limited failures — well below the separate 30-failures/min per-IP
# AUTH lockout, so that lockout never masks the ~30 successes.
BURST_REQUESTS = 40
IMAP_STORM = 40


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (tag shared; rebuild is layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def serving_nest(docker_image):
    """A container with nest + all four mail listeners + the s6 router's :443
    mapped to host ports, admin claimed, mail brought all the way to serving.
    Yields the mapped host ports + admin creds. Cleaned up after.

    Module-scoped so the two burst tests share one (expensive) bring-up; each test
    provisions its OWN mail user (``run_seal_helper`` is function-scoped, so user
    provisioning happens in the tests, not here), giving independent
    (actor, credential) ``bridge_rate_limit`` buckets so neither bleeds budget into
    the other. Plaintext storage — the rate limiter + auth-material cache are
    storage-mode-independent (the blob fetch + the CalDAV AUTH middleware), and
    plaintext is the lighter, deterministic deploy default for cert provisioning."""
    http_port, router_port, *mail = find_free_ports(6)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-rate-burst-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, 443: router_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "router_port": router_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
        }
        register_primary_domain(nest, DOMAIN)
        bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
        yield nest
    finally:
        remove_container(name)


def _basic_auth(username: str, password: str) -> dict[str, str]:
    creds = base64.b64encode(f"{username}:{password}".encode()).decode()
    return {"Authorization": f"Basic {creds}"}


# ── Tests ─────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_caldav_burst_stays_under_rate_limit_via_cache(serving_nest, run_seal_helper):
    """Test 1 — the auth-material-cache regression lock. See module
    docstring."""
    nest = serving_nest
    user = provision_mail_user(
        nest, run_seal_helper, domain=DOMAIN, local_part="burstcal",
        password="burst-caldav-pw-1")  # gitleaks:allow
    auth = _basic_auth(user["username"], user["password"])
    rp = nest["router_port"]

    # Control — one authenticated CalDAV request must auth-resolve (the path may
    # 404 after a successful auth, so "not 401" is the AUTH signal). A 401 HERE
    # means the user provisioning / credential is broken, NOT the rate limit — a
    # distinct failure from the burst tripping it, so flag it separately.
    status0, _h, body0 = sni_https_request(
        rp, f"mail.{DOMAIN}", "PROPFIND", "/caldav/", extra_headers=auth)
    assert status0 != 401, (
        f"control CalDAV PROPFIND must auth-resolve for {user['username']}; got 401 "
        f"— provisioning/credential broken, not the rate limit. body={body0[:200]!r}\n\n"
        f"── bridge diagnostics ──\n{bridge_diag(nest['name'])}")

    # The burst — BURST_REQUESTS authenticated requests on the SAME credential,
    # all inside one 30 s cache window. With the cache: ~1 nest fetch, all resolve.
    # Without (pre-198b90035): request ~31 onward 401s as fetch_wrapped_mls_blob
    # hits fauna.bridges.rate_limited and the MDA can no longer build the session.
    statuses = []
    start = time.monotonic()
    for _ in range(BURST_REQUESTS):
        st, _h, _b = sni_https_request(
            rp, f"mail.{DOMAIN}", "PROPFIND", "/caldav/", extra_headers=auth)
        statuses.append(st)
    elapsed = time.monotonic() - start

    locked = [i for i, st in enumerate(statuses) if st == 401]
    assert not locked, (
        f"{len(locked)}/{BURST_REQUESTS} authenticated CalDAV requests 401'd during a "
        f"{elapsed:.1f}s burst (first at #{locked[0] + 1}). The MDA auth-material cache "
        f"must collapse the per-request fetch_wrapped_mls_blob calls so the "
        f"burst stays under nest's 30/60s bridge_rate_limit; a 401 means the cache "
        f"missed and nest returned fauna.bridges.rate_limited on the blob fetch.\n\n"
        f"statuses={statuses}\n\n── bridge diagnostics ──\n{bridge_diag(nest['name'])}")


@pytest.mark.feature("mail-abuse-refused")
def test_imap_reconnect_storm_trips_rate_limit(serving_nest, run_seal_helper):
    """Test 2 — an IMAP reconnect storm genuinely trips ``bridge_rate_limit`` (the
    2c over-poll class), proving Test 1's all-pass is the cache working, not a
    toothless limit. See module docstring."""
    nest = serving_nest
    user = provision_mail_user(
        nest, run_seal_helper, domain=DOMAIN, local_part="burstimap",
        password="burst-imap-pw-1")
    imaps_port = nest["mail_ports"][993]

    def _one_login(_i):
        """One full fresh IMAP session: connect + AUTHENTICATE PLAIN, then close.
        Each fresh session re-fetches the wrapped MLS blob from nest (IMAP auth is
        session-scoped — no cross-session cache), so it consumes one
        bridge_rate_limit event. Returns the tagged AUTH status ("OK"/"NO"/"BAD"),
        or ``ERR:<exc>`` if the attempt raised (a rate-limited fetch can also drop
        the connection / withhold the AUTHENTICATE continuation)."""
        from helpers.mail_wire import _imap_auth_plain, _imaps_connect_addr
        deadline = time.monotonic() + 60.0
        try:
            sock, buf = _imaps_connect_addr("127.0.0.1", imaps_port, DOMAIN, deadline)
            try:
                return _imap_auth_plain(
                    sock, buf, "a1", user["username"], user["password"], deadline)
            finally:
                sock.close()
        except Exception as e:  # noqa: BLE001 — a tripped fetch may reset the conn
            return f"ERR:{type(e).__name__}"

    # Fire the storm concurrently so > 30 fetch_wrapped_mls_blob calls pile inside
    # the 60 s window (a real reconnect loop / multi-MUA storm). IMAP has no
    # auth-material cache, so each login fetches; IMAP_STORM (> 30) logins exceed
    # the 30/60 s budget and nest returns fauna.bridges.rate_limited on the
    # overflow, which the MDA surfaces as an AUTH failure.
    with ThreadPoolExecutor(max_workers=IMAP_STORM) as pool:
        results = list(pool.map(_one_login, range(IMAP_STORM)))

    ok = [r for r in results if r == "OK"]
    failed = [r for r in results if r != "OK"]
    assert ok, (
        f"at least one IMAP login must SUCCEED (the credential is valid); all "
        f"{IMAP_STORM} failed → setup broken, not the rate limit. results={results}\n\n"
        f"── bridge diagnostics ──\n{bridge_diag(nest['name'])}")
    assert failed, (
        f"an IMAP reconnect STORM of {IMAP_STORM} fresh logins on one credential must "
        f"trip nest's 30/60s bridge_rate_limit (each fresh session re-fetches "
        f"fetch_wrapped_mls_blob — IMAP has no cross-session auth-material cache), so "
        f"some logins MUST fail with fauna.bridges.rate_limited → AUTH fail. None did, "
        f"so either the limit is not enforced on the mail path or the storm did not "
        f"pile within the 60s window. results={results}\n\n"
        f"── bridge diagnostics ──\n{bridge_diag(nest['name'])}")
