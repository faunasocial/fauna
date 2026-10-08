"""tier_3 e2e: the successor's sealed corpus survives the succession.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § Re-key scope — the
``BackupKey`` corpus row and the ratified blockquote under it ("the successor's
client treats the corpus re-seal as **urgent aftermath** — started at first
successor sign-in, surfaced with progress, resumed until complete").

**Why this is a separate file from the ceremony it depends on.**
``test_identity_succession_ceremony.py`` proves the *account* comes back — the
statement lands, the app relaunches as the successor, the handle travels. It
deliberately says nothing about the user's **data**, and the two can diverge
completely: ownership of the corpus moves inside the nest's succession
transaction, but the *seal* does not, so a successor can own every row it has
and be unable to open a single one. That state is not corruption — it is data
sealed to a key the successor's own seed does not derive.

**What this test would catch that no unit test can.** The unit tests cover the
carry and the progress projection against fakes. What none of them can reach is
the actual composition on a live nest: whether the successor's account runtime
is handed its attested predecessors at all, whether their delegable schedules
come out of the account registry, whether the predecessor's rows are the ones
the nest serves after ownership re-pointed, and whether what the successor's
walk carries is still readable. Every one of those is a link between components
that only a real ``fauna-nest`` puts in the same room.

**Why a muted word is the witness.** The assertion has to be a preference the
user set *before* the ceremony and can still see *after* it. Muted keywords are
exactly that: a user-global list on the account plane's delegable ``moderation``
rows, written through the ordinary Settings UI, with no separate nest-side row
that could make it survive for the wrong reason. A successor's inherited
preference values reach it only through its account-plane walk, which carries
its attested predecessors' delegable rows (``succession-aftermath.md`` § Re-key
scope); if that carry never happens, the list cannot come back. Driving it
through the UI rather than a config RPC is convention 8: the app is the only
configuration surface, so the write under test is a user's write.

⚠ **The precondition is the whole test.** A fresh account has no preference
row at rest, so a succession run against one carries *nothing* and would pass
this test while the mechanism was entirely broken. The muted word written
before the ceremony is what forces a row to exist, and therefore what makes the
assertion after it meaningful. Do not "simplify" this by dropping the
pre-ceremony write.

**The second journey here — aftermath leg 2, the ``NestBackupKey`` re-grant.**
Same shape, different corpus surface, and a sharper vacuity trap. The
succession transaction *deletes* the owner's ``nest_backup_keys`` row, and the
nest's backup sweep enumerates owners **by that row**
(``delegation_runner.rs::list_nest_backup_key_owners``), so a successor without
one is skipped entirely — backups silently stop. The destination registry rows
are stranded the same way, being keyed to the retired owner and deliberately not
re-pointed. Leg 2 rebuilds both from the destination list, a
``fauna.state.backup`` row in the successor's account store, which is why it
runs in the post-store-ready pass.

⚠ **Why that journey must NOT assert through the Backups page.**
``read_backup_status`` heals a missing enrollment on mount, so opening Backups
makes the projection correct **whether or not the aftermath ran** — an assertion
there would pass against a completely unwired hook. The witness is therefore the
Settings § Recovery kit progress line, read without ever visiting Backups. This
is the same class of trap as the empty-account one above: a surface that
repairs what you came to measure.

**The three adjudication journeys — what the aftermath ASKS, not what it does.**
The three above each prove a leg *ran*. These prove the other half of the
ratified shape (§ Re-key scope → *Adjudicating what the aftermath carries
across*): re-register everything immediately — backups restarting must not
become user-gated — and then **report** every carried-across row until the owner
keeps or removes it. That second half exists because the seed thief the whole
ceremony answers could have added a backup destination, minted a trust, or been
seated in a group at any point in the pre-succession window, indistinguishable
from the owner's own. Three planes, three surfaces, one rule:

- **destinations** (Backups page) — the mark is defense-in-depth: a thief's
  destination still only receives segments sealed under a key it lacks;
- **trusts** (Nests page) — the mark is mandatory: a thief-added grantee is
  handed live *read* capability by the successor's own client;
- **members** (a thread's member chips) — the population the ceremony
  structurally cannot evict, since it evicts the stolen *credential* and not an
  identity the thief seated under a second name.

⚠ **Absence, not emptiness, is the un-raised state on all three.** A permanently
rendered mark would train the user straight past the one succession that matters,
so every "not raised" assertion here polls *visibility*, never text.

Convention 14 throughout: every wait is a deadline poll on latency-independent
state behind a named generous budget, never a settle-sleep.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app that has both the
recovery-kit section and the muted-words page — today tui, the lead app; the
others join as their legs land.
"""
from __future__ import annotations

import time
import uuid

import pytest

from common.auth import register_user
from helpers.diagnostics import account_plane_log_lines as _account_plane_log
from helpers.diagnostics import aftermath_log_lines as _aftermath_log
from helpers.diagnostics import row_judge_log_lines as _row_judge_log
from helpers.diagnostics import mail_burn_line as _mail_burn_line
from helpers.diagnostics import remint_line as _remint_line
from helpers.diagnostics import backup_regrant_line as _backup_regrant_line
from helpers.inherited_corpus import CORPUS_READ_S, seed_an_image_under_this_identity
from helpers.succession_ceremony import (
    require_stolen_gate,
    settled_actor_id,
    wait_for_successor_actor,
)
from helpers.succession_retry import assert_the_retry_affordance_matches_the_sweep
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_SECRET_HEX_LEN = 64

# The ceremony tears one session down and launches another over a fresh
# identity: the shared `SUCCESSION_AND_RELAUNCH_S`
# (`helpers/succession_ceremony.py`) covers it. The inherited preferences then
# arrive with the successor's account-plane walk against a nest the app is
# already connected to, which starts concurrently with the post-auth hooks, on
# a heavily loaded shared build machine.
_RESEAL_S = 90.0

# Distinctive enough that finding it after the ceremony cannot be a coincidence
# or a default.
_WITNESS_WORD = "succession-corpus-witness"

# Leg 2 runs in the post-store-ready pass, behind the successor's account
# store coming up and the chain re-point, so its budget covers that plus the
# enroll round trips.
_BACKUP_REGRANT_S = 120.0
# Leg 3 runs inside the replica's own `load()` — a barrier ahead of the restore
# rather than a post-auth hook — so its budget covers the ceremony's relaunch
# plus the plane's launch, on a loaded shared box.
_MLS_RESEAL_S = 120.0
# The adjudication marks are raised by the post-store-ready pass, on the same
# schedule as leg 2, and the Keep that clears one is one load + save + re-read
# against the nest.
_ADJUDICATION_S = 120.0
# How long a re-entered Backups page has to finish READING its destination list
# before a read of that list means anything. Named separately from the budgets
# above because it bounds a different thing: not a leg settling, but one page's
# own async load resolving after a route change (web fetches the list in
# `onMount`). A green re-entry pays only the round trip; the ceiling is generous
# because the alternative is a settle-sleep, which convention 14 forbids.
_LIST_HYDRATION_S = 15.0
# How long a settings sub-page has to paint after its nav edge returns. Short
# by the standards of this file on purpose: it bounds a SKIP decision, not an
# assertion, so an app that genuinely lacks the page should not pay a
# ceremony-sized ceiling to find that out — while an app that has it needs only
# its already-awaited nav-edge read to land.
_PAGE_RENDER_S = 20.0
# The inherited corpus's names and bytes: `fauna.media.list` after the relaunch,
# then the on-appear thumbnail fetch + decrypt for each listed item. Both ride
# the successor's freshly connected client, so this covers a cold list plus a
# blob GET, not just a render.
_CORPUS_READ_S = CORPUS_READ_S

# Leg 6 (the MSEK burn) runs last in the post-store-ready pass — behind the
# successor's account store coming up — and then does two
# revoke RPCs and a degenerate rotation per credential before the rows
# re-render. Same generous ceiling as its siblings; a green run pays none of it
# (convention 14).
_MAIL_BURN_S = 120.0

# Leg 8 (the tier period-key rotation) runs in the post-store-ready pass too —
# the period keys are `fauna.state.subscriptions` rows in the successor's
# account store — and then costs one capability read, one `key_blob.get`, one
# store write and one mint+upload per tier. Same
# generous ceiling as its siblings; a green run pays none of it (convention 14).
_PERIOD_ROTATION_S = 120.0


def _require_recovery_section(app) -> None:
    """Open Recovery Kit, or skip declaring the class if the section never lands.

    Thin wrapper over ``SettingsActions.open_recovery_kit_or_skip`` (lifted
    there 2026-08-22 so every recovery-kit journey test shares one gate);
    kept as a local alias since every call site in this file already reads
    ``_require_recovery_section(app)``.
    """
    app.settings.open_recovery_kit_or_skip()


def _require_member_review_page(app) -> None:
    """Skip, declaring the class, on an app without the permanent review page.

    A third gate, separate from the section's and the driver's, because it is a
    third thing an app can lack: `succession-propagation.md` § Propagation rules
    TWO review surfaces, and an app can render the ephemeral kit-side pass — as
    tui did between 2026-08-10 and 2026-08-17 — while the permanent page that
    holds a deferred backlog does not exist yet.

    Anchors on either of the page's two mutually-exclusive states: a row means a
    backlog, `member-review-empty` means none, and an app with the page always
    shows exactly one of them. Anchoring on the row alone would misread an app
    that HAS the page and legitimately has nothing on it as an app that lacks it.

    ⚠ Polls rather than reading once. A point-in-time read taken while the
    page's nav-edge roster read is still in flight would declare a built page
    unbuilt — a false skip, which is the failure the whole `skip_unbuilt`
    accounting exists to make visible rather than manufacture.
    """

    def _rendered() -> bool:
        return app.is_visible("member-review-row") or app.is_visible(
            "member-review-empty"
        )

    try:
        wait_until(_rendered, _PAGE_RENDER_S, diagnose=lambda: "")
        return
    except Exception:
        pass
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="member_review",
        detail=(
            "succession-propagation.md § Propagation ruling 1, item (iv) — the "
            "permanent review view; tui leads (2026-08-17) and the other six "
            "follow in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/succession-propagation.md "
            "§ Implementation status today (the item-(iv) bullet's Gap)"
        ),
    )


def _require_ephemeral_review_pass(app) -> None:
    """Skip, declaring the class, on an app without the ephemeral kit-side pass.

    A fourth gate: an app can have BOTH the recovery-kit section and the
    permanent review page — `succession-propagation.md` § Propagation rules a
    THIRD surface, the ephemeral pass, and an app can lack exactly it while
    having the other two (every app but tui and web, as of 2026-08-21).

    Anchors on `member-review-defer-button` rather than the row: the row id is
    shared with the permanent page, so its presence alone cannot tell "this
    page renders the ephemeral pass" from "the driver is mid-navigation and
    still showing the last page it was on" — the defer button exists on no
    surface but this one.
    """

    def _rendered() -> bool:
        return app.settings.member_review_defer_visible()

    try:
        wait_until(_rendered, _ADJUDICATION_S, diagnose=lambda: "")
        return
    except Exception:
        pass
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="member_review_ephemeral_pass",
        detail=(
            "succession-propagation.md § Propagation ruling 1, item (ii) — the "
            "ephemeral kit-side review pass; tui and web lead, the other five "
            "follow in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/succession-propagation.md "
            "§ Implementation status today (the item-(ii) bullet's Gap)"
        ),
    )


def _navigate_to_witness_or_skip(app) -> None:
    """Land on the muted-words page, or skip declaring the class.

    ⚠ **This does the navigation itself, on purpose.**
    ``MutedWordsActions.navigate`` ends in a ``wait_for`` that *raises* when the
    page never renders, so a caller that navigated first and gate-checked second
    would ERROR on an app lacking the page instead of skipping — reporting a
    broken test where the truth is an unbuilt surface. Point 7's classes only
    work if the gate is reached.

    ``skip_unbuilt`` (temporary debt; fails under ``--strict-app``) rather than a
    declared absence: every app owes this page.
    """
    try:
        app.muted_words.navigate()
        return
    except Exception:
        pass
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="muted-words",
        detail="moderation.md § Muted keywords — the account-plane witness",
        tracked="docs/goal/behavior/moderation.md § Muted keywords",
    )


# `settled_actor_id` — the Status-page actor-id read that every succession
# journey polls on — lives in `helpers/succession_ceremony.py` since 2026-08-27
# (this file's copy navigated with a plain `.navigate()`; the shared one takes
# the ceremony journey's `_navigate_subpage("status")`, which is what reads the
# Status sub-page on every app). ⚠ `account-actor-id` renders Status-page-only
# (`settings.md` § Live-data placement) — reading it after navigating into
# Account always returns "". That trap cost the journey a session.


def _rendered_actor_id(app) -> str:
    """The signed-in actor id, read off the Status sub-page once it paints.

    ⚠ Never a bare ``app.settings.navigate()`` before the read: on iOS a plain
    settings navigate lands on the root page *list*, where ``account-actor-id``
    does not render, so the read answers "" every time (``SettingsActions.navigate``'s
    iOS carve-out). Three journeys in this file read it that way from 2026-08-03
    and failed on iOS at their first assertion with ``got ''`` — a symptom that
    reads like a login the app never took, and was first recorded as one. ``settled_actor_id`` navigates
    to Status explicitly.

    Polled rather than read once: the sub-page paints after its nav edge
    returns, so a single read can sample a page that is about to render the id
    (convention 14). Returns the last read, "" included, so the caller's own
    assertion owns the message.
    """
    deadline = time.monotonic() + _PAGE_RENDER_S
    actor = settled_actor_id(app)
    while len(actor) != _SECRET_HEX_LEN and time.monotonic() < deadline:
        time.sleep(0.3)
        actor = settled_actor_id(app)
    return actor


