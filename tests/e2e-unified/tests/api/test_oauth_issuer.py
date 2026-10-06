"""tier_3 API e2e: the nest's OAuth issuer surfaces — ``/oauth/jwks`` and the
two discovery documents (TP5 slice S1), and ``/oauth/par``, the first of the
request-taking endpoints (S2).

``docs/goal/behavior/authorization-server.md`` § The issuer rules that the
authorization server is "a nest-native surface on the nest's own domain" and
"is up whenever the nest is up";
``docs/goal/architecture/key-material-hierarchy.md`` § Audience: deployment
infrastructure → *Issuer signing key* rules that the public half "is served at
``/oauth/jwks`` as a key **set** with ``kid``s".

**These tests run on the PLAIN nest, and that is load-bearing.** The issuer
moved to nest custody precisely so it stops depending on an optional bridge
being enrolled and compiled, so the default ``standalone`` mode's binary — no
``bluesky``, no bridge features — is the flavor that proves it. Reaching for
``bluesky_nest_binary`` here would test the one configuration the move exists
to make unnecessary.

That property is the *standalone* run's to prove, and it still does: the two
fixtures below go through ``_start_dedicated_nest``, whose standalone provider
is ``_make_nest`` with a lazily-resolved ``nest_binary`` — byte-identical to
taking the binary as a fixture parameter, which is what they used to do. Taking
it as a parameter is what excluded the whole file from ``--nest docker``, and it
bought nothing here: no test below reads a host path, inspects the nest's files
or touches the process, so there was never anything a container could not
witness. Under ``--nest docker`` the same assertions run against the shipped
image, which additionally proves the issuer is up in the artifact that ships —
a strictly wider claim, not a substitute for the standalone one.

Two arms, because the issuer identifier *is* the nest's apex domain:

* claimed onto a domain → the documents answer and name that domain;
* no domain claimed → all three answer ``503``, not ``404``. The surface is
  real; its prerequisite has not arrived. Same shape as the sibling
  ``/.well-known/atproto-oauth-client`` (``test_bluesky_oauth.py``) and
  ``/.well-known/carddav``, and it self-heals: a post-boot claim re-points the
  live apex with no restart.

The admin half — ``fauna.oauth.{issuer_key_status,rotate_issuer_key}`` — is
covered below on the same claimed nest, because a rotation is only meaningful
against the JWKS it changes: the assertion that matters is that the OLD key is
still served afterwards, which no unit test of the rotation alone can make.

Not covered here, and deliberately: the PDS protected-resource re-point.
``tests/api/`` spins no PDS bridge, so that assertion belongs beside
``test_atproto_pds_consent.py``'s ``consent_bridge`` fixture, and the re-point
itself is deferred to S2, which owns the bridge-side retirement.

Nor is the https ``client_id`` fetch: resolving one dials the URL through the
SSRF guard, which correctly refuses every address a test can publish on, so its
decisions — the cache's two TTLs, which failures are remembered, when a
``jwks_uri`` is followed — are unit-tested behind a fake fetcher in
``oauth_as_client`` instead. What runs here is the loopback development client,
which resolves with no network at all and therefore exercises the endpoint's own
ordering rather than a fixture's.
"""

import json
import ssl
import urllib.error
import urllib.parse
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import port_base_url

pytestmark = pytest.mark.tier_3

#: The domain `domained_issuer_nest` is claimed onto, and the issuer identifier
#: the nest must derive from it. One constant so fixture and assertion cannot
#: drift onto different domains.
ISSUER_DOMAIN = "oauth-issuer.test"
ISSUER_URL = f"https://{ISSUER_DOMAIN}"

DISCOVERY_PATHS = (
    "/.well-known/oauth-authorization-server",
    "/.well-known/openid-configuration",
)

#: Every member that describes the authorization flow — the six endpoint URLs
#: and the rules for using them. They arrive together or not at all: a document
#: naming a door describes what is behind it, and one describing what is behind
#: a door it does not name is the same defect one step smaller.
FLOW_MEMBERS = (
    "authorization_endpoint",
    "token_endpoint",
    "pushed_authorization_request_endpoint",
    "revocation_endpoint",
    "device_authorization_endpoint",
    "backchannel_authentication_endpoint",
    "backchannel_token_delivery_modes_supported",
    "scopes_supported",
    "response_types_supported",
    "grant_types_supported",
    "code_challenge_methods_supported",
    "token_endpoint_auth_methods_supported",
    "token_endpoint_auth_signing_alg_values_supported",
    "revocation_endpoint_auth_methods_supported",
    "dpop_signing_alg_values_supported",
    "require_pushed_authorization_requests",
    "client_id_metadata_document_supported",
    "authorization_response_iss_parameter_supported",
    "subject_types_supported",
    # OIDC (TP6): the UserInfo door and the ID token's signing rule.
    "userinfo_endpoint",
    "id_token_signing_alg_values_supported",
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def domained_issuer_nest(request, nest_mode, tmp_path_factory):
    """A nest claimed onto a real domain — so it has an issuer identity."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "oauth-issuer-domained",
        claim_domain=ISSUER_DOMAIN,
    )
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def domainless_issuer_nest(request, nest_mode, tmp_path_factory):
    """The same nest with no domain claimed — nothing to call itself."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "oauth-issuer-domainless",
    )
    yield nest
    cleanup()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _tls_context(nest):
    """An SSL context for this nest's own listener, or ``None`` on plain HTTP.

    Verification is off for the reason ``common.nest.wait_for_node`` turns it
    off: the floor cert is self-signed and name-mismatched by design. The
    document is fetched from the box itself, so no trust decision under test is
    bypassed.
    """
    if not nest["url"].startswith("https://"):
        return None
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _get_json(nest, path):
    req = urllib.request.Request(f"{nest['url']}{path}", method="GET")
    with urllib.request.urlopen(req, context=_tls_context(nest)) as resp:
        assert resp.status == 200, f"{path} answered HTTP {resp.status}"
        return json.loads(resp.read())


def _expect_status(nest, path, code):
    req = urllib.request.Request(f"{nest['url']}{path}", method="GET")
    with pytest.raises(urllib.error.HTTPError) as ei:
        urllib.request.urlopen(req, context=_tls_context(nest))
    assert ei.value.code == code, (
        f"{path} should answer HTTP {code}; got HTTP {ei.value.code}"
    )


# ---------------------------------------------------------------------------
# The claimed nest
# ---------------------------------------------------------------------------

def test_both_discovery_documents_name_the_nests_own_domain(domained_issuer_nest):
    """Both documents answer, and the issuer they advertise is the domain the
    box was claimed onto — not the loopback address the test dials it on.

    That distinction is the whole point: ``iss`` is what a client pins, so it
    has to be the name the world would resolve, never the transport the
    document happened to arrive over.
    """
    docs = {p: _get_json(domained_issuer_nest, p) for p in DISCOVERY_PATHS}

    for path, doc in docs.items():
        assert doc["issuer"] == ISSUER_URL, (
            f"{path} must advertise the claimed identity domain; got "
            f"{doc.get('issuer')!r}"
        )
        assert doc["jwks_uri"] == f"{ISSUER_URL}/oauth/jwks", (
            f"{path}'s jwks_uri must hang off the issuer; got "
            f"{doc.get('jwks_uri')!r}"
        )

    first, second = (docs[p] for p in DISCOVERY_PATHS)
    assert first == second, (
        "the two discovery paths describe ONE authorization server; a client "
        f"that reads one and pins the other must not see a different document: "
        f"{first!r} vs {second!r}"
    )


def test_discovery_advertises_no_endpoint_the_nest_does_not_serve(domained_issuer_nest):
    """The document advertises what the nest SERVES, and nothing more.

    The rule that governed the whole port: each endpoint joins the document in
    the slice that makes it answer, because advertising one early sends clients
    to a ``404`` they cannot tell apart from a broken deployment — worse than an
    absent key, which reads honestly as "this server does not do that (yet)".
    ``userinfo_endpoint`` joined with OIDC and the two consent starts with TP9,
    so every endpoint member the document names today is one this nest mounts;
    the pin is that it names no OTHER.
    """
    doc = _get_json(domained_issuer_nest, DISCOVERY_PATHS[0])
    mounted = {
        "authorization_endpoint",
        "token_endpoint",
        "pushed_authorization_request_endpoint",
        "revocation_endpoint",
        "userinfo_endpoint",
        "device_authorization_endpoint",
        "backchannel_authentication_endpoint",
    }
    advertised = {member for member in doc if member.endswith("_endpoint")}
    assert advertised <= mounted, (
        f"discovery advertised {sorted(advertised - mounted)!r}, which no nest "
        f"route answers: {doc!r}"
    )


