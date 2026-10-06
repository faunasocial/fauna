"""Minimal test: upload a chunk to the nest, download it back."""

import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request

import pytest

from common import build_node, wait_for_node, CLAIM_CODE
from drivers.port_util import popen_group_kwargs, reap_descendants_of

pytestmark = [pytest.mark.skipif(
    sys.platform != "win32",
    reason="Windows-only",
), pytest.mark.tier_3]


def test_chunk_upload_download():
    nest_bin = build_node()
    tmp = tempfile.mkdtemp(prefix="fauna-chunk-")
    blob_dir = os.path.join(tmp, "blobs")
    os.makedirs(blob_dir)
    port = 13050

    # Write claim code
    with open(os.path.join(tmp, "claim-code"), "w") as f:
        f.write(CLAIM_CODE)

    nest_log = open(os.path.join(tmp, "nest.log"), "w")
    nest_env = os.environ.copy()
    nest_env["RUST_LOG"] = "debug"

    nest = subprocess.Popen(
        [
            nest_bin,
            "--bind", f"127.0.0.1:{port}",
            "--db", os.path.join(tmp, "nest.db"),
            "--blob-dir", blob_dir,
            "--handle-domain", "test.fauna.social",
        ],
        stdout=nest_log, stderr=nest_log, env=nest_env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()` is
    # `{}` there, so without this the nest's only protection is the atexit
    # sweep a killed run never reaches (testing.md § point 9). No-op off Windows.
    reap_descendants_of(nest.pid)

    try:
        wait_for_node(port)

        # Claim admin — gives us a bearer token
        from common import claim_admin
        admin = claim_admin(port, CLAIM_CODE)
        token = admin["token"]

        # Upload a chunk
        chunk_data = b"Hello, this is test chunk data!"
        import hashlib
        # Use blake3 if available, else sha256 for the test
        try:
            import blake3
            chunk_hash = blake3.blake3(chunk_data).hexdigest()
        except ImportError:
            # The nest uses blake3, but let's just POST and see what hash it returns
            pass

        upload_req = urllib.request.Request(
            f"http://127.0.0.1:{port}/api/v1/chunks",
            data=chunk_data,
            headers={
                "Content-Type": "application/octet-stream",
                "Authorization": f"Bearer {token}",
            },
            method="POST",
        )
        try:
            upload_resp = urllib.request.urlopen(upload_req)
            upload_result = json.loads(upload_resp.read())
            chunk_hash = upload_result["hash"]
            print(f"Upload OK: hash={chunk_hash}")
        except urllib.error.HTTPError as e:
            body = e.read().decode(errors="replace")
            nest_log.flush()
            print(f"=== Upload failed: {e.code} {e.reason} ===")
            print(f"Body: {body}")
            print(f"=== nest log ===")
            print(open(os.path.join(tmp, "nest.log")).read()[-3000:])
            pytest.fail(f"Chunk upload failed: HTTP {e.code}: {body}")

        # Download the chunk
        download_req = urllib.request.Request(
            f"http://127.0.0.1:{port}/api/v1/chunks/{chunk_hash}",
            headers={"Authorization": f"Bearer {token}"},
            method="GET",
        )
        try:
            download_resp = urllib.request.urlopen(download_req)
            downloaded = download_resp.read()
            print(f"Download OK: {len(downloaded)} bytes")
            assert downloaded == chunk_data, (
                f"Downloaded data doesn't match: {downloaded!r} != {chunk_data!r}"
            )
        except urllib.error.HTTPError as e:
            body = e.read().decode(errors="replace")
            nest_log.flush()
            print(f"=== Download failed: {e.code} {e.reason} ===")
            print(f"Body: {body}")
            print(f"=== nest log ===")
            print(open(os.path.join(tmp, "nest.log")).read()[-3000:])
            pytest.fail(f"Chunk download failed: HTTP {e.code}: {body}")

    finally:
        nest.kill()
        nest.wait()