@pytest.mark.feature("take-your-account-back")
def test_the_successor_can_still_read_the_config_it_inherited(
    ungranted_app, nest_instance
):
    """write a muted word → succeed → the successor still reads it back.

    Uses ``ungranted_app`` — a dedicated fresh actor — because the ceremony
    re-points the whole account and revokes every session of it; driven against
    the shared session ``test_user`` it would sign every other test out.
    """
    app = ungranted_app

    # ── Precondition: put a preference row at rest, sealed under THIS identity ──
    # Without this the account has no row at rest and the successor's walk has
    # nothing to carry, which would make every assertion below vacuously true.
    _navigate_to_witness_or_skip(app)
    app.muted_words.add(_WITNESS_WORD)
    wait_until(
        lambda: _WITNESS_WORD in app.muted_words.words(),
        _RESEAL_S,
        diagnose=lambda: (
            "the witness must be at rest BEFORE the ceremony, or this test "
            f"proves nothing: words={app.muted_words.words()!r} "
            f"error={app.error_text()!r}"
        ),
    )

    # ── The ceremony ────────────────────────────────────────────────────────
    _run_succession(app)

    # ── The assertion: the inherited corpus is READABLE as the successor ────
    # This is the whole point. Ownership of the row moved inside the nest's
    # succession transaction, but it is still sealed under the retired
    # identity's delegable schedule; the successor's account-plane walk opens
    # it under that schedule and carries it into the successor's own state
    # (succession-aftermath.md § Re-key scope). If that carry did not happen,
    # the list comes back empty.
    #
    # Each poll is a fresh VISIT, never a re-read of one visit's paint. The
    # page reads the list once per visit, and the carry finishes after the
    # actor id switches (which is all `_run_succession` waits for). A single
    # visit that lands between the switch and the walk paints an honest empty
    # list and keeps it — which is what this wait used to poll for 90 s on web,
    # three runs in four.
    wait_until(
        lambda: _WITNESS_WORD in app.muted_words.revisit(),
        _RESEAL_S,
        diagnose=lambda: (
            "the successor must be able to read the config it inherited "
            "(succession-aftermath.md § Re-key scope, the BackupKey corpus row), "
            f"but reads words={app.muted_words.words()!r} "
            f"(expected {_WITNESS_WORD!r}); error={app.error_text()!r}. "
            "An empty list here means the successor's account-plane walk never "
            "carried its attested predecessor's delegable rows: either the "
            "runtime was handed no predecessor schedule, or the walk never "
            "reached the predecessor's rows."
            f"\n  aftermath: {_aftermath_log(app)}"
            f"\n  account runtime: {_account_plane_log(app)}"
        ),
    )

    # And it must be genuinely the successor's, not merely readable once: a
    # write as the successor proves the plane accepts its writes over what it
    # inherited.
    second = f"{_WITNESS_WORD}-after"
    app.muted_words.add(second)
    wait_until(
        lambda: second in app.muted_words.words(),
        _RESEAL_S,
        diagnose=lambda: (
            "a successor must be able to WRITE its preferences, not only read "
            "them. "
            f"words={app.muted_words.words()!r} error={app.error_text()!r}"
        ),
    )
    # The add is a delta against the stored list, so the inherited term rides
    # along: a successor's first write must not replace what it inherited.
    assert _WITNESS_WORD in app.muted_words.words(), (
        "the successor's first write dropped the term it inherited: "
        f"words={app.muted_words.words()!r} error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.feature("take-your-account-back")
def test_the_successors_open_muted_words_page_paints_what_it_inherited(
    ungranted_app, nest_instance
):
    """write a muted word → succeed → enter Muted words ONCE → the word paints.

    The store-change notice's succession case (``account-runtime.md``
    § Multi-instance concurrency → *A runtime's own pump is a source of the
    notice too*): the successor's walk carries its predecessor's delegable rows
    after the actor id switches, so a page entered in those first moments reads
    an honest empty list — and the run that carried them must repaint it. The
    journey above proves the carry by re-visiting; this one never re-visits, so
    it is red on an app whose open page ignores the notice. Marked for the apps
    that consume it AND run the ceremony end to end today (tui, web): linux
    consumes the notice too, but its succession switch does not complete in this
    harness (the journey above is red there at ``_run_succession``), so its leg
    joins when that is green.
    """
    from helpers.fleet import poked_pass
    from helpers.waiting import account_pump_role

    app = ungranted_app
    _navigate_to_witness_or_skip(app)
    app.muted_words.add(_WITNESS_WORD)
    wait_until(
        lambda: _WITNESS_WORD in app.muted_words.words(),
        _RESEAL_S,
        diagnose=lambda: (
            "the witness must be at rest BEFORE the ceremony, or this test "
            f"proves nothing: words={app.muted_words.words()!r} "
            f"error={app.error_text()!r}"
        ),
    )

    _run_succession(app)

    # The ONE visit, right after the switch — never left, never re-entered.
    app.muted_words.navigate()

    def the_open_page_lists_it():
        poked_pass(app.driver, what="the successor's walk")
        try:
            return _WITNESS_WORD in app.muted_words.words()
        except LookupError:
            return False  # a row the list rebuilt away between the count and the read

    wait_until(
        the_open_page_lists_it,
        _RESEAL_S,
        interval=1.0,
        diagnose=lambda: (
            "the successor's OPEN Muted words page never painted the inherited "
            f"{_WITNESS_WORD!r} without a re-visit; pump role (runtime, holder) = "
            f"{account_pump_role(app.driver)}; a re-visit now reads "
            f"{app.muted_words.revisit()!r} (if it lists the word, the carry landed "
            "and the notice never re-drove the open page); "
            f"error={app.error_text()!r}"
            f"\n  aftermath: {_aftermath_log(app)}"
            f"\n  account runtime: {_account_plane_log(app)}"
        ),
    )


def _run_succession(app) -> tuple[str, str]:
    """Mint a kit, run the stolen-identity ceremony, return (old, new) actor.

    Shared by both journeys in this file. Anchors the switch on the rendered
    actor id changing — latency-independent state, never elapsed time.
    """
    # Explicit "status" rather than the bare `app.settings.navigate()`: every
    # caller of this helper up to now entered from OUTSIDE settings, where a
    # bare navigate's "settings-tab" click lands fresh on the default (Status)
    # sub-page. Both journeys in this file enter from an ALREADY-open settings
    # sub-page (nostr) — re-clicking "settings-tab" while already inside
    # settings is a no-op on the sidebar-swap shells, leaving the sub-page on
    # nostr instead of resetting to Status (found running this journey on
    # linux, row 319: `account-actor-id` never appeared, not even after a
    # generous poll — a real navigation gap, not a render race). The explicit
    # form is the one `settings.navigate()`'s own docstring already prescribes
    # for exactly this "must land on Status" need (see its iOS carve-out), and
    # `_rendered_actor_id` is where this file takes it.
    old_actor = _rendered_actor_id(app)
    assert len(old_actor) == _SECRET_HEX_LEN, (
        "the Status page must render the signed-in actor id before the "
        f"ceremony, so the switch is observable; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    _require_recovery_section(app)
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )

    # Re-enter so the ceremony runs from a clean render rather than over a
    # freshly minted secret.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)

    new_actor = wait_for_successor_actor(app, old_actor)
    assert new_actor != old_actor, (
        "the account must belong to a freshly minted identity before the "
        "aftermath assertion means anything"
    )

    # Every succession this file runs also settles the sweep RETRY's render
    # gate, free (`settings.md` § Recovery kit → *Finishing an unfinished group
    # sweep*): the button is on screen exactly when the report owes work, and
    # where it is, the press answers in words. Asserted HERE — in the shared
    # ceremony driver, not in one journey — so it rides every aftermath journey
    # on every app, which is what covers the apps whose own closing-act leg is
    # still unbuilt and that therefore cannot reach the end of the sibling
    # ceremony file's kit-holder journey (web today; identity-succession.md § The
    # RecoveryKey → *At succession* owns that gap).
    sweep = app.driver.get_state("data.succession_sweep")
    if sweep is not None:
        assert_the_retry_affordance_matches_the_sweep(app, sweep)

    return old_actor, new_actor


@pytest.mark.feature("take-your-account-back")
def test_the_successors_backups_restart_without_a_user_command(
    ungranted_app, nest_instance, second_nest
):
    """enroll a backup destination → succeed → backups run again, unprompted.

    § Re-key scope's ``NestBackupKey`` row: *"Old grant revoked; successor
    derives + grants the new one."* The nest holds up the first half in the
    succession transaction; this proves the client holds up the second at the
    successor's first sign-in, with the user issuing no command at all.
    """
    app = ungranted_app
    app.backups.require_destination_management_supported()

    # A v1 destination is a nest the owner *administers*, so the enroll's
    # destination-side handshake only succeeds once this owner's identity is
    # registered there — a stranger is correctly rejected (`require_registration`
    # is the nest default). Fixture setup arranging a precondition, which is
    # outside convention 8's UI-mutation rule; the enrollment itself below is
    # driven through the UI. Shape borrowed from `test_backup_destination_crud`.
    owner_actor = _rendered_actor_id(app)
    assert len(owner_actor) == _SECRET_HEX_LEN, (
        "the owner's actor id must be readable before registering it on the "
        f"destination nest; got {owner_actor!r}, error={app.error_text()!r}"
    )
    try:
        register_user(
            second_nest["port"],
            owner_actor,
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        # Already registered — the destination nest is session-scoped.
        pass

    # ── Precondition: a real enrollment, made through the ordinary UI ───────
    # Convention 8: the mutation under test is a user's, not an RPC shortcut.
    # This is the pre-ceremony witness — without it the successor has no
    # destinations, leg 2 answers `NothingConfigured`, and every assertion
    # below is vacuously true.
    app.backups.navigate()
    app.backups.add_destination(second_nest["url"], name="Aftermath")
    wait_until(
        lambda: app.backups.destination_count() >= 1,
        _BACKUP_REGRANT_S,
        diagnose=lambda: (
            "the destination must be enrolled BEFORE the ceremony or this test "
            f"proves nothing: count={app.backups.destination_count()} "
            f"error={app.error_text()!r}"
        ),
    )

    _run_succession(app)

    # ── The assertion ───────────────────────────────────────────────────────
    # ⚠ Read the Settings progress line, and do NOT navigate to Backups first:
    # `read_backup_status` heals a missing enrollment on mount, so a Backups
    # visit would repair exactly what this test came to measure and the
    # assertion would pass against a hook that never ran.
    def _regrant_line() -> str:
        try:
            app.settings.open_recovery_kit()
            return app.settings.aftermath_backup_regrant_status()
        except Exception:
            return ""

    wait_until(
        lambda: _regrant_line() == S.settings.recovery_kit.backup_regrant_done,
        _BACKUP_REGRANT_S,
        diagnose=lambda: (
            "the successor's post-auth hook must re-grant the NestBackupKey and "
            "re-register the destinations (succession-aftermath.md § Re-key "
            f"scope, the NestBackupKey row), but the line reads "
            f"{_regrant_line()!r} (expected "
            f"{S.settings.recovery_kit.backup_regrant_done!r}); "
            f"error={app.error_text()!r}. An empty line means leg 2 never ran "
            "at all — the successor is absent from the nest's backup sweep, so "
            "nothing of theirs is being backed up anywhere."
        ),
    )

    # Belt and braces, and only now that the load-bearing assertion has passed:
    # the destination the predecessor enrolled is the successor's again.
    app.backups.navigate()
    wait_until(
        lambda: app.backups.destination_count() >= 1,
        _BACKUP_REGRANT_S,
        diagnose=lambda: (
            "the inherited destination must project for the successor: "
            f"count={app.backups.destination_count()} error={app.error_text()!r}"
        ),
    )


def _reopen_backups(app) -> None:
    """Leave the Backups page and come back, forcing a fresh read from the nest.

    Entering Backups re-reads the ``fauna.state.backup`` destinations (``app.rs`` —
    ``nav_enter_op``), so this is what makes an assertion after it a statement
    about what is **at rest on the nest** rather than about a projection the app
    happened to be holding. Navigating to Backups while already on Backups is
    not guaranteed to re-fire the page's enter hook, so the round trip goes via
    another page on purpose.

    ⚠ **Coming back is not the same as having read**, and the gap between the two
    is wide enough to invert a conclusion. `navigate` returns on the route change;
    web then fetches the list in `onMount` (`+page.svelte` — `loadDestinations`),
    so a `destination_count()` immediately afterwards samples the DOM *before* the
    read resolves and answers 0. The page is not lying while that is true — it
    paints neither rows nor the empty-state line until the read lands
    (`destinations.length === 0 && destinationsHydrated`) — but a caller reading
    through it cannot tell "not yet" from "nothing there", and a poll that
    re-enters on every tick can sample early on *every* tick and never once see a
    list that is in fact present.

    That is not hypothetical: it is what made 2026-08-21 record leg (b) as a carry-across break — `count=0` with an empty
    `error` and a re-entry that provably succeeded — while the sibling journey
    (`..._backups_restart_without_a_user_command`), which navigates **once** and
    then polls the count patiently, asserted the same row on the same page and
    passed in the same run.

    So the re-entry ends by waiting for the list to hydrate. Deliberately a soft
    wait: it makes no assertion of its own — a genuinely empty list costs the
    budget and the *caller's* assertion is what fails, with its own diagnostic.
    """
    app.settings.navigate()
    app.backups.navigate()
    _await_destination_list(app)


def _await_destination_list(app) -> None:
    """Wait for a freshly-entered Backups page to finish reading its list.

    A deadline poll on observable state, never a settle-sleep (convention 14):
    it returns the moment a row is painted. Returns quietly on timeout by design
    — see `_reopen_backups` for why this must not assert.
    """
    deadline = time.monotonic() + _LIST_HYDRATION_S
    while time.monotonic() < deadline:
        if app.backups.destination_count() >= 1:
            return
        time.sleep(0.3)


@pytest.mark.feature("take-your-account-back")
def test_the_successor_is_asked_about_the_destination_it_inherited(
    ungranted_app, nest_instance, second_nest
):
    """enroll a destination → succeed → the row is raised for review, and Keep
    settles it at rest.

    § Re-key scope → *Adjudicating what the aftermath carries across*: the
    aftermath re-registers every carried-across destination **immediately**
    (backups restarting must not become user-gated) and then *reports* each row
    until the owner keeps or removes it. The sibling test above proves the first
    half — backups restart unprompted. This proves the second, which is the half
    that exists because the seed thief the whole ceremony answers could have
    added a ``backup.destinations`` row pointing at a box of their choosing,
    indistinguishable from one the owner added.

    **What only a full-stack run can establish.** The mark's write point, its
    projection and ``keep_backup_destination`` are each pinned in-crate against
    fakes, and tui pins that a raised row paints the pair and an ordinary row
    paints neither. What none of them can reach is the composition: that the
    mark the post-store-ready pass raises survives the round trip through a real
    nest's account store, that the successor's Backups page — which the
    predecessor's row reaches only after the re-registration *and* the page's
    own enrollment heal — renders it, and that the Keep press lands
    back at rest rather than only in the projection the app is holding.

    ⚠ **Unlike leg 2's journey, this one MAY visit the Backups page**, and must.
    The trap that journey states — ``read_backup_status`` heals a missing
    enrollment on mount, so a visit repairs what it came to measure — does not
    apply here: the heal re-issues the nest-side grant and registrations and
    writes no ``fauna.state.backup`` row at all (``backup_enroll.rs`` — it takes
    ``&cfg.backup.destinations`` and never saves), so it can neither raise nor
    clear a mark. The mark IS what is asserted, and the page is where a user
    meets it.

    ⚠ **The pre-ceremony enrollment is the whole test**, exactly as the muted
    word is above: the destination raise marks the rows that are *there*, so a succession over
    an owner with no destinations raises nothing and every assertion below would
    be vacuously true. The count assertion before the mark poll is what keeps
    that honest.

    ⚠ **Absence, not emptiness, is the un-raised state** — the mark element is
    not rendered at all on an ordinary row (``ui.yaml``'s registry entry), so the
    post-Keep assertion polls visibility, never text.
    """
    app = ungranted_app
    app.backups.require_destination_management_supported()

    # Fixture setup arranging a precondition (outside convention 8's UI-mutation
    # rule), same shape as the leg-2 journey above: a v1 destination is a nest
    # the owner administers, so the enroll's handshake only succeeds once this
    # owner is registered there.
    owner_actor = _rendered_actor_id(app)
    assert len(owner_actor) == _SECRET_HEX_LEN, (
        "the owner's actor id must be readable before registering it on the "
        f"destination nest; got {owner_actor!r}, error={app.error_text()!r}"
    )
    try:
        register_user(
            second_nest["port"],
            owner_actor,
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        # Already registered — the destination nest is session-scoped.
        pass

    # ── Precondition: a real enrollment, made through the ordinary UI ───────
    app.backups.navigate()
    app.backups.add_destination(second_nest["url"], name="Adjudication")
    wait_until(
        lambda: app.backups.destination_count() >= 1,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the destination must be enrolled BEFORE the ceremony or this test "
            f"proves nothing: count={app.backups.destination_count()} "
            f"error={app.error_text()!r}"
        ),
    )
    # It must NOT be raised yet: the mark means "carried across a succession",
    # and no succession has happened. Without this the post-ceremony assertion
    # would pass against a client that renders the pair unconditionally.
    assert not app.backups.destination_unattested_mark_visible(0), (
        "a destination the owner just added themselves must not be raised for "
        "review — the mark would then be permanent furniture and would train "
        "the user straight past the one succession that matters. mark reads "
        f"{app.backups.destination_unattested_mark_text(0)!r}"
    )
    assert not app.backups.destination_keep_visible(0), (
        "Keep renders only beside a raised mark; an ordinary row offering it "
        "means the pair is unconditional"
    )

    _run_succession(app)

    # ── Barrier: let the aftermath SETTLE before reading its product ────────
    # The pass raises the mark and leg 2 re-registers the row; until both have run
    # there is nothing on the Backups page to find, and polling that page for
    # something the account has not produced yet cannot distinguish "not yet"
    # from "never". The sibling journey
    # (`..._backups_restart_without_a_user_command`) waits on exactly this line
    # before it reads the same page, and passes; this journey polled Backups
    # from the instant the ceremony returned, and read the two progress lines
    # only in its own failure diagnostic — by which time both said *done*,
    # which is precisely the reading that cannot be had from inside the poll.
    #
    # A causal barrier on the account's own published progress, not a settle
    # sleep (convention 14): it returns the moment leg 2 reports.
    wait_until(
        lambda: _backup_regrant_line(app) == S.settings.recovery_kit.backup_regrant_done,
        _BACKUP_REGRANT_S,
        diagnose=lambda: (
            "the aftermath must settle before its adjudication surface can be "
            "read: leg 2's line reads "
            f"{_backup_regrant_line(app)!r} (expected "
            f"{S.settings.recovery_kit.backup_regrant_done!r}); "
            f"error={app.error_text()!r}"
        ),
    )

    # ── The mark: the successor is ASKED about the row it inherited ─────────
    # The destination raise marks every destination the ceremony carried
    # across, off the parked ceremony. The poll re-enters
    # the page each time so it is reading the nest, not a held projection.
    # ⚠ The swallowed navigation error is REPORTED, not discarded (convention 6).
    # `count=0` in the diagnostic below is ambiguous on its own: it means the
    # page shows no destination row, which is equally true when the re-entry
    # threw and left the app on some other page, where the id cannot appear at
    # all. That ambiguity cost a reader a wrong conclusion on 2026-08-21 — the
    # count was read as "the successor's list came back empty" when the
    # evidence could not carry it. So the last failure is kept and printed.
    last_nav_error: list[str] = []
    # ⚠ The HIGH-WATER row count across the whole poll, and it exists because a
    # single sample is what made this journey mis-diagnose itself twice.
    # `diagnose` runs once, after the budget is spent, so a bare
    # `destination_count()` there reports whatever THAT last tick happened to
    # see — and the first mount after a ceremony legitimately sees zero (the
    # aftermath is a head start, not a barrier: `+layout.svelte` says so, and
    # account-plane readers self-heal on the nav edge). Twice that arbitrary `0`
    # was read as "the successor's list came back empty", once into the
    # tracker and once into `succession-aftermath.md` as fact;
    # both times the row was present and the real gap was the unbuilt mark.
    # A maximum cannot tell that story: if the row was EVER listed, it says so.
    best_count = [0]

    def _raised() -> bool:
        try:
            _reopen_backups(app)
        except Exception as e:  # noqa: BLE001 — reported, see above
            last_nav_error.append(f"{type(e).__name__}: {e}")
            return False
        last_nav_error.clear()
        try:
            best_count[0] = max(best_count[0], app.backups.destination_count())
        except Exception:  # noqa: BLE001 — the mark read below is what reports
            pass
        try:
            return app.backups.destination_unattested_mark_visible(0)
        except Exception as e:  # noqa: BLE001
            last_nav_error.append(f"reading the mark: {type(e).__name__}: {e}")
            return False

    wait_until(
        _raised,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the successor's Backups page must raise the inherited destination "
            "for review (succession-aftermath.md § Re-key scope → Adjudicating "
            "what the aftermath carries across), but the row paints no mark: "
            f"count={app.backups.destination_count()} "
            # The high-water mark beside the last sample. They disagree exactly
            # when the count is noise rather than evidence — believe this one.
            f"(best seen across the poll: {best_count[0]}) "
            f"mark={app.backups.destination_unattested_mark_text(0)!r} "
            f"keep={app.backups.destination_keep_visible(0)} "
            f"error={app.error_text()!r}; "
            # Which of the two `count=0` stories is true. Empty means the page
            # WAS re-entered and genuinely lists nothing; non-empty means the
            # count says nothing about the destination list.
            f"last re-entry {last_nav_error[-1] if last_nav_error else 'succeeded'}; "
            # ⚠ **The regrant line below is not colour — it is the strongest
            # single fact in this message, and reading it as colour cost two
            # sessions.** `backup_regrant_done` renders for EXACTLY one outcome,
            # `BackupRegrantOutcome::Regranted`, which is unreachable past the
            # `destinations.is_empty()` early return in
            # `regrant_nest_backup_key`. So whenever that line reads *done*, the
            # successor's own config read had ALREADY returned a non-empty
            # destination list — and no `count=0` sampled here can mean "the row
            # is gone at rest", however settled leg 2 is. (A comment here once
            # said the opposite; it was wrong, and it propagated into
            # `succession-aftermath.md` before a probe refuted it 2026-08-21.)
            # Trust order in this message: the regrant line, then the high-water
            # count, then the last sample.
            f"backup-regrant leg reads {_backup_regrant_line(app)!r}. An unraised row means the owner is "
            "never asked about a destination a seed thief could have added — "
            "it just keeps receiving their backups."
        ),
    )
    assert app.backups.destination_count() >= 1, (
        "the inherited destination must still be listed — a mark on a row that "
        "vanished is not an adjudication surface"
    )
    assert (
        app.backups.destination_unattested_mark_text(0)
        == S.backups.backup_destination_unattested_mark
    ), (
        "the review copy must be the ratified string — it must read as a "
        "review prompt and must not accuse, because after a recovery almost "
        "every row is the owner's own. got "
        f"{app.backups.destination_unattested_mark_text(0)!r}"
    )
    assert app.backups.destination_keep_visible(0), (
        "a raised row must offer Keep — without it the only way to clear the "
        "mark is to remove a destination the owner may well want to keep"
    )

    # ── Keep settles it, and settles it AT REST ────────────────────────────
    app.backups.keep_destination(0)
    wait_until(
        lambda: not _raised(),
        _ADJUDICATION_S,
        diagnose=lambda: (
            "pressing Keep must clear that row's review mark "
            "(keep_backup_destination writes the cleared config back and the "
            "page re-reads it), but the row still reads "
            f"{app.backups.destination_unattested_mark_text(0)!r}; "
            f"error={app.error_text()!r}"
        ),
    )
    # Keep is not Remove: the row stays, and stays removable forever after.
    assert app.backups.destination_count() >= 1, (
        "Keep must leave the destination enrolled — it closes the raising "
        "event, it does not retire the row"
    )
    # And the clearing is at rest, not local: this read comes back off the nest.
    #
    # ⚠ Deliberately scoped to THIS device's re-read, and no wider — but the
    # reason has changed twice, so do not re-copy the old one. The plane now
    # carries the owner's verdict at rest (`unattested_destination_marks`,
    # lifted 2026-08-11) and its writers ride the CAS + merge path, so the
    # cross-device property is real code with in-crate two-device pins
    # (`fauna-client-config` `backup_enroll.rs`:
    # `a_concurrent_removal_is_not_resurrected_by_this_devices_write`,
    # `a_keep_and_a_concurrent_removal_both_survive`). What is still missing is
    # a *journey* witness — a second seat against this nest — which no
    # single-seat test can supply. That is the declared open
    # gap, and this assertion is a
    # statement about persistence only.
    _reopen_backups(app)
    assert not app.backups.destination_unattested_mark_visible(0), (
        "the adjudication must survive a re-read from the nest — a Keep that "
        "lives only in the projection the app is holding would re-raise the "
        "row on the owner's very next sign-in"
    )
    assert not app.backups.destination_keep_visible(0), (
        "Keep must retire with its mark; a Keep button beside no mark is an "
        "affordance for a question nobody is being asked"
    )


@pytest.fixture
def grant_mint_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest whose ADMIN identity this test succeeds.

    **The reason it cannot ride ``nest_instance`` + ``ungranted_app`` the way its
    destination-plane sibling does:** the ceremony **re-points the signed-in
    identity's account**, and the identity that mints here is this nest's admin.
    On a dedicated nest that is contained; on the shared session nest it would
    re-point the nest every other test in the run is talking to.

    ⚠ **The admin login itself is no longer the reason — that claim was stale.**
    This fixture used to justify itself first by "minting a trust is admin-gated:
    holder discovery (``list_service_users``) is admin-only in v1", citing
    ``nests.md`` § Known gap. That gap **CLOSED 2026-07-17** (``ui/nests.md``
    § Implementation status today → *Non-admin holder discovery*: the kind is now
    ``User | Admin`` with a class-scoped reply, and a plain user's mint→revoke
    path is pinned by ``conformance_capability_trust_client::a_plain_user_
    discovers_holders_and_mints_then_revokes``). Being admin is now **incidental**
    — the first identity to claim a fresh nest is its admin either way — so
    nothing here depends on it. Left as-is deliberately rather than converted to a
    plain-user mint: nobody has run this journey that way, and re-pointing the
    admin on a nest that is ours alone costs nothing, so the change would be
    unverified churn on a live journey. Reason enough to revisit if this fixture
    ever needs to share a nest.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-grant-nest")
    yield nest
    cleanup()


def _reopen_nests(app) -> None:
    """Leave the Nests page and come back, forcing a fresh read.

    The trust facet folds its rows out of the owner's ``fauna.state.succession-ledger`` grant log on
    entry, so the round trip is what makes an assertion after it a statement
    about what is at rest rather than about a held projection. Via another page
    on purpose: re-navigating to the page you are on is not guaranteed to
    re-fire its enter hook.
    """
    app.settings.navigate()
    app.linked_nests.navigate()


@pytest.mark.feature("take-your-account-back")
def test_the_successor_is_asked_about_the_trust_it_inherited(app, grant_mint_nest):
    """mint a trust → succeed → the row is raised for review, and Keep settles it.

    The **stricter** of the two adjudication planes, and the reason § Re-key
    scope → *Adjudicating what the aftermath carries across* rules the mark
    mandatory here rather than defense-in-depth: a backup destination a thief
    added still only ever receives segments sealed under a key it lacks, whereas
    a thief-added grantee is handed live **read** capability by the successor's
    own client, at the moment leg 4 re-mints the ledger it inherited.

    **What only a full-stack run can establish.** ``mark_carried_across_succession``
    stamping the live grants, ``GrantUnattestedMark::any_open``,
    ``project_grant``'s join and ``keep_grant_mark``'s record-don't-delete rule
    are each pinned in-crate against fakes, and tui pins that a marked row paints
    the pair. What none of them reaches is that the mark survives leg 4 itself:
    the re-mint **derives a replacement grant id** (successor secret ∥ old id, so
    racing devices converge) and moves each mark onto it, so the row a successor
    sees is not the row that was stamped. A join that silently missed would leave
    every inherited trust unraised while every unit test stayed green.

    ⚠ **The pre-ceremony mint is the whole test** — the grant-mark raise marks
    the grants that are *live* at the ceremony, so a succession over an owner who has granted
    nothing raises nothing and every assertion below would be vacuously true.

    ⚠ **Absence, not emptiness, is the un-raised state**, exactly as on the
    destination plane, so the post-Keep assertion polls visibility, never text.

    ⚠ **Copy check deliberately omitted here.** The rendered text must use trust
    vocabulary and never say "capability"/"grant" (``participants.md`` § Naming)
    — a rule tui already pins on the rendered string in ``settings/nests.rs``,
    which is the right altitude for it. Re-asserting the exact sentence here
    would only duplicate that pin one layer up.
    """
    app.nest_trust.require_mint_test_setup_supported()

    # Sign in as the dedicated nest's admin: Admin ⊇ User covers the tier-create
    # and the mint, and `apply_session_patch` puts the seed in the account
    # registry, which is what lets the ceremony below run from this session at
    # all (`registry.add_account`, then `establish`).
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, grant_mint_nest)

    admin = grant_mint_nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": grant_mint_nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": "test-device-succession-grant",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })

    # ── Precondition: a real grant, minted through the ordinary UI ──────────
    tier = f"gold-{uuid.uuid4().hex[:6]}"
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"the tier the mint picker's derivability filter reads must exist; "
        f"error={subs.error_text()!r}"
    )

    app.linked_nests.navigate()
    assert app.nest_trust.mint_button_present(), (
        "the mint affordance must be present (an enrolled holder + a held "
        f"tier); error={app.error_text()!r}"
    )
    app.nest_trust.open_mint()
    app.nest_trust.select_scope(app.nest_trust.paywalled_label(tier))
    app.nest_trust.confirm_mint()
    assert app.nest_trust.wait_for_grant_count(1), (
        "the trust must be live BEFORE the ceremony or this test proves "
        f"nothing; error={app.error_text()!r}"
    )
    assert not app.nest_trust.grant_unattested_mark_visible(0), (
        "a trust the owner just minted themselves must not be raised for "
        "review — a permanently-rendered mark would train the user straight "
        "past the one succession that matters. mark reads "
        f"{app.nest_trust.grant_unattested_mark_text(0)!r}"
    )

    _run_succession(app)

    # ── The mark: the successor is ASKED about the trust it inherited ───────
    def _raised() -> bool:
        try:
            _reopen_nests(app)
            return app.nest_trust.grant_unattested_mark_visible(0)
        except Exception:
            return False

    wait_until(
        _raised,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the successor's Nests page must raise the inherited trust for "
            "review (succession-aftermath.md § Re-key scope → Adjudicating what "
            "the aftermath carries across), but the row paints no mark: "
            f"grants={app.nest_trust.grant_count()} "
            f"mark={app.nest_trust.grant_unattested_mark_text(0)!r} "
            f"keep={app.nest_trust.grant_keep_visible(0)} "
            f"error={app.error_text()!r}; "
            # The Settings line comes last: `_settings_line` navigates, and
            # `error-message` is per-page.
            # ⚠ On a shell that paints no `recovery-kit-*-status` element the
            # line answers `""` unconditionally, so it cannot separate "leg 4
            # is broken" from "no leg ran". The log can: `notASuccessor` there
            # means the predecessor link is missing and no leg ran.
            f"aftermath log says {_aftermath_log(app)}; "
            f"remint leg reads {_remint_line(app)!r}. A grant count of 0 means leg 4 never "
            "re-minted the ledger at all; a non-zero count with no mark means "
            "the re-mint dropped the mark on the way to the replacement id, so "
            "a grantee a thief added is silently handed live read capability."
        ),
    )
    assert app.nest_trust.grant_count() >= 1, (
        "the inherited trust must still be listed — a mark on a row that "
        "vanished is not an adjudication surface"
    )
    assert app.nest_trust.grant_keep_visible(0), (
        "a raised row must offer Keep — without it the only way to clear the "
        "mark is to revoke a trust the owner may well want to keep"
    )

    # ── Keep settles it, and settles it AT REST ────────────────────────────
    app.nest_trust.keep_grant(0)
    wait_until(
        lambda: not _raised(),
        _ADJUDICATION_S,
        diagnose=lambda: (
            "pressing Keep must settle that row's review mark (keep_grant_mark "
            "records a Kept verdict and the facet re-folds), but the row still "
            f"reads {app.nest_trust.grant_unattested_mark_text(0)!r}; "
            f"error={app.error_text()!r}"
        ),
    )
    # Keep is not Revoke: the trust stays, and stays revocable forever after.
    assert app.nest_trust.grant_count() >= 1, (
        "Keep must leave the trust live — it closes the raising event, it does "
        "not revoke the row"
    )
    # And the verdict is at rest, not local. This is the half the grant plane
    # was lifted onto the member plane's verdict-at-rest shape for: a Keep that
    # only deleted the mark locally would be re-raised by an ordinary sign-in on
    # the owner's second device.
    _reopen_nests(app)
    assert not app.nest_trust.grant_unattested_mark_visible(0), (
        "the adjudication must survive a re-read — a Keep that lives only in "
        "the projection the app is holding would re-raise the row on the "
        "owner's next sign-in and on every other device they own"
    )
    assert not app.nest_trust.grant_keep_visible(0), (
        "Keep must retire with its mark; a Keep button beside no mark is an "
        "affordance for a question nobody is being asked"
    )


