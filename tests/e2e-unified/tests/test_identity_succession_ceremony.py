"""tier_3 e2e: taking an account back from a stolen identity secret.

Goal docs: ``docs/goal/behavior/identity-succession.md`` § Enforcement on the
home nest (the one atomic transaction) and § Propagation → *Own device fleet*;
``docs/goal/ui/settings.md`` § Recovery kit (placement, the type-to-confirm
gate, which state enables what).

This is the *driving* half of succession. Its sibling
``test_identity_succession_refusal.py`` covers what every OTHER device sees —
it mints its succession from a test-only fixture signer and then proves the
refusal routes a relaunched app to the import screen. Nothing there touches a
button, so nothing there can tell you whether a **user** can reach the remedy
at all. That is what this file pins, and why it is worth a full-stack run
rather than more unit tests:

- **The ceremony is reachable from the UI and lands on a real nest.** The unit
  tests cover the gate, the projection and the shared composition; only a
  running ``fauna-nest`` proves the app sends a chain-valid statement rather
  than a well-formed one.
- **The app comes back up as the successor, on its own.** The succession
  transaction revokes the driving session's own bearers, so the moment it
  succeeds the app is signed in as a key the nest refuses. Recovering from
  that — persist the successor seed, switch, re-launch — is the step that
  turns "the statement landed" into "the user has their account back", and it
  crosses the ceremony, the account registry and the launch machine. No unit
  test spans those three.
- **The handle comes with it.** The handle moves inside the nest transaction,
  so a successor that could not be reached at the old handle would mean the
  remedy cost the user their name — the thing they would least accept losing.
- **The ceremony's closing act really runs — the successor is minted a fresh
  kit and SHOWN it.** The old kit retires with the old identity and the
  succession transaction deletes the old escrow row, so from the moment the
  statement lands until this mint the account has no kit and no escrow at all
  (§ The RecoveryKey → *At succession*, ratified 2026-08-03). The tier_1
  chain — ``the_successors_owed_kit_is_minted_unbidden_and_lands_where_it_
  renders`` and its three siblings — proves the flag survives the switch and
  the post-auth hook navigates, but it necessarily fakes the nest, so it can
  say nothing about whether the mint *landed*, whether the same-ceremony
  escrow put landed with it, or whether the secret reached the screen. An
  unshown mint is the sharp failure: it registers a kit **nobody holds**,
  which is strictly worse than never-created.

Convention 14 throughout: every assertion is on latency-independent state
behind a named generous budget, never a settle-sleep. The one genuinely
asynchronous step — the app tearing down one session and launching another —
is anchored by polling for the *state change itself* (the actor id the Account
page renders), not by waiting a fixed time and hoping.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app whose UI has the
section — today tui, the lead app; the others join as their legs land.
"""
from __future__ import annotations

import json
import re
import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.succession_ceremony import (
    STOLEN_GATE_REVEAL_S,
    SUCCESSION_AND_RELAUNCH_S,
    wait_for_successor_actor,
    closing_act_console,
    kit_on_screen,
    require_stolen_gate,
)
from helpers.succession_retry import assert_the_retry_affordance_matches_the_sweep
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_SECRET_HEX_LEN = 64

# The member-side budget: after alice's ceremony reports it ran, bob still has
# to poll his channel, decode the statement, and settle it through his witness
# (tier 1 off the harvested head — no dial — or tier 2's 15 s-bounded round
# trip if his harvest sweep has not reached alice yet, in which case a later
# sweep tick plus a re-delivery is what converges). Sized far above any of
# those on a loaded box; a green run pays nothing for the ceiling.
_MEMBER_REPOINT_S = 120.0

# How long bob's peer-anchor harvest sweep may take to reach a nameless roster
# row that is already rendering. The sweep re-diffs the thread snapshot on a
# 5 s cadence and the fetch is one `fauna.profile.get` on his own connection,
# so this is ~24 cadences. Sized in the same generous class as the two budgets
# above deliberately: this wait is the *ordering barrier* the harvest race
# needs (see the test's ⚠ ordering note), and on a green run it costs nothing.
# A red HERE is a genuinely late harvest, which is a product finding with its
# own fix — the point of asserting it separately from the re-point below.
_HARVEST_ANCHOR_S = 120.0

