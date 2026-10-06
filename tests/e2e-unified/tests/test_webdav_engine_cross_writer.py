"""tier_3: the chunk plane's two writers — an app's sync engine and the Go
WebDAV MDA — are ONE writer, witnessed across the real binaries.

`test_webdav_read_write_roundtrip.py` pins DAV PUT → DAV GET; both legs run
inside the Go MDA, so it cannot tell whether the MDA seals the way the apps do.
Until 2026-09-03 it did not: the MDA sealed each chunk
RAW through a bare `encrypt_chunk` FFI export while every Rust writer sealed
the FRAMED body (`0x00 ‖ P` / `0x01 ‖ zstd(P)`), under the same deterministic
(key, nonce) — two plaintexts one byte apart under one nonce, a two-time pad
that recovers user content with no key. And the MDA's GET never unframed, so an
app-written file read over DAV came back with its frame bytes. The DAV-only
round trip stayed green through all of it. This module is the cross-writer leg
that could not:

  1. **engine write → DAV GET.** A file dropped into a location bound under the
     served set is chunked + sealed by the REAL `fauna-sync-agent`'s engine and
     uploaded; a real WebDAV client GETs it and must receive the original bytes.
     RED before the fix (the MDA returned `0x01 ‖ zstd(P)`).
  2. **DAV PUT → engine hydration.** A real WebDAV client PUTs a file whose
     first byte is `0x00`; the engine must hydrate it byte-identically into the
     bound location. RED before the fix twice over: the MDA sealed it raw, and
     the apps' shared walk stripped the "frame byte" unconditionally, so the
     whole-file verify failed and the file never arrived.

Three further journeys ride the same bound-location choreography, because each
outcome is about what reaches "your folder" on the device:

  3. **mount edits reach the folder** — DELETE, MOVE (rename, and into a
     subfolder) and COPY through the mount do the same to the bound folder.
  4. **readable after unserving** — a mount-written file, after the app unserves
     the folder (rotating its key away), still hydrates byte-identically into a
     place bound only afterwards, and still lists in Media: unserving first signs
     the mount's rows as the owner's (`writer-signed-change-records.md` ruling
     (7)(b)), and without that every reader drops them once serving is off.
  5. **serving a folder that already holds files** — files the engine synced
     before the folder was served open through the mount byte-identically once
     the app serves it.

Both directions ride the production paths a user would: the bound-location UI
(`folder-location-*`), the agent's own "file uploaded" log line as the upload
witness, and the RFC-4918 client (`helpers/webdav_client.WebDAVClient`).

Runs on **linux, tui, macOS and windows** — the four apps whose real agent's engine
log the driver can read back, so the upload witness resolves, and that carry the
`serve_enable_folder` test command the served-set precondition needs. linux, tui and
macOS direct-spawn the agent as a child whose log is the app's own stderr; macOS does
so only when the launch carries the `real_sync_agent` marker, and without it the app
binds folders with nothing behind them and the upload witness times out (a false
red, not a cross-writer bug). windows spawns the agent DETACHED on the run's own pipe
(`real_sync_agent` + `isolated_sync_agent`) and its driver's `app_stderr_text` reads
that agent's own log next to the app's.

Served-set + bind preconditions are fixture setup (carve-out (b), exactly as in
the round-trip module); the mutations under test are the engine's upload and the
DAV PUT. `webdav-server.md` § Key model + § Implementation status (4b);
`mls-group-key-material.md` § Per-chunk file-sync key.
"""

import uuid
from pathlib import Path

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.waiting import wait_until
from helpers.webdav_roundtrip import (
    enable_mail_and_client,
    serve_enable_folder,
    unthrottled,
    wait_status,
)

