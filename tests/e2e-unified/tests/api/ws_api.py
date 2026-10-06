"""WS-RPC seeding/query helpers for the `tests/api/` (tier_3) suite.

The feed/posts/interact HTTP twins (`POST /api/v1/posts`,
`GET|POST|DELETE /api/v1/feeds*`, `POST /api/v1/posts/{id}/interact`) were
deleted by the WS-RPC-everywhere migration (T4). These helpers drive the replacement `fauna.posts.*` / `fauna.feed.*`
kinds over the canonical WS-RPC wire, so API-tier tests keep exercising the
real nest ingest/query path with the least per-test churn.

Wire contracts (verified against `bins/fauna-nest/src/{posts,feed}_handlers.rs`
+ `libs/fauna-protocol/src/{posts,feed}.rs`, 2026-05-24):

* ``fauna.posts.create``  — ``{"body": <signed-post bytes>}`` → ``{"post_id": hex}``
  (reuses the same ``ingest_post_core`` pipeline the HTTP twin used).
* ``fauna.posts.delete``  — ``{"body": <signed-tombstone bytes>}``
  → ``{"post_id": hex, "deleted": bool}`` (author-only; drives
  ``delete_post_core`` and its derived-copy propagation legs).
* ``fauna.posts.get``     — ``{"post_id": hex}`` → ``{"body": <post bytes>,
  "legal_takedown"?: {...}}`` (the by-id read door; a missing, deleted or
  quarantine-gated post answers ``fauna.posts.not_found``).
* ``fauna.posts.interact``— ``{"post_id": hex, "action": str, "body"?: str}``
  → ``{"action", "source", "result"}`` (``result`` is a JSON string).
* ``fauna.feed.create``   — ``{"name", "rules", "combination", "scope"?,
  "contributor_seeds"?}`` → ``{"feed_id"}``. Rules ride typed
  (``Vec<FilterRule>``, a list of externally-tagged dicts).
* ``fauna.feed.list``     — ``{}`` → ``{"feeds": [FeedSummary]}``.
* ``fauna.feed.posts``    — ``{"feed_id", "limit"?, ...}`` → ``{"posts": [FeedPostItem], ...}``.
* ``fauna.feed.local.posts`` — ``{"limit"?, ...}`` → ``{"posts": [...], ...}``.
* ``fauna.feed.delete``   — ``{"feed_id"}`` → ``{}``.
* ``fauna.bridges.list``  — ``{}`` → ``{"bridges": [WireBridgeStatus]}``; plus
  ``link`` / ``unlink`` / ``set_settings`` — the unified bridge control plane
  (``bridges.md``), which replaced the per-provider HTTP twins.

Each ``FeedPostItem`` is ``{post_id, author, body, created_at, tags,
has_media, is_reply, source, score?}`` (``score`` is i64 micro-units).

Caller class: ``fauna.posts.create`` and the ``fauna.feed.*`` kinds are
``User``-class (``bridge_method_allowlist.rs``); pass an actor created via
``common.auth.create_actor_and_register(port, admin_signing_key=...)`` (which registers
a User-class actor and returns its ``signing_key`` + ``actor_id_bytes``).

Connection reuse: opening a WS-RPC connection costs an HTTP challenge/verify
round-trip plus the WebSocket upgrade. Seeding loops (some tests publish 50+
posts) would pay that per call, so we cache one open ``WsRpcAdminClient`` per
``(base_url, actor_id)`` and close them all at process exit.
"""

from __future__ import annotations

import atexit
from typing import Any, Optional

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import port_base_url


# (base_url, actor_id_hex) → open client. The WsRpcAdminClient is the generic
# WS-RPC client (Admin name is historical); a User-class actor's keypair makes
# it a User-class client, which is what the feed/posts kinds require.
_CLIENTS: dict[tuple[str, str], WsRpcAdminClient] = {}


def _base_url(port: int) -> str:
    # `http://` for the usual plain-HTTP tier_3 nests; `https://` for one marked
    # via `common.auth.mark_tls_nest` (the real-Mastodon harness's TLS nest). The
    # WS-RPC core reads the scheme (`https://`→`wss://` + CERT_NONE), so a marked
    # port's feed/posts/bridge calls ride the same self-signed floor as the rest.
    return port_base_url(port)


