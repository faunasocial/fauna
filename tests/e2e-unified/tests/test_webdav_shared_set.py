"""tier_3: a folder that is both SHARED and SERVED over WebDAV — the owner's
mount and a member's app see the same files, across the real binaries.

`webdav-server.md` § Key model, the custody-note bullet: "A set both shared and
served works unchanged — custody at the real `ChannelId`; members via the MLS
envelope, the MDA via the blob". Since 2026-10-04 (the same doc's § Implementation
status today, and `writer-signed-change-records.md` ruling (7)(b)(ii)) the served
state itself rides that envelope: serving a shared set re-publishes the content-key
envelope carrying the owner's custody stamp, and a member's agent learns from it
that rows the MDA wrote are the owner's to vouch for (`row_judge`'s reader
exemption keys on `binding.webdav_served`). Every other WebDAV journey runs
single-seat, so none of them can see whether a MEMBER's engine opens what the
mount wrote, or whether the mount opens what a member's engine wrote. This one has
two seats:

  * **owner** — the mail venue's claimed admin on the cached `app`, mail enabled
    through its own UI (the MSEK the serve seals the `WebdavKeysBlob` under) and
    the WebDAV client authenticated with that mail credential;
  * **member** — a fresh app launch (tui, linux, macOS, windows or android,
    `_MEMBER_SEATS`) signed in as a handled actor the owner shares the set with
    and promotes to writer (a reader never binds an engine).

Journey, every mutation through the app UI (convention 8): the owner creates the
set, shares it, promotes the member, the member accepts; both bind a place and the
member hydrates an owner-written seed file (the ordinary shared path, so a red
below cannot be misread as a sharing fault). Then the owner flips
`folder-webdav-toggle` ON for the shared set, and:

  (a) a file the member's app syncs into the set GETs byte-identically through
      the owner's mount;
  (b) a file PUT through the mount hydrates byte-identically into the member's
      bound place.

Before (b) the journey waits on a causal barrier, not a settle sleep (convention
14): the member's agent restarting the set's engine after the serve — the served
flag is part of the engine's key-material stamp (`engine_driver.rs`
`engine_stamp`), so that restart is the member's custody ingest of the
re-published envelope arriving at the engine. A red at the barrier or at (b) is a
member-side (7)(b)(ii) defect, not a fixture flake.

**Venue.** `dedicated_caldav_mailbox_less_nest` is the mail venue (real MTA + MDA)
whose registration is OPEN with the handle domain `fauna.test` — the one shape
that can host a second, handled seat. The two-seat folder fixtures
(`folder_share_owner_app` / `folder_share_recipient_app`) sit on `handled_nest`,
which runs no MDA; the member seat here is the same generator those fixtures use
(`conftest._folder_share_recipient_seat`), pointed at this venue.

tui leads (`docs/goal/architecture/testing.md` § Default app and nest mode); the
other bound-folder apps follow by gaining a param here — the owner is the run's
cached `app`, the member a `_MEMBER_SEATS` arm, so every owner × member pairing
the run's app axis can drive is collected (`served_share_member` is in
`conftest._REAL_SECOND_APP_FIXTURES`: both seats must be drivable here). macOS
is a MEMBER column only — the owner promotes the member to writer, and apple
declares no owner-side access-grant UI (`ui/folders.md`), so a macOS owner arm
is a declared absence and the macOS witness is `[tui-macos]`, run with
`--app macos,tui`. macOS spawns its agent only under `real_sync_agent`, which
the module already carries; the restart barrier reads the macOS launch's
`app.err`, where the direct-spawned agent's stderr lands.

linux is an owner AND a member column too — it carries the owner-side
access-grant UI and direct-spawns its agent like tui, so `--app linux` on Linux
collects `[linux-linux]` (and, with `tui` added, the tui × linux pairings); the
restart barrier reads the linux launch's `app.err`, where the child agent's log
interleaves. Its module mark is load-bearing for the OWNER seat, as for windows.

windows is an owner AND a member column — it carries the owner-side
access-grant UI — so `--app windows` on Windows collects `[windows-windows]`, and
adding `tui` collects the tui × windows pairings too. Its module mark is
load-bearing for the OWNER seat: the app-axis filter keeps an item only when the
cached `app`'s value is among the item's marks, and only the windows member arm
brings that mark in on its own — `[windows-tui]` needs the module's. windows
spawns its agent DETACHED, so the module
carries the cross-writer module's pairing: `real_sync_agent` +
`isolated_sync_agent` put the owner's agent on the run's own pipe and data dir
(`real_sync_agent` alone would mean the installed product's per-SID pipe), and
a fresh windows member seat gets a pipe and data dir of its own
(`conftest._own_windows_share_seat_agent`), so two windows seats never share one
agent. The restart barrier reads `app_stderr_text`, which on windows stitches
that seat's detached agent log in. `isolated_sync_agent` is a no-op on the other
apps.

android binds no folder (`architecture/apps/sync-agent.md` § Scope per platform),
so it cannot hold a writer's place and its column is a MEMBER-READS leg of the
same test (`_member_reads_leg`): the same venue and the same share/promote/accept, then the owner
serves the set, writes a file through the mount, and the android member opens
that file on its own Media page and downloads it — the bytes must be the ones the
mount wrote. There is no member bind, no seed hydration, no leg (a) and no
restart barrier there (android runs no sync agent, so there is no engine to
restart): the file painting on the member's Media page and downloading to the
same bytes IS the barrier, the Media-side reader exemption reading the MDA's row
(`writer-signed-change-records.md` ruling (7)(b)(ii) and rule (6)'s digest echo).
Before the serve the member reads the owner's seed the same way, so a red after
the serve cannot be misread as a sharing fault. The owner is the tui seat:
`--app android,tui` collects `[tui-android]` (an android owner is a declared
absence — it binds no place for the seed). The module's `android` mark is what
lets the feature catalog see this test as android's witness: it reads marks off
the module and the test, never off a parametrize arm's own mark.

web is an OWNER column — the witness is `[web-tui]`, run with `--app web,tui`
(and `[web-android]` with `--app web,android`). web binds no local folder
(`webdav-server.md` § Implementation status today), so it cannot hold a writer
member's place; as owner it keeps the outcome whole on the bound-folder member
legs: it enables mail, shares, promotes and serves through its own UI exactly as
tui does, and (a), (b) and the barrier between them stay on the member's real
engine. Only the owner's pre-serve seed changes shape (`_owner_seed`): with no
bound place, a web owner puts it in through the Media page (`file-upload` into
the shared set, the content-keyed `MediaMachine::upload_content_keyed`), and
the member reading it back is the same ordinary-shared-path precondition. web's
node url for the venue is the per-test SPA proxy (`enable_mail_and_client` →
`dedicated_node_url`).
"""

