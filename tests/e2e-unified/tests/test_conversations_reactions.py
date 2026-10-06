"""tier_3 e2e: conversation message reactions (conversations.md § Reactions & message delete).

Phase C windows leg — verifies the WinUI render (C2):
the per-bubble ⋯ ``dm-message-actions-button`` → ``dm-message-actions-menu`` →
``dm-reaction-option[i]`` quick-set path.

Three assertions:

1. **Reaction UI shape (single-client)**: open a FaunaMls thread, confirm the ⋯
   ``dm-message-actions-button`` is present, open the flyout, and confirm the six
   ``dm-reaction-option`` MenuFlyoutItems are realized by AutomationId — this is
   the key FlaUI realization check.  The actual reaction-pill round-trip is gated
   on ``me_actor()`` (the FaunaMls backend's ``self_address``), which the e2e mock
   backend does not set.  The pill path is therefore deferred to the real-FaunaMls
   tier_3 suite (``test_fauna_mls_real_roundtrip``/linux).  The render wiring is
   proven by the C# unit tests in ``ConversationsReactionsTests``.

2. **Cross-member peer reaction** (client-agnostic, runs green wherever the
   linux sender fixture is available; ``real_faunamls_app`` skips on non-linux/web).

3. **Capability gate** (mail/SMTP thread, ``supports_reactions=False``, own message):
   the ⋯ flyout button is absent entirely — reactions and delete are both
   capability-gated off for SMTP, and mark-as-spam (the third, deliberately
   rail-**independent** action — ``conversations.md`` § Reactions & message
   delete, ``mail-spam.md``) only ever offers on a *received* message, so an
   **own** SMTP message isolates the reactions/delete gate from it.

⚠ FlaUI ``MenuFlyoutItem`` realization: if ``dm-reaction-option`` count = 0 after
the flyout opens, the action helper raises with BLOCKED context — do NOT weaken the
assertion.
"""

import time

import pytest

from tests.api import conv_api

pytestmark = [pytest.mark.tier_3]

# Six quick-set emoji — the fixed shared order from C2/ui.yaml.
_QUICK_SET_EMOJI = ["👍", "❤️", "😂", "😮", "😢", "🙏"]
_QUICK_SET_COUNT = len(_QUICK_SET_EMOJI)


@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("reactions-and-message-delete")
def test_reaction_option_elements_present_in_flyout(logged_in_app):
    """The ⋯ flyout on a FaunaMls bubble carries 6 ``dm-reaction-option`` items
    reachable by AutomationId — the key FlaUI render-shape proof.

    Uses ``create_mls_group`` + a SENT message so the bubble lands in the open
    thread (``inject_inbound`` keys a thread by sender, not the open group).  The
    ⋯ button is shown because ``supports_reactions=True`` for FaunaMls.

    Note: clicking a ``dm-reaction-option`` triggers ``ToggleReactionAsync`` on the
    VM, which no-ops in the e2e mock environment because the FaunaMls mock backend
    does not expose a ``self_address`` (``me_actor()`` returns ``None``).  The pill
    round-trip is deferred to the real-FaunaMls tier_3 suite; this test proves only
    the render shape is correct.
    """
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    # Seed an own FaunaMls bubble in an open thread — the ⋯ button shows because
    # supports_reactions=True for FaunaMls. Seeded uniformly across every app
    # (seed_own_message's per-app branch collapsed). Own peer, own
    # thread: the default peer is shared with other files that assert
    # bubble/pill counts in this session-scoped run.
    conv.seed_own_message(
        "hello reaction shape test", recipient="bob-reactions@self-nest.test"
    )
    assert d.count("dm-message-text") >= 1, "no message bubble rendered after seeding"

    # The ⋯ button must be present (supports_reactions = True for FaunaMls).
    actions_count = d.count("dm-message-actions-button")
    assert actions_count >= 1, (
        "dm-message-actions-button must be present for a FaunaMls thread "
        "(supports_reactions=True); got count=0. Capability gate or ⋯ render is broken."
    )

    # Open the ⋯ flyout — this proves the button is Invokable.
    conv.open_message_actions(message_index=0)

    # Poll for the flyout items to appear in the popup UIA subtree.
    deadline = time.time() + 5.0
    option_count = 0
    while time.time() < deadline:
        option_count = d.count("dm-reaction-option")
        if option_count >= _QUICK_SET_COUNT:
            break
        time.sleep(0.2)

    assert option_count == _QUICK_SET_COUNT, (
        f"Expected {_QUICK_SET_COUNT} dm-reaction-option items (one per quick-set "
        f"emoji: {_QUICK_SET_EMOJI}) in the ⋯ flyout; got {option_count}. "
        "If count=0, the MenuFlyoutItems are NOT realized by AutomationId after the "
        "flyout opens — BLOCKED: C2 render shape needs a fix "
        "(e.g. MenuFlyoutItem→Button swap or explicit AutomationPeer override). "
        "If count is between 1-5, the flyout opened but some items are missing."
    )


