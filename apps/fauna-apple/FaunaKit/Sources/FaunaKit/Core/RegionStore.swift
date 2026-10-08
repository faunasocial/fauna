import StoreKit
import SwiftUI

/// The region content plane on apple (`region-blocking.md` § The content plane
/// → *How an app obtains its region's policy*, *The blocked render and the
/// transparency surface*) — a paint of the shared `FfiRegionPlane`
/// (`libs/fauna-ffi/src/region.rs`), the one face the four UniFFI apps drive.
/// linux's `apps/fauna-linux/src/region.rs` and web's `$lib/region.svelte.ts`
/// are the reference legs.
///
/// Everything that decides — verify, fold, persist, the refresh cadence, the
/// scorer join, the composed verdict, which verbs get a placeholder and which
/// language of the reason it shows — is shared Rust behind the plane. What is
/// left here is exactly what the design lets diverge:
///
/// - [`leaf`] — **the platform leaf** (one function): the storefront on a
///   store-distributed build, the OS's user-set region otherwise;
/// - where the device record lives — `AccountStateDir.base`, the
///   install-scoped `<Application Support>/Fauna` (never the `SecretStore`: an
///   envelope may reach 4 MiB);
/// - when the relay is asked — login, every reconnect, and a one-minute
///   `refreshIfDue` tick on the shared cadence.
///
/// App-scoped (held by `AppState`/`MacAppState` beside `ContentPolicyStore`)
/// and kept across sign-out and identity switch: a region is a fact about the
/// device, not the account. Not actor-isolated, for the same reason as
/// `ContentPolicyStore` (both shells construct it in a stored property).
@Observable
public final class RegionStore {
    /// Bumped whenever the plane's answer may have changed (a relay reply
    /// folded) — every surface that composed a verdict or painted the settings
    /// section reads it, so SwiftUI repaints them.
    public private(set) var revision = 0

    @ObservationIgnored private var plane: FfiRegionPlane?
    @ObservationIgnored private var api: APIClient?
    @ObservationIgnored private var tick: Task<Void, Never>?

    /// The one-minute tick; the shared cadence
    /// (`REFRESH_INTERVAL_SECS`, decided in Rust) says whether a tick asks.
    static let tickIntervalSeconds: UInt64 = 60

    /// The app's UI language — the authority's reason is shown in it where the
    /// authority wrote one (the app's own strings are English today; linux's
    /// `UI_LANG`).
    static let uiLang = "en"

    public init() {}

    /// Which leaf this build reads (`region-blocking.md` § Region determination).
    enum Leaf: Equatable {
        /// A self-built or sideloaded build: the OS's user-set region (System
        /// Settings → Language & Region), known at once.
        case systemRegion(code: String?)
        /// A store-distributed build: the storefront, which StoreKit answers
        /// asynchronously — the plane stands on its recorded declaration until
        /// it does.
        case storefrontPending
    }

    /// **The platform leaf** — a store-distributed build reads its storefront,
    /// a self-built or sideloaded one the OS's user-set region. No network is
    /// consulted to decide which, and there is no in-app override. The code is
    /// handed over verbatim and shared Rust decides whether it is a region code;
    /// one the registry enrols nobody for (a UN M.49 area such as `001`, which
    /// `RegionCode::parse` accepts) simply binds no policy.
    static func leaf(storeDistributed: Bool, osRegion: String?) -> Leaf {
        storeDistributed ? .storefrontPending : .systemRegion(code: osRegion)
    }

    /// Whether the App Store (or TestFlight) installed this build: its receipt
    /// is present. A local file check, never a StoreKit transaction call, which
    /// could reach the store — and prompt a sideloaded user to sign in — when
    /// nothing is cached. macOS reads the documented `_MASReceipt` path
    /// (`appStoreReceiptURL` is deprecated at this target); iOS the bundle's
    /// receipt URL.
    static var isStoreDistributed: Bool {
        #if os(macOS)
        let receipt: URL? = Bundle.main.bundleURL.appendingPathComponent("Contents/_MASReceipt/receipt")
        #else
        let receipt = Bundle.main.appStoreReceiptURL
        #endif
        guard let receipt else { return false }
        return FileManager.default.fileExists(atPath: receipt.path)
    }

    /// Set while a store build's storefront has not answered yet.
    @ObservationIgnored private var storefrontPending = false

    /// Open the plane on first use — ahead of the first fetch — restoring the
    /// device record. In a test-capable build the shared e2e override replaces
    /// the leaf's code (keeping its source), so this needs no test seam of its own.
    private func openedPlane() -> FfiRegionPlane {
        if let plane { return plane }
        let configDir = AccountStateDir.base.path
        let opened: FfiRegionPlane
        switch Self.leaf(storeDistributed: Self.isStoreDistributed, osRegion: Locale.current.region?.identifier) {
        case .systemRegion(let code):
            opened = FfiRegionPlane.open(declaredCode: code, source: .systemRegion, configDir: configDir)
        case .storefrontPending:
            opened = FfiRegionPlane.openPending(configDir: configDir)
            storefrontPending = true
        }
        plane = opened
        return opened
    }

    /// A store build's storefront, once — ISO 3166-1 alpha-3, converted to the
    /// region in shared Rust. `nil` (no storefront) declares nothing.
    @MainActor
    private func resolveStorefrontIfPending(_ plane: FfiRegionPlane) async {
        guard storefrontPending else { return }
        storefrontPending = false
        let alpha3 = await Storefront.current?.countryCode
        if plane.redeclareStorefront(storefrontAlpha3: alpha3) {
            revision += 1
        }
    }