def _client_for(base_url: str, actor: dict) -> WsRpcAdminClient:
    """Return an open, cached WS-RPC client for ``actor`` against ``base_url``.

    ``actor`` is a ``common.auth.create_actor_and_register`` dict — it carries
    ``actor_id_bytes`` (32-byte Ed25519 pubkey) and ``signing_key`` (PyNaCl
    ``SigningKey``).

    ⚠ Returns an OPEN client, which means checking. A cached client can be
    closed without anyone exiting it: `_reconnect()` is `close()` + `_connect()`,
    so a reconnect whose `_connect()` raises (nest mid-restart, or this port
    recycled onto a different nest) leaves the instance dead in the cache, and
    every later caller gets `call() outside of \\`with client:\\` context` — a
    message that names the wrong cause. `ensure_open()` re-opens in place, which
    keeps the cache's own contract instead of relying on nothing ever dropping.
    """
    actor_id_bytes = actor["actor_id_bytes"]
    key = (base_url.rstrip("/"), actor_id_bytes.hex())
    client = _CLIENTS.get(key)
    if client is None:
        client = WsRpcAdminClient(
            base_url,
            actor_id=actor_id_bytes,
            signing_key=bytes(actor["signing_key"]),
        )
        client.__enter__()
        _CLIENTS[key] = client
        return client
    try:
        return client.ensure_open()
    except Exception:
        # The re-open failed too, so this entry is worthless — drop it rather
        # than leave a dead client for the next caller to trip over, and let the
        # connection error itself surface (it is the real diagnosis).
        _CLIENTS.pop(key, None)
        raise


def close_all_ws() -> None:
    """Close every cached WS-RPC connection. Safe to call repeatedly."""
    for client in list(_CLIENTS.values()):
        try:
            client.close()
        except Exception:
            pass
    _CLIENTS.clear()


atexit.register(close_all_ws)


# ── posts ──────────────────────────────────────────────────────────────────


def create_post(port: int, actor: dict, post_bytes: bytes) -> str:
    """``fauna.posts.create`` — returns the hex ``post_id``.

    Drop-in replacement for the old ``POST /api/v1/posts`` (which returned
    ``{"post_id": ...}``); ``post_bytes`` is the signed embed-as-bytes
    encoding from ``bare.sign_and_encode_post``.
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.posts.create", {"body": bytes(post_bytes)})
    return reply["post_id"]


def delete_post(port: int, actor: dict, tombstone_bytes: bytes) -> dict:
    """``fauna.posts.delete`` — returns ``{"post_id", "deleted"}``.

    ``tombstone_bytes`` is the signed embed-as-bytes wire from
    ``bare.sign_and_encode_tombstone``. ``deleted`` is ``False`` when the post
    was already gone — the idempotent success, never an error. Author-only:
    a non-author caller raises ``RpcCallError`` (``permission_denied``).

    This drives the same ``delete_post_core`` propagation pipeline the apps
    do (``bins/fauna-nest/src/routes.rs``), so the derived-copy legs — nostr
    kind-5, Bluesky, outbox, paired replica, and the ActivityPub ``Delete``
    push — all fire behind it.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.posts.delete", {"body": bytes(tombstone_bytes)})


def get_post(port: int, actor: dict, post_id_hex: str) -> bytes:
    """``fauna.posts.get`` — the by-id read door; returns the raw post bytes.

    The single-post read every app's deep-link/permalink door makes
    (``posts_handlers::posts_get_handler`` → ``routes::get_post_core``), and
    the read half of "a deleted post is gone, not hidden": a post that never
    existed, was deleted, or is quarantine-gated for this caller raises
    ``clients.ws_rpc_admin_client.RpcCallError`` with code
    ``fauna.posts.not_found`` — the WS-RPC analogue of the HTTP twin's 404,
    never an empty ``body``.

    (An empty ``body`` means something else entirely: a legally-taken-down
    post, whose reply additionally carries ``legal_takedown``. This helper
    returns the bytes; a caller testing that path reads the raw reply.)
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.posts.get", {"post_id": post_id_hex})
    return bytes(reply["body"])


def interact(
    port: int, actor: dict, post_id: str, action: str, body: Optional[str] = None
) -> dict:
    """``fauna.posts.interact`` — returns ``{action, source, result}``.

    Raises ``clients.ws_rpc_admin_client.RpcCallError`` on ``ok=false`` (e.g.
    ``fauna.posts.invalid_params`` for a non-hex post_id, ``fauna.posts.not_found``
    for an unknown post) — the WS-RPC analogue of the old 4xx HTTP statuses.
    """
    payload: dict[str, Any] = {"post_id": post_id, "action": action}
    if body is not None:
        payload["body"] = body
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.posts.interact", payload)


# ── bridges (the unified control plane — bridges.md) ─────────────────────────


def bridge_list(port: int, actor: dict) -> list[dict]:
    """``fauna.bridges.list`` — returns the inner ``bridges`` list.

    Each entry is ``{id, name, available, linked, identity, mode, settings,
    supports_follows, link_modes, error}``. This is the canonical
    "is my bridge linked, and as whom" read — the replacement for the
    per-provider HTTP status twins (e.g. the deleted
    ``GET /api/v1/activitypub/status``).
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.bridges.list", {})
    return reply.get("bridges", [])


