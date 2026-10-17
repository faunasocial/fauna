"""Quoting a Bluesky post from Fauna, and the refusal when no Bluesky account
is linked.

`docs/goal/behavior/bridges.md` § Cross-posting → *A post that references a
Bluesky record* (a quote of a Bluesky-origin post is cross-posted **whatever
the mode**, carrying ``embed: app.bsky.embed.record {uri, cid}``) and
§ Interactions (the interact door refuses a reply or quote with no linked
account, with the remedy — the typed ``fauna.bluesky.not_linked``). Catalog
outcomes ``atproto`` 40 and 42.

The far end, the link through the real OAuth callback and the poll-now seam
are `test_bluesky_feed_ingest.py`'s, imported rather than copied; read its
module docstring for why each is a precondition and not a shortcut (e2e rule
8b). Mode changes and the unlink in the refusal test are preconditions too,
set over the user's own ``fauna.bridges.*`` calls; the subject — the quote
and the reply/quote gesture — is the app's.

**42 is an honest red today.** The app renders the typed code's string
(``error.bluesky.not_linked``, "…Link one on the AT Protocol page first.")
since 2026-10-08, but the nest's door still mints a plain ``400`` that reaches
the app as the generic string; minting the code is the open nest
build.
The assertion is the promise — the user is told where to link one — and turns
green with that build, never by weakening it.

tier_3: a locally-built nest and the real app; the atproto network is a fake.
tui first, the lead app; the other apps gain their markers in the cross-app
lift.
"""

from __future__ import annotations

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    FOLLOWED_DID,
    FOLLOWED_HANDLE,
    _link_through_callback,
    _poll_now,
    atproto_far_end,
    bluesky_feed_nest,
    bluesky_feed_spa_url,
)

pytestmark = [pytest.mark.tier_3]

#: `write_through` — the Bluesky bridge's cross-posting mode
#: (`bluesky/bridge_provider.rs`): 0 disabled, 1 automatic, 2 manual.
MODES = {"disabled": 0, "auto": 1, "manual": 2}


def _call(nest, kind: str, params: dict) -> dict:
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        return client.call(kind, params)


def _own_posts(nest) -> list[dict]:
    local = _call(nest, "fauna.feed.local.posts", {"limit": 100})
    return [
        p for p in local.get("posts", [])
        if p.get("author") == nest["user"]["actor_id_hex"]
    ]


def _signed_in_on_the_ingested_post(request, app, nest, far, text, rkey) -> tuple[str, int]:
    """Publish a followed post at the far end, ingest it, sign the app in and
    find its card. Returns ``(post_uri, card_index)``."""
    from conftest import _login_app_as

    post_uri = far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey=rkey, text=text,
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll should store the followed post: {report!r}; "
        f"far end saw {far.requests!r}"
    )
    wire = [
        p for p in _call(nest, "fauna.feed.local.posts", {"limit": 50}).get("posts", [])
        if p.get("source") == "bluesky" and p.get("body") == text
    ]
    assert len(wire) == 1, f"the ingested post should be on the wire once: {wire!r}"
    post_id = wire[0]["post_id"]

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.feed.navigate()
    deadline = time.monotonic() + 30
    index = -1
    while time.monotonic() < deadline:
        index = app.feed.post_index_by_id(post_id)
        if index >= 0:
            break
        time.sleep(0.3)  # sleep-ok: pacing between poll iterations
    assert index >= 0, (
        f"the ingested Bluesky post {post_id} is on the wire but not in the "
        f"app's feed; error={app.error_text()!r}"
    )
    return post_uri, index


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("atproto")
@pytest.mark.parametrize("mode", sorted(MODES))
def test_a_quote_of_a_bluesky_post_is_published_on_bluesky_carrying_it(
    request, app, bluesky_feed_nest, atproto_far_end, mode,
):
    """``feed-quote-button`` on an ingested Bluesky post → the linked PDS
    receives a ``createRecord`` of an ``app.bsky.feed.post`` whose embed names
    the quoted record — under every cross-posting mode, because the reference
    is the intent (bridges.md § Cross-posting)."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    _call(nest, "fauna.bridges.set_settings", {
        "bridge_id": "bluesky", "settings": {"write_through": MODES[mode]},
    })
    post_uri, index = _signed_in_on_the_ingested_post(
        request, app, nest, far, f"a bluesky post to quote ({mode})", f"3kquote{MODES[mode]}",
    )

    app.feed.quote_post(index)

    def _quotes():
        out = []
        for r in far.created_snapshot():
            record = r.get("record") or {}
            embed = record.get("embed") or {}
            target = (embed.get("record") or {})
            # `app.bsky.embed.record` carries {uri, cid}; a quote with media
            # (`recordWithMedia`) nests it one level deeper.
            if "record" in target and isinstance(target["record"], dict):
                target = target["record"]
            if r.get("collection") == "app.bsky.feed.post" and target.get("uri") == post_uri:
                out.append(r)
        return out

    deadline = time.monotonic() + 30
    quotes = []
    while time.monotonic() < deadline:
        quotes = _quotes()
        if quotes:
            break
        time.sleep(0.3)  # sleep-ok: pacing between poll iterations
    assert quotes, (
        f"with cross-posting {mode!r}, quoting a Bluesky post must publish the "
        f"quote on Bluesky with its embed naming {post_uri}; createRecord "
        f"bodies seen: {far.created_snapshot()!r}; app error={app.error_text()!r}"
    )
    embed = quotes[0]["record"]["embed"]
    target = embed.get("record") or {}
    if isinstance(target.get("record"), dict):
        target = target["record"]
    assert target.get("cid"), f"the quote's embed must carry the quoted record's cid: {embed!r}"
    assert "reply" not in quotes[0]["record"], (
        f"a quote is not a reply: {quotes[0]['record']!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("atproto")
@pytest.mark.parametrize("kind", ["reply", "quote"])
def test_replying_or_quoting_with_no_bluesky_account_linked_is_refused_with_the_remedy(
    request, app, bluesky_feed_nest, atproto_far_end, kind,
):
    """With the Bluesky account unlinked after the post arrived, the reply /
    quote gesture is refused inline with the message that says where to link
    one — and nothing is composed or written to the far end."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    post_uri, index = _signed_in_on_the_ingested_post(
        request, app, nest, far, f"a bluesky post to answer ({kind})", f"3krefuse{kind}",
    )
    # Unlinking drops the account row only — the ingested post stays.
    _call(nest, "fauna.bridges.unlink", {"bridge_id": "bluesky"})
    created_before = len(far.created_snapshot())
    own_before = _own_posts(nest)

    if kind == "reply":
        app.feed.reply_post("a reply that must be refused", index)
    else:
        app.feed.quote_post(index)

    error = ""
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        error = app.error_text()
        if error:
            break
        time.sleep(0.2)  # sleep-ok: pacing between poll iterations
    remedy = S.error.bluesky.not_linked
    assert remedy in error, (
        f"the {kind} of a Bluesky post with no account linked must refuse "
        f"inline with the remedy {remedy!r}; error-message read {error!r}. "
        f"(Red until the nest mints the typed `fauna.bluesky.not_linked`; the "
        f"door's plain 400 renders the generic string.)"
    )
    # The refusal precedes composing (the eligibility ack is sent first), so
    # once it renders the outcome is settled.
    assert _own_posts(nest) == own_before, (
        f"a refused {kind} still created a post: {_own_posts(nest)!r}"
    )
    assert len(far.created_snapshot()) == created_before, (
        f"a refused {kind} still wrote to Bluesky: {far.created_snapshot()!r}"
    )
