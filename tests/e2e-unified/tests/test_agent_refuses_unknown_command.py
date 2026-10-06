"""Convention 11's own self-test: an unknown command is REFUSED, never dropped.

`docs/goal/architecture/e2e-conventions.md` § convention 11 is the harness's
oldest and most expensive rule — *"every app's in-app test agent either honours
a command or fails loudly … an unknown command, an unimplemented one, and a seam
that throws all surface on the app's own `error-message` element, never in a
`.debug` log and never as a bare `return`."* Every clause of it has a named price
in sessions: a missing `conversations_real_resolve_send_new` on apple read as MLS
**at-rest data loss**, and a swallowed `inject_inbound` throw read as a lost async
race.

**Yet until this file the rule had no test, and that gap had already been paid
for.** Surveyed 2026-08-16 across all seven agents: tui refuses in
`automation.rs::apply_command`'s wrapper, web in `e2e-commands.ts`'s registry
miss, android in `TestAgent.kt`'s `else ->` arm, apple in
`FaunaMacApp.swift`/`FaunaApp.swift`'s `testAgentFailure("unknown command: …")`,
windows in `TestAgent.cs`'s `default:` arm — and **linux's single
`match cmd.action.as_str()` ended in a bare `_ => {}`**, so an unimplemented
command on linux acked green and was observable nowhere. Six apps honouring a
contract is not the contract holding; it is five witnesses and one blind spot,
and the blind spot is invisible precisely because a silent drop produces no
signal anywhere.

**Why this is worth a test rather than a code review.** The failure mode is
*absence* — no error, no effect, no log the driver reads — so it cannot be caught
downstream: it surfaces as whatever assertion happens to run next, in whatever
feature the dropped command belonged to. That is the disguise convention 11
names, and the reason the rule ends up re-learned rather than enforced. This test
converts it into a direct, per-app, one-line verdict.

⚠ **What this test deliberately does NOT assert: that the command was ignored.**
That would be a negative assert with no causal anchor (convention 14). The
observable here is entirely positive — a specific message appeared on the app's
own error surface — which is exactly why no barrier and no settle-sleep are
needed.
"""

import pytest

pytestmark = pytest.mark.tier_3

# A name no app implements and none ever will. Spelled to be greppable and
# obviously synthetic, so a future session meeting it in a log knows instantly
# that a test put it there rather than hunting for a real feature.
UNKNOWN_ACTION = "fauna_e2e_no_such_command_convention_11_probe"


def test_an_unknown_command_surfaces_on_the_apps_own_error_surface(logged_in_app):
    """The whole of convention 11's floor, on whichever app is under test.

    The precondition matters as much as the assertion: a stale error would make
    this pass against an agent that dropped the command silently, which is the
    single failure this file exists to catch.
    """
    app = logged_in_app

    assert not app.has_error(), (
        "a pre-existing error would make the assertion below vacuous — this test "
        f"can only prove anything if the message it reads is the one {UNKNOWN_ACTION} "
        f"caused. Current text: {app.error_text()!r}"
    )

    # The ack is NOT the observable. Every bridge-backed app acks either way, by
    # design — the driver's poll has to terminate — so a green return here says
    # nothing about whether the command was honoured, refused, or dropped on the
    # floor. That is the entire reason the assertions below read the error
    # surface instead.
    #
    # **Two loud channels are legal, and the difference is architectural rather
    # than a per-app choice.** The five bridge-backed agents stamp the app's own
    # agent-failure slot, which the state serializer publishes into
    # `messages.error` ahead of any page error. **web** cannot: its commands run
    # as in-page JS through `window.__fauna_callCommand`, whose registry miss
    # *throws*, and the throw propagates out of `_execute_js` into this call —
    # which is strictly louder than a state field, and is the same channel
    # convention 2 already sanctions for auth failures. What the convention
    # forbids is silence, not a particular surface, so this test accepts either
    # and requires the same thing of both: that it NAMES the command.
    surfaced = ""
    try:
        app.driver.call_command(UNKNOWN_ACTION, {})
    except Exception as exc:  # web's registry-miss throw — loud, and that is the point
        surfaced = f"{type(exc).__name__}: {exc}"
    else:
        surfaced = app.error_text()

    assert surfaced, (
        f"the agent acked {UNKNOWN_ACTION!r} and said nothing — a SILENT DROP, "
        "which e2e-conventions.md § convention 11 forbids outright. Downstream "
        "this reads as a product bug in whatever feature the next command "
        "belongs to: no error, no effect, and nothing naming the app that "
        "swallowed it. Fix the agent's unknown-action arm (it must write the "
        "app's agent-failure slot, the one the state serializer reads into "
        "`messages.error` ahead of any page error, or throw) — do not weaken "
        "this test."
    )
    assert UNKNOWN_ACTION in surfaced, (
        "the refusal must NAME the command it refused (convention 6: failures "
        f"diagnose themselves). Got {surfaced!r}, which tells the next session "
        "that something was refused but not what — and on a walk driving many "
        "commands that is nearly as unhelpful as silence. All seven agents "
        "already interpolate the action into their refusal text; keep it that way."
    )


