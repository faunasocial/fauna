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
        modifier(FaunaPossibleModifier(answer: answer))
    }
}

struct FaunaPossibleModifier: ViewModifier {
    let answer: Possibility

    @ViewBuilder
    func body(content: Content) -> some View {
        if answer.isShown {
            let reason = answer.reasonText ?? ""
            // `.disabled` wraps `content`, so an inner `automation*`
            // registration is a descendant and reads the result; SwiftUI's
            // disable is cumulative, so nothing inside can re-enable it.
            content
                .disabled(!answer.isPossible)
                .help(reason)
                .accessibilityHint(reason)
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
                .accessibilityHidden(true)
        }
    }
}
