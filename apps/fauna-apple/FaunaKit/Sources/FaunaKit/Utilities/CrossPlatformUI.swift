//  CrossPlatformUI.swift
//  FaunaKit
//
//  Cross-platform shims so the iOS-shaped SwiftUI view code in `Fauna-iOS/` also
//  *typechecks* for the macOS host. The `FaunaiOS` target is built (not run) for
//  the macOS host by `swift build` / `swift test` — see the `swift-test` recipe
//  in `justfile`. The iOS app never *runs* on macOS, so these macOS
//  implementations only need to compile.
//
//  Two kinds of shim live here:
//   1. `#if os(macOS)` re-declarations of iOS-only SwiftUI/UIKit API — no-op view
//      modifiers, `ToolbarItemPlacement` fallbacks, `fullScreenCover`→`sheet`,
//      `NSColor` aliases for the iOS semantic colors. They exist *only* on
//      platforms that lack the real API, so there is never an ambiguity on iOS
//      where SwiftUI/UIKit already provides them.
//   2. Cross-platform helpers (`Pasteboard`, `OpenURL`) defined on every
//      platform that pick the right platform API internally — the same pattern
//      `CopyButton` already uses for the clipboard.

import SwiftUI

// MARK: - 1. macOS re-declarations of iOS-only SwiftUI API

#if os(macOS)

// MARK: TextField autocapitalization

public struct TextInputAutocapitalization: Sendable {
    public static let never = TextInputAutocapitalization()
    public static let words = TextInputAutocapitalization()
    public static let sentences = TextInputAutocapitalization()
    public static let characters = TextInputAutocapitalization()
}

public extension View {
    /// macOS no-op — there is no on-screen keyboard to autocapitalize.
    func textInputAutocapitalization(_ autocapitalization: TextInputAutocapitalization?) -> some View { self }
}

// MARK: keyboardType

public enum UIKeyboardType: Sendable {
    case `default`, asciiCapable, numbersAndPunctuation, URL, numberPad, phonePad
    case namePhonePad, emailAddress, decimalPad, twitter, webSearch, asciiCapableNumberPad
}

public extension View {
    func keyboardType(_ type: UIKeyboardType) -> some View { self }
}

// MARK: navigationBarTitleDisplayMode

public enum NavigationBarItem {
    public enum TitleDisplayMode: Sendable { case automatic, inline, large }
}

public extension View {
    func navigationBarTitleDisplayMode(_ displayMode: NavigationBarItem.TitleDisplayMode) -> some View { self }
}

// MARK: statusBarHidden

public extension View {
    func statusBarHidden(_ hidden: Bool = true) -> some View { self }
}

// MARK: ToolbarItemPlacement — iOS bar placements → sensible macOS fallbacks

public extension ToolbarItemPlacement {
    static var topBarLeading: ToolbarItemPlacement { .automatic }
    static var topBarTrailing: ToolbarItemPlacement { .automatic }
    static var navigationBarLeading: ToolbarItemPlacement { .automatic }
    static var navigationBarTrailing: ToolbarItemPlacement { .automatic }
}

// MARK: fullScreenCover → sheet

public extension View {
    func fullScreenCover<Item: Identifiable, Content: View>(
        item: Binding<Item?>,
        onDismiss: (() -> Void)? = nil,
        @ViewBuilder content: @escaping (Item) -> Content
    ) -> some View {
        sheet(item: item, onDismiss: onDismiss, content: content)
    }

    func fullScreenCover<Content: View>(
        isPresented: Binding<Bool>,
        onDismiss: (() -> Void)? = nil,
        @ViewBuilder content: @escaping () -> Content
    ) -> some View {
        sheet(isPresented: isPresented, onDismiss: onDismiss, content: content)
    }
}

// MARK: listStyle(.insetGrouped)

public extension ListStyle where Self == DefaultListStyle {
    /// macOS fallback for iOS's `.insetGrouped` (which is unavailable on macOS).
    static var insetGrouped: DefaultListStyle { DefaultListStyle() }
}

// MARK: iOS semantic colors used via `Color(.xxx)`

public extension NSColor {
    /// Closest macOS analogue of `UIColor.secondarySystemBackground`.
    static var secondarySystemBackground: NSColor { .windowBackgroundColor }
    /// `UIColor.separator` → `NSColor.separatorColor`.
    static var separator: NSColor { .separatorColor }
}

#endif // os(macOS)

// MARK: - 2. Cross-platform helpers (defined on every platform)

