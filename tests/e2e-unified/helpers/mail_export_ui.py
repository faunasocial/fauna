"""Reading what the mail-export wizard saved, and the account's export
sessions — shared by every module that exports through the app and opens the
archive it wrote.

Authority: `docs/goal/behavior/mail-export.md` § Compression wrapper,
§ Container shape — zip inside zstd, § Download flow, § Session row model.
"""

from __future__ import annotations

import os
import shutil
import zipfile


def download_listing(driver) -> list[str]:
    """Everything in the app's download dir right now, sorted.

    Asks the driver for the directory at every call rather than holding a path
    across a wait: android's is a host mirror of the device's directory that
    only `download_dir()` itself refreshes (`drivers/base.py::download_dir`).
    """
    download_dir = driver.download_dir()
    if not download_dir or not os.path.isdir(download_dir):
        return []
    return sorted(os.listdir(download_dir))


def saved_archives(driver) -> list[str]:
    """The export archives sitting in the app's download dir, newest last.

    § Compression wrapper pins the name as
    `fauna-export-<handle>-<format>-<iso-date>.zip.zst`, so the prefix is the
    contract and the rest (which handle, which day) is not this test's business.
    """
    names = [
        n for n in download_listing(driver)
        if n.startswith("fauna-export-") and n.endswith(".zip.zst")
    ]
    return [os.path.join(driver.download_dir(), n) for n in names]


def archive_members(path: str, tmp_path) -> list[tuple[zipfile.ZipInfo, bytes]]:
    """Open a saved `.zip.zst` to its zip members, in archive order, each with
    its bytes.

    § Container shape is a zip whose entries are all `Stored`, wrapped in one
    zstd stream, and ratifies that the **user** runs `zstd -d` once — so this
    does exactly that decompression, then opens the zip. Through the standard
    library's `compression.zstd` (Python 3.14, the version `.python-version`
    pins) rather than the `zstd` CLI, which not every dev box carries (macOS has
    none) — a skip there left the journey's one content assertion unasserted.
    """
    from compression import zstd

    unzstd = os.path.join(str(tmp_path), f"{os.path.basename(path)}.zip")
    with zstd.open(path, "rb") as src, open(unzstd, "wb") as dst:
        shutil.copyfileobj(src, dst)
    with zipfile.ZipFile(unzstd) as zf:
        return [(info, zf.read(info)) for info in zf.infolist()]


def archive_entries(path: str, tmp_path) -> dict[str, bytes]:
    """Open a saved `.zip.zst` to `{entry name: bytes}` (`archive_members`)."""
    return {info.filename: data for info, data in archive_members(path, tmp_path)}


def list_sessions(nest_instance, signing_key) -> list[dict]:
    """The caller's own export sessions, as the nest projects them
    (`fauna.bridges.list_export_sessions`, the same User-class kind every app's
    wizard lists with). Read-only: an observation, not a step of the journey."""
    from common.auth import _authed_call

    reply = _authed_call(
        nest_instance["url"], signing_key, "fauna.bridges.list_export_sessions", {}
    )
    return list(reply.get("sessions") or [])


def clear_prior_exports(nest_instance, user) -> None:
    """Dispose of every export the account still holds — a finished or failed
    one is discarded, an unfinished one cancelled — so the wizard opens on its
    Format step: it shows a failed export until it is dismissed
    (`mail-export.md` § A failed export is a record the wizard shows until the
    user moves on). Test isolation for a session-scoped account, not a journey
    step."""
    from common.auth import _authed_call

    for s in list_sessions(nest_instance, user["signing_key"]):
        state = s.get("state")
        if state in ("completed", "errored"):
            kind = "fauna.bridges.discard_export_blob"
        elif state in ("running", "paused"):
            kind = "fauna.bridges.cancel_export_session"
        else:
            continue
        _authed_call(
            nest_instance["url"], user["signing_key"], kind,
            {"session_id": s["session_id"]},
        )
