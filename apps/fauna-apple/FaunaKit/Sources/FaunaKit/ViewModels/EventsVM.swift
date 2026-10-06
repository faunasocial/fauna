import SwiftUI

@MainActor @Observable
public class EventsVM {
    public var calendars: [FaunaCalendar] = []
    public var selectedCalendar: FaunaCalendar?
    /// The `calendar-visibility` display filter's checked set (events.md §
    /// Where logic lives → *Which calendars display*) — client-side only,
    /// never persisted server-side. An **empty** set means "no filter" (the
    /// full union); `seedVisibleCalendarIds` fills it on every path that
    /// replaces `calendars`, so a fresh page paints every box checked rather
    /// than leaning on that rule (mirrors tui/linux/web/android).
    public var visibleCalendarIds: Set<String> = []
    public var events: [EventSummary] = []
    public var invitedEvents: [EventSummary] = []
    public var selectedEvent: EventDetail?
    public var selectedEventAttendees: [Attendee] = []
    public var reminderOffset: String?

    public var isLoading = false
    public var errorMessage: String?
    /// The one-line outcome of the last calendar `.ics` import or export — the
    /// imported/skipped counts, or the path an export was written to — painted
    /// beside the import/export controls (`CalendarFileControls`) with no id of its
    /// own, as tui's `ics_notice` is (events.md § Import / Export). Belongs to the
    /// selected calendar and the account: `selectCalendar` and `reset()` drop it.
    public var icsNotice: String?
    /// What `calendar-import-file` holds: the path of the `.ics` file to import,
    /// typed or filled by the shell's native picker. Account-scoped — `reset()` drops it.
    public var icsImportPath = ""
    public var currentDateLabel: String = ""

    // Forms
    public var showNewCalendar = false
    public var showNewEvent = false
    public var creatingCalendar = false
    public var creatingEvent = false
    public var inviting = false
    public var deleting = false
    public var reminderLoading = false

    /// The events API surface, held as the `EventsAPI` protocol rather than the
    /// concrete `APIClient` so this view model's publish ordering can be driven
    /// against a scripted API in a FaunaKit test (see `EventsAPI`).
    private var api: (any EventsAPI)?
    private var displayDate = Date()

    /// The displayed date for the week/day grids and the month grid. Day-grid
    /// navigation + the month-cell single-click drill-in mutate it;
    /// `calendar-date-label` describes the range it currently sits in.
    public var currentDate: Date { displayDate }

    /// The calendar view mode, in the **shared** vocabulary
    /// (`fauna_core::caltime::CalendarViewMode` over UniFFI), held here rather
    /// than in each shell's own `@State`.
    ///
    /// It lives on the view model because two things that must never disagree
    /// both read it: how far one pan click moves (`calendarPanStep`) and what
    /// `calendar-date-label` says. macOS and iOS each used to declare their own
    /// four-case Swift enum *and* their own `@State`, while the pan ignored the
    /// mode entirely and always stepped a month — so a week click jumped four
    /// weeks and a day click thirty days (`ui/events.md` § Where logic lives →
    /// *View mode + visible range*).
    public var viewMode: FfiCalendarViewMode = .agenda {
        didSet { refreshDateLabel() }
    }

    /// When non-nil, the new-event compose form seeds its start from this date
    /// (the Outlook month-cell double-click → new-event-prefilled flow). The form
    /// reads + clears it on appear.
    public var composePrefillDate: Date?

    // ── Draft persistence (reserved-folders.md § Drafts Sync, rail "events") ──
    //
    // Unlike FeedVM/ConversationsVM's rails, Events has no manager to hang a
    // generic FfiDraftsSync off (reserved-folders.md's 2026-08-17 ruling keeps
    // the three trigger shapes separate for exactly this reason): the typed
    // FfiEventDraftsSync (`libs/fauna-ffi::event_drafts`) carries the five
    // user-authored fields directly, so THIS view model is the rail object
    // that holds the live draft — the New Event opener reads
    // `resumableDraft` without consuming it (events.md § Persistence's
    // non-destructive-resume rule: a modal compose torn down and rebuilt
    // within a session must not show an empty form for a draft the nest
    // still holds), and every autosave tick updates it immediately, not when
    // the debounce fires.
    private var eventDraftsSync: FfiEventDraftsSync?
    private var draftsSaveTask: Task<Void, Never>?
    private var draftsRestoreTask: Task<Void, Never>?

    /// Bumped on every `attachEventDrafts` call (login, re-login, or an
    /// account switch that tears the client down and rebuilds it). Captured
    /// by the restore/save tasks below and checked before they touch
    /// `resumableDraft` or issue a save, so a call already in flight when the
    /// actor changes can still return after being cancelled and land nothing
    /// (`account-scoping.md` § The scoping taxonomy, the in-memory corollary:
    /// cancelling a task is necessary but never sufficient — a call already
    /// suspended inside `restoreDrafts()` is not cancelled mid-call, so it
    /// still returns and still assigns, unless the assignment itself is
    /// generation-gated).
    private var draftsGeneration: Int = 0

