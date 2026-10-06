"""tier_3: a real WebDAV client reads AND writes a served folder end-to-end.

The full-round-trip half of the WebDAV tier_3 matrix (the mount/gate half is
`test_webdav_mount_and_gate.py`): a scripted RFC-4918 WebDAV client
(`helpers/webdav_client.WebDAVClient` — the surface GNOME Files / Dolphin /
Finder / Cyberduck / rclone use) drives the REAL binary stack (real nest + real
Go mail-bridge MDA, real HTTPS Basic → `davauth` AEAD-unwrap AUTH) through a
served folder's **write then read** cycle:

  PUT a file → the MDA chunks + seals it under the set's M2 content key through
  the sync engine's OWN seal (shared-Rust FFI `webdav_seal_file` = `seal_blob`:
  the one frame→AEAD→re-key door), uploads the ciphertext + the byte-identical
  manifest via the bulk-byte routes, and records an ordinary `sync_changes` change
  → GET it back → the MDA fetches the manifest + ciphertext chunks and opens them
  through the apps' shared walk (`webdav_open_file`) → the original plaintext.

The cross-writer half — an app's engine writes, DAV reads, and vice-versa — is
`test_webdav_engine_cross_writer.py`; this file pins the DAV-only round trip.

**Only tier_3 catches this:** the seal on PUT and the open on GET happen inside
the REAL Go MDA over the REAL wire, under the content key delivered by the
MSEK-sealed `WebdavKeysBlob` the client provisioned — a cross-binary
encrypt→store→decrypt contract no stub or in-process twin exercises (the sibling
of the CardDAV `test_carddav_roundtrip` seal/open proof).

**The served-set precondition** (a content-keyed, served Sync set + the sealed
`WebdavKeysBlob`) is arranged via the `serve_enable_folder` linux test-agent
command — the first non-test caller of the slice-2 orchestration
(`FoldersAuthor::serve_enable` + `reconcile_webdav_keys_blob`; its production
caller is the not-yet-built slice-6 per-set serve toggle). This is fixture setup
(carve-out (b): the *mutation under test* is the WebDAV PUT/GET through the real
client; arranging the served set is setup — exactly like `enable_caldav_mailbox`
mints the CalDAV MSEK). Because the WebDAV PUT itself does the chunk+seal+upload,
an EMPTY served set suffices — no folder bind, no back-catalogue re-seal.

webdav-server.md § Protocol surface + § Bulk-byte plane + § Key model.
"""

import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.webdav_roundtrip import (
    enable_mail_and_client,
    serve_enable_folder,
    wait_status,
)

# All 7 apps implement the mail-settings page + the serve_enable_folder
# test-agent command. iOS joined in the apple catalog trickle-down pass: its
# `serve_enable_folder` twin was the ONLY thing missing — the production control
# the hook stands in for (the shared `FolderWebdavToggle` in FaunaKit's
# `FoldersContent`) has served both apple targets since 2026-07-12, which is why
# `webdav-server.md` § Implementation status called this "a test-proof leg, not a
# missing control". The lift made the handler shared
# (`ServeEnableFolderTestCommand`), so both shells now answer one implementation.
# tui's (2026-09-19) runs the same `FoldersAuthor::serve_set` its
# `folder-webdav-toggle` gesture runs, over the conversations rail's one live
# session (`SettingsState::serve_enable_folder_for_test`).
# `real_conversations` (macOS): `APIClient.serveFolderWebdav` reuses the ONE
# cached `sharedConversationsSession()` (the production WebDAV 6b-2 per-set-
# toggle's own session, `webdav-server.md` § Implementation status) rather than
# building an ad-hoc one per call (unlike linux's `build_folders_author`,
# which mints its own MLS engine inline) — without the marker apple never
# activates that session at login and `awaitSharedConversationsSession()`
# throws "Conversations session not active".
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    # web joined in its catalog trickle-down pass: `serve_enable_folder` runs
    # over the conversations manager's folders author (`$lib/conversations`).
    pytest.mark.web,
    # android's (2026-10-05) answers through `ApiClient.serveSetFolder`, the face
    # its `folder-webdav-toggle` drives, over the live conversations session.
    pytest.mark.android,
    pytest.mark.real_conversations,
]

