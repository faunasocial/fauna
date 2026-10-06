import Foundation
import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// The ONE door every `FAUNA_E2E_*` environment read in hand-written apple Swift
/// goes through — a `#if DEBUG` real plus a same-signature production twin, so a
/// Release artifact contains neither the reads nor the variable names
/// (`e2e-automation-surface-gating.md` § The convention, convention 15: the
/// compile-time exclusion is the outer boundary, a runtime env check is only the
/// inner switch within an already test-capable build).
///
/// **Copied deliberately from windows' `FaunaApp.Core/Services/E2eEnv.cs`**
/// (`#if DEBUG || FAUNA_E2E_AGENT` real + `#else` twin, 2026-08-10), the leg that
/// settled this shape: one reviewable surface instead of N chances to forget a
/// directive, and every call site's production arm unchanged by construction.
/// Priorities #1/#3 — the same concept, spelled the same way, on both desktop apps.
///
/// **Why a twin rather than deleting the members from the Release build.** A
/// `#if DEBUG`-only declaration turns any future production-path read into a
/// Release-ONLY compile error, which no debug build and no e2e run can see — the
/// failure mode that left `just windows-release` unbuildable on `origin/main`
/// undetected (§ Implementation status today, the C# leg). The twin keeps every
/// caller compiling in both arms and makes the production answer a constant.
///
/// **Severity is the payload's, not the predicate's** (the rule § Implementation
/// status today applies to `cred_file_dir()` and to windows'
/// `AccountReauth.ConfirmActivationAsync`). Three of these variables are
/// *redirects*, not mode toggles: `FAUNA_E2E_CREDENTIAL_DIR` relocates the
/// credential store AND stands in for the device-owner re-auth verdict,
/// `FAUNA_E2E_DOWNLOAD_DIR` relocates the user's exported snapshot and account
/// data. Their call sites therefore carry their OWN `#if DEBUG` around the whole
/// e2e branch as well: a dead branch behind a constant-`nil` twin is soundness
/// only an optimizer can see, and this family does not accept that as a gate
/// (§ Implementation status today, the tui `download_dir()` bullet).
///
/// ⚠ **It lives in THIS file, not one of its own, and that is load-bearing.**
/// `AutomationRegistry.swift` is one of the few files `check-apple-ios-typecheck`
/// can typecheck STANDALONE for the iOS simulator — the one target no other build
/// covers — and that script typechecks each file ALONE by design, so a reference
/// to any other FaunaKit file drops this one into the residual set. A separate
/// `Core/E2eEnv.swift` was written first and the merge gate refused it, exactly
/// as its ratchet's second direction promises. Same reason the `logMessage` call
/// in `realConversationsGateVerdict` stays shell-side.
public enum E2eEnv {
    #if DEBUG
    private static var env: [String: String] { ProcessInfo.processInfo.environment }

    /// The legacy cross-process XCUITest bridge's base URL.
    public static var bridgeUrl: String? { env["FAUNA_E2E_BRIDGE"] }

    /// The in-process automation server's port, one per launched instance.
    public static var agentPort: String? { env[agentPortName] }

    /// The NAME of [`agentPort`], for the one caller that *composes* an
    /// environment rather than reading one: `InstanceSpawner` hands a spawned
    /// child its own port. DEBUG-arm only and deliberately unpaired — its single
    /// call site is `#if DEBUG` too, and a twin would put the literal back into
    /// the Release artifact, which is the whole thing this enum prevents. (The
    /// shared-Rust scan makes the same carve-out for its six spawn-side
    /// `pub const` names — `e2e-automation-surface-gating.md` § Implementation
    /// status today.)
    public static let agentPortName = "FAUNA_E2E_AGENT_PORT"

    /// The harness-owned credential directory: the file-backed keychain
    /// (`<dir>/keychain.json`) and the stage-2 re-auth verdict
    /// (`<dir>/reauth-result`) both live here.
    public static var credentialDir: String? { env["FAUNA_E2E_CREDENTIAL_DIR"] }

    /// Where a downloaded snapshot file / exported account data is written with
    /// no save panel or share sheet.
    public static var downloadDir: String? { env["FAUNA_E2E_DOWNLOAD_DIR"] }

    /// Where the home-screen widget's unread snapshot is written instead of the
    /// shared app-group container — which resolves to the REAL home even under
    /// `CFFIXED_USER_HOME`, so a test launch must never write there. A redirect,
    /// so its call site carries its own `#if DEBUG` (the rule above).
    public static var widgetDir: String? { env["FAUNA_E2E_WIDGET_DIR"] }

    /// Run the real shared-Rust `ConversationsSession` receive loop.
    public static var realConversations: Bool { env["FAUNA_E2E_REAL_CONVERSATIONS"] != nil }

    /// Spawn the real external `fauna-sync-agent` as a private child.
    public static var realSyncAgent: Bool { env["FAUNA_E2E_REAL_SYNC_AGENT"] != nil }

    /// Actuate a disabled control anyway (the enumeration mode) instead of
    /// refusing with HTTP 409.
    public static var permissiveActuation: Bool { env["FAUNA_E2E_PERMISSIVE_ACTUATION"] != nil }

    /// Run-scoped sink every `DISABLED-ACTUATION` marker is appended to.
    public static var actuationLog: String? { env["FAUNA_E2E_ACTUATION_LOG"] }
    #else
    // Production twins. Same signatures, constant answers, no variable names in
    // the artifact — the half a `strings -a` of a Release build has to see.
    public static var bridgeUrl: String? { nil }
    public static var agentPort: String? { nil }
    public static var credentialDir: String? { nil }
    public static var downloadDir: String? { nil }
    public static var widgetDir: String? { nil }
    public static var realConversations: Bool { false }
    public static var realSyncAgent: Bool { false }
    public static var permissiveActuation: Bool { false }
    public static var actuationLog: String? { nil }
    #endif
}

/// Are we in *any* E2E test mode? True under the legacy cross-process XCUITest
/// bridge (`FAUNA_E2E_BRIDGE`) **or** the in-process automation server
/// (`FAUNA_E2E_AGENT_PORT`).
///
/// Use this — never a bare `FAUNA_E2E_BRIDGE` check — for behavior that must hold
/// for **both** apple driving backends: the deterministic mock conversation
/// backends (`ConversationsVM.init`), authenticate-only login
/// (`applySessionPatch`), the in-memory keychain, suppressing the auto-updater.
/// Gating only on `FAUNA_E2E_BRIDGE` silently excludes the in-process path — the
/// exact drift that left the whole conversation cluster's injected threads empty
/// in-process (a `FAUNA_E2E_BRIDGE`-only gate that was never broadened when the
/// in-process server landed). One predicate so the next broadening can't miss a
/// site. See docs/goal/architecture/apps/apple-e2e-automation.md.
///
/// **Every member below reads the environment through [`E2eEnv`]**, whose
/// production twin answers `nil`/`false` with no variable name compiled in — so
/// in a Release build this whole enum is a constant `false`/`nil` and the
/// artifact carries none of the names (convention 15,
/// `e2e-automation-surface-gating.md` § The convention). The enum itself stays
/// outside `#if DEBUG` deliberately: 21 production-path call sites read
/// `isActive` and have to compile in both arms.
public enum FaunaE2E {
    public static var isActive: Bool {
        E2eEnv.bridgeUrl != nil || E2eEnv.agentPort != nil
    }

    /// E2E-only: run the **real** shared-Rust `ConversationsSession` receive loop
    /// (real-decrypted inbound mail / MLS DMs) under the automation server, on top of
    /// the deterministic mock conversation backends `ConversationsVM.init` installs.
    /// Gated by `FAUNA_E2E_REAL_CONVERSATIONS` — the apple twin of windows'
    /// `App.xaml.cs` set_state harness and linux `conv_backend`
    /// (`start_conversations_session`, which runs the real session for every e2e
    /// login). A test that must exercise the real receive loop (the
    /// `real_conversations` marker + `test_mail_client_{receive,send,spam_receive}`)
    /// launches with this flag, and the `applySessionPatch` login path then builds the
    /// real session + `activate`s it (running `startReceiveLoop`). Only meaningful when
    /// `isActive`. Set session-wide by the macos/ios `_build_client_config` branches,
    /// mirroring windows.
    ///
    /// ⚠ **"On top of", not "instead of" — and that distinction is load-bearing.** The
    /// session is built over the VM's *existing* manager
    /// (`conversationsSessionOverManager` → `ConversationsSession::from_manager`), so
    /// activating **registers the real FaunaMls + SMTP rails onto that manager**,
    /// per-rail — the mock rails the session does not serve (Nostr / Bluesky / ActivityPub)
    /// survive, and every thread already injected survives too. It used to *replace* the
    /// manager, which silently destroyed both. Because this flag is **session-wide**
    /// (any one `real_conversations`-marked test flips every apple config in the run),
    /// that swap turned unrelated inject-based tests red at random — whichever ones lost
    /// the race against the detached activation `Task`.
    public static var realConversations: Bool { E2eEnv.realConversations }

    /// This launch's [`realConversations`] verdict as a log line — **worded for
    /// both arms**, which is the whole point. `nil` outside e2e.
    ///
    /// The gate is read once, deep inside each shell's `applySessionPatch`, as a
    /// bare `if`. An `if` that does nothing when it is false is *silent*: a launch
    /// whose env never arrived looks exactly like a launch that opened the gate and
    /// then had a quiet receive loop, and a test above it passes against the mock
    /// while claiming the real rail. That is not hypothetical — the 2026-09-21 apple
    /// run of `test_recipient_picker` found an iOS session with no
    /// `ConversationsSession` at all and no way to tell the two cases apart from the
    /// log alone. One line on each arm makes the next reader's question answerable
    /// in the log they already have (`e2e-conventions.md` § point 6: failures
    /// diagnose themselves).
    ///
    /// Every shell that reads the gate logs this immediately before branching, so
    /// the verdict is stamped whether or not the branch is taken. **Wording lives
    /// here, not in the shells** — the assertion that reads it is cross-app, so two
    /// copies of the sentence would be two things to keep in step. The `logMessage`
    /// CALL stays shell-side deliberately: it comes from `FaunaFFISwift`, and this
    /// file is one of the few that still typecheck STANDALONE for the iOS
    /// simulator — the one target no other build covers — which importing the FFI
    /// module here would end. That coverage is worth more than three saved lines.
    public static var realConversationsGateVerdict: String? {
        guard isActive else { return nil }
        return realConversations
            ? "[e2e] real-conversations gate OPEN — building the real ConversationsSession"
            : "[e2e] real-conversations gate CLOSED (FAUNA_E2E_REAL_CONVERSATIONS unset) "
              + "— keeping the deterministic mock conversation backends"
    }

    /// E2E-only: run the **real external `fauna-sync-agent`** for this launch,
    /// spawned as a private child (`FfiChildAgentSpawner`) on the launch's own
    /// isolated socket — rather than the machine-global `social.fauna.sync-agent`
    /// LaunchAgent, which a test must never touch (testing.md point 10).
    /// Gated by `FAUNA_E2E_REAL_SYNC_AGENT`, the same flag windows' real-agent
    /// suites use, so the opt-in is one concept on both desktop apps
    /// (priorities #1/#3).
    ///
    /// Without it an e2e launch keeps the old behavior — no agent at all, folder
    /// rows seeded via `sync_inject_locations` — which is right for the many tests
    /// that only assert folder-binding UI. It is NOT right for a test that
    /// expects bytes to move: that combination is what made the macOS seat of
    /// the multiseat live test bind a folder and sync in neither direction
    /// (run 20260724-02). Only meaningful when `isActive`.
    public static var realSyncAgent: Bool { E2eEnv.realSyncAgent }

