"""Tier_3 two-driven-GUI FaunaMls receive proof — the **web** leg of layer 5.

Layer 5 (web leg) of the durable inbox-apply consumer
(``docs/goal/architecture/api-layers.md`` § Inbox & Messaging — "Durable
inbox-apply consumer", layer 5; the native leg is
``test_fauna_mls_two_client_inbox_drain.py``). Proves a **web** GUI member
*receives + decrypts* a real Welcome + message from a real-engine sender, over
two driven GUIs and the real wire — no CLI-minted Welcome (the layer-4 web proof
``test_fauna_mls_web_receive.py`` 's shortcut).

* **alice** (``real_faunamls_linux_sender``) is a fresh real-engine **linux** GUI
  app (the SENDER). She resolves bob by 64-hex actor id and sends — the real
  MLS group bootstrap: fetch bob's key package → create the group → deliver the
  Welcome to bob's durable inbox → post the Application envelope.
* **bob** (``real_faunamls_app``, web) is the cached **web** GUI. The sender is
  a linux GUI rather than a second web one for cross-app value (one real
  linux engine, one real web engine) — historically it was also forced by the
  snap-Chromium singleton flock, which is gone (bundled Chromium since
  2026-07-14; a second web driver is safe now). bob receives over the layer-4 durable
  poll: ``startReceivePoll`` ticks the wasm ``drainInbox`` (join the Welcome's group)
  then ``pollConversations`` (pull the newly-bound channel's ciphertext) → his web
  engine **decrypts** alice's message into a FaunaMls thread.

⚠ **The drain arm is isolated EXPLICITLY as of 2026-08-15 — it used to rest on a
premise that had been false for a month.** The original wording ("bob has NO push
subscription on web, poll-only by construction") stopped being true on **2026-07-12**,
when the SPA grew a push arm on the same
``fauna.conversations.{channel.message,welcome.received}`` kinds (``transport.md``
§ Push events). From then until this fix either arm could satisfy the assertion below,
so a dead *drain* arm did not red here — the test still passed, which is why nothing
reported it. It now suppresses bob's push + reconnect arms for the delivery window
(``set_conv_push_suppressed``, web's twin of native's
``FAUNA_E2E_SUPPRESS_CONV_PUSH``), restoring what the docstring always claimed.

Its mirror is ``test_conv_rail_push_wakes_web.py``, which mutes the *ticker* instead
and so proves the push arm alone. Between them each arm of the web rail is covered on
its own rather than jointly — the split convention 14 asks for.

The same round-trip also proves the FaunaMls-rail **web blob client** (the wasm
``WsConversationsRpc::blob_get``, previously stubbed ``Err``): alice sends a second
message with an image attachment (her native ``blob_put`` seals + uploads the blob
to the nest's content-addressed ``/api/v1/blob``); bob's web GUI fetches it via the
wasm ``blob_get``, opens it under the message epoch key, and renders
``dm-attachment-image`` off the real nest blob — not a local inject. This is the
only test path for the web blob client (``docs/goal/ui/conversations.md`` §
Attachments).

Same-nest **by construction**: alice + bob share the one session ``nest_instance``
(alice the linux driver direct, bob the web SPA via the proxy), so nothing here
exercises a cross-nest hop. This docstring used to explain that as the wasm
``ingestWelcome`` being "native-gated", citing ``caldav-server.md`` § Impl status;
both halves were wrong — the ingest arm reads ``welcome.nest_url`` on every client
and that doc never owned this status. The cross-nest receive question is owned by
``direct-messages.md`` § Implementation status today and witnessed by
``test_fauna_mls_cross_nest_receive.py``; this test's scope is the drain arm,
same-nest.

web-only — the native two-real-GUI proof is ``test_fauna_mls_two_client_inbox_drain``
(``--client linux``).
"""

from __future__ import annotations

import time

import pytest

from helpers.app_surface import declared_absence
from tests.api import conv_api

pytestmark = [pytest.mark.web, pytest.mark.tier_3]

# A real 1x1 PNG so the receiver's image-render arm actually decodes it (same
# fixture byte string `test_conversations_attachments.py` uses for the inject
# rail). The byte-correct seal→blob_put→blob_get→open round-trip is covered
# in-process by `attachment_round_trips_outbound_to_inbound_faunamls`; here we
# prove the WEB transport leg — the wasm `blob_get` over a real nest.
_PNG_1x1_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAf"
    "FcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAA"
    "AABJRU5ErkJggg=="
)


