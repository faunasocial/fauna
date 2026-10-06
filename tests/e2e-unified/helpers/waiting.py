"""Shared deadline-poll primitive for e2e tests (e2e-conventions.md convention 14).

``wait_until`` replaces the ~20 duplicated per-file ``_poll``/``_wait_until``
clones surveyed 2026-07-23 (12 + 8 copies across ``tests/``). A budget is a
ceiling, not a target: a green run returns the instant the condition holds
and pays nothing; only a genuine failure spends it, still bounded by the
per-test 900s ceiling (convention 9). Named budgets live in
``helpers/budgets.py`` — pick one there rather than a bare float.
"""
import json
import time
import urllib.request

from helpers import budgets


def wait_until(predicate, budget_s, *, interval=0.3, diagnose=None):
    """Poll ``predicate()`` until it returns a truthy value; return that value.

    Raises ``AssertionError`` if ``budget_s`` elapses first. ``diagnose``, when
    given, is called with no arguments ONLY on timeout and its return value is
    appended to the failure message — so failures self-diagnose (convention 6)
    without paying the cost of running it on every green poll tick.
    """
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    detail = f": {diagnose()}" if diagnose is not None else ""
    raise AssertionError(f"wait_until: timed out after {budget_s}s{detail}")


def wait_registry_index(read_index, n, *, active=None, budget_s=30):
    """Poll a persisted ``AccountIndex`` until it lists exactly ``n`` accounts
    — and, when ``active`` is given, until its active pointer names that actor
    too; return the index.

    ``read_index`` is the per-app store read (each switcher module owns where
    its app's file lives). Wait on EVERY field you then assert: an append is
    two registry writes on some apps — apple's ``completeAppendedAccount``
    lands ``addAccount`` (count grows, active still the OLD account) and only
    then, across an ``await``, the switch that moves ``active`` — so a
    count-only wait can return inside that gap and read a stale pointer
    (convention 14).
    """
    last = None

    def _settled():
        nonlocal last
        last = read_index()
        if not last or len(last.get("accounts", [])) != n:
            return None
        if active is not None and last.get("active") != active:
            return None
        return last

    want = f"{n} accounts" + (f" with active={active!r}" if active is not None else "")
    return wait_until(
        _settled, budget_s, interval=0.5,
        diagnose=lambda: f"registry never reached {want}; last index={last!r}",
    )


# The app-published counter of initiated session teardowns. One key on every
# app — see ``fauna_e2e_agent::SESSION_GENERATION_KEY`` for the contract and for
# why each app bumps it at the *initiation* point rather than where the teardown
# lands.
SESSION_GENERATION_KEY = "session_generation"


def session_generation(driver):
    """Read the app's session generation, or ``None`` if it does not publish one.

    ``None`` is a *refusal*, not a zero: an app that has not built the counter
    must not be silently treated as "never relaunched" (convention 11 — honour
    the contract or refuse loudly). ``assert_no_relaunch`` turns it into a
    skip-or-fail decision rather than a quiet pass.
    """
    value = driver.get_state(SESSION_GENERATION_KEY)
    return value if isinstance(value, int) else None


def _read_generation_across_a_possible_relaunch(driver, budget_s):
    """Read the generation, tolerating an app that is mid-relaunch.

    Returns the counter, or ``None`` if it never became readable. Exceptions are
    swallowed *while polling* on purpose: a relaunch in flight is exactly when
    the read throws (destroyed execution context on web, a torn-down agent
    elsewhere), and that is the case whose verdict we most need.
    """
    deadline = time.monotonic() + budget_s
    while True:
        try:
            value = session_generation(driver)
            if value is not None:
                return value
        except Exception:  # noqa: BLE001 — mid-relaunch reads legitimately fail.
            pass
        if time.monotonic() >= deadline:
            return None
        time.sleep(0.3)


def assert_no_relaunch(driver, trigger, settled, *, budget_s=10.0, what="the gesture"):
    """Prove ``trigger`` did NOT tear the authenticated session down.

    Convention 14's negative-assert shape, replacing the settle-sleep
    ``sleep(3); assert still_here``. A fixed window is unsound in both
    directions: it silently false-*passes* when a real relaunch is merely late
    (which is exactly what a loaded machine produces), and it taxes every green
    run for a delay that, on a healthy app, never happens at all.

    The anchor is causal instead:

    1. read the generation,
    2. run ``trigger()``,
    3. positively wait for ``settled()`` — the trigger handler's OWN completion
       observable (e.g. "the prompt closed"), so we are past the handler rather
       than guessing,
    4. ``driver.barrier()`` — every UI-thread item enqueued before this point
       has now run (``fauna_e2e_agent::BARRIER``),
    5. assert the generation is unchanged.

    A relaunch is initiated *synchronously* inside the very handler step 3
    awaited, so by step 5 it would already have been counted. That is what makes
    the absence provable rather than merely unobserved.

    ⚠ **Why reading the counter after the barrier is sound here, when reading
    the barrier's own probe that way is NOT** (``BARRIER_ACK_PROBE_KEY``
    documents the vacuity that trap caused): the generation is *monotonic*, so a
    late read can only ever reveal MORE teardowns, never fewer. Settling makes
    this assertion stricter, where it made the probe's vacuous. The property
    depends on the counter surviving the teardown it counts — which is why web
    persists it across its document navigation rather than holding it in memory.

    ``settled`` is required, not optional, and deliberately so: without it the
    barrier would anchor to the click's dispatch rather than to the handler's
    completion, and a relaunch initiated a few statements later would slip past.
    """
    before = session_generation(driver)
    if before is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{SESSION_GENERATION_KEY}' — convention 14's negative-assert "
            "observable. Implement it on the app (see "
            "`fauna_e2e_agent::SESSION_GENERATION_KEY`) rather than falling "
            "back to a settle-sleep, which cannot prove this at all."
        )

    trigger()

    # ⚠ Both steps below can THROW rather than fail when the relaunch actually
    # happens — and on web that is the normal case, not an edge one: the
    # teardown is a document navigation, so a `settled()` read or a `barrier()`
    # issued across it dies with "Execution context was destroyed" before the
    # counter is ever consulted. Letting that escape would make web's red an
    # opaque driver crash instead of this function's self-diagnosing message
    # (convention 6), and would leave the counter — the actual proof, and the
    # only reason it is persisted across the navigation — never read at all.
    #
    # So a failure here is DEFERRED, not swallowed: the generation read below
    # gets the first word, and if it reports no teardown (i.e. the failure was
    # something else entirely) the original error is re-raised untouched.
    deferred = None
    try:
        # The handler's own completion — the causal anchor. Named budget,
        # deadline poll: a green run pays nothing.
        wait_until(
            settled,
            budget_s,
            diagnose=lambda: (
                f"{what} never reached its settled state, so the generation "
                "check below would have run against an unfinished handler"
            ),
        )
        # Everything the handler enqueued has now run.
        driver.barrier()
    except Exception as exc:  # noqa: BLE001 — deliberately broad; see above.
        deferred = exc

    # Read through a short tolerant poll: after a relaunch the app is
    # mid-rebuild, and on web the agent has to be re-injected into the new
    # document before it can answer at all. The counter itself survives that
    # (`$lib/generation-e2e` keeps it in `sessionStorage`) — which is precisely
    # what this read depends on, and why an in-memory web counter would read 0
    # here and turn the whole assertion into a false pass.
    #
    # A green run pays nothing: the first read succeeds and returns immediately.
    after = _read_generation_across_a_possible_relaunch(driver, budget_s)

    # The failure deferred above gets the last word only if the counter reports
    # nothing — i.e. it was a genuine unrelated error, not the relaunch.
    if after == before and deferred is not None:
        raise deferred

    assert after == before, (
        f"{what} must NOT tear the session down, but the app's "
        f"{SESSION_GENERATION_KEY} moved {before} -> {after} "
        f"(`fauna_e2e_agent::SESSION_GENERATION_KEY`). Each increment is one "
        "initiated teardown/relaunch, counted synchronously by the app at the "
        "moment it commits to it."
    )


