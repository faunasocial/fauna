import Testing
import Foundation
@testable import FaunaKit

// Regression tests for the in-process `AutomationRegistry` token-precise
// `unregister`.
//
// The registry maps `id -> [Entry]` and the in-process driver addresses repeated
// ids (list rows) by their occurrence index. Each on-screen row registers on
// `.onAppear` and unregisters on `.onDisappear`. Removing a NON-tail row (e.g.
// `delete_filter(0)` of 2 email filters) used to corrupt the list: the old LIFO
// `unregister` popped the *last* slot, not the disappearing row's, leaving a
// stale slot at index 0 and shifting every survivor. That is the
// `test_email_filter_crud[macos]` "second delete is a no-op" bug — `filter_count()`
// stuck at 1 because the 2nd `delete_filter(0)` re-fired the already-removed
// row's closure against a dead filter id. Token-keyed removal fixes it for every
// indexed list on both apple apps (shared FaunaKit; macOS + iOS in-process).

@MainActor
private func valueEntry(_ value: String) -> AutomationRegistry.Entry {
    AutomationRegistry.Entry(value: { value })
}

/// On-screen sentinel geometry at `frame` inside a large window — the shape the
/// document-order tests position slots with.
private func geo(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat = 100, _ h: CGFloat = 20) -> (() -> SentinelGeometry?) {
    { SentinelGeometry(frame: CGRect(x: x, y: y, width: w, height: h),
                       windowBounds: CGRect(x: 0, y: 0, width: 1000, height: 1000)) }
}

/// A slot PARKED off-window on the x-axis — the exact shape of the UIKit
/// nav-transition zombie: a dead pooled cell re-attached at the parallax parking
/// offset (observed x≈−74 for a 340pt card). Wide enough to still OVERLAP the
/// window, so only the x-ORIGIN test (not an overlap test) rules it absent.
private func parkedGeo(_ x: CGFloat = -74) -> (() -> SentinelGeometry?) {
    { SentinelGeometry(frame: CGRect(x: x, y: 100, width: 340, height: 40),
                       windowBounds: CGRect(x: 0, y: 0, width: 400, height: 900)) }
}

@Test @MainActor func unregisterRemovesTheNamedSlotNotTheLastOne() {
    let reg = AutomationRegistry.shared
    // Unique id so this can't collide with a parallel test on the shared singleton.
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID(), t2 = UUID()
    reg.register(id, token: t0, valueEntry("r0"))
    reg.register(id, token: t1, valueEntry("r1"))
    reg.register(id, token: t2, valueEntry("r2"))
    #expect(reg.count(id) == 3)

    // The HEAD row (index 0) disappears — the exact `delete_filter(0)` shape.
    reg.unregister(id, token: t0)

    // Survivors must be r1, r2 IN ORDER. Under the old LIFO bug the last slot
    // (r2) was popped, leaving a stale r0 at index 0 — what made the 2nd delete
    // re-target the already-gone row.
    #expect(reg.count(id) == 2)
    #expect(reg.entry(id, index: 0)?.value?() == "r1")
    #expect(reg.entry(id, index: 1)?.value?() == "r2")

    // Drain so the shared singleton doesn't leak into other tests.
    reg.unregister(id, token: t1)
    reg.unregister(id, token: t2)
    #expect(reg.count(id) == 0)
}

@Test @MainActor func unregisterIsTolerantOfUnknownAndDoubleRemoval() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, valueEntry("only"))
    // A token never registered under this id is a no-op (not a crash / mis-pop).
    reg.unregister(id, token: UUID())
    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
    #expect(reg.count(id) == 0)
    // Double-unregister of the same token is tolerated (LIFO version guarded this
    // with `!list.isEmpty`; the token version guards with the firstIndex lookup).
    reg.unregister(id, token: t0)
    #expect(reg.count(id) == 0)
}

// MARK: - `refresh` — the re-render half of the lifecycle
//
// `register` runs once per view IDENTITY (`.onAppear`), but a SwiftUI view is a
// VALUE: every body re-evaluation builds a fresh `Entry` whose closures capture
// that pass's values. A view that captures its model struct BY VALUE
// (`let folder: FolderSummary`, read as `folder.webdavEnabled`) therefore had
// its FIRST render's closures frozen in the registry forever — the driver read a
// stale value while the on-screen SwiftUI rendering stayed fully live (SwiftUI
// re-derives the real `Binding` at interaction time). That is the folder
// serve-OFF "silent no-op": the toggle's `apply(!isOn)` kept computing
// `apply(true)` because the registered `isOn` never updated. Views capturing a
// REFERENCE type (`vm.snapshot?.webdavEnabled`) happened to dodge it — a stale
// closure still reads through the live reference — which is why this survived so
// long. `_AutomationRegister` now calls `refresh` on every body pass.

/// The exact bug shape: an `Entry` whose closure captures a struct model BY VALUE,
/// as `FolderWebdavToggle` / `FolderConflictPolicyPicker` do.
private struct RowModel { var servedOn: Bool }

@MainActor
private func rowEntry(_ model: RowModel) -> AutomationRegistry.Entry {
    // Captures `model` — a frozen COPY, exactly like the real value-type views.
    AutomationRegistry.Entry(value: { model.servedOn ? "on" : "off" })
}

@Test @MainActor func refreshSwapsInTheCurrentRendersClosures() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID()

    // Two rows appear; row 1's model starts un-served.
    reg.register(id, token: t0, rowEntry(RowModel(servedOn: false)))
    reg.register(id, token: t1, rowEntry(RowModel(servedOn: false)))
    #expect(reg.entry(id, index: 1)?.value?() == "off")

    // The nest write lands and row 1 RE-RENDERS with a fresh model value. Before
    // `refresh` existed this was invisible to the registry (still "off") — the
    // driver then computed `apply(!isOn)` = `apply(true)`, an idempotent no-op, so
    // the serve-OFF click silently did nothing.
    reg.refresh(id, token: t1, rowEntry(RowModel(servedOn: true)))
    #expect(reg.entry(id, index: 1)?.value?() == "on")

    // Order and count are untouched — `refresh` replaces IN PLACE by token, so the
    // flat occurrence-index heuristic and token-precise `unregister` keep their
    // exact semantics. Row 0 is unaffected by row 1's re-render.
    #expect(reg.count(id) == 2)
    #expect(reg.entry(id, index: 0)?.value?() == "off")

    reg.unregister(id, token: t0)
    reg.unregister(id, token: t1)
    #expect(reg.count(id) == 0)
}

