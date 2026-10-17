"""tier_3: the per-role, per-axis rules of bridge onboarding auto-approval, over
the real socket (``docs/goal/behavior/mail-bridge-lifecycle.md`` § Onboarding
auto-approval (the box's own bridges)).

The goal doc's rule: the mail-sending helper (MTA) approves itself only once
mail is on; the mail-and-calendar helper (MDA) once mail or any of calendar,
contacts or files is on; every other role never — and a revoked helper is never
resurrected by an enable. ``test_mail_bridge_approval.py`` proves the mail axis
for the two mail roles; this file proves the rest of the table:

- a rejected helper stays rejected once mail is on (it neither returns to the
  pending feed nor approves itself on its next poll);
- a calendar-, contacts- or files-only nest approves the MDA and holds the MTA;
- the Bluesky helper and the content processor wait whatever is switched on.

Each test runs on ``dedicated_no_mail_nest``, a fresh claimed nest on which no
axis was ever enabled, so the starting state is "everything unset" by
construction rather than by collection order. Each enroll uses a random pubkey —
enrollment only stores it, and a harness nest has no blessed-key registry, so no
proof-of-possession signature is needed.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.bridge_enrollment import enroll_bridge

pytestmark = pytest.mark.tier_3


def _admin(nest):
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _set(nest, kind: str, enabled: bool) -> None:
    with _admin(nest) as admin:
        assert admin.call(kind, {"enabled": enabled}).get("ok") is True, kind


def _ids(rows) -> set[str]:
    return {r["bridge_id"] for r in rows}


@pytest.mark.feature("admin-bridges")
def test_a_rejected_helper_stays_rejected_once_mail_is_on(dedicated_no_mail_nest):
    """admin-bridges 8: reject a pending MTA and MDA, switch mail on, and the
    next poll of each answers ``revoked`` — never ``pending`` and never
    ``approved`` — and neither row shows in the pending feed or the approved
    roster. The control is a third helper enrolled pending in the same window
    and NOT rejected: the same enable approves it on its poll, so a ``revoked``
    answer is the reject holding, not the enable failing to reach the poll."""
    nest = dedicated_no_mail_nest
    url = nest["url"]
    rejected = {}
    for role in ("mta", "mda"):
        pk = secrets.token_bytes(32)
        bridge_id = f"{role}-{secrets.token_hex(4)}"
        assert enroll_bridge(url, pk, role, bridge_id) == "pending"
        rejected[bridge_id] = (pk, role)
    control_pk = secrets.token_bytes(32)
    assert enroll_bridge(url, control_pk, "mta") == "pending"

    with _admin(nest) as admin:
        for pk, _role in rejected.values():
            admin.call("fauna.bridges.reject_pending_bridge", {"ed25519_pubkey": pk})

    _set(nest, "fauna.bridges.set_mail_enabled", True)

    assert enroll_bridge(url, control_pk, "mta") == "approved", (
        "control: a pending helper's poll must self-heal to approved once mail is on"
    )
    for bridge_id, (pk, role) in rejected.items():
        status = enroll_bridge(url, pk, role, bridge_id)
        assert status == "revoked", (
            f"{bridge_id}: a rejected helper's poll after mail-on must stay revoked, got {status!r}"
        )

    with _admin(nest) as admin:
        pending = _ids(admin.call("fauna.bridges.list_pending_bridges", {})["bridges"])
        approved = _ids(
            admin.call("fauna.bridges.list_service_users", {"status": "approved"})[
                "service_users"
            ]
        )
        revoked = _ids(
            admin.call("fauna.bridges.list_service_users", {"status": "revoked"})[
                "service_users"
            ]
        )
    for bridge_id in rejected:
        assert bridge_id not in pending, f"{bridge_id} must never return to the pending feed"
        assert bridge_id not in approved, f"{bridge_id} must never approve itself"
        assert bridge_id in revoked, f"{bridge_id} must still be listed revoked"


@pytest.mark.feature("admin-bridges")
@pytest.mark.parametrize(
    "axis",
    [
        "fauna.bridges.set_caldav_enabled",
        "fauna.bridges.set_carddav_enabled",
        "fauna.bridges.set_webdav_enabled",
    ],
)
def test_a_dav_only_nest_approves_the_mda_and_holds_the_mta(dedicated_no_mail_nest, axis):
    """admin-bridges 10: with mail never switched on, switching on ONE of
    calendar, contacts or files makes a fresh mail-and-calendar helper (MDA)
    approve itself at its first enrol and never show as pending, while a fresh
    mail-sending helper (MTA) waits. Before the switch the same MDA enrol lands
    pending, so the approval is the switch's doing."""
    nest = dedicated_no_mail_nest
    url = nest["url"]
    before = secrets.token_bytes(32)
    assert enroll_bridge(url, before, "mda") == "pending", "nothing on → the MDA waits"

    _set(nest, axis, True)

    mda_id = f"mda-{secrets.token_hex(4)}"
    mta_id = f"mta-{secrets.token_hex(4)}"
    assert enroll_bridge(url, secrets.token_bytes(32), "mda", mda_id) == "approved", (
        f"{axis} on → the MDA approves itself"
    )
    assert enroll_bridge(url, secrets.token_bytes(32), "mta", mta_id) == "pending", (
        f"{axis} on, mail off → the MTA waits for mail"
    )
    with _admin(nest) as admin:
        pending = _ids(admin.call("fauna.bridges.list_pending_bridges", {})["bridges"])
    assert mda_id not in pending, "an auto-approved MDA never shows as pending"
    assert mta_id in pending, "the waiting MTA shows as a pending card"


@pytest.mark.feature("admin-bridges")
def test_non_mail_helpers_always_wait_for_the_admin(dedicated_no_mail_nest):
    """admin-bridges 11: the Bluesky helper (``atproto.pds``) and the content
    processor land pending with nothing switched on AND with mail, calendar,
    contacts and files all switched on — and their re-poll under everything-on
    still answers pending. The MDA enrolled in the everything-on window is the
    control: it approves itself there, so the window really is open."""
    nest = dedicated_no_mail_nest
    url = nest["url"]
    roles = ("atproto.pds", "content-processor")

    off = {role: secrets.token_bytes(32) for role in roles}
    for role, pk in off.items():
        assert enroll_bridge(url, pk, role) == "pending", f"{role}: nothing on → pending"

    for kind in (
        "fauna.bridges.set_mail_enabled",
        "fauna.bridges.set_caldav_enabled",
        "fauna.bridges.set_carddav_enabled",
        "fauna.bridges.set_webdav_enabled",
    ):
        _set(nest, kind, True)

    assert enroll_bridge(url, secrets.token_bytes(32), "mda") == "approved", (
        "control: with every axis on, the MDA approves itself"
    )
    for role, pk in off.items():
        assert enroll_bridge(url, pk, role) == "pending", (
            f"{role}: an existing pending row must not self-heal under any axis"
        )
        assert enroll_bridge(url, secrets.token_bytes(32), role) == "pending", (
            f"{role}: a fresh enrol with every axis on must still wait for the admin"
        )