def await_session_actor(driver, actor_id, *, budget_s, what="the switch"):
    """Wait until the app holds a live authenticated session as ``actor_id``.

    The cross-app observable for **"this identity reached a WORKING session"**,
    as opposed to "a row for it was written". Every app publishes
    ``session.{authenticated,actor_id}`` at one depth, and no app sets them
    without its launch machine having reached ``Online`` — which costs a real
    silent-challenge round trip against the nest. So this asserts a live
    connection with no second probe, and it stays honest on an app whose UI
    would happily render a stale shell for the *previous* identity.

    Built 2026-08-14 for the account-append journeys, whose registry-only
    assertions left "Add account → you land in a working session as the new
    identity" unproven on all 7 apps (`long-term-store.md` § Multi-account
    evolution, design Decision 1 — the append switches to the new account).

    ⚠ **A tier_3 caller must install the dial override first.** A wizard-entered
    account's ``nest_url`` is derived from the typed handle domain and is
    therefore uniform-https (`fauna_provisioning::probe::
    resolve_handle_domain_with_local_port`), which a plain-HTTP tier_3 nest
    cannot serve. The sanctioned seam is
    ``driver.set_provider_base_urls({"nest": nest["url"]})``, which mirrors into
    the process-global store-read dial (`fauna_launch_machine::dial`) every
    launch-from-store — a relaunch, an "Add account" switch — resolves through.
    Without it this waits out its whole budget on a launch that cannot connect.
    """
    def _session():
        state = driver.get_state() or {}
        session = state.get("session") or {}
        return session if (
            session.get("authenticated") and session.get("actor_id") == actor_id
        ) else None

    def _diagnose():
        state = driver.get_state() or {}
        # The two failures look nothing alike and want opposite next moves, so
        # name which one this is rather than making the reader infer it: NO
        # session means the launch could not connect (suspect the dial override);
        # a session belonging to the OUTGOING actor means the switch never
        # happened at all, which is a product finding, not a harness gap.
        session = state.get("session") or {}
        if session.get("authenticated") and session.get("actor_id") != actor_id:
            verdict = (
                f"the app is STILL authenticated as {session.get('actor_id')} — "
                f"the switch to {actor_id} never took effect. This is not the "
                "dial override: a launch that could not connect leaves NO "
                "session at all. Look at the app's switch glue"
            )
        else:
            verdict = (
                f"no session as {actor_id}, and none for anyone else either — a "
                "launch that could not connect leaves the screen parked on the "
                "launch surface (nav view 'welcome'). If this is tier_3, check "
                "that the dial override is installed (see this helper's docstring)"
            )
        # Whatever the app itself said, when it offers a way to ask. Duck-typed
        # rather than per-app branched: a driver that grows a log accessor is
        # picked up here for free, and one that has none costs nothing.
        #
        # ⚠ Filtered, NOT tailed. A raw tail is worthless for a wait that just
        # spent its whole budget: the app kept logging for those two minutes, so
        # the healthy background chatter (the sync agent alone reprints two INFO
        # lines every 30s) evicts the very lines that explain the failure. Keep
        # the loud ones from the whole run, then a short tail for context.
        tail = ""
        reader = getattr(driver, "app_stderr_text", None)
        if callable(reader):
            try:
                text = reader()
            except Exception as exc:                      # noqa: BLE001
                text = f"<unreadable: {exc}>"
            if text:
                loud = [
                    ln for ln in text.splitlines()
                    if any(m in ln for m in ("ERROR", "WARN", "panic", "PANIC"))
                ]
                if loud:
                    tail += "; app stderr (loud lines): " + " | ".join(loud[-40:])
                tail += f"; app stderr tail: ...{text[-800:]}"
        return (
            f"{what} never reached a working session as {actor_id}: {verdict}. "
            f"Last session={session!r}, nav={state.get('nav')!r}{tail}"
        )

    return wait_until(_session, budget_s, interval=0.5, diagnose=_diagnose)


# The app-published count of completed account-activation gestures — see
# ``fauna_e2e_agent::ACTIVATION_GESTURES_KEY`` for the contract and for why the
# apps whose re-auth prompt is a native OS dialog need one at all.
ACTIVATION_GESTURES_KEY = "activation_gestures"


def activation_gestures(driver):
    """Read the count of completed activation gestures, or ``None`` if unpublished.

    ``None`` is a refusal, not a zero — same contract as ``session_generation``.
    """
    value = driver.get_state(ACTIVATION_GESTURES_KEY)
    return value if isinstance(value, int) else None


def activation_gesture_completed(driver):
    """Build the ``settled`` predicate for a switcher-row tap, baselined NOW.

    The ``settled`` argument ``assert_no_relaunch`` requires, for the apps that
    have no in-app re-auth prompt to watch close (windows/apple/android — their
    prompt is a native OS dialog with no test ID, and under the e2e seam it is a
    file read, so a declined activation changes nothing on screen by design).

    ⚠ **Call this in the argument list, not earlier and not inside a lambda.**
    It reads the baseline at call time, and Python evaluates arguments before
    entering ``assert_no_relaunch`` — i.e. before ``trigger()`` runs — which is
    exactly the ordering this needs::

        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, index=1),
            activation_gesture_completed(driver),
        )

    Baselining it *after* the trigger would make the predicate fire on the
    gesture's own past and reintroduce the race (``ACTIVATION_GESTURES_KEY``
    documents why the counter shape is what rules that out).

    ⚠ **Precondition, and the one way to misuse this:** no *other* activation
    gesture may be in flight when the baseline is read. Native drivers serialize
    state asynchronously (a ~50-150 ms stale window), so a baseline captured
    while an earlier tap's increment is still unpublished reads one low — and
    the predicate would then be satisfied by that earlier gesture rather than by
    the trigger's, anchoring the barrier too early. Every caller so far reads the
    baseline after a settled positive wait, which makes this free; a test that
    taps two rows back to back must wait out the first tap before baselining the
    second.
    """
    before = activation_gestures(driver)
    if before is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{ACTIVATION_GESTURES_KEY}' — the completion observable "
            "`assert_no_relaunch` anchors its barrier to on an app with no "
            "in-app re-auth prompt. Implement it on the app (see "
            "`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`); passing a "
            "trivially-true `settled` instead would anchor the barrier to the "
            "click's dispatch and silently restore the race."
        )

    def completed():
        now = activation_gestures(driver)
        return now is not None and now > before

    return completed


# The app-published critical-alert sweep pass counters, ``{"started", "completed"}``.
# One key at one depth on every app — ``fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY``
# owns the contract, and ``CriticalAlerts::sweep_passes_started`` the reasoning.
ALERT_SWEEP_PASSES_KEY = "alert_sweep_passes"


def alert_sweep_passes(driver):
    """Read ``(started, completed)`` sweep passes, or ``None`` if unpublished.

    ``None`` is a refusal, not a zero — same contract as ``session_generation``:
    an app that has not built the counters must not be quietly treated as
    "swept", because every caller below is asking a *negative* question whose
    false-pass is silent (convention 11 — honour or refuse loudly).
    """
    value = driver.get_state(ALERT_SWEEP_PASSES_KEY)
    if not isinstance(value, dict):
        return None
    started, completed = value.get("started"), value.get("completed")
    if not isinstance(started, int) or not isinstance(completed, int):
        return None
    return started, completed


def await_sweep_pass_after(driver, planted, *, budget_s, what="the planted condition"):
    """Wait until a critical-alert sweep pass that began AFTER ``planted`` ran.

    The sweep-side twin of ``assert_no_relaunch``: convention 14's causal anchor
    for the question "my planted condition raises NO banner". The sweep is a
    fire-and-forget spawn on the app's post-auth hook, so before this the only
    option was to sleep a generous window and peek — unsound in both directions
    (a real alarm that is merely late passes silently; every green run pays the
    whole window).

    Usage — ``planted`` is the value ``alert_sweep_passes(driver)[0]`` returned
    at the moment the condition was planted, read BEFORE the trigger:

        started, _ = alert_sweep_passes(driver)
        directory.tamper(...)            # plant
        reestablish_session(...)         # trigger: spawns a fresh sweep
        await_sweep_pass_after(driver, started, budget_s=SWEEP_PASS_S)
        assert banner_is_down()

    ⚠ **Why the comparison is against *started* and not simply "completed grew
    by one".** An app spawns one sweep loop per session establishment, and
    re-establishing without a teardown leaves the previous loop running (nothing
    called ``clear_all``, so its stop signal never fires) — so several passes can
    be in flight, and a completion seen after the plant may belong to a pass that
    read the world before it. ``completed > started_at_plant_time`` is immune to
    that by pigeonhole: only ``started`` passes existed when the condition was
    planted, so the (started+1)-th completion must be a pass that began later.
    It needs no count of how many loops are running, which is what keeps the
    barrier sound as apps change how often they establish.
    """
    def one_finished():
        passes = alert_sweep_passes(driver)
        if passes is None:
            return None
        return passes[1] > planted

    if alert_sweep_passes(driver) is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{ALERT_SWEEP_PASSES_KEY}' — the critical-alert sweep's causal "
            "barrier. Implement it on the app (see "
            "`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`; the counting itself is "
            "already shared, so the app only republishes two getters) rather "
            "than sleeping, which cannot prove a negative at all."
        )

    wait_until(
        one_finished,
        budget_s,
        diagnose=lambda: (
            f"no critical-alert sweep pass completed after {what} was planted "
            f"(passes at plant: started={planted}; now: "
            f"{alert_sweep_passes(driver)}). Without a completed pass the "
            "banner read that follows would prove nothing — the sweep may "
            "simply not have looked yet."
        ),
    )


