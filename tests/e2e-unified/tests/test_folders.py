"""Folder creation wizard (Devices page) — cross-app e2e.

The wizard is a thin renderer over the shared-Rust `FolderWizardMachine`
(`libs/fauna-folders-machine`, surfaced via `libs/fauna-ffi`): name → device
places → review → create. (The type step and the create-time retention inputs
retired with folders re-model phase 2 slice e; the scan-frequency step and the
per-row `folder-frequency-select` retired with phase 5, 2026-08-20 — the
reconcile cadence is a hard-coded constant, `file-sync.md` § Config.) These
tests drive the real wizard UI against a real nest binary (tier_3) so the
`POST /folders` + per-device member adds `submit()` performs are exercised
end-to-end.

Action helpers live on `BackupsActions` (`app.backups`) — the folder / device
helpers were consolidated there alongside the backups-page snapshot helpers, so
this suite reuses them rather than forking a parallel actions class.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.launch_harness import reached_authenticated_app
from helpers.app_surface import declared_absence, skip_unbuilt
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.set_names import find_set
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("folders")
def test_folders_load_surfaces_no_error(logged_in_app):
    """Loading the folder list surfaces NO error on any client.

    Regression guard for the macOS symptom seen in the 2026-06-08/09 e2e runs:
    `error-message` = "Failed to load folders: The data couldn't be read
    because it isn't in the correct format." — the pre-WS-RPC `listFolders()`
    did `JSONDecoder().decode(...)` over the (since-deleted nest-side)
    `/api/v1/file-sets` HTTP route, so a drifted/absent reply threw a Foundation
    `DecodingError` that polluted the global `error-message`. The
    HTTP→WS-RPC migration removed that throwing decode (the
    retention codec now swallows with `try?`); this test pins that the
    list-on-appear stays clean so the decode-on-load class can't silently
    return. Portable: every app loads its folder list on the devices page.
    """
    logged_in_app.backups.navigate_folders()
    # The list renders its empty/add affordance once the load settles.
    assert logged_in_app.driver.is_visible("folder-add-button"), (
        "devices page should render the folder-add-button once the load settles: "
        f"{logged_in_app.driver.diagnose('folder-add-button')} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert not logged_in_app.has_error(), (
        f"folder load surfaced an error: {logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("folders")
def test_folders_empty_state(logged_in_app):
    """The persistent `folder-add-button` opens the creation wizard.

    RED until the wizard renderer (Task 7) wires `folder-add-button` to open
    the `AdwDialog`: the add button already exists (stub), but clicking add was
    a no-op so `wizard-name-input` never appeared. The load-bearing contract is
    that clicking add opens the wizard — exercised here regardless of how many
    sets the actor already holds.

    Seed-robust: `nest_instance`/`test_user` are session-scoped, so earlier
    batch tests (this file's own create tests, `test_device_cards`, …) leave
    folders behind for the same actor. A brittle absolute `folder_count()
    == 0` therefore fails under co-run while passing on a clean nest — the same
    co-run-isolation class fixed in an internal follow-up track. The
    `folder-add-button` is the persistent add affordance (shown whether the
    list is empty or populated, as every create test relies on), so the
    add→wizard assertion holds without requiring a fresh actor.
    """
    logged_in_app.backups.navigate_folders()
    assert logged_in_app.driver.is_visible("folder-add-button"), (
        "devices page should render the persistent folder-add-button: "
        f"{logged_in_app.driver.diagnose('folder-add-button')} "
        f"error={logged_in_app.error_text()!r}"
    )

    logged_in_app.backups.add_folder()
    # Wait, don't bare-`is_visible`: the wizard sheet presents with an animation
    # (and on macOS the `folder-add-button` tap is preceded by a scroll-into-view),
    # so a same-tick `is_visible` races the present — the same wait the passing
    # `create_folder_via_wizard` does after `add_folder()`. `wait_for` raises a
    # clear TimeoutError if the wizard genuinely never opens.
    logged_in_app.driver.wait_for("wizard-name-input")


@pytest.mark.feature("folders")
def test_create_folder_via_wizard_sync_mode(logged_in_app):
    """Walk the wizard in sync mode and confirm the folder is created.

    `submit()` does `POST /api/v1/file-sets` (the bearer actor owns it), then
    the dialog closes and the list refreshes via `fetch_folders`, so the new
    row appears for this actor.
    """
    name = f"sync-set-{secrets.token_hex(4)}"
    logged_in_app.backups.navigate_folders()
    count = logged_in_app.backups.create_folder_via_wizard(name)
    assert count >= 1
    # Dialog closed on success.
    assert logged_in_app.driver.is_absent("wizard-name-input"), (
        "wizard dialog should have closed after a successful create "
        "(wizard-name-input still visible): "
        f"{logged_in_app.driver.diagnose('wizard-name-input')} "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("folders")
def test_edit_folder_paths(logged_in_app):
    """Edit a folder's selective-sync include/exclude paths and confirm they
    persist (Task 16).

    The expander body's `folder-include-paths` / `folder-exclude-paths`
    entries write through `fauna.folders.update` (only the path fields are
    sent — `None` leaves mode/retention unchanged). On save the list refreshes,
    so re-expanding the row reads the entries back pre-filled from the nest's
    stored, comma-joined paths — proving the round-trip rather than just the
    local entry text.
    """
    name = f"paths-set-{secrets.token_hex(4)}"
    b = logged_in_app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    b.find_and_expand_folder(name)
    b.set_include_paths("/docs, /photos")
    b.set_exclude_paths("*.tmp, .git")
    b.save_paths()

    # Save refreshed the list (rows rebuilt collapsed); re-expand and read back
    # from the nest-authoritative summary.
    b.find_and_expand_folder_until(name, "folder-include-paths")
    assert b.get_include_paths() == "/docs, /photos"
    assert b.get_exclude_paths() == "*.tmp, .git"


# `test_edit_folder_frequency` (the per-row `folder-frequency-select` round-trip
# through `fauna.folders.schedule.set`) retired with phase 5 of the folders
# re-model (2026-08-20): the reconcile cadence is a hard-coded constant, not a
# per-folder choice (`file-sync.md` § Config, the phase-5 block). The kind stays
# registered nest-side for released apps (`conformance_folders.rs`
# `schedule_set_existing_and_missing`); no app renders the picker.


@pytest.mark.feature("folders")
def test_delete_folder(logged_in_app, nest_instance, test_user):
    """Delete a folder through the destructive confirmation dialog (Task 17).

    A wizard-created sync set has no snapshots, so `fauna.folders.delete`
    succeeds (the non-cascading delete only errors when snapshots remain). The
    `folder-delete-button` opens an `adw::MessageDialog`; confirming fires the
    delete and the list refreshes with the row gone.

    The verdict is the NAMED row, in the UI and on the nest — not the row
    count. The count is shared with every other source of `folder-row`s (on
    linux, the offline-share group scopes, listed separately), so "one fewer
    row" read at one instant was an assertion on what else happened to arrive
    in the same refresh: every whole-suite linux sweep since 2026-09-21 reded
    it `25 == 24` with the named row gone.
    """
    name = f"del-set-{secrets.token_hex(4)}"
    b = logged_in_app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    b.find_and_expand_folder(name)
    b.delete_folder()
    b.confirm_folder_delete()

    b.wait_for_folder_row_gone(name)
    assert find_set(_nest_sets(nest_instance["url"], test_user), name) is None, (
        f"the row left the list but {name!r} is still on the nest — the list "
        f"hid it rather than deleting it. error={logged_in_app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("local-folder-sync")
def test_bind_location_nested_under_folder(logged_in_app, tmp_path):
    """Bind a local folder UNDER a folder — the desktop local-folder binding
    lifted from the former `Settings → Sync` page into Settings → Folders,
    nested per set with NO free-text set name (spec § 3, O-1; ui.yaml `folders`
    `platform_elements: linux, tui`).

    The set is created once via the wizard; binding a folder is an action *within*
    that set's expander, so the typed path (`folder-location-path-input` +
    `folder-location-add-button`) is bound to the enclosing set — there is no
    `folder-location-fileset-input`. The binding renders as a `folder-location-row` under
    the set and (post-A3) routes over the per-user socket to the REAL external
    `fauna-sync-agent` this e2e launch direct-spawned, which persists it in its
    own `config.toml` — the device-local source of truth an agent restart
    reloads (`sync-agent.md` § Control plane split; the agent `config.toml` is the only store).

    Covers **linux and tui** (A6): both direct-link the shared
    `fauna_client_sync::agent` provisioner and, under e2e, direct-spawn the same
    `fauna-sync-agent` binary into an isolated world the driver owns. The tui leg
    runs on **Windows as well as unix** since 2026-07-24 (`sync-agent.md`
    § Implementation status), where that isolation is a per-launch `--pipe-name`
    + `--data-dir` rather than a private `XDG_RUNTIME_DIR` — the agent's windows
    seam being machine-global. The test branches on neither: `drivers/tui.py`
    owns the isolation and answers `sync_agent_state_base` for the one place the
    config path differs. The assertion is latency-independent state (the agent's
    own persisted `config.toml`, polled against a generous ceiling), so it
    survives a heavily loaded build machine (testing.md § point 9; the
    2026-07-23 brittle-e2e directive).
    """
    from helpers.sync_agent_config import bound_sync_folder, describe_agent_config

    name = f"bind-set-{secrets.token_hex(4)}"
    b = logged_in_app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    idx = b.find_and_expand_folder(name)
    # A real (existing) folder so the engine the add starts has a watch dir.
    src = tmp_path / "bind-src"
    src.mkdir()
    folder_path = str(src)

    # The expanded set's nested binding form is the only visible folder-location-* form
    # (one row expanded at a time), so the unindexed add IDs resolve to it.
    logged_in_app.driver.type_text("folder-location-path-input", folder_path)
    logged_in_app.driver.click("folder-location-add-button")

    # The bound folder renders as a folder-location-row under THIS set. Generous
    # ceiling (not an expectation) so a loaded machine doesn't false-red.
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if logged_in_app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            break
        time.sleep(0.3)
    assert logged_in_app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1, (
        f"bound folder did not render under the folder; error={logged_in_app.error_text()!r}"
    )

    # ... and the AGENT persisted it bound to THIS set (no free-text name — the
    # set is contextual): the bind was pushed over the socket and written to the
    # agent's own config.toml (in this launch's isolated XDG world), so an agent
    # restart rehydrates the binding. The push is async (optimistic UI), hence
    # the poll. `bound_sync_folder` resolves the config across the flat AND the
    # per-actor-scoped layout — the agent re-scopes to `<base>/<actor-hex>/` on
    # provision (helpers/sync_agent_config.py), so the flat path alone is a file
    # that never appears.
    config_home = logged_in_app.driver.config_home
    # `sync_agent_state_base` is the driver's answer for a launch whose agent
    # root is NOT derivable from the config home — windows, where the driver
    # pins `--data-dir` because the default root is machine-global. `None`
    # everywhere else, so this is one uniform call, not a platform branch
    # (testing.md § point 7).
    agent_base = getattr(logged_in_app.driver, "sync_agent_state_base", None)
    bound = None
    # Generous ceiling: the agent is a freshly direct-spawned child that must come
    # up, bind its socket, accept the push, and rewrite its config — a chain that
    # stretches on a heavily loaded build machine. A ceiling, not an expectation
    # (testing.md § point 9).
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        bound = bound_sync_folder(config_home, folder_path, agent_base)
        if bound:
            break
        time.sleep(0.2)
    assert bound is not None and bound.get("folder") == name, (
        f"the agent's config.toml never gained the binding for {folder_path!r} "
        f"→ {name!r}; got {bound!r}. {describe_agent_config(config_home, agent_base)}"
    )


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("local-folder-sync")
def test_folder_save_paths_does_not_drop_pending_location_path(logged_in_app, tmp_path):
    """Regression for a real 2026-07-20 manual-test-loop report: a user typed/picked a
    local folder path into `folder-location-path-input` (the device-local binding form
    nested under a folder), then clicked `folder-save-paths` — the button that
    commits the SET's selective-sync `include`/`exclude` patterns
    (`docs/goal/ui/folders.md:28,52,244`), a different concept co-located in the same
    expander — instead of the dedicated `folder-location-add-button`. The pending folder
    path was silently dropped: no error, no effect, and the field renders empty again
    on the next expand (`folder_binding.rs`'s `path_row` is rebuilt fresh from the
    persisted `folder_map`, which never received the binding).

    This is exactly the project's iron-clad "a test agent/command must not silently
    drop an action" failure shape applied to production UI, not a test agent: a click
    that looks like it should work produced no error and no effect. `folder-save-paths`
    must also commit any pending typed/picked folder path as a safety net, so the two
    adjacent "save"-shaped controls can't silently lose user input at each other's
    expense.
    """
    from helpers.sync_agent_config import bound_sync_folder, describe_agent_config

    name = f"save-paths-bug-{secrets.token_hex(4)}"
    b = logged_in_app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)

    src = tmp_path / "save-paths-src"
    src.mkdir()
    folder_path = str(src)

    # Fill the folder-binding path field (the picker itself isn't e2e-driveable —
    # same substitution the sibling test above uses) but click Save Paths instead
    # of the dedicated Add button, exactly as reported.
    logged_in_app.driver.type_text("folder-location-path-input", folder_path)
    logged_in_app.driver.click("folder-save-paths")

    # Assert against the agent's own persisted state (same seam the sibling test
    # above verifies), not the rendered row: clicking Save Paths also kicks off an
    # async `set_folder_paths` WS-RPC round-trip whose self-refresh rebuilds the
    # whole folder list (`update_folder_list` destroys + recreates every row),
    # which collapses the just-expanded `adw::ExpanderRow` — a real but SEPARATE
    # UX rough edge, not what this regression is about. The persisted binding is
    # the actual thing that must not silently vanish.
    # Resolved across the flat AND per-actor-scoped agent layouts — see
    # `helpers/sync_agent_config.py` (the agent re-scopes to `<base>/<actor-hex>/`
    # on provision, so the flat path alone never appears).
    config_home = logged_in_app.driver.config_home
    bound = None
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        bound = bound_sync_folder(config_home, folder_path)
        if bound:
            break
        time.sleep(0.2)
    assert bound is not None and bound.get("folder") == name, (
        f"pending folder path {folder_path!r} was silently dropped when Save Paths was "
        f"clicked instead of the dedicated Add button; error={logged_in_app.error_text()!r}. "
        f"{describe_agent_config(config_home)}"
    )


@pytest.mark.windows
@pytest.mark.feature("local-folder-sync")
def test_bind_location_nested_under_folder_windows(logged_in_app, tmp_path):
    """Bind a local folder UNDER a folder on WINDOWS — the windows twin of the linux
    nested-binding test (spec § 3, O-1; ui.yaml `folders` `platform_elements: windows`).

    The binding lifted from the retired flat `Settings → Sync` page into Settings →
    Folders, nested per set with NO free-text set name (the set is contextual; the
    removed `folder-location-fileset-input`). Windows persists the binding over the
    `fauna-sync` named-pipe IPC, NOT the agent `config.toml` (the linux source of truth), and
    there is no live `fauna-sync-agent` in e2e — so this drives the nested add against
    the in-memory pipe fake (`sync_inject_locations([])`) rather than asserting a disk file
    (the C#↔Rust pipe wire codec is pinned separately by `DagCborIpcTests`). inject BEFORE
    navigating so `FoldersPage.OnNavigatedTo` wires `LocationsViewModel` onto the fake
    (`FoldersPage.TestPipeOverride`). The set is created via the wizard (real nest);
    binding is an action WITHIN that set's expander, so the typed path
    (`folder-location-path-input` + `folder-location-add-button`) binds to the enclosing set and
    renders a `folder-location-row` under THIS set (the fake's AddFolder relists Folders →
    CollectionChanged → RefreshExpandedFolders).
    """
    app = logged_in_app
    # Seed an EMPTY in-memory pipe fake BEFORE the page loads (OnNavigatedTo reads the
    # static TestPipeOverride to build the page's LocationsViewModel).
    app.sync_locations.inject_locations([])

    name = f"bind-set-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    idx = b.find_and_expand_folder(name)
    src = tmp_path / "bind-src"
    src.mkdir()
    folder_path = str(src)

    # One row expanded at a time, so the unindexed nested add IDs resolve to THIS set.
    app.driver.type_text("folder-location-path-input", folder_path)
    app.driver.click("folder-location-add-button")

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            break
        time.sleep(0.3)
    assert app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1, (
        f"bound folder did not render nested under the folder; error={app.error_text()!r}"
    )
    assert folder_path in app.driver.get_text(
        "folder-location-path", scope=f"folder-row[{idx}]"
    ), f"nested folder-location-row path mismatch; error={app.error_text()!r}"


@pytest.mark.macos
@pytest.mark.feature("local-folder-sync")
def test_bind_location_nested_under_folder_macos(logged_in_app, tmp_path):
    """Bind a local folder UNDER a folder on macOS — the macOS twin of the linux/
    windows nested-binding tests above (spec § 3, O-1; ui.yaml `folders`
    `platform_elements: macos`).

    macOS's own flat `Settings → Sync` page (`SyncSettingsView`) was retired at the
    same 2026-06-28 unification linux/windows migrated at, replaced by
    `MacFoldersView`'s `MacFolderBindingSection` nested per set with NO free-text
    set name (the removed `folder-location-fileset-input`). Binding lives on
    `LocationsModel`'s in-memory list (no live `fauna-sync-agent` in this e2e
    launch unless `FAUNA_E2E_REAL_SYNC_AGENT` is set — apple's peer of windows' fake
    pipe / linux's real agent), swapped via the uniform `sync_inject_locations`
    command — inject BEFORE navigating so the handler seeds `LocationsModel`
    before the page first reads it. The set is created via the wizard (real nest);
    binding is an action WITHIN that set's expander, so the typed path
    (`folder-location-path-input` + `folder-location-add-button`) binds to the enclosing set
    and renders a `folder-location-row` under THIS set.
    """
    app = logged_in_app
    app.sync_locations.inject_locations([])

    name = f"bind-set-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    idx = b.find_and_expand_folder(name)
    src = tmp_path / "bind-src"
    src.mkdir()
    folder_path = str(src)

    # One row expanded at a time, so the unindexed nested add IDs resolve to THIS set.
    app.driver.type_text("folder-location-path-input", folder_path)
    app.driver.click("folder-location-add-button")

    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            break
        time.sleep(0.3)
    assert app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1, (
        f"bound folder did not render nested under the folder; error={app.error_text()!r}"
    )
    assert folder_path in app.driver.get_text(
        "folder-location-path", scope=f"folder-row[{idx}]"
    ), f"nested folder-location-row path mismatch; error={app.error_text()!r}"


@pytest.mark.feature("share-a-folder")
def test_folder_owner_side_sharing_affordances(logged_in_app):
    """Owner-side cross-user Sharing UI renders on an expanded folder-row.

    This is the LEAD contract the other `*-folders-ui` clients mirror
    (`docs/goal/ui/folders.md` § Sharing — user-approved 2026-06-30). On an
    owner-only (unshared) set the "Shared with" section shows the
    `folder-share-button` (which opens the REUSED conversations recipient-picker),
    but — because the actor read (`fauna.folders.members.list_actors`) returns
    `fauna.folders.not_shared` — there is NO `folder-shared-badge` and NO
    `folder-member-item`, and that empty state is NOT surfaced as a page error.
    Clicking Share… opens the picker dialog (`recipient-picker-input` +
    `folder-share-confirm`).

    The full share-with-a-second-actor round-trip (a `folder-member-item` appears
    with status "Active", the badge reads "Shared · 1", and remove rotates the
    content key) is a follow-on: it needs a second actor that has published a
    KeyPackage (the MLS group admit), the same 2-actor setup the conversations
    add-participant / thread-membership suites use. The share/remove crypto itself
    is proven in the shared-Rust `fauna-client-folders` tests.
    """
    name = f"share-set-{secrets.token_hex(4)}"
    b = logged_in_app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)

    # The owner-side Share… affordance renders on the expanded row.
    assert b.share_button_visible(), (
        "expanded folder-row should render the owner-side folder-share-button: "
        f"{logged_in_app.driver.diagnose('folder-share-button')} "
        f"error={logged_in_app.error_text()!r}"
    )
    # An owner-only set is NOT shared: no members, no badge, and no page error
    # (the `not_shared` read is an empty roster, not a failure).
    assert b.shared_member_count() == 0, (
        "an unshared set should list no shared-with members: "
        f"count={b.shared_member_count()}"
    )
    assert not b.shared_badge_visible(), (
        "an unshared set should not show the folder-shared-badge"
    )
    assert not logged_in_app.has_error(), (
        f"owner-side sharing surfaced an unexpected error: {logged_in_app.error_text()!r}"
    )

    # Share… opens the reused recipient-picker + the folder-share-confirm response.
    b.open_share_dialog()
    assert b.share_dialog_open(), (
        "clicking folder-share-button should open the recipient-picker dialog: "
        f"{logged_in_app.driver.diagnose('recipient-picker-input')}"
    )
    assert logged_in_app.driver.is_visible("folder-share-confirm"), (
        "the share dialog should expose the folder-share-confirm response: "
        f"{logged_in_app.driver.diagnose('folder-share-confirm')}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_folder_full_share_round_trip(folder_share_owner_app):
    """Full owner-side share round-trip: share a set with a SECOND actor, assert
    the roster + badge, then remove and assert the set is unshared again.

    The LEAD follow-on to `test_folder_owner_side_sharing_affordances` (which only
    proves the owner-side affordances on an *unshared* set). This drives the real
    share crypto through the client UI: `FoldersAuthor::share_set` fetches the
    recipient's published KeyPackage, admits them to a fresh MLS group, binds the
    set's channel on the nest, and delivers the Welcome — so the recipient lands on
    the roster (`fauna.folders.members.list_actors`) with status "Active" and the
    badge reads "Shared · 1". A remove evicts + rotates the content key, emptying
    the roster and hiding the badge. The share/remove crypto itself is unit-proven
    in `fauna-client-folders`; this proves the UI drives it end-to-end.

    Needs a recipient that is BOTH handle- and actor-id-resolvable, and has
    *published a KeyPackage*: the share dialog resolves the typed recipient
    per-app (linux → bare local-part handle via `ConversationsClient::
    actor_by_handle`; apple → the raw actor-id, since the handle path would need
    a real DNS-resolvable domain `handled_nest`'s synthetic one isn't — see
    `BackupsActions.share_recipient`'s docstring for the full per-app
    divergence; windows classifies locally through the same shared
    `classify_recipient`, so it takes the actor-id branch like apple), and
    `share_set` → `keypackage_fetch` (an actor with no KP →
    `NoKeyPackage`). So this rides `folder_share_owner_app` (a fresh driver —
    linux/macOS/iOS/tui/windows, per `folder_share_owner_app`'s fixture params — logged in as
    a handled owner on the handle-enabled `handled_nest`, its MLS session live) and
    seeds `bob` headlessly with `register_handled_actor` + a real minted KeyPackage
    (`conv_api.mint_key_packages` → `keypackage_upload`) — the same seed
    `cross_nest_foreign_actor` / the conversations 2-actor suites use. `bob` never
    comes online: the owner-side roster shows him from the Welcome delivery alone.

    macOS/iOS share their account's MLS SQLite store with the session-cached driver
    other tests use — verified live combined with the rest of `test_folders.py`
    (all 12 passed) so this is a watch item, not a hard isolation requirement; see
    `folder_share_owner_app`'s docstring for the full trace if a flaky
    "database is locked"-shaped failure ever shows up here.
    """
    import secrets

    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app

    # A handle-resolvable recipient with a PUBLISHED (real, parseable) KeyPackage so
    # the owner can fetch it + admit him to the MLS group (no KP → NoKeyPackage). Two
    # KPs so the single share's destructive fetch leaves the pool observably non-empty.
    bob_handle = "folderbob" + secrets.token_hex(2)
    bob = register_handled_actor(nest["port"], handle=bob_handle, domain=MAIL_PRIMARY_DOMAIN)
    conv_api.keypackage_upload(
        nest["port"], bob, conv_api.mint_key_packages(bytes(bob["signing_key"]), 2)
    )

    b = app.backups
    name = f"share-rt-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    row = b.find_and_expand_folder(name)

    # Owner-only to start: no members, no badge.
    assert b.shared_member_count() == 0, "a freshly-created set starts unshared"

    # Share the set with bob by his bare handle → FoldersAuthor::share_set.
    b.open_share_dialog()
    assert b.share_dialog_open(), (
        "clicking folder-share-button should open the recipient-picker dialog: "
        f"{app.driver.diagnose('recipient-picker-input')}"
    )
    b.share_recipient(handle=bob_handle, actor_id_hex=bob["actor_id_hex"])

    # The share runs async (resolve → keypackage.fetch → MLS group → nest bind →
    # welcome deliver → roster re-read → UI fill); poll until bob lands on the roster.
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.shared_member_count() == 1:
            break
        time.sleep(0.5)
    # Self-diagnosing (convention 6): a bare count==0 cannot tell "the share never
    # ran" from "it ran and the roster did not repaint", and the app's own
    # error-message stays EMPTY for the first of those on any client whose picker
    # reports a classify/resolve failure on `recipient-resolve-status` instead
    # (windows' confirm handler returns after `UpdateResolveState(Error)` without
    # touching the error bar). So report both halves: the NEST's roster is the
    # ground truth for "did the share land", and the picker's resolve state is
    # the ground truth for "did the client even get an actor id to share with".
    def _share_diagnosis() -> str:
        try:
            rostered = conv_api.folder_member_actors(nest["port"], owner, name)
        except Exception as exc:  # the roster read itself is diagnostic
            rostered = f"<unreadable: {exc!r}>"
        try:
            resolve = app.driver.get_attr("recipient-resolve-status", "state")
        except Exception:
            resolve = "<element gone>"
        return (
            f"nest roster={rostered} (ground truth: >0 means the share LANDED and "
            f"only the roster render is at fault; 0 means it never ran), "
            f"recipient-resolve-status={resolve!r}"
        )

    assert b.shared_member_count() == 1, (
        f"sharing set {name!r} with {bob_handle!r} should add exactly one roster "
        f"member; count={b.shared_member_count()} error={app.error_text()!r} "
        f"{_share_diagnosis()}"
    )
    assert not app.has_error(), (
        f"sharing surfaced an unexpected error: {app.error_text()!r}"
    )

    # bob shows on the roster with his handle and status "Active". `row` scopes
    # into the expanded folder-row (FolderMemberRow lives inside its
    # `.automationScope("folder-row", index:)` container on apple — a bare
    # `folder-member-item[i]` scope 404s there; see BackupsActions.member_handle).
    assert b.member_handle(0, row=row) == bob_handle, (
        f"the roster should show bob's resolved handle {bob_handle!r}, got "
        f"{b.member_handle(0, row=row)!r} "
        f"diagnose={app.driver.diagnose('folder-member-handle', scope=f'folder-row[{row}]/folder-member-item[0]')!r}"
    )
    assert b.member_status(0, row=row) == "Active", (
        f"a shared member reads status 'Active' (the nest roster reports only actors "
        f"the share reached), got {b.member_status(0, row=row)!r}"
    )

    # The "Shared · 1" badge shows on the row.
    assert b.shared_badge_visible(), "the folder-shared-badge should show once shared"
    badge = b.shared_badge_text()
    assert "Shared" in badge and "1" in badge, (
        f"the badge should read 'Shared · 1' for one member, got {badge!r}"
    )

    # Remove bob → the roster empties, the badge hides (content key rotated).
    b.remove_shared_member(0, row=row)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.shared_member_count() == 0:
            break
        time.sleep(0.5)
    assert b.shared_member_count() == 0, (
        f"removing the sole member should empty the roster; still "
        f"{b.shared_member_count()} error={app.error_text()!r}"
    )
    assert not b.shared_badge_visible(), (
        "the folder-shared-badge should hide once the set is unshared again"
    )
    assert not app.has_error(), (
        f"removing the member surfaced an unexpected error: {app.error_text()!r}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_folder_member_role_and_cap_editing(folder_share_owner_app):
    """Multi-writer Phase 1 (linux LEAD; ``ui/folders.md`` § Sharing): a share
    defaults the member to Reader; promoting them to Writer on the member row
    drives ``fauna.folders.members.set_access`` and — while the cap is blank —
    shows the ``folder-writer-uncapped-warning``; setting a byte cap hides it;
    demoting back to Reader persists (never rotates; the row repaints from the
    nest's authoritative role row after every write).
    """
    import secrets

    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    if not (app.driver.is_linux() or app.driver.is_tui() or app.driver.is_windows()):
        # Declared debt rather than a bare skip (convention 7): the shared
        # `member_access_options()` catalog + `members.set_access` are app-agnostic,
        # so every remaining shell owes only the member-row controls.
        #
        # windows joined 2026-09-03 and its
        # story is worth keeping, because the gate outlived its cause twice. The UI
        # had been built and rendering all along; what blocked the CAP
        # half was `set_member_cap`'s type-then-click-same-field commit contract
        # reaching the bridge's only "commit an editable" gesture — a physical Enter.
        # `SendInput` needs an ATTACHED INTERACTIVE DESKTOP, not merely the right
        # foreground window, so on a box whose automation session runs disconnected
        # (`query session` → `Disc`) it fails `Win32Exception(5)` however correct the
        # foreground handling is. Two real bugs were fixed on the way to that
        # diagnosis (`ForceForeground` attaching to the wrong thread; a bare
        # `is_visible` missing the below-the-fold trap), and neither was the cause.
        # The cause went away when the bridge stopped needing SendInput here at all:
        # `Actions.cs::CommitEditable` now commits by shifting UIA focus to the
        # field's nearest focusable neighbour, firing the app's own `LostFocus`
        # handler. Convention 14 — the dependency is removed, not retried harder.
        skip_unbuilt(
            app.driver,
            surface="folder-member-cap-input",
            detail="multi-writer member-row byte-cap editing (linux + tui + windows built)",
            tracked="ui/folders.md § Sharing (multi-writer Phase 1)",
        )

    bob_handle = "rolebob" + secrets.token_hex(2)
    bob = register_handled_actor(nest["port"], handle=bob_handle, domain=MAIL_PRIMARY_DOMAIN)
    conv_api.keypackage_upload(
        nest["port"], bob, conv_api.mint_key_packages(bytes(bob["signing_key"]), 2)
    )

    b = app.backups
    name = f"role-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    row = b.find_and_expand_folder(name)

    b.open_share_dialog()
    assert b.share_dialog_open()
    # The share dialog carries the role select, defaulted to Reader — leave it
    # (the share-time Writer grant path is API-proven in
    # conformance_shared_folders; here we drive the post-hoc row editor).
    assert app.driver.is_visible("folder-share-role-select"), (
        "the share dialog should carry folder-share-role-select"
    )
    b.share_recipient(handle=bob_handle, actor_id_hex=bob["actor_id_hex"])

    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.shared_member_count() == 1:
            break
        time.sleep(0.5)
    assert b.shared_member_count() == 1, (
        f"share should land one member; error={app.error_text()!r}"
    )

    # Reader default; no warning on a reader row.
    assert b.member_access(0, row=row) == "reader", (
        f"a fresh member defaults to reader, got {b.member_access(0, row=row)!r}"
    )
    assert not b.writer_uncapped_warning_visible(0, row=row)

    # Promote to Writer (uncapped) → the advisory warning shows once the roster
    # re-read repaints the row.
    b.set_member_access("writer", 0, row=row)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if b.member_access(0, row=row) == "writer":
            break
        time.sleep(0.5)
    assert b.member_access(0, row=row) == "writer", (
        f"promotion should persist via members.set_access; error={app.error_text()!r}"
    )
    assert b.writer_uncapped_warning_visible(0, row=row), (
        "an uncapped writer row must show folder-writer-uncapped-warning"
    )

    # A byte cap hides the warning (capped writer).
    b.set_member_cap("4096", 0, row=row)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if not b.writer_uncapped_warning_visible(0, row=row):
            break
        time.sleep(0.5)
    assert not b.writer_uncapped_warning_visible(0, row=row), (
        f"a capped writer shows no warning; error={app.error_text()!r}"
    )
    assert b.member_access(0, row=row) == "writer", (
        "setting the cap must not clear the writer grant (the row writes the "
        "full access+cap pair)"
    )

    # Demote back to Reader — a plain access edit, persists.
    b.set_member_access("reader", 0, row=row)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if b.member_access(0, row=row) == "reader":
            break
        time.sleep(0.5)
    assert b.member_access(0, row=row) == "reader", (
        f"demotion should persist; error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"role/cap editing surfaced an unexpected error: {app.error_text()!r}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_folder_writer_warning_follows_whether_the_folder_is_published(
    folder_share_owner_app,
):
    """Coverage-contract outcome 9 (``ui/folders.md`` § Sharing — *A writer grant
    on a folder whose content is readable BEYOND its members warns too*): the
    ``folder-writer-published-warning`` shows exactly while the grant is
    ``writer`` AND the folder's audience is ``public`` (or it is paywalled).

    The condition is **state-based**, so this walks both orderings on ONE
    folder and both placements, driven through the app UI (convention 8):

    1. share with a person as a reader, promote them to writer — the folder is
       only shared, so NO warning (a writer on a folder that reaches nobody
       outside its members has nothing to be told);
    2. publish the folder — the warning appears on the member row (the
       *grant-then-publish* ordering) with the public sentence, and it is
       independent of the byte cap: capping the writer clears the quota warning
       and leaves this one (the two stack, neither suppresses the other; the
       cap step runs where the cap-commit gesture is proven — tui and linux);
    3. take it back to Shared — the warning goes again (state, not an event);
    4. demote the member, publish again and open the share dialog — picking
       Writer there shows the warning before anything is granted (the
       *publish-then-grant* ordering, the share-flow placement), picking Reader
       clears it. The demotion keeps the read honest: a writer's own member row
       paints the same element id.

    The paywalled sentence and every reach/access combination are pinned in
    ``fauna_folders_machine::writer_grant_reach``'s tier_1 tests; this journey
    proves the app actually asks that decision and paints its answer.
    """
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.waiting import wait_until
    from i18n.strings import S
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    driver = app.driver
    if driver.is_macos() or driver.is_ios():
        declared_absence(
            driver,
            capability="owner-side access-grant UI (folder-share-role-select, "
            "folder-member-role-select) — so no writer warning either",
            doc="docs/goal/ui/folders.md § Implementation status today "
            "(the published-folder writer warning)",
        )
    bob_handle = "reachbob" + secrets.token_hex(2)
    bob = register_handled_actor(nest["port"], handle=bob_handle, domain=MAIL_PRIMARY_DOMAIN)
    conv_api.keypackage_upload(
        nest["port"], bob, conv_api.mint_key_packages(bytes(bob["signing_key"]), 2)
    )

    b = app.backups
    name = f"reach-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    row = b.find_and_expand_folder(name)

    # 1. Shared with bob as a reader, promoted to writer: nothing reaches beyond
    #    the set yet.
    b.open_share_dialog()
    b.share_recipient(handle=bob_handle, actor_id_hex=bob["actor_id_hex"])
    wait_until(
        lambda: b.shared_member_count() == 1,
        20,
        diagnose=lambda: f"share should land one member; error={app.error_text()!r}",
    )
    b.set_member_access("writer", 0, row=row)
    wait_until(
        lambda: b.member_access(0, row=row) == "writer",
        15,
        diagnose=lambda: f"promotion should persist; error={app.error_text()!r}",
    )
    assert b.writer_published_warning_absent(0, row=row), (
        "a writer on a folder only its members can read must carry NO published "
        "warning — the condition is reach, not the writer grant alone"
    )

    # 2. Publish → the warning appears (grant-then-publish), independent of the cap.
    b.make_public(name)
    wait_until(
        lambda: b.audience_current_value() == "public",
        15,
        diagnose=lambda: f"declassify should land; error={app.error_text()!r}",
    )
    wait_until(
        lambda: b.writer_published_warning_visible(0, row=row),
        15,
        diagnose=lambda: "a writer on a now-public folder must show "
        f"folder-writer-published-warning; error={app.error_text()!r}",
    )
    assert (
        b.writer_published_warning_text(0, row=row) == S.devices.writer_public_warning
    ), "a public folder names 'anyone', not 'subscribers'"
    assert b.writer_uncapped_warning_visible(0, row=row), (
        "the uncapped writer still carries the quota warning alongside it"
    )
    if driver.is_tui() or driver.is_linux():
        # The cap commit gesture is proven on these two (the same idiom
        # `test_folder_member_role_and_cap_editing` drives); web's cap input
        # commits on a blur the shared driver does not yet issue.
        b.set_member_cap("4096", 0, row=row)
        wait_until(
            lambda: not b.writer_uncapped_warning_visible(0, row=row),
            15,
            diagnose=lambda: f"a capped writer shows no quota warning; error={app.error_text()!r}",
        )
        assert b.writer_published_warning_visible(0, row=row), (
            "a byte cap bounds the owner's quota, not what the writer can "
            "publish — the published warning must outlive it"
        )

    # 3. Back to Shared → the warning goes again.
    b.set_audience("shared")
    wait_until(
        lambda: b.audience_current_value() == "shared",
        15,
        diagnose=lambda: f"the flip-back should land; error={app.error_text()!r}",
    )
    wait_until(
        lambda: b.writer_published_warning_absent(0, row=row),
        15,
        diagnose=lambda: "un-publishing must clear the warning on the member row "
        "(state, not an event)",
    )

    # 4. Publish-then-grant: the share dialog warns before anything is granted.
    #    Bob goes back to reader first — while he is a writer on the (soon public)
    #    folder his OWN member row paints the same element id, and the dialog's copy
    #    is read unscoped, so the read below would be his row's, not the dialog's.
    b.set_member_access("reader", 0, row=row)
    wait_until(
        lambda: b.member_access(0, row=row) == "reader",
        15,
        diagnose=lambda: f"the demotion should persist; error={app.error_text()!r}",
    )
    b.make_public(name)
    wait_until(
        lambda: b.audience_current_value() == "public",
        15,
        diagnose=lambda: f"the second declassify should land; error={app.error_text()!r}",
    )
    b.open_share_dialog()
    assert b.share_dialog_open()
    assert b.share_dialog_published_warning_absent(), (
        "the Reader default carries no warning, and a reader row on a public folder "
        "paints none either — so the Writer read below can only be the dialog's own"
    )
    b.pick_share_role("writer")
    wait_until(
        b.share_dialog_published_warning_visible,
        15,
        diagnose=lambda: "picking Writer in the share dialog of a public folder "
        f"must show folder-writer-published-warning; error={app.error_text()!r}",
    )
    b.pick_share_role("reader")
    wait_until(
        b.share_dialog_published_warning_absent,
        15,
        diagnose=lambda: "a Reader grant carries no published warning",
    )
    assert not app.has_error(), (
        f"the published-warning journey surfaced an unexpected error: {app.error_text()!r}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_folder_pending_share_accept_decline(folder_share_recipient_app):
    """Recipient-side pending-share round-trip: a STRANGER's staged folder share
    renders in the "Shared with you" section, and the recipient accepts (join the
    MLS group off the chat rail + ack) or declines (roster drop + ack, never joins).

    The INVERSE of ``test_folder_full_share_round_trip`` — here the driven GUI
    is the RECIPIENT and the sharer is seeded headlessly. The recipient's contact-gate
    (``NestFolderGate``, wired identically for every app — the shared UniFFI
    factory registers it in ``libs/fauna-ffi/src/nest_client.rs``, linux's
    ``conv_backend`` mirrors the same factory call) leaves a *stranger's* folder
    Welcome un-acked (a "knock"), which the shared ``list_folder_pending_shares``
    peek surfaces as a ``folder-pending-share``. A contact's share would auto-join
    off the chat rail and never appear (not tested here — the disposition tri-state
    is unit-proven in ``fauna-conversations``).

    Headless share: a fresh registered stranger fetches the recipient's login-time
    published KeyPackage, mints a real 1:1 MLS group + Welcome against it
    (``mint_group_welcome``), and delivers it as ``channel_type="folder"`` (the
    nest stamps ``shared_by`` from the authenticated caller — ``conversations_handlers``
    ``welcome_deliver_core``). No second GUI: the sharer's group state lives only in a
    throwaway engine (the proof drives the recipient).

    Also asserts B3 member-list-visibility end-to-end: a rostered-but-un-accepted
    share must NOT appear in the folders list (only as a pending-share — the
    client ``has_group`` join-filter drops it), and once ACCEPTED (joined) the set
    appears as a read-only "shared with me" row carrying the recipient badge
    "Shared by ‹handle›". The stranger seeds a real nest folder bound to the MLS
    group (``folder_create`` + ``folder_share``) so the projection can union it
    in. Finally the recipient LEAVES the accepted set (``folder-leave-button`` →
    the self-scoped ``fauna.folders.leave`` roster-drop + local
    ``MlsEngine::forget_group``) and the read-only row disappears from the list.

    macOS/iOS share their account's MLS SQLite store with the session-cached driver
    other tests use — verified live combined with the rest of `test_folders.py`
    (all 12 passed) so this is a watch item, not a hard isolation requirement; see
    `folder_share_owner_app`'s docstring (`conftest.py`) for the full trace if a
    flaky "database is locked"-shaped failure ever shows up here.
    """
    import secrets

    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, recipient = folder_share_recipient_app
    port = nest["port"]
    recipient_id = recipient["actor_id_hex"]
    b = app.backups

    last_share: dict = {}

    def seed_stranger_share() -> tuple[str, str]:
        """A fresh stranger creates a group-bound folder and delivers its Welcome
        to the recipient (a knock). Returns ``(bare_handle, actor_id_hex)`` — the
        handle is the "Shared by ‹handle›" the recipient renders (on the pending-share
        knock row and, once accepted, the shared-with-me badge); the hex is the raw
        ActorId that must NOT leak into that surface once the handle resolves."""
        stranger_handle = "strngr" + secrets.token_hex(3)
        stranger = register_handled_actor(
            port, handle=stranger_handle, domain=MAIL_PRIMARY_DOMAIN
        )
        # The recipient's login-time KeyPackage publish is best-effort async — poll
        # a destructive fetch until one is available so the stranger can admit the
        # recipient to a fresh MLS group.
        deadline = time.monotonic() + 30
        kp = None
        while time.monotonic() < deadline:
            kp = conv_api.keypackage_fetch(port, stranger, recipient_id)
            if kp:
                break
            time.sleep(1)
        assert kp, "recipient never published a fetchable KeyPackage for the sharer"
        channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
            bytes(stranger["signing_key"]), kp
        )
        # The stranger owns a folder bound to this MLS group, so the recipient can
        # see it as a `role == "member"` row in `list_owned_and_shared` once they
        # join (B3 member-list-visibility). Without the nest folders row + share
        # bind, the projection returns nothing — a raw Welcome alone rosters the
        # recipient on the channel but there is no folder for the list to union in.
        set_name = "shared-" + secrets.token_hex(3)
        conv_api.folder_create(port, stranger, set_name)
        conv_api.folder_share(port, stranger, set_name, group_id_hex)
        # Deliver as a folder Welcome → the nest stamps shared_by (this stranger) +
        # channel_type="folder" AND rosters the recipient on the set's channel; the
        # recipient's gate then knocks (stranger).
        #
        # ``kind.group_id`` is the **raw MLS group id**, not the channel id — what the
        # real owner-side share sends (`FoldersAuthor::share_set` →
        # `WelcomeKind::Folder { group_id: hex(raw_group_id) }`,
        # `libs/fauna-client-folders/src/orchestration.rs`). This seed passed the
        # channel id until 2026-07-24, which was invisible while nothing *addressed*
        # anything by it (the field was display-only on the pending-share row). The
        # decline's roster drop is the first consumer that does: it derives the
        # channel via `ChannelId::from_group_id`, so a channel id here derives a
        # third, nonexistent channel and the leave silently no-ops.
        conv_api.welcome_deliver(
            port,
            stranger,
            recipient_id,
            channel_id_hex,
            welcome_bytes,
            kind={"type": "folder", "group_id": group_id_hex},
        )
        # Stash the sharer + set so a later step can read the OWNER-visible roster
        # (the "Shared with" list a decline must stop over-reporting).
        last_share["stranger"] = stranger
        last_share["set_name"] = set_name
        return stranger_handle, stranger["actor_id_hex"]

    # ── 1. A stranger's share stages as a pending knock + renders ──────────────
    stranger_handle, stranger_id_hex = seed_stranger_share()
    b.navigate_folders()
    count = b.wait_for_pending_shares(1)
    assert count == 1, (
        "a stranger's shared folder should stage as exactly one "
        f"folder-pending-share; count={count} error={app.error_text()!r}"
    )
    assert app.driver.is_visible("folder-share-accept-button"), (
        "the pending share should render its accept button: "
        f"{app.driver.diagnose('folder-share-accept-button')}"
    )
    # Self-diagnosing (convention 6): `is_visible` is NOT "exists" — on windows it
    # is `!IsOffscreen`, so a button that is present in the UIA tree but laid out
    # past the viewport edge reports False here and reads exactly like a missing
    # feature. accept/decline are adjacent children of one row, so reporting BOTH
    # counts plus this element's own diagnosis separates "never built" (count 0)
    # from "built but off-screen" (count 1, visible False) in a single run.
    assert app.driver.is_visible("folder-share-decline-button"), (
        "the pending share should render its decline button; "
        f"decline count={app.driver.count('folder-share-decline-button')} "
        f"accept count={app.driver.count('folder-share-accept-button')} "
        f"diagnose={app.driver.diagnose('folder-share-decline-button')}"
    )
    assert not app.has_error(), (
        f"the pending-share surface raised an unexpected error: {app.error_text()!r}"
    )

    # The pending-share row shows the sharer's nest-resolved HANDLE, not the raw
    # hex ActorId — a same-nest sharer's handle is resolved into `shared_by_handle`
    # (folders.md § Sharer identity; landed 2026-07-06). The 12-char hex prefix
    # (`short_actor`) is only a fallback for a cross-nest / handle-less sharer.
    pending_text = b.pending_share_text(0)
    assert stranger_handle in pending_text, (
        f"the pending-share row should read 'Shared by {stranger_handle}' (the "
        f"nest-resolved handle, not the shared_by hex); got {pending_text!r}"
    )
    assert stranger_id_hex[:12] not in pending_text, (
        "the pending-share row must not show the sharer's raw hex ActorId once the "
        f"handle resolves; got {pending_text!r}"
    )

    # THE SAFETY FILTER (B3 member-list-visibility): the shared set is ROSTERED
    # nest-side (the recipient is on the channel) but NOT yet JOINED (they haven't
    # accepted), so `list_owned_and_shared` returns it as a `role == "member"` row
    # that the client's `has_group` filter drops — a knock surfaces ONLY as a
    # `folder-pending-share`, NEVER in the list (folders.md § Sharing: "a
    # stranger cannot force a set into your list"). The recipient owns no sets, so
    # the list is empty until they join.
    assert b.folder_count() == 0, (
        "a rostered-but-un-accepted shared set must NOT appear in the folders "
        f"list (only as a pending-share); saw {b.folder_count()} row(s)"
    )

    # ── 2. Accept → join off the chat rail + ack → the knock is consumed ───────
    b.accept_pending_share(0)
    count = b.wait_for_pending_shares(0)
    assert count == 0, (
        f"accepting should consume the knock (join + ack); still {count} pending "
        f"error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"accepting the share surfaced an unexpected error: {app.error_text()!r}"
    )

    # Now JOINED: the accepted set flips the client `has_group` filter true, so the
    # B3 shared-with-me row appears in the folders list — read-only, carrying the
    # recipient badge "Shared by ‹handle›" over the nest-resolved owner_handle. The
    # list re-fetches on Folders-page-visible; poll (toggling to re-fire it).
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.folder_count() >= 1:
            break
        time.sleep(0.5)
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 1, (
        "the accepted shared set should appear as exactly one read-only "
        f"shared-with-me row once joined; saw {b.folder_count()} row(s) "
        f"error={app.error_text()!r}"
    )
    assert b.shared_badge_visible(), (
        "the shared-with-me row should carry the recipient folder-shared-badge: "
        f"{app.driver.diagnose('folder-shared-badge')}"
    )
    badge = b.shared_badge_text()
    assert "Shared by" in badge and stranger_handle in badge, (
        f"the recipient badge should read 'Shared by {stranger_handle}', got {badge!r}"
    )

    # ── 2b. Leave → the self-scoped roster-drop + local forget hides the row ────
    # `folder-leave-button` → `fauna.folders.leave` (drop the caller's own
    # actor_channels row, no ownerSecret, no content-key rotation) + local
    # `MlsEngine::forget_group`. Off the roster, `list_owned_and_shared` stops
    # unioning the set, so the read-only shared-with-me row disappears on the next
    # machine refresh (which the leave handler fires).
    assert app.driver.is_visible("folder-leave-button"), (
        "the read-only shared-with-me row should carry a folder-leave-button: "
        f"{app.driver.diagnose('folder-leave-button')}"
    )
    b.leave_shared_set(0)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.folder_count() == 0:
            break
        time.sleep(0.5)
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 0, (
        "leaving the shared set should drop the recipient from the roster and hide "
        f"the read-only row; still {b.folder_count()} row(s) error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"leaving the share surfaced an unexpected error: {app.error_text()!r}"
    )

    # ── 3. A second stranger's share → decline → knock consumed AND unrostered ─
    seed_stranger_share()
    sharer, shared_set = last_share["stranger"], last_share["set_name"]
    assert recipient_id in conv_api.folder_member_actors(port, sharer, shared_set), (
        "the share rosters the recipient at Welcome delivery, before they act — "
        "the row the decline must remove"
    )
    b.navigate_devices()
    b.navigate_folders()
    count = b.wait_for_pending_shares(1)
    assert count == 1, (
        f"a second stranger's share should re-stage a pending knock; count={count}"
    )
    b.decline_pending_share(0)
    count = b.wait_for_pending_shares(0)
    assert count == 0, (
        f"declining should consume the knock (never joins); still {count} "
        f"pending error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"declining the share surfaced an unexpected error: {app.error_text()!r}"
    )
    # The half that makes a decline mean something to the SHARER: the roster row
    # goes with it, so the owner's "Shared with" list stops over-reporting and a
    # later re-share is a genuine re-invite rather than the add path's no-op
    # access-refresh arm (`ui/folders.md` § Sharing → *Adding the 2nd..Nth
    # member*). Same gesture, driven through the real client UI; the Rust-plane
    # twin (decline → re-share → fresh Welcome) is
    # `conformance_shared_folders.rs::declining_a_share_drops_the_roster_row_and_a_re_share_re_invites_real_nest`.
    assert recipient_id not in conv_api.folder_member_actors(
        port, sharer, shared_set
    ), "declining an invitation removes you from the sharer's roster"


def _deliver_headless_folder_share(port: int, sharer: dict, recipient_id: str) -> str:
    """``sharer`` owns a fresh folder bound to a real MLS group and delivers its
    folder Welcome to ``recipient_id`` — the headless sharer's half of a share,
    exactly as ``test_folder_pending_share_accept_decline``'s seed lays it down
    (see that test for why ``kind.group_id`` is the raw group id). Returns the
    folder's name."""
    from helpers.waiting import wait_until
    from tests.api import conv_api

    # The recipient's login-time KeyPackage publish is async; the fetch is
    # destructive, so it is only retried until one comes back.
    kp = wait_until(
        lambda: conv_api.keypackage_fetch(port, sharer, recipient_id),
        30.0,
        diagnose=lambda: "the recipient never published a fetchable KeyPackage",
    )
    channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
        bytes(sharer["signing_key"]), kp
    )
    set_name = "shared-" + secrets.token_hex(3)
    conv_api.folder_create(port, sharer, set_name)
    conv_api.folder_share(port, sharer, set_name, group_id_hex)
    conv_api.welcome_deliver(
        port,
        sharer,
        recipient_id,
        channel_id_hex,
        welcome_bytes,
        kind={"type": "folder", "group_id": group_id_hex},
    )
    return set_name


@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_a_known_persons_share_appears_and_only_a_strangers_waits(
    folder_share_recipient_app,
):
    """A folder shared by someone you already know simply appears in your list;
    only a stranger's share waits for your answer (`ui/folders.md` § Sharing a
    folder → *Recipient side — auto for contacts, knock for strangers*).

    ``test_folder_pending_share_accept_decline`` proves the stranger half and
    says in its own docstring that the contact half was never driven. The
    arrival is decided by the recipient's own contact gate (``NestFolderGate``,
    one shared implementation on every app): a Confirmed/Accepted contact's
    folder Welcome is joined off the chat rail with no gesture, a stranger's is
    staged as a knock.

    Both shares are delivered before the recipient looks, so the one page read
    must tell them apart: the contact's set is a row in the list, badged with who
    shared it, and is NOT among the pending knocks; the stranger's is a pending
    knock and NOT in the list. The contact edge is fixture setup (convention 8
    carve-out (b)): it is the recipient's own row toward the sharer that the gate
    reads, arranged exactly as ``test_folder_member_media_decrypt.py`` does.
    """
    from common.auth import _user_call, register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.waiting import wait_until

    app, nest, recipient = folder_share_recipient_app
    port = nest["port"]
    b = app.backups

    friend_handle = "friend" + secrets.token_hex(3)
    friend = register_handled_actor(port, handle=friend_handle, domain=MAIL_PRIMARY_DOMAIN)
    stranger_handle = "strngr" + secrets.token_hex(3)
    stranger = register_handled_actor(port, handle=stranger_handle, domain=MAIL_PRIMARY_DOMAIN)

    # The recipient knows `friend`: accept + confirm their row toward them. Both
    # states map to Auto (`contact_arrival_disposition`); confirming as well keeps
    # the precondition off the weaker of the two.
    secret = recipient["signing_key"].encode().hex()
    for kind in ("fauna.knocks.accept", "fauna.contacts.confirm"):
        _user_call(port, secret, kind, {"peer_id": friend["actor_id_hex"]}, nest["url"])
    status = _user_call(
        port, secret, "fauna.contacts.status", {"peer_id": friend["actor_id_hex"]}, nest["url"]
    ).get("status")
    assert status in ("accepted", "confirmed"), (
        f"the recipient must hold the sharer as a contact; got {status!r}"
    )

    friend_set = _deliver_headless_folder_share(port, friend, recipient["actor_id_hex"])
    stranger_set = _deliver_headless_folder_share(port, stranger, recipient["actor_id_hex"])

    # ── The known person's folder is simply in the list. ──────────────────────
    # The list and the knock section both load on the page's visible edge, so
    # re-enter the page between reads (the same toggle every recipient-side test
    # here uses).
    b.navigate_folders()

    def _friend_row() -> int | None:
        for i in range(b.folder_count()):
            if friend_set in b.folder_title(i):
                return i
        b.navigate_devices()
        b.navigate_folders()
        return None

    wait_until(
        lambda: _friend_row() is not None,
        90.0,
        diagnose=lambda: (
            f"the contact's shared folder {friend_set!r} never appeared in the list "
            f"(rows: {[b.folder_title(i) for i in range(b.folder_count())]!r}, "
            f"pending knocks: {[b.pending_share_text(i) for i in range(b.pending_share_count())]!r}) "
            f"— a knock for it means the gate treated a contact as a stranger. "
            f"error={app.error_text()!r}"
        ),
    )
    row = _friend_row()
    assert row is not None
    badge_texts = [
        app.driver.get_text("folder-shared-badge", index=i)
        for i in range(app.driver.count("folder-shared-badge"))
    ]
    assert any("Shared by" in t and friend_handle in t for t in badge_texts), (
        f"the contact's row should read 'Shared by {friend_handle}'; badges: {badge_texts!r}"
    )

    # ── Only the stranger's share waits for an answer. ────────────────────────
    count = b.wait_for_pending_shares(1)
    knocks = [b.pending_share_text(i) for i in range(b.pending_share_count())]
    assert count == 1 and stranger_handle in knocks[0], (
        f"exactly the stranger's share should wait as a pending knock; got {knocks!r}"
    )
    assert not any(friend_handle in k for k in knocks), (
        f"the contact's share must never wait for an answer; knocks: {knocks!r}"
    )
    rows = [b.folder_title(i) for i in range(b.folder_count())]
    assert not any(stranger_set in r for r in rows), (
        f"a stranger's share must not enter the list before it is accepted: {rows!r}"
    )
    assert not app.has_error(), f"the arrivals surfaced an error: {app.error_text()!r}"


@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.real_conversations
@pytest.mark.parametrize(
    "folder_share_recipient_app", ["macos", "tui", "windows", "linux"], indirect=True
)
@pytest.mark.feature("local-folder-sync")
def test_writer_member_binds_location(folder_share_recipient_app, tmp_path):
    """A `writer`-access shared-set member row renders the local-folder binding
    widget and can actually bind a folder — the apple + tui + windows
    writer-binding fan-out leg (`ui/folders.md` § Sharing, multi-writer Phase 1;
    linux reference: `build_writer_member_folder_row`; windows landed
    2026-08-26, the last app). linux, the reference, joined this witness 2026-09-19 — its
    row was only ever exercised through the content-sync capstone's
    ``bind_location_under_set`` until then. The headless sharer grants
    `writer` via the raw `members.set_access` kind (fixture setup, e2e rule 8 —
    the member row's OWN UI is what this test drives); the single real app
    under test is the RECIPIENT, so this needs no second GUI instance (unlike
    the two-engine content-sync capstone, `test_folder_agent_content_sync.py`,
    which this leaves to its own linux-pinned follow-on).

    Proves the row renders the writer affordance (the folder-binding form),
    not the reader's flat read-only row, and accepts a real bind gesture with
    no error. Does NOT assert the agent's on-disk `config.toml` — see the NOTE
    at the bind assertion below for why (a standing macOS e2e-harness gap; tui
    HAS the seam, `test_bind_location_nested_under_folder` below, so this
    stays a shared assertion rather than a tui-only strengthening).
    """
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.waiting import wait_until
    from tests.api import conv_api

    app, nest, recipient = folder_share_recipient_app
    port = nest["port"]
    recipient_id = recipient["actor_id_hex"]
    b = app.backups

    # A fresh stranger owns a set, shares it, and grants the recipient `writer`
    # BEFORE delivering the Welcome — mirrors a share-time writer grant (the
    # order `members.set_access` supports either way; `folders.md` § Sharing).
    stranger_handle = "wr" + secrets.token_hex(3)
    stranger = register_handled_actor(port, handle=stranger_handle, domain=MAIL_PRIMARY_DOMAIN)
    kp = wait_until(
        lambda: conv_api.keypackage_fetch(port, stranger, recipient_id),
        30.0,
        diagnose=lambda: "recipient never published a fetchable KeyPackage for the sharer",
    )
    channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
        bytes(stranger["signing_key"]), kp
    )
    set_name = "writer-set-" + secrets.token_hex(3)
    conv_api.folder_create(port, stranger, set_name)
    conv_api.folder_share(port, stranger, set_name, group_id_hex)
    conv_api.folder_set_access(port, stranger, set_name, recipient_id, "writer")
    conv_api.welcome_deliver(
        port, stranger, recipient_id, channel_id_hex, welcome_bytes,
        kind={"type": "folder", "group_id": group_id_hex},
    )

    b.navigate_folders()
    assert b.wait_for_pending_shares(1) == 1, (
        f"the writer share should stage as one pending knock; error={app.error_text()!r}"
    )
    b.accept_pending_share(0)

    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if b.folder_count() >= 1:
            break
        time.sleep(0.5)
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 1, (
        f"the writer-access shared set should render as one row; "
        f"saw {b.folder_count()} row(s) error={app.error_text()!r}"
    )

    idx = b.find_and_expand_folder(set_name)
    # The reader row never expands to a folder-binding form — its presence here
    # IS the writer-vs-reader assertion (linux's `build_writer_member_folder_row`
    # is the ONLY member row with `folder-location-*`, mirrored here).
    # `is_visible_scrolled`, not a bare `is_visible`: this member row is the
    # ONLY row on the page, but on windows the just-expanded body can still
    # sit below the ScrollViewer's fold (`is_visible` reads UIA `IsOffscreen`,
    # which a freshly-arranged-but-unscrolled row reports honestly) — the same
    # below-the-fold class `is_visible_scrolled` exists for (its own docstring:
    # "cost three sessions on the gated admin-tab/family-tab rows"). Degrades
    # to a plain `is_visible` on a driver with no scroll-into-view support, so
    # this is a no-op change for macOS/tui.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if app.driver.is_visible_scrolled("folder-location-add-button"):
            break
        time.sleep(0.3)
    assert app.driver.is_visible_scrolled("folder-location-add-button"), (
        "a writer-access member row should render the local-folder binding form: "
        f"{app.driver.diagnose('folder-location-add-button')}"
    )
    # The leave affordance is still present (a writer is still a member, not an
    # owner) — folders.md's member-row contract holds regardless of access.
    assert app.driver.is_visible("folder-leave-button"), (
        "a writer-access member row should still carry folder-leave-button"
    )

    src = tmp_path / "writer-bind-src"
    src.mkdir()
    folder_path = str(src)
    app.driver.type_text("folder-location-path-input", folder_path)
    app.driver.click("folder-location-add-button")

    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            break
        time.sleep(0.3)
    assert app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1, (
        f"the bound folder did not render under the writer's row; "
        f"error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"binding the writer's folder surfaced an unexpected error: "
        f"{app.error_text()!r}"
    )
    # NOTE: unlike linux/tui's `test_bind_location_nested_under_folder`,
    # this does NOT also assert the agent's `config.toml` — `LocationsModel.add`
    # is optimistic (the row renders before the background agent exchange
    # resolves), and no macOS e2e test anywhere yet resolves the real
    # `fauna-sync-agent`'s config path under this harness (macOS's `config_home`
    # equivalent of linux/tui's `drivers/{linux,tui}.py` doesn't exist — a
    # standing gap for ANY macOS folder binding test, owned or shared, not
    # introduced by this leg). A real-persistence pass is a follow-on once that
    # macOS agent-config e2e seam exists.


