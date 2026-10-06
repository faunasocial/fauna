import Foundation

/// Post-onboarding serving-enablement + trust-prompt + recovery glue — the
/// authed launch-glue half of `onboarding.md` § 3b *Mechanism* (+ § 3b-ter,
/// `box-recovery.md` § Mechanism).
///
/// The onboarding wizard runs **pre-identity**, but the four serving-enablement
/// intents (email / CalDAV / CardDAV / WebDAV) are **Admin-class**, so the
/// wizard only records machine-derived intent (`onboarding.md` § 3b — the
/// former per-toggle checkboxes are retired with the encryption-mode page).
/// `OnboardingVM.performLoggedInHandoff` latches the four onto `SessionState`
/// (`pendingFirstSetupMail` / `pendingCaldavEnable` / `pendingCarddavEnable` /
/// `pendingWebdavEnable`). Once the launched client is authenticated —
/// `completeAuthenticatedLaunch` on macOS, the silent-sign-in block on iOS —
/// `applyPostClaimServingEnablement` below reads the latch **once** and hands
/// all four to the ONE shared-Rust step
/// `fauna_client_mail_settings::serving_enablement`, mirroring tui's
/// `wizard/mod.rs` LoggedIn-transition call and linux's `views/onboarding`
/// twin (priority #1/#2) — apple fires it here rather than at the transition
/// itself because the wizard hands off pre-identity (see `runPostAuthGlue`'s
/// doc for why).
///
/// Shared FaunaKit (macOS + iOS). Best-effort: any failure is recoverable from
/// the mail-settings/admin pages, so it logs and no-ops rather than surfacing
/// an error.
public enum MailEnableGlue {
    /// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*) —
    /// replaces the four per-step Admin-class writes (mail provision, the
    /// three DAV toggles, their one-MSEK-mint-path companion mints) with the
    /// ONE shared step `apply_post_claim_serving_enablement`, which resolves
    /// `am_i_admin` and does all the discrimination itself. Fires only on a
    /// fresh onboarding (the `pendingFirstSetupMail` latch is `nil` on a
    /// returning-user relaunch → no-op, which is the once-only / opt-out
    /// guarantee: `disableMail` clears the MSEK but the latch stays unset, so
    /// an opt-out is not re-minted on the next launch). Consumes all four
    /// latches together as one boundary, matching that guarantee.
    @MainActor
    public static func applyPostClaimServingEnablement(api: APIClient, session: SessionState) async {
        guard let enableEmail = session.pendingFirstSetupMail else { return }
        session.pendingFirstSetupMail = nil  // consume once
        let enableCaldav = session.pendingCaldavEnable
        let enableCarddav = session.pendingCarddavEnable
        let enableWebdav = session.pendingWebdavEnable
        session.pendingCaldavEnable = false
        session.pendingCarddavEnable = false
        session.pendingWebdavEnable = false
        do {
            try await api.applyPostClaimServingEnablement(
                email: enableEmail, caldav: enableCaldav, carddav: enableCarddav, webdav: enableWebdav)
        } catch {
            logMessage(
                level: .warn, target: "fauna.mailglue",
                message: "post-claim serving enablement failed (recoverable via mail-settings/admin pages): \(error)")
        }
    }

    /// Post-onboarding one-tap trust offer glue (`onboarding.md` § 3b-ter) —
    /// the apple leg of the trust_prompt trickle-down.
    /// `OnboardingVM.performLoggedInHandoff` latches the wizard's answer onto
    /// `session.pendingTrustPromptGranted`; this reads it once and, when the
    /// admin granted, mints the default capability-grant set via
    /// `APIClient.mintDefaultTrustSet` (`LinkedNestsAction::MintDefaultSet`).
    /// No-op on skip, and on a returning-user relaunch (the latch is unset —
    /// the wizard never reached this step).
    ///
    /// **Best-effort and log-only, matching `applyPostClaimServingEnablement`**: unlike
    /// the deployment-seed custody below, a mint failure has an always-available
    /// recovery path (the same trust facet, any time, from the Nests page), so it
    /// must not paint an error over an otherwise-completed onboarding.
    @MainActor
    public static func applyPendingTrustPromptGrant(api: APIClient, session: SessionState) async {
        guard session.pendingTrustPromptGranted else { return }
        session.pendingTrustPromptGranted = false  // consume once
        do {
            try await api.mintDefaultTrustSet()
        } catch {
            logMessage(
                level: .warn, target: "fauna.trustglue",
                message: "trust-prompt default-set mint failed (recoverable via Nests page): \(error)")
        }
    }

