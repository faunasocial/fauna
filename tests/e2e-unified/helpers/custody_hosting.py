"""Planting custody-hosting rows on a real nest over the host's own door.

A hosting row is what ``fauna.custody.hosting.register`` deposits: a host
account asks its nest to hold sealed copies for an owner, presenting the
owner-signed ``CustodyGrant`` witness that names THIS nest's identity as the
custodian (``bins/fauna-nest/src/custody_hosting_handlers.rs``, the register
door; ``docs/goal/architecture/account-replica-posture.md`` § The custody grant
+ ceremony). The witness is built by the harness's
``fauna_ffi.sign_custody_witness``, so a test plants a row without the
two-account MLS ceremony an app drives — the shape the nest-side suites and
the admin-page journeys share.

Lives in ``helpers/`` because four suites plant rows: the pump's
(``tests/api/test_custody_hosting_pump.py``), the serve door's
(``tests/api/test_custody_hosting_serve.py``), the register door's bounds
(``tests/api/test_custody_hosting_register_bounds.py``) and the admin page's
(``tests/test_admin_custody_hosting.py``).

:class:`FleetDevice` is the other side of the serve door: one of the owner's
own devices reading a held copy back from the custodian nest — the
custodied-fleet handshake through the harness
(``fauna_ffi.harness_custodied_fleet_handshake``), then the bearer held on a
connection of its own, so a test can keep a session open across a removal or
a revocation and watch its NEXT request refuse.
"""

from __future__ import annotations

import os
import time

import requests
import websocket  # type: ignore[import-untyped]  # from `websocket-client`
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

import fauna_ffi
from clients._ws_rpc_core import _WsRpcClientBase
from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

#: A far-future witness expiry (microseconds) — the window never closes.
FAR_FUTURE_US = 9_000_000_000_000_000
#: The capability row's window (seconds) — the ceremony's 90-day default.
WINDOW_SECS = 90 * 24 * 3600
#: An origin on a reserved domain: the nest-scoped dial policy admits it
#: (https, origin only — plaintext loopback is a device-scoped carve-out a
#: public nest refuses), and nothing ever answers there. For a row that is
#: only ever listed, never pulled.
UNANSWERED_OWNER_NEST_URL = "https://owner.example"


def nest_id(url: str) -> bytes:
    """The nest's identity — the ``custodian_key`` a witness must name."""
    with WsRpcAnonClient(url) as anon:
        raw = anon.call("fauna.nest.info", {})["nest_id"]
    return bytes(raw) if not isinstance(raw, str) else bytes.fromhex(raw)


def hosting_deposit(owner_secret: bytes, owner_hex: str, grant_id: bytes, custodian: bytes, *,
                    owner_nest_url: str, cap: int = 0, stopped: bool = False,
                    expires_at_us: int = FAR_FUTURE_US,
                    removed_devices: tuple[bytes, ...] = ()) -> dict:
    """The ``fauna.custody.hosting.register`` request for one row, its witness
    signed by the owner, naming ``custodian`` and any ``removed_devices``."""
    return {
        "grant_id": grant_id,
        "owner_actor_id": owner_hex,
        "witness": fauna_ffi.sign_custody_witness(
            owner_secret, grant_id, custodian, expires_at_us, removed_devices),
        "owner_nest_url": owner_nest_url,
        "owner_devices": b"",
        "retained_bytes_cap": cap,
        "stopped": stopped,
    }


