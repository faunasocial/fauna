"""tier_3 e2e: web's permanent answer to the post-succession group-sweep
retry — pressing it always says "no old state" rather than sweeping.

Goal docs: ``docs/goal/ui/settings.md`` § Recovery kit → *Finishing an
unfinished group sweep* (the button "renders on unfinished work alone" and
"must answer in words on every press"); ``docs/goal/architecture/e2e-
conventions.md`` points 8, 11, 14, 15, 17.

**Why this is a separate file from tui's.** ``test_succession_sweep_retry.py``
proves the button's render gate and its FINISHING arm — a press on the
ceremony's own device that really re-points the group. tui is the only app
today whose press can do that; the module's own ⚠ note names the arm it
deliberately leaves uncovered: "a device holding no conversation store for the
retired identity answers in a sentence rather than sweeping. Its natural home
is web, where that is the permanent answer on every press … not a staged
accident." This file is that leg.

**Why "permanent" is provable without a race.** No browser ever holds the
retired identity's MLS state (it rests in the nest replica, behind bearers the
succession revoked), so web's press handler
(``libs/fauna-wasm/src/succession.rs::succession_sweep_retry``) does not even
attempt to look for it — it is a hard-coded ``SweepRetryAnswer::NoOldState``,
no round trip, no branch. That is exactly what makes the answer deterministic
here rather than a timing-dependent "sometimes web can't find the store": it
is unconditional code, the same code on every press, forever (convention 14 —
nothing here is asserted against wall-clock behavior).

**Staging the owing sweep is identical to tui's leg.** The render gate is a
pure function of the CEREMONY's own sweep report — which for web is whatever
``sweep_as_successor_web`` produced *during the ceremony itself*, using the
old identity's conversations session that was live in this same browser tab
seconds before the switch. That is a different question from what the RETRY
can reach afterward, which is why the same nest fault (refusing one
Application envelope on the group's channel, ``channel_refusal_test_hook.rs``)
that staged tui's partial sweep stages web's — ``helpers/channel_refusal.py``
lives outside either journey file for exactly this reuse.

⚠ **This file does NOT cover the finishing arm.** Asserting a fix here would
require web to reach the retired identity's own MLS state, which the module
comment above says never happens — pinning that would either wait forever or
paper over the real answer with a poke. tui's file owns that half.

tier_3: needs a real ``fauna-nest`` binary built with ``test-hooks`` (the e2e
default — ``tests/common/nest.py::build_node``).
"""
from __future__ import annotations

import pytest

from helpers.channel_refusal import (
    APPLICATION,
    allow_channel_envelopes,
    refuse_channel_envelopes,
)
from helpers.succession_ceremony import SUCCESSION_AND_RELAUNCH_S, settled_actor_id
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

_SECRET_HEX_LEN = 64

# The words-answer arm is local and instantaneous (no round trip — see the
# module doc), but the shared assertion helper still polls; a short, generous
# ceiling against a loaded box, not a tuned race window.
_RETRY_ANSWER_S = 20.0

