"""tier_3 e2e: **a knock reaches someone on another nest** — contacts outcome 5.

``docs/features/contacts.md`` outcome 5 sat at ``(none)`` on every column, and
honestly so: until the build this test witnesses, no app could send a knock
anywhere but its own nest. Every one of the seven passed ``recipient_nest_url:
null`` to ``fauna.inbox.send``, so the nest always took its local-delivery
branch (``bins/fauna-nest/src/inbox_handlers.rs``) and a typed
``bob@other.test`` resolved against the *home* nest — finding a local ``bob``,
or nothing.

The journey, end to end and entirely through the app UI (convention 8 — no API
call stands in for the user):

1. Alice is signed in on the **home** nest. Bob is a *handled* actor on the
   **foreign** nest, which advertises its own loopback authority as
   ``handle_domain`` and opens self-service registration — so
   ``fauna.actor.by_handle`` there answers with a domain the home nest's relay
   can actually reach.
2. Alice types ``bob@<foreign-authority>`` into Find User.
   ``fauna_core::resolve::is_foreign_handle_domain`` judges the typed domain
   against the one the *same-nest* reply echoed, rules it foreign, and
   ``fauna_provisioning::probe::peer_nest_url`` derives the peer's base URL. An
   anonymous ``fauna.actor.by_handle`` runs **directly against the foreign
   nest** — the same mechanism the conversations recipient picker uses, and
   deliberately not the nest-proxied ``fauna.nest.resolve``, which refuses
   loopback / IP-literal / ``.local`` authorities by design
   (``bins/fauna-nest/src/discovery_core.rs``) and so could never be witnessed
   by a two-nest topology.
3. Alice clicks ``contacts-add-button``. The page carries the URL it resolved
   *on* into ``fauna.inbox.send`` as ``recipient_nest_url``, the home nest takes
   its federation branch and originates ``fauna.federation.inbox.deliver``, and
   the foreign nest runs the identical ``deliver_inbox_payload_core`` →
   ``allow_knock`` → ``store_knock``.
4. **The assertion:** Bob's ``fauna.knocks.list`` *on his own nest* now holds a
   pending knock from Alice.

What step 4 catches that a no-error-banner check would not: the nest replies
``inbox_id: null`` on a stored knock, and that null is **success**. A client
that reads it as failure, a payload routed to a plain inbox row instead of a
knock, or a knock stored on the *home* nest because the URL was dropped between
the lookup and the send — all three leave the UI looking healthy.

**Why the queue read is on the FOREIGN nest specifically.** The failure this
test exists to catch is silent by construction: if the page loses the peer URL
between the lookup and the send, ``fauna.inbox.send`` carries ``null``, the home
nest stores the knock locally against Bob's actor id, and the UI looks exactly
as healthy as it does on success. Reading Bob's queue *on his own nest* is what
separates the two — and it is the only read that can, since Bob has no account
on the home nest to read from.

**Red-verified 2026-09-20.** With the page's URL threading reverted to ``None``
(``Op::SendKnock``'s ``recipient_nest_url``), this fails at exactly the federated
delivery — ``wait_until: timed out after 60.0s: no knock reached the foreign
nest; app error=''`` — while every assertion before it still passes, the foreign
*lookup* included: Bob is still resolved to his real actor id on the peer nest.
So a green run really does mean the knock crossed nests, not merely that the
lookup found somebody. Note the empty ``app error``: the reverted build surfaces
nothing at all to the user, which is the silence this test is here to break.

Companion coverage, deliberately layered so this e2e is not the only witness:

* the wire leg — ``bins/fauna-nest/tests/conformance_federation_channel.rs``
  ``inbox_send_cross_nest_stores_a_knock_for_a_stranger`` (the same
  ``allow_knock`` + ``ArrivalOrigin::Federation`` combination, headless);
* the page seam — ``apps/fauna-tui/src/contacts.rs``
  ``a_peer_resolved_find_result_carries_its_nest_url_into_the_knock``;
* the decision rule — ``libs/fauna-core/src/resolve.rs``'s
  ``is_foreign_handle_domain`` unit tests.

That layering matters because this file is **exclusion class (8)**: the journey
makes one nest dial another, which in docker mode crosses the run's
user-defined network and is refused by ``federation_channel::validate_peer_url``
as non-global (``testing.md`` § Default app and nest mode, ruling (2)). The
catalog cell for outcome 5 therefore comes from standalone (and live) runs; the
class is declared by this test reading the foreign handle's ``peer_url``
contract key, which is the classifier's signal.

tier_3: two real ``fauna-nest`` binaries. tui only for now — the other six apps
already carry ``recipient_nest_url`` on their transports and inherit this
through the batched per-app trickle-down, at which point the action-layer gate
below stops skipping them.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]


def _knock_senders(base_url: str, actor: dict) -> list[str]:
    """Bob's pending-knock senders, read on the nest that holds his account."""
    with WsRpcAdminClient(
        base_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    ) as client:
        knocks = client.call("fauna.knocks.list", {}).get("knocks", [])
    return [k["sender"] for k in knocks]


