"""tier_3, TWO real nests: the publicly-synced follow **across the federation
relay**, bytes included — folders re-model phase 4 slice 4f-iv.

Owner docs: ``docs/goal/behavior/folders.md`` § Publicly-synced follow (address /
floor / strip / flip-back), ``docs/goal/architecture/federation.md`` § The public
folder read plane (the relay, the ``home_nest_actor_id`` stamp, bytes-off-channel).

What this adds over everything already green
--------------------------------------------
The plane has three proofs under it, and all three stop at the same line:

* ``bins/fauna-nest/tests/conformance_folder_public_fetch.rs`` — the read core and
  the federation *serving* handler, in process, on one deployment.
* ``tests/api/test_public_folder_fetch.py`` — the client kind over a real binary,
  driven by a stranger. **Same nest**: the request carries no ``nest_url``, so it
  takes the local arm and the relay is never entered.
* the 4f-ii unit tests — the follower's client ops against a *fake* nest.

So the arm production actually takes for a follow — **the follower's own nest
relaying to the folder's home nest** — has never run. This module runs it, on two
real ``fauna-nest`` binaries, and then follows the reply all the way to the file's
bytes.

The two seats (convention 16: two seats, ONE machine, one pytest process, no
operator round)
----------------------------------------------------------------------------
* **Owner** on nest **H** (``second_nest``) — creates the folder, declassifies it,
  publishes a file.
* **Follower** on nest **F** (``nest_instance``) — a second actor with no account,
  roster row, grant or key anywhere on H. Every read it makes goes to **its own**
  nest, which relays.

Four things only this topology can witness
------------------------------------------
1. **The relay serves at all.** ``fauna.folders.public.fetch{nest_url}`` →
   ``federation_pool::originate_folder_public_fetch`` →
   ``fauna.federation.folder.public.fetch`` on H, between two nests that have never
   been paired.
2. **The trust root is H's, not F's.** ``home_nest_actor_id`` is the follower's
   byte-plane SPKI pin (``federation.md``: *"``home_nest_actor_id`` stamp for the
   byte-plane SPKI pin"*). A relay that stamped its **own** identity — the easy
   mistake, since the local arm stamps ``state.nest_identity`` — would point every
   follower's byte dial at the wrong deployment, and no single-nest test can tell
   the two apart because there the two identities are the same value. Here they are
   different values, checked against each nest's own ``fauna.nest.info``.
3. **The strip survives the hop.** The stripped projection is applied on H and then
   *re-encoded* by F's relay arm into a fresh ``FoldersPublicFetchReply``; a field
   re-populated (or defaulted) in that re-encode would leak the owner's device
   fleet to the world with every single-nest test still green.
4. **The bytes actually move.** The follower resolves the served ``manifest_hash``
   on H's open by-hash bulk plane — *no bearer, no key, no account on H* — and
   reassembles the file. That is the whole point of a public folder, and it is the
   one leg that proves the plane hands out something a follower can use rather
   than metadata that merely looks right.

Byte plane, deliberately unauthenticated
----------------------------------------
``GET /api/v1/{manifests,chunks}/{hash}`` are mounted outside the auth layer
(``lib.rs``'s ``public_bytes`` router) — possession of the content hash *is* the
capability. The follower therefore fetches with no ``Authorization`` header at
all, and a 401/403 here would be a real regression of the ratified design
(``federation.md`` § The public folder read plane: *"manifests/chunks GET by hash
on the open bulk plane (plaintext for a public folder)"*).

Rows are seeded through the production ingest rail (``fauna.sync.changes.record``,
the fixture-setup carve-out of E2E rule 8) — the same shape
``test_public_folder_fetch.py`` and ``test_web_folder_audience.py`` seed with, and
for the same reason a private-era row cannot be seeded through it (that reasoning
lives in ``test_public_folder_fetch.py``'s docstring; it is not re-derived here).

Convention 14: every assertion here is about latency-independent state. The relay
is request/response — the reply either carries the rows or it does not — so there
is nothing to wait on and no sleep anywhere in this module.
"""

