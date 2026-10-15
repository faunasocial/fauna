"""tier_3 API: ending sign-ins, the 24-hour emergency lock, and what a recovery
does to both — witnessed at the wire, with no app in the loop.

The nest outcomes of three catalog pages that no test asserted until now:

* ``docs/features/sessions.md`` **10–12** (ending a sign-in closes its
  connections at once, leaves the others working, and reaches only your own),
  **17–20** (a lock lasts exactly 24 hours, ends every sign-in and connection,
  refuses every sign-in with the time it ends, and leaves nothing still
  connected able to act) and **27** (a lock request more than five minutes old
  is refused);
* ``docs/features/devices.md`` **16** (removing a device ends every sign-in it
  made);
* ``docs/features/take-your-account-back.md`` **26, 27** (the successor is not
  locked; the stolen key can no longer lock it and is told where it went).

Goal docs: ``docs/goal/behavior/devices.md`` § What a session is, and what
revoking one does, § Emergency lockout; ``docs/goal/architecture/
transport-connection.md`` § Connection lifecycle; ``docs/goal/behavior/
login.md`` § Silent Challenge; ``docs/goal/behavior/identity-succession.md``
§ Enforcement on the home nest.

Latency-independent by construction (convention 14): every mutation is a request
with a reply, and every "closed" is the close frame itself, read off the held
socket — never a sleep. The one clock comparison (17) brackets the nest's
``locked_until`` between two readings of the shared wall clock taken around the
request, so it holds however long the request takes.

Every actor here is a dedicated, non-admin account: a lock is account-wide for a
day and admins are exempt from it (``login.md`` § Silent Challenge), so the
shared test user can never be the subject.
"""

from __future__ import annotations

import os
import struct
import time

import pytest
import websocket
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient, _BearerToken
from common.auth import create_actor_and_register, mint_token_via_handshake
from helpers.succession import emergency_lockout, register_recovery_kit, succeed_identity

pytestmark = pytest.mark.tier_3

#: `transport-connection.md` § Revocation teardown — the close a socket gets
#: when the authority it was upgraded with is withdrawn.
CLOSE_AUTH_REVOKED = 4401
#: `account_core::EMERGENCY_LOCKOUT_SECS` — `devices.md` § Emergency lockout.
LOCK_SECS = 24 * 3600
#: `account_core::LOCKOUT_TIMESTAMP_WINDOW_SECS`.
LOCK_REQUEST_WINDOW_SECS = 300
#: Room for a nest whose clock is not this process's (a live or docker nest).
#: Far below an hour, so a lock of any other length still fails the bracket.
CLOCK_SLACK_S = 60

SESSIONS_LIST = "fauna.sessions.list"
SESSIONS_REVOKE = "fauna.sessions.revoke"
SESSIONS_LOCKOUT = "fauna.sessions.lockout"


# ── harness ──────────────────────────────────────────────────────────────────


class _HeldBearer(WsRpcAdminClient):
    """One connection upgraded with ONE given bearer, never re-minted.

    The plain client re-mints and re-dials on a refused upgrade, which is
    exactly what an outcome about a bearer that stopped working must not do: a
    reconnect here re-presents the same bearer, so a withdrawn one surfaces as
    the refusal it is.
    """

    def __init__(self, nest_url: str, actor_id_hex: str, token: str):
        super().__init__(nest_url, bytes.fromhex(actor_id_hex), bytes(32))
        self._held = token

    def _acquire_bearer(self) -> _BearerToken:
        return _BearerToken(token=self._held, expires_at=time.time() + 3600)

    def _mint_bearer(self) -> _BearerToken:  # pragma: no cover - a harness guard
        raise AssertionError("a held-bearer connection never mints a bearer of its own")


def _fresh_actor(nest) -> dict:
    """A registered non-admin account no other test shares."""
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


def _client(nest, actor) -> WsRpcAdminClient:
    """The account's own app connection, signed in over challenge/verify."""
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def _sign_in(nest, actor) -> _BearerToken:
    """One more sign-in of ``actor`` over challenge/verify — the path every app
    signs in through. Returns the bearer and its session id."""
    with _client(nest, actor) as minter:
        return minter.mint_bearer()


def _held(nest, actor, bearer: _BearerToken) -> _HeldBearer:
    return _HeldBearer(nest["url"], actor["actor_id_hex"], bearer.token)


