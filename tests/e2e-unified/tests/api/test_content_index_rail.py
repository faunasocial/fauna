"""tier_3 — the `__index` rail through the real wire: the nest **holds** a
user's private content index, **relays** it to their other devices, and
**cannot read it** — across a restart.

Goal doc: `docs/goal/behavior/content-index.md` § Encryption posture — what's
plaintext where: "every segment is AEAD-sealed under a per-segment data key …;
the nest stores and relays opaque blobs and **can never read or query the
index**." The restart half is § Don't do these → *Don't let the boot-time
`__index` purge survive Plan 5b*.

**What this adds over the in-process twin.** `bins/fauna-nest/tests/
index_survives_nest_restart.rs` drives `run_serve_loop` over a data dir it
staged by calling `record_sync_change` directly — it proves the *boot* keeps
`__index` content, which is the half that was once broken. It cannot reach the
wire: nothing there goes through `fauna.index.record`'s upload-before-record
gate, `fauna.index.list`'s reply, the blob route, or a second device. This
does, on a real binary over a real socket.

**The opacity assertion is the one that can actually go red.** The nest-side
index writer (`IndexRegistry::ingest_mail`) was LIVE until 2026-07-12, writing
*unsealed* segments under `__index` — the exact contradiction of the section
above. So the bytes recorded here carry a distinctive plaintext marker, and the
test asserts the nest never surfaces it: not from its own search backend, not
anywhere in the rail's reply. A nest that re-grew any index-reading path would
have to either choke on these bytes (they are not a valid index) or leak the
marker.

Process safety: a dedicated nest per test, stopped and re-spawned only through
its own `proc` handle (`common.nest.stop_nest` / `start_nest_in_place`) — never
`pkill`/`killall`.
"""

from __future__ import annotations

import os
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from common.nest import start_nest_in_place, stop_nest
from fauna_ffi import content_cid

pytestmark = pytest.mark.tier_3

# Carried inside the recorded bytes. A nest that could read — or index, or
# query — the segment is the only way this string reaches a nest-side surface.
_INDEX_PLAINTEXT_MARKER = "TOPSECRET-private-index-plaintext-marker-6194"

# Two real `fauna_index::paths` strings: one master-class segment and the
# master-class manifest. The nest re-derives the shape rather than trusting the
# writer (`content_index_handlers.rs::validate_index_path`), so these are not
# arbitrary names.
_SEGMENT_PATH = "__index/contact/seg-00000001.idx"
_MANIFEST_PATH = "__index/manifest.idx"