    /// The live events-compose draft, owned by this view model rather than
    /// the transient compose surface. The New Event opener reads and clears
    /// it (non-destructive resume); a day-cell gesture or a successful
    /// create clears it without ever reading it.
    public private(set) var resumableDraft: FfiEventDrafts?

    private static let monthFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMMM yyyy"
        return f
    }()

    private static let dayFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "EEEE, MMM d, yyyy"
        return f
    }()

    public init() {
        refreshDateLabel()
    }

    /// Repaint `calendar-date-label` for the current (mode, date) pair.
    ///
    /// **The label describes the VISIBLE RANGE, not always the month.** It used
    /// to be the month in every mode, which made it useless as an observable for
    /// the finer modes — seven day-view pans inside one month left it unchanged
    /// — and left apple the only app whose label did not track what the grid
    /// showed (linux's `update_date_label` is the worked example). Agenda is
    /// date-unfiltered, so it names the list rather than a date.
    private func refreshDateLabel() {
        switch viewMode {
        case .month:
            currentDateLabel = Self.monthFormatter.string(from: displayDate)
        case .week:
            currentDateLabel = WeekFormat.label(
                from: Calendar.current.startOfWeek(for: displayDate))
        case .day:
            currentDateLabel = Self.dayFormatter.string(from: displayDate)
        case .agenda:
            currentDateLabel = L.common.upcoming
        }
    }

    /// `events-prev-month` / `events-next-month` — **one visible range per
    /// click**, in whatever mode is showing.
    ///
    /// The distance is the shared policy (`FfiCalendarViewMode.panned`); the
    /// walk is `Calendar`'s. In agenda the step is `.none`, and that arm returns
    /// **before touching `displayDate` or the label at all** — "do nothing", not
    /// "move by zero": the agenda list ignores the anchor, so a click that
    /// quietly moved it would change state nothing renders and leave the user's
    /// next mode switch showing a range they never navigated to.
    public func previousPeriod() { pan(forward: false) }

    public func nextPeriod() { pan(forward: true) }

    private func pan(forward: Bool) {
        let moved = viewMode.panned(from: displayDate, forward: forward)
        guard moved != displayDate else { return }
        displayDate = moved
        refreshDateLabel()
    }

    /// Set the displayed date to `date` (the Outlook month→day drill-in: a
    /// single-click on a month-grid day cell). The caller flips the view mode to
    /// Day; this just moves the shared displayed date the Day grid reads.
    public func selectDay(_ date: Date) {
        displayDate = date
        refreshDateLabel()
    }

    /// Day-grid navigation (± one day) + jump-to-today, used by the Day view's
    /// own prev/next/today controls on both apple apps.
    public func previousDay() { selectDay(Calendar.current.date(byAdding: .day, value: -1, to: displayDate) ?? displayDate) }
    public func nextDay() { selectDay(Calendar.current.date(byAdding: .day, value: 1, to: displayDate) ?? displayDate) }
    public func goToToday() { selectDay(Date()) }

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by ``configure(api:)`` on an
    /// api-identity change **before** it re-points, and by the page's nil-client
    /// phase: More → Events is not unmounted by the iOS switch teardown, and its
    /// `.task` was a bare one-shot, so before this the page never re-loaded at a
    /// switch at all .
    ///
    /// ⚠ The events rail is why an unmount could never have stood in for this drop
    /// (the rejected shell-key seam — `ActorScope` § *State a view owns*): a
    /// surviving `eventDraftsSync` belongs to the OUTGOING account, and the app
    /// delegate's leave-flush (`reserved-folders.md` § The leave-flush promise)
    /// would write the INCOMING account's typed draft into it. So the rail goes
    /// first, through ``attachEventDrafts(_:)``'s own generation seam, which cancels
    /// an in-flight restore/save and gates their late assignment.
    ///
    /// `eventsGeneration`/`lastPublishedGeneration` deliberately do NOT reset: they
    /// are monotonic publish-ordering counters, not account data, and winding one
    /// back is how the lost-create defect happened. `displayDate`, `viewMode` and
    /// `currentDateLabel` stay too — which day the user is looking at, and in which
    /// grid, is device UI state no account owns.
    public func reset() {
        // The rail first: cancels any in-flight restore/save and supersedes their
        // late assignment (`attachEventDrafts` documents the generation seam).
        attachEventDrafts(nil)
        api = nil
        calendars = []
        selectedCalendar = nil
        visibleCalendarIds = []
        events = []
        rawEvents = []
        invitedEvents = []
        selectedEvent = nil
        selectedEventAttendees = []
        reminderOffset = nil
        composePrefillDate = nil
        isLoading = false
        errorMessage = nil
        icsNotice = nil
        icsImportPath = ""
        showNewCalendar = false
        showNewEvent = false
        creatingCalendar = false
        creatingEvent = false
        inviting = false
        deleting = false
        reminderLoading = false
    }

    public func configure(api: any EventsAPI) {
        if let current = self.api, current !== api { reset() }
        self.api = api
    }

    /// Attach this session's events-rail drafts autosync
    /// (`reserved-folders.md` § Drafts Sync) — the caller builds it from the
    /// concrete `APIClient` (`APIClient.eventDrafts()`) since `EventsAPI`
    /// stays scoped to the testable calendar/event surface. `nil` leaves
    /// persistence off (E2E / mock mode, or a build failure).
    ///
    /// The identity seam (`account-scoping.md` § The scoping taxonomy, the
    /// in-memory corollary): bumps `draftsGeneration` and cancels any
    /// in-flight restore/save from a superseded actor FIRST, then clears the
    /// rail for the INCOMING actor before the fresh restore lands — never
    /// relies on the outgoing side's drop alone (a restore
    /// already in flight when the user switches can still resolve after
    /// teardown and seed the wrong actor's rail).
    public func attachEventDrafts(_ sync: FfiEventDraftsSync?) {
        draftsGeneration &+= 1
        let generation = draftsGeneration
        draftsSaveTask?.cancel(); draftsSaveTask = nil
        draftsRestoreTask?.cancel(); draftsRestoreTask = nil
        resumableDraft = nil
        eventDraftsSync = sync
        guard let sync else { return }
        draftsRestoreTask = Task { @MainActor in
            do {
                let draft = try await sync.restoreDrafts()
                guard generation == self.draftsGeneration else { return }   // superseded
                self.resumableDraft = draft
            } catch {
                logMessage(level: .warn, target: "fauna.events.drafts",
                           message: "[drafts] restore on launch failed (non-fatal): \(error)")
            }
        }
    }

    /// Whether this session's drafts rail is attached. The shells' page
    /// `.task(id: SessionKey(client))` re-runs on every re-appearance of the
    /// page, and attaching again there would cancel the pending autosave and
    /// wipe `resumableDraft`, so a half-written event left inside the debounce
    /// window came back blank (`events.md` § Persistence: the New Event opener
    /// resumes the draft). They attach only while this is false. A new actor
    /// still gets a fresh rail: `configure(api:)`'s `reset()` detaches first.
    public var hasEventDraftsRail: Bool { eventDraftsSync != nil }

    /// The Events page was left. The view model outlives the page (it is
    /// app-scene-level, so the half-written event survives), which means the
    /// new-event form's open flag would survive too, and the page would come
    /// back with the form still open. Close it; its fields stay the draft,
    /// which New Event resumes (`events.md` § Persistence).
    public func pageLeft() {
        showNewEvent = false
    }

    /// Debounced persist after a compose edit — one window
    /// (`ConversationsVM.draftsSaveDebounce`) shared with `FeedVM`/
    /// `ConversationsVM`'s own trigger glue. Updates `resumableDraft`
    /// IMMEDIATELY (not after the debounce), so a compose surface torn down
    /// and rebuilt mid-session still resumes the very latest text — the
    /// debounce only defers the network write. A no-op when no rail is
    /// attached (pre-login / E2E).
    public func scheduleDraftsSave(_ draft: FfiEventDrafts) {
        resumableDraft = draft
        guard let sync = eventDraftsSync else { return }
        draftsSaveTask?.cancel()
        let generation = draftsGeneration
        draftsSaveTask = Task { @MainActor in
            try? await Task.sleep(for: ConversationsVM.draftsSaveDebounce)
            if Task.isCancelled { return }
            guard generation == self.draftsGeneration else { return }
            do {
                try await sync.saveDrafts(summary: draft.summary, dtstart: draft.dtstart,
                                           dtend: draft.dtend, description: draft.description,
                                           location: draft.location)
            } catch {
                logMessage(level: .info, target: "fauna.events.drafts",
                           message: "[drafts] autosave failed (transient): \(error)")
            }
        }
    }

    /// `scheduleDraftsSave(_:)` above, taking the compose form directly —
    /// was a byte-identical per-shell private wrapper on `EventFormView`
    /// (iOS) / `MacEventFormView` (macOS) until this harvest pass found it
    /// .
    public func scheduleDraftsSave(from form: EventComposeForm) {
        scheduleDraftsSave(form.asDrafts)
    }

    /// Clear the rail — a day-cell "start fresh" gesture (`beginCompose`
    /// below), or a successful create's success path. Cancels any pending
    /// autosave first so a stale debounced write can't resurrect the just-
    /// cleared draft.
    private func clearDraftsRail() {
        resumableDraft = nil
        draftsSaveTask?.cancel(); draftsSaveTask = nil
        guard let sync = eventDraftsSync else { return }
        let generation = draftsGeneration
        Task { @MainActor in
            guard generation == self.draftsGeneration else { return }
            try? await sync.saveDrafts(summary: "", dtstart: "", dtend: "",
                                        description: "", location: "")
        }
    }

    /// Flush the pending autosave immediately — the leave-flush promise
    /// (`reserved-folders.md` § The leave-flush promise): a leave door must
    /// not lose the debounce window's last edit. Called from each target's
    /// leave-door observer (window-close/quit on macOS, background
    /// transition on iOS). Best-effort, matching the rail's own save
    /// semantics — a failure here leaves the debounced write to have already
    /// covered most of the input, and the next restore picks up whatever did
    /// land.
    public func flushDraftsNow() async {
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        guard let sync = eventDraftsSync, let draft = resumableDraft else { return }
        try? await sync.saveDrafts(summary: draft.summary, dtstart: draft.dtstart,
                                    dtend: draft.dtend, description: draft.description,
                                    location: draft.location)
    }

    /// Monotonic token for `events` re-queries, so a STALE one cannot overwrite
    /// a newer one.
    ///
    /// Two re-queries are routinely in flight at once, and they finish out of
    /// order. `createCalendar` → `loadCalendars` assigns `calendars` (which is
    /// what renders the new row) and only *then* awaits its events query — and
    /// with no calendar selected that query is the UNION, a **sequential fan-out
    /// of one RPC per owned calendar**. So the moment the user taps the calendar
    /// they just made, `selectCalendar`'s single-calendar query (1 RPC) starts
    /// behind the union's N, finishes first, and the union then lands on top of
    /// it — putting every other calendar's events back on a page the user has
    /// just narrowed. It gets worse the more calendars the account has, because
    /// N is the account's calendar count.
    ///
    /// Found 2026-07-30 by `test_events.py --app macos,ios` on iOS, where the
    /// agenda is an eager `ScrollView` that registers every row and so reports
    /// the truth: after selecting a freshly-created (empty) calendar the agenda
    /// still listed 10 events, by name, from earlier tests. macOS is latently
    /// affected by the same race — nothing about it is iOS-specific; the two
    /// apps just lose the race at different rates.
    private var eventsGeneration = 0

    /// The generation of the most recently PUBLISHED list. A query may publish
    /// only if it is newer than whatever is already on screen, which is what makes
    /// the ordering total rather than just "not obviously stale".
    ///
    /// An equality guard (`generation == eventsGeneration`, the first shape of
    /// this fix) is NOT enough, because two queries can share a generation: the
    /// background refresh used to *observe* the counter instead of claiming one,
    /// so a create's re-query and a later push-driven refresh both ran under the
    /// create's generation. The refresh fetched the new event and published it,
    /// and the create's own older, pre-write re-query then passed the equality
    /// guard and put the pre-create list back — the user creates an event, the
    /// compose closes as if it worked, and the row is simply absent. Pinned by
    /// `EventsVMPublishOrderingTests.createDoesNotClobberNewerRefresh`.
    private var lastPublishedGeneration = 0

    /// Claim the next generation. Call BEFORE the `await` that fetches — EVERY
    /// re-query claims one, background refreshes included.
    private func beginEventsQuery() -> Int {
        eventsGeneration += 1
        return eventsGeneration
    }

    /// Publish a fetched list, unless something newer has already been published —
    /// in which case this result is stale and is dropped.
    ///
    /// Note this is a guard on *publishes*, not on claims, which is why a refresh
    /// can now claim a generation safely: a refresh that decides nothing changed
    /// simply never calls this, so it consumes no publishing slot and cannot
    /// invalidate a create that is still in flight (the hazard that made the
    /// earlier design have the refresh observe rather than claim — see
    /// `skippedRefreshDoesNotInvalidateInFlightCreate`).
    private func commitEvents(_ fetched: [EventSummary], generation: Int) {
        guard generation > lastPublishedGeneration else { return }
        lastPublishedGeneration = generation
        rawEvents = fetched
        events = displayedEvents(rawEvents)
    }

    /// The unfiltered fetch result `events` is re-derived from — re-run through
    /// [displayedEvents] on every fetch AND every `calendar-visibility` toggle,
    /// so toggling a box is purely local (no refetch: this cache already holds
    /// every owned calendar's events on the no-selection union arm).
    private var rawEvents: [EventSummary] = []

    /// `raw` filtered through the shared `calendar_is_displayed` composition
    /// (events.md § Where logic lives → *Which calendars display*): a live
    /// selection wins outright (visibility never applies to a selection); with
    /// none, `visibleCalendarIds` filters the union, an **empty** set meaning
    /// "no filter" (the full union). An event with no `calendarId` is never
    /// hidden (defensive only — the seam always populates it).
    private func displayedEvents(_ raw: [EventSummary]) -> [EventSummary] {
        let existingIds = calendars.map(\.id)
        let visible = Array(visibleCalendarIds)
        return raw.filter { ev in
            guard let calId = ev.calendarId else { return true }
            return calendarIsDisplayed(selected: selectedCalendar?.id, existingIds: existingIds,
                                        visibleCalendars: visible, calendarId: calId)
        }
    }

    /// Flip one calendar's `calendar-visibility` checkbox — purely local
    /// display state; re-filters the already-fetched `rawEvents`, no refetch.
    public func toggleCalendarVisibility(calendarId: String) {
        if !visibleCalendarIds.insert(calendarId).inserted {
            visibleCalendarIds.remove(calendarId)
        }
        events = displayedEvents(rawEvents)
    }

    /// Fill `visibleCalendarIds` on a calendar list that just replaced `calendars`
    /// (`loadCalendars` / `refreshIfChanged`), so the page paints every box
    /// checked instead of leaning on the empty-set-is-union rule (mirrors
    /// tui/linux/web/android `seedVisibleCalendars`). A **brand-new** calendar
    /// (not in `previous`) starts visible; an existing one keeps whatever the
    /// user chose. Unchecking every box empties the set, which the shared
    /// predicate reads as the union — so the next load re-seeds it.
    private func seedVisibleCalendarIds(previous: [FaunaCalendar], next: [FaunaCalendar]) {
        let previousIds = Set(previous.map(\.id))
        for cal in next where !previousIds.contains(cal.id) {
            visibleCalendarIds.insert(cal.id)
        }
        if visibleCalendarIds.isEmpty {
            visibleCalendarIds = Set(next.map(\.id))
        }
    }

    public func loadCalendars() async {
        guard let api else { return }
        do {
            let previous = calendars
            let loaded = try await api.listCalendars()
            // The in-flight clause (`account-scoping.md` § The scoping taxonomy): a
            // read already suspended for the outgoing account is not cancelled
            // mid-call, so it returns after the drop and would still assign.
            guard self.api === api else { return }
            calendars = loaded
            seedVisibleCalendarIds(previous: previous, next: calendars)
            let generation = beginEventsQuery()
            let items = try await queryEventItems()
            guard self.api === api else { return }
            commitEvents(items, generation: generation)
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    /// Events over the encrypted CalDAV store: when a calendar is selected,
    /// only its events; otherwise the union of every owned calendar's events
    /// (deduped by id) — the encrypted store has no cross-calendar query, so
    /// the VM fans out. Mirrors windows' `EventsViewModel.QueryEventItemsAsync`
    /// (the richest existing pattern; converged onto per priority #4 — apple
    /// previously showed nothing until a calendar was explicitly selected).
    /// Throws on the first failing fetch; the background poll (`refreshIfChanged`)
    /// wraps call sites in `try?` for best-effort degradation instead.
    private func queryEventItems() async throws -> [EventSummary] {
        guard let api else { return [] }
        // Resolve the selection against the calendars that actually exist right
        // now, at READ time — never by mutating `selectedCalendar` itself
        // (events.md § Where logic lives → "Which calendars the page is scoped
        // to"). A selection naming a calendar deleted here or by an external
        // CalDAV MUA against the same `bridge_caldav_*` store falls back to the
        // union instead of silently querying a gone calendar and reading empty
        // with no error; mirrors windows' `EventsViewModel.QueryEventItemsAsync`
        // / android's `EventsVM.kt:376`.
        let existingIds = calendars.map(\.id)
        if let resolvedId = resolveCalendarSelection(selected: selectedCalendar?.id, existingIds: existingIds) {
            return try await api.queryEvents(calendarId: resolvedId)
        }
        var byId: [String: EventSummary] = [:]
        for cal in calendars {
            for ev in try await api.queryEvents(calendarId: cal.id) {
                byId[ev.id] = ev
            }
        }
        return Array(byId.values)
    }

    /// `queryEventItems`'s COST-saving twin for the backstop poll
    /// (`refreshIfChanged` below) — `docs/goal/ui/events.md` § Implementation
    /// status today, the delta-sync backstop; NOT the quick-appearance path
    /// (`fauna.calendar.changed`, already consumed everywhere). Returns `nil`
    /// when nothing changed, so the poll can skip touching the model
    /// entirely — mirrors linux `fetch_events_inner`'s early return, widened
    /// to apple's union-of-calendars shape.
    ///
    /// Single-calendar selection: one seeded call, its own `nil`/events
    /// answer is authoritative — mirrors the shared seam's own single-call
    /// example verbatim, no merge needed.
    ///
    /// Union (no selection, N calendars): a seeded call per calendar is used
    /// as a change SIGNAL only, never partially merged — accepting one
    /// calendar's fresh events while reusing another's carried-forward rows
    /// would need a per-calendar cache this VM does not keep, and a stale
    /// entry in that cache (e.g. after a mode switch between single-select
    /// and union polling) would silently under-report the union with no
    /// error. So: every calendar unchanged → `nil` (skip, matching the
    /// single-calendar contract); any calendar changed → fall through to
    /// `queryEventItems()` for one authoritative full read of the whole
    /// union. This still cuts the overwhelmingly common all-quiet tick to
    /// zero unseals; it does not (yet) avoid re-reading unchanged calendars
    /// on a tick where a sibling calendar changed — a further optimization
    /// this comment deliberately does not claim.
    private func queryEventItemsSeeded() async throws -> [EventSummary]? {
        guard let api else { return nil }
        let existingIds = calendars.map(\.id)
        if let resolvedId = resolveCalendarSelection(selected: selectedCalendar?.id, existingIds: existingIds) {
            return try await api.queryEventsSeeded(calendarId: resolvedId)
        }
        var anyChanged = false
        for cal in calendars {
            if try await api.queryEventsSeeded(calendarId: cal.id) != nil {
                anyChanged = true
            }
        }
        guard anyChanged else { return nil }
        return try await queryEventItems()
    }

    public func loadInvitedEvents() async {
        guard let api else { return }
        do {
            let loaded = try await api.queryMyEvents(filter: "invited")
            guard self.api === api else { return }   // the in-flight clause
            invitedEvents = loaded
        } catch {
            guard self.api === api else { return }
            invitedEvents = []
        }
    }

    public func selectCalendar(_ cal: FaunaCalendar) async {
        guard let api else { return }
        selectedCalendar = cal
        selectedEvent = nil
        selectedEventAttendees = []
        reminderOffset = nil
        icsNotice = nil   // the counts / path described the previous calendar
        isLoading = true
        defer { isLoading = false }
        let generation = beginEventsQuery()
        do {
            commitEvents(try await queryEventItems(), generation: generation)
        } catch {
            errorMessage = DisplayError.http(error)
            commitEvents([], generation: generation)
        }
    }

    public func createCalendar(name: String) async {
        guard let api else { return }
        creatingCalendar = true
        defer { creatingCalendar = false }
        do {
            try await api.createCalendar(name: name)
            await loadCalendars()
            showNewCalendar = false
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func createEvent(_ request: CreateEventRequest) async {
        guard let api else { return }
        // Fall back to the first available calendar when none is explicitly
        // selected — the cross-app create-without-select contract (matches
        // linux/windows; the week/day/month grids create without a prior select).
        // Deliberately does NOT assign to `selectedCalendar` — mirrors windows'
        // `CreateEventAsync` (`cal` is a local resolution only), so a create from
        // the union-of-all-calendars browsing state (selectedCalendar == nil)
        // stays in that state afterwards instead of silently narrowing the page
        // to a single calendar.
        guard let cal = selectedCalendar ?? calendars.first else { return }
        creatingEvent = true
        defer { creatingEvent = false }
        // Honor the resolved calendar even when the form built the request with an
        // empty calendarId (no selection at submit time).
        let req = request.calendarId.isEmpty
            ? CreateEventRequest(calendarId: cal.id, summary: request.summary,
                                 dtstart: request.dtstart, dtend: request.dtend,
                                 description: request.description, location: request.location)
            : request
        do {
            try await api.createEvent(req)
            // queryEventItems(), not a single-calendar queryEvents(calendarId:) —
            // must respect the same selected-vs-union scope the rest of the VM
            // uses (loadCalendars/selectCalendar), or a create while browsing the
            // union view appears to lose events that are still there under a
            // different calendar's scope.
            let generation = beginEventsQuery()
            commitEvents(try await queryEventItems(), generation: generation)
            showNewEvent = false
            // A successful create clears the rail (events.md § Persistence) —
            // on this success path, never at the submit click: a failed
            // create above leaves the compose open and the draft intact so
            // the user can retry.
            clearDraftsRail()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Open the new-event compose for a **whole day** — the month grid's
    /// empty-day drill-in (events.md § Month grid), which names a date and no
    /// time. Seeds the shared working-hours start (`workingDayStart()`,
    /// `fauna_core::caltime::WORKING_DAY_START`), as every app does for a
    /// day-granularity origin.
    ///
    /// Deliberately a separate entry point from ``beginCompose(atSlot:)``: the
    /// caller knows whether it picked a *day* or an *instant*, and the two
    /// become indistinguishable once flattened to a `Date` — 00:00 is both "no
    /// time was chosen" and the perfectly real midnight slot. Inferring it back
    /// from the value is what pinned every empty-slot quick-create to 09:00
    /// (events.md § Implementation status today, the empty-time-slot bullet).
    public func beginCompose(onDay date: Date) {
        let cal = Calendar.current
        let start = cal.startOfDay(for: date)
        let workingStart = FaunaFFISwift.workingDayStart()
        beginCompose(prefill: cal.date(
            bySettingHour: Int(workingStart.hour),
            minute: Int(workingStart.minute),
            second: 0,
            of: start
        ) ?? start)
    }

    /// Open the new-event compose at an **exact instant** — the week/day grid's
    /// `events-time-slot-{HH-MM}` click, which carries the column's date and the
    /// slot's snapped time (events.md § Week & day timeline views). The time is
    /// used verbatim, midnight slots included.
    public func beginCompose(atSlot date: Date) {
        beginCompose(prefill: date)
    }

    /// The form seeds its start from `composePrefillDate` on appear.
    private func beginCompose(prefill date: Date) {
        // A day-cell gesture starts fresh (events.md § Persistence) — clear
        // the rail before the form opens, so its `.onAppear` resume check
        // (`resumableDraft == nil`) never fires for this open.
        clearDraftsRail()
        composePrefillDate = date
        showNewEvent = true
    }

    public func selectEvent(_ ev: EventSummary) async {
        guard let api else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            // The encrypted-CalDAV detail carries the roster + reminder in one read.
            let detail = try await api.getEvent(id: ev.id)
            selectedEvent = detail
            selectedEventAttendees = detail.attendees
            reminderOffset = detail.reminder
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Re-read the selected event after a roster/reminder mutation (invite / rsvp),
    /// refreshing the detail, roster, and reminder from one CalDAV read.
    private func refreshSelectedEvent() async {
        guard let api, let id = selectedEvent?.id else { return }
        if let detail = try? await api.getEvent(id: id) {
            selectedEvent = detail
            selectedEventAttendees = detail.attendees
            reminderOffset = detail.reminder
        }
    }

    public func deleteEvent() async {
        guard let api, let event = selectedEvent, let cal = selectedCalendar else { return }
        deleting = true
        defer { deleting = false }
        do {
            try await api.deleteEvent(id: event.id)
            selectedEvent = nil
            selectedEventAttendees = []
            let generation = beginEventsQuery()
            commitEvents(try await api.queryEvents(calendarId: cal.id),
                         generation: generation)
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func invite(email: String) async {
        guard let api, let event = selectedEvent else { return }
        inviting = true
        defer { inviting = false }
        do {
            try await api.inviteToEvent(eventId: event.id, email: email)
            await refreshSelectedEvent()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func rsvp(response: RsvpResponse) async {
        guard let api, let event = selectedEvent else { return }
        do {
            try await api.rsvpEvent(eventId: event.id, response: response)
            await refreshSelectedEvent()
            await loadInvitedEvents()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Card-level RSVP (`event-rsvp-*` on `event-card`, events.md § components):
    /// unlike `rsvp(response:)` this targets `eventId` directly rather than
    /// `selectedEvent`, so an agenda-list row can RSVP without first opening the
    /// detail page. `refreshSelectedEvent()` is a safe no-op when the RSVP'd
    /// event isn't the currently-selected one.
    public func rsvp(eventId: String, response: RsvpResponse) async {
        guard let api else { return }
        do {
            try await api.rsvpEvent(eventId: eventId, response: response)
            await refreshSelectedEvent()
            await loadInvitedEvents()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func setReminder(offset: String) async {
        guard let api, let event = selectedEvent else { return }
        reminderLoading = true
        defer { reminderLoading = false }
        do {
            try await api.setReminder(eventId: event.id, offset: offset)
            reminderOffset = offset
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func removeReminder() async {
        guard let api, let event = selectedEvent else { return }
        reminderLoading = true
        defer { reminderLoading = false }
        do {
            try await api.removeReminder(eventId: event.id)
            reminderOffset = nil
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func importCalendar(icsText: String) async -> CalendarImportResult? {
        guard let api, let cal = selectedCalendar else { return nil }
        do {
            let result = try await api.importCalendar(calendarId: cal.id, icsText: icsText)
            let generation = beginEventsQuery()
            commitEvents(try await api.queryEvents(calendarId: cal.id),
                         generation: generation)
            return result
        } catch {
            errorMessage = DisplayError.http(error)
            return nil
        }
    }

    public func exportCalendar() async -> String? {
        guard let api, let cal = selectedCalendar else { return nil }
        do {
            return try await api.exportCalendar(calendarId: cal.id)
        } catch {
            errorMessage = DisplayError.http(error)
            return nil
        }
    }

    // MARK: - `.ics` import / export controls (events.md § Import / Export)
    //
    // The guard, the counts sentence and the file name are shared logic, so both
    // apple shells say the same thing; `CalendarFileControls` owns only the
    // picker and the save presentation, which are genuinely platform UI.

    /// `calendar-import-button`: import the `.ics` file at `path` — what
    /// `calendar-import-file` holds, typed or filled by the picker — into the
    /// selected calendar, and say how many events it imported and skipped
    /// (``icsNotice``; a partial import is an expected outcome, not an error).
    ///
    /// Nothing chosen says so on `error-message` — never a silent no-op, which
    /// reads as a dead button (`events.ics_file_required`; the same guard media's
    /// `uploadFromPath` carries). Each attempt replaces the previous one's
    /// outcome, so a stale refusal never outlives a good import nor the counts a
    /// refused one.
    public func importCalendarFile(atPath path: String) async {
        errorMessage = nil
        icsNotice = nil
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            errorMessage = L.events.icsFileRequired
            return
        }
        let text: String
        do {
            text = try String(contentsOfFile: trimmed, encoding: .utf8)
        } catch {
            errorMessage = error.localizedDescription
            return
        }
        let api = self.api
        guard let result = await importCalendar(icsText: text) else { return }
        // An account switch while the import was in flight: `reset()` already
        // dropped this page's state, and the counts describe the outgoing account.
        guard self.api === api else { return }
        icsNotice = L.events.importResult(
            imported: String(result.imported),
            skipped: String(result.skipped),
            total: String(result.total))
    }

    /// `calendar-export-button`: the selected calendar as one `.ics` file's name
    /// and bytes, for the shell to write into the downloads location. Nothing is
    /// written here, so nothing is claimed yet — the shell reports where the file
    /// landed through ``calendarExported(to:)``. The name is the calendar's own,
    /// made safe as a single path component (a calendar called `Home / Away`
    /// must not walk out of the downloads directory).
    public func exportCalendarFile() async -> CalendarExportFile? {
        errorMessage = nil
        icsNotice = nil
        let api = self.api
        guard let name = selectedCalendar?.name, let text = await exportCalendar() else { return nil }
        guard self.api === api else { return nil }
        return CalendarExportFile(fileName: Self.icsFileName(forCalendar: name), data: Data(text.utf8))
    }

    /// The shell wrote the export to `path`: name the file (`events.calendar_exported`).
    public func calendarExported(to path: String) {
        icsNotice = L.events.calendarExported(path: path)
    }

    static func icsFileName(forCalendar name: String) -> String {
        let unsafe = CharacterSet(charactersIn: "/\\:").union(.controlCharacters)
        let cleaned = String(name.unicodeScalars.map { unsafe.contains($0) ? "-" : Character($0) })
            // A leading dot would hide the file; edge dots and blanks carry nothing.
            .trimmingCharacters(in: CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: ".")))
        return (cleaned.isEmpty ? "calendar" : cleaned) + ".ics"
    }

    /// Whether the calling actor organizes the selected event — gates the
    /// author-only affordances (delete / invite). Organizer-based (events.md
    /// § State & data shape), replacing the old `authorId == myActorId` check.
    public var isOrganizer: Bool {
        selectedEvent?.organizedByMe ?? false
    }

    // MARK: - Quick-appearance poll (events.md § Implementation status —
    // "Quick-appearance poll" follow-on; mirrors linux's
    // CALENDAR_POLL_INTERVAL_SECS + windows' RefreshIfChangedAsync/DispatcherTimer).

    /// Poll cadence — matches linux (`CALENDAR_POLL_INTERVAL_SECS`) and windows
    /// (`PollIntervalSecs`) so all apps surface an externally-made CalDAV
    /// write within a comparable window.
    private static let pollIntervalNanoseconds: UInt64 = 10_000_000_000

    /// Order-insensitive equality by a stable id — mirrors windows'
    /// `RowsDifferById` / linux's `rows_differ_by_id`. A same-content refetch in
    /// any wire order compares equal, so a steady-state poll is a true no-op
    /// rather than reordering-triggered UI churn (clearing + refilling a bound
    /// list every tick would flicker the page and could race an in-flight tap).
    private func sameById<T: Equatable>(_ a: [T], _ b: [T], id: (T) -> String) -> Bool {
        guard a.count == b.count else { return false }
        return a.sorted { id($0) < id($1) } == b.sorted { id($0) < id($1) }
    }

    /// Silent background refresh for the Events-page poll: re-fetch calendars +
    /// events and swap the bound arrays ONLY when the id-set or a shown field
    /// actually changed. Deliberately does NOT touch `isLoading` (a background
    /// poll must not flash the spinner) and swallows transient RPC errors —
    /// keeps the last-good view, the next tick retries (mirrors windows'
    /// `RefreshIfChangedAsync`).
    private func refreshIfChanged() async {
        guard let api else { return }
        if let fetched = try? await api.listCalendars(),
            // The in-flight clause, and the poll is where it bites hardest: this loop
            // lives in the page's `.task`, so before the session-keyed re-key it kept
            // polling as the OUTGOING account for as long as the page stayed mounted
            // — the corollary's "the loops that WRITE that state must be retired by
            // the same drop".
            self.api === api,
            !sameById(fetched, calendars, id: { $0.id })
        {
            let previous = calendars
            calendars = fetched
            seedVisibleCalendarIds(previous: previous, next: fetched)
        }
        // The refresh CLAIMS a generation like every other re-query. It used to
        // merely observe one, to avoid invalidating an in-flight foreground
        // publish — but observing made the ordering non-total: this tick and a
        // foreground query then shared a generation, and whichever finished LAST
        // won even when it was the older of the two (the lost-create defect;
        // see `lastPublishedGeneration`). Claiming is safe now because the guard
        // is on publishing, not on claiming: a tick that decides nothing changed
        // returns below without calling `commitEvents`, so it consumes no
        // publishing slot and strands nothing.
        let generation = beginEventsQuery()
        // `queryEventItemsSeeded()` (events.md § Implementation status today
        // — the delta-sync backstop) consults the shared seam before paying
        // for a full unseal; `nil` means "unchanged, don't touch the model"
        // and — like a transient RPC error — is exactly what `try?`'s
        // `nil`-on-failure-or-nil already collapses into a no-op here.
        // Compared against `rawEvents` (the last-published RAW fetch), not the
        // filtered `events` — a visibility filter can make the two permanently
        // differ in count, which would make every tick look "changed" and defeat
        // the steady-state no-op this diff exists for.
        if let fetched = try? await queryEventItemsSeeded(), !sameById(fetched, rawEvents, id: { $0.id }) {
            commitEvents(fetched, generation: generation)
        }
    }

    /// Poll while the Events page is visible. Callers wire this into a
    /// `.task { … ; await vm.pollWhileVisible() }` on the page — SwiftUI
    /// cancels the enclosing task automatically when the view disappears,
    /// which stops the loop with no explicit start/stop bookkeeping (unlike
    /// linux's timeout source / windows' DispatcherTimer, both view-lifetime
    /// objects on platforms without structured-concurrency task cancellation).
    public func pollWhileVisible() async {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: Self.pollIntervalNanoseconds)
            if Task.isCancelled { return }
            await refreshIfChanged()
        }
    }

    /// Re-pull on a `fauna.calendar.changed` push (`FaunaClient`'s
    /// `.onCalendarChanged`) — cuts the up-to-`pollIntervalNanoseconds` poll
    /// latency down to push latency. The poll above stays as backstop (a
    /// dropped push, or a session where the observer isn't wired). Same
    /// diff-before-swap `refreshIfChanged()` the poll uses — a push is a
    /// hint, not a reason to flash the spinner or churn a same-content list.
    /// Mirrors android `EventsVM.refreshFromPush` (`ApiClient
    /// .calendarChangedTick`).
    public func refreshFromPush() async {
        await refreshIfChanged()
    }
}
