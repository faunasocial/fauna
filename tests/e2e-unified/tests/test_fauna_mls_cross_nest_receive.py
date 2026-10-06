"""Tier_3 **cross-nest** FaunaMls receive proof — a GUI on nest B decrypts a DM
whose channel log lives on nest A.

The missing inverse of ``test_fauna_mls_cross_nest_roundtrip``. There the GUI is
the **sender** and the far seat is an API-tier actor with no MLS engine, so — by
its own docstring — it "does not decrypt": it can witness the nest-side effects
of a cross-nest bootstrap but never that a cross-nest DM *arrives and opens* in a
real client. The two web receive proofs are the mirror gap: both are same-nest
(``test_fauna_mls_web_receive.py`` layer 4, ``test_fauna_mls_web_receives_from_
linux_sender.py`` layer 5). So no test decided whether a client can RECEIVE
cross-nest, and three goal docs disagreed about it.
This test is the witness that settles it; ``docs/goal/behavior/direct-messages.md``
§ Implementation status today is the one owner that states the outcome.

Topology — two real nest **binaries**, the channel deliberately on the far one:

* **nest A** (``cross_nest_foreign``): alice's home. Advertises a
  ``handle_domain`` equal to its own loopback authority, so its
  ``originate_welcome_deliver`` has an ``origin_nest_url`` to stamp — a
  handle-domain-less sender nest yields an EMPTY ``nest_url`` and the recipient
  records no channel home at all (``conversations_handlers.rs``
  ``handle_domain_if_set()``; the same reason ``test_folder_cross_nest_foreign_
  row.py`` uses this fixture rather than a bare ``second_nest``). https.
* **nest B** (``nest_instance``): bob's home, the nest his GUI is logged into.

The chain under test, which ONLY a cross-nest receiver runs:

  alice mints a real 1:1 group against bob's published key package and posts the
  sealed application envelope to the channel **on nest A** → she delivers the
  Welcome with ``nest_url`` = bob's nest, so nest A relays it over the federation
  channel and stamps it with A's own ``origin_nest_url`` → the envelope lands in
  bob's durable inbox **on nest B** → bob's receive loop drains it, and the
  ingest arm reads that stamp as ``home_nest_url`` (web: ``libs/fauna-wasm/src/
  conversations.rs`` → ``ingest_welcome_by_kind``; native: the shared
  ``InboxDrainSource``) → the DM arm records the channel's home
  (``record_channel_home``) → the next rail sweep's ``channel.fetch`` carries
  ``home_nest_url``, so bob's own nest relays the pull to nest A over the
  membership-gated ``fauna.federation.channel.fetch`` → the ciphertext comes back
  and bob's engine DECRYPTS it into a thread.

**Why the body is the assertion, and why it cannot be reached same-nest.** The
message log for this channel exists on nest A and nowhere else — nest B never saw
a ``channel.send`` for it, so there is no byte on B that could satisfy the final
assertion. A recipient that recorded no channel home fetches its own nest's empty
local log, joins the group, and shows a thread with NO message; only a fetch that
followed the recorded home can produce the decrypted ``body``. That is the whole
argument, and it holds by construction rather than by observation. Step 5's
control merely makes it visible (and would catch a future change that started
mirroring foreign channels onto B) — it is a guard, not the proof, and it
tolerates a nest that refuses a non-member outright.

Client-generic: one test, every app whose real ``FaunaMlsBackend`` runs
(``real_faunamls_app`` — linux/web/tui today). The web leg is the one the docs
were undecided about; the tui leg is the lead app's first cross-nest receive
witness. Neither needs a per-app branch — the receive path is shared Rust under
both, which is the point.

Arm isolation is deliberately NOT done here (unlike the same-nest layer-5 twin's
``set_conv_push_suppressed``): which local arm wakes the rail is orthogonal to
whether the rail can reach another nest, and both arms converge on the same
drain-apply pass. The arms are split and pinned individually by
``test_fauna_mls_web_receives_from_linux_sender.py`` (drain alone) and
``test_conv_rail_push_wakes_web.py`` (push alone).

Convention 8: alice is precondition setup (a headless peer on a nest no GUI is
driving); the behaviour under test — the thread appearing, decrypted — is read
entirely through the app UI.
"""

from __future__ import annotations

import secrets

import pytest

from common.auth import register_handled_actor
from helpers.app_surface import skip_unbuilt
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import wait_until
from tests.api import conv_api

# `real_conversations`, not just `tier_3`: the launch-gated clients need the
# session-wide `FAUNA_E2E_REAL_CONVERSATIONS` env that `_apply_real_conversations
# _env` only sets for a session that COLLECTS this marker — the same reasoning
# `test_fauna_mls_cross_nest_roundtrip.py` spells out at its own pytestmark.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


