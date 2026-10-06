"""Hybrid keying: subject-keyed when subject present, else participant-keyed."""

import time
import uuid

import pytest

pytestmark = pytest.mark.tier_3


def _wait_threads(conv, predicate, timeout_s: float = 10.0):
    """Poll ``list_threads()`` until ``predicate(threads)`` holds, then return them.

    An inject's state push is **async**, so reading the thread list immediately after
    injecting can see the pre-inject snapshot. That is normally masked by the round-trip
    latency, but right after apple's **per-module app relaunch** the first read of a fresh
    process reliably loses the race — which is what made
    ``test_smtp_subject_routes_to_subject_keyed[ios]`` fail with `0 == 1` (no thread at
    all) while passing everywhere else. Whether a relaunch happened depends on which
    module ran before this one, so an unwaited read here is an order-dependence like any
    other. (Tests that OPEN the thread they injected don't need this: `inject_and_open_thread`
    / `inject_and_resolve_thread` already poll — see actions/conversations.py. This file
    asserts on the thread LIST rather than opening anything, so it waits explicitly.)
    """
    deadline = time.time() + timeout_s
    threads = conv.list_threads()
    while time.time() < deadline:
        if predicate(threads):
            return threads
        time.sleep(0.2)
        threads = conv.list_threads()
    return threads


@pytest.mark.feature("replies-and-threads")
def test_smtp_subject_routes_to_subject_keyed(logged_in_app, nest_instance):
    """Inbound SMTP with subject "Q4 budget" creates a subject-keyed thread."""
    conv = logged_in_app.conversations
    conv.inject_inbound_for_test(
        rail="Smtp",
        sender="alice@plain-email-host.test",
        subject="Q4 budget",
        body="numbers attached",
    )
    items = _wait_threads(conv, lambda ts: any(t.label == "Q4 budget" for t in ts))
    matches = [t for t in items if t.label == "Q4 budget"]
    assert len(matches) == 1


@pytest.mark.feature("replies-and-threads")
def test_smtp_in_reply_to_overrides_subject(logged_in_app):
    """Inbound SMTP with In-Reply-To routes to parent thread regardless of subject."""
    conv = logged_in_app.conversations
    parent_msg_id = conv.inject_inbound_for_test(
        rail="Smtp",
        sender="alice@plain-email-host.test",
        subject="Q4 budget",
        body="initial",
    )
    conv.inject_inbound_for_test(
        rail="Smtp",
        sender="alice@plain-email-host.test",
        subject="Re: completely different subject",
        body="follow-up",
        in_reply_to=parent_msg_id,
    )
    items = _wait_threads(
        conv,
        lambda ts: any(t.label == "Q4 budget" and t.message_count >= 2 for t in ts),
    )
    q4 = [t for t in items if t.label == "Q4 budget"]
    assert len(q4) == 1, "follow-up must merge into Q4 thread via In-Reply-To"
    assert q4[0].message_count == 2


def test_chat_no_subject_merges_participant_keyed(logged_in_app):
    """Two inbound MLS messages from same sender with no subject merge into one thread.

    The sender is unique per call. The session-scoped actor keeps every
    nest-backed thread earlier modules left behind — the federation journeys'
    ``bob…@127.0.0.1:<port>`` threads among them — so a filter on any label
    containing ``"bob"`` counted those too: ``assert 3 == 1`` in the
    2026-09-10, 09-11 and 09-14 whole-suite linux sweeps, green solo.
    """
    conv = logged_in_app.conversations
    local_part = f"bob-{uuid.uuid4().hex[:8]}"
    for body in ("first", "second"):
        conv.inject_inbound_for_test(
            rail="FaunaMls",
            sender=f"{local_part}@self-nest.test",
            subject=None,
            body=body,
        )
    items = _wait_threads(
        conv,
        lambda ts: any(local_part in t.label and t.message_count >= 2 for t in ts),
    )
    bob_threads = [t for t in items if local_part in t.label]
    assert len(bob_threads) == 1, [(t.label, t.message_count) for t in bob_threads]
    assert bob_threads[0].message_count == 2


def test_new_subject_creates_new_thread(logged_in_app):
    """Inbound SMTP with subject differing from any existing thread creates a new thread."""
    conv = logged_in_app.conversations
    conv.inject_inbound_for_test(
        rail="Smtp", sender="alice@host.test", subject="Q4 budget", body="A"
    )
    conv.inject_inbound_for_test(
        rail="Smtp", sender="alice@host.test", subject="Lunch?", body="B"
    )
    items = _wait_threads(
        conv,
        lambda ts: {"Q4 budget", "Lunch?"} <= {t.label for t in ts},
    )
    alice_threads = [
        t for t in items if "alice" in t.snippet or t.label in ("Q4 budget", "Lunch?")
    ]
    assert len(alice_threads) == 2


@pytest.mark.feature("replies-and-threads")
def test_re_prefix_normalized(logged_in_app):
    """Inbound 'Re: Q4 budget' merges with existing 'Q4 budget' thread."""
    conv = logged_in_app.conversations
    conv.inject_inbound_for_test(
        rail="Smtp", sender="alice@host.test", subject="Q4 budget", body="A"
    )
    conv.inject_inbound_for_test(
        rail="Smtp", sender="alice@host.test", subject="Re: Q4 budget", body="B"
    )
    items = _wait_threads(
        conv,
        lambda ts: any(t.label.lower() == "q4 budget" and t.message_count >= 2 for t in ts),
    )
    q4 = [t for t in items if t.label.lower() == "q4 budget"]
    assert len(q4) == 1
    assert q4[0].message_count == 2
