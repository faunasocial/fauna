import Foundation

// Email filters ride the typed WS-RPC seam directly as `FfiEmailFilter` /
// `FfiEmailFilterRule` / `FfiEmailFilterAction` (UniFFI mirrors of
// `fauna_protocol::email`), so there is no hand-rolled Swift `EmailFilter`
// type to drift from the protocol. The helpers below are the dialog ⇄ typed
// glue: a canonical picker option set (shared by both platforms' settings
// views) and the kind/tag → enum composition the view models call.

/// `FfiEmailFilter` already carries a unique `id: Int64`; this lets the
/// settings views drive `ForEach` over the filter list directly.
extension FfiEmailFilter: Identifiable {}

/// Canonical email-filter dialog options — one source of truth so the iOS and
/// macOS Privacy settings pickers can't drift apart again. Tags are the
/// `FfiEmailFilterRule`/`FfiEmailFilterAction` kind names; labels are the
/// human text shown in the picker.
public enum EmailFilterOptions {
    public static let ruleKinds: [(tag: String, label: String)] = [
        ("SenderIs", "Sender is"),
        ("SenderDomain", "Sender domain"),
        ("SubjectContains", "Subject contains"),
        ("BodyContains", "Body contains"),
    ]
    public static let actions: [(tag: String, label: String)] = [
        ("Allow", "Allow"),
        ("Discard", "Discard"),
        ("Reject", "Reject"),
        ("Forward", "Forward"),
    ]
}

// The dialog-tag → typed `FfiEmailFilterRule` / `FfiEmailFilterAction` composition
// lives in the shared `fauna_protocol::email` encoder, surfaced as the UniFFI free
// funcs `encodeEmailFilterRule(kind:value:)` / `encodeEmailFilterActionInputs(inputs:)`
// (called from `PrivacySettingsVM`, which holds the whole `FfiFilterActionInputs`). One source of truth across every app — no
// Swift-side `from(...)` factory to drift from the protocol.

extension FfiEmailFilterAction {
    /// Short human label for the filter list row. Delegates to the shared
    /// `fauna_protocol::email::filter_action_label` (UniFFI
    /// `emailFilterActionLabel(action:)`) so the action→label vocabulary can't
    /// drift per-app (same shape as `mediaSortLabel`/`contactStatusLabel`);
    /// linux/tui/web/android already delegate the same way.
    public var label: String {
        renderLocalizedText(emailFilterActionLabel(action: self))
    }
}

/// The Swift-facing spam preferences (0.0–1.0 thresholds). `APIClient` converts
/// to/from the per-mille `u16` `FfiSpamPreferences` wire shape at the
/// `fauna.spam.*` seam, so this carries no `Codable`/wire keys of its own.
public struct SpamPreferences {
    public var spamThreshold: Double
    public var phishingThreshold: Double

    public init(spamThreshold: Double = 0.5, phishingThreshold: Double = 0.8) {
        self.spamThreshold = spamThreshold
        self.phishingThreshold = phishingThreshold
    }
}

/// Canonical spam-preferences presentation options — one source of truth so the
/// iOS and macOS spam settings can't drift apart, and neither re-derives the
/// goal-doc threshold bands. Every value comes from the
/// shared `fauna_protocol::spam` contract via the `fauna-ffi` exports — the same
/// band buckets linux reads natively and android/web consume.
/// The wire carries the thresholds as per-mille `u16`; the UI presents them as a
/// 0.0–1.0 slider. See `docs/goal/ui/settings.md` § Spam threshold slider labels.
public enum SpamOptions {
    /// A 0.0–1.0 slider value as the per-mille `u16` the wire + band fn expect,
    /// via the shared `probability_to_per_mille()` export (clamp `[0,1]`, scale
    /// ×1000, round half away from zero) — no Swift-side `* 1000`.
    public static func perMille(_ threshold: Double) -> UInt16 {
        probabilityToPerMille(probability: threshold)
    }

    /// A per-mille `u16` wire value as a 0.0–1.0 slider value, via the shared
    /// `per_mille_to_probability()` export (over-range saturates at `1.0`) — no
    /// Swift-side `/ 1000`.
    public static func threshold(_ perMille: UInt16) -> Double {
        perMilleToProbability(perMille: perMille)
    }

    /// The threshold-band i18n key (`aggressive`/`moderate`/`permissive`) for a
    /// 0.0–1.0 threshold, via the shared `spam_threshold_band()` export.
    public static func bandKey(threshold: Double) -> String {
        spamThresholdBand(thresholdPerMille: perMille(threshold))
    }

    /// Localized threshold-band label for a 0.0–1.0 threshold.
    public static func bandLabel(threshold: Double) -> String {
        switch bandKey(threshold: threshold) {
        case "aggressive": return L.status.spam.aggressive
        case "moderate": return L.status.spam.moderate
        case "permissive": return L.status.spam.permissive
        default: return ""
        }
    }
}

public struct HandleChangeResponse: Codable {
    public let handle: String
}
