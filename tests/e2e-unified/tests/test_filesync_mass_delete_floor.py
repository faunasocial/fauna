"""A wholesale-vanished folder is HELD, surfaced, and propagated only on a click.

The app-facing leg of the mass-delete floor (``delete-propagation.md`` § A
wholesale-vanished folder is infrastructure failure, ratified 2026-08-02; the
verb half 2026-08-15). The floor itself, the ``deletes_held`` status projection
and the ``ApplyHeldDeletes`` verb are each tier_1-pinned in the engine and the
agent; what NO test reached until now is the three links between them and a
user: the agent's roster reaching the app's binding model, the row rendering the
hold, and the button turning that hold into recorded deletes.

**The vanish must not look like a stream of deletes.** A per-file ``unlink()``
sweep is genuine per-file evidence — the watcher delivers a ``Removed`` for each
one and they propagate, by design, with no floor involved. So this test
reproduces the shape the floor exists for, which is also the 2026-07-24 incident
shape: the bound directory is **renamed away and an empty one recreated at the
same path**. inotify follows the moved inode, so no per-child event is ever
delivered; the next reconcile scans the bound PATH, finds the root present and
every tracked file gone, and holds. Deleting the files individually instead
would make this test pass vacuously against a build with no floor at all.

The waits are named budgets over deadline polls (convention 14), sized for a
60 s scan cadence on a heavily loaded machine (the harness's
``FAUNA_E2E_RESCAN_MS`` default is 30 s — the cadence is no per-folder choice
since phase 5, and the seam is the only way to set it): a green
run pays only the real latency, and no assertion depends on when anything
happens — only on the state that must eventually hold.

Two witnesses, deliberately on opposite sides of the app:

* the **surface**, ``folder-location-deletes-held`` naming the count and
  ``folder-location-apply-deletes-button`` offering the verb — and, before the
  click, the nest listing STILL holding all three files, which is what proves
  the hold actually held rather than the deletes merely being slow;
* the **effect**, the nest listing dropping all three after the click. Without
  it the test would pass against a button that clears its own line and does
  nothing — the exact failure a UI-only assertion cannot see.
"""

import os
import secrets
import time

import pytest

from actions.media import MediaActions
from helpers.app_surface import app_name, skip_environment, skip_unbuilt
from helpers.folder_content import atomic_write, bind_location_under_set

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.macos, pytest.mark.windows]

# The bind starts the engine and the watcher drives the uploads; the 60 s scan
# cadence is the backstop for both. The hold budget covers a full missed cadence
# plus the app's own 10 s agent-status tick (which is what folds the roster onto
# the rendered row), with room to spare on a loaded box.
_UPLOAD_S = 150.0
_HOLD_S = 260.0
_CLEAR_S = 90.0
_PROPAGATED_S = 150.0
# How long a GUI app may take to land the Media listing after a re-entry.
_LISTED_S = 60.0

# Three files, comfortably above MASS_DELETE_FLOOR_MIN = 2. A one-file set is
# deliberately outside the floor (its absence is far likelier a genuine delete),
# so a smaller fixture would test the opposite rule.
_FILE_COUNT = 3


def _await_listing(app, predicate, *, describe: str, budget: float) -> list[str]:
    """Deadline-poll the Media listing until ``predicate(names)`` holds.

    One :meth:`MediaActions.reenter` per read: the listing pulls the nest on a
    nav EDGE only, so polling without leaving the page re-reads the first
    snapshot forever.
    """
    media = MediaActions(app.driver)
    deadline = time.monotonic() + budget
    names: list[str] = []
    while time.monotonic() < deadline:
        media.reenter()
        names = media.item_names()
        if predicate(names):
            return names
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(
        f"Media listing never showed {describe} within {budget:.0f}s "
        f"(last read: {names!r}; error={app.error_text()!r})"
    )


def _await_still_listed(app, names) -> None:
    """The load-bearing negative after a hold renders: NOTHING was recorded, so
    every file still lists on the nest.

    A deadline poll, never one read: a GUI app loads the Media listing
    asynchronously after :meth:`MediaActions.reenter`, so a single read can see
    the page before its load lands — an EMPTY listing that reads exactly like
    "the nest lost every file" (measured on windows 2026-09-29: the one-shot read
    this replaced failed the always-resident leg with ``listing: []``). Polling
    for "all present" stays sound: a recorded delete never comes back, so a build
    whose floor did not hold still fails, at the budget."""
    _await_listing(
        app,
        lambda listed: all(n in listed for n in names),
        describe=(
            "every held file still on the nest — a miss means the floor recorded "
            "the deletes it was supposedly HOLDING (the hold is decorative)"
        ),
        budget=_LISTED_S,
    )


