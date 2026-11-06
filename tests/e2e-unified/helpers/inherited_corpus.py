"""Seed a real image under the signed-in identity — the corpus a succession
journey later asks someone else to open.

Lifted 2026-09-26 from ``test_identity_succession_aftermath.py`` when a second
journey needed it: the recovery-kit restore after a succession
(``test_recovery_kit_restore.py``), whose "content sealed under the retired
identity opens" assertion is vacuous on an empty corpus for exactly the reason
the aftermath journey's is. One home, so the pre-ceremony assertions that make
both non-vacuous are kept once.

Since 2026-10-26 it also holds the two reads both journeys make after the
ceremony: the take-over's version (:func:`wait_for_the_take_over`) and the
restore of the predecessor's own version, the one read only a retired root
opens (:func:`restore_the_predecessors_version`).

Goal doc: ``docs/goal/behavior/succession-aftermath.md`` § Re-key scope (the
``BackupKey`` corpus row — media, folders and backups); the succession cut,
``docs/goal/architecture/succession-cut.md`` ruling (11)(d) and (f).
"""
from __future__ import annotations

import hashlib
import time
import uuid
from pathlib import Path

from helpers.app_surface import declared_absence
from helpers.diagnostics import row_judge_log_lines, take_over_log_lines
from helpers.succession_ceremony import (
    SUCCESSION_AND_RELAUNCH_S,
    kit_on_screen,
    require_stolen_gate,
    settled_actor_id,
)
from helpers.waiting import wait_until

# A kit and an actor id are both 32 bytes as lowercase hex.
_SECRET_HEX_LEN = 64

#: A real >300x300 PNG — an *image*, so the upload gesture also drives the
#: thumbnail producer, which is what gives a journey a byte-plane witness at all.
FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

#: The inherited corpus's names and bytes: `fauna.media.list` after a relaunch,
#: then the on-appear thumbnail fetch + decrypt for each listed item. Covers a
#: cold list plus a blob GET, not just a render (convention 14: a green run pays
#: none of it).
CORPUS_READ_S = 120.0

#: The local folder :func:`seed_an_image_in_a_bound_set` binds, under the
#: caller's ``tmp_path`` — named so a journey that reads the binding back (the
#: agent's ``ListLocations``, the OS's sync-root registration) addresses the
#: same directory without repeating the literal.
BOUND_CORPUS_DIR = "bound-corpus"


def require_folder_wizard(app) -> None:
    """Assert the folder create wizard's entry is on the Folders page.

    Separate from any Media gate because the two surfaces landed separately: an
    app can render the Media page and still have no way for a *user* to make
    the set an upload needs a target for.
    """
    app.backups.navigate_folders()
    assert app.is_visible("folder-add-button"), (
        "folder-add-button is not on the Folders page — every app has the create "
        "wizard that gives the upload a target (folders.md § Layout & flow), so "
        "this is a regression, not unbuilt debt:\n"
        + app.driver.diagnose("folder-add-button")
    )


def seed_an_image_in_a_bound_set(app, tmp_path: Path) -> tuple[str, str]:
    """Create a set, bind a real local folder to it, drop a real image into
    the folder, and prove the app's own engine uploaded it: listed and painted.
    Returns ``(set_name, file_name)``.

    The corpus a succession carries across **only** where a sync engine runs
    for the set on the owner's own host: the take-over (``succession-cut.md``
    ruling (11)(d)) re-records what the device holds, and a set no engine runs
    on stays history (residual (j)(6)). So a journey asking the successor to
    read its inherited corpus seeds it here, in the shape the ruling promises
    the corpus survives. The Media-page upload this module once offered beside
    it seeded a set no engine runs on, and was retired 2026-10-26 when its last
    caller moved here.

    ⚠ **The pre-ceremony assertions are the point, not scaffolding.** An upload
    that silently landed nowhere would leave an empty Media page, and *every*
    later "the corpus opens" assertion would pass by describing an empty
    corpus. The item must list **and paint** before the ceremony; that is what
    makes its absence afterwards mean something. Convention 8: the set is made through the wizard and the
    folder bound through the row's binding form; the file is the user's own,
    dropped on disk where the engine watches.
    """
    if app.driver.is_web() or app.driver.is_mobile():
        declared_absence(
            app.driver,
            capability="local-folder binding (no watched location tree), the "
            "only seed a succession carries across",
            doc="ui/folders.md § Binding (local-folder, desktop only)",
        )
    require_folder_wizard(app)
    set_name = f"succession-bound-{uuid.uuid4().hex[:8]}"
    app.backups.create_folder_via_wizard(set_name)
    watch_dir = tmp_path / BOUND_CORPUS_DIR
    watch_dir.mkdir()
    app.backups.bind_location(set_name, str(watch_dir))

    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    file_name = f"inherited-{uuid.uuid4().hex[:8]}.png"
    (watch_dir / file_name).write_bytes(FIXTURE_IMAGE.read_bytes())

    # Each poll is a fresh visit: a Media page that refreshes on becoming
    # visible and does not poll (linux) would otherwise keep a list read
    # before the engine's upload landed.
    def _listed() -> bool:
        app.media.navigate()
        return file_name in app.media.item_names()

    wait_until(
        _listed,
        CORPUS_READ_S,
        diagnose=lambda: (
            f"the engine bound to {set_name!r} must upload {file_name!r} BEFORE "
            "the ceremony or the journey proves nothing; "
            f"items={app.media.item_names()!r} error={app.error_text()!r}"
        ),
    )
    painted = app.media.wait_for_painted_thumbnails(1, timeout=CORPUS_READ_S)
    if painted is not None:
        assert painted == 1, (
            "the engine-uploaded image's thumbnail must paint BEFORE the "
            "ceremony, or a later paint assertion cannot tell a broken read "
            f"from a thumbnail never produced; got {painted}. "
            f"error={app.error_text()!r}"
        )
    return set_name, file_name


