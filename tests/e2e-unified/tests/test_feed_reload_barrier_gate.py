"""tier_1: the feed-reload barrier's timeout must be a VERDICT, not a shrug.

Pure in-process exercise of ``helpers/waiting.py::await_feed_reload_after``
against a scripted fake driver. No nest, no app, no browser, no cargo.

The barrier reads the shared ``{started, completed}`` feed-reload counters and
waits for a re-query that began after a baseline to COMMIT. Its happy path is
one comparison and has never been in doubt; what has cost this project cycles
is its **timeout text**, twice:

* it first asserted "the trigger never fired" at a timeout where ``started``
  had plainly advanced, sending a session hunting wiring the counters prove is
  fine (fixed);
* the text that replaced it asserted the opposite reading just as
  unconditionally — "still in flight, a budget-vs-chain question" — which the
  counters cannot support either. ``(started, completed)`` is byte-for-byte
  identical whether the newest reload began a second before the deadline (raise
  the ceiling) or has been parked for the whole budget (a stall: every link in
  the chain carries a 30 s RPC deadline and an *errored* fetch still commits,
  so a reload outliving that bound is not slowness).

So the barrier now also reports the newest reload's in-flight age, and this
file pins the branching — because a diagnostic that only ever runs on a failing
e2e run is otherwise witnessed by nobody until it misleads the next session.

It pins the barrier's **baseline** on the same terms, and that half is the one
that can fail SILENTLY. The release condition is only as causal as the value it
compares against, and a bare ``feed_reloads`` read is not causal at all: native
apps push their state, so a read taken right after an action the test already
awaited can answer from a snapshot published *before* it — making
``committed_gen > baseline`` satisfiable by the pre-action commit. A barrier
that releases early is invisible, so the pins below script that stale publish
deterministically rather than waiting for a run to expose it
(``feed_reload_baseline``).

Deliberately **no timing assertions**: the budgets below are poll-cadence, not
subjects. What is asserted is which reading each counter shape produces and
that both readings are offered where the counters are genuinely ambiguous
(convention 14 — the pins are latency-independent; convention 6 — the failure
must diagnose itself; convention 11 — an app without the leg refuses loudly).
"""

import pytest

from helpers.waiting import (
    FEED_RELOADS_KEY,
    await_feed_reload_after,
    feed_reload_baseline,
)

pytestmark = [pytest.mark.tier_1]

# Poll cadence only. `wait_until` polls at 0.3 s, so this is a couple of ticks —
# enough for the scripted sequences below to be observed in order. No assertion
# in this file depends on how long anything took.
BUDGET_S = 1.0


class _ScriptedDriver:
    """Publishes a scripted sequence of ``feed_reloads`` states.

    The last entry repeats forever, so a sequence ending in an unsatisfied
    state times the barrier out on purpose.
    """

    def __init__(self, states):
        self._states = list(states)
        self.reads = 0

    def get_state(self, key):
        assert key == FEED_RELOADS_KEY, f"barrier read the wrong key: {key!r}"
        self.reads += 1
        index = min(self.reads - 1, len(self._states) - 1)
        return self._states[index]


def _counts(started, completed, committed_gen=None):
    """A scripted counter reading.

    ``committed_gen`` is the GENERATION of the most recently committed reload,
    not a count. It defaults to ``completed`` because the two coincide exactly
    when no reload was ever superseded — which is the world every pin below but
    ``test_a_supersede_before_the_baseline_does_not_strand_the_barrier``
    inhabits. Pass it explicitly to script a supersede.
    """
    return {
        "started": started,
        "completed": completed,
        "committed_gen": completed if committed_gen is None else committed_gen,
    }


def test_a_commit_past_the_baseline_releases_the_barrier():
    """The pigeonhole: only a reload that BEGAN after the baseline can push
    `completed` past it, so that is the release condition."""
    driver = _ScriptedDriver([_counts(3, 3), _counts(4, 3), _counts(4, 4)])
    await_feed_reload_after(driver, 3, budget_s=BUDGET_S, what="the benign flip")


def test_a_supersede_before_the_baseline_does_not_strand_the_barrier():
    """A superseded reload NEVER commits, so `completed` lags `started`
    permanently — and comparing `completed` against a *started* baseline is
    therefore unsatisfiable once any pre-baseline reload was superseded.

    Measured on web, 2026-08-23: baseline `(3, 2)` — one
    of the first three reloads had been superseded — then the reconnect fires
    two overlapping reloads; #4 is superseded by #5, #5 COMMITS (the injected
    probe was rendered in the DOM at the failure). `completed` climbs to 3 and
    stops, never passing the baseline of 3, while generation 5 has plainly
    landed. The old pigeonhole reported that as "no re-query ever committed" —
    a structurally guaranteed false red, not a flake: the reconnect path
    overlaps reloads by construction, so the barrier could never pass there.

    The release condition is the committed GENERATION passing the baseline.
    Generations are claimed by `fetch_add` at the reload's first statement, so
    `committed_gen > baseline` says exactly "a reload that BEGAN after the
    baseline read has landed its verdict" — and no supersede can shift it.
    """
    driver = _ScriptedDriver([_counts(3, 2, 2), _counts(5, 2, 2), _counts(5, 3, 5)])
    await_feed_reload_after(driver, 3, budget_s=BUDGET_S, what="the benign flip")


