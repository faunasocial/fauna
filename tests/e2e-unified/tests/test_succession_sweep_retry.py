"""tier_3 e2e: the affordance for finishing a post-succession group sweep that
did not finish — its render, and its press.

Goal docs: ``docs/goal/ui/settings.md`` § Recovery kit → *Finishing an
unfinished group sweep* (the button "renders on unfinished work alone" and
"must answer in words on every press"); ``docs/goal/behavior/succession-
aftermath.md`` § Propagation → *MLS groups* (what the sweep and its retry
actually do); ``docs/goal/architecture/e2e-conventions.md`` points 8, 11, 14, 15.

**Why this file exists.** ``recovery-kit-sweep-retry-button`` shipped on tui
2026-08-16 and on web 2026-08-27, and until this journey landed **no e2e
anywhere had ever rendered it or pressed it**. ``helpers/succession_retry.py``
rides every succession in both suites and asserts the render gate — but every
journey there took the gate's ABSENCE half, and structurally so: those actors
hold no groups, the ceremony's sweep still runs over a live engine, and a report
of ``ran`` over zero groups owes nothing. The one journey with a real group
sweeps it *completely*, which owes nothing either. So the whole user-facing half
of the retry — that the button appears when work is owed, that pressing it
drives ``retry_group_sweep``, and that the sweep then finishes — was unproven
end-to-end, its two boundaries unit-pinned and nothing spanning app → press →
answer.

**How the owing arm is staged, and why it has to be staged at all.** A sweep
owes work only when it fails, never runs, or re-points some groups and not
others. None of those happen against a healthy nest, so the fault is injected at
the nest — ``channel_refusal_test_hook.rs``, ``--features test-hooks``, compiled
out of every production build (convention 15). The class it refuses is
load-bearing and the Rust module carries the full reasoning; the short version:

    the sweep's three per-channel wire steps are
      1. add-successor      (a Commit,      authored by the OLD leaf)
      2. the in-group succession statement (an Application, by the SUCCESSOR)
      3. remove-old         (a Commit,      by the SUCCESSOR)

    Refusing (1) is the WRONG fault: ``commit_add_successor`` merges the add
    into the old engine locally *before* publishing, so a refusal there strands
    the old engine an epoch ahead with no way to re-author — the retry answers
    ``NeedsMemberReAdd``, which is the member-side remedy, not a sweep it can
    finish. Refusing (2) is the right one: the add landed, the successor joined,
    and ``sweep_one`` aborts the group before remove-old *by design*, leaving
    exactly the resumable state the retry's own branch was written for.

So this file refuses **Application** envelopes on the group's channel for the
duration of the ceremony, and clears the refusal before the press.

⚠ **The press is a real UI gesture on a real report** (convention 8): nothing
here pokes app state or parks a view. The button is on screen because the nest
really refused a real commit, and it will go away only when a real retry really
re-points the group (the finishing arm at the end of the body).

⚠ **The finishing arm was unreachable on the ceremony's own device until
2026-08-27 — twice.** The first two runs found every press refused with "served
in another instance of this app" — no other instance anywhere: the retired
identity's `mls_state.db` was still open in the same process after the account
switch, held by a strong `Arc` cycle through the succession witness and by three
background loops that released only at their next tick. That is fixed and pinned
(`account-scoping.md` § Implementation status → the `tui (in-memory)` ledger
row), and this journey asserts the refusal is gone. The very first press past it
met `CannotRemoveSelf`: the successor's own launch restored the PREDECESSOR's
openMLS provider over the group it had just joined, seating it as the old leaf.
Ruled the same day — a `provider` seated under another identity's leaf never
restores into an engine, and the ceremony device publishes the successor's own
before the switch (`succession-aftermath.md` § Re-key scope → *What a
successor's replica restore may take from a predecessor's*) — so the tail of
the body finishes: one press, every group re-pointed, the button retired, the
wire grown.

⚠ **One thing this journey does NOT cover, deliberately.**

*The `NoOldState` arm* — a device holding no conversation store for the retired
identity answers in a sentence rather than sweeping. Its natural home is **web**,
where that is the permanent answer on every press (no browser can hold the
retired identity's MLS state — ``libs/fauna-wasm/src/succession.rs``
``succession_sweep_retry``), not a staged accident. Do not "cover" it here by
asserting an error string this journey's arm never produces.

tier_3: needs a real ``fauna-nest`` binary built with ``test-hooks`` (the e2e
default — ``tests/common/nest.py::build_node``). tui led; **linux joined
2026-09-01**, the second app whose press can finish a sweep — both are
Rust-native and drive the shared ceremony in-process. **windows joined
2026-09-20** as the third, through the ``succession_retry_group_sweep`` UniFFI
face rather than in-process, which reaches the same shared ceremony; **macos and
ios joined 2026-09-21** through that same face, one shared FaunaKit leg serving
both apple targets, and web can only ever answer in words.
"""
from __future__ import annotations

import pytest