def bridge_status(port: int, actor: dict, bridge_id: str) -> Optional[dict]:
    """The ``bridge_list`` entry for ``bridge_id``, or ``None`` if absent."""
    return next(
        (b for b in bridge_list(port, actor) if b["id"] == bridge_id), None
    )


def bridge_link(
    port: int, actor: dict, bridge_id: str, mode: str, params: Optional[dict] = None
) -> dict:
    """``fauna.bridges.link`` — returns ``{linked, identity, redirect_url}``.

    ``identity`` is ``{label, value, display}`` — for ActivityPub, ``value`` is
    the actor URL and ``display`` the ``@user@domain`` handle. Linking an
    already-linked bridge raises ``RpcCallError`` with code
    ``fauna.bridges.already_linked`` (the WS-RPC analogue of the old HTTP 409).
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.bridges.link",
        {"bridge_id": bridge_id, "mode": mode, "params": params or {}},
    )


def bridge_link_challenge(port: int, actor: dict, bridge_id: str, mode: str) -> dict:
    """``fauna.bridges.link_challenge`` — returns ``{challenge, expires_at, payload}``.

    The proof-of-possession challenge an external signer signs before a
    ``bridge_link`` in ``mode`` is accepted (Nostr ``nip07``: ``payload`` is the
    unsigned kind-22242 event; the link then carries the signed event as its
    ``proof_json`` param). A mode that holds no external identity raises
    ``RpcCallError`` with ``fauna.bridges.invalid_params``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.bridges.link_challenge", {"bridge_id": bridge_id, "mode": mode}
    )


def bridge_unlink(port: int, actor: dict, bridge_id: str) -> dict:
    """``fauna.bridges.unlink`` — drops the actor's bridge account.

    Deliberately ungated on provider availability nest-side, so a user can
    always remove a now-unavailable bridge's state (user-controls-data).
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.bridges.unlink", {"bridge_id": bridge_id})


def bridge_set_settings(port: int, actor: dict, bridge_id: str, settings: dict) -> dict:
    """``fauna.bridges.set_settings`` — updates the provider's settings object.

    ``settings`` keys are the provider's own (ActivityPub:
    ``auto_accept_follows`` / ``default_visibility`` / ``backfill`` / ``enabled``).
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.bridges.set_settings", {"bridge_id": bridge_id, "settings": settings}
    )


def bridge_add_follow(
    port: int, actor: dict, bridge_id: str, id: str, petname: Optional[str] = None
) -> dict:
    """``fauna.bridges.add_follow`` — follow a remote account through a bridge.

    ``id`` is the provider's own account identifier (ActivityPub: the remote
    actor URI). The follow is the local user's opt-in act: for ActivityPub it
    both delivers a signed ``Follow`` to the remote inbox and opens the inbound
    relationship gate for that actor's notes.
    """
    # `petname`/`extra` are sent explicitly (as nulls when unset) rather than
    # omitted — the request type carries them as plain `Option` fields, so an
    # explicit null decodes to `None` on any serde/CBOR missing-field policy.
    payload: dict[str, Any] = {
        "bridge_id": bridge_id,
        "id": id,
        "petname": petname,
        "extra": None,
    }
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.bridges.add_follow", payload)


