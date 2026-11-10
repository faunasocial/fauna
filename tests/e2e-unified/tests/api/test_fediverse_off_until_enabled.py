"""Until a member turns federation on, nothing of theirs is published to the
fediverse and fediverse servers find no account for them; a nest where nobody
has turned it on federates nothing. ``network-exposure.md`` § Target posture, per
surface (the *ActivityPub* row) owns the ruling: shipping the feature exposes
zero content — only per-actor enablement federates, in both directions — while
the routes (node-info, WebFinger, the inboxes, the instance actor) mount for
everyone. On a real ``fauna-nest`` binary built with ``activitypub``.

The member exists, has a handle and has posted before anything is asked. Every
surface a fediverse server uses to find or follow an account is then read the
way a remote server reads it: WebFinger for ``acct:<handle>@<domain>``, the
actor document, its outbox, the post's note URL, and a signed ``Follow`` to the
member's inbox from an in-test fediverse server (``helpers/fake_follower.py``).
Each answers 404 — the nest's indistinguishable "no such account" — and
node-info counts no federating account. Then the member turns federation on
over the same control plane the apps use (``fauna.bridges.link``) and the same
reads find them: WebFinger names the actor, the actor and outbox are served, a
post written now dereferences at its note URL, and the ``Follow`` is accepted
and answered with a signed ``Accept``.

Every assertion is a synchronous answer to a request (convention 14); the one
wait is the follower's ``Accept``, polled against a deadline by
``FakeFollower.wait_for``.

Witness of ``fediverse`` outcome 35.
"""

import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest

from drivers.port_util import find_free_port
from helpers.admin_wire import admit_adult
from helpers.ap_nest import enable_ap, start_ap_nest
from helpers.fake_follower import FakeFollower
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

AP_JSON = "application/activity+json"


def _get(nest, path: str, accept: str = AP_JSON):
    """``(status, raw_text)`` of an anonymous GET — never raises on a status."""
    req = urllib.request.Request(f"{nest['url']}{path}", headers={"Accept": accept})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, resp.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


def _webfinger_path(handle: str, domain: str) -> str:
    resource = urllib.parse.quote(f"acct:{handle}@{domain}", safe="")
    return f"/.well-known/webfinger?resource={resource}"


def _nodeinfo_users_total(nest) -> int:
    status, raw = _get(nest, "/.well-known/nodeinfo", accept="application/json")
    assert status == 200, f"nodeinfo discovery: HTTP {status} {raw[:300]!r}"
    href = json.loads(raw)["links"][0]["href"]
    path = urllib.parse.urlparse(href).path
    status, raw = _get(nest, path, accept="application/json")
    assert status == 200, f"nodeinfo document {path}: HTTP {status} {raw[:300]!r}"
    return json.loads(raw)["usage"]["users"]["total"]


def _follow(nest, follower, handle: str, actor_url: str, n: int):
    """POST a signed Follow to the member's inbox; ``(status, error_text)``."""
    try:
        code = follower.post_signed(
            f"{nest['url']}/ap/users/{handle}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Follow",
                "id": f"{follower.actor_uri}/activities/follow-{n}",
                "actor": follower.actor_uri,
                "object": actor_url,
            },
        )
        return code, ""
    except AssertionError as e:
        # `post_signed` raises on an HTTP error status, naming the code.
        return None, str(e)


@pytest.fixture()
def quiet_ap_nest(ap_binary, tmp_path_factory):
    """One fresh AP-federating nest on which nobody has turned federation on."""
    port = find_free_port()
    nest = start_ap_nest(
        ap_binary, tmp_path_factory.mktemp("ap_off"), port, f"127.0.0.1:{port}"
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture()
def follower():
    with FakeFollower(find_free_port()) as f:
        yield f


@pytest.mark.feature("fediverse")
def test_nothing_federates_until_the_member_turns_it_on(quiet_ap_nest, follower):
    nest = quiet_ap_nest
    domain = nest["domain"]
    handle = f"quiet{secrets.token_hex(3)}"
    member = admit_adult(nest, handle, token=True)
    now = int(time.time() * 1_000_000)
    early_post = ws_api.create_post(
        nest["port"],
        member,
        sign_and_encode_post(member["signing_key"], now, "posted before federating"),
    )
    # The URL the actor would have, were it federating (the username mints from
    # the handle at enablement — `activitypub.md` § Identity & keys).
    would_be_actor = f"https://{domain}/ap/users/{handle}"

    # ── off: no account, nothing published, nothing federating ──
    assert _nodeinfo_users_total(nest) == 0, (
        "node-info counts a federating account on a nest where nobody turned "
        "federation on"
    )
    off = {
        "webfinger": _get(nest, _webfinger_path(handle, domain), accept="application/jrd+json"),
        "actor": _get(nest, f"/ap/users/{handle}"),
        "outbox": _get(nest, f"/ap/users/{handle}/outbox"),
        "outbox page": _get(nest, f"/ap/users/{handle}/outbox?page=0"),
        "note": _get(nest, f"/ap/users/{handle}/notes/{early_post}"),
    }
    found = {k: v for k, v in off.items() if v[0] != 404}
    assert not found, (
        f"before {handle!r} turned federation on, a fediverse surface answered "
        f"other than 404 (status, body): {found}"
    )
    code, err = _follow(nest, follower, handle, would_be_actor, 1)
    assert code is None and "HTTP 404" in err, (
        f"a Follow of a member who never turned federation on was not refused as "
        f"no-such-account: status {code}, {err!r}"
    )

    # ── on: the same reads find the member ──
    username, actor_url = enable_ap(nest, member)
    assert username == handle and actor_url == would_be_actor, (
        f"enabling minted {username!r} at {actor_url!r}, not the handle-derived "
        f"{handle!r} at {would_be_actor!r} — the 'off' reads above probed the "
        f"wrong address"
    )
    assert _nodeinfo_users_total(nest) == 1

    status, raw = _get(nest, _webfinger_path(handle, domain), accept="application/jrd+json")
    assert status == 200, f"webfinger after enabling: HTTP {status} {raw[:300]!r}"
    wf = json.loads(raw)
    assert any(link.get("href") == actor_url for link in wf.get("links", [])), (
        f"webfinger does not name the actor {actor_url}: {wf}"
    )

    status, raw = _get(nest, f"/ap/users/{handle}")
    assert status == 200, f"actor after enabling: HTTP {status} {raw[:300]!r}"
    assert json.loads(raw).get("preferredUsername") == handle, raw

    status, raw = _get(nest, f"/ap/users/{handle}/outbox")
    assert status == 200, f"outbox after enabling: HTTP {status} {raw[:300]!r}"
    assert json.loads(raw).get("type") == "OrderedCollection", raw

    late_post = ws_api.create_post(
        nest["port"],
        member,
        sign_and_encode_post(member["signing_key"], now + 1, "posted while federating"),
    )
    status, raw = _get(nest, f"/ap/users/{handle}/notes/{late_post}")
    assert status == 200, (
        f"a public post written while federating does not dereference: HTTP "
        f"{status} {raw[:300]!r}"
    )
    assert json.loads(raw).get("id") == f"{actor_url}/notes/{late_post}", raw

    code, err = _follow(nest, follower, handle, actor_url, 2)
    assert code == 202, f"the inbox did not accept a Follow once federating: {code} {err!r}"
    entry = follower.wait_for("Accept")
    assert entry["activity"]["actor"] == actor_url, entry
