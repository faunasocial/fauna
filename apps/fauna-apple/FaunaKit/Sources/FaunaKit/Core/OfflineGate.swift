import SwiftUI

/// W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing for macOS + iOS — the apple leg of the
/// offline-mutation contract (`docs/goal/architecture/account-data-plane.md`
/// § The offline-mutation contract → *How a surface asks*).
///
/// A control that issues a wire kind declares it with ``SwiftUI/View/faunaGate(_:)``
/// and gets three things at once, from **one** verdict: it desensitizes when the
/// mutation cannot happen without a nest, it says why *beside itself*, and the
/// automation surface reports the same enabled-ness the user sees.
///
/// ## SwiftUI is a third seam shape, and this is why
///
/// The other two shapes exist because of what their toolkits are. **tui** rebuilds
/// its element list every frame, so it gates inside the one function that returns
/// that list (`App::page_elements`) and a page author writes no gate code.
/// **linux** registers per widget, because a GTK widget outlives the state that
/// gated it, so something must re-visit it on each transition
/// (`apps/fauna-linux/src/offline_gate.rs`).
///
/// SwiftUI is neither: views are values re-evaluated from state, so nothing needs
/// re-visiting (linux's problem does not exist) but there is also no single list
/// to gate (tui's seam does not exist). What SwiftUI *does* have, and neither
/// other toolkit does, is a **propagating environment**: `.disabled(...)` is
/// cumulative down the view tree and cannot be re-enabled by a descendant. So the
/// apple shape is one modifier at the point of declaration — the same place the
/// control already stamps its `accessibilityIdentifier`/`automation*` id — and the
/// propagation SwiftUI already performs does the rest.
///
/// **Never write `if !available` at a call site.** Scattering the check is the
/// outcome this seam exists to prevent: it splits paint, the automation surface
/// and the focus ring into three answers that drift. One modifier keeps them one
/// answer by construction.
///
/// ## The rule is not reimplemented here
///
/// The verdict comes from `offlineAffordance(kind:connectionState:)`, the UniFFI
/// face of `fauna_protocol::offline_class::affordance`. Its three rulings — only
/// class 3 desensitizes, an unregistered kind stays available, only *known*
/// offline words count as offline — are deliberately **not** restated in Swift;
/// that per-app copy is exactly what priority #2 forbids, and ruling 3 in
/// particular fails silently in the dangerous direction if a per-app copy drifts.
/// The connection word likewise comes from `connectionStateWord(state:)` rather
/// than a Swift `match`, for the same reason.
///
/// ## The automation surface is told by `.disabled`, not by a key of our own
///
/// This gate used to publish its verdict through a dedicated environment key
/// (`faunaOfflineGateEnabled`) that `_AutomationRegister` folded into the
/// registered `isEnabled`. That key is **gone**, and its removal is the proof of
/// the general fix that replaced it: the registry now reads SwiftUI's own
/// `\.isEnabled`, which is exactly "has any ancestor disabled me" — so
/// `.disabled(!verdict.available)` below *already* tells the automation surface
/// everything the key used to, and every OTHER cause of disablement (a busy-state
/// container, a `Form` section) is answered by the same read instead of being
/// invisible to the driver. See `AutomationRegistry.folding`.
public extension View {
    /// Gate this control on the wire `kind` it issues.
    ///
    /// Disables it when the shared rule says the mutation needs a nest, renders
    /// the localized reason beside it (never a banner — § R11 forbids a global
    /// "you are offline"), and publishes the same verdict to the automation
    /// registry so `/element/enabled` cannot disagree with the paint.
    ///
    /// Apply it **outside** the control's `automation*` modifier, next to where
    /// the id is stamped:
    /// ```swift
    /// Button(L.common.confirm) { confirmCreateInvite() }
    ///     .automationActivate("create-invite-confirm-btn") { confirmCreateInvite() }
    ///     .faunaGate("fauna.admin.invite_codes.create")
    /// ```
    /// The kind is checked against the shared table by a dedicated dev-fleet
    /// checker (`offline-gate-check`) — a misspelling reads as *available*
    /// (ruling 2) and would otherwise ungate the control silently.
    /// Order matters only in that the gate must be the ancestor: `.disabled` and
    /// the environment both propagate inward, so an inner automation registration
    /// sees the gate's verdict.
    func faunaGate(_ kind: String) -> some View {
        modifier(FaunaOfflineGate(kind: kind))
    }
}

