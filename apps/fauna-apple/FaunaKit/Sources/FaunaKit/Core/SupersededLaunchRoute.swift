import Foundation

/// The launch arm for a **succeeded** identity, shared by macOS and iOS
/// (`identity-succession.md` § Propagation → *Own device fleet*: the client
/// surfaces "this identity was succeeded — import the new identity").
///
/// The machine projects the refusal to `Offline { transient: false }` and
/// carries the claimed successor on the `LaunchSnapshot.supersededSuccessor`
/// side channel, so an app that does not read the channel stops retrying but
/// tells the user to update their nest, which describes the wrong problem and
/// leaves no way out. Each app's `dispatchLaunch` checks the channel ahead of
/// its generic `offline` arms, in the same position as tui's `launch.rs`,
/// linux's `main.rs` and web's onboarding page, and then calls [route].
///
/// **No new ui.yaml elements.** The affordance IS the existing import flow
/// (page `identity_import`), with the reason on that page's existing
/// `error-message`. The reason goes through the machine
/// (`beginImportIdentityWithReason`: step and reason in one mutation) rather
/// than a view-local label, because the onboarding views mirror
/// `errorMessage()` on every observer tick and would erase a label set by
/// hand on the tick the transition itself fires.
///
/// **The first message deliberately does not name the successor.** The
/// refusal's successor is *claimed* until the registration chain proves it;
/// presenting it as fact would make this client trust the nest as an
/// authorizer. The claim goes to the log. [route] then walks the chain
/// anonymously (`successionResolveVerifiedSuccessor`) and upgrades the message
/// once the walk verifies a successor. Every failure leaves the claim-free
/// message standing.
///
/// **Save when this device already holds the verified successor's key** — the
/// state a lost succession reply leaves behind (`identity-succession.md`
/// § Implementation status today, *a lost submit reply no longer destroys the
/// account*): the ceremony persisted the successor without activating it, and
/// its undecidable arm promised that reopening the app signs in as it. Then
/// there is nothing to import. The shared
/// `FfiAccountRegistry.adoptHeldSuccessor` decides (and records the succession
/// link); this records the kit and the group sweep the adoption owes
/// (``SuccessionHandoff/recordRelaunchAdoption(predecessorActorIdHex:successorActorIdHex:)``)
/// and hands the switch to the app. tui's `App::adopt_held_successor` is the
/// twin. Both halves are proofs, never claims: the successor is the chain's
/// answer, and the key is one this device minted and kept.
@MainActor
public enum SupersededLaunchRoute {
    /// Seed `machine` onto the import page with the claim-free reason, then
    /// best-effort upgrade it to name the verified successor — or, when this
    /// device holds that successor's key, adopt it through `adopt`.
    ///
    /// The refused identity's secret and nest URL come from the ACTIVE
    /// account's session material in the registry, the read
    /// `completeAuthenticatedLaunch` uses. Without the material, the
    /// claim-free message is simply final.
    ///
    /// `adopt` is the app's own account switch to the successor it is given
    /// (the same `switchAccount(to:confirmed: false)` the ceremony's
    /// `onSucceeded` ends in); it is called only after the adoption's
    /// obligations are recorded.
    public static func route(
        machine: OnboardingMachine,
        claimedSuccessor: String,
        keychain: KeychainStore,
        adopt: @escaping @MainActor (String) async -> Void
    ) {
        let registry = FaunaAccounts.registry(keychain: keychain)
        let predecessor = registry.active()
        let material = predecessor.flatMap { registry.sessionMaterial(actorId: $0) }
        let secretHex = material?.secretHex
        let nestUrl = material?.nestUrl
        logMessage(
            level: .error, target: "fauna.app",
            message: "[launch-machine] this identity was succeeded (claimed successor "
                   + "\(claimedSuccessor)) — routing to the identity-import flow")
        if let secretHex { machine.seedIdentity(secret: secretHex) }
        machine.beginImportIdentityWithReason(reason: L.onboarding.launch.identitySuperseded)

        guard let secretHex, let nestUrl else {
            logMessage(level: .warn, target: "fauna.app",
                       message: "[launch] no session material for the refused identity — cannot verify the succession")
            return
        }
        Task { @MainActor in
            let verified: String?
            do {
                verified = try await successionResolveVerifiedSuccessor(
                    nestUrl: nestUrl, oldSecret: hex_to_data(secretHex))
            } catch {
                // Only a malformed secret errors; the claim-free message stands.
                logMessage(level: .warn, target: "fauna.app",
                           message: "[launch] could not verify the succession: \(error)")
                return
            }
            guard let successor = verified else { return }
            // The walk can land after the user navigated away. Asserting a
            // supersession over whatever they are doing now would show a
            // banner from a flow they have already handled.
            guard machine.step() == .identityImport else { return }
            if let predecessor,
               registry.adoptHeldSuccessor(predecessor: predecessor, verifiedSuccessor: successor)
            {
                logMessage(level: .info, target: "fauna.app",
                           message: "[launch] this device holds the verified successor "
                                  + "\(successor); adopting it")
                SuccessionHandoff.recordRelaunchAdoption(
                    predecessorActorIdHex: predecessor, successorActorIdHex: successor)
                await adopt(successor)
                return
            }
            machine.beginImportIdentityWithReason(
                reason: L.onboarding.launch.identitySupersededVerified(successor: successor))
        }
    }
}
