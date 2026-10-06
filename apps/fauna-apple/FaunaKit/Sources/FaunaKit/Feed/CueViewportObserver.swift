import SwiftUI

// The engagement-cue capture shell's thin, live-view half
// (`docs/goal/behavior/engagement-cues.md` § Layer B / § Where logic lives) —
// the glue around the shared `FfiCueTracker`. It owns a clock, a tick, and the
// SwiftUI plumbing that measures cards; it owns NO bookkeeping rules (which
// card left, what counts as dwell, the noise floor) and NO thresholds — all of
// that is `fauna_feed::CueTracker`, reached through `FfiCueTracker`
// (capture-shell boundary revised 2026-07-29; apple was the last of four
// shells to migrate onto it, alongside linux/android/windows).
//
// Division of labour, mirroring android's `CueViewport.kt` / windows'
// `CueViewportObserver.cs` split — this file is a geometry probe and nothing
// else: it reads where each post card sits, ticks, and hands the readings to
// `FfiCueTracker`, which owns dwell bucketing, credit arithmetic, the
// hold-vs-leave policy and the single-sample noise floor. The shell reports
// *samples*, never verdicts: `recordObservation` derives the verdict inside
// shared Rust and discards it here.

/// Card frames, `postId` → frame in the **global** coordinate space. Merged
/// across every `cueCard`-tagged row in the scroll view.
///
/// Global (not `.named(...)`) because the viewport rect is read the same way,
/// and only their *relative* geometry matters — this sidesteps having to thread
/// a coordinate-space name through two per-platform feed views.
struct CueFramePreferenceKey: PreferenceKey {
    static var defaultValue: [String: CGRect] { [:] }
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

/// The scroll container's own frame (global) — the viewport the card frames are
/// measured against. Read from a `.background` on the ScrollView, which does NOT
/// scroll with the content, so this is the visible window rather than the
/// content extent.
struct CueViewportPreferenceKey: PreferenceKey {
    static var defaultValue: CGRect { .zero }
    static func reduce(value: inout CGRect, nextValue: () -> CGRect) {
        let next = nextValue()
        if next != .zero { value = next }
    }
}

/// Drives one feed scroll view's cue capture: samples the measured card frames
/// on a fixed tick, hands them to the shared `FfiCueTracker`, and reports each
/// finished exposure to the shared `FfiFeedManager`.
///
/// **Two clocks, deliberately** (mirrors linux's `viewport.rs` split exactly):
/// the per-sample dwell *credit* comes from a **monotonic** clock, so a wall-clock
/// adjustment (NTP step, DST, the user changing the date) can never inflate a
/// dwell or make one negative; the `observedAtMs` *stamp* is **wall clock**,
/// because it is what drives the shared engine's put-debounce cadence and is
/// compared against previously stored observations across sessions and devices.
@MainActor
public final class CueViewportObserver {
    /// The live feed observer, for the app-lifecycle flush hooks that sit
    /// OUTSIDE the view tree (iOS `scenePhase` → `.background`; macOS
    /// `applicationShouldTerminate`). Mirrors windows' `FeedViewModel.Current`
    /// static accessor, which its own tray-quit flush reaches the manager
    /// through. Weak — the feed view owns the instance; a nil here just means
    /// no feed is mounted, so there is nothing to drain.
    ///
    /// Single-observer assumption: both apple apps mount exactly one feed
    /// scroll view at a time (iOS `FeedListView`, macOS `MacFeedDetailView`).
    /// Two concurrently-mounted feed views would each run their own tracker and
    /// double-report the same cards, so if a split view ever mounts two, this
    /// needs to become a set and the trackers need to be de-duplicated.
    public private(set) static weak var current: CueViewportObserver?

    /// Manager generations already hydrated, so navigating away from the feed
    /// and back does NOT re-hydrate. `FeedManager::hydrate_cues` REPLACES the
    /// live `CueEngine` with the fetched rollup and clears the dirty flag, so a
    /// second hydrate on the same manager would silently discard every verdict
    /// folded since the first one — losing exactly the dwell the user just did.
    ///
    /// linux gets this for free (its whole feed view, and the manager with it,
    /// is rebuilt once per auth); SwiftUI re-runs `.task` on every re-appear, so
    /// apple has to say it explicitly. Static because the guard must outlive the
    /// view — a per-instance flag would reset on the very re-appear it exists to
    /// catch. A reference (not a bare static `Set`) only so the unit tier can
    /// hand `run` a fresh ledger per test.
    private static let hydratedGenerations = CueHydrationLedger()