def test_discovery_advertises_the_flow_the_resource_server_honours(
    domained_issuer_nest,
):
    """Discovery advertises the whole flow, and every door it names is mounted.

    "Only advertise what the nest serves" means the token is HONOURED, not that
    the route answers (``authorization-server.md`` § The issuer → *The staging
    rule covers both documents, and one pin enforces it*). The deployment's PDS
    names this issuer in its protected-resource document and verifies against
    the key set this nest feeds it, so the flow is advertised — the staging gate
    that withheld it retired with the bridge's own authorization server.

    Each POST door is dialed with a GET and must answer ``405`` — mounted,
    refusing the method — never the ``404`` of an advertised door nothing
    serves. (``/oauth/authorize`` is a GET and answers its own way.)
    """
    doc = _get_json(domained_issuer_nest, DISCOVERY_PATHS[0])
    missing = [m for m in FLOW_MEMBERS if m not in doc]
    assert not missing, (
        "the deployment's resource server honours this issuer, so discovery "
        f"must advertise the whole flow; missing {missing!r} from {sorted(doc)!r}"
    )

    for member in (
        "pushed_authorization_request_endpoint",
        "token_endpoint",
        "revocation_endpoint",
        "device_authorization_endpoint",
        "backchannel_authentication_endpoint",
    ):
        path = doc[member].removeprefix(ISSUER_URL)
        request = urllib.request.Request(f"{domained_issuer_nest['url']}{path}", method="GET")
        try:
            with urllib.request.urlopen(
                request, context=_tls_context(domained_issuer_nest)
            ) as resp:
                status = resp.status
        except urllib.error.HTTPError as e:
            status = e.code
        assert status == 405, (
            f"{member} advertises {path}; a GET there should be refused as "
            f"method-not-allowed by a mounted route, got {status}"
        )


def test_discovery_carries_the_whole_flow_and_its_rules(domained_issuer_nest):
    """The flow is all-or-nothing, and its rules come with it.

    Why each is load-bearing: ``require_pushed_authorization_requests`` because
    PAR is mandatory and a document that merely OFFERS the endpoint has not said
    so; ``S256`` alone because ``plain`` is a downgrade a client would take if
    offered; and ``revocation_endpoint_auth_methods_supported`` because RFC 8414
    §2 defaults that member to ``client_secret_basic`` — a credential class this
    AS does not implement — so silence would advertise a method every request
    using it would be refused for.
    """
    doc = _get_json(domained_issuer_nest, DISCOVERY_PATHS[0])
    present = [m for m in FLOW_MEMBERS if m in doc]
    assert present == list(FLOW_MEMBERS), (
        "the flow's members arrive together; this document names only "
        f"{present!r}, so a client learns some of the rules for a flow it "
        "cannot start (or can start under rules it was not told)"
    )

    for member, path in (
        ("pushed_authorization_request_endpoint", "/oauth/par"),
        ("authorization_endpoint", "/oauth/authorize"),
        ("token_endpoint", "/oauth/token"),
        ("revocation_endpoint", "/oauth/revoke"),
        ("device_authorization_endpoint", "/oauth/device_authorization"),
        ("backchannel_authentication_endpoint", "/oauth/bc-authorize"),
    ):
        assert doc[member] == f"{ISSUER_URL}{path}", (
            f"{member} must hang off the issuer; got {doc[member]!r}"
        )
    assert doc["require_pushed_authorization_requests"] is True, doc
    assert doc["code_challenge_methods_supported"] == ["S256"], doc
    assert doc["dpop_signing_alg_values_supported"] == ["ES256"], doc
    assert doc["grant_types_supported"] == [
        "authorization_code",
        "refresh_token",
        "urn:ietf:params:oauth:grant-type:device_code",
        "urn:openid:params:grant-type:ciba",
        # The same-device handoff's poll (authorization-server.md § Consent →
        # *How the same-device handoff is built*).
        "urn:fauna:params:grant-type:handoff",
    ], doc
    # Poll mode only: ping and push would have this nest dial a client-named URL.
    assert doc["backchannel_token_delivery_modes_supported"] == ["poll"], doc
    for member in (
        "token_endpoint_auth_methods_supported",
        "revocation_endpoint_auth_methods_supported",
    ):
        assert doc[member] == ["none", "private_key_jwt"], (
            f"{member} must name the two methods this AS implements and no "
            f"other; got {doc[member]!r}"
        )


def test_jwks_serves_a_key_set_with_kids(domained_issuer_nest):
    """``/oauth/jwks`` carries at least one usable ES256 key, with a ``kid``.

    The ``kid`` is what a token names to say which key signed it, so a key set
    without one is unusable however well-formed the rest is.
    """
    jwks = _get_json(domained_issuer_nest, "/oauth/jwks")
    keys = jwks["keys"]
    assert keys, f"the issuer must publish at least one key; got {jwks!r}"

    for key in keys:
        assert key["kty"] == "EC", key
        assert key["crv"] == "P-256", key
        assert key["alg"] == "ES256", key
        assert key["use"] == "sig", key
        assert key["kid"], f"every published key needs a kid: {key!r}"
        assert key["x"] and key["y"], f"a public key needs both coordinates: {key!r}"


def test_the_published_key_is_stable_across_reads(domained_issuer_nest):
    """The key is minted ONCE, at boot, and every read converges on it — no
    read hands out a new key.

    This is the failure that would look healthy: every read returns a
    well-formed JWKS, so the endpoint passes any single-shot check, while every
    token minted a moment ago verifies against a ``kid`` the set no longer
    carries. Two reads, same ``kid``, is the cheapest witness that the set is
    *stored* rather than minted per read.
    """
    first = _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]
    second = _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]

    assert [k["kid"] for k in first] == [k["kid"] for k in second], (
        f"the key set must be stable across reads; got {first!r} then {second!r}"
    )
    assert first == second, (
        f"the published coordinates must not change either; got {first!r} then "
        f"{second!r}"
    )


# ---------------------------------------------------------------------------
# The domainless nest
# ---------------------------------------------------------------------------

def test_a_domainless_nest_answers_503_on_every_issuer_surface(domainless_issuer_nest):
    """No claimed domain → no issuer identity → ``503``, on all three paths.

    ``503`` and not ``404``: the routes are mounted and the code is compiled
    (this is the plain binary), so the surface exists — what is missing is the
    domain that would name it. Inventing one would be worse than refusing: an
    ``iss`` of ``https://localhost`` or an IP is a value clients pin, and every
    token minted under it breaks the moment a real domain is claimed, with
    nothing to re-point them to.
    """
    for path in (*DISCOVERY_PATHS, "/oauth/jwks"):
        _expect_status(domainless_issuer_nest, path, 503)


# ---------------------------------------------------------------------------
# The admin surface — `fauna.oauth.*`
# ---------------------------------------------------------------------------

def _admin_client(nest):
    admin = nest["admin"]
    return WsRpcAdminClient(
        port_base_url(nest["port"]),
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def test_the_status_kind_names_the_key_the_jwks_is_serving(domained_issuer_nest):
    """``fauna.oauth.issuer_key_status`` reports the same key set as the JWKS.

    Two surfaces onto one key set is exactly where they can disagree — an admin
    acting on a status read that describes a different set than clients fetch is
    worse off than one with no status at all. So the assertion is the *join*,
    not either side's own well-formedness.
    """
    with _admin_client(domained_issuer_nest) as client:
        status = client.call("fauna.oauth.issuer_key_status", {})

    published = [k["kid"] for k in _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]]

    assert [k["kid"] for k in status["keys"]] == published, (
        f"status and JWKS must describe one key set; status {status['keys']!r} "
        f"vs JWKS {published!r}"
    )
    assert status["active_kid"] == published[0], (
        f"the signer is the JWKS's first key; got {status['active_kid']!r} "
        f"against {published!r}"
    )
    assert status["retirement_horizon_secs"] > 0, (
        "the admin needs the horizon to know when a rotated-out key goes away; "
        f"got {status['retirement_horizon_secs']!r}"
    )