    /// E2E-only: make the automation server **refuse** to actuate a control the
    /// real UI has disabled, instead of driving it anyway.
    ///
    /// The actuation routes (`/element/{click,double_click,type,clear,select}`)
    /// used to call the registered closure without ever consulting the entry's
    /// `isEnabled` predicate, so the harness could activate a control that is
    /// `.disabled(...)` on screen — a harness-only capability with no user
    /// analogue, and a silent divergence from web, whose Playwright `click()`
    /// auto-waits for enabled and fails loudly. That is testing.md convention
    /// 11's concern one layer down: not a *dropped* command, but an *illegal*
    /// one silently honoured, whose downstream failure reads as a product bug.
    ///
    /// **Refusal is the DEFAULT since 2026-08-05.** The staged rollout is over:
    /// both apple targets were swept permissively end-to-end and the offender
    /// list is empty but for the gate's own probe (macOS 2026-08-03: 1575 tests
    /// observed, 4 violating calls on 3 elements, all fixed; iOS 2026-08-05:
    /// 1574 tests observed, **1** violating call on 1 element — the probe
    /// driving a disabled control on purpose). Zero registration bugs on either
    /// target: every violation was the harness driving a control no user could
    /// reach, which is the thing convention 11 forbids.
    ///
    /// Two modes remain, with the polarity now inverted:
    /// - **on (default)** — refuse with HTTP 409 and a named error, which the
    ///   driver surfaces as
    ///   `RuntimeError("Bridge error (409): element is disabled …")`.
    /// - **off (`FAUNA_E2E_PERMISSIVE_ACTUATION`)** — actuate anyway, but
    ///   `NSLog` a one-line `DISABLED-ACTUATION` marker naming the
    ///   route/id/index, readable for the CURRENT launch via
    ///   `drivers/macos.py::app_stderr_text`. Across a whole suite, set
    ///   `actuationLogPath` (below) and the markers accumulate in one run-scoped
    ///   file that survives the per-module cold relaunch; without it the `NSLog`
    ///   marker dies with its launch.
    ///
    /// **The permissive mode is not vestigial — it is the ENUMERATION tool**, and
    /// it must survive the flip. A strict refusal *fails* its test, and a failed
    /// test *stops*, so a strict sweep reports at most the FIRST offender per
    /// test and hides the rest; only a permissive sweep enumerates exhaustively
    /// while turning nothing red. That is how both targets were measured, it is
    /// how a future broad change re-measures itself, and it is the shape linux
    /// and windows still owe for their own rollouts.
    ///
    /// Deliberately a launch env flag and not a nest/app setting: it is
    /// harness-only, never a user- or admin-visible choice (principles.md § One
    /// configuration surface), and it is the *inner* runtime switch within an
    /// already test-capable build — the outer boundary stays the `#if DEBUG`
    /// compile-out of testing.md convention 15.
    public static var strictEnabled: Bool { !E2eEnv.permissiveActuation }

    /// E2E-only: a **run-scoped** file that every `DISABLED-ACTUATION` marker is
    /// appended to, in addition to the `NSLog` line. Absent (the default) =
    /// no file is written and behavior is exactly as before.
    ///
    /// Why this exists at all: `NSLog` reaches only the *current launch's*
    /// `app.err`, which dies with that launch's temp dir — and the `app` fixture
    /// cold-relaunches the app at every test-module boundary (testing.md
    /// convention 10), so a whole-suite run keeps nothing but the last module's
    /// markers. That is what forced the enumeration onto the *strict* mode, where
    /// each offender is a red — and a red **stops its test**, so a strict sweep
    /// reports at most the FIRST offender per test and silently hides the rest.
    ///
    /// A sink the harness names outlives the relaunches, which is what lets ONE
    /// permissive sweep enumerate every offender — with nothing turned red, so
    /// the same pass doubles as the pre-existing-red baseline the flip decision
    /// has to subtract. Harness-only, opt-in, and never a user- or admin-visible
    /// choice (principles.md § One configuration surface).
    public static var actuationLogPath: String? { E2eEnv.actuationLog }
}

// Everything below is the in-process automation registry/server plumbing —
// compiled out of release artifacts (testing.md convention 15). The `View`
// extension methods further down (`automationActivate` etc.) stay outside this
// gate — hundreds of production views call them unconditionally — and become
// no-ops in a non-DEBUG build instead.
#if DEBUG

/// One step of an ancestor **scope path** — a scoped container id plus the
/// per-parent index of that container. The in-process analogue of one segment of
/// the e2e `scope="post-card[1]/quoted-post"` DSL (`drivers/scope.py`): the wire
/// sends the same `{id, index}` pairs, and a registered leaf captures the chain of
/// scoped containers it is nested under (the containers that applied
/// `.automationScope`). Scoped queries then filter by this path — real
/// parent/child subtree modelling, replacing the flat occurrence-index heuristic
/// for multi-level scopes (apple-e2e-automation.md § limitations (a)).
public struct AutomationScopeStep: Equatable, Sendable {
    public let id: String
    public let index: Int
    public init(id: String, index: Int) {
        self.id = id
        self.index = index
    }
}

/// A registered element's live sentinel geometry: its frame and the hosting
/// window's bounds, both normalized to a TOP-LEFT origin (AppKit's bottom-up
/// window coordinates are flipped at capture).
public struct SentinelGeometry {
    public let frame: CGRect
    public let windowBounds: CGRect
    /// Whether the hosting window is itself on screen — AppKit's `isVisible`,
    /// UIKit's `!isHidden`. An element in an ordered-out window is absent
    /// whatever its in-window frame says: a dismissed macOS sheet's
    /// `SheetPresentationWindow` is ordered out while SwiftUI can keep the
    /// content attached to it with no `.onDisappear` (measured under the e2e
    /// harness, where the app is never the active one), so a frame-only test
    /// read the closed sheet's controls present indefinitely.
    public var windowVisible: Bool = true

    /// The zombie test — deliberately horizontal-origin-only. UIKit
    /// slide-transition/reuse churn leaves a DEAD row's cell attached at the
    /// nav-transition parking position, x-shifted LEFT of the window (observed:
    /// x=-89 for a deleted row's label, x=-74 for its card — the ~-25% parallax
    /// offset; live views settle back to in-window x after the transition, dead
    /// pooled cells stay parked). A mere overlap test misses wide parked cells
    /// (x=-74 + a 340pt card still overlaps), so the test is on the frame's
    /// ORIGIN: an attached element whose x-origin lies outside the window's
    /// horizontal range is transition debris, never a present element. VERTICAL
    /// position deliberately does not participate: an eager
    /// `ScrollView { VStack }` page (registration rule 6 — the canonical iOS
    /// page shape) keeps below-the-fold content attached beyond the window's
    /// height, and the in-process driver legitimately drives those closures
    /// without scrolling (no real gestures are involved). A window that is not
    /// itself on screen (`windowVisible`) rules everything in it absent first.
    public var isOnScreen: Bool {
        windowVisible && frame.minX >= windowBounds.minX && frame.minX < windowBounds.maxX
    }
}

/// The outcome of a targeted scroll-into-view. Mirrors the linux agent's
/// `scroll_into_view` reply shape 1:1 (`automation/agent.rs`) so the wire
/// contract is identical on every app: a scroll answers `found: true`, and the
/// two ways it can fail to scroll answer an `error` string instead — the driver
/// (`http_bridge.py::scroll_to`) raises on anything that isn't `found`, so a
/// dwell can never silently measure an element it never brought on screen.
public enum AutomationScrollOutcome {
    /// Centred in at least one enclosing scroll view, carrying the element's
    /// geometry as re-read AFTER the scroll. The reply publishes it (`frame` +
    /// `viewport`) so a caller can assert how much of the element actually
    /// ended up on screen instead of trusting `found: true` — the difference
    /// between proving a dwell and assuming one, and the thing that makes a
    /// regression here name its own cause (e2e conventions point 6).
    case scrolled(SentinelGeometry?)
    /// The element is real but sits under no scroll view — nothing to scroll.
    /// An `error`, not a success, exactly as on linux: a caller asking to
    /// scroll wants the element *centred*, and "it is wherever it already was"
    /// is not that. (A caller who only needs best-effort assist uses
    /// `_scroll_into_view`, which swallows this.)
    case noScrollableAncestor
    /// The sentinel is not realized or its view is detached, so there is no
    /// live geometry to scroll to.
    case detached
}

/// Test-only registry of interactive UI elements, keyed by the same test id the
/// element carries in `.accessibilityIdentifier(...)`.
///
/// **Why this exists.** The Phase-0 spike proved
/// that SwiftUI offers no usable *in-process* introspection: the NSAccessibility
/// tree doesn't build its hierarchy without an external assistive-technology
/// client (XCUITest's AutomationMode), and the AXUIElement self-query path needs
/// TCC trust and deadlocks. Unlike GTK (which the linux agent walks directly),
/// SwiftUI has no persistent, queryable widget objects. So we build the
/// queryable surface ourselves: each automatable view registers its action /
/// value closures here via the `automation*` view modifiers, and the in-process
/// automation server (`InProcessAutomationServer`) drives the UI by looking them
/// up — no XCUITest, no AutomationMode, no host reboot, no machine-wide
/// serialization. This is the in-process analogue of GTK's widget tree.
///
/// Platform-agnostic on purpose (just closures) — macOS and iOS share it, which
/// matters because iOS event synthesis is sandboxed too, so this is the *uniform*
/// driving path for both apple apps (priorities #1/#3).
@MainActor
public final class AutomationRegistry {
    public static let shared = AutomationRegistry()

    /// One registered element. Closures are read live, so values reflect current
    /// state. `index` disambiguates repeated ids (list rows) — registration order
    /// within the live view tree.
    public struct Entry {
        public var activate: (() -> Void)?
        /// A distinct double-click handler, backing `/element/double_click` (the
        /// driver's `double_click`). Used for the Outlook day-cell model where a
        /// single click and a double click on the *same* `events-day-cell-{date}`
        /// do different things (single → Day view; double → new-event compose).
        public var doubleActivate: (() -> Void)?
        public var text: (() -> String?)?
        public var value: (() -> String?)?
        public var setValue: ((String) -> Void)?
        /// The picker's currently-rendered options, for `automationSelect`
        /// entries only — convention 11's twin rule (`e2e-conventions.md`): a
        /// `/element/select` value not among these must be refused, not written
        /// through. `nil` (the default for every other `automation*` modifier,
        /// and for an `automationSelect` call site that hasn't been converted
        /// yet) keeps today's unchecked write-through behaviour.
        public var options: (() -> [String])?
        public var isEnabled: (() -> Bool)?
        /// The element's **rendered** text, when that differs from its source `text` —
        /// backs `/element/attr?attr=visible`, which the cross-app e2e
        /// `compose_visible_text` reads. Only the compose field registers one today: its
        /// `text`/`value` are the literal markdown SOURCE (the draft, the sent bytes),
        /// while `visibleText` omits the marker runs the editor conceals. Left nil
        /// everywhere else, so `attr=visible` answers null rather than silently falling
        /// back to the source — an unregistered reader fails loudly instead of green.
        public var visibleText: (() -> String?)?
        /// The field's APPLIED styling, JSON-encoded in linux's `text-runs` shape — one
        /// record per run of characters sharing a look, its source text and the looks it
        /// carries — read off the live text storage, never recomputed from the shared
        /// decoration plan. Backs `/element/attr?attr=text-runs`; only the compose field
        /// registers one (`MarkdownFieldHandle.textRuns`), so any other element answers
        /// null.
        public var textRuns: (() -> String?)?
        /// One named key (`ArrowLeft`/`ArrowRight`/`Home`/`End`) driven through the real
        /// text view's own caret-move action, so every caret-move handler runs exactly as
        /// for a real key. Backs `/element/key`; returns `nil` on success or the refusal
        /// sentence for a key it does not drive (convention 11 — never a silent ack).
        public var pressKey: ((String) -> String?)?
        /// Named attributes beyond the fixed reads — `/element/attr?attr=<name>` answers
        /// the entry's value for `<name>` when this map carries it, BEFORE the generic
        /// value/text fallback. The apple twin of linux's `test-attr-<name>-<value>` CSS
        /// class: a state the paint shows that no fixed read names (the mail chip's
        /// `revealed` address). A name the map lacks falls through unchanged.
        public var attributes: (() -> [String: String])?
        /// Why `setValue` may not be driven right now, or `nil` when it may. Read by
        /// `/element/type` and `/element/clear` before they write, which answer a
        /// non-nil sentence as a 409 refusal — never a silent write into a control
        /// no user could type into at this moment (convention 11). Only an *entry
        /// mode* control registers one: a button whose free-entry form is an input
        /// under the same id, typeable only while that input is open (the fuller
        /// reaction picker's `dm-reaction-more-button`, conversations.md § Rendering
        /// / picker glue). `nil` everywhere else — a plain field is always typeable.
        public var typeRefusal: (() -> String?)?
    }

