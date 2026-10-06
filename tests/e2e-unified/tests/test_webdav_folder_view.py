"""tier_3: what the mount does to a served folder, seen through the app's OWN
folder view — the witness shape for an app with no bound local folder.

`test_webdav_engine_cross_writer.py` proves `files-in-standard-apps` outcomes
5, 8 and 9 through a location bound under the served set, which only the four
desktop apps with a real sync agent have. On a phone "your folder" is the
served set as the app itself lists it: the mount's delete, rename, move and
copy are manifest-level re-records of that set (`webdav-server.md` § Protocol
surface (v1) and deliberate deferrals), and the Media page's folder view lists
the same rows (`fauna.media.list`), so each outcome is reachable there with no
local copy at all (`feature-catalog.md` § Implementation status today, the
2026-09-26 marked-witness settlement). A File Provider domain is the on-demand
face of the same set, not a second one.

  5. **mount edits reach the folder** — a DELETE, a MOVE (rename, and into a
     subfolder) and a COPY through the mount show in the folder view. The view
     names a file by its base name, so the subfolder move is witnessed as the
     file still listed exactly once — moved, neither lost nor duplicated — and
     the mount's own listing pins where it went.
  9. **serving a folder that already holds files** — a file the app put into
     the folder BEFORE it was served (the Media upload, which records into the
     same `sync_changes` plane the engine writes) opens through the mount
     byte-identically once the app serves it.

Outcome 8 (a mount-written file still OPENS in the app after unserving) needs
the app to read a file's bytes, which on apple is `media-item-detail-download-button`
— spec'd for every app, not yet painted there; the
Media seam holds the served set's retired generations since 2026-09-26, so its
phone witness follows that button alone.

Serving and unserving go through the app's own `folder-webdav-toggle`, as
`test_webdav_mount_behaviours.py` does; the WebDAV side is the RFC-4918 client
(`helpers/webdav_client.WebDAVClient`). `real_conversations`: the toggle's
`serve_set` rides the live conversations session's MLS engine.
"""

import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from helpers.webdav_roundtrip import enable_mail_and_client, unthrottled, wait_status

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.ios,
    # android has no bound local folder either (`webdav-server.md` § Implementation
    # status today), and paints every element these journeys drive
    # (`media-folder-filter`, `media-item`, `folder-webdav-toggle`). Its outcome-9
    # leg needs the Media upload, which `MediaActions.upload_file` declares
    # unbuilt there.
    pytest.mark.android,
    pytest.mark.real_conversations,
]

#: A mount write reaches the nest synchronously, but the folder view reads
#: `fauna.media.list` only when the page is ENTERED (`MediaActions.reenter`), so
#: each tick re-enters; the ceiling covers a loaded simulator's page loads.
FOLDER_VIEW_S = 90.0


def _pre_serve_reseal_unwired(app, what: str) -> None:
    """The flipping client's re-seal of a folder's pre-serve files
    (`webdav-server.md` § Key model (c)) runs only when the app's serve face
    names its recording device; windows' does not yet, so on windows a file the
    Media page put into the folder before serving still rests under the owner
    key and the mount cannot open it. Every other app wires it.
    """
    if app_name(app.driver) == "windows":
        skip_unbuilt(
            app.driver,
            surface="the windows serve face's recording device for the pre-serve re-seal",
            detail=what,
            tracked="",
        )


def _dav_ok(resp, what: str) -> None:
    assert resp.status_code in (200, 201, 204), (
        f"{what} through the mount must succeed; got {resp.status_code}\n"
        f"{resp.text[:800]}"
    )


def _create_folder(app, name: str) -> None:
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    assert not app.has_error(), f"creating {name!r} raised: {app.error_text()!r}"


def _serve(app, client, name: str) -> None:
    """Serve `name` with the app's own toggle, then wait for the mount to open
    it (the MDA reads the served set from the nest and its keys from the blob
    the toggle re-provisioned)."""
    b = app.backups
    b.navigate_folders()
    b.find_and_expand_folder_until(name, "folder-webdav-toggle")
    b.toggle_webdav()
    assert not app.has_error(), f"serving {name!r} raised: {app.error_text()!r}"
    opened = wait_status(lambda: client.propfind_raw(f"{name}/", depth="1"), 207)
    assert opened.status_code == 207, (
        f"the served folder {name!r} never opened on the mount: {opened.status_code}"
    )


def _folder_view(app, folder: str) -> list[str]:
    """The folder view's file names for `folder`, read fresh from the nest."""
    media = app.media
    media.reenter()
    media.set_filter(folder)
    return media.item_names()