    /// Register the recovery kit confirmed on the onboarding `recovery_kit`
    /// screen (`identity-succession.md` § The RecoveryKey → *Creation UX*) now
    /// that an authenticated session exists — the same deferral as the trust
    /// grant above, through the shared body tui and linux call at their own
    /// handoff. Consume-once; a no-op on skip and on every relaunch. Log-only:
    /// the ceremony reports nothing to paint, and Settings' status line says
    /// whether the kit registered.
    @MainActor
    public static func registerPendingRecoveryKit(api: APIClient, session: SessionState) async {
        guard let kitHex = session.pendingRecoveryKitHex else { return }
        session.pendingRecoveryKitHex = nil  // consume once
        do {
            try await api.recoveryRegisterDeferredKit(kitHex: kitHex)
        } catch {
            logMessage(
                level: .warn, target: "fauna.recovery",
                message: "deferred recovery-kit registration failed (Settings shows the kit's status): \(error)")
        }
    }

    /// Admin host-address reporting — `domains-and-tls-bootstrap.md`
    /// § Host-address acquisition. Fires at **every** authenticated launch (not
    /// latch-gated like the enable glue above): the client reports the nest's
    /// public IP so ACME HTTP-01 gates on the strong resolve-check, and repeat
    /// reports converge an IP change idempotently (last-writer-wins nest-side).
    /// The native twin of windows' `HostAddressReporter` / linux's
    /// `AdminStatusLoaded → report_host_address()` / web's `+layout.svelte`
    /// `reportHostAddress`. Shared macOS + iOS (FaunaKit).
    ///
    /// Admin-gated (`am_i_admin`): a non-admin's `set_host_address` is refused
    /// nest-side, so gating avoids a pointless failing RPC on every non-admin
    /// connect. Fire-and-forget + best-effort — a fault is logged and must never
    /// disrupt login; the next connect retries. All classification (public vs.
    /// private/LAN/`.local`) lives in the shared FFI fn — no client logic here.
    @MainActor
    public static func reportHostAddress(api: APIClient) async {
        do {
            guard try await api.amIAdmin() else { return }
            let outcome = try await api.reportHostAddress()
            switch outcome {
            case .reported(let nestIpv4):
                logMessage(
                    level: .info, target: "fauna.hostaddress",
                    message: "reported nest public IPv4 \(nestIpv4) (strong ACME resolve-gate now in force)")
            case .skippedNoPublicIp:
                // Expected on a home-LAN box (no reflector) — the spec'd safe
                // floor, not an alarm.
                logMessage(
                    level: .debug, target: "fauna.hostaddress",
                    message: "no public IP determinable — kept self-signed floor (LAN box)")
            case .failed(let error):
                logMessage(
                    level: .warn, target: "fauna.hostaddress",
                    message: "set_host_address failed (retries next connect): \(error)")
            }
        } catch {
            logMessage(
                level: .warn, target: "fauna.hostaddress",
                message: "host-address report skipped: \(error)")
        }
    }

