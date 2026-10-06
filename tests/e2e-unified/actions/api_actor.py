"""Lightweight headless actor for multi-user tests.

An ApiActor interacts with a nest without a UI driver. Content actions
(feeds, posts) go over the same WS-RPC surface production clients
use — the nest deleted the legacy ``/api/v1/{feeds,groups,group,posts}``
HTTP twins in the WS-RPC migration, so the actor drives the
``fauna.feed.*`` / ``fauna.posts.*``
kinds via :class:`clients.ws_rpc_admin_client.WsRpcAdminClient` (a
User-class caller, keyed by this actor's Ed25519 identity). Opaque BARE
payloads are produced by the ``fauna_ffi`` builders, matching native
apps.

``health`` is the one remaining plain-REST read (health residue); inbox reads
ride the WS-RPC ``fauna.inbox.fetch`` kind (the ``GET /api/v1/inbox`` drain was
deleted in the WS-RPC-everywhere rip).

Use this for the "other users" in cross-nest or multi-user tests where
only one user drives the UI.
"""

from __future__ import annotations

import json
import os
import urllib.request
import urllib.error


class ApiActor:
    """Headless actor: WS-RPC for content + inbox reads, REST for health."""

    def __init__(self, nest_url: str, token: str, actor_id_hex: str,
                 secret_bytes: bytes | None = None):
        self.nest_url = nest_url
        self.token = token
        self.actor_id = actor_id_hex
        self.secret_bytes = secret_bytes

    # --- Transports ---

    def _request(self, method: str, path: str, body=None, timeout: float = 10):
        url = f"{self.nest_url}{path}"
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            url, data=data, method=method,
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self.token}",
            },
        )
        resp = urllib.request.urlopen(req, timeout=timeout)
        return json.loads(resp.read())

    def _ws_call(self, kind: str, payload):
        """Issue one User-class WS-RPC call over a fresh connection.

        WsRpcAdminClient authenticates with the actor's signing key via the
        challenge/verify flow, so ``secret_bytes`` is required. One
        connection per call is wasteful but the headless-actor call volume
        is tiny and the synchronous context-manager shape keeps this simple.
        """
        if self.secret_bytes is None:
            raise RuntimeError(
                "ApiActor needs secret_bytes to make WS-RPC calls "
                "(construct it with the actor's signing key)"
            )
        from clients.ws_rpc_admin_client import WsRpcAdminClient
        client = WsRpcAdminClient(
            self.nest_url,
            actor_id=bytes.fromhex(self.actor_id),
            signing_key=self.secret_bytes,
        )
        with client:
            return client.call(kind, payload)

    # --- Feed ---

    def create_feed(self, name: str, rules: list | None = None) -> str:
        """Create a feed. Returns feed_id.

        ``rules`` rides the typed ``rules`` wire field (a list of
        externally-tagged ``FilterRule`` dicts); an empty rule set is a
        catch-all feed (every non-quarantined post matches).
        """
        reply = self._ws_call("fauna.feed.create", {
            "name": name,
            "rules": rules or [],
            "combination": "any",
        })
        return reply["feed_id"]

    def post_to_feed(self, feed_id: str, body: str, tags: list[str] | None = None) -> str:
        """Publish a post. Returns post_id.

        There is no feed-scoped post-create kind: ``fauna.posts.create``
        publishes the signed post and the nest's ingest pipeline indexes it
        into every feed whose rules match (so ``feed_id`` is unused — kept
        for call-site symmetry with the old REST helper).
        """
        from fauna_ffi import build_post
        post_bytes = build_post(self.secret_bytes, body, tags)
        reply = self._ws_call("fauna.posts.create", {"body": post_bytes})
        return reply.get("post_id", "")

    def get_feed_posts(self, feed_id: str) -> list:
        """Get posts from a feed."""
        reply = self._ws_call("fauna.feed.posts", {"feed_id": feed_id})
        return reply.get("posts", [])

    def like_post(self, post_id_hex: str) -> dict:
        """Like another actor's post (``fauna.posts.interact`` action ``like``).

        This is the production like path, counter and all, so it also fires the
        author's ``like`` notification through the real ``insert_notification``
        (``bins/fauna-nest/src/interact_routes.rs``) — which is what callers
        driving a notification journey actually want from it.

        Idempotent per (actor, post) nest-side: a repeat like by this actor is a
        counter no-op and rings no second doorbell.
        """
        return self._ws_call(
            "fauna.posts.interact", {"post_id": post_id_hex, "action": "like"}
        )

    # --- Inbox (WS-RPC) / Health (REST) ---

    def get_inbox(self) -> list:
        """Peek inbox items over ``fauna.inbox.fetch`` — the WS-RPC successor of
        the deleted ``GET /api/v1/inbox`` drain (a pure peek, no mark-on-read).
        Each item is ``{id, payload}`` (``payload`` is raw ``bytes``)."""
        reply = self._ws_call("fauna.inbox.fetch", {"limit": 0})
        return reply.get("items", [])

    def health(self) -> dict:
        """Check nest health."""
        return self._request("GET", "/api/v1/health")

    # --- Bridges ---

    def bridges_list(self) -> list:
        """`fauna.bridges.list` — each entry is a `BridgeStatus` dict (id,
        name, available, linked, identity, mode, settings, supports_follows,
        link_modes, error, extra). Used to verify a UI-driven bridge link/
        unlink server-side (bridges.md § State & data shape)."""
        reply = self._ws_call("fauna.bridges.list", {})
        return reply.get("bridges", [])

    # --- Contacts ---

    def accept_knock(self, peer_id_hex: str) -> dict:
        """Add `peer_id_hex` to this actor's contacts (`fauna.knocks.accept`).

        ``CacheDb::accept_contact`` upserts an ``accepted`` contact row
        *unconditionally* — no pre-existing knock is required (the knock-creation
        path is server-side-only, fired from inbound mail). So a multi-user test
        can seed a contact edge headlessly: this actor accepts the peer, and the
        peer then shows up in this actor's ``fauna.contacts.list`` / contacts UI
        (the wired path to another actor's profile by tap-through). ``peer_id`` is
        a plain hex *string* field (not an ``ActorId``), so the hex is sent
        as-is."""
        return self._ws_call("fauna.knocks.accept", {"peer_id": peer_id_hex})

    def confirm_contact(self, peer_id_hex: str) -> dict:
        """Promote an ``accepted`` contact to ``confirmed``
        (``fauna.contacts.confirm``); a no-op on any other edge. For seeding a
        confirmed row a UI test then reads — the confirm *gesture* is the app's."""
        return self._ws_call("fauna.contacts.confirm", {"peer_id": peer_id_hex})

    def block_knock(self, peer_id_hex: str) -> dict:
        """Upsert a ``blocked`` edge to ``peer_id_hex`` (``fauna.knocks.block``),
        no knock required — the ``accept_knock`` twin, for seeding a blocked row."""
        return self._ws_call("fauna.knocks.block", {"peer_id": peer_id_hex})

    # --- Profile ---

    def profile_get(self, actor_id_hex: str | None = None) -> dict:
        """Fetch a stored profile (`fauna.profile.get`); defaults to this actor.

        Returns ``{"body": bytes}`` — the signed ``EmbedAsBytes`` wire. The
        typed decode lives client-side (``fauna_client_profile::decode_profile``,
        a CBOR + Ed25519-envelope read), so tests assert the body is *present*
        (proof the publish landed + is readable) rather than re-implementing
        that decode in Python. ``actor_id`` is a plain hex *string* field (not an
        ``ActorId``), so the hex is sent as-is. Raises
        ``RpcCallError`` (``fauna.profile.not_found``) when no profile exists."""
        return self._ws_call(
            "fauna.profile.get",
            {"actor_id": actor_id_hex or self.actor_id},
        )

    # --- Subscriptions (subscriber-side) ---
    #
    # The subscriber half of the `fauna.subscriptions.*` flow, for multi-user
    # subscription tests where one actor drives the author UI and a headless
    # second actor subscribes / verifies key material. `author_id` is the raw
    # 32-byte actor id, sent as `bytes`: a `fauna_core::identity::ActorId` wire
    # field rides as a 32-byte CBOR byte string, like every fixed-width id. The
    # rule, and why the shape is frozen by signed payloads:
    # `docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
    # "Fixed-size byte arrays".

    def subscribe(self, author_id: bytes, tier: str) -> dict:
        """Request `tier` from `author_id` (`fauna.subscriptions.subscribe`).

        Returns the reply dict: ``{"outcome": "queued", "request_id": N}`` (the
        encrypted-mode path always enqueues) or ``{"outcome": "approved",
        "tier": ..., "expires_at": ...}`` (plaintext auto-approve / already
        subscribed)."""
        return self._ws_call("fauna.subscriptions.subscribe", {
            "author_id": bytes(author_id),
            "tier": tier,
        })

    def subscription_status(self, author_id: bytes) -> dict:
        """The caller's current subscription status for `author_id`
        (`fauna.subscriptions.status.get`): ``{"tier": "gold"|None,
        "expires_at": ..., "auto_approve": bool}``. `tier` is non-null once the
        author has approved the request."""
        return self._ws_call("fauna.subscriptions.status.get", {
            "author_id": bytes(author_id),
        })

    def subscription_key_blob(self, author_id: bytes, tier_name: str) -> dict:
        """Fetch the current broadcast KeyBlob for `(author_id, tier_name)`
        (`fauna.subscriptions.key_blob.get`): ``{"version": N, "blob_hash":
        bytes, "blob_data": bytes}``. The kind is subscriber-gated AND requires
        a minted blob, so a successful call from the subscriber is end-to-end
        proof the author's encrypted-mode mint+upload landed; it raises
        `RpcCallError` (`not_subscribed` / `key_blob_not_found`) otherwise."""
        return self._ws_call("fauna.subscriptions.key_blob.get", {
            "author_id": bytes(author_id),
            "tier_name": tier_name,
        })

    def subscription_requests_list(self) -> list:
        """The calling actor's own pending subscribe/unsubscribe requests
        (`fauna.subscriptions.requests.list`, author-bearer). Each entry may
        carry `mlkem_encaps_key` (bytes, 1184 for ML-KEM-768) when the
        requester published a post-quantum ek at subscribe time — omitted
        from the dict entirely (not merely `None`) when classical
        (`post-quantum.md` § Implementation status — Subscription
        `KeyBlobEntry`, surface B slice S4b)."""
        return self._ws_call("fauna.subscriptions.requests.list", {})["requests"]

    def subscription_subscribers_list(self, tier_name: str) -> list:
        """The calling actor's own active subscribers of `tier_name`
        (`fauna.subscriptions.subscribers.list`, author-bearer). Each entry
        may carry `mlkem_encaps_key`, mirroring `subscription_requests_list`."""
        return self._ws_call("fauna.subscriptions.subscribers.list", {
            "tier_name": tier_name,
        })["subscribers"]

    def subscription_tiers_list(self) -> list:
        """The calling author's own tiers (`fauna.subscriptions.tiers.list`,
        author-bearer). Useful as a causal barrier: a returned list containing a
        just-created tier proves the nest committed it, so a client that still
        does not render it has a re-read gap rather than a race."""
        return self._ws_call("fauna.subscriptions.tiers.list", {})["tiers"]

    def subscription_create_tier(
        self, name: str, *, rank: int = 1, auto_approve: bool = False,
        price_hint: str | None = None,
    ) -> dict:
        """Create a subscription tier owned by the calling actor
        (`fauna.subscriptions.tiers.create`, author-bearer), carrying the
        required birth `encrypted_upload` envelope (an empty-roster KeyBlob
        under a throwaway period key, minted by the shared Rust helper).

        ⚠ This mints an ORPHAN tier: the period key is discarded here and
        written into no client custody, so no app can ever mint a later
        KeyBlob for it — `approve_subscriber` auto-heals a missing key only for
        the reserved `followers` tier. Fine for render-only seeding (offers
        lists, pending badges); NEVER for a test that needs a grant to
        materialize — those must have the author's app create the tier
        (`monetization.md` § Pillar 1 → the headless-author corollary)."""
        from fauna_ffi import build_tier_birth_upload
        payload: dict = {
            "name": name, "rank": rank, "auto_approve": auto_approve,
            "encrypted_upload": build_tier_birth_upload(
                self.secret_bytes, name, os.urandom(32)),
        }
        if price_hint is not None:
            payload["price_hint"] = price_hint
        return self._ws_call("fauna.subscriptions.tiers.create", payload)
