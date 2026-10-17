"""What the nest promises about a share link's lifetime and version, observed
from outside as a stranger holding the link sees it (no app driver).

Owner doc: ``docs/goal/behavior/share-links.md`` § What a link is — a link
stops working when it expires, is revoked, or that version's bytes are gone,
and editing, renaming or moving the file does not change what it serves.
Catalog outcomes ``share-links`` 4 and 11. The fragment-keyed arm's blindness
and its revoke are ``test_share_links_private_arm.py``.

Those two witnesses ride the private (fragment-keyed) link
(``helpers/private_share.py``); the lifetime and version rules are the ones
``share_routes::admit`` and the token's ``manifest_hash`` give every arm.

Outcome 12 follows them on the same arm: a link stops once its version's
bytes are no longer kept, the link itself pinning nothing. Outcomes 14 and 15
come after: which links ``fauna.share.create`` refuses (§ Which files can be
linked), and the 451 a link answers while its
file's bytes are under a legal takedown (``moderation.md`` § Legal takedown →
*The blob-serve door*). Both also mint the ordinary link with no key, through
the shared ``ShareClient::create_link`` (``fauna_ffi.harness_create_link``).
"""

from __future__ import annotations

import secrets

import blake3
import pytest
from clients.ws_rpc_admin_client import RpcCallError

import fauna_ffi

from conftest import _make_user
from helpers import budgets, waiting
from helpers.private_share import (
    arms,
    get,
    link_to_uploaded,
    seed_private_link,
    text_content,
    upload_owner_file,
)
from tests.api import ws_api
from tests.api.test_public_folder_fetch import DEVICE_ID
from tests.api.test_web_paywall_folder import _actor_client, _post_bytes

from common.auth import sync_changes_record

pytestmark = pytest.mark.tier_3

FILENAME = "plan.txt"
#: Short enough to pass inside a test, long enough that the control link made
#: moments before it is nowhere near its own end.
SHORT_LIFETIME_S = 3
LONG_LIFETIME_S = 3600


@pytest.mark.feature("share-links")
def test_a_link_stops_for_everyone_once_its_time_has_passed(nest_instance):
    nest = nest_instance
    owner = _make_user(nest)
    expiring = seed_private_link(nest, owner, text_content(400), FILENAME, SHORT_LIFETIME_S)
    control = seed_private_link(nest, owner, text_content(400), FILENAME, LONG_LIFETIME_S)

    def statuses(link):
        return [get(path, navigation=nav)[0] for path, nav in arms(link) if not nav]

    # Latency-independent (convention 14): poll until the nest has answered
    # "gone" on every part of the expiring link; never assert a wall-clock.
    waiting.wait_until(
        lambda: all(s == 410 for s in statuses(expiring)),
        budgets.RPC_ROUNDTRIP_S,
        interval=0.5,
        diagnose=lambda: f"the expiring link still answers {statuses(expiring)}",
    )
    # The viewer page (a navigation) is gone too, not only the data arms.
    assert get(expiring.path_url, navigation=True)[0] == 410

    # The control, made at the same moment with a long lifetime, still serves:
    # the 410 above is the clock, not the fixture.
    assert set(statuses(control)) == {200}, statuses(control)


@pytest.mark.feature("share-links")
def test_a_link_keeps_opening_the_file_as_it_was_after_it_is_edited_and_moved(nest_instance):
    nest = nest_instance
    url = nest["url"]
    owner = _make_user(nest)
    secret_hex = owner["signing_key"].encode().hex()
    vault = f"share-vault-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(url, bytes(owner["signing_key"]), {"name": vault})
    with _actor_client(url, owner) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    def record(path, manifest_hash, content, change_type):
        sync_changes_record(
            nest["port"],
            secret_key=secret_hex,
            folder=vault,
            device_id=DEVICE_ID.hex(),
            path=path,
            manifest_hash=manifest_hash,
            size_bytes=len(content),
            change_type=change_type,
            base_url=url,
        )

    # Version one is recorded at its path, and a link is made to it.
    v1 = text_content(400)
    v1_manifest, v1_chunks, v1_hash = upload_owner_file(nest, owner, v1)
    record("notes/plan.txt", v1_hash, v1, "create")
    link = link_to_uploaded(nest, owner, v1_manifest, v1_chunks, v1, FILENAME)
    before = {(path, nav): get(path, navigation=nav) for path, nav in arms(link) if not nav}
    assert {status for status, _ in before.values()} == {200}

    # The file is edited (a new version at the same path), then renamed and
    # moved (the old path ends, the new one begins).
    v2 = text_content(400)
    _, _, v2_hash = upload_owner_file(nest, owner, v2)
    record("notes/plan.txt", v2_hash, v2, "update")
    record("notes/plan.txt", None, v2, "delete")
    record("archive/renamed-plan.txt", v2_hash, v2, "create")

    # The link still serves, byte for byte, the version it was made for...
    after = {(path, nav): get(path, navigation=nav) for path, nav in arms(link) if not nav}
    assert after == before, "an edit or a move changed what the link serves"

    # ...and a holder of the link opens exactly that version, not the edit.
    def fetch(path_url: str) -> bytes:
        status, body = get(path_url)
        assert status == 200, (path_url, status)
        return body

    name, got = fauna_ffi.share_viewer_open(link.url, fetch)
    assert name == FILENAME
    assert got == v1
    assert got != v2