def bridge_list_follows(port: int, actor: dict, bridge_id: str) -> list[dict]:
    """``fauna.bridges.list_follows`` — returns the inner ``follows`` list."""
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.bridges.list_follows", {"bridge_id": bridge_id})
    return reply.get("follows", [])


def bridge_list_follow_requests(port: int, actor: dict, bridge_id: str) -> list[dict]:
    """``fauna.bridges.list_follow_requests`` — returns the inner ``requests`` list.

    Each entry is ``{id, name, requested_at, extra}``: the follow requests
    waiting on the actor's own account (ActivityPub: inbound ``Follow``s held
    back while ``auto_accept_follows`` is off; ``id`` is the requester's actor
    URI, ``extra.handle`` their ``@user@host`` address).
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.bridges.list_follow_requests", {"bridge_id": bridge_id})
    return reply.get("requests", [])


def bridge_resolve_follow_request(
    port: int, actor: dict, bridge_id: str, id: str, approve: bool
) -> dict:
    """``fauna.bridges.resolve_follow_request`` — approve or refuse one request.

    Idempotent: answering a request that is already gone returns ``{ok: true}``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.bridges.resolve_follow_request",
        {"bridge_id": bridge_id, "id": id, "approve": approve},
    )


# ── link previews (render-model.md § D4) ─────────────────────────────────────


def resolve_link_preview(port: int, actor: dict, url: str) -> dict:
    """``fauna.linkpreview.resolve`` — returns the internally-tagged reply dict.

    ``{"outcome": "resolved", "title", "description", "image_hash"}`` for a page
    that yielded usable OpenGraph/meta (``image_hash`` is the hex BLAKE3 of the
    nest-stored og:image blob, or ``None``), or ``{"outcome": "failed"}`` for any
    fetch/SSRF/parse failure. The nest does the SSRF-guarded fetch + parse +
    image-blob store (the client never contacts the third party).
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.linkpreview.resolve", {"url": url})


# ── pairing ──────────────────────────────────────────────────────────────────


def add_pairing(
    port: int,
    actor: dict,
    private_nest_id: bytes,
    capabilities: Optional[list[str]] = None,
    nest_url: Optional[str] = None,
) -> None:
    """``fauna.pair.add`` — the user authorizes a nest to sync their account.

    Replaces the retired peer handshake ``POST /api/v1/pair`` (per-user-pairing
    reshape, 2026-05-25): pairing is now an owner-scoped *user* bearer action,
    so the pairing row is stored under ``actor`` (the connection actor) on the
    public nest. Default capabilities are the canonical full self-sync set
    (``mls_pull``/``namespace_sync``/``post_forward``), so the paired private
    nest passes the public nest's ``is_paired`` sync-auth check.
    """
    caps = capabilities or ["mls_pull", "namespace_sync", "post_forward"]
    payload: dict[str, Any] = {
        "private_nest_id": bytes(private_nest_id),
        "capabilities": caps,
    }
    if nest_url is not None:
        payload["nest_url"] = nest_url
    client = _client_for(_base_url(port), actor)
    client.call("fauna.pair.add", payload)


# ── feeds ──────────────────────────────────────────────────────────────────


def create_feed(
    port: int,
    actor: dict,
    name: str,
    rules: Optional[list] = None,
    combination: str = "all",
    scope: Optional[str] = None,
    contributor_seeds: Optional[list[str]] = None,
) -> str:
    """``fauna.feed.create`` — returns the ``feed_id``.

    ``rules`` is the Python list of externally-tagged ``FilterRule`` dicts,
    sent as-is on the typed ``rules`` field.
    """
    payload: dict[str, Any] = {
        "name": name,
        "rules": rules or [],
        "combination": combination,
    }
    if scope is not None:
        payload["scope"] = scope
    if contributor_seeds is not None:
        payload["contributor_seeds"] = contributor_seeds
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.feed.create", payload)
    return reply["feed_id"]


def list_feeds(port: int, actor: dict) -> list[dict]:
    """``fauna.feed.list`` — returns the inner ``feeds`` list.

    The old ``GET /api/v1/feeds`` returned the list directly; the WS reply
    wraps it as ``{"feeds": [...]}``, so we unwrap to preserve call-site
    ergonomics (``for f in list_feeds(...)``).
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.feed.list", {})
    return reply.get("feeds", [])


