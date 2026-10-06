"""tier_3 e2e: the session-start critical-alert sweep raises a condition whose
own page was never opened.

Goal docs: ``docs/goal/behavior/critical-alerts.md`` § Mechanism → *Who runs
the detector* (the sweep's contract) and § Goal (why a page-local warning is
structurally unseeable); ``docs/goal/behavior/identity-succession.md``
§ The RecoveryKey → *Replacement* (the 30-day window this feeder watches).

**What only a full-stack run can prove.** The sweep crate's unit tests already
pin its logic against an in-memory transport — post, clear, and the fail-safe
"an unreachable feeder never clears a standing alert". What they cannot reach
is the claim the feature actually rests on: *that session establishment runs it
at all*, against a real nest, with the alert crossing the shared registry into
the app's own chrome on a page that is not the one that made the condition.
Between "the sweep is correct" and "the sweep is called" is precisely where
feeder #2 sat unnoticed — its projection was built, tested, and driven by
nobody, so the alarm was loud nowhere. That gap is what this test closes, and
why it asserts on the *feed* page rather than Settings.

The journey, driven the way a user would (convention 8 — every mutation is a
driver UI action, no RPC shortcut standing in for the user):

0. login dispatches the session-establish sweep; the test barrier-waits for
   that pass to complete before touching anything — see the latency note;
1. a fresh actor creates a recovery kit (so the seed-alone path is offered);
2. they use **"I lost my kit"**, which parks the 30-day window on the nest;
3. the session is established again (the app's universal post-auth hook, the
   same seam a relaunch or a sign-in takes);
4. on the **feed** page, the banner is up and names the window.

Latency discipline (convention 14): the sweep is spawned, so step 4 is a
deadline poll for a caused state transition with a generous ceiling — a green
run pays only the real round-trip. No settle-sleeps, no wall-clock asserts.
Step 0's barrier-wait needs the same discipline in the other direction: the
login-dispatched pass races the ceremony round trips with no completion-
ordering guarantee, so waiting for "a pass finished" only AFTER the ceremonies
would prove nothing about ordering. Barriering right after login instead makes
"the pre-ceremony pass found nothing" true by construction — nothing exists
yet to find.

**Between steps 2 and 3, whether "nothing has posted the alert yet" is even
true is app-specific — found live on web, apps row 315.** Every web route
mounts by calling ``identity.init()`` (``apps/fauna-web/src/routes/{feed,
settings,backups,events,media,conversations,admin,onboarding}/+page.svelte``),
whose async ``refreshFromServer`` tail hands the root layout's
``$identity``-object-gated sweep effect (``routes/+layout.svelte``) a FRESH
object on every mount — not only at login. So navigating into Settings to
build the kit (unavoidable; the section lives nowhere else) already re-sweeps
the account once or twice before anything is parked, and navigating to feed to
*read* the banner sweeps it again — and that pass now legitimately finds the
just-parked window. This is web genuinely re-checking on every navigation, not
a false poster: the text that lands is correct (right headline, right detail,
right fingerprint), just earlier than tui/linux/native's once-per-login
cadence would put it. Whether natives should also re-sweep on navigation, or
web's per-mount ``identity.init()`` should be dampened, is an open
architectural question, not something this test's own scope decides — so the negative
assertion below is native-only, and web instead gets the weaker, still
meaningful invariant that ONLY a sweep pass may post this key (never some
other code path), by requiring the pass counter to have moved.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app that both renders
the banner and calls the sweep — today tui, the lead app; the other six join as
their legs land (`critical-alerts.md` § Implementation status today).
"""
from __future__ import annotations

import time

import pytest

from helpers.budgets import ALERT_SWEEP_PASS_S
from helpers.waiting import alert_sweep_passes, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"

# A distinctive fragment of the headline, so the assertion pins the *feeder*
# rather than "some alert is up" — a custody alarm from another feeder would
# otherwise satisfy a bare presence check.
_HEADLINE_FRAGMENT = "replacement of your account recovery key"