@Test @MainActor func refreshOfAnUnknownTokenIsANoOpAndNeverResurrectsASlot() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()

    // Before either show signal has registered (the first body pass) there is no
    // slot yet: a no-op, NOT an append — `register` places it moments later, in
    // document order.
    reg.refresh(id, token: t0, rowEntry(RowModel(servedOn: true)))
    #expect(reg.count(id) == 0)

    reg.register(id, token: t0, rowEntry(RowModel(servedOn: false)))
    #expect(reg.count(id) == 1)

    // A body pass can also land AFTER identity death removed the slot. `refresh`
    // must not resurrect it (which a blind upsert would, re-appending it out of
    // document order and corrupting every survivor's occurrence index).
    reg.unregister(id, token: t0)
    reg.refresh(id, token: t0, rowEntry(RowModel(servedOn: true)))
    #expect(reg.count(id) == 0)
}

// MARK: - The kept-slot hide/show lifecycle (iOS Gap B)
//
// A `NavigationStack` detail push fires the root's `.onDisappear`, but a pop
// never fires a matching `.onAppear` — the root was never structurally removed,
// so it never "re-appears". Under the old remove-on-disappear lifecycle that
// permanently deregistered every root-level element after ONE detail push
// (`calendar-date-label`, iOS Gap B: count == 0 on a visibly rendered label,
// unrecoverable by any navigation). The registry now HIDES on off-screen (slot
// kept, in place) and the window-attachment sentinel's re-attach `register`s
// again — an upsert that un-hides IN PLACE, restoring document order without
// the blind re-append that would shift every survivor's occurrence index.

@Test @MainActor func hideThenRegisterRestoresTheSlotInDocumentOrder() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID(), t2 = UUID()
    reg.register(id, token: t0, valueEntry("r0"))
    reg.register(id, token: t1, valueEntry("r1"))
    reg.register(id, token: t2, valueEntry("r2"))

    // The MIDDLE row goes off screen: hidden slots are invisible to every
    // lookup — indistinguishable over the wire from an absent element — and the
    // survivors' occurrence indices close ranks exactly as removal used to.
    reg.hide(id, token: t1)
    #expect(reg.count(id) == 2)
    #expect(reg.entry(id, index: 0)?.value?() == "r0")
    #expect(reg.entry(id, index: 1)?.value?() == "r2")

    // The element comes back (pop re-attached the root). THE GAP-B PROPERTY:
    // it returns at its ORIGINAL position — never re-appended to the tail,
    // which would shift r2's index (the corruption the old lifecycle's
    // refuse-to-resurrect stance existed to prevent).
    reg.register(id, token: t1, valueEntry("r1-back"))
    #expect(reg.count(id) == 3)
    #expect(reg.entry(id, index: 0)?.value?() == "r0")
    #expect(reg.entry(id, index: 1)?.value?() == "r1-back")
    #expect(reg.entry(id, index: 2)?.value?() == "r2")

    for t in [t0, t1, t2] { reg.unregister(id, token: t) }
    #expect(reg.count(id) == 0)
}

@Test @MainActor func hidingRequiresBothOffScreenSignals() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, valueEntry("row"))

    // `.onDisappear` alone = a covered NavigationStack root (still attached):
    // the element must stay visible — pop restores it with no balancing signal.
    reg.hide(id, token: t0, signal: .disappear)
    #expect(reg.count(id) == 1)

    // A show signal resets the vote…
    reg.register(id, token: t0, valueEntry("row"))
    // …so a later lone window-detach = a macOS table row scrolled out of an
    // eagerly realized List: also stays visible (the driver contract counts it).
    reg.hide(id, token: t0, signal: .windowDetach)
    #expect(reg.count(id) == 1)

    // Both signals since the last show — a TabView switch, an iOS lazy-List
    // scroll-out, an iOS NavigationStack push over the root — now hide.
    reg.hide(id, token: t0, signal: .disappear)
    #expect(reg.count(id) == 0)

    // The show signal (window re-attach / .onAppear) restores it.
    reg.register(id, token: t0, valueEntry("row"))
    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
}

@Test @MainActor func hideIsTolerantOfUnknownTokensAndDoubleHide() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, valueEntry("only"))
    reg.hide(id, token: UUID()) // unknown → no-op
    #expect(reg.count(id) == 1)
    reg.hide(id, token: t0)
    reg.hide(id, token: t0) // double-hide (detach + `.onDisappear` echo) → no-op
    #expect(reg.count(id) == 0)
    reg.unregister(id, token: t0)
}

@Test @MainActor func refreshUpdatesAHiddenSlotWithoutUnhidingIt() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, rowEntry(RowModel(servedOn: false)))
    reg.hide(id, token: t0)

    // A body pass of a hidden view (a background tab re-rendering on a data
    // push) must keep the closures fresh WITHOUT resurrecting visibility —
    // being rendered by SwiftUI is not the same as being on screen.
    reg.refresh(id, token: t0, rowEntry(RowModel(servedOn: true)))
    #expect(reg.count(id) == 0)

    // When the element really comes back, the read is the fresh one.
    reg.register(id, token: t0, rowEntry(RowModel(servedOn: true)))
    #expect(reg.entry(id, index: 0)?.value?() == "on")
    reg.unregister(id, token: t0)
}