def _listed_ids(client: WsRpcAdminClient) -> set[str]:
    return {s["token_id"] for s in client.call(SESSIONS_LIST, {})["sessions"]}


def _close_code(client: WsRpcAdminClient) -> int:
    """Read the held socket until the nest closes it; its close code.

    Anything the nest sends first (a Push) is read past. Bounded by the
    socket's own read timeout, a ceiling a healthy nest never comes near.
    """
    raw = client._ws
    assert raw is not None, "the connection was never open"
    while True:
        opcode, data = raw.recv_data(control_frame=True)
        if opcode == websocket.ABNF.OPCODE_CLOSE:
            return struct.unpack("!H", data[:2])[0] if len(data) >= 2 else 1005


def _upgrade_status(nest, actor_id_hex: str, token: str) -> int:
    """The HTTP status a fresh connection upgraded with ``token`` meets
    (101 when it opens)."""
    try:
        with _HeldBearer(nest["url"], actor_id_hex, token):
            return 101
    except websocket.WebSocketBadStatusException as refused:
        return refused.status_code


def _refusal(call) -> RpcCallError:
    with pytest.raises(RpcCallError) as refused:
        call()
    return refused.value


def _assert_still_this_socket(client: WsRpcAdminClient, raw, what: str) -> None:
    assert client._ws is raw, (
        f"{what}: the connection was replaced — the nest closed the one it had, "
        "and the harness re-dialed behind the call"
    )


# ── Sessions page, outcomes 10, 11: ending one sign-in ───────────────────────


