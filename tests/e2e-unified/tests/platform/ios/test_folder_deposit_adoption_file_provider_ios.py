"""tier_3: a third-party deposit adopted by the iOS File Provider host's refresh.

iOS keeps no synced local folder (``local-folder-sync.md``'s absences), so the
two-seat witnesses of ``test_folder_deposit_adoption.py`` take no ``ios`` seat;
an iPhone reaches a deposit only as a capability host — the File Provider
extension, whose host's ``Refresh`` arm runs the shared adoption pass after its
fold (``docs/goal/behavior/on-demand-files-apple.md`` § Apple File Provider
binding → *Third-party deposit adoption on a capability host*;
``docs/goal/behavior/folder-deposit.md`` § Implementation status today). One
journey on a throwaway simulator, on ``test_file_provider_eviction.py``'s
scaffolding:

  1. The owner enrolls the extension's machine principal, makes a folder
     custody-first, and enables mail through the apps' shared Mail settings
     machine (``harness_enable_mail``): the MSEK the host reads out of the
     sealed mail custody to open the deposit, and the recipient key the nest
     seals it to.
  2. A connected app asks for the bare deposit scope; the owner approves
     through the production approving path (``harness_answer_consent`` — the
     grant's ``Mint`` recorded in the owner's log, so no capability sweep
     revokes it) choosing the folder, and the app deposits one file over the
     HTTP door.
  3. The extension is provisioned and the domain registered; ``signal`` wakes
     ``enumerateChanges`` → ``host.refresh()``, whose adoption pass lands the
     deposit in the folder.
  4. ``read`` (the app is the only in-simulator reader) returns the deposited
     bytes under the deposited name; the owner's inbox drains
     (``fauna.folders.deposits.list`` is empty); and neither the bytes nor the
     name rests in clear anywhere on the nest's disk.

The second journey adopts into a folder the owner SHARES: an owner tui seat
shares the set with a member's tui seat, who accepts and binds it; the owner
seat is then closed (a live owner root would adopt first), the deposit is
parked, and the host's refresh re-seals it under the set's M2 content key —
the change carries a content-key generation — and the member's seat receives
it once, as any shared-folder change.

Mutation carve-out (testing conventions point 8): the folder, the mail enable
and the approve are fixture setup through the shared machines the apps run;
the action under test is the capability host's adoption, driven through
``signalEnumerator`` the way the deposit door's nudge reaches it. No app seat
runs beside the extension: the host builds no account runtime, so nothing here
sweeps the grant either way.
"""

import contextlib
import hashlib
import secrets
import sys

import blake3
import cbor2
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.principal_session import alice_client
from helpers.waiting import wait_until
from tests.api.test_folder_deposit import (  # noqa: F401 — metadata_server/consent_nest_env are pytest fixtures adopted by import
    BARE_SCOPE,
    HTTP_MARKER,
    HTTP_NAME,
    _client,
    _everything_on_disk,
    _http_deposit,
    _inbox,
    consent_bridge,
    consent_nest,
    consent_nest_env,
    metadata_server,
)
from tests.api.test_folder_read_principal import _approve
from tests.platform.ios.test_file_provider_eviction import (  # noqa: F401 — fauna_ios_fp_app/fp_device are pytest fixtures adopted by import
    _extension_diagnostics,
    _fauna,
    fauna_ios_fp_app,
    fp_device,
)
from tests.platform.macos.test_file_provider_read_path import _BACKUP_KEY_CONTEXT

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="the iOS simulator runs on macOS only"),
    pytest.mark.tier_3,
    pytest.mark.ios,
]

# Ceilings a green run never pays (convention 14).
ADOPT_WINDOW = 180.0
DRAIN_WINDOW = 60.0