@pytest.mark.feature("contacts")
def test_cross_nest_knock_reaches_the_peers_own_queue(
    logged_in_app, nest_instance, test_user, cross_nest_foreign,
):
    """Adding ``bob@<other-nest>`` lands a pending knock in Bob's queue THERE."""
    app = logged_in_app
    app.contacts.require_cross_nest_knock()

    foreign = cross_nest_foreign
    foreign_url = foreign["url"]
    authority = foreign["authority"]

    # Reading `peer_url` is what marks this test exclusion class (8) for the
    # mode axis (`helpers/nest_surface.py`'s `PEER_AUTHORITY_KEYS`), and the
    # read is load-bearing rather than a marker flag: the authority Alice types
    # is precisely the authority her home nest is then asked to DIAL, so the two
    # must name the same peer. In standalone they are the same string; in docker
    # they diverge, which is the divergence the class exists for.
    assert foreign["peer_url"].endswith(authority), (
        "the authority typed into Find User must be the one the home nest is "
        f"asked to dial: typed host {authority!r}, peer_url "
        f"{foreign['peer_url']!r}"
    )

    # Bob: a handled actor on the FOREIGN nest, fresh per test (the foreign nest
    # is session-scoped, so a fixed localpart would collide with an earlier run
    # of this test under another `--app`). No key packages needed — a knock is
    # the inbox plane, not MLS.
    from common.auth import register_handled_actor

    bob = register_handled_actor(
        foreign["port"],
        handle=f"bob{secrets.token_hex(4)}",
        domain=authority,
        base_url=foreign_url,
    )
    bob_handle = f"{bob['handle']}@{authority}"
    alice_id = test_user["actor_id_hex"]

    assert alice_id not in _knock_senders(foreign_url, bob), (
        "precondition: Bob must not already hold a knock from Alice"
    )

    # ── The journey, through the UI ──────────────────────────────────────────
    app.contacts.navigate()
    app.contacts.find_by_handle(bob_handle)

    # Two nest round trips (same-nest probe, then the anonymous one against the
    # peer), so the result is deadline-polled, never slept for (convention 14).
    resolved = wait_until(
        lambda: app.contacts.actor_id_result_text() or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"find-error={app.contacts.find_error_text()!r} "
            f"{app.driver.diagnose('contact-actor-id-result')}"
        ),
    )
    assert resolved == bob["actor_id_hex"], (
        f"the foreign lookup should resolve {bob_handle} to Bob's id on the "
        f"peer nest; got {resolved!r}. A same-nest fallback would report "
        "not-found or a LOCAL actor of the same localpart"
    )

    app.contacts.add_contact_no_settle()

    # ── The assertion: the knock is on BOB'S nest, from Alice ────────────────
    senders = wait_until(
        lambda: _knock_senders(foreign_url, bob) or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"no knock reached the foreign nest; app error="
            f"{app.error_text()!r}"
        ),
    )
    assert alice_id in senders, (
        f"Bob should hold a pending knock from Alice {alice_id[:16]}… on his "
        f"OWN nest; senders={senders}"
    )

    assert not app.has_error(), (
        f"sending the cross-nest knock surfaced an error banner: "
        f"{app.error_text()!r}"
    )
