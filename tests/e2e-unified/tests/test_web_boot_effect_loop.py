"""Boot invariants read off the browser console ring.

Both guards here share one shape: a defect the SPA *survives* — it logs and
carries on — so the console is the only witness, and nothing fails until
somebody reads it. That is precisely the class that outlives every session
which merely notices it. Each is an assertion about what a booted, signed-in
SPA did NOT log.

── 1. No self-invalidating Svelte-5 effect ──────────────────────────────────

`effect_update_depth_exceeded` is Svelte's *cap*, not its recovery: an `$effect`
that writes a `$state` it also read re-dirties itself, Svelte re-runs it until it
hits the depth limit, and then it **throws mid-flush**. Whatever else was queued
in that flush is collateral. So a "survived" depth-exceeded is not benign — it is
an unbounded amount of the layout's post-auth work silently not running, and the
symptom it produces (a page that never reaches a usable state) looks like a bug in
whatever surface happened to be downstream, never like a reactivity defect.

That is exactly how it presented: two of these fired on EVERY boot of the SPA
(once per mount of the initial double-mount) and were logged as a follow-up
residual — "self-limited, boot settles, renderer 99% idle" — while sibling
sessions chased the resulting page failures in the admin hub and the composer.

The owner was `$lib/screenTime.svelte.ts`'s `usageEpoch += 1`: a read-modify-write
of a `$state`, reached synchronously from the root layout's screen-time `$effect`.
Svelte 5 tracks reads *dynamically* — through called functions, not just the
effect body — so the `+=`'s read enrolled `usageEpoch` as a dependency of the very
effect that then wrote it. Fixed by publishing the epoch from a plain counter
(`bumpUsageEpoch()`), leaving the `$state` write-only.

This guard is deliberately about the CLASS, not that one call site: any future
effect/derived that writes what it reads fails here, at boot, naming itself —
rather than surfacing as an unexplained empty list three surfaces downstream.

── 2. No actor-scoped drop that throws ──────────────────────────────────────

The second guard's own case is in its test docstring below.

── 3. The launch folder pass runs, and after the account runtime ────────────

The third is the same class with a positive witness; its case is in its test
docstring too.

Web-only by construction: a browser console is the witness, and only the web app
has one (convention 7 — a structural impossibility elsewhere, not an unbuilt gap).
"""

from __future__ import annotations

import pytest

from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# The launch folder pass's three terminal lines (`$lib/conversations` and
# `$lib/launch-pass`) and the runtime's own started line
# (`fauna-wasm`'s `account_runtime::start`).
_PASS_RAN = "folders: launch pass ran"
_PASS_FAILED = "folders: resume pending removals failed at launch"
_PASS_SKIPPED = "folders: launch pass skipped"
_RUNTIME_STARTED = "account runtime: started for this tab"

# Matched as a substring so both the bare code and the `https://svelte.dev/e/<code>`
# URL form (which is what the browser console actually carries) hit.
#
# Deliberately just this one code. Svelte's sibling reactivity error
# `state_unsafe_mutation` belongs to the same class and would be worth guarding
# too — but this suite has never been observed clean of it, and asserting on an
# error nobody has confirmed absent would land a red whose cause is unknown.
# Add it in the same change that first observes a boot without it.
_LOOP_ERROR_CODES = ("effect_update_depth_exceeded",)

# `actorScope.ts`'s own words: "A reset that throws is a bug in that reset". It
# logs and carries on deliberately — a half-dropped switch is worse than a noisy
# one — which makes the console the ONLY witness, and an unwatched witness is how
# this ran on every single web login for months.
_RESET_FAILURE_MARKER = "[actor-scope] reset failed"


def test_boot_has_no_svelte_reactivity_loop(logged_in_app):
    """A fully booted, authenticated SPA logged no reactivity-loop error.

    Latency-independent (convention 14): `logged_in_app` yields only once the app
    is booted and signed in, and the console ring holds this boot's lines — so
    this reads a settled terminal state and never races the boot. There is no
    budget to size and a green run pays nothing. (The ring is bounded at 500 and
    can evict its head on a multi-boot journey; this one boots once, and a green
    run here is an assertion about what the SPA did NOT log, so an eviction
    would only ever hide an offender — `console_log` says so in a leading line
    when it happens.)
    """
    driver = logged_in_app.driver
    console = driver.console_log()

    offenders = [
        line for line in console if any(code in line for code in _LOOP_ERROR_CODES)
    ]

    assert not offenders, (
        f"the SPA boot logged {len(offenders)} Svelte reactivity-loop error(s) — an "
        f"$effect or $derived is writing a $state it also reads, so it re-dirties "
        f"itself until Svelte's depth cap throws MID-FLUSH and abandons whatever "
        f"else that flush had queued (typically the layout's post-auth effects: "
        f"deployment-seed custody leg, mail-epoch refresh, host-address report, critical-alert "
        f"sweep). Find the offending effect by the frames in these lines, then make "
        f"its write not read — see `$lib/screenTime.svelte.ts`'s `bumpUsageEpoch()` "
        f"for the shape. Offending lines: {offenders!r}"
    )


