"""The two reads every succession journey polls on, the budget they share, and
the ceremony steps the bound-seat journeys drive.

Lifted 2026-08-27 from three verbatim-in-behaviour copies
(``test_identity_succession_ceremony.py``, ``test_identity_succession_aftermath.py``,
``test_succession_sweep_retry.py``) when a fourth journey needed them — the
instance-lock suite's bound-seat succession. One home, so the traps each read
guards against (below) are guarded once. :func:`succeed_identity_from` joined
2026-09-19 from the tui, windows and apple bound-seat copies when linux's
became the fifth. :func:`require_stolen_gate` joined 2026-09-27 from the
ceremony, aftermath and kit-restore journeys' three copies.
:func:`wait_for_successor_actor` joined 2026-09-28, replacing the "wait until
settled, then read again" pair twelve journeys carried, whose second read raced
the switch it was waiting on; it also waits for the successor's live session.
"""
from __future__ import annotations

import re

from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until

#: An actor id and a RecoveryKey secret both render as 64 hex characters.
SECRET_HEX_LEN = 64

_HEX64 = re.compile(r"[0-9a-fA-F]{64}")

#: The succession tears one session down and launches another over a fresh
#: identity: a full re-launch, a silent challenge and a status read. Sized far
#: above any non-pathological run on a loaded box — a green run pays nothing
#: for the ceiling, and this is the alternative to a sleep, not a supplement to
#: one (e2e-conventions.md convention 14).
SUCCESSION_AND_RELAUNCH_S = 120.0


def settled_actor_id(app) -> str:
    """The Status page's actor id, or "" while the app is mid-relaunch.

    During the switch the app tears its session down and comes back through the
    launch machine, so the Status page is transiently absent. Re-navigating on
    each poll is what makes the wait observe the *new* session rather than a
    stale render of the old one; a driver error mid-teardown reads as "not
    settled yet" rather than as a failure. ``account-actor-id`` is
    Status-page-only (``settings.md`` § Live-data placement), so this
    deliberately does not call ``open_recovery_kit()`` — that would navigate
    away from the element read below. ``_navigate_subpage("status")``, not
    plain ``.navigate()``: iOS's idiomatic settings nav lands a plain navigate
    on the root page *list*, not the Status sub-page
    (``test_settings.py::test_actor_id_visible``) — read on every poll tick
    here, so a plain navigate would read empty for the whole ceiling.

    ⚠ **Only a whole actor id counts as settled.** A page still loading paints
    a placeholder (windows' Status page reads ``"--"`` until its load lands —
    measured 2026-09-28), and a wait that took any non-empty, non-old read as
    "the successor" handed ``"--"`` on to a nest query. Anything that is not 64
    hex reads as "not settled yet", the same answer as a mid-teardown driver
    error.
    """
    try:
        app.settings._navigate_subpage("status")
        actor = app.settings.actor_id()
    except Exception:
        return ""
    if len(actor) != SECRET_HEX_LEN or any(c not in "0123456789abcdefABCDEF" for c in actor):
        return ""
    return actor


