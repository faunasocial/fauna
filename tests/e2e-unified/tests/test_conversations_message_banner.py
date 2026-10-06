"""The running app raises a system notification for a new message — except in
the conversation you already have open.

`docs/features/conversations.md` outcome 11, which until this file was witnessed
on **no column at all**, on either of the two apps that had built the firing.
Authority: `docs/goal/ui/conversations.md` § Where logic lives — the *when /
for-whom* decision is the shared `MessageNotificationTracker`'s and has exactly
three rules (seed silently on the first non-empty snapshot; fire when a thread's
`unread_count` rises, a brand-new thread with unread messages included, while its
newest activity is at or past the launch floor; suppress the selected
thread), while the *firing* is the only per-app glue.

**What is asserted, and what deliberately is not.** The app appends every banner
it hands to the platform's toast API to the shared fired-banner log, published as
`fauna_e2e_agent::MESSAGE_BANNERS_KEY`; this file reads that log. It never looks
at the OS notification centre — that is the last inch (does the toast *appear*,
and look right), not the mechanism, and no test on any platform can read it
anyway. The log is appended at the firing site, after every suppression the app
applies, so it means "the user was shown this", not "the tracker returned this":
glue that swallowed a decision still fails here.

**Latency independence (convention 14).** Two of the three rules are *negative* —
a banner that must NOT appear — and there is nothing on screen to watch for its
non-arrival. Every negative read is therefore anchored on
`waiting.await_banner_pass_after`, which waits for a diff tick that *began* after
the message was planted to finish; no wall-clock sleep appears here, and none
may be added (a timing-keyed test of this shape is defunct, not flaky).

**Why one journey rather than three test functions.** The tracker is stateful
across snapshot ticks and the app process is module-scoped, so the three rules
are three states of one machine reached in order: the login load must be silent
*before* the tracker is seeded, and the open-thread suppression is only
meaningful *after* a fire has been proven possible. Split across functions they
would share the same live tracker anyway, in whatever order pytest picked.
"""

from __future__ import annotations

import uuid

import pytest

from helpers import waiting
from helpers.app_log_section import app_text
from helpers.app_surface import app_name, skip_unbuilt


def _why(driver):
    """Extra diagnosis for a banner that did not fire (convention 6).

    A missing banner has two very different causes — the shared tracker decided
    against it, or the app's firing glue dropped what it decided — and the log
    alone cannot tell them apart. Every app's firing path says so in the app's
    own words when it declines or drops a fire (apple's
    `MessageBannerObserver` logs `[banner] not raised …` at `.warn`), so hand
    the matching lines to the failure instead of making the next reader re-run
    a 25-minute suite to see them.

    Read through `app_log_section.app_text`, which is the one place the
    `app_log_text` / `app_stderr_text` pair is walked — every driver family
    keeps the app's own words under one of those two names, so this filter
    reaches all of them without any driver growing a third spelling of it.
    """
    lines = [ln for ln in app_text(driver).splitlines() if "banner" in ln.lower()]
    return f"\nApp log (banner lines): {lines[-5:]}" if lines else ""


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# The apps that have built the *firing* leg and publish the log. Everything else
# is debt with a row pointed at it, declared here rather than inferred from a
# missing state key: an app that had built the firing and then broke the key must
# fail, not skip (convention 7 — a skip is not coverage).
#
# ⚠ `macos` is deliberately NOT here, and not because the app is short: it shares
# every line of the firing leg with `ios` (both are FaunaKit's
# `MessageBannerObserver`). What decides the macOS column is the LAUNCH SHAPE,
# which is run-level rather than per-app — so it cannot be a membership question.
# See `_launched_as_a_bundle`.
_BANNER_APPS = {"android", "ios", "linux", "tui", "web", "windows"}


def _launched_as_a_bundle(driver) -> bool:
    """Did this macOS launch come from the `.app`, or from the bare binary?

    `NotificationHost.isAvailable` is `Bundle.main.bundleURL.pathExtension ==
    "app" && bundleIdentifier != nil`, so a bare-binary process has no
    notification host at all: the app hands no banner to the platform and — by
    `MessageBannerObserver`'s deliberate design — records none rather than
    recording around the guard. The driver already reports which of the two ran
    (`drivers/macos.py::bundle_path`, `None` exactly for a bare-binary launch),
    so this module asserts the column under `--macos-artifact` and declares it
    short otherwise.

    The flag is run-level and single-valued — `docs/goal/architecture/apps/
    apple-e2e-automation.md` § Artifact launch mode, *"so any macOS journey can
    be replayed against the artifact"* — so no per-module launch shape is
    invented here; this module is simply one of the journeys that replay covers.
    The default stays the bare binary (rule 9 of the same doc), which is why
    `macos` cannot go in `_BANNER_APPS`: that set is consulted unconditionally
    and would red every ordinary run.
    """
    reader = getattr(driver, "bundle_path", None)
    return bool(reader()) if callable(reader) else False

# Why each column is short, in the goal doc's own words. The shipped pointer is
# the status row itself — `conversations.md` § Implementation status today, the
# *DM OS-toast firing* row — which carries the same per-app verdicts and is
# where a reader should look first.
_TRACKED = "conversations.md § Implementation status today (DM OS-toast firing)"

#: What `skip_unbuilt` names as missing — per-app, because macOS's is not the
#: others'. `skip_unbuilt` composes "<app> has not built <surface>", which is
#: the plain truth for an app with no firing leg and a falsehood for macOS:
#: the banner IS built there and the bare-binary process simply has nothing to
#: hand it to. One shared string would put "has not built the … banner" in the
#: run summary directly above a detail saying it is built.
_SURFACE = "the new-message OS banner (conversations outcome 11)"
_SURFACE_MACOS = (
    "a notification host for the new-message OS banner (conversations "
    "outcome 11) in this launch shape"
)

