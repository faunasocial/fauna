"""What a taken-down post leaves, and what no door serves of a withheld item's
attachments — `docs/features/admin-nest.md` outcomes 42 and 43, observed from
outside a running nest (no app driver).

`docs/goal/behavior/takedown-and-block.md` § Legal takedown → *Posts* names the
serve paths a takedown binds besides the post read itself: (1) the author's web
site, re-rendered at once and again on the overturn; (2) the account export's
`posts` domain, which keeps the entry with an empty body and the citation;
(4) Search; and federation, which DISCLOSES the withholding — the WS peer fetch
carries the `legal_takedown` marker with an empty body and the HTTP twin answers
451. § Legal takedown → *The blob-serve door* and § Admin block of a reported
item (*Mechanism*) bind the bytes: every door onto the store refuses a withheld
blob, with 451 for a takedown and 403 for a block — "honesty about the ground
extends to the status code". The Rust witnesses (the § Implementation status
today entries) pin each half in-process; this is the same flow through the real
binary, read where a reader, a peer or a stranger reads it.

No app is driven: posts, the publish link, the report and the website folder go
over the author's own WS-RPC and HTTP calls, the takedown and block over the
admin's — `fauna.moderation.legal_takedown` and `fauna.moderation.admin_block`,
the calls the admin-nest console sends (its journeys are
`tests/test_admin_legal_takedown.py` and `tests/test_admin_block.py`). Every
assertion is a state read after a call the nest has acknowledged — the takedown,
the block and their overturns rebuild the blob withhold and re-render the site
before they reply — so nothing waits on a clock but the Search precondition,
which polls the index on a named budget (convention 14).
"""

from __future__ import annotations

import io
import json
import secrets
import time
import urllib.error
import urllib.request
import zipfile

import pytest
import requests

import fauna_ffi

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_share_links_nest_promises import LONG_LIFETIME_S, _plaintext_file
from tests.api.test_web_folder_audience import DEVICE_ID

pytestmark = pytest.mark.tier_3

LEGAL_REFERENCE = "EU-DSA-2026/0042"


# ── shared seams ─────────────────────────────────────────────────────────────


def _user(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )


def _admin(nest) -> dict:
    sk = nest["admin"]["signing_key"]
    return {"actor_id_bytes": bytes(sk.verify_key), "signing_key": sk}


