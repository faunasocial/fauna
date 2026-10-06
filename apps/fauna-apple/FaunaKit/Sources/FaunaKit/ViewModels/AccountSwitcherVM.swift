import Foundation

/// The multi-account switcher's read/remove half (`long-term-store.md` § Multi-account
/// evolution). Shared by macOS + iOS — one view model behind one FaunaKit section.
///
/// It holds **no** identity state of its own: the registry is the authority for which
/// accounts exist and which is active, and it is cheap to rebuild (a stateless view over
/// the Keychain), so every read goes to `FaunaAccounts.registry()` rather than to a cached
/// copy that could disagree with the store the launch machine routes on.
///
/// The *switch* deliberately does not live here. Activating tears down the authenticated
/// session and rebuilds it, which touches app-owned state (the launch machine, onboarding,
/// the menu-bar controller) that a FaunaKit view model cannot reach — so the section calls
/// out through an `onSwitch` closure that each app root fills in, the same seam
/// `onFactoryReset` / `onAccountReset` already use.
@MainActor @Observable
public final class AccountSwitcherVM {
    public private(set) var accounts: [FfiAccountEntry] = []
    /// The account **this window serves** — the row marked "in use": active indicator,
    /// no switch, no remove. Not the registry's active pointer: a bound macOS secondary
    /// serves an account the registry does not name as active (`account-scoping.md`
    /// § Concurrent instances → *Remove-account also refuses the account THIS instance
    /// serves*).
    public private(set) var activeActorId: String?
    /// Surfaced through the page's `error-message` element.
    public private(set) var error: String?

    /// The account this window's session runs as (the app's `SessionState.actorId`),
    /// or `nil` before one is admitted. Apple guards its session with the raw
    /// per-account instance lock, never the shared process holder, so the served
    /// account is handed in rather than read from Rust.
    private let servedActorIdProvider: @MainActor () -> String?
    /// This window's raw per-account instance lock, exactly as `SignOutSection`
    /// carries it (see its doc) — passed to `removeAccountBlocked`'s `ownLock` so
    /// a lone macOS window's own reflection is put down around the probe. `nil`
    /// on iOS, which takes no such lock; `remove(actorId:)` then leans on
    /// `servedActorId` alone to still refuse removing the account this window
    /// serves.
    private let ownLock: FfiAccountInstanceLock?

    public init(
        servedActorId: @escaping @MainActor () -> String? = { nil },
        ownLock: FfiAccountInstanceLock? = nil
    ) {
        self.servedActorIdProvider = servedActorId
        self.ownLock = ownLock
    }

    /// `fauna_client_accounts::session_account`'s rule: the account this window serves,
    /// or — before any is admitted, which only a primary can be — the registry's active
    /// account, which is what a primary resolves to.
    private func servedActorId(_ registry: FfiAccountRegistry) -> String? {
        servedActorIdProvider() ?? registry.active()
    }

    /// Re-read the registry. Called on appear and after a remove — the section renders
    /// straight off `accounts`, so this is what makes the list live-refresh **in place**,
    /// with no re-navigation (the linux e2e asserts exactly that).
    public func reload() {
        let registry = FaunaAccounts.registry()
        accounts = registry.list()   // add order — one row per entry
        activeActorId = servedActorId(registry)
    }

    public func isActive(_ entry: FfiAccountEntry) -> Bool {
        guard let activeActorId else { return false }
        return entry.actorId.caseInsensitiveCompare(activeActorId) == .orderedSame
    }

    /// The row title. **The shared FFI fn, never a hand-rolled fallback** — linux and web
    /// each once re-derived "handle, else short actor id" and drifted on the empty-handle
    /// case (`libs/fauna-core/src/format.rs` is the one implementation).
    public func label(for entry: FfiAccountEntry) -> String {
        accountDisplayLabel(handle: entry.handle, actorId: entry.actorId)
    }

    /// Drop an account from this install (its per-actor slots + index entry). No
    /// confirmation dialog — matching linux/web; the identity itself is not destroyed, it
    /// is only forgotten here, and the user can re-add it by importing the secret.
    ///
    /// Only ever offered on **non-active** rows, so this never has to re-route a live
    /// session (the shared `remove()` would promote the first remaining account, but the
    /// running client would still be authenticated as the removed one).
    ///
    /// **The remove itself refuses, not only the button** — asked before the registry
    /// removal, which drops the account's secret slots (a refusal after it would leave
    /// the stores on disk with nothing left to sign in to them). Two refusals, both the
    /// unified `removeAccountBlocked` door (`erase_guard.rs`, shared with `SignOutSection`):
    /// the account this window serves (removing it would unlink the stores this process
    /// runs from), then one a sibling instance — of this app OR another app declared at
    /// the shared store root, e.g. tui/linux — serves. Each paints its own line, because
    /// the remedies differ.
    ///
    /// *Erasure follows scope* (`account-scoping.md`): removing an account drops that
    /// actor's account-scoped stores too, not only its credential slots — a forgotten
    /// account whose MLS state stayed on disk is still on this device.
    ///
    /// No push-row drop here, deliberately: `common.md` § Registration's removal
    /// bullet holds a non-active account's row is nothing under this contract
    /// (the `fauna.push.*` verbs are actor-scoped on the *connection* actor, and
    /// this is only ever offered on non-active rows per the doc above) — a drop
    /// call here would target the wrong actor.
    /// `FfiEraseRemoveBlocked` is a uniffi enum with the line inside each arm's own
    /// payload, not a struct field — unlike `FfiEraseBlocked` (sign-out/start-over),
    /// so `remove(actorId:)` needs this to reach it uniformly.
    private static func line(_ blocked: FfiEraseRemoveBlocked) -> LocalizedText {
        switch blocked {
        case .servedHere(let line): return line
        case .servedElsewhere(_, let line): return line
        }
    }