# linux + tui + macos + windows: the first three direct-spawn the REAL
# `fauna-sync-agent` as a child whose stderr is the app's own under e2e
# (`drivers/linux.py`, `drivers/tui.py` — each with its own private
# `XDG_RUNTIME_DIR` — and `drivers/macos.py`, whose `app_stderr_text` reads the
# launch's `app.err`, the file the shared `fauna_log` writes into); windows spawns
# it detached, and `drivers/windows.py::app_stderr_text` reads the agent's OWN
# daily log next to the app's. Either way the bound-location choreography and the
# agent-stderr upload witness (`helpers/folder_content`) resolve — the same chain
# `test_public_website_folder_serve.py` already runs on macos and windows. tui
# joined 2026-09-20: its `serve_enable_folder` test command landed 2026-09-19 over
# the SAME `folders::serve_set` its `folder-webdav-toggle` gesture runs
# (`automation.rs`, echoed as `webdav_serve_reply`), so both halves this module
# needs already speak for the column. macos joined 2026-09-21: its
# `serve_enable_folder` (`FaunaMacApp.swift`, over `APIClient.serveFolderWebdav`)
# has served the round-trip module since 2026-07-15. windows joined 2026-09-21:
# its `serve_enable_folder` (`App.WebdavServeEnableFolderForTest`, over the
# production `INestRpcClient.FoldersServeSetAsync`) has served the round-trip
# module since 2026-09-07 — for both a witness gap, not an implementation gap. A
# red on a newly covered app is a genuine product bug there, not test breakage.
# `real_conversations` for the same reason the round-trip module carries it:
# `serve_enable_folder` is built over the live conversations session's MLS engine.
#
# `real_sync_agent` is what makes a macOS launch spawn that child at all
# (`conftest._apply_real_sync_agent_env` sets `FAUNA_E2E_REAL_SYNC_AGENT` only when
# a collected item carries it; FaunaMacApp otherwise builds no provisioner) — a
# no-op on linux/tui, which spawn unconditionally. `isolated_sync_agent` is its
# windows pairing: together they mean "a real agent on this run's OWN pipe + data
# dir" (`conftest.windows_isolates_its_sync_agent`, `_apply_isolated_sync_agent_env`),
# and the data dir is what `app_stderr_text` reads the upload witness from —
# without it the witness sees only the app's own log and times out. It is a no-op
# on macos/linux/tui.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.real_conversations,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
]

def _await_file_bytes(
    path: Path, expected: bytes, *, app, seat: str, window: float = SYNC_WINDOW_SECS
) -> None:
    """Deadline-poll until `path` holds exactly `expected`, else fail naming
    which half broke (conventions point 6: the failure must diagnose itself)."""
    actual: bytes | None = None

    def _arrived() -> bool:
        nonlocal actual
        try:
            actual = path.read_bytes()
        except (FileNotFoundError, OSError):
            actual = None
        return actual == expected

    try:
        wait_until(_arrived, window, interval=1.0)
        return
    except AssertionError:
        pass
    if actual is None:
        detail = "NEVER ARRIVED — the engine never hydrated the DAV-written file"
    else:
        detail = (
            f"content mismatch: {len(actual)} bytes vs {len(expected)} expected; "
            f"head {actual[:8]!r} vs {expected[:8]!r}"
        )
    pytest.fail(
        f"[{seat}] {path.name}: {detail}.\n"
        f"  A DAV-written file that never lands (or lands with different bytes) is "
        f"the MDA sealing a shape the apps' shared walk cannot open — the two "
        f"chunk-plane writers have diverged again.\n"
        f"  app error element: {app.error_text()!r}\n" + agent_diagnosis(app, seat)
    )


