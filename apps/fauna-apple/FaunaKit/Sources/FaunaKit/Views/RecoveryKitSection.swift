import SwiftUI

/// The Settings/Account **Recovery Kit** section (`docs/goal/ui/settings.md`
/// § Recovery kit) — the RecoveryKey's Settings home, shared by macOS and iOS.
///
/// Placed immediately after Identity export, which `settings.md` requires and
/// which is also the reason: the two are siblings, each revealing a root secret
/// once, as 64-hex + QR, with a warning beside it, and neither persisting
/// anything. `IdentityExportSection` is this view's structural template.
///
/// **Every decision comes from shared Rust.** The status line's state, which of
/// the actions it enables, and what the succession did arrive already decided
/// through `RecoveryKitVM` → `APIClient` → `libs/fauna-ffi/src/recovery.rs`. ⚠
/// Nothing here may re-derive enablement from `status.kind`: `allowsStolen` is
/// unconditionally true (theft is exactly the no-kit case) and `allowsReplace`
/// stays true during a pending window.
///
/// **The section holds no secret longer than the screen shows it.** A minted kit
/// is displayed once and dropped when the page goes away; there is deliberately
/// no "show it again" path and there can never be one
/// (`identity-succession.md` § The RecoveryKey — *Custody*).
public struct RecoveryKitSection: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = RecoveryKitVM()
    /// Whether this section is currently in the view tree.
    ///
    /// ⚠ Load-bearing, not bookkeeping: an account switch can leave TWO of these
    /// sections mounted at once (iOS pops the settings page in the teardown and
    /// re-pushes it a main-actor turn later, so the outgoing one outlives the
    /// incoming one's mount), and the succession's owed kit is a ONE-SHOT claim.
    /// Without this, the section being torn down can win that claim and mint into
    /// a view model that is about to leave the screen. Defaults to `true` so a
    /// section whose `.task` outruns its `onAppear` is never wrongly blocked —
    /// only an observed `onDisappear` sets it false.
    @State private var onScreen = true

    /// The identity the current `vm` state belongs to — `nil` before the first
    /// hydrate. Compared, never rendered: see `hydrate()`.
    @State private var hydratedActor: String?

    /// Switch to the successor (its actor id) after a succession that the device
    /// managed to persist — the account moved, so this is an account *switch*,
    /// never the sign-out reset the delete path takes. ⚠ Not called on the
    /// persist-failure arm: tearing the session down there takes the only copy of
    /// the successor seed with it.
    private let onSucceeded: (String) -> Void

    /// The signed-in identity — **one value, two roles, and that is the point.**
    /// At ceremony time it is the identity the account is moving *away* from, so
    /// the succession records which registry row the retired identity is before
    /// the switch makes it unnameable. At hydrate time it is whoever is signed in
    /// *now*, which is what lets the owed kit be claimed by the successor's
    /// session and refused to the departing one (`SuccessionHandoff`).
    private let sessionActorIdHex: String?

    public init(sessionActorIdHex: String?, onSucceeded: @escaping (String) -> Void) {
        self.sessionActorIdHex = sessionActorIdHex
        self.onSucceeded = onSucceeded
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            sweepChrome
            aftermathProgressLines

            automationText(Ids.recoveryKitStatus, statusLine)
                .font(.caption)
                .foregroundStyle(.secondary)

            Text(L.settings.recoveryKit.desc)
                .font(.caption)
                .foregroundStyle(.secondary)

            mintedKitDisplay

            // The kit-in-hand entry. Rendered whenever a ceremony that consumes
            // one is available, which is what `optional_elements` means for this
            // id — it is the onboarding `recovery_entry` screen's own field
            // reused inline, not a settings-scoped twin.
            if vm.phraseFieldVisible {
                TextField(L.settings.recoveryKit.kitPhrasePlaceholder, text: $vm.phraseInput)
                    .textFieldStyle(.roundedBorder)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
                    .accessibilityIdentifier(Ids.recoveryEntryPhraseField)
                    .automationField(Ids.recoveryEntryPhraseField, text: $vm.phraseInput)
            }

            actionButtons
            stolenAction
            letGoAction

            if let error = vm.errorText {
                ErrorBanner(message: error)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.recoveryKitSection)
        // The container needs a live registration of its own, not just an
        // accessibility id: `is_visible` reads the in-process AutomationRegistry
        // rather than the AX tree, so a bare `.accessibilityIdentifier` here
        // would leave the section reading as absent while its children resolve.
        .automationValue(Ids.recoveryKitSection, text: { statusLine })
        // Keyed, not bare: this section outlives both of the events that
        // invalidate everything in it. **A session identity change** — an account
        // switch, or the succession's own switch — re-scopes the whole section,
        // and a bare `.task` never re-fires because the view is never re-mounted
        // (the user is already standing on this page, which is exactly where a
        // succession leaves them). **A client arriving a beat late** would
        // otherwise leave an owed kit undischarged until the user happened back.
        // One key covers both, and `.task(id:)` cancels the in-flight pass on
        // change rather than racing it.
        .task(id: hydrateKey) { await hydrate() }
        .onAppear {
            onScreen = true
            StolenCeremonyHold.shared.accountAppeared()
        }
        .onDisappear {
            onScreen = false
            // Leaving Account is also the edge a held-back supersession waits
            // for (`settings.md` § Recovery kit → *The persist-failure message
            // survives the page*): the user has had the whole visit to copy the
            // key, and the dead session under it may now go the ordinary way.
            Task { @MainActor in await StolenCeremonyHold.shared.accountLeft() }
            // Logged because this is the one edge that can un-show an
            // already-minted kit, and it is invisible from every other
            // witness: the section reads empty afterwards exactly as it does
            // when the mint never ran. Lengths only, never the secret.
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] section disappeared "
                                + "(held=\(vm.mintedSecretHex?.count ?? 0) hex) → clearing")
            vm.clearHeldSecrets()
        }
    }

    /// What makes this section's state stale: who is signed in, and **which
    /// actor's seat** the client in the environment signs as. Anything else is a
    /// render, not a re-read.
    ///
    /// ⚠ The second half was `client == nil ? "no-client" : "client"` until
    /// 2026-08-26, and mere presence cannot see the event this section exists to
    /// survive: an account switch replaces one *live* `FaunaClient` with another,
    /// so the key never changed and `.task(id:)` never re-fired. A hydrate that
    /// ran while the environment still held the PREDECESSOR's client therefore
    /// configured the view model with that seat and kept it for the rest of the
    /// session — measured on iOS: the successor's kit
    /// minted five times over a socket the ceremony had revoked inside the nest's
    /// own transaction, 43 s, every attempt `fauna.protocol.disconnected`. macOS
    /// could not show it — `launchGate` keeps that shell unmounted until the nav
    /// is already correct, so its section mounts once with the right client,
    /// while iOS's persistent TabView has no such gate (the same asymmetry behind
    /// this row's first two defects).
    private var hydrateKey: String {
        "\(sessionActorIdHex ?? "-")|\(client?.api.boundActorIdHex ?? "no-client")"
    }

    /// Read the chain, then discharge an owed successor kit if this session is
    /// the one that owes it.
    ///
    /// **Order is load-bearing.** The status read first, so an ordinary visit
    /// paints its state before anything else; the discharge second, because it
    /// mints — and `runMint` re-reads the status itself, so the section ends on
    /// the successor's *new* state rather than the never-created one it opened
    /// on. The navigation that lands the user here is the launch's own act
    /// (`identity-succession.md` § The RecoveryKey → *At succession*: navigate
    /// synchronously, mint after) — never this view's, which by existing has
    /// already been navigated to.
    private func hydrate() async {
        // An identity change first, before anything reads or paints: a kit minted
        // for the account we just left must not linger on screen for the one we
        // just entered (the shown-once custody rule does not stop at a switch),
        // and the previous actor's status is not a weaker answer for this one —
        // it is the wrong answer, and every action's enablement hangs off it.
        // Measured: sitting on this page across a switch left the section
        // offering the PREVIOUS account's actions (2026-08-22 journey run).

        // `[succession-kit]` — the successor's closing act crosses an account
        // switch, a client rebuild and a view re-mount before it can paint, and
        // every one of those is silent. This line and its `disappeared`/`discharge`
        // siblings are what separate "hydrate never fired" from "fired with no
        // client" from "fired and the claim was refused" from "minted and then
        // wiped" — four states one empty `recovery-kit-secret-display` cannot
        // tell apart, and the reason an iOS failure
        // cost two blind runs. Identities and lengths only, never the secret.
        logMessage(level: .info, target: "fauna.app",
                   message: "[succession-kit] hydrate: actor=\(sessionActorIdHex ?? "-") "
                            + "was=\(hydratedActor ?? "<first>") "
                            + "client=\(client == nil ? "nil" : "live") "
                            + "kitOwed=\(SuccessionHandoff.kitOwed) "
                            + "owedTo=\(SuccessionHandoff.successorActorIdHex ?? "-")")
        if let previous = hydratedActor, previous != (sessionActorIdHex ?? "-") {
            vm.resetForIdentityChange()
        }
        hydratedActor = sessionActorIdHex ?? "-"
        if let client { vm.configure(api: client.api) }
        // Mirrored, not consumed: `SuccessionHandoff.sweep` outlives every
        // hydrate (same lifetime as `sweepStateJson`, cleared only on a
        // factory reset), so re-reading it here on an ordinary re-hydrate is
        // idempotent — it only ever changes via `record` or a successful
        // `retrySweep()`, both of which this mirror must reflect.
        vm.sweepView = SuccessionHandoff.sweep
        await vm.loadStatus()
        await vm.loadDeadGenerations()

        // The claim is ONE-SHOT, so only a section that is actually on screen may
        // take it — see `onScreen`. A section on its way out that claimed here
        // would mint the successor's only RecoveryKey into a view model the user
        // can no longer see, and the obligation would be spent.
        if onScreen {
            // A relaunch adoption's owed sweep first — the ceremony's own order
            // (sweep, then kit). A no-op on every other hydrate.
            await vm.dischargeOwedSweep(sessionActorIdHex: sessionActorIdHex)
            await vm.dischargeOwedSuccessionKit(sessionActorIdHex: sessionActorIdHex)
            // The residual race, closed: `onDisappear` can still land between the
            // check above and the mint below (the ceremony is a network round
            // trip). A secret held by a section that is no longer on screen was
            // shown to nobody, so put the obligation back rather than let it be
            // spent — `rearmUnshownKit` carries the full argument for why erring
            // this way is the only safe direction. The kit ITSELF goes with the
            // obligation: it is now the registered chain head, so the next live
            // section must show it, never mint again (a second no-prior mint is
            // refused — the 2026-09-24 re-mint loop).
            if !onScreen, let stranded = vm.heldKit, let successor = sessionActorIdHex {
                logMessage(level: .warn, target: "fauna.app",
                           message: "[succession-kit] minted into a section that left the "
                                    + "screen — handing the kit to the next live one for "
                                    + "\(successor)")
                SuccessionHandoff.rearmUnshownKit(successor: successor, stranded: stranded)
                vm.clearHeldSecrets()
            }
        } else if SuccessionHandoff.kitOwed {
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] discharge skipped: section is off-screen "
                                + "— leaving the obligation for the live one")
        }
        logMessage(level: .info, target: "fauna.app",
                   message: "[succession-kit] hydrate done: on-screen="
                            + "\(vm.mintedSecretHex?.count ?? 0) hex")
    }

    // MARK: - Post-succession sweep

    /// The post-succession group sweep's own lines, at the TOP of the section
    /// — tui's order, above the aftermath progress lines below
    /// (`docs/goal/ui/settings.md` § Recovery kit → *The sweep's own lines*).
    /// The two lines carry the ids the user approved 2026-09-25 —
    /// `recovery-kit-sweep-status` (what the sweep did) and, its OWN element,
    /// never a qualifier on the first, `recovery-kit-sweep-unvouched-status`
    /// (the roster it cannot vouch for) — so the outcome the user is told can
    /// be read off the screen as well as asserted as STATE
    /// (`data.succession_sweep`, which stays as the lines' machine twin). Each
    /// element is ABSENT, never present and empty, when the projection
    /// returns no line for it. **Selected off the carried view, never matched
    /// on `kind` here** — which arm says what, and the silence on a
    /// succession with no groups at all, are the shared `sweep_copy`
    /// projection's alone.
    @ViewBuilder private var sweepChrome: some View {
        if let view = vm.sweepView {
            let copy = sweepCopy(view: view, rendersRetry: true)
            if let outcome = copy.outcome {
                automationText(Ids.recoveryKitSweepStatus, renderLocalizedText(outcome))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let unattested = copy.unattested {
                automationText(Ids.recoveryKitSweepUnvouchedStatus,
                               renderLocalizedText(unattested))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The retry, directly under the two lines — gated on `owesWork`,
            // deliberately NOT on whether THIS device can finish it: hiding
            // the button where the retry cannot run would leave the degraded
            // copy above naming a control that is not on screen, the exact
            // dishonesty the gate exists to prevent. Every press answers in
            // words on `error-message` (`retrySweep`'s doc).
            if view.owesWork {
                Button(L.settings.recoveryKit.sweepRetry) {
                    Task { await vm.retrySweep() }
                }
                .disabled(vm.busy)
                .accessibilityIdentifier(Ids.recoveryKitSweepRetryButton)
                .automationActivate(Ids.recoveryKitSweepRetryButton,
                                    isEnabled: { !vm.busy }) {
                    Task { await vm.retrySweep() }
                }
            }
        }
    }

    // MARK: - Aftermath progress

    /// The seven post-succession aftermath-progress lines
    /// (`docs/goal/ui/settings.md` § Recovery kit → *The post-succession
    /// aftermath's progress lines*; the seventh,
    /// `recovery-kit-inherited-filters-status`, is `succession-aftermath.md`'s
    /// own — Rule-A sign-off 2026-08-15). Rendered ABOVE `statusLine`, the same
    /// order tui/linux/web use: they are the freshest thing on this screen and
    /// describe what a ceremony/pass just did, while `statusLine` describes the
    /// kit's own standing state. Each line renders a **shared projection's
    /// already-resolved text** via `FaunaClient.aftermathProgress`, never a
    /// Swift `match` on the leg (priority #1/#3).
    @ViewBuilder private var aftermathProgressLines: some View {
        let progress = client?.aftermathProgress ?? AftermathProgress()

        if let line = progress.backupRegrant {
            automationText(Ids.recoveryKitBackupRegrantStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        // Leg 3 — reports from `fauna-ffi`'s __mls re-seal launcher, a
        // different pass than the rest; see `AftermathProgress.mlsReseal`.
        if let line = progress.mlsReseal {
            automationText(Ids.recoveryKitMlsResealStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        if let line = progress.grantRemint {
            automationText(Ids.recoveryKitGrantRemintStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        // Leg 5 — always nil on this app today; see `AftermathProgress.corpusReseal`.
        if let line = progress.corpusReseal {
            automationText(Ids.recoveryKitCorpusResealStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        // Leg 7 before leg 6, same reason the task runs them in that order: the
        // burn is the only leg that takes something away, so every restoring
        // leg reports above it.
        if let line = progress.draftsReseal {
            automationText(Ids.recoveryKitDraftsResealStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        if let line = progress.mailBurn {
            automationText(Ids.recoveryKitMailBurnStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        if let count = client?.recoveryInheritedFiltersOpen, count > 0 {
            automationText(
                Ids.recoveryKitInheritedFiltersStatus,
                L.settings.recoveryKit.inheritedFilters(count: "\(count)")
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    // MARK: - Status

    /// The one status line. An un-hydrated section paints `status_loading`
    /// rather than nothing — it still owes the user a reason for the dead
    /// buttons (`ui/README.md` rule 5) — and the driver reads that as "" so it
    /// stays usable as the causal barrier for "the chain read completed".
    private var statusLine: String {
        guard let status = vm.status else { return L.settings.recoveryKit.statusLoading }
        switch status.kind {
        case "never-created": return L.settings.recoveryKit.statusNeverCreated
        case "registered": return L.settings.recoveryKit.statusRegistered
        case "registered-no-escrow": return L.settings.recoveryKit.statusRegisteredNoEscrow
        case "replacement-pending":
            return L.settings.recoveryKit.statusReplacementPending(days: pendingDays)
        default:
            // A kind this build does not know is a NEWER nest/app pairing, not a
            // corrupt read. Say the honest thing rather than inventing a state.
            return L.settings.recoveryKit.statusLoading
        }
    }

    /// Whole days left in a pending window — the shared rounding rule
    /// (`libs/fauna-client-recovery/src/replacement.rs::days_remaining_from`),
    /// not re-derived here: only `now` is this view's to own, matching
    /// `remaining_secs`'s own contract of a caller-supplied clock.
    private var pendingDays: String {
        guard let landsAt = vm.status?.pendingLandsAt else { return "0" }
        let now = Int64(Date().timeIntervalSince1970)
        return String(recoveryPendingDaysRemaining(landsAt: landsAt, now: now))
    }

    // MARK: - The shown-once kit

    /// A ceremony's minted secret, rendered through the **onboarding screen's
    /// own** ids rather than settings-specific twins, because it is the same
    /// artifact shown the same way (priority #3).
    @ViewBuilder private var mintedKitDisplay: some View {
        if let secret = vm.mintedSecretHex {
            // The display is the bare hex (what a user writes on paper); the copy
            // button and the QR both carry the account-naming `fauna://recovery`
            // URI the VM built once at mint — a copied kit then restores knowing
            // its account exactly as a scanned one does (`identity-succession.md`
            // § The RecoveryKey, *Which encoding each affordance carries*). No
            // URI (the seed-shown arm, or a builder refusal) → the bare secret,
            // which the restore parser accepts too.
            let payload = vm.mintedKitUri ?? secret
            VStack(alignment: .leading, spacing: 8) {
                automationText(Ids.recoveryKitSecretDisplay, secret)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)

                Button(L.common.copy) { Pasteboard.copy(payload) }
                    .accessibilityIdentifier(Ids.recoveryKitSecretCopyBtn)
                    .automationActivate(Ids.recoveryKitSecretCopyBtn) {
                        Pasteboard.copy(payload)
                    }

                if let matrix = try? qrMatrix(data: payload) {
                    QrCodeView(matrix: matrix)
                        .frame(width: 200, height: 200)
                        .accessibilityIdentifier(Ids.recoveryKitQr)
                        .automationValue(Ids.recoveryKitQr, text: { "\(matrix.size)" })
                }

                // ⚠ A failed escrow put is NOT rendered as an error: the
                // registration has landed, so the secret above is live and is the
                // only copy in existence. Rendering a plain error here would tell
                // the user to discard the one thing they must write down.
                if !vm.mintedEscrowStored {
                    Text(L.settings.recoveryKit.statusRegisteredNoEscrow)
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
        }
    }

    // MARK: - Actions

    @ViewBuilder private var actionButtons: some View {
        if let status = vm.status {
            Button(L.settings.recoveryKit.create) {
                Task { await vm.createOrReplaceKit(usingHeldPhrase: false) }
            }
            .disabled(!status.allowsCreate || vm.busy)
            .accessibilityIdentifier(Ids.recoveryKitCreateButton)
            .automationActivate(Ids.recoveryKitCreateButton,
                                isEnabled: { status.allowsCreate && !vm.busy }) {
                Task { await vm.createOrReplaceKit(usingHeldPhrase: false) }
            }
            .faunaGate("fauna.recovery.registration.submit")

            Button(L.settings.recoveryKit.replace) {
                Task { await vm.createOrReplaceKit(usingHeldPhrase: true) }
            }
            .disabled(!status.allowsReplace || vm.busy)
            .accessibilityIdentifier(Ids.recoveryKitReplaceButton)
            .automationActivate(Ids.recoveryKitReplaceButton,
                                isEnabled: { status.allowsReplace && !vm.busy }) {
                Task { await vm.createOrReplaceKit(usingHeldPhrase: true) }
            }
            .faunaGate("fauna.recovery.registration.submit")

            Button(L.settings.recoveryKit.lost) {
                Task { await vm.requestSeedAloneReplacement() }
            }
            .disabled(!status.allowsLost || vm.busy)
            .accessibilityIdentifier(Ids.recoveryKitLostButton)
            .automationActivate(Ids.recoveryKitLostButton,
                                isEnabled: { status.allowsLost && !vm.busy }) {
                Task { await vm.requestSeedAloneReplacement() }
            }
            .faunaGate("fauna.recovery.replacement.request")

            // Renders only in the no-escrow state — the kit-in-hand re-put that
            // restores phrase recovery WITHOUT retiring the held kit.
            if status.allowsEscrowReseal {
                Button(L.settings.recoveryKit.escrowReseal) {
                    Task { await vm.resealEscrowWithHeldKit() }
                }
                .disabled(vm.busy)
                .accessibilityIdentifier(Ids.recoveryKitEscrowResealButton)
                .automationActivate(Ids.recoveryKitEscrowResealButton,
                                    isEnabled: { !vm.busy }) {
                    Task { await vm.resealEscrowWithHeldKit() }
                }
                .faunaGate("fauna.recovery.escrow.put")
            }

            // Renders only while a seed-alone replacement is in its window.
            if status.pendingLandsAt != nil {
                Button(L.settings.recoveryKit.veto) {
                    Task { await vm.vetoPendingReplacement() }
                }
                .disabled(vm.busy)
                .accessibilityIdentifier(Ids.recoveryPendingVetoButton)
                .automationActivate(Ids.recoveryPendingVetoButton,
                                    isEnabled: { !vm.busy }) {
                    Task { await vm.vetoPendingReplacement() }
                }
                .faunaGate("fauna.recovery.replacement.veto")
            }
        }
    }

    /// The succession trigger and its type-to-confirm gate — the same idiom
    /// account deletion uses, for the same reason: this is irreversible and
    /// re-points the whole account, so a bare click must not reach it.
    ///
    /// The warning rides as ID-less chrome, exactly as tui and web render it.
    /// Render gate: [`RecoveryKitVM/stolenVisible`].
    @ViewBuilder private var stolenAction: some View {
        if vm.stolenVisible {
            Text(L.settings.recoveryKit.stolenWarning)
                .font(.caption)
                .foregroundStyle(.orange)

            TextField(L.settings.recoveryKit.stolenConfirmPlaceholder,
                      text: $vm.stolenConfirmInput)
                .textFieldStyle(.roundedBorder)
                #if os(iOS)
                // The gate compares against a literal, so an autocapitalized
                // "Succeed" would never arm the button and the user would have no
                // way to see why.
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                #endif
                .accessibilityIdentifier(Ids.identityStolenConfirmField)
                .automationField(Ids.identityStolenConfirmField, text: $vm.stolenConfirmInput)

            Button(L.settings.recoveryKit.stolen, role: .destructive) {
                Task {
                    await vm.succeedWithHeldKit(
                        predecessorActorIdHex: sessionActorIdHex,
                        onSucceeded: onSucceeded)
                }
            }
            .disabled(!vm.stolenArmed)
            .accessibilityIdentifier(Ids.identityStolenButton)
            .automationActivate(Ids.identityStolenButton,
                                isEnabled: { vm.stolenArmed }) {
                Task {
                    await vm.succeedWithHeldKit(
                        predecessorActorIdHex: sessionActorIdHex,
                        onSucceeded: onSucceeded)
                }
            }
            // The COMMIT gates, not the buffer — the two fields beside it stay
            // typeable with no nest, exactly as account deletion's do.
            .faunaGate("fauna.recovery.succession.submit")
        }
    }

    /// The let-go of a dead generation, beside the four actions
    /// (`settings.md` § Recovery kit, the fifth act; `account-data-taxonomy.md`
    /// § The generation machinery → *Fleet-scope reclamation*, clause (3)(j)):
    /// the line, its type-to-confirm field and its button — all three ONLY
    /// while the runtime's dead read answers non-empty, the veto's shape. The
    /// line's copy is the shared projection's; the button arms on the confirm
    /// word alone, and `RecoveryKitVM.letGo()` re-checks it when it fires.
    @ViewBuilder private var letGoAction: some View {
        if let line = vm.letGoStatus {
            automationText(Ids.recoveryKitUnreadableStatus, renderLocalizedText(line))
                .font(.caption)
                .foregroundStyle(.orange)

            TextField(L.settings.recoveryKit.letGoConfirmPlaceholder,
                      text: $vm.letGoConfirmInput)
                .textFieldStyle(.roundedBorder)
                #if os(iOS)
                // The gate compares against an upper-case literal, so the
                // keyboard opens in capitals and autocorrect must not rewrite
                // what is typed.
                .textInputAutocapitalization(.characters)
                .autocorrectionDisabled()
                #endif
                .accessibilityIdentifier(Ids.recoveryKitLetGoConfirmField)
                .automationField(Ids.recoveryKitLetGoConfirmField, text: $vm.letGoConfirmInput)

            Button(L.settings.recoveryKit.letGo, role: .destructive) {
                Task { await vm.letGo() }
            }
            .disabled(!vm.letGoArmed)
            .accessibilityIdentifier(Ids.recoveryKitLetGoButton)
            .automationActivate(Ids.recoveryKitLetGoButton,
                                isEnabled: { vm.letGoArmed }) {
                Task { await vm.letGo() }
            }
            // No offline gate: the act ends in the retire door, which the
            // shared table declares OfflineSafe, so nothing here desensitizes.
        }
    }

}
