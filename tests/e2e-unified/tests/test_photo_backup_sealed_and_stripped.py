"""tier_3 — a backed-up photo rests on the nest sealed, and its location and
camera metadata never leave the phone.

Target state: `docs/goal/behavior/sync-engine-deployments.md` § Implementation
status today (android byte-sync) — android's photo ingress once uploaded its
chunks in plaintext; it now runs the shared `FfiSyncEngineHost`, whose ingest is
the ordinary sealed chunk pipeline, and its library ingress strips EXIF through
the shared lossless face — and § Apple apps — convergence design → *Ingress
metadata-strip convergence*, which owns the strip being lossless and done on the
device. This file is the witness for `docs/features/photo-backup.md` outcomes 9
(*"Your backed-up photos rest on the nest sealed; the nest never holds a readable
copy"*) and 10 (*"A backed-up photo's location and camera metadata are stripped
before it leaves the phone, without re-encoding the picture"*).

**Both outcomes are about what the NEST holds, so both are read off the nest's
own bulk-binary plane with no credentials** (`helpers.photo_backup.rest_bytes`)
— never asked of a client helper that would open the copy with the owner's keys
and so answer a different question. The nest serves exactly the bytes the phone
uploaded (`chunk_routes::download_chunk` undoes only its own at-rest framing).

Outcome 9 is three observations of one photo's upload, each of which a
plaintext upload fails:

  - **the manifest is the sealed shape** — `stored_hashes` present (the chunks
    are AEAD ciphertext keyed by their ciphertext hash), `sealed_hashes` present
    and the plaintext `file_hash`/`chunk_hashes` blanked (`ChunkManifest::
    check_hash_shape`), so even the plaintext's content address is not on the
    nest;
  - **every chunk the manifest names is ciphertext** — served under its key,
    `blake3(body) == key` (the nest's F9 check holds it to that), and the body
    carries neither the photo's bytes nor its PNG signature;
  - **the photo's plaintext addresses hold nothing** — neither `blake3(photo)`
    (the key a plaintext or framed-plaintext upload rests under) nor
    `blake3(0x00 ‖ photo)` (an unkeyed framed upload) is a chunk on the nest.

Outcome 10 is the differential `test_photo_backup_byte_fidelity.py` pins for
outcome 8, on a fixture whose `eXIf` is a genuine camera EXIF block — a GPS IFD
with a latitude and a longitude, and a camera make and model
(`helpers.photo_backup.a_camera_exif`). Two photos of the same single pixel go
into the library, one plain, one carrying that block. The sealed upload is
deterministic in its plaintext (the plaintext hash is the AEAD salt —
`ChunkManifest.chunk_hashes`), so the two rest under ONE `manifest_hash` exactly
when the phone sent the same bytes for both — and the plain one had nothing to
strip. Identical hashes and a `size_bytes` equal to the plain file's therefore
say the metadata came off ON THE PHONE (the nest only ever received the
metadata-free bytes; it cannot strip a sealed file) and the pixels were not
re-encoded (a re-encoder lands on some third byte string). The row that asked for
this witness spoke of restoring the copy and reading its tags; the content
address states the same thing exactly with none of a restore's own failure
modes, which is the reasoning `test_photo_backup_byte_fidelity.py`'s module
docstring gives at length.

**iOS and android.** Both run the shared engine against a per-device library
(the simulator's, the emulator's MediaStore). macOS runs the same engine against
the machine-global host Photos library, out of reach of an ordinary launch under
convention 10; its column runs these very functions from
`tests/real_session/test_photo_backup_macos.py` (convention 12's macOS arm). Keep every assertion a state read (convention 14): the barriers are
Media listing the photo and the app reporting no pass in flight.
"""

from __future__ import annotations

import blake3
import cbor2
import pytest

from common.auth import sync_changes_list
from helpers import budgets
from helpers.photo_backup import (
    EXIF_CANARY,
    EXIF_GPS_IFD,
    EXIF_MAKE,
    EXIF_MODEL,
    GPS_LATITUDE,
    GPS_LONGITUDE,
    a_camera_exif,
    a_real_png,
    a_tag,
    await_photo_in_media,
    await_quiet,
    enable_photo_backup,
    exif_tags,
    fixture_creates,
    granted,
    photo_library_set,
    png_chunks,
    prepare_photos_access,
    rest_bytes,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
    pytest.mark.android,
]

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _digest(link) -> bytes:
    """The 32-byte BLAKE3 digest a manifest's `ContentHash` field names — the
    tail of its tag-42 CID link."""
    assert isinstance(link, cbor2.CBORTag) and link.tag == 42, (
        f"expected a tag-42 CID link in the manifest, got {link!r}"
    )
    return bytes(link.value)[-32:]


