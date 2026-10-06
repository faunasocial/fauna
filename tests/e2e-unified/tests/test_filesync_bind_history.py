"""Fresh-bind onto a set WITH HISTORY must not destroy the user's local file.

The two owed tier_3 legs of the 2026-07-24 delete-then-create fixes
(both batch-latest folds; the synced-bytes
delete guard), whose only prior coverage was tier_1 units that point the engine
at an unreachable URL. This module drives the REAL flow — client UI → external
``fauna-sync-agent`` → local nest → ``changes.list`` replay — for the exact
shape that destroyed data live on 2026-07-24: a folder freshly bound to a set
whose history holds add-then-delete for a path the user already has a newer
file at.

ONE scenario pins BOTH arms, because they are two halves of one replay batch:

* **Create arm** (the destructive half): replaying ``create@N`` when
  ``delete@N+1`` follows downloads a stale version OVER the user's newer file.
  The fold (``batch_latest_seq``) must skip it. **Its observable here MOVED
  on 2026-09-20 — from conflict NOISE to PROGRESS** (ruled in
  ``docs/goal/behavior/conflicts.md`` clause 5): the stale create is no
  longer downloaded-and-fought-off, it is DEFERRED one rung above the merge
  arm, and a deferred row holds the anchor below itself, so Q (seq above it)
  never arrives. The barrier assertion is what reds now; the conflict count
  stays as the no-noise floor. Read the 2026-09-20 note below before reading
  a zero conflict count as proof of the fold.
* **Delete arm**: applying ``delete@N+1`` to a path holding content the nest
  never had destroys it. Depending on startup interleaving the protection is
  the fold (the fresh upload's own ``create@M`` supersedes the tombstone in the
  same batch) or the synced-bytes guard (an unsynced row is never the
  tombstone's subject) — the test is deliberately robust to either order, and
  a regression in either arm reds it (see the red-verify note below).

The one-batch requirement the win-seat recon named: both changes must arrive in ONE ``changes.list`` batch, else
``batch_latest_seq`` never contains the later seq and the test passes vacuously
against unfixed code. Guaranteed structurally here: the second launch is a
FRESH isolated world (per-launch agent data dir → per-set sync db → anchor 0),
so its first pull fetches the set's whole history and
``apply_remote_changes`` folds it as one batch. That is also exactly the live
2026-07-24 repro (a fresh machine binding onto history).

RED-VERIFIED 2026-07-29 (mac, tui), each arm independently against a locally
reverted hunk in ``libs/fauna-sync-engine/src/engine.rs`` (agent rebuilt per
revert, test rerun):

* delete-arm fold + guard reverted (apply the tombstone whenever the file
  exists — the pre-fix arm) → ``[delete arm] 'kept.txt' is GONE`` — the
  replayed tombstone destroys the user's file;
* create-arm fold ``continue`` neutered → ``[create arm] the fresh bind
  produced 1 conflict review row(s)``. Defense-in-depth finding worth keeping:
  with the fold gone the stale download is *fought off by conflict
  auto-resolve* (mtime newest-wins keeps the local file and records a
  conflict), so the final CONTENT assertion alone cannot see the fold — the
  fold's own observable is **zero conflict rows** (a skipped change conflicts
  with nothing; a downloaded one leaves audit noise for a file the user never
  edited concurrently). The content assertions still stand as the
  no-user-data-loss floor — if auto-resolve ever picked the stale side, they
  red.

RE-VERIFIED 2026-09-19 (linux), after the premise fixes below: the
delete-arm revert (fold ``false &&``, tombstone applied whenever the file
exists) still reds as ``[delete arm] 'kept.txt' is GONE``. The create-arm fold
neutered alone stayed GREEN that day — no conflict row and the user's content
intact — reading as "the create arm is no longer observable here".

RE-MEASURED 2026-09-20 (linux, tui) — that reading was too weak. Clean tree: green in 343 s. Create-arm fold alone
neutered (``false &&`` over ``content_superseding_seq ... && fold_licensed``):
**RED**, at the replay barrier rather than the conflict count, and the agent
log says exactly why —

    file uploaded path="kept.txt"                       <- the startup converge
    fauna.sync.changes: pull served=4 since=0
    causal: verbatim adopt deferred - own pending rows unlisted
        (transient cap; anchor holds) path="kept.txt" seq=11
    pulled remote changes changes=0 anchor=10           <- and again, every pull

Every caller's startup path converges the LOCAL half before the first pull, so
the fresh bind has RECORDED its own ``kept.txt`` when the batch is judged: an
untracked path frontier plus that recorded witness make the stale create
judge FAST-FORWARD, which the leg-4 DEFER cap refuses while the live base is
empty. The cap holds the anchor BELOW the stale create — so Q, which sits
ABOVE it, never applies, and neither does this device's own echo (the one
thing that would advance the frontier and release the cap: measured parked at
tier_1 too, so the cap's release rung is its own open question).
So the create arm IS still observable here: through the barrier, not the
conflict row. The 2026-07-29 conflict-row observable is genuinely gone — the
cap now intercepts above the merge arm that used to record it.

RULED CLOSED 2026-09-21 (``docs/goal/behavior/conflicts.md`` clause 5, the
fresh-bind-park record): the shared judge's
untracked-frontier arms now honour the edit-frontier the record ack counted,
so a stale create that predates the fresh bind's own row is judged the SIBLING
it is — merged, never capped. Two consequences for this module: the park is
unreachable (Q always materializes; the second test below is the CARRIER-LESS
shape, where no fold can hide it), and a neutered fold's red signature moves
BACK to the conflict count — the sibling merge records a conflict row — while
the barrier assertion stays as the park detector.

The causal barrier for the negative claim ("the stale body was never written"):
history also carries a second, never-deleted file Q which the fresh bind MUST
download — Q materializing proves the anchor-0 replay applied before the
assertions read P (convention 14: negative asserts anchor to causal barriers,
never settle-sleeps).

Nest-side witnesses go through the Media explorer UI with
:meth:`MediaActions.reenter` per read (the listing pulls the nest only on a nav
edge — the run-``20260724-07`` lesson): the tombstone is "P dropped from the
listing" (``fauna.media.list`` excludes tombstoned members) and the survivor's
re-upload is "P listed again". The reader actor is DEDICATED to this test, so
the all-media view is exactly this set's content — no filter dependence.

Windows was skipped as UNVERIFIED, then briefly as VERIFIED FAILING: a
2026-07-31 run found the tui app dying with ``thread 'main' has overflowed its
stack`` before any sync assertion, making this test a BYSTANDER to an app
crash. **That crash is fixed and the skip is gone** — see ``apps/fauna-tui``'s
``build.rs`` and ``app::PageOp::run`` for the two halves.

⚠ Two claims from that filing were WRONG; do not inherit them.

1. *"The app falls back to the identity screen and the bridge is already dead
   at the first navigation."* No: the POST of the nav command SUCCEEDED (a dead
   bridge raises ``BridgeDead`` from ``_post``; the traceback reached the
   *ack-timeout* line below it). The app signed in, painted the feed and
   reached "Connected" — the nav to Settings is what killed it.
2. *"A win user running fauna-tui hits an app crash."* No: measured peak
   main-thread stack for that nav was 837 KiB in a **debug** build against
   26 KiB in **release**. Only debug/e2e builds ever approached Windows'
   1 MiB reserve; shipped release artifacts were never at risk.

The real blast radius was wider than this test: *every* Settings sub-page was
unreachable on windows tui (bare ``settings`` crashed too), while every other
top-level page had room to spare.

tui and linux both run it (linux joined 2026-09-19): both arms are the shared
engine's (``fauna_sync_engine::engine``), and both apps spawn the same external
``fauna-sync-agent`` through the shared ``ChildSpawner``; the linux driver's
per-launch XDG world gives the second launch the same anchor-0 sync db.

It was red on every app from at least 2026-08-28 to 2026-09-19 for two harness
reasons, neither a product fault: both launches signed in with the SAME device
id, so the second read the whole history as its own echo (`_launch_signed_in`);
and, run without the session ``test_user`` fixture, the first launch alone
filled the ``free`` tier's two-device cap, so the second's upload was refused
(the ``set_tier_caps`` call in the test).
"""

