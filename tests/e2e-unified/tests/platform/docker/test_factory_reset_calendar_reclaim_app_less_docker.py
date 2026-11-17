"""tier_4, app-less: after a factory reset and a fresh claim, a nest running the
released image serves the calendar again.

The `[nest]` outcome of `docs/features/factory-reset.md` that the app-driving
docker twin (``test_factory_reset_calendar_reclaim_docker.py``) could not carry:
the marker attributes an app-driving test's records to the apps it drove, so a
witness counting for every column must drive no app (`feature-catalog.md`
§ Cell semantics, § The marker). Owner of the promise:
`docs/goal/architecture/nest/common.md` § Factory reset.

Every step is the production path, with the shared client code the apps run
reached through the e2e harness library:

1. the real image boots, joined to a scanner-wired
   network (enabling mail needs content scanners beside the nest), is claimed
   and given its primary local domain;
2. mail is turned on by the shared Mail settings machine
   (``harness_enable_mail``): the MSEK sealed into the account's mail custody,
   as an app's first enable leaves it;
3. an event is made the way an app's Events page makes one
   (``harness_calendar_make_event``): the MSEK read back out of custody through
   the shared ``dav_store_context``, the calendars loaded through
   ``list_calendars_with_personal`` (the lazy ``Personal`` — the step the live
   ``CalendarsLoaded: 0`` symptom broke), the event sealed, put and read back
   from the nest;
4. ``fauna.admin.factory_reset`` — the real s6 supervisor restarts the nest into
   the boot-time wipe (``helpers.factory_reset_and_reclaim_docker``) — and the
   same identity claims it again;
5. steps 2 and 3 again: the calendar loads, takes the new event and serves it
   back, and the event made before the reset is gone with the wipe.

The image under test is this tree's keyed tag unless ``FAUNA_NEST_TEST_IMAGE``
names a released one (``helpers.docker_build``).
"""

import subprocess
import uuid

import pytest

import fauna_ffi
from common.auth import claim_admin, mark_tls_nest, unmark_tls_nest

from .helpers import (
    docker_build,
    factory_reset_and_reclaim_docker,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    scanner_wired_network,
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
CLAIM_CODE = "RCAL2"   # pinned so the post-reset re-claim reuses it


@pytest.fixture()
def reclaim_nest():
    """A fresh claimed nest running as the Docker image, primary domain
    registered, on a scanner-wired network so mail can be turned on."""
    docker_build(get_repo_root())

    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-reclaim-cal-appless-{http_port}"
    with scanner_wired_network("fauna-reclaim-cal-appless") as (network, scan_env):
        start_container_with_ports(
            name,
            {443: http_port, **mail_ports},
            env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000", **scan_env},
            network=network,
        )
        try:
            wait_for_health(http_port, name)
            # The image's listener is HTTPS-only; every port-keyed
            # `common.auth` dial reads its scheme from this registration.
            mark_tls_nest(http_port)
            url = f"https://127.0.0.1:{http_port}"
            admin = claim_admin(http_port, CLAIM_CODE, base_url=url, handle="admin")
            nest = {"name": name, "port": http_port, "url": url, "admin": admin}
            register_primary_domain(nest, DOMAIN)
            yield nest
        finally:
            # A port outlives its nest (`find_free_port` hands it out again),
            # so the https posture must not.
            unmark_tls_nest(http_port)
            remove_container(name)


def _nest_error_lines(name: str, n: int = 25) -> str:
    """The container's error lines (its RPC refusals, ERRORs, panics and account
    lines, s6-multiplexed), the last ``n`` — what a refusal the harness only
    sees as a generic error was on the image's side (convention 6)."""
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True,
                          timeout=30)
    lines = [ln for ln in (logs.stdout + logs.stderr).splitlines()
             if any(w in ln for w in ("nest rpc error", "ERROR", "panicked", "account"))]
    return "\n".join(lines[-n:])


def _mail_on_and_make_event(nest, *, phase: str) -> tuple[str, dict]:
    seed = bytes(nest["admin"]["signing_key"])
    uid = f"{phase}-{uuid.uuid4().hex[:10]}@e2e.test"
    summary = f"{phase} supper"
    try:
        fauna_ffi.harness_enable_mail(nest["url"], seed)
        made = fauna_ffi.harness_calendar_make_event(
            nest["url"], seed, uid=uid, summary=summary,
        )
    except RuntimeError as e:
        raise AssertionError(
            f"{phase}: turning mail on or making the event was refused: {e}\n"
            f"── the container's error lines ──\n{_nest_error_lines(nest['name'])}"
        ) from e
    assert made["calendars"] >= 1, (
        f"{phase}: the calendar load must list the Personal calendar (the lazy "
        f"provision a fresh or re-claimed account gets); it listed "
        f"{made['calendars']} — the live `CalendarsLoaded: 0` symptom: {made!r}"
    )
    assert made["events"].get(uid) == summary, (
        f"{phase}: the event just made must be served back from the nest's "
        f"Personal calendar under the custody's key: {made!r}"
    )
    return uid, made


@pytest.mark.feature("factory-reset")
def test_a_reset_and_reclaimed_image_serves_the_calendar_again(reclaim_nest):
    nest = reclaim_nest

    # ── Before the reset: the baseline, so the reset is the only variable. ──
    before_uid, _ = _mail_on_and_make_event(nest, phase="before")

    # ── The real s6 factory reset, then the same identity claims it again. ──
    factory_reset_and_reclaim_docker(nest, claim_code=CLAIM_CODE)
    # The wipe cleared the local domains; the admin re-adds the primary one.
    register_primary_domain(nest, DOMAIN)

    # ── After: mail on again, the calendar loads, takes and serves an event. ──
    _, made = _mail_on_and_make_event(nest, phase="after")
    assert before_uid not in made["events"], (
        f"the event made before the reset must be gone with the wipe — a "
        f"calendar still serving it was never reset: {made!r}"
    )