from __future__ import annotations

import secrets
import uuid
from pathlib import Path

import pytest

from actions.media import MediaActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.set_names import find_set
from helpers.waiting import wait_until
from helpers.webdav_roundtrip import enable_mail_and_client, unthrottled, wait_status

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.android,
    # The OWNER seat only (`_MEMBER_SEATS` carries no web arm).
    pytest.mark.web,
    pytest.mark.real_conversations,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
]

# Inbound changes have no push signal on the WS-RPC plane: a member's engine
# learns of a new row on its own rescan tick (30 s under e2e), so a hydration
# window must clear several ticks — the content-sync twin's window.
_HYDRATION_S = 360.0
# The serve's envelope re-publish → the member's receive loop ingests it → the
# custody sink re-pushes the agent's content-key bindings → the engine restarts.
_SERVED_INGEST_S = 240.0


def _read_or_none(path: Path) -> bytes | None:
    try:
        return path.read_bytes()
    except (FileNotFoundError, OSError):
        return None


def _restart_count(app, set_name: str) -> int:
    """How many times this seat's agent has restarted ``set_name``'s engine —
    counted, never merely matched: binding already produces one (the counting
    lesson `test_folder_member_media_decrypt._engine_restart_count` records)."""
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return 0
    return sum(
        1
        for line in text.splitlines()
        if "restarting engine" in line and set_name in line
    )


