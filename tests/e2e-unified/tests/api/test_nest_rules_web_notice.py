"""tier_3 API e2e: the nest's rules reach its public web pages.

Outcome ``nest-rules`` 6 (`docs/goal/behavior/nest-rules.md` § The nest content
policy → *The publisher leg*): what the nest's rules block, the nest's public
web pages show the same notice in place of — the frame every app shows
(``S.nest_rules.blocked_notice``) and the admin's reason verbatim — and the
blocked words appear on no page a stranger can fetch. Read from outside, as a
stranger's browser sees it: plain HTTP with the author's site host.

The mechanism: ``fauna.admin.content_policy.set`` stores the signed rules and
re-renders every site before it answers
(``content_policy_handlers.rs`` → ``rerender_after_publisher_policy_change``);
the render folds the rules over each post (``web_content/region.rs``).

Dedicated nest: rules bind every member of a nest, so setting them on the
session-shared one would change what every sibling test sees.
"""

from __future__ import annotations

import html
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

NEST_DOMAIN = "rules.example"
REASON = "No gambling adverts on this nest."


@pytest.fixture
def rules_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-rules-web", claim_domain=NEST_DOMAIN
    )
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


def _get(url: str, path: str, host: str) -> tuple[int, str]:
    req = urllib.request.Request(url.rstrip("/") + path, headers={"Host": host})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, html.unescape(resp.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as err:
        return err.code, html.unescape(err.read().decode("utf-8", "replace"))


def _publish(nest, actor, text: str, slug: str, offset_us: int) -> None:
    post = sign_and_encode_post(actor["signing_key"], 1_760_000_000_000_000 + offset_us, text)
    ws_api.web_publish_set(nest["port"], actor, ws_api.create_post(nest["port"], actor, post), slug)


def _set_rules(nest, document: dict) -> dict:
    sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as ws:
        return ws.call("fauna.admin.content_policy.set", {"document": document})


@pytest.mark.feature("nest-rules")
def test_the_public_web_pages_show_the_rules_notice_in_place_of_what_they_block(rules_nest):
    nest = rules_nest
    term = f"zqcasino{secrets.token_hex(4)}"
    handle = f"writer{secrets.token_hex(3)}"
    actor = admit_adult(nest, handle, domain=NEST_DOMAIN, token=True)
    host = f"{handle}.{NEST_DOMAIN}"

    _publish(nest, actor, f"Tonight at the {term} hall", "blocked-one", 0)
    _publish(nest, actor, "A walk by the river", "clean-one", 1_000_000)
    with _actor_client(nest["url"], actor) as ws:
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    frame = S.nest_rules.blocked_notice(nest=NEST_DOMAIN)

    # Control: before any rule, both pages serve their own words and no notice.
    for slug, words in (("blocked-one", term), ("clean-one", "A walk by the river")):
        status, body = _get(nest["url"], f"/post/{slug}", host)
        assert status == 200 and words in body, (slug, status, body[:400])
        assert "fauna-nest-notice" not in body, (slug, body[:400])

    reply = _set_rules(nest, {
        "version": 2,
        "scorers": [{"name": "kw", "kind": "keywords", "bytes": term.encode()}],
        "rules": [{
            "factor": f"nest:{NEST_DOMAIN}/kw",
            "min_permille": 1,
            "verdict": "block",
            "reason_code": "no-gambling-adverts",
            "reason": {"default": REASON},
        }],
    })
    assert reply.get("policy") and not reply.get("defect"), reply

    status, body = _get(nest["url"], "/post/blocked-one", host)
    assert status == 200, (status, body[:400])
    assert 'class="fauna-nest-notice"' in body and f'data-nest="{NEST_DOMAIN}"' in body, body
    assert frame in body, f"the page does not show the apps' notice {frame!r}: {body}"
    assert REASON in body, f"the admin's reason is not shown verbatim: {body}"
    assert term not in body, "the blocked words are still on the page"

    status, body = _get(nest["url"], "/post/clean-one", host)
    assert status == 200 and "A walk by the river" in body, body
    assert "fauna-nest-notice" not in body, "a post the rules do not touch got the notice"

    # The front page and the feed list the blocked post only under its notice.
    for path in ("/", "/feed.xml"):
        status, body = _get(nest["url"], path, host)
        assert status == 200, (path, status)
        assert term not in body, f"{path} still carries the blocked words"