# The new-message OS banners an app actually fired, plus the diff-tick counters
# that make a negative read of them sound —
# ``{"started": N, "completed": M, "fired": [{"thread_id", "label"}, …]}``. One
# key at one depth on every app — ``fauna_e2e_agent::MESSAGE_BANNERS_KEY`` owns
# the contract, and ``fauna_conversations::notification`` the recording.
MESSAGE_BANNERS_KEY = "message_banners"


def message_banners(driver):
    """Read this app's fired-banner log, or ``None`` if it publishes none.

    Returns ``{"started": int, "completed": int, "fired": [dict, …]}``.

    ``None`` is a refusal, not "no banners fired" — the distinction is the whole
    point here (convention 11). Every assertion built on this key is really about
    *absence* somewhere: an app that has not built the firing at all would
    otherwise satisfy "the open thread raised no banner" perfectly, and the
    negative half of the witness would pass on all six columns that have not
    built the feature.
    """
    value = driver.get_state(MESSAGE_BANNERS_KEY)
    if not isinstance(value, dict):
        return None
    started, completed, fired = (
        value.get("started"),
        value.get("completed"),
        value.get("fired"),
    )
    if not isinstance(started, int) or not isinstance(completed, int):
        return None
    if not isinstance(fired, list):
        return None
    return {"started": started, "completed": completed, "fired": fired}


def require_message_banners(driver):
    """``message_banners`` or a loud refusal naming what the app owes."""
    banners = message_banners(driver)
    if banners is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{MESSAGE_BANNERS_KEY}' — the new-message banner log that "
            "witnesses `conversations` outcome 11. Implement it on the app (see "
            "`fauna_e2e_agent::MESSAGE_BANNERS_KEY`; the recording and the JSON "
            "are already shared, so an app that fires the banner owes three "
            "calls and one getter read) rather than letting a silent empty list "
            "stand in — an app that fires nothing would pass every negative "
            "assertion here."
        )
    return banners


def fired_banner_threads(driver):
    """The thread ids this app has fired a banner for, in firing order."""
    return [
        entry.get("thread_id")
        for entry in require_message_banners(driver)["fired"]
        if isinstance(entry, dict)
    ]


def await_banner_pass_after(driver, planted, *, budget_s=None, what="the message"):
    """Wait until a banner diff tick that BEGAN after ``planted`` has finished.

    The banner-side twin of ``await_sweep_pass_after``, and it exists for exactly
    the same reason: the sharp assertions about this key are negative ones — *a
    message into the thread I already have open raises NO banner* — and there is
    nothing on screen to watch for their arrival. Sleeping is unsound in both
    directions and is convention 14's defunct shape.

    Usage — ``planted`` is ``message_banners(driver)["started"]`` read at the
    moment the message was planted, BEFORE the inject::

        planted = waiting.message_banners(driver)["started"]
        conv.inject_inbound_for_test(...)          # plant
        waiting.await_banner_pass_after(driver, planted)
        assert waiting.fired_banner_threads(driver) == before

    ⚠ **Why the comparison is against ``started`` rather than "``completed``
    grew".** A tick that merely *finished* after the plant may have read its
    snapshot before it, so its silence says nothing; only the (planted+1)-th
    completion is guaranteed to belong to a tick that began later. Same
    pigeonhole ``await_sweep_pass_after`` documents in full, and it holds
    however many observers the app drives the tracker from.
    """
    budget_s = budgets.MESSAGE_BANNER_PASS_S if budget_s is None else budget_s
    require_message_banners(driver)

    def one_finished():
        banners = message_banners(driver)
        if banners is None:
            return None
        return banners["completed"] > planted

    wait_until(
        one_finished,
        budget_s,
        diagnose=lambda: (
            f"no new-message banner diff tick completed after {what} was "
            f"planted (started at plant: {planted}; now: "
            f"{message_banners(driver)}). Without a completed tick the "
            "banner-log read that follows would prove nothing — the app's "
            "snapshot observer may simply not have run yet."
        ),
    )


def rescore_worklist_serves(nest_url):
    """Read the nest's ``(serves, units)`` re-score worklist counters.

    Snapshots ``GET /api/v1/test/capabilities/rescore_worklist`` (gated on
    ``--features test-hooks``, which ``build_node()`` enables). Both counters
    are cumulative and monotonic; ``units`` is the total number of work-units
    handed out across all ``serves``, so a pair read before and after a plant
    tells you both *that* the nest decided and *whether it handed out any work*.
    """
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/capabilities/rescore_worklist", method="GET"
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, (
            f"rescore worklist hook returned {resp.status} — is the nest built "
            "with --features test-hooks?"
        )
        body = json.loads(resp.read().decode())
    return body["serves"], body["units"]


def await_rescore_worklist_serve_after(nest_url, planted, *, budget_s, what="the planted condition"):
    """Wait until the nest served the re-score drain a worklist AFTER ``planted``.

    The nest-side twin of ``await_sweep_pass_after``: convention 14's causal
    anchor for the question "the drain does NOT advance this obligation". Before
    this, those arms slept a 6s settle window and peeked — unsound in both
    directions, and (worse here) *vacuous*: without evidence that a drain ran at
    all after the revoke, "the row did not advance" is trivially true.

    Usage — ``planted`` is the ``serves`` count read at the moment the condition
    was planted, BEFORE the poke that wakes the drain:

        serves, _ = rescore_worklist_serves(nest_url)
        revoke_grant(...)                 # plant
        poke_config_changed(...)          # trigger: wakes the MDA's drain
        await_rescore_worklist_serve_after(nest_url, serves, budget_s=...)
        assert scorer_version(...) == 1   # still owed

    ⚠ **Why ONE counter is sound here, where the alert sweep needs a pair.**
    ``await_sweep_pass_after`` compares completions against a plant-time
    ``started`` because the app-side sweep is its own decider and several sweep
    loops can be in flight, so a completion may belong to a pass that read the
    pre-plant world. The drain is not like that: the **nest** decides at both
    ends of a run, and re-checks at each —

    1. ``rescore_worklist_handler`` intersects the holder's *live*
       ``content.read{kind}`` grants with the obligation gap, so a revoked
       owner is absent from the answer; and
    2. ``submit_scores_handler`` re-authorizes every row against a live
       ``content.label-write`` grant, **fail-closed on the whole batch** — so a
       run that fetched its worklist *before* the revoke still cannot write back
       after it.

    A serve counted after the plant is therefore a decision made against the
    post-plant world, and no earlier in-flight run can sneak a write past it.
    That is why the pigeonhole argument the sweep needs does not arise, and why
    a plain "``serves`` advanced" is the whole barrier.

    The nest bumps the counter **after** building the unit list
    (``bins/fauna-nest/src/rescore_drain_test_hook.rs``), so an observed serve is
    a decision already made, never one in progress — the ordering pinned by
    ``serves_are_counted_after_the_unit_list_is_decided``.
    """
    def one_served():
        return rescore_worklist_serves(nest_url)[0] > planted

    wait_until(
        one_served,
        budget_s,
        diagnose=lambda: (
            f"the nest served no re-score worklist after {what} was planted "
            f"(serves at plant: {planted}; now: {rescore_worklist_serves(nest_url)}). "
            "Without a serve the assertion that follows would prove nothing — "
            "the drain may simply not have looked yet. Is an MDA-role bridge "
            "enrolled, and did the config_changed poke reach it?"
        ),
    )


# The app-published per-channel folded-in inbound MLS commit counts,
# ``{channel_hex: count}``. One key at one depth on every app —
# ``fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`` owns the contract, and
# ``FaunaMlsBackend::folded_commits`` the reasoning.
MLS_FOLDED_COMMITS_KEY = "mls_folded_commits"


def mls_folded_commits(driver, channel_hex):
    """Read how many inbound MLS commits this app folded in on ``channel_hex``.

    Returns an ``int`` (``0`` for a channel with no fold-in yet), or ``None`` if
    the app does not publish the key at all.

    ``None`` is a refusal, not a zero — same contract as ``alert_sweep_passes``.
    The distinction is the whole reason the shared derivation publishes ``{}``
    rather than ``null`` for an app with no session yet: "this app has folded
    nothing in" and "this app has no leg" are different answers, and reading the
    second as the first would make the barrier below wait out its entire budget
    on a gap that should have been named in one line.
    """
    value = driver.get_state(MLS_FOLDED_COMMITS_KEY)
    if not isinstance(value, dict):
        return None
    count = value.get(channel_hex, 0)
    return count if isinstance(count, int) else None