@Test @MainActor func registerIsAnUpsertNeverADuplicateAppend() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()

    // Both show signals (sentinel attach + `.onAppear`) fire for one appearance;
    // the second must not create a second slot.
    reg.register(id, token: t0, valueEntry("attach"))
    reg.register(id, token: t0, valueEntry("appear"))
    #expect(reg.count(id) == 1)
    #expect(reg.entry(id, index: 0)?.value?() == "appear") // latest payload wins
    reg.unregister(id, token: t0)
}

@Test @MainActor func lookupsResolveInOnScreenDocumentOrderNotRegistrationOrder() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID(), t2 = UUID()

    // Register BOTTOM-UP — exactly what a macOS `Form`'s rows do (the wizard
    // frequency-pick defect: flat index 1 of 7 options resolved to the 6th).
    // Each slot carries its sentinel geometry; lookups must sort by it.
    reg.register(id, token: t2, geometry: geo(0, 40), valueEntry("row2"))
    reg.register(id, token: t1, geometry: geo(0, 20), valueEntry("row1"))
    reg.register(id, token: t0, geometry: geo(0, 0), valueEntry("row0"))

    #expect(reg.entry(id, index: 0)?.value?() == "row0")
    #expect(reg.entry(id, index: 1)?.value?() == "row1")
    #expect(reg.entry(id, index: 2)?.value?() == "row2")
    // Same-row (equal y) resolves left-to-right — grid/row-major order.
    let t3 = UUID()
    reg.register(id, token: t3, geometry: geo(200, 0), valueEntry("row0-right"))
    #expect(reg.entry(id, index: 1)?.value?() == "row0-right")

    for t in [t0, t1, t2, t3] { reg.unregister(id, token: t) }
}

@Test @MainActor func lookupsKeepInsertionOrderWhenAnySlotLacksGeometry() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID()

    // One slot has no sentinel frame (nil closure / nil frame): the sort must
    // fall back to insertion order wholesale — never a half-sorted mix.
    reg.register(id, token: t0, valueEntry("first"))
    reg.register(id, token: t1, geometry: geo(0, 0, 10, 10), valueEntry("second"))
    #expect(reg.entry(id, index: 0)?.value?() == "first")
    #expect(reg.entry(id, index: 1)?.value?() == "second")

    reg.unregister(id, token: t0)
    reg.unregister(id, token: t1)
}

// MARK: - Geometry-FIRST effective visibility (the zombie fix)
//
// An ATTACHED slot (sentinel realized → geometry() non-nil) is decided by its
// live frame ALONE; votes are consulted only for a geometry-less (detached / no
// sentinel) slot. This replaces the v3 both-votes-gated guard, whose
// short-circuit `if !(sawDisappear && sawDetach) { return true }` let a zombie
// that churn gave only ONE off-screen vote read present despite its parked
// geometry (`test_event_create_and_delete[ios]`: 'Meeting A' lingering after a
// successful delete). Geometry-FIRST removes the vote race entirely.

@Test @MainActor func attachedSlotParkedOffWindowIsAbsentEvenWithOneVote() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // A dead pooled cell re-attached at the nav parking offset, delivered only
    // ONE of the two off-screen votes by the transition churn — the CAD zombie.
    // v3 read it present (`!(false && true)` == true, short-circuiting before
    // geometry); geometry-FIRST rules it absent on its parked x-origin alone.
    reg.register(id, token: t0, geometry: parkedGeo(), valueEntry("zombie"))
    reg.hide(id, token: t0, signal: .windowDetach)
    #expect(reg.count(id) == 0)
    reg.unregister(id, token: t0)
    #expect(reg.count(id) == 0)
}

@Test @MainActor func attachedSlotParkedOffWindowIsAbsentWithNoVotesAtAll() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // Transition debris can re-attach with NO disappear vote at all. An attached
    // sentinel parked off-window on x is absent regardless — the geometry is the
    // whole truth. (A horizontal carousel item scrolled off the right edge is the
    // same shape: absent until scrolled back, which the driver contract wants.)
    reg.register(id, token: t0, geometry: parkedGeo(), valueEntry("debris"))
    #expect(reg.count(id) == 0)
    reg.unregister(id, token: t0)
}

/// The attached-but-ORDERED-OUT shape: a dismissed sheet whose window is no
/// longer on screen while SwiftUI keeps its content attached to it. Measured on
/// macOS under the e2e harness (an inactive app): after the room settings
/// sheet's close, its `SheetPresentationWindow` read `isVisible == false` with
/// the parent's `sheets` empty — yet no `.onDisappear` fired and the sentinel
/// still resolved that window, so geometry-FIRST read the Save button present
/// for as long as anyone looked. In-window coordinates, so only the window's
/// own visibility can rule it absent.
private func orderedOutGeo() -> (() -> SentinelGeometry?) {
    { SentinelGeometry(frame: CGRect(x: 396, y: 198, width: 54, height: 24),
                       windowBounds: CGRect(x: 0, y: 0, width: 470, height: 242),
                       windowVisible: false) }
}

@Test @MainActor func attachedSlotInAnOrderedOutWindowIsAbsentWithNoVotesAtAll() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // No vote arrives (SwiftUI never tore the content down), and the frame is
    // squarely inside its window — the window itself is what is gone.
    reg.register(id, token: t0, geometry: orderedOutGeo(), valueEntry("dismissed sheet"))
    #expect(reg.count(id) == 0)
    #expect(reg.debugDump().contains("[0] HIDDEN(window-hidden)"))
    reg.unregister(id, token: t0)
}

@Test @MainActor func attachedSlotSettledOnScreenIsPresentDespiteBothVotes() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // A NavigationStack root uncovered by a pop: both votes stood from the push,
    // and the re-attach fires no vote-clearing `register` — but its settled
    // on-screen geometry revives it (the Gap-B restore needs no extra signal).
    reg.register(id, token: t0, geometry: geo(0, 100), valueEntry("root"))
    reg.hide(id, token: t0, signal: .disappear)
    reg.hide(id, token: t0, signal: .windowDetach)
    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
}