/// Write a string to the system pasteboard. Mirrors `CopyButton`'s internal logic.
public enum Pasteboard {
    public static func copy(_ string: String) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(string, forType: .string)
        #else
        UIPasteboard.general.string = string
        #endif
    }
}

/// Open a URL in the user's default handler (browser, mail client, …). The
/// ONE injection point every FaunaKit call site shares (CredentialsForm's
/// hosted-auth verification link, the DNS/VPS provider-link buttons,
/// BridgeManagerVM, AccountSettingsVM, PushNotificationsRows) — gating here
/// covers all of them at once, mirroring windows' `UrlOpener.cs`.
///
/// Under the e2e harness (`FaunaE2E.isActive`) the open is suppressed and the
/// URL is logged instead of handed to the OS: a real browser is an inherited
/// channel that outlives the test run, steals foreground from the app under
/// test, and squats idle connections on the e2e `fake_cloud` fixture
/// (e2e-conventions.md point 10 — every channel through which the box can
/// reach into the app is closed at launch, by default). The handoff stays
/// observable rather than silent — a test can assert the app reached this
/// seam with the right address via the shared `fauna_log` ring.
public enum OpenURL {
    public static func open(_ url: URL) {
        #if DEBUG
        if FaunaE2E.isActive {
            logMessage(level: .info, target: "fauna.ui",
                       message: "open-url: suppressed under the e2e harness: \(url.absoluteString)")
            return
        }
        #endif
        #if os(macOS)
        NSWorkspace.shared.open(url)
        #else
        UIApplication.shared.open(url)
        #endif
    }
}

/// Open a payment/subscription URL if it passes the anti-phishing
/// `isSafePaymentUrl` gate (F-CL2 redirect class — the URL is nest/author-
/// supplied, so untrusted the same way linux's `views/profile/offers.rs` and
/// web's `isSafeNavUrl` treat it); otherwise reports `L.subscriptions
/// .unsafePaymentUrl` via `setError`. `PostUnlockOfferTeaser` and
/// `ProfileView`'s `SubscriptionOfferRow` each hand-rolled this identical
/// guard against their own VM's differently-named error property — a closure
/// rather than a `Binding` so callers whose `vm` is a plain `let` reference
/// (not `@Bindable`) don't need one.
public func openPaymentURL(_ url: String, setError: (String) -> Void) {
    guard isSafePaymentUrl(url: url) else {
        setError(L.subscriptions.unsafePaymentUrl)
        return
    }
    if let u = URL(string: url) { OpenURL.open(u) }
}

/// Present the system share sheet (`UIActivityViewController`) over the app's
/// current key window — iOS only. Every call site's own macOS presentation
/// (`AccountSettingsView`'s `NSSavePanel`; the other two call sites have no
/// macOS branch at all) stays at its own call site, out of scope here; a
/// macOS call to this function is simply a no-op.
public enum ShareSheet {
    public static func present(items: [Any]) {
        #if os(iOS)
        let activityVC = UIActivityViewController(activityItems: items, applicationActivities: nil)
        if let windowScene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
           let rootVC = windowScene.windows.first?.rootViewController {
            rootVC.present(activityVC, animated: true)
        }
        #endif
    }
}

// MARK: Image-from-bytes (dm-attachment-image, and any view rendering raw image bytes)

/// The platform's bitmap image type — `NSImage` on macOS, `UIImage` on iOS.
/// Mirrors the `Image(nsImage:)` / `Image(uiImage:)` split already used in
/// `IdentityExportSection` (QR), in one shared alias.
#if os(macOS)
public typealias FaunaPlatformImage = NSImage
#else
public typealias FaunaPlatformImage = UIImage
#endif

/// Decode raw image bytes (the plaintext an attachment `blob_hash` resolves to
/// via `ConversationsManager.attachment_bytes`) into the platform image, or
/// `nil` if the bytes aren't a decodable image. Used by `DmMessageBubble` to
/// render `dm-attachment-image` for real (conversations.md § Attachments).
public enum FaunaImage {
    public static func decode(_ data: Data) -> FaunaPlatformImage? {
        FaunaPlatformImage(data: data)
    }
}

public extension Image {
    /// Build a SwiftUI `Image` from a decoded platform image — the
    /// cross-platform `Image(nsImage:)` / `Image(uiImage:)` branch in one place.
    init(platformImage: FaunaPlatformImage) {
        #if os(macOS)
        self.init(nsImage: platformImage)
        #else
        self.init(uiImage: platformImage)
        #endif
    }
}