def _served(nest, name: str) -> bool:
    """The nest-authoritative per-set serve flag, read as the owner — the row
    found by its name hash (a sealed set rests no plaintext name;
    `helpers/set_names`)."""
    admin = nest["admin"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as ws:
        reply = ws.call("fauna.folders.list", {})
    row = find_set(reply.get("folders", []), name)
    return bool(row and row.get("webdav_enabled"))


def _require_owner_seat(app) -> None:
    """The owner seat is the run's cached ``app``; skip, declared, where that app
    cannot be the owner of this journey."""
    from helpers.app_surface import declared_absence

    if app.driver.is_macos() or app.driver.is_ios():
        # The owner promotes the member to writer, and apple carries no
        # owner-side access-grant UI; its column is the MEMBER seat of a tui
        # owner (`[tui-macos]`, the `test_folder_writer_revocation.py` shape).
        declared_absence(
            app.driver,
            capability="owner-side access-grant surface (folder-member-role-select)",
            doc="ui/folders.md § Implementation status today (macOS and iOS have none at all)",
        )
    if app.driver.is_android():
        # The owner binds a place to seal the seed (and, in the bound-folder
        # witness, to read leg (a) back); android binds none. Its column is the
        # MEMBER seat of a tui owner (`[tui-android]`).
        declared_absence(
            app.driver,
            capability="user-bindable folder location (the owner's sealed seed)",
            doc="architecture/apps/sync-agent.md § Scope per platform",
        )


def _share_with_writer_member(owner_app, member_app, member, nest) -> str:
    """Owner creates a set, shares it with ``member`` and promotes them to
    writer; the member accepts the knock and sees the set listed. Every gesture
    through the two apps' UI (the content-sync choreography). Returns the set's
    name."""
    from tests.api import conv_api

    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30,
        interval=1.0,
        diagnose=lambda: (
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the set's MLS group"
        ),
    )

    set_name = f"shared-dav-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    owner_row = ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        20,
        interval=0.5,
        diagnose=lambda: (
            f"the share should land exactly one member; error={owner_app.error_text()!r}"
        ),
    )
    ob.set_member_access("writer", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "writer",
        15,
        interval=0.5,
        diagnose=lambda: (
            "the member must persist as writer — a reader never binds an engine, "
            f"so neither leg could run; error={owner_app.error_text()!r}"
        ),
    )

    mb = member_app.backups
    mb.navigate_folders()
    mb.wait_for_pending_shares(1)
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )

    # The folder list re-fetches on page-VISIBLE, so toggle away and back after
    # each miss (the proven pattern in the content-sync twin).
    def _accepted_set_listed() -> bool:
        if any(set_name in mb.folder_title(i) for i in range(mb.folder_count())):
            return True
        mb.navigate_devices()
        mb.navigate_folders()
        return False

    wait_until(
        _accepted_set_listed,
        90,
        interval=1.0,
        diagnose=lambda: (
            f"[member] the accepted set {set_name!r} never appeared in the folder "
            f"list; error={member_app.error_text()!r}\n"
            + agent_diagnosis(member_app, "member")
        ),
    )
    return set_name


def _owner_seed(owner_app, set_name: str, tmp_path) -> tuple[str, bytes]:
    """The owner puts a seed file into the shared set before any serving —
    through a bound place on an app that binds one, through the Media page on
    web, which binds none (the module docstring's web paragraph). Returns
    ``(seed_name, seed_body)``."""
    seed_name = f"seed-{uuid.uuid4().hex[:8]}.txt"
    seed_body = f"owner seed — {uuid.uuid4().hex}\n".encode()
    if owner_app.driver.is_web():
        picked = tmp_path / seed_name
        picked.write_bytes(seed_body)
        om = owner_app.media
        om.navigate()
        om.ensure_loaded()
        om.set_filter(set_name)
        om.upload_file(str(picked))
        wait_until(
            lambda: seed_name in om.item_names(),
            60,
            interval=1.0,
            diagnose=lambda: (
                f"[owner] the Media upload of {seed_name} into the shared set "
                f"{set_name!r} never listed; items={om.item_names()!r} "
                f"error={owner_app.error_text()!r}"
            ),
        )
        assert not owner_app.has_error(), (
            f"[owner] the seed upload raised an error: {owner_app.error_text()!r}"
        )
        return seed_name, seed_body
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")
    atomic_write(owner_folder / seed_name, seed_body)
    await_agent_upload(owner_app, seed_name, seat="owner")
    return seed_name, seed_body


