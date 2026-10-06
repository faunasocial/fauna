"""Interop against a peer that refuses **unsigned** fetches — the phase-2 core.

Most of the fediverse runs permissive, which is what `test_interop.py` covers by
default. A *strict* peer additionally refuses unsigned fetches of its own
objects, which our outbound actor `GET` must therefore sign with the instance
actor. Two peers reach that state by different routes, and the `strict_peer`
fixture accepts either:

  * **Mastodon** with `AUTHORIZED_FETCH=true` — a deployment choice
    (`just e2e-fediverse-strict-test`).
  * **GoToSocial**, which has no permissive mode at all: 0.22.1 exposes no
    equivalent knob, so *every* run of it exercises this
    (`just e2e-gotosocial-test`). It is the reason these assertions are no
    longer reachable only behind an opt-in flag.

**Which direction actually breaks, and why it is not discovery.** Strictness
governs fetches *of the peer's* objects. When the peer resolves
`@user@nest.test` it is the peer that dials us, signing its own request — our
GET plays no part, so discovery is expected to work. Our GET matters on the
*inbound* leg: every activity arriving at our inbox makes us fetch the sending
actor's document to get the key we verify its signature with (`process_inbox`
step 6 → `fetch_remote_actor`). If that fetch is refused we cannot verify, so
the activity is rejected — and the first activity any federation exchange sends
is the **Follow**. Hence the gap surfaced as "the follow is never accepted", not
as "the account cannot be found".

**Status: these assert the fix, not the gap.** Phase 2 landed the instance actor
and signed GETs, so the follow assertion below is inverted from the
characterization form it was written in: it asserts the Follow *is* accepted,
and — because a pass for the wrong reason is the failure mode that cost the most
here — that the nest's own log shows neither a failed remote-actor fetch nor an
unsigned-dial fallback.

Opt-in like the rest of the harness (`FAUNA_E2E_FEDIVERSE=1`), plus the
`fediverse_strict` marker, which is deselected on a permissive run.
"""

import pytest

from common import create_actor_and_register
from helpers.ap_nest import enable_ap, nest_log
from helpers.budgets import CROSS_NEST_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.fediverse_interop,
    pytest.mark.fediverse_strict,
]


@pytest.fixture(scope="module")
def strict_pair(strict_peer):
    """A peer-local user on a strict peer + a fresh AP-enabled nest actor."""
    peer = strict_peer
    _username, token = peer.new_user("strictuser")

    nest_user = create_actor_and_register(
        peer.nest["port"], admin_signing_key=peer.nest["admin"]["signing_key"]
    )
    username, actor_url = enable_ap(peer.nest, nest_user)
    return {
        "token": token,
        "nest_user": nest_user,
        "username": username,
        "actor_url": actor_url,
        "acct": f"{username}@{peer.nest['domain']}",
    }


def test_secure_mode_stack_reports_itself_strict(strict_peer):
    """The fixture really did boot in secure mode.

    Guards the whole module against its worst failure mode: silently running a
    permissive stack, so every result below means nothing.
    """
    assert strict_peer.authorized_fetch is True


def test_discovery_still_works_under_authorized_fetch(
    strict_peer, strict_pair
):
    """Strictness does NOT break discovery — the peer signs its own fetch of us.

    Pins the direction analysis in the module docstring. If this ever fails, the
    gap is wider than "our GET is unsigned" and phase 2's scope must be
    re-derived before any signing code is written.
    """
    peer = strict_peer
    acct = strict_pair["acct"]

    account = wait_until(
        lambda: peer.resolve_account(acct, strict_pair["token"]),
        CROSS_NEST_S,
        diagnose=lambda: (
            f"strict {peer.name} could not resolve @{acct}. Discovery is the leg "
            f"where THE PEER dials US and signs its own request, so this should not "
            f"depend on our unsigned GET — a failure here means the secure-mode gap "
            f"is larger than the audit recorded, and phase 2 needs re-scoping."
        ),
    )
    assert account["acct"].lower() == acct.lstrip("@").lower()


