"""Bridge enrollment for the harness, over the product's own WS-RPC kinds.

A bridge enrolls itself: it announces its Ed25519 key over the anonymous
pre-identity WS (`fauna.bridges.request_enrollment`, loopback-gated at the
dispatcher), and an admin approves the pending row
(`fauna.bridges.approve_pending_bridge`, Admin-class) —
`docs/goal/behavior/mail-bridge-lifecycle.md` § Cold boot / § Pending approval.
A fixture that needs a bridge approved BEFORE its process starts makes the same
two calls on the bridge's behalf; there is no other enrollment surface.

Deliberately free of `conftest` imports, so the lean helpers that avoid loading
the big conftest (`windows_caldav_nest.py`) can use it too.
"""

from __future__ import annotations

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient


def enroll_bridge(nest_url: str, ed25519_pubkey: bytes, role: str, bridge_id: str = "") -> str:
    """Create (or poll) the enrollment row for `ed25519_pubkey` exactly as the
    bridge's own cold boot does, and return its status (`pending`, `approved`,
    or `revoked`). An empty `bridge_id` lets nest synthesize
    `<role>-<pubkey-prefix>`. The harness reaches its nest on 127.0.0.1, so the
    loopback gate passes; a nest with no blessed registry for the role (every
    harness nest) admits the call without a proof-of-possession signature."""
    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call(
            "fauna.bridges.request_enrollment",
            {"ed25519_pubkey": bytes(ed25519_pubkey), "role_hint": role, "bridge_id": bridge_id},
        )
    return reply["status"]


def approve_bridge(nest_url: str, admin_signing_key, ed25519_pubkey: bytes, role: str) -> None:
    """Approve an enrolled bridge as the admin. Idempotent on an approved row;
    nest refuses a role that differs from the enrolled one. `admin_signing_key`
    is the admin's PyNaCl `SigningKey` (`nest["admin"]["signing_key"]`)."""
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin_signing_key.verify_key),
        signing_key=bytes(admin_signing_key),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.approve_pending_bridge",
            {"ed25519_pubkey": bytes(ed25519_pubkey), "role": role},
        )


def enroll_and_approve_bridge(
    nest_url: str, admin_signing_key, ed25519_pubkey: bytes, role: str, bridge_id: str = ""
) -> None:
    """Pre-approve a bridge before its process starts: enroll its key, then
    approve it. Approval also inserts the bridge's audit-only `users` row, so
    its challenge-response auth resolves on first connect. A nest with the
    role's enable axis on auto-approves at enrollment; the approve is then a
    no-op."""
    enroll_bridge(nest_url, ed25519_pubkey, role, bridge_id)
    approve_bridge(nest_url, admin_signing_key, ed25519_pubkey, role)


def revoke_bridge(nest_url: str, admin_signing_key, ed25519_pubkey: bytes) -> None:
    """Revoke an enrolled bridge as the admin, over the app's own rotate kind
    (`fauna.bridges.revoke_service_user`). Idempotent on an already-revoked
    row; the row stays, `revoked`, so the key can never re-pend."""
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin_signing_key.verify_key),
        signing_key=bytes(admin_signing_key),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.revoke_service_user", {"bridge_actor_id": bytes(ed25519_pubkey)}
        )


class PlantedBridges:
    """Approved bridge rows a test plants on a SHARED nest with no bridge
    process behind them, revoked again by :meth:`retire_all` (the
    `planted_bridges` fixture's teardown).

    An approved `mta`/`mda` row that never connects is not inert: the mail
    health readout counts every approved mail helper and reads one with no live
    socket as down (`fauna_mail::health`, "N of M connected"), so a row left
    behind on the session nest turns a later test's healthy readout into
    `bridge_down`."""

    def __init__(self, nest_url: str, admin_signing_key):
        self.nest_url = nest_url
        self._admin_sk = admin_signing_key
        self._planted: list[bytes] = []

    def plant(self, ed25519_pubkey: bytes, role: str, bridge_id: str = "") -> None:
        """Enroll and approve `ed25519_pubkey` (`enroll_and_approve_bridge`),
        recorded for retirement."""
        self._planted.append(bytes(ed25519_pubkey))
        enroll_and_approve_bridge(self.nest_url, self._admin_sk, ed25519_pubkey, role, bridge_id)

    def retire_all(self) -> None:
        """Revoke every planted row; a row the test itself already revoked (a
        rotate) is a no-op."""
        while self._planted:
            revoke_bridge(self.nest_url, self._admin_sk, self._planted.pop())
