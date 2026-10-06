"""Tier_3 real-wire **cross-nest** FaunaMls round-trip on ``--client {web,linux}``.

This is the GUI counterpart of the native two-nest proof
``bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs`` — it
closes out the web pairing follow-up work
(``docs/goal/architecture/federation.md`` § Implementation status today →
slice 3 step 4: "the **browser** end-to-end cross-nest GUI proof"; the same
section flagged it harness-blocked — this test supplies the missing harness).

Topology — two real nest **binaries** on distinct loopback ports:

* **Home** (``nest_instance``): alice authenticates here over her client's
  production stack (``FaunaMlsBackend`` → ``WsConversationsRpc`` wasm /
  ``NestConversationsRpc`` native) and drives the recipient picker. The home
  nest carries the originating **federation relay**.
* **Foreign** (``cross_nest_foreign``): advertises ``handle_domain =``
  its own loopback authority and opens self-service registration, so **bob** is
  a *handled* actor (``bob@<foreign-authority>``) with a published real key
  package — ``fauna.actor.by_handle`` reports him ``addressable``.

What this exercises that the same-nest ``test_fauna_mls_real_roundtrip`` cannot:

1. **resolve** — alice types the *foreign* handle ``bob@<foreign-authority>``;
   ``resolve_recipient`` → ``resolve_address`` sees a foreign domain → routes to
   ``resolve_foreign`` → the seam's ``actor_by_handle_remote`` opens an anonymous
   WS **directly to the foreign nest** (the browser reaches ``127.0.0.1:<port2>``
   cross-origin; WS is exempt from CORS) and reads ``addressable`` off
   ``fauna.actor.by_handle``.
2. **data plane** — ``send`` bootstraps the group: bob's key package is fetched
   and the MLS Welcome delivered via the **home nest's federation relay**
   (``nest_url`` = the foreign authority; the home nest signs + forwards). We
   observe the foreign-side effects: bob's key package consumed and a Welcome
   relayed into his inbox **on the foreign nest**.

bob is an API-tier actor (no MLS engine), so — exactly as in the same-nest test
— he observes the nest-side effects but does not decrypt; the decrypt round-trip
is proven with two real engines in
``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`` and the native
two-nest conformance test. Runs on ``--client web`` and ``--client linux``
(whichever drives the real ``FaunaMlsBackend`` over its seam); the native UniFFI
apps wait on Track E.
"""

from __future__ import annotations

import pytest

from helpers.agent_refusal import refusal_from
from tests.api import conv_api
from tests.api.conv_api import inbox as _inbox

# `real_conversations`, not just `tier_3`: on windows `real_faunamls_app`
# needs the launch-time `FAUNA_E2E_REAL_CONVERSATIONS` gate, which
# `_apply_real_conversations_env` only sets for a session that COLLECTS a
# `@pytest.mark.real_conversations` test (session-wide, not per-test — see
# the fixture's own docstring). Without this file's own marker it only
# worked by accident when collected alongside `test_fauna_mls_real_roundtrip.py`
# (which does carry it); run this file alone and every test here errors at
# setup: "real FaunaMls backend did not activate within 60.0s." Mirrors
# `test_fauna_mls_real_roundtrip.py`'s own pytestmark.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