@pytest.mark.feature("take-your-account-back")
def test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor(
    ungranted_app, nest_instance
):
    """create a kit → "my identity was stolen" → signed in as the successor.

    Uses ``ungranted_app`` — a dedicated fresh actor — for a sharper version of
    the reason ``test_recovery_kit_settings.py`` does: this ceremony does not
    merely write to the account's registration chain, it **re-points the whole
    account and revokes every session of it**. Driven against the shared
    session ``test_user`` it would sign every other test out.
    """
    app = ungranted_app
    # account-actor-id renders on the Status landing (settings.md § Live-data
    # placement), not the Account sub-page open_recovery_kit() navigates into
    # below — the two sub-pages are NOT interchangeable (test_settings.py's
    # test_actor_id_visible), so the read must happen before that navigation.
    # `_navigate_subpage("status")`, not plain `.navigate()`: iOS's idiomatic
    # settings nav lands a plain navigate on the root page *list*, not the
    # Status sub-page (test_settings.py::test_actor_id_visible).
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        "the Status page must render the signed-in actor id before the "
        f"ceremony, so the switch is observable; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    app.settings.open_recovery_kit_or_skip()

    # The kit is the whole authorization: the thief holds the seed, so what
    # makes this remedy work is the credential they do NOT have.
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )

    # Re-enter the page so the ceremony runs from a clean render — and so the
    # gate's own visibility is checked on a section that is merely registered,
    # not one still showing a freshly minted secret.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)

    # The gate is a SECOND condition, not a replacement for the status one: an
    # unarmed field must leave the irreversible action out of reach.
    assert not app.is_enabled("identity-stolen-button"), (
        "the type-to-confirm gate starts unarmed, so one stray click cannot "
        f"re-point an account; error surface: {app.error_text()!r}"
    )

    app.settings.succeed_identity_with_held_kit(held)

    # ── The ceremony's closing act (§ The RecoveryKey → *At succession*).
    #
    # Read FIRST, and with no navigation of our own: entering Account clears
    # any kit on screen (the shown-once custody rule), so the actor-id poll
    # below — which re-navigates on every tick — would wipe the very evidence
    # this is looking for. The property is "the user was SHOWN a kit", not
    # merely "a kit was registered": an unshown mint leaves a kit nobody
    # holds, whose only route back is the 30-day seed-alone window.
    wait_until(
        lambda: len(kit_on_screen(app)) == _SECRET_HEX_LEN,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no kit on screen for the successor (reads {kit_on_screen(app)!r}), "
            f"error={app.error_text()!r}{closing_act_console(app)}"
        ),
    )
    assert kit_on_screen(app) != held, (
        "the successor must mint a FRESH RecoveryKey — the old one retired "
        "with the old identity (§ The RecoveryKey), so re-showing it would "
        "leave the account guarded by a key this ceremony just retired"
    )

    # The ceremony killed this session and the app is re-launching as the
    # successor. Anchor on the state change itself — the Account page rendering
    # a DIFFERENT actor id, then a live session as it — rather than on elapsed
    # time.
    new_actor = wait_for_successor_actor(app, old_actor)
    assert new_actor != old_actor, (
        "the account must belong to a freshly minted identity; the app is "
        "still signed in as the key the thief holds"
    )
    assert len(new_actor) == _SECRET_HEX_LEN, (
        f"the successor id is a 64-hex actor id, got {new_actor!r}"
    )

    # The successor's kit state is honest — and honest here means the section
    # reports the protection the closing act just restored, read back off the
    # account rather than off the outcome that minted it.
    #
    # ⚠ This asserted the OPPOSITE until 2026-08-10, and the history is worth
    # keeping: the assertion was written 2026-08-02, when a succession left the
    # successor kitless and merely warned. § The RecoveryKey → *At succession*
    # ratified mint-and-show the next day and tui built it the same day, which superseded that shape — and left this test asserting
    # the retired one. Read the § before "restoring" a never-created expectation
    # here: a successor that reads never-created now means the closing act did
    # not run.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    status = app.settings.recovery_kit_status()
    assert status == S.settings.recovery_kit.status_registered, (
        "the successor's kit must be registered AND the new seed escrowed — "
        "the no-escrow variant would mean the registration landed while the "
        "same-ceremony escrow put did not, leaving recovery-by-phrase "
        f"unavailable on a fresh account; reads {status!r}"
    )
    assert not app.is_enabled("recovery-kit-create-button"), (
        "a kit is registered for the successor now, so create is spent — "
        f"offering it again would open the wrong ceremony; status reads {status!r}"
    )
    assert not app.settings.recovery_kit_secret(), (
        "the kit is shown ONCE (§ The RecoveryKey — *Custody*): re-entering the "
        "section must not re-display the secret the ceremony just minted, and "
        "the successor's identity seed is persisted rather than displayed, so "
        "nothing here may be mistaken for a recovery kit either"
    )

    # ⚠ **MEASURED 2026-08-27: this journey asserts the ABSENCE half**, not the
    # presence half it was expected to. Its actor holds no groups, and the
    # ceremony's sweep still runs over a live engine, so the report is
    # `status='ran' groups=0` — which owes nothing, so no button. The comment
    # here previously predicted `no_engine`; the run refuted it, and the
    # prediction is left recorded rather than quietly deleted because the same
    # wrong guess is what made the gap below invisible for a session.
    #
    # ⚠ So the button's RENDER and its PRESS are covered by nothing, on any
    # app — no journey in this suite produces an owing sweep, and staging one
    # needs a partial sweep (≥2 groups, one failing) or fault injection. What the call below still buys is
    # real: no app may paint a STANDING retry button over a completed sweep.
    sweep = app.driver.get_state("data.succession_sweep")
    assert sweep is not None, (
        "the ceremony reported no sweep at all — the retry's render gate is a "
        "function of that report, so its absence is a broken ceremony, not a "
        "quiet no-op"
    )
    assert_the_retry_affordance_matches_the_sweep(app, sweep)


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_the_ceremony_re_points_a_real_group_and_evicts_the_stolen_leaf(
    ungranted_app, nest_instance
):
    """A group the user really holds is really re-pointed by the button press.

    The sibling test above proves the *account* comes back. It cannot say
    anything about the user's groups, because its actor holds none — the sweep
    runs inside the ceremony and reports zero outcomes, which renders nothing
    and asserts nothing. That gap is the whole reason this test exists: without
    it, "the thief keeps reading your group conversations" is a regression the
    suite would not notice.

    What only a full-stack run can establish (the unit proof
    ``libs/fauna-client-recovery/tests/group_sweep.rs`` necessarily fakes the
    nest, and three engines in one process cannot exercise any of it):

    - **The group the app actually holds is the one that gets swept.** The
      group is bootstrapped through the app's own real ``FaunaMlsBackend`` —
      real key packages, a real Welcome, a real channel — so the sweep
      enumerates it off the live engine the signed-in session is holding, which
      is the only engine a real user has.
    - **A real nest accepts the sweep's commits from a non-member sender.** The
      successor is not yet a member when it publishes the old leaf's removal;
      that this works at all rests on the auto-register transport ruling
      (``identity-succession.md`` § Implementation status today). It is pinned
      in isolation by ``test_mls_channels.py``; here it is exercised by the
      ceremony that depends on it, against a live nest.
    - **The engines survive the account switch.** The sweep persists both, and
      the switch tears the pair down moments later.

    ⚠ **The post-condition here is read as STATE**: ``data.succession_sweep``,
    the machine-readable twin of the sweep's two lines (pinned tui-side by
    ``settings::tests::the_sweep_state_carries_the_same_two_facts_as_its_lines``).
    It says what HAPPENED, as counts, and every app that runs the ceremony
    publishes it — which is why this journey stays the group-eviction witness on
    all of them. What the user is TOLD — the two lines themselves, on their own
    ids since 2026-09-25 — is witnessed by the sibling
    ``test_the_ceremony_tells_you_what_it_swept_and_whom_it_cannot_vouch_for``,
    over this same ceremony (``_ceremony_over_a_real_group``).

    ⚠ **What is asserted is eviction of the succeeded credential, and nothing
    wider.** ``old_leaf_removed_everywhere`` answers exactly that
    (``identity-succession.md`` § Propagation → *MLS groups*); there is
    deliberately no combined "the user is safe" verdict, because a leaf the
    thief seated under a second identity survives this ceremony untouched. An
    assertion phrased as "the account is now safe" would be re-introducing the overclaim the report's own shape refuses.
    """
    _ceremony_over_a_real_group(ungranted_app, nest_instance)


