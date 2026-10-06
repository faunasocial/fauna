"""tier_1 unit tests for ``ConversationsActions.inject_and_resolve_thread`` — the
shared 7-app helper that answers *"which thread did my inject land in?"*.

Why these are tier_1 and not an e2e run. The defect they pin only *manifests*
under load — the conversations receive loop drains inbound every 2 s under e2e,
so an unrelated arrival during the resolve window used to make the injected
message unresolvable. Betting on a loaded machine to reproduce that is exactly
the wall-clock dependence ``docs/goal/architecture/e2e-conventions.md`` § point
14 forbids; the resolver is a pure function of *(the thread-list timeline, the
injected content)*, so it is driven here through a stub ``list_threads`` that
simply *reports* two changed threads. Same defect, deterministic, no load bet.

The stub drives the helper through its three real doors (``set_state`` /
``get_state("data.conversation_threads")`` / ``call_command``), never a private
seam, so it stays honest about what the helper actually reads.
"""
import pytest

from actions.conversations import ConversationsActions

pytestmark = pytest.mark.tier_1


def _row(thread_id, snippet, message_count=1, label=""):
    """One ``data.conversation_threads`` row, in the shared shape every app
    serializes (``libs/fauna-conversations/src/state_json.rs``)."""
    return {
        "thread_id": thread_id,
        "label": label,
        "snippet": snippet,
        "rail": "FaunaMls",
        "flavor": "OneToOne",
        "unread_count": 0,
        "participant_count": 2,
        "message_count": message_count,
    }


class _StubDriver:
    """Replays a scripted sequence of thread-list payloads.

    ``timeline`` is a list of payloads: each ``get_state`` call pops the next one
    and the last payload repeats forever, so a test writes only the states it
    cares about and the resolver may poll as often as it likes.
    """

    def __init__(self, timeline):
        self._timeline = list(timeline)
        self.commands = []

    def set_state(self, _payload):
        return None

    def get_state(self, key):
        assert key == "data.conversation_threads", key
        if len(self._timeline) > 1:
            return self._timeline.pop(0)
        return self._timeline[0]

    def call_command(self, name, payload):
        self.commands.append((name, payload))
        return None


def _conv(timeline):
    return ConversationsActions(_StubDriver(timeline))


# --- The defect: an unrelated concurrent change must not fail the resolve ---


def test_unrelated_concurrent_thread_change_does_not_fail_the_resolve():
    """The defect, reproduced without a load bet.

    ``t-1`` is *our* landing thread (its snippet becomes the body we injected);
    ``t-2`` is an unrelated thread that grew a message in the same window —
    normal under e2e, where the receive loop drains inbound every 2 s. Both are
    "changed", so the pre-fix resolver raised *"one inject landed in several
    threads at once"* even though the injected content names ``t-1`` unambiguously.
    """
    before = [_row("t-1", "older", message_count=1), _row("t-2", "unrelated", message_count=3)]
    after = [
        _row("t-1", "hello from carol", message_count=2),
        # The concurrent arrival: a different thread, a different body.
        _row("t-2", "something else entirely", message_count=4),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="carol@self-nest.test", body="hello from carol"
        )
        == "t-1"
    )


def test_a_brand_new_unrelated_thread_does_not_fail_the_resolve():
    """Same defect, the *new thread* shape: an unrelated inbound that creates its
    own thread is equally "changed" (not in ``before`` at all)."""
    before = [_row("t-1", "older", message_count=1)]
    after = [
        _row("t-1", "the injected body", message_count=2),
        _row("t-9", "a stranger said hi", message_count=1),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="carol@self-nest.test", body="the injected body"
        )
        == "t-1"
    )


def test_our_inject_creating_a_new_thread_still_resolves_under_concurrency():
    """Our own inject may be the *new* thread while an existing one changes
    concurrently — the mirror of the case above, and the one web hits most (a
    fresh thread plus lazily-hydrated ``message_count`` on prior rows)."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older too", message_count=1)]
    after = [
        # Prior rows hydrate their detail-derived counts: "changed" but not ours.
        _row("t-1", "older", message_count=7),
        _row("t-2", "older too", message_count=4),
        _row("t-3", "brand new conversation", message_count=1),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="Smtp", sender="alice@host.test", subject="Q4", body="brand new conversation"
        )
        == "t-3"
    )


# --- The resolver must never become LESS precise than it was ---


def test_the_single_unambiguous_candidate_still_resolves():
    """The overwhelmingly common path — nothing else moved — is unchanged."""
    before = [_row("t-1", "older", message_count=1)]
    after = [_row("t-1", "hi", message_count=2)]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(rail="FaunaMls", sender="bob@self-nest.test", body="hi")
        == "t-1"
    )


def test_two_threads_ending_on_the_injected_body_is_still_a_hard_failure():
    """Genuine ambiguity — two changed threads whose last message *is* our body
    — keeps the loud failure. Silently picking one would be the wrong-thread read
    the helper exists to prevent."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older", message_count=1)]
    after = [_row("t-1", "hi", message_count=2), _row("t-2", "hi", message_count=2)]
    conv = _conv([before, after])

    with pytest.raises(AssertionError, match="ends several changed threads at once"):
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="bob@self-nest.test", body="hi", timeout_s=0.6
        )


