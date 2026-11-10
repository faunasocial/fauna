"""A seat's door to a shared folder's files — the one gesture the succession
witnesses vary by app.

The three succession witnesses (take-your-account-back outcome 35,
share-a-folder outcomes 23 and 31) assert the same things on every app: which
key generation a file was sealed under, which seat holds it, which change rows
the nest keeps, which badge a member's row paints. What differs is only how a
seat ADDS a file to the set and how it READS one back:

- **a bound local folder** (every app ``surfaces.toml``'s
  ``bound-location-upload-witness`` lists as built — tui, linux, macos,
  windows): the seat binds a directory under the set; adding is a file written
  into it, the real ``fauna-sync-agent`` uploading it; reading is the file
  materializing there, the seat's engine having opened it;
- **the Media page** (every other app — web, whose browser keeps no bound local
  folder, and ios, which runs no sync agent of its own): adding is
  the Media page's upload into the set (``media-folder-filter`` on the set's
  row, ``file-upload`` + ``upload-button``: ``MediaMachine::upload_selected``
  seals a shared set under its current content key); reading is the Media
  page's download (``media-item-detail-download-button``), whose plaintext is
  the proof of the key — a seat lacking the sealing generation fails closed.

So a witness asks :func:`seat_file_door` for each seat's door and keeps one
copy of its assertions (feature-catalog.md § Cell semantics: a column the goal
doc does not excuse still owes the outcome, through the door it has).
Convention 8: both doors are the user's own gestures in the app.
"""
from __future__ import annotations

from pathlib import Path

from helpers.folder_content import agent_diagnosis
from helpers.folder_content import atomic_write
from helpers.folder_content import await_agent_upload
from helpers.folder_content import bind_location_under_set
from helpers.waiting import wait_until

#: A Media-page upload reaching the uploader's own listing: the seal, the POST,
#: the manifest record and the refresh — or, for a member whose custody has not
#: yet ingested the set's content key, the fail-closed refusal and the retry.
_UPLOAD_LANDED_S = 240.0
#: How long one read waits for the set's ``media-folder-filter`` option; a
#: polling caller retries, so a miss is "not readable yet", never a failure.
_FILTER_OFFER_S = 10.0
#: The detail's download: the version rows, then the fetch and the open.
_DOWNLOAD_S = 60.0


def _read_or_none(path: Path) -> str | None:
    # Bytes, decoded here: text mode would read in the box's locale and fold
    # line endings, so a seat's copy could compare equal to a payload it is not.
    try:
        return path.read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError):
        return None


class BoundFolderDoor:
    """A bound local folder under the set (the native apps that run the real
    sync agent)."""

    def __init__(self, app, set_name: str, folder: Path, *, seat: str) -> None:
        self.app, self.folder, self.seat = app, folder, seat
        folder.mkdir(parents=True, exist_ok=True)
        bind_location_under_set(app, set_name, folder, seat=seat)

    def add(self, name: str, payload: str, *, await_landed: bool = True) -> None:
        """Write ``name`` into the bound folder; with ``await_landed``, wait for
        the agent's own log to report it uploaded."""
        atomic_write(self.folder / name, payload)
        if await_landed:
            await_agent_upload(self.app, name, seat=self.seat)

    def read(self, name: str) -> str | None:
        """``name``'s text in the bound folder — the seat's engine opened it —
        or ``None`` while it is not there."""
        return _read_or_none(self.folder / name)

    def diagnosis(self) -> str:
        return agent_diagnosis(self.app, self.seat)


