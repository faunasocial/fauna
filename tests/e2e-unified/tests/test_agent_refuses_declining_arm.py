"""Convention 11's *second* clause: a recognised arm that DECLINES is loud.

`tests/test_agent_refuses_unknown_command.py` pins the floor — an action no app
implements surfaces rather than vanishing. This file pins the half that floor
cannot reach, and that is where the rule's real cost has always been:

    *"an unknown command, **an unimplemented one, and a seam that throws** all
    surface on the app's own `error-message` element"*
    (`docs/goal/architecture/e2e-conventions.md` § convention 11)

An arm that is *present*, is *dispatched to*, and then quietly does nothing looks
identical to a working one from the driver's side — it acks green — so the
missing effect surfaces as whatever assertion runs next, in whatever feature the
command belonged to. Both instances this file pins were exactly that shape, and
both were found by reading rather than by any failing test (2026-08-21):

* `screen_time_heartbeat` advanced the fake clock and then ran the heartbeat only
  `if (secret)`. With no identity on the seat it moved the clock, sent nothing,
  and returned success — so the absent usage row read downstream as a screen-time
  accrual bug.
* `conversations_accept_recipient` discarded `acceptCurrentRecipientChip()`'s
  boolean, so a commit that landed no chip acked green and surfaced ~5 s later as
  the action layer's generic *"chip not added"*, which names neither the command
  nor the reason.

**App scope, and what the remaining apps owe.** The two `screen_time_heartbeat`
cases are web-specific arms and stay `web`-marked. The third
(`conversations_accept_recipient` against an untouched picker) is a *cross-app*
command, so it is marked for every app whose arm has been made loud: web
(2026-08-21), then tui and linux (2026-08-28, this file's rename from
`test_web_agent_refuses_declining_arm.py`), then **macos and ios** (2026-08-28),
and **windows** (2026-08-29 — the last leg). Before that each acked green in
exactly this situation — tui's `accept_recipient` discarded
`accept_current_recipient_chip()`'s boolean and returned `true` whenever a manager
existed, linux's `e2e_accept_recipient` returned the boolean to a `main.rs` caller
that dropped it, and both apple targets discarded it with a literal `_ =`.
**windows dropped it twice over**: its arm called
`ConversationsPage.Current?.AcceptVisibleRecipientPicker()`, so the boolean was
discarded AND an unmounted conversations page turned the whole command into a
silent no-op through the `?.` — the second drop being invisible from the driver in
exactly the same way. It now drives `ConversationsManagerHost.Instance` directly,
like every sibling. The six marks below are every app the arm exists on; android's
agent has the refusal slot but no `conversations_accept_recipient` arm at all.

**Apple needed more than the boolean, and the two halves had to land together.**
Both apple targets also skipped the *probe-then-commit* order every sibling app
drives, so their accept could only ever use the format-only
`try_parse_typed_address` — which cannot produce `TypedAddress::Fauna` by design.
A typed Fauna handle therefore committed no chip over the agent while working
fine for a real user, and honouring the boolean alone would have turned that
latent under-honouring into a loud, *correct-looking* refusal on exactly the
inputs that should succeed. Apple's refusal also needed a new home: its
`testAgentFailure` wrote to `AppMessages.error`, the page's own banner mirror
(`ErrorBanner.onAppear`/`onDisappear`), which is the wrong-slot trap tui and
android each shipped first — apple was the third. It now stamps
`AppMessages.refusedAgentCommand`, published by `errorForDisplay` ahead of the
page banner and cleared only at `reset`.

**The refusal reason is shared, not re-spelled per app.**
`fauna_conversations::manager::ACCEPT_RECIPIENT_NO_CHIP_REASON` is the single
home for the text both Rust apps report, so the three surfaces cannot drift
apart in wording; web's TypeScript arm carries the same sentence.

**Why the assertions read either an exception or `error-message`.** The apps do
not share one channel and are not meant to: web's commands run as in-page JS
through `window.__fauna_callCommand`, so a throw propagates out of `_execute_js`
into the driver call — strictly louder than a state field, and the channel
convention 2 already sanctions for auth failures — while the native apps have no
call stack reaching the driver and stamp their refusal slot instead.
`refusal_from` reads both, so the assertion bodies below stay app-agnostic. The
floor test records the same architectural split; what the convention forbids is
silence, not a particular surface.
"""

import pytest

from helpers.agent_refusal import refusal_from

# The app axis is per-test here, not module-wide: the two screen_time cases pin
# web-only arms, while the accept-recipient case is cross-app. `iter_markers()`
# (conftest's deselection hook) sees decorator marks exactly as it sees these.
pytestmark = [pytest.mark.tier_3]


@pytest.mark.web
def test_screen_time_heartbeat_refuses_without_an_identity(app):
    """No identity on the seat ⇒ the heartbeat arm refuses instead of no-opping.

    Uses the logged-OUT `app` fixture on purpose: that is the state in which the
    arm used to silently skip its only side effect.
    """
    surfaced = refusal_from(app, "screen_time_heartbeat", {"minutes": 30})

    assert surfaced, (
        "screen_time_heartbeat acked green on a seat with no identity and did "
        "nothing — a SILENT DECLINE, which e2e-conventions.md § convention 11 "
        "forbids just as squarely as an unknown command. Downstream this reads "
        "as a screen-time accrual bug: the clock moved, no usage was reported, "
        "and nothing named the command that skipped itself."
    )
    assert "screen_time_heartbeat" in surfaced, (
        f"the refusal must NAME the command it refused (convention 6). Got {surfaced!r}"
    )


@pytest.mark.web
def test_screen_time_heartbeat_refuses_a_non_numeric_minutes(app):
    """A malformed payload is a decline, not a no-op.

    `Number("soon")` is `NaN`, and `advanceTestClock(NaN)` moves the skew
    nowhere while reporting success — the bad-payload corner convention 11 names
    alongside the unwired-collaborator one.
    """
    surfaced = refusal_from(app, "screen_time_heartbeat", {"minutes": "soon"})

    assert surfaced, (
        "screen_time_heartbeat accepted a non-numeric `minutes`, advanced the "
        "clock by NaN and acked green — the accrual assertion then fails as a "
        "product bug with nothing pointing back at the payload."
    )
    assert "minutes" in surfaced, (
        f"the refusal must name the field it rejected (convention 6). Got {surfaced!r}"
    )


@pytest.mark.web
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
def test_accept_recipient_refuses_when_no_chip_commits(logged_in_app):
    """An accept against an untouched picker commits nothing ⇒ it must say so.

    Nothing is typed and the composer is never opened, so the manager has no
    resolvable recipient and `acceptCurrentRecipientChip()` returns `false`.
    Before the fix that boolean was discarded and the command returned success.
    """
    app = logged_in_app

    assert not app.has_error(), (
        "a pre-existing error would make the assertion below vacuous. "
        f"Current text: {app.error_text()!r}"
    )

    surfaced = refusal_from(app, "conversations_accept_recipient", {})

    assert surfaced, (
        "conversations_accept_recipient committed no chip and acked green — a "
        "SILENT DECLINE. The action layer then polls 5 s and fails with the "
        "generic 'chip not added', which names neither the command nor why it "
        "declined; convention 11 wants the agent to answer, not the poll."
    )
    assert "conversations_accept_recipient" in surfaced, (
        f"the refusal must NAME the command it refused (convention 6). Got {surfaced!r}"
    )