@Test @MainActor func attachedSlotBelowFoldVerticallyStaysPresent() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // Rule 6 eager `ScrollView { VStack }`: content attached BELOW the window's
    // height (large y) but at normal x must stay present — the in-process driver
    // drives those closures without scrolling. `isOnScreen` deliberately tests
    // the x-axis only, so vertical overflow never parks a slot.
    reg.register(
        id, token: t0,
        geometry: {
            SentinelGeometry(frame: CGRect(x: 0, y: 5000, width: 100, height: 20),
                             windowBounds: CGRect(x: 0, y: 0, width: 400, height: 900))
        },
        valueEntry("below-fold"))
    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
}

@Test @MainActor func geometrylessSlotStillDecidedByVotes() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    // No sentinel geometry (detached, or a context without one — macOS's
    // eagerly-realized below-the-fold rows, an iOS lazy-`List` scroll-out): the
    // vote-based answer is unchanged from v3. One vote keeps it present; both
    // hide it.
    reg.register(id, token: t0, valueEntry("no-geo"))
    reg.hide(id, token: t0, signal: .windowDetach)
    #expect(reg.count(id) == 1)
    reg.hide(id, token: t0, signal: .disappear)
    #expect(reg.count(id) == 0)
    reg.unregister(id, token: t0)
}

@Test @MainActor func hiddenSlotsAreInvisibleToScopedQueries() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID()
    let scope = [AutomationScopeStep(id: "card", index: 0)]
    reg.register(id, token: t0, path: scope, valueEntry("scoped"))
    reg.register(id, token: t1, path: [AutomationScopeStep(id: "card", index: 1)], valueEntry("other"))
    #expect(reg.hasScopePath(id))
    #expect(reg.resolvedCount(id, scope: scope) == 1)

    reg.hide(id, token: t0)
    #expect(reg.resolvedCount(id, scope: scope) == 0)
    #expect(reg.scopedEntries(id, scope: scope).isEmpty)

    reg.hide(id, token: t1)
    // No visible slot carries a path → the id no longer participates in
    // subtree scoping at all (and is absent from the debug dump).
    #expect(!reg.hasScopePath(id))
    #expect(!reg.allIds().contains(id))

    reg.unregister(id, token: t0)
    reg.unregister(id, token: t1)
}

// MARK: - Scoped-singleton resolution (the admin-dns cert-delegate class)
//
// A child that exists on only ONE row (an inline `@State` reveal — the
// cert-delegate submit/cancel form) but is addressed with a row scope
// (`scope="admin-dns-domain[N]"`) is unresolvable under the legacy flat
// occurrence-index heuristic: the heuristic maps scope index N to occurrence
// index N of the child id, and a 1-element id has no occurrence N≥1 — so
// `is_visible` reads False and click 404s FOREVER (the
// `test_admin_dns_cert_delegate_and_remove` / `_auto_renew` reds, macos+ios,
// deterministic, misfiled for a session as an appear-geometry bug). Real
// subtree paths (goal-doc rule 5: a container driven with `scope=` pushes
// `.automationScope`) resolve it by containment instead.

@Test @MainActor func scopedSingletonResolvesByPathNotFlatIndex() {
    let reg = AutomationRegistry.shared
    let row = "test-dns-row-\(UUID().uuidString)"
    let submit = "test-dns-submit-\(UUID().uuidString)"
    let r0 = UUID(), r1 = UUID(), s = UUID()
    // Two rows self-register (per-row presence entries), each carrying its own
    // scope step — the shape `.automationScope(row, index:)` produces.
    reg.register(row, token: r0, path: [AutomationScopeStep(id: row, index: 0)], valueEntry("row0"))
    reg.register(row, token: r1, path: [AutomationScopeStep(id: row, index: 1)], valueEntry("row1"))
    // The revealed submit exists ONLY on row 1 — a scoped singleton.
    reg.register(submit, token: s, path: [AutomationScopeStep(id: row, index: 1)], valueEntry("submit"))

    let scope0 = [AutomationScopeStep(id: row, index: 0)]
    let scope1 = [AutomationScopeStep(id: row, index: 1)]
    #expect(reg.resolvedVisible(submit, scope: scope1, leafIndex: 0))
    #expect(reg.resolvedEntry(submit, scope: scope1, leafIndex: 0)?.value?() == "submit")
    // Row 0 — where nothing is revealed — correctly reads absent.
    #expect(!reg.resolvedVisible(submit, scope: scope0, leafIndex: 0))
    #expect(reg.resolvedCount(submit, scope: scope0) == 0)

    reg.unregister(row, token: r0)
    reg.unregister(row, token: r1)
    reg.unregister(submit, token: s)
}

@Test @MainActor func scopedSingletonWithoutPathFallsToFlatHeuristicAndMisses() {
    // The PRE-fix shape, locked in as documentation of the failure class: with
    // no `.automationScope` on the rows the singleton registers with an empty
    // path, `hasScopePath` is false, and the scoped query falls back to the
    // flat heuristic — scope row[1] demands occurrence index 1 of a 1-element
    // id (absent), while row[0] coincidentally resolves. This is why a page
    // whose per-row children all exist on EVERY row can pass for months and
    // then break the day a row-scoped SINGLETON (a reveal form) is added.
    let reg = AutomationRegistry.shared
    let row = "test-dns-row-\(UUID().uuidString)"
    let submit = "test-dns-submit-\(UUID().uuidString)"
    let s = UUID()
    reg.register(submit, token: s, valueEntry("submit"))

    #expect(!reg.resolvedVisible(submit, scope: [AutomationScopeStep(id: row, index: 1)], leafIndex: 0))
    #expect(reg.resolvedVisible(submit, scope: [AutomationScopeStep(id: row, index: 0)], leafIndex: 0))

    reg.unregister(submit, token: s)
}