import urllib.error
import urllib.request

import cbor2
import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import create_actor_and_register
from common.envelope import cid_from_link

from tests.api.test_web_paywall_folder import _actor_client, _post_bytes

pytestmark = pytest.mark.tier_3

FOLDER = "xnest-follow-plane"
DEVICE_ID = bytes([0xF3] * 32)
FILE_PATH = "notes/published.txt"
# Big enough to chunk into more than one blob on any reasonable chunker setting,
# so the reassembly below is an ordered walk rather than a single-chunk identity.
CONTENT = b"the follower reads this across a federation relay.\n" * 4096


# ── helpers ────────────────────────────────────────────────────────────────


def _nest_id(url: str) -> str:
    """The deployment identity ``fauna.nest.info`` reports — the same value the
    public-fetch reply stamps as ``home_nest_actor_id``
    (``discovery_core::nest_info_core`` and ``folder_handlers``'s local arm both
    read ``state.nest_identity.public_key_bytes()``)."""
    with WsRpcAnonClient(url) as anon:
        return anon.call("fauna.nest.info", {})["nest_id"]


def _publish(url: str, port: int, owner: dict, path: str, body: bytes) -> tuple[int, str]:
    """Chunk + upload + record one plaintext file into the public folder on H,
    returning ``(seq, manifest_hash_hex)``."""
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(body, None)
    for store_key, chunk in chunks:
        assert (
            _post_bytes(port, "/api/v1/chunks", owner["token"], chunk, store_key)
            == store_key.hex()
        )
    manifest_hash = _post_bytes(port, "/api/v1/manifests", owner["token"], manifest_bytes)
    reply = fauna_ffi.harness_record_change(
        url, bytes(owner["signing_key"]),
        {
            "folder": FOLDER,
            "device_id": DEVICE_ID.hex(),
            "path": path,
            "manifest_hash": manifest_hash,
            "size_bytes": len(body),
            "change_type": "create",
        },
    )
    return reply["seq"], manifest_hash


def _set_audience(url: str, owner: dict, audience: str) -> None:
    with _actor_client(url, owner) as ws:
        ws.call("fauna.folders.update", {"name": FOLDER, "audience": audience})


def _fetch(ws, *, home_peer_url: str | None, **address) -> dict:
    """One follower read. ``home_peer_url`` present ⇒ the relay arm.

    The parameter is the PEER authority — what the follower's nest is asked to
    dial — so it is fed from the handle's ``peer_url`` contract key, never from
    ``url``.  The two are the same string wherever no network boundary
    intervenes and differ in docker mode, where ``url`` is a published
    ``127.0.0.1`` port and only ``peer_url`` names an authority the other
    container can reach.  Reading the key is also what marks this test class (8)
    (``testing.md`` § Default app and nest mode, ruling (2)).
    """
    req = {"since": 0, **address}
    if home_peer_url is not None:
        req["nest_url"] = home_peer_url
    return ws.call("fauna.folders.public.fetch", req)


def _get_open_bytes(home_url: str, path: str) -> bytes:
    """GET on H's open by-hash bulk plane — **no Authorization header**, because a
    follower holds no credential on the home nest."""
    req = urllib.request.Request(home_url.rstrip("/") + path, method="GET")
    with urllib.request.urlopen(req) as resp:
        return resp.read()