class MediaPageDoor:
    """The Media page's upload and download, addressed at the set's row
    (``media-folder-filter``: an own set by its name, a set shared with the seat
    by its ``local:<id>`` row — ``media.md`` § Encryption at rest → *The filter
    and the upload target address the row*)."""

    def __init__(self, app, filter_value: str, scratch: Path, *, seat: str) -> None:
        self.app, self.value, self.scratch, self.seat = app, filter_value, scratch, seat
        scratch.mkdir(parents=True, exist_ok=True)
        self.upload_answers: list[str] = []

    def _scoped(self) -> list[str]:
        media = self.app.media
        media.reenter()
        media.ensure_loaded()
        media.set_filter(self.value, offer_timeout=_FILTER_OFFER_S)
        return media.item_names()

    def _upload(self, path: Path) -> bool:
        """One upload gesture; ``False`` when the page answered with an error
        (the member's fail-closed refusal before its custody holds the key, or a
        refused sign-in), its words kept for the diagnosis."""
        media = self.app.media
        try:
            self._scoped()
        except Exception as exc:  # noqa: BLE001 — the option not offered yet; retry
            self.upload_answers.append(f"<filter {self.value!r} not offered: {exc}>")
            return False
        media.upload_file(str(path))
        if media.wait_for_error(timeout=3.0):
            self.upload_answers.append(self.app.error_text())
            return False
        return True

    def add(self, name: str, payload: str, *, await_landed: bool = True) -> None:
        """Upload ``name`` into the set from the Media page; with
        ``await_landed``, wait until the seat's own listing of the set shows it,
        repeating the gesture after a refusal (the user's move)."""
        path = self.scratch / name
        # The payload's own bytes (`atomic_write`'s rule): text mode would
        # upload `\r\n` for each `\n` on Windows.
        path.write_bytes(payload.encode("utf-8"))
        uploaded = self._upload(path)
        if not await_landed:
            return

        def _landed() -> bool:
            nonlocal uploaded
            try:
                if name in self._scoped():
                    return True
            except Exception:  # noqa: BLE001 — the page repainted; next tick
                pass
            if not uploaded:
                uploaded = self._upload(path)
            return False

        wait_until(
            _landed,
            _UPLOAD_LANDED_S,
            interval=3.0,
            diagnose=lambda: (
                f"[{self.seat}] the Media-page upload of {name!r} into "
                f"{self.value!r} never reached the seat's own listing; upload "
                f"answers {self.upload_answers!r}\n" + self.diagnosis()
            ),
        )

    def read(self, name: str) -> str | None:
        """``name``'s plaintext through the Media page's download, or ``None``
        while the set's listing does not show it or the download produces no
        file (a missing key fails closed — no plaintext)."""
        media = self.app.media
        try:
            names = self._scoped()
        except Exception:  # noqa: BLE001 — not offered / repainting: not readable yet
            return None
        if name not in names:
            return None
        try:
            media.open_item_detail(names.index(name))
            media.wait_for_version_count(1)
            return media.download_open_item(timeout=_DOWNLOAD_S).decode()
        except Exception:  # noqa: BLE001 — no plaintext this tick
            return None
        finally:
            try:
                if self.app.driver.is_visible("media-item-detail"):
                    media.close_detail()
            except Exception:  # noqa: BLE001
                pass

    def diagnosis(self) -> str:
        out = [f"  [{self.seat}] error-message={self.app.error_text()!r}"]
        try:
            lines = self.app.driver.console_log()
            out.append(
                f"  [{self.seat}] browser console (last {min(40, len(lines))} of "
                f"{len(lines)}):\n" + "\n".join(f"    {ln}" for ln in lines[-40:])
            )
        except Exception as exc:  # noqa: BLE001 — diagnosis only
            out.append(f"  [{self.seat}] browser console = <unavailable: {exc!r}>")
        return "\n".join(out)


def seat_file_door(
    app, set_name: str, tmp_path: Path, *, seat: str, shared_from: tuple | None = None
):
    """The door ``app``'s seat adds and reads ``set_name``'s files through: a
    bound folder on an app that keeps one, the Media page on web. ``seat`` names
    the seat in every diagnosis. ``shared_from`` is ``(nest, owner_actor)`` for a
    seat the set is SHARED WITH: its Media page addresses the set by the owner's
    row id, read once here off ``fauna.folders.list`` (the row survives the
    owner's recovery — the set stays on the same nest).

    Which door an app has is the ``bound-location-upload-witness`` surface's
    built list — the one registry, never an app name matched here."""
    from helpers.app_surface import app_name, surface_built

    if surface_built("bound-location-upload-witness", app_name(app.driver)):
        return BoundFolderDoor(app, set_name, tmp_path / f"{seat}-bound", seat=seat)
    value = set_name
    if shared_from is not None:
        from tests.api import conv_api

        nest, owner = shared_from
        row_id = conv_api.folder_row_id(nest["port"], owner, set_name)
        assert row_id is not None, (
            f"[{seat}] the owner's set {set_name!r} has no row on the nest to address"
        )
        value = app.media.shared_set_value(row_id)
    return MediaPageDoor(app, value, tmp_path / f"{seat}-uploads", seat=seat)


def stolen_seat(alice_app, request, alice, nest_instance, *, screenshot_tag: str):
    """A second seat of ``alice``'s seed on the app under test — the thief's
    device runs the same app the owner does: a second launch on a native app
    (``_alice_extra_seat``), a twin page in its own ``BrowserContext`` on web
    (``_alice_web_twin_seat``). A generator yielding ``(seat_app, driver)``;
    the caller closes it."""
    from conftest import _alice_extra_seat, _alice_web_twin_seat
    from helpers.app_surface import app_name

    name = app_name(alice_app.driver)
    if name == "web":
        return _alice_web_twin_seat(
            alice_app.driver, request, alice, screenshot_tag=screenshot_tag
        )
    return _alice_extra_seat(
        name, request, alice, nest_instance, screenshot_tag=screenshot_tag
    )


__all__ = ["seat_file_door", "stolen_seat", "BoundFolderDoor", "MediaPageDoor"]

