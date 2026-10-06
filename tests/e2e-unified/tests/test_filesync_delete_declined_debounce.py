"""The delete-vs-edit CONFLICT arm, tier_3: a declined delete keeps the edit.

`file-sync.md` § Conflicts -> *Delete-vs-edit (ratified 2026-07-29)*: a remote
tombstone racing local content the nest lacks must never win, because applying
it would destroy the only copy of those bytes (`principles.md` § No user-data
loss). The engine + badge + guide landed at tier_1; this is the owed end-to-end proof — a real client, a
real `fauna-sync-agent`, a real local nest — that a mid-debounce edit survives
a delete issued through the product's own Media UI.

**The deterministic precondition every naive shape lacked:** without control
over the local-write debouncer, an edit auto-uploads in ~2s (`DEBOUNCE_DELAY`)
-- almost always faster than a test can drive the delete click, so the
KeepConflict arm (`engine.rs`'s `Tombstone::KeepConflict` -> `report_declined_
delete`) is nearly unreachable by chance. `FAUNA_E2E_DEBOUNCE_MS`
(`always_resident.rs::debounce_delay`, compile-gated per testing.md convention
15) holds the live watcher's debounce open indefinitely, so the edit below sits
`LocallyModified` for the entire test instead of racing to `Synced` first.

**Why the rescan tick is ALSO neutered (`FAUNA_E2E_RESCAN_MS`, 24h):** the
periodic catch-up pass (`always_resident::run_watch_loop`'s tick arm) calls the
SAME `LocalWriteHost::converge` -> `reconcile` -> `upload_pending` sequence
used at bind time, entirely independent of the debouncer. Left at its default
cadence it would upload the mid-debounce edit itself well inside a typical test
run and the decline would never fire — so both of the engine's two upload
paths (the live watcher and the rescan tick) must be pushed past the test's own
budget, not just one. Until phase 5 (2026-08-20) this test chose the cadence
through the wizard's frequency picker (`frequency_index=6`, 86400 s); the
de-knob made every seat tick at the 300 s constant regardless — silently
re-arming the second upload path for any run longer than five minutes — so
the cadence now rides the engine's compile-gated test seam
(`always_resident::rescan_interval`), the debounce override's twin.

**How "sync a file to Synced" happens AT ALL under a global huge-debounce
launch:** the file is written to the folder path BEFORE it is bound. Binding
triggers the engine's startup `catch_up_pass` (`engine_lifecycle::run_engine_
loop`), which converges the local half FIRST (`reconcile` finds the pre-
existing file, `upload_pending` uploads it immediately) — a scan-and-upload
path that never touches the live watcher's debounced pipeline. Only a write
that happens AFTER the folder is already bound and watched goes through the
debounced `LocalWrites` path this test is exercising.

**The causal barrier:** `SyncEngine::report_declined_delete` only ever runs
after the engine has evaluated an incoming delete change against the local
row's actual state (`engine.rs`'s `Tombstone::KeepConflict` arm) and found
locally-diverged content — so a `delete_declined` conflict review row appearing
at all is proof the decline happened; the survival assert only runs after that
(convention 14 — no wall-clock assumption). The delete change reaches this same
seat's engine via the same-nest push nudge (`notify_sync_changed` fires
`PushEvent::SyncChanged` at every connected participant of the set, owner
included -- `file-sync.md` § Remote-change nudge), not via the (neutered)
rescan tick.

RED-VERIFY (do one, document which): revert the `KeepConflict` arm in
`engine.rs` (make every settled-row delete `Tombstone::Apply`) and rerun — the
edited file is unlinked and the delete-declined assertions fail; or launch
without `FAUNA_E2E_DEBOUNCE_MS` set and rerun — the edit races the ~2s default
debounce and typically uploads before the delete lands, so the KeepConflict
precondition collapses and the test is flaky-to-vacuous rather than reliably
red (the exact naive-shape failure mode this knob exists to remove).

tui and linux both run it (linux joined 2026-09-19): the arm under test is the
shared engine's (`fauna_sync_engine::engine`), and both apps spawn the same
external `fauna-sync-agent` through the shared `ChildSpawner`, which inherits
the launch environment the two test seams read.

The **windows app** joined 2026-09-21 as its own leg. What stood in its way was
never the product: the skip was keyed on `sys.platform == "win32"` — the
MACHINE — while its stated reason was only ever about the TERMINAL client's
seat, so it also skipped a windows-app run nobody had tried. Rescoped to
`sync_app == "tui"`; the windows leg carries the two markers its detached
agent spawn needs (`test_folder_agent_content_sync.py`'s windows leg documents
the pair).

The **macOS app** joined the same day, as the fourth and last desktop seat, on
one marker rather than two: it spawns through the same shared `ChildSpawner`
the direct-spawn pair uses (so its agent is already on the app's own inherited
stderr), but its PRODUCTION spawner is launchd, which an e2e launch must not
bootstrap — `real_sync_agent` is what swaps that for the child spawner.
"""