@pytest.mark.feature("files-in-standard-apps")
def test_engine_and_webdav_mda_are_one_chunk_writer(
    app, dedicated_mail_nest, request, tmp_path
):
    """An engine-written file reads back over WebDAV byte-identically, and a
    WebDAV-written file (first byte `0x00`) hydrates into the bound location
    byte-identically — the two writers seal, and both readers open, one shape.
    """
    if not (
        app.driver.is_linux()
        or app.driver.is_tui()
        or app.driver.is_macos()
        or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="the bound-location + agent-stderr upload witness choreography",
            detail="helpers/folder_content is the linux+tui+macOS+windows shape "
            "(each app runs the real fauna-sync-agent and its driver reads the "
            "engine log back: off the app's stderr for the three that direct-spawn "
            "it, off the isolated agent's own log for windows); web/iOS/Android "
            "have no bound local folder at all",
            tracked="",
        )

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    # Arrange: a served, content-keyed Sync set "docs" + the MSEK-sealed keys
    # blob for the MDA, then a local location bound under it so the REAL agent's
    # engine writes to and hydrates from the same set the MDA serves.
    served = serve_enable_folder(app.driver, "docs", create=True)
    assert served and served >= 1, (
        f"reconcile must carry at least the just-served set; got served_sets={served!r}"
    )
    bound = tmp_path / "docs-bound"
    bound.mkdir()
    bind_location_under_set(app, "docs", bound, seat="owner")

    # ── 1. engine write → DAV GET ─────────────────────────────────────────
    # Well over the 4 KiB compression floor and highly compressible, so the
    # engine's seal takes the zstd arm (`0x01 ‖ zstd(P)`) — the shape a
    # non-unframing reader returns verbatim.
    engine_name = f"engine-{uuid.uuid4().hex[:8]}.txt"
    engine_body = (
        f"engine-secret-{uuid.uuid4().hex}\n" + ("a line the engine wrote\n" * 600)
    ).encode()
    atomic_write(bound / engine_name, engine_body)
    await_agent_upload(app, engine_name, seat="owner")

    seen: list = []
    got = wait_status(lambda: client.get(f"docs/{engine_name}"), 200, seen=seen)
    assert got.status_code == 200, (
        f"GET of the engine-written file must be 200; got {got.status_code}\n"
        f"{got.text[:800]}\n"
        # The FIRST answer is the diagnosis; a long poll's last answer is often
        # the nest's bridge rate limit (helpers.webdav_roundtrip.DAV_POLL_INTERVAL_S).
        f"  every answer, in order: {seen}\n" + agent_diagnosis(app, "owner")
    )
    assert got.content == engine_body, (
        "an engine-written file must read back over WebDAV byte-identically — "
        f"{len(got.content)} bytes vs {len(engine_body)}, head "
        f"{got.content[:4]!r} vs {engine_body[:4]!r} (a `0x01`/`0x00` head is the "
        "MDA returning the compression frame it never stripped)"
    )

    # ── 2. DAV PUT → engine hydration ─────────────────────────────────────
    # First byte 0x00: by inspection an uncompressed frame, by the manifest's
    # plaintext hash not one. The MDA frames through the one seal door, so the
    # 0x00 rides INSIDE the frame and the readers' framed reading opens it —
    # the manifest's plaintext hash, not the first byte, picks the reading.
    dav_name = f"dav-{uuid.uuid4().hex[:8]}.bin"
    dav_body = b"\x00" + (
        f"dav-secret-{uuid.uuid4().hex}\n" + ("a line the DAV client wrote\n" * 600)
    ).encode()
    put = client.put(f"docs/{dav_name}", dav_body)
    assert put.status_code in (201, 204), (
        f"PUT to the served writable set must create the file (201/204); got "
        f"{put.status_code}\n{put.text[:800]}"
    )
    _await_file_bytes(bound / dav_name, dav_body, app=app, seat="owner")

    assert not app.has_error(), (
        f"the cross-writer round trip surfaced an error: {app.error_text()!r}"
    )


def _require_bound_folder_apps(app):
    if not (
        app.driver.is_linux()
        or app.driver.is_tui()
        or app.driver.is_macos()
        or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="the bound-location + agent-stderr upload witness choreography",
            detail="helpers/folder_content is the linux+tui+macOS+windows shape; "
            "web/iOS/Android have no bound local folder at all — an app without "
            "one witnesses these outcomes through its own folder view "
            "(test_webdav_folder_view.py)",
            tracked="",
        )


def _await_gone(path: Path, *, app, seat: str, window: float = SYNC_WINDOW_SECS) -> None:
    """Deadline-poll until `path` no longer exists in the bound folder."""
    try:
        wait_until(lambda: not path.exists(), window, interval=1.0)
    except AssertionError:
        pytest.fail(
            f"[{seat}] {path.name} is still in the bound folder — a change made "
            f"through the mount never reached the folder on this device.\n"
            f"  app error element: {app.error_text()!r}\n" + agent_diagnosis(app, seat)
        )