    /// The running loop's FFI calls (the tracker it samples + the manager it
    /// reports to) — held so the lifecycle flush below can reach them without
    /// a `FeedVM`.
    private var calls: CueCaptureCalls?
    private var frames: [String: CGRect] = [:]
    private var viewport: CGRect = .zero

    public init() {}

    // MARK: - Geometry intake (called from the SwiftUI preference plumbing)

    func updateFrames(_ frames: [String: CGRect]) { self.frames = frames }
    func updateViewport(_ rect: CGRect) { self.viewport = rect }

    // MARK: - Lifecycle

    /// Runs the capture loop until the surrounding `.task` is cancelled (feed
    /// disappears, or the manager generation changes), then drains and flushes.
    ///
    /// Structured on purpose: `.task(id:)` ties the loop's lifetime to the view's,
    /// so there is no unwire path to forget — the linux `is_mapped()`-break and
    /// windows `OnNavigatedFrom` teardown, expressed as cancellation.
    ///
    /// `onError` now carries the generation captured at the top of this run
    /// (in `run(feedVM:calls:hydrated:onError:)`, before its first await) — a `.task(id: feedVM.managerGeneration)` cancellation is
    /// cooperative, so an FFI await already in flight when the account
    /// switches can still complete and report a failure afterward. Threading
    /// the generation lets the caller (the guarded `FeedVM.landClientErrorMessage`
    /// lander) refuse a write from a generation that is no longer current,
    /// rather than trusting cancellation to have already stopped this loop
    /// (`account-scoping.md` § The scoping taxonomy, the in-flight landing
    /// rules, `:208-236`).
    func run(feedVM: FeedVM, onError: @escaping (Int, String) -> Void) async {
        guard let manager = feedVM.manager else { return }
        // apple's feed (`FeedListView` on iOS, `MacFeedDetailView` on macOS) is
        // a deliberately non-virtualizing eager `ScrollView { VStack }` — every
        // post-card view stays realized regardless of scroll position, so a row
        // that merely could not be measured this sample is mid-layout, never
        // disposed. `.holdUnmeasured` is the matching leave model (linux's GTK
        // `ListBox` and windows' non-virtualizing `StackPanel` use the same one;
        // android's virtualizing `LazyColumn` is the one app on `.absenceIsLeave`).
        let tracker = FfiCueTracker(leaveModel: .holdUnmeasured)
        await run(feedVM: feedVM, calls: .live(manager: manager, tracker: tracker),
                  hydrated: Self.hydratedGenerations, onError: onError)
    }

    /// `run`'s body over injected FFI calls — the unit-tier seam
    /// (`CallSiteCaptureOrderingTests`): a test hands it calls that suspend
    /// across a `FeedVM.reset()` and then fail, so this function's own
    /// generation capture — BEFORE its first await — is executed, not read. Production passes the process-wide
    /// `hydratedGenerations` ledger; a test passes a fresh one.
    func run(feedVM: FeedVM, calls: CueCaptureCalls, hydrated: CueHydrationLedger,
             onError: @escaping (Int, String) -> Void) async {
        let generation = feedVM.managerGeneration
        self.calls = calls
        Self.current = self

        await Self.hydrateIfNeeded(feedVM: feedVM, calls: calls, hydrated: hydrated, onError: onError)

        while !Task.isCancelled {
            do {
                try await Task.sleep(for: .milliseconds(cueSampleIntervalMs()))
            } catch {
                break   // cancelled mid-sleep — fall through to the drain below
            }
            await tick(feedVM: feedVM, calls: calls, generation: generation, onError: onError)
        }

        // Teardown drain. Fire-and-forget in a DETACHED task: the surrounding
        // `.task` is already cancelled here, and an FFI call made from a
        // cancelled task is not guaranteed to run to completion. The process
        // stays alive when a feed view merely disappears, so this is linux's
        // `bounded = false` hide-to-tray case — the put finishes on its own.
        if Self.current === self { Self.current = nil }
        self.calls = nil
        let left = calls.drainAll(Self.wallNowMs())
        Task.detached { @MainActor in
            await Self.emit(left, to: calls, generation: generation, onError: onError)
            try? await calls.flushCues()
        }
    }

