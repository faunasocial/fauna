"""Seed a real image under the signed-in identity — the corpus a succession
journey later asks someone else to open.

Lifted 2026-09-26 from ``test_identity_succession_aftermath.py`` when a second
journey needed it: the recovery-kit restore after a succession
(``test_recovery_kit_restore.py``), whose "content sealed under the retired
identity opens" assertion is vacuous on an empty corpus for exactly the reason
the aftermath journey's is. One home, so the pre-ceremony assertions that make
both non-vacuous are kept once.

Goal doc: ``docs/goal/behavior/succession-aftermath.md`` § Re-key scope (the
``BackupKey`` corpus row — media, folders and backups).
"""
from __future__ import annotations

import uuid
from pathlib import Path

from helpers.waiting import wait_until

#: A real >300x300 PNG — an *image*, so the upload gesture also drives the
#: thumbnail producer, which is what gives a journey a byte-plane witness at all.
FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

#: The inherited corpus's names and bytes: `fauna.media.list` after a relaunch,
#: then the on-appear thumbnail fetch + decrypt for each listed item. Covers a
#: cold list plus a blob GET, not just a render (convention 14: a green run pays
#: none of it).
CORPUS_READ_S = 120.0


def require_folder_wizard(app) -> None:
    """Skip, declaring the class, on an app without the folder create wizard.

    Separate from any Media gate because the two surfaces landed separately: an
    app can render the Media page and still have no way for a *user* to make
    the set an upload needs a target for.
    """
    app.backups.navigate_folders()
    if app.is_visible("folder-add-button"):
        return
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="folder-add-button",
        detail=(
            "folders.md § Layout & flow — the create wizard that gives the "
            "upload below a target; tui leads and the other six follow"
        ),
        tracked="docs/goal/ui/folders.md § Implementation status today",
    )


def seed_an_image_under_this_identity(app, tmp_path: Path) -> tuple[str, str]:
    """Create a set, upload a real image into it, and prove it listed and
    painted. Returns ``(set_name, file_name)``.

    ⚠ **The pre-ceremony assertions are the point, not scaffolding.** An upload
    that silently landed nowhere (a fresh actor has no folder, and the gesture
    then reports ``media.error_no_set`` and uploads nothing) would leave an
    empty Media page, and *every* later "the corpus opens" assertion would pass
    by describing an empty corpus. Asserting the item listed **and painted**
    here is what makes its absence afterwards mean something.

    Convention 8: the set is created through the real wizard and the file
    uploaded through the real picker — the corpus under test is a user's.
    """
    # A set for the upload to land in: a fresh actor owns none, and
    # `upload_selected` with no resolvable target uploads nothing.
    require_folder_wizard(app)
    set_name = f"succession-corpus-{uuid.uuid4().hex[:8]}"
    app.backups.create_folder_via_wizard(set_name)

    # A distinct basename: the recorded member path is the picked file's
    # basename, so a unique name records a new member rather than updating
    # one, and makes a later read unambiguous.
    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    file_name = f"inherited-{uuid.uuid4().hex[:8]}.png"
    picked = tmp_path / file_name
    picked.write_bytes(FIXTURE_IMAGE.read_bytes())

    app.media.navigate()
    app.media.upload_file(str(picked))
    assert app.media.wait_for_item_count(1) == 1, (
        "the upload must land BEFORE the ceremony or the journey proves "
        "nothing — an empty corpus passes every read assertion after it. "
        f"items={app.media.item_names()!r} error={app.error_text()!r}"
    )
    assert app.media.item_name(0) == file_name, (
        f"the uploaded file should be the item, got {app.media.item_name(0)!r}"
    )
    painted = app.media.wait_for_painted_thumbnails(1, timeout=CORPUS_READ_S)
    if painted is not None:
        assert painted == 1, (
            "the image's thumbnail must paint BEFORE the ceremony, or a later "
            "paint assertion cannot distinguish a broken read from a thumbnail "
            f"that was never produced; got {painted}. error={app.error_text()!r}"
        )
    return set_name, file_name


def seed_an_image_in_a_bound_set(
    app, tmp_path: Path, *, serve_over_webdav: bool = False
) -> tuple[str, str]:
    """Create a set, bind a real local folder to it, drop a real image into
    the folder, and prove the app's own engine uploaded it: listed and painted.
    Returns ``(set_name, file_name)``.

    The corpus a succession carries across **only** where a sync engine runs
    for the set on the owner's own host: the take-over (``succession-cut.md``
    ruling (11)(d)) re-records what the device holds, and a set no engine runs
    on stays history (residual (j)(6)). So a journey asking the successor to
    read its inherited corpus seeds it here, in the shape the ruling promises
    the corpus survives, rather than through a Media-page upload into a set no
    engine runs on (:func:`seed_an_image_under_this_identity`).

    Same non-vacuity duty as that helper: the item must list and paint before
    the ceremony. Convention 8: the set is made through the wizard and the
    folder bound through the row's binding form; the file is the user's own,
    dropped on disk where the engine watches.

    ``serve_over_webdav`` flips the row's ``folder-webdav-toggle`` before the
    folder is bound, so the set's chunks rest under content keys while its
    thumbnail still seals under the owner's bare ``BackupKey`` (the engine's
    ``maybe_upload_thumbnail``). The caller enables mail first: serving needs
    the MSEK the first mail enable mints.
    """
    require_folder_wizard(app)
    set_name = f"succession-bound-{uuid.uuid4().hex[:8]}"
    app.backups.create_folder_via_wizard(set_name)
    if serve_over_webdav:
        app.backups.find_and_expand_folder(set_name)
        app.backups.toggle_webdav()
        assert not app.has_error(), (
            f"serving {set_name!r} over WebDAV raised: {app.error_text()!r}"
        )
    watch_dir = tmp_path / "bound-corpus"
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