@pytest.mark.feature("conversations")
def test_fauna_mls_cross_nest_roundtrip(
    real_faunamls_app, cross_nest_foreign, cross_nest_foreign_actor
):
    app = real_faunamls_app
    foreign = cross_nest_foreign
    bob = cross_nest_foreign_actor
    foreign_port = foreign["port"]
    foreign_base = foreign["url"]
    # The recipient picker resolves a *handle* form (``localpart@authority``),
    # not a 64-hex actor id — Form 2 (foreign-routable) of `resolve_address`.
    # The localpart comes off the fixture (a per-test `bob<random>`), never a
    # literal: the foreign nest is session-scoped, so each test registers its own.
    bob_handle = f"{bob['handle']}@{bob['authority']}"

    # Precondition: bob is addressable on the foreign nest (two KPs published).
    assert conv_api.keypackage_count(foreign_port, bob, bob["actor_id_hex"], scheme="https") == 2, \
        "bob should start with two published key packages on the foreign nest"

    # ── alice resolves the FOREIGN handle and sends — the cross-nest bootstrap ─
    # resolve_recipient → resolve_address("bob@foreign") → resolve_foreign →
    # actor_by_handle_remote (anon WS direct to the foreign nest) → addressable
    # chip; then send: fetch bob's KP + deliver the Welcome via the HOME nest's
    # relay (nest_url = the foreign authority).
    app.conversations.real_resolve_send_new(bob_handle, "hi bob across nests")

    # The thread landed on the **FaunaMls** rail, asserted BEFORE the nest-side
    # counts so this test diagnoses its own most likely failure (convention 6).
    # `resolve_foreign` maps *every* seam error — including a transport failure
    # reaching the foreign nest — to `NotFound` (`fauna_mls.rs`), and
    # `resolve_recipient` then falls back to the format-only parse
    # (`manager.rs`), which turns `bob@<authority>` into a plain
    # `TypedAddress::Email` (`address.rs`: any `@` is an email address). The send
    # then goes out over SMTP and throws nothing at all, so an unreachable
    # foreign nest presents downstream as a bare `assert 2 == 1` on a key-package
    # count with no hint of the cause. This assert names it instead.
    rails = sorted({t.rail for t in app.conversations.list_threads()})
    assert rails == ["FaunaMls"], (
        f"the foreign handle should resolve on the FaunaMls rail, got rails={rails}. "
        "'Smtp' means the recipient resolve fell through to the email fallback — "
        "the foreign nest was unreachable from this client (its anonymous "
        "`actor_by_handle_remote` hop failed), NOT that the MLS bootstrap misbehaved."
    )

    # bob's key package was consumed on the foreign nest (the home relay fetched
    # it there — the home nest has none of bob's packages).
    assert conv_api.keypackage_count(foreign_port, bob, bob["actor_id_hex"], scheme="https") == 1, \
        "the cross-nest bootstrap should consume one of bob's foreign key packages"

    # ... and the MLS Welcome was relayed into bob's inbox ON THE FOREIGN NEST.
    assert len(_inbox(foreign_base, bob)) >= 1, \
        "a Welcome should be relayed to bob's inbox on the foreign nest"


# ── Discovery-failure semantics ─────────────────────────────────────────────
#
# ``docs/goal/architecture/federation.md`` § Peer-auth model → *Discovery-failure
# semantics* (ratified 2026-08-29) + ``docs/goal/ui/conversations.md`` § Errors &
# edge cases → *The picker tells the truth*. The product bug behind the
# harness gap the test above records: a foreign nest that did not ANSWER used
# to read exactly like "not a Fauna peer", the recipient degraded to a plain
# Email chip, and the DM left over SMTP with no error raised anywhere. The rule
# now: a domain this account already converses with over Fauna is KNOWN; when
# its nest does not answer the picker says so (``error``), commits no chip, and
# nothing is sent on any rail. A never-seen domain that does not answer is
# email by ruling (first contact), exactly like a domain with no nest at all.