@pytest.mark.feature("sessions")
def test_ending_a_sign_in_closes_its_connections_at_once_and_leaves_the_others_working(
    nest_instance,
):
    """Sessions page, outcomes 10 + 11 (`devices.md` § What a session is, and what revoking
    one does; `transport-connection.md` § Connection lifecycle → the per-token
    twin of the revocation teardown)."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    doomed, kept = _sign_in(nest, actor), _sign_in(nest, actor)

    with _client(nest, actor) as app, _held(nest, actor, doomed) as d, _held(
        nest, actor, kept
    ) as k:
        assert {doomed.token_id, kept.token_id} <= _listed_ids(d), (
            "precondition: both extra sign-ins are live sessions of the account"
        )
        app_raw, kept_raw = app._ws, k._ws

        assert app.call(SESSIONS_REVOKE, {"token_id": doomed.token_id})["ok"] is True

        # 10 — the connection that sign-in held closes, at once, by itself.
        assert _close_code(d) == CLOSE_AUTH_REVOKED, (
            "ending a sign-in must close the connection it already held (4401) — "
            "the bearer is checked once, at the upgrade, so without the teardown "
            "it keeps dispatching as the account"
        )
        assert _upgrade_status(nest, actor["actor_id_hex"], doomed.token) == 401, (
            "the ended sign-in must open no new connection either"
        )

        # 11 — every other sign-in is untouched: same sockets, still answering.
        listed = _listed_ids(k)
        _assert_still_this_socket(k, kept_raw, "the other sign-in")
        assert doomed.token_id not in listed and kept.token_id in listed, listed
        _listed_ids(app)
        _assert_still_this_socket(app, app_raw, "the app that ended the sign-in")


# ── Sessions page, outcome 12: only your own ─────────────────────────────────


@pytest.mark.feature("sessions")
def test_naming_someone_elses_sign_in_ends_nothing(nest_instance):
    """Sessions page, outcome 12 (`devices.md` § Session Management)."""
    nest = nest_instance
    intruder, victim = _fresh_actor(nest), _fresh_actor(nest)
    victims_sign_in = _sign_in(nest, victim)

    with _client(nest, intruder) as theirs, _held(nest, victim, victims_sign_in) as v:
        victim_raw = v._ws
        refused = _refusal(
            lambda: theirs.call(SESSIONS_REVOKE, {"token_id": victims_sign_in.token_id})
        )
        assert refused.code == "fauna.sessions.not_found", (
            "another account's session id must read as no such session — never "
            f"ended, and never confirmed to exist; got {refused.code}"
        )
        assert victims_sign_in.token_id in _listed_ids(v), (
            "the victim's sign-in must still be listed"
        )
        _assert_still_this_socket(v, victim_raw, "the victim's sign-in")


# ── Sessions page, outcomes 17, 18: the lock from a signed-in app ────────────


@pytest.mark.feature("sessions")
def test_a_lock_lasts_exactly_24_hours_whatever_the_request_asks(nest_instance):
    """Sessions page, outcome 17 (`devices.md` § Emergency lockout: a hard-coded 24 hours,
    and neither request carries a duration).

    Both lock kinds, each asked by a request that names a shorter window: the
    signed-in kind with a stray ``duration_secs`` (accepted and ignored, or
    refused as malformed — either way it cannot shorten anything), and the
    signed-out kind, whose signed bytes have no room for a duration at all.
    """
    nest = nest_instance

    signed_in = _fresh_actor(nest)
    with _client(nest, signed_in) as app:
        before = int(time.time())
        try:
            locked_until = app.call(SESSIONS_LOCKOUT, {"duration_secs": 60})["locked_until"]
        except RpcCallError as refused:
            assert refused.code == "fauna.protocol.malformed", refused
            before = int(time.time())
            locked_until = app.call(SESSIONS_LOCKOUT, {})["locked_until"]
        after = int(time.time())
    _assert_24_hours(locked_until, before, after, "the signed-in lock")

    signed_out = _fresh_actor(nest)
    before = int(time.time())
    locked_until = emergency_lockout(
        nest["url"],
        actor_id_hex=signed_out["actor_id_hex"],
        identity_seed_hex=bytes(signed_out["signing_key"]).hex(),
    )
    after = int(time.time())
    _assert_24_hours(locked_until, before, after, "the signed-out lock")


def _assert_24_hours(locked_until: int, before: int, after: int, what: str) -> None:
    low, high = before + LOCK_SECS - CLOCK_SLACK_S, after + 1 + LOCK_SECS + CLOCK_SLACK_S
    assert low <= locked_until <= high, (
        f"{what} must end exactly 24 hours after it was set: locked_until="
        f"{locked_until}, request sent at {before}..{after} → expected "
        f"{before + LOCK_SECS}..{after + LOCK_SECS} (±{CLOCK_SLACK_S}s clock slack)"
    )


@pytest.mark.feature("sessions")
def test_locking_ends_every_sign_in_and_closes_every_connection_the_pressers_included(
    nest_instance,
):
    """Sessions page, outcome 18 (`devices.md` § Emergency lockout).

    The app that pressed the lock first hears that it landed — its Reply
    arrives before its own socket closes (`transport-connection.md`
    § Connection lifecycle) — and only then is the socket asserted gone.
    """
    nest = nest_instance
    actor = _fresh_actor(nest)
    other = _sign_in(nest, actor)

    with _client(nest, actor) as presser, _held(nest, actor, other) as elsewhere:
        pressers_bearer = presser._acquire_bearer()
        reply = presser.call(SESSIONS_LOCKOUT, {})
        assert reply["ok"] is True and reply["locked_until"] > time.time(), reply

        assert _close_code(elsewhere) == CLOSE_AUTH_REVOKED, (
            "every other connection of the account must close at once"
        )
        assert _close_code(presser) == CLOSE_AUTH_REVOKED, (
            "the presser's own connection must close too, once its Reply is out"
        )

    for bearer, what in ((pressers_bearer, "the presser's"), (other, "the other")):
        assert _upgrade_status(nest, actor["actor_id_hex"], bearer.token) == 401, (
            f"{what} sign-in must be ended by the lock: its bearer opens nothing"
        )


# ── Sessions page, outcomes 19, 20: while locked ─────────────────────────────


@pytest.mark.feature("sessions")
def test_while_locked_nobody_signs_in_and_the_refusal_says_when_the_lock_ends(
    nest_instance,
):
    """Sessions page, outcome 19 (`login.md` § Silent Challenge; `transport-connection.md`
    § Connection lifecycle) — on BOTH sign-in paths: the direct handshake and
    the challenge/verify every app signs in through. The owner holding the
    right key is refused like anyone else."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    locked_until = emergency_lockout(
        nest["url"],
        actor_id_hex=actor["actor_id_hex"],
        identity_seed_hex=bytes(actor["signing_key"]).hex(),
    )

    for path, attempt in (
        ("the direct handshake", lambda: mint_token_via_handshake(nest["url"], actor["signing_key"])),
        ("challenge/verify", lambda: _sign_in(nest, actor)),
    ):
        refused = _refusal(attempt)
        assert refused.code == "fauna.auth.account_locked", (
            f"{path} must refuse a locked account as locked; got {refused.code}"
        )
        assert refused.details == locked_until, (
            f"{path}'s refusal must say when the lock ends ({locked_until}); "
            f"got details={refused.details!r}"
        )


