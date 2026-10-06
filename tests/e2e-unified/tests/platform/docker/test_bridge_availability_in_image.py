"""E2E test (tier_4): which bridges a **shipped** nest can actually offer.

Every other bridge witness in the suite runs against a nest a *test* started,
with the flags that test chose. This one asks the question only a real
deployment artifact can answer: boot the image the way a user's box boots it —
``docker/s6/fauna-nest/run``'s fixed ``fauna-nest --config /data/nest.toml
--blob-dir /data/blobs``, no test-chosen nest argv anywhere — claim it, and read
``fauna.bridges.list`` back over the wire.

**Why this is tier_4-unique.** The answer is a property of the *packaging*, not
of the code: all three providers are compiled in (the Dockerfile builds
``--features bluesky,nostr,activitypub``) and
all three register at boot (``bins/fauna-nest/src/lib.rs``), so every
tier_1–tier_3 witness of "the nest lists its bridges" is satisfied. What only the
artifact can show is which of them the artifact's own launch line leaves
*available*.

**What it found, and what it now pins (2026-09-02).** It first found Bluesky
``available: false`` on a freshly claimed shipped nest with ActivityPub available
beside it — because ``BlueskyProvider::available`` hung off a boot-time
``--bluesky-public-url`` CLI flag that **no shipped launch path passes** (neither
the s6 run script above nor ``bins/fauna-nest/install.sh``'s ``ExecStart``), while
``docs/guides/bridges-bluesky-nostr.md`` § Bluesky, today told users the account
link "ships now". The flag was also the banned operator tier of the
one-configuration-surface invariant (``docs/goal/principles.md``): a nest's own
public URL is nobody's *choice*.

That flag is gone. The OAuth ``client_id`` is now derived from the deployment's
own identity domain (``AppState::handle_domain_if_set`` →
``bluesky::oauth_public_url``), so this module pins the derivation **from both
sides, on the artifact**:

- ``shipped_nest`` — the default deploy shape, a **domainless** claim. Bluesky is
  unavailable, and says why. That is the honest answer, not a residual gap: an
  OAuth round needs Bluesky's authorization server to fetch this nest's
  client-metadata document *by name* over public HTTPS, which a domainless box
  cannot offer.
- ``domained_nest`` — the same image, same launch line, claimed onto a real
  domain. Bluesky is available and advertises its OAuth link mode, with **no
  extra argv**. This is the assertion the removed flag made impossible, and it is
  the one a user's "I installed Fauna and opened Settings → AT Protocol" depends on.

ActivityPub, available on both boxes, stays the control on each.
"""

import subprocess

import pytest