#: A stable, distinguishing fragment of `sweep_retry_no_old_state` rather than
#: the full sentence — proves this is THAT arm specifically (not merely "some
#: sentence arrived") without pinning wording the copy owns.
_NO_OLD_STATE_FRAGMENT = "does not have the conversation history"


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_a_half_done_sweep_answers_no_old_state_on_web(ungranted_app, nest_instance):
    """A sweep the ceremony could not finish puts the button on screen, and
    web's press always answers "no old state" — never silence, never a sweep.

    Three things only a full-stack run can establish, mirroring tui's file:

    1. **A partial sweep really renders the button on web too** — the render
       gate is app-agnostic (a pure function of the report), but until this
       journey landed no run had ever produced the owing arm on web either.
    2. **The press really reaches web's own path and answers** — crosses the
       wasm boundary and comes back with the specific NoOldState sentence,
       never silence (convention 11) and never a refreshed report (that would
       mean web somehow swept, which the module doc says cannot happen).
    3. **The press changes nothing.** No round trip means no wire growth and
       no report mutation — asserted here as the negative image of tui's
       "one press finished the job", proving this arm is genuinely inert
       rather than merely untested.
    """
    from tests.api import conv_api
    from tests.api.conv_api import inbox as _inbox
    from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

    app = ungranted_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Skip-gate FIRST, before any nest-side setup: an app without the
    # succession driver must report "not built" rather than fail on a real
    # backend it also does not have.
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    app.settings.open_recovery_kit_or_skip()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the pre-ceremony actor id must render; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    # The REAL conversations backend: the ceremony sweeps the old engine off
    # the live session, so without it there is no group for the fault to land
    # on and the sweep would report NoEngine before the ceremony ever ran.
    app.conversations.enable_real_faunamls()

    # An API-tier peer with REAL key packages — the real backend parses each
    # one during bootstrap, so fake byte strings yield no group.
    bob = conv_api.reachable_peer(port, admin_sk, old_actor)

    # The group, through the app's own recipient resolution — convention 8.
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "before the theft")
    thread = _wait_thread_snippet(app, "before the theft", timeout=30.0)
    assert thread is not None, (
        "the group must exist before the ceremony — the sweep needs something "
        f"to leave half-done; error: {app.error_text()!r}"
    )
    channel = thread.channel_id_hex
    assert channel, "the thread must be bound to a channel after the send"
    assert len(_inbox(base, bob)) >= 1, "a real Welcome should be delivered to bob"
    envelopes_before = len(conv_api.channel_fetch(port, bob, channel, after=0))
    assert envelopes_before >= 1, "the Application envelope should be on the channel"

    # ── stage the fault: the statement cannot be told to the members ─────────
    # Armed AFTER the group is built, so every step that makes the group real
    # ran against a healthy nest and only the sweep meets the refusal — the
    # identical fault tui's file stages, for the identical reason (its own
    # module doc owns the "why the middle envelope" reasoning).
    refuse_channel_envelopes(port, channel, APPLICATION)
    try:
        app.settings.navigate()
        app.settings.open_recovery_kit()
        app.settings.create_recovery_kit()
        app.wait_for("recovery-kit-secret-display", timeout=30.0)
        held = app.settings.recovery_kit_secret()
        assert len(held) == _SECRET_HEX_LEN, (
            f"the kit is 64-hex, got {len(held)}; error: {app.error_text()!r}"
        )

        app.settings.navigate()
        app.settings.open_recovery_kit()
        app.settings.succeed_identity_with_held_kit(held)

        wait_until(
            lambda: settled_actor_id(app) not in ("", old_actor),
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: (
                f"still reads actor={settled_actor_id(app)!r} (old={old_actor!r}) "
                f"sweep={app.driver.get_state('data.succession_sweep')!r} "
                f"error={app.error_text()!r}"
            ),
        )

        # ── the sweep's own account: it ran, and it fell short ───────────────
        sweep = app.driver.get_state("data.succession_sweep")
        assert sweep is not None, (
            "no sweep ran during the ceremony — the retry's render gate is a "
            "function of that report, so its absence is a broken ceremony"
        )
        assert sweep.get("status") == "ran", (
            "the sweep must have RUN and fallen short, not failed to start: a "
            "'no_engine'/'failed' report owes work for a different reason and "
            f"would prove nothing about the partial arm; got {sweep!r}"
        )
        assert sweep.get("groups", 0) >= 1, (
            f"the sweep saw no groups, so nothing could be left owing; {sweep!r}"
        )
        assert sweep.get("groups_old_leaf_removed", 0) < sweep.get("groups", 0), (
            "the nest refusal did not leave the sweep partial — every group was "
            "re-pointed, so there is nothing for the retry to finish and this "
            f"journey would silently prove nothing; got {sweep!r}"
        )
        assert sweep_owes_work(sweep), (
            f"the staged report must owe work — the render gate reads it; {sweep!r}"
        )
    finally:
        # Clear before the press (and before any failure exits) — an armed
        # nest during the press would leave a NoOldState answer ambiguous
        # (was it the identity, or the still-refused wire?).
        allow_channel_envelopes(port, channel)

    # Captured HERE, not before the ceremony: the ceremony's own doomed sweep
    # attempt already grew the channel by one (add-successor's Commit, step 1
    # of the three-step wire sequence, is NOT refused — only the Application
    # in step 2 is, per the module doc's "why the middle envelope" reasoning).
    # The press-changed-nothing check below is about the PRESS, so its
    # baseline must be taken after the ceremony settles, not before it starts.
    envelopes_before_press = len(conv_api.channel_fetch(port, bob, channel, after=0))

    # ── the button is on screen, and pressing it answers ─────────────────────
    # The shared gate helper: asserts PRESENT (owing work), presses, and
    # requires one of the two witnesses an answer can take. On web the answer
    # is always the sentence half — asserted specifically below.
    assert_the_retry_affordance_matches_the_sweep(app, sweep)

    # ── the answer is SPECIFICALLY no-old-state, not merely "some sentence" ──
    error = app.error_text()
    assert _NO_OLD_STATE_FRAGMENT in error, (
        "web's press must answer the specific no-old-state sentence — no "
        "browser ever holds the retired identity's MLS state, so any other "
        f"answer (or none) means the press took a path it should not have "
        f"reached; got error_text={error!r}, expected fragment "
        f"{_NO_OLD_STATE_FRAGMENT!r} (full copy: "
        f"{S.settings.recovery_kit.sweep_retry_no_old_state!r})"
    )

    # ── the press changed NOTHING — no round trip, so no mutation anywhere ───
    # The negative image of tui's "one press finished the job": web's answer
    # is a local, hard-coded projection (module doc), so the report must be
    # byte-identical to what the ceremony staged, and no envelope may have
    # reached the channel.
    after = app.driver.get_state("data.succession_sweep")
    assert after == sweep, (
        "web's press must not mutate the sweep report — it never touches the "
        f"old engine at all; was {sweep!r}, now {after!r}"
    )
    envelopes_after = len(conv_api.channel_fetch(port, bob, channel, after=0))
    assert envelopes_after == envelopes_before_press, (
        "web's press must reach the nest for nothing — the channel gained "
        f"{envelopes_after - envelopes_before_press} envelope(s) it should not "
        f"have (pre-ceremony count was {envelopes_before!r}, distinct from the "
        "press-time baseline on purpose — the ceremony's own doomed sweep "
        "attempt already grew the channel once, via the add-successor Commit "
        "that is not refused)"
    )

    # ── and the button stays on screen: nothing was fixed, so nothing retired
    app.settings.navigate()
    app.settings.open_recovery_kit()
    wait_until(
        lambda: app.driver.count("recovery-kit-sweep-retry-button") >= 1,
        _RETRY_ANSWER_S,
        diagnose=lambda: (
            "recovery-kit-sweep-retry-button disappeared after an answer that "
            "fixed nothing — the render gate must still read the same owing "
            f"report; sweep={sweep!r}"
        ),
    )