    /// A registered `Entry` tagged with the stable token of the
    /// `_AutomationRegister` instance that placed it, so `unregister` can remove
    /// *exactly that view's* slot rather than the most-recently-registered one.
    /// (Plain LIFO removal corrupted an indexed list whenever a *non-tail* row
    /// disappeared: deleting row 0 of N fires row 0's `.onDisappear`, which under
    /// LIFO popped row N-1's slot — leaving a stale slot at index 0 and shifting
    /// every survivor. That is the email-filter "second delete is a no-op" bug:
    /// `filter_count()` stuck at 1 because the 2nd `delete_filter(0)` re-fired the
    /// already-removed row's closure. Token removal keeps the surviving rows'
    /// slots in their original order, so the flat occurrence index stays aligned
    /// with the visible rows.)
    private struct Slot {
        let token: UUID
        /// The chain of scoped containers (`.automationScope`) this element is
        /// nested under, outermost-first — its ancestor scope path. Empty for an
        /// element under no scoped container (the common case; such an id resolves
        /// scoped queries via the legacy flat occurrence-index heuristic). A
        /// non-empty path opts the id into real subtree filtering (`scopedSlots`).
        var path: [AutomationScopeStep]
        var entry: Entry
        /// The two off-screen votes seen since the last `.onAppear` (see `hide`):
        /// voting a slot out requires BOTH, because each alone also fires for
        /// states that must stay visible (`.onDisappear` for a covered
        /// NavigationStack root; window detach for a macOS table row scrolled
        /// out of an eagerly realized list). Cleared only by `register` (the
        /// SwiftUI-level `.onAppear`/first-show path) — a bare window re-attach
        /// deliberately does NOT clear them (`attach`), because UIKit
        /// transition churn re-attaches dead pooled cells too; those stay
        /// voted-out and their parked geometry keeps them absent, while a live
        /// view's settled on-screen geometry revives it via
        /// `effectivelyVisible` with no further signal.
        var sawDisappear = false
        var sawDetach = false

        /// The driver-facing visibility answer, **geometry-FIRST**: an ATTACHED
        /// slot (its sentinel realized, so `geometry()` is non-nil) is decided
        /// by live geometry ALONE — votes are irrelevant. On-screen (x-origin
        /// within the window) = present; parked off-window on x = absent. Votes
        /// decide ONLY a geometry-less slot (detached — `sentinelGeometry`
        /// returns nil once `view.window` is nil — or a context with no sentinel
        /// at all: macOS's eagerly-realized below-the-fold rows, an iOS
        /// lazy-`List` scroll-out, watchOS).
        ///
        /// Why geometry must be primary, not a both-votes-gated guard (the v3
        /// bug): UIKit transition churn does not reliably deliver BOTH off-screen
        /// votes to a re-attached dead cell — the `test_event_create_and_delete`
        /// zombie arrived with only ONE vote, so the old
        /// `!(sawDisappear && sawDetach)` short-circuit read it present despite
        /// its parked geometry. Deciding an attached slot purely on its frame
        /// removes the vote race entirely: a live view settled on screen (a
        /// NavigationStack root uncovered by a pop, which fires no balancing
        /// `.onAppear`) is present because its geometry says so, and a dead cell
        /// parked at the nav-transition offset is absent for the same reason,
        /// however many votes churn happened to deliver. This is the semantics
        /// rule 2a already ratifies (apple-e2e-automation.md).
        var effectivelyVisible: Bool {
            if let geo = geometry?() { return geo.isOnScreen }
            return !(sawDisappear && sawDetach)
        }
        /// Live sentinel geometry of the registering view (`nil` while detached
        /// or no sentinel is realized). Drives the on-screen zombie guard and
        /// document-order resolution (`visibleSlots`) plus the
        /// `/element/attr?attr=frame` read.
        var geometry: (() -> SentinelGeometry?)?
        /// Centre this slot's sentinel in each of its enclosing scroll views —
        /// backs `POST /element/scroll-into-view`. Travels WITH `geometry`
        /// (same owner, same wiring point, set together on every path that sets
        /// one) because both read the same live sentinel view: a slot with a
        /// frame but no way to scroll to it would be exactly the half-wired
        /// state the route's stub-era trap was made of.
        var scrollIntoView: (() -> AutomationScrollOutcome)?
    }

    private var entries: [String: [Slot]] = [:] {
        didSet { onChange?() }
    }

    /// Called after every change to the slot table — a show, hide, removal, or a
    /// body pass's closure refresh. The painted-errors observer's trigger (the
    /// apple twin of windows' `LayoutUpdated`): it marks the frame dirty and
    /// reads it once per burst, so it must never read synchronously from here.
    public var onChange: (@MainActor () -> Void)?

    /// What the screen shows right now: every visible slot's `(id, text)` — the
    /// same text `/element/text` answers, empty-text slots dropped. The painted
    /// frame the `painted_errors` observable is fed.
    public func paintedTexts() -> [(id: String, text: String)] {
        var frame: [(id: String, text: String)] = []
        for id in entries.keys.sorted() {
            for slot in visibleSlots(id) {
                let text = slot.entry.text?() ?? slot.entry.value?() ?? ""
                if !text.isEmpty { frame.append((id: id, text: text)) }
            }
        }
        return frame
    }

    /// The driver-facing view of an id: its visible slots, in **on-screen document
    /// order** (top-to-bottom, then left-to-right), resolved from each slot's live
    /// sentinel geometry — occurrence indices are indices into THIS list.
    ///
    /// Registration order is NOT the driver contract, because SwiftUI does not
    /// deliver it reliably: a macOS `Form`'s rows register **bottom-up** (which
    /// made flat index 1 of the 7 `wizard-frequency-option` rows resolve to the
    /// 6th option — the wizard "frequency pick does not land" defect), and an iOS
    /// lazy `List` registers rows as they materialize. Geometry is what the
    /// driver's index semantics ("the N-th one on screen") actually mean, and it
    /// also makes the flat index agree with `.automationScope`'s visual indices.
    /// Slots without geometry (no sentinel realized) keep insertion order — the
    /// sort falls back to it whenever any visible slot lacks a frame, so the
    /// pre-sentinel behavior is preserved rather than half-sorted.
    private func visibleSlots(_ id: String) -> [Slot] {
        let vis = (entries[id] ?? []).filter { $0.effectivelyVisible }
        guard vis.count > 1 else { return vis }
        let keyed = vis.map { ($0, $0.geometry?()) }
        guard keyed.allSatisfy({ $0.1 != nil }) else { return vis }
        return keyed
            .enumerated()
            .sorted { a, b in
                let (ra, rb) = (a.element.1!.frame, b.element.1!.frame)
                if ra.minY != rb.minY { return ra.minY < rb.minY }
                if ra.minX != rb.minX { return ra.minX < rb.minX }
                return a.offset < b.offset // stable tie-break: insertion order
            }
            .map(\.element.0)
    }

    /// Enabled only under the in-process e2e driver, so the modifiers are a no-op
    /// (zero registration overhead) in production. Read once.
    public static let isEnabled: Bool = E2eEnv.agentPort != nil

    private init() {}

    // MARK: - Registration (called by the view modifiers)

    /// Show `token`'s element: an UPSERT. If the token already holds a slot —
    /// visible or hidden — its payload is replaced and it is marked visible **in
    /// place**, so a re-shown element comes back at its original position (never
    /// re-appended out of document order — the hazard the old
    /// remove-on-disappear lifecycle could not avoid, because a removed slot's
    /// position was gone). A token with no slot appends one, in appearance order.
    ///
    /// Called from both show signals — the window-attachment sentinel's attach
    /// and the `.onAppear` fallback — which may both fire for one appearance;
    /// the upsert makes the second a no-op.
    public func register(
        _ id: String, token: UUID, path: [AutomationScopeStep] = [],
        geometry: (() -> SentinelGeometry?)? = nil,
        scrollIntoView: (() -> AutomationScrollOutcome)? = nil, _ entry: Entry
    ) {
        if var list = entries[id], let idx = list.firstIndex(where: { $0.token == token }) {
            list[idx].path = path
            list[idx].entry = entry
            list[idx].sawDisappear = false
            list[idx].sawDetach = false
            if geometry != nil { list[idx].geometry = geometry }
            if scrollIntoView != nil { list[idx].scrollIntoView = scrollIntoView }
            entries[id] = list
        } else {
            entries[id, default: []].append(
                Slot(token: token, path: path, entry: entry, geometry: geometry,
                     scrollIntoView: scrollIntoView))
        }
    }

    /// The window-ATTACH show path: update a slot's payload + geometry — or
    /// place it if absent — WITHOUT clearing the off-screen votes. UIKit
    /// re-attaches dead pooled cells during transition churn, so a bare attach
    /// must not resurrect a voted-out slot by fiat; a genuinely re-shown view's
    /// settled on-screen geometry revives it through `effectivelyVisible`
    /// instead, and a parked zombie's off-window geometry keeps it absent.
    public func attach(
        _ id: String, token: UUID, path: [AutomationScopeStep] = [],
        geometry: (() -> SentinelGeometry?)? = nil,
        scrollIntoView: (() -> AutomationScrollOutcome)? = nil, _ entry: Entry
    ) {
        if var list = entries[id], let idx = list.firstIndex(where: { $0.token == token }) {
            list[idx].path = path
            list[idx].entry = entry
            if geometry != nil { list[idx].geometry = geometry }
            if scrollIntoView != nil { list[idx].scrollIntoView = scrollIntoView }
            entries[id] = list
        } else {
            entries[id, default: []].append(
                Slot(token: token, path: path, entry: entry, geometry: geometry,
                     scrollIntoView: scrollIntoView))
        }
    }

    /// Replace the closures of the slot `token` already placed for `id` — the
    /// **re-render** half of the lifecycle, called from `_AutomationRegister.body`
    /// on every body pass.
    ///
    /// `register` fires once per view *identity* (`.onAppear`), but a SwiftUI view
    /// is a VALUE: every body re-evaluation builds a fresh `Entry` whose closures
    /// capture *that pass's* values. A view that captures its model struct BY VALUE
    /// (`let folder: FolderSummary`, read as `folder.webdavEnabled`) therefore
    /// had its FIRST render's closures frozen here forever, so the driver read a
    /// stale value while the on-screen SwiftUI rendering stayed fully live (SwiftUI
    /// re-derives the real `Binding` at interaction time). That is the folder
    /// serve-OFF "silent no-op": the toggle's `apply(!isOn)` kept computing
    /// `apply(true)` because the registered `isOn` never updated. Views capturing a
    /// REFERENCE type (`vm.snapshot?.webdavEnabled`) dodged it by luck — a stale
    /// closure still reads through the live reference. Refreshing every pass makes
    /// the two capture styles behave identically, so no future author has to know
    /// the difference.
    ///
    /// In place, by token: slot ORDER, COUNT, and VISIBILITY never change, so the
    /// flat occurrence-index heuristic keeps its exact semantics — a body pass of
    /// a hidden view (a background tab re-rendering on a data push) must update
    /// the closures without un-hiding anything. A token with **no** slot is a
    /// deliberate no-op, not an upsert — it means the first body pass, before
    /// either show signal has placed the slot; `register` places it moments
    /// later, in appearance order.
    public func refresh(
        _ id: String, token: UUID, path: [AutomationScopeStep] = [],
        geometry: (() -> SentinelGeometry?)? = nil,
        scrollIntoView: (() -> AutomationScrollOutcome)? = nil, _ entry: Entry
    ) {
        guard var list = entries[id],
              let idx = list.firstIndex(where: { $0.token == token }) else { return }
        list[idx].path = path
        list[idx].entry = entry
        if geometry != nil { list[idx].geometry = geometry }
        if scrollIntoView != nil { list[idx].scrollIntoView = scrollIntoView }
        entries[id] = list
    }