@pytest.mark.feature("share-links")
def test_a_link_stops_once_its_version_is_no_longer_kept(nest_instance):
    """Outcome 12 (`share-links.md` § What a link is): a link pins nothing —
    it dies with its version's bytes. Two files are linked: one whose version
    a folder record keeps, and one no record keeps (uploaded, linked, never
    recorded). The admin's collection with no grace window then drops the
    bytes nothing keeps: that link stops on every part, and the kept one still
    serves — so the link alone held nothing on the nest."""
    nest = nest_instance
    url = nest["url"]
    owner = _make_user(nest)
    vault = f"share-vault-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(url, bytes(owner["signing_key"]), {"name": vault})
    with _actor_client(url, owner) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    kept_content = text_content(400)
    kept_manifest, kept_chunks, kept_hash = upload_owner_file(nest, owner, kept_content)
    sync_changes_record(
        nest["port"],
        secret_key=owner["signing_key"].encode().hex(),
        folder=vault,
        device_id=DEVICE_ID.hex(),
        path="notes/kept.txt",
        manifest_hash=kept_hash,
        size_bytes=len(kept_content),
        change_type="create",
        base_url=url,
    )
    kept = link_to_uploaded(nest, owner, kept_manifest, kept_chunks, kept_content, FILENAME)
    dropped = seed_private_link(nest, owner, text_content(400), FILENAME, LONG_LIFETIME_S)

    def statuses(link):
        return [get(path)[0] for path, nav in arms(link) if not nav]

    assert set(statuses(kept)) == {200} and set(statuses(dropped)) == {200}

    admin_sk = nest["admin"]["signing_key"]
    admin = {"actor_id_bytes": bytes(admin_sk.verify_key), "signing_key": admin_sk}
    swept = ws_api._client_for(ws_api._base_url(nest["port"]), admin).call(
        "fauna.admin.gc", {"grace_period_secs": 0, "dry_run": False}
    )

    gone = statuses(dropped)
    assert all(status in (404, 410) for status in gone), (
        f"a link whose version's bytes are gone still answers {gone} ({swept!r})"
    )
    assert set(statuses(kept)) == {200}, "the kept version's link still serves"


def _plaintext_file(nest: dict, owner: dict, content: bytes) -> tuple[bytes, list, str]:
    """Upload ``content`` as a plaintext (public-folder-shaped) file: its
    chunks keyed by their plaintext hash, then its manifest. Returns
    ``(manifest bytes, chunks, manifest hash hex)``."""
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(content, None)
    for store_key, body in chunks:
        echoed = _post_bytes(nest["port"], "/api/v1/chunks", owner["token"], body, store_key)
        assert echoed == store_key.hex(), (echoed, store_key.hex())
    manifest_hash = _post_bytes(nest["port"], "/api/v1/manifests", owner["token"], manifest_bytes)
    return manifest_bytes, chunks, manifest_hash


