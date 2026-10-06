"""Content sync through the REAL external ``fauna-sync-agent`` on a LOCAL nest.

The tier_3 client-path pin (the
linux app→agent content-key push, code landed). The unit tests
there pin the *mechanism* (the blob is computed, pushed, re-pushed on a custody
event); they do NOT prove the production path — app → agent → engine → nest →
*member* — actually seals under the shared M2 key. That is what this module
proves, and it is exactly the gap the  lesson names: "BUILT" claims verified
at engine level, never through the production client path.

Two tests, deliberately ordered weakest-first so a failure diagnoses itself
(``testing.md`` § conventions point 6):

1. :func:`test_agent_uploads_bound_folder_file` — ONE actor. Binding a folder
   under an ordinary set and dropping a file in it must reach the nest. Proves
   the app→agent→engine→nest **upload** half on a local nest (the multiseat
   suite only ever proved it against the live box, same-actor). Says nothing
   about *which key* sealed it.
2. :func:`test_writer_member_decrypts_owner_upload` — TWO concurrent linux GUI
   apps + their two real agents. The owner shares the set, promotes the member
   to **writer**, and writes a file; the member binds their own folder and must
   materialize the file's bytes. Decryption is the assertion: if the owner's
   agent sealed under ``BackupKey`` (the post-A3-cutover regression
   fixed) the member's M2-keyed agent cannot decrypt and the bytes never land.

Why the member must be a **writer**: ``decide_engine_content_binding``
(``libs/fauna-sync-engine/src/engine_lifecycle.rs``) refuses to run an engine for
a reader — a reader never hydrates content (``file-sync.md`` § Multi-writer, the
readers-never-bind iron rule). So the promotion is a precondition of the test,
not a variation of it.

Both tests pin their fixtures via indirect parametrization: the agent-hosted
engine is the desktop shape, and linux and tui each direct-spawn the real agent
under e2e with a private ``XDG_RUNTIME_DIR`` per launch (``drivers/linux.py``,
``drivers/tui.py``), which is what lets two apps hold two independent agent
sockets in one test. The single-actor upload pin runs on every desktop app there
is — linux, tui, windows and macOS (2026-09-21) — through one shared body,
:func:`_agent_uploads_bound_folder_file`, with only the spawn shape carried by
markers; the two-actor
capstone keeps linux-on-both-seats as its reference and adds one cross-platform
member leg per GUI app that cannot host the linux seat — a tui owner with a
macOS member, and a tui owner with a windows member (2026-09-21). All three
share one body, :func:`_writer_member_decrypts_owner_upload`; only markers and
fixture parametrization differ.
"""

import secrets
import time
from pathlib import Path

import pytest