import secrets
import time
from pathlib import Path

import pytest

from actions import ActionLayer
from actions.media import MediaActions
from common.auth import UNBINDING_MAX_DEVICES, create_actor_and_register, set_tier_caps
from conftest import _E2E_LOGIN_DEVICE_ID, _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.folder_content import agent_diagnosis, atomic_write, bind_location_under_set

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

#: Apps whose seat drives a real `fauna-sync-agent` this test can reach. Grow
#: this and the module's markers together.
_SUPPORTED_APPS = ("tui", "linux")

# Engine start + eager pull + watcher-driven upload all ride the bind; the 60 s
# scan cadence (frequency index 0) is the backstop, so every window comfortably
# covers a full cadence miss. Named budgets, deadline polls — green runs pay
# only the real latency (convention 14).
_LISTING_S = 120.0
_REPLAY_S = 120.0

_P = "kept.txt"  # the path with add-then-delete history AND a newer local file
_Q = "history-marker.txt"  # never deleted — its download is the replay barrier

_STALE_BODY = "stale body v1 — must never land on disk again\n"
_USER_BODY = "the user's newer content — destroying this is data loss\n"
_Q_BODY = "history marker — the fresh bind must download me\n"

# The carrier-less shape: a path the peer created and NEVER deleted, which the
# fresh bind already holds a DIFFERENT file at; and a file written after the
# bind, whose upload proves the folder still syncs outbound.
_C = "same-name.txt"
_PEER_C_BODY = "the peer's file at the shared name — never deleted\n"
_USER_C_BODY = "the user's own file at the shared name — created before the bind\n"
_LATER = "after-bind.txt"
_LATER_BODY = "written after the bind — proves the folder still syncs\n"


