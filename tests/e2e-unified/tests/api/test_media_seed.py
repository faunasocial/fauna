"""E2E API test (tier_3): the cross-set media-seeding data path.

Drives, directly against a real ``fauna-nest`` binary (no browser, no FlaUI), the
exact production RPC chain the cross-app ``seeded_media_app`` fixture (and its
``test_all_media_cross_set`` UI test) builds on:

    fauna.sync.register (write-cap device)
      → fauna.folders.create ×2 (owned, mode=sync)
        → fauna.sync.changes.record ×N (create, non-null manifest)
          → fauna.media.list returns the cross-set aggregate

This is the API-vs-UI split-out: a seeded-media
failure could be UI-side (rendering) or API-side (the seed never produced the
rows). This test isolates the API side so a UI red elsewhere is unambiguous, and
guards the seed contract FlaUI-free (the only place the heavy/flaky win UI run can
be skipped while still proving the data path).

``fauna.sync.changes.record`` is the *production* RPC the ``fauna-sync`` daemon
itself uses — not a test backdoor — so this also exercises the real
device-write-capability + set-ownership authz
(``bins/fauna-nest/src/sync_handlers.rs``) and the
``change_type != 'delete' AND manifest_hash IS NOT NULL`` read filter
(``bins/fauna-nest/src/db/sync_storage.rs``). No real chunk/blob upload is needed:
``fauna.media.list`` never dereferences the manifest (mirrors the Rust
``conformance_media_list`` seed).

Post-S9-flip (2026-08-02) the record must carry ``path_sealed`` (the helper
seals through the real ``fauna_core::label_custody::seal_path`` funnel by
default, via ``fauna_ffi.seal_path``; sealless is refused
``fauna.sync.path_seal_required``) and the listing is asserted hash-addressed —
the plaintext ``path`` wire field serves only the empty-string scrub sentinel
(``file-sync.md`` § Sealed names & paths → contract step).
"""

import hashlib

import pytest

from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register
from common.auth import (
    _user_call,
    sync_changes_record,
    sync_register,
    user_create_folder,
)

pytestmark = pytest.mark.tier_3

# Two owned folders, three media items total — the same shape the
# ``seeded_media_app`` fixture seeds.
_PLAN = {
    "media-seed-photos": ["photos/sunset.jpg", "photos/forest.png"],
    "media-seed-clips": ["clips/intro.mp4"],
}


def _manifest(path: str) -> str:
    return hashlib.blake2b(f"manifest:{path}".encode(), digest_size=32).hexdigest()


def _device_for(secret_key: str) -> str:
    """A per-actor 32-byte hex device id (registration is keyed on device_id, not
    actor, and ``nest_instance`` is session-shared — a fixed id would leak its
    registered state across tests). Mirrors the ``seeded_media_app`` derivation."""
    return hashlib.blake2b(b"seed-device:" + secret_key.encode(),
                           digest_size=32).hexdigest()


def _path_hash(path: str) -> str:
    """The canonical hash-addressed path key — ``fauna_core::sync::path_hash``,
    plain BLAKE3 over the normalized (forward-slash) relative path."""
    import blake3

    return blake3.blake3(path.encode()).hexdigest()


def _media_hashes(port: str, url: str, secret_key: str) -> set[str]:
    """The listing keyed hash-addressed: ``{folder}/{path_hash hex}``.

    Post-S9-flip (2026-08-02) the plaintext ``path`` wire field serves the
    empty-string scrub sentinel for an ordinary set — the sealed pair
    (``path_sealed`` + ``path_hash``) is the only label — so a raw-RPC-seeded
    listing is asserted on ``path_hash``, never on rendered plaintext
    (``file-sync.md`` § Sealed names & paths, the ratified degrade). The
    ``folder`` half stays plaintext: the set-name cutover is gated on the
    per-app hash-sender batch and has not flipped."""
    reply = _user_call(port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url)
    items = reply.get("items", [])
    # The flip's read half, pinned on every listing read: no plaintext path
    # rides this wire for an ordinary set.
    leaked = [it["path"] for it in items if it.get("path")]
    assert not leaked, f"plaintext paths on the media wire post-flip: {leaked!r}"
    return {f"{it['folder']}/{bytes(it['path_hash']).hex()}" for it in items}


