import SwiftUI

/// Shared message state for the test agent's `serializeState()`.
///
/// `ErrorBanner` automatically populates this when it appears/disappears so that
/// platform-specific state serializers can read current UI errors without holding
/// a reference to the active ViewModel.
///
/// **Category-1 capture funnel** (observability.md § What must be logged): a
/// displayed banner is "shown in the client", so it logs the message it shows to
/// the shared `fauna_log` ring at the banner's level — once per change, in the
/// setter (the `didSet` fires when a banner's `onAppear` assigns the message; an
/// event, not a per-tick paint — observability.md "Log on the *event*"). Clearing
/// a banner (`= nil`) is not a message, so it is skipped. This is O(funnels): the
/// three setters cover every `ErrorBanner`/`WarningBanner`/`InfoBanner` site.
@MainActor
public final class AppMessages {
    public static var error: String? = nil {
        didSet { logIfPresent(error, level: .error) }
    }
    public static var warning: String? = nil {
        didSet { logIfPresent(warning, level: .warn) }
    }
    public static var info: String? = nil {
        didSet { logIfPresent(info, level: .info) }
    }

    /// Which of the three banner slots a banner mirrors into.
    public enum Slot { case error, warning, info }

    private static func read(_ slot: Slot) -> String? {
        switch slot {
        case .error: return error
        case .warning: return warning
        case .info: return info
        }
    }

    private static func write(_ slot: Slot, _ value: String?) {
        switch slot {
        case .error: error = value
        case .warning: warning = value
        case .info: info = value
        }
    }

    /// Every banner showing right now, oldest first, per slot. An entry is one
    /// banner instance (its `@State` token) showing one text.
    private static var showing: [Slot: [(owner: UUID, message: String)]] = [:]

    /// A banner showing `message` came on screen; the slot mirrors it.
    ///
    /// `owner` names the banner instance. The same text can be on screen twice
    /// at once — the iOS conversations page shows a failed send on the list
    /// root's page banner AND on the pushed thread screen's — so only the
    /// instance that showed a text may retract it (`bannerDisappeared`).
    public static func bannerAppeared(_ slot: Slot, owner: UUID, message: String) {
        var entries = showing[slot] ?? []
        entries.removeAll { $0.owner == owner && $0.message == message }
        entries.append((owner, message))
        showing[slot] = entries
        if read(slot) != message { write(slot, message) }
    }

    /// The banner `owner` stopped showing `message`.
    ///
    /// Retracts only that showing, keyed on the text as well as the owner: a
    /// banner whose text is replaced in place (`.id(message)`) may be told the
    /// old text disappeared AFTER the new text appeared. If the slot still
    /// holds the retracted text, it falls back to the newest banner still on
    /// screen (which may show the very same text), else empties. A slot already
    /// holding something else — another banner's text, or `nil` after a nav
    /// clear — is left alone, so a disappear never resurrects a message.
    public static func bannerDisappeared(_ slot: Slot, owner: UUID, message: String) {
        var entries = showing[slot] ?? []
        entries.removeAll { $0.owner == owner && $0.message == message }
        showing[slot] = entries
        guard read(slot) == message else { return }
        let fallback = entries.last?.message
        if fallback != message { write(slot, fallback) }
    }

    /// Forget every showing banner and empty the three slots — the unit tests'
    /// clean page.
    static func resetBannerSlots() {
        showing = [:]
        error = nil; warning = nil; info = nil
    }

    private static func logIfPresent(_ message: String?, level: LogLevel) {
        guard let message, !message.isEmpty else { return }
        logMessage(level: level, target: "fauna.ui", message: message)
    }

    /// Apply the `messages` half of a TestAgent state patch — one implementation
    /// for macOS and iOS (priority #2), a byte-identical per-target twin until
    /// this harvest pass.
    #if DEBUG
    public static func applyPatch(_ messages: [String: Any]) {
        if let error = messages["error"] as? String {
            Self.error = error
        } else if messages.keys.contains("error") {
            Self.error = nil
        }
        if let warning = messages["warning"] as? String {
            Self.warning = warning
        } else if messages.keys.contains("warning") {
            Self.warning = nil
        }
        if let info = messages["info"] as? String {
            Self.info = info
        } else if messages.keys.contains("info") {
            Self.info = nil
        }
    }
    #endif

    #if DEBUG
    /// Convention 11's refusal slot: what the test agent could not honour.
    ///
    /// Deliberately **not** `error` above. That one is the *page's* banner
    /// mirror — `ErrorBanner.onAppear` assigns it and `onDisappear` clears it —
    /// so a refusal parked there is overwritten by the next banner to render and
    /// the driver reads back someone else's message, or none. This slot is
    /// **page-independent and nav-independent**, and is cleared **only** by
    /// `clearRefusedAgentCommand()` at the agent's `reset` — the per-test
    /// boundary every `app` fixture drives.
    ///
    /// This is the third app to need the distinction spelled out: tui and
    /// android both shipped the wrong slot first and had to be corrected
    /// (`e2e-conventions.md` § convention 11's build-out record). The apple twin
    /// of tui's `App::refused_agent_command`, linux's
    /// `SharedState::agent_command_failure`, and android's
    /// `AppMessages.refusedAgentCommand`.
    public static var refusedAgentCommand: String? = nil