def _alert_text(app) -> str:
    """The first active alert row's text. Empty when the banner is absent,
    which is the presence rule itself (`critical-alerts.md` § Rendering
    contract) — callers never test the registry a second way.

    Row 0 is the whole story here: this journey raises exactly one alert, and
    an alert's *lines* (headline + detail) are joined into its single
    ``critical-alert[N]`` row by the app, not split across rows. Reading row 0
    bare is also the only correct spelling — an indexed element is addressed by
    a driver-level index, never by a ``critical-alert[0]`` id string, which the
    agent would look up as a literal id and never find (the first version of
    this test did exactly that and reported an empty banner while the alarm was
    plainly painted on screen).
    """
    if not app.driver.is_visible(CRITICAL_ALERTS):
        return ""
    return app.driver.get_text(CRITICAL_ALERT) or ""


def _poll_for_alert(app, deadline_s: float = 60.0) -> str:
    """Deadline-poll the feed page until the banner names the pending window.

    The sweep is spawned at session establishment and completes on its own
    schedule, so this waits for the *caused transition* (an alert whose text
    names this feeder) rather than for a duration."""
    deadline = time.monotonic() + deadline_s
    text = ""
    while time.monotonic() < deadline:
        text = _alert_text(app)
        if _HEADLINE_FRAGMENT in text:
            return text
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
    return text