@pytest.mark.feature("admin-nest")
def test_rotation_adds_a_key_and_keeps_serving_the_old_one(domained_issuer_nest):
    """``fauna.oauth.rotate_issuer_key`` ADDS — it does not replace.

    This is the whole reason the issuer holds a key *set*. A rotation that
    replaced the key in place would invalidate every token minted in the seconds
    before it, against a JWKS that no longer carries their ``kid`` — a
    self-inflicted outage on an operation an admin is told is safe. So the
    load-bearing assertion is not "a new kid appeared" but "the OLD kid is still
    published afterwards", which is the half that breaks silently.

    Runs last against this module's nest, deliberately: it mutates the key set
    the earlier tests read.
    """
    before = [k["kid"] for k in _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]]
    assert before, "nothing to rotate away from"
    outgoing = before[0]

    with _admin_client(domained_issuer_nest) as client:
        rotated = client.call("fauna.oauth.rotate_issuer_key", {})
        status = client.call("fauna.oauth.issuer_key_status", {})

    new_kid = rotated["kid"]
    assert new_kid != outgoing, (
        f"a rotation must mint a NEW key; got the incumbent {new_kid!r} back"
    )
    assert rotated["rotated_at"] > 0, f"the rotation instant is missing: {rotated!r}"

    after = [k["kid"] for k in _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]]
    assert new_kid in after, f"the new key must be published: {after!r}"
    assert outgoing in after, (
        f"the OUTGOING key must still be served until its horizon — every token "
        f"minted before the rotation names it; JWKS after = {after!r}"
    )
    assert after[0] == new_kid, (
        f"the signer must lead the set so a client reading only the first entry "
        f"gets the current key; got {after!r}"
    )

    assert status["active_kid"] == new_kid, (
        f"status must name the new signer; got {status['active_kid']!r}"
    )
    retired = {k["kid"]: k.get("retired_at") for k in status["keys"]}
    assert retired.get(new_kid) is None, (
        f"the signer is not retired; got {retired!r}"
    )
    assert retired.get(outgoing), (
        f"the outgoing key must carry the instant it was retired, which is what "
        f"dates its horizon; got {retired!r}"
    )


@pytest.mark.feature("admin-nest")
def test_a_forced_rotation_drops_every_other_key_from_the_jwks_at_once(
    domained_issuer_nest,
):
    """``fauna.oauth.force_rotate_issuer_key`` DROPS — no horizon wait.

    The compromise response (``authorization-server.md`` § The issuer → *Two
    rotation arms*). A thief holding a leaked scalar chooses ``exp`` themselves,
    so the access-token lifetime bounds nothing they mint; the only thing that
    does is the leaked ``kid`` leaving the served JWKS, and the ordinary arm
    above keeps it there for the whole horizon. So the load-bearing assertion
    is the mirror image of the rotation test's: after the forced arm, the JWKS
    the very next read serves carries the new signer and **nothing else** — the
    incumbent AND the still-inside-its-horizon retired key are both gone, at
    the same instant, with no sweep and no wait.

    Runs after the ordinary rotation test on purpose: that leaves the set with
    two keys (signer + retired-but-served), which is exactly the state a leak
    of the store hands an attacker — both must go, not just the signer.
    """
    with _admin_client(domained_issuer_nest) as client:
        before = _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]
        if len(before) < 2:
            # Selected on its own (a `-k` run), the ordinary rotation above has
            # not run: put the set in the two-key state this test is about
            # rather than depending on collection order.
            client.call("fauna.oauth.rotate_issuer_key", {})
            before = _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]
        before_kids = sorted(k["kid"] for k in before)
        assert len(before_kids) >= 2, (
            f"precondition: an ordinary rotation leaves an outgoing key served; got {before_kids!r}"
        )

        forced = client.call("fauna.oauth.force_rotate_issuer_key", {})
        status = client.call("fauna.oauth.issuer_key_status", {})

    new_kid = forced["kid"]
    assert new_kid not in before_kids, (
        f"the forced arm must mint a NEW key; got an existing one back: {new_kid!r}"
    )
    assert forced["rotated_at"] > 0, f"the rotation instant is missing: {forced!r}"
    assert sorted(forced["dropped_kids"]) == before_kids, (
        f"every key that existed before the call is reported dropped — the signer "
        f"and the retired-but-served one alike; reported {forced['dropped_kids']!r} "
        f"vs served-before {before_kids!r}"
    )

    after = [k["kid"] for k in _get_json(domained_issuer_nest, "/oauth/jwks")["keys"]]
    assert after == [new_kid], (
        f"the very next JWKS read must carry the new signer and NOTHING else — a "
        f"leaked kid still served here is the 20-minute forgery window the forced "
        f"arm exists to close; got {after!r}"
    )
    assert status["active_kid"] == new_kid, (
        f"status must name the new signer; got {status['active_kid']!r}"
    )
    assert [k["kid"] for k in status["keys"]] == [new_kid], (
        f"status and JWKS must describe one key set of one key; got {status['keys']!r}"
    )


def test_a_forced_session_secret_rotation_replaces_the_generation_it_reports(
    domained_issuer_nest,
):
    """``fauna.oauth.force_rotate_session_secret`` re-mints the second signer.

    The forced issuer-key arm above reaches one of the two signers in the
    store its own threat model names; this is the door for the other — the
    HS256 secret every OAuth refresh token is MACed under
    (``authorization-server.md`` § The issuer → *Two rotation arms*). Against
    this plain nest no real refresh token can be minted (a grant needs the
    bridge's ATProto identity — ``test_atproto_pds_consent.py`` holds the
    live-token proof), so what is asserted here is the *row*: the second call
    reports, as the generation it replaced, exactly the instant the first call
    minted. A door that answered success without touching the row would pass
    a single call and fail this pair.
    """
    with _admin_client(domained_issuer_nest) as client:
        first = client.call("fauna.oauth.force_rotate_session_secret", {})
        second = client.call("fauna.oauth.force_rotate_session_secret", {})

    assert first["rotated_at"] > 0, f"the rotation instant is missing: {first!r}"
    assert second["replaced_minted_at"] == first["rotated_at"], (
        "the second rotation must replace the generation the first one minted — "
        f"reported {second['replaced_minted_at']!r}, expected {first['rotated_at']!r}"
    )
    assert second["rotated_at"] >= first["rotated_at"], (second, first)


# ---------------------------------------------------------------------------
# `/oauth/par` — pushed authorization requests (TP5 S2)
# ---------------------------------------------------------------------------
#
# These run against the same plain nest as everything above, for the same
# reason: the endpoint mounts unconditionally, so the flavor with no bridge
# features compiled is the one that proves it.
#
# The client is the **loopback development client** (`http://localhost`), which
# `oauth_client::plan_client_id` synthesizes with no network at all. That is
# what makes a full happy-path PAR testable here: an https `client_id` would
# have to be fetched, and the SSRF guard correctly refuses every address a test
# can publish on. The https path's own decisions are unit-tested behind a fake
# fetcher in `oauth_as_client`.

PAR_PATH = "/oauth/par"

#: The `htu` a conformant client proves over: the endpoint URL the nest
#: ADVERTISES, built from its claimed domain — never the loopback address the
#: test dials. See `helpers/dpop.py`'s note.
PAR_ENDPOINT_URL = f"{ISSUER_URL}{PAR_PATH}"

#: A loopback client declaring one scope and one redirect target. Both must be
#: present for `plan_par_request` to accept the request below.
LOOPBACK_CLIENT_ID = "http://localhost?scope=atproto&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"

FORM_HEADERS = {"Content-Type": "application/x-www-form-urlencoded"}


