"""tier_3: the iOS File Provider extension's relay seat serves only under the
app's foreground lease — a kept body serves by relay while the app is in
front, a placeholder answers none, and a backgrounded phone withdraws.

Owner: ``docs/goal/behavior/relay-serving.md`` § Relay serving → *Which seats
hold* → *Apple's File Provider replica*, point (2): on iOS the extension's seat
announces only while the app's foreground lease is live, serves only what its
staging root holds, and withdraws when the app's stop arrives at the
background edge or the lease lapses unrenewed.

**What this pins that the tiers below cannot.** ``file_provider_host::tests``
pins the lease's grant, revoke and lapse on the Rust side, and
``RelayLeaseRenewerTests`` the app's renewal loop over a fake door set. Neither
shows that the real app, in front on a simulator, reaches the real
extension's ``social.fauna.relay-lease`` door for a registered domain, that
the extension then announces its set on its own connection and answers the
nest's ask from its staging root, nor that sending the app to the background
takes the announce away.

**The two processes.** The iOS seat is the Debug ``Fauna-iOS`` app (the
``ios_file_provider_app`` marker: it embeds the extension), launched through
the ordinary iOS driver on the run's seat simulator and signed in as the
account — the app is what holds the lease. The domain is registered by the
same bundle's DEBUG ``FileProviderTestCLI`` (``provision`` / ``register``, as
in ``test_file_provider_eviction.py``), whose every verb is a launch of the
same app and so ends the seat's process; the seat pins its state and is
relaunched signed in (``hard_reload``) once the CLI part is done. The app's
own domain reconcile is left off (``FAUNA_E2E_FILE_PROVIDER_RECONCILE`` is
dropped from the launch): the lease lists the account's domains through
``NSFileProviderManager.domains()`` itself (``RelayLeaseDoors``), so the one
domain is the CLI's, and no reconcile can add or take one away mid-witness.

**The journey.** One account, one metadata-only folder (its bytes never rest
on the nest, so a body moves between devices only by relay):

    seat a — the File Provider domain on the iOS seat's simulator
    seat b — a tui seat that writes the placeholder, then stops
    seat c — a tui seat that reads by relay, then stops

1. *Fixture setup.* A file is written into a's domain (``write``) and lands in
   a's staging root byte for byte; b starts, writes a file of its own, and
   a's domain lists it (``signal``) without fetching it. b stops.
2. *No lease, no seat.* With b gone and the app not yet in front, the
   extension — launched by the OS for the writes and the nudge, holding the
   kept body — has never been granted a lease, so the nest reads no source
   online: on iOS the process living is not enough (on macOS it is).
3. *Kept body serves.* The app comes back in front, signed in; its lease
   reaches the extension, whose seat announces — the verdict turns true with
   no other seat running. c starts and receives the kept body, which only a's
   extension could serve.
4. *Placeholder declines.* c's ask for b's file has no holder but a, whose
   staging root holds no copy: c never has it, and a's staging root still
   holds no copy of it afterwards.
5. *Background edge.* With c gone and a still announced, the app is sent to
   the background through the simulator (another app launched in front, the
   app's process left alive): the verdict turns false — the app's stop revoked
   the lease, or at worst the lease lapsed unrenewed within ``RELAY_LEASE``.
6. *After.* A coordinated open of b's file fails, and a's staging root still
   holds the kept body and no copy of b's file.

Every wait is on a state, never on time (convention 14); the windows are
ceilings a green run never pays. Mutation carve-out (convention 8): the
folder, the writes through the domain and the sign-in are fixture setup; the
actions under test are the app's foreground and background edges.
"""

