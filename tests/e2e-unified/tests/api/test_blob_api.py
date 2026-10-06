"""E2E API test: Blob upload and download roundtrip.

Upload binary data to the blob store, download it back, and verify
byte-identical content. Tests with both small and larger payloads.

⚠ All three tests here
failed ``HTTP Error 400: Bad Request`` on upload, and it was the TEST that was
stale. They posted an ``application/octet-stream`` body — the shape that retired
with the **blob strict flip on 2026-07-01**, ratified in
``docs/goal/architecture/encryption-at-rest.md`` § Don't do these ("The legacy
``application/octet-stream`` wire shape retired with the flip: a non-multipart
Content-Type now returns 400") and implemented at
``bins/fauna-nest/src/blob_routes.rs::upload_blob``. The flip shipped with all
six per-app wire-up tracks closed, so the code is the ratified shape and every
real uploader already speaks it; this suite was simply never updated, and
nothing runs it on a merge path (``merge-gate-check.md`` § Accepted gaps item
(6)), so it sat red.

The uploads now go through ``helpers/blob_upload.py`` — the one writer for this
wire shape, extracted in the same change rather than adding a fourth
hand-rolled copy of the boundary assembly.

These payloads are plaintext, so they upload as ``PublicPost`` (the one class
whose verifier expects plaintext bytes and a real MIME); the sealed classes
would reject them for AEAD shape. That is the honest class for what this file
tests — a byte-identical roundtrip through the blob store.
"""

import os

from common import create_actor_and_register
from helpers.blob_upload import download_blob, upload_blob

import pytest

pytestmark = pytest.mark.tier_3

# Plaintext bytes ⇒ the PublicPost verifier, which wants a real type/subtype.
PLAINTEXT_CLASS = "PublicPost"
PLAINTEXT_MIME = "application/octet-stream"


def api_upload_blob(port, token, data):
    """Upload plaintext bytes to the blob store. Returns the hex blob hash."""
    return upload_blob(
        port, token, data, audience_class=PLAINTEXT_CLASS, mime=PLAINTEXT_MIME
    )


def api_download_blob(port, blob_hash):
    """Download a blob by hash. Returns raw bytes."""
    return download_blob(port, blob_hash)


def test_blob_small_roundtrip(two_nodes):
    """Upload a small blob and download it byte-identical.

    Verifies:
    1. Upload returns a 64-char hex hash
    2. Download returns the exact same bytes
    3. Hash is deterministic (same data = same hash)
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    data = b"Hello, this is a test blob with some content!"

    # Upload
    blob_hash = api_upload_blob(port, actor["token"], data)
    assert len(blob_hash) == 64, f"Hash should be 64 hex chars, got {len(blob_hash)}"
    print(f"Uploaded {len(data)} bytes, hash: {blob_hash[:16]}...")

    # Download
    downloaded = api_download_blob(port, blob_hash)
    assert downloaded == data, (
        f"Downloaded {len(downloaded)} bytes != original {len(data)} bytes"
    )
    print("Download matches upload (byte-identical)")

    # Upload same data again — hash should be identical
    blob_hash_2 = api_upload_blob(port, actor["token"], data)
    assert blob_hash_2 == blob_hash, "Same data should produce same hash"
    print("Deterministic hashing confirmed")


def test_blob_binary_roundtrip(two_nodes):
    """Upload random binary data (simulating an image) and verify roundtrip.

    Verifies:
    1. Upload 10KB of random bytes
    2. Download matches exactly
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    # Generate random binary data (simulating an image file)
    data = os.urandom(10 * 1024)  # 10KB

    blob_hash = api_upload_blob(port, actor["token"], data)
    assert len(blob_hash) == 64
    print(f"Uploaded {len(data)} bytes of random data")

    downloaded = api_download_blob(port, blob_hash)
    assert downloaded == data, "Binary roundtrip failed"
    print(f"Downloaded {len(downloaded)} bytes — matches original")


def test_blob_multiple_uploads(two_nodes):
    """Upload multiple distinct blobs, verify each has unique hash and downloads correctly.

    Verifies:
    1. Three different blobs get three different hashes
    2. Each downloads correctly
    3. No cross-contamination
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    blobs = [
        b"First blob content",
        b"Second blob with different data",
        b"Third blob, also unique!",
    ]

    hashes = []
    for i, data in enumerate(blobs):
        h = api_upload_blob(port, actor["token"], data)
        hashes.append(h)
        print(f"Blob {i+1}: {h[:16]}...")

    # All hashes should be unique
    assert len(set(hashes)) == 3, "Each blob should have a unique hash"
    print("All hashes unique")

    # Download each and verify
    for i, (h, expected) in enumerate(zip(hashes, blobs)):
        downloaded = api_download_blob(port, h)
        assert downloaded == expected, f"Blob {i+1} content mismatch"
    print("All 3 blobs verified byte-identical")