@pytest.fixture()
def index_nest(request, nest_mode, tmp_path_factory):
    """A nest exclusive to one test — it gets stopped and re-spawned in place."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "index-rail-nest"
    )
    yield nest
    cleanup()


def _ws(nest: dict, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _put_blob(nest_url: str, token: str, data: bytes) -> str:
    """`PUT /api/v1/blob/{cid_b32}` — the byte half of the rail. Returns the hex
    blake3 hash `fauna.index.record` references the bytes by."""
    import blake3

    cid = content_cid(data)
    req = urllib.request.Request(
        f"{nest_url}/api/v1/blob/{cid}",
        data=data,
        method="PUT",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/octet-stream",
        },
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"blob PUT returned {resp.status}"
    return blake3.blake3(data).hexdigest()


def _get_blob(nest_url: str, blob_hash: str) -> bytes:
    """`GET /api/v1/blob/{hash}` — public, no bearer."""
    with urllib.request.urlopen(
        f"{nest_url}/api/v1/blob/{blob_hash}", timeout=30.0
    ) as resp:
        assert resp.status == 200, f"blob GET returned {resp.status}"
        return resp.read()


def _opaque_segment(tag: str) -> bytes:
    """Bytes shaped like what the nest actually receives: high-entropy payload
    it has no key for, framed by nothing it can parse.

    The marker rides *inside* deliberately. A sealed segment would not carry it
    in the clear — but the nest cannot tell sealed bytes from these, which is
    the whole point: it never looks. If any nest path ever opens, indexes, or
    FTS-ingests an `__index` blob, this is what it would find, and the
    assertions below are what would catch it.
    """
    return b"".join(
        [
            b"\x00\xff\x11not-a-valid-index-segment\x00",
            f"{_INDEX_PLAINTEXT_MARKER}:{tag}".encode(),
            os.urandom(256),
        ]
    )


@pytest.mark.feature("private-search-index")
def test_private_index_rests_opaque_syncs_to_a_second_device_and_survives_restart(
    index_nest,
):
    """Record two `__index` blobs → the nest holds them, serves them to a second
    device byte-identically, cannot read or query them, and still has them after
    a restart.

    Latency-independent (convention 14): every step is ordered by an RPC or an
    HTTP reply — `record` returns its journal `seq` after the row is written,
    and `start_nest_in_place` blocks on `/api/v1/health` before returning. No
    sleep, no poll, no deadline.
    """
    nest = index_nest
    url = nest["url"]
    actor = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )

    segment = _opaque_segment("segment")
    manifest = _opaque_segment("manifest")

    # ── the rail's two halves: bytes over HTTP, the reference over WS-RPC.
    seg_hash = _put_blob(url, actor["token"], segment)
    man_hash = _put_blob(url, actor["token"], manifest)

    with _ws(nest, actor) as ws:
        assert ws.call("fauna.index.list", {})["entries"] == [], (
            "sanity: a fresh actor has published nothing — the first-run state "
            "a builder starts from"
        )

        # Upload-before-record is a CONTRACT, not an ordering habit: a row the
        # nest cannot resolve is the corruption the builder's publish order
        # exists to prevent. Prove the gate is live before trusting the writes.
        with pytest.raises(RpcCallError) as unheld:
            ws.call(
                "fauna.index.record",
                {
                    "path": _SEGMENT_PATH,
                    "blob_hash": "11" * 32,
                    "size_bytes": len(segment),
                },
            )
        assert "bytes_not_held" in str(unheld.value), (
            "recording a blob the nest does not hold must fail with the typed "
            f"`bytes_not_held` code, got {unheld.value}"
        )

        seg_seq = ws.call(
            "fauna.index.record",
            {
                "path": _SEGMENT_PATH,
                "blob_hash": seg_hash,
                "size_bytes": len(segment),
            },
        )["seq"]
        man_seq = ws.call(
            "fauna.index.record",
            {
                "path": _MANIFEST_PATH,
                "blob_hash": man_hash,
                "size_bytes": len(manifest),
            },
        )["seq"]
        assert isinstance(seg_seq, int) and isinstance(man_seq, int), (
            f"record must return the journal seq a catch-up pull orders by, got "
            f"{seg_seq!r} / {man_seq!r}"
        )

        # ── STORES: both paths are live, pointing at the bytes we uploaded.
        entries = {e["path"]: e for e in ws.call("fauna.index.list", {})["entries"]}
        assert set(entries) == {_SEGMENT_PATH, _MANIFEST_PATH}, (
            f"the rail must list exactly the two recorded paths, got {sorted(entries)}"
        )
        assert entries[_SEGMENT_PATH]["blob_hash"] == seg_hash
        assert entries[_SEGMENT_PATH]["size_bytes"] == len(segment)
        assert entries[_MANIFEST_PATH]["blob_hash"] == man_hash

        # ── CANNOT READ: every field the rail serves is a reference, never
        # anything derived from the bytes. A content-derived field appearing
        # here would mean the nest opened the segment.
        assert set(entries[_SEGMENT_PATH]) == {"path", "blob_hash", "size_bytes"}, (
            "`fauna.index.list` must serve (path, blob_hash, size_bytes) and "
            f"nothing content-derived; got {sorted(entries[_SEGMENT_PATH])}"
        )

        # ── CANNOT QUERY: the nest's own search backend has never seen these
        # bytes. This is the standing regression pin for the deleted nest-side
        # writer (`IndexRegistry::ingest_mail`, live until 2026-07-12), which
        # put unsealed index content exactly where a query could reach it.
        hits = ws.call("fauna.search.query", {"query": _INDEX_PLAINTEXT_MARKER})
        assert hits["results"] == [], (
            "the nest must never be able to query the private index — "
            f"searching for a string carried inside a recorded `__index` blob "
            f"returned {hits['results']!r}. A nest-side index ingest has "
            "re-grown (content-index.md § Encryption posture)."
        )

    # ── SYNCS: a second device of the same actor enumerates the rail and pulls
    # the bytes. This is the replica-refresh path `fauna.index.list` exists for.
    with _ws(nest, actor) as second_device:
        replica = {
            e["path"]: e["blob_hash"]
            for e in second_device.call("fauna.index.list", {})["entries"]
        }
    assert replica == {_SEGMENT_PATH: seg_hash, _MANIFEST_PATH: man_hash}, (
        f"a second device must see the whole published rail, got {replica}"
    )
    assert _get_blob(url, seg_hash) == segment, (
        "the nest must relay the segment VERBATIM — the bytes came back changed, "
        "so something on the nest re-encoded a blob it has no key for"
    )

    # ── A RESTART DOES NOT LOSE IT. Same data dir, same port, no factory reset.
    #
    # The pid check is a non-vacuity guard, not ceremony: every assertion below
    # would pass just as well against a `stop_nest`/`start_nest_in_place` pair
    # that had quietly become a no-op, and the test would then claim a
    # restart-survival property it never exercised.
    pid_before = nest["proc"].pid
    stop_nest(nest)
    start_nest_in_place(nest)
    assert nest["proc"].pid != pid_before, (
        "the nest was not actually restarted (same pid) — everything below would "
        "pass without ever exercising a boot"
    )

    with _ws(nest, actor) as after:
        survived = {
            e["path"]: e["blob_hash"]
            for e in after.call("fauna.index.list", {})["entries"]
        }
    assert survived == {_SEGMENT_PATH: seg_hash, _MANIFEST_PATH: man_hash}, (
        "a nest restart lost `__index` rows — a boot path is purging the user's "
        f"private index again (content-index.md § Don't do these); got {survived}"
    )
    assert _get_blob(url, seg_hash) == segment, (
        "the rail survived the restart but its BYTES did not — the row now "
        "points at a blob the nest no longer holds"
    )