@pytest.mark.feature("take-your-account-back")
@pytest.mark.real_conversations
def test_the_successors_first_session_raises_the_member_it_cannot_vouch_for(
    ungranted_app, nest_instance
):
    """hold a real group → succeed → the successor is asked about its member.

    § Propagation → *MLS groups*, item 3a: the sweep reports every member it can
    vouch for **nothing** about, the aftermath writes that roster down, and the
    review surfaces render it. The two sibling journeys either side of this one
    prove the ceremony's own halves (the group is re-pointed; the inherited
    preferences come back). Neither says anything about the roster, and the roster is the
    half a compromised user actually acts on: the ceremony evicts the *credential
    the thief stole*, and is structurally unable to evict an identity the thief
    seated in the group under a second name. That population is exactly what the
    mark exists to surface.

    **What only a full-stack run can establish.** The store, the projection and
    the collapse are pinned in-crate; tui pins that a person under review paints
    the pair and an ordinary member paints neither. What none of them reaches is
    the composition, which is four components deep and crosses the account
    switch: the sweep must run over a **live engine** and record a roster, that
    roster must survive the switch (``succession_sweep`` is one of two fields the
    authenticated-state teardown preserves), the post-store-ready pass must
    write it into the successor's succession ledger, and the
    conversations plane must then read it back and render it against a thread the
    successor reaches through its own re-sealed ``__mls`` replica. Every seam
    there is a place the roster can arrive empty while every unit test stays
    green.

    ⚠ **The pre-ceremony group is the whole test.** A succession over an actor
    who holds no groups sweeps nothing, reports nobody and raises no items — the
    ``NothingToRaise`` arm — and every assertion below would be vacuously true.
    The real ``FaunaMlsBackend`` and the API-tier peer are what make the roster
    non-empty; the sweep-report assertion before the mark poll keeps it honest.

    ⚠ **Read the thread before blaming the mark.** The successor reaching its own
    inherited thread at all is a precondition of rendering anything on its member
    chips, so it is asserted separately and first — otherwise a plane that came
    up empty would present as "the review was never raised".
    """
    from tests.api import conv_api
    from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

    app = ungranted_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Skip-gates first, before any nest-side setup: an app without the driver
    # must report "not built" rather than fail on a backend it also lacks.
    old_actor = _rendered_actor_id(app)
    _require_recovery_section(app)
    require_stolen_gate(app)
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the pre-ceremony actor id must render; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )

    # The REAL conversations backend: the sweep reports `NoEngine` without it,
    # `succession_review_roster` answers empty for that arm by construction, and
    # this test would prove nothing. The sweep assertion below is what keeps
    # that from passing silently.
    app.conversations.enable_real_faunamls()

    # An API-tier peer with REAL key packages — the real backend parses each one
    # during bootstrap. bob holds no MLS engine of his own here; he exists to
    # make the group real and to be the member the sweep cannot classify.
    bob = conv_api.reachable_peer(port, admin_sk, old_actor)

    app.conversations.real_resolve_send_new(bob["actor_id_hex"], "before the theft")
    thread = _wait_thread_snippet(app, "before the theft", timeout=30.0)
    assert thread is not None, (
        "the group must exist before the ceremony — a succession over zero "
        f"groups raises nobody; error: {app.error_text()!r}"
    )
    thread_id = thread.thread_id

    _run_succession(app)

    # ── The sweep really did observe somebody ───────────────────────────────
    # Read from the report that rode across the switch, not from the screen:
    # its rendered form is deliberately ID-less prose.
    sweep = app.driver.get_state("data.succession_sweep")
    assert sweep is not None and sweep.get("status") == "ran", (
        "the sweep did not run over a live engine, so no roster was observed "
        f"and this test would prove nothing about the review; got {sweep!r}"
    )

    # ── The successor reaches its inherited thread ──────────────────────────
    def _thread_visible() -> bool:
        try:
            app.conversations.navigate()
            return any(
                t.thread_id == thread_id for t in app.conversations.list_threads()
            )
        except Exception:
            return False

    wait_until(
        _thread_visible,
        _MLS_RESEAL_S,
        diagnose=lambda: (
            "the successor must reach the group it inherited before anything "
            "can be rendered on its member chips — this is leg 3's plane "
            "(succession-aftermath.md § Re-key scope, the BackupKey corpus row's "
            f"__mls half), and it reads mls={app.settings.aftermath_mls_reseal_status()!r} "
            f"error={app.error_text()!r}"
        ),
    )

    # ── The review is raised on the member the ceremony could not classify ──
    def _flagged() -> bool:
        try:
            app.conversations.navigate()
            app.conversations.open_thread_by_id(thread_id)
            return app.conversations.any_member_unattested_mark_visible()
        except Exception:
            return False

    wait_until(
        _flagged,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the successor's first session must raise the members the sweep "
            "could vouch for nothing about (identity-succession.md "
            "§ Propagation → MLS groups, item 3a), but no member chip carries "
            f"the mark: chips={app.conversations.member_chip_count()} "
            f"sweep={app.driver.get_state('data.succession_sweep')!r} "
            f"error={app.error_text()!r}. An unraised roster means the identity "
            "a thief seated in this group — the one population the credential "
            "eviction structurally cannot reach — is never surfaced to the "
            "owner at all."
        ),
    )

    flagged_at = next(
        i
        for i in range(app.conversations.member_chip_count())
        if app.conversations.member_unattested_mark_visible(i)
    )
    assert (
        app.conversations.member_unattested_mark_text(flagged_at)
        == S.conversations.detail.member_unattested_mark
    ), (
        "the review copy must be the ratified string; got "
        f"{app.conversations.member_unattested_mark_text(flagged_at)!r}"
    )

    # ── The EPHEMERAL pass renders in the SAME session, on the Recovery Kit
    # section — the second of § Propagation's three renderings, and the one
    # unique to "a sweep ran THIS run" rather than "an item happens to be
    # open". `open_recovery_kit` is the
    # Account sub-page's own entry, the same one `_run_succession` used above.
    app.settings.open_recovery_kit()
    _require_ephemeral_review_pass(app)
    wait_until(
        lambda: app.settings.member_review_row_count() > 0,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the ephemeral pass must render on the Recovery Kit section during "
            "the successor's first session — the roster is non-empty on the "
            f"chip, but rows={app.settings.member_review_row_count()} here; "
            f"error={app.error_text()!r}"
        ),
    )
    ephemeral_rows = app.settings.member_review_row_count()
    assert app.settings.member_review_row_text(0).strip(), (
        "the ephemeral pass's row must name the same person the chip does; "
        f"got {app.settings.member_review_row_text(0)!r}"
    )

    # Defer hides the pass and decides nothing — the item must still be open
    # afterward, on the PERMANENT page, proving defer clears the ephemeral
    # surface and never the roster.
    app.settings.member_review_defer()
    wait_until(
        lambda: app.settings.member_review_row_count() == 0
        and not app.settings.member_review_defer_visible(),
        _ADJUDICATION_S,
        diagnose=lambda: (
            "Review The Rest Later must hide the ephemeral pass; it still "
            f"shows {app.settings.member_review_row_count()} row(s), defer "
            f"visible={app.settings.member_review_defer_visible()}, "
            f"error={app.error_text()!r}"
        ),
    )

    # ── The PERMANENT page holds the same roster, and answering there closes it ──
    #
    # The sibling above proves the roster reaches the *member chip*, the
    # rendering § Propagation calls the load-bearing one. This proves the
    # rendering that answers the other question — *what is left to review?* —
    # and it is a different composition: a page whose whole content is the
    # succession-ledger roster, read on its own nav edge rather than off a thread the
    # conversations plane already loaded.
    #
    # ⚠ A skip here cannot cost the assertions above their run: every app
    # without this page also lacks the recovery-kit section, and would have
    # skipped at `_require_recovery_section` before the ceremony. The gate is
    # for the trickle-down window in which an app has the section and the
    # succession driver but not yet the permanent view.
    app.settings.open_member_review()
    _require_member_review_page(app)

    wait_until(
        lambda: app.settings.member_review_row_count() > 0,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the same roster the member chip renders must be answerable from "
            "the permanent page — that page is where a deferred backlog lives, "
            "and a backlog nobody can reach is what item (iv) was ratified to "
            f"fix; rows={app.settings.member_review_row_count()} "
            f"empty={app.settings.member_review_is_empty()} "
            f"error={app.error_text()!r}"
        ),
    )
    opened = app.settings.member_review_row_count()
    assert not app.settings.member_review_is_empty(), (
        "the empty line and the rows are mutually exclusive; both painted with "
        f"{opened} row(s) open"
    )
    assert app.settings.member_review_row_text(0).strip(), (
        "a row that names nobody is a row nobody can act on; got "
        f"{app.settings.member_review_row_text(0)!r}"
    )
    # Defer must clear the EPHEMERAL surface alone — the roster itself is
    # untouched, so the permanent page inherits exactly what was deferred,
    # neither fewer (a lost item) nor more (a duplicate raise).
    assert opened == ephemeral_rows, (
        f"the permanent page shows {opened} row(s) after defer, but the "
        f"ephemeral pass showed {ephemeral_rows} before it — defer must not "
        "change the roster, only which surface renders it"
    )

    # Both verdicts are offered on every row, each scoped INSIDE the row it
    # decides — so a press always lands on the person that row names. This is
    # the ID-level half of the success definition, asserted per row rather than
    # globally, because a global count cannot tell one row's pair from another's.
    for index in range(opened):
        assert app.settings.member_review_row_has_both_verdicts(index), (
            f"row {index} must offer Keep and Remove, both scoped inside it; "
            "an unscoped pair acts on whichever person happens to be first"
        )

    # Answering here must actually close the item — the round trip no unit test
    # reaches: decide → a succession-ledger write in the successor's account
    # store → re-read → re-render. Keep rather than Remove, deliberately:
    # Remove's verdict is DERIVED from a live eviction, and a partial one leaves
    # the row standing on purpose, so it is the wrong gesture for an assertion
    # about the surface emptying.
    for _ in range(opened):
        before = app.settings.member_review_row_count()
        if before == 0:
            break
        app.settings.member_review_keep(0)
        wait_until(
            lambda before=before: app.settings.member_review_row_count() < before,
            _ADJUDICATION_S,
            diagnose=lambda: (
                "Keep must close every open item for that person and the "
                "re-read must drop their row; the roster still reads "
                f"{app.settings.member_review_row_count()} row(s), "
                f"error={app.error_text()!r}"
            ),
        )

    wait_until(
        app.settings.member_review_is_empty,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "worked through, the page must say so — 'empty again once the "
            "backlog is worked through' is half of what makes this surface "
            "carry no dismiss affordance of its own; rows="
            f"{app.settings.member_review_row_count()} error={app.error_text()!r}"
        ),
    )


