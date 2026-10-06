"""tier_4 faithful repro attempt for the live CalDAV ``CalendarsLoaded: 0`` bug —
the factory-reset → re-claim → enable-mail → encrypted-calendar cycle run against
the **real Docker image**, where the s6 supervisor restarts the nest for real.

Why this exists
---------------
The live CalDAV roundtrip (``test_caldav_live_nest.py`` vs example.com) hit a bug NO
local e2e caught: after ``factory_reset → re-claim (same identity) → enable-mail``
the linux Events page showed ``CalendarsLoaded: 0`` and ``create_event`` failed
("Event did not appear", ``actions/events.py:68``). The tier_3 cycle test
(``tests/test_factory_reset_calendar_reclaim.py``) proved the **nest-side** re-claim
cycle is SOUND — a 4-variant matrix (plaintext|encrypted × fresh-reonboard|
same-window) eliminated the live candidate "msek not derivable after re-claim". So
the live failure must live where tier_3 cannot reach: the real Docker image + the
s6 ``longrun`` supervisor that restarts the nest for real on ``factory_reset``
(tier_3 re-spawns the binary itself), plus the real MDA/MTA s6 sidecars
cold-booting after the wipe.

This test runs the SAME enable-mail → Events → create-event flow twice — once on a
fresh claim, once after a REAL (s6-driven) factory-reset + re-claim — so the reset
is the only variable. Encrypted storage + the faithful live mirror.

Scope — what this DOES and does NOT exercise
--------------------------------------------
The linux Events ``CalendarsLoaded`` path is **client ↔ nest** (WS-RPC): the client
fetches calendars, the nest reads the actor's ``fauna.state.mail`` ``msek`` and lazily
provisions the Personal calendar (``apps/fauna-linux/src/client.rs`` ``fetch_calendars``
→ ``caldav_context`` → ``provision_calendar``). The MDA is NOT on that path, so this
test does NOT approve the bridge or wait for external CalDAV serving — that would
exercise the MDA's cold-boot recoverability, a *different* invariant. It targets the
``CalendarsLoaded: 0`` symptom directly: real image + real s6 restart + the client's
own calendar fetch after re-claim. (``enable_mail`` itself only writes
``/data/imap-enabled`` and mints the actor's msek + credential, all nest-side and
independent of the bridge actually serving — so the calendar must load regardless.)

What this caught, and the fix (now GREEN)
-------------------------------------------------------------------------------------------------
This test originally surfaced a tier_4-only recoverability gap that tier_3 (plain
HTTP) structurally cannot catch: after the real s6 factory-reset → re-claim, the
linux app could not re-establish its WSS WS-RPC connection to the
(self-signed-TLS) nest — ``connection-status`` stayed ``Disconnected`` and the
post-reclaim Events page was unreachable (it was ``xfail`` while the bug stood).

Root cause (nest-side, fixed): the TLS channel binding
(``auth_handlers::build_cert_binding``) signs the served-cert SPKI with the
**deployment signing key** — the ``nest_keypair`` DB row, which migrations
**randomly regenerate whenever the DB is absent**. Factory reset wipes ``nest.db``,
so the post-reset boot minted a *new random* deployment identity; the
channel-binding ``nest_actor_id`` changed; a self-signed/TOFU client's
process-global identity **pin** (from phase 1) then mismatched → ``IdentityError``
in graduation → no reconnect, **no TLS error**. (A WebPKI client like example.com, and
the Python ``CERT_NONE`` client, are unaffected — they never pin the binding.) The
fix makes the deployment key durable across a reset (``nest_deployment.key``,
preserved like ``nest_identity.key``; ``bins/fauna-nest/src/deployment_key.rs``
reconciles the DB row from it at boot) so the re-claimed nest re-presents the SAME
``nest_actor_id`` and pinned clients reconnect (``security.md`` § Transport trust;
``common.md`` § Factory reset; unit test ``deployment_key::tests::
deployment_key_is_stable_across_factory_reset_wipe``).

So phase 2 now completes the full cycle — reconnect → enable mail →
``CalendarsLoaded: 1`` → create event — exactly like phase 1, proving the calendar
path is sound after a re-claim against the real image (the live ``CalendarsLoaded:
0`` does NOT reproduce here; that bug lives elsewhere, see the cross-reference).

The ``[diag] ... connected=False`` probe before ``driver.reset()`` is **expected
and non-fatal**: a *persistent* phase-1 client's keepalive does not silently
auto-reconnect to a box that was wiped + re-claimed (its TTL-cached bearer is a
session the fresh nest forgot) — a wiped box correctly routes the client through
re-onboarding (phase 2's ``driver.reset()`` + re-login), which is the faithful live
flow and now succeeds. Seamless keepalive auto-reconnect of a long-lived client
across a reset is a separate, lower-priority robustness follow-on (bearer/session
staleness on the keepalive path, distinct from the transport-identity fix here).

Linux-only: the linux Events page is the encrypted-CalDAV-store (``bridge_caldav_*``)
surface where the live symptom appears (``docs/goal/ui/events.md`` Decision B); the
other apps have no such encrypted-CalDAV-store surface. The skip is checked first so the heavy
image build never runs for the parametrized web app.
"""

