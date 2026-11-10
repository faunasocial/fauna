"""tier_3 API e2e: **any folder you own can be served to your file manager
whatever it holds — and a reserved folder is refused**
(``docs/goal/behavior/webdav-server.md`` § What the WebDAV namespace *is*, in
Fauna terms: served ⇔ ``webdav_enabled`` ∧ not reserved; folders carry no type
— ``folders.md`` § Target re-model — so what a set holds never gates it).

A ``[nest]`` witness for ``docs/features/files-in-standard-apps.md`` outcome
18, every step a production one, app-less, on the session nest and its real
Go MDA:

- a fresh member enables mail through the shared ``MailSettingsMachine``
  (``fauna_ffi.harness_enable_mail``) — the MSEK in its sealed mail custody,
  the generated plain credential handed back for a file manager's Basic login
  — and takes an address on the MDA's domain;
- it creates two sets custody-first, writes JPEG bytes into one through the
  apps' own engine (``harness_sync_pass``), leaves the other plain, and serves
  both through the shared ``FoldersAuthor::serve_enable`` plus the
  ``WebdavKeysBlob`` re-provision a face runs after the flip
  (``harness_serve_set``);
- a file manager logged in as that member lists BOTH at the files root, and
  reads the photo back byte-identical;
- the member's reserved ``__drafts`` set (born by one compose-rail save,
  ``fauna.drafts.put``) cannot be served: ``fauna.folders.update`` with
  ``webdav_enabled`` answers ``fauna.folders.invalid_request``
  (``reserved-folders.md`` § The management surface refuses the namespace —
  whole), it is absent from the root listing, and a PROPFIND on it is 404.
"""
from __future__ import annotations

import secrets
import urllib.parse

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.mail_aliases import add_exact_alias
from helpers.set_names import addressed
from helpers.webdav_client import WebDAVClient
from helpers.webdav_roundtrip import unthrottled

pytestmark = pytest.mark.tier_3

#: A minimal JPEG (SOI, APP0/JFIF, EOI) — what makes the second set a photo
#: set is only its bytes; nothing on the nest records a kind.
JPEG = (
    b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
    + secrets.token_bytes(64)
    + b"\xff\xd9"
)
#: The member's one writing device, stable across its passes as a real seat's
#: is.
DEVICE = secrets.token_bytes(32)


def _names_at(client: WebDAVClient, path: str = "") -> list[str]:
    """The decoded last segments of the collection's children — the set names
    a file manager shows at the files root."""
    resp = unthrottled(lambda: client.propfind_raw(path, depth="1"))
    assert resp.status_code == 207, (
        f"PROPFIND {path or '/'} must list the collection; got "
        f"{resp.status_code}:\n{resp.text[:600]}"
    )
    hrefs = [e.href for e in WebDAVClient._parse_multistatus(resp)]
    self_path = urllib.parse.unquote(
        urllib.parse.urlsplit(client._url(path)).path
    ).rstrip("/")
    names = []
    for href in hrefs:
        p = urllib.parse.unquote(urllib.parse.urlsplit(href).path or href).rstrip("/")
        if p != self_path:
            names.append(p.rsplit("/", 1)[-1])
    return names


@pytest.mark.feature("files-in-standard-apps")
def test_any_owned_folder_is_served_whatever_it_holds_and_a_reserved_one_is_refused(
    nest_instance, mail_bridge_mda, tmp_path,
):
    nest = nest_instance
    url = nest["url"]
    member = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    seed = bytes(member["signing_key"])

    # Mail on through the apps' shared machine; the credential is what a file
    # manager logs in with, the address what it logs in as.
    password = fauna_ffi.harness_enable_mail(url, seed)
    assert isinstance(password, str) and password, (
        f"enabling mail must hand back the generated plain credential; got {password!r}"
    )
    address = add_exact_alias(
        url, member["signing_key"], mail_bridge_mda.domain, f"files-{secrets.token_hex(3)}"
    )

    # Two ordinary sets: one holding photo bytes, one plain. Both served.
    photos, plain = f"photos-{secrets.token_hex(3)}", f"notes-{secrets.token_hex(3)}"
    photos_id = fauna_ffi.harness_create_set(url, seed, {"name": photos})["id"]
    fauna_ffi.harness_create_set(url, seed, {"name": plain})
    # Served before the photo is written, so its bytes rest under the served
    # key (an app's toggle re-seals earlier files; the harness's does not).
    for name in (photos, plain):
        fauna_ffi.harness_serve_set(url, seed, name, True)
    watch = tmp_path / "photos-watch"
    watch.mkdir()
    (watch / "beach.jpg").write_bytes(JPEG)
    recorded, pending = fauna_ffi.harness_sync_pass(
        url, seed, f"local:{photos_id}", watch, tmp_path / "photos-state", DEVICE,
    )
    assert recorded == ["beach.jpg"] and pending == 0, (
        f"the member's engine wrote the photo into its set: {recorded!r}, {pending} pending"
    )

    files = WebDAVClient(
        f"https://127.0.0.1:{mail_bridge_mda.caldav_port}", address, password, verify=False
    )
    files.wait_until_serving()
    root = _names_at(files)
    assert photos in root and plain in root, (
        f"both served sets — the photo set and the plain one — must be listed at the "
        f"files root; got {root!r}"
    )
    got = unthrottled(lambda: files.get(f"{photos}/beach.jpg"))
    assert got.status_code == 200 and got.content == JPEG, (
        f"the photo must read back byte-identical through the mount; got "
        f"{got.status_code}, {len(got.content)} bytes"
    )

    # The reserved half: a compose-rail save births the member's `__drafts`.
    with WsRpcAdminClient(
        url, actor_id=member["actor_id_bytes"], signing_key=seed,
    ) as ws:
        ws.call("fauna.drafts.put", {"path": "conversations", "blob": secrets.token_bytes(48)})
        with pytest.raises(RpcCallError) as refused:
            ws.call("fauna.folders.update", addressed("__drafts", webdav_enabled=True))
    assert refused.value.code == "fauna.folders.invalid_request", (
        f"serving a reserved set must be refused invalid_request; got "
        f"{refused.value.code}: {refused.value}"
    )

    root = _names_at(files)
    assert "__drafts" not in root, f"a reserved set is never listed; got {root!r}"
    assert photos in root and plain in root, (
        f"the refusal must leave the served sets listed; got {root!r}"
    )
    probe = unthrottled(lambda: files.propfind_raw("__drafts/", depth="0"))
    assert probe.status_code == 404, (
        f"PROPFIND on the reserved set must be 404; got {probe.status_code}:\n"
        f"{probe.text[:600]}"
    )
