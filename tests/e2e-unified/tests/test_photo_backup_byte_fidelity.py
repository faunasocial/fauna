"""tier_3 — the copy the nest keeps is the picture the phone took.

Target state: `docs/goal/behavior/sync-engine-deployments.md` § Apple apps —
convergence design → *Ingress metadata-strip convergence* (`:308`), which owns the
claim that every strip is **lossless** — "metadata is removed from the container
without decoding or re-compressing the media payload, so a file with nothing to
strip returns byte-identical". This file is the witness for
`docs/features/photo-backup.md` outcome 8: *"The copy your nest keeps is the
picture the phone took, not a re-encoded one."*

**This outcome is a fidelity claim, and it was once false.** The section records
what it is guarding: each native app used to hand-roll its own stripper that
*decoded and re-encoded the pixels*, "permanently degrading the backed-up copy —
which is the copy a restore returns — a fidelity bug, not just divergence".
apple's was `ImageMetadataStripper.swift` (ImageIO decode/re-encode, which also
dropped C2PA); it was deleted 2026-07-22 and `PhotoBackupEngine.stripImageMetadata`
now calls the shared lossless `stripMediaMetadata` face. Nothing outside
`libs/fauna-media`'s own unit tests has ever checked that the app still does — and
the difference between the two worlds is invisible in every other assertion this
feature has, because a re-encoded photo still arrives, still lists in Media, and
still has a plausible size.

**The witness is a DIFFERENTIAL, which is what makes it decisive.** Two photos go
into the library with the *same one pixel* and different filenames: one plain, one
carrying an `eXIf` chunk and a `tEXt` comment (two of the four kinds
`fauna_media::process`'s PNG arm removes, `process.rs:519`). The plain one has
nothing to strip, so by the lossless property it is stored exactly as the phone
holds it — it is the **reference**. The assertion is then that the nest's stored
copies of the two are the *same bytes*:

  - **identical `manifest_hash`** — the manifest hash is content-derived and
    path-independent (two uploads of identical content under different names share
    it, which is visible in any `changes.list` reply). Equality therefore *is* a
    byte-identity proof, and it is the nest's own identity record rather than a
    re-derivation by the test. A re-encoder fails it: its output is not the plain
    file's bytes.
  - **`size_bytes` equal to the PLAIN file's length**, and strictly smaller than
    the metadata-bearing one's — so the metadata provably came off, and nothing was
    padded or rewritten on the way. For an `ingestFile` upload `size_bytes` is the
    exact **plaintext** length (`SyncEngine::upload_file` records `data.len()`), not
    a rendered size and not the ciphertext's.

Together those separate all three worlds: a missing stripper stores 157 bytes with
a different hash; a re-encoder stores some third byte string with a third hash;
only the lossless path lands on the plain file's exact bytes.

**Why this rather than downloading the file.** The at-rest copy is sealed, so only
a client can unseal it, and the client path that exposes bytes to a test
(`snapshot-file-download-button`) needs a snapshot captured by raw RPC and
attributed to the ingesting device. That machinery would add three failure modes
of its own to prove something the content address already states exactly. Should a
future slice want the bytes in hand, the pattern is
`test_backups.py::test_snapshot_file_download_button_downloads_sealed_bytes`.

**Identifying THIS test's uploads, given `path` is `None`.** `SyncChange.path` is
documented as the plaintext path "for newer changes" and is `None` in practice —
the path travels sealed (`path_sealed`), so no test can match a change by name.
The changes are picked out by `seq` instead, the per-folder monotonic counter: read
the high-water mark before the photos are added, and everything above it is this
test's. That is also why the fixture's pixel is derived from its tag — with every
photo in the suite being the same red pixel, a content-addressed assertion could
be satisfied by a sibling test's identical bytes.

The shared strip's own guarantees are pinned a layer down, in
`libs/fauna-media/tests/process_test.rs` —
`strip_metadata_removes_exif_and_text_chunks_from_png`,
`strip_metadata_is_lossless_when_there_is_nothing_to_strip` and
`strip_metadata_restores_the_exact_pre_injection_bytes`. **What no Rust test can
say is whether the app still calls it**, which is this file's whole job: a
unit-tested lossless function nothing invokes leaves the outcome false.

**Why this module is iOS-only** — macOS runs the same engine against the
machine-global host Photos library, out of reach of an ordinary launch under
convention 10; the macOS column runs this very function from
`tests/real_session/test_photo_backup_macos.py` (convention 12's macOS arm).
"""

from __future__ import annotations

import pytest

from common.auth import sync_changes_list, user_folders_list
from helpers import budgets
from helpers.photo_backup import (
    EXIF_CANARY,
    a_real_png,
    a_tag,
    await_photo_in_media,
    await_quiet,
    enable_photo_backup,
    granted,
    png_chunks,
    prepare_photos_access,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
]


def _photo_set(port: int, secret_key: str) -> str:
    """The set the ingress resolved to, asked of the NEST.

    Not read off a folder row: that row's text is a rendered label (name plus mode
    plus size), not the identifier `changes.list` takes. The shared resolver
    names it `"Photo Library"` (`folders.md` § Photo backup → *Target set model*).
    """
    folders = user_folders_list(port, secret_key=secret_key).get("folders", [])
    return next(
        (f["name"] for f in folders if f.get("name") == "Photo Library"),
        "",
    )