def test_a_stuck_started_counter_names_the_trigger_wiring():
    """`started` never moved: the re-query was never initiated. This is the
    revert-red signature (the reconnect resync's feed arm switched off), and it
    must NOT be reported as a budget question."""
    driver = _ScriptedDriver([_counts(3, 3)])
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(driver, 3, budget_s=BUDGET_S, what="the benign flip")

    message = str(excinfo.value)
    assert "re-query mechanism never ran" in message, message
    assert "trigger wiring" in message, message
    # The opposite reading must be absent — offering both here is what made the
    # original text useless.
    assert "in flight" not in message, message


def test_a_backwards_started_counter_names_the_replaced_manager():
    """`started` came back LOWER than the baseline. A generation claimed by a
    monotonic ``fetch_add`` on one manager cannot decrease, so this is not a
    statement about the trigger at all — the app built a new feed manager and
    the barrier is now reading an instance that did not exist at baseline.

    Measured on windows 2026-08-28: baseline 4,
    post-flip ``(started=2, completed=2, committed_gen=2)``. The old text called
    that "`started` did not move ... this is about the trigger wiring" and sent
    the reader at wiring that was never implicated — the same misattribution
    class the age discriminator was added to stop.
    """
    driver = _ScriptedDriver([_counts(2, 2)])
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(driver, 4, budget_s=BUDGET_S, what="the benign flip")

    message = str(excinfo.value)
    assert "BACKWARDS" in message, message
    assert "REPLACED its feed manager" in message, message
    # Both neighbouring verdicts must be absent: they point somewhere useless.
    assert "re-query mechanism never ran" not in message, message
    assert "in flight" not in message, message
    # And it must say the barrier cannot decide the re-hydrate while this holds,
    # rather than leaving the reader to trust an unreachable release condition.
    assert "not comparable" in message or "no reading of these counters is comparable" in message, message


def test_an_advanced_started_counter_reports_the_age_and_both_readings():
    """`started` moved but nothing committed. The counters alone cannot say
    whether the budget expired mid-chain or a reload is parked, so the timeout
    must hand over the discriminator (the in-flight age) and name what each
    reading implies — never assert one of them."""
    driver = _ScriptedDriver([_counts(3, 3), _counts(4, 3), _counts(5, 3)])
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(driver, 3, budget_s=BUDGET_S, what="the benign flip")

    message = str(excinfo.value)
    assert "The trigger DID fire" in message, message
    assert "re-query mechanism never ran" not in message, message

    # The discriminator itself, and that it is attributed to the NEWEST reload.
    assert "in flight" in message, message
    assert "started=5" in message, message

    # Both readings offered, each with its next move.
    assert "size the ceiling" in message, message
    assert "STALL" in message, message
    assert "never add a sleep" in message, message


def test_an_app_without_the_leg_refuses_instead_of_reading_as_zero():
    """Convention 11: `None` is a refusal, not a zero. An app that has not
    built the counter must not read as "a re-query has landed"."""
    driver = _ScriptedDriver([None])
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(driver, None, budget_s=BUDGET_S)

    message = str(excinfo.value)
    assert FEED_RELOADS_KEY in message, message
    assert "feed_reloads_json" in message, message


def test_a_reload_already_newest_at_arming_reports_a_FLOOR_not_a_measurement():
    """The barrier cannot see back past its own arming. When the newest reload
    was already the newest at the first poll it began at or before then, so its
    age is a LOWER BOUND — and the text must say so.

    This is the branch web's 2026-08-22 run actually hit, reporting an age of
    300.1s against a 300.0s budget. Read as a measurement that says "parked the
    whole budget"; read correctly it says "at least the whole budget, possibly
    longer". The conclusion survives either way here, but a diagnostic that
    quietly upgrades a floor to a measurement is the same overclaim this file
    exists to prevent.
    """
    driver = _ScriptedDriver([_counts(5, 3)])
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(driver, 3, budget_s=BUDGET_S, what="the benign flip")

    message = str(excinfo.value)
    assert "ALREADY the newest when the barrier armed" in message, message
    assert "AT LEAST" in message, message
    assert "a floor" in message, message
    # ...and it must NOT carry the other branch's precise-measurement phrasing
    # ("began Xs into the budget"), which would state an age it cannot know.
    assert "into the" not in message, message