import contextlib
import hashlib
import os
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import blake3
import cbor2
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import register_user, set_folder_residency, sync_status, user_create_folder
from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, _user_call, mint_and_hold_token, set_tier_caps
from helpers import sync_seats
from helpers.set_names import item_in_set
from helpers.waiting import wait_until
from tests.platform.ios.test_file_provider_eviction import (
    _extension_diagnostics,
    _fauna,
    _keep_logs,
    _register,
    _replica,
    _sim,
)
from tests.platform.macos.test_file_provider_read_path import _BACKUP_KEY_CONTEXT

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="the iOS simulator runs on macOS only"),
    pytest.mark.tier_3,
    pytest.mark.ios,
    pytest.mark.ios_file_provider_app,
]

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0
# The background edge: the app's revoke lands at once; a revoke that never
# reached the extension lapses within `RELAY_LEASE` (30 s, `file_provider_host.rs`).
WITHDRAW_WINDOW = 120.0

_KEPT = "kept.txt"
_PEER = "peer.txt"
# Launched in front of Fauna to send it to the background; ships on every runtime.
_FRONT_APP = "com.apple.Preferences"


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _staged(udid: str, folder_ref: str) -> dict[str, str]:
    """What a's staging root holds: ``{rel: sha256}`` from ``Fauna staged`` (a
    launch of the app, so it ends the seat's process)."""
    out: dict[str, str] = {}
    for line in _fauna(udid, "staged", folder_ref).splitlines():
        if line.startswith("staged: ") and " sha256=" in line:
            rel = line[len("staged: "):].split(" bytes=", 1)[0]
            out[rel] = line.rsplit(" sha256=", 1)[1].strip()
    return out


def _launch_ios_seat(nest: dict, request):
    """The iOS seat: the File Provider app on the run's seat simulator, through
    the ordinary driver door, with the app's domain reconcile left off (module
    docstring)."""
    from conftest import _build_app_config
    from drivers import create_driver
    from drivers.inprocess_agent import InProcessAgentDriver
    from helpers import feature_ledger

    config = _build_app_config("ios", nest, request)
    config["environment"].pop("FAUNA_E2E_FILE_PROVIDER_RECONCILE", None)
    driver = create_driver("ios")
    feature_ledger.note_app("ios")
    driver.launch(config)
    if isinstance(driver, InProcessAgentDriver):
        driver.assert_render_ready()
    return driver


