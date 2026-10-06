"""E2E test: Bluesky OAuth residue + the unified `fauna.bridges.{list,link}`
contract for the Bluesky provider.

The consume-side Bluesky control-plane was ripped to WS-RPC / unified surfaces:

- `auth/status` / `auth/start` / `auth` (DELETE) — deprecated exact dups of the
  unified `fauna.bridges.{list,link,unlink}` kinds — were **deleted** (Commit C).
  Their behaviour now lives on the unified bridge WS-RPC surface. The two
  `fauna.bridges.*` tests below are the **replacement coverage** the deletion
  owed: `conformance_bridges_ui.rs` exercises a *fake* provider, so this is the
  first real-Bluesky-provider coverage of the `list`/`link` contract over the
  live socket (`bridges.md` § Linking a Bluesky account (OAuth);
  `api-layers.md` § Bluesky).
- `dm/*`, `feed/*`, `search/*`, `settings`, `interact/*` were deleted in Commit A
  (zero-consumer / unified-Conversations homes).
- `feed/thread/{uri}` + `thread?post_id=` moved onto the `bluesky.feed.thread`
  WS-RPC kind (Commit B; tier_3 `bins/fauna-nest/tests/conformance_bluesky.rs`).

What remains under `/api/v1/bluesky/` is OAuth 2.0 residue (far end is Bluesky's
OAuth server, not a Fauna binary): the callback and the client-metadata document
asserted here.

The **positive** `fauna.bridges.link` path (`mode:"oauth"` → reply carries
`redirect_url`) is not exercised *here* because it calls the real atproto
`oauth.authorize(handle)` (a live `.well-known/atproto-did` + DID-doc resolve).
It is covered instead by the tier_3 Rust conformance test
`bins/fauna-nest/tests/conformance_bluesky_link.rs`, which fakes the atproto far
end in-process at the `HttpClient::send_http` boundary (no network). This file
keeps the deterministic half: the `list` assertion and the negative `link`
(unknown mode).

**Which nest, and what changed under these tests (2026-09-02).** They used to
start a nest by hand, on a fixed port, with `--bluesky-public-url` — a flag no
deployment artifact passed, which is why all three were recorded PREMISE in
`test_nest_mode_axis.py::_NEST_BINARY_DISPOSITIONS`: the flag was not incidental
setup, it was the subject, and no routed form could have asserted what they
assert. **That premise was a product defect, and it is fixed**: the flag is
deleted and the OAuth `client_id` derives from the identity domain the nest
learns at CLAIM.

So the argv is gone, the hand-rolled `Popen` is gone, and the nests below are
ordinary dedicated nests separated by one thing only — whether the claim carries
a domain. What they still need is the `bluesky` **cargo feature**, since
`BlueskyProvider` registers under `#[cfg(feature = "bluesky")]` and the harness's
default nest binary builds `test-hooks,nostr` (`common.nest.build_node` — every
provider adds startup surface, so a test builds what it exercises). Hence they
name `bluesky_nest_binary` to the provider seam (`_start_dedicated_nest`'s
`binary=`, `testing.md` § Default app and nest mode, ruling (1)): standalone
builds it, and a container run serves the shipped image, which builds all three
providers — so these run against the real artifact in every mode that owns its
nest (2026-10-05).
"""

import json
import ssl
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3

#: The domain `domained_bluesky_nest` is claimed onto, and the OAuth public URL
#: the nest derives from it. One constant, so the fixture and the assertion can
#: never drift onto different domains.
HOSTED_DOMAIN = "bsky-oauth.test"
HOSTED_URL = f"https://{HOSTED_DOMAIN}"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def domained_bluesky_nest(request, nest_mode, tmp_path_factory):
    """A nest with the `bluesky` provider compiled in, claimed onto a real
    domain — so it derives an OAuth client and offers the bridge.

    `claim_domain` is a WIRE act, not a boot flag: it rides the claim as
    `mail_domain`, becomes the primary `mail_domains` row, and that row IS the
    deployment identity. It is the whole difference between this fixture and
    `domainless_bluesky_nest` below.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "bluesky-oauth-domained",
        binary="bluesky_nest_binary", claim_domain=HOSTED_DOMAIN,
    )
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def domainless_bluesky_nest(request, nest_mode, tmp_path_factory):
    """The same binary, claimed with no domain — `handle_domain()` stays the
    `localhost` placeholder, nothing derives, and the bridge is honestly
    unavailable. The counter-witness that stops the pair above reading as
    "Bluesky is simply always on"."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "bluesky-oauth-domainless",
        binary="bluesky_nest_binary",
    )
    yield nest
    cleanup()


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_bluesky_client_metadata_served(domained_bluesky_nest):
    """The .well-known/atproto-oauth-client endpoint returns valid metadata, and
    the `client_id` it advertises is **the nest's own identity domain** — not
    the address the test happens to dial it on.

    That distinction is the whole of the derivation. The document is fetched
    over the loopback URL this nest is reachable at, and every URL *inside* it
    is `https://fauna.test/...` because that is the domain the box was claimed
    onto and the name Bluesky's authorization server would have to resolve.

    OAuth 2.0 residue (served *to* Bluesky's OAuth server) — stays HTTP.
    """
    req = urllib.request.Request(
        f"{domained_bluesky_nest['url']}/.well-known/atproto-oauth-client",
        method="GET",
    )
    with urllib.request.urlopen(req, context=_tls_context(domained_bluesky_nest)) as resp:
        assert resp.status == 200
        meta = json.loads(resp.read())

    assert meta.get("client_id") == f"{HOSTED_URL}/.well-known/atproto-oauth-client", (
        f"the client_id is the claimed identity domain, self-referencing the "
        f"document being served; got {meta.get('client_id')!r}"
    )
    assert meta["redirect_uris"], "an OAuth client must publish a redirect URI"
    assert meta["redirect_uris"][0].startswith(HOSTED_URL), (
        f"the redirect URI rides the same derived origin as the client_id; got "
        f"{meta['redirect_uris'][0]!r}"
    )
    assert "callback" in meta["redirect_uris"][0]