def wait_for_successor_actor(
    app, old_actor: str, *, diagnose=None, guard_alive: bool = False
) -> str:
    """Wait until ``app`` is signed in as the successor of ``old_actor``; return
    the successor's actor id.

    Two waits, and each closes a race a single one leaves open:

    * **The id comes from the wait itself.** Never wait on
      :func:`settled_actor_id` and then call it again for the value: the second
      read is a fresh navigate and can land in a later relaunch tick that reads
      ``""``. Measured on windows 2026-09-28: the wait passed, the re-read
      returned ``""``, and the journey then queried the nest for
      ``actor_id=''`` (``invalid actor_id hex``) — a red about the harness, not
      the app.
    * **Then the live session, not just the rendered id.** A rendered successor
      id proves only that the page read it, and on windows it reads the
      registry's active pointer before the switch's rebuild has connected.
      ``await_session_actor`` asserts the state provider's
      ``session.{authenticated,actor_id}``, which no app publishes before its
      launch machine is Online as that actor. Only then is the switch finished,
      so a later step (or the teardown's ``reset``) cannot land mid-switch.

    ``diagnose`` extends the first wait's message (a journey's own context, say
    a console tail); the second carries ``await_session_actor``'s own.
    ``guard_alive`` makes the first wait an instance-lock suite's
    ``wait_alive_until`` on ``app.driver``, so a seat that dies of its own
    binding mid-ceremony is reported with its stderr at once.
    """
    from helpers.waiting import await_session_actor

    def _successor() -> str:
        actor = settled_actor_id(app)
        return actor if actor not in ("", old_actor) else ""

    def _diagnose() -> str:
        # The error surface is masked: on a persist-failure arm it carries the
        # only copy of the successor's secret key, and a failure message lands
        # in logs and ledgers. The two actor ids stay readable — they are the
        # diagnosis, and neither is a secret.
        error = _HEX64.sub("<64-hex>", app.error_text() or "")
        return (
            f"still reads actor={settled_actor_id(app)!r} (old={old_actor!r}) "
            f"error={error!r}"
            + (diagnose() if diagnose is not None else "")
        )

    if guard_alive:
        from helpers.instance_guard import wait_alive_until

        new_actor = wait_alive_until(
            app.driver, _successor, SUCCESSION_AND_RELAUNCH_S,
            "before the Status page settled on the successor", _diagnose,
        )
    else:
        new_actor = wait_until(_successor, SUCCESSION_AND_RELAUNCH_S, diagnose=_diagnose)
    await_session_actor(
        app.driver, new_actor, budget_s=SUCCESSION_AND_RELAUNCH_S,
        what="the switch to the successor",
    )
    return new_actor


def kit_on_screen(app) -> str:
    """The kit secret currently displayed, or "" while the app is mid-relaunch.

    The deliberate inverse of :func:`settled_actor_id`: that one re-navigates
    on every poll so it observes the new session, and this one navigates
    **nowhere**, because entering the Account sub-page clears any kit on screen
    (the shown-once custody rule — ``identity-succession.md`` § The RecoveryKey
    → *Custody*). The successor's post-auth hook has already landed the app on
    the section that renders it, so there is nothing to navigate to; a poll
    that "helpfully" did would destroy what it was polling for.

    A driver error mid-teardown reads as "not on screen yet" rather than as a
    failure, for the same reason it does there.
    """
    try:
        return app.settings.recovery_kit_secret()
    except Exception:
        return ""


def closing_act_console(app, *, label: str = "") -> str:
    """The browser console tail, for a web failure of the closing act — "" on
    every other app.

    Convention 6, applied where these journeys need it most. The closing act is
    a chain of four steps across a document swap (the ceremony parks the
    obligation; the launch peeks it and navigates; the section claims it; the
    mint shows it), and **every** break in that chain presents identically from
    the DOM: no kit on screen, empty error surface. Which step failed is
    observable only in the console, where each step reports itself — so a
    failure without these lines sends the reader back to static call-graph
    reasoning about a four-link chain.

    ``label`` names the page when a journey holds more than one tab, since each
    tab keeps its own ring. Web-only because ``console_log`` is: the native
    drivers have no browser ring, and their own equivalents are already in
    their app logs. Lifted 2026-09-21 from ``test_identity_succession_ceremony.py``
    when the second-tab journeys needed it.
    """
    if not app.driver.is_web():
        return ""
    name = f" ({label})" if label else ""
    try:
        lines = app.driver.console_log()
    except Exception as e:  # a driver mid-teardown must not mask the real failure
        return f"\n(console{name} unavailable: {e})"
    # The chain's own reports are written early — the ceremony, the relaunch's
    # silent sign-in, the landing — and a busy page can push them out of a
    # tail, so they are quoted from the whole ring first.
    chain = [ln for ln in lines if any(m in ln for m in _CHAIN_MARKERS)]
    return (
        f"\nbrowser console{name} — the chain's own lines:\n" + "\n".join(chain[-40:])
        + f"\nbrowser console{name} (tail):\n" + "\n".join(lines[-40:])
    )