@pytest.mark.feature("take-your-account-back")
@pytest.mark.real_conversations
def test_the_successors_conversations_unlock_without_a_user_command(
    succeedable_app, nest_instance
):
    """hold a conversation → succeed → the conversations plane comes back up.

    Uses ``succeedable_app`` — a dedicated fresh actor **plus its own
    credentials** — for two reasons. The dedicated half is what every ceremony
    test in this file needs: the ceremony revokes the predecessor's bearers, so
    driven against the shared session ``test_user`` it signs every LATER test in
    the run out. This test took ``logged_in_app`` until 2026-08-30, and that is
    exactly what it did to a 3 347-test sweep — see
    ``test_no_succession_on_the_shared_identity.py``, which now refuses the
    shape at collection time. The *credentials* half is what lets the
    pre-ceremony witness below be **confirmed at rest** rather than assumed
    (``fauna.mls.get`` as this actor) — the same trade, for the same reason,
    that ``test_the_successors_unsent_drafts_come_back`` makes with the
    ``__drafts`` plane.

    § Re-key scope's ``BackupKey`` corpus row covers ``__mls``, and this is the
    leg that repairs the plane a succession would otherwise leave dark: the
    openMLS ``provider`` snapshot — the crypto state every group's decryption
    depends on — stays sealed to the retired identity, so ``MlsStateSync::load``
    fails at the unseal.

    ⚠ **The pre-ceremony witness is load-bearing** (the rule). A successor
    whose predecessor never ran the MLS plane has no ``provider`` blob at rest,
    the pass answers ``NothingStored``, the line renders nothing, and every
    assertion below would be vacuously true. So a real MLS group is bootstrapped
    over the REAL ``FaunaMlsBackend`` first, and the nest is asked whether its
    sealed bytes actually landed, before the ceremony runs.

    ⚠ **A mock-backend seam cannot stand in for that witness, and one silently
    did for nineteen days.** This test seeded through
    ``conversations.seed_own_message`` (the ``inject_own_for_test`` seam) until
    today. That seam — like ``create_mls_group`` beside it — is a **thread-store
    fixture over ``MockRailBackend``**: it materialises a rendered bubble and
    touches the MLS engine not at all, so the openMLS ``provider`` gains no
    channel and nothing is put at rest for leg 3 to re-seal. Web moved onto that
    seam on 2026-08-12 as an app-uniformity lift (priority #1, correct in
    itself) from its old ``create_mls_group`` + compose-send path, and this
    journey's witness was hollowed out in the same stroke. The failure it
    produced looked exactly like a broken product leg: an instrumented solo run
    measured ``the __mls re-seal pass examined the replica provider_ours=true
    channels=0 histories=0 owed=0`` — the re-seal loop never executed a single
    iteration, and settled ``AlreadyCurrent`` having examined nothing. Leg 3
    itself has been built and green on web since 2026-08-21
    (``succession-aftermath.md`` § Implementation status today). **So the real
    backend is not an optional strengthening here — it is the only seam this
    journey has that produces the crypto state it asserts about.**

    ⚠ **Assert the exact done string, never a non-empty line.** Leg 3 has a
    fifth state the other legs lack — *partly-owed* — which also renders. A
    non-emptiness check would pass on a pass that left conversations sealed.
    """
    app, user = succeedable_app
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.conversations_restart import fauna_thread, wait_replica_settled
    from tests.api import conv_api

    def _own_client():
        # A raw User-class WS-RPC connection as this actor — a read-only
        # 'device' observing its own replica plane (`fauna.mls.get`). The same
        # shape the restart-survival witnesses use.
        return WsRpcAdminClient(
            node_url,
            actor_id=user["actor_id_bytes"],
            signing_key=bytes(user["signing_key"]),
        )

    # ── Precondition: a REAL openMLS group, and its sealed replica at rest ──
    # Only the real `FaunaMlsBackend` drives the MLS engine, and only the engine
    # puts a channel-bearing `provider` at rest for leg 3 to re-seal (see the
    # docstring's second warning for what the mock seam does instead).
    app.conversations.navigate()
    app.conversations.enable_real_faunamls()

    # An API-tier peer that the bootstrap can actually reach: registered, with
    # real key packages to fetch, AND accepting us — without the third the nest
    # refuses the Welcome as `fauna.conversations.forbidden` and no group forms.
    peer = conv_api.reachable_peer(port, admin_sk, user["actor_id_hex"])

    # Exclude anything already bound so the pick cannot land on a thread another
    # test in this session restored through the cross-device replica.
    pre_existing = {
        t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex
    }
    app.conversations.real_resolve_send_new(
        peer["actor_id_hex"],
        "the conversation that must survive the succession",
    )
    thread = fauna_thread(app, exclude=pre_existing)
    channel_hex = thread.channel_id_hex
    assert channel_hex, (
        "the 1:1 must BIND A CHANNEL before the ceremony — an unbound thread "
        "means no MLS group was bootstrapped, so the `provider` at rest would "
        "carry zero channels and leg 3 would settle AlreadyCurrent having "
        f"examined nothing; error={app.error_text()!r}"
    )

    # The replica autosave is DEBOUNCED, so a ceremony that outran it would leave
    # the predecessor's crypto state un-uploaded and the pass would answer
    # NothingStored. Poll the nest's own plane rather than sleeping past it
    # (convention 14) — settled, not merely present: `snapshot_replica` binds the
    # channel well before the own message is appended, so a presence check can
    # green on a message-less slice.
    wait_replica_settled(_own_client, "provider")
    wait_replica_settled(_own_client, f"history/{channel_hex}")

    _run_succession(app)

    # ── The assertion ───────────────────────────────────────────────────────
    # Why the exception is KEPT rather than swallowed (convention 6 — a failure
    # must diagnose itself): this read has FOUR very different ways to be empty,
    # and the rendered line collapses every one of them to `""`.
    #
    #   1. `open_recovery_kit()` never got to the page — a read that did not
    #      happen, which is what `last_exc` below separates out;
    #   2. the barrier never ran: `MlsStateSync::load` skips it outright when
    #      `predecessors` is empty (`libs/fauna-client-mls-sync/src/sync.rs`),
    #      so the sink is never called even once;
    #   3. it ran and settled `NothingStored` — no `provider` blob at rest;
    #   4. it ran and settled `AlreadyCurrent`.
    #
    # Arms 3 and 4 paint nothing BY DESIGN (`ReplicaResealOutcome::settled_line`
    # returns `None` for both: "a line announcing a no-op at every later sign-in
    # trains the user to ignore the one that matters"), so an empty line is not
    # evidence of a broken leg — and `ui.yaml`'s own scoping for
    # `recovery-kit-mls-reseal-status` says the same thing ("renders only for an
    # identity that succeeded from another AND has a replica at rest"). Telling
    # the reader "an empty line means leg 3 never ran" is therefore a diagnosis
    # the code cannot support, and it cost a session a whole static root-cause
    # chain built on the wrong arm.
    # The last failure is carried out to the diagnose string, and the aftermath
    # LOG — which reports the arm unconditionally, web included since this
    # commit — is quoted beside it to separate 2 from 3 from 4.
    last_exc: list[str] = []

    def _mls_line() -> str:
        try:
            app.settings.open_recovery_kit()
            line = app.settings.aftermath_mls_reseal_status()
        except Exception as exc:  # noqa: BLE001 — reported, not silenced
            last_exc.append(f"{type(exc).__name__}: {exc}")
            return ""
        last_exc.clear()
        return line

    wait_until(
        lambda: _mls_line() == S.settings.recovery_kit.mls_reseal_done,
        _MLS_RESEAL_S,
        diagnose=lambda: (
            "the successor's replica re-seal must run inside the plane's own "
            "load (succession-aftermath.md § Re-key scope, the BackupKey corpus "
            f"row), but the line reads {_mls_line()!r} (expected "
            f"{S.settings.recovery_kit.mls_reseal_done!r}); "
            f"error={app.error_text()!r}. "
            + (
                f"The read itself kept RAISING ({last_exc[-1]}) — so this is "
                "NOT evidence about leg 3 at all: the page was never reached. "
                "Fix the read before drawing any conclusion about the re-seal."
                if last_exc
                else "The read succeeded and returned an empty line. That "
                "narrows it to THREE arms, which the rendered line cannot "
                "tell apart: the pass never ran (no predecessor key resolved, "
                "so the barrier is skipped without a round trip), or it ran "
                "and answered NothingStored (no `provider` blob at rest — the "
                "pre-ceremony witness above did not durably land), or it ran "
                "and answered AlreadyCurrent. Only the last of those is "
                "healthy, and `settled_line` paints nothing for either "
                "settled arm on purpose. The log below is what separates "
                "them — read it before concluding anything."
            )
            + f" aftermath log: {_aftermath_log(app)}"
        ),
    )