from helpers.folder_content import (
    SYNC_WINDOW_SECS as _SYNC_WINDOW_SECS,
)
from helpers.folder_content import (
    agent_diagnosis as _agent_diagnosis,
)
from helpers.folder_content import (
    atomic_write as _atomic_write,
)
from helpers.folder_content import (
    await_agent_upload as _await_agent_upload,
)
from helpers.folder_content import (
    bind_location_under_set as _bind_location_under_set,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# The scan cadence is no per-folder choice since phase 5 (2026-08-20): every
# e2e launch ticks at the harness's 30 s `FAUNA_E2E_RESCAN_MS` default
# (`drivers/tui.py` / `drivers/linux.py`; the compile-gated seam is
# `always_resident::rescan_interval`), so a scan-driven path still fires inside
# the poll windows below — the cadence the retired wizard picker used to set.


def _await_file_content(
    path: Path, expected: str, *, seat: str, app, window: float = _SYNC_WINDOW_SECS
) -> None:
    """Poll until ``path`` holds exactly ``expected``, else fail naming the
    broken direction (point 6: the failure must diagnose itself)."""
    deadline = time.monotonic() + window
    actual: str | None = None
    while time.monotonic() < deadline:
        try:
            actual = path.read_text()
        except (FileNotFoundError, OSError):
            actual = None
        if actual == expected:
            return
        time.sleep(1.0)
    if actual is None:
        detail = "NEVER ARRIVED (the member's agent never materialized it)"
    else:
        detail = f"content mismatch: expected {expected!r}, got {actual!r}"
    pytest.fail(
        f"[{seat}] {path.name}: {detail}.\n"
        f"  A file that never arrives while the owner's agent DID report "
        f"uploading it (the assertion just above) is the BackupKey-vs-M2 "
        f"sealing regression: the owner's agent sealed under the wrong key and the "
        f"member's M2-keyed engine cannot decrypt it.\n"
        f"  app error element: {app.error_text()!r}\n" + _agent_diagnosis(app, seat)
    )


def _agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path):
    """Shared body for every app's leg of "a file dropped into a bound folder is
    uploaded by the background helper".

    The weaker of the two pins, and the one that isolates a failure: it exercises
    app → agent → engine → nest **upload** on a LOCAL nest with no second actor
    involved. ``test_bind_location_nested_under_folder`` already proves the
    *binding* reaches the agent's own ``config.toml``; this proves the bound
    engine then actually moves bytes.

    **Why ONE body rather than one per app.** Nothing below is app glue: the
    wizard, the bind and the drop are the same gestures over the same element
    ids, and everything from the bound engine onward is the real
    ``fauna-sync-agent`` and shared Rust. Only how each app's agent is *spawned*
    differs, and that difference is carried by markers on the legs below, never
    by the body.

    If this fails, the two-actor test below cannot be read as a key-sealing
    result — the upload half is broken first.
    """
    app, _nest, _owner = folder_share_owner_app

    set_name = f"agentsync-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = tmp_path / "owner-bound"
    folder.mkdir()
    _bind_location_under_set(app, set_name, folder, seat="owner")

    filename = f"hello-{secrets.token_hex(3)}.txt"
    _atomic_write(folder / filename, "agent upload probe\n")

    _await_agent_upload(app, filename, seat="owner")

    assert not app.has_error(), (
        f"the upload round-trip surfaced an error: {app.error_text()!r}"
    )


@pytest.mark.parametrize("folder_share_owner_app", ["linux", "tui"], indirect=True)
@pytest.mark.tui
@pytest.mark.real_conversations
@pytest.mark.feature("local-folder-sync")
def test_agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path):
    """The direct-spawn pair's leg. linux and tui both run it (tui joined
    2026-09-19): each direct-spawns the real ``fauna-sync-agent`` under e2e with
    its own private runtime dir as an inherited-fd child, so the agent's output
    reaches the app's own captured stderr, which is the witness
    ``await_agent_upload`` reads — no extra marker needed.
    """
    _agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path)


@pytest.mark.parametrize("folder_share_owner_app", ["windows"], indirect=True)
@pytest.mark.windows
@pytest.mark.real_conversations
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_windows_agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path):
    """Windows' leg, and it needs two markers the pair above does not.

    windows spawns ``fauna-sync-agent.exe`` **detached** rather than as an
    inherited-fd child (``drivers/windows.py::app_stderr_text``), so the two
    halves the shared body depends on are both marker-gated: ``real_sync_agent``
    sets ``FAUNA_E2E_REAL_SYNC_AGENT``, without which the bridge launch leaves
    ``HydrationSessionEnabled`` false and NO agent runs at all; and
    ``isolated_sync_agent`` sets ``FAUNA_E2E_SYNC_AGENT_DATA_DIR``, which is
    what lets ``app_stderr_text()`` stitch the detached agent's own rolling log
    into the text ``await_agent_upload`` polls. Without the second one the
    upload could succeed and the witness would still never see it.
    """
    _agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path)


