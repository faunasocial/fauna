import SwiftUI

/// The tier picker shared by every admin surface that lets an admin choose a tier by
/// name: **a cycle `Button`, not a SwiftUI `Picker`/`Menu`** — verified 2026-06-08 that
/// neither a `Picker` (titled, `.labelsHidden()`, or otherwise) nor a `Menu` exposes its
/// `accessibilityIdentifier` to XCUITest on macOS (the control 404s at `findOne`), while
/// `Button`/`Text` resolve fine. The button's label is the current value; a tap advances
/// to the next name in `names` (the e2e cycles it via repeated taps until the label
/// reads the target). `getText(id)` reads the label = current value. Same shared ids as
/// every app; only the widget is platform-native (web `<select>`, linux
/// `GtkDropDown`, android `ExposedDropdownMenu`, here a cycle button).
///
/// `names` is caller-supplied rather than read off a fixed view-model, because
/// different callers cycle different name sets over the same widget shape — the admin
/// Users hub cycles quota-tier names (`AdminVM.tierNames`); the Tiers page's
/// membership-designation row cycles three different sets per row (the admin's own
/// subscription-tier names, and quota-tier names twice — "admits at" / "lapses to").
func tierCycleButton(
    id: String,
    names: [String],
    current: @escaping () -> String,
    onPick: @escaping (String) -> Void
) -> some View {
    // The cycle action — the XCUITest path taps the `Button` (a tap advances to the
    // next name; XCUITest can't drive a `select`). For the in-process driver the
    // registry exposes a real `/element/select`: `automationSelect` maps the wire
    // string straight to `onPick`, the *same* mutation choosing a menu item performs,
    // so `driver.select(id, target)` applies the target in one call instead of
    // cycle-clicking.
    func cycle() {
        guard !names.isEmpty else { return }
        let cur = current()
        let next = (names.firstIndex(of: cur).map { $0 + 1 } ?? 0) % names.count
        onPick(names[next])
    }
    return Button {
        cycle()
    } label: {
        Text(current().isEmpty ? "—" : current())
            .frame(minWidth: 70)
    }
    .buttonStyle(.bordered)
    .accessibilityIdentifier(id)
    // `value` re-reads `current()` LIVE (not a captured snapshot): several callers'
    // rows persist across a post-mutation list refetch (same key → no onAppear), so a
    // captured string would freeze the registry read to the pre-change value.
    .automationSelect(id, value: { current() }) { newTier in onPick(newTier) }
}