    /// One of the two off-screen signals a live view identity can emit. Neither
    /// alone proves the element left the screen: `.disappear` also fires for a
    /// NavigationStack root that is merely COVERED by a pushed detail (macOS keeps
    /// it attached; the element is still conceptually present and pop restores it
    /// with no balancing `.onAppear`), and `.windowDetach` also fires for a macOS
    /// table row scrolled out of an eagerly realized `List` (which the driver
    /// contract counts — macOS tests address below-the-fold rows by index, the
    /// pre-sentinel behavior). Only the CONJUNCTION hides; `.definitive` is both
    /// at once, for callers with no sentinel to disagree (the pre-sentinel
    /// lifecycle, where `.onDisappear` alone was trusted).
    public enum HideSignal: Equatable {
        case disappear
        case windowDetach
        case definitive
    }

    /// Which off-screen signal an `.onDisappear` carries, given what the element's
    /// sentinel says about its window attachment at that instant.
    ///
    /// Exactly ONE state is a cover: **still attached** (`true`) — a
    /// `NavigationStack` detail pushed over a live root, which must stay visible
    /// because the pop restores it with no balancing `.onAppear`. It votes once.
    ///
    /// The other two are real exits and vote definitively:
    ///  * `nil` — no sentinel realized in this context (watchOS; a sentinel-less
    ///    macOS row). The pre-sentinel lifecycle trusts `.onDisappear` alone.
    ///  * `false` — realized, and already OFF its window. Holding out for a
    ///    window-detach vote that has demonstrably already been missed is what
    ///    stranded a slot on every `.navigationDestination` pop.
    ///
    /// This does not touch rule 2a's conjunction (`Slot.effectivelyVisible` still
    /// needs BOTH votes to hide a geometry-less slot) — it makes the votes this
    /// layer emits accurate. Mis-voting cannot hide a live element: an on-screen
    /// view has geometry, and geometry-FIRST ignores votes for it.
    public static func hideSignal(sentinelAttached: Bool?) -> HideSignal {
        (sentinelAttached ?? false) ? .disappear : .definitive
    }

    /// Record an off-screen signal for `token`'s slot; the slot hides once BOTH
    /// signals have been seen since its last show (see `HideSignal`). The hidden
    /// slot keeps its position in the array — this kept-slot hide/show pair is
    /// what fixes iOS Gap B, where the old remove-on-`.onDisappear` lifecycle
    /// permanently deregistered a `NavigationStack` root's elements (a pushed
    /// detail fires the root's `.onDisappear`, but a pop never fires a matching
    /// `.onAppear`, because the root was never structurally removed — only its
    /// *window attachment* is restored, which is the show signal
    /// `_AttachmentSentinel` carries). No matching token → no-op.
    public func hide(_ id: String, token: UUID, signal: HideSignal = .definitive) {
        guard var list = entries[id],
              let idx = list.firstIndex(where: { $0.token == token }) else { return }
        switch signal {
        case .disappear: list[idx].sawDisappear = true
        case .windowDetach: list[idx].sawDetach = true
        case .definitive:
            list[idx].sawDisappear = true
            list[idx].sawDetach = true
        }
        entries[id] = list
    }

    /// Retire EVERY slot under `id` at once — the de-registration signal a
    /// SwiftUI **presentation**'s content can never emit for itself.
    ///
    /// `.alert` / `.confirmationDialog` content is the one context where this
    /// file's whole lifecycle is blind. Measured on macOS (`folder-residency-
    /// confirm`, after the confirm was answered): `VISIBLE(no-geo) geo=nil
    /// votes=-`. Every signal is missing, and structurally so — the
    /// `_AttachmentSentinel` rides in `.background(...)`, which an alert's
    /// content builder never realizes, so there is no probe to report a window
    /// detach and none to die; and SwiftUI does not fire `.onDisappear` for
    /// alert content either. `.onAppear` DOES fire, so such a slot registers on
    /// present and then no signal can ever hide it again: it reads visible for
    /// the rest of the process, and `is_visible` on a dismissed confirm answers
    /// the driver `true` forever.
    ///
    /// The fix cannot live in the content (nothing there knows) nor in a
    /// conditionally-rendered invisible shim (e2e convention 1 forbids it), so
    /// the **host** declares the lifetime from its own `isPresented` binding —
    /// `View.automationPresentation(ids:isPresented:)` is the only caller.
    ///
    /// Safe to hand a bare id, by construction: this only casts the two votes,
    /// and `Slot.effectivelyVisible` is geometry-FIRST, so a slot settled on
    /// screen elsewhere under the same id is decided by its frame and shrugs
    /// this off. Re-presenting restores the slot through the ordinary
    /// `register` upsert, which clears both votes. No matching id → no-op.
    public func hideAll(_ id: String) {
        guard var list = entries[id] else { return }
        for i in list.indices {
            list[i].sawDisappear = true
            list[i].sawDetach = true
        }
        entries[id] = list
    }

    /// Remove the slot a specific `_AutomationRegister` instance placed, located
    /// by its stable `token` (NOT LIFO), so removing a non-tail list row leaves
    /// the surviving rows' slots in their original order — keeping the flat
    /// registry's occurrence index aligned with the visible row order. Called on
    /// view-identity DEATH (`_AutomationLifecycle.deinit` — SwiftUI released the
    /// view's `@State` storage, so no signal can ever fire for this token again);
    /// mere off-screen-ness is `hide`. Tolerant of double-unregister (no matching
    /// token → no-op).
    public func unregister(_ id: String, token: UUID) {
        guard var list = entries[id],
              let idx = list.firstIndex(where: { $0.token == token }) else { return }
        list.remove(at: idx)
        if list.isEmpty { entries.removeValue(forKey: id) } else { entries[id] = list }
    }

    // MARK: - Lookup (called by the automation server)
    //
    // Every lookup sees only VISIBLE slots — a hidden slot is indistinguishable
    // from an absent one over the wire, so `is_visible`/`count` keep their exact
    // driver-facing meaning ("on screen now") under the kept-slot lifecycle.

    public func count(_ id: String) -> Int {
        visibleSlots(id).count
    }

    public func entry(_ id: String, index: Int = 0) -> Entry? {
        let list = visibleSlots(id)
        guard index < list.count else { return nil }
        return list[index].entry
    }

    /// The indexed slot's live window-space frame (top-left origin) — backs
    /// `/element/attr?attr=frame`, the geometry read the harness's
    /// `assert_on_screen` builds on. `nil` for a missing index or a slot whose
    /// sentinel is not realized.
    public func frame(_ id: String, index: Int = 0) -> CGRect? {
        let list = visibleSlots(id)
        guard index < list.count else { return nil }
        return list[index].geometry?()?.frame
    }

    // MARK: - Scope-aware lookup (real parent/child subtree modelling)
    //
    // The registry is a flat `id -> [Slot]` map, but each slot now carries its
    // ancestor scope path (the chain of `.automationScope` containers it sits
    // under). A scoped query resolves by **descendant matching**
    // (e2e-conventions.md § convention 1, ruled 2026-08-14): each step names a
    // container found ANYWHERE below the previous step's subtree, so a scope may
    // name only the containers it cares about (`quoted-post`), leaving the ones
    // between it and the root implicit — `scope_root` below is the flat-registry
    // emulation of linux's `find_scoped` walk / tui's `Registry::scope_root`.
    // This replaces the old innermost-occurrence-index heuristic for any id whose
    // containers opt in via `.automationScope`; ids whose containers do NOT (the
    // common case) keep the legacy heuristic untouched (see `hasScopePath`).
    //
    // ⚠ Prior to 2026-08-24 this filtered `path.starts(with: scope)` — a
    // ROOT-ANCHORED prefix, so a scope naming only an inner container (e.g. bare
    // `scope="quoted-post"`, no `post-card` prefix) matched NOTHING. That bug was
    // already LIVE, not merely latent: `QuotedPostCard`'s own
    // `.automationScope(Ids.quotedPost)` nests inside `post-card`'s scope since
    // 2026-06-28 — no red test caught it only because every
    // existing scoped query already spelled the full chain.

    /// Whether ANY registered slot of `id` carries a non-empty ancestor path —
    /// i.e. the id participates in real subtree scoping. Drives the server's
    /// "path-based vs. legacy flat-heuristic" branch so un-retrofitted ids keep
    /// their proven behavior.
    public func hasScopePath(_ id: String) -> Bool {
        visibleSlots(id).contains { !$0.path.isEmpty }
    }

    /// Resolve a query scope to the concrete ancestor path of the container it
    /// names — the descendant walk (e2e-conventions.md § convention 1),
    /// specialized to how apple actually builds paths: EVERY
    /// `.automationScope(id, index:)` call site is the OUTERMOST modifier over
    /// the container's own registration (the *self-scoping* idiom —
    /// `QuotedPostCard`, `post-card`, `device-card`, … — confirmed across every
    /// call site 2026-08-24), so a container's identity is a **verbatim
    /// `(id, index)` value already baked into a visible slot's own path** by the
    /// call site that scoped it — never something this walk has to *count* or
    /// *number* itself.
    ///
    /// So each step resolves to the shortest prefix, of any currently VISIBLE
    /// slot's path (any id), ending in a step that equals it exactly, found at
    /// or below the previously resolved root. `nil` = no visible slot currently
    /// witnesses that exact container, which every caller turns into an empty
    /// match set — the same "absent scope → no results" contract as linux's
    /// `scope_root`, and the correct read of "a hidden slot is indistinguishable
    /// from an absent one" for the CONTAINER itself, not only its children.
    ///
    /// ⚠ **This is deliberately NOT tui's `Registry::scope_root` algorithm**,
    /// which additionally *numbers* sibling instances that share one identical
    /// parent path and never call their own scope (an implicit-occurrence
    /// idiom no apple container uses — every one passes an explicit `index:`).
    /// An earlier version of this walk ported that occurrence-counting
    /// faithfully and broke `hiddenSlotsAreInvisibleToScopedQueries`: hiding a
    /// container's one witness caused a LATER sibling to be silently
    /// renumbered into the hidden one's slot, because occurrence counting only
    /// sees who is left, never who is missing. Verbatim matching has no such
    /// failure mode — a step whose one witness is hidden simply has no visible
    /// witness, full stop — and it is sufficient for every apple container
    /// today. Extending this to implicit occurrence-counting, if a future
    /// container ever needs it, wants tui's `declared`-bookkeeping shape and a
    /// fresh look at this exact regression first.
    private func scopeRoot(_ scope: [AutomationScopeStep]) -> [AutomationScopeStep]? {
        var root: [AutomationScopeStep] = []
        for step in scope {
            guard let resolved = firstVisibleWitness(of: step, atOrBelow: root.count, under: root) else {
                return nil
            }
            root = resolved
        }
        return root
    }

    /// The resolved ancestor prefix of the first VISIBLE slot whose path
    /// contains `step` verbatim at some depth ≥ `floor`, itself extending
    /// `root`. Searches `step.id`'s own slots (the self-scoping idiom — where
    /// the match is virtually always found, and `visibleSlots` is already
    /// correctly document/geometry-ordered).
    ///
    /// Falls back to every OTHER id's slots ONLY when `step.id` has **never**
    /// self-registered anything at all (`entries[step.id] == nil` — no slot,
    /// hidden or not; the container-with-no-id-of-its-own shape, not exercised
    /// by any apple container today, only kept alive by an existing test's
    /// synthetic fixture). This gate is load-bearing, not an optimization: a
    /// step.id that DOES self-register must resolve from its OWN visibility
    /// alone, or hiding one instance's self-registration while an unrelated
    /// descendant elsewhere still happens to carry the same literal step value
    /// in its path would let the fallback "resurrect" it through that
    /// descendant — silently answering a query for the hidden instance with a
    /// DIFFERENT, still-visible sibling's subtree. Gating on ever-registered
    /// (not currently-visible) is what tells apart "this id never registers
    /// itself" (needs the fallback) from "this id's one registration is just
    /// hidden right now" (must NOT fall back) — `entries` keeps a hidden slot
    /// in place (only `unregister` removes it), so the check is exact. That
    /// fallback's cross-id order is in any case NOT guaranteed (`Dictionary`
    /// iteration), which is exactly why apple containers self-register instead
    /// of relying on it.
    private func firstVisibleWitness(
        of step: AutomationScopeStep, atOrBelow floor: Int, under root: [AutomationScopeStep]
    ) -> [AutomationScopeStep]? {
        for slot in visibleSlots(step.id) {
            guard slot.path.starts(with: root) else { continue }
            for depth in floor..<slot.path.count where slot.path[depth] == step {
                return Array(slot.path[0...depth])
            }
        }
        guard entries[step.id] == nil else { return nil }
        for (otherId, slots) in entries where otherId != step.id {
            for slot in slots where slot.effectivelyVisible {
                guard slot.path.starts(with: root) else { continue }
                for depth in floor..<slot.path.count where slot.path[depth] == step {
                    return Array(slot.path[0...depth])
                }
            }
        }
        return nil
    }