@pytest.mark.parametrize("folder_share_owner_app", ["macos"], indirect=True)
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.real_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_macos_agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path):
    """macOS' leg — one marker more than the direct-spawn pair, one less than
    windows.

    ``real_sync_agent`` is needed because macOS' PRODUCTION spawner is
    launchd (``LaunchdSyncAgentSpawner``), which an e2e launch must not
    bootstrap; the marker sets ``FAUNA_E2E_REAL_SYNC_AGENT``, under which the
    app constructs the shared ``FfiChildAgentSpawner`` instead
    (``libs/fauna-ffi/src/sync_agent_provisioning.rs``). ``isolated_sync_agent``
    is NOT needed, because that child is a plain ``Command::spawn()`` with no
    ``Stdio`` override (``fauna_client_sync::agent_spawner::ChildSpawner``), so
    it inherits the app's own stderr — which is exactly the ``app.err`` file
    ``drivers/macos.py::app_stderr_text`` reads, the same inherited-fd shape
    linux and tui have. Nothing else differs: the wizard, the bind and the drop
    are the shared body's, and everything past the bound engine is the real
    agent and shared Rust.

    macOS as the OWNER seat, unlike ``test_macos_writer_member_decrypts_owner_
    upload`` below, which needs a promote-to-writer UI macOS does not have yet.
    This pin needs no second actor at all, so that gap does not reach it.
    """
    _agent_uploads_bound_folder_file(folder_share_owner_app, tmp_path)


