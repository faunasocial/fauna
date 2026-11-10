"""No surface a stranger reaches without signing in answers how many members and
posts the nest holds. ``network-exposure.md`` § Target posture, per surface (the
*Discovery* row) owns the ruling: discovery *should* reveal the nest's identity,
its coarsened version, which features and registration policy it offers and
whether a handle exists, and it *must not* reveal user or post counts — those
stay Admin-only. On a real ``fauna-nest`` binary.

The nest under test federates over ActivityPub, because the fediverse node-info
document — the one discovery surface whose upstream schema *carries* counts
(``usage.users`` and ``usage.localPosts``) — is served only there. It binds every
interface so the stranger can be a real non-loopback peer: every read below is
made against this box's own LAN address (``common.nest.lan_ipv4()``), which the
nest sees as a non-loopback source while the packets never leave the box.

The counts are made non-zero first — six admitted members (plus the admin) and
eleven posts — so a leaked figure would be a number the test knows. The nest's
own population figure is read where the ruling lets it be read: the
router-status report, which answers only a loopback caller (``worker.md`` §
Router Status Endpoint). That same report, asked from the LAN address, must be
refused with no body field — the stranger's leg of the same surface.

Surfaces walked, all reachable without signing in: ``fauna.nest.info``,
``fauna.setup.status``, ``fauna.handle.available`` and ``fauna.actor.by_handle``
over the anonymous WS-RPC connection; ``GET /api/v1/health``; the node-info
discovery link and the document it names; ``/internal/router-status``.

Node-info's ``usage.users.total`` is a published figure, not a population one:
it counts the accounts that turned federation on (each of which is already a
public fediverse actor). So it reads 0 here while seven members exist, and 1
once one of them federates — never the member count.

Witness of ``admin-dashboard`` outcome 10.
"""

import json
import re
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest

from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.nest import lan_ipv4
from drivers.port_util import find_free_port
from helpers.admin_wire import admit_adult
from helpers.ap_nest import enable_ap, start_ap_nest
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

MEMBERS = 6
POSTS = 11

# Keys that name a population or volume count in any vocabulary the nest has
# spoken (router-status) or upstream NodeInfo speaks. None may appear in a body
# a stranger reads, whatever its value.
FORBIDDEN_KEYS = {
    "current_users",
    "max_users",
    "user_count",
    "users_count",
    "member_count",
    "members_count",
    "post_count",
    "posts_count",
    "total_users",
    "total_posts",
    "localposts",
    "localcomments",
    "activemonth",
    "activehalfyear",
}

# A leaf whose path names one of these words is count-shaped; an integer there
# equal to a figure this test made is a leaked count.
COUNT_WORDS = re.compile(r"(?i)user|member|post|account|people|note|total|count|usage")


def _leaves(obj, path=()):
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield from _leaves(v, path + (str(k),))
    elif isinstance(obj, (list, tuple)):
        for i, v in enumerate(obj):
            yield from _leaves(v, path + (str(i),))
    else:
        yield path, obj


def _count_leaks(body, figures: set) -> list:
    """Every forbidden key, and every count-shaped integer leaf equal to one of
    ``figures``, as ``(path, value)``."""
    leaks = []
    for path, value in _leaves(body):
        if any(seg.lower() in FORBIDDEN_KEYS for seg in path):
            leaks.append(("/".join(path), value))
            continue
        if isinstance(value, bool) or not isinstance(value, int):
            continue
        # A cap (`max_free_users`) is a policy, not a count.
        if any(seg.lower().startswith(("max_", "min_")) for seg in path):
            continue
        if value in figures and any(COUNT_WORDS.search(seg) for seg in path):
            leaks.append(("/".join(path), value))
    return leaks


def _http_get(url: str, accept: str = "application/json"):
    """``(status, raw_text)`` — never raises on an HTTP error status."""
    req = urllib.request.Request(url, headers={"Accept": accept})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, resp.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