def test_boot_drops_every_actor_scoped_reset_without_throwing(logged_in_app):
    """No registered actor-scoped drop threw while signing this session in.

    The sibling of the guard above, and the same shape: a boot invariant whose
    only witness is the browser console.

    `resetActorScopedState()` runs every registered drop under its own
    `try`/`catch` on purpose — one throwing reset must not strand the others,
    because a HALF-dropped switch is the silent wrong-actor render that whole
    seam exists to prevent. The cost of that deliberate choice is that a broken
    drop is invisible: it logs, the switch completes, and the state that drop
    owned stays live for the incoming actor. Class 1/4 of
    `account-scoping.md` § The scoping taxonomy's in-memory corollary is
    breached and nothing fails.

    Measured: `resetScreenTime` called `usage()` — a LAZY CONSTRUCTOR that
    builds its handle through wasm — so it threw `WASM not initialized` on
    every login that lands before the module is up. The e2e agent's
    `set_state` patch is exactly that door: it bypasses `identity.login()`,
    which is what would have run `ensureWasm()`. So this fired on every web
    test login, twice per login, and the two lines after the throw
    (`heartbeat.reset()` and `bumpUsageEpoch()`) never ran. It sat in the ring
    unread long enough to become the ambient noise that sessions chasing
    composer failures had to explain away.

    Latency-independent (convention 14) for the same reason as its sibling:
    `logged_in_app` yields a booted, signed-in app, so this reads a settled
    terminal state with no budget to size.

    Web-only by construction: a browser console is the witness (convention 7 —
    a structural impossibility elsewhere, not an unbuilt gap).
    """
    driver = logged_in_app.driver
    console = driver.console_log()

    offenders = [line for line in console if _RESET_FAILURE_MARKER in line]

    assert not offenders, (
        f"{len(offenders)} registered actor-scoped drop(s) THREW while signing in "
        f"— `resetActorScopedState()` swallowed each one, so the switch reported "
        f"success while the state those drops own stayed live for the incoming "
        f"actor (account-scoping.md § The scoping taxonomy, in-memory corollary). "
        f"A drop must be pure module/component state: no wasm face, no lazy "
        f"constructor, no round trip — see `$lib/screenTime.svelte.ts`'s "
        f"`heartbeat?.reset()` for the shape (it read `usage()`, which BUILDS the "
        f"handle through wasm). Offending lines: {offenders!r}"
    )


def test_boot_runs_the_launch_folder_pass_once_the_account_runtime_has_started(
    logged_in_app,
):
    """The launch folder pass ran this boot, and only after custody was readable.

    The pass resumes a member removal a crash left staged between its key
    rotation and its publish — without it the removed member keeps the live
    content key (`mls-group-key-material.md` § M2 → *Rotate-on-removal*) — and
    re-seeds the homes of cross-nest folder channels. Both read the folder-key
    custody, which rests in this tab's account runtime, and the conversations
    build starts that runtime without awaiting it.

    Measured before the fix: on every launch the pass ran first, logged
    `resume pending removals failed at launch … the account runtime is not
    running`, was swallowed, and the recovery waited for a launch that happened
    to win the race. The SPA survived, so the console was the only witness.

    Three assertions, the positive one first — a guard on the warning's
    absence alone would stay green if the pass stopped running altogether:
    the pass's own ran line is present, the runtime's started line precedes
    it, and neither the failure nor the skip line was logged.

    Latency-independent (convention 14): the wait is on the pass reaching ANY
    of its three terminal lines, a state every boot reaches; the budget is a
    ceiling a green run never approaches, not a delay.
    """
    driver = logged_in_app.driver

    def terminal():
        lines = driver.console_log()
        hit = any(
            marker in line
            for line in lines
            for marker in (_PASS_RAN, _PASS_FAILED, _PASS_SKIPPED)
        )
        return lines if hit else None

    console = wait_until(
        terminal,
        90,
        diagnose=lambda: (
            "the launch folder pass reached none of its terminal lines — it never "
            "ran, or the conversations manager was never built this boot. Console "
            f"tail: {driver.console_log()[-25:]!r}"
        ),
    )

    failed = [ln for ln in console if _PASS_FAILED in ln or _PASS_SKIPPED in ln]
    assert not failed, (
        "the launch folder pass did not complete on a normal boot — a staged member "
        "removal would stay un-resumed until a later launch. `not running` in the "
        "line means the pass ran before the account runtime had started "
        "(`$lib/launch-pass` is what orders them); a skip means the runtime's start "
        f"failed. Offending lines: {failed!r}"
    )

    ran = next(i for i, ln in enumerate(console) if _PASS_RAN in ln)
    started = next(
        (i for i, ln in enumerate(console) if _RUNTIME_STARTED in ln), None
    )
    assert started is not None and started < ran, (
        "the launch folder pass logged its ran line without the account runtime's "
        "started line ahead of it — the pass read a custody that was not yet "
        f"readable, or the ring evicted the line. started index: {started}, ran "
        f"index: {ran}. Console head: {console[:3]!r}"
    )