@pytest.mark.feature("critical-alerts")
def test_session_start_sweep_raises_the_pending_replacement_on_a_page_it_never_visited(
    app, nest_instance, request
):
    """A parked 30-day window is loud on the feed page after the next session
    start — the set-and-forget half of the banner's contract.

    A **dedicated fresh actor**, held by the test rather than taken from
    ``ungranted_app``, for two reasons: the ceremony writes this account's
    registration chain and parks a replacement on it, so the session-scoped
    shared user would make test ORDER load-bearing for everyone else reading
    that account; and step 3 has to log the *same* actor in a second time,
    which needs the user dict the fixture does not hand back.
    """
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user)

    # The login-dispatched sweep pass must be forced to COMPLETE here, before
    # anything is parked — not merely awaited later. The sweep is a fire-and-
    # forget spawn with no completion ordering guarantee against a subsequent
    # user action; waiting for "a pass has completed" *after* the ceremonies
    # below only proves a pass finished, never that it finished BEFORE they
    # ran. Barriering here instead makes "pass #1 found nothing" true by
    # construction: nothing exists yet to find.
    wait_until(
        lambda: (alert_sweep_passes(app.driver) or (0, 0))[1] >= 1,
        ALERT_SWEEP_PASS_S,
        diagnose=lambda: (
            "the login-dispatched critical-alert sweep pass never completed "
            f"(passes now: {alert_sweep_passes(app.driver)}) — proceeding "
            "without it would let a slow first pass read the ceremonies' "
            "parked window and pass or fail this test by luck"
        ),
    )
    passes_before_ceremony = (alert_sweep_passes(app.driver) or (0, 0))[1]

    app.settings.navigate()
    try:
        app.settings.open_recovery_kit()
    except TimeoutError:
        # `open_recovery_kit()` hard-waits on the section (correct for the
        # apps that have it); an app without it yet raises rather than
        # returning, so the presence check below — this test's own designed
        # skip path — must run regardless of whether the wait succeeded.
        pass

    if not app.is_visible("recovery-kit-create-button"):
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            app.driver,
            surface="recovery-kit-section",
            detail=(
                "settings.md § Recovery kit; the sweep needs this section to "
                "create the condition, so an app without it cannot run this "
                "journey yet — tui leads and the other six follow"
            ),
            tracked=(
                "docs/goal/behavior/critical-alerts.md "
                "§ Implementation status today"
            ),
        )

    # ── 1. A kit exists, so the seed-alone path is offered at all. ──
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    assert app.is_enabled("recovery-kit-lost-button"), (
        "a registered kit enables the seed-alone replacement; it reads disabled "
        f"with status {app.settings.recovery_kit_status()!r}, error surface: "
        f"{app.error_text()!r}"
    )

    # ── 2. "I lost my kit" — parks the window on the nest. ──
    app.settings.replace_lost_kit()
    # The barrier is this ceremony's OWN caused transition — the status line
    # moving to the pending arm. Not `recovery-kit-secret-display`: step 1's kit
    # already satisfies it, so a wait on it returns before this ceremony has
    # landed and a read after it can pass on step 1's "registered" line. And a
    # status read rather than a `wait_for` on the veto button: `wait_for` may
    # scroll its target into view (windows' does), moving the very layout this
    # step observes. The day count is the one moving part, so match the fixed
    # words before it, taken from the shared string every app renders.
    pending_head = S.settings.recovery_kit.status_replacement_pending(days="\0").split("\0")[0]
    wait_until(
        lambda: pending_head in app.settings.recovery_kit_status(),
        30.0,
        diagnose=lambda: (
            "the ceremony's status re-read rides its own outcome, so a window "
            f"that landed must show; error surface: {app.error_text()!r}; "
            # Convention 6: an empty read is two different findings — a line
            # never painted (count=0, or the loading text) versus one painted
            # and pushed out of view (count>=1, visible=False, a frame off the
            # window) — and only the snapshot tells them apart.
            f"{app.driver.diagnose('recovery-kit-status', attrs=('frame',))} "
            f"{app.driver.diagnose('recovery-kit-secret-display', attrs=('frame',))}"
        ),
    )

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    if app.driver.is_web():
        # See the module docstring: web re-sweeps on every page mount (a fresh
        # `identity.init()` object per navigation), so a pass legitimately runs
        # again between the plant above and this read — the alert CAN already be
        # up here, correctly. What must still hold on every app is that ONLY a
        # sweep pass may have posted it: assert the pass counter actually moved
        # if the alert is present, catching a genuine second poster without
        # asserting a per-login-only cadence web doesn't have.
        if _HEADLINE_FRAGMENT in _alert_text(app):
            passes_now = (alert_sweep_passes(app.driver) or (0, 0))[1]
            assert passes_now > passes_before_ceremony, (
                "the alert is up but no further sweep pass ran since it was "
                f"planted (passes then={passes_before_ceremony}, now={passes_now}) "
                "— something other than the sweep posted this key"
            )
    else:
        # Nothing has raised the alert yet, and nothing else ever will before
        # step 3: the sweep is the only caller of the projection anywhere in
        # the tree, its one pre-ceremony pass already ran (the barrier above
        # forced it to finish and find nothing, since nothing existed yet to
        # find), and native apps sweep once per login, not per navigation.
        assert _HEADLINE_FRAGMENT not in _alert_text(app), (
            "nothing but the session-start sweep may post this alert — a second "
            "poster would make the rest of this test prove nothing"
        )

    # ── 3. Establish the session again: the universal post-auth hook, the same
    #       seam a relaunch or a fresh sign-in takes. ──
    _login_app_as(app, request, nest_instance, user)

    # ── 4. …and the window is loud on the feed page, which no part of this
    #       journey's condition lives on. ──
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    text = _poll_for_alert(app)
    assert _HEADLINE_FRAGMENT in text, (
        "the parked 30-day window must reach the every-page banner after a "
        f"session start; banner reads {text!r}, error surface: "
        f"{app.error_text()!r}"
    )
    assert S.critical_alerts.recovery_replacement_pending in text, (
        "the row renders the shared i18n headline verbatim, so all seven apps "
        f"say the same thing; got {text!r}"
    )
    # The detail line carries the countdown and fingerprint a user checks
    # against a kit in hand — its absence would leave an alarm they cannot act
    # on (`critical-alerts.md` § Feeders — why this feeder clears the bar).
    assert "The replacement takes effect in" in text, (
        f"the detail line must ride with the headline; got {text!r}"
    )