class Host:
    """A host account's hosting doors, over one connection. Every row it
    planted and did not remove is removed on exit, so a shared nest is left as
    it was found.

    ``owner_nest_url`` is where the pump would dial the owner; it defaults to
    the nest itself (owner and host on one private nest — the pump suite's
    shape), and a row that is never pulled names
    :data:`UNANSWERED_OWNER_NEST_URL` instead."""

    def __init__(self, nest, actor, *, owner_nest_url: str | None = None):
        self.nest = nest
        self.actor = actor
        self.owner_nest_url = owner_nest_url or nest["url"]
        self.custodian = nest_id(nest["url"])
        self.ws = WsRpcAdminClient(
            nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]))
        self.grants: list[bytes] = []

    def __enter__(self):
        self.ws.__enter__()
        return self

    def __exit__(self, *exc):
        try:
            for g in self.grants:
                self.ws.call("fauna.custody.hosting.remove", {"grant_id": g})
        finally:
            self.ws.__exit__(*exc)

    def custody(self, owner, *, grant_id: bytes | None = None, cap: int = 0,
                expires_at_us: int = FAR_FUTURE_US) -> bytes:
        """The owner mints the custody on their nest; this host deposits it."""
        g = grant_id or os.urandom(16)
        owner_secret = bytes(owner["signing_key"])
        now = int(time.time())
        reply = fauna_ffi.harness_mint_custody(
            self.nest["url"], owner_secret, g, self.custodian, now, now + WINDOW_SECS)
        assert reply.get("ok") is True, reply
        self.grants.append(g)
        self.register(owner, g, cap=cap, expires_at_us=expires_at_us)
        return g

    def plant(self, owner, *, grant_id: bytes | None = None) -> bytes:
        """Deposit a row with no capability minted behind it — enough for
        every surface that LISTS rows (the admin registry), never for a pull."""
        g = grant_id or os.urandom(16)
        reply = self.register(owner, g)
        assert reply.get("ok") is True, reply
        self.grants.append(g)
        return g

    def register(self, owner, g: bytes, *, cap: int = 0, stopped: bool = False,
                 expires_at_us: int = FAR_FUTURE_US,
                 removed_devices: tuple[bytes, ...] = ()) -> dict:
        return self.ws.call("fauna.custody.hosting.register", hosting_deposit(
            bytes(owner["signing_key"]), owner["actor_id_hex"], g, self.custodian,
            owner_nest_url=self.owner_nest_url, cap=cap, stopped=stopped,
            expires_at_us=expires_at_us, removed_devices=removed_devices))

    def rows(self) -> dict[bytes, dict]:
        return {bytes(r["grant_id"]): r
                for r in self.ws.call("fauna.custody.hosting.list", {})["rows"]}

    def remove(self, g: bytes) -> dict:
        self.grants.remove(g)
        return self.ws.call("fauna.custody.hosting.remove", {"grant_id": g})

    def forget(self, g: bytes) -> None:
        """Drop ``g`` from the rows this host removes on exit — someone else
        (an admin) removed it."""
        self.grants.remove(g)


#: The account-state plane a custody copies (`ACCOUNT_STATE_SCOPE`) and its
#: feed's item class (`ItemClass::StateEntry`).
STATE_SCOPE = "state"
STATE_FEED = "state-entry"


class FleetDevice(_WsRpcClientBase):
    """One of the owner's devices at the CUSTODIAN nest: the custodied-fleet
    handshake once, at :meth:`connect`, then every call over that one bearer.

    The handshake is the shared Rust mint (the harness signs the device's
    root-signed cert — the owner's, or ``cert_signer``'s for a device another
    account vouches for); a refusal raises ``RpcCallError`` from
    :meth:`connect`. Re-dialing after an idle gap presents the SAME bearer, so
    the session is the bearer — exactly what a removal or a revocation must cut
    at the next request."""

    def __init__(self, nest_url: str, owner, device_secret: bytes, *,
                 cert_signer: bytes | None = None):
        super().__init__(nest_url)
        self.owner = owner
        self.device_secret = device_secret
        self.cert_signer = cert_signer
        self.key = FleetDevice.key_of(device_secret)
        self.token: str | None = None

    def connect(self) -> "FleetDevice":
        self.token = fauna_ffi.harness_custodied_fleet_handshake(
            self._base_url, bytes(self.owner["signing_key"]), self.device_secret,
            self.cert_signer)
        self.ensure_open()
        return self

    def _connect(self) -> None:
        assert self.token is not None, "connect() first — the handshake mints the bearer"
        self._ws = websocket.create_connection(
            f"{self._ws_base()}/api/v1/ws/{self.key.hex()}",
            subprotocols=["fauna.v1", f"bearer.{self.token}"],
            timeout=self._http_timeout,
            sslopt=self._ssl_opt(),
        )

    def state_rows(self, scope: str = STATE_SCOPE) -> list[dict]:
        """One page of the owner's account-state feed for ``scope`` (``state``,
        or the fleet-only ``state-fleet``), as the custodian nest serves it —
        the rows verbatim, the sealed ``entry`` inline. Raises
        ``RpcCallError`` on a refusal."""
        reply = self.call("fauna.sync.changes.list", {
            "item_class": STATE_FEED, "scope": scope, "frontier": {}})
        return reply["changes"]


    @staticmethod
    def key_of(device_secret: bytes) -> bytes:
        """The Ed25519 public key of a 32-byte device seed — the device's id at
        the custodian nest."""
        return ed25519.Ed25519PrivateKey.from_private_bytes(device_secret).public_key(
        ).public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def custody_hosting_hook(url: str, hook: str, body: dict | None = None) -> dict:
    """POST one of the hosting pump's test hooks (`run-now`, `hold`, `clock`)."""
    resp = requests.post(f"{url}/api/v1/test/custody_hosting/{hook}", json=body, timeout=120)
    assert resp.status_code == 200, f"custody_hosting/{hook} returned {resp.status_code}: {resp.text}"
    return resp.json()


def pump(nest) -> dict:
    """One pump pass on ``nest``; its report."""
    return custody_hosting_hook(nest["url"], "run-now")
