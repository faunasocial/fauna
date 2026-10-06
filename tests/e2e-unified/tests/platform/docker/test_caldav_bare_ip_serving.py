"""tier_4 e2e: a domainless / bare-IP nest serves CalDAV on a PUBLISHED port.

The home2 keystone (``docs/goal/behavior/caldav-server.md`` § Network exposure;
tracked internally). A box reached by a bare IP has
no usable ``mail.<domain>`` SNI, so the always-up ``fauna-sni-router`` (which
fronts container :443 and routes CalDAV only via that SNI) can never reach the
MDA's CalDAV listener — and the entrypoint historically pinned CalDAV to loopback
``127.0.0.1:8444``, so on a bare-IP box CalDAV was simply **unreachable** while
the web SPA worked on ``https://<ip>/`` (the router's default → nest). That is the
exact bug the user reported on home2.

The fix (this branch): when ``FAUNA_LAN_BIND_IP`` is set (the home-relay / bare-IP
shape), the entrypoint writes the new ``caldav_bind_host`` operator-hatch carrying
*only* the LAN interface, and the Go MDA binds ``<LAN-IP>:<admin-port>`` directly
— router-bypassing, reachable by bare IP, with the admin port (nest state) still
governing the number (no pin; the rebind path stays live). It is the exact CalDAV
analogue of the IMAP LAN bind, driven by the LAN topology **independent of
``FAUNA_DOMAIN``**.

Only tier_4 can prove this: the entrypoint that writes ``caldav_bind_host``, the
s6 supervisor that brings the MDA up on ``caldav-enabled``, and the MDA's
``resolveMDAListenAddrs`` that binds the published port all live **inside the
deploy image**. Spawning binaries directly (tier_3, ``test_caldav_admin_port_rebind``
/ ``test_caldav_onboarding_variants``) bypasses the Dockerfile + s6 + entrypoint +
router, so it is green even with the home2 bug present (it connects straight to
the MDA's port).

What this asserts, against the host port mapped to the MDA's directly-bound
CalDAV listener (NO SNI router in the path):
  A. An unauthenticated ``PROPFIND /caldav/`` returns ``401`` with
     ``WWW-Authenticate: Basic realm="fauna-caldav"`` — the MDA's own realm
     (``internal/mda/caldav/auth.go``), which nest never emits. This is the proof
     the MDA's CalDAV listener is **bound + published + serving on the bare-IP
     port**: precisely what was unreachable before. (Same assertion level as the
     SNI-router tier_4 test, but here the connection is *direct* to the published
     bind, with no ``mail.<domain>`` SNI — the bare-IP path.)
  B. No domain is registered; the box is ``FAUNA_DOMAIN``-less. The MDA serves
     nest's self-signed FLOOR cert (verify=False), so the listener answers TLS.

Note on the test harness's ``FAUNA_LAN_BIND_IP=0.0.0.0``: a real home box sets a
*specific* LAN IP, binding CalDAV at ``<LAN-IP>:8443`` (the DEFAULT CalDAV port).
The tier_4 bridge-network harness cannot predict the container's eth0 IP before
start, so it binds all-interfaces ``0.0.0.0:8443`` to make the published port
reachable. This does NOT collide with the nest listener: the nest's internal port
moved off 8443 to ``3000``, so ``8443`` is CalDAV-only on every interface — exactly
the default port a fresh home box serves, with no admin-port override needed. The
entrypoint + MDA code path exercised is **identical** to the specific-LAN-IP case
(the entrypoint writes ``caldav_bind_host = <value>`` either way); only the value
differs. On the private NAT axis a ``0.0.0.0`` bind additionally logs a loud
plaintext-exposure warning (``resolveMDAListenAddrs``); this test uses the default
public axis to keep that out of the log assertions.
"""

import subprocess
import time

import pytest
import requests
import urllib3


from .helpers import (
    admin_ws,
    await_approved_bridges,
    await_keypairs,
    claim_admin_api,
    docker_build,
    find_free_ports,
    flag_present,
    get_repo_root,
    is_commanded_up,
    remove_container,
    restart_service,
    start_container_with_ports,
    svstat,
    wait_for_health,
    wait_for_tls_handshake,
    IMAGE_TAG,
)

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

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

CLAIM_CODE = "BAREIP01"


