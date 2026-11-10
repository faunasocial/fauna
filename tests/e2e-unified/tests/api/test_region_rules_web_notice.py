"""tier_3 API e2e: the declared region's content policy reaches the nest's public web pages.

Outcome ``region-rules`` 8 (`docs/goal/behavior/region-blocking.md` § The
nest-as-publisher leg): what the policy of the nest's declared region (its situs)
blocks, the nest's public web pages show the region notice in place of — the
frame every app shows (``S.region.blocked_notice``) and the authority's reason
verbatim — and the blocked body appears on no page a stranger can fetch; a
``collapse`` verdict keeps the post behind the reveal with the reason under the
frame. Read from outside, as a stranger's browser sees it: plain HTTP with the
author's site host.

The mechanism: the ``test-hooks`` seed signs a synthetic content policy into the
relay cell for the region (``region_relay_test_hook``), the publisher-registry
hook makes the public render verify against the same synthetic registry (the
compiled-in one enrols nobody), and ``fauna.admin.region.set`` declares the situs
and re-renders every site before it answers
(``region_tier::region_set_handler`` → ``rerender_public_sites``); the render
folds the situs chain's policies over each post (``web_content/region.rs``).

Dedicated nest: a declared situs binds every site on the nest, so declaring one
on the session-shared nest would change what every sibling test sees.
"""

from __future__ import annotations

import html
import json
import secrets
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import open_registration
from helpers.admin_wire import admit_adult
from i18n.strings import S
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

NEST_DOMAIN = "situs.example"
REGION = "XZ"
AUTHORITY = "Synthetic Test Authority"
BLOCK_REASON = "Withheld under the Synthetic Act, section 7."
COLLAPSE_REASON = "Hidden under the Synthetic Act, section 9."


@pytest.fixture
def situs_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "region-rules-web", claim_domain=NEST_DOMAIN
    )
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


# A name no other function in the tree has: the mode-axis reach graph
# (`helpers/kind_reach.py`) is keyed on the bare function name.
def _region_web_hook(nest_url: str, path: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/region/{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def _situs_get(url: str, path: str, host: str) -> tuple[int, str]:
    req = urllib.request.Request(url.rstrip("/") + path, headers={"Host": host})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, html.unescape(resp.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as err:
        return err.code, html.unescape(err.read().decode("utf-8", "replace"))


def _situs_publish(nest, actor, text: str, slug: str, offset_us: int) -> str:
    post = sign_and_encode_post(actor["signing_key"], 1_760_000_000_000_000 + offset_us, text)
    post_id = ws_api.create_post(nest["port"], actor, post)
    ws_api.web_publish_set(nest["port"], actor, post_id, slug)
    return post_id


def _declare_situs(nest, region: str | None) -> dict:
    sk = nest["admin"]["signing_key"]
    params = {"region": region} if region else {}
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as ws:
        return ws.call("fauna.admin.region.set", params)


@pytest.mark.feature("region-rules")
def test_the_public_web_pages_show_the_region_notice_in_place_of_what_the_situs_policy_blocks(
    situs_nest,
):
    nest = situs_nest
    tag = secrets.token_hex(4)
    blocked_body = f"zqblocked{tag} a body the authority withholds"
    collapsed_body = f"zqcollapsed{tag} a body behind the reveal"
    clean_body = f"zqclean{tag} a walk by the river"
    handle = f"writer{secrets.token_hex(3)}"
    actor = admit_adult(nest, handle, domain=NEST_DOMAIN, token=True)
    host = f"{handle}.{NEST_DOMAIN}"

    blocked_id = _situs_publish(nest, actor, blocked_body, "blocked-one", 0)
    collapsed_id = _situs_publish(nest, actor, collapsed_body, "collapsed-one", 1_000_000)
    _situs_publish(nest, actor, clean_body, "clean-one", 2_000_000)
    with _actor_client(nest["url"], actor) as ws:
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    # Control: before any situs, every page serves its own words and no notice.
    for slug, words in (
        ("blocked-one", blocked_body),
        ("collapsed-one", collapsed_body),
        ("clean-one", clean_body),
    ):
        status, body = _situs_get(nest["url"], f"/post/{slug}", host)
        assert status == 200 and words in body, (slug, status, body[:400])
        assert "fauna-region-notice" not in body, (slug, body[:400])

    armed = _region_web_hook(nest["url"], "publisher-registry", {"region": REGION})
    assert armed.get("ok") and armed["authority_name"] == AUTHORITY, armed
    seeded = _region_web_hook(nest["url"], "content-policy", {
        "region": REGION,
        "sequence": 1,
        "rules": [
            {"verdict": "block", "reason": BLOCK_REASON, "content_ids": [blocked_id]},
            {"verdict": "collapse", "reason": COLLAPSE_REASON, "content_ids": [collapsed_id]},
        ],
    })
    assert seeded["authority_name"] == AUTHORITY, seeded

    # A policy cached for a region the nest does not declare binds no page.
    status, body = _situs_get(nest["url"], "/post/blocked-one", host)
    assert status == 200 and blocked_body in body, body[:400]

    reply = _declare_situs(nest, REGION)
    assert reply.get("ok"), reply

    blocked_frame = S.region.blocked_notice(region=REGION, authority=AUTHORITY)
    status, body = _situs_get(nest["url"], "/post/blocked-one", host)
    assert status == 200, (status, body[:400])
    assert 'class="fauna-region-notice"' in body and f'data-region="{REGION}"' in body, body
    assert 'data-region-verdict="block"' in body, body
    assert blocked_frame in body, f"the page does not show the apps' notice {blocked_frame!r}: {body}"
    assert BLOCK_REASON in body, f"the authority's reason is not shown verbatim: {body}"
    assert blocked_body not in body, "the blocked body is still on the page"

    collapsed_frame = S.region.collapsed_notice(region=REGION, authority=AUTHORITY)
    status, body = _situs_get(nest["url"], "/post/collapsed-one", host)
    assert status == 200, (status, body[:400])
    assert 'data-region-verdict="collapse"' in body, body
    assert collapsed_frame in body, f"the reveal does not carry the apps' frame {collapsed_frame!r}: {body}"
    assert COLLAPSE_REASON in body, f"the authority's reason is not under the frame: {body}"
    assert collapsed_body in body, "a collapse keeps the post behind the reveal"

    status, body = _situs_get(nest["url"], "/post/clean-one", host)
    assert status == 200 and clean_body in body, body
    assert "fauna-region-notice" not in body, "a post the policy does not name got the notice"

    # No page a stranger can fetch carries the blocked body.
    for path in ("/", "/feed.xml"):
        status, body = _situs_get(nest["url"], path, host)
        assert status == 200, (path, status)
        assert blocked_body not in body, f"{path} still carries the blocked body"

    # Withdrawing the situs re-renders the body back.
    assert _declare_situs(nest, None).get("ok")
    status, body = _situs_get(nest["url"], "/post/blocked-one", host)
    assert status == 200 and blocked_body in body, body[:400]
    assert "fauna-region-notice" not in body, body[:400]
