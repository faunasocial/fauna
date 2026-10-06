"""Affordances are disabled per ThreadCapabilities; clients never branch on rail."""

import uuid

import pytest

pytestmark = pytest.mark.tier_3

@pytest.mark.feature("message-formatting", "conversation-attachments")
def test_bridged_thread_disables_attachment_and_topic(logged_in_app):
    # A thread on a bridge this account's registry does not list takes the
    # bridged rail's most-restrictive vector — no attachments, no subject
    # (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
    # adapter*, ruling 2 (b)). The declared-vector overlay itself is pinned in
    # `fauna-conversations` (`backends::bridged`).
    logged_in_app.conversations.require_attachment_button_supported()
    logged_in_app.conversations.inject_and_open_thread(
        rail="Bridged",
        bridge_id="example",
        sender="@alice-capgate:example.org",
        body="bridged capability gate",
    )
    assert logged_in_app.driver.get_attr("attachment-button", "disabled") == "true"
    assert logged_in_app.driver.get_attr("topic-toggle-button", "disabled") == "true"

@pytest.mark.feature("message-formatting", "conversation-attachments")
def test_fauna_oneonone_enables_all_compose_affordances(logged_in_app):
    logged_in_app.conversations.inject_and_open_thread(
        rail="FaunaMls", sender="bob-capgate@self-nest.test", body="hi"
    )
    for elem in ("attachment-button", "topic-toggle-button", "markdown-bold-button"):
        assert logged_in_app.driver.get_attr(elem, "disabled") in ("false", None)

@pytest.mark.feature("message-formatting")
def test_smtp_enables_markdown(logged_in_app):
    # html-mail Slice 1 flipped Smtp `supports_markdown` → true: mail
    # bodies are composed as markdown (rendered to multipart/alternative), so the
    # compose markdown toolbar un-gates on the SMTP rail like every other rail that
    # supports markdown. (Was test_smtp_disables_markdown, stale since that flip.)
    # A body with WORDS in it, unique to this test. The resolver behind
    # `inject_and_open_thread` finds the thread this inject landed in by
    # narrowing every changed thread to the one whose snippet is this body;
    # `"..."` tokenizes to nothing, so with the list moving under the test (the
    # mail rail's launch re-drain lands records for minutes after sign-in) it
    # failed as "one inject landed in several threads at once" (2026-09-22
    # whole-suite linux sweep). A subject of its own keeps it off every other
    # test's `hi` mail thread as well.
    nonce = uuid.uuid4().hex[:8]
    logged_in_app.conversations.inject_and_open_thread(
        rail="Smtp",
        sender="alice@host.test",
        subject=f"markdown gate {nonce}",
        body=f"markdown toolbar gate {nonce}",
    )
    assert logged_in_app.driver.get_attr("markdown-toolbar", "disabled") in ("false", None)
