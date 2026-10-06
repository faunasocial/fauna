"""Live-apply of Settings → Sync location-map edits (Linux Track 4).

Validates `docs/goal/architecture/apps/linux.md` § File Sync: adding or
removing a folder↔folder mapping starts/stops the corresponding in-process
`SyncEngine` **live**, without a sign-out/sign-in. Before Track 4 the driver
read the location map only at auth-success, so edits applied "on the next
sign-in" (the limitation this track removes).

The `sync_add_location` / `sync_remove_location` test-agent commands mirror the
Sync-tab add/remove handlers exactly (persist the device-local map, then send
the `SyncDriver` a live `Start`/`Stop`); they only bypass the native GTK
`FileChooserDialog` (linux) / folder picker (windows) the human path uses. The
running engine's flag is surfaced at `data.sync.running` and the device-local
map at `data.sync.locations` on both apps.

linux + windows: linux drives its in-process `SyncDriver` directly; windows
drives the REAL out-of-process `fauna-sync-agent.exe` over its control pipe
(the `isolated_sync_agent` + `real_sync_agent` markers — see
`sync_live_apply_app` below — spawn an isolated agent instance and gate the
app's hydration/provisioning loop onto it, so this never touches the box's
installed or per-SID agent; testing.md § conventions point 10). tier_3 — a
real `fauna-nest` binary backs the login, and the started engine makes real
(best-effort) `register_device` calls against it.
"""
from __future__ import annotations

import time

import pytest

from drivers.base import detached_agent_log_text
from helpers.windows_sync_agent import serving_agent

# reclaim_cycle: the sync area's representative in the post-reclaim gate — under
# `--reclaim-cycle` the shared nest is wiped + re-claimed before this runs, so the
# in-process SyncEngine's live start/stop + register_device is exercised
# post-reclaim. See `just e2e-reclaim-cycle-test`.
pytestmark = [pytest.mark.tier_3, pytest.mark.reclaim_cycle]


# A POSITIVE wait, so a generous ceiling is free on a green run (testing.md § conventions
# point 14) and the only thing it buys is a trustworthy verdict under load. Sized well above
# the slowest thing that legitimately sits between the command and the engine serving: on
# windows the app's sync-agent session installs only after `SyncAgentSession.StartAsync`
# completes its nest round-trips — ~10 s against a nest connection that is still down at
# login, and longer against a slow or contended one — and only then is the binding pushed and
# the engine started. The binding itself is recorded and reported immediately regardless
# (`FolderBindingsController`), so nothing here is racing for correctness; this ceiling covers
# the agent-side start-up alone.
SYNC_APPLY_BUDGET_S = 120.0

#: The nest set this test binds to. A module constant because the FIXTURE has to
#: create it on the nest and the test has to bind to it, and the two must agree —
#: see `sync_live_apply_app`'s *The set must exist on the nest first* note.
LIVE_APPLY_SET = "live-apply-set"


def _wait_for_sync(driver, predicate, timeout=SYNC_APPLY_BUDGET_S):
    """Poll `data.sync` until `predicate(sync_obj)` is true; return the obj."""
    deadline = time.monotonic() + timeout
    sync = {}
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        sync = (state.get("data") or {}).get("sync") or {}
        if predicate(sync):
            return sync
        time.sleep(0.2)
    return sync


def _folders(sync_obj) -> set[str]:
    return {f.get("folder") for f in sync_obj.get("locations", [])}


