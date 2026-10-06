"""tier_4 e2e: the linux desktop app UI turns mail on for a nest running as
the **real Docker deployment image** — and the deployed mail bridge must boot.

This is the test the user asked for: *"can the linux desktop app turn mail on for
a local nest run in docker?"* Every prior client-UI mail test (e.g.
``tests/test_mail_client_full_roundtrip.py``) drives the linux UI against a nest
**binary** whose ``fauna-mail-bridge`` is spawned directly by the fixture — which
bypasses the one thing a real deployment depends on: the Docker image's s6
run-scripts gate the mail-bridge services on the ``/data/imap-enabled`` flag, and
that flag is written **only** by ``fauna.bridges.set_mail_enabled`` (see
``bins/fauna-nest/src/mail_enable.rs`` and ``docker/s6/fauna-mail-bridge-mta/run``).
So a binary test can never catch a deployment in which nothing actually flips the
subsystem on. That is exactly what tier_4 (real image + sidecars) exists to catch.

What this asserts: after the admin turns mail on through the linux app UI, the
mail bridge inside the Docker image actually **comes up** — the durable
``/data/imap-enabled`` flag is written and a bridge self-enrolls (a pending
service-user the admin could approve). Booting is the minimal observable "the
subsystem turned on"; the full approve → serving → send/receive round-trip is the
richer follow-on once the enable path exists (see TODO).

History: this pinned a production bug (commit landing tier_4). The per-user mail
enable mints the user's credential + recipient key, but used to **not** flip the
deployment-wide subsystem — ``set_mail_enabled`` had no client-UI caller, so on
the Docker image the flag was never written, the s6-supervised bridge stayed
down, and mail never worked even though every binary e2e was green. Fixed via
design A: ``MailSettingsMachine::enable_mail`` (shared Rust) now fires
``fauna.bridges.set_mail_enabled(true)`` as its final step, scoped to the admin by
the nest's Admin-class gate (a non-admin's call is rejected → no-op). This test is
the green guard for that path. See ``docs/goal/behavior/mail-bridge-lifecycle.md``
§ Enable flow (tracked internally).

**Prod-parity posture (Gap 2g — ``testing.md`` § Gap 2 Target): deliberate opt-out.**
The Gap-2g default makes Encrypted + the enforced inbound DMARC perimeter the
standard tier_4 shape, with ``relax_spam_policy``/plaintext explicit per-test
opt-OUTs. This test legitimately opts out of both: the unit under test is whether
the **client UI's enable-mail flips the deployment subsystem on** (writes
``/data/imap-enabled`` → the s6-supervised bridge boots + self-enrolls). It stops at
bridge-boot — it delivers **no inbound message** and reads back **no sealed copy** —
so neither the enforced inbound DMARC gate (no inbound leg to gate) nor Encrypted
storage (no seal/decrypt path exercised) would change what's asserted. The
``register_primary_domain`` + ``relax_spam_policy`` + plaintext here are setup so a
*correctly* enabled subsystem would reach serving (not the unit under test); the
enforced-perimeter + Encrypted fidelity lives in the inbound/round-trip tests
(``test_mail_deploy_inbound_round_trip``, ``_bidirectional_round_trip``,
``_forwarder_ndr``).
"""

import time

import pytest

from helpers.app_surface import skip_unbuilt

from .helpers import (
    admin_ws,
    bridge_diag,
    claim_admin_api,
    create_network,
    docker_build,
    find_free_port,
    find_free_ports,
    flag_present,
    get_repo_root,
    register_primary_domain,
    relax_spam_policy,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_scanner_sidecars,
    wait_for_health,
)

try:
    import subprocess

    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"   # the deployment's primary (local) mail domain
CLAIM_CODE = "UIEN1"


