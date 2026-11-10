"""The two feature-gated bridge planes at a succession — full-stack (tier_3)
API E2E.

Witnesses `take-your-account-back` outcome **48** — *"Your fediverse identity
carries on under your handle, while bridges to outside networks the account
had set up are cut."* — the ruling of
`docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
(the two feature-gated bridge planes), whose one discriminator is *who chose
the external destination*:

* the **`ap_*` family moves** — the ActivityPub username is minted from the
  Fauna handle and frozen, and the actor URL, WebFinger and the inbox all
  resolve through it, so an identity left behind would strand every remote
  `Follow` on the retired actor and push the successor onto ``<handle>-2``;
* the **`bluesky_*` family burns** — its destination is whatever DID completed
  the OAuth flow, an ordinary gesture a seed thief can make, and carried
  forward its cross-post setting would publish the successor's every public
  post into the repo the thief chose.

Two nests, because no single standalone build compiles both providers (the
image ships all three): each test owns its nest, so the ceremony touches no
other module's state. The pre-ceremony assertions make each post-ceremony one
evidence: a bridge reading "unlinked" afterwards proves a cut only because it
read "linked" before.

Latency-independent (convention 14): every call is a request with a reply.
Process safety: nests start through the dedicated-nest seam and stop with
their fixture; no ``pkill``/``killall``.
"""

from __future__ import annotations

import urllib.parse

import pytest
import requests

from common.auth import create_actor_and_register
from tests.api.test_succession_repoint_planes import _succeed, _ws

pytestmark = pytest.mark.tier_3

AP_BRIDGE_ID = "activitypub"
BLUESKY_BRIDGE_ID = "bluesky"
BLUESKY_DOMAIN = "bsky-succession.test"
AP_DOMAIN = "ap-succession.test"


@pytest.fixture(scope="module")
def bridges_nest_binary():
    """fauna-nest with the `activitypub` provider (+ test-hooks) — the default
    `nest_binary` does not compile ActivityPub. Delegates to conftest's
    memoized ensure layer, so the collection-time prebuild is replayed."""
    from conftest import _ensure_bridges_nest_built

    return _ensure_bridges_nest_built()


@pytest.fixture(scope="module")
def succession_ap_nest(request, nest_mode, tmp_path_factory):
    """A nest with ActivityPub compiled in, owned by this module, claimed onto
    an identity domain — WebFinger answers only for the deployment's handle
    domain, and a domainless nest has none."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-ap-nest",
        binary="bridges_nest_binary",
        claim_domain=AP_DOMAIN,
    )
    yield nest
    cleanup()


@pytest.fixture
def atproto_far_end():
    from helpers.atproto_fakes import FakeAtprotoFarEnd

    far = FakeAtprotoFarEnd()
    yield far
    far.close()


@pytest.fixture
def succession_bluesky_nest(request, nest_mode, tmp_path_factory, atproto_far_end):
    """A `bluesky` nest claimed onto a public domain (Bluesky OAuth derives
    its client from it), its OAuth client pointed at the fake far end."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-bluesky-nest",
        binary="bluesky_nest_binary",
        claim_domain=BLUESKY_DOMAIN,
        extra_env={"FAUNA_TEST_ATPROTO_FAR_END": atproto_far_end.url},
    )
    yield nest
    cleanup()


def _fresh_actor(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )


def _bridge(ws, bridge_id: str) -> dict:
    bridges = ws.call("fauna.bridges.list", {})["bridges"]
    entry = next((b for b in bridges if b["id"] == bridge_id), None)
    assert entry is not None, f"the {bridge_id} bridge must be offered: {bridges!r}"
    return entry


def _ap_get(nest, path: str, accept: str) -> tuple[int, dict | None]:
    resp = requests.get(
        f"{nest['url']}{path}", headers={"Accept": accept}, verify=False, timeout=30
    )
    try:
        body = resp.json()
    except ValueError:
        body = None
    return resp.status_code, body


def _ap_presence(nest, username: str, domain: str) -> dict:
    """What a remote server sees of the AP identity: WebFinger's self link and
    the actor document's ``preferredUsername``."""
    resource = f"acct:{username}@{domain}"
    wf_status, wf = _ap_get(
        nest,
        f"/.well-known/webfinger?resource={urllib.parse.quote(resource, safe='')}",
        "application/jrd+json",
    )
    actor_status, actor = _ap_get(nest, f"/ap/users/{username}", "application/activity+json")
    return {
        "webfinger": (wf_status, (wf or {}).get("links", [{}])[0].get("href")),
        "actor": (actor_status, (actor or {}).get("preferredUsername")),
    }