@pytest.mark.feature("conversations")
def test_fauna_mls_web_receives_from_linux_sender(
    request, real_faunamls_app, real_faunamls_linux_sender, nest_instance, test_user
):
    bob_app = real_faunamls_app
    if not bob_app.driver.is_web():
        declared_absence(
            bob_app.driver,
            capability="the web layer-5 receive proof",
            doc="testing.md § Cross-app e2e conventions, point 7 (the "
            "native two-GUI drain proof is its own twin, "
            "test_fauna_mls_two_client_inbox_drain.py --client linux)",
        )
    alice_app, alice = real_faunamls_linux_sender
    port = nest_instance["port"]
    bob_actor_id = test_user["actor_id_hex"]  # the web GUI is the recipient
    body = "hi web via drain"

    # ── Precondition: bob's WEB engine has published a key package on the nest ──
    # (the real-backend opt-in fires `ensureKeypackages` fire-and-forget), so
    # alice's resolve can promote bob's 64-hex actor id to a Fauna chip and her
    # bootstrap can fetch one. bob keeps the private half — that lets him DECRYPT.
    deadline = time.time() + 20
    while time.time() < deadline:
        if conv_api.keypackage_count(port, alice, bob_actor_id) >= 1:
            break
        time.sleep(0.5)
    else:
        raise AssertionError(
            "bob's web client never published a key package "
            "(real-backend ensureKeypackages)"
        )
    before = conv_api.keypackage_count(port, alice, bob_actor_id)

    # bob must ACCEPT alice, or the nest refuses her Welcome outright: since
    # 2026-08-02 the DM plane consults the recipient's inbox mode, and bob's
    # stored default is `allow_knock` ⇒ Knock ⇒ `fauna.conversations.forbidden`
    # (direct-messages.md § Reach policy). Arranged on bob's side because HE is
    # the recipient here — the mirror of the sender-side fixtures elsewhere.
    conv_api.accept_contact(port, test_user, alice["actor_id_hex"])

    # ── Isolate the DRAIN arm: bob's push + reconnect arms go inert for the rest
    # of this test, so the only thing that can deliver either message below is the
    # backstop ticker's `drainInbox` → `pollConversations` pass — which is what
    # this test has always claimed to prove. Web's twin of native's
    # `FAUNA_E2E_SUPPRESS_CONV_PUSH` (see the ⚠ in the module docstring for why
    # this became necessary). Restored via a finalizer rather than a try/finally
    # so the assertions below stay at one indent level; the shared session driver
    # must not leak a suppressed rail into the next test either way.
    request.addfinalizer(lambda: bob_app.driver.set_conv_push_suppressed(False))
    bob_app.driver.set_conv_push_suppressed(True)

    # ── alice (real linux engine) resolves bob by actor id and sends — the real
    # cross-engine bootstrap. With bob's push arm suppressed the Welcome reaches
    # him ONLY from the durable inbox queue his web `startReceivePoll` drains.
    alice_app.conversations.real_resolve_send_new(bob_actor_id, body)

    # alice's bootstrap consumed one of bob's key packages (the Welcome was minted
    # against it) — a nest-side check the cross-engine bootstrap really fired.
    assert conv_api.keypackage_count(port, alice, bob_actor_id) < before, \
        "the 1:1 bootstrap should consume one of bob's key packages"

    # ── bob's WEB GUI drains his inbox, joins the Welcome's group, pulls the
    # channel history, and DECRYPTS alice's message into a FaunaMls thread.
    # Poll his snapshot until the 1:1 thread carrying the decrypted body appears —
    # the canonical InboxEnvelope{Welcome} → fauna.inbox.fetch → drainInbox →
    # ingestWelcome → pollConversations → real-engine decrypt round-trip over the
    # real wire, with the push arm suppressed so the drain is the only path.
    deadline = time.time() + 40
    got = None
    while time.time() < deadline:
        fauna = [
            t for t in bob_app.conversations.list_threads() if t.rail == "FaunaMls"
        ]
        got = next((t for t in fauna if body in t.snippet), None)
        if got is not None:
            break
        time.sleep(1.0)

    threads_dump = [
        (t.rail, t.flavor, t.snippet, t.message_count)
        for t in bob_app.conversations.list_threads()
    ]
    assert got is not None, (
        "bob's web GUI should receive + decrypt alice's message via the drain "
        "backstop — his push + reconnect arms are suppressed for this test, so "
        "this is the DRAIN arm failing, not a missing push; his threads were: "
        f"{threads_dump}"
    )
    assert got.flavor == "OneToOne", \
        f"expected a 1:1 FaunaMls thread, got flavor={got.flavor!r}"
    assert got.message_count >= 1, \
        "the decrypted Application envelope should surface as a message"

    # ── C.2 / A-remainder: the FaunaMls-rail web blob client (the wasm `blob_get`).
    # alice sends a second message carrying an image attachment over the REAL rail:
    # her native `blob_put` seals the bytes under the channel epoch blob key and
    # uploads the sealed blob to the nest's content-addressed `/api/v1/blob`. bob's
    # web GUI must fetch it via the wasm `blob_get` (the previously-stubbed arm this
    # track implements), open it under the message epoch key, cache the plaintext,
    # and render `dm-attachment-image` — NOT a local inject. This is the only test
    # path for the web blob client (`conversations.md` § Attachments).
    alice_thread = next(
        (t for t in alice_app.conversations.list_threads() if t.rail == "FaunaMls"),
        None,
    )
    assert alice_thread is not None, \
        "alice should have her FaunaMls thread after the bootstrap send"
    alice_app.conversations.real_send_attachment(
        alice_thread.thread_id, "see attached", "pic.png", "image/png", _PNG_1x1_B64
    )

    # Wait until the attachment message has surfaced in bob's snapshot (a second
    # message) — by then the inbound path has run `blob_get` → decrypt → cache —
    # then open the thread and assert the rendered image attachment.
    deadline = time.time() + 40
    while time.time() < deadline:
        bob_fauna = [
            t for t in bob_app.conversations.list_threads() if t.rail == "FaunaMls"
        ]
        if bob_fauna and bob_fauna[0].message_count >= 2:
            break
        time.sleep(1.0)

    bob_app.conversations.open_thread_by_rail("FaunaMls")
    assert bob_app.driver.is_visible("thread-header"), "thread detail empty on select"
    deadline = time.time() + 40
    rendered = 0
    while time.time() < deadline:
        rendered = bob_app.driver.count("dm-attachment-image")
        if rendered >= 1:
            break
        time.sleep(1.0)
    assert rendered >= 1, (
        "bob's web GUI should render the inbound FaunaMls attachment off the "
        "fetched + decrypted nest blob (wasm blob_get over /api/v1/blob), got "
        f"{rendered} dm-attachment-image elements"
    )