def _backed_up(app, tmp_path, port: int, secret_key: str, photos) -> tuple[str, list[dict]]:
    """Put ``photos`` (``(tag, path)`` pairs) in the library, turn backup on
    through the app, and return the set's name and its fixture-sized creates
    once every photo lists in Media and no pass is in flight."""
    driver = app.driver
    prepare_photos_access(app)
    for _, photo in photos:
        driver.add_photo_to_library(str(photo))
    enable_photo_backup(app)
    granted(app)

    folder = wait_until(
        lambda: photo_library_set(port, secret_key) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the nest holds no photo-library set after enabling backup, so the "
            "resolver never created or adopted one"
        ),
    )
    for tag, _ in photos:
        await_photo_in_media(app, tag, what=f"enabling backup (photo {tag})")
    app.backups.navigate_folders()
    await_quiet(driver)

    creates = wait_until(
        lambda: (lambda cs: cs if len(cs) >= len(photos) else None)(
            fixture_creates(port, secret_key, folder)),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the nest recorded fewer than {len(photos)} fixture-sized creates in "
            f"{folder!r} although every photo lists in Media: "
            f"{sync_changes_list(port, secret_key=secret_key, folder=folder).get('changes')!r}"
        ),
    )
    return folder, creates


@pytest.mark.feature("photo-backup")
def test_a_backed_up_photo_rests_on_the_nest_sealed(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Back up one photo and require everything the nest holds for it to be
    ciphertext, with nothing at the photo's own plaintext address."""
    app = logged_in_app
    port, nest_url = nest_instance["port"], nest_instance["url"]
    secret_key = test_user["signing_key"].encode().hex()

    tag = a_tag()
    photo = a_real_png(tmp_path, tag=tag)
    photo_bytes = photo.read_bytes()

    _, creates = _backed_up(app, tmp_path, port, secret_key, [(tag, photo)])
    # The newest fixture-sized create is this test's: Media listing the photo
    # proved it arrived, and a pass never re-uploads a photo its dedup ledger
    # already holds. Same size is a cross-check, not the selector.
    ours = creates[-1]
    assert ours.get("size_bytes") == len(photo_bytes), (
        f"the newest fixture-sized create records {ours.get('size_bytes')!r} B "
        f"where this test's photo is {len(photo_bytes)} B (it has nothing to "
        f"strip, so a lossless ingress stores it whole): {ours!r}"
    )

    raw_manifest = rest_bytes(nest_url, "manifests", ours["manifest_hash"])
    assert raw_manifest is not None, (
        f"the nest serves no manifest under the create's own manifest_hash: {ours!r}"
    )
    manifest = cbor2.loads(raw_manifest)

    # ── The manifest is the sealed shape. ──
    assert manifest.get("stored_hashes"), (
        "THE PHOTO RESTS ON THE NEST IN PLAINTEXT: its manifest carries no "
        "`stored_hashes`, so its chunks are the photo's own bytes keyed by their "
        "plaintext hash, readable by anyone holding the hash — the bug "
        "`sync-engine-deployments.md` § Implementation status today records "
        f"android's old `uploadChunked` for. Manifest keys: {sorted(manifest)!r}"
    )
    assert manifest.get("sealed_hashes"), (
        "the manifest's plaintext hashes ride unsealed (`sealed_hashes` absent), "
        "so the nest holds the photo's content address — a confirmation oracle for "
        f"anyone guessing the picture. Manifest keys: {sorted(manifest)!r}"
    )
    assert not manifest.get("chunk_hashes") and _digest(manifest["file_hash"]) == bytes(32), (
        "the sealed manifest still names the photo's plaintext hashes beside its "
        f"sealed ones: file_hash={manifest.get('file_hash')!r}, "
        f"chunk_hashes={manifest.get('chunk_hashes')!r}"
    )

    # ── Every chunk it names is ciphertext. ──
    for link in manifest["stored_hashes"]:
        key = _digest(link)
        body = rest_bytes(nest_url, "chunks", key.hex())
        assert body is not None, f"the manifest names chunk {key.hex()} the nest does not hold"
        assert blake3.blake3(body).digest() == key, (
            f"chunk {key.hex()} is not addressed by its own bytes — the nest keyed it "
            "by some other hash, which only a plaintext upload's proof allows"
        )
        assert photo_bytes not in body and PNG_MAGIC not in body, (
            f"chunk {key.hex()} carries the photo readable: the backed-up bytes "
            f"rest unsealed. First bytes: {body[:16]!r}"
        )

    # ── Nothing rests at the photo's plaintext addresses. ──
    for what, address in (
        ("its plaintext hash", blake3.blake3(photo_bytes).digest()),
        ("its framed-plaintext hash", blake3.blake3(b"\x00" + photo_bytes).digest()),
    ):
        held = rest_bytes(nest_url, "chunks", address.hex())
        assert held is None, (
            f"the nest holds a chunk at the photo's {what} ({address.hex()}): a "
            f"readable copy of the photo rests beside the sealed one ({len(held)} B, "
            f"first bytes {held[:16]!r})"
        )


@pytest.mark.feature("photo-backup")
def test_a_photos_location_and_camera_never_leave_the_phone(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Back up a plain photo and the same picture carrying a camera's GPS and
    make/model EXIF, and require the nest to hold the same sealed bytes for both."""
    app = logged_in_app
    port = nest_instance["port"]
    secret_key = test_user["signing_key"].encode().hex()

    tag = a_tag()
    plain_tag, camera_tag = f"{tag}plain", f"{tag}camera"
    exif = a_camera_exif()
    plain = a_real_png(tmp_path, tag=plain_tag, pixel_from=tag)
    camera = a_real_png(tmp_path, tag=camera_tag, pixel_from=tag, metadata=True, exif=exif)
    plain_bytes, camera_bytes = plain.read_bytes(), camera.read_bytes()

    # The fixture must carry what the outcome names, or the assertion is vacuous.
    tags = exif_tags(exif)
    assert {EXIF_MAKE, EXIF_MODEL, EXIF_GPS_IFD} <= tags["ifd0"], tags
    assert {GPS_LATITUDE, GPS_LONGITUDE} <= tags["gps"], tags
    assert dict(png_chunks(camera_bytes)).get(b"eXIf") == exif, (
        "the camera fixture's eXIf chunk is not the EXIF block built for it"
    )
    assert EXIF_CANARY in camera_bytes and EXIF_CANARY not in plain_bytes

    # And the two differ ONLY in metadata, or one stored copy for both would
    # prove something other than the strip.
    def payload(raw):
        return [(k, v) for k, v in png_chunks(raw)
                if k not in (b"eXIf", b"tEXt", b"iTXt", b"zTXt")]
    assert payload(plain_bytes) == payload(camera_bytes), (
        "the two fixtures differ outside their metadata chunks"
    )

    _, creates = _backed_up(
        app, tmp_path, port, secret_key, [(plain_tag, plain), (camera_tag, camera)],
    )
    ours = creates[-2:]

    hashes = {c.get("manifest_hash") for c in ours}
    sizes = {c.get("size_bytes") for c in ours}
    assert len(hashes) == 1, (
        "THE PHOTO'S LOCATION OR CAMERA METADATA LEFT THE PHONE, OR ITS PIXELS "
        "WERE RE-ENCODED. The nest's copy of the photo carrying a GPS position and "
        "a camera make/model is not the same upload as the plain photo of the "
        "identical pixel — so the phone sent bytes other than the metadata-free "
        "original: either the EXIF went up with it (sealed, but on the nest all the "
        "same, and readable by every device the owner unseals on) or the ingress "
        "decoded and re-compressed the picture instead of lifting the metadata "
        "out (`sync-engine-deployments.md` § Ingress metadata-strip convergence). "
        f"Manifest hashes: {hashes!r}, sizes: {sizes!r} (plain {len(plain_bytes)} B, "
        f"with camera EXIF {len(camera_bytes)} B)"
    )
    assert sizes == {len(plain_bytes)}, (
        f"the nest records {sizes!r} B for both where the metadata-free original "
        f"is {len(plain_bytes)} B and the camera photo {len(camera_bytes)} B — "
        "equal to the larger means nothing was stripped; anything else means the "
        "file was rewritten rather than having its metadata lifted out"
    )
