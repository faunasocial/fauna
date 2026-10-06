import SwiftUI

/// Shared view-model for the admin **`admin-dns`** page (macOS + iOS, one FaunaKit
/// VM). The page is the deployment's single
/// DNS-management surface: the per-domain record matrix + live red/green
/// verification, domain add/remove/restore CRUD, the per-domain catch-all +
/// role-address designations, managed-mode DNS-provider credentials + publish,
/// and the full TLS-cert lifecycle (status badge, managed/manual issuance,
/// manual-paste, one-time CNAME renewal-delegation, default-on auto-renew).
///
/// Two shared-Rust machines drive it, merged by domain name (priority #2 — the
/// merge is the same per-app glue linux/web/windows do):
///   - `DnsManagementMachine` (`fauna-client-dns`) — record matrix, verify, cert,
///     managed-mode credentials/publish. Held **credentialed** (loads the
///     `fauna.state.dns` tip-sealed DNS-provider credential + ACME account
///     per-operation; the nest never sees the key — `dns-management.md` § Where
///     the credential lives).
///   - `LocalDomainMachine` (`fauna-client-mail-settings`) — domain CRUD +
///     catch-all + role-address designation.
///
/// Both are **pull-based** (no observer callback, like `ForwardersVM` /
/// `BridgeApprovalVM`): re-assigning a snapshot drives the SwiftUI re-render.
/// Both machines are held for the page lifetime so a suspended manual ACME order
/// survives `BeginManualIssueCert` → `CompleteManualIssueCert`
/// (`tls-certificates.md` § C; linux `client.rs::dns_machine`).
///
/// Target behavior: `docs/goal/behavior/dns-management.md` § App surface +
/// `docs/goal/architecture/nest/tls-certificates.md` § C. Reference renderer:
/// linux (`apps/fauna-linux/src/views/admin.rs::build_dns_page` /
/// `build_domain_section` / `build_cert_issuance` / `build_cert_delegation`).
@MainActor @Observable
public final class AdminDnsVM {
    // MARK: - Rendered state (re-read after each hydrate/dispatch)

    /// DNS record matrix + verify + cert + held credentials. `nil` until configure.
    public private(set) var dns: DnsSnapshot?
    /// Domain CRUD + catch-all + role-address. `nil` until configure.
    public private(set) var local: LocalDomainsSnapshot?
    /// Admin actors for the catch-all + role-address pickers (id → label).
    /// Plain `public var`, not `private(set)`, matching `AdminVM.users` —
    /// both back a pure display-option derivation a unit test seeds directly.
    public var actors: [FfiAdminUser] = []

    /// The single page error surface (`error-message`) — both connect/build
    /// failures and the machines' `snapshot.error` route here (admin-dns has no
    /// separate action-error element, unlike admin-aliases).
    public var errorMessage: String?
    public private(set) var isLoading = false

    private var api: APIClient?
    private var dnsMachine: DnsManagementMachine?
    private var localMachine: LocalDomainMachine?
    private var admin: FfiAdminClient?
    /// Cached own-nest id (`SelfNest.id`) — the cert `target_nest_id`.
    private var targetNestId: Data?

    public init() {}

    // MARK: - Merge accessors (by domain name)

    /// Active-domain names — authoritative from the CRUD machine (is_primary /
    /// catch-all / role-address), falling back to the DNS record-matrix domain
    /// list when the CRUD snapshot hasn't loaded yet (read-only; linux does the
    /// same fall-back).
    public var activeDomainNames: [String] {
        if let active = local?.active, !active.isEmpty { return active.map(\.domain) }
        return dns?.domains.map(\.domain) ?? []
    }
    public var removedDomains: [LocalDomainView] { local?.softDeleted ?? [] }
    public func localDomain(_ name: String) -> LocalDomainView? {
        local?.active.first { $0.domain == name }
    }
    public func dnsDomain(_ name: String) -> DomainView? {
        dns?.domains.first { $0.domain == name }
    }
    public func certStatus(_ name: String) -> CertStatusRow? {
        dns?.certStatuses.first { $0.domain == name }
    }
    public func delegation(_ name: String) -> DelegationView? {
        dns?.delegations.first { $0.domain == name }
    }
    /// The single in-flight manual ACME order, if it targets `name`.
    public func pendingCert(_ name: String) -> PendingCertIssue? {
        guard let p = dns?.pendingCert, p.domain == name else { return nil }
        return p
    }
    public var credentials: [CredentialSummary] { dns?.credentials ?? [] }
    /// Sorted, de-duped zones across all held credentials — the delegate-zone
    /// picker; an empty result disables the delegate affordance.
    public var credZones: [String] {
        Array(Set((dns?.credentials ?? []).flatMap(\.zones))).sorted()
    }