# --- Conflict policy (file-sync.md § Conflicts; ui.yaml IDs user-approved 2026-07-11) ---
#
# Marked to the apps that render the two selects: apple (macos+ios), linux and
# windows. web + android have NOT lifted them (zero occurrences in their sources), so
# they are deselected rather than left standing-red — the house convention. Drop the
# marker for a client the moment it lands the surface; the gap is entrusted in
# internal follow-up tracks for web and android.


def _nest_sets(nest_url, test_user) -> list[dict]:
    """Every folder row the logged-in actor owns, straight off the nest
    (`fauna.folders.list`) — the ground truth these tests verify against. Find a
    set in it with `helpers.set_names.find_set`: a sealed set's row rests no
    plaintext name (schema 114), so it is found by its `name_hash`."""
    client = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        reply = client.call("fauna.folders.list", {})
    return list(reply.get("folders", []))


def _policy_of(nest_url, test_user, name: str) -> str | None:
    """The nest-authoritative `folders.conflict_policy` for the set called `name`.

    The ground-truth read that makes these tests wrong-row-proof: the picker renders on
    every owner row, so a mutation aimed at the wrong row would still read back
    consistently *in the UI*. Asserting on the named set's nest row cannot agree with a
    wrong-row write. Wire-additive (`Option<String>`), so an unset policy is `None` —
    which IS "auto" (`libs/fauna-protocol/src/folders.rs`).
    """
    row = find_set(_nest_sets(nest_url, test_user), name)
    if row is None:
        raise AssertionError(f"folder {name!r} is not on the nest")
    return row.get("conflict_policy")


