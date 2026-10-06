"""tier_3: the WebDAV files surface mounts, authenticates, and gates served sets.

The first client-integration proof for the WebDAV slice (webdav-server.md): a
scripted WebDAV client (`helpers/webdav_client.WebDAVClient`, speaking raw RFC
4918 over HTTPS Basic Auth — the surface GNOME Files / Dolphin / Finder /
Cyberduck / rclone use) drives the REAL binary stack (real nest + real
mail-bridge MDA, real HTTPS, real `davauth` AEAD-unwrap AUTH) end-to-end.

This is the CHEAP, served-set-free half of the WebDAV tier_3 matrix — it needs
NO content-key fixture (no seal, no `WebdavKeysBlob`), because it proves only the
mount + AUTH + the *listing/gate* plane:

  1. The shared :443 DAV listener actually mounts `/webdav/` at boot (mda.go
     `davMounts(...webdavOn...)`; webdavOn = `WebDAVEnabled` mirror of nest's
     `webdav_enabled.unwrap_or(mail_enabled)`, so a mail-enabled box serves it).
  2. HTTP Basic → `davauth` AEAD-unwrap-as-auth admits the mail credential (the
     same one IMAP/CalDAV/CardDAV share).
  3. A root PROPFIND round-trips `webdav_list_folders` through the real MDA →
     an EMPTY served-set list (the fresh admin has flagged nothing), so the root
     collection lists zero child collections.
  4. The served-set gate: an unserved set path 404s on PROPFIND and on GET
     (`isServed` false / `webdav_list_files` → `set_not_served` → mapErr 404) —
     you cannot browse or read a set the actor has not flagged for serving.

The full read+write round-trip (a served set round-trips a file's bytes; a
`read_only` set rejects PUT 403) rides a SECOND tier_3 test that arranges the
served-set precondition (serve-enable + `WebdavKeysBlob` provision) — see
(tracked internally). This test banks the deployment /
mount / AUTH / gate proof on its own, immediately.

Only tier_3 catches this: the mount happens inside the real Go MDA's shared DAV
listener wired from the real nest config projection — no stub or in-process twin
exercises the `webdav_enabled` → listener-bind → `/webdav/` mux path.

A DEDICATED nest (`dedicated_mail_nest`) is used, mirroring the CardDAV/CalDAV
round-trips: the nest admin must be the only actor enabling mail there. At MDA boot `webdav_enabled` inherits `mail_enabled` (unset → true),
so the shared :443 listener mounts `/webdav/` from boot exactly as it mounts
`/carddav/` for the CardDAV round-trip — no separate WebDAV enable/port.

webdav-server.md § Process topology & attach pattern + § MDA↔nest WS-RPC contract
(webdav_list_folders / webdav_list_files) + § Protocol surface.
"""

import uuid

import pytest
import requests

from helpers.webdav_roundtrip import enable_mail_and_client

# Drives the client's own "Enable mail" settings UI to mint the AEAD-unwrap AUTH
# credential (linux + windows implement the mail-settings page; the CardDAV /
# CalDAV tier_3 twins scope the same way). macOS/iOS join 2026-07-15 (same
# mail-settings enable-plain flow already proven by the CalDAV/CardDAV twins —
# no WebDAV-specific client UI is exercised here, just the mount/AUTH/gate on
# the real MDA). Scope to the implementing clients so --client deselects it on
# the others instead of failing on the missing toggle.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # tui added 2026-08-21: `enable_mail_and_client` drives only the
    # generic mail-settings enable-plain gesture (already tui-proven via
    # test_folder_webdav_toggle.py's tui marker); the rest is a raw WebDAV
    # client against the MDA, driver-agnostic.
    pytest.mark.tui,
    # web added: same reasoning as tui — proven via
    # test_folder_webdav_toggle.py's web marker.
    pytest.mark.web,
    # android added 2026-10-05: its mail-settings form paints every element
    # `enable_mail_plain` drives, and the dedicated nest reaches the device
    # through `_relaunch_trusting_nest`'s `adb reverse`.
    pytest.mark.android,
]


@pytest.mark.feature("files-in-standard-apps")
def test_webdav_mount_auth_and_served_set_gate(app, dedicated_mail_nest, request):
    """The real WebDAV surface mounts, authenticates, lists an empty served set,
    and gates unserved sets — the served-set-free deployment/mount/AUTH proof.

    RED before the Go WebDAV terminator + the `mda.go` `/webdav/` mount landed:
    the shared :443 listener never mounted `/webdav/`, so every PROPFIND/GET 404'd
    (or the listener refused the connection entirely).
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    # 1) Mount + AUTH + empty served-set list. A root PROPFIND Depth:1 must 207
    #    (the listener mounts /webdav/, davauth admits the credential) and list
    #    ZERO child collections (the fresh admin has flagged no set for serving —
    #    webdav_list_folders returns []). This is the real MDA round-tripping the
    #    nest-authoritative served-set enumeration.
    root = client.propfind_raw("", depth="1")
    assert root.status_code == 207, (
        "root PROPFIND of /webdav/{user}/ must be 207 Multi-Status — the shared "
        "DAV listener mounts /webdav/ and davauth admits the mail credential; got "
        f"HTTP {root.status_code} {root.reason}\n{root.text[:800]}"
    )
    children = client.child_hrefs("", depth="1")
    assert children == [], (
        "a fresh admin has flagged no set for serving, so the root collection must "
        f"list zero child set collections (webdav_list_folders → []); got {children!r}"
    )

    # 2) Auth is actually enforced: a request WITHOUT the credential is challenged
    #    (401), never served. Proves the mount is not an open door.
    anon = requests.request(
        "PROPFIND",
        client.root,
        headers={"Depth": "0"},
        verify=False,
        timeout=30.0,
    )
    assert anon.status_code == 401, (
        f"an unauthenticated PROPFIND must be 401-challenged; got {anon.status_code}"
    )

    # 3) The served-set gate. An unserved set is unreachable: neither browsable
    #    (PROPFIND → 404, isServed false) nor readable (GET → 404, webdav_list_files
    #    → set_not_served → mapErr 404). A random name the actor never flagged.
    unserved = f"never-served-{uuid.uuid4().hex[:8]}"
    pf = client.propfind_raw(f"{unserved}/", depth="1")
    assert pf.status_code == 404, (
        f"PROPFIND of an unserved set {unserved!r} must be 404 (isServed false); "
        f"got {pf.status_code}\n{pf.text[:800]}"
    )
    g = client.get(f"{unserved}/some-file.txt")
    assert g.status_code == 404, (
        f"GET a file under an unserved set {unserved!r} must be 404 "
        f"(webdav_list_files → set_not_served → 404); got {g.status_code}"
    )

    # 4) The root collection itself is not a GET-able resource (a directory).
    #    emersion maps GET of a collection to 405 Method Not Allowed (Open with
    #    rel=="" → StatusMethodNotAllowed). Confirms the root is a browse point,
    #    not a file.
    gc = client.get("")
    assert gc.status_code in (405, 501, 400), (
        f"GET of the root collection must not return a body (405/501/400); "
        f"got {gc.status_code}"
    )