def _post_par(nest, body, headers=None):
    """POST the PAR endpoint, returning (status, headers, parsed-json-or-None).

    A refusal is a normal answer here, not an exception, so the ``HTTPError`` is
    caught and unwrapped: every assertion below is about the *content* of a
    refusal — its error code and the nonce it hands back — which is exactly what
    an unhandled exception would throw away.

    The headers come back as the ``HTTPMessage`` itself and **not** as a
    ``dict``. Header names are case-insensitive and this server sends them
    lowercased, so ``dict(resp.headers)["DPoP-Nonce"]`` misses a header that is
    plainly there — a false failure that looks exactly like a real one.
    ``HTTPMessage.get`` is case-insensitive, which is the whole reason to keep
    it.
    """
    request = urllib.request.Request(
        f"{nest['url']}{PAR_PATH}",
        data=body.encode() if isinstance(body, str) else body,
        method="POST",
        headers=headers or {},
    )
    try:
        with urllib.request.urlopen(request, context=_tls_context(nest)) as resp:
            return resp.status, resp.headers, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            parsed = json.loads(raw)
        except ValueError:
            parsed = None
        return e.code, e.headers, parsed


def _form(**fields):
    return urllib.parse.urlencode(fields)


def _valid_par_form(client_id=LOOPBACK_CLIENT_ID, scope="atproto", **overrides):
    """The parameter set `plan_par_request` accepts, so a test that is about
    one refusal changes exactly the one parameter it is about."""
    fields = dict(
        client_id=client_id,
        response_type="code",
        redirect_uri="http://127.0.0.1/cb",
        scope=scope,
        state="client-csrf-token",
        code_challenge="E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        code_challenge_method="S256",
    )
    fields.update(overrides)
    return _form(**fields)


def _fresh_nonce(nest):
    """The nonce a first-contact client gets from its own first refusal."""
    status, headers, body = _post_par(nest, _form(client_id="x"), FORM_HEADERS)
    assert status == 400, f"expected the no-proof refusal, got {status}: {body!r}"
    nonce = headers.get("DPoP-Nonce")
    assert nonce, f"a refusal must hand back a usable nonce; headers={dict(headers)!r}"
    return nonce


def test_par_refuses_without_a_proof_and_hands_back_a_usable_nonce(domained_issuer_nest):
    """A first-contact client cannot have a nonce, so the refusal carries one.

    This is the whole recovery story of RFC 9449's server-supplied nonces: the
    client's very first request is *required* to fail, and the response that
    fails it is what makes the retry possible. A refusal without a ``DPoP-Nonce``
    would strand every correct client on its first call, with nothing in the
    answer telling it what to do differently.
    """
    status, headers, body = _post_par(
        domained_issuer_nest, _valid_par_form(), FORM_HEADERS
    )
    assert status == 400, f"expected 400, got {status}: {body!r}"
    assert body["error"] == "invalid_dpop_proof", body
    assert headers.get("DPoP-Nonce"), f"no nonce to retry with: {dict(headers)!r}"


def test_par_reads_no_body_before_a_proof_is_presented(domained_issuer_nest):
    """The ORDERING, asserted from outside: the DPoP gate runs before the form.

    A body that is not a form at all still answers ``invalid_dpop_proof`` and
    never ``invalid_request`` — because the body was never read. That ordering
    is what stops an anonymous caller making this nest parse, or later fetch,
    anything before it has completed a round trip and proved possession of a
    key. A refactor that "tidied" the form parse upward would flip this code,
    and nothing else would notice.
    """
    status, _, body = _post_par(
        domained_issuer_nest, b'{"not":"a form"}', {"Content-Type": "application/json"}
    )
    assert status == 400, f"expected 400, got {status}: {body!r}"
    assert body["error"] == "invalid_dpop_proof", (
        f"the body was read before the proof was checked: {body!r}"
    )


def test_par_accepts_a_dpop_bound_request_and_issues_a_request_uri(domained_issuer_nest):
    """The happy path: nonce, proof, form → RFC 9126's ``201`` and a
    ``request_uri``.

    Signed for real against the endpoint URL the nest *advertises*, which is
    also the assertion hiding inside this one: the proof names
    ``https://oauth-issuer.test/oauth/par`` while the test dials
    ``https://127.0.0.1:<port>``, so a server deriving ``htu`` from the
    request's own Host header would refuse this and accept a forgery.
    """
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    nonce = _fresh_nonce(domained_issuer_nest)
    proof = make_proof(key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=nonce)
    status, headers, body = _post_par(
        domained_issuer_nest, _valid_par_form(), {**FORM_HEADERS, "DPoP": proof}
    )
    assert status == 201, f"expected RFC 9126's 201, got {status}: {body!r}"
    assert body["request_uri"].startswith("urn:ietf:params:oauth:request_uri:"), body
    assert body["expires_in"] == 90, f"the PAR lifetime moved: {body!r}"
    assert headers.get("Cache-Control") == "no-store", dict(headers)
    assert headers.get("DPoP-Nonce"), "a success carries a nonce too"


def test_par_refuses_a_replayed_proof(domained_issuer_nest):
    """One proof, one request. The second presentation is a replay.

    Everything about the second call still validates — same live nonce, same
    good signature, same endpoint — so the only thing that can refuse it is the
    replay set, which is what this pins. It also pins the ordering that set
    depends on: the identifier is recorded only *after* the signature verified.
    """
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    nonce = _fresh_nonce(domained_issuer_nest)
    proof = make_proof(
        key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=nonce, jti="replayed-once"
    )
    form = _valid_par_form()
    first, _, first_body = _post_par(
        domained_issuer_nest, form, {**FORM_HEADERS, "DPoP": proof}
    )
    assert first == 201, f"the first use must succeed: {first_body!r}"

    second, _, second_body = _post_par(
        domained_issuer_nest, form, {**FORM_HEADERS, "DPoP": proof}
    )
    assert second == 400, f"a replay must be refused, got {second}: {second_body!r}"
    assert second_body["error"] == "invalid_dpop_proof", second_body
    assert "already been used" in second_body["error_description"], second_body


def test_par_refuses_a_client_supplied_request_uri(domained_issuer_nest):
    """A client pushes parameters; it does not push the handle they are filed
    under.

    Accepting one would let a caller name the handle its own request is stored
    at — and therefore squat on, or overwrite, one it was never issued.
    """
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    nonce = _fresh_nonce(domained_issuer_nest)
    proof = make_proof(key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=nonce)
    status, _, body = _post_par(
        domained_issuer_nest,
        _valid_par_form(
            request_uri="urn:ietf:params:oauth:request_uri:chosen-by-the-caller"
        ),
        {**FORM_HEADERS, "DPoP": proof},
    )
    assert status == 400, f"expected 400, got {status}: {body!r}"
    assert body["error"] == "invalid_request", body
    assert "issued by this server" in body["error_description"], body


def test_par_refuses_a_permission_set_rather_than_guessing_at_it(domained_issuer_nest):
    """An ``include:`` scope is resolved THROUGH the PDS bridge, and this nest
    has none connected — so it refuses ``invalid_scope`` at once: the
    closed-world answer, not a degraded one, and not a thirty-second wait.

    Pinned because the tempting failures are the other direction: expanding a
    set this server could not verify, or silently dropping the ``include:`` and
    issuing a narrower grant than the user was shown. Both are worse than
    refusing. The bridge-PRESENT arms — the set resolving onto the card and
    into the token, and a set the bridge's own chain refuses — live beside the
    ``consent_bridge`` fixture in ``test_atproto_pds_consent.py``.
    """
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    nonce = _fresh_nonce(domained_issuer_nest)
    proof = make_proof(key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=nonce)
    client_id = (
        "http://localhost?scope=atproto+include%3Acom.example.set"
        "&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"
    )
    status, _, body = _post_par(
        domained_issuer_nest,
        _valid_par_form(client_id=client_id, scope="atproto include:com.example.set"),
        {**FORM_HEADERS, "DPoP": proof},
    )
    assert status == 400, f"expected 400, got {status}: {body!r}"
    assert body["error"] == "invalid_scope", body


