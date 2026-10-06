"""Publish a signed WASM community labeler into a nest's labeler registry.

Fixture setup (convention 8 carve-out (b)): publishing is a publisher's act,
not a user gesture any app offers in v1, so a journey that needs a labeler in
the catalog arranges it here and drives only what a user does with it. The
publisher is a fresh registered User whose keypair IS the labeler's
``algorithm_id`` (public key is identity); the seal-helper's
``publish-labeler`` mode signs the metadata over the module's exact bytes, and
``fauna.labelers.publish`` admits it (the nest re-verifies the binding).

Shared by the catalog journey (``test_labeler_catalog.py``) and the community
room's labeler journey (``test_conversation_room_labelers.py``).
"""

from __future__ import annotations

import base64
import time
from pathlib import Path

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "labeler"


def publish_wasm_labeler(
    run_seal_helper, nest_instance, wat_path: Path, *, content_kind: str = "post"
) -> bytes:
    """Publish ``wat_path``'s module as a ``wasm`` labeler scoring
    ``content_kind`` (``post`` is public: subscribing or naming it needs no
    capability grant). Returns the labeler id bytes; its hex is the id every
    catalog row and room labeler set carries."""
    admin = nest_instance["admin"]
    publisher = create_actor_and_register(
        nest_instance["port"], base_url=nest_instance["url"],
        admin_signing_key=admin["signing_key"],
    )
    pub_seed = bytes(publisher["signing_key"])  # 32-byte Ed25519 seed
    labeler_id = publisher["actor_id_bytes"]  # == algorithm_id (verify key)
    wat = Path(wat_path).read_bytes()
    metadata_blob = run_seal_helper(
        "publish-labeler",
        {
            "signing_seed_b64": base64.b64encode(pub_seed).decode(),
            "wasm_b64": base64.b64encode(wat).decode(),
            "version": 1,
            "needs_text": True,
            "needs_hashtags": False,
            "needs_media_metadata": False,
            "needs_author": False,
            "max_memory_bytes": 16 * 1024 * 1024,
            "max_cpu_microseconds": 100_000,
            "updated_at": int(time.time()),
        },
    )
    pub_ws = WsRpcAdminClient(nest_instance["url"], actor_id=labeler_id, signing_key=pub_seed)
    with pub_ws:
        reply = pub_ws.call(
            "fauna.labelers.publish",
            {
                "metadata_blob": metadata_blob,
                "wasm_bytes": wat,
                "content_kind": content_kind,
                "artifact_kind": "wasm",
            },
        )
    assert reply.get("ok") is True, f"publish reply not ok: {reply!r}"
    published_id = bytes(reply["labeler_id"])
    assert published_id == labeler_id, "publish echoed a different labeler_id"
    return labeler_id