    /// The entries of `id` whose ancestor path is a descendant of the scope's
    /// resolved container (`scopeRoot`), in registration (document) order.
    public func scopedEntries(_ id: String, scope: [AutomationScopeStep]) -> [Entry] {
        guard let root = scopeRoot(scope) else { return [] }
        return visibleSlots(id)
            .filter { $0.path.starts(with: root) }
            .map(\.entry)
    }

    /// Scope-aware count. Path-based (count within the scoped subtree) when `id`
    /// participates and a scope is given; otherwise the legacy global count
    /// (byte-identical to the previous `/element/count`, which ignored scope).
    public func resolvedCount(_ id: String, scope: [AutomationScopeStep]) -> Int {
        guard !scope.isEmpty, hasScopePath(id) else { return count(id) }
        return scopedEntries(id, scope: scope).count
    }

    /// Resolve `id`+`scope`+`leafIndex` to the concrete SLOT — the shared shape
    /// behind every `/element/*` reader (`resolvedEntry`, `resolvedScrollIntoView`,
    /// `resolvedFrame`), since geometry and the scroll closure live on the slot,
    /// not the `Entry`. Path branch: `leafIndex`-th match within the scope's
    /// resolved subtree (descendant matching, `scopeRoot`). Legacy branch (no
    /// scope, or `id` doesn't participate): the flat occurrence index = the
    /// innermost scope step's index, else `leafIndex` — exactly the previous
    /// server behavior.
    private func resolvedSlot(_ id: String, scope: [AutomationScopeStep], leafIndex: Int) -> Slot? {
        let slots = visibleSlots(id)
        if !scope.isEmpty, hasScopePath(id) {
            guard let root = scopeRoot(scope) else { return nil }
            let scoped = slots.filter { $0.path.starts(with: root) }
            return leafIndex < scoped.count ? scoped[leafIndex] : nil
        }
        let flat = scope.last?.index ?? leafIndex
        return flat < slots.count ? slots[flat] : nil
    }

    /// Scope-aware indexed entry — see `resolvedSlot`.
    public func resolvedEntry(_ id: String, scope: [AutomationScopeStep], leafIndex: Int) -> Entry? {
        resolvedSlot(id, scope: scope, leafIndex: leafIndex)?.entry
    }

    /// Scope-aware live window-space frame (top-left origin) — backs
    /// `/element/attr?attr=frame`, the geometry read the harness's
    /// `assert_on_screen` builds on. `nil` for an unresolved scope/index or a
    /// slot whose sentinel is not realized. Slot-level (see `resolvedSlot`):
    /// before 2026-08-24 this attribute was stuck on the flat occurrence index
    /// even under a scoped query (this registry's own prior comment named the
    /// gap), so a scoped frame read could report the wrong row's geometry.
    public func resolvedFrame(_ id: String, scope: [AutomationScopeStep], leafIndex: Int) -> CGRect? {
        resolvedSlot(id, scope: scope, leafIndex: leafIndex)?.geometry?()?.frame
    }

    /// Scope-aware targeted scroll — backs `POST /element/scroll-into-view`. See
    /// `resolvedSlot`: going slot-level is deliberate, `scope="post-card[7]"` has
    /// to scroll the 7th CARD, and a flat index over the leaf id would scroll
    /// some other row entirely — silently, since the wrong row scrolls just as
    /// successfully as the right one.
    ///
    /// `nil` = no such element (the caller 404s). A resolved element that cannot
    /// be scrolled answers through `AutomationScrollOutcome`, not `nil`.
    public func resolvedScrollIntoView(
        _ id: String, scope: [AutomationScopeStep], leafIndex: Int
    ) -> (() -> AutomationScrollOutcome)? {
        guard let slot = resolvedSlot(id, scope: scope, leafIndex: leafIndex) else { return nil }
        // A slot with no sentinel (watchOS, a sentinel-less context) resolves but
        // cannot scroll — report it as such rather than 404ing a real element.
        return slot.scrollIntoView ?? { .detached }
    }

    /// Scope-aware visibility (a missing element is `false`, never an error).
    /// Path branch: ≥1 match within the scoped subtree past `leafIndex`. Legacy
    /// branch: global count > the flat occurrence index (the previous behavior).
    public func resolvedVisible(_ id: String, scope: [AutomationScopeStep], leafIndex: Int) -> Bool {
        if !scope.isEmpty, hasScopePath(id) {
            return scopedEntries(id, scope: scope).count > leafIndex
        }
        return count(id) > (scope.last?.index ?? leafIndex)
    }

    /// All ids with at least one visible slot (for the `/registry` debug dump —
    /// the in-process analogue of an accessibility-tree snapshot).
    public func allIds() -> [String] {
        entries.filter { $0.value.contains { $0.effectivelyVisible } }.keys.sorted()
    }

    /// One visible registered element, as `GET /registry` publishes it.
    ///
    /// The *structured* twin of `debugDump`: same registry, but every field is a
    /// value a caller can assert on rather than a line a human reads. That
    /// difference is the whole point — a text dump can diagnose one failure, but
    /// only a structured list can carry a **general invariant** over every element
    /// on screen (e2e conventions point 17: assert general invariants, not only
    /// hand-picked outcomes). `driver.tree()` is what apple had, and it is why the
    /// offline gate's forcing function stalled at "a MIS-PLACED declaration is
    /// caught, a MISSING one is invisible" — nothing could ask *every* control on
    /// an admin page whether it was actuable.
    public struct ElementSnapshot: Equatable, Sendable {
        /// The test id, as stamped by `.accessibilityIdentifier` / `automation*`.
        public let id: String
        /// The occurrence index **within this id's visible, document-ordered
        /// list** — i.e. exactly the `index=` the driver addresses this element
        /// with, so a violation this list reports can be re-driven verbatim.
        public let index: Int
        /// What `/element/enabled` answers for it right now, `nil` predicate
        /// resolved the same way the server resolves it (`?? true`).
        public let enabled: Bool
        /// Whether an `isEnabled` predicate exists at all, distinguishing
        /// *declared* enabled from *defaulted* enabled. An invariant that flags
        /// live controls needs this to say WHICH kind of gap it found: a call site
        /// that declared nothing, or one whose predicate genuinely answers true.
        public let declaresEnabled: Bool
        /// Has an `activate` closure — the driver can click it. The population an
        /// "every actuable control is disabled here" invariant quantifies over.
        public let actuable: Bool
        /// Has a `setValue` closure — the driver can type into it. Editable
        /// surfaces are actuable in the sense that matters for a gate (typing into
        /// a field whose save cannot happen is the same dead end as clicking a
        /// dead button), so an invariant may quantify over both.
        public let editable: Bool
        /// The ancestor `.automationScope` chain, rendered as the wire DSL
        /// (`post-card[1]/quoted-post`), or `""` for an element under no scoped
        /// container.
        public let scope: String
        /// Live window-space frame (top-left origin), or `nil` when no sentinel is
        /// realized. Lets a report say *where* an offender is without a second run.
        public let frame: CGRect?
        /// What `/element/text` answers for it right now (`nil` = registers no
        /// text reader) — linux's registry record carries the same field, so a
        /// geometry read can tell WHICH block a frame belongs to
        /// (`actions/events.py::_timeline_frames`) in the one round trip.
        public var text: String? = nil
    }

    /// Every VISIBLE registered element, id-sorted then in document order —
    /// the structured registry snapshot backing `GET /registry`.
    ///
    /// Visible-only, deliberately: it is the *screen* an invariant quantifies
    /// over, and a hidden slot is indistinguishable from an absent one everywhere
    /// else on the wire (`count`, `is_visible`), so including them here would make
    /// this the one lookup with different visibility semantics. `debugDump`
    /// remains the everything-including-hidden view for diagnosing why a slot
    /// resolved absent.
    ///
    /// **No cap, no sampling.** A truncated list reads as "checked everything"
    /// when it did not, which is the failure mode an invariant checker exists to
    /// prevent.
    public func snapshot() -> [ElementSnapshot] {
        var rows: [ElementSnapshot] = []
        for id in entries.keys.sorted() {
            for (index, slot) in visibleSlots(id).enumerated() {
                let e = slot.entry
                rows.append(ElementSnapshot(
                    id: id,
                    index: index,
                    enabled: e.isEnabled?() ?? true,
                    declaresEnabled: e.isEnabled != nil,
                    actuable: e.activate != nil,
                    editable: e.setValue != nil && e.typeRefusal?() == nil,
                    scope: slot.path.map { "\($0.id)[\($0.index)]" }.joined(separator: "/"),
                    frame: slot.geometry?()?.frame,
                    text: e.text?()))
            }
        }
        return rows
    }

    // MARK: - Inherited disablement

    /// Fold an **inherited** enabled verdict into an entry's own `isEnabled`.
    ///
    /// SwiftUI's `\.isEnabled` is exactly "has any ancestor disabled me":
    /// cumulative down the view tree and non-revocable by a descendant. Reading it
    /// where the element registers therefore answers *every* cause of disablement
    /// at once — the offline gate's `.disabled(!available)`, a busy-state container,
    /// a `Form` section — where the old shape could only answer the one cause it
    /// had been given a dedicated environment key for.
    ///
    /// Why this matters more than it looks: `isEnabled` used to be a closure each
    /// call site passed **by hand**, and `nil` reads as *enabled*
    /// (`InProcessAutomationServer`: `e.isEnabled?() ?? true`). A control greyed
    /// only because an ancestor disabled it therefore reported `enabled=true` to
    /// the driver — the driver's answer and the real UI disagreeing in the
    /// direction that costs debugging, because a test then actuates it, nothing
    /// happens, and the failure presents as a *product* bug. That is e2e
    /// conventions point 11's concern applied to the enabled read rather than to
    /// command dispatch, and `strictEnabled` could not save you: it consults the
    /// very predicate that is `nil`.
    ///
    /// Enabled inherits unchanged — deliberately returning the entry **as-is**
    /// rather than wrapping its predicate, so the overwhelmingly common case keeps
    /// a `nil` `isEnabled` (same `/element/enabled` answer, same `debugDump`
    /// capability letters, no allocation per body pass). Disabled short-circuits to
    /// `false`: an ancestor's disable cannot be argued out of by the control's own
    /// predicate, so there is nothing left to ask.
    ///
    /// ⚠ **Ancestors only, and that is a SwiftUI fact, not a shortcut.** The
    /// verdict is whatever the environment holds *where the `automation*` modifier
    /// is applied*, so a `.disabled(...)` written **outside** the automation
    /// modifier (the common order — `.automationActivate(…).disabled(…)`, and any
    /// container-level disable) is seen, while one written **inside** it
    /// (`.disabled(…).automationActivate(…)`) is not: there the automation modifier
    /// is the ancestor. Those call sites are the ones that already hand-mirror the
    /// predicate into `isEnabled:`, which still wins on specificity, so the fold is
    /// a strict improvement over the previous answer everywhere and a complete one
    /// wherever the disable is an ancestor.
    public static func folding(_ entry: Entry, inheritedEnabled: Bool) -> Entry {
        guard !inheritedEnabled else { return entry }
        var folded = entry
        folded.isEnabled = { false }
        return folded
    }

