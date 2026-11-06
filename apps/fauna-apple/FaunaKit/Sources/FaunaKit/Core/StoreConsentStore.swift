import Foundation
import os

/// The iOS **store-consent round** (`account-age-band.md` § The account age
/// band → the store-law developer duties bullet, items (3) and (4); § App
/// surface → *Store-consent surfaces*) — the android `StoreConsentStore.kt`
/// twin. At launch and on every return to the foreground, before any page: read
/// whether the store's significant-change duty applies to the person on this
/// device (`AgeRangeService.requiredRegulatoryFeatures`, iOS 26.4+) and the
/// app's `AppStore.ageRatingCode` (iOS 26.2+), hand them with this install's
/// device-local state, today's UTC date and the last PermissionKit answer to
/// the ONE shared call (`storeConsentRound`), write the state it returns back,
/// and publish the lock's message lines.
///
/// **Nothing here decides anything.** Whether the person is covered, whether a
/// change is pending (the ledger head past `consented_through`, or a changed
/// rating — the synthetic `rating_changed` entry), what the lock says and
/// whether it may ask are all the shared round's. This glue is three seams —
/// `StoreConsentSignals` (the store reads), `StoreConsentAsker` (the
/// PermissionKit ask and the adult acknowledgment sheet) and
/// `StoreConsentStateStore` (the device-local record) — so it runs on macOS
/// under `swift test` (the `StoreAgeClaimRound` shape). The real seams are
/// `AppleStoreConsentSignals` / `AppleStoreConsentAsker`, live on iOS only;
/// macOS declares the absence (`apps/macos.md` § Declared absences) and never
/// constructs this store.
///
/// The record is about the person the store knows on this device, never the
/// Fauna account: it is not per-account, and signing out leaves it.
///
/// Not class-level `@MainActor` for the reason `ScreenTimeStore` gives: it is
/// built as a plain stored-property default on `AppState`. Every method is
/// `@MainActor` instead.
@Observable
public final class StoreConsentStore {
    /// The lock's message lines, resolved, or `nil` for "not locked" — the
    /// single decision point the global `store-consent-lock` renders from.
    public private(set) var lockLines: [String]?
    /// Whether the lock carries `store-consent-lock-ask-button` (the round's
    /// `Locked.can_ask`). False whenever the app is not locked.
    public private(set) var canAsk = false
    /// How many asks this process handed to the asker — the e2e observable
    /// for "the ask button sent the ask" (the real ask is a system dialog the
    /// harness cannot drive).
    public private(set) var asksSent = 0

    private var signals: StoreConsentSignals
    private var asker: StoreConsentAsker
    private let stateStore: StoreConsentStateStore
    private let round: (FfiStoreConsentInput) -> FfiStoreConsentRound
    private let today: () -> FfiUtcDate
    /// How long one store read may take before it counts as no answer. The
    /// platform reads carry no deadline of their own — on a device with no App
    /// Store account (the iOS simulator) they never return — and rounds run one
    /// at a time, so one unbounded read would stall every later round: the
    /// foreground re-checks, a PermissionKit answer arriving, the test agent.
    private let readTimeout: Duration
    /// The last PermissionKit answer, consumed by the next round that leaves
    /// the app unlocked; never persisted — an approval is persisted as the
    /// round's `consented_through` / `last_rating`.
    private var askAnswer: FfiAskAnswer?
    /// The pending changes of the current lock — what one ask coalesces.
    private var pending: [FfiSignificantChange] = []
    /// Rounds run one at a time: a foreground re-check, an answer arriving and
    /// a test injection must never interleave between read and write-back.
    private var inFlight: Task<Void, Never>?
    private let log = Logger(subsystem: "social.fauna", category: "store-consent")

    public init(
        signals: StoreConsentSignals = AppleStoreConsentSignals(),
        asker: StoreConsentAsker = AppleStoreConsentAsker(),
        stateStore: StoreConsentStateStore = DefaultsStoreConsentStateStore(),
        round: @escaping (FfiStoreConsentInput) -> FfiStoreConsentRound = { storeConsentRound(input: $0) },
        today: @escaping () -> FfiUtcDate = StoreConsentStore.todayUtc,
        readTimeout: Duration = .seconds(5)
    ) {
        self.signals = signals
        self.asker = asker
        self.stateStore = stateStore
        self.round = round
        self.today = today
        self.readTimeout = readTimeout
    }