def get_feed(port: int, actor: dict, feed_id: str) -> dict:
    """``fauna.feed.get`` — returns the full feed record, including its
    ``composition`` (content-moderation-and-ranking.md § Composition; ``None``
    when the feed carries no factor-weight entries)."""
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.feed.get", {"feed_id": feed_id})


def feed_posts(port: int, actor: dict, feed_id: str, limit: Optional[int] = None,
               search: Optional[str] = None) -> list[dict]:
    """``fauna.feed.posts`` — returns the inner ``posts`` list.

    ``search`` is the apps' feed-search re-query term (a MANDATORY narrowing
    filter — it must AND with the feed's rules whatever the feed's combination).
    """
    payload: dict[str, Any] = {"feed_id": feed_id}
    if limit is not None:
        payload["limit"] = limit
    if search is not None:
        payload["search"] = search
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.feed.posts", payload)
    return reply.get("posts", [])


def local_feed_posts(port: int, actor: dict, limit: Optional[int] = None,
                     search: Optional[str] = None) -> list[dict]:
    """``fauna.feed.local.posts`` — returns the inner ``posts`` list.

    ``search`` is the comma-separated body-search the apps' feed search box
    sends (nest-side ``search_to_filter`` → ``BodyContains``).
    """
    payload: dict[str, Any] = {}
    if limit is not None:
        payload["limit"] = limit
    if search is not None:
        payload["search"] = search
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.feed.local.posts", payload)
    return reply.get("posts", [])


def trending_feed_posts(port: int, actor: dict, limit: Optional[int] = None) -> list[dict]:
    """``fauna.feed.trending.posts`` — the built-in **Trending** virtual read.

    Returns the inner ``posts`` list, score-ordered (highest ``trending`` factor
    first). No feed row backs it (``trending.md`` § The read model): the nest
    composes the implicit ``[(trending, 1000)]`` weight + the caller's global
    factor set and reads **public posts only** (``query_feed_scored_public`` —
    ``gated_tier IS NULL``). Each ``FeedPostItem`` carries ``post_id`` + ``score``
    (i64 micro-units). ``User``-class kind (``bridge_method_allowlist.rs``).
    """
    payload: dict[str, Any] = {}
    if limit is not None:
        payload["limit"] = limit
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.feed.trending.posts", payload)
    return reply.get("posts", [])


def delete_feed(port: int, actor: dict, feed_id: str) -> None:
    """``fauna.feed.delete``."""
    client = _client_for(_base_url(port), actor)
    client.call("fauna.feed.delete", {"feed_id": feed_id})


# ── search ─────────────────────────────────────────────────────────────────


def search(
    port: int,
    actor: dict,
    query: str,
    limit: Optional[int] = None,
    content_type: Optional[str] = None,
) -> list[dict]:
    """``fauna.search.query`` — returns the inner ``results`` list.

    The old ``GET /api/v1/search`` HTTP twin was deleted in the WS-RPC search
    migration (``bins/fauna-nest/src/search_handlers.rs`` — "transport migration
    of the old ``GET /api/v1/search`` route"); the WS reply wraps the hits as
    ``{"results": [...]}``, so we unwrap for call-site ergonomics.
    """
    payload: dict[str, Any] = {"query": query}
    if limit is not None:
        payload["limit"] = limit
    if content_type is not None:
        payload["content_type"] = content_type
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.search.query", payload)
    return reply.get("results", [])


# ── moderation ───────────────────────────────────────────────────────────────


def attach_labels(port: int, actor: dict, content_id: str, labels: list[dict]) -> int:
    """``fauna.labels.attach`` — attach content labels; returns ``stored``.

    ``labels`` entries are ``{"category": str, "confidence_per_mille": int}``
    (0–1000; the dag-cbor wire forbids floats — the nest maps per-mille to the
    stored 0.0–1.0 confidence). Content type is fixed to ``"post"`` nest-side.
    The caller is a scoring position submitting its verdict — a client-side
    classifier or a granted content-processing holder; the nest itself never
    classifies content (``content-scoring.md`` § The placement matrix).
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call(
        "fauna.labels.attach", {"content_id": content_id, "labels": labels}
    )
    return reply["stored"]


def moderation_stats(port: int, actor: dict) -> list[dict]:
    """``fauna.moderation.stats`` — aggregate label counts.

    Returns the inner ``labels`` list, each entry
    ``{"category", "count", "avg_confidence_per_mille"}``. Replaces the old
    ``GET /api/v1/moderation/stats`` whose twin returned a category-keyed dict.
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.moderation.stats", {})
    return reply.get("labels", [])


