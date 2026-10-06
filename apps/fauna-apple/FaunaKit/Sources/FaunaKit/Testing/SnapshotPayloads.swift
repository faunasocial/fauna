import Foundation

/// JSON payload shapes for the `call_machine_method` E2E bridge
/// (`tests/e2e-unified/drivers/machine_test_setter.py`). The wire format
/// mirrors `serde_json::Value` for the corresponding Rust types so a
/// Python test can build a dict and have it land as a typed snapshot
/// without an extra schema layer.

public struct HandleCheckSnapshotPayload: Decodable {
    public let phase: String                 // "Idle" | "Parsing" | ... | "Complete"
    public let outcome: HandleCheckOutcomePayload
    public let message: LocalizedTextPayload
    public let continue_enabled: Bool
    public let control_checkbox_visible: Bool
    public let control_checkbox_checked: Bool

    public func intoSnapshot() -> HandleCheckSnapshot {
        HandleCheckSnapshot(
            phase: parsePhase(phase),
            outcome: outcome.intoOutcome(),
            message: message.intoLocalizedText(),
            continueEnabled: continue_enabled,
            controlCheckboxVisible: control_checkbox_visible,
            controlCheckboxChecked: control_checkbox_checked
        )
    }
}

public struct InviteRequestSnapshotPayload: Decodable {
    public let state: InviteRequestStatePayload
    public let message: LocalizedTextPayload
    public let continue_enabled: Bool
    public let recheck_visible: Bool
    public let out_of_band_code_state: OobCodeStatePayload
    public let oob_message: LocalizedTextPayload
    /// The store-age onboarding notice the machine derives from the platform
    /// claim (`family-safety.md` § App surface → *Age-band surfaces*); absent
    /// on the wire = nothing to paint.
    public let age_notice: LocalizedTextPayload?

    public func intoSnapshot() -> InviteRequestSnapshot {
        InviteRequestSnapshot(
            state: state.intoState(),
            message: message.intoLocalizedText(),
            continueEnabled: continue_enabled,
            recheckVisible: recheck_visible,
            outOfBandCodeState: out_of_band_code_state.intoState(),
            oobMessage: oob_message.intoLocalizedText(),
            ageNotice: age_notice?.intoLocalizedText()
        )
    }
}

public struct LocalizedTextPayload: Decodable {
    public let key: String
    public let args: [String: String]

    public func intoLocalizedText() -> LocalizedText {
        LocalizedText(key: key, args: args)
    }
}

/// Shared decode skeleton for every enum in this file mirroring a Rust serde
/// externally-tagged wire shape: a unit variant decodes as a bare string,
/// any variant with fields as a single-key dict. `HandleCheckOutcomePayload`/`InviteRequestStatePayload`/
/// `OobCodeStatePayload` each hand-rolled the identical bare-string dispatch
/// and the two `DecodingError` constructions — only that prefix and tail were
/// ever identical; the per-variant `Args` struct and its field mapping onto
/// the enum's associated values genuinely differ per type and stay inline.
protocol RustTaggedEnumPayload: Decodable {}

extension RustTaggedEnumPayload {
    /// The matched case for a bare-string wire value, `nil` for anything
    /// else (the caller falls through to its own single-key-dict decode),
    /// or throws for a string this build doesn't recognize as any case.
    static func decodeBareStringCase(_ decoder: Decoder, typeName: String,
                                      cases: [String: Self]) throws -> Self? {
        guard let s = try? decoder.singleValueContainer().decode(String.self) else { return nil }
        if let match = cases[s] { return match }
        throw DecodingError.dataCorruptedError(in: try decoder.singleValueContainer(),
            debugDescription: "unknown \(typeName) variant: \(s)")
    }

    static func missingVariantKeyError(_ decoder: Decoder, typeName: String) throws -> DecodingError {
        DecodingError.dataCorruptedError(in: try decoder.singleValueContainer(),
            debugDescription: "missing \(typeName) variant key")
    }
}

/// `HandleCheckOutcome` is decoded from either a bare string (for unit
/// variants `None`/`FormatInvalid`/`TldInvalid`/`RegisteredNoNest`/
/// `NestRunningUserUnregistered`) OR a single-key dict for the variants
/// with associated values (`{"DomainAvailable": {...}}` etc.).
public enum HandleCheckOutcomePayload: RustTaggedEnumPayload {
    case none
    case formatInvalid
    case tldInvalid
    case domainAvailable(buyableViaProvider: Bool, price: TldPriceQuotePayload?)
    case registeredNoNest
    case alreadyOnNest(handleMatches: Bool, currentHandle: String?)
    case nestRunningUserUnregistered
    case probeError(phase: String, transient: Bool, cause: String)

