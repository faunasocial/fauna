import SwiftUI

/// The locked launch surface, ui.yaml page `launch_account_locked`
/// (`docs/goal/ui/sessions.md` § Layout & flow → *The locked surface (leg 2)*;
/// behavior `devices.md` § The locked state), and the onboarding step it opens,
/// `identity_stolen_entry`. Shared by macOS (`MacAppState.LaunchGate.accountLocked`)
/// and iOS (`AppState.accountLocked`), per priority #2; twin of tui's `locked.rs`,
/// linux's launch view `AccountLocked` phase and android's
/// `LaunchAccountLockedScreen`.
///
/// The standing notice — that the account is locked, when the lock ends, and
/// that a lock the owner did not set means somebody holds the secret key — and
/// its ONE action, the stolen-identity ceremony. No Retry (nothing clears the
/// lock before its time; the launch machine's own refresh at that time signs
/// back in) and no fallthrough ("use a different nest" would misstate the
/// problem).
public struct LaunchAccountLockedView: View {
    @Bindable var model: LockedLaunchModel
    /// Activate the successor and re-run launch — each shell's account switch.
    private let onSwitch: @MainActor (String) async -> Void

    public init(model: LockedLaunchModel, onSwitch: @escaping @MainActor (String) async -> Void) {
        self.model = model
        self.onSwitch = onSwitch
    }

    public var body: some View {
        if model.inStolenEntry {
            stolenEntry
        } else {
            notice
        }
    }

    // MARK: - launch_account_locked

    private var notice: some View {
        VStack(spacing: 20) {
            Image(systemName: "lock.fill")
                .font(.system(size: 44))
                .foregroundStyle(.orange)
            Text(L.onboarding.launch.accountLockedTitle)
                .font(.title2.bold())
                .multilineTextAlignment(.center)
            // One element, the three sentences in the ratified order — the
            // witness reads them as one notice.
            automationText(Ids.launchAccountLockedNotice, model.noticeText)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button(L.settings.recoveryKit.stolen) { model.openStolenEntry() }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier(Ids.launchAccountLockedStolenButton)
                .automationActivate(Ids.launchAccountLockedStolenButton) { model.openStolenEntry() }
        }
        .padding(32)
        .frame(maxWidth: 520)
    }

    // MARK: - identity_stolen_entry

    /// Composed of existing elements only (`recovery-entry-phrase-field`,
    /// `identity-stolen-confirm-field`, `identity-stolen-button`,
    /// `recovery-entry-back-button`, `error-message`). The gate starts unarmed
    /// and is re-checked in the action arm; nothing typed here is persisted.
    private var stolenEntry: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(L.settings.recoveryKit.stolen)
                    .font(.title2.bold())
                Text(L.settings.recoveryKit.stolenWarning)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                TextField(L.settings.recoveryKit.kitPhrasePlaceholder, text: $model.phrase)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .autocorrectionDisabled()
                    .accessibilityIdentifier(Ids.recoveryEntryPhraseField)
                    .automationField(Ids.recoveryEntryPhraseField, text: $model.phrase)
                TextField(L.settings.recoveryKit.stolenConfirmPlaceholder, text: $model.confirm)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .accessibilityIdentifier(Ids.identityStolenConfirmField)
                    .automationField(Ids.identityStolenConfirmField, text: $model.confirm)
                if let error = model.error {
                    ErrorBanner(message: error)
                }
                HStack(spacing: 12) {
                    Button(L.common.back) { model.closeStolenEntry() }
                        .controlSize(.large)
                        .disabled(model.busy)
                        .accessibilityIdentifier(Ids.recoveryEntryBackButton)
                        .automationActivate(Ids.recoveryEntryBackButton, isEnabled: { !model.busy }) {
                            model.closeStolenEntry()
                        }
                    Spacer()
                    Button(L.settings.recoveryKit.stolen, role: .destructive) { succeed() }
                        .buttonStyle(.borderedProminent)
                        .tint(.red)
                        .controlSize(.large)
                        .disabled(!model.armed)
                        .accessibilityIdentifier(Ids.identityStolenButton)
                        .automationActivate(Ids.identityStolenButton, isEnabled: { model.armed }) { succeed() }
                }
            }
            .padding(24)
            .frame(maxWidth: 560, alignment: .leading)
        }
    }

    private func succeed() {
        Task { await model.succeed(onSwitch: onSwitch) }
    }
}

/// The locked surface's state — the lock shown, which of its two pages is up,
/// and the stolen-identity step's inputs. Each shell holds one while its launch
/// gate is the lock and asks ``noticeHolds(_:)`` on every launch-machine tick,
/// so the notice follows the machine out with no timer of the app's own.
@MainActor @Observable
public final class LockedLaunchModel {
    /// `LaunchSnapshot.lockedUntilSecs` of the lock this surface shows.
    public let lockedUntilSecs: UInt64
    public private(set) var inStolenEntry = false
    public var phrase = ""
    public var confirm = ""
    public private(set) var busy = false
    public private(set) var error: String?
    /// Re-read the launch machine through the shell's follow — what Back from
    /// `identity_stolen_entry` does, since the notice stood unconditionally
    /// while the step was up and the machine may have moved on meanwhile.
    @ObservationIgnored public var refollow: (() -> Void)?