def await_folded_commit_after(
    driver, channel_hex, baseline, *, budget_s, what="the peer's commit"
):
    """Wait until this app folds in an inbound commit on ``channel_hex`` that
    landed AFTER ``baseline`` was read.

    Convention 14's causal anchor for **"the other device's epoch takeover has
    been incorporated here"** — the precondition a cross-device test needs before
    driving a send, so the send exercises the takeover path rather than a stale
    blind-append at an epoch this device already lost.

    ⚠ **Why the natural barrier does not exist, and must not be re-attempted.**
    The obvious anchor is "wait for device A to render device B's message". For
    two devices of the *same* actor it is not weak but impossible: they share one
    MLS leaf, and a sender cannot MLS-decrypt its own application messages
    (``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync, which
    is why own history rides the ``history/<ch>`` replica instead of log replay).
    It was built and measured RED at the full 90s handshake budget on tui before
    this observable existed. What A *does* process is B's **commit** — this count.

    Usage — read ``baseline`` BEFORE the peer writes::

        folded = mls_folded_commits(device_a.driver, channel_hex)
        device_b.conversations.real_send(...)      # B's takeover commit lands
        await_folded_commit_after(device_a.driver, channel_hex, folded,
                                  budget_s=MLS_COMMIT_FOLD_S)
        device_a.conversations.real_send(...)      # provably a takeover now

    A baseline read *after* the peer's send can already include the fold-in, in
    which case this waits out its whole budget for a second commit nobody will
    ever author. That failure is loud (the diagnose below prints both values),
    but the ordering is the caller's to get right.
    """
    if baseline is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{MLS_FOLDED_COMMITS_KEY}' — the cross-device MLS fold-in barrier. "
            "Implement it on the app (see "
            "`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`; the counting and the JSON "
            "shape are both already shared, so the app leg is a one-line read of "
            "`state_json::mls_folded_commits_json_for_session`) rather than "
            "sleeping — for twin devices no amount of sleeping can prove this, "
            "because the message-rendering proxy is cryptographically "
            "unavailable."
        )

    wait_until(
        lambda: (mls_folded_commits(driver, channel_hex) or 0) > baseline,
        budget_s,
        diagnose=lambda: (
            f"this device never folded in {what} on channel {channel_hex} "
            f"(folded commits at baseline: {baseline}; now: "
            f"{mls_folded_commits(driver, channel_hex)}). Its receive loop did "
            "not incorporate the epoch transition, so a send issued now would "
            "blind-append at a stale epoch instead of taking the epoch over — "
            "and the channel would still grow, which is exactly why the "
            "record-count assertion downstream cannot catch this."
        ),
    )


# The receive loop's cycle counters + its run-one-now poke, one key and one
# command name at one depth on every app — ``fauna_e2e_agent::
# CONV_RECEIVE_CYCLES_KEY`` / ``CONV_RECEIVE_NOW`` own both contracts.
CONV_RECEIVE_CYCLES_KEY = "conv_receive_cycles"
CONV_RECEIVE_NOW = "conv_receive_now"


def conv_receive_cycles(driver):
    """Read ``(started, completed)`` full receive-loop cycles, or ``None`` if the
    app does not publish them.

    ``None`` is a refusal, not a zero — same contract as ``alert_sweep_passes``:
    an app with no leg must never read as "a cycle has run", or a caller waiting
    on delivery would spend its whole budget on a gap that should have been
    named in one line (convention 11).
    """
    value = driver.get_state(CONV_RECEIVE_CYCLES_KEY)
    if not isinstance(value, dict):
        return None
    started, completed = value.get("started"), value.get("completed")
    if not isinstance(started, int) or not isinstance(completed, int):
        return None
    return started, completed


def poke_receive_cycle(driver, *, timeout=20):
    """Ask this app's receive loop for one cycle NOW (``CONV_RECEIVE_NOW``).

    Fire-and-forget by contract — the ack is not the barrier, because an awaited
    reply would hang whenever no loop is running. Pair it with
    ``await_receive_cycle_after`` on a baseline read BEFORE the trigger.
    """
    driver.call_command(CONV_RECEIVE_NOW, timeout=timeout)


def await_receive_cycle_after(driver, baseline, *, budget_s, what="the arrival"):
    """Wait until a full receive-loop cycle that began AFTER ``baseline`` finished.

    Convention 14's causal anchor for **"this app has looked for what I just
    sent it"** — the client receive loop (MLS conversations, the durable
    inbox-apply drain, and the mail read-feeds that ride it) is push-driven with
    a 30 s backstop ticker, and before this the e2e's only lever was to *shorten*
    that ticker (``FAUNA_CONV_POLL_SECS=2``). A shortened tick is still a
    wall-clock dependence: it lowers the odds of a false pass without changing
    the shape.

    Usage — ``baseline`` is ``conv_receive_cycles(driver)[0]`` read BEFORE the
    trigger::

        started, _ = conv_receive_cycles(app.driver)
        peer.send(...)                                 # the arrival
        poke_receive_cycle(app.driver)                 # ask for a cycle now
        await_receive_cycle_after(app.driver, started, budget_s=RECEIVE_CYCLE_S)
        assert thread_is_there()                       # a cycle has provably looked

    ⚠ **Why the comparison is against *started*.** A cycle already in flight when
    the trigger landed may have read the world before it, so "completed grew"
    proves nothing. ``completed > started_at_trigger`` is immune by pigeonhole:
    only ``started`` cycles existed then, so the (started+1)-th completion must
    have begun later — the identical argument ``await_sweep_pass_after`` makes,
    and it needs no count of how many loops are live (linux replaces a session's
    loop on re-injection, and the old one runs until its next liveness check).

    ⚠ This proves the app **looked**, never that anything arrived. Assert the
    arrival itself afterwards — a cycle that swept an empty inbox completes
    exactly like one that decrypted a message.
    """
    if baseline is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{CONV_RECEIVE_CYCLES_KEY}' — the receive loop's delivery barrier. "
            "Implement it on the app (see "
            "`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`; the counting and the "
            "JSON shape are both already shared, so a native app leg is a "
            "one-line read of `state_json::conv_receive_cycles_json`) rather "
            "than shortening its poll cadence and hoping."
        )

    wait_until(
        lambda: (conv_receive_cycles(driver) or (0, 0))[1] > baseline,
        budget_s,
        diagnose=lambda: (
            f"no receive cycle that began after {what} ever completed "
            f"(cycles started at baseline: {baseline}; now: "
            f"{conv_receive_cycles(driver)}). Either the loop is not running "
            "(nothing polls, and no push can rescue a suppressed-push test) or "
            "the poke never reached it — a cycle that merely found nothing "
            "still completes, so this is about the loop, not the arrival."
        ),
    )


# The account-plane pump's cycle counters + its run-one-pass-now poke — the
# ``conv_receive_cycles`` twin for the account runtime.
# ``fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`` / ``ACCOUNT_PUMP_NOW`` own both
# contracts (counting: ``fauna_sync_engine::account_runtime::PumpCycles``).
ACCOUNT_PUMP_CYCLES_KEY = "account_pump_cycles"
ACCOUNT_PUMP_NOW = "account_pump_now"


def account_pump_cycles(driver):
    """Read ``(started, completed)`` full account-pump passes, or ``None`` if
    the app does not publish them (convention 11: ``None`` is a refusal, never
    a zero — an app without the account-store leg must not read as "a pass has
    run")."""
    value = driver.get_state(ACCOUNT_PUMP_CYCLES_KEY)
    if not isinstance(value, dict):
        return None
    started, completed = value.get("started"), value.get("completed")
    if not isinstance(started, int) or not isinstance(completed, int):
        return None
    return started, completed