def moderation_actions(port: int, actor: dict) -> list[dict]:
    """``fauna.moderation.actions`` — the **connection actor's own** obligation
    actions (the HTTP twin's ``?actor=`` any-actor query is dropped; the WS
    connection knows its caller). Returns the inner ``actions`` list.
    """
    client = _client_for(_base_url(port), actor)
    reply = client.call("fauna.moderation.actions", {})
    return reply.get("actions", [])


def moderation_appeal(port: int, actor: dict, content_id_hex: str, reason: str) -> dict:
    """``fauna.moderation.appeal`` — appeal an enforcement decision recorded
    against ``content_id_hex`` (the second leg of the transparency triple,
    ``moderation.md`` § Legal takedown). Returns ``{status, content_id}``;
    ``status`` is ``"appeal_recorded"``, or ``"appeal_already_recorded"`` when
    this caller's earlier appeal is still pending (nothing new is written).

    The nest refuses a ``content_id`` it holds NO enforcement record for with
    ``fauna.moderation.not_found`` — the appeal trail records appeals against
    real decisions, not arbitrary strings — and a post appeal from anyone but
    the post's author with ``fauna.moderation.permission_denied``. ``reason``
    is required and bounded (``fauna.moderation.invalid_params`` when empty or
    over ``MAX_APPEAL_REASON_BYTES``).
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.moderation.appeal", {"content_id": content_id_hex, "reason": reason}
    )


def moderation_legal_takedown(
    port: int,
    actor: dict,
    content_id_hex: str,
    *,
    content_type: str = "post",
    legal_reference: str = "",
    restore: bool = False,
) -> dict:
    """``fauna.moderation.legal_takedown`` — the Admin-only legal-compulsion
    takedown / overturn (``moderation.md`` § Legal takedown). ``actor`` must be
    an admin. ``legal_reference`` is REQUIRED when ``restore`` is false (the
    structural guard that makes this compulsion, not policy). Returns
    ``{status, ...}`` with ``status`` ``"taken_down"`` / ``"restored"``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.moderation.legal_takedown",
        {
            "content_id": content_id_hex,
            "content_type": content_type,
            "legal_reference": legal_reference,
            "restore": restore,
        },
    )


def moderation_train(port: int, actor: dict, content_id_hex: str, verdict: str) -> dict:
    """``fauna.moderation.train`` — the nest half of the client's mark-as-spam/ham
    on a post.

    ``content_id_hex`` is the hex 32-byte post id (a post's content-addressed id
    IS its report-hash — ``report-sharing.md`` § Content identity); ``verdict`` is
    ``"spam"`` or ``"ham"``. Returns ``{status, verdict}`` (``status: "trained"``
    acknowledges the correction). It trains NO model — the per-user spam model
    rests sealed and only the user's app trains it, via ``put_spam_model``. What
    it does: when the connection actor has opted in to report sharing
    (``report_share.set{share:true}``), a ``"spam"`` verdict records a
    k-anonymized report row for the post (``report-sharing.md`` § Report
    capture); a ``"ham"`` verdict withdraws it. The caller must be able to READ
    the post.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.moderation.train", {"content_id": content_id_hex, "verdict": verdict}
    )


def report_share_set(port: int, actor: dict, share: bool) -> dict:
    """``fauna.moderation.report_share.set`` — the connection actor opts in/out of
    distributed report sharing (default off). Caller-scoped (no ``actor_id`` on
    the wire — even an admin sets only their own; ``report-sharing.md`` § Client
    wire). ``share=false`` deletes the actor's existing report rows and recomputes
    every affected aggregate (below k → withdrawn). Returns ``{share}``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.moderation.report_share.set", {"share": bool(share)})


