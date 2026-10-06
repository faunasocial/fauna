import Testing
import Foundation
@testable import FaunaKit

// The events **pan policy** on apple: `events-prev-month` / `events-next-month`
// move ONE VISIBLE RANGE per click, in whatever mode is showing, and move
// nothing at all in agenda (`docs/goal/ui/events.md` § Where logic lives →
// *View mode + visible range*).
//
// Tier_1 because it is pure mechanism — the distance comes from shared Rust
// (`calendarPanStep`) and the walk from `Calendar` — and because the e2e leg
// alone witnesses the `.none` arm weakly: an agenda click that quietly moved the
// anchor renders nothing, so only a direct read of `currentDate` can tell
// "did not move" from "moved somewhere invisible". Split the mile: this pins the
// arithmetic, e2e pins the wiring.
//
// Before this, `previousPeriod`/`nextPeriod` added ±1 month unconditionally
// despite their names, so a week click jumped ~4 weeks and a day click ~30 days.

@MainActor
private func vm(mode: FfiCalendarViewMode, at date: Date) -> EventsVM {
    let v = EventsVM()
    v.viewMode = mode
    v.selectDay(date)
    return v
}

private let anchor: Date = {
    var c = DateComponents()
    c.year = 2026; c.month = 3; c.day = 15; c.hour = 12
    return Calendar.current.date(from: c)!
}()

private func days(from a: Date, to b: Date) -> Int {
    Calendar.current.dateComponents([.day], from: a, to: b).day ?? 0
}

@Test @MainActor func monthViewPansOneMonth() {
    let v = vm(mode: .month, at: anchor)
    v.nextPeriod()
    #expect(Calendar.current.component(.month, from: v.currentDate) == 4)
    v.previousPeriod()
    #expect(v.currentDate == anchor)
}

@Test @MainActor func weekViewPansSevenDays() {
    let v = vm(mode: .week, at: anchor)
    v.nextPeriod()
    #expect(days(from: anchor, to: v.currentDate) == 7)
    v.previousPeriod()
    #expect(v.currentDate == anchor)
}

@Test @MainActor func dayViewPansOneDay() {
    let v = vm(mode: .day, at: anchor)
    v.nextPeriod()
    #expect(days(from: anchor, to: v.currentDate) == 1)
    v.previousPeriod()
    #expect(v.currentDate == anchor)
}

/// The arm the e2e cannot see. `.none` is "do nothing", not "move by zero": the
/// agenda list is date-unfiltered, so an anchor that drifted here would surface
/// later, as a range the user never navigated to, the moment they switch modes.
@Test @MainActor func agendaPansNothingAtAll() {
    let v = vm(mode: .agenda, at: anchor)
    let labelBefore = v.currentDateLabel
    v.nextPeriod()
    v.nextPeriod()
    v.previousPeriod()
    #expect(v.currentDate == anchor)
    #expect(v.currentDateLabel == labelBefore)
}

// `calendar-date-label` describes the VISIBLE RANGE, not always the month. It
// used to be the month in every mode, which made it useless as an observable for
// the finer modes: seven day-view pans inside one month left it unchanged, so a
// journey asserting on it could pass while the anchor had in fact moved a week.

@Test @MainActor func theLabelDistinguishesEveryMode() {
    let month = vm(mode: .month, at: anchor).currentDateLabel
    let week = vm(mode: .week, at: anchor).currentDateLabel
    let day = vm(mode: .day, at: anchor).currentDateLabel
    let agenda = vm(mode: .agenda, at: anchor).currentDateLabel
    #expect(Set([month, week, day, agenda]).count == 4)
}

/// The property the journey actually leans on: one click must MOVE the label in
/// every dated mode. This is what a month-only label could not offer.
@Test @MainActor func oneClickMovesTheLabelInEveryDatedMode() {
    for mode in [FfiCalendarViewMode.month, .week, .day] {
        let v = vm(mode: mode, at: anchor)
        let before = v.currentDateLabel
        v.nextPeriod()
        #expect(v.currentDateLabel != before, "the label must move in \(mode)")
    }
}

/// Switching modes repaints the label without moving the anchor — the month→day
/// drill-in depends on it (it flips the mode, then selects the day).
@Test @MainActor func switchingModeRepaintsTheLabelInPlace() {
    let v = vm(mode: .month, at: anchor)
    let monthLabel = v.currentDateLabel
    v.viewMode = .day
    #expect(v.currentDate == anchor)
    #expect(v.currentDateLabel != monthLabel)
}

/// The ids and the toggle order are the shared vocabulary's, not apple's — the
/// pin that keeps the `calendar-view-*` ids from drifting off the wire words the
/// other six apps switch on.
@Test func theToggleSpeaksTheSharedVocabulary() {
    let modes = calendarViewModes()
    #expect(modes.map(\.wire) == ["agenda", "month", "week", "day"])
    #expect(modes.map(\.accessibilityId) == [
        "calendar-view-agenda", "calendar-view-month",
        "calendar-view-week", "calendar-view-day",
    ])
}
