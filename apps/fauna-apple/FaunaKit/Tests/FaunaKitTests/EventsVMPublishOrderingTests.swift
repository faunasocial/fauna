import Testing
import Foundation
@testable import FaunaKit

// `EventsVM` publish ordering — the rule that two concurrent `events` re-queries
// must resolve to the LATEST-STARTED one's result, never an older one's.
//
// The Events page has more than one writer. A foreground action (`createEvent` /
// `selectCalendar` / `loadCalendars`) re-queries, and so do the 10 s poll and the
// `fauna.calendar.changed` push (both via `refreshIfChanged`). They overlap
// routinely and finish out of order, so whichever publishes LAST wins unless the
// view model orders them. When the loser is the newer query, the user sees a list
// that does not match what they just did — the just-created event missing, or a
// narrowed page re-widened to every calendar.
//
// These are the deterministic pins for that ordering. They exist because the only
// prior instrument was a tier_3 e2e run whose race window opens only under machine
// load: `test_event_create_and_delete[macos]` reproduced at ~35% under contention
// (2026-07-30, ) and 0/5 on a quiet machine the next session, which is not a
// verdict either way. The ordering itself is pure mechanism, so it is tested here
// at tier_1 and e2e is left to prove the wiring (testing.md — split the mile).

/// A scripted `EventsAPI` whose `queryEvents` can be held open, so a test can pin
/// an exact interleaving of two in-flight re-queries rather than hope for one.
///
/// `queryEvents` answers from `scripted` in call order; a call whose index is in
/// `gated` suspends until `release(call:)` is invoked for it. Everything else is a
/// minimal success stub — these tests are about ordering, not payload mapping.
@MainActor
private final class ScriptedEventsAPI: EventsAPI {
    /// Per-call results for `queryEvents`, indexed by call order.
    var scripted: [[EventSummary]] = []
    /// Call indices that must suspend until explicitly released.
    var gated: Set<Int> = []
    /// Calendars `listCalendars` answers with (the poll re-reads them first).
    var calendars: [FaunaCalendar] = []

    private(set) var queryCallCount = 0
    /// The `calendarId` each `queryEvents` call received, in call order — lets a
    /// test assert WHICH calendar(s) were actually queried, not just how many
    /// events came back (a mock that answers purely by call order can't
    /// otherwise distinguish "queried the stale selection" from "fell back to
    /// the union" when both happen to make the same number of calls).
    private(set) var queriedCalendarIds: [String] = []
    private var continuations: [Int: CheckedContinuation<Void, Never>] = [:]
    private var pendingRelease: Set<Int> = []

    /// Let a gated call proceed. Safe to call before the call arrives — the
    /// permission is remembered, which removes any need to poll for arrival.
    func release(call index: Int) {
        if let c = continuations.removeValue(forKey: index) {
            c.resume()
        } else {
            pendingRelease.insert(index)
        }
    }

    /// Suspend until this call's index has been released.
    private func waitIfGated(_ index: Int) async {
        guard gated.contains(index), pendingRelease.remove(index) == nil else { return }
        await withCheckedContinuation { continuations[index] = $0 }
    }

    func listCalendars() async throws -> [FaunaCalendar] { calendars }

    func queryEvents(calendarId: String) async throws -> [EventSummary] {
        let index = queryCallCount
        queryCallCount += 1
        queriedCalendarIds.append(calendarId)
        await waitIfGated(index)
        return index < scripted.count ? scripted[index] : []
    }

    /// These ordering tests are about the RACE, not the backstop — delegating
    /// straight to `queryEvents` keeps every call-count/interleaving
    /// assertion here meaning exactly what it did before this seam existed
    /// (never reports "unchanged").
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? {
        try await queryEvents(calendarId: calendarId)
    }

    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }
    func createCalendar(name: String) async throws {}
    func createEvent(_ request: CreateEventRequest) async throws {}
    func deleteEvent(id: String) async throws {}
    func inviteToEvent(eventId: String, email: String) async throws {}
    func rsvpEvent(eventId: String, response: RsvpResponse) async throws {}
    func setReminder(eventId: String, offset: String) async throws {}
    func removeReminder(eventId: String) async throws {}
    func exportCalendar(calendarId: String) async throws -> String { "" }

    func getEvent(id: String) async throws -> EventDetail {
        EventDetail(id: id, uid: id, calendarId: "cal", summary: "",
                    dtstart: "", dtend: "", organizer: "", organizedByMe: true)
    }

    func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult {
        CalendarImportResult(imported: 0, skipped: 0, updated: nil, total: 0, errors: nil)
    }
}

private func event(_ id: String, _ summary: String, calendarId: String = "cal-1") -> EventSummary {
    EventSummary(id: id, uid: id, summary: summary,
                 dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00",
                 calendarId: calendarId)
}

private let calendar1 = FaunaCalendar(id: "cal-1", name: "Cal One")
private let meetingA = event("ev-a", "Meeting A")
private let meetingB = event("ev-b", "Meeting B")

/// Put the VM in the state every test below starts from: one calendar, selected,
/// listing only Meeting A. `selectCalendar` consumes `queryEvents` call 0.
@MainActor
private func vmListingMeetingA(_ api: ScriptedEventsAPI) async -> EventsVM {
    api.calendars = [calendar1]
    api.scripted = [[meetingA]]
    let vm = EventsVM()
    vm.configure(api: api)
    // Mirrors production: a `calendar-item` row can only be tapped from an
    // already-rendered (so already-loaded) `calendars` list — `selectCalendar`
    // is never reachable before `vm.calendars` holds the row it was given.
    // `queryEventItems` now resolves the selection against `vm.calendars`
    // (the stale-selection fallback, events.md § Where logic lives), so
    // skipping this would make every selection here read as "vanished".
    vm.calendars = [calendar1]
    await vm.selectCalendar(calendar1)
    #expect(vm.events.map(\.id) == ["ev-a"])
    return vm
}