@pytest.fixture(params=[a for a in _SUPPORTED_APPS if a in get_available_apps()])
def sync_app(request):
    """The app under test; its id lands in the test name (``[tui]``/``[linux]``),
    which is what conftest's ``--app`` filter reads."""
    return request.param


def _launch_signed_in(
    app_name, app_path, nest_instance, actor, request, *, device_id: str
) -> ActionLayer:
    """A FRESH isolated launch (private HOME/XDG per the driver's point-10
    isolation → fresh agent data dir → fresh per-set sync db → anchor 0),
    signed in as ``actor`` via the fixture-setup ``set_state`` carve-out.

    ``device_id`` is the seat's own: the engine answers "did this seat write
    this row?" by device id + author (`engine.rs::row_is_own`), so two launches
    standing in for two machines of one actor MUST carry different ones. Given
    the same id, the second launch reads the whole history as its own echo,
    applies none of it and advances its anchor past it — the replay barrier
    then never materializes (one of the two reasons this test was red; module
    docstring)."""
    driver = create_driver(app_name)
    driver.launch(
        {
            "app_path": app_path,
            "url": nest_instance["url"],
            "environment": {
                **_seeded_environment(request, nest_instance),
                # Diagnosis only: the engine's lines land in the agent's own
                # log under the launch's private data dir, not the app stderr.
                "RUST_LOG": "info,fauna_sync_engine=debug",
            },
        }
    )
    driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": actor["signing_key"].encode().hex(),
                "handle": "bindhist-user",
                "actor_id": actor["actor_id_hex"],
                "device_id": device_id,
            },
            "nav": {"stack": [{"view": "feed"}]},
        }
    )
    return ActionLayer(driver)


