"""`_AutomationRegister` lifecycle: the regression gate for the closure fix, for the iOS
Gap-B self-unregister fix, and for the stale-sheet-slot wizard defect (apple only).

Registration on both apple apps runs through one ViewModifier
(`FaunaKit/Sources/FaunaKit/Testing/AutomationRegistry.swift`), which backs **every**
`automation*` element — so anything wrong in it is wrong fleet-wide, and apple's standing
policy is not to touch it without an isolated repro carrying a CONTROL. That is this file.

Since the 2026-07-16 window-attachment lifecycle (goal doc rule 2a) the registry is driven
by THREE signals: **show** = window attach (an `_AttachmentSentinel` representable in every
registered element's `.background`; `.onAppear` doubles as an upsert-idempotent fallback) —
an upsert that un-hides a kept slot IN PLACE, restoring document order; **off-screen** =
window detach → `hide` (slot kept; `.onDisappear` hides only when the sentinel agrees the
view is really detached — a disappear while still attached is a cover, not an exit); and
**gone** = view-identity death (`@State` storage deinit) → `unregister`, the only signal
that removes. `refresh` still swaps closures + path on every body pass, in place by token,
never touching visibility or order.

**Class 1 — the STALE ENTRY CLOSURE. ✅ FIXED (;
post-fix green observed on macOS 2026-07-16). The two tests below are its REGRESSION
GATE — they must stay green.** Before the fix, the registry served the closures captured at
a view's first `.onAppear` forever. A closure over a *reference* type (a VM class) still
read live data and survived by luck; a closure over a *value* type froze — and
`FoldersContent.swift`'s per-row controls all capture `let folder: FolderSummary`. The
severity split was whether the WRITE derived from the READ: the pickers do
`set: { v in apply(v) }` (harness supplies the value ⇒ write correct, **read-back** stale —
that is what these two tests pin), while `FolderWebdavToggle` did `{ apply(!isOn) }` over a
stale `isOn` ⇒ its second (OFF) click recomputed `apply(true)`, an idempotent no-op. That
was the folder serve-OFF bug. Same cause, now closed. **This file keeps the A/B anyway** —
a fix that regresses would otherwise be invisible, since a stale read looks exactly like a
pass to any test that never remounts.

**Class 2 — the SELF-UNREGISTER (iOS Gap B). ✅ FIXED (2026-07-16, the window-attachment
lifecycle; the gesture test below is its REGRESSION GATE).** The old lifecycle removed the
slot on `.onDisappear` and could only restore it from a fresh `.onAppear` — but SwiftUI's
pair is UNBALANCED for a `NavigationStack` root: a detail push fires the root's
`.onDisappear`, and the pop fires **no** matching `.onAppear`, because the root was never
structurally removed — it never "re-appears". So one push permanently deregistered every
root-level element. The bisect trace that pinned it (iOS, 2026-07-12, pre-fix)::

    navigate=1 → switch-month=1 → switch-week=1 → switch-day=1 → switch-agenda=1
              → create-event(sheet)=1 → open-detail(push)=1 → leave-detail=0

`calendar-date-label` survived every view-mode switch **and** the compose sheet — then was
gone once the pushed detail was left, unrecoverable by any re-`navigate()` (the property
that long masqueraded as a render race). The label is maximally exposed because it lives in
the *persistent* header `HStack` (`Fauna-iOS/Views/Events/CalendarListView.swift`:54-64),
OUTSIDE the `Group { if viewMode == … }` (`:67`) that swaps — its identity, and its token,
survive everything. The platform window attachment IS balanced (UIKit re-attaches the
uncovered root's view on pop), which is exactly the signal the sentinel now carries.

⚠ **View-mode switches and `resetToFactory` cycles alone did NOT reproduce it** (probed
2026-07-12: a minimal sequence of exactly those passes on iOS *and* macOS, unfixed). The
long-retired `_skip_ios_date_label_gap` docstring blamed "resetToFactory cycles +
view-mode switches", which is why the trigger stayed hidden — it was the detail **push**.

**Class 3 / Class 4 — flat occurrence indices were REGISTRATION order, which SwiftUI does
not deliver in document order. ✅ FIXED (2026-07-16, geometry-ordered resolution).** The old
flat `id -> [Slot]` order was append-order: an iOS lazy `List` registers rows as they
materialise, and a macOS `Form` registers its rows **bottom-up** — which made flat index 1
of the 7 `wizard-frequency-option` rows resolve to the 6th option (21600s), the real cause
of the "wizard frequency pick does not land" defect (Class 4 below; the pick landed, on the
wrong option). Lookups now sort same-id visible slots by their attachment-sentinel's live
window geometry (top-to-bottom, then left-to-right), so a flat index means "the N-th one on
screen" and agrees with `.automationScope`'s visual indices on both platforms. Identity-based
row resolution (`_row_scope_index`) remains the more robust idiom for content-addressed rows
and stays.

**The PROBE changed on 2026-08-20 — the property did not.** Every test here used the
scan-frequency controls (`folder-frequency-select`, `wizard-frequency-option`) as its probe
because they were the per-row / same-id-repeated controls closest to hand. Folders re-model
phase 5 retired both (the reconcile cadence is a hard-coded constant — `file-sync.md`
§ Config, the phase-5 block), so the probes moved to `folder-conflict-policy-select`: the
SAME shape (a per-row `Picker` over `let folder: FolderSummary`, applied on change through
`DevicesMachine::set_folder_conflict_policy`, nest-observable on `fauna.folders.list`'s
`conflict_policy`), one per owner row, registered by the same `FoldersContent.swift`
header `HStack`. Class 1 reads back a per-row write with and without a remount, exactly as
before; Class 4's same-id flat-index property is pinned across two rows of that picker
(two `Form` rows registering bottom-up is the defect's own geometry) rather than across the
seven buttons of a wizard step that no longer exists. These re-probed tests are apple-only and
were re-shaped on the Linux dev VM, where they cannot run — **their first apple pass landed
2026-08-25: all four green on both macOS and iOS**, no probe-shape
surprises.

**How to run it.** ``--runxfail`` gives hard PASS/FAIL while iterating on a candidate fix::

    pytest tests/e2e-unified/tests/test_automation_registry_lifecycle.py \
        --client ios --runxfail -v

**The CONTROL is the load-bearing half.** `test_picker_read_back_is_fresh_after_a_remount`
gets its correct read by *remounting*, so it would stay green even against a broken registry
— that is what makes it a control: it pins the variable. A candidate Gap-B fix that makes
registration blindly re-append would "resurrect a dead row out of document order and shift
every survivor's index" (the registry's own words) — and that surfaces here as the control or
the Class-1 pair going red, rather than as a silent, fleet-wide index corruption.

**VERIFICATION STATE.** The pre-fix reproductions (2026-07-12) were driven for real on
`--client macos` AND `--client ios` against the pre-fix tree: Class 1 reproduced
deterministically on both, Class 2 reproduced on iOS with the trace shown, Class 3 was
caught on iOS, Class 4 on both. The post-fix passes (2026-07-16, the window-attachment
lifecycle): every test in this file green on macOS and iOS — all former xfail/skip markers
deleted, so the whole file is now a hard regression gate.

Goal docs: `docs/goal/architecture/apps/apple-e2e-automation.md` (registration
lifecycle) + `docs/goal/ui/events.md`.
"""