@pytest.fixture()
def lan_ap_nest(ap_binary, tmp_path_factory):
    """One AP-federating nest bound on every interface, plus its LAN base URL.

    Raises (via ``lan_ipv4``) on a box with no non-loopback IPv4: falling back
    to loopback would silently test the loopback-admitted branch of the
    router-status gate instead of the stranger's.
    """
    lan = lan_ipv4()
    port = find_free_port()
    nest = start_ap_nest(
        ap_binary,
        tmp_path_factory.mktemp("anon_counts"),
        port,
        f"127.0.0.1:{port}",
        bind_host="0.0.0.0",
    )
    try:
        yield nest, f"http://{lan}:{port}"
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.mark.feature("admin-dashboard")
def test_no_anonymous_surface_answers_how_many_members_and_posts(lan_ap_nest):
    nest, stranger = lan_ap_nest

    # ── make the counts non-zero, so a leak is a number we know ──
    members = []
    for i in range(MEMBERS):
        handle = f"cnt{secrets.token_hex(3)}{i}"
        members.append(admit_adult(nest, handle, token=True))
    now = int(time.time() * 1_000_000)
    for i in range(POSTS):
        author = members[i % MEMBERS]
        ws_api.create_post(
            nest["port"],
            author,
            sign_and_encode_post(author["signing_key"], now + i, f"count me {i}"),
        )

    # The admin-side truth: router-status answers a loopback caller with the
    # nest's population. It must count what we made (admin + members).
    status, raw = _http_get(f"{nest['url']}/internal/router-status")
    assert status == 200, (
        f"router-status refused its loopback caller: HTTP {status} {raw[:300]!r}"
    )
    population = json.loads(raw)["current_users"]
    assert population >= MEMBERS + 1, (
        f"router-status counts {population} users after admitting {MEMBERS} "
        f"members beside the admin — the figure this test hunts for is wrong: {raw}"
    )
    figures = {population, MEMBERS, POSTS}

    # ── the stranger's reads, from a non-loopback source ──
    bodies = {}
    with WsRpcAnonClient(stranger) as anon:
        bodies["fauna.nest.info"] = anon.call("fauna.nest.info", {})
        bodies["fauna.setup.status"] = anon.call("fauna.setup.status", {})
        bodies["fauna.handle.available (taken)"] = anon.call(
            "fauna.handle.available", {"handle": members[0]["handle"]}
        )
        bodies["fauna.handle.available (free)"] = anon.call(
            "fauna.handle.available", {"handle": f"free{secrets.token_hex(4)}"}
        )
        bodies["fauna.actor.by_handle"] = anon.call(
            "fauna.actor.by_handle", {"handle": members[0]["handle"]}
        )
    assert bodies["fauna.handle.available (taken)"].get("available") is False, (
        f"handle existence is ruled public, and a taken handle read as free: "
        f"{bodies['fauna.handle.available (taken)']}"
    )

    status, raw = _http_get(f"{stranger}/api/v1/health")
    assert status == 200, f"health did not answer the stranger: HTTP {status} {raw[:300]!r}"
    bodies["GET /api/v1/health"] = json.loads(raw)

    status, raw = _http_get(f"{stranger}/.well-known/nodeinfo")
    assert status == 200, f"nodeinfo discovery: HTTP {status} {raw[:300]!r}"
    bodies["GET /.well-known/nodeinfo"] = json.loads(raw)
    hrefs = [link["href"] for link in bodies["GET /.well-known/nodeinfo"]["links"]]
    assert hrefs, f"nodeinfo discovery names no document: {raw}"
    doc_path = urllib.parse.urlparse(hrefs[0]).path
    status, raw = _http_get(f"{stranger}{doc_path}")
    assert status == 200, f"nodeinfo document {doc_path}: HTTP {status} {raw[:300]!r}"
    nodeinfo = json.loads(raw)
    bodies[f"GET {doc_path}"] = nodeinfo

    leaks = {
        surface: found
        for surface, body in bodies.items()
        if (found := _count_leaks(body, figures))
    }
    assert not leaks, (
        f"an anonymous surface answered a member or post count (population "
        f"{population}, members {MEMBERS}, posts {POSTS}): {leaks}\nbodies: {bodies}"
    )

    # Node-info's one usage figure is the published-actor count: nobody has
    # turned federation on, so it is 0 while the nest holds `population` users.
    usage = nodeinfo.get("usage", {})
    assert usage.get("users", {}).get("total") == 0, (
        f"node-info's usage.users.total is not the federating-account count (0) "
        f"on a nest of {population} users: {nodeinfo}"
    )
    assert set(usage) <= {"users"} and set(usage.get("users", {})) <= {"total"}, (
        f"node-info's usage carries more than the published-actor total: {usage}"
    )

    # The stranger's leg of router-status: refused, with no body field.
    status, raw = _http_get(f"{stranger}/internal/router-status")
    assert status == 403, (
        f"router-status answered a non-loopback peer HTTP {status} (want 403, the "
        f"loopback-only gate): {raw[:500]!r}"
    )
    for field in ("current_users", "max_users", "nest_id", "version"):
        assert field not in raw, (
            f"router-status's refusal to a non-loopback peer carries {field!r}: {raw!r}"
        )

    # Once one member federates, the figure moves to 1 — what was published,
    # still never the population.
    enable_ap(nest, members[0])
    status, raw = _http_get(f"{stranger}{doc_path}")
    assert status == 200, f"nodeinfo document {doc_path}: HTTP {status} {raw[:300]!r}"
    after = json.loads(raw)
    assert after.get("usage", {}).get("users", {}).get("total") == 1, (
        f"with one of {population} users federating, node-info should count "
        f"exactly that one: {after}"
    )
    assert not _count_leaks(after, figures), (
        f"node-info leaked a count once a member federated: {after}"
    )