    public init(from decoder: Decoder) throws {
        if let bare = try Self.decodeBareStringCase(decoder, typeName: "HandleCheckOutcome", cases: [
            "None": .none, "FormatInvalid": .formatInvalid, "TldInvalid": .tldInvalid,
            "RegisteredNoNest": .registeredNoNest,
            "NestRunningUserUnregistered": .nestRunningUserUnregistered,
        ]) {
            self = bare
            return
        }
        struct Args: Decodable {
            let DomainAvailable: DomainAvailableArgs?
            let AlreadyOnNest: AlreadyOnNestArgs?
            let ProbeError: ProbeErrorArgs?
        }
        struct DomainAvailableArgs: Decodable {
            let buyable_via_provider: Bool
            let price: TldPriceQuotePayload?
        }
        struct AlreadyOnNestArgs: Decodable {
            let handle_matches: Bool
            let current_handle: String?
        }
        struct ProbeErrorArgs: Decodable {
            let phase: String
            let transient: Bool
            let cause: String
        }
        let a = try Args(from: decoder)
        if let v = a.DomainAvailable {
            self = .domainAvailable(buyableViaProvider: v.buyable_via_provider, price: v.price)
        } else if let v = a.AlreadyOnNest {
            self = .alreadyOnNest(handleMatches: v.handle_matches, currentHandle: v.current_handle)
        } else if let v = a.ProbeError {
            self = .probeError(phase: v.phase, transient: v.transient, cause: v.cause)
        } else {
            throw try Self.missingVariantKeyError(decoder, typeName: "HandleCheckOutcome")
        }
    }

    public func intoOutcome() -> HandleCheckOutcome {
        switch self {
        case .none: return .none
        case .formatInvalid: return .formatInvalid
        case .tldInvalid: return .tldInvalid
        case .domainAvailable(let buyable, let price):
            return .domainAvailable(buyableViaProvider: buyable, price: price?.intoQuote())
        case .registeredNoNest: return .registeredNoNest
        case .alreadyOnNest(let matches, let cur):
            return .alreadyOnNest(handleMatches: matches, currentHandle: cur)
        case .nestRunningUserUnregistered: return .nestRunningUserUnregistered
        case .probeError(let phase, let transient, let cause):
            return .probeError(phase: parsePhase(phase), transient: transient, cause: cause)
        }
    }
}

public struct TldPriceQuotePayload: Decodable {
    public let registration_cents: UInt64
    public let renewal_cents: UInt64?
    public let currency: String
    public let tld: String

    public func intoQuote() -> TldPriceQuote {
        TldPriceQuote(
            tld: tld,
            registrationCents: registration_cents,
            renewalCents: renewal_cents ?? 0,
            currency: currency
        )
    }
}

/// `InviteRequestState` mirrors the Rust serde shape: bare strings for
/// unit variants; single-key dicts for variants with fields.
public enum InviteRequestStatePayload: RustTaggedEnumPayload {
    case idle
    case submitting
    case rechecking
    // ⚠ There is deliberately no `approved` case (retired 2026-08-12 with
    // `InviteRequestState::Approved`). An injected-`Approved` payload is exactly
    // what kept that variant looking reachable while production could never
    // serve it — do not reintroduce one.
    case denied(reason: String, requestId: String)
    case pendingReview(requestId: String, lastCheckedMs: UInt64)
    case error(transient: Bool, context: String, cause: String)

    public init(from decoder: Decoder) throws {
        if let bare = try Self.decodeBareStringCase(decoder, typeName: "InviteRequestState", cases: [
            "Idle": .idle, "Submitting": .submitting, "Rechecking": .rechecking,
        ]) {
            self = bare
            return
        }
        struct Args: Decodable {
            let Denied: DeniedArgs?
            let PendingReview: PendingReviewArgs?
            let Error: ErrorArgs?
        }
        struct DeniedArgs: Decodable {
            let reason: String
            let request_id: String
        }
        struct PendingReviewArgs: Decodable {
            let request_id: String
            let last_checked_ms: UInt64
        }
        struct ErrorArgs: Decodable {
            let transient: Bool
            let context: String
            let cause: String
        }
        let a = try Args(from: decoder)
        if let v = a.Denied {
            self = .denied(reason: v.reason, requestId: v.request_id)
        } else if let v = a.PendingReview {
            self = .pendingReview(requestId: v.request_id, lastCheckedMs: v.last_checked_ms)
        } else if let v = a.Error {
            self = .error(transient: v.transient, context: v.context, cause: v.cause)
        } else {
            throw try Self.missingVariantKeyError(decoder, typeName: "InviteRequestState")
        }
    }

    public func intoState() -> InviteRequestState {
        switch self {
        case .idle: return .idle
        case .submitting: return .submitting
        case .rechecking: return .rechecking
        case .denied(let r, let id): return .denied(reason: r, requestId: id)
        case .pendingReview(let id, let ms):
            return .pendingReview(requestId: id, lastCheckedMs: ms)
        case .error(let t, let ctx, let c):
            return .error(transient: t, context: parseErrorContext(ctx), cause: c)
        }
    }
}