// The roster row is this class too (`contacts.md` § The private overlay):
// `contact-public-name` and `contact-labels` exist only on a row whose person
// carries a nickname / a label, and the driver reads them with
// `scope="contact-row[i]"`. Both targets' rows therefore declare
// `.automationScope(Ids.contactRow, index:)` — outermost, so the row's own
// presence anchor self-scopes — with an index that is FLAT across the status
// groups the rows are nested in (`ContactsVM.rosterGroups`, pinned in
// `ContactOverlayTests`). This pins the shape that declaration produces.
@Test @MainActor func contactRowSecondaryLinesResolveByRowScopeAcrossStatusGroups() {
    let reg = AutomationRegistry.shared
    let row = Ids.contactRow
    func path(_ i: Int) -> [AutomationScopeStep] { [AutomationScopeStep(id: row, index: i)] }

    // Three rows — two in the first status group, one in the second — indexed
    // 0, 1, 2. Every row paints its name and its presence anchor.
    let anchors = (0..<3).map { _ in UUID() }
    let names = (0..<3).map { _ in UUID() }
    for i in 0..<3 {
        reg.register(row, token: anchors[i], path: path(i), valueEntry("row\(i)"))
        reg.register(Ids.contactName, token: names[i], path: path(i), valueEntry("name\(i)"))
    }
    // Only the LAST row's person — the second group's first row — carries a
    // nickname and a label.
    let publicName = UUID(), labels = UUID()
    reg.register(Ids.contactPublicName, token: publicName, path: path(2), valueEntry("alice"))
    reg.register(Ids.contactLabels, token: labels, path: path(2), valueEntry("Family"))

    #expect(reg.resolvedEntry(Ids.contactPublicName, scope: path(2), leafIndex: 0)?.value?() == "alice")
    #expect(reg.resolvedEntry(Ids.contactLabels, scope: path(2), leafIndex: 0)?.value?() == "Family")
    // A row without an overlay reads both lines absent — never the other
    // row's, which is what the flat occurrence-index heuristic would answer
    // for row 0.
    for i in 0..<2 {
        #expect(!reg.resolvedVisible(Ids.contactPublicName, scope: path(i), leafIndex: 0))
        #expect(reg.resolvedCount(Ids.contactLabels, scope: path(i)) == 0)
    }
    // The row's own anchor resolves under its own scope step.
    #expect(reg.resolvedEntry(row, scope: path(1), leafIndex: 0)?.value?() == "row1")

    for i in 0..<3 {
        reg.unregister(row, token: anchors[i])
        reg.unregister(Ids.contactName, token: names[i])
    }
    reg.unregister(Ids.contactPublicName, token: publicName)
    reg.unregister(Ids.contactLabels, token: labels)
}

// MARK: - Descendant-matching scope resolution (apple was the last
// root-anchored scope matcher)
//
// Before 2026-08-24 `scopedEntries`/`resolvedScrollIntoView` filtered
// `path.starts(with: scope)` — a ROOT-ANCHORED prefix, so a scope naming only
// an inner container (bare `scope="quoted-post"`, no `post-card` prefix)
// matched NOTHING, even though the element is on screen. That was already
// LIVE, not merely latent: `QuotedPostCard`'s own
// `.automationScope(Ids.quotedPost)` nests inside `post-card`'s scope since
// 2026-06-28 — no red test caught it only because every existing scoped query
// already spelled the full chain. `scopeRoot` fixes it via descendant
// matching (e2e-conventions.md § convention 1), mirroring tui's
// `Registry::scope_root`.

@Test @MainActor func bareInnerScopeResolvesByDescendantMatchingNotRootAnchoredPrefix() {
    let reg = AutomationRegistry.shared
    let card = "test-post-card-\(UUID().uuidString)"
    let quoted = "test-quoted-post-\(UUID().uuidString)"
    let badge = "test-unverified-badge-\(UUID().uuidString)"

    // Two cards, both self-scoping (their own registration ends with itself —
    // the shape `.automationScope(id, index:)` applied as the OUTERMOST
    // modifier over the container's own `.automationActivate` produces).
    let c0 = UUID(), c1 = UUID()
    reg.register(card, token: c0, path: [AutomationScopeStep(id: card, index: 0)], valueEntry("card0"))
    reg.register(card, token: c1, path: [AutomationScopeStep(id: card, index: 1)], valueEntry("card1"))

    // Only card 1 quotes a post. `QuotedPostCard`'s own shape: it self-scopes
    // (path ends with itself) AND a leaf inside it (the badge) carries the
    // same full path.
    let quotedPath = [AutomationScopeStep(id: card, index: 1), AutomationScopeStep(id: quoted, index: 0)]
    let q1 = UUID(), b1 = UUID()
    reg.register(quoted, token: q1, path: quotedPath, valueEntry("quoted"))
    reg.register(badge, token: b1, path: quotedPath, valueEntry("unverified"))

    // The regression itself: a BARE scope naming only the inner container
    // (no `post-card` prefix) must still resolve — this is exactly
    // `scope="quoted-post"`, never spelled by any existing python test today,
    // which is why the root-anchored bug shipped invisibly.
    let bareQuoted = [AutomationScopeStep(id: quoted, index: 0)]
    #expect(reg.resolvedVisible(badge, scope: bareQuoted, leafIndex: 0))
    #expect(reg.resolvedEntry(badge, scope: bareQuoted, leafIndex: 0)?.value?() == "unverified")
    #expect(reg.scopedEntries(badge, scope: bareQuoted).count == 1)

    // The full two-element chain — what every existing python test spells —
    // must still resolve identically (descendant matching is a superset of
    // root-anchored prefix matching, so this must not regress).
    let fullChain = [AutomationScopeStep(id: card, index: 1), AutomationScopeStep(id: quoted, index: 0)]
    #expect(reg.resolvedVisible(badge, scope: fullChain, leafIndex: 0))

    // Card 0 quoted nothing — a scope naming card 0's (absent) quoted-post
    // correctly resolves to nothing, never card 1's by accident.
    let card0Quoted = [AutomationScopeStep(id: card, index: 0), AutomationScopeStep(id: quoted, index: 0)]
    #expect(!reg.resolvedVisible(badge, scope: card0Quoted, leafIndex: 0))
    #expect(reg.scopedEntries(badge, scope: card0Quoted).isEmpty)

    reg.unregister(card, token: c0)
    reg.unregister(card, token: c1)
    reg.unregister(quoted, token: q1)
    reg.unregister(badge, token: b1)
}