@pytest.mark.feature("take-your-account-back")
def test_the_fediverse_identity_carries_on_under_the_same_handle(succession_ap_nest):
    """Outcome 48, its fediverse half: the `ap_*` family MOVES.

    The account enables federation; WebFinger and the actor document answer
    under its username. After the ceremony the successor's bridge list shows
    the same identity linked, and a remote server resolving the same
    ``acct:`` finds the same actor URL — no ``<handle>-2``, no stranded
    follower."""
    nest = succession_ap_nest
    user = _fresh_actor(nest)
    with _ws(nest, user) as ws:
        linked = ws.call(
            "fauna.bridges.link", {"bridge_id": AP_BRIDGE_ID, "mode": "enable", "params": {}}
        )
    assert linked["linked"] is True, linked
    actor_url = linked["identity"]["value"]
    username = actor_url.rsplit("/", 1)[-1]
    domain = linked["identity"]["display"].rsplit("@", 1)[-1]
    before = _ap_presence(nest, username, domain)
    assert before == {"webfinger": (200, actor_url), "actor": (200, username)}, before

    heir = _succeed(nest, user)

    with heir.client(nest) as heir_ws:
        entry = _bridge(heir_ws, AP_BRIDGE_ID)
    assert entry["linked"] is True, f"the successor must hold the AP identity: {entry!r}"
    assert (entry.get("identity") or {}).get("value") == actor_url, (
        f"the successor's AP identity must be the same actor: {entry!r}"
    )
    after = _ap_presence(nest, username, domain)
    assert after == before, (
        f"a remote server must resolve the same handle to the same actor after "
        f"the ceremony: before {before!r}, after {after!r}"
    )


def _link_bluesky(nest, far, user) -> None:
    """The real link: ``fauna.bridges.link`` starts the OAuth flow at the fake
    far end's PAR endpoint; its redirect lands on the nest's callback."""
    before = len(far.par_states)
    with _ws(nest, user) as ws:
        reply = ws.call(
            "fauna.bridges.link",
            {"bridge_id": BLUESKY_BRIDGE_ID, "mode": "oauth", "params": {"handle": far.handle}},
        )
    assert reply.get("redirect_url"), f"the oauth link starts a redirect: {reply!r}"
    assert len(far.par_states) > before, f"the link never reached PAR: {far.requests!r}"
    resp = requests.get(
        f"{nest['url']}/api/v1/bluesky/auth/callback",
        params={"code": "fake-code", "state": far.par_states[-1], "iss": far.issuer},
        allow_redirects=False,
        verify=False,
        timeout=30,
    )
    location = resp.headers.get("location", "")
    assert "result=linked" in location, (
        f"the callback must complete the link; status {resp.status_code}, location {location!r}"
    )


def _write_through(entry: dict):
    return next(
        (s.get("value") for s in entry.get("settings") or [] if s.get("key") == "write_through"),
        None,
    )


@pytest.mark.feature("take-your-account-back")
def test_a_bridge_to_an_outside_network_the_account_set_up_is_cut(
    succession_bluesky_nest, atproto_far_end
):
    """Outcome 48, its outside-network half: the `bluesky_*` family BURNS.

    The account links a Bluesky DID and turns cross-posting on. After the
    ceremony the successor's Bluesky bridge reads unlinked, with no identity
    and no cross-post setting carried — so nothing the successor posts goes
    to a repo it did not choose."""
    nest = succession_bluesky_nest
    user = _fresh_actor(nest)
    _link_bluesky(nest, atproto_far_end, user)
    with _ws(nest, user) as ws:
        ws.call(
            "fauna.bridges.set_settings",
            {"bridge_id": BLUESKY_BRIDGE_ID, "settings": {"write_through": 1}},
        )
        before = _bridge(ws, BLUESKY_BRIDGE_ID)
    assert before["linked"] is True and before.get("identity"), (
        f"the Bluesky link must stand before the ceremony: {before!r}"
    )
    assert _write_through(before) == 1, f"cross-posting must be on before: {before!r}"

    heir = _succeed(nest, user)

    with heir.client(nest) as heir_ws:
        after = _bridge(heir_ws, BLUESKY_BRIDGE_ID)
    assert after["linked"] is False and not after.get("identity"), (
        f"the successor must not inherit the Bluesky link: {after!r}"
    )
    assert _write_through(after) in (None, 0), (
        f"the cross-post setting must not be carried: {after!r}"
    )