public enum OobCodeStatePayload: RustTaggedEnumPayload {
    case idle
    case verifying
    case valid(inviteId: String, supervisedBy: String?)
    case invalid(reason: String)
    case error(cause: String)

    public init(from decoder: Decoder) throws {
        if let bare = try Self.decodeBareStringCase(decoder, typeName: "OobCodeState", cases: [
            "Idle": .idle, "Verifying": .verifying,
        ]) {
            self = bare
            return
        }
        struct Args: Decodable {
            let Valid: ValidArgs?
            let Invalid: InvalidArgs?
            let Error: ErrorArgs?
        }
        // `supervised_by` = the guardian's handle when the code carries a supervised
        // designation; absent for an ordinary code (`family-safety.md` § Wire & data
        // shape). Threaded so a snapshot can drive `invite-code-supervised-notice`.
        struct ValidArgs: Decodable { let invite_id: String; let supervised_by: String? }
        struct InvalidArgs: Decodable { let reason: String }
        struct ErrorArgs: Decodable { let cause: String }
        let a = try Args(from: decoder)
        if let v = a.Valid { self = .valid(inviteId: v.invite_id, supervisedBy: v.supervised_by) }
        else if let v = a.Invalid { self = .invalid(reason: v.reason) }
        else if let v = a.Error { self = .error(cause: v.cause) }
        else {
            throw try Self.missingVariantKeyError(decoder, typeName: "OobCodeState")
        }
    }

    public func intoState() -> OobCodeState {
        switch self {
        case .idle: return .idle
        case .verifying: return .verifying
        case .valid(let id, let supervisedBy): return .valid(inviteId: id, supervisedBy: supervisedBy)
        case .invalid(let r): return .invalid(reason: r)
        case .error(let c): return .error(cause: c)
        }
    }
}

private func parsePhase(_ s: String) -> HandleCheckPhase {
    switch s {
    case "Idle": return .idle
    case "Parsing": return .parsing
    case "DnsLookup": return .dnsLookup
    case "NestProbe": return .nestProbe
    case "ChallengeResponse": return .challengeResponse
    case "PriceLookup": return .priceLookup
    case "Complete": return .complete
    default: return .idle
    }
}

private func parseErrorContext(_ s: String) -> ErrorContext {
    switch s {
    case "Submitting": return .submitting
    case "Rechecking": return .rechecking
    case "Redeeming": return .redeeming
    default: return .submitting
    }
}

/// Decodes a `callMachineMethodWithResult` JSON return value into a value
/// embeddable directly in a `serializeState()` dict. The state dict is
/// itself JSON-encoded at the bridge's wire boundary, so stashing the raw
/// JSON *string* as-is would double-encode it — the Python driver would read
/// back a string instead of the expected dict/scalar (the exact class of bug
/// the android fake-cloud entrust flagged for its `JSONTokener` re-parse).
/// Reader results can be a top-level JSON object (`provisioning_snapshot`) or
/// a bare scalar (`provider_base_url`'s quoted string, or `null`), so parsing
/// allows fragments. Shared by macOS/iOS (`FaunaMacApp`/`FaunaApp`).
public func machineMethodResultValue(from json: String) -> Any {
    guard let data = json.data(using: .utf8),
          let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
    else { return json }
    return parsed
}

/// Reference-type stash for the most recent value-returning
/// `call_machine_method` dispatch result, held via `@State` on `FaunaMacApp`/
/// `FaunaApp` (mirrors `conversationsVM`/`feedVM`: a class instance mutated in
/// place, not a plain value reassigned). **This indirection is load-bearing,
/// not stylistic** — `startInProcessAgentIfNeeded()` wires the bridge's
/// `commandHandler`/`stateProvider` closures from inside `init()`, capturing
/// `self` before SwiftUI has installed the "live" `@State` storage for the
/// instance that actually renders (`body`) and gets read again later; per
/// Apple's own guidance, `@State` must never be read or written from `init()`.
/// A bare `@State private var machineMethodResult: Any?` written through that
/// init-time `self` silently no-ops the assignment — every subsequent read
/// (from that closure OR the live instance) sees only the wrapper's initial
/// `nil`, which is exactly the fake-cloud-provisioning regression this class
/// fixes (`set_provider_base_urls` applied correctly in shared Rust, but the
/// bridge's `provider_base_url` read-back always reported `None`). Class
/// *identity* is what @State actually shares across struct copies here, not
/// @State's own live-binding machinery — the same reason mutating
/// `conversationsVM.manager`'s properties (a class instance) works from this
/// same init-time closure while reassigning a plain `@State` value doesn't.
public final class MachineMethodResultBox {
    public var value: Any?
    public init() {}
}