def _ceremony_over_a_real_group(app, nest_instance) -> dict:
    """Run the theft remedy over a group the app really holds; return the sweep.

    The body of ``test_the_ceremony_re_points_a_real_group_and_evicts_the_stolen_
    leaf`` (its docstring is the why), shared with the on-screen witness below
    so the two cannot drift onto different ceremonies. Every assertion here —
    the sweep ran over a live engine, evicted the old leaf from every group, the
    retry gate matches, and the nest saw the commits — holds for both callers.
    """
    from tests.api import conv_api
    from tests.api.conv_api import inbox as _inbox
    from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Skip-gate FIRST, before any nest-side setup: an app without the succession
    # driver must report "not built" rather than fail on a real backend it also
    # does not have. account-actor-id is Status-page-only, so it is read on the
    # landing before open_recovery_kit() navigates away (see the sibling test).
    # `_navigate_subpage("status")`, not plain `.navigate()` — see the sibling
    # test's comment (iOS lands a plain navigate on the root page list).
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    app.settings.open_recovery_kit_or_skip()
    require_stolen_gate(app)
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the pre-ceremony actor id must render; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    # The REAL conversations backend is not decoration here: the ceremony takes
    # the old engine off the live session (`app.conversations.real_session`), so
    # without it the sweep reports NoEngine and this test would prove nothing.
    # The assertion on `status == "ran"` below is what keeps that honest.
    #
    # Deliberately NOT paired with a `disable_real_faunamls()` teardown, unlike
    # the `real_faunamls_app` fixture. Two reasons, and the second is why a
    # teardown here would be worse than none: the activation cannot outlive this
    # module (tui overrides `recover()`, so the module-boundary cold relaunch
    # rebuilds the app — convention 10), and the only later test in this file
    # touches no conversations. Meanwhile this test ends with the app signed in
    # as a DIFFERENT account, so a teardown call would be driving a
    # mid-succession app and could raise over the top of a real failure.
    app.conversations.enable_real_faunamls()

    # An API-tier peer with REAL key packages — the real backend parses each one
    # during bootstrap, so the fake byte strings the mock tests use yield no
    # group at all. bob has no MLS engine; he is here to make the group real and
    # to observe the channel from outside the app.
    bob = conv_api.reachable_peer(port, admin_sk, old_actor)

    # The group, through the app's own recipient resolution — convention 8: the
    # mutation under test is driven the way a user drives it.
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "before the theft")
    thread = _wait_thread_snippet(app, "before the theft", timeout=30.0)
    assert thread is not None, (
        "the group must exist before the ceremony — a succession over zero "
        f"groups is precisely the hole this test fills; error: {app.error_text()!r}"
    )
    channel = thread.channel_id_hex
    assert channel, "the thread must be bound to a channel after the send"
    assert len(_inbox(base, bob)) >= 1, "a real Welcome should be delivered to bob"
    envelopes_before = len(conv_api.channel_fetch(port, bob, channel, after=0))
    assert envelopes_before >= 1, "the Application envelope should be on the channel"

    # ── the theft remedy, exactly as the sibling test drives it ──────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )

    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.succeed_identity_with_held_kit(held)

    wait_for_successor_actor(
        app, old_actor,
        diagnose=lambda: f" sweep={app.driver.get_state('data.succession_sweep')!r}",
    )

    # ── the sweep's own account of what it did ───────────────────────────────
    # It rode across the account switch on purpose (`succession_sweep` is the
    # one field the authenticated-state teardown preserves), which is why this
    # read works at all after the app has come back up as the successor.
    sweep = app.driver.get_state("data.succession_sweep")
    assert sweep is not None, (
        "no sweep ran during the ceremony — the succession re-pointed the "
        "account and left every group holding the stolen credential"
    )
    assert sweep.get("status") == "ran", (
        "the sweep did not run over a live engine (a 'no_engine' status means "
        "conversations were never up, so this test proved nothing about "
        f"groups); got {sweep!r}"
    )
    assert sweep.get("groups", 0) >= 1, (
        "the sweep saw no groups, so it swept nothing — the actor's real group "
        f"was not enumerated off the live engine; got {sweep!r}"
    )
    assert sweep.get("old_leaf_removed_everywhere") is True, (
        "the succeeded credential still holds a leaf in at least one group: "
        f"{sweep.get('groups_old_leaf_removed')} of {sweep.get('groups')} "
        "re-pointed. Per-group state (the reason rides in `Failed(..)`): "
        f"{sweep.get('outcomes')!r}"
    )

    # This run swept every group, so the retry has nothing to finish — the
    # NEGATIVE half of the render gate, which only a completed sweep can assert.
    assert_the_retry_affordance_matches_the_sweep(app, sweep)

    # ── and the nest independently agrees it happened ────────────────────────
    # The report above is the app's own account; this is the corroboration that
    # it reached the wire. The sweep publishes add-successor, the in-group
    # statement, then remove-old — so the channel a NON-member observes must
    # have grown. Asserting growth rather than an exact count keeps this from
    # breaking on a future carrier change that is not this test's subject.
    envelopes_after = len(conv_api.channel_fetch(port, bob, channel, after=0))
    assert envelopes_after > envelopes_before, (
        "the sweep's commits never reached the nest: the channel still carries "
        f"{envelopes_after} envelopes. A report claiming the groups were "
        "re-pointed while the members were never told is the failure mode the "
        "add-before-join ordering exists to prevent"
    )
    return sweep


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_the_ceremony_tells_you_what_it_swept_and_whom_it_cannot_vouch_for(
    ungranted_app, nest_instance
):
    """The ceremony TELLS the user what it did — two lines, two facts.

    ``docs/features/take-your-account-back.md`` outcome 22 is a claim about
    what the user is told: how many groups the old key was removed from and,
    as its own statement, which members the ceremony cannot vouch for. The
    sibling above asserts what HAPPENED (``data.succession_sweep``); this reads
    the two lines off the screen, by the ids the user approved on 2026-09-25
    (``settings.md`` § Recovery kit → *The sweep's own lines*).

    ⚠ **Expected strings are derived, not chosen.** Which arm says what is
    ``SweepView::copy``'s decision; this test mirrors that projection over the
    sweep's own counts and resolves the same ``en.yaml`` keys, so a wrong arm,
    a wrong count, or a line folded into the other fails here.

    ⚠ **Two elements, never one.** The roster is its own line, not a qualifier
    on the eviction, and there is no combined "you are safe" verdict to find on
    either — the same refusal ``SweepReport`` makes structurally.
    """
    driver = ungranted_app.driver
    if not (driver.is_tui() or driver.is_macos() or driver.is_ios()):
        # Every other app that runs the ceremony PAINTS these lines already
        # (settings.md § Implementation status today); what they lack is the
        # two approved ids — parity debt, not a platform absence. tui leads,
        # macos/ios follow (the shared FaunaKit RecoveryKitSection).
        skip_unbuilt(
            driver,
            surface="recovery-kit-sweep-status / recovery-kit-sweep-unvouched-status",
            detail="the sweep's two lines are painted but the ids approved 2026-09-25 "
                   "are not tagged yet; tui, macos and ios tag them",
            tracked="",
        )
    app = ungranted_app
    sweep = _ceremony_over_a_real_group(app, nest_instance)

    # The ceremony above swept every group (it asserts so), so SweepView::copy
    # takes its all-removed arm; the roster line renders exactly when the sweep
    # counted members it cannot vouch for.
    expected_outcome = S.settings.recovery_kit.sweep_all_removed(
        groups=str(sweep["groups"])
    )
    unvouched = sweep.get("unattested_members", 0)
    expected_roster = (
        S.settings.recovery_kit.sweep_unattested(count=str(unvouched))
        if unvouched > 0 else ""
    )

    app.settings.navigate()
    app.settings.open_recovery_kit()
    wait_until(
        lambda: app.settings.sweep_status() == expected_outcome,
        30.0,
        diagnose=lambda: (
            f"the sweep line reads {app.settings.sweep_status()!r}, expected "
            f"{expected_outcome!r} for sweep={sweep!r}; error={app.error_text()!r}"
        ),
    )
    roster = app.settings.sweep_unvouched_status()
    assert roster == expected_roster, (
        f"the roster line reads {roster!r}, expected {expected_roster!r} — "
        f"{unvouched} member(s) the sweep cannot vouch for, as its OWN line"
    )
    # The real group holds bob, whom the ceremony did not add, so the roster is
    # never empty here — which is what makes the two-elements half assertable.
    assert unvouched >= 1 and roster, (
        "a real group with a peer the ceremony did not add must report that "
        f"peer as unvouched; sweep={sweep!r}"
    )
    assert expected_outcome not in roster and expected_roster not in app.settings.sweep_status(), (
        "the two facts must stay two elements — one line carries the other"
    )