def _agent_log_tail(app, wanted=None, limit=40) -> str:
    """The ISOLATED AGENT's own log — the half `_diagnose` used to throw away.

    **Why this is not optional on windows.** The engine-start decision is the
    AGENT's, and its two most likely refusals announce themselves only here:
    `"not in the app-pushed key material yet; withholding its engine until the
    next content-key push names it"` (`engine_driver.rs`'s fail-closed arm for a
    bound set the app's blob does not name — since 2026-09-08 this means NO engine
    for that set, i.e. exactly the `running: False` this test asserts against) and
    `"capability un-provisioned; stopping engines"`. The app log cannot carry
    either: the app does not know why the agent declined. The fixture has always
    captured this file and nothing ever read it, so every `running: False` failure
    here was diagnosed from the one log that structurally cannot answer the
    question.

    Read from the agent's own daily-rolling file log under the launch's
    `--data-dir` (`driver.sync_agent_state_base`) — the one agent this launch
    has, whichever process started it (`sync_live_apply_app`). Only windows has
    an out-of-process agent whose log the app's own does not carry; elsewhere the
    driver answers no such dir and this is "".
    """
    if not app.driver.is_windows():
        return ""
    text = detached_agent_log_text(getattr(app.driver, "sync_agent_state_base", None))
    if not text:
        return "agent log: <none under the launch's agent --data-dir>"
    lines = text.splitlines()
    if wanted:
        lines = [ln for ln in lines if any(w in ln for w in wanted)]
    return "agent log: " + (" | ".join(lines[-limit:]) or "<no matching lines>")


def _diagnose(app) -> str:
    """Why did a sync command fail to move the state? (conventions point 6.)

    `sync_add_location` / `sync_remove_location` refuse LOUDLY on the app's own
    `error-message` when they cannot honour the command (point 11) — most often
    "no sync-agent session on this actor", which is a *provisioning* failure and
    reads nothing like the engine bug the bare assertion suggests. The agent-side
    log lines separate the two halves further: a session that never started at
    all logs `[sync-agent] session not started`, whereas a started-but-degraded
    one logs `provisioner start failed`. Both are cheap to read and turn a
    "the engine didn't start" verdict into a self-classifying failure.
    """
    bits = []
    try:
        err = app.error_text()
        if err:
            bits.append(f"app error-message: {err!r}")
        else:
            # Deliberately reported rather than omitted: `_currentErrorMessage` is an
            # app-WIDE mirror that any page overwrites as it opens/closes its own error
            # bar, so an empty read here does NOT prove the command never refused —
            # only that nothing is on screen NOW. Stating that keeps the next reader
            # from concluding "no refusal happened" from silence.
            bits.append("app error-message: <empty at read time (may have been cleared)>")
    except Exception:  # pragma: no cover - diagnostics must never mask a failure
        pass
    reader = getattr(app.driver, "app_log_text", None)
    if reader is not None:
        try:
            # Broad on purpose. A narrow filter is a bet on which half already failed,
            # and it loses: the `postAction ENTER/EXIT/FINALLY` markers (TestAgent's own
            # dispatch trace) are what separate "the command never ran" from "it ran and
            # the agent ignored it", and a `[sync-agent]`-only filter silently discards
            # exactly those.
            wanted = (
                "[sync-agent]", "[hydration]", "[TestAgent]", "postAction",
                "sync_add_location", "sync_remove_location", "pipe", "capabilit",
                "bearer", "provision",
            )
            lines = [
                ln
                for ln in (reader() or "").splitlines()
                if any(w in ln for w in wanted)
            ]
            bits.append(
                "app log: " + (" | ".join(lines[-25:]) or "<no matching lines>")
            )
        except Exception:  # pragma: no cover
            pass
    # Broad on purpose here too, and for the same reason: the interesting agent
    # lines are refusals nobody predicted the shape of.
    agent = _agent_log_tail(
        app,
        wanted=(
            "withholding", "engine", "capabilit", "provision", "bearer",
            "location", "AddLocation", "SetLocationFolder", "reconcile", "WARN",
            "ERROR",
        ),
    )
    if agent:
        bits.append(agent)
    return "; ".join(bits) or "<no diagnostics available>"


