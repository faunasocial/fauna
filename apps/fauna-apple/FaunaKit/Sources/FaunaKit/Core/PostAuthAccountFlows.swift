import Foundation

/// Merge one identity the session client's mint confirmed into the account's
/// server-data cache (handle / domain / tier, the switcher rows) and the live
/// `SessionState.handle` — tui's and linux's `apply_self_identity`. An empty
/// field keeps whatever the cache already holds (the launch machine wrote it,
/// or an earlier mint did), so an empty handle never clobbers a cached one.
@MainActor
public func applySelfIdentity(
    _ identity: LaunchIdentity,
    actorId: String,
    registry: FfiAccountRegistry,
    session: SessionState?
) {
    let cur = registry.list().first { $0.actorId == actorId }
    let handle = identity.handle.isEmpty ? cur?.handle : identity.handle
    let domain = identity.domain.isEmpty ? cur?.domain : identity.domain
    let tier = identity.tier.isEmpty ? cur?.tier : identity.tier
    do {
        try registry.updateCache(actorId: actorId, handle: handle, domain: domain, tier: tier)
    } catch {
        logMessage(level: .warn, target: "fauna.accounts",
                   message: "[self-identity] cache update failed: \(error)")
    }
    if let handle, !handle.isEmpty {
        session?.handle = handle
    }
    if let domain, !domain.isEmpty {
        session?.domain = domain
    }
}

/// Apply every identity `next` yields until it returns `nil` (the session
/// client was torn down) or the task is cancelled — the body of
/// ``FaunaClient/startSelfIdentityFollow(session:)``, split out so a test can
/// script the subscription.
@MainActor
public func followSelfIdentity(
    next: () async -> LaunchIdentity?,
    apply: (LaunchIdentity) -> Void
) async {
    while !Task.isCancelled, let identity = await next() {
        guard !Task.isCancelled else { return }
        apply(identity)
    }
}

/// The FORCED post-auth identity re-check (`transport-trust.md` § Post-auth
/// surfacing, channel 3) — the body of the `#if DEBUG` `silent_sign_in` test
/// command `test_nest_identity_pin_post_auth.py` drives, and the one place
/// apple still runs a challenge on demand. An authenticated launch no longer
/// runs it: ``FaunaClient/startSelfIdentityFollow(session:)`` reads the session
/// mint's reply instead, and that mint's own session-ending verdict reaches
/// ``escalateSessionEnding(_:ceremonyHold:escalate:)`` through the connection
/// observer (`transport-connection.md` § The dial budget). It re-runs the same
/// silent challenge the boot-time `LaunchMachine` ran against the
/// now-authenticated session, the analog of linux's and tui's own
/// `silent_sign_in` command.
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
        case .AccountLocked:
            // Locked while signed in (`devices.md` § The locked state) — the
            // fourth escalating verdict, tui's `SilentSignInVerdict::Locked`
            // arm: the re-run launch meets the lock and paints its notice.
            logMessage(level: .warn, target: "fauna.app",
                       message: "[post-auth] this account is locked — re-entering launch")
            await escalate()
        default:
            return
        }
    } catch {
        // Reachability / classification noise — best-effort, never escalate.
    }
}

/// Route a session-ending verdict (`transport-trust.md` § Post-auth surfacing) to the
/// launch surface — the one door both the forced re-check above and the
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
    case .nestIdentityChanged, .signInRefused, .appOutdated, .nestOutdated:
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
/// `reportRefused` receives only an error thrown by `registry.addAccount`; the
/// first-run `LoggedIn` terminal (`persistLoggedIn`) stays log-only.
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
    switchAccount: (String, Bool) async -> Void,
    reportRefused: @MainActor (Error) -> Void
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
        // The wizard is already gone, so the refusal (a full account list: the shared
        // `add_refused_copy` line in `FfiError.General`) is painted on the switcher.
        reportRefused(error)
        return
    }

    await switchAccount(newActor, false)
}
