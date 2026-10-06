"""The `barrier` test-agent command's cross-app self-test (convention 14, D4).

`docs/goal/architecture/e2e-conventions.md` § convention 14 defines `barrier` as
the causal anchor negative asserts use instead of a settle-sleep: the agent acks
it **only after all UI-thread work enqueued before the command has run**. That is
a promise about *ordering*, and an implementation that quietly acks early is
worse than no barrier at all — every negative assert built on it silently reverts
to the race it was written to remove, and nothing downstream can tell.

So the barrier needs a self-test of its own, and it cannot be written against
whatever async product path each app happens to have (that would measure the
path's timing, not the barrier). It is written against a deliberate probe:

    `barrier_probe` enqueues a BATCH of work items on the same queue real work
    rides, and acks WITHOUT waiting for them.

**Why a batch is the load-bearing detail.** A one-item probe is nearly vacuous on
any app whose UI queue drains on its own: the item lands microseconds later
regardless, so a `barrier` that did nothing would usually still pass. On tui the
command arm and the UI arm are two branches of one `tokio::select!`, which polls
branches in *random* order — a do-nothing barrier beating a single queued message
is a coin flip, i.e. a flaky test rather than a proof. With N items it must win
that race N times running, so a do-nothing barrier fails with probability
`1 - 2^-N`. The assertion below reads the LAST item's value, so any prefix of the
batch is a visible, diagnosable failure rather than a near-miss.

⚠ **This file must never grow a sleep.** A `time.sleep` between the probe and the
read would make every assertion here pass against a completely broken barrier —
the exact vacuity convention 14 exists to eliminate, in the one file whose whole
job is to prove the alternative works. A merge-time sleep-ratchet gate enforces
this mechanically.

⚠ **What is deliberately NOT asserted here: "the probe has not landed yet."**
That is a negative assert with no causal anchor — the very anti-pattern this
command exists to remove, and it cannot use the barrier to anchor itself without
circularity. The probe's early-ack property is what keeps the test below honest,
so it *is* pinned — but at tier_1, where it is deterministic: tui's
`automation::tests::barrier_drains_work_queued_before_the_command` asserts the
probe applied nothing before the drain ran, and mutation-grading it (M2: make the
probe apply eagerly) reds that assertion.

**Per-app shapes under test** (`fauna_e2e_agent::BARRIER` documents why they are
not interchangeable): tui drains its `UiMessage` channel, because `tokio::select!`
polls branches in random order; linux round-trips a glib **idle** callback,
because its command drain is a glib *timeout* source and timeouts outrank idles;
web yields two macrotask turns, because awaiting a promise orders only against
microtasks.
"""

import pytest

pytestmark = pytest.mark.tier_3

# Mirrors `fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT`. Passed explicitly so
# the assertion below cannot silently drift from whatever the app defaults to.
PROBE_COUNT = 64

# Mirrors `fauna_e2e_agent::BARRIER_ACK_PROBE_KEY` — what the barrier saw at its
# OWN ack, frozen. ⚠ Never assert the live `barrier_probe` key instead: the read
# below happens a round trip after the ack, by which time every app has drained
# its queue anyway, so the live key passes against a barrier that does nothing.
# That is measured, not hypothetical — see that constant's docs.
ACK_KEY = "barrier_ack_probe"


def _last_value(token: str) -> str:
    """Mirrors `fauna_e2e_agent::barrier_probe_value` for the final item."""
    return f"{token}#{PROBE_COUNT - 1}"


