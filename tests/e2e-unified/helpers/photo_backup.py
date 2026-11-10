"""The preamble, fixtures and pass barriers every apple photo-backup witness shares.

`docs/features/photo-backup.md` has eight outcomes and they are witnessed by
several journeys — the original `test_photo_backup_library_ingest.py` (outcomes
1, 2, 4) plus one file per remaining behaviour — and every one of them needs the
same four things before it can assert anything: a Photos grant that actually
took, a photo in the (simulated) library, backup enabled through the app's own
toggle, and a way to tell one backup pass from the next. Copied per journey those
would drift five ways (priorities #1/#4); here they are one shape.

**Every comment in this module was paid for.** The grant dance in particular is
the residue of a session that spent itself reading a fixture failure as a product
bug — `test_photo_backup_library_ingest.py`'s module docstring is the full
account, and `IosDriver._finish_photos_grant` is the fix.

Authority for the behaviour these helpers set up: `docs/goal/ui/folders.md`
§ Photo backup (apple + android).
"""
from __future__ import annotations

import secrets
import struct
import urllib.error
import urllib.request
import zlib
from pathlib import Path

import pytest

from actions.media import MediaActions
from common.auth import sync_changes_list, user_folders_list
from helpers import budgets
from helpers.set_names import find_set
from helpers.waiting import (
    describe_photo_backup_funnel,
    photo_backup_funnel,
    wait_until,
)

#: The `eXIf` payload every fidelity fixture carries. Its whole job is to be
#: findable: an assertion that metadata "was stripped" is vacuous unless the
#: fixture provably carried some, which is the same guard
#: `libs/fauna-media/tests/process_test.rs` states as "fixture builder must embed
#: the canary; otherwise the strip assertion is vacuous".
EXIF_CANARY = b"canary-exif-data-do-not-leak"


def _png_chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))


def a_real_png(path: Path, *, tag: str, metadata: bool = False,
               pixel_from: str | None = None, exif: bytes | None = None) -> Path:
    """Write a genuine 1x1 PNG. Small, but a real decodable image — PhotoKit
    refuses to import bytes it cannot parse, so a fake payload would fail at
    `simctl addmedia` rather than at anything a test is about.

    Built here rather than committed as a binary fixture so the filename (and so
    the Media row) carries a per-run unique `tag`: an assertion must not be
    satisfiable by some earlier run's leftover.

    With ``metadata=True`` the file additionally carries an ``eXIf`` chunk
    holding :data:`EXIF_CANARY` and a ``tEXt`` comment — two of the four chunk
    kinds `fauna_media::process`'s PNG arm removes (`eXIf`/`tEXt`/`iTXt`/`zTXt`,
    `process.rs:519`). That is what makes a strip observable at all: on
    metadata-free input a lossless strip is a no-op by definition, so a fixture
    with nothing to strip cannot tell a working stripper from a missing one.

    ``exif`` (with ``metadata=True``) replaces the bare canary as the ``eXIf``
    payload — :func:`a_camera_exif` builds the genuine TIFF a camera writes, with
    a location and a camera make/model, for the witness whose outcome names
    exactly those two kinds of metadata.
    """
    def chunk(kind: bytes, data: bytes) -> bytes:
        return _png_chunk(kind, data)

    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)   # 1x1, 8-bit truecolor
    # The one pixel's colour is derived from `pixel_from` (default: `tag`), so the
    # file's CONTENT is unique per run and not just its name. That matters for any
    # assertion read off a content-addressed record (`SyncChange.manifest_hash`):
    # every photo in this family used to be the same red pixel, so every upload in
    # the whole suite shared one manifest hash and an assertion about "the change
    # this test made" could be satisfied by a sibling test's identical bytes.
    #
    # Passing `pixel_from` explicitly is how a caller gets two files that are the
    # SAME PICTURE under different names — which is what a differential fidelity
    # assertion needs, and what deriving the pixel from the (necessarily distinct)
    # `tag` would quietly deny it.
    seed = (pixel_from or tag).ljust(6, "0")
    rgb = bytes(int(seed[i:i + 2], 16) for i in (0, 2, 4))
    idat = zlib.compress(b"\x00" + rgb, 9)                # one filtered pixel
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
    if metadata:
        # Between IHDR and IDAT, which is where a camera writes them.
        png += chunk(b"eXIf", exif if exif is not None else EXIF_CANARY)
        png += chunk(b"tEXt", b"Comment\x00" + EXIF_CANARY)
    png += chunk(b"IDAT", idat) + chunk(b"IEND", b"")
    out = path / f"fauna-photo-{tag}.png"
    out.write_bytes(png)
    return out


