"""Folder audience + website toggle — the phase-4 serving contract on a real
``fauna-nest`` binary (folders re-model phase 4; ``folders.md`` § Target
re-model; the public exception is owned by ``principles.md`` § The user always
controls their data).

The latent-404 closure, half one (the nest half): a **born-public** folder with
the **website toggle** on serves its plaintext-synced bytes to any visitor over
the real routes — chunk POST → manifest POST → ``fauna.sync.changes.record``
(the production ingest rail, fixture-setup carve-out of E2E rule 8) →
toggle-keyed ``web_files`` fan-out → the apex serve walk. The full app-driven
proof (a tui seat's real sync agent uploading through the engine's public arm)
is the phase-4 exit test and lands with the UI leg; this file pins the
nest-side contract it will ride.

Also pinned here, over the same real binary:

- the toggle is the fan-out key: flipping the website OFF stops the site
  serving (the ``web_files`` rows remain; the resolver refuses a toggle-off
  owner) — and flipping it back on serves again with no re-ingest;
- a record into a public folder may rest a **plaintext path** (the S9
  exemption's audience arm) — the seed below carries no ``path_sealed`` and is
  accepted;
- the audience projects on ``fauna.folders.list`` (``"public"``), and the
  transition matrix's nest half is pinned in
  ``bins/fauna-nest/tests/conformance_folder_audience.rs`` — not re-proven
  here.
"""

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

from tests.api.test_web_paywall_folder import _actor_client, _get, _post_bytes

pytestmark = pytest.mark.tier_3

SITE = "public-site"
PAGE = "hello.html"
MARKER = "phase-4-public-site-marker"
BODY = f"<h1>Hello</h1>\n<p>{MARKER}</p>\n".encode()
DEVICE_ID = bytes([0xD4] * 32)


def _seed_public_file(url: str, port: int, creator: dict, folder: str, path: str, content: bytes):
    """The plaintext production shape a public folder's engine uploads:
    unsealed chunks keyed by their plaintext hashes, a manifest with no
    ``stored_hashes``, and a record carrying no ``content_key_version`` and —
    the audience arm under test — no ``path_sealed``."""
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(content, None)
    for store_key, body in chunks:
        got = _post_bytes(port, "/api/v1/chunks", creator["token"], body, store_key)
        assert got == store_key.hex()
    manifest_hash = _post_bytes(port, "/api/v1/manifests", creator["token"], manifest_bytes)
    fauna_ffi.harness_record_change(
        url, bytes(creator["signing_key"]),
        {
            "folder": folder,
            "device_id": DEVICE_ID.hex(),
            "path": path,
            "manifest_hash": manifest_hash,
            "size_bytes": len(content),
            "change_type": "create",
        },
    )


