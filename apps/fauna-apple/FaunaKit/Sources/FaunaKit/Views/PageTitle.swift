import SwiftUI

/// Replaces `.navigationTitle()` and adds an `accessibilityIdentifier("page-heading")`
/// so E2E tests can verify the page loaded via a universal element ID.
///
/// Usage: `.pageTitle("Events")` instead of `.navigationTitle("Events")`
struct PageTitleModifier: ViewModifier {
    let title: String

    func body(content: Content) -> some View {
        content
            .navigationTitle(title)
            #if os(macOS)
            // On macOS, adding a ToolbarItem(placement: .principal) from inside
            // the NavigationSplitView detail causes EXC_BREAKPOINT in
            // -[NSToolbar _insertNewItemWithItemIdentifier:] when switching pages.
            // Use a hidden accessibility element instead for E2E test discovery.
            .overlay(alignment: .top) {
                automationText(Ids.pageHeading, title)
                    .frame(width: 0, height: 0)
                    .opacity(0)
                    .accessibilityElement()
                    .accessibilityLabel(title)
            }
            #else
            .toolbar {
                ToolbarItem(placement: .principal) {
                    automationText(Ids.pageHeading, title)
                        .font(.headline)
                }
            }
            #endif
    }
}

public extension View {
    func pageTitle(_ title: String) -> some View {
        modifier(PageTitleModifier(title: title))
    }
}
