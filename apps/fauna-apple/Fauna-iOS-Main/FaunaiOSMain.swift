import FaunaKit
import FaunaiOSLib
import Foundation

/// The iOS entry point — a thin shell over `FaunaiOSLib`, compiled by BOTH the
/// SwiftPM `FaunaiOS` executable (the e2e simulator bundle) and the `.xcodeproj`
/// `Fauna-iOS` app target (the appex-carrying bundle), so the two build worlds
/// share one entry point and cannot drift (M4, file-sync.md § On-Demand Files →
/// Apple File Provider binding; mirrors `FaunaMacOSMain`).
@main
enum FaunaiOSMain {
    static func main() {
        // Hidden File Provider test-arg path (register|remove|list|provision|
        // revoke|signal), DEBUG builds only (convention 15) — on iOS driven via
        // `simctl launch` arguments; the headless half of the Files-app simulator
        // smoke. Domain registration must run from the appex-embedding app's own
        // process, same as macOS. A Release build's entry is just `launch()`.
        PlatformMainEntry.run { FaunaApp.main() }
    }
}
