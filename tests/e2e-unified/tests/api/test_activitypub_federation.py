"""E2E test: ActivityPub federation between fauna-nest and the fediverse.

Two classes:

* ``TestActivityPubFederation`` — the actor-serving surface: enable/disable,
  WebFinger discovery, the actor document, NodeInfo, the pull outbox, the
  followers collection.
* ``TestActivityPubFollowerDelivery`` — the **produce direction**
  (``docs/goal/behavior/activitypub.md`` §§ The produce direction / Post
  deletion): an in-test fediverse server plays a real remote follower, so the
  signed-HTTP-POST delivery loop the unit tests stop short of
  (``ApSyncWorker::process_delivery_queue`` → ``deliver``) is exercised
  end-to-end. Only the last inch — a real Mastodon *rendering* the note — needs
  a human; every mechanism below is headless. The follower itself lives in
  ``helpers/fake_follower.py``, shared with the app-surface witnesses.
"""

import json
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import pytest

from common import (
    create_actor_and_register,
    forwarded_post_id,
    sign_post_envelope,
)
from clients.ws_rpc_admin_client import RpcCallError
from clients.ws_rpc_federation_client import FederationChannelClient
from drivers.port_util import find_free_port
from helpers.ap_nest import (
    AP_BRIDGE_ID,
    ap_post_map_row,
    build_ap_nest_binary,
    content_row_exists,
    deliver_note,
    delivery_jobs_for,
    enable_ap,
    start_ap_nest,
)
from helpers.fake_follower import (
    FakeFollower,
    assert_valid_http_signature,
)
from tests.api import ws_api
from tests.api.bare import (
    post_reference,
    sign_and_encode_post,
    sign_and_encode_tombstone,
)

pytestmark = pytest.mark.tier_3

AS_PUBLIC = "https://www.w3.org/ns/activitystreams#Public"


def tiny_png() -> bytes:
    """A valid 1x1 PNG: real image bytes for the blob verifiers."""
    import struct
    import zlib

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00\xff"))
        + chunk(b"IEND", b"")
    )


def fetch_served(nest, url: str) -> bytes:
    """Dereference a URL this nest published, the way a remote server does.

    Published URLs name the nest's AP domain over `https`; the fixture nest
    serves that domain in plain HTTP on loopback, so the path is fetched there.
    """
    prefix = f"https://{nest['domain']}/"
    assert url.startswith(prefix), f"{url} is not on this nest's domain"
    with urllib.request.urlopen(f"{nest['url']}/{url[len(prefix):]}") as resp:
        assert resp.status == 200, f"{url} does not resolve: {resp.status}"
        return resp.read()


def feed_bodies(nest, user) -> list[str]:
    """The bodies of every post on the user's local feed page."""
    return [p["body"] for p in ws_api.local_feed_posts(nest["port"], user, limit=200)]


def fanouts_initiated(nest) -> int:
    """Post bridge fan-outs this nest has **initiated** so far.

    `GET /api/v1/test/posts/fanouts` (gated on `--features test-hooks`, which
    the ap_nest fixture's build enables). The counter is bumped synchronously
    inside `spawn_post_bridge_fanout`, i.e. at the point a handler *decides* to
    fan a post out and before that handler replies — so a caller reads it,
    makes a synchronous RPC, reads it again, and the difference is a statement
    about the decision rather than about elapsed time
    (`e2e-conventions.md` § convention 14; rationale in
    `bins/fauna-nest/src/post_fanout_test_hook.rs`).
    """
    status, body = api_get(nest["url"], "/api/v1/test/posts/fanouts")
    assert status == 200, (
        f"fan-out counter hook returned {status} — is the nest built with "
        "`--features test-hooks`?"
    )
    return body["initiated"]


def api_get(base_url, path, token=None, accept=None):
    """GET request with optional Accept header."""
    url = f"{base_url}{path}"
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if accept:
        headers["Accept"] = accept
    req = urllib.request.Request(url, headers=headers, method="GET")
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, {}


@pytest.fixture()
def fake_follower():
    """A remote fediverse follower served on a free loopback port."""
    with FakeFollower(find_free_port()) as follower:
        yield follower


@pytest.fixture()
def strict_follower():
    """The same follower in secure mode: it refuses UNSIGNED actor fetches.

    The headless stand-in for the real-peer strict-fetch harness
    (`tests/platform/fediverse/`, opt-in behind `FAUNA_E2E_FEDIVERSE=1`). Running
    the same shape here keeps the regression inside a default `--tier 3` run,
    where it is actually caught.
    """
    with FakeFollower(find_free_port(), require_signed_get=True) as follower:
        yield follower


@pytest.fixture()
def ua_demanding_follower():
    """A follower that refuses any request without a `User-Agent`, as GoToSocial does.

    The headless stand-in for the GoToSocial half of the interop harness
    (`tests/platform/fediverse/`, opt-in). Same reasoning as `strict_follower`:
    the opt-in suite is where the divergence was *found*, but a default
    `--tier 3` run is where a regression must be *caught*.
    """
    with FakeFollower(find_free_port(), require_user_agent=True) as follower:
        yield follower


@pytest.fixture(scope="module")
def ap_binary():
    """Build fauna-nest with activitypub feature."""
    return build_ap_nest_binary()


@pytest.fixture()
def ap_nests(ap_binary, tmp_path_factory):
    """Start two AP-enabled nest instances."""
    tmp = tmp_path_factory.mktemp("ap_federation")
    # Use localhost:port as the domain — AP will use these for actor URLs
    nest_a = start_ap_nest(ap_binary, tmp / "a", 13101, "127.0.0.1:13101")
    nest_b = start_ap_nest(ap_binary, tmp / "b", 13102, "127.0.0.1:13102")
    try:
        yield {
            "a": nest_a,
            "b": nest_b,
        }
    finally:
        nest_a["proc"].kill()
        nest_b["proc"].kill()
        nest_a["proc"].wait()
        nest_b["proc"].wait()