def test_a_domainless_nest_does_not_take_authorization_requests(domainless_issuer_nest):
    """``503``, for the same reason the read surfaces answer it.

    A pushed request is the first step of a flow whose later steps mint tokens
    under an ``iss``. Taking one on a box with no issuer identity would accept a
    flow that could only fail after the user has been redirected — the one place
    a refusal is most expensive.
    """
    status, _, _ = _post_par(domainless_issuer_nest, _valid_par_form(), FORM_HEADERS)
    assert status == 503, f"expected 503, got {status}"


# ---------------------------------------------------------------------------
# `/oauth/authorize` and its long-poll — the consent ceremony (TP5 S2)
# ---------------------------------------------------------------------------
#
# These drive the whole nest-side ceremony end to end: PAR → the consent page →
# the row it opened on the approving account → the approval over that account's
# own WS-RPC → the poll that turns it into a redirect. Everything but the final
# `/oauth/token` exchange, which is the next slice's.
#
# The approving account is the nest's own admin, which is a user with a role —
# so its `login_hint` resolves, the consent lands assigned, and
# `list_pending_consents` (USER class, self-scoped) is how the test finds the
# consent id. That is exactly the path a user's app takes.

AUTHORIZE_PATH = "/oauth/authorize"
POLL_PATH = "/oauth/authorize/poll"

#: The admin's handle, set by `claim_admin`. Used as the PAR's `login_hint` so
#: the consent is assigned rather than unassigned — the assigned path is the one
#: a real ceremony takes, and the only one `list_pending_consents` can find.
ADMIN_HANDLE = "admin"


def _push_par(nest, *, login_hint=None):
    """Push a valid authorization request and return its `request_uri`."""
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    nonce = _fresh_nonce(nest)
    proof = make_proof(key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=nonce)
    fields = {} if login_hint is None else {"login_hint": login_hint}
    status, _, body = _post_par(
        nest, _valid_par_form(**fields), {**FORM_HEADERS, "DPoP": proof}
    )
    assert status == 201, f"PAR failed: {status} {body!r}"
    return body["request_uri"]


def _get_authorize(nest, request_uri, client_id=LOOPBACK_CLIENT_ID):
    """GET the consent page. Returns (status, headers, body-text)."""
    url = (
        f"{nest['url']}{AUTHORIZE_PATH}?"
        + urllib.parse.urlencode({"client_id": client_id, "request_uri": request_uri})
    )
    request = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(request, context=_tls_context(nest)) as resp:
            return resp.status, resp.headers, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.headers, e.read().decode()


def _flow_token(page_html):
    """The poll credential the page carries, as the page's own script reads it."""
    import re

    match = re.search(r'data-flow="([^"]+)"', page_html)
    assert match, f"the page carries no flow token:\n{page_html[:800]}"
    return match.group(1)


def _poll(nest, flow_token):
    request = urllib.request.Request(
        f"{nest['url']}{POLL_PATH}",
        data=urllib.parse.urlencode({"flow": flow_token}).encode(),
        method="POST",
        headers=FORM_HEADERS,
    )
    try:
        with urllib.request.urlopen(request, context=_tls_context(nest)) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, None


def _resolve_consent(nest, code, approved):
    """Approve or decline, as the approving account's own app would.

    Finds the consent by the binding code the page displayed — which is also the
    check a real user performs, and it means the test cannot accidentally
    resolve some other ceremony's row.
    """
    with _admin_client(nest) as client:
        pending = client.call("fauna.bridges.atproto.list_pending_consents", {})
        matches = [c for c in pending["consents"] if c["code"] == code]
        assert matches, (
            f"no pending consent carries the page's binding code {code!r}; "
            f"got {[c['code'] for c in pending['consents']]!r}"
        )
        consent_id = matches[0]["consent_id"]
        resolved = client.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent_id, "approved": approved},
        )
        assert resolved["resolved"], f"the consent did not resolve: {resolved!r}"
    return matches[0]


def _binding_code(page_html):
    import re

    match = re.search(r'id="binding-code">([^<]+)<', page_html)
    assert match, f"the page shows no binding code:\n{page_html[:800]}"
    return match.group(1)


def test_the_consent_page_renders_the_request_and_locks_itself_down(domained_issuer_nest):
    """The page names the requesting client, shows a binding code, and forbids
    every origin.

    The CSP is asserted here rather than left to a unit test because the header
    and the page are only one artifact once they leave the same handler — a page
    whose script the CSP does not permit is a page that silently never polls.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    status, headers, page = _get_authorize(domained_issuer_nest, request_uri)
    assert status == 200, f"expected the consent page, got {status}: {page[:400]}"

    assert "Approve this in your Fauna app" in page
    assert LOOPBACK_CLIENT_ID.split("?")[0] in page, "the requesting origin is not shown"
    assert _binding_code(page), "no binding code to compare against the app"
    assert _flow_token(page), "no flow token, so the page can never poll"

    csp = headers.get("Content-Security-Policy") or ""
    assert "default-src 'none'" in csp, csp
    assert "frame-ancestors 'none'" in csp, csp
    assert "'nonce-" in csp, f"the page's own script is not permitted: {csp}"
    assert headers.get("Cache-Control") == "no-store", dict(headers)


def test_a_request_uri_is_single_use(domained_issuer_nest):
    """The second use of a `request_uri` is refused — and refused with the same
    wording as an expired or unknown one.

    Single use lives with the store, so no path can forget it. The identical
    wording is deliberate: a caller poking at handles must not learn which kind
    of dead it found.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    first, _, _ = _get_authorize(domained_issuer_nest, request_uri)
    assert first == 200

    second, _, page = _get_authorize(domained_issuer_nest, request_uri)
    assert second == 400, f"a spent request_uri was accepted again: {second}"
    assert "Unknown or expired request" in page

    unknown, _, unknown_page = _get_authorize(
        domained_issuer_nest, "urn:ietf:params:oauth:request_uri:never-issued"
    )
    assert unknown == 400
    assert unknown_page == page, (
        "a spent handle and a never-issued one must be indistinguishable"
    )


def test_a_mismatched_client_id_cannot_redeem_someone_elses_request(domained_issuer_nest):
    """RFC 9126 §4: the authorization request's `client_id` must identify the
    same client the pushed request was made for.

    And the handle is spent by the attempt either way — single use is the
    store's rule, and a mismatched attempt is precisely the replay it exists to
    spend.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    status, _, page = _get_authorize(
        domained_issuer_nest, request_uri, client_id="http://localhost"
    )
    assert status == 400, f"a mismatched client_id was accepted: {status}"
    assert "Unknown or expired request" in page

    again, _, _ = _get_authorize(domained_issuer_nest, request_uri)
    assert again == 400, "the mismatched attempt did not spend the handle"


def test_an_unknown_flow_token_is_answered_without_holding(domained_issuer_nest):
    """A poll credential nobody was issued is answered immediately.

    No hold and no consent read, so guessing costs a map lookup inside the
    poll's own budget rather than a held request slot.
    """
    import time

    started = time.monotonic()
    status, body = _poll(domained_issuer_nest, "not-a-flow-token-we-minted")
    elapsed = time.monotonic() - started
    assert status == 404, f"expected 404, got {status}: {body!r}"
    assert body["status"] == "expired", body
    assert "redirect" not in body, body
    # Generously above any plausible round trip and far below the hold budget,
    # so this asserts "did not hold" rather than measuring the machine.
    assert elapsed < 10, f"an unknown token was held for {elapsed:.1f}s"


def test_a_declined_request_redirects_the_client_to_its_own_callback(domained_issuer_nest):
    """A decline is an ANSWER, not a timeout: the page redirects into the
    client's own callback carrying `error=access_denied`, the client's `state`,
    and RFC 9207's `iss`.

    That is what stops a declined ceremony hanging on a page the user has
    already finished with.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    _, _, page = _get_authorize(domained_issuer_nest, request_uri)
    flow = _flow_token(page)
    _resolve_consent(domained_issuer_nest, _binding_code(page), approved=False)

    status, body = _poll(domained_issuer_nest, flow)
    assert status == 200, f"{status}: {body!r}"
    assert body["status"] == "resolved", body
    redirect = body["redirect"]
    assert redirect.startswith("http://127.0.0.1/cb?"), redirect
    assert "error=access_denied" in redirect, redirect
    assert "state=client-csrf-token" in redirect, redirect
    assert "iss=" in redirect, redirect
    assert "declined" in urllib.parse.unquote(redirect), redirect