    /// The single in-flight primary-domain rename, or `nil` — drives
    /// `admin-dns-rename-banner` + the per-row `admin-dns-domain-rename-state`
    /// (`mail-primary-domain-rename.md` § UX surface).
    public var activeRename: PrimaryDomainRenameView? { local?.activeRename }
    /// Whether "Rename primary domain" is offerable — a primary plus at least
    /// one active non-primary domain to promote (the two-step rule). A UX hint
    /// only; the nest re-validates every dispatch.
    public var renameAvailable: Bool { local?.renameAvailable ?? false }

    /// Whether every active domain is managed — drives the `admin-dns-manage-all-toggle`
    /// reflected state (the shared `DnsSnapshot::all_domains_managed` projection;
    /// adopting clients call it, never re-fold `mode == "managed"`).
    public var allManaged: Bool {
        guard let dnsMachine, !activeDomainNames.isEmpty else { return false }
        return dnsMachine.allDomainsManaged(activeDomains: activeDomainNames)
    }
    /// Per-domain managed read — the shared `DomainView::is_managed()` projection.
    public func isManaged(_ name: String) -> Bool {
        guard let d = dnsDomain(name) else { return false }
        return domainIsManaged(view: d)
    }
    /// The auto-renew checkbox + cert issuance treat managed OR delegated domains
    /// alike (only those can auto-issue — `tls-certificates.md` § C.3).
    public func isManagedOrDelegated(_ name: String) -> Bool {
        isManaged(name) || delegation(name) != nil
    }
    public var isBusy: Bool {
        isLoading || dns?.status == .working || dns?.status == .loading
            || local?.status == .working || local?.status == .loading
    }

    /// Display label for an actor id (catch-all / role-address picker): the
    /// shared cross-app `adminPickerOption` (handle, else full actor hex —
    /// never the editable, non-unique `label`; `admin.md` § 2's two-halves
    /// rule), or the full hex directly when the actor isn't in the loaded
    /// list (mirrors linux's fallback).
    public func actorLabel(_ id: Data) -> String {
        if let a = actors.first(where: { $0.actorId == id }) {
            return adminPickerOption(user: a)
        }
        return L.admin.actorIdFallbackLabel(short: hexFull(bytes: id))
    }

    // MARK: - Lifecycle (pull-based; machines built once, hydrate on every appear)

    public func configure(api: APIClient) async {
        self.api = api
        if dnsMachine == nil {
            do {
                dnsMachine = try await api.dnsManagementMachine()
                localMachine = try await api.localDomainsMachine()
                admin = try await api.adminClient()
            } catch {
                errorMessage = DisplayError.message(error)
                return
            }
        }
        await hydrate()
    }

    /// Full page load: matrix → live verify → cert-status → domain list → actors.
    /// Mirrors linux `fetch_dns_records` + the cert-status refresh.
    public func hydrate() async {
        isLoading = true
        defer { isLoading = false }
        await refreshDnsMatrix()
        await localRefresh()
        await loadActors()
    }
    public func refresh() async { await hydrate() }

    /// Re-fetch the DNS record matrix → live verify → served-cert status.
    /// Shared by `hydrate()` and the rename actions below that move records/
    /// certs (start/complete/abort re-target or widen the SAN set; extend does
    /// not, so it skips this — mirrors web `refreshDnsMatrix`).
    private func refreshDnsMatrix() async {
        await dnsDispatch(.refresh)
        await dnsDispatch(.verifyRecords(domain: nil))
        await dnsDispatch(.refreshCertStatus)
    }

    // MARK: - DNS-management actions