@pytest.mark.feature("conversations")
def test_fauna_mls_cross_nest_receive(
    real_faunamls_app, nest_instance, test_user, cross_nest_foreign
):
    """A DM sent from another nest arrives in bob's GUI and decrypts — the
    channel log never leaves nest A, so the body proves the cross-nest fetch."""
    bob_app = real_faunamls_app
    driver = bob_app.driver
    if not (driver.is_web() or driver.is_tui() or driver.is_linux()):
        skip_unbuilt(
            driver,
            surface="the real-wire cross-nest FaunaMls receive seam",
            detail="the real FaunaMlsBackend activates here only behind the "
            "launch-time gate with no client-side readiness signal, so a run "
            "would silently prove nothing (see `real_faunamls_app`'s docstring)",
            tracked="`docs/goal/behavior/direct-messages.md` § Implementation "
            "status today (Track E, the same gate "
            "`test_fauna_mls_cross_nest_roundtrip` carries)",
        )

    nest_b = nest_instance  # bob's home — the nest his GUI is logged into
    nest_a = cross_nest_foreign  # alice's home — and the channel's home
    bob_id = test_user["actor_id_hex"]
    body = f"hi bob across nests {secrets.token_hex(3)}"

    # ── alice: a handled actor on nest A (the far nest) ───────────────────────
    alice = register_handled_actor(
        nest_a["port"],
        handle="xnalice" + secrets.token_hex(3),
        domain=nest_a["authority"],
        base_url=nest_a["url"],
    )

    # ── 1. bob's GUI engine has published a key package on his own nest ───────
    # The real-backend opt-in fires `ensureKeypackages` fire-and-forget, so the
    # publish lands a moment after the fixture returns. bob is the authed caller
    # for the count of his own packages.
    wait_until(
        lambda: conv_api.keypackage_count(nest_b["port"], test_user, bob_id) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "bob's GUI never published a key package "
        "(real-backend ensureKeypackages) — nothing for alice's cross-nest "
        "bootstrap to fetch",
    )

    # ── 2. bob accepts alice, or his nest refuses her Welcome outright ────────
    # Since 2026-08-02 the DM plane consults the recipient's inbox mode, and a
    # fresh actor's stored default is `allow_knock` ⇒ Knock ⇒
    # `fauna.conversations.forbidden` (`direct-messages.md` § Reach policy).
    # Arranged on bob's side because HE is the recipient. Per-pair, keyed on
    # alice's actor id, which the relayed envelope carries as its sender.
    conv_api.accept_contact(nest_b["port"], test_user, alice["actor_id_hex"])

    # ── 3. alice fetches bob's key package CROSS-NEST ─────────────────────────
    # Nest A relays the fetch to nest B (`originate_keypackage_fetch`); this also
    # proves the A→B federation channel dials at all, which is the precondition
    # for step 4's relay in the other direction.
    kp = wait_until(
        lambda: conv_api.keypackage_fetch(
            nest_a["port"], alice, bob_id,
            nest_url=nest_b["peer_url"], scheme="https",
        ),
        MLS_HANDSHAKE_S,
        interval=1.0,
        diagnose=lambda: "alice's nest never consumed one of bob's key packages "
        "over the A→B federation relay — either bob published none or the relay "
        "never dialled",
    )

    # ── 4. A real 1:1 group + Welcome + sealed application message ────────────
    # bob is a member from group creation, so once his GUI joins the Welcome he
    # can decrypt the envelope — that is what makes this a DECRYPT proof rather
    # than a join proof.
    channel_id, welcome_bytes, app_envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), kp, body
    )

    # The message log lands ON NEST A and is never written to nest B. Everything
    # this test proves rests on that asymmetry.
    conv_api.channel_send(
        nest_a["port"], alice, channel_id, app_envelope, scheme="https"
    )

    # ── 5. Control: bob's own nest holds NO copy of that log ──────────────────
    # Asserted BEFORE the delivery so it reads as a property of the topology, not
    # a race with the drain. A nest that is not the channel's home answers either
    # with an empty log or by refusing the reader as a non-member; both are the
    # same fact, and naming both is what keeps a later refusal-shape change from
    # silently turning this control into a no-op.
    try:
        local = conv_api.channel_fetch(nest_b["port"], test_user, channel_id)
        assert local == [], (
            f"bob's OWN nest must hold no messages for a channel homed on nest A; "
            f"it returned {len(local)} — the cross-nest assertion below would then "
            "be satisfiable without any federation hop, making this test vacuous"
        )
    except AssertionError:
        raise
    except Exception:
        pass  # refused as a non-member: the same fact, stated louder

    # ── 6. The CROSS-NEST welcome relay: nest A → nest B, stamped with A's URL ─
    conv_api.welcome_deliver(
        nest_a["port"], alice, bob_id, channel_id, welcome_bytes,
        kind={"type": "dm"},
        nest_url=nest_b["peer_url"],
        scheme="https",
    )

    # ── 7. THE PROOF: the decrypted body surfaces in bob's GUI ────────────────
    # Drain → ingest (reading the `nest_url` stamp as the channel's home) → a
    # rail sweep whose `channel.fetch` carries `home_nest_url` → bob's nest
    # relays the pull to nest A → decrypt. A latency-independent state poll
    # (convention 14): the assertion is the thread, never the elapsed time.
    got = wait_until(
        lambda: next(
            (
                t for t in bob_app.conversations.list_threads()
                if t.rail == "FaunaMls" and body in (t.snippet or "")
            ),
            None,
        ),
        MLS_HANDSHAKE_S,
        interval=1.0,
        diagnose=lambda: (
            "bob's GUI never decrypted alice's cross-nest message. His threads: "
            f"{[(t.rail, t.flavor, t.snippet) for t in bob_app.conversations.list_threads()]}. "
            "A FaunaMls thread with an EMPTY snippet means the Welcome arrived and "
            "the group was joined, but the channel fetch did not follow the "
            "recorded home nest — the cross-nest receive arm. No thread at all "
            "means the relayed Welcome never reached his inbox on nest B."
        ),
    )
    assert got.flavor == "OneToOne", \
        f"expected a 1:1 FaunaMls thread, got flavor={got.flavor!r}"
    assert got.message_count >= 1, (
        "the decrypted application envelope should surface as a message; the "
        f"thread carried the body but message_count={got.message_count}"
    )