def _admin_ws(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _ws(nest, who: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(who["actor_id_bytes"]), signing_key=bytes(who["signing_key"])
    )


def _get(url: str, path: str = "") -> tuple[int, bytes]:
    """An anonymous GET — a stranger, a crawler, a peer's HTTP fetch."""
    req = urllib.request.Request(url.rstrip("/") + path if path else url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


class _ServedAtApex:
    """Serve ``actor``'s site at the nest's apex for the block's duration. The
    apex actor is a nest-wide singleton, so it is always given back."""

    def __init__(self, nest, actor):
        self.nest, self.actor = nest, actor

    def __enter__(self):
        with _admin_ws(self.nest) as ws:
            ws.call("fauna.web.set_apex_actor", {"actor_id": bytes(self.actor["actor_id_bytes"])})
        return self

    def __exit__(self, *exc):
        with _admin_ws(self.nest) as ws:
            ws.call("fauna.web.set_apex_actor", {"actor_id": None})


# ── outcome 42 ───────────────────────────────────────────────────────────────


def _export_post_entry(nest, author: dict, post_id: str) -> dict:
    """The author's archive (``GET /api/v1/export``, the route every app's
    export button drives) and the ``export/posts/<id>.json`` entry in it."""
    resp = requests.get(
        f"{nest['url']}/api/v1/export",
        headers={"Authorization": f"Bearer {author['token']}"},
        timeout=120,
    )
    assert resp.status_code == 200, f"GET /api/v1/export: {resp.status_code} {resp.text[:300]}"
    with zipfile.ZipFile(io.BytesIO(resp.content)) as z:
        name = f"export/posts/{post_id}.json"
        assert name in z.namelist(), (
            f"the archive must keep the post's entry (the absence declared, never silent); "
            f"posts entries: {[n for n in z.namelist() if n.startswith('export/posts/')]}"
        )
        return json.loads(z.read(name))


def _quoting(nest, who: dict, word: str) -> list[dict]:
    """The Search hits whose snippet quotes ``word`` (a hit's ``content_id`` is
    the corpus key, not the post id, so the snippet is what names the post)."""
    return [h for h in ws_api.search(nest["port"], who, word) if word in (h.get("snippet") or "")]


def _peer_fetch(reader_nest, reader: dict, home_url: str, post_id: str) -> dict:
    """``fauna.posts.get_remote`` — the reader's nest fetches the post from its
    home over federation and forwards the home's answer verbatim."""
    with _ws(reader_nest, reader) as ws:
        return ws.call("fauna.posts.get_remote", {"post_id": post_id, "nest_url": home_url})


@pytest.mark.feature("admin-nest")
def test_a_taken_down_post_leaves_the_website_export_search_and_peers_and_a_restore_reopens_the_page(
    two_nodes,
):
    """Outcome 42 — a published, indexed post is taken down under a legal order:
    its web page and its index entry are gone, a fresh export keeps the entry
    with no body and the citation, Search quotes it to nobody — the author
    included — and a peer's fetch is told it is withheld (the WS marker with an
    empty body; the HTTP twin's 451). The overturn brings the page back. A
    sibling post nobody ordered down keeps every door, so each absence above is
    the takedown, not the fixture."""
    home, peer = two_nodes["nest_a"], two_nodes["nest_b"]
    url, port = home["url"], home["port"]
    author, reader, remote_reader = _user(home), _user(home), _user(peer)

    doomed_word = f"ordereddown{secrets.token_hex(5)}"
    kept_word = f"staysup{secrets.token_hex(5)}"
    now_us = int(time.time() * 1_000_000)
    kept_id = ws_api.create_post(
        port, author,
        sign_and_encode_post(author["signing_key"], now_us, f"Kept Page\n\nThis says {kept_word}."),
    )
    doomed_id = ws_api.create_post(
        port, author,
        sign_and_encode_post(
            author["signing_key"], now_us + 1_000_000, f"Doomed Page\n\nThis says {doomed_word}."
        ),
    )
    kept_slug = ws_api.web_publish_set(port, author, kept_id, slug="kept-page")
    doomed_slug = ws_api.web_publish_set(port, author, doomed_id, slug="doomed-page")
    peer_url = home["peer_url"]

    with _ServedAtApex(home, author):
        # ── Before the order: every door serves the post. ──
        status, page = _get(url, f"/post/{doomed_slug}.html")
        assert status == 200 and doomed_word.encode() in page, (
            f"precondition: the published post renders a page; got {status}"
        )
        status, index = _get(url, "/")
        assert status == 200 and f"/post/{doomed_slug}".encode() in index, (
            f"precondition: the site index links the post; got {status}"
        )
        entry = _export_post_entry(home, author, doomed_id)
        assert entry.get("data_hex") and not entry.get("legal_takedown_ref"), (
            f"precondition: the export carries the post's body; entry {entry}"
        )
        for who, label in ((reader, "a reader"), (author, "the author")):
            wait_until(
                lambda who=who: _quoting(home, who, doomed_word), RPC_ROUNDTRIP_S,
                interval=0.5,
                diagnose=lambda who=who: (
                    f"precondition: Search quotes the post to {label}; hits "
                    f"{ws_api.search(port, who, doomed_word)!r}"
                ),
            )
        fetched = _peer_fetch(peer, remote_reader, peer_url, doomed_id)
        assert fetched.get("body") and fetched.get("legal_takedown") is None, (
            f"precondition: a peer fetches the post's body; reply keys {sorted(fetched)}"
        )
        assert _get(url, f"/api/v1/posts/{doomed_id}")[0] == 200, (
            "precondition: the anonymous HTTP twin serves the post"
        )

        taken = ws_api.moderation_legal_takedown(
            port, _admin(home), doomed_id, legal_reference=LEGAL_REFERENCE
        )
        assert taken.get("status") == "taken_down", taken

        # ── (1) The author's web site: the page and its index link are gone
        # when the call returns; the site itself stays up. ──
        status, page = _get(url, f"/post/{doomed_slug}.html")
        assert status == 404 and doomed_word.encode() not in page, (
            f"\"taken_down\" must mean the page is already gone; got {status}"
        )
        status, index = _get(url, "/")
        assert status == 200, f"the site survives the takedown of one post; got {status}"
        assert f"/post/{doomed_slug}".encode() not in index and doomed_word.encode() not in index, (
            "the re-rendered index still lists the taken-down post"
        )
        assert f"/post/{kept_slug}".encode() in index, "the kept post stays on the index"
        status, page = _get(url, f"/post/{kept_slug}.html")
        assert status == 200 and kept_word.encode() in page, (
            f"the kept post's page keeps serving; got {status}"
        )

        # ── (2) The export: the entry stays, its body withheld and cited. ──
        entry = _export_post_entry(home, author, doomed_id)
        assert not entry.get("data_hex"), (
            f"a taken-down post's body must not ride its author's archive; entry {entry}"
        )
        assert entry.get("legal_takedown_ref") == LEGAL_REFERENCE, (
            f"the entry declares the absence with the order's citation; entry {entry}"
        )
        kept_entry = _export_post_entry(home, author, kept_id)
        assert kept_entry.get("data_hex"), "the kept post's body still rides the archive"

        # ── (4) Search: no snippet of the body, to a reader or its author. ──
        for who, label in ((reader, "a reader"), (author, "the author")):
            hits = _quoting(home, who, doomed_word)
            assert not hits, f"Search still quotes the taken-down post to {label}: {hits!r}"
        assert _quoting(home, reader, kept_word), "Search still finds the kept post"

        # ── Federation: a peer's fetch is told the post is withheld. ──
        fetched = _peer_fetch(peer, remote_reader, peer_url, doomed_id)
        assert not fetched.get("body"), "a peer's fetch must carry no body"
        assert fetched.get("legal_takedown"), (
            f"a peer's fetch discloses the withholding with the marker; reply {fetched!r}"
        )
        status, body = _get(url, f"/api/v1/posts/{doomed_id}")
        assert status == 451 and doomed_word.encode() not in body, (
            f"the HTTP twin answers 451 Unavailable For Legal Reasons; got {status}"
        )

        # ── The overturn: the author's publish link still stands, so the page
        # comes back. ──
        restored = ws_api.moderation_legal_takedown(
            port, _admin(home), doomed_id, legal_reference="", restore=True
        )
        assert restored.get("status") == "restored", restored
        status, page = _get(url, f"/post/{doomed_slug}.html")
        assert status == 200 and doomed_word.encode() in page, (
            f"an overturn re-renders the page; got {status}"
        )


# ── outcome 43 ───────────────────────────────────────────────────────────────


def _withhold_by_takedown(nest, post_id: str):
    reply = ws_api.moderation_legal_takedown(
        nest["port"], _admin(nest), post_id, legal_reference=LEGAL_REFERENCE
    )
    assert reply.get("status") == "taken_down", reply

    def lift():
        restored = ws_api.moderation_legal_takedown(
            nest["port"], _admin(nest), post_id, legal_reference="", restore=True
        )
        assert restored.get("status") == "restored", restored

    return lift


def _withhold_by_block(nest, post_id: str):
    """A user reports the post and the admin blocks it under that report — the
    admin-nest console's Block on a queue row."""
    reporter = _user(nest)
    with _ws(nest, reporter) as ws:
        report_id = ws.call(
            "fauna.moderation.abuse_report.submit",
            {"subject": {"kind": "post", "cid": post_id}, "reason": "harassment"},
        )["report_id"]
    with _admin_ws(nest) as ws:
        reply = ws.call("fauna.moderation.admin_block", {"report_id": report_id})
    assert reply.get("status") == "blocked", reply

    def lift():
        with _admin_ws(nest) as ws:
            restored = ws.call(
                "fauna.moderation.admin_block", {"report_id": report_id, "restore": True}
            )
        assert restored.get("status") == "restored", restored

    return lift


@pytest.mark.feature("admin-nest")
@pytest.mark.parametrize(
    ("ground", "withhold", "status"),
    [
        ("taken down", _withhold_by_takedown, 451),
        ("blocked", _withhold_by_block, 403),
    ],
    ids=["legal-takedown-451", "admin-block-403"],
)
def test_no_door_serves_a_withheld_posts_attachment(nest_instance, ground, withhold, status):
    """Outcome 43 — the picture of a post that is taken down (451) or blocked
    (403) is refused at every door onto the store: the blob and chunk doors the
    feed's media fetch reads, a share link to the same bytes, and the author's
    website serving them as a file. Each door answers with the ground's own
    status, never the other's and never the bytes; lifting the withhold serves
    them again, so the refusal is the withhold, not the fixture.

    The withhold binds a blob every live record naming it is withheld for, and
    blobs are content-addressed: the website's file, the share link's file and
    the post's picture are one chunk, so each door is asked for the same
    bytes."""
    nest = nest_instance
    url, port = nest["url"], nest["port"]
    author = _user(nest)
    secret = bytes(author["signing_key"])

    picture = b"\x89PNG\r\n\x1a\n" + secrets.token_bytes(512)
    _, chunks, manifest_hash = _plaintext_file(nest, author, picture)
    assert len(chunks) == 1, "a small file is one chunk"
    chunk_hex = chunks[0][0].hex()
    post_id = ws_api.create_post(
        port, author,
        fauna_ffi.build_post_with_media(
            secret, f"a picture, then {ground}",
            [{"blob_cid": fauna_ffi.content_cid(picture), "mime": "image/png",
              "size_bytes": len(picture)}],
        ),
    )
    _, share_url = fauna_ffi.harness_create_link(
        url, secret, bytes.fromhex(manifest_hash), "picture.png", LONG_LIFETIME_S
    )

    # The author's website carries the same picture as a file.
    site = f"withheld-attachment-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(url, secret, {"name": site, "audience": "public"})
    with _ws(nest, author) as ws:
        ws.call("fauna.folders.update", {"name": site, "website_enabled": True})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    fauna_ffi.harness_record_change(
        url, secret,
        {"folder": site, "device_id": DEVICE_ID.hex(), "path": "picture.png",
         "manifest_hash": manifest_hash, "size_bytes": len(picture), "change_type": "create"},
    )

    doors = {
        "the blob door": f"{url}/api/v1/blob/{chunk_hex}",
        "the chunk door": f"{url}/api/v1/chunks/{chunk_hex}",
        "a share link": share_url,
        "the website": f"{url}/picture.png",
    }

    with _ServedAtApex(nest, author):
        def answers() -> dict[str, tuple[int, bool]]:
            out = {}
            for door, target in doors.items():
                code, body = _get(target)
                out[door] = (code, picture in body)
            return out

        before = answers()
        assert all(code == 200 and served for code, served in before.values()), (
            f"precondition: every door serves the picture before it is {ground}: {before}"
        )

        lift = withhold(nest, post_id)
        during = answers()
        wrong = {door: a for door, a in during.items() if a != (status, False)}
        assert not wrong, (
            f"every door must refuse the picture of a {ground} post with {status} and no "
            f"bytes; these did not: {wrong} (all answers: {during})"
        )

        lift()
        after = answers()
        assert all(code == 200 and served for code, served in after.values()), (
            f"lifting the withhold serves the picture again at every door: {after}"
        )
