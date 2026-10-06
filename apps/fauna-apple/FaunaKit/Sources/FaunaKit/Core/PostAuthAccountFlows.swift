import Foundation

/// One-shot best-effort post-auth identity re-check (`security.md` § Post-auth
/// surfacing) — the native analog of linux's `launch_authenticated()`
/// `silent_sign_in()` call and tui's `spawn_domain_refresh`: re-run the same
/// silent challenge the boot-time `LaunchMachine` ran, once, against the
/// now-authenticated session, so a nest identity that changes AFTER that
/// challenge is still caught before the session goes stale.
/// `FfiError.NestIdentityChanged` and `FfiError.IdentitySuperseded` are the
/// verdicts that escalate — every other outcome (success, transient
/// reachability, a rejected secret) is swallowed: a live session must never
/// tear itself down over a flaky refresh, only a proven identity change. A
/// supersession goes through ``StolenCeremonyHold``, since this device's own
/// stolen-identity ceremony may be what caused it.
///
/// `escalate` is the app's ``escalateToLaunchSurface(switchInFlight:tearDown:runLaunch:)``
/// wrapper. Was a byte-identical per-target twin (both bodies match exactly)
/// until this harvest pass found it .
@MainActor
public func performPostAuthSilentSignIn(
    session: SessionState,
    ceremonyHold: StolenCeremonyHold? = nil,
    escalate: @escaping @MainActor () async -> Void
) async {
    guard let nodeUrlString = session.nodeUrl,
          let url = URL(string: nodeUrlString),
          let secret = session.secretHex else { return }
    let api = APIClient(nodeUrl: url)
    do {
        // `nil` is the nest's `not_registered` — suspended or removed while
        // signed in, the third session-ending verdict (`onboarding.md`
        // § App-launch routing, the previously-signed-in row, mid-session).
        if try await api.silentSignIn(secret: secret) == nil {
            await escalateSessionEnding(.signInRefused,
                                        ceremonyHold: ceremonyHold, escalate: escalate)
        }
    } catch let ffiError as FfiError {
        switch ffiError {
        case .NestIdentityChanged:
            await escalateSessionEnding(.nestIdentityChanged,
                                        ceremonyHold: ceremonyHold, escalate: escalate)
        case .IdentitySuperseded:
            await escalateSessionEnding(.superseded,
                                        ceremonyHold: ceremonyHold, escalate: escalate)
        default:
            return
        }
    } catch {
        // Reachability / classification noise — best-effort, never escalate.
    }
}

/// Route a session-ending verdict (`security.md` § Post-auth surfacing) to the
/// launch surface — the one door both the post-auth refresh above and the
/// connection supervisor's stop (`FaunaClient`'s connection-state observer,
/// `.faunaSessionEnding`) go through, so the two cannot disagree. Re-entering
/// the real launch flow is what lands the right screen: the re-run challenge
/// earns the same refusal, and `runLaunch()`'s routing takes it from there — the
/// identity-import route for a supersession (`identity-succession.md`
/// § Propagation → *Own device fleet*), the same as a cold start.
///
/// A supersession is routed through `ceremonyHold` (default ``StolenCeremonyHold/shared``;
/// a test passes its own), which holds it back while this device's own
/// stolen-identity ceremony owns the Account page.
@MainActor
public func escalateSessionEnding(
    _ verdict: FfiSessionEndingVerdict,
    ceremonyHold: StolenCeremonyHold? = nil,
    escalate: @escaping @MainActor () async -> Void
) async {
    logMessage(level: .warn, target: "fauna.app",
               message: "[post-auth] session-ending verdict \(verdict) — re-entering launch")
    switch verdict {
    case .superseded:
        await (ceremonyHold ?? .shared).escalate(escalate)
    case .nestIdentityChanged, .signInRefused:
        await escalate()
    }
}