@pytest.mark.feature("take-your-account-back")
def test_the_confirm_gate_refuses_rather_than_dropping_the_command(
    ungranted_app, nest_instance
):
    """An armed-but-phrase-less succession refuses **on error-message**.

    Two properties in one run. The obvious one is testing.md point 11: a
    command an agent can drive must be honoured or refused out loud, never
    dropped. The load-bearing one is that the refusal is *local* — no
    succession is attempted, so the account is untouched afterwards. A ceremony
    that round-tripped first and failed at the nest would look identical on
    screen while having told a server that this account is being re-pointed.
    """
    app = ungranted_app
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()

    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    assert app.settings.recovery_kit_secret()

    # account-actor-id is Status-page-only (see the sibling test's comment) —
    # read it before open_recovery_kit() navigates into the Account sub-page.
    # `_navigate_subpage("status")`, not plain `.navigate()` — iOS lands a
    # plain navigate on the root page list, not the Status sub-page.
    app.settings._navigate_subpage("status")
    before = app.settings.actor_id()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)

    # Arm the gate but leave the phrase empty: the button goes live on the
    # confirm word alone, so this is a state a real user can reach by typing
    # SUCCEED before pasting.
    app.driver.fill("identity-stolen-confirm-field", "SUCCEED")
    app.driver.click("identity-stolen-button")

    wait_until(
        lambda: bool(app.error_text()),
        30.0,
        diagnose=lambda: (
            f"no refusal surfaced; status="
            f"{app.settings.recovery_kit_status()!r}"
        ),
    )

    # The account is untouched — the refusal happened before any statement was
    # signed, let alone submitted. `_navigate_subpage("status")`, not plain
    # `.navigate()` — see the read above.
    app.settings._navigate_subpage("status")
    assert app.settings.actor_id() == before, (
        "a refused ceremony must not have re-pointed the account"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_a_real_member_re_points_the_succeeded_participant_on_their_own_client(
    succeedable_app, succession_member_app, nest_instance
):
    """The **member's** side of the ceremony: bob's own client re-points alice's
    participant row onto her successor, against a live nest.

    Every succession test beside this one drives or observes the *succeeding*
    seat. But the in-group succession statement is posted **for the members** —
    they are its entire audience — and until this test nothing asserted that a
    real member client, holding a real MLS engine, ever renders continuity
    instead of "a stranger joined and alice left". The nine unit tests over the
    wired path (``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs``,
    ``libs/fauna-client-recovery/tests/witness.rs``) drive a spy chain source
    against a mock nest; three engines in one process cannot exercise the
    harvest, the dial, or the nest that answers it.

    What only a full-stack run establishes, in the order the flow needs them:

    - **A Welcome-joined member can anchor the peer who added him.** bob's
      roster row for alice carries no handle *the owner typed* — leaf
      credentials carry none, and the name the room home's roster read puts
      on the row afterwards is display only, never a succession anchor
      (``identity-succession.md`` § The succession statement → *which
      participant handles anchor tier 2*) — so tier 2's handle anchor does
      not exist for him, and tier 1 needs a chain head nothing had harvested.
      That was the hole; the remedy is the peer-profile harvest (same
      doc, *the peer-profile harvest*), which rides bob's own authenticated
      connection on an ordinary read path and attempts every Fauna row, named
      or not. Here it runs for real, against a real profile alice really
      published.
    - **The statement verifies and re-points, end to end.** Alice's app posts
      it between add-successor and remove-old; bob's inbound poll decodes it,
      his witness settles it, and his thread store moves the row.
    - **The re-point is in place.** Same slot, and the label/handle are
      untouched — continuity, not a membership churn the user must interpret.

    ⚠ **The assertion is on ``participant_actor_ids``, and it has to be.** The
    re-point keeps the list position *and* the handle (the nest moved the
    handle to the successor inside the succession transaction), so
    ``thread-member-chip[i]``'s text, the thread label and
    ``participant_count`` are **identical** before and after — every one of
    them is equally green against a client that re-pointed nothing. The actor
    id is the only thing that moves, which is why the field exists (added with
    this test; pinned in shared Rust by
    ``the_repoint_is_readable_through_the_state_contract_a_driver_polls``).

    ⚠ **Fixture ordering is load-bearing, twice over** — both because the
    harvest is a *producer* that must run before the statement arrives:

    1. **Alice registers her kit BEFORE the group is created.** The profile
       head mirror is written at kit registration (``publish_recovery_head``),
       and a head-less profile seeds nothing — so the kit comes first here,
       unlike in the sibling ceremony test where it is created last.
    2. **Bob's harvest must have LANDED before the ceremony runs** — and that
       is asserted, not inferred from a cadence. His sweep re-diffs the roster
       every 5 s and harvests the nameless rows it finds, so seeing the thread
       only guarantees there is a row to find; it does not guarantee the fetch
       and the anchor-plane write beat a ceremony. **They did not** on this
       test's first full-stack runs (2026-08-10): the witness answered
       `no_anchor` with `held_head_seq: None` against a *readable* store
       holding zero heads, while the harvest report — read at the end — showed
       a perfectly successful `Seeded`. It had simply landed afterwards. The
       mechanism was never broken; a 5 s cadence lost a race to a ceremony
       under load. So the wait on ``data.succession_witness``'s `harvest` entry
       below is the ordering **barrier**, and reading it as mere belt-and-
       braces is how this test would go back to failing as a mystery.

    ⚠ **The residual that leaves is a PRODUCT one and is tracked, not fixed
    here.** Exactly ONE delivery arrived in 120 s, so the "a heal re-delivery
    converges" escape the harvest ratification declares did not fire: a member
    whose harvest loses that race renders the bare add indefinitely. Note the
    obvious fix is forbidden — harvesting *on demand* when the witness finds no
    anchor would fetch a profile signed by the very key a seed thief holds,
    which is rule 4 of § The succession statement → *the peer-profile
    harvest*.

    ⚠ **The harvest barrier's premise is provenance, not namelessness.** Since
    the id-keyed roster read (``conversation-rooms.md`` § Implementation
    status today), a same-nest member's Welcome-joined row is usually named
    before the sweep reaches it — and for five days the sweep skipped every
    named row, so this barrier never saw alice and the journey was red on
    linux and tui. The ruling makes a
    room-home name display only and has the sweep attempt every Fauna row,
    so the barrier stands as written: bob's label for alice may well read
    ``handle@domain`` here, and the harvest entry must land regardless.

    ⚠ **Single-nest by construction, and that is exactly the reach this proves,
    not an accident of the fixture.** alice and bob share ``nest_instance``, so
    bob's harvest fetch — which rides his own authenticated connection — can
    reach alice's profile. A cross-nest peer answers ``not_found`` today (the
    cross-nest transport is a declared follow-on). Do not read this test as
    cross-nest coverage.

    The refusal control is deliberately at tier 1, not here: forging an
    unverifiable statement onto a live channel would need a second signer and
    prove less than ``an_unverified_statement_repoints_nothing`` already does
    over the same driver. What this test adds is the half a mock cannot fake.
    """
    from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

    alice, alice_user = succeedable_app
    bob, bob_actor = succession_member_app
    old_actor = alice_user["actor_id_hex"]

    # Skip-gate FIRST (before any nest-side setup): an app without the
    # succession driver must report "not built" rather than fail later on a
    # real backend it also does not have. `_navigate_subpage("status")`, not
    # plain `.navigate()` — iOS lands a plain navigate on the root page list.
    alice.settings._navigate_subpage("status")
    assert alice.settings.actor_id() == old_actor, (
        "the fixture's dedicated actor must be the one signed in — every "
        "assertion below names it; error surface: "
        f"{alice.error_text()!r}"
    )
    alice.settings.open_recovery_kit_or_skip()
    require_stolen_gate(alice)

    # (1) The kit FIRST — registering it is what mirrors alice's chain head
    # into her profile, and bob's harvest is what reads it back.
    alice.settings.create_recovery_kit()
    alice.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = alice.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {alice.error_text()!r}"
    )

    # The group, through alice's own real engine and her own recipient
    # resolution — convention 8: the mutation is driven the way a user drives
    # it. bob receives it on his real engine, so his roster row for alice is a
    # Welcome-joined row: whatever name the room home later puts on it, no
    # gesture of bob's did, and the harvest is what anchors her.
    alice.conversations.enable_real_faunamls()
    alice.conversations.real_resolve_send_new(bob_actor["actor_id_hex"], "before the theft")

    # (2) bob SEES the thread — the causal barrier the harvest needs, and the
    # pre-state the re-point is measured against.
    thread = _wait_thread_snippet(bob, "before the theft", timeout=60.0)
    assert thread is not None, (
        "bob never received alice's message, so there is no member-side row to "
        f"re-point; error surface: {bob.error_text()!r}"
    )
    ids_before = thread.participant_actor_ids
    assert ids_before, (
        "bob's client serializes no participant actor ids — without them this "
        "test could only assert continuity through the chip text, which the "
        "re-point leaves unchanged either way. On an app whose state "
        f"serializer has not picked the field up, this is the gap. row={thread._row!r}"
    )
    assert old_actor in ids_before, (
        "before the ceremony bob's row must name ALICE — if it already named "
        "someone else, the assertion after the ceremony would prove nothing "
        f"about the re-point. ids={ids_before!r} alice={old_actor}"
    )
    alice_at = ids_before.index(old_actor)
    label_before = thread.label
    participants_before = thread.participant_count

    # The harvest's INPUT, asserted as a precondition rather than left to be
    # inferred from a re-point that never comes. bob anchors alice by fetching
    # her signed profile over his OWN authenticated connection, so this reads
    # it exactly as he does — same call, same reader, same nest. A missing
    # profile here means the kit's head mirror never landed
    # (``publish_recovery_head``), which would leave bob with no tier-1 anchor
    # and turn the assertion below into an unexplained 120 s timeout.
    from actions.api_actor import ApiActor

    bob_api = ApiActor(
        nest_instance["url"],
        bob_actor["token"],
        bob_actor["actor_id_hex"],
        bytes(bob_actor["signing_key"]),
    )
    alice_profile = bob_api.profile_get(old_actor)
    assert alice_profile.get("body"), (
        "alice published no readable profile, so bob's peer-anchor harvest has "
        "nothing to seed from — the kit registration's head mirror is the "
        f"producer (identity-succession.md § The RecoveryKey). got: {alice_profile!r}"
    )

    # And the harvest's OUTPUT, asserted before the ceremony rather than
    # inferred from a re-point that never comes. bob's sweep must have
    # anchored alice *while the whole ceremony is still ahead of the
    # statement* — that ordering is the design (rule 4: harvest on
    # ordinary read paths, never at verify time), so a harvest that only
    # lands afterwards is a product bug, not a slow fixture, and it would
    # otherwise present as an unexplained 120 s timeout two waits below.
    # Latency-independent (convention 14): a named generous ceiling on a
    # state poll, never a sleep.
    def _bobs_harvest_of_alice():
        report = bob.driver.get_state("data.succession_witness") or {}
        for entry in report.get("harvest", []):
            if entry.get("actor_id") == old_actor:
                return entry
        return None

    def _bobs_label_for_alice():
        # Re-read when diagnosing, not at the thread's first render: a name can
        # land on bob's row after he first sees the thread.
        for t in bob.conversations.list_threads():
            ids = t.participant_actor_ids
            if t.thread_id == thread.thread_id and old_actor in ids:
                displays = t.participant_displays
                at = ids.index(old_actor)
                return displays[at] if at < len(displays) else None
        return None

    wait_until(
        _bobs_harvest_of_alice,
        _HARVEST_ANCHOR_S,
        diagnose=lambda: (
            "bob's peer-anchor harvest never even attempted alice, so the "
            "statement below has nothing to verify against. The sweep attempts "
            "EVERY Fauna row, named or not (the name-keyed skip was retired "
            "2026-09-15 — identity-succession.md § which participant handles "
            "anchor tier 2), so a handle@domain label on bob's row for her "
            f"({_bobs_label_for_alice()!r}) is not a reason for the miss: if it "
            "reads as one, the skip is back. A short actor id with no harvest "
            "entry means the sweep is not running or cannot see the thread. "
            f"witness={bob.driver.get_state('data.succession_witness')!r}"
        ),
    )
    harvested = _bobs_harvest_of_alice()
    assert harvested["outcome"].startswith("Seeded"), (
        "bob's harvest reached alice and did NOT seed an anchor from her "
        "profile, so tier 1 has nothing to settle the statement with: "
        "NothingNew means her profile carried no recovery_head (the kit's "
        "head mirror, publish_recovery_head, is the producer); Refused names "
        "the shape the seeding gate rejected; SeedLost means the write "
        "reported success and did not read back. "
        f"got {harvested!r}"
    )

    from tests.api import conv_api as _conv_api

    envelopes_before = len(
        _conv_api.channel_fetch(
            nest_instance["port"], bob_actor, thread.channel_id_hex, after=0
        )
    )

    # ── the theft remedy, exactly as the sibling ceremony test drives it ─────
    alice.settings.navigate()
    alice.settings.open_recovery_kit()
    alice.settings.succeed_identity_with_held_kit(held)

    successor = wait_for_successor_actor(
        alice, old_actor,
        diagnose=lambda: (
            f" sweep={alice.driver.get_state('data.succession_sweep')!r}"
        ),
    )
    sweep = alice.driver.get_state("data.succession_sweep")
    assert sweep is not None and sweep.get("status") == "ran", (
        "the sweep did not run over a live engine, so no statement was ever "
        f"posted for bob to verify; got {sweep!r}"
    )

    # ── and now the half nothing has ever asserted: BOB's own render ─────────
    def _bobs_ids():
        for t in bob.conversations.list_threads():
            if t.thread_id == thread.thread_id:
                return t.participant_actor_ids
        return []

    # Split the two halves the timeout below could otherwise conflate: the
    # channel growing proves alice's sweep really published add-successor +
    # the statement + remove-old to the nest, so a stalled re-point after this
    # is BOB's side (his poll, his harvest, or his witness), never a statement
    # that was never posted.
    from tests.api import conv_api

    channel = thread.channel_id_hex
    assert channel, "bob's thread must be bound to a channel"
    wait_until(
        lambda: len(conv_api.channel_fetch(nest_instance["port"], bob_actor, channel, after=0))
        > envelopes_before,
        _MEMBER_REPOINT_S,
        diagnose=lambda: (
            "the ceremony's commits never reached the channel bob reads: still "
            f"{len(conv_api.channel_fetch(nest_instance['port'], bob_actor, channel, after=0))} "
            f"envelopes (was {envelopes_before}). alice's sweep reported "
            f"{alice.driver.get_state('data.succession_sweep')!r}"
        ),
    )

    wait_until(
        lambda: _bobs_ids()[alice_at:alice_at + 1] == [successor],
        _MEMBER_REPOINT_S,
        diagnose=lambda: (
            f"bob's participant ids are {_bobs_ids()!r}; expected the successor "
            f"{successor!r} at index {alice_at} (was alice {old_actor!r}). "
            "The statement DID reach the channel (asserted above), so this is "
            "bob's side. Read data.succession_witness in this order: "
            "statements.seen == 0 means his inbound poll never even decrypted "
            "the body (a stalled Commit walk, a failed decrypt) and no anchor "
            "question below is implicated; statements.no_witness means the "
            "session wired none; then peers[].outcome names the arm — "
            "no_anchor is a missing HARVEST (a producer), walk_failed is a "
            "nest that could not be reached or did not confirm the chain. On "
            "no_anchor, keep going: anchor_store says whether the place "
            "anchors rest was readable at all (unreadable indicts bob's own "
            "anchor plane; read-and-empty indicts the producer), and "
            "harvest[] names what the sweep got for each peer — no entry for "
            "alice means the sweep never reached her, NothingNew means her "
            "profile carried no recovery_head, Refused means its shape was "
            "rejected, StoreFailed means the write never landed. "
            f"bob witness={bob.driver.get_state('data.succession_witness')!r}; "
            f"bob error surface: {bob.error_text()!r}; alice's sweep: "
            f"{alice.driver.get_state('data.succession_sweep')!r}"
        ),
    )

    ids_after = _bobs_ids()
    assert old_actor not in ids_after, (
        "the retired identity must not survive anywhere in bob's row — a "
        f"stranger-plus-ghost pair is what re-pointing exists to avoid: {ids_after!r}"
    )
    after = next(t for t in bob.conversations.list_threads() if t.thread_id == thread.thread_id)
    assert after.participant_count == participants_before, (
        "the re-point must not change the roster SIZE — a member added and "
        f"another left is precisely the render this ceremony exists to avoid; "
        f"{participants_before} → {after.participant_count}"
    )
    assert after.label == label_before, (
        "the thread label must survive the re-point (the handle moved to the "
        f"successor inside the nest transaction): {label_before!r} → {after.label!r}"
    )


