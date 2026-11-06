// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import Foundation
import QuartzCore
#if os(macOS)
import AppKit
#endif

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// The engagement-cue test clock's apple doors (convention 14,
/// `e2e-latency-independent-assertions.md`): `feed_cue_clock_freeze`,
/// `feed_cue_clock_thaw`, `feed_cue_clock_advance {ms}`, `feed_cue_probe` and
/// `feed_cue_scroll_by {px}` — the same names and reply shapes as tui's,
/// linux's (`feed::viewport::test_clock`), web's (`#lib/feed-cues-e2e`) and
/// android's (`TestAgent.feedCueCommand`). The frozen clock itself is the
/// shared tracker's (`fauna_feed::CueTracker` § The test clock), reached over
/// `FfiCueTracker`'s test-helpers faces.
///
/// The clock lives on the auth's ONE tracker (`FeedVM.cueTracker()`), so
/// freeze, thaw and advance work off the Feed page too; the probe's measuring
/// sample and the pixel step need the mounted list
/// (`CueViewportObserver.current`).
///
/// Every sample taken here resolves only once the exposures it ended — and
/// any the sampling tick ended before it — are recorded, the engine's train
/// and any put included (`CueViewportObserver.record`), so an ack means
/// nothing more is on its way: the step is the record barrier, as on web and
/// android, on both apple apps alike (an iOS app has no quit door a test can
/// wait out, and a macOS app outlives its last window).
///
/// One implementation for macOS + iOS, like `FeedReloadHoldTestCommand`.
public enum FeedCueClockTestCommand {
    public static let actions: Set<String> = [
        "feed_cue_clock_freeze", "feed_cue_clock_thaw", "feed_cue_clock_advance",
        "feed_cue_probe", "feed_cue_scroll_by",
    ]

    public enum Outcome {
        /// The command's reply, for the shell's result slot.
        case reply([String: Any])
        /// Convention 11: the shell surfaces the reason loudly (and replies
        /// `{error}`, the shape linux's refusal takes).
        case refused(String)
    }

    @MainActor
    public static func apply(_ action: String, _ command: [String: Any], feedVM: FeedVM) async -> Outcome {
        guard let tracker = feedVM.cueTracker() else {
            return .refused("\(action): no feed manager (pre-auth)")
        }
        let observer = CueViewportObserver.current
        func probe() -> [String: Any] {
            let json = tracker.probeForTest()
            return (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
        }
        func frozen() -> Bool { probe()["frozen"] as? Bool ?? false }
        /// One sample of the mounted list, recorded — a no-op off the page.
        func land() async {
            guard let sample = observer?.sampleNow, let record = observer?.recordNow else { return }
            await record(sample())
        }

        switch action {
        case "feed_cue_clock_freeze":
            tracker.freezeClockForTest(
                monoNowMs: CueViewportObserver.monoNowMs(), wallNowMs: CueViewportObserver.wallNowMs())
            return .reply(["ok": true])
        case "feed_cue_clock_thaw":
            tracker.thawClockForTest()
            return .reply(["ok": true])
        case "feed_cue_clock_advance":
            let ms = (command["ms"] as? NSNumber)?.uint64Value ?? 0
            do {
                try tracker.advanceClockForTest(ms: ms)
            } catch {
                return .refused("feed_cue_clock_advance(\(ms)): \(error)")
            }
            // Sample at once, so each step lands as exactly one sample's
            // credit (the tick could fold two advances into one capped sample).
            await land()
            return .reply(["ok": true])
        case "feed_cue_probe":
            // Frozen, a sample credits nothing — it only measures — so the
            // probe reads the list as laid out after the next frame (a
            // `scroll_to` just before lands only then), not as the last tick saw it.
            if observer != nil, frozen() {
                await nextFrame()
                await land()
            }
            return .reply(probe())
        case "feed_cue_scroll_by":
            // Scroll the post list's real offset by `px`, then answer once the
            // moved layout has reached the capture, from a sample of its own,
            // with the probe as THAT sample left it — a later read could
            // already show the feed's re-rank in answer to an exposure the step
            // ended. `moved: false` at the end of the list.
            guard let observer, let sample = observer.sampleNow, let record = observer.recordNow else {
                return .refused("feed_cue_scroll_by: the feed page is not mounted")
            }
            let px = CGFloat((command["px"] as? NSNumber)?.doubleValue ?? 0)
            let delivered = observer.framesDelivered
            guard let moved = AutomationRegistry.shared.scrollEnclosingScrollView(of: "post-card", by: px)
            else {
                return .refused("feed_cue_scroll_by: no post-card on screen under a scroll view")
            }
            await nextFrame()
            // A moved list re-publishes its card frames; wait a bounded number
            // of frames for that delivery rather than sample the old layout.
            var frames = 0
            while moved, observer.framesDelivered == delivered, frames < maxFramesForLayout {
                await nextFrame()
                frames += 1
            }
            let left = sample()
            let after = probe()
            await record(left)
            return .reply(["ok": true, "moved": moved, "probe": after])
        default:
            return .refused("\(action): unhandled feed cue command")
        }
    }

    /// How many display frames a scroll step waits for its layout to reach
    /// the capture before it samples anyway (a frame count, never a duration).
    private static let maxFramesForLayout = 60

    /// Resume once the next display frame has run whole — the display link
    /// fires at the frame's start and a main-queue hop runs after it, so a
    /// read sees what that frame laid out (android's `awaitLaidOutFrame`,
    /// linux's `after_next_frame` twin).
    @MainActor
    private static func nextFrame() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            FrameWaiter.start { DispatchQueue.main.async { continuation.resume() } }
        }
    }
}

/// One display-link callback, then gone.
@MainActor
private final class FrameWaiter: NSObject {
    private var done: (() -> Void)?
    private var link: CADisplayLink?

    static func start(_ done: @escaping () -> Void) {
        let waiter = FrameWaiter()
        waiter.done = done
        #if os(macOS)
        guard let view = NSApplication.shared.windows.first(where: { $0.isVisible })?.contentView
        else {
            done()   // no window to pace a frame by — nothing is laid out anyway
            return
        }
        let link = view.displayLink(target: waiter, selector: #selector(fire))
        #else
        let link = CADisplayLink(target: waiter, selector: #selector(fire))
        #endif
        waiter.link = link
        link.add(to: .main, forMode: .common)
    }

    @objc private func fire() {
        link?.invalidate()
        link = nil
        let done = self.done
        self.done = nil
        done?()
    }
}

#endif
#endif // !FAUNA_KIDS