def _wait_policy(nest_url, test_user, name: str, want, timeout: float = 20.0):
    """Poll the nest row until its conflict policy matches `want` (a set of accepted
    values — `auto` is `None` or the literal, depending on whether the client writes
    the default explicitly). Returns the last value seen, matching or not."""
    deadline = time.monotonic() + timeout
    seen = None
    while time.monotonic() < deadline:
        seen = _policy_of(nest_url, test_user, name)
        if seen in want:
            return seen
        time.sleep(0.3)
    return seen


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.feature("folders")
def test_folder_conflict_policy_round_trip(logged_in_app, nest_instance, test_user):
    """The per-set conflict policy is editable in place and persists to the nest.

    `folder-conflict-policy-select` (indexed, every owner row) writes
    `folders.conflict_policy` — the column the *resolving device* reads to decide
    whether to auto-merge or take latest-wins (`docs/goal/behavior/file-sync.md`
    § Conflicts). MUTATION is UI-driven (the picker); VERIFICATION reads the nest
    ground truth via `fauna.folders.list`, mirroring test_folder_webdav_toggle.

    backup-type sets have no conflict policy, so the select count tracks the number of
    *sync-type* sets, not the row count.
    """
    app = logged_in_app
    b = app.backups
    nest_url = nest_instance["url"]

    name = f"policy-set-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    row = b.find_and_expand_folder(name)

    # The picker renders on the owner row...
    assert b.conflict_policy_count() >= 1, (
        "a folder must render folder-conflict-policy-select: "
        f"{app.driver.diagnose('folder-conflict-policy-select')} "
        f"error={app.error_text()!r}"
    )
    # ...and a fresh set starts on the default policy (auto — merge where possible).
    assert _policy_of(nest_url, test_user, name) in (None, "auto"), (
        "a freshly-created set should start on the default 'auto' conflict policy"
    )

    # MUTATION (UI): flip THIS set's picker. `row` (the folder-row index) scopes
    # the pick by real subtree containment on every platform except web
    # (set_conflict_policy's own carve-out — web's flat `index=0` default covers
    # it there, since at most one row is ever expanded).
    b.set_conflict_policy("latest_wins_always", row=row)

    # VERIFICATION (nest ground truth, by NAME — a wrong-row write cannot pass this).
    seen = _wait_policy(nest_url, test_user, name, {"latest_wins_always"})
    assert seen == "latest_wins_always", (
        f"selecting latest_wins_always must persist to the nest row for {name!r}; "
        f"nest reports {seen!r}. If the UI reads back correctly but the nest does not, "
        f"the write landed on another row. error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"setting the conflict policy raised an error: {app.error_text()!r}"
    )

    # And back again — the edit is not one-way.
    if not b.conflict_policy_count():
        row = b.find_and_expand_folder(name)
    b.set_conflict_policy("auto", row=row)
    seen = _wait_policy(nest_url, test_user, name, {None, "auto"})
    assert seen in (None, "auto"), (
        f"selecting auto must persist to the nest row for {name!r}; got {seen!r}"
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.feature("folders")
def test_sync_default_conflict_policy_stamps_new_sets(
    logged_in_app, nest_instance, test_user
):
    """The page-level default is stamped onto NEWLY created sets, not existing ones.

    `sync-default-conflict-policy-select` (unindexed — one control in the page's "Sync
    defaults" section) is the `fauna.state.sync-prefs` `default_conflict_policy`, which the
    wizard reads at `open_wizard` and stamps onto the set it creates. That "new sets
    only" scope is the assertion: a set created BEFORE the default changes keeps its
    own policy.

    ⚠ Restores the default afterwards. `test_user` is SESSION-scoped, so a leaked
    `latest_wins_always` default would silently stamp every set a later test creates —
    the same shared-session contamination class as the spam-model teardown in
    test_mail_client_*.
    """
    app = logged_in_app
    b = app.backups
    nest_url = nest_instance["url"]

    b.navigate_folders()
    # A set created under the CURRENT default — its policy must survive the change below.
    before = f"policy-before-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(before)
    before_policy = _policy_of(nest_url, test_user, before)

    try:
        # MUTATION (UI): change the page-level default.
        b.navigate_folders()
        b.set_default_conflict_policy("latest_wins_always")

        # A NEW set is stamped with it...
        after = f"policy-after-{secrets.token_hex(4)}"
        b.create_folder_via_wizard(after)
        seen = _wait_policy(nest_url, test_user, after, {"latest_wins_always"})
        assert seen == "latest_wins_always", (
            "the sync-default conflict policy must be stamped onto a newly created "
            f"set; nest reports {seen!r} for {after!r}. error={app.error_text()!r}"
        )
        # ...while the set created beforehand keeps the policy it was made with.
        assert _policy_of(nest_url, test_user, before) == before_policy, (
            f"changing the default must NOT retro-edit the existing set {before!r} "
            "(the default is stamped at creation, not a live pointer)"
        )
        assert not app.has_error(), (
            f"changing the sync default raised an error: {app.error_text()!r}"
        )
    finally:
        # Session-scoped actor: restore the default so later tests create 'auto' sets.
        b.navigate_folders()
        b.set_default_conflict_policy("auto")


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.feature("folders")
def test_sync_default_conflict_policy_survives_relaunch(logged_in_app):
    """The page-level default conflict policy is read back from the account store
    after a relaunch — it is `fauna.state.sync-prefs` (`load_sync_prefs` /
    `save_sync_prefs`), not process memory.

    Non-default value on purpose: an absent default renders `auto`, so a page that
    forgot the choice would pass an assertion written against `auto`.

    ⚠ Restores the default afterwards. `test_user` is SESSION-scoped, so a leaked
    `latest_wins_always` would stamp every set a later test creates.
    """
    driver = logged_in_app.driver
    b = logged_in_app.backups

    try:
        b.navigate_folders()
        b.set_default_conflict_policy("latest_wins_always")
        assert b.get_default_conflict_policy() == "latest_wins_always", (
            "precondition: the default must actually be set before a relaunch can "
            "prove the page re-reads it"
        )

        # Same two preconditions as test_settings.py's inbox-mode relaunch test: the
        # client store and the injected identity must both survive the relaunch.
        if not driver.preserve_state_across_relaunch():
            skip_unbuilt(
                driver,
                surface="a client store that can be pinned across a relaunch",
                detail=(
                    "the driver hands the relaunched process a fresh store, so the "
                    "app returns signed out and cannot re-read the stored default"
                ),
                tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
            )
        if not driver.relaunch_preserves_injected_identity():
            skip_unbuilt(
                driver,
                surface="a login that survives a relaunch",
                detail=(
                    "logged_in_app injects the session with set_state and on this "
                    "app the injection does not reach a store the relaunch keeps, "
                    "so the stored default can never be re-read. The product "
                    "behaviour here is UNVERIFIED, not known-good"
                ),
                tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
            )
        assert driver.recover(), "the app did not come back up after the relaunch"
        reached_authenticated_app(driver, timeout=90)

        b.navigate_folders()
        # Deadline poll, not a settle-sleep (convention 14).
        wait_until(
            lambda: b.get_default_conflict_policy() == "latest_wins_always",
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                "after a relaunch the Sync defaults select must show the account's "
                f"stored default 'latest_wins_always', but it reports "
                f"{b.get_default_conflict_policy()!r} (an unchosen 'auto' means the "
                f"page never read the stored prefs). error={logged_in_app.error_text()!r}"
            ),
        )
    finally:
        # Session-scoped actor: restore the default so later tests create 'auto' sets.
        b.navigate_folders()
        b.set_default_conflict_policy("auto")


