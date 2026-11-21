import SwiftUI

/// The apple apps' one painter of the shared possibility answer
/// (`docs/goal/ui/README.md` § Offer only what is possible, rule 6).
///
/// Whether an affordance can succeed now is decided once, in shared Rust, and
/// answered in `Possibility` (`libs/fauna-core/src/possibility.rs`, carried
/// over UniFFI whole): `.possible`, `.blocked(reason:)` or `.absent`. A view
/// never re-derives it — no `.disabled(!vm.machine.canContinueX())` beside a
/// separate `if let reason = …BlockedReason()`, which is two answers that can
/// disagree. It hands the one answer to ``SwiftUI/View/faunaPossible(_:)``
/// and, where the reason should read within eyeshot, places a
/// ``FaunaPossibilityReason`` beside the control.
///
/// tui applies the same answer where it assembles a page's elements
/// (`App::page_elements`); SwiftUI has no such list, so the apple shape is one
/// modifier at the control's declaration, exactly like ``SwiftUI/View/faunaGate(_:)``
/// — which is itself this modifier fed by `offlinePossibility(kind:connectionState:)`.
public extension Possibility {
    /// Offer it enabled — only `.possible`.
    var isPossible: Bool {
        if case .possible = self { return true }
        return false
    }

    /// Paint it at all — everything but `.absent`.
    var isShown: Bool {
        if case .absent = self { return false }
        return true
    }

    /// The reason a blocked affordance gives, rendered through the app's i18n
    /// runtime; `nil` when possible or absent (nothing to explain), and when
    /// the reason renders empty (a blank caption is no reason).
    var reasonText: String? {
        guard case .blocked(let reason) = self else { return nil }
        let text = renderLocalizedText(reason)
        return text.isEmpty ? nil : text
    }

    /// The gesture's refusal, asked again when the submit fires (rule 4's form
    /// case, rule 7: the action asks what the paint asked, and a refusal is
    /// never silent): `nil` when it may go ahead, else the text for the page's
    /// `error-message` — the blocked answer's own reason, or `""` for an
    /// absent one (the surface should not have been on screen; nothing to
    /// add). The twin of web's `formRefusal` and tui's `Errors::refuse`.
    var refusal: String? {
        switch self {
        case .possible: return nil
        case .blocked(let reason): return renderLocalizedText(reason)
        case .absent: return ""
        }
    }

    /// Both preconditions at once, with `Possibility::and`'s meaning: absent
    /// wins, then the receiver's own reason (pass the page's own, more
    /// specific answer as the receiver), then the other's.
    func and(_ other: Possibility) -> Possibility {
        switch (self, other) {
        case (.absent, _), (_, .absent): return .absent
        case (.blocked, _): return self
        case (.possible, _): return other
        }
    }

    /// The same answer with a blocked reason's ARGUMENTS put through the i18n
    /// lookup first — ``renderLocalizedTextNested(_:)``'s rule, applied once
    /// where the answer is made so every painter (``reasonText``,
    /// ``refusal``, ``SwiftUI/View/faunaPossible(_:)``) renders it alike. For
    /// an answer whose reason carries a key as an argument: the features
    /// gate's restriction (`{window}` = `features.window_day`). A reason with
    /// no key argument renders the same either way.
    func resolvingNestedArgs() -> Possibility {
        guard case .blocked(let reason) = self else { return self }
        return .blocked(reason: LocalizedText(key: reason.key, args: reason.args.mapValues { L.lookup($0) }))
    }
}

/// Whether `feature`'s own affordance (the zap-signer designate, the claim
/// redeem) can succeed now: the shared `features_gate_possibility` over the
/// features courtesy read (`docs/goal/ui/README.md` § Offer only what is
/// possible, rules 2, 5 and 6; `profile.md` § Errors & edge cases, *Unread
/// state is not possible*). `rows` is `nil` until the read answered (blocked
/// `common.still_loading`); a read that threw is `failed` (blocked
/// `common.load_failed`); a loaded read with no row for `feature`, or a
/// `hidden` one, is absent; `disabled` is blocked with the row's restriction,
/// its nested `{window}` word resolved.
public func featureGateAnswer(rows: [FfiFeatureRow]?, failed: Bool = false, feature: String) -> Possibility {
    if failed && rows == nil { return readPossibility(read: .failed) }
    return featuresGatePossibility(rows: rows, feature: feature).resolvingNestedArgs()
}

public extension View {
    /// Paint this control from the shared answer: enabled, disabled with the
    /// reason riding the control (tooltip on macOS, accessibility hint on both
    /// targets), or left out entirely.
    ///
    /// Apply it **outside** the control's `automation*` modifier, so the
    /// registry reads the same enabled-ness through `\.isEnabled`
    /// (`AutomationRegistry.folding`) and `/element/enabled` cannot disagree
    /// with the paint — no hand-mirrored `isEnabled:` closure:
    /// ```swift
    /// Button(L.common.continue) { go() }
    ///     .automationActivate(Ids.vpsConfigContinueButton) { go() }
    ///     .faunaPossible(vm.machine.vpsContinuePossibility())
    /// FaunaPossibilityReason(vm.machine.vpsContinuePossibility())
    /// ```
    func faunaPossible(_ answer: Possibility) -> some View {
        modifier(FaunaPossibleModifier(answer: answer, isOfflineGate: false))
    }
}

/// Whether the reason in ``EnvironmentValues/faunaPossibleReason`` is the
/// offline gate's (`.faunaGate`) rather than a page's own answer — so a page's
/// reason stands over the offline one whichever way the two painters nest.
private struct FaunaPossibleReasonIsOfflineKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var faunaPossibleReasonIsOffline: Bool {
        get { self[FaunaPossibleReasonIsOfflineKey.self] }
        set { self[FaunaPossibleReasonIsOfflineKey.self] = newValue }
    }
}