class _StaleUntilBarrieredDriver:
    """A native app's PUSHED state, with the timing taken out.

    Reads answer from the bridge's last published snapshot, and the app's next
    push is *in flight* at the moment the test finishes its action. So the
    first unbarriered read answers ``before`` — the state as of before the
    action the test already awaited — and every read after it answers
    ``after``, because by then the push has landed. That is the 50-150 ms
    window (``drivers/http_bridge.py::get_state``) scripted rather than raced:
    it is transient, exactly as it is in a real run, which is what makes an
    un-anchored baseline a *silent* early release rather than a hang.

    ``barrier()`` collapses the window: the ack the driver polls for arrives on
    a snapshot carrying that command's id, so a read behind it can never see
    the pre-action value.

    Nothing here waits on a clock (convention 14 — the pins stay
    latency-independent); the staleness is a scripted read count.
    """

    def __init__(self, before, after):
        self._before, self._after = before, after
        self.barriers = 0
        self.reads = 0

    def barrier(self, timeout=10.0):
        self.barriers += 1

    def get_state(self, key):
        assert key == FEED_RELOADS_KEY, f"barrier read the wrong key: {key!r}"
        self.reads += 1
        if self.barriers == 0 and self.reads == 1:
            return self._before
        return self._after


class _NoBarrierArmDriver:
    """An app whose test agent has no ``barrier`` arm (convention 11's refusal).

    Mirrors ``drivers/base.py::barrier``'s default, which raises rather than
    returning quietly precisely so a missing anchor cannot pass for one.
    """

    def __init__(self):
        self.reads = 0

    def barrier(self, timeout=10.0):
        raise NotImplementedError(
            "FakeDriver does not implement barrier() — convention 14's causal "
            "anchor. Implement it on the app's test agent."
        )

    def get_state(self, key):
        self.reads += 1
        return _counts(9, 9)


def test_the_baseline_is_read_behind_a_barrier():
    """The baseline must come from a snapshot that post-dates the action the
    test already awaited — so it is read behind the app's own barrier.

    Scripted here as the stale publish that made it necessary: the bridge still
    holds generation 1 from before the compose, while the compose's own reload
    has actually taken the app to generation 2.
    """
    driver = _StaleUntilBarrieredDriver(_counts(1, 1), _counts(2, 2))

    assert feed_reload_baseline(driver) == 2, (
        "the baseline read the bridge's PRE-action snapshot — it was not "
        "anchored behind the barrier"
    )
    assert driver.barriers == 1, (
        "exactly one barrier per baseline: no barrier means no anchor, and "
        "more than one means the read is polling for stability (banned)"
    )


def test_an_unanchored_baseline_releases_on_a_pre_action_commit():
    """The false-GREEN this anchor closes, both halves in one pin.

    With the UN-anchored read the barrier releases immediately although no
    reload ever began after the action: the stale baseline is 1, the app's own
    pre-action commit is generation 2, and ``2 > 1`` holds. That is the windows
    2026-08-29 signature — a 138 s return against a 300 s budget, deducible
    only because ``committed_gen`` never left its pre-flip value.

    With the anchored baseline the same driver, the same counters and the same
    absent reload produce the correct verdict instead: the barrier spends its
    budget and reports that the re-query never ran.

    So this pin reds in *both* directions — if the anchor is removed the second
    half stops raising, and if the anchor were to somehow over-read the first
    half stops releasing.
    """
    stale = _StaleUntilBarrieredDriver(_counts(1, 1), _counts(2, 2))
    unanchored_baseline = stale.get_state(FEED_RELOADS_KEY)["started"]
    assert unanchored_baseline == 1, "the scripted stale window did not fire"
    # No exception: the defect, reproduced. Nothing here ever begins a reload —
    # the app sits at generation 2 throughout — but the baseline was captured
    # one read too early, so the barrier's very first poll sees the app's own
    # PRE-action commit clear a baseline of 1 and releases.
    await_feed_reload_after(
        stale, unanchored_baseline, budget_s=BUDGET_S, what="the benign flip"
    )

    anchored = _StaleUntilBarrieredDriver(_counts(1, 1), _counts(2, 2))
    with pytest.raises(AssertionError) as excinfo:
        await_feed_reload_after(
            anchored,
            feed_reload_baseline(anchored),
            budget_s=BUDGET_S,
            what="the benign flip",
        )

    message = str(excinfo.value)
    assert "re-query mechanism never ran" in message, message
    assert "trigger wiring" in message, message


def test_a_missing_barrier_arm_refuses_instead_of_yielding_a_STALE_baseline():
    """Convention 11 at the anchor: an app whose agent has no ``barrier`` arm
    must refuse loudly, never hand back an un-anchored baseline.

    An anchor that silently degrades to no anchor is worse than no anchor at
    all — every barrier built on it reverts to the race it was meant to remove
    and nothing downstream can tell. So the refusal propagates, and the
    baseline is never read at all.
    """
    driver = _NoBarrierArmDriver()
    with pytest.raises(NotImplementedError) as excinfo:
        feed_reload_baseline(driver)

    assert "barrier()" in str(excinfo.value), str(excinfo.value)
    assert driver.reads == 0, (
        "the baseline was read anyway — a refused anchor must not fall back to "
        "the un-anchored read it exists to replace"
    )