#: The EXIF tags :func:`a_camera_exif` writes, by the IFD that holds them
#: (TIFF 6.0 / EXIF 2.32 tag numbers): the camera's make and model in IFD0, the
#: pointer to the GPS IFD, and the GPS IFD's latitude and longitude.
EXIF_MAKE, EXIF_MODEL, EXIF_GPS_IFD = 0x010F, 0x0110, 0x8825
GPS_LATITUDE_REF, GPS_LATITUDE, GPS_LONGITUDE_REF, GPS_LONGITUDE = 1, 2, 3, 4


def a_camera_exif() -> bytes:
    """A genuine little-endian TIFF EXIF block such as a phone camera writes:
    ``Make``/``Model`` in IFD0 (the camera — the model string is
    :data:`EXIF_CANARY`, so the block stays findable) and a GPS IFD with a
    latitude and a longitude (the location).

    A real structure rather than an opaque payload, so the photo the library
    holds is one any EXIF reader would report a location for — the thing
    `docs/features/photo-backup.md` outcome 10 says must not leave the phone.
    :func:`exif_tags` reads it back, which is how a test guards that the
    fixture really carries what it claims.
    """
    ascii_t, long_t, rational_t = 2, 4, 5

    def ifd(entries: list[tuple[int, int, int, bytes]], at: int) -> tuple[bytes, bytes]:
        """``(directory, data area)`` for an IFD written at offset ``at``; a value
        over four bytes goes to the data area right after the directory."""
        data_at = at + 2 + 12 * len(entries) + 4
        directory, data = struct.pack("<H", len(entries)), b""
        for tag, kind, count, value in entries:
            if len(value) <= 4:
                field = value.ljust(4, b"\x00")
            else:
                field = struct.pack("<I", data_at + len(data))
                data += value + (b"\x00" if len(value) % 2 else b"")
            directory += struct.pack("<HHI", tag, kind, count) + field
        return directory + struct.pack("<I", 0), data

    def rationals(*pairs: tuple[int, int]) -> bytes:
        return b"".join(struct.pack("<II", n, d) for n, d in pairs)

    make, model = b"FaunaTestCam\x00", EXIF_CANARY + b"\x00"
    gps_entries = [
        (GPS_LATITUDE_REF, ascii_t, 2, b"N\x00"),
        (GPS_LATITUDE, rational_t, 3, rationals((59, 1), (54, 1), (3021, 100))),
        (GPS_LONGITUDE_REF, ascii_t, 2, b"E\x00"),
        (GPS_LONGITUDE, rational_t, 3, rationals((10, 1), (45, 1), (1784, 100))),
    ]
    # IFD0's size does not depend on where the GPS IFD lands, so lay it out once
    # with a placeholder pointer to learn where its data area ends.
    ifd0_entries = [
        (EXIF_MAKE, ascii_t, len(make), make),
        (EXIF_MODEL, ascii_t, len(model), model),
        (EXIF_GPS_IFD, long_t, 1, struct.pack("<I", 0)),
    ]
    directory, data = ifd(ifd0_entries, 8)
    gps_at = 8 + len(directory) + len(data)
    ifd0_entries[2] = (EXIF_GPS_IFD, long_t, 1, struct.pack("<I", gps_at))
    directory, data = ifd(ifd0_entries, 8)
    gps_directory, gps_data = ifd(gps_entries, gps_at)
    return b"II*\x00" + struct.pack("<I", 8) + directory + data + gps_directory + gps_data


def exif_tags(tiff: bytes) -> dict[str, set[int]]:
    """The tag numbers of a little-endian TIFF EXIF block's IFD0 and GPS IFD —
    the read-back that keeps :func:`a_camera_exif`'s claim honest."""
    assert tiff[:4] == b"II*\x00", f"not a little-endian TIFF: {tiff[:4]!r}"

    def tags_at(at: int) -> dict[int, int]:
        (n,) = struct.unpack_from("<H", tiff, at)
        out = {}
        for i in range(n):
            tag, _kind, _count, value = struct.unpack_from("<HHII", tiff, at + 2 + 12 * i)
            out[tag] = value
        return out

    ifd0 = tags_at(struct.unpack_from("<I", tiff, 4)[0])
    gps = tags_at(ifd0[EXIF_GPS_IFD]) if EXIF_GPS_IFD in ifd0 else {}
    return {"ifd0": set(ifd0), "gps": set(gps)}