    public init(lockedUntilSecs: UInt64) {
        self.lockedUntilSecs = lockedUntilSecs
    }

    /// The notice's three sentences, the unlock time through the shared
    /// `format_unix_local`.
    public var noticeText: String {
        [
            L.onboarding.launch.accountLocked,
            L.onboarding.launch.accountLockedUntil(time: formatUnixLocal(secs: Int64(lockedUntilSecs))),
            L.onboarding.launch.accountLockedNotYours,
        ].joined(separator: " ")
    }

    /// Whether the surface still stands on `snapshot` — the shared
    /// `LaunchSnapshot::lock_notice_holds` rule (true while the machine stays
    /// parked on this lock or is mid-flight on its own refresh), and
    /// unconditionally while the user is on `identity_stolen_entry`,
    /// mid-ceremony. Twin of tui's `locked::follow_snapshot`.
    public func noticeHolds(_ snapshot: LaunchSnapshot) -> Bool {
        inStolenEntry || launchLockNoticeHolds(snapshot: snapshot, shownLockedUntilSecs: lockedUntilSecs)
    }

    var armed: Bool { !busy && confirm == RecoveryKitVM.stolenConfirmWord }

    func openStolenEntry() {
        inStolenEntry = true
        error = nil
    }

    /// Back: the inputs are dropped (nothing typed here outlives the step).
    func closeStolenEntry() {
        guard !busy else { return }
        inStolenEntry = false
        phrase = ""
        confirm = ""
        error = nil
        refollow?()
    }

    /// `identity-stolen-button` — the stolen-identity ceremony with NO session
    /// (`devices.md` § The two panic buttons, point 5: reachable from a
    /// locked-out app, over an anonymous connection). The whole ceremony is
    /// shared Rust (`succession_succeed_stolen_locked`); the `SUCCEED` gate is
    /// re-checked here, in the action arm.
    ///
    /// The succession is recorded BEFORE the switch (``SuccessionHandoff``, as
    /// the Settings ceremony does); on the persisted landed arm the successor
    /// is activated and launch re-runs, landing it signed in. Every other arm
    /// speaks on `error-message` and switches nothing — the persist-failure arm
    /// carries the only copy of the successor's key, so it goes on screen.
    func succeed(onSwitch: @MainActor (String) async -> Void) async {
        guard !busy else { return }
        guard confirm == RecoveryKitVM.stolenConfirmWord else {
            error = L.settings.recoveryKit.stolenConfirmPlaceholder
            return
        }
        guard !phrase.isEmpty else {
            error = L.settings.recoveryKit.kitPhraseRequired
            return
        }
        guard let material = FaunaAccounts.sessionMaterial(), let nestUrl = material.nestUrl else {
            error = L.settings.recoveryKit.kitPhraseRequired
            return
        }
        busy = true
        defer { busy = false }
        let outcome: FfiStolenOutcome
        do {
            outcome = try await successionSucceedStolenLocked(
                nestUrl: nestUrl,
                oldSecret: hex_to_data(material.secretHex),
                kitInput: phrase,
                accounts: FaunaAccounts.registry(),
                storePath: SuccessorStorePath(),
                storeContainerDir: AccountStateDir.storeContainerDir)
        } catch {
            // Only a failure BEFORE the ceremony could start reaches here —
            // every ceremony that ran is an outcome.
            self.error = String(describing: error)
            return
        }
        guard let landed = outcome.landed else {
            error = outcome.message.map(renderLocalizedText) ?? outcome.kind
            return
        }
        SuccessionHandoff.record(landed, predecessorActorIdHex: material.actorId)
        guard landed.persisted else {
            error = L.settings.recoveryKit.stolenPersistFailed(secret: landed.successorSecretHex)
            return
        }
        phrase = ""
        confirm = ""
        await onSwitch(landed.newActorIdHex)
    }
}

/// The shared `LaunchMachine`'s observer: forwards every change to ``onTick``
/// on the main actor. The launch flow is otherwise pulled (each entry point is
/// awaited, then one `snapshot()` is dispatched); the tick exists for the one
/// change no app call starts — the machine's own refresh at a lock's unlock
/// time, which the locked surface follows out (``LockedLaunchModel/noticeHolds(_:)``).
/// A tick with no locked surface up is ignored by the shell.
///
/// `@unchecked Sendable`: ``onTick`` is written once, before the machine runs,
/// and only read on the main actor.
public final class LaunchTicks: LaunchObserver, @unchecked Sendable {
    @MainActor public var onTick: (() -> Void)?

    public init() {}

    public func onChanged() {
        Task { @MainActor [weak self] in self?.onTick?() }
    }
}