from .helpers import (
    admin_ws,
    claim_admin_api,
    docker_build,
    find_free_port,
    generate_claim_code,
    get_repo_root,
    remove_container,
    start_container,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# No app marker on purpose: no app driver is launched, so this is
# client-independent — selected like the `tests/api/` suites, not by `--client`.
pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

#: The three providers the shipped image compiles in and registers at boot.
COMPILED_IN = ("bluesky", "nostr", "activitypub")


@pytest.fixture(scope="module")
def docker_image():
    """The shipped image, reused — never built here (see ``docker_build``)."""
    return docker_build(get_repo_root())


#: The domain ``domained_nest`` is claimed onto. Syntactically a public DNS name
#: (``is_public_dns_name`` is a host-class classifier, not a resolver), which is
#: all the derivation reads — no DNS round, no ACME order, nothing to reach.
CLAIMED_DOMAIN = "nest.home.test"


def _shipped_container(label, mail_domain=None):
    """Boot the image on its own launch line and claim it. ``mail_domain``
    carries the **domained claim**: it becomes the primary ``mail_domains`` row,
    which IS the deployment identity (``identity_domain_core``), so the nest
    acquires that domain with no ``FAUNA_DOMAIN`` boot env — the same move
    ``test_iroh_relay.py`` makes so *its* derivation has a domain to derive
    from."""
    port = find_free_port()
    claim_code = generate_claim_code()
    name = f"fauna-bridge-avail-{label}-{port}"
    start_container(name, port, env={
        "FAUNA_MODE": "public",
        "FAUNA_CLAIM_CODE": claim_code,
    })
    try:
        wait_for_health(port, name)
        yield {
            "name": name,
            "port": port,
            "url": f"https://127.0.0.1:{port}",
            "admin": claim_admin_api(port, claim_code, handle="admin",
                                     mail_domain=mail_domain),
        }
    finally:
        remove_container(name)


@pytest.fixture(scope="module")
def shipped_nest(docker_image):
    """A claimed nest booted exactly as a user's box boots: the image's own
    launch line, no test-chosen nest argv. Domainless claim — the default deploy
    shape — so nothing here depends on a DNS name or an ACME round."""
    yield from _shipped_container("plain")


@pytest.fixture(scope="module")
def domained_nest(docker_image):
    """The same shipped image and the same launch line, claimed onto a real
    domain. The ONLY difference from ``shipped_nest`` is the claim payload — no
    extra argv, no env, no config file — which is exactly the claim this module
    makes about the derivation."""
    yield from _shipped_container("domained", mail_domain=CLAIMED_DOMAIN)


def _bridges(nest) -> dict:
    with admin_ws(nest) as ws:
        listed = ws.call("fauna.bridges.list", {})["bridges"]
    return {b["id"]: b for b in listed}


@pytest.mark.feature("bridges")
def test_shipped_image_lists_the_bridges_it_compiled_in(shipped_nest):
    """The deployed artifact answers ``fauna.bridges.list`` with every provider
    it was built with, unavailable ones included — ``bridges.md`` § State & data
    shape ("an unavailable provider returns ``available: false`` and renders
    disabled, not absent"), witnessed against the artifact rather than against a
    test-started binary.

    ActivityPub's ``available: true`` is the control: it makes the Bluesky result
    the next test pins a statement about *Bluesky*, not about bridges being
    broken in the image.
    """
    bridges = _bridges(shipped_nest)

    missing = [b for b in COMPILED_IN if b not in bridges]
    assert not missing, (
        f"the image builds --features fauna-nest/{{{','.join(COMPILED_IN)}}} and "
        f"registers all three at boot, so all three must be listed; missing: "
        f"{missing} (listed: {sorted(bridges)})"
    )

    ap = bridges["activitypub"]
    assert ap["available"] is True, (
        "ActivityPubProvider::available is an unconditional `true`, so a shipped "
        f"nest must offer it; got {ap!r}"
    )


@pytest.mark.feature("bridges")
def test_shipped_image_offers_the_bluesky_bridge(domained_nest):
    """A user who installs Fauna, gives their nest a domain, and opens
    Settings → AT Protocol can link the Bluesky account they already have — the
    promise ``docs/guides/bridges-bluesky-nostr.md`` § Bluesky, today makes
    ("what ships now is an account link ... it's a standard OAuth flow").

    This was a strict xfail until 2026-09-02, when the OAuth public URL stopped
    being a CLI flag nothing passed and became a derivation from the claimed
    identity domain. Nothing about the *artifact* changed to make it pass: the
    launch line is byte-identical to ``shipped_nest``'s, and the only input is
    the domain in the claim. ``link_modes`` is asserted alongside ``available``
    because availability alone would let a nest that offers no way to link pass.
    """
    bsky = _bridges(domained_nest)["bluesky"]

    assert bsky["available"] is True, (
        f"a shipped nest claimed onto {CLAIMED_DOMAIN} derives its Bluesky OAuth "
        f"client from that domain and must offer the bridge; got {bsky!r}"
    )
    modes = bsky.get("link_modes") or []
    assert any(m["mode"] == "oauth" for m in modes), (
        f"an available, unlinked Bluesky bridge advertises its OAuth link mode; "
        f"got link_modes={modes!r}"
    )
    assert not bsky.get("error"), (
        f"an available bridge has nothing to explain; got error={bsky.get('error')!r}"
    )


@pytest.mark.feature("bridges")
def test_domainless_shipped_image_refuses_bluesky_with_a_reason(shipped_nest):
    """The other half, and the one a user on the default deploy actually meets:
    a nest with no domain cannot complete a Bluesky OAuth round (the
    authorization server must fetch this nest's client-metadata document by
    name), so it reports the bridge unavailable **and says why**.

    ``error`` is the load-bearing half. ``bridges.md`` § A bridge that cannot be
    linked right now, rule 2 says the nest's sentence is rendered verbatim in
    ``bridge-link-blocked-reason``; with ``error: None`` every app would fall
    back to the generic ``bridges.no_link_method`` and the user would be left
    staring at a dead control with no idea that a domain is what unblocks it.
    ActivityPub on the same box is the control: unavailability here is about
    Bluesky's precondition, not about bridges being broken in the image.
    """
    bridges = _bridges(shipped_nest)
    assert bridges["activitypub"]["available"] is True, (
        "control: ActivityPub is available on a domainless shipped nest"
    )

    bsky = bridges["bluesky"]
    assert bsky["available"] is False, (
        f"a domainless nest cannot present an OAuth client identity Bluesky can "
        f"resolve, so it must not advertise the bridge as linkable; got {bsky!r}"
    )
    assert not (bsky.get("link_modes") or []), (
        f"an unavailable bridge advertises no link mode — a rendered-but-inert "
        f"Link control is exactly what bridges.md forbids; got {bsky!r}"
    )
    reason = bsky.get("error")
    assert reason, (
        "the nest owes the user its own account of why the bridge is off; "
        f"got error={reason!r} on {bsky!r}"
    )
    assert "domain" in reason.lower(), (
        f"the reason must name the missing domain — the one thing the user can "
        f"actually change; got {reason!r}"
    )