@pytest.mark.feature("share-links")
def test_a_link_the_nest_could_never_serve_is_refused_when_it_is_made(nest_instance):
    """Outcome 14 (`share-links.md` § Which files can be linked): a link with
    no key to a sealed file, and a link carrying a key to a file that needs
    none, are both refused by ``fauna.share.create`` — so nobody ever holds a
    link that cannot open. Each refusal has its control: the same file made
    linkable the way the nest can serve it registers."""
    nest = nest_instance
    owner = _make_user(nest)
    secret = bytes(owner["signing_key"])

    sealed_manifest, _, sealed_hash = upload_owner_file(nest, owner, text_content(200))
    with pytest.raises(RpcCallError) as no_key:
        fauna_ffi.harness_create_link(
            nest["url"], secret, bytes.fromhex(sealed_hash), FILENAME, LONG_LIFETIME_S
        )
    assert no_key.value.code.endswith("unsupported_encrypted_set"), no_key.value.code
    record, _ = fauna_ffi.harness_create_private_link(
        nest["url"], secret, sealed_manifest, FILENAME, LONG_LIFETIME_S
    )
    assert record["key_in_fragment"] is True, "the sealed file links with its key"

    # A key over a file that needs none. The shared mint never sends one — it
    # refuses a plaintext manifest before any request leaves — so the nest's
    # own gate is reached the way only a non-conforming client reaches it: a
    # well-formed fragment-keyed token naming the plaintext manifest.
    _, _, plain_hash = _plaintext_file(nest, owner, text_content(200))
    with pytest.raises(RpcCallError) as needless_key:
        fauna_ffi.harness_create_private_link(
            nest["url"],
            secret,
            sealed_manifest,
            FILENAME,
            LONG_LIFETIME_S,
            names=bytes.fromhex(plain_hash),
        )
    assert needless_key.value.code.endswith("invalid_request"), needless_key.value.code
    assert "nothing to key" in str(needless_key.value), needless_key.value
    record, url = fauna_ffi.harness_create_link(
        nest["url"], secret, bytes.fromhex(plain_hash), FILENAME, LONG_LIFETIME_S
    )
    assert not record.get("key_in_fragment"), record
    assert get(url)[0] == 200, "the plaintext file links without a key, and the link opens"

    with _actor_client(nest["url"], owner) as ws:
        listed = ws.call("fauna.share.list", {}).get("shares", [])
    assert len(listed) == 2, f"only the two servable links were registered: {listed!r}"


@pytest.mark.feature("share-links")
def test_a_link_to_a_file_removed_under_a_legal_order_answers_unavailable_for_legal_reasons(
    nest_instance,
):
    """Outcome 15 (`moderation.md` § Legal takedown → *The blob-serve door*):
    a link whose file's bytes are under a legal-takedown withhold answers 451.

    The withhold binds a blob every live record naming it is taken down for,
    and the takedown verb acts on a post — so the file's one chunk is also the
    one picture of a post (content-addressed: the same bytes are one blob), and
    no folder record names the file. The link serves before the order, answers
    451 under it, and serves again once the order is overturned."""
    nest = nest_instance
    owner = _make_user(nest)
    secret = bytes(owner["signing_key"])
    admin_sk = nest["admin"]["signing_key"]
    admin = {"actor_id_bytes": bytes(admin_sk.verify_key), "signing_key": admin_sk}

    content = b"\x89PNG\r\n\x1a\n" + secrets.token_bytes(512)
    _, chunks, manifest_hash = _plaintext_file(nest, owner, content)
    assert len(chunks) == 1, "a small file is one chunk"
    assert chunks[0][0] == blake3.blake3(content).digest(), (
        "a plaintext chunk is keyed by its plaintext hash — the post's picture's blob id"
    )
    media = [
        {"blob_cid": fauna_ffi.content_cid(content), "mime": "image/png", "size_bytes": len(content)}
    ]
    post_id = ws_api.create_post(
        nest["port"], owner, fauna_ffi.build_post_with_media(secret, "a picture", media)
    )
    _, url = fauna_ffi.harness_create_link(
        nest["url"], secret, bytes.fromhex(manifest_hash), FILENAME, LONG_LIFETIME_S
    )
    status, body = get(url)
    assert (status, body) == (200, content), (status, len(body))

    taken = ws_api.moderation_legal_takedown(
        nest["port"], admin, post_id, legal_reference="EU-DSA-2024/12345"
    )
    assert taken.get("status") == "taken_down", taken
    status, body = get(url)
    assert status == 451, f"the link answers {status}, not unavailable for legal reasons"
    assert content not in body

    restored = ws_api.moderation_legal_takedown(
        nest["port"], admin, post_id, legal_reference="", restore=True
    )
    assert restored.get("status") == "restored", restored
    assert get(url) == (200, content), "the overturn serves the file again"