    /// One honest sample: read every card's raw geometry against the viewport,
    /// hand it to the shared tracker, and report whatever it hands back.
    ///
    /// `generation` rides straight through from `run`'s own capture (not
    /// re-read from `feedVM` here) — it never changes within one `run`
    /// invocation, and reading it fresh per tick would only invite drift from
    /// the value `onError`'s caller actually guards against.
    private func tick(feedVM: FeedVM, calls: CueCaptureCalls, generation: Int,
                      onError: @escaping (Int, String) -> Void) async {
        guard viewport.height > 0 else { return }   // not laid out yet — measure nothing

        // The FULL loaded window (not just arranged rows) — it resolves is-media
        // at first sighting AND answers "did this post leave the feed entirely",
        // which is the only reason an UNMEASURED card is ever treated as gone.
        let postsWindow = Self.postsWindow(feedVM.posts)
        let rows = Self.buildRows(frames: frames, postsWindow: postsWindow)

        let observations = calls.sample(
            rows, Array(postsWindow.keys),
            Double(viewport.minY), Double(viewport.maxY),
            Self.monoNowMs(), Self.wallNowMs())
        await Self.emit(observations, to: calls, generation: generation, onError: onError)
    }

    /// The measurement half of a sample, split out as a pure function over
    /// primitives so the omit-unarranged-rows rule below is unit-asserted
    /// without a live view.
    ///
    /// One `CueRow` per card the SwiftUI preference plumbing published a frame
    /// for; a card **omitted** from `frames` entirely (never laid out, not yet
    /// in the view tree) produces no row at all. A card that WAS published but
    /// with a non-positive height (mid-layout) is included as-is — the shared
    /// tracker treats a non-positive `height` as unmeasurable itself, so this
    /// probe no longer has to make that call (the two probe-side
    /// simplifications the shared contract allows, `cue_tracker.rs`'s own doc
    /// comment). `nonisolated` — pure arithmetic over primitives, touching no
    /// actor state, so the headless tests can call it directly.
    nonisolated static func buildRows(frames: [String: CGRect],
                                      postsWindow: [String: Bool]) -> [CueRow] {
        frames.map { postId, frame in
            CueRow(postId: postId, top: Double(frame.minY), height: Double(frame.height),
                   isMedia: postsWindow[postId] ?? false,
                   // No playback surface on apple yet — see `emit`'s comment.
                   mediaPlayedPm: nil)
        }
    }

    /// `postId` → `hasMedia` over the whole loaded window.
    ///
    /// `hasMedia` is read straight off the shared snapshot's own field — the
    /// same `PostSummary.has_media` linux's shell reads (`viewport.rs`) — rather
    /// than re-deriving media-ness from the render document client-side, so all
    /// seven apps answer "is this media" from one shared-Rust source.
    nonisolated static func postsWindow(_ posts: [PostSummary]) -> [String: Bool] {
        var window: [String: Bool] = [:]
        window.reserveCapacity(posts.count)
        for post in posts { window[post.postId] = post.hasMedia }
        return window
    }

    /// `internal` (not `private`) so `CallSiteCaptureOrderingTests` can drive
    /// it directly with a hydrate that suspends across a `reset()` and throws.
    static func hydrateIfNeeded(feedVM: FeedVM, calls: CueCaptureCalls,
                                hydrated: CueHydrationLedger,
                                onError: @escaping (Int, String) -> Void) async {
        let generation = feedVM.managerGeneration
        guard !hydrated.generations.contains(generation) else { return }
        hydrated.generations.insert(generation)
        // The Layer-B producer (`FeedManager::record_observation`) contributes
        // only once the manager's cached opt-in is filled, so a persisted opt-in
        // (set before a restart, or on another device) must be hydrated at feed
        // init — not only when the user opens Personalization (`engagement-cues.md`
        // § Layer B; linux/tui/web hydrate at the same point). Non-fatal, and
        // independent of the cue hydrate below: a failure leaves the producer
        // off, exactly as an unset opt-in would, so it is logged, never painted.
        do {
            try await calls.hydrateSignalOptin()
        } catch {
            NSLog("[CueViewportObserver] hydrateSignalOptin failed: %@", String(describing: error))
        }
        do {
            try await calls.hydrateCues()
        } catch {
            // Surfaced, never swallowed: an unopenable rollup must not read as a
            // fresh one (`FeedManager::hydrate_cues`'s hard-fail rule). Allow a
            // later re-entry to retry rather than stranding the session cue-less.
            //
            // `generation` also guards the write itself: a hydrate started
            // before an account switch can still complete and error after
            // one, and must not paint into the incoming actor's feed
            // (`account-scoping.md` § The scoping taxonomy, `:208-236`).
            hydrated.generations.remove(generation)
            if let text = DisplayError.message(error) { onError(generation, text) }
        }
    }