# `settled_actor_id` / `kit_on_screen` / `SUCCESSION_AND_RELAUNCH_S` — the
# reads every succession journey polls on — live in
# `helpers/succession_ceremony.py` since 2026-08-27 (lifted from this file's
# copies when a fourth journey needed them).


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_after_the_recovery_a_contact_can_still_add_you_to_a_conversation(
    succeedable_app, nest_instance
):
    """a contact → take the account back → the contact adds the account to a
    new conversation → it arrives on the successor and its message decrypts.

    ``succession-aftermath.md`` § Re-key scope: the succession burns the old
    key's published key packages (a thief holding the old seed must not be
    added in the account's name), and the client republishes its pool at next
    sign-in. If the republish never ran, an invitation to a recovered account
    would find no key package, and nothing would tell either side. The nest
    half is unit-pinned; the republish rides every session start
    (``fauna_conversations`` ``start_receive_loop``'s replenish). This is the
    first run that proves the successor's own session reaches it.

    The contact is an API-tier peer (fixture setup, convention 8's carve-out):
    what is under test is whether the successor's app, after the ceremony,
    can be invited and read what it was sent. The contact edge is made by the
    account BEFORE the recovery, because that is who invites a recovered
    account in practice — people who already knew it.
    """
    from common import create_actor_and_register
    from helpers.budgets import MLS_HANDSHAKE_S
    from tests.api import conv_api

    app, user = succeedable_app
    port = nest_instance["port"]

    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    app.settings.open_recovery_kit_or_skip()
    require_stolen_gate(app)

    # The real conversations backend: it is what publishes key packages at
    # session start, so without it this would prove nothing about the app.
    app.conversations.enable_real_faunamls()

    alice = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    conv_api.accept_contact(port, user, alice["actor_id_hex"])

    # ── the recovery, through the UI ─────────────────────────────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(app, old_actor)

    # ── the new key is invitable ─────────────────────────────────────────
    wait_until(
        lambda: conv_api.keypackage_count(port, alice, new_actor) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "the successor published no key package, so nobody can add the "
            "recovered account to a conversation; old key's pool holds "
            f"{conv_api.keypackage_count(port, alice, old_actor)}, error "
            f"surface: {app.error_text()!r}"
        ),
    )
    kp = conv_api.keypackage_fetch(port, alice, new_actor)
    assert kp is not None, "a counted key package must be fetchable"

    body = f"welcome back {uuid.uuid4().hex[:6]}"
    channel, welcome, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), kp, body
    )
    conv_api.welcome_deliver(port, alice, new_actor, channel, welcome)

    # ── …and the invitation reaches the successor's app ──────────────────
    wait_until(
        lambda: any(
            t.channel_id_hex == channel
            for t in app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "the successor never joined the conversation it was invited to — "
            "the invitation went nowhere; error surface: "
            f"{app.error_text()!r}"
        ),
    )
    conv_api.channel_send(port, alice, channel, envelope)
    wait_until(
        lambda: any(
            t.channel_id_hex == channel and body in (t.snippet or "")
            for t in app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "the successor joined but cannot read what it was sent — the key "
            "package it published is not one it holds the private half of"
        ),
    )