def test_an_approval_the_account_cannot_complete_says_so_rather_than_hanging(
    domained_issuer_nest,
):
    """The admin approves, but holds no ACTIVE ATProto identity — so the
    ceremony ends in an honest refusal redirect rather than a code that could
    only fail at the token endpoint.

    Answering honestly here is no oracle: it is POST-consent, so the caller has
    already been told a human approved. What it proves is the whole ceremony
    wiring — the PAR crossed into a consent row on the right account, the
    approval over that account's own WS-RPC resolved it, the poll saw the
    resolution and built exactly one redirect out of it.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    _, _, page = _get_authorize(domained_issuer_nest, request_uri)
    flow = _flow_token(page)
    _resolve_consent(domained_issuer_nest, _binding_code(page), approved=True)

    status, body = _poll(domained_issuer_nest, flow)
    assert status == 200, f"{status}: {body!r}"
    assert body["status"] == "resolved", body
    redirect = body["redirect"]
    assert "error=access_denied" in redirect, (
        f"an account with no ATProto identity must not receive a code: {redirect}"
    )
    assert "cannot+complete" in redirect or "cannot%20complete" in redirect, redirect
    assert "state=client-csrf-token" in redirect, redirect


def test_a_resolved_flow_answers_the_same_redirect_twice(domained_issuer_nest):
    """A lost poll response must not cost the user their ceremony.

    The retry re-reads the memoized answer rather than minting a second one —
    the property that makes "exactly one authorization code per ceremony"
    structural. Asserted from outside, because the store-level test cannot see
    that the HTTP path actually goes through the memoization.
    """
    request_uri = _push_par(domained_issuer_nest, login_hint=ADMIN_HANDLE)
    _, _, page = _get_authorize(domained_issuer_nest, request_uri)
    flow = _flow_token(page)
    _resolve_consent(domained_issuer_nest, _binding_code(page), approved=False)

    first_status, first = _poll(domained_issuer_nest, flow)
    second_status, second = _poll(domained_issuer_nest, flow)
    assert first_status == second_status == 200
    assert first["redirect"] == second["redirect"], (
        f"the ceremony answered twice differently:\n{first!r}\n{second!r}"
    )


def test_a_domainless_nest_serves_no_consent_page(domainless_issuer_nest):
    """`503`, for the same reason every other issuer surface answers it: the
    redirect this page eventually builds carries an `iss`, and a nest with no
    issuer identity has none to put there.
    """
    status, _, _ = _get_authorize(
        domainless_issuer_nest, "urn:ietf:params:oauth:request_uri:anything"
    )
    assert status == 503, f"expected 503, got {status}"


# ---------------------------------------------------------------------------
# `/oauth/token` and `/oauth/revoke` (TP5 S2c)
# ---------------------------------------------------------------------------
#
# ⚠ **The happy path is not here, and cannot be.** Redeeming a code requires an
# approving account with an ACTIVE ATProto identity, which requires a real PDS
# bridge to mint one — so the full journey (code → pair → rotation → revocation)
# lives beside the `consent_bridge` fixture in
# `tests/test_atproto_pds_consent.py::test_the_nests_own_authorization_server_issues_and_revokes_a_grant`,
# where that identity exists. `tests/api/` spins no bridge, so a test needing one
# here would be a test that cannot pass.
#
# What IS here is every decision these two endpoints make *before* reaching a
# grant: the gate ordering, the grant-type dispatch, the refusals, and the
# revocation endpoint's deliberate refusal to be a validity oracle. Those are
# reachable on a plain nest, and this is the flavor that proves the endpoints
# mount with no bridge features compiled at all.

TOKEN_PATH = "/oauth/token"
REVOKE_PATH = "/oauth/revoke"
TOKEN_ENDPOINT_URL = f"{ISSUER_URL}{TOKEN_PATH}"
REVOKE_ENDPOINT_URL = f"{ISSUER_URL}{REVOKE_PATH}"


def _post_oauth(nest, path, body, headers=None):
    """POST one of the AS endpoints, returning (status, headers, body-text).

    Text rather than parsed JSON, because ``/oauth/revoke``'s success answer is
    an EMPTY body — the one endpoint here whose most important response has
    nothing to parse.
    """
    request = urllib.request.Request(
        f"{nest['url']}{path}",
        data=body.encode() if isinstance(body, str) else body,
        method="POST",
        headers=headers or {},
    )
    try:
        with urllib.request.urlopen(request, context=_tls_context(nest)) as resp:
            return resp.status, resp.headers, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.headers, e.read().decode()


def _oauth_error(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return {}


def _proved(nest, endpoint_url, form):
    """A form request carrying a fresh, valid DPoP proof for `endpoint_url`.

    The nonce comes from this endpoint's own refusal, which is how a real
    first-contact client gets one — so the helper walks the same two-step round
    trip rather than borrowing PAR's nonce.
    """
    from helpers.dpop import DpopKey, make_proof

    path = urllib.parse.urlparse(endpoint_url).path
    status, headers, _ = _post_oauth(nest, path, _form(x="1"), FORM_HEADERS)
    assert status in (400, 401), f"expected the no-proof refusal, got {status}"
    nonce = headers.get("DPoP-Nonce")
    assert nonce, f"a refusal must hand back a usable nonce; headers={dict(headers)!r}"
    key = DpopKey()
    proof = make_proof(key, htm="POST", htu=endpoint_url, nonce=nonce)
    return _post_oauth(nest, path, form, {**FORM_HEADERS, "DPoP": proof})


def test_token_refuses_without_a_proof_and_hands_back_a_usable_nonce(domained_issuer_nest):
    """Possession first, on this endpoint as on PAR — and the refusal carries
    the nonce that lets a first-contact client recover from the very response
    that told it so.
    """
    status, headers, raw = _post_oauth(
        domained_issuer_nest,
        TOKEN_PATH,
        _form(grant_type="authorization_code", code="anything"),
        FORM_HEADERS,
    )
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_dpop_proof", raw
    assert headers.get("DPoP-Nonce"), f"headers={dict(headers)!r}"


def test_token_reads_no_body_before_a_proof_is_presented(domained_issuer_nest):
    """A body far over the cap is refused for the MISSING PROOF, not for its
    size — which is the only way to see that the gate really precedes the read.

    If the body were read first (a `DefaultBodyLimit` layer, or a `Bytes`
    extractor) this would come back as a `413`-shaped complaint about length,
    and an anonymous caller would have made the nest buffer a megabyte before
    proving anything at all.
    """
    status, _, raw = _post_oauth(
        domained_issuer_nest, TOKEN_PATH, "grant_type=" + "a" * 1_000_000, FORM_HEADERS
    )
    assert status == 400, f"{status}: {raw[:200]!r}"
    assert _oauth_error(raw).get("error") == "invalid_dpop_proof", (
        f"a body over the cap must still be refused for the missing proof: {raw[:400]!r}"
    )


def test_a_missing_grant_type_is_malformed_and_an_unknown_one_is_unsupported(
    domained_issuer_nest,
):
    """RFC 6749 §5.2 has a code for each, and the difference is what tells a
    client author whether they sent the wrong thing or asked for a grant this
    server does not implement.
    """
    status, _, raw = _proved(domained_issuer_nest, TOKEN_ENDPOINT_URL, _form(client_id="x"))
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_request", raw

    status, _, raw = _proved(
        domained_issuer_nest, TOKEN_ENDPOINT_URL, _form(grant_type="client_credentials")
    )
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "unsupported_grant_type", raw


def test_an_unknown_authorization_code_is_refused_as_a_bad_grant(domained_issuer_nest):
    """`invalid_grant`, and — the part worth pinning — the SAME answer a
    redeemed or expired code gets.

    The store consumes on lookup, so "unknown", "expired" and "already
    redeemed" are one state by construction; telling them apart would let a
    caller probing codes learn which of its guesses had ever been real.
    """
    status, _, raw = _proved(
        domained_issuer_nest,
        TOKEN_ENDPOINT_URL,
        _form(
            grant_type="authorization_code",
            code="a-code-this-nest-never-minted",
            client_id=LOOPBACK_CLIENT_ID,
            redirect_uri="http://127.0.0.1/cb",
            code_verifier="whatever",
        ),
    )
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_grant", raw


def test_a_garbage_refresh_token_is_refused_as_a_bad_grant(domained_issuer_nest):
    """The refresh grant reaches its own verifier, and an unverifiable token is
    one refusal — not a crash, and not a distinguishable one.

    It also proves the grant's own budget is spent without breaking the request:
    this is the path that meters after `grant_type` is parsed.
    """
    status, _, raw = _proved(
        domained_issuer_nest,
        TOKEN_ENDPOINT_URL,
        _form(grant_type="refresh_token", refresh_token="not.a.token"),
    )
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_grant", raw


def test_revoke_requires_a_proof_and_a_token(domained_issuer_nest):
    """Both refusals are about the REQUEST, which is why they are not the
    uniform 200 every token OUTCOME gets.

    RFC 7009 asks for no proof; this AS requires one anyway, because without it
    anyone holding stolen token bytes could sign the honest user's app out —
    a denial of service reachable with a credential that is otherwise useless.
    """
    status, headers, raw = _post_oauth(
        domained_issuer_nest, REVOKE_PATH, _form(token="anything"), FORM_HEADERS
    )
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_dpop_proof", raw
    assert headers.get("DPoP-Nonce"), f"headers={dict(headers)!r}"

    status, _, raw = _proved(domained_issuer_nest, REVOKE_ENDPOINT_URL, _form(client_id="x"))
    assert status == 400, f"{status}: {raw!r}"
    assert _oauth_error(raw).get("error") == "invalid_request", raw


def test_revoke_answers_an_identical_empty_200_to_every_token_it_cannot_place(
    domained_issuer_nest,
):
    """The oracle rule, which is this endpoint's whole design.

    A thief holding stolen token bytes but not the DPoP key must not be able to
    learn from this endpoint whether the token is live — nor when the honest
    client's rotation retires it. So unknown, malformed, expired and
    other-plane are all ONE answer, and it is the same answer a genuine
    revocation gets: 200, empty, no-store.

    Asserted as a SET rather than one shape at a time, because the property is
    that the answers are indistinguishable — three tokens of quite different
    kinds producing byte-identical responses is the evidence, where three
    separate assertions that each is 200 would not be.
    """
    answers = set()
    for token in (
        "not-a-jwt-at-all",
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ4In0.",
        "a.b.c",
    ):
        status, headers, raw = _proved(
            domained_issuer_nest, REVOKE_ENDPOINT_URL, _form(token=token)
        )
        answers.add((status, raw, headers.get("Cache-Control")))
    assert len(answers) == 1, (
        f"revocation answered these tokens differently, which makes it a "
        f"validity oracle on token bytes: {answers!r}"
    )
    (status, raw, cache), = answers
    assert status == 200, f"{status}: {raw!r}"
    assert raw == "", f"RFC 7009's success answer carries no body: {raw!r}"
    assert cache == "no-store", cache


def test_the_hint_orders_the_search_and_never_limits_it(domained_issuer_nest):
    """RFC 7009 §2.1: a server that cannot honour `token_type_hint` must extend
    its search to the other type.

    A client that mislabels its own token still gets it revoked — which is the
    whole point of the requirement, and the reason the hint chooses which
    verifier runs FIRST rather than which one runs at all. Visible from outside
    only as the answer being unchanged by the hint, which is exactly the
    property.
    """
    seen = set()
    for hint in (None, "access_token", "refresh_token", "nonsense"):
        fields = {"token": "a.b.c"}
        if hint is not None:
            fields["token_type_hint"] = hint
        status, _, raw = _proved(
            domained_issuer_nest, REVOKE_ENDPOINT_URL, _form(**fields)
        )
        seen.add((status, raw))
    assert len(seen) == 1, f"the hint changed the outcome: {seen!r}"


def test_a_domainless_nest_serves_neither_tokens_nor_revocations(domainless_issuer_nest):
    """`503`, for the reason every other issuer surface answers it — and here it
    is the sharpest: a token's `iss` is a value clients pin, so minting one
    under a guessed identity would produce credentials that stop verifying the
    moment a real domain is claimed, with nothing to re-point them to.
    """
    for path in (TOKEN_PATH, REVOKE_PATH):
        status, _, raw = _post_oauth(
            domainless_issuer_nest, path, _form(grant_type="refresh_token"), FORM_HEADERS
        )
        assert status == 503, f"{path}: {status} {raw!r}"
        assert _oauth_error(raw).get("error") == "temporarily_unavailable", raw


# ---------------------------------------------------------------------------
# OIDC (TP6) — the ID token and `/oauth/userinfo`
# ---------------------------------------------------------------------------
#
# ``docs/goal/behavior/authorization-server.md`` § OIDC (TP6): an ``openid``
# grant's token reply carries an ID token (ES256 under the issuer key,
# ``typ: JWT``, ``aud`` = the client, ``sub`` = the actor id,
# ``preferred_username`` = the handle, ``nonce`` from PAR), and
# ``/oauth/userinfo`` answers behind the DPoP-bound access token.
#
# **The whole journey runs HERE, on the plain nest** — unlike the ATProto
# grant's (see the ⚠ above `/oauth/token`'s section). An OIDC-only sign-in
# reaches nothing at the PDS, so it needs no ATProto identity: `sub` is the
# actor id, and the approving admin — who has none — completes it. That is the
# property "Sign in with Fauna" rests on, and the standalone binary with no
# bridge compiled is the configuration that proves it.

USERINFO_PATH = "/oauth/userinfo"
USERINFO_ENDPOINT_URL = f"{ISSUER_URL}{USERINFO_PATH}"

#: A "Sign in with Fauna" client: it declares only the OIDC family — no
#: ``atproto`` — which is exactly the declaration the base-scope rule now
#: lets through.
OIDC_CLIENT_ID = (
    "http://localhost?scope=openid%20profile%20email"
    "&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"
)

#: RFC 7636 Appendix B's verifier for the challenge `_valid_par_form` sends.
CODE_VERIFIER = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"


def _b64url_decode(part):
    import base64

    return base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))


def _verify_es256_jwt(token, jwks):
    """Verify a compact ES256 JWS against the served key set, by ``kid``, and
    return ``(header, claims)`` — the relying party's own check, done with no
    trust in anything but the JWKS."""
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec, utils

    header_b64, claims_b64, signature_b64 = token.split(".")
    header = json.loads(_b64url_decode(header_b64))
    keys = [k for k in jwks["keys"] if k["kid"] == header["kid"]]
    assert len(keys) == 1, f"no served key carries kid {header['kid']!r}: {jwks!r}"
    jwk = keys[0]
    public = ec.EllipticCurvePublicNumbers(
        int.from_bytes(_b64url_decode(jwk["x"]), "big"),
        int.from_bytes(_b64url_decode(jwk["y"]), "big"),
        ec.SECP256R1(),
    ).public_key()
    raw = _b64url_decode(signature_b64)
    assert len(raw) == 64, "an ES256 JWS signature is r||s, 64 bytes"
    der = utils.encode_dss_signature(
        int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")
    )
    # Raises InvalidSignature on a forgery — the test fails loudly there.
    public.verify(der, f"{header_b64}.{claims_b64}".encode(), ec.ECDSA(hashes.SHA256()))
    return header, json.loads(_b64url_decode(claims_b64))


def _userinfo(nest, headers):
    request = urllib.request.Request(
        f"{nest['url']}{USERINFO_PATH}", method="GET", headers=headers
    )
    try:
        with urllib.request.urlopen(request, context=_tls_context(nest)) as resp:
            return resp.status, resp.headers, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, e.headers, None


def _userinfo_nonce(nest):
    """The nonce a first-contact caller gets from UserInfo's own refusal."""
    status, headers, _ = _userinfo(nest, {})
    assert status == 401, f"expected the no-token refusal, got {status}"
    nonce = headers.get("DPoP-Nonce")
    assert nonce, f"a refusal must hand back a usable nonce; headers={dict(headers)!r}"
    return nonce