def _reopen(app, set_name: str) -> int:
    """Land on Folders with ``set_name`` expanded, and return its row index.

    ``find_and_expand_folder`` TOGGLES the expander, so calling it in a poll
    loop alternately opens and closes the very body being polled. Guard on the
    nested binding form the same way :func:`bind_location_under_set` does, so
    this is idempotent from either prior state.
    """
    b = app.backups
    b.navigate_folders()
    app.driver.wait_for("folder-add-button", timeout=30)
    for i in range(b.folder_count()):
        if set_name in b.folder_title(i):
            if not app.driver.is_visible("folder-location-path-input"):
                b.expand_folder(i)
            return i
    raise AssertionError(f"folder {set_name!r} not found among the rows")


def _make_always_resident(app, idx: int) -> None:
    """Flip a fresh on-demand binding to always-resident, through the row's switch.

    A fresh windows binding starts on-demand (on-demand-files.md § On-Demand
    Files, user ruling 2026-09-26), which makes the bound directory itself a
    CfApi sync root, and that registration outlives the agent's connection until
    unbind or a mode change. The cloud filter then refuses the rename-away this
    test stages as its vanish (``WinError 395``, access to the cloud file is
    denied). The floor under test is the always-resident engine's rule, so the
    fixture flips the binding the way a user would, with the per-binding
    ``folder-location-mode-toggle``. The mode change is what unregisters the
    root. On a host whose rows render no toggle (apple, linux) a binding is
    always-resident already, so there is nothing to do.
    """
    scope = f"folder-row[{idx}]"
    if app.driver.count("folder-location-mode-toggle", scope=scope) == 0:
        return
    loc = app.sync_locations
    if loc.mode_toggle_state(scope=scope) != "on-demand":
        return
    after = loc.toggle_mode(scope=scope)
    assert after == "always", (
        f"could not flip the fresh binding to always-resident (read {after!r}); "
        f"error={app.error_text()!r}"
    )


def _held_text(app, idx: int) -> str | None:
    """The row's ``folder-location-deletes-held`` line, or ``None`` when absent.

    Absent is the ordinary reading — the element renders only while the hold
    stands — so this must distinguish "not there" from "there and empty" rather
    than letting a driver miss read as a blank string.
    """
    scope = f"folder-row[{idx}]"
    if app.driver.count("folder-location-deletes-held", scope=scope) == 0:
        return None
    return app.driver.get_text("folder-location-deletes-held", scope=scope)


def _await_hold(app, set_name: str, count: int) -> None:
    """Deadline-poll until the row surfaces the hold, and assert it names
    ``count`` and offers its verb. Both elements ride one condition, so a run
    where the line paints and the button does not is a real defect, not a race
    — they are asserted together."""
    deadline = time.monotonic() + _HOLD_S
    held = None
    idx = -1
    while time.monotonic() < deadline:
        idx = _reopen(app, set_name)
        held = _held_text(app, idx)
        if held is not None:
            break
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    assert held is not None, (
        f"the bound folder emptied wholesale and no hold was ever surfaced within "
        f"{_HOLD_S:.0f}s — either the floor did not engage (the deletes propagated "
        f"silently, which is the data loss it exists to prevent) or the count never "
        f"reached the row; error={app.error_text()!r}"
    )
    assert str(count) in held, f"the hold line does not name the held count {count}: {held!r}"
    assert (
        app.driver.count("folder-location-apply-deletes-button", scope=f"folder-row[{idx}]")
        == 1
    ), (
        "the hold rendered without its verb — the user is told their folder "
        "emptied and given no way to act on it"
    )


def _apply_and_await_clear(app, set_name: str) -> None:
    """Click the hold's verb and deadline-poll until the surface retracts. The
    app sends only the set; the agent re-derives what is actually missing at
    click time."""
    idx = _reopen(app, set_name)
    app.driver.click("folder-location-apply-deletes-button", scope=f"folder-row[{idx}]")
    deadline = time.monotonic() + _CLEAR_S
    while time.monotonic() < deadline:
        idx = _reopen(app, set_name)
        if _held_text(app, idx) is None:
            return
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(
        f"the hold line survived the apply — the row still offers to delete files "
        f"the nest no longer holds; error={app.error_text()!r}"
    )


