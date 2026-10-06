import Foundation

/// The immutable snapshot of the two inputs the shared content-policy render
/// engine composes (`family-safety.md` § Content policy, Slice C):
///
/// 1. the guardian's per-category floor (the `fauna.family.status` reply's
///    supervision fold, `FfiFamilyStatus.supervision`; `nil` unless
///    supervised), and
/// 2. the viewer's OWN spam/phishing per-mille thresholds
///    (`fauna.spam.get_preferences`) — the every-user un-darking of
///    `moderation.md` § Categories & enforcement item 1.
///
/// A value type, not the store, so a SwiftUI view can hold it as a plain `let`
/// and a unit test can drive [`verdictFor`] with fixed inputs — no store, no
/// live connection (android's `ContentPolicyInputs` split, for the same reason).
public struct ContentPolicyInputs: Equatable {
    public var contentPolicy: FfiContentPolicy?
    public var ownSpamPermille: UInt16?
    public var ownPhishingPermille: UInt16?
    /// The guardian's **Guardian Notify** knob (`family-safety.md` § Guardian
    /// Notify) — whether the ward's client should count guardian-floor
    /// render-enforcement events. `false` (the wire default) until a
    /// supervised viewer's policy read lands; always `false` for an
    /// unsupervised viewer (`contentPolicy == nil`).
    public var contentNotify: Bool

    public init(
        contentPolicy: FfiContentPolicy? = nil,
        ownSpamPermille: UInt16? = nil,
        ownPhishingPermille: UInt16? = nil,
        contentNotify: Bool = false
    ) {
        self.contentPolicy = contentPolicy
        self.ownSpamPermille = ownSpamPermille
        self.ownPhishingPermille = ownPhishingPermille
        self.contentNotify = contentNotify
    }

    /// The client render verdict for one item's `labels` — one of
    /// `"show" | "badge" | "collapse" | "block"`, resolved by the shared
    /// `contentRenderVerdict` (the strictest-wins compose of the guardian floor
    /// and the viewer's own thresholds, **entirely in Rust** — priority #2: no
    /// rule assembly lives in Swift, exactly as linux keeps it out of the shell
    /// and web keeps it out of JS).
    ///
    /// With neither half set no rule can fire, so the strictest reachable
    /// verdict is `badge` — which both render surfaces treat like `show`. That
    /// case short-circuits without the FFI call, which keeps a
    /// default-constructed value usable from previews and from view tests that
    /// run without the host library loaded.
    public func verdictFor(_ labels: [ContentLabelEntry]) -> String {
        if contentPolicy == nil && ownSpamPermille == nil && ownPhishingPermille == nil {
            return "show"
        }
        return contentRenderVerdict(
            labels: labels,
            contentPolicy: contentPolicy,
            ownSpamPermille: ownSpamPermille,
            ownPhishingPermille: ownPhishingPermille)
    }

    /// The render decision for one item, plus the **Guardian Notify** record —
    /// every guardian-floor render-enforcement on `itemId` is counted, deduped
    /// per item per local day, so a re-render never re-counts. Guardian Notify
    /// is a lens on the GUARDIAN's floor alone — `GuardianNotifyCadence.record`
    /// never sees `ownSpamPermille`/`ownPhishingPermille`, so a viewer's
    /// own-threshold collapse is never billed to the guardian's readout.
    ///
    /// The verdict composes the **region content policy** as the third
    /// strictest-wins source (`region-blocking.md` § Where it composes). This is
    /// the one door every apple render surface calls (macOS + iOS feed
    /// post-cards, both post details, the conversation bubble), so the
    /// record-then-verdict sequence can't drift between them and none reaches
    /// the composed call for one source while bypassing it for another. The
    /// decision's `placeholder` is the region arm, painted AHEAD of the family
    /// arm. A nil `region` (a preview, a view test) falls back to the
    /// two-source `verdictFor`.
    @MainActor
    public func recordedRender(
        itemId: String, labels: [ContentLabelEntry], region: RegionStore?, subject: RegionSubject
    ) -> RegionRenderDecision {
        GuardianNotifyCadence.shared.record(itemId: itemId, labels: labels, contentPolicy: contentPolicy)
        if let region {
            return region.render(labels: labels, inputs: self, subject: subject)
        }
        return RegionRenderDecision(verdict: verdictFor(labels), placeholder: nil)
    }
}