import secrets
import time
from datetime import datetime, timedelta

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

# apple-only: `_AutomationRegister` is FaunaKit's. The other five apps (including tui) have
# their own registries and are not exposed to this defect class.
pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.ios,
]

# The conflict-policy catalog (`fauna_folders_machine::conflict_policy_options`; the values
# `folder-conflict-policy-select` renders and writes — `file-sync.md` § Conflicts, policy).
#
# ⚠ Do NOT hard-code the starting value. A wizard-created set starts on whatever the
# machine stamped — the user's injected default, else the nest column default (`auto`) —
# and nothing here should assume which. So these tests CALIBRATE off the nest instead:
# read the actual starting value, then pick the catalog value that differs. That keeps
# the repro honest about what it is proving (a stale READ) rather than smuggling in an
# unverified assumption about the WRITE.
_POLICY_CATALOG = ("auto", "latest_wins_always")


def _a_different_policy(current: str) -> str:
    """The catalog policy that is not `current` — so a read-back is distinguishable."""
    return next(p for p in _POLICY_CATALOG if p != current)


def _set_policy(b, policy: str, row: int) -> None:
    """Write `folder-conflict-policy-select` on `folder-row[row]` by SCOPE.

    Apple registers `.automationScope(Ids.folderRow, index:)` on every row
    (`FoldersContent.swift`), so the scoped address resolves here. The shared
    `BackupsActions.set_conflict_policy` now rides the same scope path on apple
    too (flipped 2026-08-25, row 153) — this file still addresses the picker
    directly rather than going through it, deliberately: this is the isolated
    registry-lifecycle CONTROL (module docstring), and it must not depend on a
    shared helper another change could later re-break underneath it.
    """
    b.driver.select("folder-conflict-policy-select", policy, scope=f"folder-row[{row}]")
    time.sleep(1)  # sleep-ok: the shared helper's own post-select settle, mirrored


