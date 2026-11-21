// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Rich compose dialog for feed posts (ui.yaml `feed-compose-dialog`). Shared
/// macOS+iOS — presented as a `.sheet` by the host view (`compose-dialog-button`
/// on both `FeedListView` (iOS, also toolbar `compose-button`) and
/// `MacFeedDetailView` (macOS)); reads/writes the same manager-backed compose
/// fields (`vm.composeText`/`vm.composeTags`) the inline composer uses, so text
/// typed in either surface stays in sync.
public struct FeedComposeDialog: View {
    let vm: FeedVM
    let onClose: () -> Void

    public init(vm: FeedVM, onClose: @escaping () -> Void) {
        self.vm = vm
        self.onClose = onClose
    }

    public var body: some View {
        NavigationStack {
            Form {
                Section(L.common.post) {
                    TextField(L.feed.post.whatsOnYourMind, text: Binding(
                        get: { vm.composeText },
                        set: { vm.setComposeText($0) }
                    ), axis: .vertical)
                    .lineLimit(5...20)
                    TextField(L.feed.post.tagsPlaceholder, text: Binding(
                        get: { vm.composeTags },
                        set: { vm.setComposeTags($0) }
                    ))
                    if let composeError = vm.composeError {
                        Text(composeError)
                            .font(.caption)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier(Ids.composeError)
                            // Register the error read with the in-process driver —
                            // a bare `.accessibilityIdentifier` never enters the
                            // AutomationRegistry, so a submit failure here would
                            // read as absent (e2e points 2/6/11).
                            .automationValue(Ids.composeError, text: { vm.composeError })
                    }
                    FaunaPossibilityReason(vm.postSubmitPossibility)
                    // The sheet's own `error-message` — the draft's refusal or the
                    // failed save while it is open, the page's behind it standing
                    // down (`FeedVM.errorMessage`); a failed save shows on
                    // `compose-error` above too (`feed.md` § Errors & edge cases).
                    if let formError = vm.modalFormError(.compose) {
                        ErrorBanner(message: formError)
                    }
                }
            }
            .navigationTitle(L.feed.post.compose)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L.common.cancel, action: onClose)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L.common.post, action: submit)
                        .disabled(!vm.composeReady)
                        .accessibilityIdentifier(Ids.postSubmitButton)
                        // The sheet's own Post, under the sheet's scope below, so
                        // `scope="feed-compose-dialog"` presses THIS one — not the
                        // inline composer's Post still registered beneath the
                        // sheet, whose press never closes the dialog.
                        .automationActivate(
                            Ids.postSubmitButton,
                            isEnabled: { vm.composeReady },
                            perform: submit
                        )
                        .faunaPossible(vm.postSubmitPossibility)
                }
            }
        }
        .onAppear { vm.modalFormOpened(.compose) }
        .onDisappear { vm.modalFormClosed(.compose) }
        #if os(macOS)
        // macOS `.sheet` doesn't auto-size a `Form` the way iOS's full-screen
        // sheet does — an explicit frame, mirroring `MacFeedDetailView`'s
        // `PostDetailSheet`/`ReplySheet` sibling dialogs.
        .frame(width: 480, height: 420)
        #endif
        // The dialog's own registration rides the sheet's root, never the
        // `Form`: a macOS `Form` lays out wider than its sheet (its frame
        // starts left of the window's edge once the error rows grow it), and
        // the registry reads a slot whose frame starts off the window as
        // parked — a refused send then read as a closed dialog with the sheet
        // still up (as `FeedReplyDialog` registers on its sized root).
        // `.contain`, so the root's id never overwrites its children's.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.feedComposeDialog)
        // The driver's Escape door — the dismiss the toolbar's Cancel
        // (`.cancellationAction`, which a real Escape key fires) runs. ui.yaml
        // declares no dismiss element inside the dialog, so Escape is the way
        // out on every app, as tui's dialog and `ImageLightbox` already take it;
        // without it a sheet left up by a press could not be closed through
        // the registry at all.
        .automationValue(Ids.feedComposeDialog, text: { "" }, pressKey: { key in
            guard key == "Escape" else { return "feed-compose-dialog takes only Escape" }
            onClose()
            return nil
        })
        // A scope container (the self-scoping idiom: outermost over the
        // dialog's own `feed-compose-dialog` registration), so a query scoped
        // to the dialog reads the sheet's own controls — its Post, its
        // `error-message` — never the page's behind it.
        .automationScope(Ids.feedComposeDialog, index: 0)
    }

    /// Closes only on a saved post; a failure keeps the dialog open with its
    /// text and the reason inside it — the refusal or the failed save on its
    /// own `error-message` (the save's also on `compose-error`), as linux's
    /// compose dialog (`ui/README.md` § Offer only what is possible, rule 7).
    private func submit() {
        Task {
            if await vm.submitComposeForm() { onClose() }
        }
    }
}
#endif // !FAUNA_KIDS