def _caldav_flag_present(name: str) -> bool:
    """True once nest has materialized ``/data/caldav-enabled`` (the durable
    CalDAV-enable gate the s6 MDA run-script checks — ``mail_enable.rs``
    ``CALDAV_ENABLE_FLAG``).

    A thin alias for the shared helper now that it takes the flag name; kept so
    the two call sites below still read as a CalDAV question rather than a
    string argument."""
    return flag_present(name, "caldav-enabled")


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (tag shared; rebuild is layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def bare_ip_caldav_nest(docker_image):
    """A DOMAINLESS container (no FAUNA_DOMAIN) whose MDA binds CalDAV on a
    routable, PUBLISHED port via ``caldav_bind_host`` (set by the entrypoint from
    ``FAUNA_LAN_BIND_IP``). Yields the mapped host ports + admin creds.

    Bring-up is CalDAV-ONLY (no MTA — a domainless box has no domain, so external
    mail genuinely cannot run; the MTA correctly idles): ``set_caldav_enabled``
    writes ``/data/caldav-enabled`` → ``mda_should_run`` brings the MDA up, it
    self-enrolls over loopback + auto-approves, and serves nest's floor cert (no
    ``provision_self_signed_cert`` — the floor is written on every nest entry path
    and ``fetch_tls_cert_blob`` seals it regardless of domain)."""
    http_port, caldav_host_port = find_free_ports(2)
    name = f"fauna-bare-ip-caldav-{http_port}"
    start_container_with_ports(
        name,
        # 3000 → nest API/web (behind the router's default backend); 8443 → the
        # MDA's directly-bound CalDAV listener on the DEFAULT port, router-bypassing.
        # The nest's internal port is 3000, so 8443 is CalDAV-only — no collision
        # even with the all-interfaces (FAUNA_LAN_BIND_IP=0.0.0.0) harness bind.
        {3000: http_port, 8443: caldav_host_port},
        env={
            # NO FAUNA_DOMAIN — the bare-IP / domainless case (the home2 shape).
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # The home-relay / bare-IP topology marker: drives the entrypoint to
            # write caldav_bind_host (+ the IMAP LAN binds, inert here — IMAP off).
            "FAUNA_LAN_BIND_IP": "0.0.0.0",
        },
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        # CalDAV needs a committed storage mode (the floor cert + serving path);
        # plaintext is the "I trust the box" home-box default.
        nest = {
            "name": name,
            "port": http_port,
            "caldav_host_port": caldav_host_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
        }

        with admin_ws(nest) as admin_client:
            # No set_caldav_port: the MDA binds the DEFAULT CalDAV port (8443),
            # exactly what a fresh home box uses. (Before the nest→3000 move this
            # needed a non-8443 admin port to dodge a nest-loopback :8443 collision;
            # with the nest on 3000, 8443 is free for the real CalDAV default.)
            assert admin_client.call(
                "fauna.bridges.set_caldav_enabled", {"enabled": True}
            ).get("ok") is True

            # The s6 supervisor brings the MDA up on the caldav-enabled flag.
            deadline_up = time.monotonic() + 30
            while time.monotonic() < deadline_up:
                if _caldav_flag_present(name) and is_commanded_up(
                    svstat(name, "fauna-mail-bridge-mda")
                ):
                    break
                time.sleep(0.5)
            assert _caldav_flag_present(name), \
                "/data/caldav-enabled must materialize on set_caldav_enabled(true)"
            assert is_commanded_up(svstat(name, "fauna-mail-bridge-mda")), \
                f"MDA must be commanded up; got {svstat(name, 'fauna-mail-bridge-mda')!r}"

            await_keypairs(name, ("mda",))
            assert "mda" in await_approved_bridges(admin_client, ("mda",)), \
                "mda bridge must self-enroll + auto-approve over loopback"
            # Re-run cold boot against approved state so it fetches the floor cert
            # + binds the CalDAV listener immediately.
            restart_service(name, "fauna-mail-bridge-mda")

        # The MDA serves CalDAV implicit-TLS off the floor cert; a completed TLS
        # handshake on the published port is the observable proof the listener is
        # bound + reachable by bare IP (no banner — CalDAV is HTTP-over-TLS).
        wait_for_tls_handshake("127.0.0.1", caldav_host_port, timeout=90)
        yield nest
    finally:
        remove_container(name)


@pytest.mark.feature("calendar-in-standard-apps")
def test_bare_ip_caldav_listener_serves_on_published_port(bare_ip_caldav_nest):
    """(A+B) An unauthenticated PROPFIND to the DIRECTLY-published CalDAV port
    (no SNI router) is challenged with the MDA's own ``fauna-caldav`` realm —
    proof the MDA's CalDAV listener is bound + published + serving on the bare-IP
    box, the exact reachability the home2 bug lacked."""
    port = bare_ip_caldav_nest["caldav_host_port"]
    resp = requests.request(
        "PROPFIND",
        f"https://127.0.0.1:{port}/caldav/",
        verify=False,  # nest's self-signed floor cert (no registered domain)
        timeout=30,
    )
    assert resp.status_code == 401, (
        f"unauthenticated PROPFIND /caldav/ on the published bare-IP CalDAV port "
        f"must be a 401 auth challenge (proves the MDA listener is serving here); "
        f"got {resp.status_code}, body={resp.text[:200]!r}"
    )
    www_auth = resp.headers.get("WWW-Authenticate", "")
    assert "fauna-caldav" in www_auth, (
        f"the 401 must carry the MDA's WWW-Authenticate realm fauna-caldav "
        f"(proves the MDA CalDAV listener answered on the published bare-IP port, "
        f"not nest); got header {www_auth!r}"
    )