import secrets
import sys
import time

import pytest

from actions import ActionLayer
from actions.media import MediaActions
from common.auth import create_actor_and_register
from conftest import (
    _apply_isolated_sync_agent_env,
    _apply_macos_sync_agent_bin,
    _apply_real_sync_agent_env,
    _E2E_LOGIN_DEVICE_ID,
    _seeded_environment,
    get_available_apps,
)
from drivers import create_driver
from helpers.folder_content import agent_diagnosis, atomic_write, bind_location_under_set

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

#: Apps whose seat drives a real `fauna-sync-agent` this test can reach. Grow
#: this and the module's markers together.
_SUPPORTED_APPS = ("tui", "linux")

# >= the 900s per-test timeout ("never fires during the test") -- both the watcher debounce AND the rescan cadence
# must clear it, or their own upload path resolves the edit before the delete
# can be declined. One value drives both seams.
_DEBOUNCE_MS = "86400000"  # 24h

_LISTING_S = 120.0
_CONFLICT_S = 120.0

_P = "declined.txt"
_INITIAL_BODY = "synced content -- the baseline the delete targets\n"
_EDITED_BODY = (
    "edited after sync, never re-uploaded (huge debounce) -- must survive the decline\n"
)


@pytest.fixture(params=[a for a in _SUPPORTED_APPS if a in get_available_apps()])
def sync_app(request):
    """The app under test; its id lands in the test name (``[tui]``/``[linux]``),
    which is what conftest's ``--app`` filter reads."""
    return request.param


def _launch_signed_in(app_name, app_path, nest_instance, actor, request) -> ActionLayer:
    """A launch with the debounce override wired in from the start — the whole
    launch's local-write debouncer is affected, not just one folder."""
    driver = create_driver(app_name)
    environment = {
        **_seeded_environment(request, nest_instance),
        # Diagnosis only: the engine's lines land in the agent's own
        # log under the launch's private data dir, not the app stderr.
        "RUST_LOG": "info,fauna_sync_engine=debug",
        # The knob under test (`always_resident.rs::debounce_delay`).
        "FAUNA_E2E_DEBOUNCE_MS": _DEBOUNCE_MS,
        # Its twin (`always_resident.rs::rescan_interval`): the tick's
        # own converge pass is the SECOND upload path that must clear
        # the test's budget (module docstring) — the debounce override
        # alone is not enough. Overrides the harness's 60 s default.
        "FAUNA_E2E_RESCAN_MS": _DEBOUNCE_MS,
    }
    if app_name == "windows":
        # This module hand-builds its launch config instead of going through
        # `_build_app_config`, which is where a marker normally becomes an env
        # var — so the windows seat's two markers would be inert here. Call the
        # same conftest helpers `_build_app_config` calls, rather than spelling
        # the env names out: `_apply_isolated_sync_agent_env` sets FOUR
        # (pipe name, agent binary, data dir, credential store), and a
        # hand-rolled copy would drift the moment one is added.
        _apply_real_sync_agent_env(environment, request)
        _apply_isolated_sync_agent_env(environment, request)
    elif app_name == "macos":
        # Same inert-marker reason as windows above, and the same two-call
        # shape `_build_app_config`'s macos arm uses — macOS' harness is a PAIR
        # and neither half works alone: `_apply_real_sync_agent_env` swaps
        # launchd (which an e2e launch must never bootstrap) for the shared
        # `FfiChildAgentSpawner`, and `_apply_macos_sync_agent_bin` pins WHICH
        # binary that child is, or the app resolves the box's installed agent
        # off PATH. Omitting the second half is a run that spends its whole
        # budget polling an empty Media listing with no error anywhere
        # (2026-09-21). None of windows' detached-agent plumbing applies: the
        # macOS child inherits the app's own fds.
        _apply_real_sync_agent_env(environment, request)
        _apply_macos_sync_agent_bin(environment, request)
    driver.launch(
        {
            "app_path": app_path,
            "url": nest_instance["url"],
            "environment": environment,
        }
    )
    driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": actor["signing_key"].encode().hex(),
                "handle": "declined-user",
                "actor_id": actor["actor_id_hex"],
                "device_id": _E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        }
    )
    return ActionLayer(driver)


