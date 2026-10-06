import SwiftUI

/// The Settings/Account **Recovery Kit** section's state
/// (`docs/goal/ui/settings.md` § Recovery kit).
///
/// ## What this deliberately does NOT do
///
/// It decides nothing. The status line's state, which of the four actions each
/// state enables, and what the succession ceremony did all arrive already
/// decided from `libs/fauna-ffi/src/recovery.rs` — which in turn composes the
/// shared `fauna_client_recovery` ceremonies every other app runs. **Never
/// derive enablement from `status.kind` here.** Two of the predicates do not
/// follow from the state the way a renderer would guess:
///
/// * `allowsStolen` is **unconditionally true**, including with no kit ever
///   created — a thief who took the seed before a kit existed is precisely the
///   case the ceremony is for; and
/// * `allowsReplace` stays true **during** a pending replacement window, because
///   replacing with a kit you hold is how an owner ends that window at once.
///
/// Both are pinned Rust-side by `enablement_does_not_follow_from_the_status_kind`.
///
/// ## The kit is held only while the screen shows it
///
/// `mintedSecretHex` is the single copy of a freshly minted recovery root in
/// this process. There is no path that shows it again and there can never be one
/// (`identity-succession.md` § The RecoveryKey — *Custody*), so it is dropped on
/// leaving the page and never written anywhere.
@MainActor @Observable
public class RecoveryKitVM {
    /// The section's state, or `nil` until the chain read resolves. An
    /// un-hydrated section must not *claim* a state — the view paints
    /// `status_loading` in that window, which is not an answer.
    public var status: FfiRecoveryKitStatus?
    /// True while the chain read is in flight.
    public var loading = false

    /// The kit-in-hand buffer behind `recovery-entry-phrase-field` — the
    /// onboarding screen's own id, reused inline (`settings.md` § Recovery kit →
    /// *Kit-in-hand entry*). Feeds replace, stolen, veto and the escrow re-seal.
    public var phraseInput = ""
    /// The type-to-confirm buffer behind `identity-stolen-confirm-field`.
    public var stolenConfirmInput = ""

    /// A secret a ceremony just minted, shown once. Cleared on leaving the page.
    public var mintedSecretHex: String?
    /// The `fauna://recovery` URI for `mintedSecretHex` — what the copy button
    /// puts on the pasteboard and the QR encodes (`identity-succession.md` § The
    /// RecoveryKey, *Which encoding each affordance carries*); the on-screen
    /// display stays the bare hex. Built ONCE per mint, in the same synchronous
    /// step that sets the secret, so the view never paints a copy button or QR
    /// the URI has not reached. `nil` for the one arm that shows a seed rather
    /// than a kit (the stolen ceremony's persist-failure fallback) and when the
    /// builder refuses — the view then falls back to the bare secret, which the
    /// restore parser accepts too. It embeds the secret, so it is dropped with it.
    public var mintedKitUri: String?
    /// Whether the escrow blob landed with the registration that minted
    /// `mintedSecretHex`. ⚠ `false` is **not** an error to render: the
    /// registration has already landed, so the shown secret is live and is the
    /// only copy in existence. Say the kit works *and* that phrase recovery is
    /// not yet armed.
    public var mintedEscrowStored = true
    /// Unix seconds a seed-alone replacement lands, when the mint came from the
    /// `lost` ceremony. `nil` for create and replace, which land immediately.
    public var mintedLandsAt: Int64?

    /// True while any ceremony is in flight — every button reads it, so a second
    /// click cannot start a second irreversible ceremony.
    public var busy = false
    /// The section's one error surface. ⚠ Never assign this directly outside
    /// `parkStolenMessage(_:)` — every other writer in this view model MUST go
    /// through `setErrorText(_:)`, which drops the write while a parked
    /// stolen-ceremony message is still pending acknowledgment
    /// (`stolenFailedMessagePending`).
    public var errorText: String?
    /// True while a stolen-identity ceremony's parked message — the landed
    /// arm's persist-failure message, or the undecided outcome that
    /// `carriesTheOnlySeed` (`applyStolenOutcome`) — sits in `errorText`,
    /// not yet acknowledged (`docs/goal/ui/settings.md` § Recovery kit → *The
    /// persist-failure message survives the page*) — it is the ONLY surviving
    /// copy of the account's new key, so it must win over every other write to
    /// this shared slot until the user leaves the Account sub-page or the
    /// signed-in identity changes (`acknowledgeStolenFailedMessage`). Mirrors
    /// linux's `PENDING_STOLEN_FAILED_MESSAGE`
    /// (`apps/fauna-linux/src/settings/mod.rs`). Internal, not `private`, so a
    /// test can seed the parked state directly rather than driving the whole
    /// async ceremony through a real `APIClient`.
    var stolenFailedMessagePending = false

