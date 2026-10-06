"""tier_3: what a file manager sees on a mounted, served folder — beyond the
bare write→read round trip.

`test_webdav_read_write_roundtrip.py` pins PUT → GET; `test_webdav_mount_and_gate.py`
pins sign-in and the served-set gate. This module walks the rest of what a person
relies on once a folder is mounted, driving the REAL binary stack (real nest + real
Go mail-bridge MDA) with a scripted RFC 4918 client
(`helpers/webdav_client.WebDAVClient` — the surface GNOME Files / Dolphin / Finder
/ Cyberduck / rclone use), and turning serving on and off through the APP's own
per-folder toggle (`folder-webdav-toggle`) — the mutation a person makes:

  * the listing carries each file's size and last-modified date
    (`getcontentlength` / `getlastmodified`);
  * a save from a stale copy is refused with 412 instead of overwriting the newer
    one, and the newer copy survives;
  * the nest holds the served file only sealed: neither its content nor its name
    rests readable anywhere in the nest's data directory;
  * the change shows in the folder's device activity as the network drive
    (the `"WebDAV"` pseudo-device);
  * once the app stops serving the folder, the mount no longer lists or opens it;
  * the file app sees space used and left (RFC 4331), and a file over the
    storage allowance is refused with 507 and costs nothing;
  * a file of exactly 512 MiB saves and reads back; one byte more is refused.

`webdav-server.md` § Protocol surface (v1) and deliberate deferrals, § Key model,
§ Threat model.
"""

import datetime
import email.utils
import hashlib
import os
import pathlib
import secrets
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_environment
from helpers.waiting import wait_until
from helpers.webdav_client import WebDAVError
from helpers.webdav_roundtrip import enable_mail_and_client, unthrottled, wait_status

# All 7 apps: this module serves through the production `folder-webdav-toggle`,
# which every app carries (`webdav-server.md` § Implementation status today,
# slice 6b), reads the device-activity rows all 7 render, and enables mail on
# the dedicated nest through the mail-settings page every app paints
# (`enable_mail_and_client`).
# `real_conversations`: the toggle's `serve_set` rides the live conversations
# session's MLS engine (see `test_folder_webdav_toggle.py`).
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

# The label the nest gives every WebDAV write's device
# (`fauna_core::label_custody::WEBDAV_PSEUDO_DEVICE_LABEL`).
_NETWORK_DRIVE_LABEL = "WebDAV"


def _serve_through_the_app(app, client, name):
    """Create a Sync folder and serve it with the app's own toggle, then wait
    for the mount to list it (the MDA reads the served set from the nest and
    its keys from the blob the toggle re-provisioned)."""
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    b.toggle_webdav()
    assert not app.has_error(), f"serving {name!r} raised: {app.error_text()!r}"

    def _listed() -> bool:
        resp = client.propfind_raw("", depth="1")
        return resp.status_code == 207 and any(
            name in h for h in client.child_hrefs("", depth="1")
        )

    wait_until(
        _listed,
        45.0,
        interval=2.5,
        diagnose=lambda: f"root listing: {client.propfind_raw('', depth='1').text[:600]}",
    )


def _unserve_through_the_app(app, name):
    b = app.backups
    b.navigate_folders()
    if not b.webdav_toggle_visible():
        b.find_and_expand_folder_until(name, "folder-webdav-toggle")
    b.toggle_webdav()
    assert not app.has_error(), f"unserving {name!r} raised: {app.error_text()!r}"


def _listing_entry(client, path):
    """The folder listing's entry for `path`, re-reading through a rate-limit
    refusal (`unthrottled`'s reason) until the listing answers."""
    found = []

    def _read() -> bool:
        try:
            found[:] = [client.entry(path)]
        except WebDAVError as e:
            if e.resp is not None and "rate_limited" in e.resp.text:
                return False
            raise
        return True

    wait_until(_read, 90.0, interval=5.0)
    return found[0]