def account_pump_role(driver):
    """Read ``(runtime_up, is_holder)`` for this app's account runtime, or
    ``None`` if the app does not publish the key at all.

    **Read this before waiting on ``account_pump_cycles``.** W5.1 (account-data-plane.md § Workstreams)'s
    engine-singleton election decides which co-located process pumps a store,
    and a runtime that lost it (ordinarily to the sync agent) **runs no pass** —
    its counters are frozen by design, at ``(0, 0)`` if it never held the role.
    (No *pass*: a seed-holding non-holder still runs its **seed pass** — the
    linked-nest secondary leg and the custody arm riding it — which these
    counters do not count; ``account-runtime.md`` § Multi-instance concurrency
    → *The seed-leg role*.)
    Without the role, that is indistinguishable from "no runtime assembled" and
    from "elected, no pass yet", so a test that waits for a poked pass is
    really asserting an election outcome nothing guarantees. That is not
    theoretical: it is how ``test_account_runtime_pump.py`` failed on a healthy
    tui *and* a healthy linux in one run on 2026-08-18, while another instance
    in the same run reached ``(4, 3)``.

    So:

    * ``None``            — no account-store leg on this app (convention 11).
    * ``(False, False)``  — leg present, no runtime: still assembling, or the
                            assembly failed (best-effort by design).
    * ``(True, False)``   — assembled, NOT the pump holder. Do not wait for a
                            pass; convergence arrives through the shared store.
                            Its seed pass still runs: wait on that leg's own
                            effect or log line, never on these counters.
    * ``(True, True)``    — assembled and pumping. Only here is
                            ``poke_account_pump`` + ``await_pump_cycle_after``
                            a meaningful barrier.

    An app that publishes the counters without the role (not elected) reads as
    ``(True, False)`` — the conservative answer, which makes a caller decline
    to wait rather than hang.
    """
    value = driver.get_state(ACCOUNT_PUMP_CYCLES_KEY)
    if not isinstance(value, dict):
        return None
    if not isinstance(value.get("started"), int):
        return None
    return bool(value.get("runtime")), bool(value.get("holder"))


def account_runtime_role_or_skip(driver):
    """``account_pump_role(driver)``, or this app's convention-7 skip when it
    publishes no account-store leg at all.

    The one home of that classification — every app hosts the account runtime
    (web since 2026-09-29, `account-client-lifecycle.md` § The client-side
    lifecycle), so an app without the leg is unbuilt debt — for every test that
    needs the account runtime before it can assert anything. Lifted out of
    ``test_account_runtime_pump.py`` when ``test_relaunch_device_accrual.py``
    needed the identical answer."""
    role = account_pump_role(driver)
    if role is None:
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            driver,
            surface="account_pump_cycles / account_pump_now",
            detail=(
                "this app hosts no W3 account-store runtime yet, so it "
                "publishes neither the pump's completion barrier nor its "
                "run-one-pass-now poke. The lift is one more consumer of "
                "libs/fauna-client-account-runtime (tui and linux have it)"
            ),
            tracked="",
        )
    return role


def _pump_stall_cause(driver):
    """Name WHY a pump wait timed out, from the role — convention 6.

    Before the role was published this diagnosis was a list of three
    possibilities the reader had to disambiguate from the app log, and the one
    that actually fires most often (not the holder) reads as a product failure
    if you do not know to look for it.
    """
    role = account_pump_role(driver)
    if role is None:
        return (
            " The app stopped publishing the key mid-wait, which should not "
            "happen — treat it as a harness fault, not a pump fault."
        )
    runtime_up, is_holder = role
    if not runtime_up:
        return (
            " NO RUNTIME is assembled on this app: either it is still "
            "assembling (it happens on a task spawned at the post-auth hook, "
            "off the login path) or the assembly FAILED — it is best-effort by "
            "design, so grep the app log for 'account runtime: assembly "
            "failed'. Waiting longer will not help if it failed."
        )
    if not is_holder:
        return (
            " THIS IS NOT A DEFECT, and it is the common case: the runtime is "
            "up but ANOTHER co-located process holds the W5.1 engine-singleton "
            "lock — ordinarily the sync agent, which hosts the same store. A "
            "non-holder runs no pass by design, so no amount of poking will "
            "move these counters. Convergence still arrives, through the "
            "shared store. A test that needs a pass must gate on "
            "`account_pump_role(...) == (True, True)` first. A seed-holding "
            "non-holder DOES still run its seed pass (the linked-nest "
            "secondary leg and the custody arm riding it), which these "
            "counters do not count: a missing linked leg is not explained by "
            "this role — look for the seed pass in the app log instead."
        )
    return (
        " The runtime is up AND holds the role, so a poked pass genuinely did "
        "not complete: the poke never reached the runtime, or a pass is "
        "wedged. This one is a real finding — check the app log."
    )


def poke_account_pump(driver, *, timeout=20):
    """Ask this app's account runtime for one full pump pass NOW
    (``ACCOUNT_PUMP_NOW`` — ``reconcile_now``, the ticker's own work on
    demand), followed by a ceremony drive so any act the pass left owed (an
    unposted custody receipt) posts without waiting for a production edge.

    Fire-and-forget by contract — pair with ``await_pump_cycle_after`` on a
    baseline read BEFORE the trigger, exactly like ``poke_receive_cycle``.
    """
    driver.call_command(ACCOUNT_PUMP_NOW, timeout=timeout)


def await_pump_cycle_after(driver, baseline, *, budget_s, what="the pump work"):
    """Wait until a full account-pump pass that began AFTER ``baseline``
    finished — the ``await_receive_cycle_after`` pigeonhole (compare
    ``completed`` against the pre-trigger ``started``), for the pump.

    ⚠ Proves the pump **ran**, never what it did — assert the effect itself
    afterwards (a pass with nothing due completes identically).
    """
    if baseline is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{ACCOUNT_PUMP_CYCLES_KEY}' — the account pump's completion "
            "barrier. Implement it on the app (see "
            "`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`; the counting lives in "
            "shared Rust — `AccountStoreHandle::pump_cycles` — so a native app "
            "leg is a one-line read) rather than waiting out the ticker."
        )

    wait_until(
        lambda: (account_pump_cycles(driver) or (0, 0))[1] > baseline,
        budget_s,
        diagnose=lambda: (
            f"no account-pump pass that began after {what} ever completed "
            f"(passes started at baseline: {baseline}; now: "
            f"{account_pump_cycles(driver)}; role: {account_pump_role(driver)})."
            + _pump_stall_cause(driver)
        ),
    )


def await_account_runtime_assembled(driver, *, budget_s=budgets.ACCOUNT_RUNTIME_ASSEMBLY_S):
    """Wait until this app has an assembled W3 account-store runtime — role,
    not counters (``account_pump_role``'s ``runtime_up``).

    True for a holder and a non-holder alike, which is why it is the barrier
    worth waiting on unconditionally: assembly (the credential-slot read, the
    store open, the writer-key/principal-bundle mint) is a task spawned OFF
    the login path with no other completion signal, so a caller that reads
    the account-store's namespace right after login — the way
    ``wait_until_online``'s transport barrier invites, since it covers only
    the transport — races that background task. Returns the ``(runtime_up,
    is_holder)`` pair on success.

    No-op-shaped for an app with no account-store leg at all: ``driver`` must
    publish the key first (``account_pump_role`` returning ``None`` is a
    convention-11 refusal, e.g. web — declare that with ``app_surface`` at the
    call site, don't call this)."""
    wait_until(
        lambda: (account_pump_role(driver) or (False, False))[0],
        budget_s,
        interval=1.0,
        diagnose=lambda: (
            "the app never assembled an account-store runtime "
            f"(role: {account_pump_role(driver)}). It publishes the leg, so "
            "this is not a missing implementation: the assembly is best-effort, "
            "so it either failed or was never started. The app log separates "
            "those — grep it for 'account runtime:' and read which line is "
            "there: 'mounted the account store' = installed (then this is a "
            "publication bug, not an assembly one); 'assembly failed' = tried "
            "and lost; 'superseded during assembly' = a teardown landed while "
            "it assembled; 'no actor id' / 'no config dir' / 'malformed "
            "secret' = refused at the door; NONE of them = the post-auth hook "
            "never fired on this path at all, which is a wiring bug in the "
            "login path being exercised, not in the runtime."
        ),
    )
    return account_pump_role(driver)


def await_device_removal_ready(driver):
    """The barrier before a ``device-remove-button`` click: on an app that
    hosts an account runtime the removal is REFUSED until that runtime has
    assembled (it resolves which fleet member the row names from the account
    store — `account-data-taxonomy.md` § The generation machinery →
    *Fleet-scope reclamation*, clause (4)), and assembly is a task spawned off
    the login path. Without this a prompt click reads the refusal on
    ``error-message`` — which a test expecting a *different* refusal there
    would pass on for the wrong reason.

    Web included: its Devices page reaches the tab's runtime through the
    account port (`account-client-lifecycle.md` § *The account port*), so a
    web removal issued before the runtime has assembled is refused exactly as
    a native one is. An app that publishes no account-pump leg hosts no
    runtime and has nothing to wait for."""
    if account_pump_role(driver) is None:
        return
    await_account_runtime_assembled(driver)


# The post-claim serving-enablement step's runs.
# ``fauna_e2e_agent::SERVING_ENABLEMENT_KEY`` owns the contract (recording:
# ``fauna_client_mail_settings::serving_enablement::apply_serving_enablement`` —
# one ``{actor_id, decided, completed}`` record per ``LoggedIn`` handoff,
# ``completed`` set once every planned step has answered, the decide-nothing run
# included, by a drop guard so every started run lands).
SERVING_ENABLEMENT_KEY = "serving_enablement"