def test_fused_barrier_probe_orders_the_batch(logged_in_app):
    """The **grading** test: enqueue and barrier inside ONE command round trip.

    This is the test that makes each app's barrier mechanism load-bearing, and
    it exists because its two-command sibling below could not do that job on
    most apps. Measured 2026-08-13: deleting linux's glib-idle round trip (M4)
    or web's two `await macrotask()` turns (M5) left the sibling GREEN, because
    the driver's round trip *between* `barrier_probe` and `barrier` is itself
    long enough for those queues to drain unaided — so a do-nothing barrier was
    indistinguishable from a correct one. Only tui, whose `select!` grants no
    such free drain, was discriminated.

    Fusing deletes the gap rather than measuring it (`e2e-conventions.md`
    § convention 14; `fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`). The app
    enqueues the batch and runs its own barrier before acking this one command,
    so the frozen ack-time value can only be the last item if the barrier
    genuinely ordered the batch — and is `None` if the app acked without
    draining, with no second round trip left to rescue it.

    ⚠ An app that ignores the unknown payload field fails here rather than
    passing: ignoring it means acking early, which freezes `None`. That is
    convention 11's "never silently drop" satisfied by construction, and it is
    why the fused form needs no separate refusal surface.
    """
    app = logged_in_app
    token = "barrier-fused"

    assert app.driver.get_state(ACK_KEY) is None, (
        "a stale ack token would make the assertion below vacuous — the app's "
        "reset path is supposed to clear it"
    )

    app.driver.call_command(
        "barrier_probe",
        {"token": token, "count": PROBE_COUNT, "barrier": True},
    )

    assert app.driver.get_state(ACK_KEY) == _last_value(token), (
        "a FUSED `barrier_probe` must not ack until its own batch has run "
        "(e2e-conventions.md § convention 14). None means the app acked "
        f"without barriering at all; a `#<i>` below {PROBE_COUNT - 1} means it "
        "barriered partway through. Unlike the two-command test below, no "
        "inter-command gap can mask either failure here — which is exactly why "
        "this test, and not that one, is what pins the mechanism."
    )


def test_barrier_waits_for_work_enqueued_before_it(logged_in_app):
    """The contract in its REAL usage shape: probe, then a separate `barrier`.

    ⚠ **Grading, stated exactly so this is not mistaken for the pin:** on linux
    and web this test cannot discriminate the barrier's mechanism — the round
    trip between the two commands drains those queues on its own (mutants M4/M5
    survived it, 2026-08-13). It is kept because it is the shape every real
    negative assert uses — act in one command, `barrier()` in the next — so it
    catches an ack path that hangs or breaks ordering across commands, which the
    fused test above cannot see. The *mechanism* is pinned above; this is the
    usage-shape smoke.
    """
    app = logged_in_app
    token = "barrier-ordered"

    # Precondition: nothing has published a token yet. Without this the test
    # could pass on a leftover value from an earlier test in a session-scoped
    # app process (the `barrier_probe` clear-on-reset exists for this reason).
    assert app.driver.get_state(ACK_KEY) is None, (
        "a stale ack token would make the assertion below vacuous — the app's "
        "reset path is supposed to clear it"
    )

    # The probe acks EARLY by construction: its work is queued, not applied.
    app.driver.call_command("barrier_probe", {"token": token, "count": PROBE_COUNT})

    # The barrier is the only thing that can make that work observable.
    app.barrier()

    assert app.driver.get_state(ACK_KEY) == _last_value(token), (
        "after `barrier`, ALL UI work enqueued before it must have run "
        "(e2e-conventions.md § convention 14). A `#<i>` below "
        f"{PROBE_COUNT - 1} means the agent acked the barrier partway through "
        "its UI-thread queue; None means it acked before draining at all. Both "
        "silently break every negative assert built on this command."
    )


def test_barrier_is_idempotent_and_safe_on_an_idle_app(logged_in_app):
    """A barrier over an empty queue returns; it does not block or fail.

    The contract bounds the barrier to work enqueued *before* it — so with
    nothing enqueued there is nothing to wait for. An implementation that
    blocked for "a bit more" would be a settle-sleep wearing the barrier's name,
    and would show up here as a timeout rather than as a passing no-op.
    """
    app = logged_in_app

    app.barrier()
    app.barrier()

    # The property is that both calls RETURNED — an implementation that waited
    # for more than it promised shows up as a `barrier()` timeout above, not as
    # a failed assertion here. This read just confirms the agent is still
    # answering afterwards. Deliberately app-agnostic: an earlier version
    # asserted `session.authenticated is True`, which failed the app whose
    # barrier was working perfectly: web's `getState` derives that flag from the
    # identity store, where it read None until 2026-08-17 and now means the
    # narrower "verified against the nest on this load" — neither of which this
    # test is about.
    assert app.driver.get_state() is not None, (
        "the agent still answers after two back-to-back barriers"
    )