def test_a_lone_changed_thread_wins_by_default_once_the_budget_is_spent():
    """The escape hatch that keeps the narrowing from ever resolving *less* often
    than the raw change signals did.

    Nothing matches the body — here because a second message landed in our own
    thread and took over the preview, which no content check can see. One thread
    and only one moved, so after the budget it is the answer, exactly as it was
    before content matching existed. Failing instead would turn a case the old
    resolver got right into a red."""
    before = [_row("t-1", "older", message_count=1)]
    after = [_row("t-1", "a later arrival overtook the preview", message_count=3)]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="FaunaMls",
            sender="carol@self-nest.test",
            body="the body nobody will ever see",
            timeout_s=0.6,
        )
        == "t-1"
    )


def test_nothing_ever_lands_still_fails_with_the_never_surfaced_message():
    before = [_row("t-1", "older", message_count=1)]
    conv = _conv([before, before])

    with pytest.raises(AssertionError, match="never surfaced"):
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="bob@self-nest.test", body="nobody delivers this", timeout_s=0.6
        )


# --- The content match must model the shared snippet transform, not the raw body ---


def test_a_markdown_body_matches_its_stripped_plaintext_snippet():
    """``ThreadSummary.snippet`` is ``markdown_to_plaintext(body)`` in shared Rust
    (``store/threads.rs::summarize``), identical on all 7 apps — so the match is
    against the *stripped* text, never the raw body. ``**bold** preview`` →
    ``bold preview``."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "x", message_count=1)]
    after = [
        _row("t-1", "bold preview", message_count=2),
        _row("t-2", "y", message_count=2),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="Smtp", sender="alice@host.test", body="**bold** preview"
        )
        == "t-1"
    )


def test_a_link_body_matches_the_label_only_snippet():
    """A markdown link contributes its *label* to the plaintext preview, not its
    href — so the snippet carries strictly fewer words than the body."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "x", message_count=1)]
    after = [
        _row("t-1", "see the doc", message_count=2),
        _row("t-2", "y", message_count=2),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="Smtp",
            sender="alice@host.test",
            body="see the [doc](https://example.test/budget)",
        )
        == "t-1"
    )


def test_our_message_landing_after_the_unrelated_one_still_resolves():
    """The polling half of the fix. In the window where *only* unrelated threads
    have changed there is nothing to match yet — the resolver must keep waiting
    for our message rather than concluding anything. Raising there (or returning
    an unrelated thread) is the defect wearing a different hat."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older too", message_count=1)]
    # Only the unrelated thread has moved so far.
    mid = [_row("t-1", "older", message_count=1), _row("t-2", "a stranger", message_count=2)]
    # Now ours lands too.
    after = [
        _row("t-1", "the message under test", message_count=2),
        _row("t-2", "a stranger", message_count=2),
    ]
    conv = _conv([before, mid, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="carol@self-nest.test", body="the message under test"
        )
        == "t-1"
    )


def test_a_shorter_snippet_does_not_swallow_the_exact_one():
    """A candidate whose snippet is a *subset* of our body — an unrelated thread
    whose last message happens to be one of our words — must not tie with the
    thread that carries the body in full. The exact-token tier is what separates
    them; without it both are subsequence matches and the resolve fails."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older too", message_count=1)]
    after = [
        _row("t-1", "hello from carol", message_count=2),
        _row("t-2", "carol", message_count=2),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="FaunaMls", sender="carol@self-nest.test", body="hello from carol"
        )
        == "t-1"
    )


def test_an_empty_snippet_never_matches():
    """A changed thread with no words in its snippet (a deleted last message, a
    punctuation-only body) tells us nothing — it must not be treated as matching
    every inject, which is what a vacuously-true containment check would do.

    The body is deliberately a *link* body so its plaintext preview drops the
    href and no candidate matches the tokens exactly: that is what forces the
    containment branch to run. An exact-matching body would short-circuit above
    it and this test would pin nothing — as an earlier draft of it did, until a
    mutant that made an empty needle vacuously true survived it.
    """
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older too", message_count=1)]
    after = [
        _row("t-1", "see the doc", message_count=2),
        _row("t-2", "", message_count=2),
    ]
    conv = _conv([before, after])

    assert (
        conv.inject_and_resolve_thread(
            rail="Smtp",
            sender="alice@host.test",
            body="see the [doc](https://example.test/budget)",
            timeout_s=0.6,
        )
        == "t-1"
    )


def test_a_punctuation_only_body_carries_no_signal_and_keeps_the_old_behavior():
    """``body="..."`` WAS a real call site (``test_thread_rename.py`` and
    ``test_capability_gating.py`` until 2026-09-22, when both moved to word-bearing
    bodies after the mail rail's launch re-drain made the list move under them).
    It tokenizes to nothing, so it can never discriminate — the helper must say so
    rather than match every candidate."""
    before = [_row("t-1", "older", message_count=1), _row("t-2", "older", message_count=1)]
    after = [_row("t-1", "...", message_count=2), _row("t-2", "moved too", message_count=2)]
    conv = _conv([before, after])

    with pytest.raises(AssertionError, match="several threads at once"):
        conv.inject_and_resolve_thread(
            rail="Smtp", sender="alice@host.test", subject="Q4", body="...", timeout_s=0.6
        )