def serving_enablement(driver):
    """Read the key's value (``{"started", "completed", "runs": [...]}``), or
    ``None`` if the app does not publish it (convention 11: ``None`` is a
    refusal, never an empty list)."""
    value = driver.get_state(SERVING_ENABLEMENT_KEY)
    if not isinstance(value, dict) or not isinstance(value.get("runs"), list):
        return None
    return value


def await_serving_enablement_for(
    driver, actor_id_hex, *, budget_s=budgets.SERVING_ENABLEMENT_S, what="the onboarding"
):
    """Wait until the post-claim serving-enablement run for ``actor_id_hex`` —
    the identity the test onboarded — has finished, and return the intents it
    decided.

    Convention 14's causal anchor for **"the post-claim glue has nothing left to
    write"**: once this actor's run is ``completed`` the deployment toggles are
    final for this onboarding, so ONE read of them is a sound verdict in both
    directions — a slow ``set_caldav_enabled(true)`` cannot miss the read, and a
    glue that wrongly enabled late for a local handle cannot slip past it. This
    replaces the fixed settle windows the calendar-onboarding witnesses used to
    sleep.

    Keyed on the actor, not on a count against a baseline: the onboarding
    helpers relaunch the app on the way to the claim, which resets any
    process-global count (a baseline read before the relaunch is meaningless —
    measured on tui 2026-09-23, where a baseline of 1 met a fresh process's
    ``(1, 1)`` and the wait could never release). Every test mints a fresh admin
    identity, so its run is unambiguous in any process.

    ⚠ Proves the glue **ran to the end**, never that a step succeeded (every
    step logs its own failure and the run still completes). Assert the
    deployment state itself afterwards.
    """
    actor_id_hex = actor_id_hex.lower()

    def reading():
        try:
            return serving_enablement(driver)
        except Exception:  # noqa: BLE001 — a read mid-relaunch legitimately fails.
            return None

    # The leg is published unconditionally from launch, so a key still absent
    # after the UI-settle budget is a missing leg, not a slow one.
    try:
        wait_until(reading, budgets.UI_SETTLE_S)
    except AssertionError:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{SERVING_ENABLEMENT_KEY}' — the post-claim serving-enablement "
            "completion anchor. Route the app's `LoggedIn` handoff through the "
            "shared step (`fauna_client_mail_settings::serving_enablement`; "
            "native `rpc_glue::dispatch_post_claim_serving_enablement`, FFI "
            "`apply_post_claim_serving_enablement`) and publish "
            "`serving_enablement_json` rather than waiting out a settle window."
        ) from None

    def landed():
        value = reading()
        for run in (value or {}).get("runs", []):
            if str(run.get("actor_id", "")).lower() == actor_id_hex and run.get("completed"):
                return run
        return None

    run = wait_until(
        landed,
        budget_s,
        diagnose=lambda: (
            f"the serving-enablement run for {actor_id_hex[:12]}… never finished "
            f"after {what} (the key now: {reading()}). No record for the actor = "
            "the LoggedIn handoff never called the step (or the app relaunched "
            "after it); a record with completed=false = a step is parked."
        ),
    )
    return run.get("decided")


# The shared feed manager's reload counters — the ``conv_receive_cycles`` twin
# for the feed's re-query funnel. ``fauna_e2e_agent::FEED_RELOADS_KEY`` owns the
# contract (counting: ``fauna_feed::FeedManager::reload_counts`` — ``started``
# is the reload's own supersede generation, bumped synchronously at initiation;
# ``completed`` counts committed results, Ok and Err alike, and a superseded
# reload never completes). Deliberately no poke: every consumer so far anchors
# on a *product* trigger (the reconnect-triggered re-fetch), and the manager's
# refresh seams are all UI-drivable — add one only when a test genuinely needs
# a cycle no product edge can supply.
FEED_RELOADS_KEY = "feed_reloads"


def feed_reloads(driver):
    """Read ``(started, completed, committed_gen)`` feed reloads, or ``None`` if
    the app does not publish them (convention 11: ``None`` is a refusal, never a
    zero — an app without the leg must not read as "a re-query has landed").

    ``committed_gen`` — the generation of the newest COMMITTED reload — is the
    barrier's release condition; the first two are its timeout diagnosis. A leg
    publishing only the old pair is the same refusal: it hand-rolled the shape
    instead of reading ``fauna_feed::feed_reloads_json``, and answering it with
    the retired count arithmetic is how a healthy app reads as broken.
    """
    value = driver.get_state(FEED_RELOADS_KEY)
    if not isinstance(value, dict):
        return None
    started = value.get("started")
    completed = value.get("completed")
    committed_gen = value.get("committed_gen")
    if not all(isinstance(v, int) for v in (started, completed, committed_gen)):
        return None
    return started, completed, committed_gen


def feed_reload_baseline(driver):
    """Read ``await_feed_reload_after``'s baseline BEHIND a barrier.

    The release condition ``committed_gen > baseline`` is only as causal as the
    baseline it compares against, and a bare ``feed_reloads(driver)`` is not.
    Native apps do not *serve* their state, they **push** it (windows'
    ``TestAgent.PushState``), so ``/app/state`` answers from the bridge's last
    published snapshot — ``drivers/http_bridge.py::get_state``'s own docstring
    names the 50-150 ms window. A baseline read right after an action the test
    already awaited can therefore carry a triple from BEFORE that action's own
    reload, and ``committed_gen > baseline`` is then satisfiable by the
    **pre-action** commit. The barrier releases having proved nothing — and
    that is the dangerous direction: a red barrier gets read, an early release
    is silent, and the run then fails (or passes) somewhere downstream for
    reasons that have nothing to do with the mechanism it was built to witness.

    Measured on windows, 2026-08-29 — deduced from the run's own numbers, not
    inferred from the mechanism: the app trace shows ``committed_gen`` never
    leaving its pre-flip value of 2, yet the whole test returned in 138 s
    against the barrier's 300 s budget ALONE, so it RELEASED rather than timing
    out, which requires a baseline below 2.

    ``driver.barrier()`` is the anchor, and it is already the cross-app
    contract for exactly this (convention 14's causal anchor, graded per app by
    ``test_agent_barrier.py``): each agent acks it only from behind its own UI
    queue, and the ack the driver polls for arrives ON a published snapshot
    carrying that command's id — so the snapshot the bridge holds when this
    returns was serialized at or after the barrier, and the triple read from it
    cannot predate the action. Web republishes its snapshot inside
    ``call_command`` for the same reason (``drivers/web.py::barrier``).

    ⚠ Deliberately NOT a settle-sleep, and not a poll-until-the-counter-stops
    read. Convention 14 bans both, and either would re-introduce the wall-clock
    dependency the counters exist to remove.

    ⚠ An app whose agent has no ``barrier`` arm REFUSES here — loudly, naming
    the missing arm — rather than handing back an un-anchored baseline. An
    anchor that silently degrades to no anchor is precisely the false-GREEN
    this function exists to close (convention 11: honour it or refuse).

    Returns the ``started`` generation to hand ``await_feed_reload_after``, or
    ``None`` when the app publishes no leg at all — which that barrier turns
    into its own loud refusal.
    """
    driver.barrier()
    triple = feed_reloads(driver)
    return None if triple is None else triple[0]