/// Tear the session down and re-enter launch — the body every
/// ``escalateSessionEnding(_:ceremonyHold:escalate:)`` arm performs.
///
/// `switchInFlight` is `inout` — each caller's own `@MainActor private static
/// var`, guarding re-entrancy per app type, identically typed but not a shared
/// symbol; `tearDown`/`runLaunch` are closures because each app's
/// teardown/relaunch bodies stay genuinely per-platform. Credentials are KEPT
/// (a teardown, not a reset): the account moved, not the person.
@MainActor
public func escalateToLaunchSurface(
    switchInFlight: inout Bool,
    tearDown: () async -> Void,
    runLaunch: () -> Void
) async {
    guard !switchInFlight else { return }
    switchInFlight = true
    await tearDown()
    if !FaunaE2E.isActive {
        await FileProviderCoordinator.signOut()
    }
    runLaunch()
    switchInFlight = false
}

/// The append wizard's `LoggedIn` exit — reuse the whole switch path, the new
/// account is just another account now (a just-added account is never
/// flagged, so this is always the plain unconfirmed activation).
///
/// **The appended identity lives in the wizard MACHINE, never the store**
/// (`onboarding.md` § Long-term store contract → *Append mode is exempt*): an
/// append's moment 1 (`confirmIdentity(append: true)`) writes nothing, and its
/// `LoggedIn` terminal skips `persistLoggedIn`, so this is the ONE write that
/// registers it — secret from `effectiveSecret()`, home nest from the wizard's
/// `LoggedIn` outcome, and the sync device id resolved for the APPENDED actor
/// (`deviceIdForActor`), never read from a single slot that names the outgoing
/// account's id (the bug android shipped 2026-09-22). An abandoned append
/// therefore leaves the store untouched (`onboarding.md` § Multi-account).
///
/// `dismissWizard`/`switchAccount` are closures: `isAddingAccount` lives on
/// two distinct `@Observable` state classes (`AppState`/`MacAppState`), and
/// `switchAccount` itself is genuinely platform-divergent (macOS's
/// multi-instance `pushManager` routing has no iOS analog) — passing it
/// through avoids forcing either into a shared protocol for a single call.
/// Was a byte-identical per-target twin (differing only in one comment
/// line) until this harvest pass found it .
@MainActor
public func completeAppendedAccount(
    wizard: OnboardingMachine,
    dismissWizard: () -> Void,
    switchAccount: (String, Bool) async -> Void
) async {
    // Read the wizard before dismissing it — the machine is the only holder.
    let secretHex = wizard.effectiveSecret()
    let nestUrl: String = {
        if case .loggedIn(let url, _) = wizard.wizardOutcome() { return url }
        return wizard.nestUrl()
    }()
    let handle = wizard.currentHandle()
    // A phrase-only restore's predecessor seeds (empty on every ordinary run),
    // persisted below once the restored identity is added — before the switch,
    // and linked to it by name since it is not active yet.
    let restoredPredecessors = wizard.restoredPredecessors()
    dismissWizard()

    guard let secretHex, !nestUrl.isEmpty,
          let actorId = try? actor_id_from_secret(secretHex) else {
        logMessage(level: .error, target: "fauna.accounts",
                   message: "[add-account] wizard finished without full identity material; dismissing")
        return
    }

    let keychain = KeychainStore()
    let deviceId: String
    do {
        deviceId = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)
    } catch {
        // Same fallback as the first-run terminal (`performLoggedInHandoff`).
        deviceId = generate_device_id()
        logMessage(level: .error, target: "fauna.accounts",
                   message: "[add-account] deviceIdForActor failed: \(error) — minting a fresh random id")
    }

    let registry = FaunaAccounts.registry(keychain: keychain)
    let newActor: String
    do {
        newActor = try registry.addAccount(
            secretHex: secretHex, nestUrl: nestUrl, deviceId: deviceId)
        if !handle.isEmpty {
            let entry = registry.list().first { $0.actorId == newActor }
            try? registry.updateCache(
                actorId: newActor, handle: handle, domain: entry?.domain, tier: entry?.tier)
        }
        if !restoredPredecessors.isEmpty {
            registry.persistRestoredPredecessors(
                restoredActor: newActor, predecessors: restoredPredecessors)
        }
    } catch {
        logMessage(level: .error, target: "fauna.accounts",
                   message: "[add-account] addAccount failed: \(error); dismissing the wizard")
        return
    }

    await switchAccount(newActor, false)
}