@pytest.mark.feature("take-your-account-back")
# The production launch composition on the native apps: what this journey
# reads after the ceremony is decided by the successor's launch reconcile (the
# succession cut, `succession-cut.md` ruling (11)), which on windows, macOS and
# iOS runs only inside the real conversations receive loop's post-restore hook
# (`fauna_client_folders::FolderRemovalResume`). Without the marker a native
# seat never runs it and the journey reads a corpus the cut would have made
# history — measured green on windows on 2026-10-07 for exactly that reason,
# while tui (which always runs the real session) reads `[]`.
@pytest.mark.real_conversations
def test_the_successor_can_read_the_corpus_it_inherited(
    ungranted_app, nest_instance, tmp_path
):
    """upload an image → succeed → the successor's NAMES render, its BYTES open.

    § Re-key scope's ``BackupKey`` corpus row names *media, folders and
    backups*, and ``identity-succession.md`` § Implementation status today
    carries the two bullets this journey is the end-to-end evidence for: *"a
    successor READS its predecessor-sealed corpus"* (the chunk/label planes,
    ``FileDownloadKeys::predecessor_backup_keys``) and *"a successor READS the
    raw-AEAD Library plane too"* (``MediaMachine::open_library_blob``, the bare
    ``BackupKey`` blobs a thumbnail and a Media-page upload's blob-store primary
    rest under). Both bullets shipped with tier_1 funnel pins and tui call-site
    pins; **neither has ever been driven by a real successor against a live
    nest**, which is exactly the gap row 36 was filed for.

    **What only a live nest can decide.** A succession re-points corpus
    *ownership* inside one nest-side transaction and moves no *seal*. So the
    successor authenticates as a brand-new identity, is handed back blobs it
    genuinely owns, and derives its owner key from its own new seed — under
    which every one of those blobs fails the AEAD tag. Whether the retired root
    is offered as a read candidate at the opening site is a property of the
    composition (post-auth hook → registry walk → ``MediaMachine`` glue →
    fetch → decrypt → paint), and every link in it is in a different component.

    **Two planes, asserted separately, because they are separate funnels** —
    ``render_sealed_paths``'s label render for the names,
    ``MediaMachine::open_library_blob`` for the bytes. The goal doc states the
    stake: *"a successor that opened its bytes but not its names would render an
    empty file list over a corpus it can read"*.

    ⚠ **But they share one feed, and the ORDER of these assertions is what that
    buys.** Severing tui's single injection — the one post-auth walk shared by
    the agent, Settings and the ``__mls`` barrier — does **not** produce "names
    without bytes". The *names* go first: every item's path label degrades to
    ``SealedLabelRender::Omit``, so ``fauna.media.list`` renders **empty** and
    the byte plane is never reached at all. So the name assertion is this
    journey's tripwire and the paint assertion is strictly downstream of it —
    reversing them would report a dark corpus as a thumbnail bug.

    **Mutation-verified 2026-08-11, three severances, and the matrix is the
    point** (each reverted after; the record is here so no later session pays
    for it twice):

    ==================================================  ========  ====  =====
    severance                                           folder  name  paint
    ==================================================  ========  ====  =====
    none (baseline)                                     pass      pass  pass
    tui's shared injection (``media/mod.rs``)           pass      RED   n/r
    ``open_library_blob``'s retired-root loop only      pass      pass  RED
    the shared resolver (``session.rs``) → empty        pass      RED   n/r
    ==================================================  ========  ====  =====

    Row 2 is what makes the paint assertion non-vacuous: it is the one severance
    that reaches *only* the byte plane, and it reds *only* that assertion — so
    the paint is doing its own work rather than riding the name's. Rows 1 and 3
    show why the folder assertion is documented at its own site as **not** a
    read-fallback pin: it survives even total removal of the retired roots.

    ⚠ **The thumbnail paint is the sharp assertion, and it is the reason this
    test uploads an image rather than any file.** A painted ``media-thumbnail``
    on tui is half-block art — the picture *is* the element's characters — so
    counting a painted tile is a direct read of *fetch → decrypt → render*
    having succeeded on a blob sealed under a key the successor does not hold.
    A failed tag paints the ``□`` placeholder instead, which is not counted, and
    is exactly the dark-corpus state. Clients that cannot inspect the paint
    return ``None`` and self-skip that half (``MediaActions.painted_thumbnail_count``).

    ⚠ **The pre-ceremony assertions are not scaffolding — they are what stops
    this from being vacuous**, in the same family as the muted word above and
    the ``NothingStored`` trap in this file's header. An upload that silently
    landed nowhere (a fresh actor has no folder, and the gesture then reports
    ``media.error_no_set`` and uploads nothing) would leave the successor with
    an empty Media page, and *every* assertion after the ceremony would pass by
    describing an empty corpus. Asserting the item listed **and painted** before
    the ceremony is what makes their absence afterwards mean something.

    Convention 8 throughout: the set is created through the real wizard and the
    file uploaded through the real picker — the corpus under test is a user's.
    """
    app = ungranted_app

    # ── Precondition: a set holding a real image, sealed under THIS
    # identity's BackupKey, listed AND painted before the ceremony — the
    # docstring's non-vacuity bullet, kept once in the shared helper.
    _, inherited_file = seed_an_image_under_this_identity(app, tmp_path)

    # ── The ceremony ────────────────────────────────────────────────────────
    _run_succession(app)

    # ── The set survived the ownership re-point ─────────────────────────────
    # ⚠ **This is NOT a read-fallback pin, and a future session must not read
    # it as one.** Mutation-verified 2026-08-11: it stays green with the shared
    # resolver returning an empty list — i.e. with every retired root gone from
    # every plane — because a wizard-created set's own row is not what the
    # predecessor keys open. What it does prove is worth one line anyway: the
    # nest's succession transaction re-pointed the set to the new owner, so the
    # successor still *owns* the container whose contents the next two
    # assertions then try to read. Kept deliberately ahead of them so a lost
    # container and an unreadable corpus cannot present as the same failure.
    #
    # Each poll is a fresh VISIT (`revisit_folders`), never a re-read of one
    # visit's paint — the config journey's rule above, for the same cause. The
    # page renders a set only once custody opens its sealed name, and the
    # successor's account runtime mounts after the actor id switches (all
    # `_run_succession` waits for); on an app that does not consume the
    # store-change notice yet, a visit landing before it paints an empty list
    # and keeps it. The name itself still rests under the PREDECESSOR's owner
    # root (a succession re-seals nothing), so the successor's Devices machine
    # must also be handed the paired predecessor chain
    # (`DevicesMachine::set_predecessor_chain`): without it every visit lists
    # nothing — measured on windows 2026-10-07, re-visits included.
    wait_until(
        lambda: len(app.backups.revisit_folders()) >= 1,
        _CORPUS_READ_S,
        diagnose=lambda: (
            "the succession must leave the successor owning the folder it "
            "inherited — the nest re-points corpus ownership in its succession "
            f"transaction — got count={app.backups.folder_count()} "
            f"error={app.error_text()!r}. This failing means the container is "
            "gone, or that custody never opened its name, which are different "
            "breaks from the corpus being unreadable; the two assertions below "
            "cover that half."
            f"\n  account runtime: {_account_plane_log(app)}"
        ),
    )

    # ── The NAMES half: the label plane came across ─────────────────────────

    app.media.navigate()
    wait_until(
        lambda: inherited_file in app.media.item_names(),
        _CORPUS_READ_S,
        diagnose=lambda: (
            "the successor must render the NAME of the media it inherited, but "
            f"lists {app.media.item_names()!r} (expected {inherited_file!r}); "
            f"error={app.error_text()!r}. An empty list over a corpus the nest "
            "re-pointed to this owner is the dark-corpus break: ownership moved, "
            "the seal did not, and no retired root was offered at the open — "
            "UNLESS the row judge refused the predecessor-signed rows, which "
            "empties the list before any label is opened: row-judge log "
            f"{_row_judge_log(app)}"
        ),
    )

    # ── The BYTES half: the raw-AEAD Library plane opened ───────────────────
    # This is the assertion that cannot be satisfied by a name that travelled in
    # the clear. The thumbnail is a single AEAD blob under the *bare* retired
    # `BackupKey`; painting it means `open_library_blob` tried the current key,
    # failed, and succeeded on a candidate from the registry walk.
    painted_after = app.media.wait_for_painted_thumbnails(1, timeout=_CORPUS_READ_S)
    if painted_after is not None:
        assert painted_after == 1, (
            "the successor must be able to OPEN the bytes it inherited "
            "(identity-succession.md § Implementation status today — "
            "`MediaMachine::open_library_blob` offers each retired root after "
            f"the current key), but {painted_after} of 1 thumbnails painted; "
            f"error={app.error_text()!r}. A 0 here with the name still listed "
            "is precisely the half-broken state the two planes are asserted "
            "separately to catch: the successor can see what it owns and cannot "
            "read any of it."
        )


