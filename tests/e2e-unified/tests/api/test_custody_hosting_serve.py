"""What a nest holds for people elsewhere, handed back — only sealed, only to
the owner's own devices, and no longer once the owner takes the permission
back — on a real ``fauna-nest`` binary, through the doors the owner's devices
ride.

Owner doc: ``docs/goal/architecture/account-replica-posture.md`` § Replica
posture → *The custody grant + ceremony*, the *custodian-NEST serve door*
bullet: the door answers from the keyless pair store the hosting pump fills
(ruling 1), admits only a device the owner's root signed whose key no grant
names as removed (rulings 2–4, one opaque ``fauna.auth.not_registered`` at the
handshake, the class re-derived on every request), and learns of a revocation
through the pump — an authenticated refusal at the owner nest's custody
handshake marks the row, and the door refuses a marked row (ruling 5).

The rig is ``test_custody_hosting_pump.py``'s: owner and host are two accounts
of ONE private nest (``domainless_nest``), the owner mints the custody through
``fauna_ffi.harness_mint_custody``, the host deposits over
``fauna.custody.hosting.register``, and the pump is driven only through its
test hooks. The owner's device is ``helpers.custody_hosting.FleetDevice``: the
custodied-fleet handshake through the shared Rust mint, then its bearer held
on a connection of its own. Every assertion is on state (convention 14).

Ports the api-tier half of ``bins/fauna-nest/tests/
conformance_custody_nest_serve_client.rs`` onto the binary, plus the serving
half of revocation, which only the pump's refusal mark makes reachable.
"""

import os
import secrets
import sqlite3

import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.custody_hosting import FleetDevice, Host, custody_hosting_hook, pump

pytestmark = pytest.mark.tier_3

#: A delegable account-state kind the owner can seal a row of from the harness
#: (`fauna_ffi.account_state_seal_delegable_row`) — the pump copies the whole
#: `state` plane, whatever the kind.
KIND = "fauna.state.personalization"

#: The handshake's one opaque refusal (ruling 2).
NOT_REGISTERED = "fauna.auth.not_registered"
#: What a live session's next request answers once its device stops being one
#: the door serves: the class re-derives to none on that dispatch.
SESSION_REFUSED = "fauna.bridges.permission_denied"


@pytest.fixture(scope="module")
def serve_nest(domainless_nest):
    """``domainless_nest`` with its periodic pump held for the module, so the
    only passes are this file's own ``run-now`` pokes."""
    custody_hosting_hook(domainless_nest["url"], "hold", {"held": True})
    yield domainless_nest
    custody_hosting_hook(domainless_nest["url"], "hold", {"held": False})


def _account(nest) -> dict:
    return create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])


def _seal_and_put(nest, owner, marker: bytes) -> bytes:
    """The owner seals one account-state row carrying ``marker`` and puts it on
    its nest. Returns the sealed envelope, verbatim."""
    writer = secrets.token_bytes(32)
    writer_key = FleetDevice.key_of(writer)
    item_key, envelope = fauna_ffi.account_state_seal_delegable_row(
        bytes(owner["signing_key"]), writer, KIND, "topic:" + secrets.token_hex(16), marker, 1)
    with WsRpcAdminClient(nest["url"], actor_id=owner["actor_id_bytes"],
                          signing_key=bytes(owner["signing_key"])) as ws:
        ws.call("fauna.account.state.put", {
            "scope": "state", "writer_id": writer_key.hex(), "writer_seq": 1,
            "item_key": item_key, "op": "state-put", "entry": envelope})
    return envelope


def _pulled(nest, host: Host) -> None:
    report = pump(nest)
    assert report["pulled"] == len(host.grants) and report["failed"] == 0, (
        f"every row of the pass must pull: {report!r}")


def _refused(call) -> str:
    with pytest.raises(RpcCallError) as refused:
        call()
    return refused.value.code


def _files(root: str):
    for dirpath, _, names in os.walk(root):
        for name in names:
            path = os.path.join(dirpath, name)
            with open(path, "rb") as f:
                yield path, f.read()


@pytest.mark.feature("admin-held-custody")
def test_a_held_copy_comes_back_sealed_and_byte_identical_and_the_nest_never_held_a_key(
        serve_nest):
    """The owner's device reads the copy back byte for byte, and the readable
    text sealed inside it is nowhere on the nest's disk — not in the pair store
    the copy lives in, which does hold the sealed bytes, and not anywhere else
    the nest writes."""
    owner = _account(serve_nest)
    marker = b"held-copy-plaintext-" + secrets.token_hex(16).encode()
    envelope = _seal_and_put(serve_nest, owner, marker)
    with Host(serve_nest, _account(serve_nest)) as host:
        host.custody(owner)
        _pulled(serve_nest, host)

        with FleetDevice(serve_nest["url"], owner, secrets.token_bytes(32)).connect() as device:
            served = [bytes(r["entry"]) for r in device.state_rows() if r.get("entry")]
        assert served.count(envelope) == 1, (
            "the copy comes back to the owner's device byte for byte")

        store = os.path.join(serve_nest["tmp_dir"], "custody-hosting",
                             host.actor["actor_id_bytes"].hex(), owner["actor_id_hex"])
        held = dict(_files(store))
        assert any(envelope in data for data in held.values()), (
            f"the pair store at {store} holds the sealed copy (so the search below looks "
            f"where the copy is): {sorted(held)!r}")
        for path, data in _files(serve_nest["tmp_dir"]):
            assert marker not in data, f"the readable text reached the nest's disk: {path}"


