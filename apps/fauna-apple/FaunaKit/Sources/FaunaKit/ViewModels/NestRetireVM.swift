#if !FAUNA_KIDS
import Foundation
import Observation

/// Which surface opened the `nest_retire` page (`nest-retirement.md` § Layout & flow).
public enum RetireEntry: Equatable, Sendable {
    /// `launch-retire-button` on the launch retry surface — no session.
    case launch
    /// `admin-nest-retire-button` over a live admin session.
    case admin
}

/// Hosts the `nest_retire` page over the shared `NestRetireMachine`
/// (`docs/goal/behavior/nest-retirement.md` § Where logic lives): this view-model
/// only builds the machine from what the app knows, forwards gestures and
/// repaints on a tick — no page logic and no provider branch (priority #2). The
/// lift of `apps/fauna-tui/src/wizard/nest_retire.rs` and android's `NestRetireVM`.
///
/// Shared by macOS and iOS. One machine per page visit: ``open`` builds it,
/// ``leave`` drops it. The VPS token typed into it is never persisted.
///
/// The machine's snapshot getters are untracked reads, so every machine call
/// bumps ``tick``; a view reads ``tick`` first and re-reads the snapshot.
@MainActor @Observable
public final class NestRetireVM {
    /// How long resolving the saved nest's host for the *current* badge may take.
    private static let resolveBound: Duration = .seconds(5)
    /// Repaint cadence while a machine call (verify / the run) is in flight.
    private static let inFlightRepaint: Duration = .milliseconds(200)

    public private(set) var machine: NestRetireMachine?
    public private(set) var entry: RetireEntry = .launch
    /// Bumped after every machine call; the page re-reads the snapshot on it.
    public private(set) var tick = 0
    /// The typed credential fields as painted (the machine holds them secret).
    public var creds: [String: String] = [:]
    public private(set) var confirmTyped = ""

    public init() {}

    private func bump() { tick += 1 }

    /// Open the page. The machine takes the e2e provider base-URL overrides at
    /// construction (`nil` in every release build), the saved nest's host as an
    /// attribution candidate, and — once resolved — that host's IPv4 for the
    /// *current* badge. The rest of what the app knows lands asynchronously
    /// (``readInputs``): every held DNS credential and the remembered DNS slots
    /// through the shared `retireHeldInputs`, and, from `admin-nest`, the nest's
    /// active local domains (§ Credential stance (b), § DNS cleanup → *Several
    /// domains* (1)).
    public func open(
        entry: RetireEntry,
        nestUrl: String,
        vpsBaseUrl: String?,
        dohBaseUrl: String?,
        activeLocalDomains: @escaping () async -> [String]
    ) {
        finish()
        self.entry = entry
        let host = URL(string: nestUrl)?.host
        let candidates = [host].compactMap { $0 }.filter { !Self.isIpLiteral($0) }
        let m = NestRetireMachine(
            candidateDomains: candidates, currentIpv4: nil,
            heldDnsProviderIds: [], heldDnsCredsJson: [],
            providerBaseUrl: vpsBaseUrl, dohBaseUrl: dohBaseUrl)
        machine = m
        // Verify is where the machine reads the inputs, so it waits for them.
        m.expectAppInputs()
        Task { [weak self] in await self?.readInputs(m, entry: entry, activeLocalDomains: activeLocalDomains) }
        if let host {
            Task { [weak self] in await self?.resolveCurrent(m, host: host) }
        }
        bump()
    }

    private func readInputs(
        _ m: NestRetireMachine, entry: RetireEntry, activeLocalDomains: () async -> [String]
    ) async {
        if entry == .admin {
            let domains = await activeLocalDomains()
            if !domains.isEmpty { m.addCandidateDomains(domains: domains) }
        }
        // The shared read: `fauna.state.dns` through the account runtime, bounded
        // in Rust. The admin entry waits for a runtime still assembling; the
        // launch entry (no session) reads only one that is already up.
        let held = await retireHeldInputs(waitForRuntime: entry == .admin)
        m.setHeldDnsJson(
            heldDnsProviderIds: held.heldDnsProviderIds, heldDnsCredsJson: held.heldDnsCredsJson)
        m.setRememberedSlotsJson(
            dkimJson: held.rememberedDkimJson, atprotoJson: held.rememberedAtprotoJson,
            faunaSelfJson: held.rememberedFaunaSelfJson)
        // A first-listing refusal is not "the account holds none": say why.
        if let reason = held.notReadyReason { m.setInputError(message: reason) }
        m.appInputsLanded()
        bump()
    }