def _get_policy(b, row: int) -> str:
    return b.driver.get_text("folder-conflict-policy-select", scope=f"folder-row[{row}]")

def _nest_sets(nest_url, test_user) -> list[dict]:
    """Every folder row the logged-in actor owns, straight off the nest
    (`fauna.folders.list`) — the ground truth that separates a bad WRITE from a bad READ.
    A sealed set's row rests no plaintext name (schema 114): find one with
    `helpers.set_names.find_set`, by its `name_hash`.
    """
    client = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        reply = client.call("fauna.folders.list", {})
    return list(reply.get("folders", []))


def _nest_policy(nest_url, test_user, name: str) -> str:
    """The nest-authoritative `conflict_policy` for the set called `name`
    (absent on the wire = the column default, `auto`)."""
    from helpers.set_names import find_set

    row = find_set(_nest_sets(nest_url, test_user), name)
    if row is None:
        raise AssertionError(f"folder {name!r} is not on the nest")
    return row.get("conflict_policy") or "auto"


def _row_scope_index(b, name: str) -> int:
    """The `.automationScope` index of the folder row called `name`.

    ⚠ **This is NOT the index `find_and_expand_folder` returns.** That one is the row's
    position in the registry's flat `id -> [Slot]` list, i.e. **registration order** —
    the order `.onAppear` happened to fire. `.automationScope("folder-row", index:)`
    numbers rows in **visual order** (`Array(folders.enumerated())`). The two agree on
    macOS and **diverge on iOS**, whose lazy `List` registers rows as they materialise —
    so passing the flat index as a scope index there reads a *different row* (observed
    2026-07-12: nest said 21600s while `folder-row[1]` read the 60s this file's control
    test had written to another set). That divergence is defect class 3 in this module's
    docstring: `Slot.path`, like `Slot.entry`, is frozen at registration.

    So resolve the row by **identity rather than position**: `folder-row` registers its
    own automation value as the set name (`FoldersContent.swift`:228), and a row's own
    slot sits inside its own scope, so a scoped read returns that row's name.
    """
    for j in range(b.folder_count()):
        if name in b.driver.get_text("folder-row", scope=f"folder-row[{j}]"):
            return j
    raise AssertionError(
        f"no folder-row scope index resolves to {name!r} among {b.folder_count()} rows "
        f"— {b.driver.diagnose('folder-row')}"
    )


def _seed_expanded_sync_set(b, name: str) -> int:
    """Create a folder, expand its row, and return that row's index.

    The index is load-bearing: `folder-conflict-policy-select` renders in the COLLAPSED
    header (`FoldersContent.swift`, the row's `HStack`), above the expander gate — so
    every owner row has one and the flat occurrence index addresses the *first* row,
    not the expanded one. Both Class-1 tests therefore address their picker by
    `scope="folder-row[i]"` (the `.automationScope("folder-row", index:)` retrofit).
    Without that, a second Sync set in the session-scoped nest would let this file write
    one row and read another — which would counterfeit exactly the staleness it is trying
    to prove.
    """
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    # Remount the page before reading any index. Creating a set APPENDS a slot to the
    # flat registry, while `.automationScope(index:)` numbers rows in VISUAL order — and
    # a row's scope path, like its Entry, is frozen at its first `.onAppear`. So after a
    # mid-list insert the two index spaces diverge, and we would expand one row while
    # scope-querying another. A remount re-registers every row in document order, making
    # the index `find_and_expand_folder` returns valid in BOTH spaces. (This does not
    # weaken the repro: staleness needs no remount between the WRITE and the READ, and
    # this one happens before the write.)
    b.navigate_devices()
    b.navigate_folders()
    b.find_and_expand_folder(name)  # expands by flat index — correct for the click itself
    return _row_scope_index(b, name)  # ...but the PICKERS must be addressed by scope index