def _await_listing(app, predicate, *, describe: str) -> list[str]:
    """Deadline-poll the Media listing (one :meth:`MediaActions.reenter` per
    read — each poll re-pulls the nest) until ``predicate(names)`` holds."""
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
        f"(last read: {names!r}; error={app.error_text()!r})\n"
        f"{agent_diagnosis(app, 'listing')}"
    )


def _adopt_set(app, set_name: str) -> None:
    """Poll with page round-trips until the (already created) set's row shows —
    a fresh mount re-fetches the list, same as the multiseat adopt."""
    b = app.backups
    deadline = time.monotonic() + _LISTING_S
    while time.monotonic() < deadline:
        b.navigate_folders()
        app.driver.wait_for("folder-add-button", timeout=30)
        for i in range(b.folder_count()):
            if set_name in b.folder_title(i):
                return
        time.sleep(1.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(f"folder {set_name!r} never appeared in the second launch's list")


def _await_file(path: Path, expected: str, *, describe: str, app, seat: str) -> None:
    deadline = time.monotonic() + _REPLAY_S
    actual: str | None = None
    while time.monotonic() < deadline:
        try:
            actual = path.read_text()
        except (FileNotFoundError, OSError):
            actual = None
        if actual == expected:
            return
        time.sleep(1.0)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    pytest.fail(
        f"{describe}: {path.name!r} never held the expected content within "
        f"{_REPLAY_S:.0f}s (last read: {actual!r})\n{agent_diagnosis(app, seat)}"
    )


@pytest.mark.feature("local-folder-sync")
def test_fresh_bind_onto_tombstoned_history_preserves_local_file(
    sync_app, nest_instance, tmp_path, request
):
    app_path = request.getfixturevalue(f"{sync_app}_app_path")

    # Two launches stand in for two machines of one actor, and the first alone
    # fills the shipped `free` tier's two-device cap (its sync device and its
    # account runtime's machine row), so the second's first upload is refused
    # `fauna.sync.device_limit_exceeded` and the survivor never re-uploads.
    # Lifted exactly as the session `test_user` fixture lifts it, and for the
    # same reason (the cap is proven nest-side — `set_tier_caps`); left to that
    # fixture's side effect, this test passed or failed on whether an earlier
    # test in the run had happened to request it.
    set_tier_caps(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES,
        base_url=nest_instance["url"],
    )
    # A DEDICATED actor (admin-admitted — the shared nest's registration is
    # closed), so the all-media listing reads are exactly this test's files.
    actor = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    set_name = f"bindhist-{secrets.token_hex(4)}"

    # ── Launch 1: arrange the history — P created then deleted, Q kept ──────
    folder_a = tmp_path / "history-writer"
    folder_a.mkdir()
    app1 = _launch_signed_in(
        sync_app, app_path, nest_instance, actor, request, device_id=_E2E_LOGIN_DEVICE_ID
    )
    try:
        b = app1.backups
        b.navigate_folders()
        app1.driver.wait_for("folder-add-button", timeout=30)
        b.create_folder_via_wizard(set_name)
        bind_location_under_set(app1, set_name, folder_a, seat="history-writer")

        atomic_write(folder_a / _P, _STALE_BODY)
        atomic_write(folder_a / _Q, _Q_BODY)
        _await_listing(
            app1,
            lambda names: _P in names and _Q in names,
            describe=f"both uploads ({_P!r} create@N, {_Q!r})",
        )

        # The local delete records the tombstone (delete@N+1) — witnessed on
        # the nest by `fauna.media.list` dropping the member (tombstones are
        # excluded from the listing).
        (folder_a / _P).unlink()
        _await_listing(
            app1,
            lambda names: _P not in names and _Q in names,
            describe=f"the tombstone for {_P!r} (listing drops it, keeps {_Q!r})",
        )
    finally:
        app1.driver.teardown()  # reaps the launch's agent (point 9/10)

    # ── Launch 2: a fresh world binds a folder ALREADY holding a newer P ────
    folder_b = tmp_path / "fresh-bind"
    folder_b.mkdir()
    atomic_write(folder_b / _P, _USER_BODY)

    app2 = _launch_signed_in(
        sync_app, app_path, nest_instance, actor, request, device_id=secrets.token_hex(32)
    )
    try:
        _adopt_set(app2, set_name)
        bind_location_under_set(app2, set_name, folder_b, seat="fresh-bind")

        # Causal barrier: Q materializing proves the anchor-0 history replay
        # (the batch carrying create@N + delete@N+1 for P) has applied.
        _await_file(
            folder_b / _Q, _Q_BODY, describe="the replay barrier", app=app2, seat="fresh-bind"
        )

        # The two arms, post-replay: the stale create@N body was never written
        # over the user's file, and the tombstone never deleted it.
        assert (folder_b / _P).exists(), (
            f"[delete arm] {_P!r} is GONE from the freshly bound folder — the "
            f"replayed tombstone was applied to a file whose content the nest "
            f"never had (user-data loss; principles.md § No user-data loss)"
        )
        assert (folder_b / _P).read_text() == _USER_BODY, (
            f"[create arm] {_P!r} no longer holds the user's content — the "
            f"stale create@N body was downloaded over it "
            f"(got: {(folder_b / _P).read_text()!r})"
        )

        # The survivor must UPLOAD: P was tombstone-excluded from the listing,
        # so it reappearing proves a fresh create recorded on the nest.
        _await_listing(
            app2,
            lambda names: _P in names and _Q in names,
            describe=f"the survivor's re-upload ({_P!r} listed again)",
        )

        # And surviving the sync loop settled — the content is still the
        # user's after the re-list (no late clobber).
        assert (folder_b / _P).read_text() == _USER_BODY

        # [create arm, the fold's own observable] The stale create@N must be
        # SKIPPED, not fought off: without the fold the download runs and the
        # conflict auto-resolve machinery rescues the content (mtime
        # newest-wins) while recording a conflict — the final bytes then look
        # fine, but the user gets spurious conflict noise for a file they
        # never edited concurrently. Zero conflict rows is what proves the
        # fold. Sound zero-read: on every app the folder rows and the
        # conflict review list ride ONE machine snapshot, so the set row
        # having rendered (find_and_expand above / re-check here) proves the
        # conflicts field of that same snapshot is loaded.
        b2 = app2.backups
        b2.navigate_folders()
        b2.find_and_expand_folder(set_name)
        conflict_rows = app2.driver.count("conflict-file-info")
        assert conflict_rows == 0, (
            f"[create arm] the fresh bind produced {conflict_rows} conflict "
            f"review row(s) — the stale create@N reached the merge arm "
            f"instead of being FOLDED away (batch_latest_seq skip, "
            f"9aaa3c02f). Since 2026-09-20 the fold's own red signature is "
            f"the replay barrier above (a neutered fold DEFERS the stale "
            f"create and parks the anchor below it); a conflict row here "
            f"means something reached auto-resolve that the cap used to "
            f"refuse — read the module docstring's 2026-09-20 note"
        )
    finally:
        app2.driver.teardown()


@pytest.mark.feature("local-folder-sync")
def test_fresh_bind_onto_a_same_named_never_deleted_file_keeps_syncing(
    sync_app, nest_instance, tmp_path, request
):
    """The CARRIER-LESS fresh bind — the live user-facing symptom of the
    fresh-bind park (``docs/goal/behavior/conflicts.md`` clause 5, ruled closed
    2026-09-21).

    The tombstoned test above is saved by the create-arm fold: the tombstone
    is a carrier the stale create folds under. Bind a folder holding
    ``same-name.txt`` onto a set where another device created a DIFFERENT
    ``same-name.txt`` and never deleted it, and there is nothing to fold: the
    fresh bind's startup converge records its own file, the first pull lists
    the peer's earlier create for the same path, and pre-ruling the judge read
    that create as a fast-forward the DEFER cap then held — parking the
    anchor below it, so nothing above it (Q here) ever arrived and the folder
    silently stopped receiving. No error, no conflict row, no download: the
    only symptom was silence. Under the ruling the peer's create is judged the
    SIBLING it is and merged, so the replay passes it.

    Three assertions, each a causal barrier or a byte comparison (convention
    14): Q materializes (the anchor passed the peer's create); the user's own
    bytes are still at the shared path — kept outright or inside a merge,
    never silently replaced by the peer's file (no-user-data-loss); and a file
    written AFTER the bind uploads and lists, so the folder syncs outbound too.
    The conflict row the sibling merge may record is deliberately not asserted
    either way: two devices genuinely created different files at one path, so
    a review row is honest, and which resolution the auto-resolver picks is
    ``conflicts.md``'s to specify, not this test's.
    """
    app_path = request.getfixturevalue(f"{sync_app}_app_path")

    # Same two harness lifts as the tombstoned test (module docstring).
    set_tier_caps(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES,
        base_url=nest_instance["url"],
    )
    actor = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    set_name = f"bindsame-{secrets.token_hex(4)}"

    # ── Launch 1: the peer creates C, then Q (Q's seq sits ABOVE C's) ──────
    folder_a = tmp_path / "peer"
    folder_a.mkdir()
    app1 = _launch_signed_in(
        sync_app, app_path, nest_instance, actor, request, device_id=_E2E_LOGIN_DEVICE_ID
    )
    try:
        b = app1.backups
        b.navigate_folders()
        app1.driver.wait_for("folder-add-button", timeout=30)
        b.create_folder_via_wizard(set_name)
        bind_location_under_set(app1, set_name, folder_a, seat="peer")

        atomic_write(folder_a / _C, _PEER_C_BODY)
        _await_listing(app1, lambda names: _C in names, describe=f"the peer's {_C!r} create")
        # Q AFTER C, so its row is the one a cap parked below C would strand.
        atomic_write(folder_a / _Q, _Q_BODY)
        _await_listing(
            app1,
            lambda names: _C in names and _Q in names,
            describe=f"the barrier {_Q!r} above {_C!r}",
        )
    finally:
        app1.driver.teardown()

    # ── Launch 2: a fresh world binds a folder ALREADY holding its own C ───
    folder_b = tmp_path / "fresh-bind"
    folder_b.mkdir()
    atomic_write(folder_b / _C, _USER_C_BODY)

    app2 = _launch_signed_in(
        sync_app, app_path, nest_instance, actor, request, device_id=secrets.token_hex(32)
    )
    try:
        _adopt_set(app2, set_name)
        bind_location_under_set(app2, set_name, folder_b, seat="fresh-bind")

        # The park detector: Q can only land once the anchor has passed the
        # peer's C — a capped-and-parked replay never writes it.
        _await_file(
            folder_b / _Q,
            _Q_BODY,
            describe="the replay barrier (a parked cap never lets Q past the peer's create)",
            app=app2,
            seat="fresh-bind",
        )

        # No user-data loss: the user's bytes are still at the shared path —
        # kept, or inside a merge — never silently replaced by the peer's file.
        c_text = (folder_b / _C).read_text()
        assert "the user's own file at the shared name" in c_text, (
            f"{_C!r} no longer holds the user's content after the replay "
            f"(got: {c_text!r}) — the peer's create was adopted over it"
            f"\n{agent_diagnosis(app2, 'fresh-bind')}"
        )

        # And the folder still syncs OUTBOUND after the replay: a file written
        # now uploads and lists (a parked engine never got here either — its
        # watcher loop was live, but the user saw nothing arrive and nothing
        # they could do about it).
        atomic_write(folder_b / _LATER, _LATER_BODY)
        _await_listing(
            app2,
            lambda names: _LATER in names and _Q in names and _C in names,
            describe=f"the post-bind upload ({_LATER!r} listed)",
        )
    finally:
        app2.driver.teardown()