class TestActivityPubFederation:
    """Test ActivityPub federation between two local nests."""

    def test_enable_activitypub(self, ap_nests):
        """Users can enable AP federation and get a handle."""
        nest_a = ap_nests["a"]
        user_a = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        reply = ws_api.bridge_link(nest_a["port"], user_a, AP_BRIDGE_ID, "enable")
        assert reply["linked"] is True, f"link failed: {reply}"
        assert reply["identity"]["value"].startswith("https://")
        assert "@" in reply["identity"]["display"]

        # The bridge list should show it linked, as the same handle.
        bridge = ws_api.bridge_status(nest_a["port"], user_a, AP_BRIDGE_ID)
        assert bridge["linked"] is True
        assert bridge["identity"]["display"] == reply["identity"]["display"]

    @pytest.mark.feature("fediverse")
    def test_enable_derives_username_from_handle(self, ap_nests):
        """A handled actor's AP username is their Fauna handle, verbatim.

        `activitypub.md` § Identity & keys (user-ratified 2026-07-16): the
        username mints from the handle at enablement and freezes there. The
        admin-created actors every other test in this file uses are
        handle-less and pin the legacy 16-hex fallback implicitly; this one
        self-registers with a handle (`fauna.account.register`) and asserts
        the natural identity, resolvable via WebFinger.
        """
        from common.auth import register_handled_actor

        nest_a = ap_nests["a"]
        handle = f"apuser{int(time.time()) % 1000000}"
        # Sign over the nest's identity domain (boot-seeded from `[nest]
        # domain` — see `start_ap_nest`).
        user = register_handled_actor(nest_a["port"], handle, nest_a["domain"])

        username, actor_url = enable_ap(nest_a, user)
        assert username == handle, (
            f"AP username must be the Fauna handle: got {username!r}, handle {handle!r}"
        )
        assert actor_url.endswith(f"/ap/users/{handle}")

        # WebFinger resolves the handle-shaped acct: URI to this actor.
        domain = nest_a["domain"]
        resource = f"acct:{handle}@{domain}"
        status, data = api_get(
            nest_a["url"],
            f"/.well-known/webfinger?resource={urllib.request.quote(resource, safe='')}",
        )
        assert status == 200
        assert data["subject"] == resource
        assert data["links"][0]["href"] == actor_url

    def test_enable_already_enabled_raises_already_linked(self, ap_nests):
        """Enabling AP twice raises `fauna.bridges.already_linked` (was HTTP 409)."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        ws_api.bridge_link(nest_a["port"], user, AP_BRIDGE_ID, "enable")
        with pytest.raises(RpcCallError) as exc:
            ws_api.bridge_link(nest_a["port"], user, AP_BRIDGE_ID, "enable")
        assert exc.value.code == "fauna.bridges.already_linked", exc.value.code

    def test_disable_activitypub(self, ap_nests):
        """Users can disable AP federation."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        ws_api.bridge_link(nest_a["port"], user, AP_BRIDGE_ID, "enable")
        ws_api.bridge_unlink(nest_a["port"], user, AP_BRIDGE_ID)

        bridge = ws_api.bridge_status(nest_a["port"], user, AP_BRIDGE_ID)
        assert bridge["linked"] is False

    @pytest.mark.feature("fediverse")
    def test_webfinger_discovery(self, ap_nests):
        """WebFinger resolves an AP-enabled user's handle."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        username, _ = enable_ap(nest_a, user)
        domain = nest_a["domain"]

        resource = f"acct:{username}@{domain}"
        status, data = api_get(
            nest_a["url"],
            f"/.well-known/webfinger?resource={urllib.request.quote(resource, safe='')}",
        )
        assert status == 200
        assert data["subject"] == resource
        assert len(data["links"]) >= 1
        assert data["links"][0]["rel"] == "self"
        assert "activity+json" in data["links"][0]["type"]

    @pytest.mark.feature("fediverse")
    def test_actor_endpoint(self, ap_nests):
        """The AP actor endpoint serves a valid Person object."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        username, _ = enable_ap(nest_a, user)

        status, data = api_get(
            nest_a["url"],
            f"/ap/users/{username}",
            accept="application/activity+json",
        )
        assert status == 200
        assert data["type"] == "Person"
        assert data["preferredUsername"] == username
        assert "publicKey" in data
        assert "inbox" in data
        assert "outbox" in data

    def test_nodeinfo(self, ap_nests):
        """The NodeInfo discovery link resolves to a real NodeInfo document.

        A flow assertion, not a shape check: the discovery link is *followed*,
        the way a fediverse peer follows it. Asserting only that ``links`` is
        non-empty is exactly what let the href dangle at the deleted
        ``/api/v1/node-info`` twin from 2026-06-05 (the HTTP
        discovery rip) to 2026-07-16 — the link was present the whole time and
        pointed at nothing, so peers got the SPA's HTML fallback.
        """
        nest_a = ap_nests["a"]

        status, data = api_get(nest_a["url"], "/.well-known/nodeinfo")
        assert status == 200
        links = data.get("links", [])
        assert len(links) >= 1

        rel = "http://nodeinfo.diaspora.software/ns/schema/2.1"
        hrefs = [link["href"] for link in links if link.get("rel") == rel]
        assert hrefs, f"no {rel} link in {links}"
        href = hrefs[0]

        # The href must name this nest's AP domain — a peer resolves it globally.
        parsed = urllib.parse.urlparse(href)
        assert parsed.netloc == nest_a["domain"], f"href points off-nest: {href}"

        # Follow it, as a peer would (over the fixture's http origin). Read the
        # raw response rather than `api_get`, so a dangling href diagnoses
        # itself: json-decoding the SPA's HTML fallback surfaces only as an
        # opaque "Expecting value: line 1 column 1".
        doc_url = f"{nest_a['url']}{parsed.path}"
        try:
            with urllib.request.urlopen(doc_url) as resp:
                status, raw = resp.status, resp.read().decode()
                ctype = resp.headers.get("Content-Type", "")
        except urllib.error.HTTPError as e:
            raise AssertionError(
                f"discovery href {href} does not resolve: HTTP {e.code}"
            ) from None

        assert status == 200, f"discovery href {href} does not resolve: {status}"
        assert "json" in ctype.lower(), (
            f"discovery href {href} served Content-Type {ctype!r} — the link "
            f"dangles at a route this nest does not serve, so a peer gets the "
            f"SPA's HTML fallback instead of a NodeInfo document. "
            f"Body starts: {raw[:80]!r}"
        )
        doc = json.loads(raw)

        assert doc["version"] == "2.1"
        # Only NodeInfo's own vocabulary — the nest's internal `fauna`/`nostr`/
        # `bluesky` protocol tokens are absent from the schema enum and would
        # fail a strict peer validator.
        assert doc["protocols"] == ["activitypub"], f"protocols: {doc['protocols']}"
        assert doc["software"]["name"] == "fauna"
        assert doc["services"] == {"inbound": [], "outbound": []}
        assert isinstance(doc["openRegistrations"], bool)

        # Fingerprint-lean: the `major.minor` line only, never the patch level
        # (same posture as the anonymous `fauna.nest.info` reply).
        version_parts = doc["software"]["version"].split(".")
        assert len(version_parts) == 2 and all(p.isdigit() for p in version_parts), (
            f"software.version must be coarsened to major.minor: {doc['software']['version']}"
        )

        # `usage.users.total` counts AP-*enabled* accounts, not nest population:
        # a shipped-but-unenabled nest reports 0, and enabling one account
        # moves it to 1.
        assert doc["usage"]["users"]["total"] == 0

        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        enable_ap(nest_a, user)

        _, doc_after = api_get(nest_a["url"], parsed.path)
        assert doc_after["usage"]["users"]["total"] == 1

    @pytest.mark.feature("fediverse")
    def test_outbox_empty_without_backfill(self, ap_nests):
        """Outbox returns 0 items when backfill is disabled."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        username, _ = enable_ap(nest_a, user)

        status, data = api_get(
            nest_a["url"],
            f"/ap/users/{username}/outbox",
            accept="application/activity+json",
        )
        assert status == 200
        assert data["type"] == "OrderedCollection"
        assert data["totalItems"] == 0

    def test_followers_collection_empty(self, ap_nests):
        """Followers collection starts empty."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        username, _ = enable_ap(nest_a, user)

        status, data = api_get(
            nest_a["url"],
            f"/ap/users/{username}/followers",
            accept="application/activity+json",
        )
        assert status == 200
        assert data["type"] == "OrderedCollection"
        assert data["totalItems"] == 0

    @pytest.mark.feature("fediverse")
    def test_update_settings(self, ap_nests):
        """AP settings can be updated, and the new values read back."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        enable_ap(nest_a, user)
        ws_api.bridge_set_settings(
            nest_a["port"],
            user,
            AP_BRIDGE_ID,
            {"auto_accept_follows": False, "default_visibility": "unlisted"},
        )

        # The provider's settings are the read-back surface the Bridges page
        # renders — assert the write landed, which the old 204 never proved.
        settings = {
            s["key"]: s["value"]
            for s in ws_api.bridge_status(nest_a["port"], user, AP_BRIDGE_ID)["settings"]
        }
        assert settings["auto_accept_follows"] is False
        assert settings["default_visibility"] == "unlisted"

    @pytest.mark.feature("bridges")
    def test_bridge_api_shows_activitypub(self, ap_nests):
        """The unified bridge API lists ActivityPub as available.

        `fauna.bridges.list` (handler in
        `bins/fauna-nest/src/bridges_ui_handlers.rs::list_handler`) is the
        available-bridges surface — and, since the `/api/v1/activitypub/*` rip,
        the *only* ActivityPub control plane.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        bridges = ws_api.bridge_list(nest_a["port"], user)
        bridge_ids = [b["id"] for b in bridges]
        assert AP_BRIDGE_ID in bridge_ids, f"bridges: {bridge_ids}"

        ap_bridge = next(b for b in bridges if b["id"] == AP_BRIDGE_ID)
        assert ap_bridge["available"] is True
        assert ap_bridge["supports_follows"] is True
        # The capability the Bridges card reads before it asks for the list
        # (`bridges.md` § Follow requests).
        assert ap_bridge["supports_follow_requests"] is True

    @pytest.mark.feature("fediverse")
    def test_actor_document_says_whether_followers_are_approved_by_hand(self, ap_nests):
        """`manuallyApprovesFollowers` is the negation of the *accept follows by
        itself* setting — the field peers render the lock and their "request
        sent" state from (`activitypub.md` § Follow requests).

        Flow: `fauna.bridges.set_settings` → `ap_accounts.auto_accept_follows`
        → `get_actor` reads the account row → the served document.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, _ = enable_ap(nest_a, user)

        def served_flag():
            status, doc = api_get(
                nest_a["url"], f"/ap/users/{username}", accept="application/activity+json"
            )
            assert status == 200, f"actor document unavailable: {status}"
            return doc["manuallyApprovesFollowers"]

        assert served_flag() is False, "follows are accepted by themselves by default"
        ws_api.bridge_set_settings(
            nest_a["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": False}
        )
        assert served_flag() is True
        ws_api.bridge_set_settings(
            nest_a["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": True}
        )
        assert served_flag() is False

    def test_actor_returns_404_for_disabled_user(self, ap_nests):
        """The actor endpoint returns 404 after AP is disabled."""
        nest_a = ap_nests["a"]
        user = create_actor_and_register(nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"])

        username, _ = enable_ap(nest_a, user)
        ws_api.bridge_unlink(nest_a["port"], user, AP_BRIDGE_ID)

        status, _ = api_get(
            nest_a["url"],
            f"/ap/users/{username}",
            accept="application/activity+json",
        )
        assert status == 404

    @pytest.mark.feature("fediverse")
    def test_a_later_handle_change_leaves_the_fediverse_address_as_it_was(self, ap_nests):
        """The AP username mints from the handle at enablement and freezes
        (`activitypub.md` § Identity & keys): a handle change applied later
        moves the Fauna handle and leaves the fediverse address where it was.

        Flow: `fauna.profile.handle.change` schedules the pending action → the
        `run_due` test hook runs the same executor the background tick runs →
        `fauna.actor.by_handle` resolves the NEW handle (the change applied)
        while WebFinger and the actor document keep answering for the OLD one.
        """
        nest_a = ap_nests["a"]
        old = f"apfrozen{int(time.time()) % 1000000}"
        new = f"{old}moved"
        from common.auth import register_handled_actor

        user = register_handled_actor(nest_a["port"], old, nest_a["domain"])
        username, actor_url = enable_ap(nest_a, user)
        assert username == old

        ws_api._client_for(ws_api._base_url(nest_a["port"]), user).call(
            "fauna.profile.handle.change", {"handle": new}
        )
        req = urllib.request.Request(
            f"{nest_a['url']}/api/v1/test/pending_actions/run_due",
            data=b"{}",
            method="POST",
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req) as resp:
            ran = json.loads(resp.read())
        assert ran.get("executed", 0) >= 1, f"the handle change never applied: {ran}"
        assert ws_api.actor_by_handle(nest_a["port"], new)["actor_id"] == user["actor_id_hex"], (
            "precondition: the Fauna handle moved"
        )

        domain = nest_a["domain"]
        status, data = api_get(
            nest_a["url"],
            f"/.well-known/webfinger?resource={urllib.request.quote(f'acct:{old}@{domain}', safe='')}",
        )
        assert status == 200, "the fediverse address must still resolve after the change"
        assert data["links"][0]["href"] == actor_url
        status, doc = api_get(
            nest_a["url"], f"/ap/users/{old}", accept="application/activity+json"
        )
        assert status == 200 and doc["id"] == actor_url
        assert doc["preferredUsername"] == old
        status, _ = api_get(
            nest_a["url"],
            f"/.well-known/webfinger?resource={urllib.request.quote(f'acct:{new}@{domain}', safe='')}",
        )
        assert status == 404, "the new handle names no fediverse account"

    @pytest.mark.feature("fediverse")
    def test_the_actor_carries_the_profiles_name_picture_and_bio(self, ap_nests):
        """The actor document is rendered from the user's signed `Profile`
        (`activitypub.md` § Architecture → Actor serving): `name` is the display
        name, `summary` the bio, and `icon` the avatar, at a URL that serves
        the avatar's bytes to anyone who fetches it, as a peer does.

        Flow: blob PUT → `fauna.profile.set` with a profile naming that blob →
        `get_actor` decodes the newest profile row → `fauna_profile_to_ap_person`
        → the served document → its `icon.url` dereferenced.
        """
        import fauna_ffi
        from helpers.xrpc_client import put_blob

        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, _ = enable_ap(nest_a, user)

        avatar = tiny_png()
        avatar_cid = fauna_ffi.content_cid(avatar)
        put_blob(nest_a["url"], user["token"], avatar_cid, avatar)
        ws_api._client_for(ws_api._base_url(nest_a["port"]), user).call(
            "fauna.profile.set",
            {
                "body": fauna_ffi.build_profile_with_pictures(
                    bytes(user["signing_key"]),
                    display_name="Ada Lovelace",
                    bio="Analyst & engine enthusiast",
                    avatar_cid=avatar_cid,
                )
            },
        )

        status, doc = api_get(
            nest_a["url"], f"/ap/users/{username}", accept="application/activity+json"
        )
        assert status == 200
        assert doc["name"] == "Ada Lovelace"
        assert doc["summary"] == "<p>Analyst &amp; engine enthusiast</p>"
        icon = doc.get("icon")
        assert icon and icon.get("type") == "Image", f"no avatar on the actor: {doc}"
        assert fetch_served(nest_a, icon["url"]) == avatar, (
            "the actor's icon URL must serve the avatar's bytes"
        )

    @pytest.mark.feature("fediverse")
    def test_the_follower_collection_is_a_count_never_the_members(
        self, ap_nests, fake_follower
    ):
        """`followers` is count-only (`activitypub.md` § Implementation status
        today, gap 2 — deliberate): two accepted followers read as
        `totalItems: 2`, and nothing in the document names either of them.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)

        with FakeFollower(find_free_port(), username="carol") as second:
            for follower in (fake_follower, second):
                code = follower.post_signed(
                    f"{nest_a['url']}/ap/users/{username}/inbox",
                    {
                        "@context": "https://www.w3.org/ns/activitystreams",
                        "type": "Follow",
                        "id": f"{follower.actor_uri}/activities/follow-1",
                        "actor": follower.actor_uri,
                        "object": actor_url,
                    },
                )
                assert code == 202
                follower.wait_for("Accept")

            req = urllib.request.Request(
                f"{nest_a['url']}/ap/users/{username}/followers",
                headers={"Accept": "application/activity+json"},
            )
            with urllib.request.urlopen(req) as resp:
                raw = resp.read().decode()
            doc = json.loads(raw)
            assert doc["type"] == "OrderedCollection"
            assert doc["totalItems"] == 2
            for key in ("orderedItems", "items", "first", "last"):
                assert key not in doc, f"the collection exposes {key!r}: {doc}"
            for follower in (fake_follower, second):
                assert follower.actor_uri not in raw, "a follower is named"

    @pytest.mark.feature("fediverse")
    def test_publishing_history_serves_earlier_public_posts(self, ap_nests):
        """With `backfill` on, the pull outbox serves a post written before
        federation was ever enabled (`activitypub.md` § Architecture → the pull
        outbox): the root counts it and links its first page, the page carries
        its `Create`, and the note id dereferences.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        body_text = "written long before I federated"
        post_id = ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), body_text),
        )
        username, actor_url = enable_ap(nest_a, user)
        ws_api.bridge_set_settings(nest_a["port"], user, AP_BRIDGE_ID, {"backfill": True})

        status, root = api_get(
            nest_a["url"], f"/ap/users/{username}/outbox", accept="application/activity+json"
        )
        assert status == 200
        assert root["totalItems"] == 1, root
        assert root["first"] == f"{actor_url}/outbox?page=0", root

        status, page = api_get(
            nest_a["url"],
            f"/ap/users/{username}/outbox?page=0",
            accept="application/activity+json",
        )
        assert status == 200
        items = page["orderedItems"]
        assert [i["type"] for i in items] == ["Create"], items
        note = items[0]["object"]
        assert note["id"] == f"{actor_url}/notes/{post_id}"
        assert body_text in note["content"]
        status, served = api_get(
            nest_a["url"],
            f"/ap/users/{username}/notes/{post_id}",
            accept="application/activity+json",
        )
        assert status == 200 and served["id"] == note["id"], "the note id dereferences"


class TestActivityPubFollowerDelivery:
    """The produce direction, against a fediverse server that really receives.

    The push's unit tests (`activitypub::push::tests`) stop at the delivery
    queue: they assert the right jobs were enqueued. Everything past that —
    the worker picking the job up, decrypting the account's RSA key, signing
    the POST, and a remote actually accepting it — is what these tests cover,
    with `FakeFollower` standing in for the remote server.
    """

    def _account_public_key(self, nest, username: str) -> str:
        """The RSA public key the nest publishes for this account."""
        status, actor_doc = api_get(
            nest["url"], f"/ap/users/{username}", accept="application/activity+json"
        )
        assert status == 200, f"actor document unavailable: {status}"
        return actor_doc["publicKey"]["publicKeyPem"]

    def _follow(self, nest, follower, username: str, actor_url: str) -> None:
        """Deliver a signed Follow to the user's inbox (auto-accept is default-on)."""
        code = follower.post_signed(
            f"{nest['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Follow",
                "id": f"{follower.actor_uri}/activities/follow-1",
                "actor": follower.actor_uri,
                "object": actor_url,
            },
        )
        assert code == 202, f"inbox did not accept the Follow: HTTP {code}"

    @pytest.mark.feature("fediverse")
    def test_follow_is_auto_accepted_and_delivered(self, ap_nests, fake_follower):
        """A signed Follow is verified, recorded, and answered with an Accept.

        The Accept proves the whole inbound half: the nest fetched our actor
        document, verified our HTTP signature against the key it found there,
        and delivered a signed activity back to the inbox we advertised.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)

        self._follow(nest_a, fake_follower, username, actor_url)

        entry = fake_follower.wait_for("Accept")
        assert entry["activity"]["actor"] == actor_url
        assert_valid_http_signature(
            entry,
            self._account_public_key(nest_a, username),
            f"/users/{fake_follower.username}/inbox",
        )

    def test_follow_from_a_secure_mode_peer_is_accepted(self, ap_nests, strict_follower):
        """A peer that refuses UNSIGNED actor fetches can still federate with us.

        The whole phase-2 gap in one assertion, headless. To verify this Follow
        the nest must fetch the sender's actor document for its key; the sender
        serves that document only to a *signed* request; so the Accept coming
        back proves our outbound GET carried a valid instance-actor signature.

        Before the instance actor existed this could not pass — and it failed in
        a way that pointed nowhere near the cause: `fetch_remote_actor` ignored
        the HTTP status, so the 401's JSON body became an "actor" with an empty
        key, was cached for 24 h, and the error surfaced as a PEM parse failure.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)

        self._follow(nest_a, strict_follower, username, actor_url)

        entry = strict_follower.wait_for("Accept")
        assert entry["activity"]["actor"] == actor_url

        # …and it was OUR signature that unlocked it, not a lucky retry: the
        # served fetch carried a Signature naming the nest's instance actor.
        signed_gets = [g for g in strict_follower.actor_gets if "signature" in g]
        assert signed_gets, (
            "the nest never sent a signed actor GET, so the Accept came from "
            f"somewhere else. Actor GETs seen: {strict_follower.actor_gets}"
        )
        assert "/ap/instance#main-key" in signed_gets[-1]["signature"], (
            "the actor GET was signed, but not with the instance actor — "
            f"signature was {signed_gets[-1]['signature']!r}"
        )

    def test_follow_from_a_user_agent_demanding_peer_is_accepted(
        self, ap_nests, ua_demanding_follower
    ):
        """A peer that refuses UA-less requests can federate with us.

        GoToSocial answers a request carrying no `User-Agent` with `418 I'm a
        teapot`. Because verifying an inbound Follow requires first fetching the
        sender's actor document, a UA-less nest cannot complete *any* exchange
        with such a peer — the Accept below is the whole property.

        This is a real regression pin, not a hypothetical: our outbound AP client
        set no `User-Agent` at all from the feature's inception, and Mastodon —
        the only peer the harness had — never minded. The defect surfaced the
        first time a second implementation was put in front of it.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)

        self._follow(nest_a, ua_demanding_follower, username, actor_url)

        entry = ua_demanding_follower.wait_for("Accept")
        assert entry["activity"]["actor"] == actor_url

        # Green for the right reason: assert the header was actually present on
        # the fetch, not merely that the exchange completed.
        agents = [
            g.get("user-agent") for g in ua_demanding_follower.actor_gets
        ]
        assert any(agents), (
            "the nest completed the exchange but sent no User-Agent on any actor "
            f"GET, so this passed for the wrong reason. GETs seen: "
            f"{ua_demanding_follower.actor_gets}"
        )
        # Ours is the static, version-less product string (`activitypub.md`
        # § Architecture, the User-Agent bullet: a version would tell any
        # stranger which release this nest runs); its exact text is pinned
        # headlessly, this only asks that the header attributes us.
        assert any(a and a.startswith("fauna-nest") for a in agents), (
            f"the actor GET carried a User-Agent, but not ours: {agents!r}"
        )

    def test_a_refused_actor_fetch_is_not_cached_as_an_actor(
        self, ap_nests, strict_follower
    ):
        """A non-2xx actor response must not become a cached actor.

        Drives the refusal directly: `fauna.bridges.add_follow` resolves a
        remote actor by URI, and this peer refuses the *unsigned* probe the
        request makes before... no — it refuses nothing signed, so we assert the
        inverse property that actually matters at rest: after any interaction,
        the remote-actor cache never holds a row with an empty public key.

        An empty-key row is the residue of a swallowed error response, and it is
        silently fatal: the cache is authoritative for 24 h, so every later
        activity from that actor fails signature verification against an empty
        PEM — reported as a crypto error, pointing at the wrong subsystem.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        self._follow(nest_a, strict_follower, username, actor_url)
        strict_follower.wait_for("Accept")

        import sqlite3

        conn = sqlite3.connect(f"file:{nest_a['db_path']}?mode=ro", uri=True, timeout=10.0)
        try:
            rows = conn.execute(
                "SELECT uri, public_key_pem FROM ap_remote_actors"
            ).fetchall()
        finally:
            conn.close()

        assert rows, "no remote actor was cached at all — the fetch never succeeded"
        keyless = [uri for uri, pem in rows if not pem]
        assert not keyless, (
            f"cached remote actors with an EMPTY public key: {keyless}. A refused "
            "or malformed actor response was stored as if it were an actor."
        )

    @pytest.mark.feature("fediverse")
    def test_post_is_pushed_to_follower_then_chased_by_delete(
        self, ap_nests, fake_follower
    ):
        """A local post reaches an accepted follower as a signed `Create`, and
        deleting it chases that copy with a signed `Delete`.

        This is the produce direction end-to-end (`activitypub.md` §§ The
        produce direction / Post deletion): `ingest_post_core` → the push →
        `ap_delivery_queue` → the sync worker's signed POST → a remote inbox,
        and the same chain for the `Delete` leg on the pushed-witness rule.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        pubkey_pem = self._account_public_key(nest_a, username)
        inbox_path = f"/users/{fake_follower.username}/inbox"

        # Become an accepted follower — only accepted follows are fanned out to.
        self._follow(nest_a, fake_follower, username, actor_url)
        fake_follower.wait_for("Accept")

        # ── The Create push ──
        body_text = "hello fediverse, from an e2e nest"
        post_id = ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), body_text),
        )

        entry = fake_follower.wait_for("Create")
        create = entry["activity"]
        note_url = f"{actor_url}/notes/{post_id}"

        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        assert create["actor"] == actor_url
        note = create["object"]
        assert note["id"] == note_url, "note id must be push/pull-symmetric"
        assert body_text in note["content"]
        # default_visibility is `public` on a fresh account: Public in `to`,
        # the followers collection in `cc`.
        assert "https://www.w3.org/ns/activitystreams#Public" in create["to"]
        assert f"{actor_url}/followers" in create["cc"]

        # ── The Delete leg ──
        reply = ws_api.delete_post(
            nest_a["port"],
            user,
            sign_and_encode_tombstone(
                user["signing_key"], post_id, int(time.time() * 1_000_000)
            ),
        )
        assert reply["deleted"] is True, f"post was not deleted: {reply}"

        entry = fake_follower.wait_for("Delete")
        delete = entry["activity"]
        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        assert delete["actor"] == actor_url
        assert delete["object"] == note_url, "the Delete must name the pushed note"

    @pytest.mark.feature("fediverse")
    @pytest.mark.parametrize("kind", ["Reply", "Quote"])
    def test_a_reply_or_quote_of_an_ingested_note_reaches_its_author(
        self, ap_nests, fake_follower, kind
    ):
        """A reply to, or quote of, a note the follower sent us is the user's
        own signed post, derived into the fediverse form and delivered to the
        note's author (`activitypub.md` § Reply and quote).

        The whole chain through the real worker: the follower's `Create{Note}`
        is ingested and mapped → `fauna.posts.create` of a signed post whose
        `Reference` names that note's local id → the push resolves the target
        through `ap_post_map` + the cached actor → a signed `Create` lands on
        the follower's inbox carrying `inReplyTo` + a Mention + the author in
        `to` (a reply), or the three quote spellings + the FEP-e232 `Link` +
        the `RE:` line (a quote).
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        pubkey_pem = self._account_public_key(nest_a, username)
        self._follow(nest_a, fake_follower, username, actor_url)
        fake_follower.wait_for("Accept")

        note_id = f"{fake_follower.actor_uri}/statuses/answer-me"
        code = fake_follower.post_signed(
            f"{nest_a['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Create",
                "id": f"{note_id}/activity",
                "actor": fake_follower.actor_uri,
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
                "object": {
                    "type": "Note",
                    "id": note_id,
                    "attributedTo": fake_follower.actor_uri,
                    "content": "<p>what do you think?</p>",
                    "published": "2026-09-28T08:00:00Z",
                    "to": ["https://www.w3.org/ns/activitystreams#Public"],
                },
            },
        )
        assert code == 202, f"inbox did not accept the Create: HTTP {code}"
        row = ap_post_map_row(nest_a, note_id)
        assert row is not None, "the follower's note was never ingested"

        body_text = f"my {kind.lower()} from an e2e nest"
        ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"],
                int(time.time() * 1_000_000),
                body_text,
                references=[post_reference(kind, row["fauna_post_id"])],
            ),
        )

        entry = fake_follower.wait_for("Create")
        assert_valid_http_signature(
            entry, pubkey_pem, f"/users/{fake_follower.username}/inbox"
        )
        note = entry["activity"]["object"]
        assert body_text in note["content"]
        if kind == "Reply":
            assert note["inReplyTo"] == note_id
            assert fake_follower.actor_uri in note["to"], note["to"]
            mentions = [t for t in note.get("tag", []) if t.get("type") == "Mention"]
            assert [m["href"] for m in mentions] == [fake_follower.actor_uri], note
        else:
            for key in ("quote", "quoteUri", "_misskey_quote"):
                assert note.get(key) == note_id, (key, note)
            links = [t for t in note.get("tag", []) if t.get("type") == "Link"]
            assert [link["href"] for link in links] == [note_id], note
            assert f"RE: <a href=\"{note_id}\">" in note["content"], note["content"]
            assert "inReplyTo" not in note

    @pytest.mark.feature("fediverse")
    def test_no_push_to_a_follower_that_never_got_accepted(
        self, ap_nests, fake_follower
    ):
        """A post is NOT pushed to a follower whose Follow was never accepted.

        With auto-accept off the follow stays `pending`, and the fan-out skips
        it — the outbound half of "delivering is the contract of accepting the
        Follow". Guards against a regression that fans out to every follow row.

        Asserted without waiting out a clock (`e2e-conventions.md` § convention
        14), on two positive signals: the fan-out counter moving proves the
        post was fanned out at all, and a second follower — approved by hand,
        so it is in the fan-out — receiving the `Create` proves the push ran to
        completion (it enqueues every follower's job in one pass before the
        worker is nudged). Only then is the pending follower's queue read.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)

        ws_api.bridge_set_settings(
            nest_a["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": False}
        )

        self._follow(nest_a, fake_follower, username, actor_url)

        with FakeFollower(find_free_port(), username="carol") as approved:
            self._follow(nest_a, approved, username, actor_url)
            ws_api.bridge_resolve_follow_request(
                nest_a["port"], user, AP_BRIDGE_ID, approved.actor_uri, True
            )
            approved.wait_for("Accept")

            body_text = "not for pending followers"
            before = fanouts_initiated(nest_a)
            ws_api.create_post(
                nest_a["port"],
                user,
                sign_and_encode_post(
                    user["signing_key"], int(time.time() * 1_000_000), body_text
                ),
            )
            assert fanouts_initiated(nest_a) == before + 1, (
                "the post never fanned out, so the absence below would be vacuous"
            )
            entry = approved.wait_for("Create")
            assert body_text in entry["activity"]["object"]["content"]

        assert delivery_jobs_for(nest_a, fake_follower.inbox_url) == [], (
            "the nest queued an activity for a follower it never accepted"
        )
        assert fake_follower.received == [], (
            "a pending follower received "
            f"{[e['activity'].get('type') for e in fake_follower.received]}"
        )

    def _held_back_follow(self, nest, follower):
        """An account with *accept follows by itself* off, and a signed Follow
        from `follower` resting on it. Returns `(user, username, actor_url)`.
        """
        user = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest, user)
        ws_api.bridge_set_settings(
            nest["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": False}
        )
        self._follow(nest, follower, username, actor_url)
        return user, username, actor_url

    @pytest.mark.feature("fediverse")
    def test_a_held_back_follow_is_listed_as_a_request(self, ap_nests, fake_follower):
        """With the setting off, a `Follow` is a follow request the account's
        owner can see: `fauna.bridges.list_follow_requests` lists it, named by
        the requester's actor URI, display name and `@user@host` address — and
        the actor document tells peers the account approves followers by hand.

        Flow: signed `Follow` → `handle_follow` records the pending
        `ap_follows` row → the list kind → the provider reads pending inbound
        rows beside `ap_remote_actors` (`activitypub.md` § Follow requests).
        """
        nest_a = ap_nests["a"]
        user, username, _actor_url = self._held_back_follow(nest_a, fake_follower)

        requests = ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID)
        assert [r["id"] for r in requests] == [fake_follower.actor_uri], requests
        request = requests[0]
        assert request["name"] == fake_follower.display_name
        assert request["extra"] == {
            "handle": f"@{fake_follower.username}@127.0.0.1"
        }, request
        assert isinstance(request["requested_at"], int)

        # Nothing was answered: the request waits for the account's decision.
        assert fake_follower.received == [], [
            e["activity"].get("type") for e in fake_follower.received
        ]

        status, actor_doc = api_get(
            nest_a["url"], f"/ap/users/{username}", accept="application/activity+json"
        )
        assert status == 200
        assert actor_doc["manuallyApprovesFollowers"] is True, (
            "an account holding follows back must advertise manual approval"
        )

        # A stranger's account sees none of it: the list is self-scoped.
        other = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        enable_ap(nest_a, other)
        assert ws_api.bridge_list_follow_requests(nest_a["port"], other, AP_BRIDGE_ID) == []

    @pytest.mark.feature("fediverse")
    def test_approving_a_request_sends_accept_and_later_posts_reach_it(
        self, ap_nests, fake_follower
    ):
        """Approving a held-back follow lands a signed `Accept` naming the
        requester's own `Follow` on its inbox, and from then on the follower
        is in the post fan-out.

        Flow: `fauna.bridges.resolve_follow_request {approve: true}` → the one
        accept function the auto arm also runs → `ap_delivery_queue` → the
        worker's signed POST; then `fauna.posts.create` → the push → a signed
        `Create` on the same inbox.
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._held_back_follow(nest_a, fake_follower)
        pubkey_pem = self._account_public_key(nest_a, username)
        inbox_path = f"/users/{fake_follower.username}/inbox"

        reply = ws_api.bridge_resolve_follow_request(
            nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri, True
        )
        assert reply["ok"] is True

        entry = fake_follower.wait_for("Accept")
        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        accept = entry["activity"]
        assert accept["actor"] == actor_url
        assert accept["object"]["type"] == "Follow"
        assert accept["object"]["id"] == f"{fake_follower.actor_uri}/activities/follow-1"
        assert accept["object"]["actor"] == fake_follower.actor_uri

        assert ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID) == []
        # Answering it again (another device) succeeds and sends nothing new.
        again = ws_api.bridge_resolve_follow_request(
            nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri, True
        )
        assert again["ok"] is True

        body_text = "for the follower I approved"
        ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), body_text),
        )
        entry = fake_follower.wait_for("Create")
        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        assert body_text in entry["activity"]["object"]["content"]
        accepts = [e for e in fake_follower.received if e["activity"].get("type") == "Accept"]
        assert len(accepts) == 1, "the repeated approval must not send a second Accept"

    @pytest.mark.feature("fediverse")
    def test_refusing_a_request_sends_reject_and_later_posts_do_not_reach_it(
        self, ap_nests, fake_follower
    ):
        """Refusing a held-back follow lands a signed `Reject` naming the
        requester's own `Follow` on its inbox, removes the request, and a later
        post is not delivered to it.

        The "does not reach" half is asserted without waiting out a clock
        (`e2e-conventions.md` § convention 14): a second follower is approved on
        the same account, so the post's fan-out has a delivery whose ARRIVAL
        proves the push ran to completion — and the push enqueues every
        follower's job in one pass before the worker is nudged, so by the time
        the approved follower holds the `Create`, a job for the refused one
        would already sit in the queue. The queue is then read directly.
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._held_back_follow(nest_a, fake_follower)
        pubkey_pem = self._account_public_key(nest_a, username)

        with FakeFollower(find_free_port(), username="carol") as approved:
            self._follow(nest_a, approved, username, actor_url)
            listed = ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID)
            assert sorted(r["id"] for r in listed) == sorted(
                [fake_follower.actor_uri, approved.actor_uri]
            )

            reply = ws_api.bridge_resolve_follow_request(
                nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri, False
            )
            assert reply["ok"] is True

            entry = fake_follower.wait_for("Reject")
            assert_valid_http_signature(
                entry, pubkey_pem, f"/users/{fake_follower.username}/inbox"
            )
            reject = entry["activity"]
            assert reject["actor"] == actor_url
            assert reject["object"]["type"] == "Follow"
            assert reject["object"]["id"] == f"{fake_follower.actor_uri}/activities/follow-1"
            assert reject["object"]["actor"] == fake_follower.actor_uri

            remaining = ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID)
            assert [r["id"] for r in remaining] == [approved.actor_uri], (
                "the refused request is gone; the other still waits"
            )

            ws_api.bridge_resolve_follow_request(
                nest_a["port"], user, AP_BRIDGE_ID, approved.actor_uri, True
            )
            approved.wait_for("Accept")

            body_text = "not for the follower I refused"
            ws_api.create_post(
                nest_a["port"],
                user,
                sign_and_encode_post(
                    user["signing_key"], int(time.time() * 1_000_000), body_text
                ),
            )
            entry = approved.wait_for("Create")
            assert body_text in entry["activity"]["object"]["content"]

        # The fan-out is complete (its other delivery arrived). No job for the
        # refused requester's inbox exists in any state, and none arrived.
        queued = delivery_jobs_for(nest_a, fake_follower.inbox_url)
        assert [json.loads(a)["type"] for a in queued] == ["Reject"], (
            "the only activity ever queued for a refused requester is its Reject"
        )
        assert [e["activity"].get("type") for e in fake_follower.received] == ["Reject"]

    @pytest.mark.feature("fediverse")
    def test_turning_auto_accept_back_on_accepts_the_waiting_requests(
        self, ap_nests, fake_follower
    ):
        """Turning *accept follows by itself* back on answers every request
        that arrived while it was off: each requester gets a signed `Accept`,
        the list empties, and a later post reaches them.

        Flow: `fauna.bridges.set_settings {auto_accept_follows: true}` → the
        provider's `update_settings` → the one accept function, once per
        pending inbound row → `ap_delivery_queue` → the worker.
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._held_back_follow(nest_a, fake_follower)
        pubkey_pem = self._account_public_key(nest_a, username)

        with FakeFollower(find_free_port(), username="carol") as second:
            self._follow(nest_a, second, username, actor_url)
            waiting = ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID)
            assert len(waiting) == 2, waiting

            ws_api.bridge_set_settings(
                nest_a["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": True}
            )

            for follower in (fake_follower, second):
                entry = follower.wait_for("Accept")
                assert_valid_http_signature(
                    entry, pubkey_pem, f"/users/{follower.username}/inbox"
                )
                assert entry["activity"]["actor"] == actor_url
                assert entry["activity"]["object"]["actor"] == follower.actor_uri
            assert ws_api.bridge_list_follow_requests(nest_a["port"], user, AP_BRIDGE_ID) == []

            body_text = "for everyone who was waiting"
            ws_api.create_post(
                nest_a["port"],
                user,
                sign_and_encode_post(
                    user["signing_key"], int(time.time() * 1_000_000), body_text
                ),
            )
            for follower in (fake_follower, second):
                entry = follower.wait_for("Create")
                assert body_text in entry["activity"]["object"]["content"]

    def _accepted_follower(self, nest, follower):
        """A fresh federated account `follower` follows, accepted by itself.
        Returns `(user, username, actor_url)`.
        """
        user = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest, user)
        self._follow(nest, follower, username, actor_url)
        follower.wait_for("Accept")
        return user, username, actor_url

    @pytest.mark.feature("fediverse")
    def test_default_visibility_decides_the_addressing(self, ap_nests, fake_follower):
        """Each `default_visibility` addresses a pushed post its own way
        (`activitypub.md` § The produce direction): `public` →
        `to=[Public] cc=[followers]`, `unlisted` → `to=[followers] cc=[Public]`,
        `followers_only` → `to=[followers]` with Public nowhere.

        Flow: `fauna.bridges.set_settings` → `ap_accounts.default_visibility`
        → the push's `fauna_post_to_ap_note` addressing → the delivered
        `Create` and its Note.
        """
        nest_a = ap_nests["a"]
        user, _username, actor_url = self._accepted_follower(nest_a, fake_follower)
        followers = f"{actor_url}/followers"
        expected = {
            "public": ([AS_PUBLIC], [followers]),
            "unlisted": ([followers], [AS_PUBLIC]),
            "followers_only": ([followers], []),
        }

        for visibility, (to, cc) in expected.items():
            ws_api.bridge_set_settings(
                nest_a["port"], user, AP_BRIDGE_ID, {"default_visibility": visibility}
            )
            body_text = f"addressed as {visibility}"
            ws_api.create_post(
                nest_a["port"],
                user,
                sign_and_encode_post(
                    user["signing_key"], int(time.time() * 1_000_000), body_text
                ),
            )
            entry = fake_follower.wait_for(
                "Create", where=lambda a, t=body_text: t in a["object"]["content"]
            )
            create = entry["activity"]
            for doc in (create, create["object"]):
                assert doc["to"] == to, (visibility, doc["to"])
                assert doc.get("cc", []) == cc, (visibility, doc.get("cc"))

    @pytest.mark.feature("fediverse")
    def test_a_gated_post_is_neither_pushed_nor_listed(self, ap_nests, fake_follower):
        """A gated (sold) post never reaches the fediverse (`activitypub.md`
        § The produce direction): no job for it is queued for the follower, the
        published history neither lists nor counts it, and its note id does
        not resolve — while a public post beside it does all three.

        The push half is asserted on a positive signal, not a clock: the gated
        post's fan-out is started a whole RPC round-trip before the public
        one's, so by the time the public `Create` has been DELIVERED, a job for
        the gated post would already sit in the queue.
        """
        import fauna_ffi
        from clients.ws_rpc_admin_client import WsRpcAdminClient
        from helpers.blob_upload import SEALED_MIME, upload_blob

        nest_a = ap_nests["a"]
        user, username, actor_url = self._accepted_follower(nest_a, fake_follower)
        ws_api.bridge_set_settings(nest_a["port"], user, AP_BRIDGE_ID, {"backfill": True})

        secret = bytes(user["signing_key"])
        with WsRpcAdminClient(
            nest_a["url"], actor_id=user["actor_id_bytes"], signing_key=secret
        ) as ws:
            ws.call(
                "fauna.subscriptions.tiers.create",
                {
                    "name": "gold",
                    "rank": 2,
                    "description": None,
                    "price_hint": "5 EUR / month",
                    "payment_url": "https://pay.example/gold",
                    "auto_approve": False,
                    "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                        secret, "gold", bytes(range(32))
                    ),
                },
            )
        gated_bytes, sealed_blob = fauna_ffi.build_gated_post(
            secret,
            "Paywalled: the teaser",
            "Paywalled: the teaser\n\nThe part you pay for.",
            "gold",
            2,
            b"\x11" * 32,
            bytes(range(32)),
        )
        upload_blob(
            nest_a["port"],
            user["token"],
            sealed_blob,
            audience_class="PeriodRestrictedPost",
            mime=SEALED_MIME,
        )
        gated_id = ws_api.create_post(nest_a["port"], user, gated_bytes)

        public_text = "free for everyone"
        public_id = ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"], int(time.time() * 1_000_000), public_text
            ),
        )
        fake_follower.wait_for("Create")

        queued = [json.loads(a) for a in delivery_jobs_for(nest_a, fake_follower.inbox_url)]
        creates = [a for a in queued if a["type"] == "Create"]
        assert [a["object"]["id"] for a in creates] == [f"{actor_url}/notes/{public_id}"], (
            f"only the public post may be queued: {[a['object']['id'] for a in creates]}"
        )
        assert all(gated_id not in json.dumps(a) for a in queued)

        status, root = api_get(
            nest_a["url"], f"/ap/users/{username}/outbox", accept="application/activity+json"
        )
        assert root["totalItems"] == 1, f"the gated post is counted: {root}"
        _, page = api_get(
            nest_a["url"],
            f"/ap/users/{username}/outbox?page=0",
            accept="application/activity+json",
        )
        assert [i["object"]["id"] for i in page["orderedItems"]] == [
            f"{actor_url}/notes/{public_id}"
        ]
        status, _ = api_get(
            nest_a["url"],
            f"/ap/users/{username}/notes/{gated_id}",
            accept="application/activity+json",
        )
        assert status == 404, "the gated post's note id must not resolve"

    @pytest.mark.feature("fediverse")
    def test_photos_arrive_as_attachments(self, ap_nests, fake_follower):
        """A photo post's pushed Note carries the photo as a `Document`
        attachment whose URL serves the photo's bytes (`activitypub.md`
        § Reply and quote — the translation the push shares).

        Flow: `POST /api/v1/blob` (a public image) → a signed `TextWithMedia`
        post naming it → the push → the delivered Note's `attachment` → its
        `url` dereferenced the way the follower's server fetches it.
        """
        from helpers.blob_upload import upload_blob
        from tests.api.bare import media_item

        nest_a = ap_nests["a"]
        user, _username, _actor_url = self._accepted_follower(nest_a, fake_follower)
        photo = tiny_png()
        blob_hex = upload_blob(
            nest_a["port"], user["token"], photo, audience_class="PublicPost", mime="image/png"
        )
        ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"],
                int(time.time() * 1_000_000),
                "a photo for my followers",
                media_items=[media_item(blob_hex, "image/png", len(photo))],
            ),
        )

        note = fake_follower.wait_for("Create")["activity"]["object"]
        attachments = note.get("attachment", [])
        assert len(attachments) == 1, f"the photo did not attach: {note}"
        attachment = attachments[0]
        assert attachment["type"] == "Document"
        assert attachment["mediaType"] == "image/png"
        assert fetch_served(nest_a, attachment["url"]) == photo, (
            "the attachment URL must serve the photo's bytes"
        )

    @pytest.mark.feature("fediverse")
    def test_liking_or_boosting_a_fediverse_post_reaches_its_author(
        self, ap_nests, fake_follower
    ):
        """`fauna.posts.interact` on an ingested note sends a signed `Like`
        (like) or `Announce` (repost) naming the note to its author's inbox
        (`activitypub.md` § Architecture → Outbound interactions).
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        pubkey_pem = self._account_public_key(nest_a, username)
        note_id = deliver_note(nest_a, fake_follower, username, actor_url, "boost me")
        post_id = ap_post_map_row(nest_a, note_id)["fauna_post_id"]

        for action, verb in (("like", "Like"), ("repost", "Announce")):
            ws_api.interact(nest_a["port"], user, post_id, action)
            entry = fake_follower.wait_for(verb, where=lambda a: a.get("object") == note_id)
            assert_valid_http_signature(
                entry, pubkey_pem, f"/users/{fake_follower.username}/inbox"
            )
            assert entry["activity"]["actor"] == actor_url

    @pytest.mark.feature("fediverse")
    def test_liking_or_boosting_is_refused_while_federation_is_off(
        self, ap_nests, fake_follower
    ):
        """With federation switched off, a like or a boost of a fediverse post
        is refused with `fauna.activitypub.switched_off` and queues nothing for
        its author — the actor it would be signed by answers 404
        (`activitypub.md` § Architecture → Outbound interactions).

        Flow: `bridge_set_settings {enabled: false}` → `fauna.posts.interact`
        → `interact.rs` refuses before the verb `match` → the posts mapper
        carries the code through → no `ap_delivery_queue` row for the author's
        inbox. Turning federation back on and liking again queues a `Like` on
        that same inbox, so the negative is not vacuous.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        note_id = deliver_note(nest_a, fake_follower, username, actor_url, "do not boost me")
        post_id = ap_post_map_row(nest_a, note_id)["fauna_post_id"]

        def interactions():
            return [
                a
                for a in (json.loads(j) for j in delivery_jobs_for(nest_a, fake_follower.inbox_url))
                if a.get("type") in ("Like", "Announce") and a.get("object") == note_id
            ]

        ws_api.bridge_set_settings(nest_a["port"], user, AP_BRIDGE_ID, {"enabled": False})
        for action in ("like", "repost"):
            with pytest.raises(RpcCallError) as exc:
                ws_api.interact(nest_a["port"], user, post_id, action)
            assert exc.value.code == "fauna.activitypub.switched_off", (action, exc.value.code)
        assert interactions() == [], "a refused like or boost was queued"

        ws_api.bridge_set_settings(nest_a["port"], user, AP_BRIDGE_ID, {"enabled": True})
        ws_api.interact(nest_a["port"], user, post_id, "like")
        assert [a["type"] for a in interactions()] == ["Like"]

    @pytest.mark.feature("fediverse")
    def test_unfollowing_tells_the_other_server(self, ap_nests, fake_follower):
        """Removing a fediverse follow delivers a signed `Undo{Follow}` naming
        the unfollowed actor to its inbox, and the follow leaves the list.

        Flow: `fauna.bridges.add_follow` → a signed `Follow` on the remote
        inbox; `fauna.bridges.remove_follow` → the outbound `ap_follows` row
        deleted → `Undo{Follow}` enqueued and delivered.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        pubkey_pem = self._account_public_key(nest_a, username)
        inbox_path = f"/users/{fake_follower.username}/inbox"

        ws_api.bridge_add_follow(nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri)
        follow = fake_follower.wait_for("Follow")["activity"]
        assert follow["object"] == fake_follower.actor_uri
        assert [f["id"] for f in ws_api.bridge_list_follows(
            nest_a["port"], user, AP_BRIDGE_ID
        )] == [fake_follower.actor_uri]

        ws_api.bridge_remove_follow(nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri)
        entry = fake_follower.wait_for("Undo")
        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        undo = entry["activity"]
        assert undo["actor"] == actor_url
        assert undo["object"]["type"] == "Follow"
        assert undo["object"]["actor"] == actor_url
        assert undo["object"]["object"] == fake_follower.actor_uri
        assert ws_api.bridge_list_follows(nest_a["port"], user, AP_BRIDGE_ID) == []

    @pytest.mark.feature("fediverse")
    def test_a_delete_after_federation_is_off_still_reaches_the_followers(
        self, ap_nests, fake_follower
    ):
        """A post pushed while federation was on is chased by its `Delete` even
        after federation is turned off (`activitypub.md` § Post deletion: the
        leg is deliberately not gated on `enabled`).
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._accepted_follower(nest_a, fake_follower)
        pubkey_pem = self._account_public_key(nest_a, username)
        post_id = ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"], int(time.time() * 1_000_000), "pushed while federated"
            ),
        )
        fake_follower.wait_for("Create")

        ws_api.bridge_set_settings(nest_a["port"], user, AP_BRIDGE_ID, {"enabled": False})
        status, _ = api_get(
            nest_a["url"], f"/ap/users/{username}", accept="application/activity+json"
        )
        assert status == 404, "precondition: federation is off"

        reply = ws_api.delete_post(
            nest_a["port"],
            user,
            sign_and_encode_tombstone(
                user["signing_key"], post_id, int(time.time() * 1_000_000)
            ),
        )
        assert reply["deleted"] is True
        entry = fake_follower.wait_for("Delete")
        assert_valid_http_signature(
            entry, pubkey_pem, f"/users/{fake_follower.username}/inbox"
        )
        assert entry["activity"]["actor"] == actor_url
        assert entry["activity"]["object"] == f"{actor_url}/notes/{post_id}"

    def test_forwarded_post_is_pushed_to_follower(self, ap_nests, fake_follower):
        """A forwarded post fans out to the author's accepted followers like a
        local create, and re-forwarding the same bytes is idempotent.

        The paired private→public topology (`activitypub.md` § The produce
        direction, paired deployments): the author's posts originate on their
        private nest and reach the public nest as
        ``fauna.federation.post.forward`` — the fan-out mandate is the author's
        own AP enablement *on the publishing nest*, not the post's arrival
        path, so the forwarded receive must run the same create-side fan-out.
        Idempotency guard: a private outbox retries until acked, so a
        re-delivered forward (the stored-post UNIQUE arm) must not enqueue a
        second `Create`.

        nest_b plays the dialing private nest — under the default
        ``SubmissionPolicy::Open`` the forward handler verifies the post's
        author envelope but does not gate on pairing.
        """
        nest_a = ap_nests["a"]
        nest_b = ap_nests["b"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        pubkey_pem = self._account_public_key(nest_a, username)
        inbox_path = f"/users/{fake_follower.username}/inbox"

        # Become an accepted follower first, as in the local-create twin.
        self._follow(nest_a, fake_follower, username, actor_url)
        fake_follower.wait_for("Accept")

        # Author-sign a post and relay it over the federation channel — the
        # exact wire a private nest's outbox worker emits (verbatim canonical
        # `EmbedAsBytes`; both nests derive the same content-addressed id).
        body_text = "forwarded from my home nest, for the fediverse"
        post_bytes_hex, post_envelope_hex = sign_post_envelope(
            user["signing_key"], body_text
        )
        post_bytes = bytes.fromhex(post_bytes_hex)
        envelope_raw = bytes.fromhex(post_envelope_hex)
        expected_id = forwarded_post_id(post_bytes, envelope_raw)
        forward_req = {
            "post_bytes": post_bytes,
            "post_envelope": envelope_raw,
        }

        before_first = fanouts_initiated(nest_a)
        with FederationChannelClient(initiator=nest_b, target=nest_a) as fed:
            fed.call("fauna.federation.post.forward", forward_req)

            entry = fake_follower.wait_for("Create")

            # A re-delivered forward (outbox retry) hits the already-stored arm
            # and must not fan out again. `post.forward` decides this
            # synchronously — `newly_stored = !post_exists(post_id)` gates the
            # `spawn_post_bridge_fanout` call, and both run before the handler
            # replies — so the RPC's own return is the barrier, and the counter
            # read straight after it is a complete answer.
            after_first = fanouts_initiated(nest_a)
            fed.call("fauna.federation.post.forward", forward_req)
            after_second = fanouts_initiated(nest_a)

        # Non-vacuity first: the counter must have moved for the ORIGINAL
        # forward, otherwise "it did not move for the duplicate" is trivially
        # true and this arm proves nothing.
        assert after_first == before_first + 1, (
            "the first forward must initiate exactly one fan-out — the counter "
            f"went {before_first} → {after_first}; without that the idempotency "
            "assertion below would pass vacuously"
        )
        assert after_second == after_first, (
            "re-forwarded post fanned out again: the fan-out counter went "
            f"{after_first} → {after_second} across the re-delivery, so the "
            "already-stored arm did not suppress the second Create"
        )

        create = entry["activity"]
        assert_valid_http_signature(entry, pubkey_pem, inbox_path)
        assert create["actor"] == actor_url
        note = create["object"]
        assert note["id"] == f"{actor_url}/notes/{expected_id}", (
            "note id must be push/pull-symmetric with the forwarded post id"
        )
        assert body_text in note["content"]
        assert "https://www.w3.org/ns/activitystreams#Public" in create["to"]
        assert f"{actor_url}/followers" in create["cc"]

        # The delivery-side twin of the counter assertion above, now free of any
        # wait. It is deliberately kept but is NOT the load-bearing half: a
        # fan-out is asynchronous (`spawn_post_bridge_fanout` spawns; the push
        # then enqueues onto `ap_delivery_queue` and nudges the worker), so a
        # duplicate that is merely late reads exactly like a duplicate that
        # never existed — which is why the 8 s grace window this replaced could
        # not decide the question. The counter can, because it is bumped at the
        # decision point, before the RPC that made it returned.
        creates = [
            e for e in fake_follower.received if e["activity"].get("type") == "Create"
        ]
        assert len(creates) == 1, (
            f"re-forwarded post fanned out again: {len(creates)} Creates delivered"
        )


class TestActivityPubInboundIngest:
    """The consume direction: what a real peer sends must actually parse.

    The opt-in interop harness (`tests/platform/fediverse/`) is where peer
    conformance is *discovered*; a default `--tier 3` run never boots a
    third-party server, so this is where a relapse is *caught*. Both spellings
    below are the same AS2 document, and a peer picks between them freely.
    """

    def _deliver_note(self, nest, follower, username, note):
        """Deliver a signed `Create{Note}` to the user's own inbox.

        The per-user inbox, so the relationship gate's *addressed* arm admits
        the note on the inbox target alone — this test is about parsing, and a
        follow-graph precondition would give it a second way to fail.
        """
        code = follower.post_signed(
            f"{nest['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Create",
                "id": f"{note['id']}/activity",
                "actor": follower.actor_uri,
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
                "object": note,
            },
        )
        assert code == 202, f"inbox did not accept the Create: HTTP {code}"

    def test_a_note_is_ingested_whether_or_not_its_properties_are_arrays(
        self, ap_nests, fake_follower
    ):
        """A single-valued `to`/`cc`/`tag` must ingest exactly like an array.

        JSON-LD compaction drops the array wrapper around a one-element
        property, so `"tag": {…}` and `"tag": [{…}]` are the SAME document —
        and serde fails the *whole* Note on one mis-shaped field. Real
        GoToSocial 0.22.1 sends the compacted form for a reply that mentions one
        account, which made every inbound reply from that peer a `400` with the
        opaque `invalid type: map, expected a sequence`; Mastodon always emits
        arrays and never exposed it. Asserting BOTH forms is the point: a pin on
        the array form alone is what let this survive.
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest_a, user)
        mention = {
            "type": "Mention",
            "href": actor_url,
            "name": f"@{username}@{nest_a['domain']}",
        }

        compacted = {
            "type": "Note",
            "id": f"{fake_follower.actor_uri}/statuses/compacted",
            "attributedTo": fake_follower.actor_uri,
            "content": "<p>compacted properties</p>",
            "published": "2026-07-22T12:00:00Z",
            # Single values, NOT arrays — the legal compacted spelling.
            "to": "https://www.w3.org/ns/activitystreams#Public",
            "cc": actor_url,
            "tag": mention,
        }
        expanded = {
            **compacted,
            "id": f"{fake_follower.actor_uri}/statuses/expanded",
            "content": "<p>array properties</p>",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "cc": [actor_url],
            "tag": [mention],
        }

        for note in (compacted, expanded):
            self._deliver_note(nest_a, fake_follower, username, note)

        for note in (compacted, expanded):
            row = ap_post_map_row(nest_a, note["id"])
            assert row is not None, (
                f"the note {note['id']!r} was accepted (202) but never stored — "
                f"the inbound Create was dropped, and a 202 says nothing about "
                f"that because a rejected activity answers the PEER, not us. "
                f"Read the nest log for the reason."
            )
            assert content_row_exists(nest_a, row["fauna_post_id"]), (
                f"{note['id']!r} mapped to {row['fauna_post_id']} but wrote no "
                f"content projection row — mapped but unreadable"
            )

    @pytest.mark.feature("feed-read")
    def test_an_ingested_notes_author_carries_the_bridged_face(
        self, ap_nests, fake_follower
    ):
        """The bridged-author face, end to end over one bridge (bridges.md
        § Unified feed ingestion → *Bridged authors*).

        The inbox's actor fetch caches the peer's document — `preferredUsername`,
        `name`, `icon` — and that cache write projects the author's face under
        the synthetic id the note rests under. `fauna.feed.local.posts` then
        decorates the page after its query: the ingested note comes back with
        `author_display` = `{handle: @user@host, display_name, avatar_url}`, the
        avatar rewritten through the shared media proxy rather than the remote
        origin, while the user's own native post carries the nest's own
        projection of their handle — never a bridge-shaped one (the field's
        third filler, bridges.md § Unified feed ingestion → *Bridged authors*
        → *The local native author*).
        """
        nest_a = ap_nests["a"]
        user = create_actor_and_register(
            nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
        )
        username, _actor_url = enable_ap(nest_a, user)

        note = {
            "type": "Note",
            "id": f"{fake_follower.actor_uri}/statuses/faced",
            "attributedTo": fake_follower.actor_uri,
            "content": "<p>a note with a face</p>",
            "published": "2026-09-26T12:00:00Z",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        }
        self._deliver_note(nest_a, fake_follower, username, note)
        row = ap_post_map_row(nest_a, note["id"])
        assert row is not None, "the note was accepted (202) but never stored"

        # The user's own post: a native author, no face.
        ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"], int(time.time() * 1_000_000), "my own native post"
            ),
        )

        posts = ws_api.local_feed_posts(nest_a["port"], user, limit=50)
        by_id = {p["post_id"]: p for p in posts}
        assert row["fauna_post_id"] in by_id, (
            f"the ingested note {row['fauna_post_id']} is not on the local feed page: "
            f"{sorted(by_id)}"
        )
        bridged = by_id[row["fauna_post_id"]]
        assert bridged["source"] == "activitypub"
        face = bridged.get("author_display")
        assert face, (
            "the bridged row carries no author_display — the actor-cache write "
            "did not project the face, or the page was not decorated"
        )
        assert face["handle"] == f"@{fake_follower.username}@127.0.0.1"
        assert face["display_name"] == fake_follower.display_name
        assert face["avatar_url"] == (
            "/api/v1/media/proxy?url="
            + urllib.parse.quote(fake_follower.icon_url, safe="")
        ), "the avatar rides behind the shared media proxy, never the remote origin"

        native = [p for p in posts if p["author"] == user["actor_id_hex"]]
        assert native, "the user's own post is on the page"
        # A local native author is named from their `users` handle and the
        # display name of their latest signed profile, projected by the nest in
        # the same decoration pass (`feed_routes::NativeAuthorRows::faces`).
        # This user stored no profile, so the face is the handle alone; a
        # native avatar is a content address, never a URL.
        for p in native:
            face = p.get("author_display")
            assert face, "a local native author is named from their users row"
            assert face["handle"] == user["handle"], face
            assert not face.get("display_name"), face
            assert not face.get("avatar_url"), face

    # ── what comes in ──────────────────────────────────────────────────────
    #
    # The inbox processes an activity before it answers 202, so every read
    # below follows a request whose reply is the barrier — nothing waits on a
    # clock. A drop is still answered 202 (it answers the PEER), so each
    # absence is paired with a positive control delivered after it.

    def _create(self, peer, url, text, *, to, cc=(), extra=None):
        """`peer` POSTs a signed `Create{Note}` to `url`; returns the note id."""
        note_id = f"{peer.actor_uri}/statuses/{uuid.uuid4().hex[:10]}"
        note = {
            "type": "Note",
            "id": note_id,
            "attributedTo": peer.actor_uri,
            "content": f"<p>{text}</p>",
            "published": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "to": list(to),
            "cc": list(cc),
            **(extra or {}),
        }
        code = peer.post_signed(
            url,
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Create",
                "id": f"{note_id}/activity",
                "actor": peer.actor_uri,
                "to": list(to),
                "cc": list(cc),
                "object": note,
            },
        )
        assert code == 202, f"inbox did not accept the Create: HTTP {code}"
        return note_id

    def _user(self, nest):
        user = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"]
        )
        username, actor_url = enable_ap(nest, user)
        return user, username, actor_url

    @pytest.mark.feature("fediverse")
    def test_a_fediverse_reply_threads_under_your_post(self, ap_nests, fake_follower):
        """A remote `Note` whose `inReplyTo` names your pushed note is stored
        as a reply to your post, and your post's reply count moves
        (`activitypub.md` § Reply and quote).

        Flow: your post pushed → its note URL mapped → the peer's reply
        `Create` → `inbound_references` resolves `inReplyTo` through the map to
        a `Reference::Reply` → `record_inbound_reference_engagements` → the
        local feed's `reply_count`.
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._user(nest_a)
        code = fake_follower.post_signed(
            f"{nest_a['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Follow",
                "id": f"{fake_follower.actor_uri}/activities/follow-1",
                "actor": fake_follower.actor_uri,
                "object": actor_url,
            },
        )
        assert code == 202
        fake_follower.wait_for("Accept")
        post_id = ws_api.create_post(
            nest_a["port"],
            user,
            sign_and_encode_post(
                user["signing_key"], int(time.time() * 1_000_000), "what do you all think?"
            ),
        )
        note_url = fake_follower.wait_for("Create")["activity"]["object"]["id"]

        reply_id = self._create(
            fake_follower,
            f"{nest_a['url']}/ap/users/{username}/inbox",
            "I think it is grand",
            to=[AS_PUBLIC],
            cc=[actor_url],
            extra={"inReplyTo": note_url},
        )
        row = ap_post_map_row(nest_a, reply_id)
        assert row is not None, "the reply was never ingested"

        posts = {p["post_id"]: p for p in ws_api.local_feed_posts(nest_a["port"], user, limit=200)}
        assert posts[post_id]["reply_count"] == 1, posts[post_id]
        reply = posts[row["fauna_post_id"]]
        assert reply["is_reply"] is True
        assert bytes.fromhex(post_id) in ws_api.get_post(
            nest_a["port"], user, row["fauna_post_id"]
        ), "the stored reply must reference your post"

    @pytest.mark.feature("fediverse")
    def test_a_remote_delete_removes_the_post_from_your_feed(
        self, ap_nests, fake_follower
    ):
        """A `Delete` from a note's author withdraws the ingested post: it
        leaves the feed and the by-id read answers not-found
        (`activitypub.md` § Post deletion → inbound `Delete`).
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._user(nest_a)
        text = "a note I will take back"
        note_id = deliver_note(nest_a, fake_follower, username, actor_url, text)
        post_id = ap_post_map_row(nest_a, note_id)["fauna_post_id"]
        assert any(text in b for b in feed_bodies(nest_a, user)), "precondition"

        code = fake_follower.post_signed(
            f"{nest_a['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Delete",
                "id": f"{note_id}#delete",
                "actor": fake_follower.actor_uri,
                "object": note_id,
            },
        )
        assert code == 202
        assert not any(text in b for b in feed_bodies(nest_a, user)), (
            "a note its author deleted is still in the feed"
        )
        with pytest.raises(RpcCallError) as exc:
            ws_api.get_post(nest_a["port"], user, post_id)
        assert exc.value.code == "fauna.posts.not_found"

    @pytest.mark.feature("fediverse")
    def test_a_remote_edit_replaces_the_post_in_your_feed(self, ap_nests, fake_follower):
        """An `Update{Note}` from its author replaces the ingested post: the
        feed shows the edited text, and the superseded version is gone
        (`activitypub.md` § Architecture → `Update`: tombstone-old + store-new).
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._user(nest_a)
        note_id = deliver_note(nest_a, fake_follower, username, actor_url, "the first draft")

        code = fake_follower.post_signed(
            f"{nest_a['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Update",
                "id": f"{note_id}#update-1",
                "actor": fake_follower.actor_uri,
                "to": [AS_PUBLIC],
                "object": {
                    "type": "Note",
                    "id": note_id,
                    "attributedTo": fake_follower.actor_uri,
                    "content": "<p>the edited version</p>",
                    "published": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "to": [AS_PUBLIC],
                },
            },
        )
        assert code == 202
        bodies = feed_bodies(nest_a, user)
        assert sum("the edited version" in b for b in bodies) == 1, bodies
        assert not any("the first draft" in b for b in bodies), (
            "the superseded version is still in the feed beside the edit"
        )

    @pytest.mark.feature("fediverse")
    def test_a_post_that_was_not_public_never_enters_your_feed(
        self, ap_nests, fake_follower
    ):
        """A followers-only or a direct `Note` never enters the feed, even from
        an account you follow (`activitypub.md` § Architecture → the inbound
        audience gate). Three shapes: followers-only, direct to someone else
        (dropped), and direct to you — which goes to your conversation with the
        sender instead (built 2026-10-03), so it too is absent from the feed. A
        public note from the same sender, delivered last, is the control.
        """
        nest_a = ap_nests["a"]
        user, username, actor_url = self._user(nest_a)
        ws_api.bridge_add_follow(nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri)
        inbox = f"{nest_a['url']}/ap/users/{username}/inbox"
        sender_followers = f"{fake_follower.actor_uri}/followers"

        hidden = {
            "only for my followers": self._create(
                fake_follower, inbox, "only for my followers", to=[sender_followers]
            ),
            "just between me and dave": self._create(
                fake_follower,
                inbox,
                "just between me and dave",
                to=["http://127.0.0.1:1/users/dave"],
            ),
            "a private word for you": self._create(
                fake_follower, inbox, "a private word for you", to=[actor_url]
            ),
        }
        control = self._create(fake_follower, inbox, "this one is public", to=[AS_PUBLIC])
        assert ap_post_map_row(nest_a, control) is not None, "the control never entered"

        bodies = feed_bodies(nest_a, user)
        assert any("this one is public" in b for b in bodies)
        for text, note_id in hidden.items():
            assert not any(text in b for b in bodies), f"{text!r} entered the feed"
            assert ap_post_map_row(nest_a, note_id) is None, f"{text!r} became a post"

    @pytest.mark.feature("fediverse")
    def test_only_followed_or_addressed_posts_enter_your_feed(
        self, ap_nests, fake_follower
    ):
        """On the shared inbox a public `Note` enters only when a local account
        opted in (`activitypub.md` § Architecture → the inbound relationship
        gate): its author is followed, or it names the account. A stranger's
        public note naming nobody here is dropped.
        """
        nest_a = ap_nests["a"]
        user, _username, actor_url = self._user(nest_a)
        shared_inbox = f"{nest_a['url']}/ap/inbox"

        with FakeFollower(find_free_port(), username="erin") as stranger, FakeFollower(
            find_free_port(), username="frank"
        ) as addresser:
            ws_api.bridge_add_follow(
                nest_a["port"], user, AP_BRIDGE_ID, fake_follower.actor_uri
            )
            dropped = self._create(stranger, shared_inbox, "from a stranger", to=[AS_PUBLIC])
            followed = self._create(
                fake_follower, shared_inbox, "from someone I follow", to=[AS_PUBLIC]
            )
            addressed = self._create(
                addresser, shared_inbox, "said to you", to=[AS_PUBLIC], cc=[actor_url]
            )

        assert ap_post_map_row(nest_a, followed) is not None
        assert ap_post_map_row(nest_a, addressed) is not None
        assert ap_post_map_row(nest_a, dropped) is None, "a stranger's note was stored"
        bodies = feed_bodies(nest_a, user)
        assert any("from someone I follow" in b for b in bodies)
        assert any("said to you" in b for b in bodies)
        assert not any("from a stranger" in b for b in bodies)