def _await_folder_view(app, folder: str, want, what: str) -> list[str]:
    """Poll the folder view until `want(names)` holds; fail naming what it
    listed (convention 6)."""
    seen: list[list[str]] = [[]]

    def _holds() -> bool:
        seen[0] = _folder_view(app, folder)
        return want(seen[0])

    wait_until(
        _holds,
        FOLDER_VIEW_S,
        interval=2.0,
        diagnose=lambda: (
            f"{what}: the folder view of {folder!r} lists {sorted(seen[0])!r}; "
            f"error={app.error_text()!r}"
        ),
    )
    return seen[0]


@pytest.mark.feature("files-in-standard-apps")
def test_mount_edits_show_in_the_apps_folder_view(app, dedicated_mail_nest, request):
    """Deleting, renaming, moving into a subfolder and copying a file through
    the mount does the same to it in the folder as the app lists it."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    name = f"view-{uuid.uuid4().hex[:8]}"
    _create_folder(app, name)
    _serve(app, client, name)

    tag = uuid.uuid4().hex[:8]
    keep, rename, move, copy, delete = (
        f"{stem}-{tag}.txt" for stem in ("keep", "rename", "move", "copy", "delete")
    )
    for fname in (keep, rename, move, copy, delete):
        body = f"{fname} {uuid.uuid4().hex}\n".encode() * 50
        _dav_ok(unthrottled(lambda: client.put(f"{name}/{fname}", body)), f"saving {fname}")
    _await_folder_view(
        app, name,
        lambda names: {keep, rename, move, copy, delete} <= set(names),
        "every file saved through the mount must list in the app",
    )

    renamed = f"renamed-{tag}.txt"
    copied = f"copy-{tag}-copy.txt"
    sub = f"sub-{tag}"
    _dav_ok(
        unthrottled(lambda: client.move(f"{name}/{rename}", f"{name}/{renamed}")),
        "renaming a file",
    )
    _dav_ok(
        unthrottled(lambda: client.move(f"{name}/{move}", f"{name}/{sub}/{move}")),
        "moving a file into a subfolder",
    )
    _dav_ok(
        unthrottled(lambda: client.copy(f"{name}/{copy}", f"{name}/{copied}")),
        "copying a file",
    )
    _dav_ok(unthrottled(lambda: client.delete(f"{name}/{delete}")), "deleting a file")

    expected = sorted([keep, renamed, move, copy, copied])
    names = _await_folder_view(
        app, name,
        lambda names: sorted(names) == expected,
        "the folder view must follow the mount's rename, move, copy and delete "
        f"(want exactly {expected!r})",
    )
    assert names.count(move) == 1, (
        f"the file moved into {sub!r} must list once — moved, not copied; {names!r}"
    )
    # Where the moved file went, by the mount's own listing (the view shows base
    # names only).
    moved_to = {e.name for e in client.propfind(f"{name}/{sub}/", depth="1")}
    assert move in moved_to, f"{move!r} must be in {sub!r} on the mount; {moved_to!r}"
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.feature("files-in-standard-apps")
def test_serving_a_folder_the_app_already_filled_reaches_its_files(
    app, dedicated_mail_nest, request, tmp_path
):
    """A file the app put into a folder before serving it opens through the
    mount byte-identically once the app serves the folder (the one-time re-seal
    of the existing files under the served folder's key — `webdav-server.md`
    § Key model)."""
    _pre_serve_reseal_unwired(
        app,
        "a Media upload made before serving rests under the owner key and the "
        "windows serve face does not yet run the flipping client's re-seal",
    )
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    name = f"filled-{uuid.uuid4().hex[:8]}"
    _create_folder(app, name)

    fname = f"before-{uuid.uuid4().hex[:8]}.txt"
    body = (f"put here before serving {uuid.uuid4().hex}\n" * 300).encode()
    picked = tmp_path / fname
    picked.write_bytes(body)
    media = app.media
    media.navigate()
    media.set_filter(name)
    media.upload_file(str(picked))
    _await_folder_view(
        app, name, lambda names: fname in names,
        "the file the app put into the unserved folder must list there",
    )
    assert wait_status(lambda: client.propfind_raw(f"{name}/", depth="1"), 404).status_code == 404, (
        f"precondition: {name!r} is not served yet, so the mount must not open it"
    )

    # MUTATION (UI): serve the folder now.
    _serve(app, client, name)

    seen: list = []
    got = wait_status(lambda: client.get(f"{name}/{fname}"), 200, seen=seen)
    assert got.status_code == 200, (
        f"a file the folder held before serving must open through the mount; "
        f"GET {fname} answered {got.status_code}\n  every answer: {seen}"
    )
    assert got.content == body, (
        f"{fname} must come through the mount byte-identical: "
        f"{len(got.content)} bytes vs {len(body)}"
    )
    listed = {e.name for e in client.propfind(f"{name}/", depth="1") if not e.is_collection}
    assert fname in listed, f"{fname!r} must list through the mount; listed {sorted(listed)}"
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"
