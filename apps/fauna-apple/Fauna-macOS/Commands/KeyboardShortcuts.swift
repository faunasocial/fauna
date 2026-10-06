import SwiftUI
import FaunaKit

struct SidebarShortcutCommands: Commands {
    @FocusedBinding(\.selectedSidebar) var selectedSidebar

    var body: some Commands {
        // Labels derive from the single keyed source `SidebarItem.label` so the
        // command menu can never re-diverge from the sidebar rows (#1/#3).
        CommandMenu(L.common.navigation) {
            Button(SidebarItem.conversations.label) {
                selectedSidebar = .conversations
            }
            .keyboardShortcut("1")

            Button(SidebarItem.contacts.label) {
                selectedSidebar = .contacts
            }
            .keyboardShortcut("2")

            Button(SidebarItem.events.label) {
                selectedSidebar = .events
            }
            .keyboardShortcut("3")

            Button(SidebarItem.feed.label) {
                selectedSidebar = .feed
            }
            .keyboardShortcut("4")

            Button(SidebarItem.media.label) {
                selectedSidebar = .media
            }
            .keyboardShortcut("5")

            Button(SidebarItem.backups.label) {
                selectedSidebar = .backups
            }
            .keyboardShortcut("6")

            // Status is folded into the Settings shell (its default sub-page) —
            // settings.md § Navigation model. ⌘7 opens Settings (lands on Status).
            Button(SidebarItem.settings.label) {
                selectedSidebar = .settings
            }
            .keyboardShortcut("7")
        }
    }
}

// FocusedValue key for sidebar selection
struct SelectedSidebarKey: FocusedValueKey {
    typealias Value = Binding<SidebarItem>
}

extension FocusedValues {
    var selectedSidebar: Binding<SidebarItem>? {
        get { self[SelectedSidebarKey.self] }
        set { self[SelectedSidebarKey.self] = newValue }
    }
}