    /// Full diagnostic dump backing the `/tree` debug endpoint: EVERY slot of
    /// every id — hidden ones included — with the exact inputs
    /// `effectivelyVisible` decides on (live sentinel geometry, off-screen
    /// votes) plus the ancestor scope path and the Entry's capability letters
    /// (A=activate D=doubleActivate T=text V=value S=setValue E=isEnabled).
    ///
    /// One failing e2e run's `driver.tree()` should carry everything needed to
    /// say WHY a lookup resolved absent — geometry-parked, vote-hidden,
    /// geometry-less, or a scope-path mismatch — without a second instrumented
    /// run (e2e conventions point 6: failures diagnose themselves). The bare
    /// `allIds()` dump this replaces could only say an id existed, which left
    /// geometry-vs-votes-vs-path diagnoses to guesswork across whole sessions.
    public func debugDump() -> String {
        var lines: [String] = []
        for id in entries.keys.sorted() {
            let slots = entries[id] ?? []
            lines.append("\(id) (\(count(id))/\(slots.count) visible)")
            for (i, slot) in slots.enumerated() {
                let geo = slot.geometry?()
                let state: String
                if let geo {
                    state = geo.isOnScreen
                        ? "VISIBLE(geo)"
                        : (geo.windowVisible ? "HIDDEN(geo-parked)" : "HIDDEN(window-hidden)")
                } else {
                    state = (slot.sawDisappear && slot.sawDetach)
                        ? "HIDDEN(votes)" : "VISIBLE(no-geo)"
                }
                let g = geo.map {
                    String(format: "(%.0f,%.0f %.0fx%.0f in %.0fx%.0f)",
                           $0.frame.minX, $0.frame.minY, $0.frame.width, $0.frame.height,
                           $0.windowBounds.width, $0.windowBounds.height)
                } ?? "nil"
                var votes: [String] = []
                if slot.sawDisappear { votes.append("disappear") }
                if slot.sawDetach { votes.append("detach") }
                let path = slot.path.map { "\($0.id)[\($0.index)]" }.joined(separator: "/")
                var caps = ""
                if slot.entry.activate != nil { caps += "A" }
                if slot.entry.doubleActivate != nil { caps += "D" }
                if slot.entry.text != nil { caps += "T" }
                if slot.entry.value != nil { caps += "V" }
                if slot.entry.setValue != nil { caps += "S" }
                if slot.entry.isEnabled != nil { caps += "E" }
                lines.append(
                    "  [\(i)] \(state) geo=\(g)"
                    + " votes=\(votes.isEmpty ? "-" : votes.joined(separator: "+"))"
                    + " path=\(path.isEmpty ? "-" : path) caps=\(caps)")
            }
        }
        return lines.joined(separator: "\n")
    }
}

#endif

// MARK: - View modifiers

public extension View {
    /// Register an `activate` closure for `id` (a tappable control). The closure
    /// is the same action the control performs, so the automation server's
    /// `/element/click` fires the *real* handler — exercising the production code
    /// path, not a shortcut. No-op in production.
    ///
    /// Pass `isEnabled` for a control that can be `.disabled(...)` so
    /// `/element/enabled` (the driver's `is_enabled`/`is_disabled`) reflects the
    /// real interactivity — mirror the same predicate the `.disabled(...)`
    /// modifier uses. Convention: the `perform` closure must invoke the same
    /// method/symbol the control's own action does (extract to a named method
    /// when it's more than a single call) so the two never silently diverge.
    /// Pass `value` for a control that is **both** actuatable and readable —
    /// a Toggle (read "on"/"off") or a cycle-button (read the current option).
    /// It registers the activate AND the read in the **same** `Entry`, so the
    /// server's index-0 lookup serves both `/element/click` and
    /// `/element/text`/`attr`. (Applying a separate `.automationValue(id,…)`
    /// modifier for the same id would create a *second* entry, splitting the
    /// activate and read across two index slots — use this instead.)
    ///
    /// Pass `doubleActivate` for a control that also has a distinct double-click
    /// action (the Outlook `events-day-cell`: single-click drills into Day view,
    /// double-click opens new-event compose). It backs `/element/double_click`;
    /// callers without one need not pass it (the server falls back to the single
    /// activate). The real SwiftUI view should *also* wire `.onTapGesture(count:)`
    /// gestures so a human gets the same two behaviors.
    ///
    /// Pass `text` when the control's **rendered label** differs from `value` and
    /// is what a cross-app test matches on. `/element/text` reads `text` first and
    /// only falls back to `value`, so a repeated row can expose a stable machine
    /// identity (`value`) *and* the human-visible string every other app's
    /// driver returns for the same id (`text`) — the `calendar-event-block`
    /// shape: `value` is the event id (events.md § Week & day timeline views),
    /// `text` is the summary, which is what web/windows/tui/android blocks read
    /// back. Without it the apple text read is the only one in the fleet that
    /// answers with an opaque id, and an identity assert can't be written once
    /// for all 7 apps.
    ///
    /// `attributes` publishes named reads beyond `text`/`value` (`Entry.attributes`).
    ///
    /// Pass `enterText` (+ `typeRefusal`) for an *entry mode* button — one whose
    /// free-entry form is an input carrying the button's own id while it is open
    /// (the fuller reaction picker). `enterText` writes the input's binding, the
    /// same storage a keystroke or an OS emoji panel fills; `typeRefusal` names why
    /// a type is refused while the input is closed (`Entry.typeRefusal`). One
    /// `Entry` for both, so the id never splits across two index slots.
    func automationActivate(
        _ id: String,
        isEnabled: (() -> Bool)? = nil,
        text: (() -> String?)? = nil,
        value: (() -> String?)? = nil,
        attributes: (() -> [String: String])? = nil,
        doubleActivate: (() -> Void)? = nil,
        enterText: ((String) -> Void)? = nil,
        typeRefusal: (() -> String?)? = nil,
        perform action: @escaping () -> Void
    ) -> some View {
        #if DEBUG
        modifier(_AutomationRegister(id: id, entry: .init(
            activate: action, doubleActivate: doubleActivate,
            text: text, value: value, setValue: enterText, isEnabled: isEnabled,
            attributes: attributes, typeRefusal: typeRefusal)))
        #else
        self
        #endif
    }

    /// Register a live text/value reader for `id` (a label / read-only field /
    /// toggle). `text` backs `/element/text`; `value` backs
    /// `/element/attr?attr=value` and the toggle `state`; `attributes` publishes
    /// named reads beyond them (`Entry.attributes`), as on `automationActivate`.
    /// No-op in production.
    func automationValue(
        _ id: String,
        text: (() -> String?)? = nil,
        value: (() -> String?)? = nil,
        isEnabled: (() -> Bool)? = nil,
        attributes: (() -> [String: String])? = nil
    ) -> some View {
        #if DEBUG
        modifier(_AutomationRegister(id: id, entry: .init(
            text: text, value: value, isEnabled: isEnabled, attributes: attributes)))
        #else
        self
        #endif
    }

    /// Register an editable text field for `id`, taking its `Binding` **once** so
    /// there is no value duplication. The bound storage backs the write path
    /// (`/element/type` and `/element/clear` → `setValue`) and the read path
    /// (`/element/text`). The text-field analogue of `automationActivate`:
    /// `setValue` mutates the *same* `@State`/binding the field is bound to, so
    /// the typed value reaches the real view model exactly as a user keystroke
    /// would. Applied alongside `.accessibilityIdentifier(id)`. No-op in production.
    ///
    /// `visibleText` is the optional **rendered**-text reader for a field whose display
    /// differs from its source — the compose field, whose markdown markers may be
    /// concealed. It backs `/element/attr?attr=visible` and leaves `/element/text`
    /// answering the source, so a draft read-back and a "what does the user see" read
    /// stay distinguishable (`Entry.visibleText`).
    ///
    /// `textRuns` / `pressKey` are the compose field's live-view doors (`Entry.textRuns`,
    /// `Entry.pressKey`): the applied-styling read and the caret-key door, both answered by
    /// the real text view behind the field.
    ///
    /// `commit` is the field's own commit action, for a type-then-commit input
    /// (`input_commit` in ui.yaml): typing writes only the draft, and the
    /// commit — what Return does for a user — backs `/element/click` and, with
    /// no `pressKey` door, `/element/key Enter`, the idiom the cross-app
    /// action layer drives such an input with on every app.
    func automationField(
        _ id: String,
        text: Binding<String>,
        isEnabled: (() -> Bool)? = nil,
        visibleText: (() -> String?)? = nil,
        textRuns: (() -> String?)? = nil,
        pressKey: ((String) -> String?)? = nil,
        commit: (() -> Void)? = nil
    ) -> some View {
        #if DEBUG
        modifier(_AutomationRegister(id: id, entry: .init(
            activate: commit,
            text: { text.wrappedValue },
            value: { text.wrappedValue },
            setValue: { text.wrappedValue = $0 },
            isEnabled: isEnabled,
            visibleText: visibleText,
            textRuns: textRuns,
            pressKey: pressKey)))
        #else
        self
        #endif
    }

    /// Register a Picker / menu / value-set control for `id`. `value` reads the
    /// current selection (backs `/element/text` + `/element/attr`); `set` maps a
    /// wire option string to the bound selection — the same mutation choosing
    /// the menu item performs — and backs `/element/select`. Use for any control
    /// the driver drives with `select(id, value)` (SwiftUI `Picker`, a custom
    /// menu, …). For a String-backed selection, `set` is just the binding
    /// setter; for an enum-backed one, `set` maps the wire string to the case.
    /// One `Entry` (read + select together). No-op in production.
    ///
    /// `options` is the picker's currently-rendered choices, read live like
    /// every other closure here — pass it so `/element/select` can refuse a
    /// value this frame never painted (convention 11's twin rule,
    /// `e2e-conventions.md`) instead of writing it through unchecked. Default
    /// `nil` keeps a not-yet-converted call site's existing write-through
    /// behaviour; every call site should gain one as it's converted (tracked in
    /// `e2e-conventions.md` § Implementation status today).
    func automationSelect(
        _ id: String,
        value: @escaping () -> String?,
        options: (() -> [String])? = nil,
        isEnabled: (() -> Bool)? = nil,
        set: @escaping (String) -> Void
    ) -> some View {
        #if DEBUG
        modifier(_AutomationRegister(id: id, entry: .init(
            value: value, setValue: set, options: options, isEnabled: isEnabled)))
        #else
        self
        #endif
    }

    /// Mark this view as a **scoped container** for the in-process driver: every
    /// `automation*` element inside it captures `(id, index)` as one step of its
    /// ancestor scope path, so scoped queries (`scope="post-card[1]/quoted-post"`)
    /// resolve by real subtree containment instead of the flat occurrence-index
    /// heuristic. `index` is the container's position among its same-id siblings
    /// within the *parent* scope (a `ForEach` row's offset; `0` for a singleton
    /// like the one `quoted-post` per `post-card`).
    ///
    /// Apply it where the index is known — at the `ForEach` row for list items
    /// (`MacPostCardView(...).automationScope("post-card", index: offset)`), or
    /// inside the component itself for a singleton (`QuotedPostCard`). The
    /// container's own `.accessibilityIdentifier` / `.automationValue` is
    /// unaffected (a container self-registers with its own step in the path — the
    /// self-scoping idiom `scopeRoot` resolves as witness 1, not a second scope
    /// level). No-op cost in production
    /// (`AutomationRegistry.isEnabled`), like every `automation*` modifier.
    ///
    /// This is the apple analogue of how linux's `scope_root` descends the real
    /// GTK widget tree (`apps/fauna-linux/src/automation/find.rs`) — apple has no
    /// queryable tree, so containers push their identity down the SwiftUI
    /// environment for leaves to record (priorities #1/#3).
    func automationScope(_ id: String, index: Int = 0) -> some View {
        #if DEBUG
        modifier(_AutomationScope(id: id, index: index))
        #else
        self
        #endif
    }