/// App-scoped cache of the render-engine inputs, shared by **both** social
/// surfaces (feed post-card + conversation bubble) so they can never drift on
/// how a content floor is enforced — the apple twin of linux's
/// `crate::content_policy` module, web's `contentPolicy.svelte.ts` and android's
/// `@Singleton ContentPolicyStore` (priority #2/#3, one concept everywhere).
///
/// Held by each shell's app state beside `FamilyStatusStore` and injected into
/// the SwiftUI environment at the app root, so one cache serves every surface
/// rather than each view re-reading. Refreshed on the same two triggers as the
/// family gates: post-auth and on WS reconnect.
///
/// **Keep-on-failure, not fail-closed-to-empty.** A failed read leaves each
/// half exactly as it was — "read failed" and "read succeeded and reports
/// unsupervised" are different facts, and only the second may clear a loaded
/// half (family-safety.md § Content policy, the unfetched-policy ruling's
/// clause 1; linux/android/tui/web are the reference legs). The safe direction
/// is still to *under*-enforce a viewer's own collapse when a half was never
/// loaded at all — never to hide content the nest never asked us to hide — but
/// once a guardian floor IS loaded, an airplane-mode blip must not lift it.
/// (Contrast the *knob-parsing* fail-closed rule, which is strict-by-default
/// and lives in shared Rust: an unparseable floor renders `block`.)
///
/// Not actor-isolated, for the same reason as the sibling `FamilyStatusStore`:
/// both `AppState` and `MacAppState` construct it in a stored property, which a
/// `@MainActor` initializer could not serve. Mutation is pinned to the main
/// actor in `refresh` instead.

/// One `refresh` half's read outcome — `.failed` on any error (network, FFI,
/// …), `.succeeded` with the fetched value (itself possibly nil, e.g. an
/// unsupervised viewer's floor) otherwise. Kept apart from `refresh` so the
/// keep-on-failure merge below is unit-testable without a live `APIClient`.
enum ContentPolicyReadResult<T> {
    case failed
    case succeeded(T)
}

@Observable
public final class ContentPolicyStore {
    public private(set) var inputs = ContentPolicyInputs()

    public init() {}

    /// Restore (or clear) the guardian half from the persisted last-known
    /// supervision snapshot, ahead of the first `refresh` landing
    /// (`family-safety.md` § Content policy, clause 2 — "loaded at launch
    /// ahead of the first read"). The viewer's OWN thresholds are untouched —
    /// they are deliberately not in the snapshot and re-arrive with their own
    /// read. `nil` clears the guardian half to unsupervised, which is what
    /// makes this safe to call at every session establish, including the
    /// bare `client == nil` teardown phase: a departing account's floor can
    /// never bleed into the next one's first paint (android's construction-
    /// time seed is the reference shape; apple's stores are app-scoped rather
    /// than session-scoped, so the seed lives at session establish instead —
    /// see `seedSupervisionSnapshot`).
    @MainActor
    func seed(from snapshot: FfiSupervisionSnapshot?) {
        let half = Self.guardianHalf(of: snapshot)
        inputs.contentPolicy = half.policy
        inputs.contentNotify = half.notify
        GuardianNotifyCadence.shared.setEnabled(inputs.contentNotify)
    }

    /// The guardian half a supervision fold carries — the floor and the
    /// Guardian Notify knob, `(nil, false)` for no fold. Both doors onto that
    /// half route through it: `seed` (a cold-launch restore) and a successful
    /// `refresh` (a live read), so the two cannot disagree on what the store is
    /// fed — the contract `FfiFamilyStatus.supervision` exists to keep.
    static func guardianHalf(of snapshot: FfiSupervisionSnapshot?) -> (policy: FfiContentPolicy?, notify: Bool) {
        (snapshot?.contentPolicy, snapshot?.contentNotify ?? false)
    }