def _serve_shared_set(owner_app, nest, set_name: str) -> None:
    """MUTATION (UI): the owner flips ``folder-webdav-toggle`` ON for the shared
    set, and the nest's own serve flag confirms it."""
    ob = owner_app.backups
    ob.navigate_folders()
    ob.find_and_expand_folder_until(set_name, "folder-webdav-toggle")
    assert ob.webdav_toggle_enabled(), (
        "folder-webdav-toggle must be interactive on a shared set once mail is "
        f"set up; error={owner_app.error_text()!r}"
    )
    ob.toggle_webdav()
    wait_until(
        lambda: _served(nest, set_name),
        30,
        interval=0.5,
        diagnose=lambda: (
            f"toggling folder-webdav-toggle ON must serve the shared set "
            f"{set_name!r}; error={owner_app.error_text()!r}"
        ),
    )
    assert not owner_app.has_error(), f"serve-on raised an error: {owner_app.error_text()!r}"


# ── the member-reads leg (a member seat that binds no folder) ──────────

# The member's Media listing re-pulls `fauna.media.list` on each re-entry; a row
# the mount just wrote is on the nest at once, so this window covers the paint
# and the member's custody read of the served set, never an engine cycle.
_MEDIA_PAINT_S = _SERVED_INGEST_S
_DOWNLOAD_S = 60.0


def _member_media_reads(member_app, name: str, expected: bytes, window: float, why: str) -> None:
    """Re-enter the member's Media page until ``name`` paints, open it, and
    download it through ``media-item-detail-download-button`` — the bytes the
    member's app saves must be ``expected``. Produced plaintext is proof of the
    content key: a member whose custody lacks the sealing generation fails
    closed (`content_open_roots`), so no plaintext without the key."""
    media = MediaActions(member_app.driver)
    names: list[str] = []

    def _painted() -> bool:
        nonlocal names
        media.reenter()
        media.wait_for_loaded()
        names = media.item_names()
        return name in names

    wait_until(
        _painted,
        window,
        interval=2.0,
        diagnose=lambda: (
            f"[member] {name!r} never painted on the member's Media page (saw "
            f"{names!r}). {why} error={member_app.error_text()!r}"
        ),
    )
    media.open_item_detail(names.index(name))
    assert media.wait_for_version_count(1) >= 1, (
        f"[member] {name}'s version rows never loaded, so the download trigger "
        f"cannot paint; error={member_app.error_text()!r}"
    )
    # `download_open_item` → `wait_for_downloaded_file` re-reads
    # `driver.download_dir()` on every poll: android's is a host mirror of the
    # device's download dir, refreshed only by that call.
    try:
        got = media.download_open_item(timeout=_DOWNLOAD_S)
    except AssertionError as exc:
        pytest.fail(
            f"[member] the download of {name!r} produced no file: {exc}. A "
            "content-key miss fails closed in `content_open_roots`; a member "
            "whose custody never ingested the set's envelope looks like this."
        )
    assert got == expected, (
        f"[member] the downloaded {name!r} is not the bytes written — {len(got)} "
        f"vs {len(expected)}, head {got[:8]!r} vs {expected[:8]!r}. A wrong key "
        "cannot produce plausible plaintext, so this is a chunk-walk fault."
    )


def _member_reads_leg(owner_app, member_app, client, nest, set_name: str, tmp_path) -> None:
    """The column of a member seat that binds no folder (android): the owner's
    seed, then a mount-written file, each read on the member's own Media page.
    No member bind, no seed hydration, no leg (a), no restart barrier — the
    member runs no agent; the mount-written file painting and downloading to the
    same bytes IS the barrier (the module docstring)."""
    # ── 2. the ordinary shared path, before any serving ──────────────────
    seed_name, seed_body = _owner_seed(owner_app, set_name, tmp_path)
    _member_media_reads(
        member_app, seed_name, seed_body, _MEDIA_PAINT_S,
        "Nothing is served yet: the ordinary shared path is broken, so nothing "
        "below can be read as a WebDAV result.",
    )

    # ── 3. MUTATION (UI): the owner serves the shared set ────────────────
    _serve_shared_set(owner_app, nest, set_name)

    # ── the owner's mount writes → the member's Media page reads ─────────
    dav_name = f"dav-{uuid.uuid4().hex[:8]}.bin"
    dav_body = b"\x00" + (
        f"dav-secret-{uuid.uuid4().hex}\n" + ("a line the mount wrote\n" * 600)
    ).encode()
    put = unthrottled(lambda: client.put(f"{set_name}/{dav_name}", dav_body))
    assert put.status_code in (201, 204), (
        f"PUT into the served shared set must create the file (201/204); got "
        f"{put.status_code}\n{put.text[:800]}"
    )
    _member_media_reads(
        member_app, dav_name, dav_body, _MEDIA_PAINT_S,
        "The MDA wrote the row under the owner's custody stamp (ruling "
        "(7)(b)(ii)); a member who read the seed but not this file is refusing "
        "the MDA's row — a member-side defect, not a fixture flake.",
    )