    /// `admin-dns-domain-mode` toggle — mirror linux `dns_set_mode`: Refresh,
    /// SetMode (the shared machine publishes the records itself on opt-in), then
    /// — ONLY when SetMode succeeded — re-verify. The guard keeps SetMode's
    /// error (a missing covering credential, or a failed publish after the
    /// committed opt-in) on screen: a trailing verify would clear it.
    public func setMode(domain: String, managed: Bool) async {
        await dnsDispatch(.refresh)
        guard await dnsDispatch(.setMode(domain: domain, managed: managed)) else { return }
        await dnsDispatch(.verifyRecords(domain: nil))
    }

    /// `admin-dns-manage-all-toggle` — mirror linux `dns_set_all_managed`:
    /// Refresh, then SetMode (which publishes on opt-in) per domain, STOPPING at the first failure so the offending domain's error
    /// (typically a missing covering credential) survives; only a clean sweep
    /// re-verifies. Same guard as `setMode` — an unguarded chain would wipe the
    /// rejection (domains already switched keep their new mode).
    public func setAllManaged(_ managed: Bool) async {
        await dnsDispatch(.refresh)
        var errored = false
        for name in activeDomainNames {
            guard await dnsDispatch(.setMode(domain: name, managed: managed)) else {
                errored = true; break
            }
        }
        if !errored {
            await dnsDispatch(.verifyRecords(domain: nil))
        }
    }

    public func putCredentials(providerId: String, fields: [DnsCredentialField], label: String) async {
        await dnsDispatch(.putCredentials(providerId: providerId, fields: fields, label: label))
    }
    public func clearCredentials(index: UInt32) async {
        await dnsDispatch(.clearCredentials(index: index))
    }

    // MARK: - Cert-lifecycle actions

    /// Single "Get / Renew" button: managed/delegated → one-step `IssueCert`;
    /// manual → two-phase `BeginManualIssueCert` (populates `pending_cert`).
    public func issueCert(domain: String, managedOrDelegated: Bool) async {
        guard let nestId = await resolveTargetNestId() else { return }
        if managedOrDelegated {
            await dnsDispatch(.issueCert(domain: domain, targetNestId: nestId))
        } else {
            await dnsDispatch(.beginManualIssueCert(domain: domain, targetNestId: nestId))
        }
        await dnsDispatch(.refreshCertStatus)
    }
    public func completeManualIssue() async {
        await dnsDispatch(.completeManualIssueCert)
        await dnsDispatch(.refreshCertStatus)
    }
    public func cancelManualIssue() async { await dnsDispatch(.cancelManualIssueCert) }
    public func delegateRenewal(domain: String, zone: String) async {
        await dnsDispatch(.delegateRenewal(domain: domain, targetZone: zone))
    }
    public func removeDelegation(domain: String) async {
        await dnsDispatch(.removeDelegation(domain: domain))
    }
    public func setAutoRenew(domain: String, enabled: Bool) async {
        await dnsDispatch(.setAutoRenew(domain: domain, enabled: enabled))
    }

    // MARK: - Local-domain (CRUD + catch-all + role-address) actions

    public func addDomain(_ domain: String) async {
        // The default mirrors `local_domains.rs` DEFAULT_CERT_MODE. The MTA-STS
        // policy mode is not sent: the nest sets and advances it.
        await localDispatch(.addDomain(domain: domain,
                                       mtaStsCertMode: "expand_primary"))
        await dnsDispatch(.refresh)
        await dnsDispatch(.verifyRecords(domain: nil))
    }
    public func removeDomain(_ domain: String) async {
        await localDispatch(.removeDomain(domain: domain))
        await dnsDispatch(.refresh)
    }
    public func restoreDomain(_ domain: String) async {
        await localDispatch(.restoreDomain(domain: domain))
        await dnsDispatch(.refresh)
    }
    public func setCatchAll(domain: String, actorId: Data?) async {
        await localDispatch(.setCatchAllActor(domain: domain, actorId: actorId))
    }
    public func setRoleAddress(domain: String, role: RoleAddressKind, actorId: Data?) async {
        await localDispatch(.setRoleAddress(domain: domain, role: role, actorId: actorId))
    }