def the_owners_host(app, user, nest_instance, seats):
    """The device that holds the corpus and runs the ceremony: the app under
    test, or on a phone or web column a desktop seat of the same account.

    A phone or web binds no folder, so it cannot seed the one corpus a
    succession carries across, and the take-over runs in a sync engine on the
    owner's own host (``succession-cut.md`` ruling (11)(d)). Neither excuses it
    from an outcome about what the successor reads: either can be the device
    that reads. So the column keeps the outcome with the app under test as the
    device the kit brings up, and the desktop seat plays the host (a helper
    seat, ``feature-catalog.md`` § The marker: the outcome is observed on the
    app the run selected).
    """
    if not seats.binds_no_folder:
        return app
    host, _ = seats.launch("corpus-owners-host", user=user, nest=nest_instance)
    return host


def seed_under_a_then_succeed(
    app, tmp_path: Path
) -> tuple[str, str, list[tuple[str, str]]]:
    """bind a folder, drop an image in under A → A → B through the UI → read
    B's closing-act kit → wait for the take-over.

    Returns ``(inherited_file, successor_kit, before)``, ``before`` being the
    item's versions read before the ceremony. The steps every journey that
    reads the corpus on another device shares. ``app`` is the owner's host
    (:func:`the_owners_host`).

    - **The image is seeded into a BOUND set** (:func:`seed_an_image_in_a_bound_set`)
      and proven listed and painted: "content sealed under A opens" is
      otherwise satisfied by an empty corpus. Bound, because after the
      succession cut (``succession-cut.md`` ruling (11)) a head A signed judges
      *history* and only the take-over (11)(d) brings it back, which runs only
      in a sync engine on the owner's own host. A set no engine runs on stays
      history (residual (j)(6)) on every device, so a Media-page upload left
      the restored device listing ``[]`` by design (measured 2026-10-26 on tui:
      ``reminted: 1`` at the ceremony and nothing taken over).
    - **The successor's kit is read off the screen before any navigation**
      (``kit_on_screen``): entering Account clears a kit on screen (shown-once
      custody), and A's kit retired with A, so this is the one kit the user
      holds once the ceremony ends.
    - **The take-over lands on THIS device before anything else reads**
      (:func:`wait_for_the_take_over`). Any other device holds no local state
      for the set and never adopts history ((11)(d): no other device, however
      fresh, ever does), so a host gone before its take-over leaves the corpus
      history for good — residual (j)(6), not what these journeys ask.
    """
    _, inherited_file = seed_an_image_in_a_bound_set(app, tmp_path)
    before = item_versions(app, inherited_file)
    assert before, (
        "the inherited item must show its predecessor-recorded version before "
        "the ceremony, or the take-over has nothing to be told apart from; "
        f"versions={before!r} error={app.error_text()!r}"
    )

    old_actor = settled_actor_id(app)
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the Status page must render A's actor id first; got {old_actor!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN

    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)

    # The closing act's kit — read FIRST and with no navigation of our own.
    wait_until(
        lambda: kit_on_screen(app) not in ("", held),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no fresh kit on screen for the successor (reads "
            f"{kit_on_screen(app)!r}), error={app.error_text()!r}"
        ),
    )
    successor_kit = kit_on_screen(app)
    assert len(successor_kit) == _SECRET_HEX_LEN

    wait_until(
        lambda: settled_actor_id(app) not in ("", old_actor),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"still reads actor={settled_actor_id(app)!r} (old={old_actor!r}) "
            f"error={app.error_text()!r}"
        ),
    )
    wait_for_the_take_over(
        app,
        inherited_file,
        before,
        context=lambda: "(on the ceremony's own device, before anything else reads)",
    )
    return inherited_file, successor_kit, before