    public func remove(actorId: String) {
        error = nil
        let registry = FaunaAccounts.registry()
        if let blocked = removeAccountBlocked(
            baseDir: AccountStateDir.base.path,
            actorIdHex: actorId,
            storeContainerDir: AccountStateDir.storeContainerDir,
            ownLock: ownLock,
            servingHere: servedActorId(registry)
        ) {
            error = renderLocalizedText(Self.line(blocked))
            logMessage(level: .warn, target: "fauna.accounts",
                       message: "[account-switcher] remove(\(actorId)) REFUSED: \(blocked)")
            return
        }
        do {
            try registry.remove(actorId: actorId)
        } catch {
            self.error = DisplayError.message(error)
            logMessage(level: .error, target: "fauna.accounts",
                       message: "[account-switcher] remove(\(actorId)) failed: \(error)")
            return
        }
        AccountStateDir.erase(actorIdHex: actorId)
        reload()
    }

    /// Flip the per-account "require re-auth to activate" flag (Stage 2,
    /// `long-term-store.md` § Multi-account evolution) — the
    /// `account-require-confirm-toggle` write path. Setting the flag never
    /// demands re-auth itself; only *activation* does.
    public func setRequireConfirm(actorId: String, require: Bool) {
        error = nil
        do {
            try FaunaAccounts.registry().setRequireConfirm(actorId: actorId, require: require)
        } catch {
            self.error = DisplayError.message(error)
            logMessage(level: .error, target: "fauna.accounts",
                       message: "[account-switcher] setRequireConfirm(\(actorId), \(require)) failed: \(error)")
            return
        }
        reload()
    }

    /// Paint a switch the registry refused. The FFI's `FfiError.General` already
    /// carries the shared `switch_refused_copy` line (it names the target and says
    /// the user is still on the identity they were using — true, because the refusal
    /// precedes every teardown), so `DisplayError.message` shows it verbatim.
    /// linux's / tui's `paint_switch_refusal`.
    public func reportSwitchRefused(_ refusal: Error) {
        error = DisplayError.message(refusal)
    }

    /// The Stage-2 gate in front of the app's switch seam. Resolves the re-auth
    /// question **before** anything app-owned runs, so the app's mutation-first
    /// invariant (`setActive` before teardown) holds unchanged:
    ///
    /// - unflagged account → `proceed(actorId, false)` synchronously — the app
    ///   calls plain `setActive`, exactly the Stage-1 path;
    /// - flagged account → the native re-auth prompt ([`AccountReauth`]); on
    ///   success `proceed(actorId, true)` — the app calls `setActiveConfirmed`;
    ///   on decline **nothing happens** (ratified: a pure no-op — no registry
    ///   mutation, no teardown, no banner; the user stays on the current account).
    ///
    /// **The decline arm's only observable is [`ActivationGestures`]**, counted as
    /// the last statement of the gesture's handler. "Nothing happens" is precisely
    /// what makes the ratified no-op untestable without it: a test proving the
    /// tap did not switch has nothing to wait for, so it slept and peeked, which
    /// false-passes whenever a real relaunch is merely late. See
    /// `fauna_e2e_agent::ACTIVATION_GESTURES_KEY` — **every** arm below counts,
    /// including the two that decide nothing, because the contract is one tap =
    /// one increment whatever it decided.
    public func requestSwitch(to entry: FfiAccountEntry,
                              proceed: @escaping @MainActor (_ actorId: String, _ confirmed: Bool) -> Void) {
        guard !isActive(entry) else {
            // A tap on the ACTIVE row still completed a gesture. Counting it is
            // the contract, not a courtesy: a caller waiting on this counter
            // must not have to know in advance which row it hit.
            ActivationGestures.recordCompleted()
            return
        }
        guard entry.requireConfirmToActivate else {
            proceed(entry.actorId, false)
            ActivationGestures.recordCompleted()
            return
        }
        let actorId = entry.actorId
        Task { @MainActor in
            // The count must be the LAST thing this job does on either arm, so
            // observing it proves the verdict was already dispatched — on the
            // approve arm that means `proceed`'s switch `Task` is enqueued, and
            // a `barrier` issued afterwards is therefore ordered behind it.
            // `defer` (Swift's `finally`, which the key's contract names) rather
            // than two call sites: an arm added here cannot forget to count
            // itself, and a thrown error still counts — a gesture that failed
            // still *finished*, and a `settled` firing only on the happy path
            // would hang the negative assert instead of failing it.
            defer { ActivationGestures.recordCompleted() }
            guard await AccountReauth.confirmActivation() else {
                logMessage(level: .info, target: "fauna.accounts",
                           message: "[account-switch] re-auth declined for \(actorId); staying on the current account")
                return
            }
            proceed(actorId, true)
        }
    }
}
