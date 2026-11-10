import SwiftUI

/// Thin SwiftUI-friendly proxy over `OnboardingMachine` (UniFFI). All wizard
/// state and transitions live in the Rust state machine; this class:
///
///   1. owns the machine instance,
///   2. implements `OnboardingObserver` to translate machine notifications
///      into `@Observable` invalidations on the main actor,
///   3. exposes a small set of convenience getters so SwiftUI views read
///      `vm.step` / `vm.currentHandle` etc. instead of `vm.machine.step()`
///      everywhere — they're equivalent,
///   4. wraps the machine's identity-confirm calls so the returned secret
///      hex is persisted to `KeychainStore` immediately. Long-term identity
///      lives in `KeychainStore` only; the wizard owns no persistence.
///
/// Shared by the macOS and iOS apps; identical behaviour on both. The legacy
/// per-stage state (claimHandle, hetznerCloudToken, etc.) and the legacy
/// `OnboardingStep` enum are gone — the machine's enum (defined in
/// `libs/fauna-onboarding-machine/src/state.rs`) is the source of truth and
/// flows in via the UniFFI-generated Swift binding.
@MainActor @Observable
public final class OnboardingVM {
    /// The UniFFI-generated Swift wrapper around the Rust state machine. SwiftUI
    /// views read snapshots via `vm.machine.<getter>()` and write through
    /// `vm.machine.<mutator>(...)` — SwiftUI never holds parallel state of its
    /// own (substitution rules tracked internally).
    public let machine: OnboardingMachine

    private let observerBox: ObserverBox

    /// Stable bridge to the long-lived `KeychainStore` (secret_key, device_id,
    /// node_url) — populated at identity-confirm time and consumed at the end
    /// of onboarding by `completeOnboarding`. The wizard itself doesn't read
    /// from the Keychain; it just feeds the secret back through return values.
    public let keychain = KeychainStore()