@pytest.mark.feature("sessions")
def test_while_locked_nothing_still_connected_can_act_and_no_new_connection_opens(
    nest_instance,
):
    """Sessions page, outcome 20 (`transport-connection.md` § Connection lifecycle).

    The lock comes from the signed-out door — the thief-proof one, pressed from
    a device that holds no session — while the account holds a live
    connection elsewhere."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    held = _sign_in(nest, actor)

    with _held(nest, actor, held) as connected:
        assert held.token_id in _listed_ids(connected), "precondition: it can act"
        emergency_lockout(
            nest["url"],
            actor_id_hex=actor["actor_id_hex"],
            identity_seed_hex=bytes(actor["signing_key"]).hex(),
        )
        assert _close_code(connected) == CLOSE_AUTH_REVOKED, (
            "the lock must close the connection the account still held — the "
            "next frame on it is the close, so nothing it sends is served"
        )
        # What a client does next — dial again on the bearer it holds — meets
        # the door, never a connection to send a request on.
        connected.close()
        with pytest.raises(websocket.WebSocketBadStatusException) as refused:
            connected.ensure_open()
        assert refused.value.status_code == 401, (
            "the locked account's bearer must open no new connection; got HTTP "
            f"{refused.value.status_code}"
        )


# ── Sessions page, outcome 27: a captured lock request goes stale ────────────


@pytest.mark.feature("sessions")
def test_a_lock_request_more_than_five_minutes_old_is_refused(nest_instance):
    """Sessions page, outcome 27 (`devices.md` § Emergency lockout): a captured, correctly
    signed lock request presented late locks nothing."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    stale = int(time.time()) - LOCK_REQUEST_WINDOW_SECS - CLOCK_SLACK_S

    refused = _refusal(
        lambda: emergency_lockout(
            nest["url"],
            actor_id_hex=actor["actor_id_hex"],
            identity_seed_hex=bytes(actor["signing_key"]).hex(),
            timestamp=stale,
        )
    )
    assert refused.code == "fauna.account.invalid_request", refused

    bearer = _sign_in(nest, actor)
    with _held(nest, actor, bearer) as app:
        assert bearer.token_id in _listed_ids(app), (
            "the stale request must have locked nothing: the account still signs "
            "in and acts"
        )


# ── devices 16: removing a device ────────────────────────────────────────────


def _granted_device(nest, actor) -> tuple[str, SigningKey]:
    """A device row on ``actor``'s account whose key holds the account's
    root-signed grant — what an enrolled app's device key is. Fixture setup."""
    from helpers.enrollment import register_granted_device

    key = SigningKey(os.urandom(32))
    device_id, _ = register_granted_device(
        nest["url"], actor, "e2e-removed-device", principal_seed=bytes(key)
    )
    return device_id, key


def _device_sign_in(nest, actor, device_key: SigningKey) -> dict:
    """``fauna.auth.device_handshake`` — a sign-in made by the device's own
    key, the way an enrolled app's account runtime makes it."""
    from helpers.enrollment import device_sign_in

    return device_sign_in(nest["url"], actor["actor_id_bytes"], device_key)