def _assert_listing_carries_size_and_date(client, path, body, before, after):
    e = _listing_entry(client, path)
    assert e is not None, f"the listing must carry {path!r}"
    assert e.content_length == len(body), (
        f"the listing must show the file's size: getcontentlength="
        f"{e.content_length!r}, the file is {len(body)} bytes"
    )
    assert e.last_modified, "the listing must carry the file's last-modified date"
    modified = email.utils.parsedate_to_datetime(e.last_modified)
    # HTTP dates are second-granular: widen the window by a second each side.
    assert (
        before - datetime.timedelta(seconds=1)
        <= modified
        <= after + datetime.timedelta(seconds=1)
    ), (
        f"getlastmodified {e.last_modified!r} must be when the file was saved "
        f"(between {before.isoformat()} and {after.isoformat()})"
    )
    return e


def _assert_nest_holds_it_only_sealed(handle, content_marker: bytes, name: str):
    """Walk every file under the nest's data directory: the content marker must
    appear in none, and the file's name in none of the database files (paths
    rest sealed, `path-sealing.md`)."""
    db_path = handle.nest.get("db_path")
    if not db_path or not os.path.exists(db_path):
        skip_environment(
            "the nest's data directory is not on this filesystem (docker/live "
            "mode) — the at-rest walk needs the standalone nest's files"
        )
    data_dir = pathlib.Path(db_path).parent
    name_bytes = name.encode()
    walked = 0
    for p in data_dir.rglob("*"):
        if not p.is_file():
            continue
        try:
            blob = p.read_bytes()
        except OSError:
            continue  # a file removed mid-walk (a WAL checkpoint) holds nothing
        walked += 1
        assert content_marker not in blob, (
            f"the served file's content rests READABLE in {p} — the nest must "
            f"hold served files only in sealed form"
        )
        if p.name.startswith(pathlib.Path(db_path).name):
            assert name_bytes not in blob, (
                f"the served file's name rests readable in {p} — a DAV write "
                f"must record its path sealed"
            )
    assert walked > 0, f"the at-rest walk read nothing under {data_dir}"


