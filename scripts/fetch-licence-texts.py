#!/usr/bin/env python3
"""Fetch the canonical licence texts the notices generator falls back on.

    just licence-texts-fetch

One text per id in deny.toml's `[licenses] allow` list, written to
`supply-chain/licence-texts/<id>.txt` (an `X WITH Y` id: the licence text, a
blank line, the exception text, in `X-WITH-Y.txt`), from the SPDX licence-list
data repository at ONE pinned commit, through the GitHub contents API — a host
the dependency tooling's approved list already holds (dep-bump-report.py
ALLOWED_HOSTS). Each fetched blob is checked against the git blob id the API
reports for it before it is written, and `SOURCE.toml` records the pinned
commit and, per file, the blob ids and the written file's SHA-256, which the
cheap gate re-checks. A text whose id has left the allow list is removed in
the same run, so the directory holds exactly one text per allowed id.

The texts are third-party text: this script writes them and prints only file
names, sizes and checksums; no session opens them (owner:
docs/goal/architecture/shipped-notices.md § How the notices file is produced).

Standard library only. Exit 0 = every text written; 1 = a fetch or check failed.
"""

from __future__ import annotations

import base64
import hashlib
import json
import sys
import tomllib
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DENY = REPO / "deny.toml"
OUT = REPO / "supply-chain" / "licence-texts"

# SPDX licence-list data v3.27.0 (tag `v3.27.0` of spdx/license-list-data).
SPDX_REPO = "spdx/license-list-data"
SPDX_VERSION = "3.27.0"
SPDX_COMMIT = "d46e94e2c78ceede1cfc63cfa0396472d2798d4c"
API = "https://api.github.com"
MAX_TEXT = 256 * 1024


def text_file_name(licence_id: str) -> str:
    return licence_id.replace(" WITH ", "-WITH-") + ".txt"


def git_blob_sha(data: bytes) -> str:
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def fetch(spdx_id: str) -> tuple[bytes, str]:
    url = f"{API}/repos/{SPDX_REPO}/contents/text/{spdx_id}.txt?ref={SPDX_COMMIT}"
    req = urllib.request.Request(url, headers={"User-Agent": "licence-texts-fetch", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        meta = json.loads(r.read(4 * MAX_TEXT))
    if meta.get("encoding") != "base64":
        raise RuntimeError(f"{spdx_id}: unexpected encoding")
    data = base64.b64decode(meta["content"])
    if len(data) >= MAX_TEXT:
        raise RuntimeError(f"{spdx_id}: over 256 KiB")
    if git_blob_sha(data) != meta.get("sha"):
        raise RuntimeError(f"{spdx_id}: blob id mismatch")
    data.decode("utf-8")
    return data, meta["sha"]


def main() -> int:
    allow = tomllib.loads(DENY.read_text(encoding="utf-8"))["licenses"]["allow"]
    OUT.mkdir(parents=True, exist_ok=True)
    record = [
        "# Written by scripts/fetch-licence-texts.py — do not edit by hand.",
        "# The fallback licence texts of the notices generator, fetched from the SPDX",
        "# licence-list data at one pinned commit; the cheap gate re-checks each sha256.",
        "",
        "[spdx]",
        f'repository = "{SPDX_REPO}"',
        f'version = "{SPDX_VERSION}"',
        f'commit = "{SPDX_COMMIT}"',
        "",
    ]
    failed = 0
    written: set[str] = set()
    for licence_id in sorted(allow):
        parts = licence_id.split(" WITH ")
        try:
            got = [fetch(p) for p in parts]
        except Exception as e:  # noqa: BLE001 — reported, counted, exit 1
            print(f"licence-texts-fetch: FAILED {licence_id}: {type(e).__name__}: {e}")
            failed += 1
            continue
        data = b"\n\n".join(d.rstrip(b"\n") for d, _ in got) + b"\n"
        name = text_file_name(licence_id)
        (OUT / name).write_bytes(data)
        written.add(name)
        sha = hashlib.sha256(data).hexdigest()
        blobs = ", ".join(f'"{b}"' for _, b in got)
        record += [f'[text."{name}"]', f'id = "{licence_id}"', f"blobs = [{blobs}]", f'sha256 = "{sha}"', ""]
        print(f"licence-texts-fetch: wrote {name} ({len(data)} bytes, sha256 {sha[:16]})")
    if failed:
        print(f"licence-texts-fetch: FAILED — {failed} of {len(allow)} ids; SOURCE.toml not rewritten")
        return 1
    (OUT / "SOURCE.toml").write_text("\n".join(record), encoding="utf-8")
    for stale in sorted(p for p in OUT.glob("*.txt") if p.name not in written):
        stale.unlink()
        print(f"licence-texts-fetch: removed {stale.name} (its id is no longer in deny.toml's allow list)")
    print(f"licence-texts-fetch: OK — {len(allow)} texts at SPDX {SPDX_VERSION} ({SPDX_COMMIT[:12]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