def _oidc_sign_in(nest, *, scope, nonce):
    """Run the whole OIDC ceremony as a relying party would, and return
    ``(dpop_key, token_reply)``: PAR under one DPoP key → the consent page →
    the admin approves in its app → the poll releases the code → the token
    exchange, proved by the SAME key."""
    from helpers.dpop import DpopKey, make_proof

    key = DpopKey()
    proof = make_proof(key, htm="POST", htu=PAR_ENDPOINT_URL, nonce=_fresh_nonce(nest))
    status, _, body = _post_par(
        nest,
        _valid_par_form(
            client_id=OIDC_CLIENT_ID, scope=scope, nonce=nonce, login_hint=ADMIN_HANDLE
        ),
        {**FORM_HEADERS, "DPoP": proof},
    )
    assert status == 201, f"PAR refused an OIDC sign-in: {status} {body!r}"

    status, _, page = _get_authorize(nest, body["request_uri"], client_id=OIDC_CLIENT_ID)
    assert status == 200, f"no consent page: {status} {page[:400]}"
    flow = _flow_token(page)
    _resolve_consent(nest, _binding_code(page), approved=True)
    status, polled = _poll(nest, flow)
    assert status == 200 and polled["status"] == "resolved", polled
    redirect = urllib.parse.urlsplit(polled["redirect"])
    query = urllib.parse.parse_qs(redirect.query)
    assert "code" in query, (
        f"an approved OIDC-only sign-in must release a code even though the "
        f"account holds no ATProto identity: {polled['redirect']}"
    )

    path = urllib.parse.urlparse(TOKEN_ENDPOINT_URL).path
    status, headers, _ = _post_oauth(nest, path, _form(x="1"), FORM_HEADERS)
    token_nonce = headers.get("DPoP-Nonce")
    assert token_nonce, dict(headers)
    status, _, raw = _post_oauth(
        nest,
        path,
        _form(
            grant_type="authorization_code",
            code=query["code"][0],
            client_id=OIDC_CLIENT_ID,
            redirect_uri="http://127.0.0.1/cb",
            code_verifier=CODE_VERIFIER,
        ),
        {
            **FORM_HEADERS,
            "DPoP": make_proof(key, htm="POST", htu=TOKEN_ENDPOINT_URL, nonce=token_nonce),
        },
    )
    assert status == 200, f"the token exchange failed: {status} {raw!r}"
    return key, json.loads(raw)