    /// Stamp a test-agent refusal where the harness can actually see it.
    ///
    /// `POST /app/commands` acks 200 before the handler runs, so the HTTP reply
    /// can never carry the outcome — the only honest channel is the app's own
    /// error surface (`error-message`, which every driver reads via
    /// `app.error_text()`; convention 2). Anything the agent cannot honour lands
    /// here loudly instead of vanishing into a `.debug` log.
    ///
    /// One implementation for macOS and iOS (priority #2): both targets' private
    /// `testAgentFailure` delegate here rather than keeping a byte-identical
    /// twin each.
    public static func reportRefusedAgentCommand(_ message: String) {
        // The `[TestAgent]` prefix stays (AgentRefusalSlotTests.swift:174,
        // test_sync_live_apply.py:104 both scan for it) — this is additive, not
        // a swap. `test agent refused command` is the literal substring
        // `_assert_command_honoured` greps every sibling's refusal for
        // (`actions/conversations.py::_AGENT_FAILURE_MARKERS`); without it an
        // apple refusal is written, logged and rendered, but invisible to that
        // assert.
        let composed = "[TestAgent] test agent refused command: \(message)"
        logMessage(level: .error, target: "fauna.testagent", message: composed)
        refusedAgentCommand = composed
    }

    /// Clear the refusal slot. Called at the agent's `reset` and nowhere else —
    /// see `refusedAgentCommand` for why a nav must not reach it.
    public static func clearRefusedAgentCommand() {
        refusedAgentCommand = nil
    }
    #endif

    /// What the state protocol's `error` field publishes.
    ///
    /// A test-agent refusal wins over the page's own banner: the refusal is the
    /// reason the page is in whatever state it is in, and it is the message the
    /// harness is waiting to read. Mirrors android's `errorForDisplay` and the
    /// precedence tui's `screen_error_text()` applies.
    public static var errorForDisplay: String? {
        #if DEBUG
        return refusedAgentCommand ?? error
        #else
        return error
        #endif
    }
}

/// Unified error display for e2e test observability.
///
/// All views should use this instead of inline `Text(error).foregroundStyle(.red)`.
/// The accessibility identifier `error-message` allows e2e tests to detect and
/// read error messages across all platforms using a single element ID.
///
/// This view also updates `AppMessages.error` on appear/disappear so that
/// `serializeState()` always reflects the most recently shown error.
public struct ErrorBanner: View {
    public let message: String
    /// This instance's claim on its slot (`AppMessages.bannerAppeared`).
    @State private var owner = UUID()

    public init(message: String) {
        self.message = message
    }

    public var body: some View {
        // Hidden when there's nothing to show — matches the cross-app contract
        // ("the error-message label is built hidden, shown only on error"), so the
        // in-process `is_visible("error-message")` reads false on a clean page (the
        // flat registry only carries the id while a real message is shown).
        // XCUITest implicitly dropped an empty zero-frame Text from its a11y tree;
        // the in-process registry registers on `.onAppear` regardless of content,
        // so an always-rendered empty banner read "visible" forever — breaking
        // every "page must start with no error surfaced" assertion in-process.
        if message.isEmpty {
            Color.clear.frame(width: 0, height: 0)
        } else {
            Text(message)
                .foregroundStyle(.red)
                .font(.caption)
                .accessibilityIdentifier(Ids.errorMessage)
                // In-process e2e read of `error-message`, wired once on the shared
                // component so every call site (conversations send error, …) is
                // covered. Env-gated no-op in production.
                .automationValue(Ids.errorMessage, text: { message })
                .onAppear {
                    Task { @MainActor in AppMessages.bannerAppeared(.error, owner: owner, message: message) }
                }
                .onDisappear {
                    Task { @MainActor in
                        AppMessages.bannerDisappeared(.error, owner: owner, message: message)
                    }
                }
                // Identity keyed on the text. Both reads above are published
                // on appear, and a message replaced IN PLACE (one banner, new
                // text — the superseded launch's claim-free reason upgraded to
                // the verified one) keeps the view's identity, so neither
                // re-fired and the driver kept reading the first text. A new
                // identity per message re-runs the pair; the old one's
                // disappear is guarded on its own text, so it cannot clear the
                // new one's.
                .id(message)
        }
    }
}

/// Warning display for non-fatal issues (e.g. slow connection, missing optional feature).
/// Uses `warning-message` accessibility identifier for e2e test observability.
public struct WarningBanner: View {
    public let message: String
    /// This instance's claim on its slot (`AppMessages.bannerAppeared`).
    @State private var owner = UUID()

    public init(message: String) {
        self.message = message
    }

    public var body: some View {
        Text(message)
            .foregroundStyle(.orange)
            .font(.caption)
            .accessibilityIdentifier(Ids.warningMessage)
            .onAppear {
                Task { @MainActor in AppMessages.bannerAppeared(.warning, owner: owner, message: message) }
            }
            .onDisappear {
                Task { @MainActor in
                    AppMessages.bannerDisappeared(.warning, owner: owner, message: message)
                }
            }
    }
}

/// Informational message display (e.g. sync complete, new version available).
/// Uses `info-message` accessibility identifier for e2e test observability.
public struct InfoBanner: View {
    public let message: String
    /// This instance's claim on its slot (`AppMessages.bannerAppeared`).
    @State private var owner = UUID()

    public init(message: String) {
        self.message = message
    }

    public var body: some View {
        Text(message)
            .foregroundStyle(.secondary)
            .font(.caption)
            .accessibilityIdentifier(Ids.infoMessage)
            .onAppear {
                Task { @MainActor in AppMessages.bannerAppeared(.info, owner: owner, message: message) }
            }
            .onDisappear {
                Task { @MainActor in
                    AppMessages.bannerDisappeared(.info, owner: owner, message: message)
                }
            }
    }
}