@pytest.mark.feature("admin-held-custody")
def test_the_nest_hands_a_held_copy_back_only_to_the_owners_own_devices(serve_nest):
    """The owner's device reads; the host's own device, a stranger's device
    and a device the owner removed are each refused at the handshake with one
    opaque code; and a device admitted BEFORE its removal is refused at its
    next request once the re-signed grant naming it is deposited."""
    owner, stranger = _account(serve_nest), _account(serve_nest)
    _seal_and_put(serve_nest, owner, b"only-for-the-owner")
    with Host(serve_nest, _account(serve_nest)) as host:
        g = host.custody(owner)
        _pulled(serve_nest, host)
        url = serve_nest["url"]

        with FleetDevice(url, owner, secrets.token_bytes(32)).connect() as own:
            assert own.state_rows(), "the owner's own device is served"

        host_secret = bytes(host.actor["signing_key"])
        assert _refused(lambda: FleetDevice(
            url, owner, host_secret, cert_signer=host_secret).connect()) == NOT_REGISTERED, (
            "the host's own device is not the owner's fleet")
        assert _refused(lambda: FleetDevice(
            url, owner, secrets.token_bytes(32),
            cert_signer=bytes(stranger["signing_key"])).connect()) == NOT_REGISTERED, (
            "a device a stranger vouches for is not the owner's fleet")

        removed = FleetDevice(url, owner, secrets.token_bytes(32)).connect()
        sibling = FleetDevice(url, owner, secrets.token_bytes(32)).connect()
        with removed, sibling:
            assert removed.state_rows() and sibling.state_rows(), "both read before the removal"
            assert host.register(owner, g, removed_devices=(removed.key,)).get("ok") is True
            assert _refused(removed.state_rows) == SESSION_REFUSED, (
                "the removal cuts the device's live session at its next request")
            assert sibling.state_rows(), "the owner's other device still reads"
        assert _refused(lambda: FleetDevice(
            url, owner, removed.device_secret).connect()) == NOT_REGISTERED, (
            "and refuses the removed device's next handshake")


@pytest.mark.feature("admin-held-custody")
@pytest.mark.parametrize("paused", [False, True], ids=["live", "paused"])
def test_when_the_owner_takes_the_permission_back_the_nest_stops_serving(serve_nest, paused):
    """The owner revokes; the next pump pass's handshake is refused by the
    owner's nest and marks the copy; from then a session opened before the
    revocation is refused at its next request and a new handshake is refused.
    A copy the host has paused is no exception: the pump still asks the
    owner's nest each pass, only to hear its answer, and fetches nothing."""
    owner = _account(serve_nest)
    _seal_and_put(serve_nest, owner, b"served-until-revoked")
    with Host(serve_nest, _account(serve_nest)) as host:
        g = host.custody(owner)
        _pulled(serve_nest, host)
        if paused:
            assert host.register(owner, g, stopped=True).get("ok") is True
        device_secret = secrets.token_bytes(32)
        with FleetDevice(serve_nest["url"], owner, device_secret).connect() as device:
            assert device.state_rows(), "served before the revocation"

            fauna_ffi.harness_revoke_custody(
                serve_nest["url"], bytes(owner["signing_key"]), g, host.custodian)
            report = pump(serve_nest)
            assert report["owner_refused"] == 1 and report["pulled"] == 0, (
                f"the owner's nest refuses the revoked custody's handshake: {report!r}")
            assert report["stopped"] == (1 if paused else 0), report
            assert _refusal_mark(serve_nest, host, g) > 0, "the refusal marks the held copy"

            assert _refused(device.state_rows) == SESSION_REFUSED, (
                "the session opened before the revocation is refused at its next request")
        assert _refused(lambda: FleetDevice(
            serve_nest["url"], owner, device_secret).connect()) == NOT_REGISTERED, (
            "and a new handshake is refused")


def _refusal_mark(nest, host: Host, g: bytes) -> int:
    """The hosting row's ``owner_refused_at``, read off the nest's own DB — no
    door shows it (the door's own reading of it is what the refusals above
    witness)."""
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        (mark,) = conn.execute(
            "SELECT owner_refused_at FROM custody_hosting WHERE host_actor_id = ? AND grant_id = ?",
            (host.actor["actor_id_bytes"], g)).fetchone()
    finally:
        conn.close()
    return mark