@pytest.mark.feature("media")
def test_cross_set_media_seed_lists_and_filters(nest_instance):
    """Seeding ≥2 owned sets via ``fauna.sync.changes.record`` makes every item
    appear in the cross-set ``fauna.media.list`` aggregate, scoped to the actor's
    readable sets; a ``delete`` change removes exactly its item."""
    port = nest_instance["port"]
    url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    secret_key = bytes(actor["signing_key"]).hex()
    device_id = _device_for(secret_key)

    # A fresh actor reads no media until it owns a seeded set (the readable-set
    # boundary — no cross-user leak).
    assert _media_hashes(port, url, secret_key) == set(), "fresh actor should see no media"

    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)

    expected: set[str] = set()
    for set_name, paths in _PLAN.items():
        user_create_folder(port, set_name, secret_key=secret_key, base_url=url)
        for p in paths:
            sync_changes_record(port, secret_key=secret_key, folder=set_name,
                                device_id=device_id, path=p,
                                manifest_hash=_manifest(p), size_bytes=1024,
                                change_type="create", base_url=url)
            expected.add(f"{set_name}/{_path_hash(p)}")

    # The aggregate spans BOTH sets (a single-set view would miss one).
    assert _media_hashes(port, url, secret_key) == expected, (
        "media.list must aggregate every seeded item across both sets"
    )

    # A delete (null manifest) tombstones exactly that path; the rest survive.
    gone = f"media-seed-photos/{_path_hash('photos/forest.png')}"
    sync_changes_record(port, secret_key=secret_key, folder="media-seed-photos",
                        device_id=device_id, path="photos/forest.png",
                        manifest_hash=None, size_bytes=0, change_type="delete",
                        base_url=url)
    assert _media_hashes(port, url, secret_key) == expected - {gone}, (
        "a delete change must remove exactly its item from the media aggregate"
    )


def test_record_rejects_unregistered_device(nest_instance):
    """``fauna.sync.changes.record`` enforces the device write-capability authz —
    recording without registering the device first fails (so the seed helper's
    ``sync_register`` step is load-bearing, not incidental)."""
    port = nest_instance["port"]
    url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    secret_key = bytes(actor["signing_key"]).hex()
    # This actor's device id is never registered (no sync_register call), so the
    # record must be rejected with the device-write-capability authz.
    device_id = _device_for(secret_key)
    user_create_folder(port, "media-seed-photos", secret_key=secret_key, base_url=url)

    with pytest.raises(Exception):
        sync_changes_record(port, secret_key=secret_key, folder="media-seed-photos",
                            device_id=device_id, path="photos/sunset.jpg",
                            manifest_hash=_manifest("photos/sunset.jpg"),
                            size_bytes=1024, change_type="create", base_url=url)


def test_sealless_record_is_refused_path_seal_required(nest_instance):
    """The S9 flip's wire contract (``file-sync.md`` § Sealed names & paths →
    contract step, executed 2026-08-02): a ``fauna.sync.changes.record`` carrying
    no ``path_sealed`` against an ordinary (non-``web``, non-reserved) set is
    refused with the typed code ``fauna.sync.path_seal_required`` — never a
    silent hash-only row. The pytest twin of the Rust handler pin
    (``conformance_path_sealing.rs::a_sealless_record_is_refused_loudly``),
    proving the refusal crosses the real wire, and that the refused record left
    no row behind (the loud-refusal property's observable half)."""
    port = nest_instance["port"]
    url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    secret_key = bytes(actor["signing_key"]).hex()
    device_id = _device_for(secret_key)
    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)
    user_create_folder(port, "media-seed-photos", secret_key=secret_key, base_url=url)

    with pytest.raises(RpcCallError) as exc_info:
        sync_changes_record(port, secret_key=secret_key, folder="media-seed-photos",
                            device_id=device_id, path="photos/sunset.jpg",
                            manifest_hash=_manifest("photos/sunset.jpg"),
                            size_bytes=1024, change_type="create", base_url=url,
                            path_sealed=None)
    assert exc_info.value.code == "fauna.sync.path_seal_required", (
        f"sealless record must refuse with the typed S9 code, got "
        f"{exc_info.value.code!r}"
    )

    # Refused means refused: no hash-only row landed on either read surface.
    assert _media_hashes(port, url, secret_key) == set(), (
        "a refused sealless record must leave no media row behind"
    )

    # The same record WITH a seal is accepted — pinning that the refusal keys on
    # the missing seal, not on anything else about this fixture's shape.
    sync_changes_record(port, secret_key=secret_key, folder="media-seed-photos",
                        device_id=device_id, path="photos/sunset.jpg",
                        manifest_hash=_manifest("photos/sunset.jpg"),
                        size_bytes=1024, change_type="create", base_url=url)
    assert _media_hashes(port, url, secret_key) == {
        f"media-seed-photos/{_path_hash('photos/sunset.jpg')}"
    }