@pytest.mark.feature("public-folders-and-websites")
def test_a_born_public_website_folder_serves_and_the_toggle_gates_it(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])

    # ── 1. A folder BORN public (plaintext from its first chunk — no re-seal
    # pass ever owed), website toggle on, and a write-capable device. The
    # folder is an ordinary `sync`-mode folder: phase 4's whole point is that
    # a website is a toggle over the same substrate, not a folder type. ──
    with _actor_client(url, creator) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {
                "name": SITE,
                "audience": "public",
            },
        )
        ws.call("fauna.folders.update", {"name": SITE, "website_enabled": True})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
        # The audience projects.
        listed = ws.call("fauna.folders.list", {})
        row = next(f for f in listed["folders"] if f["name"] == SITE)
        assert row["audience"] == "public", row
        assert row["website_enabled"] is True, row

    # ── 2. Plaintext bytes through the production ingest rail — including the
    # S9 audience arm: the record carries NO path_sealed and is accepted
    # because the folder's names/paths are world-readable by ratified design. ──
    _seed_public_file(url, port, creator, SITE, PAGE, BODY)

    # Serve at the apex (the shared test nest has no domain, so the apex
    # catch-all answers every host).
    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # ── 3. The file's own bytes serve to anyone — not the info page, not
        # framed CBOR (the exact silent-corruption defect
        # web-content-hosting.md § Implementation status records). ──
        status, body, _headers = _get(url, f"/{PAGE}")
        assert status == 200, body
        assert MARKER in body, f"the public site's own bytes must serve: {body}"

        # ── 4. Website OFF: the site stops serving — the toggle, not the
        # ingest history, is what serves. ──
        with _actor_client(url, creator) as ws:
            ws.call("fauna.folders.update", {"name": SITE, "website_enabled": False})
        status, body, _headers = _get(url, f"/{PAGE}")
        assert status != 200 or MARKER not in body, (
            f"a toggle-off folder must not serve: {status} {body}"
        )

        # ── 5. Toggle back ON: serves again with no re-ingest (the web_files
        # rows and the head rows both survive the flip). ──
        with _actor_client(url, creator) as ws:
            ws.call("fauna.folders.update", {"name": SITE, "website_enabled": True})
        status, body, _headers = _get(url, f"/{PAGE}")
        assert status == 200 and MARKER in body, (
            f"flipping the website back on must serve again: {status} {body}"
        )
    finally:
        # Leave the shared session nest apex-clean for sibling tests.
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})


FLIP_SITE = "flip-back-site"
FLIP_PAGE = "index.html"
FLIP_MARKER = "ccxxi-flip-back-marker"
FLIP_BODY = f"<h1>Flip</h1>\n<p>{FLIP_MARKER}</p>\n".encode()


@pytest.mark.feature("public-folders-and-websites")
def test_a_flip_back_to_private_stops_the_anonymous_serve_on_the_next_request(nest_instance):
    """the audience flip-back is the revoke on the ANONYMOUS door too.

    ``folder_public.rs`` reads the audience from the folder's current row on
    every request, which makes ``public → private`` an immediate revoke on the
    authenticated public-fetch door. This pins the same semantics on the
    anonymous website serve: a ``website_enabled`` folder serving plaintext at
    ``GET /``, flipped back to private **nest-side only** — no engine exists in
    this test, so no client-side re-seal pass can ever run — answers 404 on the
    very next request. Before the fix, ``serve_user_file`` gated on the toggle
    and seal-state alone and never read ``audience``, so the plaintext bytes
    kept serving to the open internet until a client-side convergence that
    three reachable states (engine unaware, keyless engine, cloud-only
    placeholder) never run at all.

    The website toggle stays ON throughout — audience alone is the gate under
    test ("the flag publishes the head, the audience decides who may read it",
    ``folders.md`` § Target re-model).
    """
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])

    with _actor_client(url, creator) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {
                "name": FLIP_SITE,
                "audience": "public",
            },
        )
        ws.call("fauna.folders.update", {"name": FLIP_SITE, "website_enabled": True})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    _seed_public_file(url, port, creator, FLIP_SITE, FLIP_PAGE, FLIP_BODY)

    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # Baseline: the public site serves its own bytes.
        status, body, _headers = _get(url, f"/{FLIP_PAGE}")
        assert status == 200 and FLIP_MARKER in body, (
            f"the public site must serve before the flip: {status} {body}"
        )

        # The flip back, nest-side only (the folder is unbound, so →private is
        # the legal transition). The web_files rows are untouched by design —
        # the corpus re-seal is client-driven — which is exactly why the door
        # itself must ask the audience.
        with _actor_client(url, creator) as ws:
            ws.call("fauna.folders.update", {"name": FLIP_SITE, "audience": "private"})

        # The very next anonymous request refuses. No sleep, no convergence
        # wait: the revoke is the row read on this request or it is not a
        # revoke.
        status, body, _headers = _get(url, f"/{FLIP_PAGE}")
        assert status != 200 or FLIP_MARKER not in body, (
            "a folder flipped back to private kept serving its plaintext to "
            f"the open internet: {status} {body}"
        )
    finally:
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})