def _creates_above(port: int, secret_key: str, folder: str, seq: int) -> list[dict]:
    changes = sync_changes_list(
        port, secret_key=secret_key, folder=folder,
    ).get("changes", [])
    return [c for c in changes
            if c.get("seq", 0) > seq and c.get("change_type") == "create"]


@pytest.mark.feature("photo-backup")
def test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Back up one plain photo and one metadata-bearing photo with identical
    pixels, and require the nest to hold the same bytes for both.

    The plain photo is the reference — it has nothing to strip, so it is stored
    exactly as the phone holds it. The metadata-bearing one must come to rest on
    those same bytes: metadata gone, pixels untouched.
    """
    app = logged_in_app
    driver = app.driver
    port = nest_instance["port"]
    secret_key = test_user["signing_key"].encode().hex()

    prepare_photos_access(app)

    # One pixel, two files: `rich` is `plain` plus an eXIf chunk and a tEXt
    # comment, so their IHDR and IDAT are byte-identical by construction and the
    # ONLY difference between them is the metadata under test.
    # `pixel_from=tag` on both is load-bearing: the pixel is tag-derived by default,
    # and the two files need DISTINCT names (so Media can tell them apart) but the
    # SAME picture (so identical stored bytes mean the strip was lossless).
    tag = a_tag()
    plain_tag, rich_tag = f"{tag}plain", f"{tag}rich"
    plain_photo = a_real_png(tmp_path, tag=plain_tag, pixel_from=tag)
    rich_photo = a_real_png(tmp_path, tag=rich_tag, pixel_from=tag, metadata=True)
    plain_bytes = plain_photo.read_bytes()
    rich_bytes = rich_photo.read_bytes()

    # The fixture has to be worth stripping, or the assertion is vacuous — the same
    # guard `libs/fauna-media/tests/process_test.rs` states for its own builders.
    assert EXIF_CANARY in rich_bytes and EXIF_CANARY not in plain_bytes, (
        "the metadata-bearing fixture does not carry the canary (or the plain one "
        "does), so this test would compare two files that differ in nothing"
    )
    # And the two must differ ONLY in metadata, or "same bytes at rest" would be
    # asserting something other than the strip.
    def payload(raw):
        return [(k, v) for k, v in png_chunks(raw)
                if k not in (b"eXIf", b"tEXt", b"iTXt", b"zTXt")]
    assert payload(plain_bytes) == payload(rich_bytes), (
        "the two fixtures differ outside their metadata chunks, so identical "
        "stored bytes would not prove the strip was lossless"
    )
    assert len(rich_bytes) > len(plain_bytes), "the injection added no bytes"

    # Backup on, both photos in the library, both provably on the nest.
    driver.add_photo_to_library(str(plain_photo))
    driver.add_photo_to_library(str(rich_photo))
    enable_photo_backup(app)
    granted(app)

    folder = wait_until(
        lambda: _photo_set(port, secret_key) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the nest holds no photo-library set after enabling backup, so the "
            "resolver never created or adopted one: "
            f"{[f.get('name') for f in user_folders_list(port, secret_key=secret_key).get('folders', [])]!r}"
        ),
    )

    # ⚠ The high-water mark is read AFTER the pass, not before: `enable_photo_backup`
    # starts a pass itself, so there is no quiet moment before the uploads to
    # measure from. Instead both photos are identified by being the newest TWO
    # creates in the set — which is sound because the assertion below is about the
    # relationship BETWEEN them, and `await_photo_in_media` has already proved both
    # arrived.
    await_photo_in_media(app, plain_tag, what="enabling backup (the plain photo)")
    await_photo_in_media(app, rich_tag, what="enabling backup (the rich photo)")
    app.backups.navigate_folders()
    await_quiet(driver)

    ours = wait_until(
        lambda: (lambda cs: cs if len(cs) >= 2 else None)(
            _creates_above(port, secret_key, folder, 0)
        ),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the nest recorded fewer than two creates in "
            f"{folder!r} even though both photos list in Media: "
            f"{_creates_above(port, secret_key, folder, 0)!r}"
        ),
    )
    ours = sorted(ours, key=lambda c: c.get("seq", 0))[-2:]

    # ── The outcome ──
    hashes = {c.get("manifest_hash") for c in ours}
    sizes = {c.get("size_bytes") for c in ours}
    assert len(hashes) == 1, (
        "THE NEST'S COPY OF THE METADATA-BEARING PHOTO IS NOT THE PICTURE THE "
        "PHONE TOOK. Its stored content differs from the plain photo's, which has "
        "the identical single pixel and differs only by an eXIf chunk and a tEXt "
        "comment — so the ingress either left the metadata in, or decoded and "
        "re-compressed the pixels instead of lifting the metadata out of the "
        "container. That is the fidelity bug "
        "`sync-engine-deployments.md` § Ingress metadata-strip convergence records "
        "apple's deleted `ImageMetadataStripper` for, on the copy a restore "
        f"returns. Manifest hashes: {hashes!r}, sizes: {sizes!r} "
        f"(plain fixture {len(plain_bytes)} B, metadata-bearing {len(rich_bytes)} B)"
    )
    assert sizes == {len(plain_bytes)}, (
        "the nest's stored size is not the metadata-free original's: it holds "
        f"{sizes!r} where the plain fixture is {len(plain_bytes)} B and the "
        f"metadata-bearing one {len(rich_bytes)} B. Equal to the larger figure "
        "means nothing was stripped; anything else means the file was rewritten "
        "rather than having its metadata lifted out"
    )