def _require_mail_settings(app) -> None:
    """Skip, declaring the class, on an app with no mail-settings page."""
    if app.mail_settings.is_page_visible():
        return
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="mail-settings-enabled-toggle",
        detail=(
            "mail-settings.md § User actions — the mailbox enable + credential "
            "list; tui leads and the other six follow in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/mail-credentials.md "
            "§ Rotation and recovery → Succession"
        ),
    )


@pytest.mark.feature("take-your-account-back")
def test_the_successors_mail_passwords_are_burned(ungranted_app, nest_instance):
    """enable mail + reveal the password → succeed → every credential revoked.

    **Aftermath leg 6 — the one leg whose DONE state is not good news.** The
    pre-succession seed thief read the ``fauna.state.mail`` plane, which holds
    the MSEK *and* every credential's raw ``secret`` (``mail-credentials.md`` § Rotation and
    recovery → *Succession*: "exclusion is total and not user-selectable"), so
    the successor's client burns the lot: every row goes to *Compromised —
    access revoked*, both resting blob kinds are deleted per row, and the MSEK
    rotates with an empty survivors list.

    **What only a full-stack run can establish, and why this test exists.** The
    leg landed with 8 graded mutations against crate-level fakes and three tui
    render pins, and — uniquely among the six aftermath legs — **no end-to-end
    evidence at all**. What the fakes cannot reach is the composition: the
    post-store-ready pass running leg 6 last, over the mail custody's rows in
    the successor's account store and under the attested-predecessor gate, two
    revoke RPCs per credential against a real nest, a rotation whose survivor
    list is empty by construction, and a page that re-renders the result. A
    leg that silently never ran — a pass skipped, a gate that never opened —
    would leave every crate test green and every mail password live.

    ⚠ **The pre-ceremony reveal is the whole test.** It is what proves the bytes
    a predecessor's seed holder would be holding were *readable* before the
    ceremony — a succession run over an actor with no mail plane burns nothing
    (``NoMailMaterial`` is a legitimate "nothing owed") and would pass every
    assertion below vacuously. Do not "simplify" it away.

    ⚠ **Assertion ORDER is load-bearing, and it is not the order of interest.**
    The revoked-row count and the emptied reveal come first because they are
    statements about the *mechanism*; the progress line comes last because it is
    a statement about the *render*. Reversed, a leg that never ran would present
    as a rendering bug — the failure mode row 56 cost two journey runs.

    ⚠ **A non-empty progress line is NOT the pass condition.** Leg 6's
    running and failed arms render too, and they mean the opposite: nothing was
    burned (yet), and the predecessor's passwords still open the mailbox —
    hence the assertion names the done arm exactly rather than testing for
    presence.
    """
    app = ungranted_app

    # ── Precondition: a real mailbox, with a real readable secret ────────────
    # ``ungranted_app`` is a fresh non-admin actor made by the same `_make_user`
    # that builds the session ``test_user``, and enabling a mailbox is the
    # *user's own* gesture (`MailSettingsAction::EnableMail`), not the
    # deployment-wide Admin-class `fauna.bridges.set_mail_enabled` — so this
    # needs no dedicated nest and no admin login. The shared session nest is
    # safe precisely because the ceremony below re-points only THIS actor.
    app.mail_settings.navigate()
    _require_mail_settings(app)
    app.mail_settings.ensure_mail_enabled()

    credentials_before = app.mail_settings.credential_count()
    assert credentials_before >= 1, (
        "an enabled mailbox always has at least one credential, and the burn "
        "has nothing to prove without one; got "
        f"{credentials_before} rows, page error: "
        f"{app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.revoked_credential_count() == 0, (
        "no row may read *Compromised — access revoked* BEFORE the ceremony, or "
        "the post-ceremony count proves nothing; got "
        f"{app.mail_settings.revoked_credential_count()} of "
        f"{credentials_before} already revoked"
    )
    secret_before = app.mail_settings.reveal_credential_secret(0)
    assert secret_before, (
        "the credential secret must be REVEALABLE before the ceremony — those "
        "are the exact bytes a predecessor's seed holder would hold, and an "
        "empty reveal here would make the emptied reveal below vacuous. "
        f"error={app.error_text()!r}"
    )

    # ── The ceremony ────────────────────────────────────────────────────────
    _run_succession(app)

    # ── (1) MECHANISM: every row is revoked ─────────────────────────────────
    # The rows keep rendering when burned — they are the user's list of which
    # mail apps to set up again — so the row count says nothing here. Only the
    # revoked count does, and the burn is total, so it must equal the total.
    app.mail_settings.navigate()
    wait_until(
        lambda: (
            app.mail_settings.credential_count() >= 1
            and app.mail_settings.revoked_credential_count()
            == app.mail_settings.credential_count()
        ),
        _MAIL_BURN_S,
        diagnose=lambda: (
            "every pre-succession credential must read *Compromised — access "
            "revoked* (mail-credentials.md § Rotation and recovery → "
            "Succession: exclusion is total and not user-selectable), but "
            f"{app.mail_settings.revoked_credential_count()} of "
            f"{app.mail_settings.credential_count()} rows are marked "
            f"(was {credentials_before} rows before the ceremony); "
            f"error={app.error_text()!r} "
            f"page_error={app.mail_settings.page_error_text(timeout=2.0)!r}. "
            # The leg that would explain a zero here — quoted last, because it
            # navigates.
            f"leg 6 says {_mail_burn_line(app)!r}. Leg 6 runs in the "
            "post-store-ready pass: no leg-6 line at all "
            "means that pass never reached it."
        ),
    )

    # ── (2) MECHANISM: the revealed secret is gone ──────────────────────────
    # The mark above is a label; this is the material. `burn_mail_after_
    # succession` empties each row's `secret`, so the one-time reveal — the
    # same gesture that returned bytes before the ceremony — must now return
    # nothing at all.
    app.mail_settings.navigate()
    secret_after = app.mail_settings.reveal_credential_secret(0)
    assert secret_after == "", (
        "the burn must EMPTY each credential's stored secret, not merely label "
        f"the row: the reveal returned {secret_after!r} "
        f"({len(secret_after)} chars) after the ceremony, having returned "
        f"{len(secret_before)} chars before it. A row marked revoked whose "
        "secret still reveals means the label moved and the material did not — "
        "the predecessor's seed holder still has a working password. "
        f"error={app.error_text()!r}"
    )

    # ── (3) RENDER: the progress line names the DONE arm, not an unfinished one
    # Last, deliberately (see the docstring): a render assertion ahead of the
    # mechanism ones would report a leg that never ran as a painting bug. And
    # the arm is named exactly — presence alone would pass on the running or
    # the failed arm, either of which means the exposure is still open.
    expected = S.settings.recovery_kit.mail_burn_done(
        count=str(credentials_before)
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    wait_until(
        lambda: app.settings.aftermath_mail_burn_status() == expected,
        _MAIL_BURN_S,
        diagnose=lambda: (
            "the Recovery Kit section must tell the successor their mail apps "
            "need setting up again — leg 6's DONE arm — but reads "
            f"{app.settings.aftermath_mail_burn_status()!r} (expected "
            f"{expected!r}); error={app.error_text()!r}. "
            "Any other line — still running, or the failed arm with its "
            "reason — means the burn did NOT finish and every predecessor "
            "password still works: a failure, not a different spelling of "
            "success."
        ),
    )


# Leg 7 re-seals every ratified `__drafts` rail; the successor's launch load
# offers the retired keys so a read that beats the pass still opens the rail.
# Either path satisfies this journey, deliberately — it asserts what the user
# sees, not which half delivered it.
_DRAFTS_RESEAL_S = 120.0

# Distinctive enough that finding it after the ceremony cannot be a coincidence
# or a leftover default.
_DRAFT_WITNESS = "the reply the theft interrupted 8831"


@pytest.mark.tui
@pytest.mark.linux
# The ceremony must not start before the DEBOUNCED `fauna.drafts.put` has
# landed, which is what the poll below waits for — see `drafts_production_window`
# in pytest.ini.
@pytest.mark.drafts_production_window
@pytest.mark.feature("take-your-account-back")
def test_the_successors_unsent_drafts_come_back(succeedable_app, nest_instance):
    """type a draft -> succeed -> the successor's composer still has it.

    Uses ``succeedable_app`` rather than ``logged_in_app`` + ``test_user``: the
    read below must name the signed-in identity to the nest's ``__drafts``
    plane, and that fixture is exactly "a dedicated actor plus its own
    credentials" for this case. The shared session identity would work for the
    read and destroy the run — the ceremony revokes the predecessor's bearers,
    so every LATER test's login is refused. That is what this test did to a
    3 347-test sweep on 2026-08-30, and what
    ``test_no_succession_on_the_shared_identity.py`` now refuses at collection.

    § Re-key scope's ``BackupKey`` corpus row names ``__drafts`` explicitly.
    Every compose surface saves half-written text to its own rail sealed under
    the owner's ``BackupKey``, so a succession leaves it unreadable: the
    successor owns the blob by ordinary authenticated reads but holds no key
    that opens it, and ``DraftsClient::load`` hard-errors rather than reporting
    "no drafts" (which would let the next autosave clobber it).

    Two mechanisms can satisfy this and the test deliberately does not care
    which ran: leg 7 re-seals the rail under the successor's key, and the
    launch load offers the retired keys so a read that wins the race still
    opens it. What the user is owed is their unsent text, and that is what is
    asserted.

    marked ``tui`` + ``linux``: those are the two apps that both drive the
    pass and offer the read keys today.
    The other five extend this test as their trickle-down leg lands
    (``succession-aftermath.md`` — the ``__drafts`` bullet's per-app contract
    item).

    WARNING The pre-ceremony nest confirmation is load-bearing, and it is
    the rule this file applies everywhere. The draft save is DEBOUNCED, so a
    ceremony that ran before the ``fauna.drafts.put`` landed would leave nothing
    at rest, the pass would answer ``NothingStored``, and this test would be
    asserting against a rail that never existed. Poll the nest's own
    ``__drafts`` plane rather than sleeping past the debounce.
    """
    app, user = succeedable_app
    node_url = nest_instance["url"]
    actor_id = user["actor_id_bytes"]
    signing_key = bytes(user["signing_key"])

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    def _rail_blob():
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            return dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    baseline = _rail_blob()

    # -- Precondition: a real predecessor-sealed rail, typed through the UI ---
    # Convention 8: the mutation is a user's, not an RPC shortcut. Typing is
    # what drives the manager, whose debounced autosave is what puts the rail at
    # rest under the PREDECESSOR's key -- which is the thing under test.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", _DRAFT_WITNESS)

    wait_until(
        lambda: app.conversations.compose_body_text() == _DRAFT_WITNESS,
        _DRAFTS_RESEAL_S,
        diagnose=lambda: (
            "the draft must be in the composer BEFORE the ceremony or this "
            "test proves nothing: composer reads "
            f"{app.conversations.compose_body_text()!r} (expected "
            f"{_DRAFT_WITNESS!r}); error={app.error_text()!r}"
        ),
    )
    wait_until(
        lambda: (_rail_blob() or baseline) != baseline,
        _DRAFTS_RESEAL_S,
        diagnose=lambda: (
            "the debounced fauna.drafts.put must reach the nest __drafts plane "
            "BEFORE the ceremony. Without a rail at rest under the "
            "predecessor's key, leg 7 answers NothingStored and every "
            "assertion below is vacuous -- the pre-ceremony-witness rule this file applies to "
            "every leg."
        ),
    )

    _run_succession(app)

    # -- The assertion: the successor's unsent text is back ------------------
    # Re-opening the new-thread composer PRESERVES a restored draft
    # (conversations.md § Persistence), so the body must reappear. An EMPTY
    # composer is the failure this leg exists to prevent, and note it is
    # indistinguishable at a glance from "this successor never had drafts" --
    # which is exactly why the pre-ceremony confirmation above is mandatory.
    def _restored() -> str:
        try:
            app.conversations.navigate()
            app.driver.click("new-conversation-button")
            app.driver.wait_for("dm-text-field", timeout=10.0)
            return app.conversations.compose_body_text()
        except Exception:
            return ""

    wait_until(
        lambda: _restored() == _DRAFT_WITNESS,
        _DRAFTS_RESEAL_S,
        diagnose=lambda: (
            "a successor must get their unsent drafts back "
            "(succession-aftermath.md § Re-key scope, the BackupKey corpus "
            f"row names __drafts), but the composer reads {_restored()!r} "
            f"(expected {_DRAFT_WITNESS!r}); error={app.error_text()!r}. "
            "An EMPTY composer "
            "means neither half ran: leg 7 did not re-seal the rail AND the "
            "launch load was not offered the retired keys, so the user's "
            "half-written reply is sealed to an identity they no longer have."
        ),
    )

    # The recovered draft must also be EDITABLE, not merely readable once: a
    # successor whose composer refused further typing would still have lost the
    # reply. ``type_text`` APPENDS at the cursor (``drivers/base.py``'s
    # contract; ``clear_and_type`` is the replacing form), so type only the
    # continuation and expect it on the END of the recovered text -- which
    # asserts both halves at once: the recovered witness is still there AND the
    # new keystrokes landed on top of it.
    #
    # Scope note, deliberately narrow: this asserts the COMPOSER, not the
    # plane. Whether the successor's own autosave reaches the nest sealed under
    # the successor's key is a crypto round trip owned at tier_1 by
    # ``rekey_is_idempotent_and_the_second_pass_does_not_write``
    # (``libs/fauna-client-drafts/src/store.rs``), which types after the pass
    # and reads the new bytes back. It is not reachable from here: the drafts
    # plane is keyed by the CALLING actor (``get_drafts_blob(&actor_id, ...)``
    # in ``bins/fauna-nest/src/drafts_handlers.rs``) and the successor's
    # signing key is minted inside the app, so ``_rail_blob()`` above can only
    # ever read the PREDECESSOR's rail.
    continuation = "-edited-by-the-successor"
    edited = f"{_DRAFT_WITNESS}{continuation}"
    app.driver.type_text("dm-text-field", continuation)
    wait_until(
        lambda: app.conversations.compose_body_text() == edited,
        _DRAFTS_RESEAL_S,
        diagnose=lambda: (
            "a successor must be able to keep editing the draft they "
            "recovered: the composer reads "
            f"{app.conversations.compose_body_text()!r} (expected "
            f"{edited!r}); error={app.error_text()!r}"
        ),
    )


# Leg 3's own check: `fauna.recovery.succession.status` + a `fauna.state.nostr-confirmation` read —
# the same class of round trip `_ADJUDICATION_S` already budgets for the
# member/filter marks.
_NPUB_CONFIRM_S = 120.0


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("take-your-account-back")
def test_the_successors_subscriber_tier_is_re_keyed(succeedable_app, nest_instance):
    """sell a tier -> succeed -> the key it distributes is one the old identity never had.

    § Re-key scope's period-keys row: *"Random material in the account plane's
    ``fauna.state.subscriptions`` rows — carried to the successor with the
    generations the step-(4) re-escrow hands over; future periods
    signed/wrapped under the successor"*. The tier row above it is where the second half gets its teeth:
    the ceremony **moves** the tier plane with its key material, because those
    keys seal the author's own back catalogue and stranding them would be
    user-irrecoverable — which leaves the thief's copy live. Recovery alone
    hands the successor exactly the key a seed thief already read out of
    the ``fauna.state.subscriptions`` rows, so every audience-restricted post published
    *afterwards* would open under it. Leg 8 is what ends that, and this is its
    witness.

    **Why the assertion is an unwrapped KEY and nothing cheaper.** A bumped
    ``version``, a changed ``blob_hash``, a fresh ``rotated_at`` — every one of
    those is satisfied by republishing the SAME key, which is precisely the
    failure mode that would leave the exposure open while looking fixed. So the
    subscriber unwraps the blob before and after and compares the 32 bytes
    inside, through ``fauna_keyblob_open`` — the same
    ``fauna_core::subscription::crypto`` seam every app rides, never a
    Python-side reimplementation that would agree with itself about an envelope
    the nest might reject.

    **The blob's author is a corroborating assertion and MUST NOT become the
    gate.** The leg's first shipped predicate derived owed-ness from it and was
    wrong: `KeyBlob.author` records who last *published* a
    blob, and an ordinary (auto-)approve republishes the tier's current key
    under the caller's authorship — so the author flips to the successor over
    an unrotated key the moment the author pump drains one auto-approval. The
    leg now derives from `TierPeriod.minted_by`, the identity that minted the
    KEY, and this journey waits on the key itself for the same reason.

    ⚠ **The pre-ceremony witness is the whole test**, the rule this file applies
    to every leg. A tier with no subscriber and no minted blob would let the
    leg answer "nothing held" and every assertion below would be vacuous — so
    the tier is created through the UI, a real second actor subscribes, the
    author **approves through the UI** (convention 8: the mint is a user's
    gesture, and a headless author can never produce an approved subscription
    on a client-minted tier), and the subscriber's unwrap of the resulting blob
    is confirmed before the ceremony starts.

    ⚠ And the retained subscriber must still be able to unwrap AT ALL. A
    rotation that quietly shed the roster would also pass a bare "the key
    changed" check while having revoked a paying reader — so
    ``open_key_blob`` raising on a missing entry is load-bearing here, not a
    convenience.

    marked ``tui`` + ``linux`` + ``web`` + ``windows`` + ``macos`` + ``ios``:
    the apps that both drive the aftermath and have the Slice-A author page
    today. android extends this test as its trickle-down lands, exactly as the
    ``__drafts`` journey above says.
    """
    from actions.api_actor import ApiActor
    from common.auth import create_actor_and_register
    from fauna_ffi import open_key_blob

    app, user = succeedable_app
    tier = f"gold-{uuid.uuid4().hex[:8]}"

    # ── Precondition part 1: the author's own tier, created through the UI ──
    app.subscriptions.navigate()
    app.subscriptions.open_tiers_tab()
    app.subscriptions.create_tier(tier, rank=1, price_hint="$5/mo")
    assert app.subscriptions.wait_for_tier(tier), (
        f"the tier must exist before the ceremony or leg 8 holds no period key "
        f"to rotate and this test is vacuous; error={app.subscriptions.error_text()!r}"
    )

    # ── Precondition part 2: a real subscriber, granted through the UI ──────
    subscriber = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    sub_actor = ApiActor(
        nest_instance["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    sub_seed = bytes(subscriber["signing_key"])
    old_author_id = user["actor_id_bytes"]

    reply = sub_actor.subscribe(old_author_id, tier)
    assert reply.get("outcome") == "queued", (
        f"a fresh client-minted tier always enqueues — the nest holds no mint "
        f"authority (monetization.md § Pillar 1); got {reply!r}"
    )
    app.subscriptions.refresh()
    assert app.subscriptions.wait_for_pending_request(1), (
        f"the queued subscribe must reach the author's page before it can be "
        f"approved; error={app.subscriptions.error_text()!r}"
    )
    app.subscriptions.approve_first_request()
    assert app.subscriptions.wait_for_subscriber(1), (
        f"the approve must land the subscriber in the roster — that is the "
        f"mint+upload this test rotates; error={app.subscriptions.error_text()!r}"
    )

    # ── Precondition part 3: the key the PREDECESSOR distributes ────────────
    before_blob = sub_actor.subscription_key_blob(old_author_id, tier)
    key_before, author_before = open_key_blob(sub_seed, bytes(before_blob["blob_data"]))
    assert author_before.hex() == user["actor_id_hex"], (
        f"the pre-ceremony blob must be minted by the identity about to be "
        f"retired — that stamp is what leg 8 derives owed-ness from; got "
        f"{author_before.hex()!r} for {user['actor_id_hex']!r}"
    )

    old_actor, new_actor = _run_succession(app)

    # ── The assertion: a key the retired identity never held ────────────────
    # Anchored on the blob's author flipping — latent state the successor's
    # first session produces, never elapsed time (convention 14). Read under
    # the SUCCESSOR's actor id: the entitlement moved with the plane, which is
    # outcome 24's first clause and this journey's own precondition.
    # One read per poll tick, cached for the diagnose closure: the read is a
    # nest round trip plus an unwrap, and re-issuing it inside the predicate
    # AND the message would triple it on every tick of a 120 s ceiling.
    latest: dict[str, object] = {"key": None, "author": None, "error": ""}

    # ⚠ **Waits on the KEY, never on the blob's author** — the same correction
    # the leg itself had to make. `KeyBlob.author` records
    # who last PUBLISHED the blob, and an ordinary approve republishes the
    # current key under the caller's authorship, so a gate keyed on the author
    # flipping to the successor would go green over an unrotated key the moment
    # the author pump drains one auto-approval. The key changing is the
    # property under test, and it is latent state either way (convention 14).
    def _reread() -> bool:
        try:
            blob = sub_actor.subscription_key_blob(bytes.fromhex(new_actor), tier)
            key, author = open_key_blob(sub_seed, bytes(blob["blob_data"]))
            latest.update(key=key, author=author, error="")
        except Exception as e:  # mid-rotation the blob may be the old one, or absent
            latest.update(key=None, author=None, error=f"{type(e).__name__}: {e}")
            return False
        return key != key_before

    wait_until(
        _reread,
        _PERIOD_ROTATION_S,
        diagnose=lambda: (
            f"the tier is still distributing the key the retired identity held: "
            f"the subscriber unwraps the same 32 bytes as before the ceremony "
            f"(blob author={(latest['author'] or b'').hex()!r}, successor="
            f"{new_actor!r}, old={old_actor!r}; last read error="
            f"{latest['error']!r}). Until the rotation leg republishes, whoever "
            f"held the retired identity's seed still decrypts every "
            f"subscriber-only post this account publishes."
            + _aftermath_log(app)
        ),
    )
    key_after, author_after = latest["key"], latest["author"]

    assert author_after.hex() == new_actor, (
        f"the live blob must be minted by the successor, got {author_after.hex()!r}"
    )
    assert key_after != key_before, (
        "leg 8 must mint a FRESH period key, not merely republish the inherited "
        "one: the retired identity's seed holder read the old key out of "
        "the subscriptions plane, so a republished blob would leave every post published from "
        "now on readable by exactly the party the recovery was run against"
    )
    # `open_key_blob` raised if the roster had been shed, so reaching here is
    # itself the retained-access assertion; say so, because a reader of the two
    # asserts above would otherwise read this journey as key-only.
    assert len(key_after) == 32, (
        "the retained subscriber must still unwrap an entry of their own — a "
        "rotation that dropped them would 'pass' a key-changed check while "
        "having silently revoked a paying reader"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("take-your-account-back")
def test_the_successor_is_asked_to_confirm_its_npub(ungranted_app, nest_instance):
    """link Nostr → succeed → the successor is asked to confirm the linked npub.

    ``nostr.md`` § Key succession and rotation, leg 3 (nostr.md:73/:79): the
    ceremony's npub check is the one adjudication this section asks a human
    for — a thief who ran unlink + re-link left a *different* npub linked, and
    a ``generated``-mode original destroyed that way is unrecoverable. Legs
    1+2 (the account row re-points, every bunker connection revokes) are BUILT
    nest-side and pinned in ``conformance_succession_nostr.rs``; this is leg
    3's app surface, tui-first per the ordering rule.

    **What only a full-stack run can establish.** The predicate
    (``fauna_client_config::npub_confirmation_owed``) is pinned pure at tier_1
    against synthetic seconds. What none of that reaches is whether the real
    ``fauna.recovery.succession.status`` round trip and the real
    ``fauna.state.nostr-confirmation`` write survive an actual ceremony + account-switch + relaunch — the same
    class of gap the sibling journeys in this file close for their own legs.

    ⚠ **The pre-ceremony link is the whole test.** A succession over an
    account with no Nostr link has ``succeeded_at`` set but leaves
    ``NostrState.linked()`` false, so the banner never has anywhere to render
    — the predicate would never even be READ. Linking BEFORE
    ``_run_succession`` is what makes its ``succeeded_at.is_some()`` arm
    reachable at all.
    """
    app = ungranted_app

    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable before the ceremony; error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        "the pre-ceremony link is the whole test — without it there is no "
        f"npub for leg 3 to ask about; error: {app.nostr.page_error_text()!r}"
    )
    # No succession has run yet — the banner must not appear on an ordinary
    # linked account (the predicate's `succeeded_at.is_none()` arm).
    assert not app.nostr.is_npub_confirm_visible(), (
        "an ordinary (never-succeeded) linked account must not be asked to "
        "confirm its npub"
    )

    _run_succession(app)

    app.nostr.navigate()
    assert app.nostr.wait_for_linked(timeout=_RESEAL_S), (
        "the successor's Nostr account row must survive the ceremony (leg 1, "
        f"built nest-side); error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_npub_confirm_visible(_NPUB_CONFIRM_S), (
        "the successor must be asked to confirm the linked npub (leg 3), but "
        f"the banner never appeared; error: {app.nostr.page_error_text()!r}"
    )

    # ── Confirming dismisses it, and the write is REAL — not a client-local
    # flag: navigating away and back re-runs the nav-enter check against the
    # nest's own confirmation plane, so a stale local dismissal would resurrect it. ──
    app.nostr.confirm_npub()
    assert app.nostr.wait_for_npub_confirm_gone(_NPUB_CONFIRM_S), (
        f"confirming must dismiss the banner; error: {app.nostr.page_error_text()!r}"
    )
    app.settings.navigate()
    app.nostr.navigate()
    assert not app.nostr.is_npub_confirm_visible(), (
        "the confirmation must be nest-persisted: re-entering the page must "
        "not re-raise a banner the owner already answered"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("take-your-account-back")
def test_the_successor_can_dismiss_npub_confirm_into_a_fresh_key(
    ungranted_app, nest_instance
):
    """The "no / nothing is linked" leg: it routes into the EXISTING new-key
    path (``nostr.md``:75 — "the remedy is the existing page machinery"), not
    a bespoke one, and a freshly (re-)linked key is not itself re-flagged —
    the owner's own deliberate choice of key needs no further confirmation.
    """
    app = ungranted_app

    app.nostr.navigate()
    assert app.nostr.ensure_linked(), (
        f"the pre-ceremony link is the whole test; error: {app.nostr.page_error_text()!r}"
    )

    _run_succession(app)

    app.nostr.navigate()
    assert app.nostr.wait_for_npub_confirm_visible(_NPUB_CONFIRM_S), (
        "the successor must be asked to confirm the linked npub; error: "
        f"{app.nostr.page_error_text()!r}"
    )

    app.nostr.dismiss_npub_to_new_key()
    assert app.nostr.wait_for_unlinked(), (
        'the "no / nothing is linked" gesture must route into the existing '
        f"unlink+relink machinery, not a bespoke flow; error: {app.nostr.page_error_text()!r}"
    )

    assert app.nostr.ensure_linked(), (
        f"the owner must be able to link a fresh key right away; error: "
        f"{app.nostr.page_error_text()!r}"
    )
    assert not app.nostr.is_npub_confirm_visible(), (
        "a freshly (re-)linked key is the owner's own deliberate choice — "
        "there is nothing left for the banner to ask about"
    )


# ── Mail rules that existed before the recovery are flagged ─────────────────

# How long the aftermath's filter raise may take to reach the page after the
# successor comes up: it runs beside the member roster once the successor's
# session is established, and fetches the rules over that session. Generous,
# paid only on red (convention 14).
_FILTER_RAISE_S = 60.0


def _require_inherited_filters_review(app) -> None:
    """Skip, declaring the class, on an app that has not built the inherited-rule
    review — the Account section's line, the mark on each rule and its Keep button.

    Polls rather than reading once (`_require_member_review_page`'s reason: a
    point-in-time read taken while the section's chain read is in flight would
    declare a built section unbuilt). Without this declaration an app that lacks
    the surface graded as a bare 60 s timeout naming a line that "reads ''" —
    indistinguishable from a built app whose raise is broken, and invisible to the
    `skip_unbuilt` accounting that exists to say which.
    """

    def _rendered() -> bool:
        app.settings.navigate()
        app.settings.open_recovery_kit()
        return app.is_visible("recovery-kit-inherited-filters-status")

    try:
        wait_until(_rendered, _FILTER_RAISE_S, diagnose=lambda: "")
        return
    except Exception:
        pass
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="recovery-kit-inherited-filters-status",
        detail=(
            "succession-aftermath.md § Adjudicating what the aftermath carries "
            "across — the Account line, the per-rule mark and its Keep button; "
            "tui leads and the other apps follow in batched trickle-down"
        ),
        tracked="docs/goal/behavior/succession-aftermath.md § Implementation status today",
    )


@pytest.mark.feature("take-your-account-back")
def test_mail_rules_from_before_the_recovery_are_flagged_to_keep_or_remove(
    ungranted_app, nest_instance
):
    """a mail rule → take the account back → the Account section says rules
    from before the recovery are unchecked and where to find them → the rule is
    marked with Keep beside Delete → Keep clears it; a rule made after the
    recovery is never marked.

    ``succession-aftermath.md`` § Adjudicating what the aftermath carries
    across: a seed thief could have planted a rule that silently bins or
    redirects mail, and the ceremony carries every rule across (the move
    re-points ``owner``). The raise and the three surfaces are unit-pinned; this
    is the end-to-end prompt the doc's status section names as its gap.

    The rule is made through the real filter editor before the ceremony
    (convention 8) — it is the user's own rule, and the marker must still ask:
    the client cannot tell the owner's rules from a thief's, which is the point.
    """
    app = ungranted_app
    inherited_name = f"before-{uuid.uuid4().hex[:8]}"
    app.settings.create_email_filter(
        name=inherited_name, rule_type="SenderIs",
        rule_value="someone@example.test", action="Discard",
    )

    _run_succession(app)
    _require_inherited_filters_review(app)

    # ── The route: the Account section names the backlog and where it is ──
    expected_line = S.settings.recovery_kit.inherited_filters(count="1")

    def _inherited_line() -> str:
        app.settings.navigate()
        app.settings.open_recovery_kit()
        if not app.is_visible("recovery-kit-inherited-filters-status"):
            return ""
        return app.driver.get_text("recovery-kit-inherited-filters-status")

    wait_until(
        lambda: _inherited_line() == expected_line,
        _FILTER_RAISE_S,
        diagnose=lambda: (
            "the successor was never told a rule from before the recovery is "
            f"unchecked; the line reads {_inherited_line()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )

    # ── The rule itself carries the mark, with Keep beside Delete ──────────
    assert inherited_name in app.settings.filter_names(), (
        "the rule must come across with the account before it can be judged; "
        f"the list reads {app.settings.filter_names()!r}"
    )
    # Waited, not read once: the list re-reads its marks on the visit that
    # filter_names() just made, and that read lands after the rows it repaints.
    wait_until(
        lambda: app.driver.count("filter-unattested-mark") == 1,
        _FILTER_RAISE_S,
        diagnose=lambda: (
            "exactly the one inherited rule is marked; "
            f"{app.driver.count('filter-unattested-mark')} showing over the "
            f"list {app.settings.filter_names()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )
    assert app.driver.count("filter-review-keep-button") == 1

    # ── A rule made AFTER the recovery is the successor's own ──────────────
    app.settings.create_email_filter(
        name=f"after-{uuid.uuid4().hex[:8]}", rule_type="SenderIs",
        rule_value="other@example.test", action="Discard",
    )
    assert app.driver.count("filter-unattested-mark") == 1, (
        "only rules from before the recovery are marked; the successor's own "
        "new rule must never be"
    )

    # ── Keep answers it: the mark and the Account line both go ─────────────
    app.driver.click("filter-review-keep-button")
    wait_until(
        lambda: app.driver.count("filter-unattested-mark") == 0,
        _FILTER_RAISE_S,
        diagnose=lambda: (
            "Keep must clear the mark it answers; "
            f"{app.driver.count('filter-unattested-mark')} still showing, "
            f"error surface: {app.error_text()!r}"
        ),
    )
    assert inherited_name in app.settings.filter_names(), (
        "Keep keeps the rule — only Delete removes one"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    assert app.driver.is_absent("recovery-kit-inherited-filters-status"), (
        "with nothing left to check the Account line disappears by itself"
    )