@Test @MainActor func hidingAContainersOwnWitnessNeverResolvesItsQueryToASiblingsIndex() {
    // The regression a first draft of this fix actually shipped, caught by
    // the pre-existing `hiddenSlotsAreInvisibleToScopedQueries`: an
    // occurrence-COUNTING walk (tui's shape) renumbers whoever survives when a
    // container's own self-registration is hidden — with card 0's
    // `post-card[0]` hidden and card 1's `post-card[1]` still visible, a
    // COUNTING walk sees exactly one visible "post-card" witness and assigns
    // IT occurrence 0, so `scope="post-card[0]"` would resolve to card 1's
    // subtree instead of correctly finding nothing. Verbatim matching (this
    // fix's actual shape) cannot do that: `post-card[0]` and `post-card[1]`
    // are different VALUES, so hiding one's witness can never make a query for
    // it match the other's.
    let reg = AutomationRegistry.shared
    let card = "test-post-card-\(UUID().uuidString)"
    let leaf = "test-post-author-\(UUID().uuidString)"

    let c0 = UUID(), c1 = UUID(), l0 = UUID(), l1 = UUID()
    reg.register(card, token: c0, path: [AutomationScopeStep(id: card, index: 0)], valueEntry("card0"))
    reg.register(card, token: c1, path: [AutomationScopeStep(id: card, index: 1)], valueEntry("card1"))
    reg.register(leaf, token: l0, path: [AutomationScopeStep(id: card, index: 0)], valueEntry("author0"))
    reg.register(leaf, token: l1, path: [AutomationScopeStep(id: card, index: 1)], valueEntry("author1"))

    let scope0 = [AutomationScopeStep(id: card, index: 0)]
    #expect(reg.resolvedEntry(leaf, scope: scope0, leafIndex: 0)?.value?() == "author0")

    // Hide card 0's OWN self-registration (not the leaf) — card 1 stays fully
    // visible, self-registration included.
    reg.hide(card, token: c0)
    #expect(!reg.resolvedVisible(leaf, scope: scope0, leafIndex: 0))
    #expect(reg.scopedEntries(leaf, scope: scope0).isEmpty)
    #expect(reg.resolvedEntry(leaf, scope: scope0, leafIndex: 0) == nil)
    // Card 1's own scope is untouched by card 0's hide.
    let scope1 = [AutomationScopeStep(id: card, index: 1)]
    #expect(reg.resolvedEntry(leaf, scope: scope1, leafIndex: 0)?.value?() == "author1")

    reg.unregister(card, token: c0)
    reg.unregister(card, token: c1)
    reg.unregister(leaf, token: l0)
    reg.unregister(leaf, token: l1)
}

@Test @MainActor func anUnresolvableScopeStepMatchesNothingRatherThanCrashingOrFallingBack() {
    // A scope step naming a container index the app never painted (e.g.
    // `post-card[9]` when only 2 cards exist) must resolve to an empty match
    // set — `scopeRoot` returns nil, and every caller treats that as absent,
    // never as "fall back to the legacy flat heuristic" (that fallback is
    // gated on `hasScopePath`, which is unrelated to whether THIS particular
    // scope resolves).
    let reg = AutomationRegistry.shared
    let card = "test-post-card-\(UUID().uuidString)"
    let leaf = "test-leaf-\(UUID().uuidString)"
    let c0 = UUID(), l0 = UUID()
    reg.register(card, token: c0, path: [AutomationScopeStep(id: card, index: 0)], valueEntry("card0"))
    reg.register(leaf, token: l0, path: [AutomationScopeStep(id: card, index: 0)], valueEntry("leaf"))

    let outOfRange = [AutomationScopeStep(id: card, index: 9)]
    #expect(!reg.resolvedVisible(leaf, scope: outOfRange, leafIndex: 0))
    #expect(reg.scopedEntries(leaf, scope: outOfRange).isEmpty)
    #expect(reg.resolvedCount(leaf, scope: outOfRange) == 0)
    #expect(reg.resolvedEntry(leaf, scope: outOfRange, leafIndex: 0) == nil)

    reg.unregister(card, token: c0)
    reg.unregister(leaf, token: l0)
}

// MARK: - `debugDump` (the enriched `/tree` diagnostic)

@Test @MainActor func debugDumpShowsSlotStateGeometryVotesAndPath() {
    let reg = AutomationRegistry.shared
    let id = "test-dump-\(UUID().uuidString)"
    let t0 = UUID(), t1 = UUID(), t2 = UUID()
    reg.register(id, token: t0, path: [AutomationScopeStep(id: "row", index: 2)],
                 geometry: geo(10, 20), valueEntry("a"))
    reg.register(id, token: t1, geometry: parkedGeo(), valueEntry("b"))
    reg.register(id, token: t2, valueEntry("c"))
    reg.hide(id, token: t2, signal: .disappear)

    let dump = reg.debugDump()
    #expect(dump.contains("\(id) (2/3 visible)"))
    #expect(dump.contains("[0] VISIBLE(geo)"))
    #expect(dump.contains("path=row[2]"))
    #expect(dump.contains("[1] HIDDEN(geo-parked)"))
    // One vote alone does not hide a geometry-less slot — and the dump says
    // which vote arrived, the datum the old ids-only dump could never carry.
    #expect(dump.contains("[2] VISIBLE(no-geo)"))
    #expect(dump.contains("votes=disappear "))

    reg.unregister(id, token: t0)
    reg.unregister(id, token: t1)
    reg.unregister(id, token: t2)
}

