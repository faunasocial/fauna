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
import zlib
from pathlib import Path

import pytest

from actions.media import MediaActions
from helpers import budgets
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
               pixel_from: str | None = None) -> Path:
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
        png += chunk(b"eXIf", EXIF_CANARY)
        png += chunk(b"tEXt", b"Comment\x00" + EXIF_CANARY)
    png += chunk(b"IDAT", idat) + chunk(b"IEND", b"")
    out = path / f"fauna-photo-{tag}.png"
    out.write_bytes(png)
    return out


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
    # ⚠ IDEMPOTENT, and it has to be: the toggle's state PERSISTS across tests and
    # across launches (its `@State` seeds from the `fauna.photoBackupEnabled`
    # UserDefaults key), so a journey that is not the first in the run can find
    # backup already on — and a blind click then turns it OFF, hiding the entire
    # enabled half of the surface. Measured 2026-09-20: four witnesses failed
    # exactly that way, alternating down the run order (file 1 passed, the first
    # test of file 2 failed, its second passed, …) which reads like flakiness and
    # is not. A user who sees the switch already on does not tap it, so neither
    # does this.
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


def a_tag() -> str:
    """A per-run unique token for a fixture filename, so no assertion in this
    family can be satisfied by an earlier run's leftover row."""
    return secrets.token_hex(4)