def test_instance_actor_is_served_and_webfinger_resolvable(strict_peer):
    """The signing identity a secure-mode peer must be able to reach.

    Both halves matter and fail differently: an unreachable `keyId` means the
    peer cannot obtain our key at all, while a WebFinger that does not resolve
    back to the same document fails only on the servers that verify the two
    agree — a subset large enough to matter and small enough to hide.

    Fetched through the peer's own TLS front, i.e. over the real network path a
    peer uses, not a loopback shortcut.
    """
    peer = strict_peer
    domain = peer.nest["domain"]

    actor = peer.fetch_nest_url(f"https://{domain}/ap/instance")
    assert actor["type"] == "Application", f"instance actor is not an Application: {actor}"
    assert actor["id"] == f"https://{domain}/ap/instance"
    assert actor["publicKey"]["id"] == f"https://{domain}/ap/instance#main-key"
    assert actor["publicKey"]["publicKeyPem"].startswith("-----BEGIN PUBLIC KEY-----")
    # The document must not advertise a route we do not serve.
    outbox = peer.fetch_nest_url(actor["outbox"])
    assert outbox["type"] == "OrderedCollection"

    wf = peer.fetch_nest_url(
        f"https://{domain}/.well-known/webfinger?resource=acct:{domain}@{domain}"
    )
    assert wf["subject"] == f"acct:{domain}@{domain}"
    self_link = next(link for link in wf["links"] if link["rel"] == "self")
    assert self_link["href"] == actor["id"], (
        "WebFinger and the actor document disagree, so a peer that checks them "
        "against each other will refuse our key"
    )


@pytest.mark.feature("fediverse")
def test_follow_is_accepted_under_authorized_fetch(
    strict_peer, strict_pair
):
    """Phase 2's green: a strict peer's Follow is auto-accepted.

    This is the whole gap, end to end. To verify the inbound Follow's signature
    the nest fetches the sender's actor document; that fetch is now signed with
    the instance actor, so a secure-mode peer serves it, verification succeeds,
    and the auto-Accept goes back.

    Before the fix this could not complete: the refused fetch was not even
    reported as one — `fetch_remote_actor` ignored the HTTP status, so
    the peer's 401 JSON body was parsed as an actor with empty fields and the
    failure surfaced as a PEM parse error. Both the log assertions below exist
    because of that: "no Accept arrived" and "the wrong thing broke" are
    otherwise indistinguishable.
    """
    peer = strict_peer
    token = strict_pair["token"]

    account = wait_until(
        lambda: peer.resolve_account(strict_pair["acct"], token),
        CROSS_NEST_S,
        diagnose=lambda: "discovery failed; see the discovery test for the diagnosis",
    )

    peer.follow(account["id"], token)
    wait_until(
        lambda: peer.relationship(account["id"], token).get("following") or None,
        CROSS_NEST_S,
        diagnose=lambda: (
            f"strict {peer.name} never saw our Accept. Check the nest log below "
            "for 'fetch remote actor failed' (the signed GET was still refused — "
            "read the HTTP status it now reports) or 'signature verification failed' "
            f"(we fetched the key but rejected the Follow).\nNest log tail:\n"
            f"{nest_log(peer.nest)[-6000:]}"
        ),
    )

    log = nest_log(peer.nest)
    # Green for the RIGHT reason: the fetch that used to be refused now succeeds.
    assert "ap inbox: fetch remote actor failed" not in log, (
        "the follow was accepted, but the nest still logged a failed remote-actor "
        f"fetch — something is passing despite the gap.\nNest log tail:\n{log[-6000:]}"
    )
    assert "dialing the remote actor UNSIGNED" not in log, (
        "the nest fell back to an unsigned dial, so this passed without exercising "
        f"the instance-actor path at all.\nNest log tail:\n{log[-6000:]}"
    )