    /// The succession's outcome, held **only** so the view can render the
    /// persist-failure arm. ⚠ When `persisted` is false the caller must NOT tear
    /// the session down: that takes the only copy of the successor seed with it.
    public var landedSuccession: FfiLandedSuccession?

    /// The post-succession sweep's own view, mirrored from
    /// `SuccessionHandoff.sweep` at hydrate — carried across the account
    /// switch, replaced by `retrySweep()` on a `swept` answer, and read by the
    /// section to select `sweep_copy` at paint time (`settings.md` § Recovery
    /// kit → *The sweep's own lines*). `nil` on every ordinary sign-in that did
    /// not just run a ceremony.
    public var sweepView: FfiSweepView?

    /// The account runtime's last dead read — what the let-go renders from
    /// (`settings.md` § Recovery kit, the fifth act;
    /// `account-data-taxonomy.md` clause (3)(j)). Empty until the read lands,
    /// and whenever there is no runtime.
    public var deadGenerations = FfiDeadGenerations(generationIds: [], unreadableStatus: nil)
    /// The type-to-confirm buffer behind `recovery-kit-let-go-confirm-field`.
    public var letGoConfirmInput = ""

    private var api: APIClient?
    /// Where the stolen ceremony reports that it owns the Account page, so a
    /// supersession it causes is held back rather than tearing the page down
    /// (``StolenCeremonyHold``).
    private let ceremonyHold: StolenCeremonyHold

    public init(ceremonyHold: StolenCeremonyHold? = nil) {
        self.ceremonyHold = ceremonyHold ?? .shared
    }

    public func configure(api: APIClient) {
        self.api = api
    }

    /// Read the section's state from the **registration chain**, never a local
    /// flag — so a kit created on another device is reflected here.
    public func loadStatus() async {
        guard let api else { return }
        loading = true
        defer { loading = false }
        do {
            status = try await api.recoveryKitStatus()
        } catch {
            // A section that cannot read its chain paints no state and says why;
            // it must not fall back to a state it did not observe, because every
            // action's enablement hangs off that read.
            status = nil
            setErrorText(String(describing: error))
        }
    }

    /// Drop everything a **different identity** put here — held secrets, and the
    /// status those secrets were read against.
    ///
    /// The extra thing this does over ``clearHeldSecrets()`` is drop `status`,
    /// and that is the load-bearing half: every action's enablement hangs off it
    /// (`allowsCreate` and friends), so carrying the previous account's status
    /// into this one does not render a *weaker* answer, it renders the **wrong**
    /// one — the section offers ceremonies the signed-in account cannot run and
    /// withholds ones it can. `nil` paints `status_loading`, which is honest
    /// while the new actor's chain read is in flight.
    public func resetForIdentityChange() {
        clearHeldSecrets()
        status = nil
        // The previous account's dead read lists ITS generations: the let-go
        // must not be offered to this one until its own read lands.
        deadGenerations = FfiDeadGenerations(generationIds: [], unreadableStatus: nil)
        setErrorText(nil)
    }

    /// Drop everything the screen was holding. Called when the page goes away:
    /// the minted secret must not survive the view that displayed it, and the
    /// two typed buffers are a recovery phrase and a confirm token.
    public func clearHeldSecrets() {
        mintedSecretHex = nil
        mintedKitUri = nil
        mintedLandsAt = nil
        mintedEscrowStored = true
        phraseInput = ""
        stolenConfirmInput = ""
        // An armed gate must not survive a page leave: returning to the page
        // never finds an irreversible act already unlocked.
        letGoConfirmInput = ""
        landedSuccession = nil
        acknowledgeStolenFailedMessage()
    }

    /// Discharge a still-pending persist-failure message: the user has left
    /// the Account sub-page (`.onDisappear`, the off-screen branch of
    /// `RecoveryKitSection.hydrate()`) or the signed-in identity changed
    /// (`resetForIdentityChange`) — either way they had the whole prior visit
    /// to read or copy the successor's key (`settings.md` § Recovery kit →
    /// *The persist-failure message survives the page*, the one
    /// acknowledgment gesture this fix defines — no new `ui.yaml` element). A
    /// no-op when nothing is pending. Folded into `clearHeldSecrets()` above
    /// rather than called separately at each of its three call sites, since
    /// all three ARE this edge — that ordering is also what keeps
    /// `resetForIdentityChange`'s own `setErrorText(nil)` from being dropped
    /// by its own guard: the discharge always runs first. `public`, not
    /// `private`, for the same testability reason as
    /// `stolenFailedMessagePending`.
    public func acknowledgeStolenFailedMessage() {
        stolenFailedMessagePending = false
    }