struct FaunaPossibleModifier: ViewModifier {
    let answer: Possibility
    /// `true` for the offline gate (`.faunaGate`): its reason yields to a page's
    /// own blocked reason, the more specific one — tui's
    /// `App::element_possibility` folds `declared.and(offline)`, page first
    /// (`Possibility::and`). Without this the nesting order decided: a
    /// `.faunaGate` applied inside a page's `.faunaPossible` painted *needs a
    /// connection to your nest* over the page's *still loading* / *failed to
    /// load* (admin.md § Errors & edge cases, *Controls wait for their read*).
    let isOfflineGate: Bool
    /// The reason an ENCLOSING painter published — painters nest (the offline
    /// gate `.faunaGate` paints through this modifier inside a draft answer's
    /// `.faunaPossible`), and SwiftUI's disable is cumulative, so a possible
    /// inner answer must not blank the outer one's reason.
    @Environment(\.faunaPossibleReason) private var inheritedReason
    @Environment(\.faunaPossibleReasonIsOffline) private var inheritedIsOffline

    /// The reason this control carries, and whether it is the offline gate's:
    /// a blocked page answer's own reason; an enclosing page's blocked reason
    /// over this offline gate's; else this answer's own; and under a possible
    /// answer, what an enclosing painter published (`""` under none blocked).
    private var published: (reason: String, isOffline: Bool) {
        let inherited = inheritedReason ?? ""
        guard !answer.isPossible, let own = answer.reasonText else {
            return (inherited, inherited.isEmpty ? false : inheritedIsOffline)
        }
        if isOfflineGate, !inherited.isEmpty, !inheritedIsOffline {
            return (inherited, false)
        }
        return (own, isOfflineGate)
    }

    @ViewBuilder
    func body(content: Content) -> some View {
        if answer.isShown {
            let published = self.published
            // `.disabled` wraps `content`, so an inner `automation*`
            // registration is a descendant and reads the result; SwiftUI's
            // disable is cumulative, so nothing inside can re-enable it.
            content
                .disabled(!answer.isPossible)
                .help(published.reason)
                .accessibilityHint(published.reason)
                // The same reason, for the in-process automation registry: every
                // `automation*` registration inside reads it and publishes it as
                // its `reason` attribute (`_AutomationRegister`), so the e2e
                // driver's `blocked_reason` reads what this modifier painted —
                // `""` while the answer is possible, unless an enclosing painter
                // is blocked, whose reason then stands (the control is disabled
                // by it all the same); a page's reason stands over the offline
                // gate's whichever encloses the other.
                .environment(\.faunaPossibleReason, published.reason)
                .environment(\.faunaPossibleReasonIsOffline, published.isOffline)
        }
    }
}

/// The **visible** reason beside a control painted by
/// ``SwiftUI/View/faunaPossible(_:)`` — un-id'd chrome, rendering nothing
/// unless the answer is blocked, so it is safe to place unconditionally.
///
/// A sibling view rather than part of the modifier for the reason
/// ``FaunaOfflineReason`` gives: wrapping every control to make room for a
/// caption would restructure hierarchies that `.accessibilityElement(children:
/// .contain)` scoping depends on, and the author alone knows whether the reason
/// reads as a statement about one control or a whole section. Hidden from
/// assistive tech because the control's own hint already says it.
public struct FaunaPossibilityReason: View {
    private let answer: Possibility

    public init(_ answer: Possibility) {
        self.answer = answer
    }

    public var body: some View {
        if let reason = answer.reasonText {
            Text(reason)
                .font(.caption)
                .foregroundStyle(.secondary)
                // Read-only text: selectable and copyable (ui/README.md § Text the
                // person reads can be copied).
                .textSelection(.enabled)
                .accessibilityHidden(true)
        }
    }
}

/// The answer a control driven by one shared machine gives
/// (`docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for
/// their read*): the machine's own answer once it exists; before it exists the
/// read has not started (*still loading*), and when it could not be built the
/// page will never read (*failed to load*). The twin of tui's `machine` helper
/// in `admin::action_possibility`; Swift never spells either reason itself.
public func machinePossibility<M>(
    _ machine: M?, buildFailed: Bool = false, _ ask: (M) -> Possibility
) -> Possibility {
    if let machine { return ask(machine) }
    return readPossibility(read: buildFailed ? .failed : .pending)
}

/// Where a page's OWN read stands — a read no shared machine tracks (the
/// admin Nest page's reflective read, the tier list): the Swift twin of
/// `Possibility::once_read` with the failed state kept apart. Pending until
/// the first answer, failed after a refused first read, resolved once one
/// read succeeded — and a re-read that fails after that keeps the earlier
/// answer (the page still shows what it read), exactly as the shared
/// machines' `note_read_failed` does.
public struct PageRead: Equatable {
    public private(set) var state: ReadState = .pending

    public init() {}

    public mutating func resolved() { state = .resolved }

    public mutating func failed() {
        if state != .resolved { state = .failed }
    }

    /// The answer a control waiting on this read gives (rule 5).
    public var possibility: Possibility { readPossibility(read: state) }
}

/// Refuse a press whose answer is not possible, with its reason on the page's
/// `error-message` (`ui/README.md` § Offer only what is possible, rule 7: the
/// press asks what the paint asked, and nothing reaches the nest). `true`
/// when refused.
@MainActor
public func refusePress(_ answer: Possibility, errorMessage: inout String?) -> Bool {
    guard let refusal = answer.refusal else { return false }
    errorMessage = refusal
    return true
}
