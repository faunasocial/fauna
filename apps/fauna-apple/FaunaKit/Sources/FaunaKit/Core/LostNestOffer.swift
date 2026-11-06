import Foundation

/// Whether an optimistically entered session sits on a nest that may be gone for
/// good — the decision behind iOS's one transient escalation (`onboarding.md`
/// § App-launch routing, the entry-posture paragraph; `box-recovery.md` § The
/// plane-era recovery floor, (b)).
///
/// iOS stays in its cached shell on a transient launch verdict, and only
/// `launch_retry` offers `launch-recover-button`, so a surviving device whose box
/// died would never be offered recovery. The rule is web's
/// (`silent-sign-in-retry.ts`, `lostNestOfferDue`): a device that custodies a box
/// and still cannot reach its nest about eight seconds in is sent to the launch
/// surface. A device custodying nothing has nothing to be offered, so it is left
/// exactly as it was.
///
/// One narrowing web has no call for: a device with no network path stays put.
/// That is the case the cached shell exists for, and a dead box cannot be told
/// from it, so an offer there would take an admin's cached content away on every
/// flight.
///
/// Pure over its seams, so the decision is unit-tested apart from the launch
/// machine and the store.
public enum LostNestOffer {
    /// The waits between the launch's own failed challenge and the fourth, web's
    /// schedule up to its offer.
    public static let retryDelays: [Duration] = [.seconds(1), .seconds(2), .seconds(5)]

    public enum Outcome: Equatable {
        /// Stay on the cached session: nothing to recover, the device is offline,
        /// or a newer launch owns the shell.
        case stay
        /// A retry left the transient state: its verdict routes as any other.
        case verdictChanged
        /// Still unreachable, and this device custodies a box: leave for `launch_retry`.
        case offer
    }

    /// - Parameters:
    ///   - custodiesBox: this device's own store holds a recoverable box.
    ///   - isCurrent: the launch that asked is still the held one.
    ///   - stillTransient: runs one retry and says whether it failed the same way.
    ///   - deviceOnline: the device has a network path.
    ///   - sleep: the wait between retries; a seam for the tests.
    @MainActor
    public static func settle(
        custodiesBox: () async -> Bool,
        isCurrent: () -> Bool,
        stillTransient: () async -> Bool,
        deviceOnline: () -> Bool,
        sleep: (Duration) async -> Void = { try? await Task.sleep(for: $0) }
    ) async -> Outcome {
        guard await custodiesBox() else { return .stay }
        for delay in retryDelays {
            await sleep(delay)
            guard isCurrent() else { return .stay }
            let transient = await stillTransient()
            guard isCurrent() else { return .stay }
            if !transient { return .verdictChanged }
        }
        return deviceOnline() ? .offer : .stay
    }
}