    /// Write `errorText` — UNLESS a stolen-ceremony persist-failure message is
    /// still pending acknowledgment, in which case the write is dropped
    /// rather than clobbering the only surviving copy of the successor's key.
    /// Every writer of `errorText` in this view model, other than the park
    /// write itself (`parkStolenMessage(_:)`), MUST
    /// call this — never assign `errorText` directly. Mirrors linux's
    /// `render_account_error_label` (`apps/fauna-linux/src/settings/mod.rs`).
    /// Internal, not `private`, for the same testability reason as
    /// `stolenFailedMessagePending`: every other writer that reaches this
    /// guard needs a live `api`, so a probe exercises the guard itself
    /// directly instead of driving a ceremony through a real `APIClient`.
    func setErrorText(_ text: String?) {
        guard !stolenFailedMessagePending else { return }
        errorText = text
    }

    /// `recovery-kit-create-button` (no phrase) and `recovery-kit-replace-button`
    /// (the phrase in hand) — one ceremony, two authorization arms.
    public func createOrReplaceKit(usingHeldPhrase: Bool) async {
        guard let api, !busy else { return }
        if usingHeldPhrase && phraseInput.isEmpty {
            setErrorText(L.settings.recoveryKit.kitPhraseRequired)
            return
        }
        await runMint { try await api.recoveryCreateKit(
            heldKitInput: usingHeldPhrase ? self.phraseInput : nil) }
    }

    /// `recovery-kit-lost-button` — opens the 30-day window rather than taking
    /// effect now. Still mints and shows a secret immediately.
    public func requestSeedAloneReplacement() async {
        guard let api, !busy else { return }
        await runMint { try await api.recoveryRequestSeedAloneReplacement() }
    }

    /// `recovery-pending-veto-button` — contest a pending replacement.
    public func vetoPendingReplacement() async {
        guard let api, !busy else { return }
        guard !phraseInput.isEmpty else {
            setErrorText(L.settings.recoveryKit.kitPhraseRequired)
            return
        }
        busy = true
        defer { busy = false }
        do {
            _ = try await api.recoveryVetoPendingReplacement(heldKitInput: phraseInput)
            phraseInput = ""
            await loadStatus()
        } catch {
            setErrorText(String(describing: error))
        }
    }

    /// `recovery-kit-escrow-reseal-button` — the no-escrow repair. Restores
    /// phrase recovery **without** retiring the kit in hand, which is why it is
    /// not a create.
    public func resealEscrowWithHeldKit() async {
        guard let api, !busy else { return }
        guard !phraseInput.isEmpty else {
            setErrorText(L.settings.recoveryKit.kitPhraseRequired)
            return
        }
        busy = true
        defer { busy = false }
        do {
            _ = try await api.recoveryResealEscrowWithHeldKit(heldKitInput: phraseInput)
            phraseInput = ""
            await loadStatus()
        } catch {
            setErrorText(String(describing: error))
        }
    }

    // MARK: - The let-go of a dead generation

    /// The let-go's confirm word — shared Rust's one constant
    /// (`generation_let_go::LET_GO_CONFIRM_WORD`), so every app gates on the
    /// same text. Never localized; only its prompt is.
    static let letGoConfirmWord = recoveryLetGoConfirmWord()

    /// `recovery-kit-unreadable-status`, and the render gate of the whole trio:
    /// the line, its confirm field and its button render only while the shared
    /// projection answers one — never re-derived here from the id list.
    public var letGoStatus: LocalizedText? {
        deadGenerations.unreadableStatus
    }

    /// Whether `recovery-kit-let-go-button` is armed — on the confirm word
    /// alone.
    public var letGoArmed: Bool {
        letGoConfirmInput == Self.letGoConfirmWord && !busy
    }

    /// Read the dead generations from the account runtime, at hydrate.
    public func loadDeadGenerations() async {
        guard let api else { return }
        deadGenerations = await api.recoveryDeadGenerations()
    }