def _download_followed_file(home_url: str, manifest_hash_hex: str) -> bytes:
    """The follower's byte read, by hand: manifest by hash → ordered chunks by
    hash → each chunk unframed (the shared strict unframe, through
    ``fauna_ffi.unframe_chunk``) → the file.

    This is what ``fauna_client_folders::public_follow::download_followed_file``
    does in production (``FileDownloadKeys::default`` — no owner key, no content
    keys); doing it here in the open proves the served manifest is genuinely
    keyless-openable rather than asserting that a Rust helper said so.
    """
    manifest = cbor2.loads(_get_open_bytes(home_url, f"/api/v1/manifests/{manifest_hash_hex}"))
    assert manifest.get("stored_hashes") is None, (
        "a public folder's content rests PLAINTEXT (principles.md's public "
        "exception); `stored_hashes` present would mean AEAD ciphertext no "
        "follower can open"
    )
    assert manifest.get("sealed_hashes") is None, (
        "sealed manifest hashes would put the chunk walk behind the set's chunk "
        "root — a key a follower does not hold"
    )
    out = bytearray()
    for cid in manifest["chunk_hashes"]:
        # `ContentHash` is a tag-42 link to a 36-byte raw-codec Cid; the store
        # key is its 32-byte digest (`Cid::digest` == self.0[4..36]).
        body = _get_open_bytes(home_url, f"/api/v1/chunks/{cid_from_link(cid)[4:36].hex()}")
        out += fauna_ffi.unframe_chunk(body)
    return bytes(out)