# The member seat's arms — each carries its app mark, so the run's app axis
# selects it (the `_MEMBER_SEATS` shape of `test_folder_member_media_decrypt.py`).
# A seat that binds a folder holds the writer's place and runs both legs; one
# that binds none (android) runs the member-reads leg (`_member_reads_leg`).
_MEMBER_SEATS = [
    pytest.param("tui", marks=pytest.mark.tui),
    pytest.param("linux", marks=pytest.mark.linux),
    pytest.param("macos", marks=pytest.mark.macos),
    pytest.param("windows", marks=pytest.mark.windows),
    pytest.param("android", marks=pytest.mark.android),
]


@pytest.fixture
def served_share_member(request, dedicated_caldav_mailbox_less_nest):
    """A fresh member seat on the mail venue, over ``request.param`` —
    `(member_app, member)`."""
    from conftest import _folder_share_recipient_seat

    seat = request.param
    yield from _folder_share_recipient_seat(
        seat,
        dedicated_caldav_mailbox_less_nest.nest,
        request,
        handle_prefix="member",
        screenshot=f"teardown-{seat}-served-share-member",
    )


# Two GUI seats + two real agents + the mail venue's MTA/MDA, a real MLS
# share/accept, then three engine cycles (seed hydration, served re-key, DAV
# hydration). A ceiling, not an expectation (conventions point 9).
@pytest.mark.timeout(1800)
@pytest.mark.parametrize("served_share_member", _MEMBER_SEATS, indirect=True)
@pytest.mark.feature("files-in-standard-apps")
def test_a_shared_and_served_folder_shows_owner_mount_and_member_the_same_files(
    app, dedicated_caldav_mailbox_less_nest, served_share_member, request, tmp_path
):
    """A folder shared with a member and served over WebDAV: a member-synced file
    reads back byte-identically through the owner's mount, and a file written
    through the mount hydrates byte-identically into the member's bound place —
    or, for a member that binds no folder, downloads byte-identically from the
    member's own Media page."""
    _require_owner_seat(app)

    handle = dedicated_caldav_mailbox_less_nest
    handle.assert_mta_running()
    member_app, member = served_share_member
    owner_app = app

    client, nest, _admin_addr = enable_mail_and_client(owner_app, handle, request)

    request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))

    set_name = _share_with_writer_member(owner_app, member_app, member, nest)

    if member_app.driver.is_android():
        _member_reads_leg(owner_app, member_app, client, nest, set_name, tmp_path)
        assert not member_app.has_error(), (
            f"the member surfaced an error: {member_app.error_text()!r}"
        )
        assert not owner_app.has_error(), (
            f"the owner surfaced an error: {owner_app.error_text()!r}"
        )
        return

    # ── 2. the ordinary shared path, before any serving ──────────────────
    seed_name, seed_body = _owner_seed(owner_app, set_name, tmp_path)

    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    bind_location_under_set(member_app, set_name, member_folder, seat="member")
    wait_until(
        lambda: _read_or_none(member_folder / seed_name) == seed_body,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] the owner's seed {seed_name} never hydrated before any "
            "serving — the ordinary shared path is broken, so nothing below can "
            f"be read as a WebDAV result. error={member_app.error_text()!r}\n"
            + agent_diagnosis(member_app, "member")
        ),
    )
    # The member's engine is running at its keyed, unserved stamp now (it just
    # hydrated a file), so any restart from here on postdates the baseline.
    member_restarts = _restart_count(member_app, set_name)

    # ── 3. MUTATION (UI): the owner serves the shared set ────────────────
    _serve_shared_set(owner_app, nest, set_name)

    # ── (a) member's app writes → owner's mount reads ────────────────────
    member_name = f"member-{uuid.uuid4().hex[:8]}.txt"
    # Over the 4 KiB compression floor so the member's seal takes the zstd arm.
    member_body = (
        f"member-secret-{uuid.uuid4().hex}\n" + ("a line the member wrote\n" * 600)
    ).encode()
    atomic_write(member_folder / member_name, member_body)
    await_agent_upload(member_app, member_name, seat="member")

    seen: list = []
    got = wait_status(
        lambda: client.get(f"{set_name}/{member_name}"), 200,
        timeout=SYNC_WINDOW_SECS, seen=seen,
    )
    assert got.status_code == 200, (
        f"GET through the owner's mount of the member-written {member_name} must "
        f"be 200; got {got.status_code}\n{got.text[:800]}\n"
        f"  every answer, in order: {seen}\n"
        "  The MDA opens a shared set's rows with the blob's keys for the set's "
        "real channel (`owned_custody_channel`); a 404 while the owner's own "
        "files list means the member's row is folded out of the set.\n"
        + handle.bridge_log_hint("mda")
    )
    assert got.content == member_body, (
        "a member-written file must read back through the owner's mount "
        f"byte-identically — {len(got.content)} bytes vs {len(member_body)}, head "
        f"{got.content[:4]!r} vs {member_body[:4]!r}"
    )
    seed = unthrottled(lambda: client.get(f"{set_name}/{seed_name}"))
    assert seed.status_code == 200 and seed.content == seed_body, (
        f"the owner's pre-serve seed must read back through the mount too; got "
        f"{seed.status_code} {seed.content[:40]!r}"
    )

    # ── barrier: the member's engine took the served stamp ───────────────
    wait_until(
        lambda: _restart_count(member_app, set_name) > member_restarts,
        _SERVED_INGEST_S,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the agent never restarted {set_name!r}'s engine after the "
            f"serve (restart count stuck at {member_restarts}). Serving a shared "
            "set re-publishes the content-key envelope carrying the owner's "
            "served stamp (ruling (7)(b)(ii)); the member's custody ingest of it "
            "is what re-keys the engine with `webdav_served`, and without it the "
            "member drops every row the mount writes.\n"
            + agent_diagnosis(member_app, "member")
        ),
    )

    # ── (b) owner's mount writes → member's place hydrates ───────────────
    dav_name = f"dav-{uuid.uuid4().hex[:8]}.bin"
    dav_body = b"\x00" + (
        f"dav-secret-{uuid.uuid4().hex}\n" + ("a line the mount wrote\n" * 600)
    ).encode()
    put = unthrottled(lambda: client.put(f"{set_name}/{dav_name}", dav_body))
    assert put.status_code in (201, 204), (
        f"PUT into the served shared set must create the file (201/204); got "
        f"{put.status_code}\n{put.text[:800]}"
    )

    def _dav_detail() -> str:
        actual = _read_or_none(member_folder / dav_name)
        state = (
            "NEVER ARRIVED"
            if actual is None
            else f"{len(actual)} bytes vs {len(dav_body)}, head {actual[:8]!r}"
        )
        return (
            f"[member] the mount-written {dav_name}: {state}. The member's engine "
            "restarted after the serve, so it holds the served stamp; a row the "
            "MDA wrote that still never lands is the member's row judge refusing "
            "it — a (7)(b)(ii) member-side defect, not a fixture flake. "
            f"error={member_app.error_text()!r}\n"
            + agent_diagnosis(member_app, "member")
        )

    wait_until(
        lambda: _read_or_none(member_folder / dav_name) == dav_body,
        _HYDRATION_S,
        interval=1.0,
        diagnose=_dav_detail,
    )

    assert not member_app.has_error(), (
        f"the member surfaced an error: {member_app.error_text()!r}"
    )
    assert not owner_app.has_error(), (
        f"the owner surfaced an error: {owner_app.error_text()!r}"
    )