    /// `recovery-kit-let-go-button` — the user's confirmed act
    /// (`account-data-taxonomy.md` clause (3)(j)).
    ///
    /// The confirm word is re-checked **here**, not only in the render, for the
    /// stolen gate's reason: a test agent driving the id reaches this handler,
    /// and the data cannot be brought back. The refreshed dead read REPLACES
    /// the list, so the trio leaves the screen once nothing is dead; what the
    /// act did is said on `error-message`, and a generation that became
    /// readable again since the render is named as kept, never silently
    /// skipped.
    public func letGo() async {
        guard !busy else { return }
        guard letGoConfirmInput == Self.letGoConfirmWord else {
            setErrorText(L.settings.recoveryKit.letGoConfirmPlaceholder)
            return
        }
        guard let api else { return }
        busy = true
        defer { busy = false }
        do {
            let outcome = try await api.recoveryLetGo(
                generationIds: deadGenerations.generationIds)
            letGoConfirmInput = ""
            deadGenerations = outcome.dead
            var message = L.settings.recoveryKit.letGoDone(retired: "\(outcome.retired)")
            if outcome.kept {
                message += " " + L.settings.recoveryKit.letGoKept
            }
            setErrorText(message)
        } catch {
            letGoConfirmInput = ""
            setErrorText(L.settings.recoveryKit.letGoFailed(message: String(describing: error)))
        }
    }

    /// `identity-stolen-button` — the irreversible succession ceremony.
    ///
    /// Both gates are re-checked **here**, not only in the render: a disabled
    /// control emits no gesture, but a test agent driving the id reaches this
    /// handler, and an irreversible ceremony must refuse out loud rather than
    /// run (`settings.md` § Recovery kit).
    ///
    /// On success the account belongs to a new identity and this session's
    /// bearers were revoked inside the nest's own transaction — so the caller
    /// switches to the successor (`onSucceeded` carries its actor id). ⚠
    /// **Except** when the device failed to save the successor seed
    /// (`persisted == false`): tearing the session down then takes the only copy
    /// of the key with it, so the secret goes on screen and the session stays
    /// up. `onSucceeded` is called only on the arm where the tear-down is safe.
    ///
    /// ⚠ **`SuccessionHandoff.record` happens on BOTH arms and before either** —
    /// the succession landed either way, so the owed kit, the sweep report and
    /// the predecessor id are owed either way. On the persist-failure arm the
    /// user's route back is importing the secret below, and the obligation is
    /// still outstanding when they do.
    public func succeedWithHeldKit(
        predecessorActorIdHex: String?,
        onSucceeded: @escaping (String) -> Void
    ) async {
        guard let api, !busy else { return }
        guard stolenConfirmInput == Self.stolenConfirmWord else {
            setErrorText(L.settings.recoveryKit.stolenConfirmPlaceholder)
            return
        }
        guard !phraseInput.isEmpty else {
            setErrorText(L.settings.recoveryKit.kitPhraseRequired)
            return
        }
        busy = true
        defer { busy = false }
        // From here the nest may commit at any moment, and this session's next
        // refresh is then refused as superseded — held back until the result
        // below is handled (``StolenCeremonyHold``).
        ceremonyHold.ceremonyStarted()
        var adopted = false
        do {
            let outcome = try await api.successionSucceedWithHeldKit(heldKitInput: phraseInput)
            adopted = applyStolenOutcome(
                outcome, predecessorActorIdHex: predecessorActorIdHex, onSucceeded: onSucceeded)
        } catch {
            // Only a failure BEFORE the ceremony could start reaches here (no
            // secret, unparseable secret bytes, no connection) — every ceremony
            // that ran is an outcome above, never an error.
            setErrorText(String(describing: error))
        }
        await ceremonyHold.ceremonyEnded(
            adopted: adopted, messageParked: stolenFailedMessagePending)
    }