/// The one place the apple apps ask whether an affordance may be offered.
struct FaunaOfflineGate: ViewModifier {
    let kind: String
    @Environment(FaunaClient.self) private var client: FaunaClient?

    func body(content: Content) -> some View {
        // One call, both halves — the FFI face returns `available` and `reason`
        // together precisely so a caller cannot ask twice and get two answers
        // that disagree (`libs/fauna-ffi/src/offline.rs` module docs).
        //
        // ⚠ **No client in the environment is an UNKNOWN state, never
        // "connecting".** `connecting` is a known offline word, so treating a
        // missing client as one would grey the control permanently wherever the
        // environment does not reach — a SwiftUI presentation that loses it, a
        // preview, a view built before the session is injected — and grey it
        // while the nest is perfectly reachable. Handing the rule a word it does
        // not know is exactly what ruling 3 is written for: an unrecognised
        // state leaves the control live, because for a gate the honest answer to
        // "we do not know" is *do not block the user* (at worst the control
        // shows the error it would have shown anyway). Deliberately NOT spelled
        // as "connected": that would assert a fact we do not have, and it would
        // start passing a word this file chose rather than one the transport
        // reported. A client that IS present and reports `.connecting` still
        // gates — that is a real transport state, not an unknown.
        let state = client.map { connectionStateWord(state: $0.connectionState) } ?? "unknown"
        let verdict = offlineAffordance(kind: kind, connectionState: state)
        let reason = verdict.reason.map(renderLocalizedText)

        // NOTE the modifier order: `.disabled` wraps `content`, so an inner
        // `automation*` registration is a descendant and reads the resulting
        // `\.isEnabled` — which is the WHOLE mechanism now that the dedicated key
        // is gone. `.disabled` is cumulative in SwiftUI and a descendant cannot
        // re-enable, which gives linux's "never *enables* what the page disabled"
        // rule for free rather than by convention.
        return content
            .disabled(!verdict.available)
            // The reason rides the control itself, per affordance — never a
            // global banner (§ R11). `.help` is macOS's twin of the tooltip
            // linux attaches (`offline_gate.rs`: "the reason likewise rides the
            // widget's tooltip"); the accessibility hint carries it on **both**
            // targets, so assistive tech is told why on iOS too.
            //
            // ⚠ These two carry the reason but do not RENDER it: `.help` is a
            // hover tooltip (no hover on iOS) and a hint is only spoken. The
            // visible half is `FaunaOfflineReason` below — a sibling view the
            // author places, deliberately NOT folded in here.
            //
            // This comment used to say a visible iOS reason was owed and
            // approval-gated, "since it would need a new element id in
            // `tests/e2e-unified/ui.yaml`". **That premise was refuted
            // 2026-08-23**: android answered the same question with no element
            // id at all, because the blocker was the reason's *addressability*,
            // not its visibility (`account-data-plane.md` § Implementation
            // status → the android leg). Its second half still stands and is
            // why the caption is a sibling: wrapping every gated control would
            // restructure hierarchies that existing `.accessibilityElement
            // (children: .contain)` scoping depends on.
            .help(reason ?? "")
            .accessibilityHint(reason ?? "")
    }
}