# ── the journey ────────────────────────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
def test_a_follower_on_another_nest_reads_the_relay_and_fetches_the_bytes(
    nest_instance, second_nest
):
    """Owner declassifies on H; a stranger on F follows, browses and reads bytes;
    flip-back revokes through the relay; a re-flip resumes under the pinned id."""
    home_url, home_port = second_nest["url"], second_nest["port"]
    # The relay arm addresses H as a PEER (see `_fetch`): `url` is what this
    # process dials, `peer_url` is what the follower NEST is asked to dial.
    home_peer_url = second_nest["peer_url"]
    follower_url, follower_port = nest_instance["url"], nest_instance["port"]

    home_id, follower_id = _nest_id(home_url), _nest_id(follower_url)
    assert home_id != follower_id, (
        "the two fixtures must be genuinely distinct deployments, else assertion "
        "2 below (the relay stamps H's identity, not its own) proves nothing"
    )

    owner = create_actor_and_register(
        home_port, admin_signing_key=second_nest["admin"]["signing_key"]
    )
    owner_hex: str = bytes(owner["actor_id_bytes"]).hex()
    # No account, roster row, grant or key on H — the follower's ONLY relationship
    # to the home nest is that its own nest can reach it.
    follower = create_actor_and_register(
        follower_port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )

    with _actor_client(home_url, owner) as ws:
        fauna_ffi.harness_create_set(
            home_url, bytes(owner["signing_key"]),
            {"name": FOLDER},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    # ── 1. While PRIVATE, the relay answers exactly what absence answers — and
    # the fold must survive the hop, since a relay that let the two diverge would
    # rebuild the existence oracle the fold exists to prevent. ──
    with _actor_client(follower_url, follower) as ws:
        with pytest.raises(RpcCallError) as private_err:
            _fetch(ws, home_peer_url=home_peer_url, owner_actor_id=owner_hex, folder_name=FOLDER)
        with pytest.raises(RpcCallError) as absent_err:
            _fetch(
                ws,
                home_peer_url=home_peer_url,
                owner_actor_id=owner_hex,
                folder_name="no-such-folder-anywhere",
            )
    refusal, refusal_details = absent_err.value.code, absent_err.value.details
    assert refusal.endswith("not_found"), refusal
    assert private_err.value.code == refusal, (
        "across the relay too, a private folder must be indistinguishable from an "
        f"absent one: {private_err.value.code} vs {refusal}"
    )
    assert private_err.value.details == refusal_details

    # ── 2. The owner declassifies and publishes a real file. ──
    _set_audience(home_url, owner, "public")
    first_seq, manifest_hash = _publish(home_url, home_port, owner, FILE_PATH, CONTENT)

    with _actor_client(follower_url, follower) as ws:
        relayed = _fetch(ws, home_peer_url=home_peer_url, owner_actor_id=owner_hex, folder_name=FOLDER)

    assert [c["seq"] for c in relayed["changes"]] == [first_seq], (
        "the relay must carry the floor-filtered rows through: "
        f"{relayed['changes']!r}"
    )
    assert relayed["name"] == FOLDER
    folder_id = relayed["folder_id"]
    assert folder_id > 0, "the stable id the follow pins"

    # ── 3. The trust root is the HOME nest's identity, never the relaying one. ──
    assert relayed["home_nest_actor_id"] == home_id, (
        "the follower's byte-plane SPKI pin must be the home nest's deployment "
        f"identity ({home_id}), got {relayed['home_nest_actor_id']}"
    )
    assert relayed["home_nest_actor_id"] != follower_id, (
        "a relay that stamped its OWN identity would aim every follower's byte "
        "dial at the wrong deployment"
    )

    # ── 4. The strip survives the relay's re-encode. ──
    change = relayed["changes"][0]
    for field, what in (
        ("device_id", "the owner's device fleet"),
        ("author_actor_id", "the authorship map"),
        ("path_sealed", "the sealed label + its salt"),
        ("content_key_version", "the M2 generation"),
    ):
        assert change.get(field) is None, (
            f"{field} ({what}) must not survive the relay's re-encode"
        )
    assert change["path"] == FILE_PATH, "what a follower needs still rides"
    assert change["manifest_hash"] == manifest_hash

    # The owner's own read on H still carries the device id, so the four
    # assertions above witness the strip rather than a nest that never stamped one.
    with _actor_client(home_url, owner) as ws:
        owner_view = ws.call("fauna.sync.changes.list", {"folder": FOLDER, "since": 0})
    owner_row = next(c for c in owner_view["changes"] if c["seq"] == first_seq)
    assert owner_row["device_id"] == DEVICE_ID.hex(), (
        "the owner's own plane must still carry the device id, else the strip "
        "assertions above prove nothing"
    )

    # ── 5. Without `nest_url` the follower's own nest holds nothing — so it is
    # genuinely the relay, not a local row, that served step 2. ──
    with _actor_client(follower_url, follower) as ws:
        with pytest.raises(RpcCallError) as local_err:
            _fetch(ws, home_peer_url=None, owner_actor_id=owner_hex, folder_name=FOLDER)
    assert local_err.value.code == refusal, (
        "the folder is homed on H; F's local arm must know nothing about it"
    )

    # ── 6. The bytes. The follower fetches them off H's open by-hash plane with
    # no credential of any kind, and gets the file back byte-exact. ──
    assert _download_followed_file(home_url, manifest_hash) == CONTENT

    # ── 7. Every read after the first addresses the PINNED id, and the relay
    # resolves it the same way. ──
    with _actor_client(follower_url, follower) as ws:
        pinned = _fetch(ws, home_peer_url=home_peer_url, folder_id=folder_id)
    assert pinned["folder_id"] == folder_id
    assert [c["seq"] for c in pinned["changes"]] == [first_seq]

    # ── 8. Flip-back is revoke, through the relay, on the very next read — and
    # it is still indistinguishable from absence. ──
    _set_audience(home_url, owner, "private")
    with _actor_client(follower_url, follower) as ws:
        with pytest.raises(RpcCallError) as revoked:
            _fetch(ws, home_peer_url=home_peer_url, folder_id=folder_id)
    assert revoked.value.code == refusal, (
        "revoke reaches the follower as the same not_found — that IS the semantics"
    )
    assert revoked.value.details == refusal_details

    # ── 9. A re-flip resumes the follow under the SAME pinned id, and the floor
    # re-stamp holds across the relay: the first public window's row is gone. ──
    _set_audience(home_url, owner, "public")
    second_seq, _ = _publish(home_url, home_port, owner, "notes/resumed.txt", b"second window")

    with _actor_client(follower_url, follower) as ws:
        resumed = _fetch(ws, home_peer_url=home_peer_url, folder_id=folder_id)
    assert resumed["folder_id"] == folder_id, "the pinned id survives the flip cycle"
    assert [c["seq"] for c in resumed["changes"]] == [second_seq], (
        "each →public transition re-stamps the floor at the then-current head, so "
        f"the first window's seq {first_seq} falls below it — across the relay too"
    )