def _ensure_live_apply_set(nest_instance, test_user) -> None:
    """Create `LIVE_APPLY_SET` on the nest, owned by the account this test logs in
    as — **before** anything binds a local folder to it.

    **Why this is a correctness precondition, not tidiness.** Binding names a set;
    the engine that serves it is only started once the app has pushed a content-key
    blob that NAMES that set. Since the fail-closed rule of 2026-09-08
    (`sync-agent.md` § Control plane split → *Content keys are resolved before the
    first provision*), a bound set the blob does not name gets **no engine at all**
    — `engine_driver.rs` logs `"not in the app-pushed key material yet; withholding
    its engine…"` and waits for a later push to name it. The blob is built from the
    owner's nest-side roster (`fauna_client_folders::compute_engine_key_bindings_blob`
    → `FoldersClient::list_owned_and_shared`), so for a set that does not exist on
    the nest the promised later push can NEVER name it: `running` stays `False`
    forever, on every platform, and the 120 s budget only decides how long the test
    takes to say so.

    This is exactly what the sibling fixtures already do, with the same reasoning in
    their own words (`conftest.py`'s `media_delete_disk_app` — "The set must exist on
    the nest BEFORE the engine starts"); this test
    was the one `sync_add_location` caller that skipped it, which is why it alone
    red'd. It also matches the product path: the Folders UI binds a set that is
    *contextual on the page*, i.e. one the nest already has
    (`LocationBindingsController.AddAsync`), never a free-typed name.

    Idempotent: `test_user` is session-scoped, so a second app in one `--app sweep`
    run re-enters here and the nest answers `conflict` — which is success for our
    purposes, and the ONLY error swallowed.
    """
    from common.auth import user_create_folder

    try:
        user_create_folder(
            nest_instance["port"],
            LIVE_APPLY_SET,
            secret_key=test_user["signing_key"].encode().hex(),
            base_url=nest_instance["url"],
        )
    except Exception as e:  # noqa: BLE001 - narrowed on the message below
        if "already exists" not in str(e):
            raise


@pytest.fixture
def sync_live_apply_app(request, app, nest_instance, test_user, tmp_path):
    """`logged_in_app`, but on windows the launch's isolated `fauna-sync-agent.exe`
    is confirmed serving the run's pipe BEFORE login.

    Login (`set_state`) is what starts the windows app's session-scoped
    `HydrationSessionService` (`App.xaml.cs`, gated on `FAUNA_E2E_REAL_SYNC_AGENT`
    — the `real_sync_agent` marker on the test below); its first tick probes
    `FAUNA_E2E_SYNC_PIPE` (the `isolated_sync_agent` marker's override — see
    `isolated_sync_agent_pipe_name` in conftest.py) and, finding no pipe served,
    spawns one — on the launch's own `--data-dir` (`FAUNA_E2E_SYNC_AGENT_DATA_DIR`,
    honoured by `fauna_client_sync::agent_spawner::pinned_from_env`).

    Having it up ahead of login (rather than by fixture-request-order, which
    pytest does not guarantee) is what `serving_agent` does: it adopts the
    launch's agent when the app already spawned it, and otherwise starts it on
    that same dir. ⚠ This fixture once spawned a SECOND agent on a per-test dir
    for its stderr capture; since every windows launch brings its own agent
    (convention 10, windows axis (a)), that second agent exits as a duplicate
    whenever the launch's already serves the pipe.
    The agent's log — the only account of why an engine did not start — is its
    own daily-rolling file under the launch's dir, which `_agent_log_tail` reads.

    Non-windows apps need no external agent process spawned *here* — each
    already brings its own, isolated by construction, so this is a plain
    passthrough to `_login_app_as`: linux's `SystemdAgentSpawner` direct-spawns
    the real agent binary as a child inheriting the launch's isolated
    `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR` under e2e (`sync-agent.md` § A3 — the
    in-app `SyncDriver` residency this docstring used to name retired with the
    A3 cutover), tui direct-spawns its own into the launch's private
    `XDG_RUNTIME_DIR` (`drivers/tui.py`), and macOS (2026-08-05) builds
    `FfiChildAgentSpawner` at post-auth — a private child on THIS launch's
    home-derived socket, so the machine-global `social.fauna.sync-agent` LaunchAgent
    is never touched (`sync-agent.md` § A4; e2e-conventions.md § point 10).
    """
    from conftest import _login_app_as

    _ensure_live_apply_set(nest_instance, test_user)

    if app.driver.is_windows():
        exe = request.getfixturevalue("sync_agent_binary")
        pipe_leaf = request.getfixturevalue("isolated_sync_agent_pipe_name")
        pipe_path = r"\\.\pipe\{}".format(pipe_leaf)
        data_dir = app.driver.sync_agent_state_base
        assert data_dir, (
            "this launch pinned no agent `--data-dir` (`FAUNA_E2E_SYNC_AGENT_DATA_DIR`); "
            "the test must carry `isolated_sync_agent` (convention 10)"
        )
        with serving_agent(exe, pipe_path, data_dir, tmp_path / "agent-spawn.log"):
            _login_app_as(app, request, nest_instance, test_user)
            yield app
        return

    _login_app_as(app, request, nest_instance, test_user)
    yield app