def _dav_ok(resp, what: str) -> None:
    assert resp.status_code in (200, 201, 204), (
        f"{what} through the mount must succeed; got {resp.status_code}\n"
        f"{resp.text[:800]}"
    )


@pytest.mark.feature("files-in-standard-apps")
def test_mount_delete_rename_move_and_copy_reach_the_folder(
    app, dedicated_mail_nest, request, tmp_path
):
    """Deleting, renaming, moving into a subfolder and copying a file through
    the mount does the same to it in the folder the app syncs to this device."""
    _require_bound_folder_apps(app)
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)
    served = serve_enable_folder(app.driver, "docs", create=True)
    assert served and served >= 1, f"served_sets={served!r}"
    bound = tmp_path / "docs-bound"
    bound.mkdir()
    bind_location_under_set(app, "docs", bound, seat="owner")

    tag = uuid.uuid4().hex[:8]
    bodies = {
        name: f"{name} {uuid.uuid4().hex}\n".encode() * 50
        for name in (f"keep-{tag}.txt", f"rename-{tag}.txt", f"move-{tag}.txt",
                     f"copy-{tag}.txt", f"delete-{tag}.txt")
    }
    for name, body in bodies.items():
        _dav_ok(unthrottled(lambda: client.put(f"docs/{name}", body)), f"saving {name}")
    for name, body in bodies.items():
        _await_file_bytes(bound / name, body, app=app, seat="owner")

    renamed = f"renamed-{tag}.txt"
    _dav_ok(
        unthrottled(lambda: client.move(f"docs/rename-{tag}.txt", f"docs/{renamed}")),
        "renaming a file",
    )
    _dav_ok(
        unthrottled(
            lambda: client.move(f"docs/move-{tag}.txt", f"docs/sub-{tag}/move-{tag}.txt")
        ),
        "moving a file into a subfolder",
    )
    copied = f"copy-{tag}-copy.txt"
    _dav_ok(
        unthrottled(lambda: client.copy(f"docs/copy-{tag}.txt", f"docs/{copied}")),
        "copying a file",
    )
    _dav_ok(unthrottled(lambda: client.delete(f"docs/delete-{tag}.txt")), "deleting a file")

    # The folder on this device follows each change.
    _await_file_bytes(bound / renamed, bodies[f"rename-{tag}.txt"], app=app, seat="owner")
    _await_gone(bound / f"rename-{tag}.txt", app=app, seat="owner")
    _await_file_bytes(
        bound / f"sub-{tag}" / f"move-{tag}.txt", bodies[f"move-{tag}.txt"],
        app=app, seat="owner",
    )
    _await_gone(bound / f"move-{tag}.txt", app=app, seat="owner")
    _await_file_bytes(bound / copied, bodies[f"copy-{tag}.txt"], app=app, seat="owner")
    _await_file_bytes(
        bound / f"copy-{tag}.txt", bodies[f"copy-{tag}.txt"], app=app, seat="owner"
    )
    _await_gone(bound / f"delete-{tag}.txt", app=app, seat="owner")
    # The untouched file is still there, unchanged.
    _await_file_bytes(bound / f"keep-{tag}.txt", bodies[f"keep-{tag}.txt"], app=app, seat="owner")
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.feature("files-in-standard-apps")
def test_mount_written_files_stay_readable_after_unserving(
    app, dedicated_mail_nest, request, tmp_path
):
    """A file saved through the mount stays readable in the app after the
    folder stops being served: unserving rotates the folder's key away from
    the one the mount sealed with, and a place bound to the folder AFTER that
    still receives the file byte-identically (the back-catalogue converges to
    the owner's own key — `webdav-server.md` § Key model, Revocation)."""
    _require_bound_folder_apps(app)
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    name = f"unserve-{uuid.uuid4().hex[:8]}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    b.toggle_webdav()
    assert not app.has_error(), f"serving raised: {app.error_text()!r}"

    dav_name = f"from-the-drive-{uuid.uuid4().hex[:8]}.txt"
    body = f"written through the mount {uuid.uuid4().hex}\n".encode() * 200
    # The toggle's serve lands before the mount learns it: wait for the folder
    # to open on the mount, then save once.
    opened = wait_status(lambda: client.propfind_raw(f"{name}/", depth="1"), 207)
    assert opened.status_code == 207, f"the served folder never opened: {opened.status_code}"
    _dav_ok(unthrottled(lambda: client.put(f"{name}/{dav_name}", body)), "saving a file")

    # MUTATION (UI): stop serving the folder.
    b.navigate_folders()
    if not b.webdav_toggle_visible():
        b.find_and_expand_folder_until(name, "folder-webdav-toggle")
    b.toggle_webdav()
    assert not app.has_error(), f"unserving raised: {app.error_text()!r}"
    gone = wait_status(lambda: client.propfind_raw(f"{name}/", depth="1"), 404)
    assert gone.status_code == 404, (
        f"the unserved folder must leave the mount first; got {gone.status_code}"
    )

    # Bind a place to the folder only now: every byte it receives comes from
    # the nest, opened with the keys the app holds after unserving.
    bound = tmp_path / "after-unserve"
    bound.mkdir()
    bind_location_under_set(app, name, bound, seat="owner")
    _await_file_bytes(bound / dav_name, body, app=app, seat="owner")

    # …and the Media page still lists it: unserving first signs the mount's
    # rows as the owner's, so no reader drops them once the folder is no
    # longer served (`writer-signed-change-records.md` ruling (7)(b)).
    def _listed_in_media():
        app.media.reenter()
        return dav_name in app.media.item_names()

    wait_until(
        _listed_in_media,
        SYNC_WINDOW_SECS,
        diagnose=lambda: f"Media lists {app.media.item_names()!r}",
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.feature("files-in-standard-apps")
def test_serving_a_folder_that_already_holds_files_reaches_them(
    app, dedicated_mail_nest, request, tmp_path
):
    """A folder that already holds files, served afterwards, makes those files
    reachable through the mount with every byte intact (the one-time re-seal
    of the existing files under the served folder's key —
    `webdav-server.md` § Key model)."""
    _require_bound_folder_apps(app)
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = enable_mail_and_client(app, handle, request)

    name = f"existing-{uuid.uuid4().hex[:8]}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    bound = tmp_path / "existing"
    bound.mkdir()
    bind_location_under_set(app, name, bound, seat="owner")

    # Files the app synced BEFORE the folder was ever served.
    files = {
        f"before-{i}-{uuid.uuid4().hex[:6]}.txt": (
            f"file {i} {uuid.uuid4().hex}\n" + ("synced before serving\n" * 300)
        ).encode()
        for i in range(3)
    }
    for fname, body in files.items():
        atomic_write(bound / fname, body)
    for fname in files:
        await_agent_upload(app, fname, seat="owner")

    # MUTATION (UI): serve the folder now.
    b.navigate_folders()
    b.find_and_expand_folder_until(name, "folder-webdav-toggle")
    b.toggle_webdav()
    assert not app.has_error(), f"serving raised: {app.error_text()!r}"

    for fname, body in files.items():
        seen: list = []
        got = wait_status(
            lambda fname=fname: client.get(f"{name}/{fname}"), 200,
            timeout=SYNC_WINDOW_SECS, seen=seen,
        )
        assert got.status_code == 200, (
            f"a file the folder held before serving must open through the mount; "
            f"GET {fname} answered {got.status_code}\n  every answer: {seen}\n"
            + agent_diagnosis(app, "owner")
        )
        assert got.content == body, (
            f"{fname} must come through the mount byte-identical: "
            f"{len(got.content)} bytes vs {len(body)}"
        )
    listed = {e.name for e in client.propfind(f"{name}/", depth="1") if not e.is_collection}
    assert set(files) <= listed, (
        f"every pre-existing file must list through the mount; listed {sorted(listed)}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"
