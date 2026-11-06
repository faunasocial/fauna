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
from helpers.app_surface import app_name, skip_environment


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
    # The tracker fires only on a rise of a thread's unread count whose activity
    # is at or past the launch floor: a thread reading 0 unread never rose, one
    # reading ≥1 that fired nothing was held under the floor.
    try:
        threads = driver.get_state("data.conversation_threads") or []
        unread = {t.get("thread_id"): t.get("unread_count") for t in threads}
        selected = driver.get_state("data.selected_thread_id")
    except Exception as exc:  # never let the diagnostic mask the real assert
        unread, selected = f"(unreadable: {exc!r})", None
    why = f"\nThreads' unread counts: {unread}; selected thread: {selected!r}"
    return why + (f"\nApp log (banner lines): {lines[-5:]}" if lines else "")


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# Every app has built the *firing* leg and publishes the log. The one short
# column is a LAUNCH SHAPE, not an app: macOS shares every line of the firing leg
# with `ios` (both are FaunaKit's `MessageBannerObserver`), and what decides its
# column is run-level rather than per-app. See `_launched_as_a_bundle`.


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
    The bare-binary launch is therefore a property of the run, declared as an
    environment skip: it is not app work and not debt.
    """
    reader = getattr(driver, "bundle_path", None)
    return bool(reader()) if callable(reader) else False

#: Why the macOS bare-binary column is short, in the goal doc's own words. The
#: pointer is the status row itself — `conversations.md` § Implementation status
#: today, the *DM OS-toast firing* row.
_BARE_BINARY = (
    "the firing leg IS built and IS witnessed — by `--app ios` on the same "
    "shared FaunaKit code, and by replaying THIS module run-level under "
    "`--macos-artifact`. An ordinary `--app macos` run launches the BARE "
    "`FaunaMacOS` binary, and a non-bundle process has no notification host "
    "at all (`NotificationHost.isAvailable`), so the app hands no banner to "
    "the platform and honestly records none. Not app work and not debt: "
    "replay it with `pytest tests/e2e-unified/tests/"
    "test_conversations_message_banner.py --app macos --macos-artifact` "
    "(conversations.md § Implementation status today, DM OS-toast firing)"
)


def _require_banner_leg(driver):
    """Skip with a declared reason when the launch shape cannot fire a banner."""
    if app_name(driver) == "macos" and not _launched_as_a_bundle(driver):
        skip_environment(_BARE_BINARY)


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

    # ── Seed the tracker, and assert the seeding batch stayed silent ────────
    # After this inject the tracker is certainly seeded, whichever way the
    # account was loaded. WHICH snapshot seeded it is the tracker's to say, not
    # the thread list read above: the rails load asynchronously, so a list read
    # before the login load landed looks empty while the load seeds the tracker
    # a moment later — and `alpha` is then genuinely new, rightly bannered (a
    # 2026-10-12 sweep red read exactly that as a seed that fired). So the
    # silent-seed rule is asserted against the batch the tracker names.
    planted = banners["started"]
    alpha = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"alpha-banner-{nonce}@self-nest.test",
        subject=None,
        body="the thread that seeds the tracker",
    )
    waiting.await_banner_pass_after(d, planted, what="the seeding thread")
    seeded_with = waiting.require_message_banners(d).get("seeded_with")
    assert seeded_with, (
        "the banner log names no seeding batch after a non-empty snapshot "
        f"({waiting.message_banners(d)}) — `message_banners_json` owes "
        "`seeded_with` (fauna_conversations::notification)."
    )
    fired_from_seed = [t for t in waiting.fired_banner_threads(d) if t in seeded_with]
    assert fired_from_seed == [], (
        f"thread(s) {fired_from_seed} of the snapshot that seeded the tracker "
        f"raised a banner (seeding batch: {seeded_with}). The seeding snapshot "
        "is silent — the login load is not news."
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
