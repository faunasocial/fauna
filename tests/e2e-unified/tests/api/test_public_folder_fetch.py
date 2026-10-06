"""The public folder read plane on a real ``fauna-nest`` binary — the
publicly-synced follow's nest half (folders re-model phase 4 slice 4f-i).

Owner docs: ``docs/goal/behavior/folders.md`` § Publicly-synced follow
(address / floor / strip / flip-back), ``docs/goal/architecture/federation.md``
§ The public folder read plane (kinds and gates).

What this file adds over the in-process
``bins/fauna-nest/tests/conformance_folder_public_fetch.rs``: the read is driven
by a **second, unrelated actor** over a real WS-RPC connection to a real binary
(tier_3), which is the whole premise of the plane — the reader is neither the
owner nor a roster member, and holds no key. The conformance file proves the
core's logic; this one proves the wire actually carries it to a stranger.

**Why the floor is exercised as public → private → public, rather than by
recording a row while the folder is private.** A record carrying a plaintext
``path`` and no ``path_sealed`` is refused outside a public folder — the S9
flip's ``path_seal_required`` gate, whose plaintext arm *is* the public-audience
exemption (``sync_handlers.rs`` ``rests_plaintext_paths``). So a private-era row
cannot be seeded through the production ingest rail without the client-side
sealing machinery this API-level test has no seat for. The re-flip shape proves
the same structural property through doors production actually opens: each
→public transition stamps a NEW floor at the then-current head, so a row served
during the *previous* public window falls below it. The literal "recorded while
private" case is pinned in-process by the conformance file, whose DAO-level seed
sits underneath that gate.

Rows are recorded through the production ingest rail
(``fauna.sync.changes.record``, the fixture-setup carve-out of E2E rule 8) — the
same shape ``test_web_folder_audience.py`` seeds with.
"""

import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import create_actor_and_register

from tests.api.test_web_paywall_folder import _actor_client, _post_bytes

pytestmark = pytest.mark.tier_3

FOLDER = "public-read-plane"
DEVICE_ID = bytes([0xE7] * 32)


def _record(url: str, port: int, owner: dict, folder: str, path: str, body: bytes) -> int:
    """Record one plaintext file into ``folder``, returning its change ``seq``.

    Only legal while the folder is public — see the module docstring.
    """
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
            "folder": folder,
            "device_id": DEVICE_ID.hex(),
            "path": path,
            "manifest_hash": manifest_hash,
            "size_bytes": len(body),
            "change_type": "create",
        },
    )
    return reply["seq"]


def _set_audience(url: str, owner: dict, folder: str, audience: str) -> None:
    with _actor_client(url, owner) as ws:
        ws.call("fauna.folders.update", {"name": folder, "audience": audience})


def _fetch(ws, owner_id: bytes, folder: str, since: int = 0) -> dict:
    return ws.call(
        "fauna.folders.public.fetch",
        {"owner_actor_id": owner_id.hex(), "folder_name": folder, "since": since},
    )