#: The console prefixes each link of the closing act (and the relaunch under
#: it) reports itself with — `[succession]` in `$lib/wasm`, the settings page
#: and the root layout; `[identity]` for the relaunch's silent sign-in
#: (`$lib/store`); `[launch]` for the launch route; `[pageerror]` for what
#: escaped every handler.
_CHAIN_MARKERS = ("[succession]", "[identity]", "[launch]", "[pageerror]", "[actor-scope]")


def succeed_identity_from(app) -> tuple[str, str]:
    """Run the theft ceremony from ``app``'s seat exactly as the sibling
    journeys drive it (create a kit, then "my identity was stolen" with it).

    Returns ``(old_actor, held_kit)``; the caller waits for the closing act
    and the switch itself, since what it asserts about them differs. The
    64-hex reads are the ones ``test_identity_succession_ceremony.py`` asserts
    on every app, so a seat that renders neither fails here, before the
    ceremony, with its own error surface in the message.
    """
    app.settings._navigate_subpage("status")
    old_actor = app.settings.actor_id()
    assert len(old_actor) == SECRET_HEX_LEN, (
        f"the Status page must render the seat's actor id; got {old_actor!r}, "
        f"error surface: {app.error_text()!r}"
    )
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error: {app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.succeed_identity_with_held_kit(held)
    return old_actor, held


#: The stolen gate's reveal budget (see :func:`require_stolen_gate`). Only has
#: to cover a scroll plus the section finishing its render — no network, no
#: nest round trip — so it is sized like ``test_identity_succession_aftermath.py``'s
#: ``_PAGE_RENDER_S``, not like the journey budgets. Generous against a loaded
#: box; costs nothing when green.
STOLEN_GATE_REVEAL_S = 20.0


def require_stolen_gate(app) -> None:
    """Skip, declaring the class, on an app that renders the section but not
    the succession driver.

    Deliberately a *separate* gate from the section's: an app can render the
    status line and the create ceremony without having wired the succession
    driver, and folding the two would make this test report "no section" for an
    app that has one.

    ⚠ Reveals before reading, and polls — for the two distinct reasons a
    point-in-time bare ``is_visible`` here manufactures a false ``skip_unbuilt``:

    * **Below the fold.** The stolen pair sits under the section's wrapping
      multi-line warning, and ``open_recovery_kit`` only scrolls as far as
      ``recovery-kit-create-button``. On windows ``is_visible`` is UIA
      ``IsOffscreen`` — *in the viewport right now* — so a fully built, fully
      drivable gate reads False (measured 2026-08-24: ``count=1 visible=False``,
      while the journeys pass with the probe removed). ``is_visible_scrolled``
      is the driver contract's own answer to exactly this
      (``drivers/base.py``: a bare ``is_visible`` "conflates *the app never
      rendered it* with *it is one scroll away*"), and it degrades to a plain
      read on drivers without scroll support, so the other six are unaffected.
    * **Still rendering.** Same reason ``_require_member_review_page`` polls in
      ``test_identity_succession_aftermath.py`` — a read taken mid-render
      declares a built surface unbuilt. Scrolling alone would not cover this;
      waiting alone would not cover the fold.

    Neither masks a genuinely absent gate: an element no app ever rendered stays
    False for the whole budget and still skips.
    """

    def _revealed() -> bool:
        return app.driver.is_visible_scrolled("identity-stolen-confirm-field")

    try:
        wait_until(_revealed, STOLEN_GATE_REVEAL_S, diagnose=lambda: "")
        return
    except Exception:
        pass
    skip_unbuilt(
        app.driver,
        surface="identity-stolen-confirm-field",
        detail=(
            "settings.md § Recovery kit — the type-to-confirm gate on "
            "identity-stolen-button; tui leads"
        ),
        tracked=(
            "docs/goal/behavior/identity-succession.md "
            "§ Implementation status today"
        ),
    )