# The apps whose e2e agent hands the shared registry bridge
# (`fauna_client_accounts::call_registry_method_for_test`) its own
# `AccountRegistry` — the one delegation `refuse_secret_writes_for_test` needs.
# The name table and the fault itself are shared Rust; each other app joins by
# adding that single arm to its agent.
_REFUSAL_DOOR_APPS = ("tui", "macos", "ios", "windows", "linux", "web")

_HEX64 = re.compile(r"[0-9a-fA-F]{64}")


def _redacted(text: str) -> str:
    """``text`` with every 64-hex run masked.

    The persist-failure message MUST carry the successor's secret key — it is
    the only copy in existence (`settings.md` § Recovery kit). This test's own
    failure output must not: an assertion message or a diagnose string lands in
    logs and ledgers that outlive the run. Every string built from what the app
    renders goes through here first.
    """
    return _HEX64.sub("<64-hex>", text or "")


@pytest.mark.feature("take-your-account-back")
def test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave(
    succeedable_app, nest_instance
):
    """the keystore refuses the successor's seed → the ceremony lands anyway →
    the page shows the new key with what to do → another write to the same
    error slot cannot wipe it → leaving Account discharges it.

    ``settings.md`` § Recovery kit → *The persist-failure message survives the
    page*: once the nest has re-pointed the account, the message is the ONLY
    copy of the new key, and it renders on the Account page's shared
    ``error-message`` that every other control writes to. The fold and the
    guard are unit-pinned; what no unit test can show is the whole path — a
    real keystore write refused, the shared read-back noticing, the app
    choosing the persist-failure branch rather than switching onto a key it
    never saved — and the guard holding against a real gesture.

    The refusal is fault injection through the shared registry bridge
    (``refuse_secret_writes_for_test``, compiled out of release artifacts —
    convention 15): nothing a user can do makes a keystore refuse on demand.
    The ceremony and the second writer are both driven through the UI
    (convention 8).
    """
    app, user = succeedable_app
    stolen_seed = bytes(user["signing_key"]).hex()
    if app_name(app.driver) not in _REFUSAL_DOOR_APPS:
        skip_unbuilt(
            app.driver,
            surface="the registry E2E bridge delegation (`refuse_secret_writes_for_test`)",
            detail="one arm in this app's agent handing "
            "`fauna_client_accounts::call_registry_method_for_test` its own "
            "`AccountRegistry` — the fault and its name are already shared Rust",
            tracked="behavior/onboarding.md § E2E bridge contract — the registry "
            "dispatcher's per-app delegation",
        )

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)} chars; error surface: "
        f"{_redacted(app.error_text())!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)

    prefix = S.settings.recovery_kit.stolen_persist_failed(secret="")
    app.driver.call_machine_method(
        "refuse_secret_writes_for_test", json.dumps({"refuse": True})
    )
    try:
        app.settings.succeed_identity_with_held_kit(held)

        # ── The key is on screen, with what to do. ───────────────────────────
        # Anchored on the message itself: the ceremony's nest round trip and
        # both refused writes finish before the fold parks it.
        wait_until(
            lambda: app.error_text().startswith(prefix),
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: (
                "no persist-failure message — with the keystore refusing the "
                "successor's seed the app must show that key rather than "
                "switch onto one it never saved; error surface: "
                f"{_redacted(app.error_text())!r}"
            ),
        )
        shown = app.error_text()
        key = shown[len(prefix):]
        # Booleans, never the strings: pytest's assertion rewrite prints both
        # operands of a failed comparison, and these carry the key.
        is_hex_key = _HEX64.fullmatch(key) is not None
        assert is_hex_key, (
            "the message must end in the successor's 64-hex secret key — it is "
            f"the only way back into the account; reads {_redacted(shown)!r}"
        )
        is_new_key = key not in (stolen_seed, held)
        assert is_new_key, (
            "the key shown must be the NEW identity's, not the stolen seed or "
            "the recovery kit just spent"
        )

        # ── Another writer of the same slot cannot wipe it. ──────────────────
        # The superseded session is dead, so every control that needs the nest
        # is desensitized (the offline gate) — the stolen button included.
        # "Export my data" stays live, and its outcome writes this very slot
        # (a failure inserts its reason, a success clears it). The agent awaits
        # the export and folds its outcome before acking the click, so on
        # return the write has already been attempted — no timing involved.
        app.driver.click("settings-export-data-button")
        after = app.error_text()
        kept = after == shown
        assert kept, (
            "a later write to the Account page's error slot replaced the only "
            f"copy of the new key; it now reads {_redacted(after)!r}"
        )
    finally:
        app.driver.call_machine_method(
            "refuse_secret_writes_for_test", json.dumps({"refuse": False})
        )

    # ── Leaving Account is the acknowledgment. ────────────────────────────────
    # Through the page's own way out (`leave_account`: the desktop shell's back
    # control, mobile's idiomatic back): the same edge performs the held-back
    # supersession escalation, so the app moves on to the import route rather
    # than staying on another settings page.
    app.settings.leave_account()
    wait_until(
        lambda: key not in app.error_text(),
        STOLEN_GATE_REVEAL_S,
        diagnose=lambda: (
            "leaving Account must discharge the persist-failure message; it "
            f"still reads {_redacted(app.error_text())!r}"
        ),
    )

    # ── …and performs the supersession it held back. ─────────────────────────
    # The ceremony superseded the identity this session holds, so the escalation
    # it held back goes the ordinary way now: re-entering launch, where the
    # refusal routes to the import flow (`identity-succession.md` § Propagation
    # → *Own device fleet*). `paste-secret-field` IS that route's affordance.
    wait_until(
        lambda: app.is_visible("paste-secret-field"),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "leaving Account must perform the supersession the ceremony held "
            "back and land the import flow, but the device is still elsewhere "
            "on a refused session; error surface: "
            f"{_redacted(app.error_text())!r}"
        ),
    )