from helpers.channel_refusal import (
    APPLICATION,
    allow_channel_envelopes,
    refuse_channel_envelopes,
)
from helpers.budgets import MLS_COMMIT_FOLD_S
from helpers.succession_ceremony import SUCCESSION_AND_RELAUNCH_S, settled_actor_id
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from helpers.waiting import wait_until

# The markers are authoritative over the `app` parametrization (the collection
# hook's marker rule), so this journey only lands on drivers whose press can
# actually finish a sweep. **linux joined 2026-09-01** — it consumes
# `fauna_client_recovery::ceremony::retry_sweep_as_successor` in-process exactly
# as tui does, over its own two store paths, so the whole body below (a real
# group, a staged partial sweep, a real press that finishes it) is reachable
# there. **windows joined 2026-09-20** — its press
# reaches the same ceremony through the `succession_retry_group_sweep` UniFFI
# face (`RecoveryKitViewModel.RetrySweepAsync` → `NestRpcClient.
# SuccessionRetryGroupSweepAsync`), over the pure retired-store resolver
# `RetiredIdentityStorePath` (the windows twin of apple's), so the finishing arm
# — not merely web's answer-in-words arm — is reachable there too.
# **macos and ios joined 2026-09-21** — one shared FaunaKit leg for both apple
# targets, taking the very face windows took: `RecoveryKitSection`'s
# `recovery-kit-sweep-retry-button` is `.automationActivate`-registered under the
# same `owesWork` gate and calls `RecoveryKitVM.retrySweep()` →
# `APIClient.successionRetryGroupSweep()` → the `succession_retry_group_sweep`
# UniFFI face over the pure retired-store resolver `RetiredIdentityStorePath`
# (`Core/SuccessorStorePath.swift`), so the finishing arm is reachable on apple
# too; the report the gate reads is serialized as `data.succession_sweep` by
# `Testing/AppStateObservables.swift`. Not a declared absence for the rest: web
# joins as its leg lands, and the file's ⚠ note above says which arm it covers.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]

_SECRET_HEX_LEN = 64