    /// Declare, on the view that HOSTS a `.alert` / `.confirmationDialog`, that
    /// `ids` live only while that presentation is up.
    ///
    /// A presentation's content builder is the one place this file's lifecycle
    /// is structurally blind: the `_AttachmentSentinel` rides in
    /// `.background(...)`, which an alert never realizes, so no window-detach
    /// and no probe death can ever fire; and SwiftUI does not fire
    /// `.onDisappear` for alert content. `.onAppear` does fire, so the content's
    /// ids register on present and then read visible for the rest of the
    /// process — `is_visible` on a dismissed confirm answers `true` forever,
    /// which is what left `folder-residency-confirm` stranded (`AutomationRegistry
    /// .hideAll` carries the measurement). The host's own `isPresented` binding
    /// is the only thing that knows, so it is what declares the lifetime.
    ///
    /// Put it beside the `.alert` and list the ids that alert registers:
    ///
    ///     .alert(title, isPresented: $armed) { Button(…).automationActivate(Ids.fooConfirm) {…} }
    ///     .automationPresentation(ids: [Ids.fooConfirm], isPresented: armed)
    ///
    /// An invisible shim rendered only while dismissed would be the other way to
    /// answer the driver, and e2e convention 1 forbids it. No-op cost in
    /// production (`AutomationRegistry.isEnabled`), like every `automation*`
    /// modifier.
    func automationPresentation(ids: [String], isPresented: Bool) -> some View {
        #if DEBUG
        modifier(_AutomationPresentation(ids: ids, isPresented: isPresented))
        #else
        self
        #endif
    }
}

#if DEBUG

/// Retires `ids` when the host's presentation binding goes false. Internal —
/// use the `automationPresentation(ids:isPresented:)` `View` extension, which
/// carries the rationale.
///
/// ⚠ The TRANSITION only — deliberately NOT `initial: true`. The registry is
/// process-wide while this modifier is per-host, and these hosts repeat: one
/// `FolderRowBody` per folder, each with its own `@State` bindings. Firing on
/// first render would make a row appearing for any reason (a new folder landing
/// from another device, a list rebuild that changes identity) call `hideAll`
/// for ids a DIFFERENT row currently has armed — blanking a live confirm. A
/// true→false transition cannot do that: it only ever follows this host's own
/// presentation, and only one alert is up at a time. An already-dismissed
/// alert needs no initial sweep either, because its own dismissal is what
/// retired it.
public struct _AutomationPresentation: ViewModifier {
    let ids: [String]
    let isPresented: Bool

    public func body(content: Content) -> some View {
        if AutomationRegistry.isEnabled {
            content.onChange(of: isPresented) { _, presented in
                guard !presented else { return }
                for id in ids { AutomationRegistry.shared.hideAll(id) }
            }
        } else {
            content
        }
    }
}

/// SwiftUI environment carrier for the current ancestor scope path. Each
/// `.automationScope` reads the inherited path and injects the extended path into
/// its subtree; `_AutomationRegister` reads it to tag the registered slot.
private struct AutomationScopePathKey: EnvironmentKey {
    static let defaultValue: [AutomationScopeStep] = []
}

public extension EnvironmentValues {
    var automationScopePath: [AutomationScopeStep] {
        get { self[AutomationScopePathKey.self] }
        set { self[AutomationScopePathKey.self] = newValue }
    }
}

/// Pushes `(id, index)` onto the inherited scope path for the modified view's
/// subtree (env-only, gated so it costs nothing in production). Internal — use
/// the `automationScope` `View` extension.
public struct _AutomationScope: ViewModifier {
    @Environment(\.automationScopePath) private var inherited
    let id: String
    let index: Int

    public func body(content: Content) -> some View {
        if AutomationRegistry.isEnabled {
            content.environment(
                \.automationScopePath,
                inherited + [AutomationScopeStep(id: id, index: index)])
        } else {
            content
        }
    }
}

#endif

/// A `Text` label that **registers its own read**, taking its string **once**.
///
/// The read-side analogue of `automationField` (which takes a field's binding
/// once): the same expression backs both the rendered `Text` *and* the
/// registered `/element/text` read, so the two can never drift. This is the
/// scaling fix for the read surface — labels vastly outnumber buttons, and
/// hand-writing a `Text(expr)` + a matching `.automationValue(id, text: { expr })`
/// per label is hundreds of duplicated sites and a permanent drift hazard
/// (apple-e2e-automation.md § read ergonomics; the macOS e2e harness advisory's
/// ratified point 1).
///
/// `text` is an `@autoclosure @escaping () -> String`, so a **dynamic** label
/// (derived from live view-model state) re-reads current state on every registry
/// lookup, exactly as an inline `Text(expr)` re-evaluates each body pass — and a
/// **static** i18n constant is just as terse:
///
/// ```swift
/// automationText("page-heading", L.onboarding.handle.prompt).font(.title2)
/// automationText("handle-message-area", renderLocalizedText(snap.message))
/// ```
///
/// Applies `.accessibilityIdentifier(id)` internally — kept as the real-a11y
/// carrier and the id the still-default XCUITest path finds (the literal id stays
/// at the call site, so `ui-actual-lint` and grep still see it) — then styling
/// chains normally (`.font`, `.foregroundStyle`, `.multilineTextAlignment`, …).
/// The registration is env-gated, so in production this is a plain `Text`.
public func automationText(
    _ id: String,
    _ text: @autoclosure @escaping () -> String
) -> some View {
    Text(text())
        .accessibilityIdentifier(id)
        .automationValue(id, text: text)
}

/// A `ProgressView` that **registers its own automation read** — the
/// `%.2f`-formatted fraction backs `/element/attr?attr=value`, matching the
/// id passed to `.accessibilityIdentifier`. The `ProgressView` analogue of
/// `automationText` above; consolidates MailImportView/MailExportView's
/// identical shape.
public func automationProgressBar(
    _ id: String,
    fraction: Double
) -> some View {
    ProgressView(value: fraction)
        .accessibilityIdentifier(id)
        .automationValue(id, text: {
            String(format: "%.2f", fraction)
        })
}

#if DEBUG

/// Per-view-identity lifecycle anchor. Holds the stable `token` (so the matching
/// `hide`/`unregister` targets *this* view's slot, not the most-recently-placed
/// one), a weak handle to the window-attachment sentinel, and — being a class
/// held in `@State` — a **deinit that fires exactly when SwiftUI destroys the
/// view identity's state storage**. That deinit is the one true "this element
/// can never come back" signal, so it is where the slot is actually removed;
/// every earlier off-screen signal only hides.
@MainActor
private final class _AutomationLifecycle {
    let token = UUID()
    /// The id this token registered under — adopted on the first body pass so
    /// `deinit` knows which slot to remove.
    private(set) var id: String?

    #if os(iOS) || os(macOS)
    /// The attachment sentinel's backing platform view, if it has been realized.
    /// `.onDisappear` consults it to distinguish a COVER (a `NavigationStack`
    /// detail pushed over a still-attached root — ignore, the element is merely
    /// overlaid and pop restores it with no further signal) from a real
    /// off-screen transition (window detached — hide).
    weak var sentinel: _AttachmentProbeView?
    #endif

    func adopt(id: String) {
        if self.id == nil { self.id = id }
    }

    /// The sentinel view's live geometry — frame + hosting-window bounds, both
    /// normalized to a TOP-LEFT origin so ascending `(y, x)` is document order
    /// everywhere (AppKit window coordinates are bottom-up). `nil` while
    /// detached or before the sentinel is realized.
    var sentinelGeometry: SentinelGeometry? {
        #if os(iOS) || os(macOS)
        guard let view = sentinel, let window = view.window else { return nil }
        let r = view.convert(view.bounds, to: nil)
        #if os(macOS)
        let h = window.contentView?.bounds.height ?? window.frame.height
        return SentinelGeometry(
            frame: CGRect(x: r.minX, y: h - r.maxY, width: r.width, height: r.height),
            windowBounds: CGRect(x: 0, y: 0, width: window.frame.width, height: h),
            windowVisible: window.isVisible)
        #else
        return SentinelGeometry(
            frame: r,
            windowBounds: CGRect(origin: .zero, size: window.bounds.size),
            windowVisible: !window.isHidden)
        #endif
        #else
        return nil
        #endif
    }

    /// Centre the sentinel in EVERY enclosing scroll view, innermost outward —
    /// the real scroll behind `POST /element/scroll-into-view`.
    ///
    /// **Nearest-ancestor, not largest.** The retired XCUITest bridge picked the
    /// *largest* scroll view on screen, which is why it scrolled the background
    /// window instead of a presented sheet (`e2e_status_and_tips.md`, bucket-3
    /// failure class (b): "`scrollIntoView` picks the largest (background-window)
    /// scroll view, not the sheet's"). Walking up from the element's own sentinel
    /// makes that class unrepresentable — a sheet's rows resolve the sheet's
    /// scroll view because that is what they are inside. Same reason the walk
    /// continues OUTWARD past the first hit: the nested single-scroll
    /// `PreferencesView` panes need the inner pane AND the outer shell scrolled,
    /// the case the old bridge documented as out of reach.
    ///
    /// **Centred, not merely visible.** `scrollToVisible`-style minimal scrolling
    /// leaves the element edge-aligned at the fold, and windows' FlaUI bridge
    /// flips `!IsOffscreen` at ~25% visibility — under the shared 500‰/750‰
    /// engagement-cue gates, so an edge-aligned "success" would report a dwell
    /// the user never had. Centring clears both gates by construction.
    ///
    /// The scroll is driven through the real clip view / content offset — the
    /// same state a viewport observer samples — so a dwell measured after it is
    /// an honest exposure, not an injected one (the linux agent's
    /// `scroll_into_view` makes the identical point about its vadjustment).
    @MainActor
    func scrollSentinelIntoView() -> AutomationScrollOutcome {
        #if os(iOS) || os(macOS)
        guard let view = sentinel, view.window != nil else { return .detached }
        var scrolledAny = false
        #if os(macOS)
        var probe: NSView = view
        while let scrollView = probe.enclosingScrollView {
            // Always centre the ORIGINAL sentinel, never the inner scroll view:
            // centring a tall inner pane inside the outer shell would park the
            // element itself anywhere at all.
            if let doc = scrollView.documentView {
                let target = view.convert(view.bounds, to: doc)
                let clip = scrollView.contentView
                let viewport = clip.bounds
                var origin = viewport.origin
                // Both rects are in document coordinates, so this centres
                // correctly whether or not the document view is flipped.
                origin.y = target.midY - viewport.height / 2
                if doc.bounds.width > viewport.width {
                    origin.x = target.midX - viewport.width / 2
                }
                origin.y = min(max(0, origin.y), max(0, doc.bounds.height - viewport.height))
                origin.x = min(max(0, origin.x), max(0, doc.bounds.width - viewport.width))
                clip.scroll(to: origin)
                scrollView.reflectScrolledClipView(clip)
                scrolledAny = true
            }
            probe = scrollView
        }
        #else
        var probe: UIView? = view.superview
        while let current = probe {
            if let scrollView = current as? UIScrollView {
                // Converting INTO a UIScrollView yields content coordinates —
                // its bounds origin IS the content offset.
                let target = view.convert(view.bounds, to: scrollView)
                let inset = scrollView.adjustedContentInset
                let viewportH = scrollView.bounds.height - inset.top - inset.bottom
                let viewportW = scrollView.bounds.width - inset.left - inset.right
                var offset = scrollView.contentOffset
                offset.y = target.midY - viewportH / 2 - inset.top
                if scrollView.contentSize.width > viewportW {
                    offset.x = target.midX - viewportW / 2 - inset.left
                }
                let maxY = max(-inset.top,
                               scrollView.contentSize.height + inset.bottom - scrollView.bounds.height)
                let maxX = max(-inset.left,
                               scrollView.contentSize.width + inset.right - scrollView.bounds.width)
                offset.y = min(max(-inset.top, offset.y), maxY)
                offset.x = min(max(-inset.left, offset.x), maxX)
                scrollView.setContentOffset(offset, animated: false)
                scrolledAny = true
            }
            probe = current.superview
        }
        #endif
        // Re-read AFTER scrolling: the clip-view/content-offset writes above are
        // synchronous, so this is the settled post-scroll position, no wait
        // needed (and none wanted — testing.md convention 14).
        return scrolledAny ? .scrolled(sentinelGeometry) : .noScrollableAncestor
        #else
        return .detached
        #endif
    }

    /// Three-valued: `true`/`false` = the sentinel's window attachment; `nil` =
    /// no sentinel realized (yet, or ever — a platform/context without one), in
    /// which case callers fall back to trusting `.onAppear`/`.onDisappear` alone,
    /// the pre-sentinel lifecycle.
    var sentinelAttached: Bool? {
        #if os(iOS) || os(macOS)
        guard let sentinel else { return nil }
        return sentinel.window != nil
        #else
        return nil
        #endif
    }