@pytest.mark.timeout(3600)
@pytest.mark.feature("device-to-device-file-transfer")
def test_the_ios_file_provider_seat_serves_under_the_foreground_lease_and_withdraws_in_the_background(
    nest_instance, request, tmp_path
):
    from actions import ActionLayer
    from tests.test_filesync_seats import handle_for, harness_sign_in

    nest = nest_instance
    port, url = nest["port"], nest["url"]
    admin_sk = nest["admin"]["signing_key"]

    # ── Fixture setup: one account, one metadata-only folder. ──
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    seed = bytes.fromhex(secret_key)
    # The extension signs as an enrolled machine of its own (the app's
    # principal lives in the e2e launch's harness-owned store, which no
    # extension reads). Enrolled before the set is born, so the set's custody
    # is wrapped to it.
    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, seed)
    folder = f"fp-lease-{secrets.token_hex(4)}"
    created = user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    folder_ref = f"local:{created['id']}"
    # Metadata-only BEFORE any engine first sees the folder, so no seat ever
    # uploads these bytes and the relay is the only way they can move.
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    from nacl.signing import SigningKey

    sk = SigningKey(seed)
    token = mint_and_hold_token(url, sk)
    ext_device = secrets.token_bytes(32)
    with WsRpcAdminClient(url, actor_id=bytes(sk.verify_key), signing_key=seed) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": ext_device.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        # The extension's records go by reference, so its signer must be live
        # on a row — this device's own, as an enrolled machine's is.
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": ext_device.hex(), "authorization": cbor2.loads(device_authorization)},
        )
    backup_key = blake3.blake3(seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()

    kept_body = f"kept on the phone's extension alone {secrets.token_hex(16)}\n"
    peer_body = f"written by seat b, never fetched by a {secrets.token_hex(16)}\n".encode()

    sign_in = harness_sign_in(url, secret_key, actor_id, handle)
    run_token = sync_seats.new_run_token(seats=3)
    seats_root = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))

    def tui_seat(name: str):
        return sync_seats.make_seat(
            "tui", name=name, run_token=run_token, root=seats_root / name,
            node_url=url, node_port=port, folder=folder, sign_in=sign_in, request=request,
        )

    def listed() -> list:
        items = _user_call(
            port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url
        ).get("items", [])
        return [item for item in items if item_in_set(item, folder)]

    def status() -> dict:
        return sync_status(port, secret_key=secret_key, folder=folder, base_url=url)

    def online_destinations() -> int:
        return sum(1 for d in status().get("destinations", []) if d.get("online"))

    drv = _launch_ios_seat(nest, request)
    request.addfinalizer(drv.teardown)
    udid = drv._launch_config["udid"]
    seat_a = ActionLayer(drv)
    sign_in(seat_a, "a")
    # Every CLI verb below is a launch of the same bundle, which ends the
    # seat's process: keep its container and store for the relaunch.
    assert drv.preserve_state_across_relaunch(), "the iOS seat must pin its state"

    def a_diag() -> str:
        return (
            f"--- seat a (iOS extension) ---\nstatus={status()!r}\n"
            f"{_extension_diagnostics(udid)}"
        )

    assert "provision OK" in _fauna(
        udid, "provision", url, actor_id, ext_device.hex(), "fp-ext",
        backup_key.hex(), token, writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, folder)
    try:
        # ── 1. Fixture setup: a kept body, and a placeholder a only lists. ──
        # A host that cannot key the set's custody yet refuses and rebuilds at
        # its next request, so that one answer is waited out.
        outs: list[str] = []

        def written() -> bool:
            outs.append(_fauna(udid, "write", folder_ref, folder, _KEPT, kept_body))
            return "binding indeterminate" not in outs[-1]

        wait_until(written, WINDOW, interval=5.0, diagnose=lambda: f"{outs[-1]}\n{a_diag()}")
        assert f"write OK: {_KEPT}" in outs[-1], f"the write failed: {outs[-1]}\n{a_diag()}"
        wait_until(
            lambda: len(listed()) == 1, WINDOW, interval=1.0,
            diagnose=lambda: f"the extension never recorded {_KEPT!r}\n{a_diag()}",
        )
        assert _staged(udid, folder_ref).get(_KEPT) == _sha256(kept_body.encode()), (
            f"{_KEPT!r} is recorded but not held byte for byte in a's staging root — "
            f"the body the seat would serve\n{a_diag()}"
        )

        online_before = online_destinations()
        with contextlib.ExitStack() as stack:
            b = stack.enter_context(tui_seat("b"))
            b.await_ready(STARTUP_WINDOW)
            (b.path / _PEER).write_bytes(peer_body)
            wait_until(
                lambda: len(listed()) == 2, WINDOW, interval=1.0,
                diagnose=lambda: f"seat b never recorded {_PEER!r}\n{b.diagnostics()}",
            )
            listed_on_a: list[str] = []

            def a_lists_peer() -> bool:
                listed_on_a.append(_fauna(udid, "signal", folder_ref, folder))
                return _replica(udid, _PEER) is not None

            wait_until(
                a_lists_peer, WINDOW, interval=5.0,
                diagnose=lambda: (
                    f"a's domain never listed {_PEER!r} after the nudge: {listed_on_a[-1]}\n"
                    f"{a_diag()}"
                ),
            )
        # b is gone: its connection, and with it its announce, closes.
        wait_until(
            lambda: online_destinations() <= online_before, WINDOW, interval=1.0,
            diagnose=lambda: f"seat b's device still reads online after it stopped\n{a_diag()}",
        )
        assert _PEER not in _staged(udid, folder_ref), (
            f"a's staging root holds {_PEER!r} before anything opened it\n{a_diag()}"
        )

        # ── 2. No lease, no seat. ──
        # The app has not been in front since the domain was registered, so no
        # grant ever reached the extension: holding the kept body, launched for
        # the writes and the nudge, it must still not announce.
        assert status().get("source_online") is not True, (
            "the nest reads a source online with no seat running but a's extension, "
            "and the app never in front since the domain was registered — the iOS "
            f"seat announced without the foreground lease\n{a_diag()}"
        )

        # ── 3. Kept body serves: the app in front, its lease reaches the seat. ──
        drv.hard_reload()
        wait_until(
            lambda: status().get("source_online") is True, WINDOW, interval=1.0,
            diagnose=lambda: (
                "the app is in front and signed in, yet a's extension never announced "
                "the folder — the lease never reached the seat, or the seat never "
                f"served under it\n{a_diag()}"
            ),
        )
        online_with_a = online_destinations()

        with contextlib.ExitStack() as stack:
            c = stack.enter_context(tui_seat("c"))
            c.await_ready(STARTUP_WINDOW)
            kept_on_c = c.path / _KEPT

            def landed() -> bool:
                try:
                    return kept_on_c.read_bytes() == kept_body.encode()
                except OSError:
                    return False

            wait_until(
                landed, WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"{_KEPT!r} never reached seat c byte for byte — the folder is "
                    "metadata-only and b is gone, so the bytes can only come from a's "
                    f"extension by relay\n--- seat c ---\n{c.diagnostics()}\n{a_diag()}"
                ),
            )
            # ── 4. Placeholder declines: no holder of b's file but a, which has none. ──
            assert not (c.path / _PEER).exists(), (
                f"{_PEER!r} reached seat c with its writer gone — in a metadata-only "
                "folder no seat but b ever held it, so a body came from somewhere it "
                f"must not\n{a_diag()}"
            )
        wait_until(
            lambda: online_destinations() <= online_with_a, WINDOW, interval=1.0,
            diagnose=lambda: f"seat c's device still reads online after it stopped\n{a_diag()}",
        )
        assert status().get("source_online") is True, (
            "a's extension stopped announcing while the app was still in front — the "
            f"lease was not renewed\n{a_diag()}"
        )

        # ── 5. Background edge: another app in front, Fauna's process alive. ──
        app_pid = drv._app_pid
        fronted = _sim("launch", udid, _FRONT_APP)
        assert fronted.returncode == 0, f"could not bring {_FRONT_APP} to the front: {fronted.stderr}"
        wait_until(
            lambda: status().get("source_online") is not True, WITHDRAW_WINDOW, interval=1.0,
            diagnose=lambda: (
                "the app is in the background, yet a's extension still announces the "
                f"folder — the seat did not withdraw at the background edge\n{a_diag()}"
            ),
        )
        if app_pid is not None:
            try:
                os.kill(app_pid, 0)
            except ProcessLookupError:
                pytest.fail(
                    "the app's process was gone when the seat withdrew — the witness is "
                    "of a killed app, not of the background edge"
                )

        # ── 6. After: the placeholder answers none, the kept body stays. ──
        read = _fauna(udid, "read", folder_ref, folder, _PEER)
        assert f"read OK: {_PEER}" not in read, (
            f"opening {_PEER!r} on a succeeded with its writer gone: {read}\n{a_diag()}"
        )
        staged = _staged(udid, folder_ref)
        assert _PEER not in staged, (
            f"a's staging root holds {_PEER!r} after the failed open — a placeholder must "
            f"answer none, never be fetched into the staging root\n{a_diag()}"
        )
        assert staged.get(_KEPT) == _sha256(kept_body.encode()), (
            f"a's kept body left its staging root\n{a_diag()}"
        )
    finally:
        _keep_logs(udid, tmp_path)
        _fauna(udid, "remove", folder_ref, folder)
        _fauna(udid, "revoke")
        shutil.rmtree(seats_root, ignore_errors=True)