def _flag_served_no_keys(nest, set_name):
    """Create a Sync set + flag it `webdav_enabled` on nest via the production
    User-class RPCs, but NEVER serve-enable/reconcile it — so it is served-per-nest
    yet ABSENT from the actor's provisioned `WebdavKeysBlob`. The MDA's write
    boundary (`writableSet`) fails closed on a set whose keys aren't in the blob,
    so a PUT to it is 403 (FS-BIND-5) — the v1 proxy for "a non-writable set
    rejects PUT" (v1 `reconcile_webdav_keys_blob` seals `read_only:false` for all
    sets, so a true `read_only:true` set needs extra plumbing — tracked internally).
    Uses the admin actor (whom the WebDAV client also AUTHs as)."""
    admin = nest["admin"]
    admin_actor_id = bytes(admin["signing_key"].verify_key)
    ws = WsRpcAdminClient(
        nest["url"], actor_id=admin_actor_id, signing_key=bytes(admin["signing_key"])
    )
    with ws:
        ws.call(
            "fauna.folders.create",
            {"name": set_name},
        )
        ws.call(
            "fauna.folders.update",
            {"name": set_name, "webdav_enabled": True},
        )


@pytest.mark.feature("files-in-standard-apps")
def test_webdav_served_set_read_write_roundtrip(app, dedicated_mail_nest, request):
    """A served, content-keyed Sync set round-trips a file's bytes write→read
    through the real MDA; a served-but-blobless set rejects PUT (403); an unserved
    set is unreachable (404).

    RED before the Go WebDAV terminator's PUT chunk+seal / GET fetch+decrypt paths
    (and the served-set key delivery) landed.
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, nest, _admin_addr = enable_mail_and_client(app, handle, request)

    # Arrange the served-set precondition: an empty, content-keyed, served Sync
    # set "docs" + the MSEK-sealed WebdavKeysBlob carrying its genesis key.
    served = serve_enable_folder(app.driver, "docs", create=True)
    assert served and served >= 1, (
        f"reconcile must carry at least the just-served set; got served_sets={served!r}"
    )

    # The served set is now enumerable at the root (webdav_list_folders).
    root_children = wait_status(lambda: client.propfind_raw("", depth="1"), 207)
    assert root_children.status_code == 207
    assert any("docs" in h for h in client.child_hrefs("", depth="1")), (
        "the served set 'docs' must appear as a child collection of the root"
    )

    # WRITE: PUT a file into the served set → 201 Created. The MDA chunks + seals
    # under the content key, uploads ciphertext + the manifest, records the change.
    secret = f"webdav-roundtrip-secret-{uuid.uuid4().hex}"
    body = (secret + "\n" + ("payload line\n" * 64)).encode()
    put = client.put("docs/hello.txt", body)
    assert put.status_code in (201, 204), (
        f"PUT to the served writable set must create the file (201/204); got "
        f"{put.status_code}\n{put.text[:800]}"
    )

    # READ: GET it back → byte-identical plaintext (the tier_3-only cross-binary
    # encrypt→store→decrypt seal proof: the ciphertext at rest is useless without
    # the content key the MDA holds only via the WebdavKeysBlob).
    got = wait_status(lambda: client.get("docs/hello.txt"), 200)
    assert got.status_code == 200, (
        f"GET of the just-PUT file must be 200; got {got.status_code}\n{got.text[:800]}"
    )
    assert got.content == body, (
        "the round-tripped file body must be byte-identical to the PUT bytes "
        "(chunk seal→store→decrypt); lengths "
        f"{len(got.content)} vs {len(body)}"
    )

    # PROPFIND Depth:1 of the set lists the file with its manifest-hash ETag.
    listing = wait_status(lambda: client.propfind_raw("docs/", depth="1"), 207)
    assert listing.status_code == 207
    assert any("hello.txt" in h for h in client.child_hrefs("docs/", depth="1")), (
        "the PUT file must enumerate in a Depth:1 PROPFIND of the served set"
    )

    # A SECOND file, in a subdirectory (implicit collection) → both enumerate.
    client.put("docs/notes/todo.txt", b"a second file under an implicit dir\n")
    files_seen = wait_status(
        lambda: client.propfind_raw("docs/", depth="1"), 207
    )
    assert files_seen.status_code == 207

    # Fail-closed 403: a set flagged webdav_enabled on nest but ABSENT from the
    # provisioned blob rejects PUT (the write-boundary Guard 2 / FS-BIND-5).
    _flag_served_no_keys(nest, "locked")
    locked = client.put("locked/x.txt", b"must be rejected")
    assert locked.status_code == 403, (
        f"PUT to a served-but-blobless set must fail closed with 403; got "
        f"{locked.status_code}\n{locked.text[:800]}"
    )

    # An entirely unserved set is still unreachable on read (404).
    unserved = f"never-served-{uuid.uuid4().hex[:8]}"
    pf = client.propfind_raw(f"{unserved}/", depth="1")
    assert pf.status_code == 404, (
        f"PROPFIND of an unserved set must be 404; got {pf.status_code}"
    )