def item_versions(app, name: str) -> list[tuple[str, str]]:
    """The ``(author, timestamp)`` of every version of the Media item ``name``,
    read from a fresh visit's item detail — ``[]`` while the item is not
    listed. The detail's list loads asynchronously, so an empty read is
    re-tried briefly before it counts as empty.

    The page's own list loads asynchronously too: on windows a nav click
    rebuilds the page, so a read straight after it found no rows and called
    a listed item unlisted (measured 2026-11-04: both restore
    journeys red on the first try, green alone). So the read waits for the page
    to SAY it loaded (rows, or ``media-empty-state``) — the app's own
    observable, never a settle (convention 14)."""
    app.media.navigate()
    app.media.wait_for_loaded()
    names = app.media.item_names()
    if name not in names:
        return []
    app.media.open_item_detail(names.index(name))
    try:
        deadline = time.monotonic() + 10.0
        while app.media.version_count() < 1 and time.monotonic() < deadline:
            time.sleep(0.3)
        return [
            (app.media.version_author(i), app.media.version_timestamp(i))
            for i in range(app.media.version_count())
        ]
    finally:
        app.media.close_detail()


def wait_for_the_take_over(
    app, name: str, before: list[tuple[str, str]], *, context=lambda: ""
) -> list[tuple[str, str]]:
    """Wait until the item ``name`` lists with a version the take-over recorded
    as the successor beside the predecessor's (``succession-cut.md`` ruling
    (11)(d)), and return its versions.

    Never the name alone: the launch reconcile's cut lands some time after the
    actor switch, and until it does the predecessor's head is still the
    admitted head, so "the name lists" alone passed by reading ahead of the cut
    (measured 2026-10-21: one green run in 23 s). The take-over's own version
    exists only after the cut, so nothing earlier satisfies this poll
    (convention 14). Authors and the count only: a version's timestamp is
    relative text ("just now" → "1 min ago") and changes with no new version.

    ``context`` adds the caller's own line to the diagnosis.
    """
    before_authors = {author for author, _ in before}

    def _taken_over() -> bool:
        versions = item_versions(app, name)
        return bool(versions) and (
            len(versions) > len(before)
            or any(author not in before_authors for author, _ in versions)
        )

    wait_until(
        _taken_over,
        CORPUS_READ_S,
        diagnose=lambda: (
            "the successor must list the media it inherited with the version "
            "the take-over recorded as itself (succession-cut.md ruling "
            f"(11)(d)), but lists {app.media.item_names()!r} (expected "
            f"{name!r}), versions {item_versions(app, name)!r} against "
            f"{before!r} before the ceremony; error={app.error_text()!r}. "
            "Absent from the list: the cut made the head history and no "
            "take-over re-recorded it (the engine never ran for the set, or "
            "did not hold the path), or no retired root opened its label — "
            "UNLESS the row judge refused the predecessor-signed rows: row-judge "
            f"log {row_judge_log_lines(app)}. Listed with only the predecessor's "
            "version: the cut has not landed yet, or landed and the take-over "
            f"has not run. {context()} Cut and take-over log: "
            f"{take_over_log_lines(app)}"
        ),
    )
    return item_versions(app, name)