    /// Fold the ceremony's typed outcome onto the section (`settings.md`
    /// § Recovery kit → *The ceremony's outcome is headlined by its arm*).
    /// Returns whether the device switched to the successor.
    ///
    /// Landed keeps its two halves (the switch, or the parked persist-failure
    /// message). Every other arm paints the shared sentence verbatim and wraps
    /// nothing; the one carrying the only copy of the successor seed is parked
    /// exactly as the persist-failure message is — decided by the record's
    /// flag, never by reading its kind or key. Mirrors linux's
    /// `dispatch_persist_outcome` / `dispatch_unlanded`
    /// (`apps/fauna-linux/src/settings/mod.rs`). Internal, not `private`, for
    /// the same testability reason as `stolenFailedMessagePending`.
    func applyStolenOutcome(
        _ outcome: FfiStolenOutcome,
        predecessorActorIdHex: String?,
        onSucceeded: (String) -> Void
    ) -> Bool {
        guard let landed = outcome.landed else {
            let sentence = outcome.message.map(renderLocalizedText) ?? outcome.kind
            if outcome.carriesTheOnlySeed {
                parkStolenMessage(sentence)
            } else {
                setErrorText(sentence)
            }
            return false
        }
        phraseInput = ""
        stolenConfirmInput = ""
        landedSuccession = landed
        // Hand the ceremony's four survivors over BEFORE the switch below:
        // `onSucceeded` tears this session (and this view model) down moments
        // from now, and everything recorded here is declared to outlive that.
        SuccessionHandoff.record(landed, predecessorActorIdHex: predecessorActorIdHex)
        if landed.persisted {
            onSucceeded(landed.newActorIdHex)
            return true
        }
        // The one landed arm where the secret must go on screen. Never phrased
        // as "nothing happened" — the account DID move.
        mintedSecretHex = landed.successorSecretHex
        // A seed, not a kit: it has no `fauna://recovery` form, and an
        // earlier mint's URI must not ride along on this secret.
        mintedKitUri = nil
        mintedEscrowStored = true
        parkStolenMessage(
            L.settings.recoveryKit.stolenPersistFailed(secret: landed.successorSecretHex))
        return false
    }

    /// The park write itself — deliberately unguarded (`setErrorText` would
    /// drop it, since this call is what MAKES the message pending) and paired
    /// with the flag that makes every other writer leave it alone from here on
    /// (`stolenFailedMessagePending`, `settings.md` § Recovery kit → *The
    /// persist-failure message survives the page*).
    private func parkStolenMessage(_ sentence: String) {
        errorText = sentence
        stolenFailedMessagePending = true
    }