# ── A lost submit reply (outcome 18) ─────────────────────────────────────────
#
# The nest commits a succession BEFORE it encodes its reply, so a connection
# lost in that gap leaves the app holding the only copy of the key the account
# now belongs to (`identity-succession.md` § Implementation status today, *a
# lost submit reply no longer destroys the account*). The app keeps the seed
# on either arm and reconciles over its own anonymous connection
# (`finish_unconfirmed_succession`, `libs/fauna-client-recovery/src/ceremony.rs`,
# called from tui's ceremony op in `apps/fauna-tui/src/settings/mod.rs`). The
# crate-level arms are pinned in-process against a lossy fake transport; these
# two journeys stage the same states against the REAL nest, through
# `helpers/rpc_hold`'s drop mode: the submit handler runs and commits, and the
# connection closes instead of replying.
#
# `standalone_only`: the hook exists only in a nest compiled with `test-hooks`
# (convention 15). The kinds are process-global gates on the session nest, so
# every arm is released in a `finally`.

_SUBMIT_KIND = "fauna.recovery.succession.submit"
_LOOKUP_KIND = "fauna.recovery.succession.lookup"

# The undecidable arm is a typed outcome in shared Rust (`StolenOutcome`), and
# its sentence is an i18n key every app resolves and paints VERBATIM — so the
# journeys assert the resolved key, whole, never a substring of it. `{cause}`
# and `{reported}` are diagnostics (transport text) and match anything; the
# rest of the sentence, including that nothing is wrapped around it, is pinned.
_DIAGNOSTIC = "\x00"


def _resolved(sentence: str) -> re.Pattern[str]:
    """A resolved i18n sentence as a whole-string pattern, its diagnostic
    arguments (passed as `_DIAGNOSTIC`) free."""
    return re.compile(
        ".+".join(re.escape(part) for part in sentence.split(_DIAGNOSTIC)),
        re.DOTALL,
    )


# The way back in names the store the seed was verified in: this key carries
# no `{secret}` at all, and never reads as "nothing happened".
_OUTCOME_UNKNOWN_SAVED = _resolved(
    S.settings.recovery_kit.stolen_outcome_unknown_saved(
        cause=_DIAGNOSTIC, reported=_DIAGNOSTIC
    )
)
# Any undecided sentence, either half — what a reconcile that FOUND the
# succession must never show.
_OUTCOME_UNKNOWN_HEADLINE = S.settings.recovery_kit.stolen_outcome_unknown_saved(
    cause=_DIAGNOSTIC, reported=_DIAGNOSTIC
).split(_DIAGNOSTIC)[0]