@pytest.mark.feature("conversations")
def test_known_peer_unreachable_never_downgrades_to_email(
    real_faunamls_app, cross_nest_foreign_ephemeral
):
    """A peer alice has reached over Fauna goes away; re-resolving a handle at
    that domain must be an ``error`` — never a silently-accepted email chip."""
    app = real_faunamls_app
    foreign = cross_nest_foreign_ephemeral
    bob = foreign["actor"]
    bob_handle = f"{bob['handle']}@{bob['authority']}"

    # alice's session actor is session-scoped (`test_user`, conftest.py) and may
    # already carry threads an earlier module left on other rails — the
    # shared-identity trap: any test on the shared test_user that picks a
    # thread by flavor, label or position carries this bug
    # latently. Pin every rail check below to
    # the threads THIS run creates, not every rail alice has ever touched —
    # same idiom as `test_fauna_mls_real_roundtrip`'s
    # `known = {t.thread_id for t in ...}`.
    known = {t.thread_id for t in app.conversations.list_threads()}

    # 1. First contact while the peer is up: the FaunaMls rail claims bob and
    #    a thread lands on it (the same bootstrap the round-trip test proves).
    app.conversations.real_resolve_send_new(bob_handle, "hi bob, while your nest is up")
    threads_before = [
        t for t in app.conversations.list_threads() if t.thread_id not in known
    ]
    rails_before = sorted({t.rail for t in threads_before})
    assert rails_before == ["FaunaMls"], (
        f"precondition: the first contact must land on the FaunaMls rail, got {rails_before}"
    )

    # 2. The peer goes away. bob's domain is now a KNOWN Fauna domain that
    #    does not answer — the exact shape that used to fall through to SMTP.
    foreign["stop"]()

    # 3. Re-resolve through the picker UI: the status must read `error`, not
    #    `resolved` (the SMTP rail must not have been allowed to claim it).
    state = app.conversations.resolve_recipient(bob_handle)
    assert state == "error", (
        f"a known Fauna domain whose nest does not answer must resolve `error`, got "
        f"{state!r} — `resolved` means the chain fell through to the email rail. "
        f"{app.driver.diagnose('recipient-resolve-status')}"
    )

    # 4. Enter commits nothing: the accept command DECLINES rather than acking
    #    green (convention 11), and no chip appears. The decline rides whichever
    #    loud channel the app has: web throws out of the driver call, the native
    #    apps stamp their refusal slot. So read both (`helpers/agent_refusal.py`).
    #    A bare `call_command` here raised on web for the very decline this step
    #    expects. The command's return is the causal barrier: the manager has
    #    already decided, so the chip count is a state read, not a settle-wait.
    refusal = refusal_from(app, "conversations_accept_recipient", {})
    assert "conversations_accept_recipient" in refusal and "nothing committed" in refusal, (
        f"accepting an errored resolve must DECLINE and name why (convention 11), "
        f"got {refusal!r}"
    )
    assert app.driver.count("recipient-picker-chip") == 0, (
        f"no chip may be committed from an errored resolve: "
        f"{app.driver.diagnose('recipient-picker-chip')}"
    )

    # 5. Nothing new was created on any rail — same new-thread count, still only
    #    FaunaMls (an `Smtp` here is the downgrade this test exists to forbid).
    #    Pinned to `known` exactly like step 1 — alice's pre-existing threads
    #    (any rail) must not count against "no new thread".
    threads_after = [
        t for t in app.conversations.list_threads() if t.thread_id not in known
    ]
    assert len(threads_after) == len(threads_before), (
        f"no new thread may appear from a refused resolve: "
        f"{[(t.thread_id, t.rail) for t in threads_after]}"
    )
    rails_after = sorted({t.rail for t in threads_after})
    assert rails_after == ["FaunaMls"], (
        f"an unreachable known peer must never produce an Smtp thread, got {rails_after}"
    )


@pytest.mark.feature("conversations")
def test_first_contact_with_unreachable_authority_is_email(real_faunamls_app):
    """A domain nothing vouches for, whose nest does not answer, resolves as
    email — by ruling — and the chip commits: first contact with an
    unadvertised, unreachable nest is indistinguishable from a domain with no
    nest at all, and the chip shows the email rail."""
    from drivers.port_util import find_free_port
    app = real_faunamls_app
    # A loopback authority nothing listens on: the anonymous hop is refused at
    # once, and this account has never held a Fauna thread at that domain.
    stranger = f"nobody@127.0.0.1:{find_free_port()}"

    state = app.conversations.resolve_recipient(stranger)
    assert state == "resolved", (
        f"first contact with an unreachable, never-seen domain falls through to "
        f"email and must read `resolved`, got {state!r}. "
        f"{app.driver.diagnose('recipient-resolve-status')}"
    )
    app.conversations.accept_recipient_chip(stranger)