def png_chunks(raw: bytes) -> list[tuple[bytes, bytes]]:
    """Split PNG bytes into ``(kind, payload)`` pairs, in file order.

    The read outcome 8 rests on: comparing the ``IDAT`` payload of the stored
    copy against the original's is exactly the question "were the pixels decoded
    and re-compressed?", and it answers it without decoding anything. A whole-file
    comparison would additionally assert that the strip reproduces our
    hand-written chunk *layout* byte for byte, which is a claim about
    `img_parts`' encoder rather than about the product.
    """
    assert raw[:8] == b"\x89PNG\r\n\x1a\n", f"not a PNG: {raw[:8]!r}"
    out: list[tuple[bytes, bytes]] = []
    i = 8
    while i + 8 <= len(raw):
        (length,) = struct.unpack(">I", raw[i:i + 4])
        kind = raw[i + 4:i + 8]
        out.append((kind, raw[i + 8:i + 8 + length]))
        i += 12 + length          # length + kind + payload + crc
    return out


def prepare_photos_access(app) -> None:
    """Pre-grant Photos and cold-relaunch, so the app resolves `.authorized`
    with no prompt at all.

    PhotoKit's authorization prompt is a SpringBoard alert, not a view in our
    app, so no in-process driver can dismiss it (convention 1 — the automation
    server sees the app's own tree). The relaunch must preserve the container, or
    it would take the signed-in session with it.

    Skips — rather than fails — where the driver cannot pin its client store
    across a relaunch: that is a harness limit, and convention 7 wants it
    declared as one instead of dressed up as a product red.
    """
    driver = app.driver
    if not driver.preserve_state_across_relaunch():
        pytest.skip(
            "the ios driver cannot pin its client store across a relaunch, so "
            "the signed-in session cannot survive the launch that carries the "
            "Photos grant"
        )
    driver.grant_photos_access()
    assert driver.recover(), (
        "the cold relaunch that carries the Photos TCC grant did not come back; "
        "without it PhotoKit stays undetermined and the backup surface is inert"
    )


def enable_photo_backup(app) -> None:
    """Turn backup on through the app's own control, exactly as a user would
    (convention 8), and wait for the enabled half of the surface to render.

    ⚠ **The surface rendering proves nothing about PhotoKit.** The status and
    actions sections are gated on `photoBackupEnabled` — the Toggle's own
    `@State`, flipped synchronously by the activate — not on the authorization
    `requestPhotoAccess()` is still resolving. Callers that need the grant
    itself assert :func:`granted` separately.
    """
    driver = app.driver
    app.backups.navigate_folders()
    # `driver.wait_for` returns None and signals by RAISING, so it is called for
    # its effect, never asserted on. The self-diagnosing wait is the `wait_until`
    # over `is_visible` (convention 6).
    wait_until(
        lambda: driver.is_visible("photo-backup-enable-toggle"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            "Settings -> Folders did not render photo-backup-enable-toggle; the "
            "photo-backup surface is unreachable. "
            f"{driver.diagnose('photo-backup-enable-toggle')}"
        ),
    )
    # ⚠ IDEMPOTENT, and it has to be: the toggle's `@State` seeds from the
    # `fauna.photoBackupEnabled` defaults key, which persists for as long as the
    # launch's home does (a relaunch pin, a reused home), so a journey can find
    # backup already on — and a blind click then turns it OFF, hiding the entire
    # enabled half of the surface. Measured 2026-09-20: four witnesses failed
    # exactly that way, alternating down the run order — then because the macOS
    # bare binary's defaults leaked into the real profile across EVERY launch,
    # fixed 2026-11-04 by `AppDefaults.store`'s per-launch suite. A user who sees
    # the switch already on does not tap it, so neither does this.
    if driver.get_attr("photo-backup-enable-toggle", "value") != "on":
        driver.click("photo-backup-enable-toggle")

    assert wait_until(
        lambda: driver.is_visible("photo-backup-sync-now-button"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            "enabling backup never revealed photo-backup-sync-now-button, so "
            "the photo-backup surface did not render its enabled half. The "
            "toggle now reads "
            f"{driver.get_attr('photo-backup-enable-toggle', 'value')!r} — `off` "
            "means either this helper turned it off or `requestPhotoAccess()` "
            "reset it because the Photos grant was refused. "
            f"error={app.error_text()!r}"
        ),
    )


