"""tier_3: a half-written new message is kept when its author steps back out of
the new-conversation composer or switches to another conversation —
``docs/goal/ui/conversations.md`` § Persistence, the ``new_thread_compose``
bullet: the draft "persists across switching — clicking an existing
conversation (or toggling back to ``+``) keeps the half-written new message
intact … and switching between any of them never alters another's draft".

``test_conversations_new_thread_cancel.py`` proves the opposite direction:
Cancel is one of only two things that clear this draft (calling send is the
other). This is the everyday direction that test never asserts — the ways OUT
of the composer that must NOT clear it. The restart round-trip
(``test_conversations_draft_persistence.py``) cannot stand in for it either:
nothing here leaves the process, so a composer that dropped its draft on the
way out would pass a restart test as long as the autosave had already run.

**The two ways out, as this app's user takes them** (the per-app gesture is
``ConversationsActions.step_out_of_new_conversation``'s): tui shows the list OR
a thread, never both, so stepping out is the conversations tab landing back on
the list, and switching is opening another thread from that list. Each leg
asserts the composer really left the screen before it asserts the draft came
back, so a way out that never happened cannot pass as one that kept the text.

**A dedicated account.** The conversation to switch to is an inbound injected
before the journey starts (setup, not the journey — convention 8's precondition
carve-out), and a fresh account means no earlier test's resting draft is
restored into this composer. That matters more since the 2026-09-21 ruling
(``conversations.md`` § Persistence, *A restore FILLS*): the launch restore
lands whenever the network says and adopts any slot the author has not written
into, so on the shared session user a late restore could fill the composer
before the first keystroke. It never clears one, which is the property this
test leans on once the author has typed.

Latency-independent (convention 14): every wait polls the observable that is
the contract — the composer's own text, the composer's presence — under a
ceiling a green run never pays.
"""

import time
import uuid

import pytest

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; the other apps join through the drafts-survive cross-app lift,
    # each adding its marker with its `step_out_of_new_conversation` arm.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    # tui runs the real ConversationsSession for every e2e login regardless;
    # marked so the flag's session-wide env stays consistent when the suite
    # runs cross-app (the same reason `test_conversations_draft_persistence.py`
    # carries it).
    pytest.mark.real_conversations,
]


def _poll(read, done, timeout: float = 10.0):
    """Read until ``done(value)`` or the budget runs out; returns the last value
    read, for the failure message."""
    deadline = time.monotonic() + timeout
    value = read()
    while not done(value) and time.monotonic() < deadline:
        time.sleep(0.2)
        value = read()
    return value


@pytest.mark.feature("drafts-survive")
def test_a_half_written_new_message_is_kept_across_stepping_out_and_switching(
    app, request, nest_instance
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    conv = app.conversations
    driver = app.driver
    tag = uuid.uuid4().hex[:8]
    body = f"a new message I have not finished yet {tag}"

    # Setup: a conversation to switch to.
    other_thread = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"carol-kept-{tag}@self-nest.test",
        body="an earlier conversation",
    )

    # 1. Half-write a new message.
    conv.navigate()
    driver.click("new-conversation-button")
    driver.wait_for("dm-text-field", timeout=10.0)
    driver.type_text("dm-text-field", body)
    typed = _poll(conv.compose_body_text, lambda t: t == body)
    assert typed == body, (
        f"precondition: the message must be in the composer, got {typed!r}; "
        f"error={app.error_text()!r}"
    )

    # 2. Step back out of the composer — and make sure it really left.
    conv.step_out_of_new_conversation()
    still_open = _poll(lambda: driver.is_visible("dm-text-field"), lambda v: not v)
    assert not still_open, (
        "stepping out must take the composer off the screen, or the next step "
        f"proves nothing; {driver.diagnose('dm-text-field')}"
    )

    # 3. Back into `+`: the message is still there. (A two-pane app stepped
    #    out by leaving the page, so come back to it first.)
    conv.navigate()
    driver.click("new-conversation-button")
    driver.wait_for("dm-text-field", timeout=10.0)
    after_step_out = _poll(conv.compose_body_text, lambda t: t == body)
    assert after_step_out == body, (
        f"stepping out of the new-conversation composer must keep the half-written "
        f"message (expected {body!r}, got {after_step_out!r}); error={app.error_text()!r}"
    )

    # 4. Switch to the other conversation. Its own composer is its own draft:
    #    the new message must not have followed the author into it.
    conv.open_thread_by_id(other_thread)
    in_other = conv.compose_body_text()
    assert body not in in_other, (
        f"switching must never alter another conversation's draft, but the opened "
        f"thread's composer holds the new message: {in_other!r}"
    )

    # 5. Back to `+`: the new message is still there.
    conv.navigate()
    driver.click("new-conversation-button")
    driver.wait_for("dm-text-field", timeout=10.0)
    after_switch = _poll(conv.compose_body_text, lambda t: t == body)
    assert after_switch == body, (
        f"switching to another conversation must keep the half-written new message "
        f"(expected {body!r}, got {after_switch!r}); error={app.error_text()!r}"
    )