@pytest.fixture()
def docker_mail_nest(app):
    """A fresh **claimed** nest running as the real Docker image, on a network
    with the fake clamd/rspamd scanners (the fail-closed inbound scan gate). The
    admin is claimed via the API and the primary domain registered + spam policy
    relaxed — none of that is under test here; the unit under test is whether the
    *client UI* can subsequently turn the mail subsystem on. Storage is committed
    to plaintext (the "I trust the box" deploy default).

    Linux-only, and the skip is the FIRST thing so the heavy image build +
    container start never run for the parametrized web app (which can't drive
    this native flow)."""
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the client-UI enable-mail docker drive",
            detail="wired on linux first; the other apps are the cross-app "
            "follow-on (same scope as test_mail_client_full_roundtrip.py). "
            "Every app here has the mail-settings enable toggle already, so "
            "the leg is a driver swap — what actually gates it is a FRESH "
            "nest image on a docker-capable box, and only the primary Linux "
            "dev VM has a docker daemon at all ",
            tracked="mail-settings.md",
        )

    docker_build(get_repo_root())  # module-cached image; a no-op buildx call if fresh

    suffix = find_free_port()
    network = f"fauna-uienable-net-{suffix}"
    clamd_name = f"fauna-uienable-clamd-{suffix}"
    rspamd_name = f"fauna-uienable-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    name = None
    create_network(network)
    try:
        scan = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name
        )
        http_port, *mail = find_free_ports(5)
        mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
        name = f"fauna-mail-uienable-{http_port}"
        start_container_with_ports(
            name,
            {3000: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                "FAUNA_RSPAMD_URL": scan["rspamd_url"],
            },
            network=network,
        )
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
        }
        # Setup the UI can't express (no real DNS for a test domain), done before
        # the UI acts so a *correctly* enabled subsystem would reach serving:
        register_primary_domain(nest, DOMAIN)
        relax_spam_policy(nest)
        # Hand-built https nest — not routed through `_as_nest_handle`, so this
        # port must self-register: `_relaunch_trusting_nest`'s nest.info read
        # (and every other port-keyed `common.auth` dial) would otherwise speak
        # plain http/ws to a TLS-only listener and raise.
        from common.auth import mark_tls_nest

        mark_tls_nest(http_port)
        yield nest
    finally:
        if name:
            remove_container(name)
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


@pytest.mark.feature("turn-on-mail")
def test_linux_ui_enable_mail_boots_docker_bridge(app, docker_mail_nest):
    nest = docker_mail_nest
    name = nest["name"]

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    # ── Point the linux app at the dockerized nest as admin ──────────────
    # Inject the claimed admin identity over the bridge state protocol (the
    # established admin-e2e mechanism — the admin shell's root-stack child builds
    # on a `session` patch; see the `admin_app` fixture). The claim itself isn't
    # under test; turning mail on is.
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": nest["admin"]["secret_hex"],
        },
        "nav": {"stack": [{"view": "admin"}]},
    })
    app.driver.wait_for("admin-dashboard-heading", timeout=20.0)

    # ── Turn mail on through the mail-settings UI (the action under test) ────
    app.mail_settings.navigate()
    app.mail_settings.enable_mail(display_name="Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "the per-user mail enable (mint first credential) did not complete against "
        f"the docker nest; mail-page error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # ── Turning mail on must BOOT the deployed bridge ───────────────────────
    # On the Docker image the mail-bridge s6 services are down until
    # fauna.bridges.set_mail_enabled writes /data/imap-enabled. The admin's
    # enable_mail above fires that call (design A), so the bridge boots and
    # self-enrolls. A booted + self-enrolled pending bridge is the minimal
    # observable "the deployment mail subsystem actually came up."
    deadline = time.monotonic() + 90.0
    flag = False
    enrolled: dict = {}
    with admin_ws(nest) as admin:
        while time.monotonic() < deadline and not (flag and enrolled):
            flag = flag_present(name)
            rows = admin.call(
                "fauna.bridges.list_service_users", {"status": "pending"}
            )["service_users"]
            enrolled = {b["role"]: b for b in rows}
            if flag and enrolled:
                break
            time.sleep(1.5)

    assert flag, (
        "turning mail on through the linux client UI did not enable the deployment "
        "mail subsystem: /data/imap-enabled was never written, so the s6-supervised "
        "mail bridge in the Docker image stayed down. The admin's mail-settings "
        "enable should fire fauna.bridges.set_mail_enabled(true) (design A, "
        "MailSettingsMachine::enable_mail) — check that path didn't regress (or that "
        "the claimed actor is admin so the Admin-class call isn't rejected).\n"
        f"bridge diag:\n{bridge_diag(name)}"
    )
    assert enrolled, (
        "the deployed mail bridge never self-enrolled a pending service-user — the "
        "flag was written but the bridge did not boot/enroll within the timeout.\n"
        f"bridge diag:\n{bridge_diag(name)}"
    )