@MainActor
@Test("a create's own re-query cannot overwrite a NEWER refresh that already published")
func createDoesNotClobberNewerRefresh() async {
    let api = ScriptedEventsAPI()
    let vm = await vmListingMeetingA(api)

    // The create's own re-query (call 1) starts first but is held open, and will
    // answer with the pre-create list — the read-after-write lag this ordering
    // must tolerate. The push-driven refresh (call 2) starts later and sees both.
    api.scripted = [[meetingA], [meetingA], [meetingA, meetingB]]
    api.gated = [1]

    let create = Task {
        await vm.createEvent(CreateEventRequest(
            calendarId: "cal-1", summary: "Meeting B",
            dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))
    }
    // Let `createEvent` reach its gated re-query before the refresh starts, so the
    // interleaving under test is the real one (create's query is the OLDER of the
    // two) rather than whichever the scheduler happens to pick.
    while api.queryCallCount < 2 { await Task.yield() }

    await vm.refreshFromPush()
    #expect(vm.events.map(\.id) == ["ev-a", "ev-b"],
            "the newer refresh must publish the event it saw")

    api.release(call: 1)
    await create.value

    #expect(vm.events.map(\.id) == ["ev-a", "ev-b"],
            "the create's older, pre-create re-query must NOT overwrite the newer refresh")
}

@MainActor
@Test("a refresh that skips its own commit does not invalidate an in-flight create")
func skippedRefreshDoesNotInvalidateInFlightCreate() async {
    let api = ScriptedEventsAPI()
    let vm = await vmListingMeetingA(api)

    // The refresh (call 1) finds nothing new, so it publishes nothing at all —
    // but it DID claim a generation. The create's re-query (call 2) runs after and
    // must still be allowed to publish. A scheme where a claim alone consumed the
    // publishing slot would strand the create with nothing published in its place,
    // which is the regression this guards (and the reason the earlier design had
    // the refresh observe rather than claim).
    api.scripted = [[meetingA], [meetingA], [meetingA, meetingB]]

    await vm.refreshFromPush()
    #expect(vm.events.map(\.id) == ["ev-a"], "the refresh saw no change, so it published nothing")

    await vm.createEvent(CreateEventRequest(
        calendarId: "cal-1", summary: "Meeting B",
        dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(vm.events.map(\.id) == ["ev-a", "ev-b"],
            "the create's newer re-query must publish even though a refresh ran and skipped its commit")
}

@MainActor
@Test("a selection naming a deleted calendar falls back to the union")
func staleSelectionFallsBackToUnion() async {
    let api = ScriptedEventsAPI()
    let vm = await vmListingMeetingA(api)

    // `cal-1` vanished — deleted here, or by an external CalDAV MUA against the
    // same `bridge_caldav_*` store — leaving `cal-2` and `cal-3`. A refresh
    // re-lists calendars but must never mutate `selectedCalendar` itself
    // (events.md § Where logic lives → "Which calendars the page is scoped to"
    // — resolve at READ time only); the next query resolves the stale
    // selection against the current list and falls back to the union instead
    // of querying the gone calendar and reading empty with no error. Two
    // surviving calendars (not one) so the union's TWO calls are
    // distinguishable from a stale single-calendar query's ONE call, and the
    // asserted ids pin exactly which calendar(s) were queried — a pre-fix VM
    // would query only `cal-1` and this pin would catch it.
    let calendar2 = FaunaCalendar(id: "cal-2", name: "Cal Two")
    let calendar3 = FaunaCalendar(id: "cal-3", name: "Cal Three")
    api.calendars = [calendar2, calendar3]
    // The union's two calls each answer from their OWN calendar (cal-2/cal-3),
    // not the top-level `meetingB` (pinned to cal-1 for the cal-1-selected
    // tests above) — the visibility filter now composes on `calendarId`, so a
    // wrong-calendar fixture would spuriously hide these from the union.
    api.scripted = [[meetingA], [event("ev-b", "Meeting B", calendarId: "cal-2")],
                     [event("ev-c", "Meeting C", calendarId: "cal-3")]]

    await vm.loadCalendars()

    #expect(vm.selectedCalendar?.id == "cal-1",
            "a refresh must never clear the stale selection itself")
    #expect(api.queriedCalendarIds == ["cal-1", "cal-2", "cal-3"],
            "the fallback must query the UNION's calendars, not the vanished selection")
    #expect(vm.events.map(\.id).sorted() == ["ev-b", "ev-c"],
            "a selection naming a deleted calendar must fall back to the union, not read empty")
}

@MainActor
@Test("a stale UNION query cannot re-widen a page the user just narrowed")
func staleUnionDoesNotReWidenSelection() async {
    let api = ScriptedEventsAPI()
    api.calendars = [calendar1, FaunaCalendar(id: "cal-2", name: "Cal Two")]
    // Call order is fixed by the gate: call 0 is the union's first calendar and is
    // held open there, so `selectCalendar`'s single-calendar query is call 1, and
    // the union only reaches its second calendar (call 2) once call 0 is released.
    api.scripted = [[meetingA], [], [meetingB]]
    api.gated = [0]

    let vm = EventsVM()
    vm.configure(api: api)
    let load = Task { await vm.loadCalendars() }
    while api.queryCallCount < 1 { await Task.yield() }

    await vm.selectCalendar(calendar1)
    #expect(vm.events.isEmpty)

    api.release(call: 0)
    await load.value

    #expect(vm.events.isEmpty,
            "the union that started before the selection must not land on top of it")
}
