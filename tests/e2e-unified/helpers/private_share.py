"""Seed a sealed owner-only file (and, for the recipient's side, a private
fragment-keyed share link to it), and read a nest's answers for a link as a
stranger would — the shared setup of the private link's nest-side witnesses
(``tests/api/test_share_links_private_arm.py``) and its two journeys
(``tests/test_share_links_private.py``).

Fixture setup only (e2e convention 5): the file is chunked and sealed under
the owner root by the shared seal path and uploaded through the chunk routes.
For the recipient's journey the link is made by the shared
``ShareClient::create_private_link`` through the FFI harness; the author's
journey records the file as a version (``seed_owner_file``) and makes the link
through the app's own create control.
"""

from __future__ import annotations

import secrets
import ssl
import urllib.error
import urllib.request
from dataclasses import dataclass

import fauna_ffi

from common.auth import sync_changes_record
from tests.api.test_web_paywall_folder import _post_bytes

#: The window every "no plaintext in this answer" check slides.
WINDOW = 48


@dataclass
class PrivateLink:
    content: bytes
    filename: str
    record: dict
    #: The whole link, `<nest>/share/<token>#<key>` — the only place the key is.
    url: str
    chunk_count: int

    @property
    def path_url(self) -> str:
        """The link without its fragment: all a server ever sees."""
        return self.url.split("#", 1)[0]

    @property
    def key(self) -> str:
        return self.url.split("#", 1)[1]


def text_content(lines: int = 4000) -> bytes:
    """A plain-text file every `WINDOW`-byte window of which is unique, so "no
    plaintext window in any answer" is a real check, not a lucky miss."""
    return "".join(f"{i:06d} {secrets.token_hex(20)}\n" for i in range(lines)).encode("ascii")


def seed_private_link(nest: dict, owner: dict, content: bytes, filename: str) -> PrivateLink:
    """Upload ``content`` as an owner-only sealed file of ``owner`` and make a
    private link to it, valid for an hour."""
    secret = bytes(owner["signing_key"])
    manifest_bytes, chunks, _ = _upload_owner_file(nest, owner, content)
    record, url = fauna_ffi.harness_create_private_link(
        nest["url"], secret, manifest_bytes, filename, 3600
    )
    assert record["key_in_fragment"] is True, record
    return PrivateLink(content, filename, record, url, len(chunks))


def seed_owner_file(
    nest: dict, owner: dict, folder: str, device_id: bytes, path: str, content: bytes
) -> int:
    """Upload ``content`` sealed under ``owner``'s root and record it as the
    current version of ``path`` in the owner-only ``folder`` — what a synced
    desktop writes — through ``device_id``, already registered. Returns the
    file's chunk count (the number of ``/chunk/<i>`` arms a link to it has)."""
    _, chunks, manifest_hash = _upload_owner_file(nest, owner, content)
    # The production record seam, which seals the path under the owner root —
    # a private folder's record without one is refused (`path_seal_required`).
    sync_changes_record(
        nest["port"],
        secret_key=owner["signing_key"].encode().hex(),
        folder=folder,
        device_id=device_id.hex(),
        path=path,
        manifest_hash=manifest_hash,
        size_bytes=len(content),
        change_type="create",
        base_url=nest["url"],
    )
    return len(chunks)


def _upload_owner_file(nest: dict, owner: dict, content: bytes):
    """Seal ``content`` under the owner root (the shared seal path) and post
    its chunks, then its manifest. Returns (manifest bytes, chunks, manifest
    hash hex)."""
    secret = bytes(owner["signing_key"])
    manifest_bytes, chunks = fauna_ffi.seal_owner_file(secret, content)
    for store_key, body in chunks:
        assert not leaks(body, content), "a sealed chunk carries plaintext"
        echoed = _post_bytes(nest["port"], "/api/v1/chunks", owner["token"], body, store_key)
        assert echoed == store_key.hex(), (echoed, store_key.hex())
    manifest_hash = _post_bytes(nest["port"], "/api/v1/manifests", owner["token"], manifest_bytes)
    return manifest_bytes, chunks, manifest_hash


def get(url: str, *, navigation: bool = False) -> tuple[int, bytes]:
    """GET ``url`` as a stranger or an unfurler: no identity, no cookie, and
    never a fragment — a browser sends none, and neither does this."""
    assert "#" not in url, url
    headers = {"Accept": "text/html,application/xhtml+xml"} if navigation else {}
    # The docker image serves only TLS, on its self-signed floor cert.
    context = ssl._create_unverified_context() if url.startswith("https://") else None
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=15, context=context) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.read()


def arms(link: PrivateLink) -> list[tuple[str, bool]]:
    """The three things the nest serves for a private link — the viewer page
    (a navigation), the manifest + envelope, and every ciphertext chunk."""
    base = link.path_url
    return [(base, True), (f"{base}/manifest", False)] + [
        (f"{base}/chunk/{i}", False) for i in range(link.chunk_count)
    ]


def leaks(body: bytes, content: bytes) -> bool:
    """Does ``body`` hold any `WINDOW`-byte window of the file's plaintext?"""
    return any(content[i : i + WINDOW] in body for i in range(0, len(content) - WINDOW, 97))
