"""Every anonymous surface that answers "which version is this nest?" says only
``major.minor`` — save the one ruled exception, the health identity, which says
the exact build. ``federation.md`` § Security (the enumeration bullet) owns both
halves: the discovery surfaces are coarsened for anti-fingerprinting, and
``GET /api/v1/health`` serves the exact ``version``, ``commit`` and ``build_id``
to anyone by design (ruled 2026-10-09), so the release pipeline can tell one
artifact from another from outside the box with no credential. On a real
``fauna-nest`` binary.

The coarse surfaces walked, all reachable without signing in: ``fauna.nest.info``
and ``fauna.setup.status`` over WS-RPC, and the Nostr relay's NIP-11 information
document. The fediverse node-info document is the fourth; it is served only on
a nest federating over ActivityPub, and its coarsening is asserted where such a
nest is built (``test_activitypub_federation.py::TestActivityPubFederation::test_nodeinfo``).

The health leg pins the exception in the other direction: its ``version`` is the
full product version under the same ``major.minor`` the discovery surfaces
answer, and ``commit`` and ``build_id`` are present — so a session that gates the
fields without first replacing the deploy-verify signal (the ruled order) reds
here, not on a staging box.

Witness of ``upgrades-never-lose-data`` outcome 17.
"""

import json
import re
import ssl
import urllib.request

import pytest

from clients.ws_rpc_anon_client import WsRpcAnonClient
from tests.api.test_nostr_nest import nostr_home_nest  # noqa: F401 — fixture

pytestmark = pytest.mark.tier_3

MAJOR_MINOR = re.compile(r"^\d+\.\d+$")
FULL_VERSION = re.compile(r"^(\d+\.\d+)\.\d+")


def _get_json(url: str, path: str, accept: str = "application/json"):
    req = urllib.request.Request(url.rstrip("/") + path, headers={"Accept": accept})
    # A docker-mode nest answers over its self-signed TLS.
    context = ssl._create_unverified_context() if url.startswith("https://") else None
    with urllib.request.urlopen(req, context=context) as resp:
        return json.loads(resp.read())


@pytest.mark.feature("upgrades-never-lose-data")
def test_anonymous_version_answers_are_major_minor_and_only_health_tells_the_build(
    nostr_home_nest,  # noqa: F811
):
    nest = nostr_home_nest
    seen = {}
    with WsRpcAnonClient(nest["url"]) as anon:
        seen["fauna.nest.info"] = anon.call("fauna.nest.info", {})["version"]
        seen["fauna.setup.status"] = anon.call("fauna.setup.status", {})["version"]

    seen["NIP-11"] = _get_json(nest["url"], "/nostr/info", "application/nostr+json")[
        "version"
    ]

    exact = {k: v for k, v in seen.items() if not MAJOR_MINOR.match(str(v))}
    assert not exact, (
        f"an anonymous caller was told more than major.minor: {exact} (all: {seen})"
    )
    assert len(set(seen.values())) == 1, f"the coarse answers disagree: {seen}"

    # The one ruled exception: the health identity is the exact build.
    health = _get_json(nest["url"], "/api/v1/health")
    full = FULL_VERSION.match(str(health.get("version", "")))
    assert full, f"health did not answer the exact product version: {health}"
    assert full.group(1) == seen["fauna.nest.info"], (
        f"health's version {health['version']!r} is not under the anonymous "
        f"{seen['fauna.nest.info']!r}"
    )
    for field in ("commit", "build_id"):
        assert health.get(field), f"health serves no {field!r}: {health}"