def test_bluesky_client_metadata_absent_without_a_domain(domainless_bluesky_nest):
    """The other side of the derivation: a nest with no public identity domain
    serves no client-metadata document, because there is no name to put in it.

    503 rather than 404, and the counterpart to the `available: false` row the
    `list` test below pins: the document is not permanently absent, it is
    absent *until this box is claimed onto a domain*.
    """
    req = urllib.request.Request(
        f"{domainless_bluesky_nest['url']}/.well-known/atproto-oauth-client",
        method="GET",
    )
    with pytest.raises(urllib.error.HTTPError) as ei:
        urllib.request.urlopen(req, context=_tls_context(domainless_bluesky_nest))
    assert ei.value.code == 503, (
        f"a domainless nest has no OAuth client identity to publish; got "
        f"HTTP {ei.value.code}"
    )


# ---------------------------------------------------------------------------
# Unified bridge surface — fauna.bridges.{list,link} for the Bluesky provider
# ---------------------------------------------------------------------------

def _tls_context(nest):
    """An SSL context for this nest's own listener, or `None` when it serves
    plain HTTP.

    A harness nest is plain HTTP under `FAUNA_INSECURE_DISABLE_TLS`; the docker
    provider's is always HTTPS on its self-signed floor. Verification is off for
    the reason `common.nest.wait_for_node` turns it off: the floor cert is
    self-signed and name-mismatched by design, and client trust is
    channel-binding rather than WebPKI (`security.md` § Transport trust). The
    document is fetched from the box itself, so no trust decision under test is
    bypassed here."""
    if not nest["url"].startswith("https://"):
        return None
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _admin_client(nest) -> WsRpcAdminClient:
    """An authenticated WS-RPC client for the nest's admin actor. The admin is
    a registered actor, and `fauna.bridges.{list,link}` are User-class (Admin
    is also permitted), so it can drive both kinds; `link` is caller-scoped to
    this actor."""
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=admin["actor_id_bytes"],
        signing_key=bytes(admin["signing_key"]),
    )


def _find_bridge(bridges, bridge_id):
    return next((b for b in bridges if b["id"] == bridge_id), None)


@pytest.mark.feature("bridges")
def test_bluesky_listed_with_oauth_link_mode(domained_bluesky_nest):
    """`fauna.bridges.list` surfaces Bluesky as an available, unlinked bridge
    whose sole link mode is OAuth (`client_action: oauth_redirect`, collecting a
    handle) — the contract the deleted `auth/status` twin used to expose.

    `available` is now a statement about the *claim*: this nest was claimed onto
    a public domain, so it derives a client. Its domainless twin below is the
    counter-witness, and the pair is what stops this reading as "Bluesky is
    always on"."""
    client = _admin_client(domained_bluesky_nest)
    with client:
        bridges = client.call("fauna.bridges.list", {})["bridges"]

    bsky = _find_bridge(bridges, "bluesky")
    assert bsky is not None, "Bluesky provider must appear in fauna.bridges.list"
    assert bsky["available"] is True, "OAuth-configured node => Bluesky available"
    assert bsky["linked"] is False, "a fresh admin actor has no linked Bluesky account"

    modes = bsky["link_modes"]
    assert modes, "an available, unlinked Bluesky bridge advertises its link modes"
    oauth = next((m for m in modes if m["mode"] == "oauth"), None)
    assert oauth is not None, "Bluesky links via an `oauth` mode"
    assert oauth["client_action"] == "oauth_redirect"
    assert any(f["key"] == "handle" for f in oauth["fields"]), (
        "the OAuth link mode collects a Bluesky handle"
    )


def test_bluesky_link_rejects_unknown_mode(domained_bluesky_nest):
    """`fauna.bridges.link` with a mode the provider doesn't offer is rejected
    with the wire-stable `fauna.bridges.invalid_mode` — the deterministic half
    of the link contract (the positive `oauth` path hits the real atproto
    `authorize()` resolve, deferred to the fake-atproto harness follow-on)."""
    client = _admin_client(domained_bluesky_nest)
    with client:
        with pytest.raises(RpcCallError) as ei:
            client.call(
                "fauna.bridges.link",
                {"bridge_id": "bluesky", "mode": "sms", "params": {}},
            )
    assert ei.value.code == "fauna.bridges.invalid_mode", (
        f"unknown link mode must surface invalid_mode, got {ei.value.code!r}"
    )