@pytest.fixture
def ios_file_provider_app(fp_device):
    """The throwaway simulator with the Fauna iOS app installed, as this
    witness's app door: every verb below runs in that app, so the run is an
    observation of the ios column (``helpers/feature_ledger.py``, app
    attribution), and the catalog scan reads this fixture as app-launching
    (``scripts/features_scan.py``'s ``APP_LAUNCHING_FIXTURES``). The eviction
    tests share ``fp_device`` but witness ``[nest]`` outcomes, so the note is
    taken here, not there."""
    from helpers import feature_ledger

    feature_ledger.note_app("ios")
    return fp_device

@pytest.mark.timeout(3600)
@pytest.mark.feature("connected-apps")
def test_a_deposit_is_adopted_by_the_ios_file_provider_refresh(
    consent_nest, consent_bridge, metadata_server, ios_file_provider_app
):
    nest, udid = consent_nest, ios_file_provider_app
    url = nest["url"]
    alice = nest["user"]
    owner_seed = bytes(alice["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    set_name = "fp-deposit-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)

    # ── Fixture setup, every write through the shared machines the apps run. ──
    # The machine principal first, so the set's custody and the mail custody
    # are wrapped to it (the host keys custody as the machine it is a process of).
    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, owner_seed)
    folder = fauna_ffi.harness_create_set(url, owner_seed, {"name": set_name})["id"]
    folder_ref = f"local:{folder}"
    fauna_ffi.harness_enable_mail(url, owner_seed)

    client, _holder = _client(nest, metadata_server, BARE_SCOPE)
    token = _approve(nest, client, folder)["access_token"]
    status, body = _http_deposit(nest, client, token, folder, HTTP_NAME, HTTP_MARKER)
    assert status == 202 and body == {"accepted": True}, (status, body)
    assert len(_inbox(nest, folder)) == 1, "the deposit is parked before the host adopts it"

    # Every harness seat enrolls on one deterministic device row with a fresh
    # writer key, so the seats above displaced the principal's grant there;
    # the host records the adopted item, so its signer must be live on a row —
    # this device's own, as an enrolled machine's is.
    with WsRpcAdminClient(url, actor_id=alice["actor_id_bytes"], signing_key=owner_seed) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
        )

    assert "provision OK" in _fauna(
        udid, "provision", url, alice["actor_id_bytes"].hex(), device_id.hex(),
        "fp-ext", backup_key.hex(), alice["token"],
        writer_secret.hex(), device_authorization.hex(),
    )
    assert "register OK" in _fauna(udid, "register", folder_ref, set_name, "always-enabled")
    try:
        # ── The action under test: the host's refresh adopts the deposit. ──
        digest = hashlib.sha256(HTTP_MARKER).hexdigest()

        def adopted() -> bool:
            _fauna(udid, "signal", folder_ref, set_name)
            out = _fauna(udid, "read", folder_ref, set_name, HTTP_NAME)
            return f"read OK: {HTTP_NAME}" in out and f"sha256={digest}" in out

        wait_until(
            adopted, ADOPT_WINDOW, interval=2.0,
            diagnose=lambda: (
                f"{HTTP_NAME} never read back with the deposited bytes after `signal` — "
                f"the host's refresh did not adopt the parked deposit; "
                f"{len(_inbox(nest, folder))} item(s) still parked\n"
                f"{_extension_diagnostics(udid)}"
            ),
        )
    finally:
        _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")

    # Adoption retired the parked item: the owner's inbox is empty.
    def parked() -> list:
        with alice_client(nest) as ws:
            return ws.call(
                "fauna.folders.deposits.list", {"folder_id": folder, "after": 0}
            ).get("items", [])

    wait_until(
        lambda: not parked(), DRAIN_WINDOW, interval=1.0,
        diagnose=lambda: f"adoption left the deposit parked: {len(parked())} item(s) listed",
    )

    for path, raw in _everything_on_disk(nest):
        for marker in (HTTP_MARKER, HTTP_NAME.encode()):
            assert marker not in raw, f"{marker!r} rests in cleartext in {path}"


SHARED_NAME = "deposited-agenda-s4.txt"
SHARED_MARKER = b"fp-adopted-into-a-shared-folder-plaintext-9d2a"