    // MARK: - Primary-domain rename (mail-primary-domain-rename.md § UX surface)
    //
    // The nest owns all validation (single-active-rename / new-primary-is-
    // additional / cert-mode / TLS-posture / SAN-cap / grace-not-expired);
    // every dispatch surfaces a refusal via `local.error`. The client only
    // picks the target + grace override and reads back the projected state.
    // Mirrors web `admin/dns/+page.svelte` (`submitRename`/`completeRename`/
    // `extendRename`/`abortRename`).

    /// `admin-dns-rename-submit-button` — begin renaming the primary to an
    /// existing active additional. `graceDays` is `nil` for the nest default (7).
    public func startPrimaryRename(newPrimaryDomainId: Data, graceDays: Int64?) async {
        await localDispatch(.startPrimaryRename(newPrimaryDomainId: newPrimaryDomainId, graceDays: graceDays))
        await refreshDnsMatrix()
    }
    /// `admin-dns-rename-complete-confirm-button` — `force` is the caller's
    /// `canForceComplete` read (early-complete from `grace`, accepting the
    /// cache-flush risk); a plain complete from `readyToComplete` otherwise.
    public func completePrimaryRename(renameId: Data, force: Bool) async {
        await localDispatch(.completePrimaryRename(renameId: renameId, force: force))
        await refreshDnsMatrix()
    }
    /// `admin-dns-rename-extend-button` — push the grace window out by
    /// `additionalDays`. No DNS-matrix refresh: only the watched deadline moves.
    public func extendPrimaryRenameGrace(renameId: Data, additionalDays: Int64) async {
        await localDispatch(.extendPrimaryRenameGrace(renameId: renameId, additionalDays: additionalDays))
    }
    /// `admin-dns-rename-abort-confirm-button` — unwind the in-flight rename
    /// (the cheap pre-flip path, or the expensive post-flip inverse re-flip).
    public func abortPrimaryRename(renameId: Data, reason: String?) async {
        await localDispatch(.abortPrimaryRename(renameId: renameId, reason: reason))
        await refreshDnsMatrix()
    }

    // MARK: - Plumbing

    /// Dispatch a DNS-machine action and return whether it COMMITTED — false if
    /// the dispatch threw OR the resulting snapshot carries an error. Callers
    /// that chain a trailing dispatch (publish/verify after SetMode) must guard
    /// on this: each dispatch clears `snapshot.error` at its start, so an
    /// unguarded trailing dispatch on a failed step wipes the good error (the
    /// macOS analog of linux `dns_set_mode`'s `if set_result.is_ok()` guard,
    /// `apps/fauna-linux/src/client.rs:3442`).
    @discardableResult
    private func dnsDispatch(_ action: DnsAction) async -> Bool {
        guard let dnsMachine else { return false }
        var ok = true
        do { try await dnsMachine.dispatch(action: action) }
        catch { errorMessage = DisplayError.message(error); ok = false }
        let snap = dnsMachine.snapshot()
        dns = snap
        if let e = snap.error { errorMessage = e; ok = false }
        return ok
    }

    private func localDispatch(_ action: LocalDomainAction) async {
        guard let localMachine else { return }
        do { try await localMachine.dispatch(action: action) }
        catch { errorMessage = DisplayError.message(error) }
        let snap = localMachine.snapshot()
        local = snap
        if let e = snap.error { errorMessage = e }
    }

    private func localRefresh() async {
        guard let localMachine else { return }
        do { try await localMachine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        local = localMachine.snapshot()
    }

    private func loadActors() async {
        guard let admin else { return }
        // Every account on the nest for the catch-all + role-address pickers
        // (`fauna_client_admin::users_list_all`; `admin.md` § 2 → *Which
        // accounts a picker offers*) — never a single `fauna.admin.users.list`
        // page. A failed read keeps the list already held (mirrors linux
        // `picker_users_read`), never blocking the designation.
        if let all = try? await admin.usersListAll() {
            actors = all
        }
    }

    private func resolveTargetNestId() async -> Data? {
        if let targetNestId { return targetNestId }
        do {
            let id = try await api?.thisNestId()
            targetNestId = id
            return id
        } catch {
            errorMessage = DisplayError.message(error)
            return nil
        }
    }
}
