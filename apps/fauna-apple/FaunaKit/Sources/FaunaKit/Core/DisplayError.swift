import Foundation

/// Turning a thrown error into the text an `ErrorBanner` shows — with the one
/// class that must never reach a banner filtered out: a **cancellation**.
///
/// A cancelled request is not news. It means the app itself stopped caring: a
/// SwiftUI `.task` whose view went away (a tab switch, a launch verdict seeding
/// the wizard, a session teardown replacing the shell), or a `Task` an explicit
/// cancel retired. Nothing failed, nothing is wrong, and the user did not ask a
/// question that went unanswered — so painting "HTTP error: cancelled" in red
/// states a problem that does not exist, and, because `AppMessages.error` is the
/// GLOBAL channel an `ErrorBanner` writes on appear, that stale sentence then
/// follows the user onto whatever page they land on next.
///
/// Measured on iOS: a launch whose verdict seeded the wizard, immediately
/// replaced by an authenticated session, left `AdminVM.loadDashboard`'s
/// `URLSession` call cancelled mid-flight — and the admin shell painted its
/// banner 280 ms after the session connected, on a page whose own read had
/// succeeded.
///
/// Every `catch` that surfaces a request failure to the user routes through
/// here, rather than each deciding for itself: 60 call sites across FaunaKit
/// spelled `L.errors.httpError(detail: error.localizedDescription)` inline, so
/// one of them getting the cancellation rule right would have left the other 59
/// wrong. The `String?` return is the mechanism — `nil` is "nothing to say",
/// which is what every one of those sites' optional error field already means.
public enum DisplayError {
    /// Display text for a failed request, or `nil` when it was merely cancelled.
    public static func http(_ error: Error) -> String? {
        guard !isCancellation(error) else { return nil }
        return L.errors.httpError(detail: error.localizedDescription)
    }

    /// Display text for a failed shared-Rust call, or `nil` when it was merely
    /// cancelled.
    ///
    /// A boundary `FfiError` already carries its display text — the catalog
    /// sentence the Rust side rendered (`L.devices.errorFollowNotFound`'s
    /// wording, `errorFollowFailed(message:)`'s) — so it is shown as-is. UniFFI's
    /// generated `errorDescription` is `String(reflecting:)`, which is why
    /// `"\(error)"` painted the enum's debug shape instead:
    /// `General(msg: "No public folder by that name …")`. A variant with no
    /// sentence of its own falls back to the error's own description, so a
    /// failure is never swallowed.
    public static func message(_ error: Error) -> String? {
        guard !isCancellation(error) else { return nil }
        if let ffi = error as? FfiError {
            switch ffi {
            case .General(let msg), .NestOutdated(let msg), .GuardianApprovalRequired(let msg): return msg
            case .NestIdentityChanged, .IdentitySuperseded, .AccountLocked: break
            }
        }
        return "\(error)"
    }

    /// Was this error a cancellation rather than a failure?
    ///
    /// Three spellings reach us and all three mean the same thing: Swift
    /// concurrency's own `CancellationError`, `URLSession`'s `URLError.cancelled`
    /// (what a cancelled `.task` actually throws — and whose
    /// `localizedDescription` is the bare word "cancelled"), and the same
    /// condition arriving already bridged as an `NSError`.
    public static func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if let urlError = error as? URLError, urlError.code == .cancelled { return true }
        let nsError = error as NSError
        return nsError.domain == NSURLErrorDomain && nsError.code == NSURLErrorCancelled
    }
}