    /// The deployment-seed custody leg's post-auth entry
    /// (`box-recovery.md` § The plane-era recovery floor, (c) The writes) — fired at
    /// the universal post-auth hook on **every** connect. The leg itself (capture at
    /// store-ready, the co-admin hand-off self-heal) lives in shared Rust; this is
    /// only the apple projection of its outcome onto `RecoveryCustodyBanner`, the
    /// twin of android's `MailEnableGlueVM.runDeploymentSeedCustodyLeg` and
    /// windows' `RunDeploymentSeedCustodyLegAsync`.
    ///
    /// **A non-confirmed custody is USER-VISIBLE, never a silent log**: a swallowed
    /// failure would leave the admin believing recovery is protected and finding out
    /// only at total box loss. A thrown call (the bound nest id could not be
    /// resolved, or a bad secret) means custody could not even be attempted, so it
    /// raises the "failed" warning too. `alreadyCustodied` / `notAdmin` and the
    /// expected multi-nest `refusedDiffering` (BR-1) stay quiet.
    @MainActor
    public static func selfHealDeploymentSeedCustody(api: APIClient, session: SessionState) async {
        do {
            let outcome = try await api.selfHealDeploymentSeedCustody()
            logMessage(level: .debug, target: "fauna.recovery",
                       message: "[recovery] custody leg: \(outcome)")
            if case .captured(.refusedMismatch) = outcome {
                logMessage(level: .error, target: "fauna.recoveryglue",
                           message: "deployment-seed custody refused: seed does not derive to this nest (BR-2); recovery not protected")
            }
            session.recoveryCustodyWarning = recoveryCustodyWarning(for: outcome)
        } catch {
            logMessage(level: .warn, target: "fauna.recoveryglue",
                       message: "deployment-seed custody leg failed (custody not confirmed): \(error)")
            session.recoveryCustodyWarning = L.launch.recoveryCustodyFailed
        }
    }

    /// Pure custody-leg outcome → user-visible-warning mapping. `nil` for a
    /// silent success — `alreadyCustodied`, `notAdmin`, and a `captured` write that
    /// is `wrote` / `alreadyHeldSame` / `refusedDiffering` (the **expected**
    /// multi-nest no-op: this admin identity already custodies another nest,
    /// BR-1). `captured(.refusedMismatch)` → the loud "recovery not protected"
    /// warning (BR-2); `handoffUnavailable` / `nestHoldsNoSeed` → the "failed"
    /// warning (custody not confirmed). Factored out so it is unit-testable
    /// without a live nest.
    static func recoveryCustodyWarning(for outcome: FfiDeploymentSeedSelfHeal) -> String? {
        switch outcome {
        case .alreadyCustodied, .notAdmin:
            return nil
        case .handoffUnavailable, .nestHoldsNoSeed:
            return L.launch.recoveryCustodyFailed
        case .captured(let capture):
            switch capture {
            case .wrote, .alreadyHeldSame, .refusedDiffering:
                return nil
            case .refusedMismatch:
                return L.launch.recoveryCustodyMismatch
            }
        }
    }

    /// The full post-auth glue sequence, in the fixed order both shells' launch
    /// paths ran it as six separate awaited calls — byte-identical on both
    /// (`completeAuthenticatedLaunch` on macOS, the silent-sign-in block on
    /// iOS), just wrapped differently (macOS fires it inside a detached `Task`;
    /// iOS awaits it inline in its own already-async glue) — until this
    /// harvest pass found the inner sequence itself never drifted. The four per-step Admin-class writes
    /// that used to make this nine calls total have collapsed into the ONE
    /// `applyPostClaimServingEnablement` call below — the shared-Rust step tui and linux already call directly from their
    /// own wizard `LoggedIn` transition. Apple fires it here instead, one hop
    /// later, because the wizard hands off **pre-identity**: `api`'s
    /// authenticated WS-RPC connection and actor secret don't exist yet at the
    /// transition itself, only once the launched shell mounts and calls this.
    ///
    /// `applyPostClaimServingEnablement` (latch-gated, no-op on a
    /// returning-user relaunch — see `onboarding.md` § Enable-email at claim),
    /// then the trust-prompt grant, then two universal post-auth hooks that
    /// fire on EVERY authenticated launch regardless of the latch: host-address
    /// reporting and the deployment-seed custody leg. `am_i_admin` discrimination
    /// and the CalDAV-/CardDAV-only mailbox-mint gating now live inside the
    /// shared Rust step; every call here is independently best-effort (logs and
    /// no-ops on failure — see each method's own doc).
    @MainActor
    public static func runPostAuthGlue(api: APIClient, session: SessionState) async {
        await applyPostClaimServingEnablement(api: api, session: session)
        await applyPendingTrustPromptGrant(api: api, session: session)
        await registerPendingRecoveryKit(api: api, session: session)
        await reportHostAddress(api: api)
        await selfHealDeploymentSeedCustody(api: api, session: session)
    }
}