def _kit_in_hand(app) -> tuple[str, str]:
    """Read the signed-in actor, create a kit through the UI, and come back to
    an armed-ready stolen gate. Returns ``(old_actor, held_kit)`` — the same
    opening the first journey in this module walks, for the same reasons."""
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the Status page must render the signed-in actor id; got {old_actor!r}, "
        f"error surface: {app.error_text()!r}"
    )
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    return old_actor, held


@pytest.mark.standalone_only
@pytest.mark.feature("take-your-account-back")
def test_a_lost_submit_reply_still_brings_you_back_as_the_successor(
    succeedable_app, nest_instance
):
    """the submit commits and its reply is dropped → the app reconciles over
    its own connection, finds its successor landed → finishes exactly as if
    the reply had arrived: signed in as the successor, shown a fresh kit.

    The path production actually ran from 2026-08-23 to 2026-09-12, while a
    revocation defect dropped EVERY succession reply: the user must see an
    ordinary success, never an error, because the account really is theirs.
    """
    from helpers.rpc_hold import (
        drop_rpc_reply,
        release_rpc_hold,
        rpc_hold_status,
        wait_for_dropped_rpc,
    )

    app, _user = succeedable_app
    port = nest_instance["port"]
    old_actor, held = _kit_in_hand(app)

    dropped_before = rpc_hold_status(port, _SUBMIT_KIND)["dropped"]
    drop_rpc_reply(port, _SUBMIT_KIND)
    try:
        app.settings.succeed_identity_with_held_kit(held)
        # Proof the app's submit ran and its reply was thrown away — so what
        # follows is the reconcile arm, not the ordinary confirmed one.
        wait_for_dropped_rpc(port, _SUBMIT_KIND, count=dropped_before + 1)
    finally:
        release_rpc_hold(port, _SUBMIT_KIND)

    # The closing act ran, so the reconcile really landed as `Landed` (only
    # a landed ceremony owes the successor a kit). Read first and without
    # navigating, for the shown-once reason the first journey states.
    wait_until(
        lambda: len(kit_on_screen(app)) == _SECRET_HEX_LEN,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no kit on screen for the successor after a lost submit reply "
            f"(reads {_redacted(kit_on_screen(app))!r}) — the reconcile did not "
            f"finish the ceremony; error={_redacted(app.error_text())!r}"
        ),
    )
    assert kit_on_screen(app) != held, "the successor must be minted a FRESH kit"

    wait_for_successor_actor(app, old_actor)
    assert _OUTCOME_UNKNOWN_HEADLINE not in app.error_text(), (
        "a reconcile that FOUND the succession must not tell the user the "
        f"outcome is unknown; reads {_redacted(app.error_text())!r}"
    )


@pytest.mark.standalone_only
@pytest.mark.feature("take-your-account-back")
def test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in(
    succeedable_app, nest_instance
):
    """the submit commits and its reply is dropped, AND the reconcile's lookup
    is turned away → the app says plainly the outcome is unknown and how to get
    back in → following that (reopen the app, the nest reachable again) signs
    in as the successor.

    Outcome 18. The undecidable arm is the worst cell of the succession matrix
    — the account may already belong to a key that exists only on this device
    — and the wording is the whole safety property: it must never read as
    "nothing happened", and the way back in it names must actually work.
    """
    from helpers.rpc_hold import (
        drop_rpc_reply,
        refuse_rpc,
        release_rpc_hold,
        rpc_hold_status,
        wait_for_dropped_rpc,
        wait_for_refused_rpc,
    )

    app, _user = succeedable_app
    port = nest_instance["port"]
    # "Reopen the app" must reopen THIS device, with the seed the ceremony
    # saved — pinned before anything, while the creating launch is current.
    if not app.driver.preserve_state_across_relaunch():
        from helpers.app_surface import skip_environment

        skip_environment(
            f"the {app.driver.__class__.__name__} driver cannot pin its "
            "client-local store across a relaunch, so 'reopen the app' would "
            "open a fresh install rather than the device holding the successor"
        )
    old_actor, held = _kit_in_hand(app)

    dropped_before = rpc_hold_status(port, _SUBMIT_KIND)["dropped"]
    refused_before = rpc_hold_status(port, _LOOKUP_KIND)["refused"]
    drop_rpc_reply(port, _SUBMIT_KIND)
    refuse_rpc(port, _LOOKUP_KIND)
    try:
        app.settings.succeed_identity_with_held_kit(held)
        wait_for_dropped_rpc(port, _SUBMIT_KIND, count=dropped_before + 1)
        wait_for_refused_rpc(port, _LOOKUP_KIND, count=refused_before + 1)

        # ── Told plainly: unknown, and the way back in. ──────────────────────
        # The resolved `stolen_outcome_unknown_saved` key, whole: the outcome
        # is UNKNOWN, the successor is saved on this device so reopening signs
        # in with it — and nothing is wrapped around the sentence (a "couldn't
        # recover" headline in front of it fails the full match).
        wait_until(
            lambda: _OUTCOME_UNKNOWN_SAVED.fullmatch(app.error_text()) is not None,
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: (
                "a succession whose reply was lost and whose outcome could not "
                "be checked must paint settings.recovery_kit."
                "stolen_outcome_unknown_saved verbatim; error surface: "
                f"{_redacted(app.error_text())!r}"
            ),
        )
        shown = app.error_text()
        assert not _HEX64.search(shown.replace(old_actor, "")), (
            "the seed was saved, so the message must point at the store and "
            "never put a secret on screen"
        )
    finally:
        release_rpc_hold(port, _SUBMIT_KIND)
        release_rpc_hold(port, _LOOKUP_KIND)

    # ── Following it works. ───────────────────────────────────────────────────
    # `recover()` is the drivers' teardown + relaunch-with-the-same-config —
    # the user reopening the app, now with the nest answering again.
    assert app.driver.recover(), "the app did not come back up after a relaunch"
    # The relaunch is refused as superseded, the chain proves the successor,
    # and this device holds its key — so it is adopted, and owed the kit the
    # interrupted ceremony never minted. Read first, without navigating (the
    # shown-once reason the first journey states).
    wait_until(
        lambda: len(kit_on_screen(app)) == _SECRET_HEX_LEN,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "reopening the app must sign in as the saved successor and mint its "
            f"kit; kit reads {_redacted(kit_on_screen(app))!r}, paste-secret-field "
            f"visible={app.is_visible('paste-secret-field')}, error="
            f"{_redacted(app.error_text())!r}"
        ),
    )
    assert kit_on_screen(app) != held, "the successor must be minted a FRESH kit"
    wait_for_successor_actor(
        app, old_actor,
        diagnose=lambda: (
            " — reopening the app must sign in as the successor the message "
            "said was saved; paste-secret-field visible="
            f"{app.is_visible('paste-secret-field')}"
        ),
    )