import subprocess
import time
import uuid
from datetime import datetime, timedelta

import pytest

from common.auth import claim_admin
from helpers.app_surface import skip_unbuilt

from .helpers import (
    docker_build,
    factory_reset_and_reclaim_docker,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
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

DOMAIN = "localhost"   # the deployment's primary (local) mail domain
CLAIM_CODE = "RCAL1"   # pinned so the post-reset re-claim reuses it


def _future_dt(days_ahead: int, hour: int, minute: int = 0) -> str:
    dt = datetime.now() + timedelta(days=days_ahead)
    return dt.replace(hour=hour, minute=minute, second=0, microsecond=0).strftime(
        "%Y-%m-%dT%H:%M"
    )


def _client_log_tail(app, n: int = 60) -> str:
    """The tail of the linux app's stderr (``app.err`` in the driver's per-run
    tmp dir) — the WS-RPC connect/TLS errors that don't surface to the UI error
    element. Best-effort: returns '' if the path is unavailable."""
    import os

    tmp = getattr(app.driver, "_tmp_dir", None)
    if not tmp:
        return ""
    path = os.path.join(tmp, "app.err")
    try:
        with open(path, errors="replace") as f:
            lines = f.readlines()
        return "".join(lines[-n:])
    except OSError:
        return ""


def _poll_connected(app, timeout: float = 45.0) -> bool:
    """Non-fatal: True if ``connection-status`` reaches "Connected" within
    ``timeout``. Used to probe whether the client's keepalive auto-reconnects to a
    restarted nest WITHOUT any driver action."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if (app.driver.get_text("connection-status") or "").startswith("Connected"):
                return True
        except Exception:
            pass
        time.sleep(1.5)
    return False


def _wait_connected(app, timeout: float = 90.0) -> None:
    """Wait until the sidebar ``connection-status`` indicator reads "Connected".

    The authed shell renders before ``start_ws_rpc()``'s async ``connect()``
    completes, so firing any WS-RPC (enable-mail's ``set_mail_enabled``, the
    Events page's calendar fetch) before this races the connect → ``rpc
    disconnected``. The live CalDAV test gates on this exact indicator after
    re-claim (``test_mail_zero_cheat_live._wait_connected``); a tier_4 docker nest
    restart is heavier than a tier_3 binary respawn, so the post-reclaim reconnect
    is slower and this guard is load-bearing here where tier_3 got away without it.
    """
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            last = app.driver.get_text("connection-status") or ""
        except Exception:
            last = ""
        if last.startswith("Connected"):
            return
        time.sleep(1.5)
    raise AssertionError(
        f"WS-RPC never reached Connected within {timeout:.0f}s (last status: "
        f"{last!r}) — the client could not re-establish its live connection to the "
        f"re-claimed nest. error={app.error_text()!r}\n"
        f"── client app.err tail ──\n{_client_log_tail(app)}"
    )


def _login_admin(app, nest) -> None:
    """Log the linux app in as the nest admin on the regular (feed) shell —
    the calendar actor, mirroring the live CalDAV test. The admin identity
    survives ``factory_reset_and_reclaim_docker`` (re-claimed with the same key),
    so the same client re-points at the same actor on the re-claimed nest. Waits
    for the WS-RPC connection to settle before returning so the caller's first
    WS-RPC action does not race the connect."""
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    admin = nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": "test-device-reclaim-docker",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    _wait_connected(app)


def _enable_mail_and_open_events(app) -> None:
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.events.navigate()


def _create_event_and_assert_visible(app, summary: str, *, phase: str) -> None:
    """Select Personal + create an event + assert it renders — the exact user
    action that failed live (``CalendarsLoaded: 0`` → "Event did not appear")."""
    count = app.count("calendar-item")
    assert count >= 1, (
        f"{phase}: the Personal calendar should be present; got {count} calendars "
        f"(CalendarsLoaded: {count}). This is the live symptom reproducing. "
        f"error={app.error_text()!r}"
    )
    app.events.select_calendar("Personal")
    app.events.create_event(summary, start=_future_dt(7, 10), end=_future_dt(7, 11))
    assert any(summary in s for s in app.events.event_summaries()), (
        f"{phase}: event {summary!r} created in Personal should be visible. "
        f"error={app.error_text()!r}"
    )


@pytest.fixture()
def docker_reclaim_nest(app):
    """A fresh **claimed** nest running as the real Docker image, encrypted
    storage, primary domain registered — the linux Events page is the unit under
    test. Linux-only; the skip is first so the heavy image build never runs for
    the parametrized web app."""
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the encrypted-CalDAV calendar-after-reclaim observation",
            detail="the linux Events page is the observation surface for "
            "this docker tier_4 cycle; the other apps are the remaining "
            "cross-app follow-on. As with the two sibling docker drives, the "
            "real gate is a FRESH nest image on a docker-capable box (only "
            "the primary Linux dev VM has a daemon), not the app surface "
            "",
            tracked="caldav-server.md",
        )

    docker_build(get_repo_root())

    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-reclaim-cal-{http_port}"

    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        url = f"https://127.0.0.1:{http_port}"
        admin = claim_admin(http_port, CLAIM_CODE, base_url=url, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "url": url,
            "admin": admin,
            "mail_ports": mail_ports,
        }
        register_primary_domain(nest, DOMAIN)
        # Hand-built https nest — not routed through `_as_nest_handle`, so this
        # port must self-register: `_relaunch_trusting_nest`'s nest.info read
        # (and every other port-keyed `common.auth` dial) would otherwise speak
        # plain http/ws to a TLS-only listener and raise.
        from common.auth import mark_tls_nest

        mark_tls_nest(http_port)
        yield nest
    finally:
        remove_container(name)


@pytest.mark.feature("factory-reset")
def test_calendar_works_after_factory_reset_reclaim_docker(app, docker_reclaim_nest):
    """The Personal calendar must provision AND accept an event after a REAL
    (s6-driven) ``factory_reset → re-claim (same identity) → enable-mail`` against
    the Docker image, exactly as on a first claim. The reset is the only variable
    between the two phases, so a phase-2-only failure reproduces the live bug.

    GREEN since the deployment-key-stability fix (module docstring) — the
    post-reclaim WSS/TOFU reconnect now succeeds."""
    nest = docker_reclaim_nest

    # ── Phase 1: fresh-claim baseline (proves the operation works) ──────────
    _login_admin(app, nest)
    _enable_mail_and_open_events(app)
    _create_event_and_assert_visible(
        app, f"baseline-{uuid.uuid4().hex[:8]}", phase="phase 1 (fresh claim)"
    )

    # ── Phase 2: the REAL s6 factory-reset → re-claim cycle ─────────────────
    # fauna.admin.factory_reset → s6 restarts fauna-nest into the boot-time wipe
    # → re-claim admin (same identity). The wipe cleared local domains, so
    # re-establish them, then re-onboard the SAME client process
    # (driver.reset() = clear stores + return to onboarding, no relaunch) + re-login
    # — the faithful live flow. Only the reset cycle differs from phase 1.
    factory_reset_and_reclaim_docker(nest, claim_code=CLAIM_CODE)
    # DIAGNOSTIC (non-fatal): does the client's WSS keepalive auto-reconnect to the
    # restarted nest WITHOUT any driver action? Distinguishes a real
    # reconnect-after-restart bug (False) from a driver.reset()/set_state harness
    # artifact (True → reconnect works, the reset path is what breaks it).
    auto = _poll_connected(app, timeout=45.0)
    print(f"[diag] keepalive auto-reconnect after factory_reset (no driver action): "
          f"connected={auto}; status={app.driver.get_text('connection-status')!r}")
    register_primary_domain(nest, DOMAIN)
    app.driver.reset()
    _login_admin(app, nest)
    _enable_mail_and_open_events(app)
    _create_event_and_assert_visible(
        app, f"reclaim-{uuid.uuid4().hex[:8]}", phase="phase 2 (after re-claim)"
    )