@pytest.mark.feature("files-in-standard-apps")
def test_a_mounted_folder_behaves_like_a_network_drive(app, dedicated_mail_nest, request):
    """Served through the app's toggle: the listing carries size + date, a stale
    save is refused and the newer copy survives, the nest holds the file only
    sealed, the change shows as the network drive in the folder's activity, and
    unserving through the app closes the mount to the folder."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _addr = enable_mail_and_client(app, handle, request)

    name = f"drive-{secrets.token_hex(4)}"
    _serve_through_the_app(app, client, name)

    # ── size + last-modified in the listing ─────────────────────────────
    marker = f"served-file-content-{uuid.uuid4().hex}".encode()
    first = os.urandom(3000) + marker + os.urandom(3000)
    file_name = f"report-{secrets.token_hex(6)}.bin"
    path = f"{name}/{file_name}"
    before = datetime.datetime.now(datetime.timezone.utc)
    put = unthrottled(lambda: client.put(path, first))
    after = datetime.datetime.now(datetime.timezone.utc)
    assert put.status_code in (201, 204), f"first save: {put.status_code}\n{put.text[:600]}"
    e1 = _assert_listing_carries_size_and_date(client, path, first, before, after)
    stale_etag = e1.etag
    assert stale_etag, "the listing must carry the file's ETag"

    # ── the stale save ──────────────────────────────────────────────────
    # A second file app saves a newer copy, conditioned on the copy it read.
    newer = b"the newer copy another app saved\n" * 40
    upd = unthrottled(lambda: client.put(path, newer, if_match=stale_etag))
    assert upd.status_code in (200, 201, 204), (
        f"a save conditioned on the current ETag must succeed; got "
        f"{upd.status_code}\n{upd.text[:600]}"
    )
    # The first app, still holding the old ETag, saves over it: refused.
    stale = unthrottled(
        lambda: client.put(path, b"an edit made on the stale copy\n", if_match=stale_etag)
    )
    assert stale.status_code == 412, (
        f"a save from a copy that changed since the app last looked must be "
        f"refused with 412; got {stale.status_code}\n{stale.text[:600]}"
    )
    got = wait_status(lambda: client.get(path), 200)
    assert got.content == newer, (
        "the refused stale save must leave the newer copy in place; got "
        f"{len(got.content)} bytes, head {got.content[:40]!r}"
    )

    # ── the nest holds it only sealed ───────────────────────────────────
    # The FIRST body carried the marker. The newer copy has since replaced it as
    # the head, but its chunks stay on the nest as a retained version — so the
    # walk covers bytes the nest keeps, not just the live head.
    _assert_nest_holds_it_only_sealed(handle, marker, file_name)

    # ── the change shows as the network drive ───────────────────────────
    b = app.backups
    b.navigate_folders()
    b.find_and_expand_folder(name)

    def _drive_in_activity() -> bool:
        if b.device_activity_item_count() == 0:
            # A client that re-collapses on refresh: re-expand and look again.
            b.navigate_folders()
            b.find_and_expand_folder(name)
        return _NETWORK_DRIVE_LABEL in b.device_activity_labels()

    wait_until(
        _drive_in_activity,
        30.0,
        interval=1.0,
        diagnose=lambda: (
            f"device-activity labels={b.device_activity_labels()!r} "
            f"error={app.error_text() if app.has_error() else '(none)'}"
        ),
    )

    # ── unserving closes the mount to the folder ────────────────────────
    _unserve_through_the_app(app, name)

    def _closed() -> bool:
        resp = unthrottled(lambda: client.propfind_raw(f"{name}/", depth="1"))
        return resp.status_code == 404

    wait_until(
        _closed,
        45.0,
        interval=2.5,
        diagnose=lambda: (
            f"PROPFIND {name}/ still answers "
            f"{client.propfind_raw(f'{name}/', depth='1').status_code}"
        ),
    )
    root = unthrottled(lambda: client.propfind_raw("", depth="1"))
    assert root.status_code == 207, f"the mount root must still list: {root.status_code}"
    assert not any(name in h for h in client.child_hrefs("", depth="1")), (
        f"an unserved folder must drop out of the mount's listing: "
        f"{client.child_hrefs('', depth='1')!r}"
    )
    opened = unthrottled(lambda: client.get(path))
    assert opened.status_code == 404, (
        f"a file in an unserved folder must no longer open through the mount; "
        f"got {opened.status_code}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


def _set_own_storage_allowance(nest, max_storage_bytes: int) -> None:
    """Arrangement (not the mutation under test): set the storage allowance of
    the tier the signed-in admin is on, over the admin kinds the admin tier
    editor drives (`fauna.admin.tiers.update`)."""
    admin = nest["admin"]
    actor = bytes(admin["signing_key"].verify_key)
    ws = WsRpcAdminClient(nest["url"], actor_id=actor, signing_key=bytes(admin["signing_key"]))
    with ws:
        tier_name = ws.call("fauna.admin.users.get", {"actor_id": actor})["user"]["tier"]
        tiers = ws.call("fauna.admin.tiers.list", {})["tiers"]
        tier = next(t for t in tiers if t["name"] == tier_name)
        update = {
            k: tier[k]
            for k in ("name", "max_inbox_bytes", "max_devices", "max_blob_size", "max_feeds")
        }
        update["max_storage_bytes"] = max_storage_bytes
        ws.call("fauna.admin.tiers.update", update)


def _quota(client, path: str) -> tuple[int | None, int | None]:
    resp = unthrottled(lambda: client.quota_raw(path))
    assert resp.status_code == 207, (
        f"a quota PROPFIND on {path or 'the mount root'!r} must answer 207; got "
        f"{resp.status_code}\n{resp.text[:600]}"
    )
    return client.parse_quota(resp)


@pytest.mark.feature("files-in-standard-apps")
def test_the_mount_reports_space_and_refuses_a_file_over_the_allowance(
    app, dedicated_mail_nest, request
):
    """The file app sees space used and left (RFC 4331 quota properties), both
    moving with what is saved; a file that would pass the allowance is refused
    with 507 and costs nothing."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, nest, _addr = enable_mail_and_client(app, handle, request)
    name = f"space-{secrets.token_hex(4)}"
    _serve_through_the_app(app, client, name)

    used0, _ = _quota(client, "")
    assert used0 is not None, "the mount must report the space used"
    allowance = used0 + 64 * 1024
    _set_own_storage_allowance(nest, allowance)

    for path in ("", f"{name}/"):
        used, available = _quota(client, path)
        assert (used, available) == (used0, allowance - used0), (
            f"on {path or 'the mount root'!r} the file app must see "
            f"{used0} used / {allowance - used0} left; saw {used} / {available}"
        )

    small = os.urandom(32 * 1024)
    put = unthrottled(lambda: client.put(f"{name}/fits.bin", small))
    assert put.status_code in (201, 204), f"a file inside the allowance: {put.status_code}"
    used1, available1 = _quota(client, f"{name}/")
    assert (used1, available1) == (used0 + len(small), allowance - used0 - len(small)), (
        f"saving {len(small)} bytes must move the space used/left; saw {used1} / {available1}"
    )

    too_big = os.urandom(64 * 1024)
    refused = unthrottled(lambda: client.put(f"{name}/too-big.bin", too_big))
    assert refused.status_code == 507, (
        f"a file over the allowance must be refused with 507 Insufficient Storage; "
        f"got {refused.status_code}\n{refused.text[:600]}"
    )
    gone = unthrottled(lambda: client.get(f"{name}/too-big.bin"))
    assert gone.status_code == 404, f"the refused file must not exist: {gone.status_code}"
    assert _quota(client, f"{name}/") == (used1, available1), (
        "a refused save must cost nothing"
    )