    /// Whether a mounted feed has cue state worth waiting on at quit — lets the
    /// macOS terminate hook return `.terminateNow` (no delay at all) whenever no
    /// feed is on screen, rather than bounding a flush that would no-op.
    public static var hasPendingFlush: Bool { current != nil }

    /// Drain + seal, for the app-lifecycle hooks outside the view tree. Awaits
    /// the flush so a caller that is about to die can bound it.
    public static func flushForAppLifecycle() async {
        guard let observer = current, let calls = observer.calls else { return }
        // Deliberately no `FeedVM` here: the app-level hooks have no view to
        // read a posts window from, and everything tracked is ending anyway.
        let left = calls.drainAll(wallNowMs())
        await emit(left, to: calls, generation: 0, onError: { _, _ in })
        try? await calls.flushCues()
    }

    /// Hand the finished exposures to the shared engine
    /// (`FfiFeedManager.recordObservation`) — already past the tracker's own
    /// single-sample noise floor, so no shell-side filtering is needed or
    /// correct here. `observedAtMs` rides straight through from the tracker,
    /// which stamped every exposure in the batch with the one `wallNowMs` this
    /// sample was taken at.
    ///
    /// No playback surface on apple yet — the feed renders media as
    /// `AsyncImage` stills, with no AVPlayer anywhere, so `mediaPlayedPm` is
    /// always `nil` at the probe (`buildRows`) and the media `watch-complete`
    /// gate is unreachable here, exactly as on linux/android/windows. This is
    /// the argument that becomes real the moment apple gains inline video.
    ///
    /// `generation` is the same account-switch guard `hydrateIfNeeded` applies
    /// to its own `onError` call — `recordObservation` is exactly the FFI
    /// await a switch can race past (`account-scoping.md` § The scoping
    /// taxonomy, `:208-236`). `flushForAppLifecycle`
    /// has no live `FeedVM` to derive one from and passes a discarded
    /// placeholder — its own `onError` is already a no-op, by design.
    private static func emit(_ observations: [CueObservation], to calls: CueCaptureCalls,
                             generation: Int, onError: @escaping (Int, String) -> Void) async {
        for observation in observations {
            do {
                try await calls.recordObservation(observation)
            } catch {
                if let text = DisplayError.message(error) { onError(generation, text) }
                return   // short-circuit the batch, as linux does
            }
        }
    }

    /// The **monotonic** clock feeding dwell credit — an NTP step or a user
    /// date change must never inflate how long something was shown. Apple is
    /// one of the two shells that never drifted onto the wall clock here (the
    /// other is linux); this call site is unchanged by the migration, only its
    /// destination (`FfiCueTracker.sample`'s `monoNowMs`, not this file's own
    /// arithmetic).
    private static func monoNowMs() -> UInt64 {
        UInt64(DispatchTime.now().uptimeNanoseconds / 1_000_000)
    }

    /// The **wall** clock stamping `CueObservation.observedAtMs` — what drives
    /// the shared engine's put-debounce cadence and is compared against
    /// previously stored observations across sessions and devices.
    private static func wallNowMs() -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1000))
    }
}

// MARK: - FFI seam

/// Every FFI call `CueViewportObserver.run` makes — the shared tracker it
/// samples and the shared manager it reports to — as closures. Production
/// builds it with `.live(manager:tracker:)`; the unit tier substitutes calls
/// that suspend across a `FeedVM.reset()` and then fail, so `run`'s and
/// `hydrateIfNeeded`'s generation captures are executed, not read
/// (`account-scoping.md` § The scoping taxonomy, `:208-236`). Closures rather than the UniFFI-generated
/// `…Protocol` types, so FaunaKit's FFI surface stays exactly what it was.
struct CueCaptureCalls {
    var hydrateCues: () async throws -> Void
    var hydrateSignalOptin: () async throws -> Void
    var recordObservation: (CueObservation) async throws -> Void
    var flushCues: () async throws -> Void
    var sample: (_ rows: [CueRow], _ windowPostIds: [String], _ viewportStart: Double,
                 _ viewportEnd: Double, _ monoNowMs: UInt64, _ wallNowMs: UInt64) -> [CueObservation]
    var drainAll: (_ wallNowMs: UInt64) -> [CueObservation]