def _writer_member_decrypts_owner_upload(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """The shared body of the writer-member decryption capstone — one flow, three
    seat pairings (linux/linux, tui/macOS, tui/windows).

    Extracted 2026-09-21 when windows joined: the macOS leg had been a verbatim
    copy of the linux one and had already drifted (poll cadences, a reworded
    payload), so a third copy would have made the divergence structural
    (priority #4). Nothing in the flow is platform-specific — every seat-specific
    fact lives in the wrappers' markers and fixture parametrization, which is the
    shape ``_agent_uploads_bound_folder_file`` above already uses for the
    single-actor pin.

    Flow (every mutation driven through the client UI — ``testing.md``
    § conventions point 8): owner creates a sync set → shares it to the member →
    promotes the member to **writer** (the engine precondition) → member accepts
    the pending share → owner binds a folder and writes a file → member binds
    their own folder → the file's bytes must materialize on the member's disk.

    The decryption IS the assertion, and it has caught the wrong-key path twice:
    before the fix the linux app pushed an EMPTY ``content_key_bindings``
    blob, so the *owner's* engine ran unbound and sealed under ``BackupKey``;
    after it, the blob was still resolved from the **owner-scoped**
    ``fauna.folders.list``, which omits shared-with-me sets — so the *member's*
    engine found no entry, ran unbound, and tried the member's own ``BackupKey``
    on the owner's M2-sealed chunks (``aead::Error``). Both are silent wrong-key
    paths, not fail-closed ones, and neither is visible to a unit test of the
    push mechanism — only to this client-path run.
    """
    from tests.api import conv_api

    owner_app, nest, _owner = folder_share_owner_app
    member_app, _nest2, member = folder_share_recipient_app

    # Both seats' agent logs, dumped on EVERY outcome — a pytest-timeout kill
    # unwinds through no `pytest.fail`, so without this the one failure mode
    # that strands the run (a stalled driver call) is also the one that leaves
    # no evidence. Captured stdout is only shown for failing tests.
    request.addfinalizer(lambda: print(_agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(_agent_diagnosis(owner_app, "owner")))

    # The member's login-time KeyPackage publish is best-effort async, and the
    # owner's share must fetch one to admit them to the set's MLS group. Poll the
    # NON-destructive count (a `keypackage_fetch` probe would consume the very
    # package the share then needs).
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if conv_api.keypackage_count(
            nest["port"], member, member["actor_id_hex"]
        ) > 0:
            break
        time.sleep(1)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    else:
        pytest.fail(
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the set's MLS group"
        )

    set_name = f"m2sync-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)

    # -- Share to the member and promote to writer -----------------------
    owner_row = ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])

    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if ob.shared_member_count() == 1:
            break
        time.sleep(0.5)  # sleep-ok: poll cadence of a deadline poll
    assert ob.shared_member_count() == 1, (
        f"the share should land exactly one member; error={owner_app.error_text()!r}"
    )

    # A share defaults the member to Reader; an engine REFUSES to run for a
    # reader (`decide_engine_content_binding`), so the promotion is a
    # precondition of hydration, not a variation of the test. The row repaints
    # asynchronously from the nest's authoritative role row, hence the poll.
    ob.set_member_access("writer", 0, row=owner_row)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if ob.member_access(0, row=owner_row) == "writer":
            break
        time.sleep(0.5)  # sleep-ok: poll cadence of a deadline poll
    assert ob.member_access(0, row=owner_row) == "writer", (
        f"member did not persist as writer (got "
        f"{ob.member_access(0, row=owner_row)!r}); a reader never hydrates "
        f"content, so the pin below would be vacuous. "
        f"error={owner_app.error_text()!r}"
    )

    # -- Member accepts (the tested join path; contact auto-join is unit-only) --
    # The writer-affordance UI itself is proven by
    # `test_writer_member_binds_location`; this test only needs it to work.
    mb = member_app.backups
    mb.navigate_folders()
    mb.wait_for_pending_shares(1)
    mb.accept_pending_share(0)
    still_pending = mb.wait_for_pending_shares(0)
    assert still_pending == 0, (
        f"[member] accepting should consume the knock (join + ack); still "
        f"{still_pending} pending. error={member_app.error_text()!r}"
    )

    # The folder list re-fetches on Folders-page-*visible*, so re-entering a
    # page that is ALREADY visible does not re-fire it — the poll must toggle
    # away and back (the proven pattern in `test_folder_pending_share_accept_
    # decline`). Polling without the toggle reads an empty list forever.
    deadline = time.monotonic() + 90
    seen: list[str] = []
    while time.monotonic() < deadline:
        seen = [mb.folder_title(i) for i in range(mb.folder_count())]
        if any(set_name in t for t in seen):
            break
        time.sleep(1.0)  # sleep-ok: poll cadence of a deadline poll
        mb.navigate_devices()
        mb.navigate_folders()
    else:
        pytest.fail(
            f"[member] the accepted set {set_name!r} never appeared in the "
            f"member's folder list within 90s (saw {seen!r}); "
            f"error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        )

    # -- Owner binds + writes --------------------------------------------
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    _bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")

    filename = f"shared-{secrets.token_hex(3)}.txt"
    payload = f"sealed under the shared M2 key, not BackupKey — {secrets.token_hex(8)}\n"
    _atomic_write(owner_folder / filename, payload)

    # The upload must land on the nest first; if it does not, the member-side
    # failure below would be misread as a decryption failure.
    _await_agent_upload(owner_app, filename, seat="owner")

    # -- Member binds + must decrypt -- THE PIN ---------------------------
    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    _bind_location_under_set(member_app, set_name, member_folder, seat="member")

    # A generous window on purpose: inbound changes have NO push signal on the
    # WS-RPC plane, so the member's engine only learns of the owner's upload on
    # its own `rescan_tick` (cadence = the set's nest-projected
    # `rescan_interval_secs`; `always_resident.rs`). The window must therefore
    # clear several ticks, not one.
    _await_file_content(
        member_folder / filename, payload, seat="member", app=member_app,
        window=360.0,
    )

    assert not member_app.has_error(), (
        f"the member's hydration surfaced an error: {member_app.error_text()!r}"
    )


@pytest.mark.parametrize("folder_share_recipient_app", ["linux"], indirect=True)
@pytest.mark.parametrize("folder_share_owner_app", ["linux"], indirect=True)
@pytest.mark.real_conversations
# Documented-long (testing.md § point 9: bounded always, unbounded never). This
# single test stands up TWO GUI apps + TWO real sync agents against one nest,
# runs a real MLS share/accept, and then waits on two independent engine cycles
# (owner upload, member hydration). pytest.ini's 900s default is too tight for
# the sum; this is a CEILING, not an expectation — a healthy run is far shorter.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_writer_member_decrypts_owner_upload(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """The task-1 regression pin: a writer member's agent decrypts the owner's
    upload under the shared M2 key.

    Two concurrent linux GUI apps, each direct-spawning its own real
    ``fauna-sync-agent`` into a private ``XDG_RUNTIME_DIR``, both on the
    session-scoped ``handled_nest`` (so the member is ``by_handle``-resolvable
    for the share gesture) — linux is the KNOWN-GOOD side on both seats, which
    is why this leg, not one of the cross-platform ones below, is the reference.
    """
    _writer_member_decrypts_owner_upload(
        request, folder_share_owner_app, folder_share_recipient_app, tmp_path
    )


@pytest.mark.parametrize("folder_share_recipient_app", ["macos"], indirect=True)
@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.real_conversations
@pytest.mark.real_sync_agent
# See test_writer_member_decrypts_owner_upload's own timeout comment — same
# shape, two concurrent real agents + a real MLS share/accept.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_macos_writer_member_decrypts_owner_upload(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """The macOS analog, proving a macOS **writer member**'s real agent must now
    resolve + push its own content-key bindings
    (`sync-agent.md` § Content keys are resolved before the first provision),
    not the hardcoded empty blob `APIClient.swift` passed before this fix —
    which made every bound set look owner-only regardless of who bound it.

    **Owner is tui, not macOS** (unlike the linux capstone above). Two reasons,
    both load-bearing: linux cannot run on the macOS machine that owns the apple
    area, and macOS has no owner-side promote-to-writer UI yet
    (`folder-member-role-select` is declared debt on every app but linux, tui
    and windows — `test_folder_member_role_and_cap_editing`'s `skip_unbuilt`).
    tui already resolves/pushes its own content-key blob off the identical
    shared-Rust `compute_engine_key_bindings_blob` linux does
    (`sync-agent.md` § A6), so it is a safe, independently-correct OWNER
    reference; the writer-member folder-bind UI THIS test needs on the macOS
    side is already built (`FoldersContent.swift`'s `writerMemberFolderRow`
    → `MacFolderBindingSection`, proven live by
    `test_writer_member_binds_location` in `test_folders.py`).

    Before the fix: macOS's real agent ran every bound set unbound
    (owner-only `BackupKey`) regardless of sharing, so it could not open
    tui's correctly-M2-sealed upload (`aead::Error`) — the file never lands
    on the macOS member's disk. After the fix: it does.
    """
    _writer_member_decrypts_owner_upload(
        request, folder_share_owner_app, folder_share_recipient_app, tmp_path
    )


@pytest.mark.parametrize("folder_share_recipient_app", ["windows"], indirect=True)
@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.real_conversations
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
# See test_writer_member_decrypts_owner_upload's own timeout comment — same
# shape, two concurrent real agents + a real MLS share/accept.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_windows_writer_member_decrypts_owner_upload(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """The windows analog: a windows **writer member**'s real agent decrypts a
    tui owner's M2-sealed upload.

    **Owner is tui, not windows**, for the same shape the macOS leg uses above:
    linux cannot run on the Windows machine, and tui is an independently-correct
    owner reference off the identical shared-Rust
    `compute_engine_key_bindings_blob`. The writer-member folder-bind UI this
    needs on the windows side is built (`FoldersPage.xaml.cs`'s
    `BuildWriterMemberFolderRow`, landed 2026-08-26 as the fan-out's last app
    and proven live by `test_writer_member_binds_location` in `test_folders.py`).

    Two markers beyond the macOS leg's, for the reason
    `test_windows_agent_uploads_bound_folder_file` documents above: windows
    spawns ``fauna-sync-agent.exe`` **detached** rather than as an inherited-fd
    child, so ``real_sync_agent`` is what makes an agent run at all, and
    ``isolated_sync_agent`` is what lets ``app_stderr_text()`` stitch the
    detached agent's own rolling log into the text ``_await_agent_upload``
    polls on the owner seat's behalf.
    """
    _writer_member_decrypts_owner_upload(
        request, folder_share_owner_app, folder_share_recipient_app, tmp_path
    )