def _unreadable_text(app, idx: int) -> str | None:
    """The row's ``folder-location-unreadable`` line, or ``None`` when absent —
    the same absent-versus-empty distinction :func:`_held_text` draws."""
    scope = f"folder-row[{idx}]"
    if app.driver.count("folder-location-unreadable", scope=scope) == 0:
        return None
    return app.driver.get_text("folder-location-unreadable", scope=scope)


@pytest.mark.real_sync_agent
# BOTH real-agent markers, never just one (conftest `_apply_isolated_sync_agent_env`:
# "a real-agent UI test needs BOTH markers"; the same pairing at
# test_filesync_multiseat_live.py's pytestmark). Carrying only `real_sync_agent` is
# what made this test intermittent on windows — see the docstring paragraph below.
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_a_vanished_folder_is_held_surfaced_and_propagated_only_on_confirm(
    logged_in_app, tmp_path
):
    """Bind a folder, empty it wholesale, and drive the held→applied journey.

    ``real_sync_agent`` (macOS/windows only; linux/tui ignore it) — without it
    the macOS app binds folders with nothing behind them: correct for
    binding-UI tests, fatal here, since the whole journey depends on real
    bytes actually moving (conftest.py's own note on the marker, citing the
    2026-07-24 multiseat run this bit).

    ``isolated_sync_agent`` is the other half of that pair, and on windows it is
    load-bearing for *this* test specifically. ``real_sync_agent`` only gates
    whether the hydration loop runs at all; the agent is spawned by the APP
    (``SpawnSyncAgentDetached``), and without this marker the app hands it no
    pipe/binary/data-dir pins, so it rendezvouses on the machine-global
    ``\\\\.\\pipe\\fauna-sync.<SID>`` and keeps state in the shared
    ``%LOCALAPPDATA%\\Fauna\\sync`` (conftest.py's note on the marker). Any
    sibling checkout's or the installed product's agent then shares that
    rendezvous and can reconcile this run's bound folder — which is exactly the
    failure this test saw: the hold correctly engaged and rendered, and the nest
    listing went empty anyway, i.e. someone else's agent applied the deletes the
    floor was holding. That is a box-state dependency, and convention 10
    (``e2e-conventions.md`` § An app launch is isolated from the box it runs on)
    says to delete it rather than guard or serialize on it — hence the marker,
    not a pre-run process check.
    """
    app = logged_in_app
    token = secrets.token_hex(4)
    set_name = f"floor-{token}"
    names = [f"floor-{token}-{i}.txt" for i in range(_FILE_COUNT)]

    b = app.backups
    b.navigate_folders()
    app.driver.wait_for("folder-add-button", timeout=30)
    b.create_folder_via_wizard(set_name)

    bound = tmp_path / "bound"
    bound.mkdir()
    idx = bind_location_under_set(app, set_name, bound, seat="floor")
    _make_always_resident(app, idx)

    for i, name in enumerate(names):
        atomic_write(bound / name, f"floor fixture {token} #{i}\n")
    _await_listing(
        app,
        lambda listed: all(n in listed for n in names),
        describe=f"all {_FILE_COUNT} fixture files uploaded",
        budget=_UPLOAD_S,
    )

    # The wholesale vanish, WITHOUT per-file watcher evidence: move the bound
    # directory aside and put an empty one back at the same path. This is an
    # unmounted volume as far as the engine can tell — the root is there and
    # everything it tracked is gone at once.
    os.rename(bound, tmp_path / "stashed")
    bound.mkdir()

    # The floor holds it, and the row says so.
    _await_hold(app, set_name, _FILE_COUNT)

    # ⚠ The load-bearing negative, and the reason this test is not vacuous:
    # NOTHING was recorded. A build whose floor never engaged would have
    # propagated all three by now, so this read is what separates "held" from
    # "slow". It anchors to a causal barrier, never to elapsed time — the hold
    # rendering above already proves a reconcile pass ran over the empty root.
    _await_still_listed(app, names)

    # The explicit user action; the surface retracts (the reply's remaining_held
    # is 0)...
    _apply_and_await_clear(app, set_name)

    # ...and the deletes actually reached the nest, which is the half a
    # UI-only assertion cannot see.
    _await_listing(
        app,
        lambda listed: not any(n in listed for n in names),
        describe="every held delete propagated after the confirm",
        budget=_PROPAGATED_S,
    )