def baseline_contribution_set(port: int, actor: dict, contribute: bool) -> dict:
    """``fauna.bridges.set_baseline_contribution`` — the connection actor opts in/out
    of contributing its sealed spam model to the deployment baseline (default off;
    ``mail-spam.md`` § Encrypted-mode interaction). Caller-scoped, like
    ``report_share_set``. Returns ``{contribute}``.

    Fixture setup (``e2e-conventions.md`` convention 8's carve-out): the way a test
    that asserts a WHOLE-NEST baseline count clears the session-shared account's
    opt-in that an earlier GUI test left behind.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.bridges.set_baseline_contribution", {"contribute": bool(contribute)}
    )


def report_share_status(port: int, actor: dict) -> dict:
    """``fauna.moderation.report_share.status`` — the transparency view.

    Returns ``{share, published:[{content_hash (hex), factor, count}]}`` where
    ``published`` is **exactly** the ≥k-gated export view a peer nest would
    receive (``report-sharing.md`` § Client wire — that identity IS the
    transparency guarantee). Below k, a hash is absent from ``published`` on
    every surface; ``share`` is the caller's own opt-in state.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.moderation.report_share.status", {})


def reset_spam_model(port: int, actor: dict) -> dict:
    """``fauna.bridges.reset_spam_model`` — deletes the caller's per-user spam
    model + all training history (idempotent; ``mail-spam.md`` § Reset). Bare
    ack reply. Used as direct-RPC test cleanup where the UI reset control is
    unreachable (the apple system `.confirmationDialog` gap) but a later test in the same session-shared nest still needs the
    actor's model cleared.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.bridges.reset_spam_model", {})


def signal_share_set(port: int, actor: dict, share: bool) -> dict:
    """``fauna.moderation.signal_share.set`` — the connection actor opts in/out of
    Layer-B engagement-signal sharing (default off; ``engagement-cues.md``
    § Layer B). Caller-scoped, and INDEPENDENT of ``report_share`` — ``share=false``
    deletes only the actor's ``signal:*`` rows (never their ``report:spam`` rows)
    and recomputes each affected aggregate (below k → withdrawn). Returns ``{share}``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.moderation.signal_share.set", {"share": bool(share)})


def signal_share_status(port: int, actor: dict) -> dict:
    """``fauna.moderation.signal_share.status`` — the signal transparency view.

    Returns ``{share, published:[{content_hash (hex), factor, count}]}``.
    ``published`` is the **same** ≥k-gated federation export view
    ``report_share.status`` returns (one export function), so it carries every
    published aggregate — ``report:*`` and ``signal:*`` alike. ``share`` is the
    caller's own signal opt-in state.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.moderation.signal_share.status", {})


def signal_contribute(port: int, actor: dict, content_id_hex: str, signal: str) -> dict:
    """``fauna.moderation.signal_contribute`` — contribute one derived cue verdict
    for a PUBLIC post (``engagement-cues.md`` § Layer B write path).

    ``content_id_hex`` is the hex 32-byte post id; ``signal`` is
    ``"watch-complete"``, ``"skip"``, or ``"withdraw"`` (last-wins per item — a
    flip withdraws the old factor and inserts the new; ``withdraw`` retracts
    both). Honored only when the actor opted in (``signal_share.set{share:true}``),
    except ``withdraw`` which always applies. Returns ``{status, signal}``.
    """
    client = _client_for(_base_url(port), actor)
    return client.call(
        "fauna.moderation.signal_contribute",
        {"content_id": content_id_hex, "signal": signal},
    )


# ── discovery (pre-identity) ─────────────────────────────────────────────────


def nest_info(port: int) -> dict:
    """``fauna.nest.info`` — public node metadata over the anonymous connection.

    Replaces ``GET /api/v1/node-info`` (deleted; migrated to the pre-identity
    kind). Returns the full reply dict (carries ``domain``, ``registration``,
    ``moderation``, tiers, etc.).
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    with WsRpcAnonClient(_base_url(port)) as anon:
        return anon.call("fauna.nest.info", {})


def setup_status(port: int) -> dict:
    """``fauna.setup.status`` — the pre-identity setup/policy read (carries
    ``registration_mode``, the nest's registration posture)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    with WsRpcAnonClient(_base_url(port)) as anon:
        return anon.call("fauna.setup.status", {})


def handle_available(port: int, handle: str) -> dict:
    """``fauna.handle.available`` — is ``handle`` free on this nest?

    Replaces ``GET /api/v1/handle-available/{handle}`` (deleted). Returns
    ``{available, handle, domain, cooldown}``.
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    with WsRpcAnonClient(_base_url(port)) as anon:
        return anon.call("fauna.handle.available", {"handle": handle})