def await_feed_reload_after(driver, baseline, *, budget_s, what="the trigger"):
    """Wait until a feed reload that began AFTER ``baseline`` COMMITTED.

    Convention 14's causal anchor for **"this app has re-queried its feed since
    X"** — built for the benign-flip re-hydrate guard, which proves the
    reconnect-triggered re-fetch *ran* by observing the mechanism directly
    instead of asserting that no other delivery path exists (a settle-sleep
    absence premise any legitimate concurrent refresh could invalidate — the
    2026-07-24 sweep red on web).

    ``baseline`` is ``feed_reloads(driver)[0]`` — the STARTED count — read
    BEFORE the trigger. The release condition is ``committed_gen > baseline``:
    generations are claimed by ``fetch_add`` at the reload's first statement, so
    a committed generation past the baseline is exactly "a re-query that BEGAN
    after the baseline read has landed its verdict".

    ⚠ **Never wait on ``completed`` instead.** That two-counter pigeonhole
    shipped with this barrier and is unsound: a superseded reload never commits,
    so every supersede widens ``started - completed`` permanently and a baseline
    taken after even one leaves ``completed > baseline`` unreachable however
    healthy the app is. Web measured it on 2026-08-23 —
    the reconnect fires two overlapping reloads *by construction*, the older is
    superseded, the newer commits and renders its posts, and the barrier still
    spent its whole 300 s budget reporting "no re-query ever committed".

    ⚠ Proves the app **re-queried and landed a verdict**, never that the query
    returned what you want (an errored reload commits too). Assert the
    end-state itself afterwards.
    """
    if baseline is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish "
            f"'{FEED_RELOADS_KEY}' — the feed re-query barrier. Implement it "
            "on the app (see `fauna_e2e_agent::FEED_RELOADS_KEY`; the counting "
            "and the JSON shape are both already shared, so a native app leg "
            "is a one-line read of `FfiFeedManager::feed_reloads_json` / "
            "`fauna_feed::feed_reloads_json`) rather than re-deriving an "
            "absence premise."
        )

    # When the NEWEST reload began, tracked across the poll. Without it the two
    # remaining timeout shapes are indistinguishable — "the budget expired a
    # second after the last reload started" (size the ceiling) and "a reload has
    # been parked for the entire budget" (a real stall, since every RPC in the
    # chain carries its own deadline and an errored fetch still commits). Both
    # print the same `(started, committed)` pair, so the counters alone cannot
    # tell them apart and the old text simply asserted the first. Observation
    # only: nothing here is asserted against the clock (convention 14).
    newest = {"started": None, "at": None, "first_seen_at_arming": False}

    def _poll():
        now = feed_reloads(driver)
        if now is not None:
            started, _committed, committed_gen = now
            if newest["started"] is None:
                # First observation: this reload may have begun long before the
                # barrier armed, so its age is only ever a floor. Recorded so
                # the diagnosis says which it is holding.
                newest["first_seen_at_arming"] = True
                newest["started"], newest["at"] = started, time.monotonic()
            elif started > newest["started"]:
                newest["first_seen_at_arming"] = False
                newest["started"], newest["at"] = started, time.monotonic()
            return committed_gen > baseline
        return False

    def _diagnose() -> str:
        # The failures here need OPPOSITE next moves, and the counters already
        # separate the first split — so read them rather than asserting one.
        # Saying "the trigger never fired" at a timeout where `started` plainly
        # advanced sends the next session hunting wiring that is provably fine;
        # that misread cost a cycle on web, 2026-08-22.
        now = feed_reloads(driver)
        started, committed, committed_gen = now if now else (0, 0, 0)
        head = (
            f"no feed reload that began after {what} ever committed "
            f"(reloads started at baseline: {baseline}; now: "
            f"(started={started}, completed={committed}, "
            f"committed_gen={committed_gen}))."
        )
        if started > baseline:
            if newest["at"] is not None:
                age = time.monotonic() - newest["at"]
                # ⚠ A FLOOR, not a measurement, when the newest reload was
                # already the newest at the first poll: the barrier cannot see
                # back past its own arming, so the reload began at or before
                # then and has been in flight AT LEAST this long. Saying
                # "in flight 300.1s" for that case would be the same overclaim
                # this diagnostic exists to stop.
                if newest["first_seen_at_arming"]:
                    age_s = (
                        f"The newest reload (started={newest['started']}) was "
                        f"ALREADY the newest when the barrier armed, so it has "
                        f"been in flight AT LEAST {age:.1f}s (a floor — it "
                        f"began at or before the arming) of the {budget_s}s "
                        "budget. "
                    )
                else:
                    age_s = (
                        f"The newest reload (started={newest['started']}) began "
                        f"{age:.1f}s into the {budget_s}s budget and has been "
                        "in flight since. "
                    )
            else:
                age_s = ""
            return (
                f"{head} The trigger DID fire — {started - baseline} reload(s) "
                f"began and none committed within the budget. {age_s}"
                "An errored reload still commits and the only non-committing "
                "exits are the supersede guards (which need a NEWER "
                "generation, and the started counter IS the generation), so "
                "the newest reload was still in flight at the deadline. "
                "READ THE IN-FLIGHT AGE ABOVE: an age well under the budget "
                "means the budget simply expired mid-chain on a healthy "
                "mechanism — size the ceiling for the chain (convention 14), "
                "never add a sleep. An age spanning most of the budget is the "
                "opposite verdict: every RPC in the reload chain is bounded "
                "by its own registry-declared deadline (`KindMeta::"
                "default_deadline` — at most 60s anywhere in the registry, "
                "30s where a kind declares none) and an errored fetch still "
                "commits, so a reload parked far past (chain length x that "
                "bound) is a STALL to root-cause, not a ceiling to raise."
            )
        if started < baseline:
            # `started` IS the generation, claimed by a monotonic fetch_add on
            # one manager. It cannot decrease. A LOWER value therefore does not
            # say anything about the trigger — it says the counters no longer
            # come from the same object: the app built a NEW feed manager (which
            # counts from zero) somewhere across the trigger, and the baseline
            # belongs to an instance the barrier can no longer see. Reported as
            # its own verdict because the two neighbouring ones send the next
            # session somewhere useless: "the trigger never fired" indicts wiring
            # that may be perfectly healthy, and the release condition itself is
            # meaningless here (the new instance's generations restart below the
            # baseline, so `committed_gen > baseline` may be unreachable no
            # matter how many times the app correctly re-queries).
            return (
                f"{head} `started` went BACKWARDS ({baseline} -> {started}) — "
                "impossible for a per-manager monotonic generation, so the app "
                "REPLACED its feed manager between the baseline read and now and "
                "this count belongs to an instance that did not exist at "
                "baseline. The re-query wiring is NOT implicated by this "
                "failure, and no reading of these counters is comparable across "
                "the trigger until the replacement is explained. Hunt what "
                "rebuilds the manager (a page/view-model reconstruction, a "
                "re-navigated shell, a re-login) — and note the barrier cannot "
                "prove the re-hydrate either way while it is happening."
            )
        return (
            f"{head} The re-query mechanism never ran — `started` did not move, "
            "and an errored re-query still commits, so this is about the "
            "trigger wiring, not the fetch."
        )

    wait_until(_poll, budget_s, diagnose=_diagnose)


# The Devices/Folders machine's refresh triple — the ``feed_reloads`` twin for
# ``DevicesMachine::refresh`` (``fauna_e2e_agent::DEVICES_REFRESHES_KEY`` owns
# the contract, and the release rule is ``feed_reloads``' own:
# ``committed_gen > started-at-baseline``).
DEVICES_REFRESHES_KEY = "devices_refreshes"


def devices_refreshes(driver):
    """Read ``(started, completed, committed_gen)`` Devices/Folders refreshes, or
    ``None`` if the app does not publish them (convention 11: a refusal, never a
    zero)."""
    value = driver.get_state(DEVICES_REFRESHES_KEY)
    if not isinstance(value, dict):
        return None
    triple = tuple(value.get(k) for k in ("started", "completed", "committed_gen"))
    if not all(isinstance(v, int) for v in triple):
        return None
    return triple


def devices_refresh_baseline(driver):
    """The ``started`` count to hand ``await_devices_refresh_after``, read behind
    ``driver.barrier()`` for the reason ``feed_reload_baseline`` gives (a pushed
    state snapshot can predate the action the test already awaited). ``None``
    when the app publishes no leg — which that barrier refuses loudly."""
    driver.barrier()
    triple = devices_refreshes(driver)
    return None if triple is None else triple[0]


def await_devices_refresh_after(driver, baseline, *, budget_s, what="the trigger"):
    """Wait until a Devices/Folders refresh that began AFTER ``baseline``
    COMMITTED — convention 14's causal anchor for "this page has re-read since X".

    Proves the page re-read and landed a verdict, never that the read succeeded
    (a refresh that could reach nothing commits too): assert the end state after.
    """
    if baseline is None:
        raise AssertionError(
            f"{type(driver).__name__} does not publish '{DEVICES_REFRESHES_KEY}' — "
            "the Devices/Folders refresh barrier. Implement it on the app (see "
            "`fauna_e2e_agent::DEVICES_REFRESHES_KEY`; counting and shape are "
            "shared, so the leg is one read of `DevicesMachine::refresh_counts` / "
            "`refreshes_json`) rather than trusting a nav ack."
        )

    def _poll():
        now = devices_refreshes(driver)
        return now is not None and now[2] > baseline

    def _diagnose() -> str:
        now = devices_refreshes(driver) or (0, 0, 0)
        started, completed, committed_gen = now
        moved = (
            f"{started - baseline} refresh(es) began after it and none committed "
            "— the newest was still in flight at the deadline (every read in it "
            "carries its own deadline; size the budget for the chain)"
            if started > baseline
            else "`started` never moved — the trigger did not refresh the page"
        )
        return (
            f"no Devices/Folders refresh that began after {what} committed "
            f"(started at baseline: {baseline}; now: started={started}, "
            f"completed={completed}, committed_gen={committed_gen}): {moved}."
        )

    wait_until(_poll, budget_s, diagnose=_diagnose)


