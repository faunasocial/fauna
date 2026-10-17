"""Publish a signed community labeler into a nest's labeler registry.

Fixture setup (convention 8 carve-out (b)): publishing is a publisher's act,
not a user gesture any app offers in v1, so a journey that needs a labeler in
the catalog arranges it here and drives only what a user does with it. The
publisher is a fresh registered User whose keypair IS the labeler's
``algorithm_id`` (public key is identity); the seal-helper's
``publish-labeler`` mode signs the metadata over the artifact's exact bytes,
and ``fauna.labelers.publish`` admits it (the nest re-verifies the binding).

One core, :func:`publish_labeler_version`, signs and publishes any version of
any artifact kind; :func:`new_publisher` mints the identity a first version is
published under, and its seed is what every later version is signed with.

Shared by the catalog journey (``test_labeler_catalog.py``), the community
room's labeler journey (``test_conversation_room_labelers.py``), the trust
grants journey (``test_nest_trust_grants.py``), the mail drain
(``test_capability_labeler_drain.py``) and the nest-tier labeler witnesses
(``tests/api/test_labeler_subscriptions.py``).
"""

from __future__ import annotations

import base64
import time
from pathlib import Path

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "labeler"


def new_publisher(nest_instance) -> tuple[bytes, bytes]:
    """A fresh registered User to publish as. Returns ``(labeler_id,
    publisher_seed)``: the 32-byte verify key that IS the labeler's
    ``algorithm_id``, and the Ed25519 seed every version is signed with."""
    admin = nest_instance["admin"]
    publisher = create_actor_and_register(
        nest_instance["port"], base_url=nest_instance["url"],
        admin_signing_key=admin["signing_key"],
    )
    return publisher["actor_id_bytes"], bytes(publisher["signing_key"])


def publish_labeler_version(
    run_seal_helper,
    nest_url: str,
    labeler_id: bytes,
    pub_seed: bytes,
    artifact: bytes,
    *,
    version: int,
    content_kind: str,
    artifact_kind: str,
    label_abi: int = 1,
) -> None:
    """Sign and publish one ``version`` of the labeler ``labeler_id`` over
    ``artifact`` — a WASM/WAT module for ``wasm``, the canonical dag-cbor
    artifact for ``list`` / ``text-model`` (``wasm_bytes`` carries the artifact
    for every kind; the field name is kept for in-major wire compat, and the
    hash/size binding and signature verify are identical for all three)."""
    metadata_blob = run_seal_helper(
        "publish-labeler",
        {
            "signing_seed_b64": base64.b64encode(pub_seed).decode(),
            "wasm_b64": base64.b64encode(artifact).decode(),
            "version": version,
            "needs_text": True,
            "needs_hashtags": False,
            "needs_media_metadata": False,
            "needs_author": False,
            # The signed output revision (``LabelerOutput::label_abi``): 1 is
            # the only one a build reads today; a later one mints a module
            # newer than this app.
            "label_abi": label_abi,
            "max_memory_bytes": 16 * 1024 * 1024,
            "max_cpu_microseconds": 100_000,
            "updated_at": int(time.time()),
        },
    )
    pub_ws = WsRpcAdminClient(nest_url, actor_id=labeler_id, signing_key=pub_seed)
    with pub_ws:
        reply = pub_ws.call(
            "fauna.labelers.publish",
            {
                "metadata_blob": metadata_blob,
                "wasm_bytes": artifact,
                "content_kind": content_kind,
                "artifact_kind": artifact_kind,
            },
        )
    assert reply.get("ok") is True, (
        f"{artifact_kind} v{version} publish reply not ok: {reply!r}"
    )
    assert bytes(reply["labeler_id"]) == labeler_id, "publish echoed a different labeler_id"


def publish_wasm_labeler(
    run_seal_helper, nest_instance, wat_path: Path, *, content_kind: str = "post",
    label_abi: int = 1,
) -> bytes:
    """Publish ``wat_path``'s module as version 1 of a new ``wasm`` labeler
    scoring ``content_kind`` (``post`` is public: subscribing or naming it needs
    no capability grant). Returns the labeler id bytes; its hex is the id every
    catalog row and room labeler set carries."""
    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed,
        Path(wat_path).read_bytes(),
        version=1, content_kind=content_kind, artifact_kind="wasm",
        label_abi=label_abi,
    )
    return labeler_id