# How long the retry button may take to leave the screen once the report it
# renders from says every group is re-pointed: a re-entered section finishing
# its render, no network. Sized like the retry affordance budget, generous
# against a loaded box; a green run pays nothing for the ceiling.
_RETRY_RETIRE_S = 20.0


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_a_half_done_sweep_renders_the_retry_and_the_press_is_honoured(
    ungranted_app, nest_instance
):
    """A sweep the ceremony could not finish puts the button on screen, and
    pressing it is honoured rather than dropped.

    Three things only a full-stack run can establish, none of them reachable
    from the unit pins (``libs/fauna-client-recovery/tests/group_sweep.rs``
    necessarily fakes the nest, and the app-side render gate is pinned against a
    hand-built ``SweepStatus``):

    1. **A partial sweep really renders the button.** The report the app paints
       from is the one a real nest refusal produced, not a constructed one —
       and until this journey landed, every succession in the suite took the
       gate's *absence* half, so the presence half had never once run.
    2. **The press really reaches the app's own path and answers.** It crosses
       the action, the op and the shared ceremony driver, and comes back with
       one of the two witnesses an answer can take (a sentence on
       ``error-message``, or a refreshed report) — never silence, which is what
       convention 11 forbids.
    3. **The press never moves the report backwards.** ``sweep_groups`` is
       idempotent over finished groups; a retry may finish work or fail to, and
       may never undo it.

    4. **The press runs on the ceremony's own device.** It is not refused by
       this process's own stale lock on the retired identity's store — the bug
       this journey found on its first run, fixed 2026-08-27 (module
       docstring) — nor seated as the old leaf by its own launch's replica
       restore, the bug the first unrefused press found (fixed the same day).
    5. **One press finishes the sweep.** The refreshed report reads every group
       re-pointed, the button retires, and a member's channel fetch has grown
       past the pre-ceremony count — the re-posted statement and the remove-old
       commit really left the app.

    ⚠ The post-conditions are read as STATE (``data.succession_sweep``): the
    counts say whether the press re-pointed every group, and every app that
    runs the retry publishes them. The sweep's two lines carry ids since
    2026-09-25, and what they SAY is witnessed once, over the ceremony, by
    ``test_identity_succession_ceremony.py::test_the_ceremony_tells_you_what_it_
    swept_and_whom_it_cannot_vouch_for`` — this journey is about the press.
    """
    from tests.api import conv_api
    from tests.api.conv_api import inbox as _inbox
    from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

    app = ungranted_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Skip-gate FIRST, before any nest-side setup: an app without the succession
    # driver must report "not built" rather than fail on a real backend it also
    # does not have.
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    app.settings.open_recovery_kit_or_skip()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the pre-ceremony actor id must render; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    # The REAL conversations backend, for the sibling journey's reason: the
    # ceremony takes the old engine off the live session, so without it the
    # sweep reports NoEngine and the group below would not exist at all.
    app.conversations.enable_real_faunamls()

    # An API-tier peer with REAL key packages — the real backend parses each one
    # during bootstrap, so fake byte strings yield no group.
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
    # ran against a healthy nest and only the sweep meets the refusal.
    refuse_channel_envelopes(port, channel, APPLICATION)
    try:
        # ── the theft remedy, exactly as the sibling journeys drive it ───────
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
            f"journey would silently re-assert the ABSENCE half; got {sweep!r}"
        )
        assert sweep.get("old_leaf_removed_everywhere") is False, (
            f"a partial sweep must not claim it re-pointed everything; {sweep!r}"
        )
        assert sweep_owes_work(sweep), (
            f"the staged report must owe work — the render gate reads it; {sweep!r}"
        )
    finally:
        # Clear before the press (and before any failure exits) — the retry's
        # whole subject is that the *same* gesture succeeds once the flake is
        # gone, and a still-armed nest would make a red here ambiguous.
        allow_channel_envelopes(port, channel)

    # ── the button is on screen, and pressing it answers ─────────────────────
    # The shared gate helper: asserts PRESENT (its dead branch until today),
    # presses, and requires one of the two witnesses an answer can take.
    assert_the_retry_affordance_matches_the_sweep(app, sweep)

    # ── the press RAN on the ceremony's own device ───────────────────────────
    # The whole subject: until 2026-08-27 every press here answered
    # "served in another instance of this app" — the retired identity's engine
    # had outlived the account switch, and the retry's finishing arm was
    # unreachable on the one device it exists for. That sentence is the
    # ratified answer to a genuinely second instance, so what this device owes
    # is its ABSENCE: the retry opened the retired store and ran.
    assert "served in another instance" not in app.error_text(), (
        "the retired identity's engine outlived the account switch again — the "
        "press was refused by this process's own stale lock on the retired store "
        f"(account-scoping.md, the tui (in-memory) ledger row): {app.error_text()!r}"
    )

    # ── one press finished the job — the ceremony device's retry, reinstated ─
    # The tail below is the one landed and later narrowed
    # into a ⛔ block when the first unrefused press met `CannotRemoveSelf`: the
    # successor's launch had restored the PREDECESSOR's openMLS provider over
    # the group it had just joined, seating it as the old leaf. That is ruled
    # now — a provider seated under another identity's leaf never restores
    # (`succession-aftermath.md` § Re-key scope → *What a successor's replica
    # restore may take from a predecessor's*) — and the retry finishes.
    # ── one press finished the job ───────────────────────────────────────────
    # The helper pressed exactly ONCE and returned on the first witness of an
    # answer. The sweeping arm's answer is the refreshed report (`Swept` renders
    # no sentence), so what is left is that the report it refreshed to is the
    # FINISHED one. Read as a deadline poll rather than a single read: the
    # report is written when the op returns, and the helper's witness may have
    # been an earlier refresh of the same press.
    def _finished() -> bool:
        after = app.driver.get_state("data.succession_sweep") or {}
        return (
            after.get("groups_old_leaf_removed", -1) == after.get("groups", 0)
            and after.get("old_leaf_removed_everywhere") is True
        )

    wait_until(
        _finished,
        MLS_COMMIT_FOLD_S,
        diagnose=lambda: (
            "one press on the ceremony's own device did not finish the sweep: "
            f"report={app.driver.get_state('data.succession_sweep')!r} "
            f"answer={app.error_text()!r}. A 'served in another instance' answer "
            "here means the retired identity's engine outlived the account switch "
            "again (account-scoping.md, the tui (in-memory) ledger row)"
        ),
    )
    after = app.driver.get_state("data.succession_sweep")
    assert after.get("groups") == sweep.get("groups"), (
        "the retry changed how many groups the sweep is about — the retired "
        f"identity's group set is a fact, not a result; was {sweep!r}, now {after!r}"
    )
    assert not app.error_text().strip(), (
        "a finishing press answers through the report, never a sentence; got "
        f"{app.error_text()!r}"
    )

    # ── the button retires with the work ─────────────────────────────────────
    # The render gate is a pure function of the report (settings.md § Recovery
    # kit → *Finishing an unfinished group sweep*): fully re-pointed ⇒ nothing
    # left to finish ⇒ no button. Re-entered, not assumed still on screen.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    wait_until(
        lambda: app.driver.count("recovery-kit-sweep-retry-button") == 0,
        _RETRY_RETIRE_S,
        diagnose=lambda: (
            "recovery-kit-sweep-retry-button is still on screen after the sweep "
            f"finished — offering to re-run a pass that completed; report={after!r}"
        ),
    )

    # ── and the wire corroborates it ─────────────────────────────────────────
    # The retry's re-posted statement and its remove-old commit are envelopes
    # on the group's channel; a member's fetch growing past the count captured
    # BEFORE the ceremony is the proof they really left the app, independent
    # of anything the app reports about itself.
    wait_until(
        lambda: len(conv_api.channel_fetch(port, bob, channel, after=0))
        > envelopes_before,
        MLS_COMMIT_FOLD_S,
        diagnose=lambda: (
            f"the channel still holds {len(conv_api.channel_fetch(port, bob, channel, after=0))} "
            f"envelopes, the {envelopes_before} it held before the ceremony — the retry "
            "reported a finish that never reached the nest"
        ),
    )