# ---------------------------------------------------------------------------
# Class 1 — the stale entry closure. The two tests below differ by EXACTLY one
# step (the page round-trip), which is what isolates the defect to `.onAppear`.
# ---------------------------------------------------------------------------


def test_picker_read_back_is_fresh_after_a_remount(logged_in_app, nest_instance, test_user):
    """CONTROL — passes today. A remount re-fires `.onAppear`, so the closure is fresh.

    This is the A-side of the A/B and it must KEEP passing under any candidate fix. It is
    the same shape the retired `test_folders.py::test_edit_folder_frequency` had (green on
    both apple apps before phase 5), reproduced here so this file demonstrates both sides
    on its own.

    Leaving the page and returning tears down the row's view identity; the rebuilt row
    gets a fresh `.onAppear`, which registers a closure over the CURRENT `FolderSummary`
    — so the read-back is correct. Nothing about the write changed: the only variable is
    whether a remount happened.
    """
    b = logged_in_app.backups
    nest_url = nest_instance["url"]
    name = f"reg-remount-{secrets.token_hex(4)}"
    row = _seed_expanded_sync_set(b, name)

    target = _a_different_policy(_nest_policy(nest_url, test_user, name))
    _set_policy(b, target, row)

    # The one step the repro below omits: leave the page and come back, forcing a remount.
    b.navigate_devices()
    b.navigate_folders()
    b.find_and_expand_folder(name)
    row = _row_scope_index(b, name)

    assert _get_policy(b, row) == target, (
        "CONTROL FAILED — a remounted row must read back the value just written. If this "
        "is red, registration itself is broken rather than merely stale (or the "
        "`folder-row` scope retrofit does not resolve, in which case every test in this "
        "file is addressing the wrong row): "
        f"nest={_nest_policy(nest_url, test_user, name)!r}, "
        f"ui={_get_policy(b, row)!r} (expected {target!r}), "
        f"error={logged_in_app.error_text()!r}"
    )


def test_picker_read_back_reflects_an_in_place_write(logged_in_app, nest_instance, test_user):
    """REPRO (Class 1) — the same write, WITHOUT the remount, reads back stale.

    Identical to the control above except that it never leaves the page. The write lands
    on the nest (asserted first, so a failure here cannot be blamed on the write path),
    but the registry still serves the `value:` closure captured when the row first
    appeared — which closed over the pre-write `FolderSummary` struct.

    A user never sees this: SwiftUI re-derives the live `Binding` at interaction time, so
    the on-screen Picker is correct. ONLY the automation registry is stale — which is why
    it surfaces as an e2e-only defect, and why it silently corrupts any test that does two
    round-trips on one row without an intervening remount.
    """
    b = logged_in_app.backups
    nest_url = nest_instance["url"]
    name = f"reg-inplace-{secrets.token_hex(4)}"
    row = _seed_expanded_sync_set(b, name)

    # Calibrate: whatever the set actually starts at, the freshly-expanded row's closure was
    # registered over THAT value — so the UI must agree with the nest before any write. If
    # this is red, the row scoping is wrong and the staleness assertion below is meaningless.
    started_at = _nest_policy(nest_url, test_user, name)
    assert _get_policy(b, row) == started_at, (
        "PREMISE FAILED — a freshly-expanded row must agree with the nest before any write. "
        f"nest={started_at!r}, ui={_get_policy(b, row)!r}. Either `folder-row[{row}]` "
        "addresses the wrong row, or the read is broken independently of staleness."
    )

    target = _a_different_policy(started_at)
    _set_policy(b, target, row)

    # The WRITE is fine — pinning it isolates the defect to the READ.
    assert _nest_policy(nest_url, test_user, name) == target, (
        "the picker's `set:` closure takes the harness-supplied value, so the write must "
        "land even while the read is stale. If THIS is red the defect is bigger than a "
        f"stale read: nest={_nest_policy(nest_url, test_user, name)!r}, wrote {target!r}, "
        f"error={logged_in_app.error_text()!r}"
    )

    # THE DEFECT: same row, no remount — the registry serves the frozen `.onAppear` closure.
    assert _get_policy(b, row) == target, (
        "STALE ENTRY CLOSURE — the nest has the new value but the registry read-back is "
        f"frozen at the pre-write one ({started_at!r}). "
        f"nest={_nest_policy(nest_url, test_user, name)!r}, "
        f"ui={_get_policy(b, row)!r} (expected {target!r}). The ONLY difference "
        "from test_picker_read_back_is_fresh_after_a_remount (which passes) is that this "
        "test did not leave the page — i.e. `.onAppear` never re-fired, so the registry "
        "kept serving the closure captured over the pre-write FolderSummary. "
        f"{logged_in_app.driver.diagnose('folder-conflict-policy-select', scope=f'folder-row[{row}]')}"
    )