// MARK: - `.onDisappear`'s vote accuracy (the popped-destination zombie)
//
// Rule 2a's conjunction is UNCHANGED — a geometry-less slot still needs BOTH
// votes to hide, which `geometrylessSlotStillDecidedByVotes` above pins. What
// these pin is the accuracy of the votes the VIEW layer emits, which is where a
// `NavigationStack` pop leaked: `.onDisappear` treated "sentinel realized but
// already off its window" as a cover, and the window-detach vote then never came
// (SwiftUI tears a popped `.navigationDestination` destination down without
// always calling `didMoveToWindow(nil)`). The slot stayed VISIBLE(no-geo)
// forever, so the driver resolved occurrence [0] — a DEAD form instance — while
// the live form kept its own state: a test typing weight `1.5` submitted the
// PREVIOUS test's `2000` permille, and a `select` never switched the live branch.

/// Let any `Task { @MainActor in … }` a `deinit` enqueued actually run. A hop
/// enqueued here lands behind those, so awaiting it is a causal barrier — no
/// wall-clock wait, per e2e-conventions.md convention 14.
@MainActor private func drainMainActorHops() async {
    for _ in 0..<8 {
        await Task.yield()
        await Task { @MainActor in }.value
    }
}

@Test @MainActor func onDisappearVotesDefinitivelyUnlessTheSentinelIsStillAttached() {
    // ATTACHED = a cover (a detail pushed over a live NavigationStack root): one
    // vote, so the slot survives and the pop restores it with no `.onAppear`.
    #expect(AutomationRegistry.hideSignal(sentinelAttached: true) == .disappear)
    // DETACHED = a real exit. Waiting on a detach vote that has demonstrably
    // already been missed is exactly what stranded the slot.
    #expect(AutomationRegistry.hideSignal(sentinelAttached: false) == .definitive)
    // NO SENTINEL = the pre-sentinel lifecycle, which trusts `.onDisappear`.
    #expect(AutomationRegistry.hideSignal(sentinelAttached: nil) == .definitive)
}

@Test @MainActor func aDisappearWhileDetachedHidesAGeometrylessSlot() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"

    // The popped destination: no geometry (its sentinel is gone), and the only
    // lifecycle signal that ever arrives is `.onDisappear`.
    let popped = UUID()
    reg.register(id, token: popped, valueEntry("popped"))
    reg.hide(id, token: popped, signal: AutomationRegistry.hideSignal(sentinelAttached: false))
    #expect(reg.count(id) == 0)

    // The covered root — same absent geometry, same lone signal, but its sentinel
    // is still attached, so it MUST survive.
    let covered = UUID()
    reg.register(id, token: covered, valueEntry("covered"))
    reg.hide(id, token: covered, signal: AutomationRegistry.hideSignal(sentinelAttached: true))
    #expect(reg.count(id) == 1)

    reg.unregister(id, token: popped)
    reg.unregister(id, token: covered)
}

@Test @MainActor func aDeallocatedSentinelVotesDetachSoAPoppedSlotCannotLinger() async {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, valueEntry("form"))
    // The ordering the branch above cannot cover: `.onDisappear` arrived while
    // the view was STILL attached (a lone cover vote), and the detach callback
    // never fires because the hosting view is torn down outright.
    reg.hide(id, token: t0, signal: .disappear)
    #expect(reg.count(id) == 1)

    var probe: _AttachmentProbeView? = _AttachmentProbeView()
    probe?.slotId = id
    probe?.slotToken = t0
    // `owner` left nil = no successor probe took the slot over, which is the
    // state a real teardown leaves (the weak `owner.sentinel` is zeroed by then).
    probe = nil
    await drainMainActorHops()

    #expect(reg.count(id) == 0)
    reg.unregister(id, token: t0)
}

@Test @MainActor func aReplacedSentinelDoesNotVoteForTheSlotItsSuccessorOwns() async {
    let reg = AutomationRegistry.shared
    let id = "test-automation-row-\(UUID().uuidString)"
    let t0 = UUID()
    reg.register(id, token: t0, valueEntry("live"))
    reg.hide(id, token: t0, signal: .disappear)

    var outgoing: _AttachmentProbeView? = _AttachmentProbeView()
    outgoing?.slotId = id
    outgoing?.slotToken = t0
    // SwiftUI re-made the representable for a still-live view identity: the
    // incoming probe takes the slot over and marks this one superseded, exactly
    // as `makeProbe` does, so its death is bookkeeping rather than an exit.
    outgoing?.superseded = true
    outgoing = nil
    await drainMainActorHops()

    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
}

// MARK: - Inherited disablement (`AutomationRegistry.folding`)
//
// `Entry.isEnabled` is a closure each call site passes BY HAND, and the server
// reads a `nil` one as enabled (`e.isEnabled?() ?? true`). So a control greyed
// only because an ANCESTOR applied `.disabled(...)` used to report
// `enabled=true` to the driver: the driver's answer and the real UI disagreeing
// in the direction that costs debugging (a test actuates it, nothing happens,
// and the failure presents as a product bug — e2e conventions point 11, one
// layer down from a dropped command). `_AutomationRegister` now reads SwiftUI's
// cumulative `\.isEnabled` and folds it in through the pure function below;
// these pin the fold's four corners plus the "enabled inherits unchanged"
// identity the common case depends on.

@Test @MainActor func inheritedEnabledLeavesAnUndeclaredEntryUntouched() {
    // The overwhelmingly common case: no ancestor disabled anything and the call
    // site declared no predicate. The entry must come back with `isEnabled` still
    // NIL — not a wrapper closure answering true — so `/element/enabled` keeps its
    // exact `e != nil` semantics and `debugDump` keeps its capability letters
    // honest (an `E` would claim a predicate the call site never wrote).
    let folded = AutomationRegistry.folding(
        AutomationRegistry.Entry(activate: {}), inheritedEnabled: true)
    #expect(folded.isEnabled == nil)
}