@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_an_unreadable_subtree_is_surfaced_and_nothing_is_deleted(logged_in_app, tmp_path):
    """Part of a bound folder goes unreadable: the row says so, offers nothing,
    and the nest keeps every file (``delete-propagation.md`` § Unreadable is not
    absent — rule (3), and the 2026-09-26 Surface note).

    The fault is a real one, not a simulation: a synced subdirectory goes mode
    ``000``, which is exactly the lost-permissions case the delete rail used to
    read as *every file under it deleted*. Three witnesses, on both sides:

    * the **surface**, ``folder-location-unreadable`` naming the count — and NO
      ``folder-location-apply-deletes-button`` and no hold line, because there is
      nothing to confirm (an apply verb here would be the bug);
    * the **nest**, still listing every file under the unreadable directory,
      read only after the line rendered — the line proves a pass ran over the
      unreadable prefix, so this is a causal barrier, not elapsed time;
    * the **retraction**: permissions restored, the next pass reports zero and
      the line goes away on its own.
    """
    app = logged_in_app
    if app_name(app.driver) not in ("tui", "macos", "windows"):
        skip_unbuilt(
            app.driver,
            surface="folder-location-unreadable",
            detail="the count reaches this app's binding rows via the shared fold; the render is owed",
            tracked="delete-propagation.md § Unreadable is not absent (the Surface note names the owed renders)",
        )
    if os.name != "posix":
        skip_environment("mode bits cannot make a directory unreadable on this OS")
    if os.geteuid() == 0:
        skip_environment("running as root, which ignores mode bits — the fault cannot be produced")

    token = secrets.token_hex(4)
    set_name = f"unread-{token}"
    names = [f"unread-{token}-{i}.txt" for i in range(_FILE_COUNT)]

    b = app.backups
    b.navigate_folders()
    app.driver.wait_for("folder-add-button", timeout=30)
    b.create_folder_via_wizard(set_name)

    bound = tmp_path / "bound"
    bound.mkdir()
    idx = bind_location_under_set(app, set_name, bound, seat="unread")

    sub = bound / "sub"
    sub.mkdir()
    for i, name in enumerate(names):
        atomic_write(sub / name, f"unreadable fixture {token} #{i}\n")
    # One readable sibling, so the set's root is healthy and only the prefix is
    # lost — the case the floor's arithmetic must stay out of.
    atomic_write(bound / f"unread-{token}-root.txt", "readable sibling\n")
    _await_listing(
        app,
        lambda listed: all(n in listed for n in names),
        describe=f"all {_FILE_COUNT} files under the subdirectory uploaded",
        budget=_UPLOAD_S,
    )

    os.chmod(sub, 0o000)
    try:
        # Fixture precondition, asserted: the directory really cannot be read.
        with pytest.raises(PermissionError):
            os.listdir(sub)

        deadline = time.monotonic() + _HOLD_S
        line = None
        while time.monotonic() < deadline:
            idx = _reopen(app, set_name)
            line = _unreadable_text(app, idx)
            if line is not None:
                break
            time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
        assert line is not None, (
            f"part of the bound folder went unreadable and no line was surfaced within "
            f"{_HOLD_S:.0f}s — the count never reached the row; error={app.error_text()!r}"
        )
        assert str(_FILE_COUNT) in line, (
            f"the line does not name the withheld count {_FILE_COUNT}: {line!r}"
        )
        scope = f"folder-row[{idx}]"
        assert app.driver.count("folder-location-apply-deletes-button", scope=scope) == 0, (
            "an unreadable path offered an apply verb — there is nothing to confirm, "
            "and that button would delete files that are still on disk"
        )
        assert _held_text(app, idx) is None, (
            "an unreadable path surfaced as a mass-delete hold — they are different facts"
        )

        media = MediaActions(app.driver)
        media.reenter()
        listed = media.item_names()
        assert all(n in listed for n in names), (
            f"the nest lost files that were only unreadable (listing: {listed!r}) — "
            f"a read failure was recorded as a delete"
        )
    finally:
        os.chmod(sub, 0o755)

    deadline = time.monotonic() + _HOLD_S
    while time.monotonic() < deadline:
        idx = _reopen(app, set_name)
        if _unreadable_text(app, idx) is None:
            break
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    assert _unreadable_text(app, idx) is None, (
        f"permissions were restored and the line never retracted; "
        f"error={app.error_text()!r}"
    )