# ---------------------------------------------------------------------------
# Class 2 — the self-unregister (iOS Gap B). A preserved view identity means a
# stable @State token, so a reordered appear/disappear pair cancels itself out.
# ---------------------------------------------------------------------------


@pytest.fixture
def _calendar_backend(logged_in_app):
    """Mint the actor's MSEK so the Events page renders its real calendar surface.

    Same gate `test_events.py`'s autouse `_enable_calendar_backend` applies — the flipped
    Events page reads the encrypted CalDAV store and degrades to an empty "enable calendar"
    state without an MSEK. Idempotent across the session-scoped nest, and requested only by
    the events test below (the Class-1 folder tests do not need mail).
    """
    logged_in_app.mail_settings.navigate()
    logged_in_app.mail_settings.ensure_mail_enabled()


def test_calendar_date_label_survives_the_events_gesture_sequence(
    logged_in_app, _calendar_backend
):
    """REPRO (Class 2 / iOS Gap B) — which gesture unregisters `calendar-date-label`?

    The label lives in the events header `HStack`, OUTSIDE the `Group { if viewMode == … }`
    that swaps — so its view identity, and therefore its `@State` token, survives every
    view-mode switch. A self-unregister needs an appear/disappear pair delivered on that
    *stable* token, which is what a `NavigationStack` push/pop does to a root view on iOS.

    **The bisect this encodes (2026-07-12, `--client ios`).** View-mode switches +
    `resetToFactory` cycles alone do **NOT** reproduce Gap B — an earlier cut of this test
    drove exactly those and passed on both apps. But the FULL `test_events.py` module
    **does** reproduce it: with the skip guard bypassed, `test_month_navigation` and
    `test_calendar_date_label_visible` both fail on iOS with
    `[calendar-date-label: visible=False, count=0]`, while `test_switch_to_month_view`
    immediately before them passes. So the trigger is a gesture the module performs and a
    bare switch sequence does not — and the ones that differ all **push a detail view or
    present a sheet**. This test therefore walks the module's gesture classes and probes
    the count after EVERY one, so the failing transition names itself. That trace is the
    datum apple needs to aim a fix; `_skip_ios_date_label_gap`, a blanket skip, cannot
    give it to them.
    """
    app = logged_in_app
    d = app.driver

    trace: list[str] = []

    def step(label: str, gesture) -> None:
        """Drive one gesture, then record the label's registry count.

        The gesture is guarded: once the registry starts losing elements, a LATER gesture
        can 404 on its own button, and an unguarded exception would destroy the very
        trace we are here to collect. Record the breakage and keep probing — a failed
        gesture is itself a data point.
        """
        try:
            gesture()
        except Exception as exc:  # noqa: BLE001 — any gesture failure is signal, not noise
            trace.append(f"{label}=<gesture failed: {type(exc).__name__}>")
            return
        trace.append(f"{label}={d.count('calendar-date-label')}")

    step("navigate", app.events.navigate)

    # (a) view-mode switches — the sibling Group swaps, the header HStack persists.
    #     Known NOT to be the trigger on its own; kept so the trace shows it holding at 1.
    for mode in ("month", "week", "day", "agenda"):
        step(f"switch-{mode}", lambda m=mode: app.events.switch_view(m))

    # (b) a sheet presentation (event compose) — iOS presents this in a detached modal
    #     context, so whether it disturbs the root's appear/disappear is worth pinning.
    start = (datetime.now() + timedelta(days=1)).replace(microsecond=0)
    summary = f"reg-probe-{secrets.token_hex(3)}"
    step(
        "create-event(sheet)",
        lambda: app.events.create_event(
            summary,
            start.strftime("%Y-%m-%dT%H:%M"),
            (start + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M"),
        ),
    )

    # (c) THE TRIGGER — a NavigationStack detail PUSH, then leaving it.
    #
    #     Traced on iOS 2026-07-12:
    #       …switch-agenda=1 → create-event(sheet)=1 → open-detail(push)=1 → leave-detail=0
    #
    #     So the label survives every switch AND the sheet AND is still registered while
    #     the detail is on screen — then it is GONE once we leave. The push fires the
    #     ROOT's `.onDisappear` → `unregister(token)`; the return never fires a matching
    #     `.onAppear`, because a NavigationStack root is not structurally removed when a
    #     child is pushed over it, so it never "re-appears". That is exactly why a
    #     re-`navigate()` cannot recover the label — the property that distinguishes this
    #     from a render race, and the one nobody could previously account for.
    #
    #     NB `back_from_detail()` is NOT used here: it clicks `event-detail-back`, an
    #     id apple renders nowhere (grep `apps/fauna-apple/`), so it 404s on both
    #     apple apps and would report a phantom trigger. Leaving via `navigate()`
    #     pops the stack for real, which is what this probe needs.
    #     Update 2026-08-14: the helper is no longer caller-less —
    #     test_events.py::test_event_detail_back_returns_to_the_list drives it on the
    #     apps that paint the id. Apple's absence is now a RULED, DECLARED one, not a
    #     gap apple owes: `event-detail-back` is scoped to navigated event_detail
    #     surfaces that paint an in-surface control (tui/windows/android), and macos is
    #     split-view while ios leaves it to the system nav bar per HIG
    #     (ui/events.md § Element IDs, user-approved 2026-08-14). So this probe keeps
    #     using navigate() permanently — there is no future id here to switch to.
    step("open-detail(push)", lambda: app.events.open_event_detail(0))
    step("leave-detail", app.events.navigate)

    # (d) ...and a second navigate still cannot bring it back.
    step("re-navigate", app.events.navigate)

    dropped = [s for s in trace if s.endswith("=0") or "gesture failed" in s]
    assert not dropped, (
        "SELF-UNREGISTER — `calendar-date-label` fell out of the automation registry and "
        f"did not come back.\n  count by step: {' → '.join(trace)}\n  FIRST DROP: "
        f"{dropped[0]} — THAT GESTURE IS THE TRIGGER.\nThe label's identity (and its "
        "@State token) is preserved across it, so a reordered appear/disappear pair on "
        "one token unregisters exactly what it just registered, and `.onAppear` will not "
        "fire again for a view that never structurally re-appeared. "
        f"{d.diagnose('calendar-date-label')}"
    )


# ---------------------------------------------------------------------------
# Class 4 — flat occurrence indices resolved in REGISTRATION order, which SwiftUI
# does not deliver in document order (a macOS Form's rows register BOTTOM-UP).
# ---------------------------------------------------------------------------


def test_flat_index_resolves_same_id_rows_in_screen_order(
    logged_in_app, nest_instance, test_user
):
    """REPRO (Class 4) — flat index N of a same-id control must address the N-th one ON
    SCREEN, on a macOS `Form` that registers its rows bottom-up.

    The "wizard frequency pick does not land" defect handed over with the Gap-B repro
    (2026-07-12, reproduced on macOS AND iOS) was never a lost write: the pick always
    landed — on the WRONG option. A macOS `Form`'s rows register **bottom-up**, so the
    registry's flat occurrence index resolved against a REVERSED list and the app honestly
    persisted the mis-clicked value. The registry now resolves same-id occurrence indices
    in **on-screen document order** (top-to-bottom then left-to-right, off each element's
    attachment-sentinel geometry), so a flat index means "the N-th one on screen".

    The original probe — seven `wizard-frequency-option` buttons in one wizard step —
    retired with folders re-model phase 5. The property is pinned here on two folder rows' `folder-conflict-policy-select` instead: the same bottom-up `Form`
    geometry, two same-id pickers, and a nest-observable write per row. The test writes
    through FLAT index 1 (never a scope) and asserts on the nest that the row that sorts
    SECOND on screen changed while the first did not — under the reversed-registration
    defect the write lands on the first row instead, which is exactly the mis-addressed
    pick the old probe caught.

    Two rounds are asserted: round 1 pins the ordering itself; round 2 re-drives it
    after a page remount, pinning that a remount leaves no stale slots to shadow the
    re-registered rows (the kept-slot lifecycle removes them on identity death — the
    half of the old test that a dismissed wizard sheet used to exercise).
    """
    b = logged_in_app.backups
    d = logged_in_app.driver
    nest_url = nest_instance["url"]
    b.navigate_folders()
    # Two names that sort in a known order, so "second on screen" is a fact about the
    # list and not about creation order (apple renders `folders` in nest order, which is
    # the list's sort).
    stem = secrets.token_hex(4)
    first, second = f"reg-a-{stem}", f"reg-b-{stem}"
    created = [first, second]
    b.create_folder_via_wizard(first)
    b.create_folder_via_wizard(second)

    for round_no in (1, 2):
        # Remount so every row registers in document order before the probe — the
        # same precondition `_seed_expanded_sync_set` documents.
        b.navigate_devices()
        b.navigate_folders()
        # The two probe rows' on-screen order, by identity (`_row_scope_index` is the
        # visual index space). The probe below must address by FLAT index, so derive
        # the SECOND probe row's flat index from what is on screen: the picker's
        # flat index space is the owner-row subsequence in visual order (other
        # tests' Sync sets on the SESSION-scoped nest sit in it too — backup-type
        # rows carry no picker), so count the picker-bearing rows that sort above
        # it rather than assume the page holds only these two.
        order = sorted(created, key=lambda n: _row_scope_index(b, n))
        on_screen_first, on_screen_second = order
        second_visual = _row_scope_index(b, on_screen_second)
        flat = sum(
            1
            for j in range(second_visual)
            if d.count("folder-conflict-policy-select", scope=f"folder-row[{j}]") > 0
        )
        assert flat >= 1, (
            f"round {round_no}: {on_screen_first!r} sorts above {on_screen_second!r} yet "
            f"no picker-bearing row precedes it — the scope index space and the rows "
            f"disagree; {d.diagnose('folder-row')}"
        )
        n = d.count("folder-conflict-policy-select")

        before_first = _nest_policy(nest_url, test_user, on_screen_first)
        before_second = _nest_policy(nest_url, test_user, on_screen_second)
        target = _a_different_policy(before_second)
        pre = [d.get_text("folder-conflict-policy-select", index=j) for j in range(n)]

        # THE PROBE: a flat index, never a scope. Under the reversed-registration
        # defect this lands on a different row (the first probe row, for a two-row
        # page) — the mis-addressed pick the old wizard probe caught.
        d.select("folder-conflict-policy-select", target, index=flat)
        time.sleep(1)  # sleep-ok: the shared helper's own post-select settle, mirrored
        post = [d.get_text("folder-conflict-policy-select", index=j) for j in range(n)]

        got_second = _nest_policy(nest_url, test_user, on_screen_second)
        got_first = _nest_policy(nest_url, test_user, on_screen_first)
        assert (got_second, got_first) == (target, before_first), (
            f"round {round_no}: wrote {target!r} through FLAT index {flat} — the row that "
            f"sorts SECOND on screen ({on_screen_second!r}) must carry it and the first "
            f"({on_screen_first!r}) must be untouched; nest says second={got_second!r} "
            f"(was {before_second!r}), first={got_first!r} (was {before_first!r}).\n"
            f"  picker value-by-flat-index BEFORE: {pre}\n  AFTER: {post}\n"
            f"  (first changed + second not = the registry resolved flat indices in raw "
            f"registration order again, the reversed-Form-rows defect; neither changed = "
            f"the write path broke instead.) Round 2 (only) failing = the remount left "
            f"stale slots that shadowed the re-registered rows."
        )

    # Teardown: drop the two probe sets off the SESSION-scoped nest so later
    # modules' "count grows on create" polls don't push their new rows below a
    # lazy list's realization fold (a snapshot-less sync set deletes cleanly —
    # cleanup is fixture work, so the API shortcut is legitimate; e2e rule 8b).
    client = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        for name in created:
            client.call("fauna.folders.delete", {"name": name})
    # Remount the page so the deletion lands in the registry before the next test.
    b.navigate_devices()
    b.navigate_folders()