    /// `recovery-kit-sweep-retry-button` — finish a sweep the ceremony left
    /// unfinished (`settings.md` § Recovery kit → *Finishing an unfinished
    /// group sweep*).
    ///
    /// **The answer is the product here, not a side effect.** The button
    /// renders on unfinished work and deliberately NOT on whether this device
    /// can retry, so a press that cannot sweep must still say something. Only
    /// the `swept` arm says nothing on `error-message`: its outcome renders
    /// through the two lines above instead, off the FRESH view this replaces
    /// `sweepView` with — `SuccessionHandoff.replaceSweep` carries the same
    /// replacement into `data.succession_sweep`, so a journey asserting either
    /// witness sees the finished pass.
    public func retrySweep() async {
        guard let api, !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let answer = try await api.successionRetryGroupSweep()
            if let fresh = answer.sweep, let json = answer.sweepStateJson {
                sweepView = fresh
                SuccessionHandoff.replaceSweep(fresh, stateJson: json)
            }
            setErrorText(answer.message.map(renderLocalizedText))
        } catch {
            setErrorText(String(describing: error))
        }
    }

    /// Discharge the group sweep a **relaunch adoption** owes — an unbidden
    /// press of `recovery-kit-sweep-retry-button`, run by the successor's
    /// first authenticated session BEFORE the kit's mint (the ceremony's own
    /// order: sweep, then kit) — `succession-propagation.md` § Propagation →
    /// *Own device fleet*, the relaunch-adoption clause.
    ///
    /// Nothing happens unless ``SuccessionHandoff/sweepOwedTo`` names this
    /// session, so an ordinary visit pays one comparison. The answer folds
    /// where a press would: its sentence (if any) on `error-message`, and a
    /// report parked in ``sweepView`` and `data.succession_sweep` — the fresh
    /// one when it swept, else an arm that still owes work so the retry button
    /// renders (shared Rust picks which, never this file).
    ///
    /// Same deferrals as ``dischargeOwedSuccessionKit(sessionActorIdHex:)``,
    /// for the same reasons: no `api` yet, a busy view model, or a seat that is
    /// not yet the successor's all leave the obligation standing for the next
    /// hydrate rather than spending it on a pass that cannot run.
    public func dischargeOwedSweep(sessionActorIdHex: String?) async {
        guard let sessionActorIdHex, SuccessionHandoff.sweepOwedTo == sessionActorIdHex else {
            return
        }
        guard let api, !busy, api.boundActorIdHex == sessionActorIdHex else {
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-sweep] discharge deferred "
                                + "(api=\(api == nil ? "nil" : "live") busy=\(busy)) "
                                + "— obligation kept for the next pass")
            return
        }
        guard SuccessionHandoff.claimOwedSweep(asSuccessor: sessionActorIdHex) else { return }
        busy = true
        defer { busy = false }
        do {
            let owed = try await api.successionDischargeOwedSweep()
            sweepView = owed.parked
            SuccessionHandoff.replaceSweep(owed.parked, stateJson: owed.parkedStateJson)
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-sweep] owed sweep answered \(owed.answer.kind)")
            setErrorText(owed.answer.message.map(renderLocalizedText))
        } catch {
            // Only a session with no secret throws, before anything ran — put
            // the obligation back rather than spend it.
            SuccessionHandoff.rearmOwedSweep(successor: sessionActorIdHex)
            setErrorText(String(describing: error))
        }
    }

    /// Mint and show the successor's recovery kit — the **closing act** of an
    /// identity succession, run on the successor's own first authenticated
    /// session (`identity-succession.md` § The RecoveryKey → *At succession*:
    /// "the successor identity mints a fresh RecoveryKey — a new kit is part of
    /// the ceremony").
    ///
    /// Nothing happens unless `SuccessionHandoff.kitOwed` is set, so an ordinary
    /// visit to this section pays one boolean check. See that type for why the
    /// obligation has to cross the account switch at all, and why a *silent*
    /// background mint is not an option: the old kit retired with the old
    /// identity and the nest deleted its escrow row in the same transaction, so
    /// between the succession and this mint the account has no route back but
    /// the 30-day seed-alone window — and a mint nobody was *shown* leaves a kit
    /// nobody holds, which is strictly worse than never-created.
    ///
    /// **With no `api` yet the flag stays set** rather than being consumed: a
    /// successor whose first launch could not reach its nest is offered the kit
    /// on the next one instead of losing the step silently.
    ///
    /// ⚠ `sessionActorIdHex` is not decoration — it is what keeps the OUTGOING
    /// session from taking the obligation. This section is mounted while the
    /// ceremony runs and stays mounted through the teardown, so it re-hydrates
    /// as the departing identity and would otherwise mint against a nest that
    /// has just revoked its bearers (`SuccessionHandoff.successorActorIdHex`).
    public func dischargeOwedSuccessionKit(sessionActorIdHex: String?) async {
        // Both guards come BEFORE the claim, and `!busy` is the load-bearing one:
        // `createOrReplaceKit` returns early while another ceremony is in flight,
        // so claiming first would consume the obligation without minting anything
        // — the one way this step can be lost silently rather than retried.
        guard api != nil, !busy else {
            // Not noise: with the flag left SET this is the *retryable* refusal,
            // and it is indistinguishable at the UI from the terminal ones below.
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] discharge deferred "
                                + "(api=\(api == nil ? "nil" : "live") busy=\(busy)) "
                                + "— flag kept for the next pass")
            return
        }
        guard let sessionActorIdHex else { return }
        // The seat must belong to the successor, or the one-shot claim below is
        // spent on a socket the ceremony revoked. Deliberately the same
        // *retryable* shape as the `api == nil` deferral above — the flag stays
        // SET — because that is what this is: the client for this identity has
        // not reached the view yet. Measured on iOS 2026-08-26: the section hydrated as the successor while the environment
        // still held the PREDECESSOR's `FaunaClient`, claimed, and burned all
        // five mint attempts on the revoked seat. `RecoveryKitSection.hydrateKey`
        // now re-fires when the seat changes, so this deferral is answered rather
        // than final — the two halves are one fix and neither works alone.
        let seat = api?.boundActorIdHex
        guard seat == sessionActorIdHex else {
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] discharge deferred: seat is "
                                + "\(seat ?? "<no-secret>") but the owed successor is "
                                + "\(sessionActorIdHex) — flag kept for the next pass")
            return
        }
        guard SuccessionHandoff.claimOwedKit(asSuccessor: sessionActorIdHex) else {
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] discharge refused: "
                                + "owed=\(SuccessionHandoff.kitOwed) "
                                + "owedTo=\(SuccessionHandoff.successorActorIdHex ?? "-") "
                                + "asking=\(sessionActorIdHex)")
            return
        }
        // A kit an off-screen section already minted is the one to SHOW — it is
        // the registered chain head, so a fresh no-prior mint would be refused
        // (`SuccessionHandoff.rearmUnshownKit` carries the argument).
        if let stranded = SuccessionHandoff.takeStrandedKit(asSuccessor: sessionActorIdHex) {
            showKit(stranded)
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] discharge claimed by \(sessionActorIdHex) "
                                + "→ showing the kit an off-screen section minted "
                                + "(\(stranded.secretHex.count) hex)")
            return
        }
        logMessage(level: .info, target: "fauna.app",
                   message: "[succession-kit] discharge claimed by \(sessionActorIdHex) "
                            + "→ minting on seat \(api?.boundActorIdHex ?? "<no-secret>")")

        // ── Why this retries, and why a failure puts the obligation BACK ──────
        //
        // The claim above is one-shot and lands BEFORE the mint (it has to: see
        // the guard comment). So every way the mint can fail is a way to spend
        // the obligation having shown nothing — the silent loss this function's
        // own doc warns about, and `identity-succession.md` § The RecoveryKey's
        // "a kit nobody holds, strictly worse than never-created".
        //
        // A first-attempt failure here is the NORMAL case, not an exceptional
        // one: the ceremony revokes every session of the account inside the
        // nest's own transaction, so the successor's first mint races its own
        // reconnect by construction. Measured on iOS, 2026-08-26 : `Connection error: The connection to the nest was lost`,
        // 5.0 s after the claim, obligation spent, successor kitless for good.
        //
        // So: retry on a bounded backoff, and if every attempt fails, re-arm
        // rather than swallow it — a later pass (or a later launch) then offers
        // the kit, which is exactly what the no-reachable-nest arm above already
        // promises. Bounded, because an unbounded loop in a view task would
        // outlive the screen it paints.
        //
        // ⚠ Except when the chain already HAS a head: this mint takes the
        // no-prior arm, which the shared ceremony refuses over a registered
        // kit (`PriorKitRequired`), so no retry and no re-arm can ever land —
        // re-arming there is what looped ~280 refused mints on 2026-09-24. A
        // head this device does not hold means a mint landed whose reply was
        // lost; the status line says "registered" and the lost-kit path is the
        // route back. Read off the shared `allowsCreate` predicate, never
        // matched on `status.kind` (this type's doc).
        for attempt in 1...Self.successionMintAttempts {
            await createOrReplaceKit(usingHeldPhrase: false)
            if mintedSecretHex != nil { return }
            await loadStatus()
            if Self.mintCanNeverLand(status) {
                logMessage(level: .error, target: "fauna.app",
                           message: "[succession-kit] a kit is already registered for "
                                    + "\(sessionActorIdHex) and no screen holds it — not "
                                    + "re-arming a mint the chain head refuses")
                return
            }
            logMessage(level: .warn, target: "fauna.app",
                       message: "[succession-kit] mint attempt \(attempt) of "
                                + "\(Self.successionMintAttempts) showed nothing")
            if attempt < Self.successionMintAttempts {
                try? await Task.sleep(nanoseconds: Self.successionMintBackoffNs)
            }
        }
        logMessage(level: .error, target: "fauna.app",
                   message: "[succession-kit] every mint attempt failed — re-arming the "
                            + "obligation for \(sessionActorIdHex) rather than spending it")
        SuccessionHandoff.rearmUnshownKit(successor: sessionActorIdHex)
    }

    /// How many times the succession discharge will try to mint before giving the
    /// obligation back. Sized against the reconnect it races, not against a
    /// wall-clock budget: five attempts spans the revoke-and-reconnect window
    /// with room to spare, and the caller re-arms rather than gives up for real.
    private static let successionMintAttempts = 5
    private static let successionMintBackoffNs: UInt64 = 3_000_000_000

    /// Whether the discharge's no-prior mint is refused by construction on this
    /// chain read: the shared `allowsCreate` says a create needs no prior kit
    /// only when none is registered. An unread chain (`nil`) is NOT that answer
    /// — the read raced the reconnect, so the retry stays worth making.
    static func mintCanNeverLand(_ status: FfiRecoveryKitStatus?) -> Bool {
        status?.allowsCreate == false
    }

    /// The kit this view model holds on screen, packaged for
    /// ``SuccessionHandoff/rearmUnshownKit(successor:stranded:)`` — `nil` when
    /// nothing was minted.
    public var heldKit: SuccessionHandoff.StrandedKit? {
        mintedSecretHex.map {
            SuccessionHandoff.StrandedKit(secretHex: $0, kitUri: mintedKitUri,
                                          escrowStored: mintedEscrowStored,
                                          landsAt: mintedLandsAt)
        }
    }

    /// Put a kit on screen — the display half of ``runMint``, shared with the
    /// stranded-kit hand-over so both paint the same four fields.
    private func showKit(_ kit: SuccessionHandoff.StrandedKit) {
        mintedSecretHex = kit.secretHex
        mintedKitUri = kit.kitUri
        mintedEscrowStored = kit.escrowStored
        mintedLandsAt = kit.landsAt
        setErrorText(nil)
    }



    /// The type-to-confirm token. **Never localized** — only its prompt is
    /// (`recovery_kit.stolen_confirm_placeholder`), or the gate would differ per
    /// locale. Every app spells the same literal at its own call site, exactly as
    /// account deletion spells `"DELETE"`.
    static let stolenConfirmWord = "SUCCEED"

    /// Whether the succession trigger and its type-to-confirm gate render. Per
    /// `settings.md` § Recovery kit the stolen action belongs to "stolen
    /// (any)" — every state — and `allows_stolen` being unconditionally true
    /// says the same thing from the shared side.
    ///
    /// ⚠ Deliberately also true while the status is UNREAD or the chain read
    /// failed — widened to match windows'
    /// `StolenVisible`, on the merits, not incidentally: this ceremony exists
    /// for an owner a thief has locked out, the authenticated chain read is
    /// exactly what fails for such an owner, and the ceremony's authorization
    /// is the KIT, never the status — `succession_succeed_with_held_kit`
    /// deliberately does no status re-read of its own (`recovery.rs`: "the
    /// kit is the whole authorization, and a chain read here would only add a
    /// round trip a locked-out owner can fail on"). Hiding the trigger behind
    /// a read that may never succeed would withhold the affordance precisely
    /// from the person it is for. Only a status that POSITIVELY says stolen
    /// is disallowed hides it.
    public var stolenVisible: Bool {
        status?.allowsStolen != false
    }

    /// Whether `identity-stolen-button` is armed. The confirm gate is a **second**
    /// condition, not a replacement for the status one — `allowsStolen` is
    /// unconditionally true, so this gate is the only thing between a stray click
    /// and a re-pointed account.
    public var stolenArmed: Bool {
        stolenConfirmInput == Self.stolenConfirmWord && !busy
    }

    /// The phrase field renders while a ceremony that consumes one is
    /// reachable — replace, stolen, veto or the no-escrow re-seal. Widened to
    /// also render with the status UNREAD, same reasoning as
    /// [`stolenVisible`] above: an unread status must not take the field
    /// away from the ceremony that needs it most.
    public var phraseFieldVisible: Bool {
        guard let s = status else { return true }
        return s.allowsReplace || s.allowsStolen || s.allowsEscrowReseal
            || s.pendingLandsAt != nil
    }

    /// Run a kit-minting ceremony and surface its secret once.
    private func runMint(_ ceremony: @escaping () async throws -> FfiMintedKit) async {
        busy = true
        defer { busy = false }
        do {
            let kit = try await ceremony()
            // Display BEFORE anything else can fail: at this instant the secret
            // exists nowhere else in the world, and a ceremony whose kit is never
            // shown leaves a kit nobody holds.
            mintedSecretHex = kit.secretHex
            // The account-naming form the copy button and QR carry, built in this
            // same synchronous step (a LOCAL read — see
            // `APIClient.recoveryKitDisplayUri`) so no frame paints a copy button
            // the URI has not reached. A refusal costs the account params, never
            // the display: the view falls back to the bare kit.
            do {
                mintedKitUri = try api?.recoveryKitDisplayUri(kitSecretHex: kit.secretHex)
            } catch {
                mintedKitUri = nil
                logMessage(level: .warn, target: "fauna.app",
                           message: "[recovery-kit] display URI unavailable "
                                    + "(\(type(of: error))) — copy and QR carry the bare kit")
            }
            mintedEscrowStored = kit.escrowStored
            mintedLandsAt = kit.landsAt
            phraseInput = ""
            setErrorText(nil)
            logMessage(level: .info, target: "fauna.app",
                       message: "[succession-kit] mint landed on screen "
                                + "(\(kit.secretHex.count) hex, escrow=\(kit.escrowStored))")
            await loadStatus()
        } catch {
            // The one arm that spends the obligation without showing anything —
            // `claimOwedKit` already cleared the flag by the time we get here.
            // The seat's actor is part of the failure, not context: a mint
            // refused because this view model holds the PREVIOUS session's
            // client (whose actor the ceremony just revoked) and one refused
            // on a live successor seat whose socket dropped are the same
            // `fauna.protocol.disconnected` string, and they need opposite
            // fixes. `APIClient.boundActorIdHex` carries the full argument.
            logMessage(level: .error, target: "fauna.app",
                       message: "[succession-kit] mint FAILED on seat "
                                + "\(api?.boundActorIdHex ?? "<no-secret>"): \(error)")
            setErrorText(String(describing: error))
        }
    }
}
