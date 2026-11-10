import SwiftUI

/// The ONE declassify dialog (`folder-audience-public-confirm` plus its two
/// consequence lines), shared by the two places that make a folder public:
/// the owner row's `folder-audience-select` (`FoldersContent`) and a Public
/// folder wizard's Create (`FolderWizardSheetView`). `ui/folders.md`
/// § Audience and website serving → *Born public* requires the wizard to
/// present the row's SAME dialog, never a copy — the shape tui
/// (`declassify_dialog(confirm)`) and linux (`present_declassify_confirm`)
/// lifted the same way.
///
/// A public folder rests UNSEALED, content and names/paths alike (they become
/// the address of each file), which `principles.md` § The user always controls
/// their data holds as the single exception to sealed-at-rest — so answering
/// this alert is the one place the flip is committed. Both consequences are
/// stated because each surprised readers on its own: the names go public too,
/// and flipping back re-seals only FUTURE content.
///
/// The caller owns what confirm and cancel MEAN (the row writes the audience;
/// the wizard answers its machine's `confirmPublicCreate` /
/// `cancelPublicCreate`); this owns the dialog, its ids and their automation.
private struct DeclassifyConfirm: ViewModifier {
    @Binding var isPresented: Bool
    let onConfirm: () async -> Void
    let onCancel: () -> Void

    func body(content: Content) -> some View {
        content
            .alert(L.devices.declassifyTitle, isPresented: $isPresented) {
                Button(L.common.cancel, role: .cancel) { onCancel() }
                Button(L.devices.declassifyConfirm, role: .destructive) {
                    Task { await onConfirm() }
                }
                .accessibilityIdentifier(Ids.folderAudiencePublicConfirm)
                // Same mutation the alert's destructive Button runs, plus the
                // dismissal SwiftUI performs for a real press. **Order is
                // load-bearing** (measured on the residency twin): dismissing
                // first, or alongside a detached Task, tears the alert down
                // while the mutation is still queued and the write is LOST — so
                // the dismissal waits for the action, on the MainActor.
                // Env-gated no-op.
                .automationActivate(Ids.folderAudiencePublicConfirm) {
                    Task { @MainActor in
                        await onConfirm()
                        isPresented = false
                    }
                }
            } message: {
                // Two identifiable lines (not one joined `Text`): each
                // consequence is its own automation id, as on the other apps.
                VStack {
                    automationText(Ids.folderAudiencePublicNamesWarning, L.devices.declassifyBody)
                    automationText(
                        Ids.folderAudiencePublicResealWarning, L.devices.declassifyIrreversible)
                }
            }
            // An alert's content can never de-register its automation ids
            // (SwiftUI fires `.onAppear` for it but not `.onDisappear`), so the
            // same binding declares their lifetime — `AutomationRegistry.hideAll`
            // carries the rationale. No-op in production.
            .automationPresentation(
                ids: [
                    Ids.folderAudiencePublicConfirm, Ids.folderAudiencePublicNamesWarning,
                    Ids.folderAudiencePublicResealWarning,
                ], isPresented: isPresented)
    }
}

extension View {
    /// Present the shared declassify dialog while `isPresented` (see
    /// `DeclassifyConfirm`). `onCancel` runs on the Cancel button and on
    /// Escape (the alert's cancel role).
    func declassifyConfirm(
        isPresented: Binding<Bool>,
        onConfirm: @escaping () async -> Void,
        onCancel: @escaping () -> Void = {}
    ) -> some View {
        modifier(DeclassifyConfirm(
            isPresented: isPresented, onConfirm: onConfirm, onCancel: onCancel))
    }
}