    /// Run one round now and publish its verdict. Call at launch and on every
    /// return to the foreground.
    @MainActor
    public func refresh() async {
        let previous = inFlight
        let task = Task { @MainActor in
            await previous?.value
            await self.runRound()
        }
        inFlight = task
        await task.value
    }

    /// `store-consent-lock-ask-button`: send one PermissionKit ask carrying
    /// every pending change's description (several coalesce into one ask) —
    /// the same shared sentence a parent reads. A re-ask after a decline
    /// returns the lock to *waiting*.
    @MainActor
    public func askParent() async {
        guard canAsk, !pending.isEmpty else { return }
        do {
            try await asker.ask(updateDescription: Self.describe(pending))
        } catch {
            log.warning("store-consent ask not sent: \(String(describing: error), privacy: .public)")
            return
        }
        asksSent += 1
        askAnswer = nil
        await refresh()
    }

    /// A PermissionKit answer arrived on this device (`AskCenter.responses`).
    @MainActor
    public func receive(answer: FfiAskAnswer) async {
        askAnswer = answer
        await refresh()
    }

    @MainActor
    private func runRound() async {
        let result = round(FfiStoreConsentInput(
            signal: await readSignal(), stored: stateStore.load(), today: today()))
        switch result.verdict {
        case .locked(let entries, _, let canAsk):
            pending = entries
            self.canAsk = canAsk
        case .adultAcknowledgment(let entries):
            // The round already counts them acknowledged — so the state is
            // written back only once the system sheet was shown.
            await asker.acknowledge(updateDescription: Self.describe(entries))
            fallthrough
        default:
            pending = []
            canAsk = false
            askAnswer = nil
        }
        stateStore.save(result.persist)
        lockLines = result.message?.map(renderLocalizedText)
    }

    /// The Apple arm of the round's input, or `nil` — no signal, never a gate —
    /// when the duty cannot be read (iOS before 26.4, or the read failed).
    @MainActor
    private func readSignal() async -> FfiStoreSignal? {
        let signals = self.signals
        let read: Result<StoreRegulatoryFeatures?, Error>? = await bounded {
            do { return .success(try await signals.regulatoryFeatures()) } catch { return .failure(error) }
        }
        let features: StoreRegulatoryFeatures?
        switch read {
        case .success(let value)?:
            features = value
        case .failure(let error)?:
            log.warning("store regulatory features unavailable, no gate: \(String(describing: error), privacy: .public)")
            return nil
        case nil:
            log.warning("store regulatory features read timed out, no gate")
            return nil
        }
        guard let features else { return nil }
        // A timed-out rating read is `nil` like a failed one: the round keeps
        // the stored value.
        let rating: Int64?? = await bounded { await signals.ageRatingCode() }
        return .apple(
            parentalConsentRequired: features.parentalConsentRequired,
            adultNotificationRequired: features.adultNotificationRequired,
            askAnswer: askAnswer,
            currentRating: rating ?? nil)
    }

    /// `read`'s answer, or `nil` once `readTimeout` passes first. The read runs
    /// unstructured and is abandoned, not awaited, on a timeout — a child task
    /// would hold the round until a read that never returns did.
    @MainActor
    private func bounded<T>(_ read: @escaping () async -> T) async -> T? {
        let timeout = readTimeout
        let settled = Settled()
        return await withCheckedContinuation { (continuation: CheckedContinuation<T?, Never>) in
            Task { @MainActor in
                let value = await read()
                if settled.claim() { continuation.resume(returning: value) }
            }
            Task { @MainActor in
                try? await Task.sleep(for: timeout)
                if settled.claim() { continuation.resume(returning: nil) }
            }
        }
    }

    /// One sentence per change, resolved — what the parent reads in Apple's
    /// dialog, the same keys the lock's message renders.
    static func describe(_ entries: [FfiSignificantChange]) -> String {
        entries.map { L.lookup($0.description) }.joined(separator: "\n")
    }

    public static func todayUtc() -> FfiUtcDate {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let c = calendar.dateComponents([.year, .month, .day], from: Date())
        return FfiUtcDate(year: UInt16(c.year ?? 1970), month: UInt8(c.month ?? 1), day: UInt8(c.day ?? 1))
    }

    // MARK: - Test seam (testing.md convention 15)

