"""The in-repo hello plugin, published the way a real publisher would: a
signed client-metadata document plus its module, served on a real hostname
by ``helpers/client_metadata_server.py``, which the nest's ``test-hooks``
resolve map points at a loopback port
(``docs/goal/architecture/third-party.md`` § The runner contract).

One builder for every tier_3 test that installs a hosted plugin — the app
journey (``tests/test_plugin_install.py``) and the nest's own ingress witness
(``tests/api/test_plugin_ingress.py``) — so the document's shape lives once.
"""

from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

from helpers.client_metadata_server import (
    ClientMetadataServer,
    ed25519_did_key,
    sign_manifest,
)

HOST = "plugins.example"
DOCUMENT_PATH = "/hello.json"
MODULE_PATH = "/hello.wasm"
KIND = f"ext.{HOST}.notes"
NAME = "Hello Plugin"


# The reason the refusing variant's ``start`` answers with —
# ``fauna_plugin_host::fixture::REFUSING_PLUGIN_REASON``.
REFUSING_REASON = "the fixture plugin refuses to start"


def hello_component(out: Path, *, refusing: bool = False) -> bytes:
    """Run the ``plugin-fixture-gen`` e2e helper (prebuilt at collection time
    from the helper-binary registry, never here): the hello plugin's
    component, written to ``out`` — or, with ``refusing``, the variant whose
    ``start`` answers ``Err(REFUSING_REASON)``."""
    from helpers.helper_bins import helper_bin

    binary = helper_bin("plugin-fixture-gen")
    args = [str(binary), str(out)] + (["--refusing"] if refusing else [])
    subprocess.run(args, check=True)
    return out.read_bytes()


# A conversation-bridge declaration for the hello plugin's document — the
# ``bridge`` block (``third-party.md`` § The manifest → *The ``bridge``
# block*): a free one-label id, the ``globe`` glyph, every required capability
# off. A consent carries it to the account's roster, and ``fauna.bridges.list``
# then lists the plugin as a network row carrying the glyph.
BRIDGE_ID = "hello"
BRIDGE_GLYPH = "globe"
BRIDGE_BLOCK = {
    "id": BRIDGE_ID,
    "glyph": BRIDGE_GLYPH,
    "address_grammar": "^@[^:]+:.+$",
    "capabilities": {
        "supports_attachments": False,
        "supports_markdown": False,
        "supports_reactions": False,
        "supports_message_delete": False,
        "supports_per_message_reply": False,
        "supports_membership_change": False,
        "supports_recipient_selection": False,
        "supports_rename": False,
        "supports_subject": False,
        "delivery_mode": "Async",
    },
}


def publish_hello_plugin(
    tmp: Path,
    *,
    ingress: list | None = None,
    refusing: bool = False,
    bridge: dict | None = None,
) -> dict:
    """Serve the publisher's document + module on ``plugins.example``: a signed
    manifest whose ``execution`` pins the hello plugin's digest, one kind, one
    setting, no outbound hosts, and the given ``ingress`` declarations (none by
    default). ``refusing`` serves the variant whose ``start`` refuses
    (:func:`hello_component`), so the runner lists it ``stopped`` with
    :data:`REFUSING_REASON`. ``bridge`` adds a ``bridge`` block to the signed
    manifest (:data:`BRIDGE_BLOCK` is the one the tests use); none by default.

    Returns ``{"server", "document_url", "digest", "did_key"}``; the caller
    owns ``server`` and closes it."""
    module = hello_component(tmp / "hello.wasm", refusing=refusing)
    digest = f"sha256:{hashlib.sha256(module).hexdigest()}"
    key = ed25519.Ed25519PrivateKey.generate()
    public = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    server = ClientMetadataServer(HOST, tmp)
    payload = {
        "version": 1,
        "publisher": {"domain": HOST, "key": ed25519_did_key(public)},
        "execution": {
            "form": "wasm",
            "module": f"https://{HOST}{MODULE_PATH}",
            "digest": digest,
            "hosts": [],
        },
        "kinds": [{"kind": KIND, "class": "state", "merge": "latest-wins", "floor": "none"}],
        "settings_schema": {
            "greeting": {"label": "Greeting", "type": "string", "value": "hi"},
        },
        "ingress": list(ingress or []),
    }
    if bridge is not None:
        payload["bridge"] = bridge
    server.documents[DOCUMENT_PATH] = {
        "client_id": server.client_id(DOCUMENT_PATH),
        "client_name": NAME,
        "redirect_uris": [f"https://{HOST}/cb"],
        "scope": f"atproto fauna:records:rw:ext.{HOST}.*",
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "application_type": "web",
        "dpop_bound_access_tokens": True,
        "fauna": sign_manifest(key, payload),
    }
    server.blobs[MODULE_PATH] = module
    return {
        "server": server,
        "document_url": server.client_id(DOCUMENT_PATH),
        "digest": digest,
        "did_key": ed25519_did_key(public),
    }