def _await_listing(app, predicate, *, describe: str) -> list[str]:
    media = MediaActions(app.driver)
    deadline = time.monotonic() + _LISTING_S
    names: list[str] = []
    while time.monotonic() < deadline:
        media.reenter()
        names = media.item_names()
        if predicate(names):
            return names
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(
        f"Media listing never showed {describe} within {_LISTING_S:.0f}s "
        f"(last read: {names!r}; error={app.error_text()!r})"
    )


def _await_conflict_row(app, *, describe: str) -> int:
    """Deadline-poll the Folders page's conflict review list — the causal
    barrier for the survival assert below. Away-and-back each read (the
    conflicts field rides the same nav-edge-triggered snapshot as the set
    rows; `test_devices_conflicts.py::_refresh_folders` is the same dance)."""
    deadline = time.monotonic() + _CONFLICT_S
    count = 0
    while time.monotonic() < deadline:
        app.driver.navigate_to("feed")
        app.backups.navigate_folders()
        count = app.driver.count("conflict-file-info")
        if count >= 1:
            return count
        time.sleep(1.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(
        f"no {describe} within {_CONFLICT_S:.0f}s (last conflict-file-info count: "
        f"{count}); error={app.error_text()!r}\n"
        f"{agent_diagnosis(app, 'declined-writer')}"
    )


def _declined_delete_of_a_mid_debounce_edit_keeps_the_file(
    sync_app, nest_instance, tmp_path, request
):
    """The shared body — one flow, one seat, whichever app `sync_app` names.

    Split from its wrapper 2026-09-21 when windows joined: the windows seat
    needs the `real_sync_agent` + `isolated_sync_agent` markers and the
    tui/linux seats must NOT carry them (both are read SESSION-wide, so a
    marker on the shared function would make an `--app tui` run on a Linux dev
    machine try to build `fauna-sync-agent.exe`). Same shape as
    `test_folder_agent_content_sync.py`'s direct-spawn/detached split.
    """
    app_path = request.getfixturevalue(f"{sync_app}_app_path")

    # A DEDICATED actor (admin-admitted — the shared nest's registration is
    # closed), so the conflict review list is exactly this test's one row.
    actor = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    set_name = f"declined-{secrets.token_hex(4)}"

    # Pre-seed BEFORE binding: the bind-time `catch_up_pass` converges the
    # local half first (reconcile finds the pre-existing file, upload_pending
    # uploads it immediately) — the only path to Synced that never touches the
    # live watcher's (huge, for this whole launch) debounce.
    folder = tmp_path / "declined"
    folder.mkdir()
    atomic_write(folder / _P, _INITIAL_BODY)

    app = _launch_signed_in(sync_app, app_path, nest_instance, actor, request)
    try:
        b = app.backups
        b.navigate_folders()
        app.driver.wait_for("folder-add-button", timeout=30)
        # The rescan tick is neutered at launch (`FAUNA_E2E_RESCAN_MS` above),
        # not per folder: the cadence is no per-folder choice since phase 5.
        b.create_folder_via_wizard(set_name)
        bind_location_under_set(app, set_name, folder, seat="declined-writer")

        _await_listing(
            app,
            lambda names: _P in names,
            describe=f"the pre-seeded upload ({_P!r} via bind-time converge)",
        )

        # Now Synced. Edit on disk: the live watcher (huge debounce in effect
        # for this whole launch) touches the debouncer and never fires during
        # the test — the deterministic mid-debounce precondition.
        atomic_write(folder / _P, _EDITED_BODY)

        # Delete via the Media UI — `fauna.sync.delete_member` records a
        # tombstone on the nest with NO local filesystem interaction
        # (`MediaMachine::delete` is a pure nest-API call). It reaches back to
        # THIS seat's own engine over the same-nest push nudge and is
        # evaluated by the SAME apply-delete guard any peer's delete would
        # hit — self-issued or not, the delete change is processed uniformly.
        media = MediaActions(app.driver)
        media.reenter()
        names = media.item_names()
        assert _P in names, (
            f"{_P!r} missing from the Media listing before the delete attempt "
            f"(names={names!r}); error={app.error_text()!r}"
        )
        media.open_item_detail(names.index(_P))
        media.delete_open_item()

        # The delete gesture fails (not silently) on the MEDIA page's own
        # `error-message` on a client-side issue (e.g. no local sync device id
        # yet) — must be read HERE, before any navigation, or it is gone.
        assert not app.has_error(), (
            f"the delete gesture surfaced a page error instead of recording "
            f"the tombstone: {app.error_text()!r}"
        )

        # Causal barrier: report_declined_delete only ever runs once the
        # engine has evaluated the tombstone against the mid-debounce edit and
        # found it locally-diverged (engine.rs Tombstone::KeepConflict) — the
        # row's mere existence proves the decline ran.
        _await_conflict_row(app, describe="the delete_declined conflict review row")

        # Survival: the file exists with the EDITED (never re-uploaded)
        # content — no-user-data-loss is the whole point of the guard.
        assert (folder / _P).exists(), (
            f"[delete-vs-edit] {_P!r} is GONE — the declined delete destroyed "
            f"the mid-debounce edit despite a delete_declined review row "
            f"(principles.md § No user-data loss)"
        )
        assert (folder / _P).read_text() == _EDITED_BODY, (
            f"[delete-vs-edit] {_P!r} no longer holds the edited content "
            f"(got: {(folder / _P).read_text()!r}) — something else won"
        )
    finally:
        app.driver.teardown()  # reaps the launch's agent (point 9/10)


@pytest.mark.feature("local-folder-sync")
def test_declined_delete_of_a_mid_debounce_edit_keeps_the_file(
    sync_app, nest_instance, tmp_path, request
):
    """The direct-spawn seats (tui, linux): each spawns the real agent as an
    inherited-fd child under a private runtime dir, so no marker is needed."""
    _declined_delete_of_a_mid_debounce_edit_keeps_the_file(
        sync_app, nest_instance, tmp_path, request
    )


@pytest.mark.parametrize("sync_app", ["macos"], indirect=True)
@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_macos_declined_delete_of_a_mid_debounce_edit_keeps_the_file(
    sync_app, nest_instance, tmp_path, request
):
    """The macOS seat — ONE marker, not the windows leg's two.

    macOS spawns the agent through the same shared `ChildSpawner` the
    direct-spawn pair uses, so its output is already on the app's own
    inherited stderr. What it does NOT share with them is the default spawner:
    production macOS bootstraps launchd, which an e2e launch must not touch, so
    `real_sync_agent` is what swaps in `FfiChildAgentSpawner`
    (`libs/fauna-ffi/src/sync_agent_provisioning.rs`). `isolated_sync_agent` is
    windows-only plumbing for a DETACHED agent and does not apply here —
    `test_folder_agent_content_sync.py`'s macOS leg documents the same split.
    """
    _declined_delete_of_a_mid_debounce_edit_keeps_the_file(
        sync_app, nest_instance, tmp_path, request
    )


@pytest.mark.parametrize("sync_app", ["windows"], indirect=True)
@pytest.mark.windows
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_windows_declined_delete_of_a_mid_debounce_edit_keeps_the_file(
    sync_app, nest_instance, tmp_path, request
):
    """The windows seat, and it needs two markers the direct-spawn pair does not.

    The skip this leg replaces was keyed on ``sys.platform == "win32"`` while
    its stated reason was only ever about the TERMINAL client's seat — so it
    also skipped a windows-app run that had never been tried. Rescoped to the
    tui seat above (2026-09-21); the windows app drives a real agent perfectly
    well, it just spawns ``fauna-sync-agent.exe`` DETACHED rather than as an
    inherited-fd child, which is what the two markers are for
    (``test_folder_agent_content_sync.py``'s windows leg documents the pair).
    """
    _declined_delete_of_a_mid_debounce_edit_keeps_the_file(
        sync_app, nest_instance, tmp_path, request
    )