    static func live(manager: FfiFeedManager, tracker: FfiCueTracker) -> CueCaptureCalls {
        CueCaptureCalls(
            hydrateCues: { try await manager.hydrateCues() },
            // The Bool (the opt-in itself) is the manager's cache fill's by-product; the shell has no use for it.
            hydrateSignalOptin: { _ = try await manager.hydrateSignalOptin() },
            recordObservation: { observation in
                try await manager.recordObservation(
                    contentId: observation.contentId,
                    isMedia: observation.isMedia,
                    mediaPlayedPm: observation.mediaPlayedPm,
                    dwellMsAtSkipVisibility: observation.dwellMsAtSkipVisibility,
                    dwellMsAtLongVisibility: observation.dwellMsAtLongVisibility,
                    observedAtMs: observation.observedAtMs)
            },
            flushCues: { try await manager.flushCues() },
            sample: { rows, windowPostIds, viewportStart, viewportEnd, monoNowMs, wallNowMs in
                tracker.sample(
                    rows: rows, windowPostIds: windowPostIds,
                    viewportStart: viewportStart, viewportEnd: viewportEnd,
                    monoNowMs: monoNowMs, wallNowMs: wallNowMs)
            },
            drainAll: { wallNowMs in tracker.drainAll(wallNowMs: wallNowMs) })
    }
}

/// The manager generations already hydrated — see
/// `CueViewportObserver.hydratedGenerations` for why the process keeps one.
@MainActor
final class CueHydrationLedger {
    var generations: Set<Int> = []
}

// MARK: - SwiftUI wiring

public extension View {
    /// Tags one post card as cue-measurable. Publishes its global frame; the
    /// container's `cueCapture` collects every tagged card's frame each layout
    /// pass.
    ///
    /// `postId` (not a row index) is the key, and it must come from the card's
    /// own bound post: the shared tracker trusts it as identity, so a stale
    /// index-derived id would re-attribute one post's dwell to another the
    /// moment a re-rank reorders the list. Same load-bearing lesson as android's
    /// `items(posts, key = { it.postId })` and linux's per-row `post` test-attr.
    func cueCard(postId: String) -> some View {
        background(
            GeometryReader { geometry in
                Color.clear.preference(key: CueFramePreferenceKey.self,
                                       value: [postId: geometry.frame(in: .global)])
            }
        )
    }

    /// Wires engagement-cue capture to a feed scroll view whose rows are tagged
    /// with `cueCard(postId:)`. Apply to the `ScrollView` itself — the viewport
    /// rect is read from its frame.
    ///
    /// `onError` receives the generation captured before the failing FFI
    /// await, alongside a human-readable message, for the page's
    /// `error-message` element (the `dispatch_train` idiom linux surfaces
    /// `record_observation` failures through) — callers land it through the
    /// guarded `FeedVM.landClientErrorMessage(generation:message:)` rather
    /// than writing unconditionally, so a failure surfacing after an account
    /// switch is refused instead of painting the incoming actor's feed
    /// (`account-scoping.md` § The scoping taxonomy, `:208-236`).
    func cueCapture(feedVM: FeedVM, onError: @escaping (Int, String) -> Void) -> some View {
        modifier(CueCaptureModifier(feedVM: feedVM, onError: onError))
    }
}

struct CueCaptureModifier: ViewModifier {
    let feedVM: FeedVM
    let onError: (Int, String) -> Void

    @State private var observer = CueViewportObserver()

    func body(content: Content) -> some View {
        content
            .background(
                GeometryReader { geometry in
                    Color.clear.preference(key: CueViewportPreferenceKey.self,
                                           value: geometry.frame(in: .global))
                }
            )
            .onPreferenceChange(CueViewportPreferenceKey.self) { rect in
                MainActor.assumeIsolated { observer.updateViewport(rect) }
            }
            .onPreferenceChange(CueFramePreferenceKey.self) { frames in
                MainActor.assumeIsolated { observer.updateFrames(frames) }
            }
            // Keyed on the manager generation so a re-auth restarts capture
            // against the new actor's manager rather than reporting this
            // actor's dwell into the previous one. Cancellation on disappear is
            // what drains + flushes (see `run`).
            .task(id: feedVM.managerGeneration) {
                await observer.run(feedVM: feedVM, onError: onError)
            }
    }
}