/// The **visible** half of the gate's "say why beside itself" duty — un-id'd
/// chrome naming why an adjacent gated control is dead.
///
/// ## Why this is a sibling VIEW and not part of `.faunaGate(_:)`
///
/// A visible reason was carried as *owed and approval-gated* here from
/// 2026-08-16 to 2026-08-23, on the premise that it would need a new
/// `tests/e2e-unified/ui.yaml` element id. **That premise was wrong**, and
/// android is what refuted it (`account-data-plane.md` § Implementation status
/// → the android leg): the blocker apple had identified was the
/// *addressability* of the reason, not its visibility, and **un-id'd chrome
/// sidesteps it on any app whose harness reads text.** android renders the
/// same thing with no `testTag` at all (`DisabledControlReasonText`), and this
/// app already shipped the idiom by hand — see `WebSettingsView`'s section
/// caption, a plain un-id'd `Text` explaining why the copy buttons below it
/// are dead.
///
/// The row's *second* premise was never refuted, and is why this is a sibling
/// rather than a `VStack` folded into the modifier: wrapping every gated
/// control to make room for a caption would restructure view hierarchies that
/// existing `.accessibilityElement(children: .contain)` scoping depends on.
/// A sibling restructures nothing, and it puts placement where it belongs —
/// with the author, who alone knows whether the reason reads as a statement
/// about one control or about a whole section. `ui/README.md` § Copy
/// comprehensibility rule 5 asks for a reason *within eyeshot*, not one caption
/// per control, and `WebSettingsView` reasons about exactly that choice in
/// place ("placed above the rows so it reads as a statement about the section,
/// not about whichever row is last").
///
/// ## The kind is written twice, and that is checked, not trusted
///
/// This takes the same `kind` its neighbouring `.faunaGate(_:)` takes and
/// resolves it through the **same** `offlineAffordance(kind:connectionState:)`
/// call, so the caption cannot claim one verdict while the paint shows another.
/// Writing the kind twice is a real drift risk, and it is closed the way the
/// first one already was: the same dev-fleet checker (`offline-gate-check`)
/// reads this call as a kind-bearing site too, so a misspelling is a
/// merge-gate error rather than a caption that silently never renders —
/// ruling 2 answers *available* for an unregistered kind, and an absent
/// caption looks exactly like a control that is legitimately live.
///
/// ```swift
/// Button(L.common.confirm) { confirm() }
///     .automationActivate("create-invite-confirm-btn") { confirm() }
///     .faunaGate("fauna.admin.invite_codes.create")
/// FaunaOfflineReason("fauna.admin.invite_codes.create")
/// ```
///
/// Renders **nothing** when the gate is open, so it is safe to place
/// unconditionally: a live control has no reason to give.
public struct FaunaOfflineReason: View {
    private let kind: String
    @Environment(FaunaClient.self) private var client: FaunaClient?

    public init(_ kind: String) {
        self.kind = kind
    }

    /// The caption's whole decision, as a value — `nil` means *render nothing*.
    ///
    /// Split out of `body` on purpose. This app's e2e driver resolves elements
    /// through `AutomationRegistry`, and this caption is deliberately un-id'd
    /// chrome, so **no driver-level test can see it** — android's harness reads
    /// arbitrary text and apple's does not, which is the one place the android
    /// finding does not transfer. That would leave a shipped mechanism with no
    /// test at all, behind a "you have to look at it" excuse — which this
    /// project's authoring rules single out as its most expensive kind of
    /// claim, because every later reader inherits it and stops looking. So the
    /// mile is split: everything except the pixels is a pure
    /// function of (kind, connection word) and is pinned in
    /// `OfflineReasonCaptionTests`, and the human is left only the last inch —
    /// whether the caption *looks* right where it sits.
    static func captionText(kind: String, connectionState: String) -> String? {
        let verdict = offlineAffordance(kind: kind, connectionState: connectionState)
        guard !verdict.available else { return nil }
        guard let reason = verdict.reason.map(renderLocalizedText), !reason.isEmpty else {
            return nil
        }
        return reason
    }

    public var body: some View {
        // Same resolution as `FaunaOfflineGate`, deliberately including the
        // "no client is an UNKNOWN word, never `connecting`" rule — a caption
        // appearing wherever the environment does not reach would be telling
        // the user they are offline while the nest is perfectly reachable,
        // which is the over-claim ruling 3 exists to forbid, made visible.
        let state = client.map { connectionStateWord(state: $0.connectionState) } ?? "unknown"
        if let reason = Self.captionText(kind: kind, connectionState: state) {
            Text(reason)
                .font(.caption2)
                .foregroundStyle(.secondary)
                // Chrome, not an addressable element: no
                // `accessibilityIdentifier` and no `automation*` registration.
                // The gate already carries this same string as the control's
                // `.accessibilityHint`, so assistive tech hears it once, from
                // the control itself — a second announcement here would read
                // the reason twice on every focus.
                .accessibilityHidden(true)
        }
    }
}