@pytest.mark.linux
@pytest.mark.windows
# tui joined 2026-07-29 — see the twin note on
# `test_media.py::test_media_delete_removes_the_file_from_disk`: tui gained the
# `data.sync` `{running, locations}` block and the `sync_add_location` /
# `sync_remove_location` commands this test drives, over the same shared
# `LocationBindingsModel` the Folders UI writes.
@pytest.mark.tui
# macos joined 2026-08-05, for the same reason and over the same shared model:
# the `data.sync` `{running, locations}` block (`running` off the shared
# `any_engine_serving`, bridged for the main actor) and the two commands now
# route through `LocationsModel.add`/`.remove` — the `folder-location-*` UI's own
# path. macOS builds its private child agent only under `FAUNA_E2E_REAL_SYNC_AGENT`,
# which the `real_sync_agent` marker below sets (`sync-agent.md` § A4).
@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_sync_location_map_edits_apply_live(
    sync_live_apply_app, nest_instance, test_user, tmp_path
):
    """Add → engine runs live; remove → engine stops live (no sign-in)."""
    from common.auth import user_folder_ref

    driver = sync_live_apply_app.driver
    watch_dir = tmp_path / "synced-folder"
    watch_dir.mkdir()
    # The fixture created this set on the nest (`_ensure_live_apply_set`) — binding
    # a name the nest does not have is engine-withheld by design since 2026-09-08.
    folder = LIVE_APPLY_SET

    # Baseline: fresh per-app XDG config → no folders, engine idle.
    baseline = driver.get_state() or {}
    base_sync = (baseline.get("data") or {}).get("sync") or {}
    assert folder not in _folders(base_sync), (
        f"unexpected pre-existing mapping for {folder!r}: {base_sync}"
    )

    # --- Add a folder mapping live ---
    # By the set's ref, as the Folders UI's bind does (the name-keyed bind is
    # retired): the fixture's owner is `test_user`.
    folder_id = user_folder_ref(
        nest_instance["port"],
        folder,
        secret_key=test_user["signing_key"].encode().hex(),
        base_url=nest_instance["url"],
    )
    driver.call_command(
        "sync_add_location",
        {"path": str(watch_dir), "folder": folder, "folder_id": folder_id},
    )
    after_add = _wait_for_sync(
        driver, lambda s: s.get("running") is True and folder in _folders(s)
    )
    assert after_add.get("running") is True, (
        "sync engine did not start live after add_folder — location-map edit did "
        f"not apply without a sign-in; last sync state: {after_add}; "
        f"{_diagnose(sync_live_apply_app)}"
    )
    assert folder in _folders(after_add), (
        f"folder map missing {folder!r} after add: {after_add}; "
        f"{_diagnose(sync_live_apply_app)}"
    )

    # --- Remove it live ---
    driver.call_command("sync_remove_location", {"folder": folder})
    after_rm = _wait_for_sync(
        driver, lambda s: s.get("running") is False and folder not in _folders(s)
    )
    assert folder not in _folders(after_rm), (
        f"folder map still has {folder!r} after remove: {after_rm}; "
        f"{_diagnose(sync_live_apply_app)}"
    )
    assert after_rm.get("running") is False, (
        "sync engine still running after the only mapping was removed live — "
        f"live stop did not release the engine; last sync state: {after_rm}; "
        f"{_diagnose(sync_live_apply_app)}"
    )