# The member's KeyPackage publish, the shared folder reaching their Folders
# page, and a member's hydration of a change it never wrote (convention 14:
# ceilings a green run never pays — `test_folder_deposit_adoption.py`'s).
KEYPACKAGE_WINDOW = 30.0
MEMBER_ROW_WINDOW = 90.0
MEMBER_WINDOW = 360.0


@pytest.mark.timeout(3600)
@pytest.mark.real_conversations
@pytest.mark.feature("connected-apps")
def test_the_ios_file_provider_adopts_into_a_shared_folder_and_a_member_receives_it(
    consent_nest, consent_bridge, metadata_server, request, tmp_path, ios_file_provider_app
):
    """Adoption into a folder the owner **shares**, by the iOS capability host:
    the extension's refresh re-seals the parked item under the set's M2 content
    key — the ordinary upload's seal, read through its throwaway fleet replica
    (``on-demand-files-shared-sets.md``, decision 1′) — and the member's own
    seat receives it as any shared-folder change (``folder-deposit.md`` § The
    deposit, the adoption paragraph).

    The owner's tui seat shares the folder and is then CLOSED before the
    deposit is made: every owner root adopts after its pull, so a live owner
    seat would adopt first and the witness would no longer be the host's. The
    member's tui seat stays up; a member never adopts (only the owner's
    recipient secret opens a parked item), so it receives the file only
    through the pull every member makes.
    """
    from common.auth import sync_changes_list
    from conftest import _folder_share_owner_seat, _folder_share_recipient_seat
    from helpers.atproto_consent import HANDLE_DOMAIN
    from helpers.folder_content import agent_diagnosis, bind_location_under_set
    from helpers.folder_share import roster_index, share_through_owner_ui
    from tests.api import conv_api
    from tests.test_folder_agent_content_sync import _await_file_content
    from tests.test_folder_member_media_decrypt import _path_hash

    nest, udid = consent_nest, ios_file_provider_app
    url = nest["url"]
    alice = nest["user"]
    owner_seed = bytes(alice["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    set_name = "fp-shared-deposit-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)

    # ── Fixture setup: the principal first, so the set's custody and the mail
    # custody are wrapped to it; the set before either app launches, so the
    # owner's Folders page lists it on its first read. ──
    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, owner_seed)
    folder = fauna_ffi.harness_create_set(url, owner_seed, {"name": set_name})["id"]
    folder_ref = f"local:{folder}"
    fauna_ffi.harness_enable_mail(url, owner_seed)

    with contextlib.ExitStack() as stack:
        owner_seat = _folder_share_owner_seat("tui", nest, request, owner=alice)
        owner_app, _ = next(owner_seat)
        stack.callback(owner_seat.close)
        member_seat = _folder_share_recipient_seat(
            "tui", nest, request, handle_prefix="member", domain=HANDLE_DOMAIN,
            screenshot="teardown-fp-deposit-member",
        )
        member_app, member = next(member_seat)
        stack.callback(member_seat.close)
        request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))

        # ── The owner shares the folder; the member accepts and binds it. ──
        wait_until(
            lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
            KEYPACKAGE_WINDOW,
            diagnose=lambda: "the member never published a KeyPackage the share can admit",
        )
        ob = owner_app.backups
        row = share_through_owner_ui(owner_app, set_name, member, want=1)
        ob.set_member_access("writer", roster_index(ob, member["handle"], row=row), row=row)
        wait_until(
            lambda: ob.member_access(roster_index(ob, member["handle"], row=row), row=row)
            == "writer",
            15.0,
            diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
        )

        mb = member_app.backups
        mb.navigate_folders()
        assert mb.wait_for_pending_shares(1) == 1, (
            f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
        )
        mb.accept_pending_share(0)

        def _member_row_listed() -> bool:
            if any(set_name in t for t in mb.folder_titles()):
                return True
            mb.navigate_devices()
            mb.navigate_folders()
            return False

        wait_until(
            _member_row_listed, MEMBER_ROW_WINDOW,
            diagnose=lambda: f"[member] the folder never appeared; error={member_app.error_text()!r}",
        )
        member_folder = tmp_path / "member-bound"
        member_folder.mkdir()
        bind_location_under_set(member_app, set_name, member_folder, seat="member")

        # ── The owner's seat closes: no owner root may run while the deposit
        # is parked. ──
        owner_seat.close()

        client, _holder = _client(nest, metadata_server, BARE_SCOPE)
        token = _approve(nest, client, folder)["access_token"]
        status, body = _http_deposit(nest, client, token, folder, SHARED_NAME, SHARED_MARKER)
        assert status == 202 and body == {"accepted": True}, (status, body)
        assert len(_inbox(nest, folder)) == 1, "the deposit is parked before the host adopts it"

        # The extension's own device row, its signer live (the first case's note).
        with WsRpcAdminClient(url, actor_id=alice["actor_id_bytes"], signing_key=owner_seed) as ws:
            ws.call(
                "fauna.sync.register",
                {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
            )
            ws.call(
                "fauna.sync.device_grant.register",
                {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
            )

        assert "provision OK" in _fauna(
            udid, "provision", url, alice["actor_id_bytes"].hex(), device_id.hex(),
            "fp-ext", backup_key.hex(), alice["token"],
            writer_secret.hex(), device_authorization.hex(),
        )
        assert "register OK" in _fauna(udid, "register", folder_ref, set_name, "always-enabled")
        try:
            # ── The action under test: the host's refresh adopts the deposit. ──
            digest = hashlib.sha256(SHARED_MARKER).hexdigest()

            def adopted() -> bool:
                _fauna(udid, "signal", folder_ref, set_name)
                out = _fauna(udid, "read", folder_ref, set_name, SHARED_NAME)
                return f"read OK: {SHARED_NAME}" in out and f"sha256={digest}" in out

            wait_until(
                adopted, ADOPT_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"{SHARED_NAME} never read back with the deposited bytes after `signal` — "
                    f"the host's refresh did not adopt the parked deposit into the shared set; "
                    f"{len(_inbox(nest, folder))} item(s) still parked\n"
                    f"{_extension_diagnostics(udid)}"
                ),
            )
        finally:
            _fauna(udid, "remove", folder_ref, set_name)
            _fauna(udid, "revoke")

        wait_until(
            lambda: _inbox(nest, folder) == [], DRAIN_WINDOW, interval=1.0,
            diagnose=lambda: (
                f"adoption left the deposit parked: {len(_inbox(nest, folder))} item(s) listed"
            ),
        )
        changes = sync_changes_list(
            nest["port"], secret_key=owner_seed.hex(), folder=set_name, base_url=url,
        ).get("changes", [])
        generations = {c.get("path_hash"): c.get("content_key_version") for c in changes}
        assert generations.get(_path_hash(SHARED_NAME)) is not None, (
            "the host must seal the adopted file under the set's M2 content key — a "
            "change with no content-key generation was sealed under the owner's own "
            f"BackupKey, which no member can open: {generations!r}"
        )

        # ── …and the member's seat receives it as any shared change, once. ──
        _await_file_content(
            member_folder / SHARED_NAME, SHARED_MARKER.decode(), seat="member",
            app=member_app, window=MEMBER_WINDOW,
        )
        files = sorted(p.name for p in member_folder.iterdir() if not p.name.startswith("."))
        assert files == [SHARED_NAME], f"[member] one adopted file, never a duplicate: {files!r}"

    for path, raw in _everything_on_disk(nest):
        for marker in (SHARED_MARKER, SHARED_NAME.encode()):
            assert marker not in raw, (
                f"{marker!r} rests in cleartext in {path} — adoption re-seals under the "
                "set's M2 content key (folder-deposit.md § The deposit)"
            )
