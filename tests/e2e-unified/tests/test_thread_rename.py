"""Rename: enabled on MLS groups; disabled on 1:1 and subject-keyed."""

import pytest

pytestmark = pytest.mark.tier_3

# Each test keys its OWN thread. `nest_instance`/`test_user` are session-scoped, so a
# shared participant means a shared thread: an inbound from a peer who is already in an
# MLS group here can MERGE into that group's thread — and then the 1:1 test would assert
# the rename gate against a *group*, inverting its expectation. Distinct peers keep the
# 1:1 and the group genuinely separate. (Opening is by identity, so these are keying
# values, not lookup needles.)
GROUP_PEER = "bob@self-nest.test"
ONEONONE_PEER = "bob-rename-oneonone@self-nest.test"


@pytest.mark.feature("group-conversations")
def test_rename_mls_group(logged_in_app):
    thread_id = logged_in_app.conversations.create_mls_group([GROUP_PEER])
    logged_in_app.conversations.rename_thread("Lunch Crew")
    threads = logged_in_app.conversations.list_threads()
    # By identity: another test's group may already sit in this session-scoped list,
    # and `flavor == "MlsGroup"` would read back whichever one sorts first.
    group = next(t for t in threads if t.thread_id == thread_id)
    assert group.label == "Lunch Crew"


@pytest.mark.feature("group-conversations")
def test_rename_button_hidden_on_oneonone(logged_in_app):
    logged_in_app.conversations.inject_and_open_thread(
        rail="FaunaMls", sender=ONEONONE_PEER, body="hi"
    )
    assert logged_in_app.driver.is_absent("thread-rename-button"), (
        "rename should be hidden on non-MLS-group threads, but the button is visible: "
        f"{logged_in_app.driver.diagnose('thread-rename-button')}"
    )


def test_rename_button_hidden_on_subject_keyed(logged_in_app):
    logged_in_app.conversations.inject_and_open_thread(
        rail="Smtp",
        sender="alice@host.test",
        subject="Q4 budget rename-gate",
        # Word-bearing so the resolver can tell this inject's thread from any
        # other thread the receive loop moves meanwhile (`test_capability_gating.py`
        # says why a `"..."` body cannot).
        body="rename gate on a subject-keyed thread",
    )
    assert logged_in_app.driver.is_absent("thread-rename-button"), (
        "rename should be hidden on non-MLS-group threads, but the button is visible: "
        f"{logged_in_app.driver.diagnose('thread-rename-button')}"
    )