# What an app's inbound poll did with peer share-endpoint advertisements,
# ``{"seen": N, "no_sink": N, "captured": N, "uncaptured": N}``. One key at one
# depth on every app — ``fauna_e2e_agent::SHARE_ENDPOINTS_COUNTS_KEY`` owns the
# contract, and ``FaunaMlsBackend::share_endpoints_counts`` the counting.
SHARE_ENDPOINTS_COUNTS_KEY = "share_endpoints_counts"


def share_endpoints_counts(driver):
    """Read the share plane's ingest tally off this app.

    Returns a ``dict`` with the four integer counts, or ``None`` if the app does
    not publish the key at all.

    ``None`` is a refusal, not four zeros — same contract as
    ``mls_folded_commits``. The shared derivation publishes the zeros for a
    session that has polled nothing yet precisely so that "no advertisement has
    reached this seat" and "this app has no leg" stay different answers.
    """
    value = driver.get_state(SHARE_ENDPOINTS_COUNTS_KEY)
    if not isinstance(value, dict):
        return None
    counts = {k: value.get(k) for k in ("seen", "no_sink", "captured", "uncaptured")}
    if not all(isinstance(v, int) for v in counts.values()):
        return None
    return counts


def share_ingest_reading(driver, seat):
    """One line naming which of the share plane's ingest exits fired on ``seat``.

    The peer-transfer barrier's failure is otherwise a single sentence — *"no
    dial row appeared"* — that covers four different halves of the system, and
    telling them apart cost seven e2e runs of hand-added instrumentation. These
    counts answer it directly (conventions point 6): ``seen == 0`` means no
    advertisement crossed or decrypted here at all; ``no_sink > 0`` means one
    did and was dropped for want of a registered sink; ``uncaptured > 0`` means
    the sink refused it or could not write the dial row; ``captured > 0`` moves
    the hunt downstream of ingest entirely.

    Never raises: a diagnosis that can fail is a diagnosis that disappears
    exactly when it is needed.
    """
    try:
        counts = share_endpoints_counts(driver)
    except Exception as exc:  # pragma: no cover - diagnostic path only
        return f"  [{seat}] share ingest tally unavailable: {exc!r}"
    if counts is None:
        return (
            f"  [{seat}] share ingest tally: <not published> — this app does not "
            f"publish '{SHARE_ENDPOINTS_COUNTS_KEY}', so nothing here can say "
            "whether an advertisement ever reached it "
            "(`fauna_conversations::state_json::share_endpoints_counts_json` is "
            "the one-line leg every app publishes it with)"
        )
    seen, no_sink = counts["seen"], counts["no_sink"]
    captured, uncaptured = counts["captured"], counts["uncaptured"]
    if seen == 0:
        verdict = (
            "NO advertisement ever reached the ingest arm — it never crossed the "
            "wire, or never decrypted on this seat. Look upstream: the send side "
            "and the folder poll's own walk."
        )
    elif no_sink > 0:
        verdict = (
            "advertisements arrived with NO SINK registered — the plane is inert "
            "on this seat. Both tui and linux DO call `set_share_endpoints_sink`, "
            "so the shape to look for is a session-instance mismatch (sink wired "
            "onto a different `ConversationsSession` than the receive loop polls), "
            "never a forgotten call."
        )
    elif captured == 0 and uncaptured > 0:
        verdict = (
            "every advertisement reached the sink and was REFUSED or unwritable "
            "— the bind failed, or the account-plane write was refused. A "
            "`GenerationTip` row whose generation tip will not resolve lands "
            "here; grep 'dial row not persisted' for the whole cause chain."
        )
    elif captured > 0:
        verdict = (
            "the sink DID land dial rows — ingest is healthy and the defect is "
            "downstream of it (the pump's dial/admission, or its row weighing)."
        )
    else:
        verdict = "no advertisement was seen past the sink."
    return (
        f"  [{seat}] share ingest tally: seen={seen} no_sink={no_sink} "
        f"captured={captured} uncaptured={uncaptured} — {verdict}"
    )
# The app-published photo-backup pass funnel — what the LAST completed pass
# actually did, step by step. Apple publishes it today (both shells); android's
# identical `PhotoBackupEngine` owes the same key at the same depth when its e2e
# path exists.
PHOTO_BACKUP_KEY = "photo_backup"

#: Every counter the funnel publishes, in the order a pass walks them.
PHOTO_BACKUP_COUNTERS = (
    "assets_seen",
    "already_synced",
    "export_failed",
    "ingest_failed",
    "uploaded",
)


def photo_backup_funnel(driver):
    """Read the last pass's funnel, or ``None`` if the app publishes none.

    ``None`` is a refusal, not an empty pass — same contract as
    ``session_generation``: an app that has not built the counters must not be
    read as "the pass saw nothing", because that is precisely the answer under
    test (convention 11 — honour the contract or refuse loudly).

    Returns the raw dict: ``authorization`` (the Photos grant word) plus the
    counters in :data:`PHOTO_BACKUP_COUNTERS`.
    """
    value = driver.get_state(PHOTO_BACKUP_KEY)
    if not isinstance(value, dict):
        return None
    if not all(isinstance(value.get(k), int) for k in PHOTO_BACKUP_COUNTERS):
        return None
    return value


def describe_photo_backup_funnel(driver):
    """One line naming WHICH nothing a photo-backup pass did, for a failure message.

    A pass that uploads nothing has four causes that are identical from outside
    the app — PhotoKit yielded nothing, everything was already synced, the export
    failed, the ingest threw — three of them silent by construction. This turns
    the published funnel into the verdict.
    """
    funnel = photo_backup_funnel(driver)
    if funnel is None:
        return ("the app publishes no `photo_backup` funnel, so why the pass "
                "uploaded nothing cannot be read from here")
    counts = ", ".join(f"{k}={funnel[k]}" for k in PHOTO_BACKUP_COUNTERS)
    auth = funnel.get("authorization")
    verdict = _photo_backup_verdict(funnel, auth)
    return f"pass funnel: authorization={auth!r}, {counts} -> {verdict}"


def _photo_backup_verdict(funnel, auth):
    if funnel["uploaded"] > 0:
        return ("the app DID ingest, so the loss is nest-side or later "
                "(changes.record, the media projection)")
    if funnel["ingest_failed"] > 0:
        return "the sealed ingest threw — read the app's own error banner"
    if funnel["export_failed"] > 0:
        return "PhotoKit had assets but the export could not read them"
    if funnel["already_synced"] > 0:
        return ("every asset was already recorded as synced — a stale "
                "PhotoBackupRecord store, not a fresh library")
    if funnel["assets_seen"] == 0:
        if auth == "limited":
            return ("PhotoKit returned nothing under a LIMITED grant, which "
                    "yields only user-selected assets: a fixture problem, not "
                    "a product bug")
        return (f"PhotoKit returned no assets at all under a {auth!r} grant — "
                "the photo never reached the library the app can see")
    return "the pass saw assets and did nothing with them, silently"


def wait_rail_blob_settled(client_factory, kind, path, *, superseding=None, timeout=25.0):
    """Poll a per-actor reserved rail's ``get`` kind (``fauna.mls.get``,
    ``fauna.drafts.get``) until the sealed blob at ``path`` is present AND
    **settled** — two consecutive polls return identical bytes — the
    deterministic gate that a seat's debounced autosave uploaded that path,
    never a fixed sleep (convention 14). Returns the settled blob, which is the
    next wait's ``superseding``.

    Presence alone is NOT enough. The MLS group bootstrap binds a channel
    before the own message is appended, so the debounce routinely seals a
    message-less ``history/<ch>`` slice whose blob is already non-null; a
    second seat launched against that early blob restores a thread without
    the message and the crux assertion reads as a product bug
    (``docs/goal/behavior/devices.md`` § Implementation status today, the
    REPLICA_DEBOUNCE gap note). The blob is ``BackupKey``-sealed, so the test
    cannot read it; quiescence is the honest proxy.

    ``superseding``: a blob read BEFORE the change being waited for — the
    settled blob must differ from it. Without it, a poll landing before the
    next debounce fires sees the old blob twice and calls it settled. Each poll
    opens a fresh connection through ``client_factory`` (one identity, a
    read-only 'device').
    """
    deadline = time.monotonic() + timeout
    prev = None
    while time.monotonic() < deadline:
        with client_factory() as c:
            blob = c.call(kind, {"path": path}).get("blob")
        if blob is not None and blob != superseding:
            if prev is not None and blob == prev:
                return blob
            prev = blob
        time.sleep(0.5)
    raise AssertionError(
        f"{kind} {path!r} was never uploaded-and-settled within {timeout}s — "
        f"the autosave did not fire, or never stopped changing (a newer blob "
        f"was seen: {prev is not None})"
    )