    /// The guardian half a SUCCESSFUL `fauna.family.status` read moves: the
    /// reply's supervision fold, never its raw `policy`. The shared
    /// `SupervisionSnapshot::from_status` gates every supervised field on a
    /// named guardian (the graduation gate), so a reply that still carries a
    /// policy document but names no guardian binds no floor and turns no
    /// Notify counting on (`family-client-enforcement.md` § Implementation
    /// status today). `internal` so a test can pin it without a live
    /// `APIClient`.
    static func guardianHalf(of status: FfiFamilyStatus) -> (policy: FfiContentPolicy?, notify: Bool) {
        guardianHalf(of: status.supervision)
    }

    @MainActor
    public func refresh(api: APIClient?) async {
        // A nil api (the authenticated client not yet wired, or dropped) keeps
        // the last-known inputs in force rather than resetting — an airplane-
        // mode/reconnect blip must never lift a loaded guardian floor (clause 1;
        // linux/android/tui/web are the reference legs — family-safety.md § Content
        // policy).
        guard let api else { return }

        // `contentPolicy` + `contentNotify` come off the SAME `status` read, so a
        // failed read must leave BOTH at their previous values together — not
        // just the floor while Notify silently reverts to off (or vice versa).
        let policyResult: ContentPolicyReadResult<(policy: FfiContentPolicy?, notify: Bool)>
        if let status = try? await api.familyStatus() {
            policyResult = .succeeded(Self.guardianHalf(of: status))
        } else {
            policyResult = .failed
        }

        // The viewer's own thresholds, in the wire's per-mille `u16` — what the
        // shared engine consumes. Deliberately NOT via `getSpamPreferences()`,
        // whose 0.0–1.0 slider presentation would round-trip the value through a
        // Double for no reason.
        let ownResult: ContentPolicyReadResult<(spam: UInt16, phishing: UInt16)>
        if let own = try? await api.spamThresholdsPerMille() {
            ownResult = .succeeded(own)
        } else {
            ownResult = .failed
        }

        inputs = Self.merged(previous: inputs, policy: policyResult, own: ownResult)

        // Guardian Notify's ward-side counting (family-safety.md § Guardian
        // Notify) — the SAME post-auth + reconnect trigger as this refresh,
        // matching linux/android/windows: `set_ward_content_notify` fires
        // whenever the policy is (re)read, not lazily at the next render, so a
        // guardian turning Notify off drops any pending count promptly rather
        // than at the next flush tick.
        GuardianNotifyCadence.shared.setEnabled(inputs.contentNotify)
    }

    /// The keep-on-failure fold itself: only a SUCCESSFUL read may move a half
    /// (which may still clear it — a successful read reporting "unsupervised" is
    /// the one outcome that legitimately does); a failed read leaves that half
    /// exactly as it was. `internal`, not `private`, so `@testable import
    /// FaunaKit` can drive it directly without an `APIClient`.
    static func merged(
        previous: ContentPolicyInputs,
        policy: ContentPolicyReadResult<(policy: FfiContentPolicy?, notify: Bool)>,
        own: ContentPolicyReadResult<(spam: UInt16, phishing: UInt16)>
    ) -> ContentPolicyInputs {
        let contentPolicy: FfiContentPolicy?
        let contentNotify: Bool
        switch policy {
        case .succeeded(let value): (contentPolicy, contentNotify) = (value.policy, value.notify)
        case .failed: (contentPolicy, contentNotify) = (previous.contentPolicy, previous.contentNotify)
        }
        let ownSpamPermille: UInt16?
        let ownPhishingPermille: UInt16?
        switch own {
        case .succeeded(let value): (ownSpamPermille, ownPhishingPermille) = (value.spam, value.phishing)
        case .failed: (ownSpamPermille, ownPhishingPermille) = (previous.ownSpamPermille, previous.ownPhishingPermille)
        }
        return ContentPolicyInputs(
            contentPolicy: contentPolicy,
            ownSpamPermille: ownSpamPermille,
            ownPhishingPermille: ownPhishingPermille,
            contentNotify: contentNotify)
    }
}