@pytest.mark.windows
def test_the_refusal_slot_survives_a_nav_and_clears_at_reset(logged_in_app):
    """The slot's SHAPE, which the test above structurally cannot reach.

    `e2e-conventions.md` § convention 11's build-out record makes one demand of
    the refusal surface, and it is not "something was set":

        *a **dedicated, page-independent, nav-independent** slot cleared at
        `reset()`* … *"A pin must assert survival across the nav-clear and
        clearance at `reset`; tier_1 pins that only check 'the text is set' pass
        against both wrong versions (that is precisely how tui's first fix
        shipped believing itself correct)."*

    The wrong slot is neither hypothetical nor rare: **four apps shipped it** —
    tui, then android (which held it from 2026-07-19), then apple, then windows —
    each writing the refusal into a transient or page-scoped message the next
    navigation wipes. The test above cannot tell the two apart, because it reads
    the surface with no nav in between.

    So this drives the exact sequence the wrong slot fails: refuse, then issue the
    nav every action-layer helper issues, then read again. On windows the nav-clear
    is literal and easy to point at — `App.HandleTestCommand`'s `nav` block runs
    `_currentErrorMessage = null` on every `navigate_to`, and its `session` block
    does the same on every login — so a refusal left in the page mirror is gone
    microseconds after being set, while `_agentCommandFailure` is cleared in the
    `reset`/`logout` arms and nowhere else.

    **Why `windows`-marked rather than cross-app.** Not a platform difference in
    the rule — every bridge-backed agent owes exactly this — but in what one
    session can honestly claim. windows is the leg this was written for and the
    only app it has been run against (2026-08-29); tui, linux, android and apple
    each hold the dedicated slot with a unit-level pin of their own (android's
    `TestAgentRefusalSurfaceTest.kt`, apple's `AgentRefusalSlotTests.swift`), and
    windows can have no such twin — `FaunaApp.Tests` references `FaunaApp.Core`
    only, so neither `App` nor `TestAgent` is reachable from a C# unit test, which
    is why the property is pinned here at e2e level instead. A session on another
    machine that runs this against its own app should add that app's mark.
    """
    app = logged_in_app

    assert not app.has_error(), (
        "a pre-existing error would make every read below ambiguous. "
        f"Current text: {app.error_text()!r}"
    )

    app.driver.call_command(UNKNOWN_ACTION, {})
    refusal = app.error_text()
    assert UNKNOWN_ACTION in refusal, (
        f"the floor itself is broken — see the test above. Got {refusal!r}"
    )

    # The nav every action-layer helper issues, to a DIFFERENT page. This is the
    # step that separates a dedicated slot from a page-scoped one; a same-page nav
    # would pass against the wrong slot too.
    app.driver.navigate_to("settings")

    assert app.error_text() == refusal, (
        "the refusal did not survive a cross-page navigation — it is in a "
        "PAGE-SCOPED or transient slot, the wrong slot tui, android, apple and "
        "windows each shipped first (e2e-conventions.md § convention 11's "
        "build-out record). Every action-layer helper issues this nav, so in "
        "practice a refusal is wiped microseconds after being set and the driver "
        "reads error='' — silence wearing the disguise of a fixed bug. "
        f"Before nav: {refusal!r}; after: {app.error_text()!r}"
    )

    # A second hop, so a slot that merely survives ONE navigation (one cleared by
    # the page it leaves rather than the page it enters) is caught too.
    app.driver.navigate_to("feed")
    assert app.error_text() == refusal, (
        "the refusal survived one navigation but not two, so its lifetime is tied "
        f"to a page rather than to the test. After the second nav: "
        f"{app.error_text()!r}"
    )

    # `reset` is the per-test boundary every `app` fixture drives, and the only
    # sanctioned clear point. Without it a refusal leaks forward and reds a test
    # that did not cause it — which reads as a flake and gets the WRONG fix.
    app.driver.reset()
    assert not app.error_text(), (
        "the refusal outlived `reset`, so it will leak into the next test in the "
        "module and fail it for something it never did. `reset` must clear the "
        f"slot. Got {app.error_text()!r}"
    )