def granted(app) -> bool:
    """Assert the Photos grant actually took, BEFORE blaming the product.

    A pass that ran without the grant reports exactly what a broken ingest
    reports — a completed pass and an empty Media — and that ambiguity cost a
    whole session's diagnosis (`simctl privacy grant photos` exits 0 and writes a
    row iOS 26 then ignores; `IosDriver._finish_photos_grant` is the fix). The
    funnel publishes the live `PHPhotoLibrary.authorizationStatus`, so a harness
    failure can name itself here instead of masquerading as a product bug.
    """
    driver = app.driver
    if driver.is_android():
        # Android's funnel is a subset with no `authorization` leg
        # (`TestAgent.kt`, convention 11: an absent field is not zero); the grant
        # is `pm grant` at launch, which fails loudly there, not a live status.
        return True
    ok = wait_until(
        lambda: ((photo_backup_funnel(driver) or {}).get("authorization")
                 in ("authorized", "limited")),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            "the Photos grant did not take: the pass ran with PhotoKit "
            f"authorization {(photo_backup_funnel(driver) or {}).get('authorization')!r}, "
            "so PHAsset.fetchAssets saw an empty library and the ingest never "
            "had a file to lose. This is a FIXTURE failure, not a product one — "
            "see IosDriver._finish_photos_grant. "
            f"{describe_photo_backup_funnel(driver)}"
        ),
    )
    assert ok
    return ok


def funnel(driver) -> dict:
    """The last pass's funnel, asserted PRESENT (`fauna_e2e_agent::PHOTO_BACKUP_KEY`).

    Refuses loudly where the app publishes none, for the same reason
    `photo_backup_funnel` returns None rather than an empty dict: "no counters"
    and "no passes" are different answers, and conflating them would let every
    barrier in this module pass vacuously (convention 11).
    """
    value = photo_backup_funnel(driver)
    assert value is not None, (
        "the app publishes no `photo_backup` funnel, so a pass cannot be "
        "distinguished from no pass — every barrier in this module reads it"
    )
    return value


def passes(driver) -> tuple[int, int]:
    """``(started, completed)`` — the pass cycle counters off :func:`funnel`."""
    value = funnel(driver)
    started, completed = value.get("passes_started"), value.get("passes_completed")
    assert isinstance(started, int) and isinstance(completed, int), (
        "the funnel carries no pass cycle counters "
        f"(passes_started={started!r}, passes_completed={completed!r}); the app "
        "predates `fauna_e2e_agent::PHOTO_BACKUP_KEY`'s two-counter contract"
    )
    return started, completed


def await_pass_after(driver, baseline_completed: int, *, what: str,
                     timeout: float | None = None) -> None:
    """Block until a pass has COMPLETED beyond `baseline_completed`.

    The two-counter pigeonhole every other app-side cadence uses
    (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`): read the baseline, trigger,
    wait for the counter to pass it. Latency-independent state, never a settle
    sleep (convention 14) — and, unlike a completion *timestamp*, it attributes
    the pass to the trigger, which matters because four different edges call
    `syncNewPhotos()`.
    """
    assert wait_until(
        lambda: passes(driver)[1] > baseline_completed,
        timeout or budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            f"no backup pass completed after {what}: pass counters are "
            f"{passes(driver)} against a completed baseline of "
            f"{baseline_completed}. {describe_photo_backup_funnel(driver)}"
        ),
    )


def await_quiet(driver, *, timeout: float | None = None) -> None:
    """Block until no pass is in flight, via the Sync-now button's own enabled-ness.

    `Sync now` is `.disabled(isSyncing || engine.isBackingUp)`, so "enabled" IS
    the latency-independent statement "no pass is running" (convention 14) — and
    it is the precondition any explicitly-driven pass needs, because clicking a
    disabled control is something no user could do and the bridge refuses it
    outright (409 "element is disabled").
    """
    driver.wait_until_enabled(
        "photo-backup-sync-now-button",
        timeout=timeout or budgets.MAIL_AGENT_WRITEBACK_S,
    )