    deinit {
        guard let id else { return }
        let token = token
        // deinit is nonisolated; the registry is MainActor. State teardown can
        // land off the render pass, so hop rather than assume. The slot is
        // already hidden by then (detach/`.onDisappear` precede identity death),
        // so the deferred removal is invisible to lookups.
        Task { @MainActor in
            AutomationRegistry.shared.unregister(id, token: token)
        }
    }
}

#if os(iOS) || os(macOS)
#if os(iOS)
typealias _PlatformView = UIView
#else
typealias _PlatformView = NSView
#endif

/// Zero-cost invisible platform view whose only job is reporting **window
/// attachment** transitions. Window attachment is the signal
/// `.onAppear`/`.onDisappear` fails to be: BALANCED. UIKit/AppKit re-attach a
/// `NavigationStack` root's view on pop and a re-selected `TabView` tab's view
/// on switch-back — transitions for which SwiftUI fires no `.onAppear`, because
/// the view identity never left the hierarchy (iOS Gap B).
/// Internal rather than `fileprivate` only so `AutomationRegistryTests` can drive
/// the `deinit` vote below headlessly (`@testable import`) — it is a real
/// regression surface, and the leading underscore is the access contract that
/// matters: nothing outside this file constructs one in production.
final class _AttachmentProbeView: _PlatformView {
    var onWindowChange: (@MainActor () -> Void)?

    /// The slot this probe backs, re-captured on every `wire`, so the death hook
    /// below can vote without reaching into the SwiftUI graph.
    var slotId: String?
    var slotToken: UUID?
    /// Set on the OUTGOING probe when SwiftUI re-makes the representable for a
    /// still-live view identity (`makeProbe`), so its death is understood as
    /// bookkeeping rather than an exit. A plain stored flag on `self`, because a
    /// nonisolated `deinit` may read its own storage but not another object's
    /// `@MainActor` state.
    var superseded = false

    #if os(iOS)
    override func didMoveToWindow() {
        super.didMoveToWindow()
        onWindowChange?()
    }
    #else
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        onWindowChange?()
    }
    #endif

    /// A DEALLOCATED probe is definitively off screen — the detach vote of last
    /// resort, and the only one that survives a `NavigationStack` pop.
    ///
    /// `didMoveToWindow(nil)` is not guaranteed: popping a
    /// `.navigationDestination(isPresented:)` destination can tear the hosting
    /// views down outright, so the window-detach vote never arrives and the slot
    /// is stranded on a lone `.onDisappear` — the pooled-cell zombie that had the
    /// driver resolving occurrence [0], a dead form instance, while the live form
    /// kept its own state. `_AutomationLifecycle.deinit` cannot stand in for this:
    /// SwiftUI holds the popped destination's `@State` well past the pop, which is
    /// why three generations of the same ids were observed alive at once.
    ///
    /// Safe against churn by construction. A replacement probe marks this one
    /// `superseded` as it takes the slot over, so representable churn on a live
    /// view identity votes nothing. And even a vote that somehow lands early
    /// cannot hide a visible element: the successor re-attaches with live
    /// geometry, and geometry-FIRST ignores votes for an attached slot.
    deinit {
        guard !superseded, let slotId, let slotToken else { return }
        // deinit is nonisolated and the registry is `@MainActor`; hop rather than
        // assume, exactly as `_AutomationLifecycle.deinit` does.
        Task { @MainActor in
            AutomationRegistry.shared.hide(slotId, token: slotToken, signal: .windowDetach)
        }
    }
}

/// The window-attachment sentinel: a representable placed in the registered
/// element's `.background(...)`, so it attaches/detaches together with the
/// element's own backing view. Attach → `register` (upsert — restores a hidden
/// slot in document order); detach → `hide`. The registration payload is
/// re-wired on every SwiftUI update, so an attach fires with the latest body
/// pass's closures — the same freshness invariant `refresh` maintains.
private struct _AttachmentSentinel {
    let lifecycle: _AutomationLifecycle
    let id: String
    let path: [AutomationScopeStep]
    let entry: AutomationRegistry.Entry

    @MainActor
    private func makeProbe() -> _AttachmentProbeView {
        let view = _AttachmentProbeView()
        view.isHidden = false
        #if os(iOS)
        view.isUserInteractionEnabled = false
        view.isAccessibilityElement = false
        view.backgroundColor = .clear
        #endif
        // Hand the slot over: the probe being replaced must not fire the death
        // vote for a view identity that is still very much alive.
        lifecycle.sentinel?.superseded = true
        lifecycle.sentinel = view
        wire(view)
        return view
    }

    @MainActor
    private func wire(_ view: _AttachmentProbeView) {
        // Capture THIS update's payload; the closure is replaced wholesale on the
        // next update, never mutated.
        let token = lifecycle.token
        let id = id, path = path, entry = entry
        // The death hook's payload (see `_AttachmentProbeView.deinit`), refreshed
        // with the rest of this update rather than only at `makeProbe`, so it can
        // never lag the identity the probe is currently backing.
        view.slotId = id
        view.slotToken = token
        view.onWindowChange = { [weak view, weak lifecycle] in
            guard let view else { return }
            if view.window != nil {
                AutomationRegistry.shared.attach(
                    id, token: token, path: path,
                    geometry: { lifecycle?.sentinelGeometry },
                    scrollIntoView: { lifecycle?.scrollSentinelIntoView() ?? .detached },
                    entry)
            } else {
                AutomationRegistry.shared.hide(id, token: token, signal: .windowDetach)
            }
        }
    }
}

#if os(iOS)
extension _AttachmentSentinel: UIViewRepresentable {
    func makeUIView(context: Context) -> _AttachmentProbeView { makeProbe() }
    func updateUIView(_ view: _AttachmentProbeView, context: Context) { wire(view) }
}
#else
extension _AttachmentSentinel: NSViewRepresentable {
    func makeNSView(context: Context) -> _AttachmentProbeView { makeProbe() }
    func updateNSView(_ view: _AttachmentProbeView, context: Context) { wire(view) }
}
#endif
#endif

/// Registers `entry` for `id` while the modified view is on screen and
/// **re-registers its closures on every body pass**, gated on
/// `AutomationRegistry.isEnabled` so it costs nothing in production. Internal —
/// use the `automation*` `View` extensions.
///
/// Lifecycle (three signals, each answering a different question):
///
///  * **Show** — window attach (`_AttachmentSentinel`), with `.onAppear` as an
///    upsert-idempotent fallback. Attachment is balanced where
///    `.onAppear`/`.onDisappear` is not: a `NavigationStack` pop re-attaches the
///    root it uncovers but fires no `.onAppear` (the root never structurally
///    left), which under the old remove-on-disappear lifecycle permanently
///    deregistered every root-level element after one detail push (iOS Gap B).
///  * **Off screen** — window detach → `hide` (slot kept, in place, so the next
///    show restores document order). `.onDisappear` also hides, but only when
///    the sentinel agrees the view is really detached (or no sentinel exists) —
///    a disappear while still attached is a cover, not an exit.
///  * **Gone** — `@State` storage death (`_AutomationLifecycle.deinit`) →
///    `unregister` actually removes the slot. Only identity death removes: it is
///    the one signal that cannot be followed by a comeback.
public struct _AutomationRegister: ViewModifier {
    let id: String
    let entry: AutomationRegistry.Entry
    /// Stable per-view-identity anchor (token + sentinel handle + death hook).
    /// `@State`'s default initializer runs once per view identity and survives
    /// body re-evaluations, so a given list row keeps one token across its
    /// lifetime (and a sibling row that disappears can't pop this row's slot).
    @State private var lifecycle = _AutomationLifecycle()
    /// The ancestor scope path (chain of enclosing `.automationScope` containers)
    /// in effect where this element appears — captured at registration so scoped
    /// queries can filter by real subtree containment. Empty when the element is
    /// under no scoped container.
    @Environment(\.automationScopePath) private var scopePath
    /// SwiftUI's cumulative "has any ancestor disabled me" verdict, read where
    /// the element registers. A control that passes no `isEnabled` of its own
    /// reports it, so `/element/enabled` cannot say "clickable" while the paint
    /// says greyed — the drift the hand-mirroring convention invites, for
    /// **every** cause of disablement rather than the one the offline gate used
    /// to publish through a dedicated environment key of its own.
    @Environment(\.isEnabled) private var inheritedEnabled

    /// `entry` with the inherited disablement folded into its `isEnabled`
    /// (`AutomationRegistry.folding`, which carries the full rationale). Every
    /// registration path must use THIS and never the raw `entry` — the window-attach
    /// path registering the unfolded one is how a re-attached control silently
    /// regained `enabled=true` after a `NavigationStack` pop or a tab return.
    private var registeredEntry: AutomationRegistry.Entry {
        AutomationRegistry.folding(entry, inheritedEnabled: inheritedEnabled)
    }

    public func body(content: Content) -> some View {
        if AutomationRegistry.isEnabled {
            // Every body pass rebuilds `entry` with closures capturing THIS pass's
            // values, so the registry must take the fresh ones — `.onAppear` alone
            // fires once per view identity and would freeze the first render's
            // closures forever (see `AutomationRegistry.refresh`). `refresh` only
            // swaps the closures of a slot that already exists; visibility and
            // order are the lifecycle signals' business, below.
            //
            // Safe to mutate during view update: nothing in the SwiftUI graph
            // observes the registry (it is read over HTTP by the automation server),
            // so this invalidates no view. `let _ =` is the standard ViewBuilder
            // idiom for a per-pass side effect — it keeps the result opaque, where
            // an `AnyView` wrapper would cost production renders.
            let _ = lifecycle.adopt(id: id)
            let _ = AutomationRegistry.shared.refresh(
                id, token: lifecycle.token, path: scopePath,
                geometry: { [weak lifecycle] in lifecycle?.sentinelGeometry },
                scrollIntoView: { [weak lifecycle] in
                    lifecycle?.scrollSentinelIntoView() ?? .detached
                }, registeredEntry)
            withSentinel(content)
                .onAppear {
                    AutomationRegistry.shared.register(
                        id, token: lifecycle.token, path: scopePath,
                        geometry: { [weak lifecycle] in lifecycle?.sentinelGeometry },
                        scrollIntoView: { [weak lifecycle] in
                            lifecycle?.scrollSentinelIntoView() ?? .detached
                        }, registeredEntry)
                }
                .onDisappear {
                    // A disappear is a COVER only while the sentinel is still
                    // attached; detached or sentinel-less, it is a real exit and
                    // votes definitively (`hideSignal` carries the full rationale
                    // and the pop-leak it fixes).
                    let signal = AutomationRegistry.hideSignal(
                        sentinelAttached: lifecycle.sentinelAttached)
                    AutomationRegistry.shared.hide(id, token: lifecycle.token, signal: signal)
                }
        } else {
            content
        }
    }

    /// The window-attachment sentinel rides in `.background`, so it attaches and
    /// detaches with the element's own backing view. Platform-gated: watchOS has
    /// no representable sentinel (and no e2e driver) — there the lifecycle is
    /// `.onAppear`/`.onDisappear` alone, as before.
    @ViewBuilder
    private func withSentinel(_ content: Content) -> some View {
        #if os(iOS) || os(macOS)
        content.background(
            // `registeredEntry`, NOT `entry`: the sentinel's attach path calls
            // `AutomationRegistry.attach` with whatever payload it was last wired
            // with, so handing it the raw entry made a window RE-attach quietly
            // overwrite the slot with an unfolded `isEnabled` — a control greyed by
            // an ancestor reported `enabled=true` again from the moment it came
            // back on screen (a `NavigationStack` pop, a tab return, a row
            // scrolling into an eagerly realized list), until some later body pass
            // happened to `refresh` it. Every registration path takes the folded
            // entry or the fold is only as good as the last render.
            _AttachmentSentinel(
                lifecycle: lifecycle, id: id, path: scopePath, entry: registeredEntry)
        )
        #else
        content
        #endif
    }
}

#endif