    /// Test-only: replace the store reads with fixed answers and the asker with
    /// an inert one (so the ask button never raises a real system dialog), or
    /// with `nil` return to the real seams — then run one round, the agent's
    /// stand-in for a foreground re-check. Driven only by the DEBUG test agent's
    /// `store_consent_inject`.
    @MainActor
    public func injectTestSignals(_ fixed: FixedStoreConsentSignals?) async {
        if let fixed {
            signals = fixed
            asker = InertStoreConsentAsker()
        } else {
            signals = AppleStoreConsentSignals()
            asker = AppleStoreConsentAsker()
        }
        await refresh()
    }

    /// Test-only: forget this install's device-local record (the next round is
    /// a first run again) and every in-memory answer and ask count.
    @MainActor
    public func resetForTest() async {
        stateStore.save(FfiStoreConsentState(consentedThrough: nil, lastRating: nil))
        askAnswer = nil
        asksSent = 0
        await refresh()
    }
}

/// Who resumed `bounded`'s continuation first — main-actor confined.
@MainActor
private final class Settled {
    private var done = false
    func claim() -> Bool {
        if done { return false }
        done = true
        return true
    }
}

/// The two significant-change members of `requiredRegulatoryFeatures`.
public struct StoreRegulatoryFeatures: Equatable, Sendable {
    public let parentalConsentRequired: Bool
    public let adultNotificationRequired: Bool
    public init(parentalConsentRequired: Bool, adultNotificationRequired: Bool) {
        self.parentalConsentRequired = parentalConsentRequired
        self.adultNotificationRequired = adultNotificationRequired
    }
}

/// The store reads the round needs. Neither asks anything of the user.
public protocol StoreConsentSignals {
    /// Whether the significant-change duty applies to the person on this
    /// device, or `nil` when there is nothing to read (iOS before 26.4). `nil`
    /// and a throw never gate.
    func regulatoryFeatures() async throws -> StoreRegulatoryFeatures?
    /// `AppStore.ageRatingCode`, or `nil` (iOS before 26.2, or the read
    /// failed — the round keeps the stored value).
    func ageRatingCode() async -> Int64?
}

/// The two system surfaces the round can raise.
public protocol StoreConsentAsker {
    /// Send the PermissionKit significant-update ask to the store parent. The
    /// answer arrives later, on this device, through `StoreConsentStore.receive`.
    func ask(updateDescription: String) async throws
    /// Show the system adult acknowledgment sheet; returns once shown.
    func acknowledge(updateDescription: String) async
}

/// The device-local `{ consented_through, last_rating }` record, both `nil` on
/// first run.
public protocol StoreConsentStateStore {
    func load() -> FfiStoreConsentState
    func save(_ state: FfiStoreConsentState)
}

/// The record in the app's own `UserDefaults` — device-local, not per-account.
public struct DefaultsStoreConsentStateStore: StoreConsentStateStore {
    private static let throughKey = "social.fauna.store_consent.consented_through"
    private static let ratingKey = "social.fauna.store_consent.last_rating"
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = AppDefaults.store) { self.defaults = defaults }

    public func load() -> FfiStoreConsentState {
        FfiStoreConsentState(
            consentedThrough: (defaults.object(forKey: Self.throughKey) as? NSNumber)?.uint32Value,
            lastRating: (defaults.object(forKey: Self.ratingKey) as? NSNumber)?.int64Value)
    }

    public func save(_ state: FfiStoreConsentState) {
        if let through = state.consentedThrough {
            defaults.set(NSNumber(value: through), forKey: Self.throughKey)
        } else {
            defaults.removeObject(forKey: Self.throughKey)
        }
        if let rating = state.lastRating {
            defaults.set(NSNumber(value: rating), forKey: Self.ratingKey)
        } else {
            defaults.removeObject(forKey: Self.ratingKey)
        }
    }
}

/// Fixed store reads — the test agent's stand-in for the platform half.
public struct FixedStoreConsentSignals: StoreConsentSignals {
    public let features: StoreRegulatoryFeatures?
    public let rating: Int64?
    public init(features: StoreRegulatoryFeatures?, rating: Int64?) {
        self.features = features
        self.rating = rating
    }
    public func regulatoryFeatures() async throws -> StoreRegulatoryFeatures? { features }
    public func ageRatingCode() async -> Int64? { rating }
}

/// Raises nothing — the asker while the test agent stands in for the store.
struct InertStoreConsentAsker: StoreConsentAsker {
    func ask(updateDescription: String) async throws {}
    func acknowledge(updateDescription: String) async {}
}