# The on-demand leg's waits, on top of the budgets above: the pin reaction that
# frees a file's bytes rides the watcher's debounce (or, missing it, the rescan
# tick's pin sweep); the respawn rides the app's 30 s convergence tick
# (`fauna_ipc::convergence::DEFAULT_TICK_INTERVAL`) plus its spawn grace.
_STATUS_S = 150.0
_RESPAWN_S = 120.0
# The staging's respawn race (see `_vanish_disconnected_root`) is retried, never
# waited out: each attempt is a few hundred milliseconds against a 30 s tick.
_STAGE_ATTEMPTS = 3


def _await_status(pipe: str, paths, want: str, *, describe: str) -> None:
    """Deadline-poll the agent's ``GetFileStatus`` until every path reads
    ``want`` — the engine's own row state, which is the floor's universe."""
    from helpers.windows_sync_agent import file_status

    deadline = time.monotonic() + _STATUS_S
    got: dict[str, str] = {}
    while time.monotonic() < deadline:
        got = {str(p): file_status(pipe, str(p)) for p in paths}
        if all(v == want for v in got.values()):
            return
        time.sleep(1.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(f"{describe}: the agent never read {want!r} within {_STATUS_S:.0f}s (last: {got!r})")


def _vanish_disconnected_root(pipe: str, bound, tmp_path) -> None:
    """Stage `delete-propagation.md` § *The floor on an on-demand root* point
    (3)'s vanish that reaches the floor: with the agent DOWN, the bound directory
    is replaced by an empty one — the crash-survivor shape (a drive swapped, a
    restore tool, a user's own move while nothing ran).

    The rename-away is refused on a registered root only while its provider is
    CONNECTED (WinError 395, which is why the always-resident test flips its
    binding first); with the agent down the same rename is allowed, registration
    or not (measured on Windows 2026-09-29), so no out-of-band unregister is needed.

    The app respawns a missing agent on its convergence tick, and nothing lets a
    test hold that tick off — so a respawn can land inside the staging and
    reconnect the root under it. That is detected, never slept around: the
    rename then fails, or the pipe is served again before the directory was
    replaced, and the staging is retried from the stop.
    """
    from helpers import sync_agent_ipc as ipc
    from helpers.windows_sync_agent import shutdown_agent

    last: Exception | None = None
    for attempt in range(_STAGE_ATTEMPTS):
        shutdown_agent(pipe)
        try:
            os.rename(bound, tmp_path / f"stashed-{attempt}")
            bound.mkdir()
        except OSError as e:
            last = e
            continue
        try:
            ipc.wait_for_pipe(pipe, timeout=0.3)
        except TimeoutError:
            return  # still down: the replacement landed while nothing was connected
        # Served again already — the respawned agent may have reconnected the
        # root before the replacement; the staged state is not the one asked
        # for, so stage again.
        last = AssertionError("the agent respawned inside the staging window")
    pytest.fail(
        f"could not replace the bound directory while the agent was down in "
        f"{_STAGE_ATTEMPTS} attempts — the app kept respawning it inside the "
        f"window (last: {last!r})"
    )


@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
@pytest.mark.parametrize(
    "hydrated", [2, 1], ids=["two-hydrated-are-held", "one-hydrated-propagates"]
)
def test_an_on_demand_root_holds_only_its_hydrated_rows(logged_in_app, tmp_path, hydrated):
    """The mass-delete floor on a windows on-demand (cfapi) root — the default
    shape of a fresh windows binding — which the test above cannot stage (its
    rename-away is refused on a registered root, so it flips its binding to
    always-resident first). `delete-propagation.md` § *The floor on an on-demand
    root*, witnessed end to end:

    * **point (3)** — the vanish that reaches the floor here is the directory
      replaced while the agent is down (:func:`_vanish_disconnected_root`); the
      restarted root's startup `converge` finds every hydrated row missing;
    * **point (2)** — the floor's universe is the rows with evidence: the
      ``Synced`` rows and the *seen* ``Placeholder`` rows (`delete-propagation.md`
      § *An offline placeholder delete propagates*, decision (b)). Three files
      are uploaded and all but ``hydrated`` of them are freed back to
      placeholders ("Free up space") — which marks them seen — so it is the
      staging's **replaced directory** that decides this test: it is not the
      directory the filter registered, the restarted root boots as a fresh
      registration (decision (e)) and clears every seen mark before its sweep,
      and the universe the hold reads is the hydrated rows alone. With two
      hydrated the hold names **2, never 3**, and with ONE hydrated the floor
      must NOT engage: that one delete propagates as an ordinary delete (the
      single-file boundary). The second parameter is what makes the first
      discriminate the universe, not merely "something was held". A seen
      placeholder deleted from a root whose registration SURVIVED (the other
      half of the ruling) is pinned at the OS tier, in
      ``cfapi_live_integration.rs`` (``…_deleted_while_the_service_was_down_is_propagated``);
    * **point (4)** — the confirm reaches an on-demand root's engine and applies
      exactly the held rows: the never-hydrated files still list on the nest
      after the apply.

    Placeholder rows are re-materialized by the restarted root's fold, which
    lists ``Placeholder`` rows only, so it can never re-create a hydrated row
    and move the count in either order (`engine.rs::list_placeholder_rows`).
    """
    app = logged_in_app
    if os.name != "nt":
        skip_environment("a cfapi on-demand root exists only on windows")

    from helpers.windows_cfapi import free_up_space

    pipe = app.driver.sync_agent_pipe
    assert pipe, "this launch pinned no agent pipe — the test cannot address its own agent"

    token = secrets.token_hex(4)
    set_name = f"odfloor-{token}"
    names = [f"odfloor-{token}-{i}.txt" for i in range(_FILE_COUNT)]
    kept, freed = names[:hydrated], names[hydrated:]

    b = app.backups
    b.navigate_folders()
    app.driver.wait_for("folder-add-button", timeout=30)
    b.create_folder_via_wizard(set_name)

    bound = tmp_path / "bound"
    bound.mkdir()
    idx = bind_location_under_set(app, set_name, bound, seat="odfloor")
    mode = app.sync_locations.mode_toggle_state(scope=f"folder-row[{idx}]")
    assert mode == "on-demand", (
        f"a fresh windows binding must come up on-demand — the shape under test "
        f"(read {mode!r}); error={app.error_text()!r}"
    )

    for i, name in enumerate(names):
        atomic_write(bound / name, f"on-demand floor fixture {token} #{i}\n")
    _await_listing(
        app,
        lambda listed: all(n in listed for n in names),
        describe=f"all {_FILE_COUNT} fixture files uploaded",
        budget=_UPLOAD_S,
    )
    _await_status(pipe, [bound / n for n in names], "Synced", describe="every upload recorded")

    for name in freed:
        free_up_space(bound / name)
    _await_status(
        pipe, [bound / n for n in freed], "CloudOnly", describe="the freed files back to placeholders"
    )
    _await_status(pipe, [bound / n for n in kept], "Synced", describe="the kept files still hydrated")

    _vanish_disconnected_root(pipe, bound, tmp_path)

    from helpers import sync_agent_ipc as ipc

    ipc.wait_for_pipe(pipe, timeout=_RESPAWN_S)

    if hydrated >= 2:
        _await_hold(app, set_name, hydrated)
        # Nothing was recorded: the hold rendering proves a pass ran over the
        # emptied root, so this read is anchored to a causal barrier.
        _await_still_listed(app, names)
        _apply_and_await_clear(app, set_name)
        _await_listing(
            app,
            lambda listed: not any(n in listed for n in kept)
            and all(n in listed for n in freed),
            describe=(
                "exactly the held (hydrated) rows deleted by the confirm, "
                "the never-hydrated ones still listed"
            ),
            budget=_PROPAGATED_S,
        )
    else:
        # One hydrated row is below the floor: its delete is an ordinary one and
        # propagates with no click, while the placeholder rows — outside the
        # universe — are neither counted nor deleted.
        _await_listing(
            app,
            lambda listed: kept[0] not in listed and all(n in listed for n in freed),
            describe="the single hydrated file's delete propagated, the placeholders kept",
            budget=_HOLD_S,
        )
        idx = _reopen(app, set_name)
        assert _held_text(app, idx) is None, (
            "a single missing hydrated file surfaced as a mass-delete hold — the floor "
            "counted rows outside its universe (placeholders) or engaged below its minimum"
        )
