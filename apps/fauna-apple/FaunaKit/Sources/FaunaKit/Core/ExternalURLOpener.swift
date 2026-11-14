import SwiftUI

/// Hand a URL the app itself holds (never one a user typed) to the OS opener —
/// or, under e2e automation, record it instead of launching. The apple twin of
/// linux's `url_opener.rs` and windows' `Services/UrlOpener.cs`, for the same
/// two reasons: no e2e click may start a real browser on a dev box
/// (`e2e-conventions.md` point 10), and a journey that activates a link
/// witnesses exactly which address was handed over by reading the record the
/// harness names (`E2eEnv.openedUrlsRecord`) — one line per open, in order, the
/// file tui's fake opener and linux's seam append to (conftest `opened_urls`).
///
/// Today's callers are the archive-import page's step-1 export link and the two
/// payment links — the OTHER-profile offer row's link (`ProfileView`'s
/// `SubscriptionOfferRow`) and the sold-post teaser's link
/// (`PostUnlockOfferTeaser`), each opening the checkout URL the shared rule
/// returned; a later external link joins them here rather than calling `openURL`
/// itself.
public enum ExternalURLOpener {
    @MainActor
    public static func open(_ url: URL, with openURL: OpenURLAction) {
        #if DEBUG
        // Compiled out of release artifacts (convention 15): the runtime
        // registry gate is not the boundary, the build flavor is.
        if AutomationRegistry.isEnabled {
            recordForHarness(url)
            return
        }
        #endif
        openURL(url)
    }

    #if DEBUG
    private static func recordForHarness(_ url: URL) {
        guard let path = E2eEnv.openedUrlsRecord else { return }
        let line = Data((url.absoluteString + "\n").utf8)
        if let handle = FileHandle(forWritingAtPath: path) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        } else {
            FileManager.default.createFile(atPath: path, contents: line)
        }
    }
    #endif
}