    public init() {
        let box = ObserverBox()
        self.observerBox = box
        // The constructor carries no provider base-URL override — that parameter
        // was automation surface riding the exported UniFFI signature and is gone
        // from every production artifact (testing.md convention 15). E2E tests
        // inject overrides via call_machine_method when needed.
        //
        // It *does* carry the pending-provision store. The wizard lands the awaiting
        // slot — nest_url, handle, claim_code — through it **before** it calls
        // `create_server`, and completes it with the box's reach address the moment
        // the box exists, so a quit or crash anywhere in Server/Dns/Online resumes on
        // the next launch's "Almost ready" surface instead of orphaning a box nobody
        // can claim and a bill nobody can stop from the app
        // (`onboarding.md` § 6 *The pending-provision slot*; custody precedes
        // dispatch). The bare `new` still compiles and still provisions — it just
        // silently loses that resume — which is why `provision_slot_wiring.rs` reads
        // this exact call site as a structural pin.
        self.machine = OnboardingMachine.newWithPersistence(
            observer: box,
            persistence: FaunaAccounts.registry(keychain: keychain).pendingProvisionStore())
        box.target = self
        // The one-tap trust offer (onboarding.md § 3b-ter) — until declared,
        // the NAT step exits straight to `Done` unchanged.
        machine.setRendersTrustPrompt(renders: true)
        // The recovery-kit offer after identity confirm (onboarding.md § 1
        // Identity) — the same capability-flag shape; `RecoveryKitOfferView`
        // renders it, and the handoff below registers the confirmed kit.
        machine.setRendersRecoveryKit(renders: true)
        // Re-root the machine's own store reads (the recovery floor's custody map
        // is read from the device's store, `box-recovery.md` § The plane-era
        // recovery floor) at the sandbox-shared container on iOS; `nil` on macOS.
        machine.setStoreContainerDir(storeContainerDir: AccountStateDir.storeContainerDir)
        #if os(macOS)
        // A desktop app keeps a door key where SSH keys live: a door run writes
        // it to `~/.ssh` with a `Host` entry (`installers/vps.md` § The SSH door
        // → *The key*), as tui and linux do. The macOS app is not sandboxed
        // (Developer-ID, no `app-sandbox` entitlement), so `~/.ssh` is the real
        // one. iOS has no `~/.ssh`: the key waits in the machine for the
        // save-key button (`ProvisioningSshDoorSection`).
        machine.setSshDoorKeyDir(
            dir: FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".ssh").path)
        #endif
    }

    // ── Observer ────────────────────────────────────────────────────────────
    fileprivate func onMachineChanged() {
        // @Observable picks up via the property accesses below; we just need
        // to provoke a tracked-property read on the main actor so SwiftUI
        // re-renders. `_observerTick` is read by the convenience getters.
        //
        // Any machine transition supersedes a client-side parse error: it means
        // the user advanced (or went Back), so a stale import-parse message must
        // not outlive it. The machine owns the error surface again.
        importError = nil
        // The sign-out residue is deliberately NOT cleared here: it is state the
        // machine does not own, and until a re-sweep says otherwise the
        // statement is still true — going Back to identity_choice still owes the
        // user the line (`signOutResidue`'s doc).
        _observerTick &+= 1
        // The pending-invite poll is armed from the machine's own ticks, never
        // only from a render (`armInvitePollIfNeeded`).
        armInvitePollIfNeeded()
    }
    private var _observerTick: UInt64 = 0

    /// Client-side parse error for the identity-import field, set when the shared
    /// `parseIdentityImport` rejects the input *before* the machine is touched (see
    /// `importIdentity`). Takes precedence over the machine error in `errorMessage`;
    /// cleared on the next import attempt or any machine transition.
    private var importError: String?

    /// What a sign-out's erase could not remove, while it still owes work — the
    /// whole state of `identity_choice`'s `sign-out-residue` view
    /// (`account-scoping.md` § Erasure follows scope → *the residue surface*).
    /// `nil` is the clean outcome and the only one.
    ///
    /// Handed in by the app root — off `SessionState.signOutResidue`, in the
    /// same synchronous step that re-roots to onboarding after a sign-out or the
    /// unreadable-index start-over — or set by the launch's sign-out finish
    /// (`launchMayProceed`). Never cleared on display or by a machine
    /// transition: it is state the wizard's machine does not own (the line used
    /// to ride `errorMessage`, which the machine's own error re-reads on every
    /// tick), and until a re-sweep says otherwise the statement is still true.
    public var signOutResidue: (any SignOutResidueSurfacing)?

    /// Move a just-finished erase's residue off `session` onto this wizard —
    /// the one handover both app roots run, after a sign-out (the `onSignOut`
    /// hook) and after the unreadable-index start-over. Synchronous, before the
    /// wizard mounts: the newest erase's statement replaces whatever was up,
    /// and `nil` (a clean erase) closes the view.
    ///
    /// It also re-roots the machine at `identity_choice` (`settings.md`, the
    /// `sign-out-button` row): a wizard still at `.done` from the sign-in that
    /// opened the erased session re-runs its exit when the signed-out root
    /// mounts it (each root's `doneExit` is view state, back to `.unknown`), and
    /// that handoff re-persists the identity just erased and relaunches into it.
    public func takeSignOutResidue(from session: SessionState) {
        machine.reset()
        signOutResidue = session.signOutResidue
        session.signOutResidue = nil
    }

    /// The tail of the Remove Again presses — each waits for the one before it
    /// (`retrySignOutResidue`).
    private var signOutResidueRetries: Task<Void, Never>?

    /// The one sign-out routine Remove Again and the launch run
    /// (`SignOutResidueSurface.finish` with the agent dismissed and no lock of
    /// this process's own — the view is only ever up signed out, and a launch
    /// holds none yet). The seam a unit test replaces.
    @ObservationIgnored
    public var signOutFinish: @Sendable () -> SignOutFinishOutcome = {
        SignOutResidueSurface.finish(dismissAgent: true, written: nil)
    }

    /// Where the launch's sign-out finish stands (`launchMayProceed`). Once per
    /// `OnboardingVM` — and each app root holds exactly one for the process —
    /// so a switch's, a teardown's or a start-over's re-entry into the launch
    /// never runs it again: a remove-account's own erase may be in flight then,
    /// over the record it joined.
    private enum LaunchFinish { case notRun, running, failedClosed, done }
    private var launchFinish = LaunchFinish.notRun

    /// The launch was refused by its sign-out finish while the record still
    /// owes its wipe (`launchMayProceed`): the root shows the wizard's
    /// `identity_choice` painting the refusal line instead of starting its
    /// launch machine, until Remove Again clears it.
    public var launchFailedClosed: Bool { launchFinish == .failedClosed }

    /// How the root re-enters its launch — handed in by `launchMayProceed`, and
    /// called once the finish allows it (its first run done, or a Remove Again
    /// that no longer fails closed).
    @ObservationIgnored
    private var relaunch: (@MainActor () -> Void)?

    /// **The launch's gate on the sign-out record** — both app roots ask it
    /// first thing in their launch, before anything reads the registry
    /// (`account-scoping.md` § Erasure follows scope → *The sign-out record is
    /// on all 7 apps*, decisions 3 and 4). `true`: the finish already ran and
    /// allowed it — launch now. `false`: hold the launch. On the first ask the
    /// finish runs, off the main actor — whatever a sign-out (or a
    /// remove-account) cut short is finished: the sync agent dismissed, the
    /// wipe, each recorded account's erase, the residue re-sweep — and then
    /// `relaunch` is called, which asks again. A refused finish whose record
    /// still owes its wipe FAILS CLOSED: every later ask answers `false` (the
    /// root shows `identity_choice`, `launchFailedClosed`) until Remove Again
    /// clears it and calls `relaunch`. Twin of android's `AppLaunchVM.start`.
    public func launchMayProceed(relaunch: @escaping @MainActor () -> Void) -> Bool {
        self.relaunch = relaunch
        switch launchFinish {
        case .done: return true
        case .running, .failedClosed: return false
        case .notRun:
            launchFinish = .running
            let finish = signOutFinish
            Task { @MainActor in
                let outcome = await Task.detached { finish() }.value
                // A sign-out that landed while the finish ran recorded the
                // newer statement; it wins.
                if self.signOutResidue == nil { self.signOutResidue = outcome.residue }
                self.launchFinish = outcome.failsClosed ? .failedClosed : .done
                self.relaunch?()
            }
            return false
        }
    }

    /// Remove Again (`sign-out-residue-retry-button`): the one routine again,
    /// off the main actor — the sign-out record's work if any is still owed,
    /// then the residue re-sweep — and paint what is left: nothing, when the
    /// device is now clean. A launch that failed closed re-enters once the
    /// finish no longer refuses.
    ///
    /// Presses are **serialized, never dropped**: a press that lands while a
    /// run is going was made after whatever the user just fixed, so it waits
    /// and then runs over what that run left. That is why the button stays
    /// enabled rather than disabling itself in flight. Twin of windows'
    /// `OnboardingViewModel.RetrySignOutResidueAsync` and android's
    /// `AppLaunchVM.retrySignOutResidue`.
    public func retrySignOutResidue() async {
        let previous = signOutResidueRetries
        let finish = signOutFinish
        let press = Task { @MainActor in
            await previous?.value
            let outcome = await Task.detached { finish() }.value
            self.signOutResidue = outcome.residue
            if self.launchFinish == .failedClosed && !outcome.failsClosed {
                self.launchFinish = .done
                self.relaunch?()
            }
        }
        signOutResidueRetries = press
        await press.value
    }

    /// The app shell's hook for "a pending-invite slot was just written" — the
    /// append-mode ("Add account") adoption seam for the pending-invite journey.
    /// Set once by the app root next to `appState.onLaunchAuthenticated`, for the
    /// same reason as `signOutResidue`: this class has no reach to `AppState`.
    ///
    /// `persistPendingInviteIfNeeded` fires it with the actor id the shared writer
    /// just registered **and activated**, on the submit return AND on every poll
    /// tick that refreshes the slot, so the shell — not this class — decides
    /// whether a call is an adoption. Only an append-mode wizard adopts
    /// (`onboarding.md` § Multi-account: "the append glue adopts on the submit
    /// return … register the append identity …, write its per-actor pending-invite
    /// slot, switch to it"); anywhere else the call is a no-op.
    ///
    /// `@MainActor` so the shell can read and clear `isAddingAccount` synchronously:
    /// clearing it BEFORE the switch task starts is the one-shot latch that keeps
    /// the next poll tick from adopting a second time. Unlike the append flag the
    /// moment-1 wrappers take from their view (a one-shot user gesture), the poll
    /// that reaches this has no view context, so the read has to be live.
    public var onPendingInvitePersisted: (@MainActor (_ actorId: String) -> Void)?

    // ── Convenience getters (read freshly on every access) ─────────────────
    public var step: OnboardingStep {
        _ = _observerTick
        return machine.step()
    }
    public var currentHandle: String {
        _ = _observerTick
        return machine.currentHandle()
    }
    public var errorMessage: String? {
        _ = _observerTick
        // A client-side import-parse error (set before the machine is touched)
        // takes precedence over the machine's own error surface. The sign-out
        // residue is NOT here: it has its own view (`signOutResidue`).
        return firstNonNil(importError, machine.errorMessage())
    }
    public var isLoading: Bool {
        _ = _observerTick
        return machine.isLoading()
    }
    /// The nest's rules for the joining pages (`onboarding-nest-rules`): the
    /// machine's verified pre-sign-in read, `nil` until it lands — read off
    /// `_observerTick` so the paint follows the wizard's change callback.
    public var joiningNestRules: NestRulesSummary? {
        _ = _observerTick
        return machine.joiningNestRules(lang: RegionStore.uiLang)
    }
    public var nestUrl: String {
        _ = _observerTick
        return machine.nestUrl()
    }
    /// The provisioning page's snapshot, read off `_observerTick` so the page
    /// repaints on every change callback. A page reading `machine.provisioningSnapshot()`
    /// directly tracks nothing: once the run ends with a `finalError` the
    /// `finalError ?? errorMessage` short-circuit skips the only tracked read,
    /// and a Retry then never repaints.
    public var provisioningSnapshot: ProvisioningSnapshot {
        _ = _observerTick
        return machine.provisioningSnapshot()
    }

    // ── "Almost ready" (awaiting-manual-DNS) surface ────────────────────────

    /// The deferred-DNS "Almost ready" snapshot — records + `AwaitingDnsState`
    /// (Pending/Checking/Claiming/Claimed/Error) + a localized status message.
    /// Read off `_observerTick` so SwiftUI repaints on each `recheckManualDns()`
    /// state change.
    public var awaitingDnsSnapshot: AwaitingManualDnsSnapshot {
        _ = _observerTick
        return machine.awaitingManualDnsSnapshot()
    }

    /// The DNS records the user must add at their registrar, one line per record —
    /// the shared formatter both the records label and the copy button read, so the
    /// two can never disagree (`onboarding.md` § "Almost ready" surface).
    public var awaitingDnsRecordsText: String {
        _ = _observerTick
        return machine.awaitingDnsRecordsText()
    }

    /// Whether "Copy all" has anything to copy — the second derived difference
    /// between the two "Almost ready" modes (`onboarding.md` § "Almost ready"
    /// surface, *Two modes*): disabled, never hidden, when the records-less
    /// mode leaves nothing to copy.
    public var awaitingDnsCopyEnabled: Bool {
        _ = _observerTick
        return machine.awaitingDnsCopyEnabled()
    }

    /// Whether the "Use a different nest" exit is live — off only while a claim is
    /// in flight, ON during a bare probe (a box that never answers lives in
    /// `Checking`), so it is NOT `isAwaitingDnsBusy`. The machine decides.
    public var awaitingDnsFallthroughEnabled: Bool {
        _ = _observerTick
        return machine.awaitingDnsFallthroughEnabled()
    }

    /// The "Almost ready" exit (`onboarding-provisioning.md` § "Almost ready"
    /// surface → *Exit*): the machine clears the slot through the store it was
    /// built with and re-lands at `handle_entry` holding the identity; the observer
    /// tick then repaints, and the surface (keyed on the outcome) disappears.
    public func abandonAwaitingManualDns() {
        machine.abandonAwaitingManualDns()
    }

    /// True while `wizard_outcome() == AwaitingManualDns` — the single predicate the
    /// "Almost ready" surface renders and polls off (it is NOT an `OnboardingStep`,
    /// so both the same-session exit and the relaunch hydration reach it identically).
    public var isAwaitingManualDns: Bool {
        _ = _observerTick
        if case .awaitingManualDns = machine.wizardOutcome() { return true }
        return false
    }

    /// True while a recheck probe or claim is in flight — used to disable the
    /// recheck button so the user can't stack probes.
    public var isAwaitingDnsBusy: Bool {
        switch awaitingDnsSnapshot.state {
        case .checking, .claiming: return true
        default: return false
        }
    }

    /// Single-shot poll of the freshly-provisioned nest (the client owns the cadence;
    /// the machine runs no internal interval, keeping native and wasm identical). On a
    /// successful claim the machine routes itself off `AwaitingManualDns` to the
    /// post-claim step, or to the trust offer on the already-claimed edge; the surface
    /// hands back to the container (`isAwaitingManualDns` goes false). The slot is NOT
    /// cleared here — only at `LoggedIn` (`clearAwaitingDnsSlot`).
    public func recheckManualDns() async {
        _ = await machine.recheckManualDns()
    }

    /// Clear the awaiting-manual-DNS slot at the wizard's `LoggedIn` terminal — the ONE
    /// clearing moment (`onboarding.md` § Long-term store contract, ratified 2026-09-21;
    /// client-side delete, the launch machine never writes the store). Never at the claim
    /// itself: leaving the slot until `LoggedIn` is what lets a force-quit on the NAT page
    /// or on the trust offer relaunch back into "Almost ready" and be asked once more.
    ///
    /// The slot is per-actor in the shared registry — the only store
    /// (`long-term-store.md` § Downgrade mirror + abandoned-append recovery, RETIRED
    /// 2026-09-24) — so the registry's clear is the whole of it.
    public func clearAwaitingDnsSlot() {
        FaunaAccounts.registry(keychain: keychain).clearAwaitingDns()
    }

    // ── Identity-confirm wrappers ───────────────────────────────────────────

    /// Wraps `machine.confirmGeneratedIdentity()` and commits the returned
    /// secret through the SHARED confirm-identity moment (`confirmIdentity`,
    /// `fauna_client_accounts::persist_confirmed_identity`): on a first run the
    /// per-actor account is created, the secret READ BACK (a bare keychain
    /// write reported success on a keystore that kept nothing — at the one
    /// write whose silent failure destroys an account outright) and activated.
    /// Views call this instead of `vm.machine.confirmGeneratedIdentity()`
    /// directly. Spec acceptance criterion 3 (secret durable post-confirm).
    ///
    /// **Loud on failure** (`apps/common.md` § Credential storage: a store
    /// failure "is a loud error, not a silent fallback"): a write the keychain
    /// refused — or kept nothing of — goes to the shared machine's
    /// `identityCommitFailed`, which puts the wizard back on the screen the
    /// person confirmed on with the error on `error-message`; confirming again
    /// is the retry. linux's and tui's `commit_confirmed_identity` shape.
    ///
    /// - Parameter append: true for the "Add account" wizard over a live
    ///   session — the caller must pass the CURRENT `appState.isAddingAccount`
    ///   at the moment of the call, never a cached/stored flag (a one-sided
    ///   set would leak into a later non-append wizard run). See
    ///   `persistConfirmedSecret` for what append mode does (nothing).
    public func confirmGeneratedIdentity(append: Bool = false) throws {
        let secret = try machine.confirmGeneratedIdentity()
        persistConfirmedSecret(secret, append: append, context: "generated")
    }

    /// Wraps `machine.confirmImportedIdentity(secret:)` and commits the
    /// machine-validated secret through the same shared helper as
    /// `confirmGeneratedIdentity` (its doc has the why). Views call this
    /// instead of `vm.machine.confirmImportedIdentity(secret:)` directly.
    public func confirmImportedIdentity(secret: String, append: Bool = false) throws {
        let validated = try machine.confirmImportedIdentity(secret: secret)
        persistConfirmedSecret(validated, append: append, context: "imported")
    }

    /// The moment-1 write `confirmGeneratedIdentity`/`confirmImportedIdentity` share —
    /// the one shared call every app's confirm arm makes, **in both modes**.
    ///
    /// Append ("Add account") mode writes NOTHING: the shared helper only derives the
    /// actor id, so an abandoned append can neither leave a half-account nor shadow the
    /// active one (`onboarding.md` § Multi-account). The appended identity stays in the
    /// wizard machine (`effectiveSecret()`) until its own terminal registers it —
    /// `completeAppendedAccount` for a `LoggedIn` exit, `persistPendingInviteIfNeeded`
    /// for the pending-invite submit return. The rule lives in shared Rust, never in an
    /// app-side `if !append` (web once shipped the half-account that guard's omission
    /// makes).
    ///
    /// Returns whether the write held; on `false` the machine is already back on the
    /// confirm screen with the error, so a caller must not paint another message over it.
    @discardableResult
    private func persistConfirmedSecret(_ secret: String, append: Bool, context: String) -> Bool {
        do {
            _ = try FaunaAccounts.registry(keychain: keychain).confirmIdentity(
                secretHex: secret, append: append)
            return true
        } catch {
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] persist_confirmed_identity (\(context)) failed: \(error)")
            machine.identityCommitFailed(message: L.onboarding.sessionError.persistIdentity)
            return false
        }
    }

    /// Parse a pasted/scanned identity-import field through the shared
    /// `fauna_core::identity_qr` parser (UniFFI `parseIdentityImport`) and commit it.
    /// Accepts the union of every app form — a bare 64-hex secret, the
    /// `fauna://identity?secret=&handle=` query form, or the colon form — so web, iOS,
    /// macOS, and android all parse identical input from one crate instead of each
    /// hand-rolling the grammar (priority #2/#4; onboarding.md §1.identity_import).
    ///
    /// The face returns an empty list on a parse failure → surface the localized
    /// invalid-secret error (matching web/android). Otherwise pre-fill the handle the
    /// payload carried (when any) via `setCurrentHandle` before validating + persisting
    /// the secret through `confirmImportedIdentity`. Shared by the iOS and macOS import
    /// views; mirrors android `IdentityImportVM.importIdentity`.
    ///
    /// - Parameter append: forwarded to `confirmImportedIdentity` verbatim — see its
    ///   doc for why this must be the caller's live `appState.isAddingAccount` read.
    public func importIdentity(_ input: String, append: Bool = false) {
        importError = nil
        let parts = parseIdentityImport(input: input.trimmingCharacters(in: .whitespacesAndNewlines))
        guard parts.count == 2 else {
            importError = L.onboarding.identityImport.invalidSecret
            return
        }
        let handle = parts[1]
        if !handle.isEmpty {
            machine.setCurrentHandle(h: handle)
        }
        try? confirmImportedIdentity(secret: parts[0], append: append)
    }

    // ── Identity-recovery steps (onboarding.md § 1 Identity) — `recovery_kit`
    //    and `recovery_entry`, rendered by the shared FaunaKit
    //    `RecoveryKitOfferView` / `RecoveryEntryView`. Every decision lives in
    //    the shared machine; these only read it and fold the restore's answer.

    /// The minted kit's bare 64-hex — the on-screen display only. `nil` until
    /// the machine has minted on entry.
    public var recoveryKitSecretHex: String? {
        _ = _observerTick
        return machine.recoveryKitSecretHex()
    }

    /// The one `fauna://recovery` URI the QR and the copy button carry — never
    /// the bare hex (`identity-succession.md` § The RecoveryKey, *Which
    /// encoding each affordance carries*).
    public var recoveryKitUri: String? {
        _ = _observerTick
        return machine.recoveryKitUri()
    }

    /// `recovery-kit-secret-copy-btn` — read at click time, never cached: the
    /// URI lives exactly as long as the machine holds the pending root.
    public func copyRecoveryKit() {
        if let uri = machine.recoveryKitUri() {
            Pasteboard.copy(uri)
        }
    }

    /// `recovery-entry-submit-button` — the phrase-only restore. The typed
    /// account rides on the machine's one account field and is ALWAYS
    /// forwarded, empty included: what the field shows is what is sent (a
    /// handle left on the machine by an earlier flow must not ride along
    /// invisibly). What every refusal says is the shared
    /// `recoveryEntryOutcomeMessage` table; `superseded` routes to the import
    /// screen instead of speaking; a restored seed is committed exactly like an
    /// import, so a crash before sign-in resumes at `handle_entry`.
    ///
    /// - Parameter append: the caller's live `appState.isAddingAccount` — see
    ///   `confirmGeneratedIdentity`.
    public func submitRecoveryEntry(phrase: String, account: String, append: Bool) async {
        machine.setCurrentHandle(h: account.trimmingCharacters(in: .whitespacesAndNewlines))
        let outcome = await machine.submitRecoveryEntry(kitInput: phrase)
        switch outcome {
        case .superseded:
            machine.beginImportIdentityWithReason(reason: L.onboarding.recoveryEntry.superseded)
            return
        case .restored, .restoredPredecessorsLost:
            if let secret = machine.effectiveSecret() {
                // A refused save already put the error on the confirm screen
                // (`identityCommitFailed`); the outcome line must not paint over it.
                if !persistConfirmedSecret(secret, append: append, context: "restored") {
                    return
                }
            }
        default:
            break
        }
        if let message = recoveryEntryOutcomeMessage(outcome: outcome) {
            machine.setErrorMessage(message: renderLocalizedText(message))
        }
    }

    /// `sessions-lockout-button` on `emergency_lockout_entry` — the seed-signed
    /// lock over one anonymous connection (`devices.md` § The signed-out door).
    /// The typed account rides on the machine's one account field, ALWAYS
    /// (empty included), exactly as `recovery_entry` forwards it; the word is
    /// checked by the machine. Persists nothing. The shared table's sentence
    /// lands on `error-message` — for a refusal on this step, for `locked` on
    /// `identity_choice`, where the machine has already returned the wizard;
    /// only the unlock time is the client's (the shared `formatUnixLocal`).
    public func submitEmergencyLockout(secret: String, account: String, confirmWord: String) async {
        machine.setCurrentHandle(h: account.trimmingCharacters(in: .whitespacesAndNewlines))
        let outcome = await machine.submitEmergencyLockout(secretInput: secret, confirmWord: confirmWord)
        let unlockTime: String
        if case .locked(let until) = outcome {
            unlockTime = formatUnixLocal(secs: until)
        } else {
            unlockTime = ""
        }
        machine.setErrorMessage(
            message: renderLocalizedText(emergencyLockoutOutcomeMessage(outcome: outcome, unlockTime: unlockTime)))
    }

    // ── Invite-request persistence wrappers ─────────────────────────────────
    //
    // Per `docs/goal/behavior/onboarding.md`:
    // "Persistence happens on machine return values, not on observer ticks."
    // Views call these wrappers; the wrappers invoke the machine method,
    // then write/update/delete the keychain pending-invite slot based on
    // the resulting snapshot.

    /// Wraps `machine.wizardSubmitInviteRequest()`. When the resulting
    /// snapshot is `PendingReview`, persists the slot so the next launch
    /// can resume here. Returns the same `OnboardingStep` the machine
    /// returned.
    @discardableResult
    public func wizardSubmitInviteRequest() async -> OnboardingStep {
        await storeAgeRound?.prepareAdmission(on: machine)
        let next = await machine.wizardSubmitInviteRequest()
        persistPendingInviteIfNeeded()
        return next
    }

    /// Wraps `machine.recheckInviteStatus()`. Updates the slot's
    /// `status_json` on continued PendingReview/Approved; deletes the
    /// slot when the recheck reports the request as not-found
    /// (snapshot transitions to `Error{transient:false, context:Rechecking,
    /// cause:"invite.error.not_found"}`, per machine.rs).
    @discardableResult
    public func recheckInviteStatus() async -> OnboardingStep {
        let next = await machine.recheckInviteStatus()
        let snap = machine.inviteRequestSnapshot()
        if case .error(_, let context, let cause) = snap.state,
           case .rechecking = context,
           cause.contains("not_found") {
            clearPendingInviteSlot()
        } else {
            persistPendingInviteIfNeeded()
        }
        return next
    }

    /// Wraps `machine.redeemInvite()`. On success (`Done` with
    /// `wizardOutcome() == LoggedIn`) deletes the pending-invite slot
    /// — the identity store handoff happens in `completeOnboarding`.
    @discardableResult
    public func redeemInvite() async -> OnboardingStep {
        await storeAgeRound?.prepareAdmission(on: machine)
        let next = await machine.redeemInvite()
        if case .done = next, case .loggedIn = machine.wizardOutcome() {
            clearPendingInviteSlot()
        }
        return next
    }

    /// Clear the active account's per-actor pending-invite slot at a terminal — the
    /// registry is the only store, so this is the whole of it.
    private func clearPendingInviteSlot() {
        FaunaAccounts.launchPersistence(keychain: keychain).deletePendingInvite()
    }

    /// Whether the invite-request Continue button should be enabled: an
    /// out-of-band code that's already `.valid` always enables it (the code
    /// itself is the gate), otherwise it falls back to the snapshot's own
    /// `continueEnabled` (macOS + iOS, priority #2 — was duplicated
    /// verbatim in both views before this lift).
    public func inviteContinueEnabled() -> Bool {
        let snap = machine.inviteRequestSnapshot()
        if case .valid = snap.outOfBandCodeState { return true }
        return snap.continueEnabled
    }

    // ── `invite_request` view-action wrappers (macOS/iOS, priority #2 — were
    //    duplicated verbatim across `MacInviteRequestView`/`InviteRequestView`
    //    before this lift). Referenced by both a Button and its
    //    `.automationActivate` so the two can't diverge (apple-e2e-automation.md
    //    § Resolved design point); the async ones recompute the live snapshot
    //    inside so the registered closure reads current state, not a
    //    body-eval capture.
    /// The store-age round (`StoreAgeClaimRound`), present only once a shell
    /// attached one — iOS alone does; macOS has no store age signal, so both
    /// admission hooks above are no-ops there.
    private var storeAgeRound: StoreAgeClaimRound?

    /// Run the store-age round as the `invite_request` page appears, so the
    /// shared `age_notice` paints before submit/redeem (`family-safety.md`
    /// § App surface → *Age-band surfaces*). The round is kept for the
    /// wizard's lifetime; each admission call re-mints its spent attestation.
    public func attachStoreAgeClaim(signals: StoreAgeSignals) async {
        let round = storeAgeRound ?? StoreAgeClaimRound(signals: signals)
        storeAgeRound = round
        await round.attach(to: machine)
    }

    public func submitInviteRequest() {
        Task { _ = await wizardSubmitInviteRequest() }
    }

    public func recheckInvite() {
        Task { _ = await recheckInviteStatus() }
    }

    /// True while an invite request is pending — the predicate the pending-invite
    /// poll loop below arms and disarms itself on. Submitting the request or a
    /// relaunch hydration (`seed_pending_invite`) arms it; an admin decision or a
    /// not-found refutation (`recheckInviteStatus` above) disarms it.
    /// `onboarding.md` § The pending-invite surface.
    ///
    /// "Pending" is shared Rust's answer (`pendingInviteSlot()`): `PendingReview`,
    /// or `Rechecking` from one — never the bare `PendingReview` case. The poll's
    /// own recheck passes through `Rechecking`, so a predicate that read only
    /// `PendingReview` flipped false and back on every tick; as the poll task's
    /// `.task(id:)` key that cancelled and restarted the task from inside its own
    /// work, and under load the churn could leave no task running at all — the
    /// requester sat on `PendingReview` and never noticed the approval (macOS e2e,
    /// `test_admin_approval_advances_the_requester_with_no_user_action`).
    public var isInviteRequestPending: Bool {
        _ = _observerTick
        return machine.pendingInviteSlot() != nil
    }

    /// Whether the pending-invite poll should run: a request is pending AND the
    /// wizard is on the `invite_request` page (Back leaves the slot pending but
    /// moves the step, which stops the poll — `onboarding.md` § The pending-invite
    /// surface, "while the page is visible").
    private var invitePollWanted: Bool {
        guard case .inviteRequest = machine.step() else { return false }
        return machine.pendingInviteSlot() != nil
    }

    /// The one running pending-invite poll, or nil.
    private var invitePollTask: Task<Void, Never>?

    /// Start the pending-invite poll if it is wanted and not already running.
    /// Idempotent, so it is safe from every trigger: each machine tick
    /// (`onMachineChanged`) arms it, which is what makes the poll independent
    /// of SwiftUI rendering — the views' `.task(id: vm.isInviteRequestPending)`
    /// arm is a second trigger only. Before this, the loop lived inside that
    /// `.task` alone, so it ran only when a render observed the right key at the
    /// right moment; on macOS under load it was measured not running at all
    /// after an approval (the requester sat on `PendingReview` for 150 s with no
    /// `invite_request.status` call, while a pressed Recheck admitted at once).
    public func armInvitePollIfNeeded() {
        guard invitePollTask == nil, invitePollWanted else { return }
        invitePollTask = Task { @MainActor [weak self] in
            await self?.pollPendingInviteWhileNeeded()
            // No suspension between the loop's last check and this reset, and
            // every tick arrives on the main actor, so a request that becomes
            // pending again re-arms from its own tick.
            self?.invitePollTask = nil
        }
    }

    /// Client-owned poll loop for the `invite_request` page's pending request
    /// (`onboarding.md` § The pending-invite surface — "First poll fires
    /// immediately… then on the interval while the page is visible"). Shared by
    /// both apple apps (priority #2) and started only through
    /// `armInvitePollIfNeeded`, so at most one runs. It ends when the request
    /// resolves (an admission or a refutation) or the wizard leaves the page. A
    /// tick that finds a recheck already in flight (the user pressed Recheck)
    /// gets an immediate no-op from the machine and waits for the next interval.
    /// The cadence is read live from shared Rust (`inviteRecheckPollMs()`),
    /// never a literal — read by all 7 apps, never seven hand-copied numbers.
    private func pollPendingInviteWhileNeeded() async {
        while !Task.isCancelled && invitePollWanted {
            _ = await recheckInviteStatus()
            guard !Task.isCancelled, invitePollWanted else { break }
            try? await Task.sleep(for: .milliseconds(inviteRecheckPollMs()))
        }
    }

    /// `code` is the view's own `invite-code-input` text-field buffer.
    public func checkOobCode(_ code: String) {
        Task { await machine.verifyOobInviteCode(code: code) }
    }

    /// Cancel any in-flight invite operation, then standard Back. The slot is
    /// preserved per the target-state doc: "The store entry is NOT deleted by
    /// Back."
    public func inviteBack() {
        machine.cancelInviteOp()
        machine.back()
    }

    /// Continue is the out-of-band code's redeem and nothing else
    /// (`onboarding.md` § 3 — the button's row). The PendingReview and Approved
    /// branches retired 2026-08-12 with the continue-exit: that journey advances
    /// by polling, and `continueEnabled` is false throughout it.
    public func submitInviteContinue() {
        Task { _ = await redeemInvite() }
    }

    /// `nest_provisioning`'s Back action (mac/iOS, priority #2 — was
    /// duplicated verbatim in both views before this lift): clear any error,
    /// then pop the wizard one stage. Referenced by both the `Button` and its
    /// `automationActivate` so they never diverge (apple-e2e-automation.md §
    /// Resolved design point).
    public func provisioningBack() {
        machine.clearError()
        machine.back()
    }

    /// Write the pending-invite resume slot — at the submit return, which
    /// `onboarding.md` § 3 Persistence callouts names as "the only write
    /// moment".
    ///
    /// The slot is assembled by shared Rust (`pendingInviteSlot()`) rather than
    /// re-derived here: the nest_url rule (machine state, never the
    /// provider-override URL) and the opaque status_json are both
    /// silent-when-wrong, so they live in one place for all 7 apps. It returns
    /// nil unless the wizard is actually in `PendingReview`, which also replaces
    /// this method's former state switch.
    ///
    /// **The registry write is unconditional — append mode included.** It is the
    /// same `persistPendingInvite` the first-run journey uses, and moving the
    /// active pointer is the ratified adoption (`onboarding.md` § Multi-account),
    /// not a defect. That is the opposite of `persistConfirmedSecret`, which skips
    /// the registry in append mode because moment 1 is not the append's terminal;
    /// this write IS the terminal. What append mode adds is the live session
    /// following the registry: `onPendingInvitePersisted` hands the actor id to the
    /// app shell, which switches (see its doc). The secret is the MACHINE's
    /// (`effectiveSecret()`), never the store's: an append's moment 1 wrote nothing.
    private func persistPendingInviteIfNeeded() {
        guard let slot = machine.pendingInviteSlot() else { return }
        let nestUrl = slot.nestUrl
        let handle = slot.handle
        let requestId = slot.requestId
        let statusJson = slot.statusJson
        guard !nestUrl.isEmpty, !handle.isEmpty, !requestId.isEmpty else { return }
        guard let secretHex = machine.effectiveSecret() else {
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] pending-invite slot with no identity on the machine")
            return
        }
        // Through the SHARED per-actor registry, for the same reason as the
        // awaiting-DNS slot above: the legacy-global write this replaced was
        // invisible the moment an account index existed (an apple user who had
        // ever used "Add account"), so the relaunch lost an outstanding invite.
        do {
            let actorId = try FaunaAccounts.registry(keychain: keychain).persistPendingInvite(
                secretHex: secretHex,
                nestUrl: nestUrl, handle: handle,
                requestId: requestId, statusJson: statusJson
            )
            onPendingInvitePersisted?(actorId)
        } catch {
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] persistPendingInvite failed: \(error)")
        }
    }

    // ── Box-recovery step-4 wizard branch (box-recovery.md § Recovery UI
    //    (step 4), Task E) — `nest_recovery` / `recover_selfhosted_instructions`.
    //    Shared by macOS + iOS (priority #2); the pages themselves are thin
    //    per-platform SwiftUI reading `vm.machine.*` directly (same idiom as
    //    `MacNatModeChoiceView`/`NatModeChoiceView`), so only the parts that
    //    would otherwise be duplicated verbatim across both live here.

    /// `nest_recovery`'s Back action: came-from-identity routes back to
    /// `handle_entry` (Q2-A); came-from-launch is the surviving-device
    /// `launch-recover-button` entry (both apple targets; iOS shows `launch_retry` only
    /// for its gating case — a launch that entered no session).
    /// Both arms are the machine's own transition table — this wrapper exists
    /// only so the two views call one symbol, mirroring `provisioningBack()`.
    public func recoveryBack() {
        machine.back()
    }

    /// Selecting a box on `nest_recovery` (`recover-box-item`) — enables the two
    /// re-provision method buttons.
    public func selectRecoveryBox(_ nestActorId: String) {
        machine.selectRecoveryBox(nestActorId: nestActorId)
    }

    /// `recover-method-cloud-button` — re-provision the selected box on a cloud
    /// host. Advances to `vps_config` in recovery mode. Buttons are gated on a
    /// selection, so a thrown `InvalidTransition` here is defense-in-depth only;
    /// any other failure surfaces via `errorMessage` (mirrors android
    /// `NestRecoveryVM.recoverViaCloud`).
    public func recoverViaCloud() {
        try? machine.recoverViaCloud()
    }

    /// `recover-method-selfhosted-button` — advance to
    /// `recover_selfhosted_instructions`.
    public func recoverViaSelfhosted() {
        try? machine.recoverViaSelfhosted()
    }

    /// `recover-restore-cta` / `recover-selfhosted-continue-button` — both exit
    /// the recovery flow (mirrors linux/android's shared `m.reset()`): the box
    /// reconnects via the normal launch flow once the admin has run the
    /// installer and it is reachable.
    public func recoveryReset() {
        machine.reset()
    }

    /// Populate `nest_recovery`'s box list via `RecoverableBoxes.load` — the
    /// reachable-nest read with the device's own store as the fall-back, never
    /// either-or (a dead saved nest is the *expected* failure mode here, not a page
    /// error). No-ops once the machine already holds a non-empty list — never
    /// clobber a real read, or the e2e's `set_recovery_boxes` injection.
    public func loadRecoveryBoxesIfNeeded() async {
        guard machine.recoveryBoxes().isEmpty else { return }
        guard let secretHex = machine.effectiveSecret() else { return }
        let boxes = await RecoverableBoxes.load(
            nestUrl: machine.nestUrl(), ownerSecret: hex_to_data(secretHex))
        if !boxes.isEmpty, machine.recoveryBoxes().isEmpty {
            machine.setRecoveryBoxes(boxes: boxes)
        }
    }

    /// The `recover-selfhosted-command` line for the selected box, resolved
    /// through the pre-login `recoverSelfhostedCommandPreLogin` FFI over the
    /// shared `selfhosted_command`: the device's own store is always read, joined with
    /// the nest when a URL is saved and answers — never either-or, so a dead (or
    /// unset) saved nest still yields the line. `nil` while unresolved (no
    /// selection, or no seed anywhere) — the view keeps showing the pending
    /// placeholder with copy disabled: an unresolved (or wrong-box) command must
    /// never reach the admin's clipboard (box-recovery.md § Mechanism notes —
    /// Self-hosted command).
    public func resolveSelfhostedCommand() async -> String? {
        guard let secretHex = machine.effectiveSecret(),
              let nestActorId = machine.recoverySelectedNestId()
        else { return nil }
        let nestUrl = machine.nestUrl()
        do {
            return try await FaunaFFISwift.recoverSelfhostedCommandPreLogin(
                ownerSecret: hex_to_data(secretHex), nestActorId: nestActorId,
                nestUrl: nestUrl.isEmpty ? nil : nestUrl,
                storeContainerDir: AccountStateDir.storeContainerDir)
        } catch {
            logMessage(level: .warn, target: "fauna.recovery",
                       message: "[recovery] self-hosted command resolve failed: \(error)")
            return nil
        }
    }

    // ── End-of-flow handoff ─────────────────────────────────────────────────

    /// What the wizard ended on. The orchestrator routes the running app on
    /// this — only `.feed` enters the authenticated UI.
    ///
    /// ⚠ There is deliberately no `.inviteSubmitted` case (retired 2026-08-12).
    /// The pending-review journey never ends the wizard at all: it stays on
    /// `invite_request` and polls (`onboarding.md` § The pending-invite
    /// surface), so it never reaches `completeOnboarding`. Its two placeholder
    /// surfaces went with it — macOS's undeclared `invite-submitted-placeholder`
    /// (which was also a rule-A undeclared-id deviation) and iOS's `Color.clear`.
    public enum OnboardingExit: Equatable {
        case feed
        case awaitingManualDns
        case unknown
    }

    /// Called by the orchestrator once `step == .done`. Reads
    /// `wizardOutcome()` and persists / signals according to the
    /// target-state doc's outcome table:
    ///
    /// - `LoggedIn`              → migrate session into `SessionState`,
    ///                             delete any pending-invite slot, return `.feed`.
    /// - `AwaitingManualDns`     → save the four-field awaiting-manual-DNS slot;
    ///                             return `.awaitingManualDns`.
    @discardableResult
    /// - Parameter append: true for the "Add account" wizard over a live session.
    ///   Like `confirmGeneratedIdentity`'s flag, the caller must pass its CURRENT
    ///   `appState.isAddingAccount` read. It exempts the registry terminal below:
    ///   an append's own terminal (`completeAppendedAccount`) registers and
    ///   switches, and activating here would move `active` off the live account
    ///   before that runs.
    public func completeOnboarding(sessionState: SessionState, append: Bool = false)
        -> OnboardingExit
    {
        switch machine.wizardOutcome() {
        case .loggedIn:
            return performLoggedInHandoff(sessionState: sessionState, append: append)
        case .awaitingManualDns(let nestUrl, _, let claimCode):
            // Persist the slot so a relaunch routes to "Almost ready"
            // (`onboarding.md` § Long-term store contract), through the SHARED
            // per-actor registry — the same `persist_awaiting_dns` linux and tui call.
            //
            // This exit has no handle yet (the wizard reaches `DnsPostInstructions`
            // without a handle stage); the per-actor slot carries opaque JSON, so an
            // empty field is data rather than absence. The secret is the MACHINE's.
            //
            // `recordsJson` comes from the shared `awaitingDnsRecordsJson()` getter, never
            // a hand-rolled encode of the bound camelCase records (that round-trips to an
            // empty list, trap (c)). Deliberately registers the account with NO nest_url:
            // the nest is unclaimed, so an identity node_url would make the next launch
            // silent-challenge an unpropagated host (trap (a)); the real node_url lands
            // at the `LoggedIn` terminal once the claim completes.
            guard let secretHex = machine.effectiveSecret() else {
                logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] awaiting-DNS exit with no identity on the machine")
                return .awaitingManualDns
            }
            do {
                _ = try FaunaAccounts.registry(keychain: keychain).persistAwaitingDns(
                    secretHex: secretHex,
                    nestUrl: nestUrl,
                    handle: machine.currentHandle(),
                    dnsRecordsJson: machine.awaitingDnsRecordsJson(),
                    claimCode: claimCode
                )
            } catch {
                logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] persistAwaitingDns failed: \(error)")
            }
            return .awaitingManualDns
        case .none:
            return .unknown
        }
    }

    /// LoggedIn path — was the entirety of `completeOnboarding` before
    /// the WizardOutcome refactor. Records the identity's home nest in the
    /// registry (moment 4), migrates it into the running `SessionState` and (per target-state
    /// rule 5) deletes the pending-invite slot if one was carried.
    private func performLoggedInHandoff(sessionState: SessionState, append: Bool)
        -> OnboardingExit
    {
        // The MACHINE, never the store (`onboarding.md` § Long-term store
        // contract): the wizard has just authenticated with this secret, so
        // the machine always has it, while the store has it only if moment
        // 1's write landed — a keyring that kept nothing, a row removed out
        // from under the app, or any drive path that seeds the machine
        // without going through a confirm arm all reach `LoggedIn` with an
        // empty store slot but a live machine one. Mirrors tui/android/linux/
        // web's identical lift.
        guard let secretHex = machine.effectiveSecret() else {
            // Diagnostic: this guard should never trip — the secret is set
            // at confirm-identity time. If it does, the user can't be
            // authenticated and the wizard's error path already surfaces no
            // message, so log so we can debug it post-hoc.
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] completeOnboarding: machine.effectiveSecret() returned nil")
            return .unknown
        }
        guard let actorId = try? actor_id_from_secret(secretHex) else {
            // Diagnostic: should not trip — secretHex just came from a
            // wizard-confirmed identity. Needed up front (not just at the
            // sessionState hand-off below) because the device id this
            // identity registers under is resolved per-actor.
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] completeOnboarding: actor_id_from_secret failed for the wizard's own secret")
            return .unknown
        }
        let nodeUrl: String = {
            if case .loggedIn(let url, _) = machine.wizardOutcome() { return url }
            return machine.nestUrl()
        }()
        guard !nodeUrl.isEmpty else { return .unknown }
        clearPendingInviteSlot()
        // Claim terminal (gap CR-1, `architecture/nest/common.md` § Client-state
        // recoverability): the pending-factory-reset slot is spent inside the shared
        // `persistLoggedIn` below, by actor id, as on linux, web and android. A
        // skipped helper (append mode — clearing here would spend the LIVE account's
        // slot) or a failed one deliberately leaves the slot set: the claim was never
        // recorded on this device, so resuming the pre-filled claim surface at the
        // next launch is the recoverable state.
        // The deferred-DNS path's ONE claim terminal for the awaiting slot: an admin who
        // chose "Set up later" for DNS, then reached `LoggedIn` via the post-claim
        // NatModeChoice step or the trust offer. The awaiting-manual-DNS slot outranks
        // every other launch-routing row, so a survivor would pin the next launch on
        // "Almost ready" (`onboarding.md` § App-launch routing); and it is cleared HERE,
        // never at the claim (§ Long-term store contract, ratified 2026-09-21) — through
        // the shared `persistLoggedIn` below on the cold-boot path and through this call
        // on the append arm. A no-op on the ordinary path.
        clearAwaitingDnsSlot()

        // The named `sync_devices` row this identity registers under — the
        // persisted per-account slot when one exists, else derived from the
        // install secret (`sync-agent-credentials.md` § Credential model, the
        // RULED 2026-09-20 block; § Implementation status today → *The
        // derived named-row id*, the secret-store face). `FaunaAccounts
        // .deviceId(forActorId:)` is the one FaunaKit call site every apple
        // mint goes through — onboarding here, and the session-patch doors'
        // non-injected arm (priority #1/#2).
        let deviceId: String
        do {
            deviceId = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)
        } catch {
            // Extremely rare — only when the install secret could not be
            // minted or read back. Never block onboarding on it: fall back to
            // a one-off random id, same as this call site's behaviour before
            // the derivation existed, and log so the miss stays observable.
            deviceId = generate_device_id()
            logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] deviceIdForActor failed: \(error) — minting a fresh random id")
        }
        // Moment 4 — the wizard's logged-in terminal, through the shared helper
        // (`persist_logged_in`): record this identity's home nest **per-actor**,
        // activate it, and spend the pending-invite slot. The per-actor row is the
        // ONLY place the home nest is recorded: the next launch's routing tuple
        // reads it, and without it degrades to `(Some(secret), None, None)` →
        // `WizardAt(HandleEntry)` (`onboarding.md` § App-launch routing).
        //
        // Append mode is exempt (`onboarding.md` § Long-term store contract):
        // its own terminal (`completeAppendedAccount`) registers the identity
        // and switches to it, and activating here would move `active` off the
        // live account first.
        let registry = FaunaAccounts.registry(keychain: keychain)
        if !append {
            do {
                _ = try registry.persistLoggedIn(
                    // The reach hint (onboarding.md § Reach hint) is read off
                    // the machine at this terminal once apple's leg lands;
                    // nil keeps today's behaviour — the account waits for DNS.
                    secretHex: secretHex, nestUrl: nodeUrl, deviceId: deviceId,
                    reachIpv4: nil)
            } catch {
                logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] persist_logged_in failed: \(error)")
            }
            // The predecessor seeds a phrase-only restore recovered — empty on
            // every ordinary onboarding, the only copies left anywhere when not
            // (`identity-succession.md` § Seed escrow). AFTER moment 4 has added
            // and activated the restored identity, so the links name it. An
            // append persists them in `completeAppendedAccount` instead, after
            // its own `addAccount`.
            let predecessors = machine.restoredPredecessors()
            if !predecessors.isEmpty {
                registry.persistRestoredPredecessors(
                    restoredActor: actorId, predecessors: predecessors)
            }
        }

        sessionState.secretHex = secretHex
        sessionState.actorId = actorId
        sessionState.nodeUrl = nodeUrl
        sessionState.deviceId = deviceId
        sessionState.handle = machine.currentHandle()
        // The wizard's handle may be a full address; the registry domain arrives
        // with the launch glue's material (`SessionState.displayHandle`).
        sessionState.domain = nil
        // ⚠ NO `isAuthenticated = true` here. This hand-off is SYNCHRONOUS and runs
        // one async hop before the launch actually mounts the authenticated shell
        // (`onLaunchAuthenticated` → `runLaunch()` → `completeAuthenticatedLaunch`),
        // so assigning the flag here made it readable as `true` while the wizard's
        // own handle-entry page was still on screen — the deterministic macOS red in
        // `test_smoke_k_real_onboarding_completion_reaches_the_main_app`. The flag is
        // derived from the mounted shell now (`SessionState.isAuthenticated`,
        // e2e-conventions.md § convention 11), so it flips itself when the shell
        // arrives and there is nothing to assign.

        // Latch the onboarding enable-email / enable-caldav checkbox intents
        // for the post-auth launch glue (onboarding.md § Enable-email at
        // claim + § Enable-CalDAV at claim). The wizard runs pre-identity, so
        // the checkboxes recorded intent only (`set_enable_email` /
        // `set_enable_caldav`); the launched client's `MailEnableGlue` reads
        // these once the admin/user session exists and runs the Admin-class
        // enable. Mirrors android `OnboardingHost`'s LoggedIn-transition latch
        // + linux's `enable_email`/`enable_caldav` capture at the LoggedIn
        // outcome (priority #2/#3). Cleared on consume by the glue.
        sessionState.pendingFirstSetupMail = machine.emailEnableRequested()
        sessionState.pendingCaldavEnable = machine.caldavEnableRequested()
        sessionState.pendingCarddavEnable = machine.carddavEnableRequested()
        sessionState.pendingWebdavEnable = machine.webdavEnableRequested()
        // The one-tap trust offer's answer (onboarding.md § 3b-ter) — consume-once
        // latch, `false` on a returning-user relaunch (the wizard never reached this
        // step) exactly like a skip. The post-auth launch glue
        // (`MailEnableGlue.applyPendingTrustPromptGrant`) mints the default set.
        sessionState.pendingTrustPromptGranted = machine.takeTrustPromptGranted()
        // The kit confirmed on the `recovery_kit` screen — minted there but
        // deliberately unregistered until a signed-in connection exists
        // (`identity-succession.md` § The RecoveryKey → *Creation UX*).
        // Consume-once; `nil` if skipped. The post-auth launch glue
        // (`MailEnableGlue.registerPendingRecoveryKit`) registers it. An append
        // drops it, as tui's and linux's append handoffs do: the switch that
        // follows clears every onboarding latch, and Settings then says
        // never-created — the truth.
        let pendingKit = machine.takePendingRecoverySecret()
        if append, pendingKit != nil {
            logMessage(level: .warn, target: "fauna.onboarding", message: "[OnboardingVM] add-account: the confirmed recovery kit is not registered on this path")
        }
        sessionState.pendingRecoveryKitHex = append ? nil : pendingKit
        // The DNS credential the wizard verified, latched for the post-auth glue
        // (`MailEnableGlue.sealCapturedDnsCredential`) — like the kit, not on the
        // append path (the nest being joined is not the one this wizard set up).
        sessionState.pendingDnsCredential = append ? nil : machine.capturedDnsCredential()

        // Cache the handle in the account's server-data cache so the next cold
        // launch (and the switcher row) can show it before the silent sign-in
        // refreshes it. The server is the source of truth. Best-effort, and only
        // for an account this terminal registered — an append's identity is
        // registered by `completeAppendedAccount`, which caches the handle itself.
        let cachedHandle = machine.currentHandle()
        if !append, !cachedHandle.isEmpty {
            // `updateCache` overwrites all three fields — carry domain/tier forward.
            let entry = registry.list().first { $0.actorId == actorId }
            do {
                try registry.updateCache(
                    actorId: actorId, handle: cachedHandle,
                    domain: entry?.domain, tier: entry?.tier)
            } catch {
                logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] registry updateCache(handle) failed: \(error)")
            }
        }
        return .feed
    }

    /// Copy the rendered DNS post-instructions to the pasteboard. Shared by
    /// the copy `Button` and its `.automationActivate` so the two can't
    /// diverge (convention: apple-e2e-automation.md § registration
    /// ergonomics) — was a byte-identical per-shell private wrapper on
    /// `DnsPostInstructionsView` (iOS) / `MacDnsPostInstructionsView`
    /// (macOS) until this harvest pass found it .
    public func copyDnsPostInstructions() {
        Pasteboard.copy(machine.dnsPostInstructions() ?? "")
    }
}

/// Trampoline conforming to UniFFI's `OnboardingObserver`. The machine takes
/// the observer at construction time, before `OnboardingVM`'s `self` is fully
/// initialized — late-binding via `target` lets us avoid that ordering.
final class ObserverBox: OnboardingObserver, @unchecked Sendable {
    weak var target: OnboardingVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}