def restore_the_predecessors_version(
    app, name: str, before: list[tuple[str, str]], *, context=lambda: ""
) -> None:
    """Restore the item ``name``'s newest PREDECESSOR-recorded version from its
    version history, then prove the restored head paints and downloads as the
    seeded image — the one read of an inherited corpus only a retired root opens.

    After the take-over every listed name and painted tile reads what it
    re-sealed under the successor's own root, so all of that stays green with
    no retired root offered at all. The predecessor's own version row is the
    one thing still sealed under the retired root: the take-over records
    beside it and supersedes nothing (``succession-cut.md`` ruling (11)(d)/(f)).
    Restoring it is the read: a version another signature recorded, whose
    bytes rest under an owner root, is restored only once its bytes have OPENED
    under a root that signature may reach and been re-sealed under the current
    one (``writer-signed-change-records.md`` ruling (8)(d),
    ``MediaMachine::restore_version`` → ``reseal_inherited_version``). With no
    retired root offered the open fails and the restore records nothing,
    setting ``media.error_restore`` instead.

    ``before`` is the item's version list read BEFORE the ceremony. The row is
    picked by POSITION, never by author: the successor keeps the account's
    name, so both rows read the same ``Edited by …`` (measured 2026-10-22).
    Rows are oldest→newest and the take-over records after the ceremony, so
    the predecessor's newest version is the last of the ``before`` rows.

    Then two reads of what the restore re-sealed. **The paint**: a thumbnail
    rests as its file rests (``media.md`` § Encryption at rest), so the
    predecessor's version row names a thumbnail sealed under the retired root
    too; the restore opens it under the roots the version's signer may reach
    and re-seals it beside the bytes, or records the version WITHOUT one when
    it does not open (*A thumbnail rests as its file rests*, bound (2)). Read
    BEFORE the download, whose backfill re-renders a missing thumbnail.
    **The download** then proves the re-seal carried the predecessor's bytes,
    not merely some bytes.

    ``context`` adds the caller's own line to every diagnosis — which blob or
    which device supplied the retired root, say.
    """
    after = item_versions(app, name)
    assert len(after) > len(before), (
        "the predecessor's own version must still be listed beside the "
        "take-over's (succession-cut.md ruling (11)(f): the take-over "
        f"supersedes nothing), but versions {after!r} against {before!r} "
        "before the ceremony. On a device that ran the ceremony this is the "
        "take-over superseding history; on a device that never held the "
        "predecessor (a restore after losing every device) it is this device "
        "holding no predecessor material, so it attests no retired identity "
        "and lists no row that identity signed — measured 2026-10-26 with the "
        f"succession kit's predecessor seeds emptied. {context()}"
    )
    predecessor_row = len(before) - 1
    app.media.navigate()
    app.media.wait_for_loaded()  # the list loads after the nav (item_versions)
    app.media.open_item_detail(app.media.index_of(name))
    try:
        app.media.wait_for_version_count(len(after))
        app.media.restore_version(predecessor_row)
        # Read in the OPEN detail, which re-lists its rows once the restore
        # settles: the restore's own version is what makes the head the
        # re-sealed predecessor bytes (convention 14: wait for the row, not a
        # time), and its error banner is read here because a later visit's
        # refresh clears it (measured 2026-10-22: `error=''` after a revisit).
        wait_until(
            lambda: app.media.version_count() > len(after) or bool(app.error_text()),
            CORPUS_READ_S,
            diagnose=lambda: (
                "restoring the predecessor's version must record a new version "
                f"(file-sync.md § Restore) or report why; {app.media.version_count()} "
                f"rows against {len(after)} before the restore. {context()}"
            ),
        )
        restore_error = app.error_text()
        assert not restore_error and app.media.version_count() > len(after), (
            "the successor must open the version its predecessor sealed "
            "(identity-succession.md § Implementation status today — a "
            "successor READS its predecessor-sealed corpus): restoring it "
            "opens its bytes under a retired root before re-sealing them "
            "(writer-signed-change-records.md ruling (8)(d)), but the restore recorded "
            f"{app.media.version_count() - len(after)} new version(s) and "
            f"reported {restore_error!r}. A `media.error_restore` here with "
            "the take-over's version listed is the dark-history state: the "
            "successor reads what the take-over re-sealed and cannot open "
            f"what its predecessor left. {context()}"
        )
    finally:
        app.media.close_detail()

    # The tile paints the head's own `thumbnail_hash` and nothing re-renders
    # one on view, so a painted tile here is the retired-root open having
    # succeeded. The re-entry re-lists the page (`MediaActions.reenter`).
    app.media.reenter()
    painted_restored = app.media.wait_for_painted_thumbnails(1, timeout=CORPUS_READ_S)
    if painted_restored is not None:
        assert painted_restored == 1, (
            "the successor must paint the thumbnail of the version its "
            "predecessor sealed once restored (succession-aftermath.md "
            "§ Implementation status today — the restore opens the version's "
            "thumbnail under the retired roots and re-seals it with its bytes), "
            f"but {painted_restored} of 1 thumbnails painted; "
            f"error={app.error_text()!r}. The restore recorded and the tile a "
            "placeholder is the thumbnail not opening under any retired root "
            "(the app log names 'the restored version's thumbnail did not "
            f"re-seal'). {context()}"
        )

    app.media.open_item_detail(app.media.item_names().index(name))
    try:
        try:
            downloaded = app.media.download_open_item(timeout=CORPUS_READ_S)
        except AssertionError as exc:
            raise AssertionError(
                "the restored predecessor version must download (the head now "
                f"names its re-sealed bytes), but it never saved: {exc}; "
                f"error={app.error_text()!r}. {context()}"
            ) from exc
    finally:
        app.media.close_detail()
    expected = hashlib.sha256(FIXTURE_IMAGE.read_bytes()).hexdigest()
    got = hashlib.sha256(downloaded).hexdigest()
    assert got == expected, (
        "the restored predecessor version must carry the bytes its "
        "predecessor sealed (the re-seal moves them under the current root "
        f"verbatim), but the download of the restored version "
        f"hashed {got} ({len(downloaded)} bytes) against the seeded image's "
        f"{expected}; error={app.error_text()!r}. {context()}"
    )