def actor_by_handle(port: int, handle: str) -> dict:
    """``fauna.actor.by_handle`` — resolve ``handle`` to an actor.

    Replaces ``GET /api/v1/actor/by-handle/{handle}`` (deleted). Returns
    ``{actor_id (hex), handle, domain, addresses, addressable}``. Raises
    ``RpcCallError`` with code ``fauna.actor.not_found`` for an unknown handle
    (the WS-RPC analogue of the twin's 404).
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    with WsRpcAnonClient(_base_url(port)) as anon:
        return anon.call("fauna.actor.by_handle", {"handle": handle})


def personalization_model_fetch(port: int, actor: dict, factor: str) -> dict:
    """``fauna.personalization.model.fetch`` — the caller's sealed
    trained-model row for ``factor`` (topic-factors.md § Wire & registry).

    The blob is client-sealed under the BackupKey and nest-opaque, so tests
    assert on the ADVISORY ``sample_count`` / blob presence, never contents.
    An absent row (never trained on any device, or deleted) is
    ``{"sealed_blob": None, "sample_count": 0, "updated_at": 0}`` — not an
    error. The deterministic pre-reload gate (the draft-persistence pattern):
    poll this until ``sample_count`` rises before ``hard_reload()``, so a
    restart never races the client's seal-and-put."""
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.personalization.model.fetch", {"factor": factor})


def feed_factors_get(port: int, actor: dict) -> list[dict]:
    """``fauna.feed.factors.get`` — the caller's global factor set, as
    ``[{"factor", "weight_permille"}, …]``.

    The nest folds this set into EVERY score-ordered feed the caller reads, on
    top of the feed's own composition (``bins/fauna-nest/src/feed_routes.rs``,
    the ``order=score`` branch). On the session-scoped ``test_user`` that makes
    it an input to every ranking assertion later in the run, so a test that
    writes it snapshots it first and puts it back with :func:`feed_factors_set`.
    """
    client = _client_for(_base_url(port), actor)
    return list(client.call("fauna.feed.factors.get", {})["factors"])


def feed_factors_set(port: int, actor: dict, factors: list[dict]) -> None:
    """``fauna.feed.factors.set`` — REPLACE the caller's whole global factor set
    with ``factors``. A whole-set overwrite, so passing back exactly what
    :func:`feed_factors_get` returned restores it."""
    client = _client_for(_base_url(port), actor)
    client.call("fauna.feed.factors.set", {"factors": factors})


# ── web publishing (web-content-hosting.md § Published-post management) ──────


def web_publish_set(
    port: int, actor: dict, post_id_hex: str, slug: Optional[str] = None
) -> str:
    """``fauna.web.publish.set`` — publish one of the caller's own posts.

    Returns the **effective** slug: ``slug=None`` takes the nest's post-id-hex
    default, so a caller that echoes its own ``None`` would build a dead link.
    Caller-scoped; upsert (re-publishing the same post moves its slug).
    """
    payload: dict[str, Any] = {"post_id": bytes.fromhex(post_id_hex)}
    if slug is not None:
        payload["slug"] = slug
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.web.publish.set", payload)["slug"]


def web_publish_unset(port: int, actor: dict, post_id_hex: str) -> bool:
    """``fauna.web.publish.unset`` — take a published post down. Idempotent."""
    client = _client_for(_base_url(port), actor)
    reply = client.call(
        "fauna.web.publish.unset", {"post_id": bytes.fromhex(post_id_hex)}
    )
    return reply.get("ok", False)


def web_publish_list(port: int, actor: dict) -> list[dict]:
    """``fauna.web.publish.list`` — the caller's published posts.

    Each row is ``{post_id (bytes), slug, gated_tier?}``. ``gated_tier`` is the
    ``content_meta`` LEFT JOIN telling the management surface which rows offer
    *Copy paywall link*; it is **absent** on an ungated row (the LEFT JOIN
    finds no gate), never an error.
    """
    client = _client_for(_base_url(port), actor)
    return client.call("fauna.web.publish.list", {}).get("posts", [])