def media_rows_carrying(driver, tag: str) -> list[str] | None:
    """Media rows whose name carries `tag`, or None while there are none.

    Media pulls `fauna.media.list` on the navigation EDGE only, so a poll that
    stays on the page re-reads the first visit's snapshot for ever
    (`MediaActions.reenter`'s own docstring, and convention 14's freshness
    rider). Re-enter on every attempt.
    """
    media = MediaActions(driver)
    media.reenter()
    return [n for n in media.item_names() if tag in n] or None


def await_photo_in_media(app, tag: str, *, what: str,
                         timeout: float | None = None) -> list[str]:
    """Block until the nest really holds the photo, read through Media.

    Media reads `fauna.media.list`, so this is nest-side truth through the
    surface the outcome names — never a restatement of the app's own counter. A
    pass that reports completion having uploaded nothing is precisely the pre-B3
    failure this feature was fixed to end (`ui/folders.md` § Photo backup's ⚠).
    """
    driver = app.driver
    names = wait_until(
        lambda: media_rows_carrying(driver, tag),
        timeout or budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            f"the photo never appeared in Media after {what}. The app's own pass "
            f"funnel says where it went: {describe_photo_backup_funnel(driver)}. "
            f"Media showed: {MediaActions(driver).item_names()!r}; the app's own "
            f"error was {app.error_text()!r}"
        ),
    )
    assert names, f"expected a Media row carrying {tag!r}"
    return names


#: The set the shared resolver creates or adopts for a device's photo ingress
#: (`fauna_folders_machine::photo_library::PHOTO_LIBRARY_SET_NAME`;
#: `folders.md` § Photo backup → *Target set model*).
PHOTO_LIBRARY_SET = "Photo Library"


def photo_library_set(port: int, secret_key: str) -> str:
    """The photo-library set's name once the nest holds it, else ``""``.

    Found by its ``name_hash``, never by the row's ``name``: the resolver seals
    the set from birth, so its row rests no plaintext name (schema 114 —
    `path-sealing.md` § the set-name plane) and a by-name match finds nothing
    although the set exists. The name returned is what ``sync_changes_list``
    takes; it addresses the set by the same hash.
    """
    rows = user_folders_list(port, secret_key=secret_key).get("folders", [])
    return PHOTO_LIBRARY_SET if find_set(rows, PHOTO_LIBRARY_SET) else ""


#: Above any copy of the one-pixel fixtures and below every stock photo the
#: simulator's library ships (megabytes): how a witness tells its own uploads
#: from the rest of the library a pass backs up with them.
FIXTURE_CEILING_B = 64 * 1024


def fixture_creates(port: int, secret_key: str, folder: str) -> list[dict]:
    """The photo-library set's recorded creates of fixture size, oldest first.

    Picked out by size, not by path or a high-water mark: ``SyncChange.path``
    travels sealed (``path_sealed``), and enabling backup starts a pass at once,
    so there is no quiet moment before the uploads to measure from. A pass backs
    up the WHOLE library, stock photos included, and those are megabytes.
    """
    changes = sync_changes_list(port, secret_key=secret_key, folder=folder).get("changes", [])
    ours = [c for c in changes
            if c.get("change_type") == "create"
            and (c.get("size_bytes") or 0) < FIXTURE_CEILING_B]
    return sorted(ours, key=lambda c: c.get("seq", 0))


def rest_bytes(nest_url: str, route: str, hash_hex: str) -> bytes | None:
    """What the nest serves for ``/api/v1/<route>/<hash>`` — the exact bytes the
    phone uploaded — or ``None`` on a 404 (the nest holds nothing there).

    Fetched with NO credentials over the raw bulk-binary plane, as anyone who
    learned the hash would: what rests on the nest is the question, and a client
    helper opening it with the owner's keys would answer a different one.
    """
    try:
        with urllib.request.urlopen(
            f"{nest_url.rstrip('/')}/api/v1/{route}/{hash_hex}",
            timeout=budgets.RPC_ROUNDTRIP_S,
        ) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def a_tag() -> str:
    """A per-run unique token for a fixture filename, so no assertion in this
    family can be satisfied by an earlier run's leftover row."""
    return secrets.token_hex(4)