_UNBUILT = {
    "macos": (
        "the firing leg IS built and IS witnessed — by `--app ios` on the same "
        "shared FaunaKit code, and by replaying THIS module run-level under "
        "`--macos-artifact`. An ordinary `--app macos` run launches the BARE "
        "`FaunaMacOS` binary, and a non-bundle process has no notification host "
        "at all (`NotificationHost.isAvailable`), so the app hands no banner to "
        "the platform and honestly records none. Not app work and not debt: "
        "replay it with `pytest tests/e2e-unified/tests/"
        "test_conversations_message_banner.py --app macos --macos-artifact`"
    ),
}


def _require_banner_leg(driver):
    """Skip with a declared reason on an app that has not built the firing."""
    name = app_name(driver)
    if name in _BANNER_APPS:
        return
    if name == "macos" and _launched_as_a_bundle(driver):
        return
    skip_unbuilt(
        driver,
        surface=_SURFACE_MACOS if name == "macos" else _SURFACE,
        detail=_UNBUILT.get(name, "no firing leg"),
        tracked=_TRACKED,
    )


@pytest.mark.feature("conversations")
def test_new_message_raises_a_banner_except_in_the_open_thread(logged_in_app):
    """The three rules of the shared `MessageNotificationTracker`, end to end.

    1. The threads already there when you signed in raise nothing.
    2. A message in a thread you do not have open raises a banner naming it.
    3. A message in the thread you *do* have open raises nothing.
    """
    d = logged_in_app.driver
    conv = logged_in_app.conversations
    _require_banner_leg(d)

    nonce = uuid.uuid4().hex[:8]

    # ── Rule 3: the login load is silent ────────────────────────────────────
    # Whatever this (session-scoped, state-accumulating) account already held,
    # the app's module-boundary cold relaunch means the log starts empty and the
    # login load is the first batch this tracker ever sees. It must have seeded
    # silently — a toast storm on sign-in is the failure this rule exists to
    # stop.
    threads_at_login = conv.list_threads()
    banners = waiting.require_message_banners(d)
    assert banners["fired"] == [], (
        "signing in raised "
        f"{len(banners['fired'])} new-message banner(s) for the "
        f"{len(threads_at_login)} thread(s) already in this account "
        f"({banners['fired']}). Threads present at login are not new — the "
        "shared tracker seeds silently on its first non-empty snapshot."
    )

    # ── Seed the tracker, and assert the seeding batch when it is ours ──────
    # After this inject the tracker is certainly seeded, whichever way the
    # account was loaded. When the account was EMPTY at login this inject is
    # itself the seeding snapshot, so the same rule applies to it and is worth
    # asserting; when it was not, the assertion above already covered the seed.
    planted = banners["started"]
    alpha = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"alpha-banner-{nonce}@self-nest.test",
        subject=None,
        body="the thread that seeds the tracker",
    )
    waiting.await_banner_pass_after(d, planted, what="the seeding thread")
    if not threads_at_login:
        assert waiting.fired_banner_threads(d) == [], (
            "the FIRST batch of threads this app ever saw raised a banner "
            f"({waiting.fired_banner_threads(d)}). The account was empty at "
            "login, so this inject is the seeding snapshot and is silent by the "
            "same rule the login load follows."
        )

    # ── Rule 1: a message in a thread you do not have open raises a banner ──
    before = waiting.fired_banner_threads(d)
    planted = waiting.message_banners(d)["started"]
    beta = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"beta-banner-{nonce}@self-nest.test",
        subject=None,
        body="a message you are not looking at",
    )
    assert beta != alpha, "the two injects merged into one thread — nonce collision"
    waiting.await_banner_pass_after(d, planted, what="the unopened thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [beta], (
        "a new message in a thread that is NOT open must raise exactly one "
        f"banner for that thread. Fired since the plant: {fired[len(before):]}; "
        f"expected [{beta!r}]. Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # ── Rule 2: a message in the thread you have open raises nothing ────────
    # Opening it is a real UI gesture (convention 8), which is also what puts
    # the thread in the snapshot's `selected_thread_id` the tracker suppresses.
    conv.open_thread_by_id(beta)
    before = waiting.fired_banner_threads(d)
    planted = waiting.message_banners(d)["started"]
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"beta-banner-{nonce}@self-nest.test",
        subject=None,
        body="a message in the conversation you are already reading",
    )
    waiting.await_banner_pass_after(d, planted, what="the open thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired == before, (
        "a new message in the thread the user ALREADY HAS OPEN must raise no "
        f"banner — the user is looking at it. Fired since the plant: "
        f"{fired[len(before):]}. Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # The suppression must be the *open thread*, not the app being frontmost:
    # prove the tracker is still live by landing one more banner on the other
    # thread while `beta` stays open. Without this the assertion above would
    # also pass on an app that had simply stopped firing altogether.
    planted = waiting.message_banners(d)["started"]
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"alpha-banner-{nonce}@self-nest.test",
        subject=None,
        body="still talking over here",
    )
    waiting.await_banner_pass_after(d, planted, what="the other thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [alpha], (
        "with one thread open, a message in a DIFFERENT thread must still raise "
        f"its banner. Fired since the plant: {fired[len(before):]}; expected "
        f"[{alpha!r}]. If this is empty while the previous assertion passed, the "
        "app has stopped firing entirely rather than suppressing the open "
        f"thread. Full log: {waiting.message_banners(d)}{_why(d)}"
    )
