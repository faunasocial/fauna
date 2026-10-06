import SwiftUI
import UniformTypeIdentifiers

/// The composer's attach affordance (`compose-file`) — the paperclip that opens
/// the platform's native file picker and stages the chosen file onto the **live**
/// composer through `FeedVM.attachComposeFile`.
///
/// `feed.md` § Where logic lives → *Click `compose-file`*: "Open file picker.
/// Client glue (pickers are platform-native); staged-file metadata + validation
/// are shared." So this leaf owns exactly the *picking*; the upload, the
/// resulting `AttachedFile` metadata, and every validation (size against the
/// nest's blob cap → `compose-error`) stay in the shared seam. The pick reaches
/// the network at nothing: the bytes are held until submit, because the
/// composer's audience is what decides their seal (`ui/media.md` § Encryption
/// at rest) and it is not known when the panel returns.
///
/// **One leaf for both apple apps** (priority #2): SwiftUI's `.fileImporter`
/// presents an `NSOpenPanel` on macOS and the document browser on iOS, so the
/// composer needs no per-app picker shell. A *file* picker is also the
/// concept every sibling client already ships — linux `gtk::FileDialog`, web
/// `<input type="file">`, windows `FileOpenPicker`, android `GetContent` — and
/// `.fileImporter` is the idiom apple itself already uses (`CalendarListView`'s
/// ICS import). Hence no apple-only photo-library flow (priorities #1/#3): a
/// `PhotosPicker` would be a net-new per-app concept with zero prior art
/// here and no sibling analogue.
///
/// Content types are `[.image, .movie]` — the richest sibling filter (web's
/// `accept="image/*,video/*"`; linux/windows filter images only), per priority
/// #4. The suite composes videos through this same element
/// (`actions/feed.py::create_post_with_video`), so an images-only filter would
/// bar users from a pipeline that demonstrably works.
///
/// **No automation `activate` is registered, deliberately.** The real action is
/// an OS-owned panel no in-process agent can drive — `drivers/http_bridge.py`
/// `set_input_files`: *"Native bridges (AT-SPI, FlaUI, Apple) can't control file
/// picker dialogs"* — so a handler here could only hang the harness on a modal
/// or quietly do nothing, which is the exact silent drop e2e rule 11 forbids.
/// The element stays *readable* (`automationValue` ⇒ visible + enabled), and a
/// stray `click("compose-file")` fails loudly as a 404
/// (`InProcessAutomationServer`'s click route requires an `activate`) instead of
/// no-op'ing. The e2e attach path is the `compose.file` state-injection command,
/// which calls the very same `attachComposeFile` seam this button calls — so the
/// mechanism is covered headlessly end-to-end and only the panel's own pixels
/// ever need an eye.
public struct ComposeAttachButton: View {
    private let vm: FeedVM
    @State private var showImporter = false

    public init(vm: FeedVM) {
        self.vm = vm
    }

    public var body: some View {
        Button {
            showImporter = true
        } label: {
            Image(systemName: "paperclip")
        }
        .buttonStyle(.plain)
        .help(L.feed.post.attachImage)
        .accessibilityLabel(L.feed.post.attachImage)
        .accessibilityIdentifier(Ids.composeFile)
        .automationValue(Ids.composeFile, isEnabled: { true })
        .fileImporter(
            isPresented: $showImporter,
            allowedContentTypes: [.image, .movie]
        ) { result in
            stage(result)
        }
    }

    /// Hand the picked URL to the shared staging seam.
    ///
    /// A user-picked file is **security-scoped** on both platforms (sandboxed
    /// apple apps), so the read must be bracketed by
    /// `start`/`stopAccessingSecurityScopedResource` — the same dance
    /// `CalendarListView`'s ICS import does. The scope is opened here and closed
    /// when the upload task finishes, since `attachComposeFile` reads the file
    /// asynchronously.
    ///
    /// Every failure lands on the page's `error-message`
    /// (`FeedVM.clientErrorMessage`) rather than being swallowed: a picker that
    /// silently does nothing is indistinguishable from a broken upload, and the
    /// composer offers no other signal that the attach never happened.
    ///
    /// The security-scope failure and the `.failure` case are both synchronous
    /// (no suspension between the picker's callback and the write), so they
    /// write unguarded; the `Task`'s catch runs after `try await
    /// attachComposeFile`, so it captures `managerGeneration` before the await
    /// and lands through the guard, same class of bug as
    /// `FeedPostActionsButton`'s web-publish verbs
    /// (`account-scoping.md` § The scoping taxonomy, `:208-236`).
    private func stage(_ result: Result<URL, Error>) {
        switch result {
        case .success(let url):
            guard url.startAccessingSecurityScopedResource() else {
                vm.setClientErrorMessage(L.media.uploadFailed)
                return
            }
            Self.upload(
                vm: vm,
                attach: { try await vm.attachComposeFile(atPath: url.path) },
                finally: { url.stopAccessingSecurityScopedResource() })
        case .failure(let error):
            // A user who just cancels the panel has not hit an error — some OS
            // versions report the dismissal as `CocoaError.userCancelled`, and
            // banner-ing that would punish the ordinary "changed my mind" path.
            // Every other failure is real and must surface.
            guard (error as? CocoaError)?.code != .userCancelled else { return }
            vm.setClientErrorMessage("\(L.media.uploadFailed): \(error)")
        }
    }

    /// The upload `Task` `stage` spawns, with the awaited attach passed in as a
    /// closure — the unit-tier seam `CallSiteCaptureOrderingTests` drives with
    /// an attach that suspends across a `reset()` and then throws. The
    /// generation is captured HERE, before the `Task` (and so before its
    /// await), and the catch lands through the guarded
    /// `FeedVM.landClientErrorMessage`. `finally` runs once the attach
    /// settles, success or failure (the security-scope close). Returns the
    /// task so the test can await it.
    @MainActor
    @discardableResult
    static func upload(
        vm: FeedVM,
        attach: @escaping () async throws -> Void,
        finally: @escaping () -> Void
    ) -> Task<Void, Never> {
        let generation = vm.managerGeneration
        return Task {
            defer { finally() }
            do {
                try await attach()
            } catch {
                vm.landClientErrorMessage(generation: generation, message: "\(L.media.uploadFailed): \(error)")
            }
        }
    }
}