@pytest.mark.feature("folders")
def test_folder_device_activity_reflects_recorded_changes(
    logged_in_app, nest_instance, test_user,
):
    """Per-set device activity (`folder-device-activity-item`/-label/-count) —
    the ordinary sync change signal that makes web's `fauna.sync.changed`
    remote-change nudge e2e-pinnable.

    Records a change via the real `fauna.sync.changes.record` RPC (the
    production path `fauna-sync` itself uses, not a test backdoor) against a
    registered device — which fires a real `PushEvent::SyncChanged` at the
    already-connected browser socket (`notify_sync_changed`,
    `bins/fauna-nest/src/sync_handlers.rs`) — then asserts the expanded row's
    device-activity list picks it up with NO manual reload, and that a SECOND
    recorded change increments the count again, both within
    `PUSH_REFRESH_S` (well under the pre-existing 15s poll).
    """
    from common.auth import sync_changes_record, sync_register
    from helpers.budgets import PUSH_REFRESH_S
    from helpers.waiting import wait_until

    app = logged_in_app
    # All 7 apps render this now (apple — macOS + iOS — was the last residual): no skip gate left to guard.

    b = app.backups
    name = f"activity-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)

    # No recorded activity yet — an empty list, not an error.
    assert b.device_activity_item_count() == 0, (
        f"a freshly created set should show no device-activity rows yet: "
        f"count={b.device_activity_item_count()}"
    )
    assert not app.has_error(), (
        f"an empty device-activity list surfaced an unexpected error: {app.error_text()!r}"
    )

    device_id = secrets.token_hex(32)
    secret_key = test_user["signing_key"].encode().hex()
    sync_register(
        nest_instance["port"], secret_key=secret_key, device_id=device_id,
        label="activity-device", base_url=nest_instance["url"],
    )

    def _record_change() -> None:
        sync_changes_record(
            nest_instance["port"], secret_key=secret_key, folder=name,
            device_id=device_id, path=f"a-{secrets.token_hex(4)}.txt",
            manifest_hash=secrets.token_hex(32), size_bytes=10,
            change_type="create", base_url=nest_instance["url"],
        )

    # The row stays MOUNTED and EXPANDED throughout — no navigate(), no
    # re-expand, no manual reload. That is the whole point: a client with no
    # push arm wired to this section never converges at any timeout.
    _record_change()
    wait_until(
        lambda: b.device_activity_item_count() == 1,
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"item_count={b.device_activity_item_count()} "
            f"error={app.error_text() if app.has_error() else '(none)'}"
        ),
    )
    assert b.device_activity_change_count(0) == 1, (
        f"one recorded change should show change_count=1, got "
        f"{b.device_activity_change_count(0)}"
    )

    _record_change()
    wait_until(
        lambda: b.device_activity_change_count(0) == 2,
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"change_count={b.device_activity_change_count(0)} "
            f"error={app.error_text() if app.has_error() else '(none)'}"
        ),
    )
    assert not app.has_error(), (
        f"device-activity refresh surfaced an unexpected error: {app.error_text()!r}"
    )
