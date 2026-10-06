import SwiftUI
import UniformTypeIdentifiers

/// The selected calendar's `.ics` import / export controls — `calendar-import-file`,
/// `calendar-import-button` and `calendar-export-button` (`ui/events.md` § Import /
/// Export), sitting beside the calendar list on both apple apps.
///
/// **One view for both apple apps** (priority #2): the macOS and iOS calendar lists
/// are per-app shells, but everything here is shared SwiftUI over the shared
/// `EventsVM` — `.fileImporter` is `NSOpenPanel` on macOS and the document browser
/// on iOS (the idiom `ComposeAttachButton` already uses), so no per-app picker shell
/// is needed. The guard (`events.ics_file_required` on `error-message`), the counts
/// sentence and the export's file name are `EventsVM`'s; this view owns only the
/// picker and the save presentation, which are genuinely platform UI.
///
/// The controls exist only while a calendar is selected — both act on it — and are
/// withdrawn from the tree, not disabled, when none is (a disabled control still
/// counts to an e2e probe for presence; `has_calendar_file_controls`).
///
/// **The picker carries no id and no automation entry, deliberately**: an OS-owned
/// panel no in-process agent can drive. The e2e path is the real one — the driver
/// types a path into `calendar-import-file` (the `file-upload` shape Media uses) and
/// presses Import — so only the panel's own pixels ever need an eye.
public struct CalendarFileControls: View {
    private let vm: EventsVM
    /// The URL the picker handed back. A user-picked file is security-scoped in a
    /// sandboxed app, and the bare path in `calendar-import-file` carries no such
    /// right, so the read at Import time re-opens the scope while the path still
    /// names this file (`submitImport`).
    @State private var pickedURL: URL?
    @State private var showPicker = false

    public init(vm: EventsVM) {
        self.vm = vm
    }

    public var body: some View {
        @Bindable var vm = vm
        if vm.selectedCalendar != nil {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    TextField(L.events.importIcsTooltip, text: $vm.icsImportPath)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.calendarImportFile)
                        .automationField(Ids.calendarImportFile, text: $vm.icsImportPath)
                    Button {
                        showPicker = true
                    } label: {
                        Image(systemName: "folder")
                    }
                    .help(L.media.chooseFile)
                    .accessibilityLabel(L.media.chooseFile)
                }
                HStack(spacing: 8) {
                    Button(L.events.importIcs) { submitImport() }
                        .accessibilityIdentifier(Ids.calendarImportButton)
                        // In-process e2e actuation: the same `submitImport()` the Button
                        // fires. No-op in production.
                        .automationActivate(Ids.calendarImportButton) { submitImport() }
                    Button(L.events.exportIcs) { submitExport() }
                        .accessibilityIdentifier(Ids.calendarExportButton)
                        .automationActivate(Ids.calendarExportButton) { submitExport() }
                    Spacer(minLength: 0)
                }
                .controlSize(.small)
                // No id: the outcome sentence is not an element ui.yaml registers, and
                // tui paints its own the same way (`EventsState::ics_notice`).
                if let notice = vm.icsNotice {
                    Text(notice)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .fileImporter(
                isPresented: $showPicker,
                allowedContentTypes: [UTType(filenameExtension: "ics") ?? .data]
            ) { result in
                pick(result)
            }
        }
    }

    /// The picker filled the path box — never imports by itself (`Import` is the
    /// commit, as on every app). A cancelled panel is not an error.
    private func pick(_ result: Result<URL, Error>) {
        switch result {
        case .success(let url):
            pickedURL = url
            vm.icsImportPath = url.path
        case .failure(let error):
            guard (error as? CocoaError)?.code != .userCancelled else { return }
            vm.errorMessage = error.localizedDescription
        }
    }

    /// `calendar-import-button`'s action, factored out so the automation sibling
    /// drives the exact code path the Button does.
    private func submitImport() {
        let path = vm.icsImportPath
        let scoped = pickedURL.flatMap {
            $0.path == path.trimmingCharacters(in: .whitespacesAndNewlines) ? $0 : nil
        }
        Task {
            let opened = scoped?.startAccessingSecurityScopedResource() ?? false
            defer { if opened { scoped?.stopAccessingSecurityScopedResource() } }
            await vm.importCalendarFile(atPath: path)
        }
    }

    /// `calendar-export-button`'s action. Under e2e the file goes straight into
    /// `SnapshotFileSaver.e2eDownloadDir` with no dialog — under a name no earlier
    /// export holds, so a second export never overwrites the first — the
    /// downloads location the driver's `download_dir()` reads. Otherwise the
    /// platform-native save mechanism (`NSSavePanel` on macOS, the share sheet on
    /// iOS), as the snapshot download and `AccountSettingsView.exportAndPresent` do.
    ///
    /// iOS's share sheet hands the file to a destination the user picks, so no path
    /// is known there and no `events.calendar_exported` sentence is claimed for it.
    private func submitExport() {
        Task {
            guard let file = await vm.exportCalendarFile() else { return }
            if SnapshotFileSaver.e2eDownloadDir != nil {
                if let url = SnapshotFileSaver.saveForE2ENonOverwriting(
                    suggestedFileName: file.fileName, data: file.data) {
                    vm.calendarExported(to: url.path)
                } else {
                    vm.errorMessage = L.lookup("events.error.export")
                }
                return
            }
            #if os(macOS)
            let panel = NSSavePanel()
            panel.nameFieldStringValue = file.fileName
            panel.allowedContentTypes = [UTType(filenameExtension: "ics") ?? .data]
            guard panel.runModal() == .OK, let url = panel.url else { return }
            do {
                try file.data.write(to: url)
                vm.calendarExported(to: url.path)
            } catch {
                vm.errorMessage = error.localizedDescription
            }
            #elseif os(iOS)
            let tempUrl = FileManager.default.temporaryDirectory.appendingPathComponent(file.fileName)
            do {
                try file.data.write(to: tempUrl)
                ShareSheet.present(items: [tempUrl])
            } catch {
                vm.errorMessage = error.localizedDescription
            }
            #endif
        }
    }
}