@pytest.mark.real_conversations
@pytest.mark.feature("reactions-and-message-delete")
def test_react_cross_member_peer_sees_pill(
    real_faunamls_app, real_faunamls_linux_sender, nest_instance, test_user
):
    """A peer actor (linux engine) reacts to a message → the recipient's GUI shows the pill.

    This test requires a live linux sender fixture, available only on hosts where
    both the linux and web drivers run.  ``real_faunamls_app`` skips on non-linux/web
    (Track E for native UniFFI apps).

    Written client-agnostic so it runs on any machine where the two-engine pair is
    available (linux + web on the same host).
    """
    bob_app = real_faunamls_app
    alice_app, alice = real_faunamls_linux_sender

    # bob must ACCEPT alice first: since 2026-08-02 the DM plane consults the
    # recipient's inbox mode, and bob's stored default is `allow_knock` ⇒ Knock
    # ⇒ the nest refuses her Welcome with `fauna.conversations.forbidden`
    # (direct-messages.md § Reach policy).
    conv_api.accept_contact(
        nest_instance["port"], test_user, alice["actor_id_hex"]
    )

    # alice sends a message to bob.
    alice_app.conversations.real_resolve_send_new(test_user["actor_id_hex"], "hi from alice for react")

    # Wait until bob receives the message.
    deadline = time.time() + 40
    got = None
    while time.time() < deadline:
        threads = bob_app.conversations.list_threads()
        got = next(
            (t for t in threads if "hi from alice for react" in (t.snippet or "")), None
        )
        if got is not None:
            break
        time.sleep(1.0)
    assert got is not None, "bob never received alice's message"

    bob_app.conversations.open_thread_by_rail("FaunaMls")
    assert bob_app.driver.is_visible("thread-header"), "thread-header not visible"

    # alice reacts via UI automation on her side. She sent via the command bridge
    # (real_resolve_send_new drives her manager but leaves her GUI on the feed view
    # the fixture launched into), so open her conversations thread first — otherwise
    # her own message bubble + its ⋯ dm-message-actions-button are never rendered to
    # click. (This test runs only on --client web: real_faunamls_linux_sender skips
    # every other parametrization, so this alice-side nav was never exercised before.)
    alice_app.conversations.open_thread_by_rail("FaunaMls")
    assert alice_app.driver.is_visible("thread-header"), "alice's thread-header not visible"
    alice_app.conversations.react_to_message(message_index=0, reaction_index=0)

    # Wait until bob's bubble shows the pill from alice's reaction.
    deadline = time.time() + 30
    while time.time() < deadline:
        pills = bob_app.driver.count("dm-reaction-pill")
        if pills >= 1:
            break
        time.sleep(1.0)

    assert bob_app.driver.count("dm-reaction-pill") >= 1, (
        "bob's bubble should show a dm-reaction-pill after alice's cross-engine reaction"
    )


@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("reactions-and-message-delete")
def test_capability_gate_no_button_on_smtp_thread(logged_in_app):
    """On a mail/SMTP thread (``supports_reactions=False``,
    ``supports_message_delete=False``), the ⋯ ``dm-message-actions-button`` is
    absent (Collapsed — no UIA peer) for a message the viewer cannot react to,
    delete, or flag as spam.

    The ⋯ button visibility condition (windows ``DmMessageBubble.xaml.cs``,
    mirrored by every app) is:
    ``SupportsReactions || (SupportsMessageDelete && msg.IsOwn) || (!msg.IsOwn)``
    — the third term is mark-as-spam, deliberately **not** capability-gated
    (``conversations.md`` § Reactions & message delete: "a single
    ``dm-message-actions-button`` ... shown iff ≥1 action is available",
    listing ``dm-message-mark-as-spam-button`` among the actions;
    ``mail-spam.md``: gated ``!is_own`` only, rail-independent "mirroring
    linux/windows"). So a *received* SMTP message still shows the ⋯ (correctly
    — mark-as-spam is real and rail-independent by design). This test isolates
    the reactions/delete gate specifically by seeding an **own** message
    (``is_own=True``): mark-as-spam never applies to your own message, so on
    SMTP (no reactions, no delete) the button is genuinely absent.
    """
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="Smtp",  # MUST match ParseRail (Rail enum variant casing) — "SMTP" falls through to FaunaMls
        sender="external@example.com",
        subject="smtp-capability-gate",
        body="smtp message capability gate test",
        is_own=True,
    )
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if d.count("dm-message-text") >= 1:
            break
        time.sleep(0.1)
    assert d.count("dm-message-text") >= 1, "no bubble on SMTP thread"

    # The ⋯ button must be absent (Collapsed = no UIA peer = count 0) for an
    # own SMTP message: no reactions, no delete (mail rail), and mark-as-spam
    # never offers on your own message.
    actions_count = d.count("dm-message-actions-button")
    assert actions_count == 0, (
        f"an OWN SMTP message (supports_reactions=False, supports_message_delete=False, "
        f"mark-as-spam N/A on own messages) must have NO dm-message-actions-button "
        f"(Collapsed); got count={actions_count}. "
        "The ⋯ button visibility gate is not correctly applied for SMTP threads."
    )