_MIB = 1024 * 1024
_CAP = 512 * _MIB


def _stream(total: int, block: bytes):
    """Yield `total` bytes built from `block`, without holding them all."""
    sent = 0
    while sent < total:
        piece = block[: min(len(block), total - sent)]
        sent += len(piece)
        yield piece


@pytest.mark.feature("files-in-standard-apps")
def test_the_mount_saves_a_512_mib_file_and_refuses_a_bigger_one(
    app, dedicated_mail_nest, request
):
    """A file of exactly 512 MiB saves through the mount and reads back
    intact; one byte more is refused as too large (413) and leaves nothing."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, nest, _addr = enable_mail_and_client(app, handle, request)
    client.timeout = 900.0
    # Arrangement: an allowance that is not the limit under test.
    _set_own_storage_allowance(nest, 8 * 1024 * _MIB)
    name = f"big-{secrets.token_hex(4)}"
    _serve_through_the_app(app, client, name)

    block = os.urandom(_MIB)
    digest = hashlib.sha256()
    for piece in _stream(_CAP, block):
        digest.update(piece)

    put = unthrottled(lambda: client.put(f"{name}/exactly-512.bin", _stream(_CAP, block)))
    assert put.status_code in (201, 204), (
        f"a 512 MiB file must save through the mount; got {put.status_code}\n"
        f"{put.text[:600]}"
    )
    e = _listing_entry(client, f"{name}/exactly-512.bin")
    assert e is not None and e.content_length == _CAP, (
        f"the saved file must list at 512 MiB; entry={e!r}"
    )
    got = client.session.get(client._url(f"{name}/exactly-512.bin"), stream=True, timeout=900)
    assert got.status_code == 200, f"reading the 512 MiB file back: {got.status_code}"
    back = hashlib.sha256()
    for piece in got.iter_content(chunk_size=_MIB):
        back.update(piece)
    assert back.hexdigest() == digest.hexdigest(), "the 512 MiB file must read back intact"

    over = unthrottled(
        lambda: client.put(f"{name}/one-byte-over.bin", _stream(_CAP + 1, block))
    )
    assert over.status_code == 413, (
        f"a file one byte over 512 MiB must be refused as too large (413); got "
        f"{over.status_code}\n{over.text[:600]}"
    )
    left = unthrottled(lambda: client.get(f"{name}/one-byte-over.bin"))
    assert left.status_code == 404, f"the refused file must not exist: {left.status_code}"