@pytest.mark.feature("follow-a-public-folder")
def test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold(nest_instance):
    """The whole plane, end to end, as a stranger experiences it."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    owner_id: bytes = bytes(owner["actor_id_bytes"])
    # A completely unrelated account: not the owner, not on any roster, holding
    # no key for this folder. This is the actor the plane exists to serve.
    stranger = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(owner["signing_key"]),
            {"name": FOLDER},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    # ── 1. While PRIVATE, the stranger gets the same answer as for a folder
    # that does not exist. Both are checked and compared — a different code or
    # detail either way would be an existence oracle over private state. ──
    with _actor_client(url, stranger) as ws:
        with pytest.raises(RpcCallError) as private_err:
            _fetch(ws, owner_id, FOLDER)
        with pytest.raises(RpcCallError) as absent_err:
            _fetch(ws, owner_id, "no-such-folder-anywhere")
    refusal = absent_err.value.code
    assert refusal.endswith("not_found"), refusal
    assert private_err.value.code == refusal, (
        "a private folder must be indistinguishable from an absent one: "
        f"{private_err.value.code} vs {refusal}"
    )
    assert private_err.value.details == absent_err.value.details, (
        "…including the detail payload — it is an oracle too"
    )

    # ── 2. The owner declassifies and publishes. The stranger reads it. ──
    _set_audience(url, owner, FOLDER, "public")
    first_seq = _record(url, port, owner, FOLDER, "published.txt", b"first public window")

    with _actor_client(url, stranger) as ws:
        reply = _fetch(ws, owner_id, FOLDER)

    assert [c["seq"] for c in reply["changes"]] == [first_seq]
    assert reply["name"] == FOLDER
    assert reply["folder_id"] > 0, "the stable id a follow pins"
    # The byte-plane SPKI-pin trust root, checked by VALUE rather than for mere
    # presence: this is the identity the follower dials the open by-hash plane
    # under, so a stamp that is merely non-empty proves nothing about where the
    # follower would end up. `fauna.nest.info` is the independent witness — it
    # reports the same `state.nest_identity.public_key_bytes()` the local arm
    # stamps. (The *relay* arm's stamp cannot be checked here at all, because on
    # one deployment the home and relaying identities are the same value; that
    # needs two nests and is pinned by
    # `test_public_folder_follow_cross_nest.py`.)
    with WsRpcAnonClient(url) as anon:
        nest_id = anon.call("fauna.nest.info", {})["nest_id"]
    assert reply.get("home_nest_actor_id") == nest_id, (
        "a same-nest follower pins THIS deployment's identity: expected "
        f"{nest_id}, got {reply.get('home_nest_actor_id')!r}"
    )
    folder_id = reply["folder_id"]

    # ── 3. The stripped projection: the identity/key metadata carries no value
    # to a stranger. ──
    change = reply["changes"][0]
    assert change.get("device_id") is None, "the owner's device fleet"
    assert change.get("author_actor_id") is None, "the authorship map"
    assert change.get("path_sealed") is None, "the sealed label + its salt"
    assert change.get("content_key_version") is None, "the M2 generation"
    # …while what a follower actually needs rides.
    assert change["path"] == "published.txt"
    assert change["manifest_hash"], "the bytes are fetched by hash off the bulk plane"

    # The owner's OWN read of the same row still carries the device id — so the
    # assertions above witness the strip, not a nest that never stamped one.
    with _actor_client(url, owner) as ws:
        owner_view = ws.call("fauna.sync.changes.list", {"folder": FOLDER, "since": 0})
    owner_row = next(c for c in owner_view["changes"] if c["seq"] == first_seq)
    assert owner_row["device_id"] == DEVICE_ID.hex(), (
        "the owner's own plane must still carry the device id, else the strip "
        "assertions above prove nothing"
    )

    # ── 4. Flip-back is revoke, on the very next read. ──
    _set_audience(url, owner, FOLDER, "private")
    with _actor_client(url, stranger) as ws:
        with pytest.raises(RpcCallError) as revoked:
            _fetch(ws, owner_id, FOLDER)
    assert revoked.value.code == refusal, (
        "revoke is indistinguishable from absence — that IS the semantics"
    )
    assert revoked.value.details == absent_err.value.details

    # ── 5. The re-flip resumes the follow AND re-stamps the floor at the
    # then-current head: `first_seq` was served during the previous public
    # window and now falls below the new floor, while the folder id the follower
    # pinned is unchanged. ──
    _set_audience(url, owner, FOLDER, "public")
    second_seq = _record(url, port, owner, FOLDER, "resumed.txt", b"second public window")

    with _actor_client(url, stranger) as ws:
        reply = _fetch(ws, owner_id, FOLDER)
    assert reply["folder_id"] == folder_id, "the pinned id survives the flip cycle"
    assert [c["seq"] for c in reply["changes"]] == [second_seq], (
        "each →public transition stamps a new floor at the then-current head, so "
        f"the first public window's seq {first_seq} falls below it"
    )

    # And the floor is not defeatable by paging from the very beginning.
    with _actor_client(url, stranger) as ws:
        widest = _fetch(ws, owner_id, FOLDER, since=0)
    assert [c["seq"] for c in widest["changes"]] == [second_seq], (
        "the served floor is max(since, public_floor_seq) — since=0 cannot reach below it"
    )


# The folder the zero-follower-state test below publishes. Its own name (and its
# own owner) so it can never be confused with the read-plane folder above on a
# shared session nest.
NO_STATE_FOLDER = "public-follow-costs-nothing"


@pytest.mark.feature("follow-a-public-folder")
def test_the_owners_nest_keeps_no_record_of_who_follows(nest_instance):
    """Zero nest-side follower state: reads leave the owner's side untouched,
    and there is no surface that could list a follower.

    Owner doc: ``docs/goal/behavior/folders.md`` § Publicly-synced follow —
    *"**Zero nest-side follower state.** A follow writes **no row on the home
    nest** — no roster, no registration, no per-follower anything. …
    followers are not enumerable, a follower flood [cannot grow nest state]"*.

    What this adds over the in-process
    ``conformance_folder_public_fetch.rs::a_public_read_writes_nothing``: that
    test reads the DAO directly, which is the right instrument for "the row did
    not change" and is unavailable to anything outside the nest process. This
    one asserts the same absence through **the owner's own client surfaces**,
    which is where the promise is actually kept or broken — and it reads with
    *three distinct stranger accounts* rather than one, because a per-follower
    row is exactly what a single repeated reader cannot distinguish from a
    single upserted one.

    ⚠ **The cheap reading of this outcome is vacuous.** "The nest wrote nothing"
    is trivially true of a nest that also served nothing, so every absence below
    is paired with a positive: each stranger's read is asserted to have actually
    returned the published change first. An assertion about what did not happen
    is worth only as much as the proof that the thing it did not happen during
    did.
    """
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    owner_id: bytes = bytes(owner["actor_id_bytes"])

    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(owner["signing_key"]),
            {"name": NO_STATE_FOLDER},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    _set_audience(url, owner, NO_STATE_FOLDER, "public")
    published = _record(
        url, port, owner, NO_STATE_FOLDER, "readme.txt", b"anyone may read this"
    )

    def _owner_view() -> tuple[dict, list, int]:
        """Everything the owner can see about this folder, in one snapshot."""
        with _actor_client(url, owner) as ws:
            row = next(
                f
                for f in ws.call("fauna.folders.list", {})["folders"]
                if f["name"] == NO_STATE_FOLDER
            )
            changes = ws.call(
                "fauna.sync.changes.list", {"folder": NO_STATE_FOLDER, "since": 0}
            )["changes"]
            used = ws.call("fauna.account.get", {})["quota"]["storage"]["used_bytes"]
        return row, changes, used

    before_row, before_changes, before_used = _owner_view()

    # ── Three unrelated accounts follow, and each reads twice. Three readers,
    # not one: a nest that minted a follower row per reader and a nest that
    # upserted one row for all of them are indistinguishable from a single
    # repeated reader. Six reads, from three identities. ──
    for _ in range(3):
        stranger = create_actor_and_register(port, admin_signing_key=admin_sk)
        with _actor_client(url, stranger) as ws:
            for _attempt in range(2):
                reply = _fetch(ws, owner_id, NO_STATE_FOLDER)
                # The positive half: this read really did serve the folder, so
                # the absences below are absences *during real follows*.
                assert [c["seq"] for c in reply["changes"]] == [published], (
                    "the stranger must actually be served before an assertion "
                    "about what the serving did not write means anything"
                )

    after_row, after_changes, after_used = _owner_view()

    # ── 1. The folder row is exactly as it was. ──
    assert after_row == before_row, (
        "a public read must leave the owner's folder row untouched — no floor "
        f"move, no counter: {before_row} → {after_row}"
    )

    # ── 2. No change rows were minted. ──
    assert after_changes == before_changes, (
        f"a read must not mint change rows: {len(before_changes)} → "
        f"{len(after_changes)}"
    )

    # ── 3. Following costs the owner nothing — the meter did not move. ──
    assert after_used == before_used, (
        f"six follows must cost the owner zero bytes: {before_used} → {after_used}"
    )

    # ── 4. …and followers are not enumerable, because there is no surface that
    # could enumerate them. This is a ratchet as much as an assertion: a future
    # follower roster would land as a kind, and a kind that answers here means
    # the § Publicly-synced follow contract changed and must be re-ratified
    # before this line is relaxed. ──
    with _actor_client(url, owner) as ws:
        for invented in (
            "fauna.folders.followers.list",
            "fauna.folders.public.followers",
        ):
            with pytest.raises(RpcCallError) as absent_surface:
                ws.call(invented, {"name": NO_STATE_FOLDER})
            assert absent_surface.value.code == "fauna.protocol.unknown_kind", (
                f"{invented} answered {absent_surface.value.code} — the public "
                "follow plane is contractually un-enumerable"
            )