    /// Ask the relay at login / reconnect, and arm the minute tick. A nil api
    /// (the teardown phase of a sign-out or account switch) forgets the refresh
    /// clock, so the next login asks at once; the plane itself stays.
    @MainActor
    public func refresh(api: APIClient?) async {
        let plane = openedPlane()
        await resolveStorefrontIfPending(plane)
        guard let api else {
            self.api = nil
            plane.clearSession()
            return
        }
        self.api = api
        startTick()
        if await api.refreshRegionPlane(plane, onlyIfDue: false) {
            revision += 1
        }
    }

    @MainActor
    private func startTick() {
        guard tick == nil else { return }
        tick = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: Self.tickIntervalSeconds * 1_000_000_000)
                if Task.isCancelled { return }
                await self?.tickOnce()
            }
        }
    }

    @MainActor
    private func tickOnce() async {
        guard let api, let plane else { return }
        if await api.refreshRegionPlane(plane, onlyIfDue: true) {
            revision += 1
        }
    }

    /// One item's render decision with the region composed in — the region,
    /// the guardian floor and the viewer's own thresholds, strictest-wins in
    /// shared Rust.
    @MainActor
    func render(labels: [ContentLabelEntry], inputs: ContentPolicyInputs, subject: RegionSubject) -> RegionRenderDecision {
        _ = revision
        let r = openedPlane().render(
            labels: labels,
            contentPolicy: inputs.contentPolicy,
            ownSpamPermille: inputs.ownSpamPermille,
            ownPhishingPermille: inputs.ownPhishingPermille,
            contentIdHex: subject.contentIdHex,
            authorHex: subject.authorHex,
            text: subject.text,
            hashtags: subject.hashtags,
            hasMedia: subject.hasMedia,
            lang: Self.uiLang)
        return RegionRenderDecision(verdict: r.verdict, placeholder: r.placeholder)
    }

    /// What the settings region section paints.
    @MainActor
    public func view() -> FfiRegionView {
        _ = revision
        return openedPlane().view()
    }
}

/// The scorer input a bundled region scorer reads for one item — the item's id,
/// author, text, hashtags and whether it carries media (`FfiRegionPlane.render`).
public struct RegionSubject {
    public var contentIdHex: String?
    public var authorHex: String?
    public var text: String
    public var hashtags: [String]
    public var hasMedia: Bool

    /// A feed post (card or detail) — linux's `region::post_input`.
    public static func post(_ post: PostSummary) -> RegionSubject {
        RegionSubject(
            contentIdHex: post.postId, authorHex: post.author, text: post.body,
            hashtags: post.tags, hasMedia: post.hasMedia)
    }

    /// A conversation bubble, post-decrypt — the zero id stands in for the
    /// author, as on tui and linux.
    public static func message(id: String, text: String) -> RegionSubject {
        RegionSubject(contentIdHex: id, authorHex: nil, text: text, hashtags: [], hasMedia: false)
    }
}

/// One item's render decision: the composed verdict, and the region
/// placeholder to paint AHEAD of the family arm when the region drove a
/// `block` or `collapse` (`nil` → the app's existing arms).
public struct RegionRenderDecision {
    public let verdict: String
    public let placeholder: FfiRegionPlaceholder?
    /// The viewer's own report hid this item — paint "You reported this"
    /// (`moderation.report.hidden_placeholder`) in place of the body.
    public var reported: Bool = false

    /// The region placeholder this item paints now: a `block`, or a `collapse`
    /// not yet revealed (the reveal shares the family reveal state).
    public func withheld(revealed: Bool) -> FfiRegionPlaceholder? {
        guard let placeholder else { return nil }
        return placeholder.verb == "block" || !revealed ? placeholder : nil
    }

    /// Whether the region blocks this item — convention 17's verdict side.
    public var isRegionBlocked: Bool { placeholder?.verb == "block" }
}

/// Convention 17's "a region Block never renders silent" state field
/// (`tests/e2e-unified/helpers/frame_invariants.py`, published as
/// `region_block_render`) — tui's/linux's `region::block_render_json`.
/// `blocked` is the set of on-screen items whose composed verdict the region
/// blocks, registered by each surface's item container from the verdict it
/// computed off its snapshot (independent of which arm painted);
/// `placeholders` is the set of block placeholders actually painted. An arm
/// that drops the placeholder shows up as `placeholders < blocked`.
@MainActor
public enum RegionBlockRender {
    static var blocked: Set<String> = []
    static var painted: Set<String> = []

    public static var stateFragment: [String: Any] {
        ["blocked": blocked.count, "placeholders": painted.count]
    }
}

extension View {
    /// Register this item container's verdict-side walk for convention 17 while
    /// it is on screen: `key` names the surface + item, `blocked` is whether its
    /// composed verdict is a region block. No-op in production.
    func regionBlockWitness(_ key: String, blocked: Bool) -> some View {
        #if DEBUG
        self
            .onAppear { if blocked { RegionBlockRender.blocked.insert(key) } }
            .onDisappear { RegionBlockRender.blocked.remove(key) }
            .onChange(of: blocked) { _, now in
                if now { RegionBlockRender.blocked.insert(key) } else { RegionBlockRender.blocked.remove(key) }
            }
        #else
        self
        #endif
    }
}