    private func resolveCurrent(_ m: NestRetireMachine, host: String) async {
        let ipv4 = await withTaskGroup(of: String?.self) { group -> String? in
            group.addTask { await Task.detached { Self.ipv4(of: host) }.value }
            group.addTask {
                try? await Task.sleep(for: Self.resolveBound)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first
        }
        if let ipv4, machine === m {
            m.setCurrentIpv4(ipv4: ipv4)
            bump()
        }
    }

    nonisolated private static func isIpLiteral(_ host: String) -> Bool {
        host.contains(":") || host.allSatisfy { $0.isNumber || $0 == "." }
    }

    nonisolated private static func ipv4(of host: String) -> String? {
        var hints = addrinfo()
        hints.ai_family = AF_INET
        hints.ai_socktype = SOCK_STREAM
        var res: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, nil, &hints, &res) == 0, let first = res else { return nil }
        defer { freeaddrinfo(res) }
        var addr = first.pointee.ai_addr.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
            $0.pointee.sin_addr
        }
        var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        guard inet_ntop(AF_INET, &addr, &buf, socklen_t(INET_ADDRSTRLEN)) != nil else { return nil }
        return String(cString: buf)
    }

    /// Run `body` while repainting on a tick, so the run's step rows move.
    private func repainting<T>(_ body: () async -> T) async -> T {
        let repaint = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.inFlightRepaint)
                self?.bump()
            }
        }
        let result = await body()
        repaint.cancel()
        bump()
        return result
    }

    /// Run a machine call, repainting while it is in flight.
    private func runOp(_ op: @escaping (NestRetireMachine) async -> Void) {
        guard let m = machine else { return }
        Task { [weak self] in await self?.repainting { await op(m) } }
    }

    // MARK: credentials

    public func selectProvider(_ id: String) {
        guard let m = machine else { return }
        if m.selectedProvider() != id { creds.removeAll() }
        m.selectProvider(providerId: id)
        bump()
    }

    public func setCred(_ field: String, _ value: String) {
        machine?.setCredentialField(fieldId: field, value: value)
        creds[field] = value
        bump()
    }

    public func verify() { runOp { await $0.verify() } }

    /// The `hosted-auth` field's begin / wait — routed here (not straight to the
    /// machine) so the button's label repaints while the sign-in is pending.
    public func hostedAuthBegin(_ fieldId: String) async -> HostedAuthPrompt? {
        guard let m = machine else { return nil }
        let prompt = await m.hostedAuthBegin(fieldId: fieldId)
        bump()
        return prompt
    }

    public func hostedAuthWait(_ fieldId: String) async {
        guard let m = machine else { return }
        await repainting { await m.hostedAuthWait(fieldId: fieldId) }
    }

    // MARK: list / confirm

    public func select(_ serverId: String) {
        machine?.select(serverId: serverId)
        bump()
    }

    public func fetchTransferCode() { runOp { await $0.fetchTransferCode() } }

    public func beginConfirm() {
        confirmTyped = ""
        machine?.beginConfirm()
        bump()
    }

    public func setConfirmName(_ typed: String) {
        machine?.setConfirmName(typed: typed)
        confirmTyped = typed
        bump()
    }

    public func cancelConfirm() {
        confirmTyped = ""
        machine?.back()
        bump()
    }

    public func confirm() { runOp { await $0.confirm() } }
    public func retry() { runOp { await $0.retry() } }
    public func forceServer() { runOp { await $0.forceServer() } }

    /// Back, as tui settled it: from credentials — and from a run stopped on a
    /// failure (disarming it) — leave the page; from list / confirm one state
    /// back; never while a step is in flight. Returns whether the page is left.
    public func back() -> Bool {
        guard let m = machine else { return true }
        switch m.snapshot().phase {
        case .credentials, .running, .done:
            return true
        case .list, .confirm:
            confirmTyped = ""
            m.back()
            bump()
            return false
        case .listing:
            return false
        }
    }

    /// Close the page: drop the token and everything fetched with it. Returns
    /// whether this session's own nest was retired from `admin-nest` — the
    /// session this device holds is then for a nest that no longer exists, so the
    /// host signs out keeping credentials and re-enters launch routing.
    public func leave() -> Bool {
        let retiredCurrent = machine?.snapshot().retired?.current == true
        let dropSession = retiredCurrent && entry == .admin
        finish()
        return dropSession
    }

    private func finish() {
        machine?.cancel()
        machine = nil
        creds.removeAll()
        confirmTyped = ""
    }

    // MARK: admin entry input

    /// The nest's ACTIVE local domains, re-read live — the admin entry's
    /// attribution candidates (soft-deleted ones no longer publish). Best-effort:
    /// no connection or a failed read is empty.
    public static func activeLocalDomains(api: APIClient) async -> [String] {
        do {
            let local = try await api.localDomainsMachine()
            try await local.hydrate()
            return local.snapshot().active.map(\.domain)
        } catch {
            logMessage(level: .warn, target: "fauna.retire",
                       message: "[retire] active local domains read failed: \(error)")
            return []
        }
    }

    /// The e2e provider base-URL override a page-bound machine reads at
    /// construction. Read back from the wizard machine's own override map (the
    /// value `set_provider_base_urls` put there), so one harness gesture redirects
    /// both pages; the read-back arm is `test-helpers`-gated in Rust and answers
    /// `nil` in every release artifact (testing.md convention 15).
    public static func providerOverride(_ key: String, from machine: OnboardingMachine) -> String? {
        // The machine's test dispatcher is exported only by the `test-helpers`
        // FFI flavor, which every DEBUG build links and no release build does
        // (the apps' own `callMachineMethod` bridges sit behind `#if DEBUG` for
        // the same reason), so a release artifact never names the method.
        #if DEBUG
        guard let json = machine.callMachineMethodWithResult(name: "provider_base_url", jsonArg: "\"\(key)\""),
              let value = machineMethodResultValue(from: json) as? String
        else { return nil }
        return value.isEmpty ? nil : value
        #else
        return nil
        #endif
    }
}
#endif