def test_an_openid_sign_in_mints_an_id_token_the_jwks_verifies(domained_issuer_nest):
    """The ID token verifies against ``/oauth/jwks`` and says exactly what
    § OIDC says it says — and the discovery document advertises the members a
    relying party reads to check it."""
    nest = domained_issuer_nest
    doc = _get_json(nest, DISCOVERY_PATHS[1])
    assert doc["userinfo_endpoint"] == USERINFO_ENDPOINT_URL, doc
    assert doc["id_token_signing_alg_values_supported"] == ["ES256"], doc
    assert doc["subject_types_supported"] == ["public"], doc
    for scope in ("openid", "profile", "email"):
        assert scope in doc["scopes_supported"], doc["scopes_supported"]

    _, reply = _oidc_sign_in(nest, scope="openid profile", nonce="n-0S6_WzA2Mj")
    assert "id_token" in reply, f"an openid grant's reply carries no id_token: {reply!r}"

    header, claims = _verify_es256_jwt(reply["id_token"], _get_json(nest, "/oauth/jwks"))
    assert header["typ"] == "JWT" and header["alg"] == "ES256", header
    admin_actor = bytes(nest["admin"]["signing_key"].verify_key).hex()
    assert claims["iss"] == ISSUER_URL, claims
    assert claims["aud"] == OIDC_CLIENT_ID, claims
    assert claims["sub"] == admin_actor, claims
    assert claims["preferred_username"] == ADMIN_HANDLE, claims
    assert claims["nonce"] == "n-0S6_WzA2Mj", claims
    assert "email" not in claims, (
        f"`email` was not requested, so no email claim may leave: {claims!r}"
    )
    assert claims["exp"] > claims["iat"], claims

    # The access token is the OTHER class under the same key.
    access_header, _ = _verify_es256_jwt(reply["access_token"], _get_json(nest, "/oauth/jwks"))
    assert access_header["typ"] == "at+jwt", access_header


def test_userinfo_answers_under_the_dpop_bound_access_token_and_refuses_a_bearer(
    domained_issuer_nest,
):
    """UserInfo releases the same claims as the ID token under the access token
    AND its DPoP proof; the same token presented as a ``Bearer`` is refused."""
    from helpers.dpop import make_proof

    nest = domained_issuer_nest
    key, reply = _oidc_sign_in(nest, scope="openid profile", nonce="n-userinfo")
    access = reply["access_token"]

    # The audience is the set of the token's readers (§ The issuer). An
    # OIDC-only grant is exercised at the nest and nowhere else, so its access
    # token names the ISSUER — one reader, so a plain string — and not a PDS
    # this bridge-less nest does not have.
    _, access_claims = _verify_es256_jwt(access, _get_json(nest, "/oauth/jwks"))
    assert access_claims["aud"] == ISSUER_URL, (
        "an OIDC-only grant's access token must name the nest issuer as its "
        f"one reader: {access_claims!r}"
    )

    proof = make_proof(
        key,
        htm="GET",
        htu=USERINFO_ENDPOINT_URL,
        nonce=_userinfo_nonce(nest),
        access_token=access,
    )
    status, headers, body = _userinfo(
        nest, {"Authorization": f"DPoP {access}", "DPoP": proof}
    )
    assert status == 200, (
        f"UserInfo refused a token naming this issuer as its reader: {status}"
    )
    assert body["sub"] == bytes(nest["admin"]["signing_key"].verify_key).hex(), body
    assert body["preferred_username"] == ADMIN_HANDLE, body
    assert headers.get("DPoP-Nonce"), "every answer carries the next nonce"

    status, headers, _ = _userinfo(nest, {"Authorization": f"Bearer {access}"})
    assert status == 401, f"a DPoP-bound token was honoured as a bearer: {status}"
    assert "DPoP" in (headers.get("WWW-Authenticate") or ""), dict(headers)


def test_an_id_token_is_refused_where_an_access_token_is_required(domained_issuer_nest):
    """The ``typ`` separation, from outside: the ID token is signed by the SAME
    key and carries a valid proof over itself, and UserInfo still refuses it,
    because ``typ: JWT`` is not ``typ: at+jwt`` inside the signed bytes."""
    from helpers.dpop import make_proof

    nest = domained_issuer_nest
    key, reply = _oidc_sign_in(nest, scope="openid", nonce="n-typ")
    id_token = reply["id_token"]
    proof = make_proof(
        key,
        htm="GET",
        htu=USERINFO_ENDPOINT_URL,
        nonce=_userinfo_nonce(nest),
        access_token=id_token,
    )
    status, headers, _ = _userinfo(
        nest, {"Authorization": f"DPoP {id_token}", "DPoP": proof}
    )
    assert status == 401, f"an ID token was accepted as an access token: {status}"
    assert 'error="invalid_token"' in (headers.get("WWW-Authenticate") or ""), dict(headers)


def test_profile_without_openid_is_refused_at_par(domained_issuer_nest):
    """``profile`` and ``email`` are claims OF a sign-in; asked for without
    ``openid`` they would be card rows nothing ever honours."""
    from helpers.dpop import DpopKey, make_proof

    nest = domained_issuer_nest
    client_id = "http://localhost?scope=profile&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"
    proof = make_proof(DpopKey(), htm="POST", htu=PAR_ENDPOINT_URL, nonce=_fresh_nonce(nest))
    status, _, body = _post_par(
        nest,
        _valid_par_form(client_id=client_id, scope="profile"),
        {**FORM_HEADERS, "DPoP": proof},
    )
    assert status == 400, f"{status} {body!r}"
    assert body["error"] == "invalid_scope", body
    assert "openid" in body["error_description"], body
