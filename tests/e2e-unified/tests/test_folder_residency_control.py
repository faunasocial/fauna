"""tier_3 — the content-residency control itself (`folder-nest-residency-select`,
`folder-residency-confirm`; folders re-model phase 5, tui-led 2026-08-20).

**Why this exists.** `test_orchestrator.py::test_ws_metadata_only_folder_relays_
bytes_without_resting_them` proves the *consequence* — a metadata-only folder's
bytes never rest on the nest — with two real daemons and no UI at all. Nothing
proved the *control*. So the consent gate on a flip that deletes the nest's only
copy of a folder's content had, on every one of the 7 apps, no test of its own:
`behavior/file-sync.md` § Content residency says it follows the
`folder-audience-public-confirm` pattern, and that sentence was load-bearing
prose no run had ever checked.

That gap is not theoretical. linux's leg (landed 2026-08-21) reset its picker
only on **cancel**, so for as long as the confirm sat unanswered the row painted
`metadata_only` while the folder was still `full` — found and fixed 2026-08-27
by reading the two confirms side by side, not by any
test. This file is the test that would have caught it, and it is deliberately
the exact shape of `test_folder_audience_control.py`: the two confirms on this
page are ONE rule, and their tests should fail for the same reasons.

The three, and why each is a separate assertion rather than one end-state check:

1. **Picking `metadata_only` arms; it does not flip.** A regression that flipped
   on the bare select would pass any test that only looked at the end state, so
   the un-answered state is asserted on the NEST: nothing may have moved yet.
   The stakes are why v1 has no custody-inferred softening — the owner's
   explicit consent is the only gate there is.
2. **While armed, the select keeps painting the folder's CURRENT residency.**
   This is the assertion linux failed, and it is the one an app is most likely
   to get wrong: a picker that owns its own selection state (a GTK `DropDown`, a
   WinUI `ComboBox`, a Compose dropdown) commits the pick the instant it is
   made, and the leg must put it back by hand on the arming path. Apps that
   re-render from state (tui, apple's snapshot-bound `Picker`) pass it for free.
3. **Answering the confirm is what flips it** — and the flip back to `full`
   commits directly, because only one direction destroys anything.

MUTATION is UI-driven throughout (convention 8 — the select is selected and the
confirm is clicked, never a raw `folders.update`); the folder is fixture setup
via the ordinary wizard (the documented carve-out), and VERIFICATION reads the
nest's ground truth through `fauna.folders.list`, the black-box idiom its
siblings `test_folder_audience_control.py` / `test_folder_nest_place.py` use.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.set_names import find_set
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui led phase 5; linux joins with the leg that fixed its optimistic paint.
    # The other five join as theirs are graded — this marker list is the parity
    # ledger, exactly as `test_folder_audience_control.py` keeps its own.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    # web's leg landed with the control itself; its marker followed in web's
    # catalog trickle-down pass.
    pytest.mark.web,
    # apple joined 2026-09-21, once the real bug below was measured and fixed.
    pytest.mark.macos,
    pytest.mark.ios,
    # ── The apple bug this file found, and what it actually was ──────────
    #
    # For three passes `test_the_flip_back_to_full_needs_no_confirm` failed on
    # macos+ios while its twin passed, and two plausible mechanisms were
    # proposed and REFUTED before the third was measured. Recorded so nobody re-proposes them:
    #
    #   * NOT `residencyArmed` going unset by the seam ALONE (pass 2's fix 1),
    #     NOT resetting it before vs. after the `await` (fix 2 — resetting
    #     BEFORE tears the alert down with the mutation still queued, LOSES the
    #     write, and reds the arming leg too), NOT `@MainActor` on the confirm
    #     methods (fix 3), NOT a no-op `Picker` write-back (fix 4).
    #   * NOT "`.automationActivate` registers the confirm whether or not the
    #     alert is presented" (pass 3's reading). The slot really is created
    #     only on present.
    #   * NOT the vote mechanism pass 3 derived from the registry either. That
    #     predicted the dismissed slot would carry the detach vote alone; the
    #     measurement found ZERO votes, so pass 3's probe (read the slot's
    #     `votes=`) settles nothing on its own and is retired with the rest.
    #
    # The measurement that settled it, dumping the registry after answering the
    # confirm: `folder-residency-confirm [0] VISIBLE(no-geo) geo=nil votes=-`,
    # and — the smoking gun — the NEXT test's folder wizard never opened. Two
    # independent defects, and BOTH had to be fixed:
    #
    #   1. **The alert was genuinely still on screen.** A real press of a
    #      SwiftUI alert Button runs the action AND lets SwiftUI clear the
    #      `isPresented` binding; `.automationActivate` reaches the action only,
    #      so the seam answered the confirm and left the modal up — which is why
    #      a later sheet could not open. The seams now mirror the whole press
    #      (`FoldersContent`, all three folder confirms).
    #   2. **An alert's content can never de-register its ids.** SwiftUI fires
    #      `.onAppear` for alert content but not `.onDisappear`, and the
    #      automation sentinel rides in `.background(...)`, which an alert never
    #      realizes — hence `geo=nil` and `votes=-`: no window-detach vote, no
    #      probe death, nothing. So even a correctly dismissed alert would keep
    #      answering `is_visible` = true forever. The host's binding now
    #      declares the ids' lifetime (`View.automationPresentation`, over
    #      `AutomationRegistry.hideAll`), which is the general fix for every
    #      `.alert`/`.confirmationDialog`-hosted control on both apple targets.
    #
    # A conditionally-rendered invisible shim was never available — convention 1
    # forbids it. Headless regression tests for (2) live in
    # `AutomationRegistryTests.swift`; (1) is covered by this file's own arm.
    #
    # ⚠ Historical note, kept because it was the reason apple sat out
    # for three passes: this was never an unbuilt control. The arming leg
    # passed on macos+ios all along; only the flip-back leg failed, and it
    # failed because the two defects above compound — the seam left the
    # alert up, and nothing could have retired the confirm's id even if it
    # had not. That is also why the arming leg and the audience twin stayed
    # green throughout: neither ever asserts a confirm is GONE.
]

# Generous ceilings, not expectations (convention 14): every wait below is a
# deadline poll on STATE, never a settle-sleep.
_FLAG_WINDOW_SECS = 30.0
_UI_WINDOW_SECS = 15.0

_FULL = "full"
_METADATA_ONLY = "metadata_only"


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _folder_row(client, name: str) -> dict | None:
    """The nest's own row for `name`, or None. Ground truth — never the app's
    rendering of it."""
    with client:
        reply = client.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


def _row_residency(row: dict) -> str:
    """Classify the nest row's residency the way the wire type says to.

    ⚠ **The projection spells `full` as ABSENT, not as the string `"full"`**
    (`libs/fauna-protocol/src/folders.rs` — "Absent/empty/unrecognised ⇒ full
    residency"; the explicit token is sent only on the flip BACK, as
    `FolderUpdateRequest.residency`). So a test that asserts `row["residency"]
    == "full"` asserts a spelling the nest never emits and can only fail. Apply
    the same fail-closed classification the apps apply — which is also what
    makes this ground truth rather than a second guess at it.
    """
    return row.get("residency") or _FULL


def _await_residency(client, name: str, residency: str) -> dict:
    """Poll the nest row until `residency` holds.

    The failure names both the wanted and the observed row: the two interesting
    ways this fails — the gesture never reached the nest, and the nest refused
    the transition — are distinguishable only from the row itself.
    """

    def _matches():
        row = _folder_row(client, name)
        if row is None or _row_residency(row) != residency:
            return None
        return row

    return wait_until(
        _matches,
        _FLAG_WINDOW_SECS,
        interval=0.5,
        diagnose=lambda: (
            f"wanted residency={residency!r}, nest row is "
            f"{_folder_row(client, name)!r}"
        ),
    )


@pytest.mark.feature("folders")
def test_the_residency_control_arms_before_it_evicts(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"residency-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    # The control renders on expand and reads `full` — the fail-closed
    # normalization, which is also what a fresh folder genuinely is.
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-nest-residency-select", timeout=_UI_WINDOW_SECS)
    assert b.residency_current_value() == _FULL, (
        "a fresh folder paints `full`; anything else means the select is "
        "showing a raw column value instead of the normalized one"
    )

    # ── 1. Picking `metadata_only` ARMS. It must not flip. ──
    b.set_residency(_METADATA_ONLY)
    wait_until(
        b.residency_confirm_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"confirm not armed; error={app.error_text()!r}",
    )

    # ── 2. …and while armed, the select still paints the CURRENT residency. ──
    assert b.residency_current_value() == _FULL, (
        "while the confirm is armed the select must keep painting the folder's "
        "CURRENT residency — painting `metadata_only` before the answer reports "
        "a residency the folder does not have, on the one gate that exists to "
        "ask before the nest's only copy of the content is deleted"
    )

    # …and the nest has not moved. This is the assertion that makes the arming
    # real rather than decorative.
    row = _folder_row(client, name)
    assert row is not None and _row_residency(row) != _METADATA_ONLY, (
        "arming the residency confirm must write NOTHING — a folder that went "
        "metadata-only on the bare select would have had the nest drop its "
        "chunk bytes without its owner having answered the one gate that exists "
        f"to ask them. Nest row: {row!r}"
    )

    # ── 3. Answering the confirm is what flips it. ──
    b.confirm_residency()
    _await_residency(client, name, _METADATA_ONLY)


@pytest.mark.feature("folders")
def test_the_flip_back_to_full_needs_no_confirm(
    logged_in_app, nest_instance, test_user
):
    """Only one direction destroys anything. Going back to `full` adds a nest
    copy rather than removing one, so it commits on change like every other knob
    on this block — arming it too would teach the user that the confirm is
    noise, which is exactly what must not happen to a consent gate."""
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"residency-back-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-nest-residency-select", timeout=_UI_WINDOW_SECS)

    # Get there through the gate, since that is the only door.
    b.set_residency(_METADATA_ONLY)
    wait_until(
        b.residency_confirm_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"confirm not armed; error={app.error_text()!r}",
    )
    b.confirm_residency()
    _await_residency(client, name, _METADATA_ONLY)

    # ⚠ Re-expand ONLY IF NEEDED before reading the control again — the
    # `folder-audience-public-confirm` / `folder_bound_flip_back` precedent
    # (`b.audience_select_visible()` there). A write refreshes the page
    # machine, and an app is free to rebuild its whole folder list off the new
    # snapshot; whether the rebuilt `AdwExpanderRow` keeps its expanded state
    # is NOT guaranteed (linux's `crate::confirm_dialog` migration off
    # `adw::MessageDialog` onto the embedded `adw::AlertDialog` flipped it from collapsed-on-rebuild to staying expanded here —
    # a real, observed behavior change, not a guess), so `find_and_expand_folder`
    # is a TOGGLE and calling it unconditionally can CLOSE an already-expanded
    # row instead of opening a collapsed one. No goal doc asks expansion to
    # survive a refresh either way, so checking first is the only shape that is
    # correct under both.
    #
    # ...and checking ONCE is not enough on linux, which rebuilds its whole
    # folder list on every `DevicesMachine` snapshot, collapsing every row. A
    # write lands a BURST of them — three within 0.4 s of this confirm in the
    # 2026-09-14 whole-suite sweep, on a session actor holding 19+ folders — so a
    # single check-then-toggle opened a row the next snapshot closed, and the wait
    # timed out on an absent select (red in the 09-10, 09-11 and 09-14 sweeps,
    # green solo). The re-expand therefore repeats, still only while the select is
    # absent, until the select paints.
    def _paints(value: str) -> bool:
        if not app.driver.is_visible("folder-nest-residency-select"):
            b.find_and_expand_folder(name)
            return False
        try:
            return b.residency_current_value() == value
        except (LookupError, TimeoutError):
            return False  # a snapshot closed the row between the check and the read

    # The select paints the new value — the repaint comes from the nest's row,
    # not from the pick that asked for it.
    assert wait_until(
        lambda: _paints(_METADATA_ONLY),
        _UI_WINDOW_SECS,
        diagnose=lambda: (
            "after the confirm the select must paint the folder's new residency, "
            "read back from the nest's own row: "
            f"{app.driver.diagnose('folder-nest-residency-select')}"
        ),
    )

    # ── The flip back commits directly. ──
    if not app.driver.is_visible("folder-nest-residency-select"):
        b.find_and_expand_folder_until(
            name, "folder-nest-residency-select", timeout=_UI_WINDOW_SECS
        )
    b.set_residency(_FULL)
    _await_residency(client, name, _FULL)
    # The nest converging is not the app being done: a confirm armed AFTER the app's own
    # commit is on screen only once the app has resumed from that write, so the negative read
    # below is not evidence until the select has repainted the new value — the same settle
    # the first flip uses. (A mutant arming the confirm after the commit survived this test
    # before the settle was added.)
    assert wait_until(
        lambda: _paints(_FULL),
        _UI_WINDOW_SECS,
        diagnose=lambda: (
            "after the flip back the select must paint the folder's restored residency: "
            f"{app.driver.diagnose('folder-nest-residency-select')}"
        ),
    )
    assert not b.residency_confirm_visible(), (
        "the flip back to `full` must not arm the destructive confirm — it "
        "restores the nest's copy rather than deleting it, and a gate that "
        "fires on a harmless direction trains the user to click through it"
    )
