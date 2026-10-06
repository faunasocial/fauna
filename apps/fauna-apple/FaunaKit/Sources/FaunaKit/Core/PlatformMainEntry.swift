import Foundation

/// The hidden-File-Provider-test-arg-then-launch shape shared by every apple
/// platform's `@main` entry point (`FaunaiOSMain`/`FaunaMacOSMain`): in a DEBUG
/// build, intercept the six FP test verbs before SwiftUI's own argv handling ever
/// sees them, else fall through to the real app. Both entry points hand-kept this
/// in sync (each doc-commented "mirrors X") until this shared door.
///
/// **The interception exists only in DEBUG builds** (convention 15,
/// `e2e-automation-surface-gating.md`): the verbs write credentials and act on the
/// user's File Provider domain from argv, so a Release build's entry is just
/// `launch()` — `FileProviderTestCLI` is not compiled into it at all.
public enum PlatformMainEntry {
    public static func run(launch: () -> Void) {
        #if DEBUG && canImport(FileProvider)
            if let code = FileProviderTestCLI.run(arguments: Array(CommandLine.arguments.dropFirst())) {
                exit(code)
            }
        #endif
        launch()
    }
}