@pytest.mark.feature("devices")
def test_removing_a_device_ends_every_sign_in_it_made_for_good(nest_instance):
    """devices 16 (`devices.md` § What a session is, and what revoking one
    does): the device's sign-ins end and their connections close; the device
    key can never sign in again; the account's other sign-ins are untouched."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    device_id, device_key = _granted_device(nest, actor)
    device_pub_hex = bytes(device_key.verify_key).hex()
    first, second = _device_sign_in(nest, actor, device_key), _device_sign_in(
        nest, actor, device_key
    )

    with _client(nest, actor) as app, _held(nest, actor, _as_bearer(first)) as d1, _held(
        nest, actor, _as_bearer(second)
    ) as d2:
        minted_by = {
            s["token_id"]: s.get("minted_by_device") for s in app.call(SESSIONS_LIST, {})["sessions"]
        }
        assert minted_by.get(first["token_id"]) == device_pub_hex and minted_by.get(
            second["token_id"]
        ) == device_pub_hex, f"precondition: both sign-ins are the device's: {minted_by}"
        app_raw = app._ws

        assert app.call("fauna.sync.devices.delete", {"device_id": device_id})["deleted"] is True

        for held, what in ((d1, "first"), (d2, "second")):
            assert _close_code(held) == CLOSE_AUTH_REVOKED, (
                f"the device's {what} sign-in must close its connection at once"
            )
        for minted, what in ((first, "first"), (second, "second")):
            assert _upgrade_status(nest, actor["actor_id_hex"], minted["token"]) == 401, (
                f"the device's {what} sign-in must open nothing new"
            )

        refused = _refusal(lambda: _device_sign_in(nest, actor, device_key))
        assert refused.code == "fauna.auth.not_registered", (
            "the removed device's key must never sign in again — for good, not "
            f"until its grant is re-sent; got {refused.code}"
        )

        listed = _listed_ids(app)
        _assert_still_this_socket(app, app_raw, "the account's own app")
        assert not {first["token_id"], second["token_id"]} & listed, listed


def _as_bearer(reply: dict) -> _BearerToken:
    return _BearerToken(token=reply["token"], expires_at=time.time() + 3600, token_id=reply["token_id"])


# ── take-your-account-back 26, 27 ────────────────────────────────────────────


def _register_kit(nest, actor) -> str:
    """The owner's recovery kit, registered while they can still sign in."""
    return register_recovery_kit(
        nest["url"],
        actor_id_hex=actor["actor_id_hex"],
        identity_seed_hex=bytes(actor["signing_key"]).hex(),
    )


def _take_back(nest, actor, kit: str) -> tuple[str, SigningKey]:
    """Take the account back with ``kit``. Returns the successor's id (hex)
    and key."""
    seed_hex = bytes(actor["signing_key"]).hex()
    successor_key = SigningKey(os.urandom(32))
    successor_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=actor["actor_id_hex"],
        recovery_secret_hex=kit,
        successor_seed_hex=bytes(successor_key).hex(),
        old_seed_hex=seed_hex,
    )
    assert successor_id == bytes(successor_key.verify_key).hex(), successor_id
    return successor_id, successor_key


def _successor_acts(nest, successor_key: SigningKey) -> None:
    """The successor signs in — both paths — and acts."""
    mint_token_via_handshake(nest["url"], successor_key)
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(successor_key.verify_key), signing_key=bytes(successor_key)
    ) as successor:
        assert successor.call(SESSIONS_LIST, {})["sessions"], "the successor's own sign-in is listed"


@pytest.mark.feature("take-your-account-back")
def test_taking_the_account_back_lands_in_an_account_that_is_not_locked(nest_instance):
    """take-your-account-back 26 (`devices.md` § The two panic buttons): the
    thief locked the account; the recovery lands anyway, and the account it
    lands in is not locked."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    kit = _register_kit(nest, actor)
    emergency_lockout(
        nest["url"],
        actor_id_hex=actor["actor_id_hex"],
        identity_seed_hex=bytes(actor["signing_key"]).hex(),
    )
    assert _refusal(lambda: _sign_in(nest, actor)).code == "fauna.auth.account_locked", (
        "precondition: the old account really is locked"
    )

    _, successor_key = _take_back(nest, actor, kit)
    _successor_acts(nest, successor_key)


@pytest.mark.feature("take-your-account-back")
def test_after_taking_it_back_the_stolen_key_cannot_lock_the_account_and_is_told_where_it_went(
    nest_instance,
):
    """take-your-account-back 27 (`identity-succession.md` § Enforcement on the
    home nest: the lockout kind refuses the old key with ``superseded`` naming
    the successor)."""
    nest = nest_instance
    actor = _fresh_actor(nest)
    successor_id, successor_key = _take_back(nest, actor, _register_kit(nest, actor))

    refused = _refusal(
        lambda: emergency_lockout(
            nest["url"],
            actor_id_hex=actor["actor_id_hex"],
            identity_seed_hex=bytes(actor["signing_key"]).hex(),
        )
    )
    assert refused.code == "fauna.auth.superseded", refused
    assert (refused.details or {}).get("new_actor_id_hex") == successor_id, (
        f"the refusal must name where the account went ({successor_id}); "
        f"got details={refused.details!r}"
    )
    _successor_acts(nest, successor_key)