@Test @MainActor func inheritedEnabledPreservesTheCallSitesOwnPredicate() {
    // Enabled inherits unchanged, so a hand-mirrored predicate still decides —
    // it is the more specific claim and must not be widened to "true".
    var own = false
    let folded = AutomationRegistry.folding(
        AutomationRegistry.Entry(activate: {}, isEnabled: { own }),
        inheritedEnabled: true)
    #expect(folded.isEnabled?() == false)
    own = true
    #expect(folded.isEnabled?() == true)
}

@Test @MainActor func inheritedDisabledOverridesAnAbsentPredicate() {
    // The bug this fold exists for: greyed by an ancestor, declared nothing,
    // previously reported enabled.
    let folded = AutomationRegistry.folding(
        AutomationRegistry.Entry(activate: {}), inheritedEnabled: false)
    #expect(folded.isEnabled?() == false)
}

@Test @MainActor func inheritedDisabledOverridesAnEnabledPredicate() {
    // A descendant cannot argue its way out of an ancestor's `.disabled(...)` —
    // SwiftUI's own rule, so the registry must not answer otherwise. This is the
    // half that keeps the fold from being merely advisory.
    let folded = AutomationRegistry.folding(
        AutomationRegistry.Entry(activate: {}, isEnabled: { true }),
        inheritedEnabled: false)
    #expect(folded.isEnabled?() == false)
}

@Test @MainActor func foldingKeepsEveryOtherCapability() {
    // The fold rewrites exactly one field. A dropped `activate`/`text`/`value`/
    // `setValue` would silently 404 or blank the element rather than report it
    // disabled — a much louder failure than the one being fixed, so pin it.
    let folded = AutomationRegistry.folding(
        AutomationRegistry.Entry(
            activate: {}, doubleActivate: {}, text: { "t" }, value: { "v" },
            setValue: { _ in }, options: { ["a"] }, isEnabled: { true },
            visibleText: { "vis" }),
        inheritedEnabled: false)
    #expect(folded.activate != nil)
    #expect(folded.doubleActivate != nil)
    #expect(folded.text?() == "t")
    #expect(folded.value?() == "v")
    #expect(folded.setValue != nil)
    #expect(folded.options?() == ["a"])
    #expect(folded.visibleText?() == "vis")
    #expect(folded.isEnabled?() == false)
}

// MARK: - Presentation-hosted slots (`.alert` / `.confirmationDialog`)
//
// A SwiftUI presentation's content builder gets NO de-registration signal at
// all. Measured 2026-09-21 on macOS against the real app, dumping the registry
// after answering `folder-residency-confirm`:
//
//     folder-residency-confirm (1/1 visible)
//       [0] VISIBLE(no-geo) geo=nil votes=- path=folder-row[0] caps=A
//
// `votes=-` is ZERO votes — neither `.onDisappear` (SwiftUI does not fire it for
// alert content) nor the `_AttachmentSentinel`'s window-detach, nor the probe's
// `deinit` vote of last resort. `geo=nil` holds for the alert's whole life,
// presented or not, because the sentinel rides in `.background(...)` and an
// alert's content builder never realizes it. `.onAppear` DOES fire, so the slot
// registers on present and then nothing can ever retire it: the confirm reads
// visible for the rest of the process, and the one test that asserts a confirm
// is GONE (`test_folder_residency_control.py::
// test_the_flip_back_to_full_needs_no_confirm`) fails on both apple targets
// while its twin — which only ever asserts the confirm is THERE — passes.
//
// The host knows what the content cannot: its own `isPresented` binding. So the
// presentation declares the lifetime, and `hideAll` is what it declares it with.

@Test @MainActor func hideAllRetiresAPresentationHostedSlotThatCanNeverVoteItselfOut() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-confirm-\(UUID().uuidString)"
    let t0 = UUID()
    // Exactly the measured shape: registered on `.onAppear`, no sentinel ever
    // realized (no geometry closure at all), and not one vote.
    reg.register(id, token: t0, valueEntry("confirm"))
    #expect(reg.count(id) == 1)
    #expect(reg.debugDump().contains("[0] VISIBLE(no-geo)"))

    // Dismissal: the host's binding flipped false. Nothing else ever will.
    reg.hideAll(id)
    #expect(reg.count(id) == 0)

    // Re-presenting restores it — `register` clears the votes, the same
    // hide/show pair every other kept-slot lifecycle uses.
    reg.register(id, token: t0, valueEntry("confirm"))
    #expect(reg.count(id) == 1)
    reg.unregister(id, token: t0)
}

@Test @MainActor func hideAllCannotRetireALiveOnScreenSlotSharingTheId() {
    let reg = AutomationRegistry.shared
    let id = "test-automation-confirm-\(UUID().uuidString)"
    let (t0, t1) = (UUID(), UUID())
    // Geometry-FIRST is what makes `hideAll` safe to hand a bare id: a slot
    // settled on screen is decided by its frame, so a presentation's dismissal
    // can never blank a live element that happens to share the id.
    reg.register(id, token: t0, valueEntry("in-alert"))
    reg.register(id, token: t1, geometry: geo(10, 20), valueEntry("on-screen"))
    #expect(reg.count(id) == 2)

    reg.hideAll(id)
    #expect(reg.count(id) == 1)
    #expect(reg.entry(id, index: 0)?.value?() == "on-screen")
    reg.unregister(id, token: t0)
    reg.unregister(id, token: t1)
}

@Test @MainActor func hideAllIsTolerantOfAnUnknownId() {
    let reg = AutomationRegistry.shared
    reg.hideAll("test-automation-never-registered-\(UUID().uuidString)")
}
