import FaunaKit
import FaunaMacOSLib
import Foundation

/// The macOS entry point — a thin shell over `FaunaMacOSLib`, compiled by BOTH the
/// SwiftPM `FaunaMacOS` executable (mac-app / mac-debug / e2e) and the `.xcodeproj`
/// `Fauna` app target (the appex-carrying bundle), so the two build worlds share one
/// entry point and cannot drift (M2 slice 3b, file-sync.md § On-Demand Files →
/// Apple File Provider binding).
@main
enum FaunaMacOSMain {
    static func main() {
        // Hidden File Provider test-arg path (register|remove|list|provision|revoke|
        // signal), DEBUG builds only (convention 15) — the headless driver the
        // tier_3 read-path proof uses. Domain registration must run from the
        // appex-embedding app's own process, which is why it lives behind the app
        // binary rather than a separate tool. A Release build's entry is just `launch()`.
        PlatformMainEntry.run { FaunaMacApp.main() }
    }
}
