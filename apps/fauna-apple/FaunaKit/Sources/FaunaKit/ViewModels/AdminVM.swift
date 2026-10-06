import SwiftUI

/// View-model for the consolidated `admin-users` hub (Pending requests / Invite
/// / Users) + the admin dashboard, driven off the shared `fauna.admin.*` WS-RPC
/// kinds via the UniFFI `FfiAdminClient` (`FfiNestClient.admin()`) — **not** the
/// deleted `/admin/api/*` HTTP twins (no-http-ws-rpc-everywhere directive,
/// admin.md § Where logic lives). Shared FaunaKit, consumed by macOS + iOS.
///
/// Admission = assign a TIER (the tier *is* the quota — admin.md § Users): the
/// per-row / mint / approve tier pickers all bind to `tierNames`. Action
/// failures (tier change / evict / mint / approve / deny) route to
/// `actionError` (the `admin-users-action-error` surface), not the app banner.
@MainActor @Observable
public class AdminVM {
    // Dashboard
    public var stats: FfiAdminStats?
    public var version: String?

    // Users section
    public var users: [FfiAdminUser] = []
    public var totalUsers: Int = 0
    public private(set) var offset: Int = 0
    public let pageSize = 50

    /// Every account on the nest (`fauna_client_admin::users_list_all`) — the
    /// guardian pickers' source, kept separate from the paginated `users` page
    /// above (`admin.md` § 2 → *Which accounts a picker offers*). Refreshed
    /// alongside `loadUsers()` (mirrors linux `picker_users_read`, read beside
    /// every users-page fetch); a failed read keeps the list already held
    /// rather than emptying the pickers. Plain `public var`, not
    /// `private(set)`, matching `users` — both back a pure display-option
    /// derivation a unit test seeds directly.
    public var allUsers: [FfiAdminUser] = []

    // Tier definitions (the tier pickers' options)
    public var tiers: [FfiAdminTier] = []
    public var tierNames: [String] { tiers.map { $0.name } }

    // Invite section
    public var inviteCodes: [FfiAdminInviteCode] = []
    public var mintedCode: String?
    public var showCreateInvite = false

    // Pending requests section
    public var inviteRequests: [FfiAdminInviteRequest] = []

    // Registration section — the raw wire posture (`nil`/unrecognized ⇒ render
    // read-only, never coerced — public-mode.md § Implementation status today).
    public var registrationMode: String?
    public var maxFreeUsers: UInt64?
    /// The age require-knob (`SetupStatusReply.age_verification_required` —
    /// family-safety.md § The account age band D5+D6), read back from the same
    /// `fauna.setup.status` as the posture; the view drafts a toggle over it.
    public var ageVerificationRequired = false

    public var isLoading = false
    public var isAdmin = false
    /// App-banner-level error (dashboard load failures).
    public var errorMessage: String?
    /// Users-hub action error (`admin-users-action-error`) — the three sections'
    /// action failures land here, per admin.md § Errors.
    public var actionError: String?

    private var api: APIClient?

    public init() {}

    /// Wire the VM to the authenticated `APIClient`. The admin WS-RPC kinds are
    /// session-gated (the caller must be a nest admin) — no admin token.
    public func configure(api: APIClient) {
        self.api = api
    }

    private func admin() async throws -> FfiAdminClient {
        guard let api else { throw APIError.ffiError("AdminVM not configured") }
        return try await api.adminClient()
    }

    // MARK: - Gating

    /// `fauna.account.am_i_admin` — fail-closed: any error ⇒ not admin.
    public func checkAdmin() async {
        guard let api else { isAdmin = false; return }
        do { isAdmin = try await api.amIAdmin() }
        catch { isAdmin = false }
    }

    // MARK: - Guardian pickers (family-safety.md § Wire & data shape)

    /// The candidate guardians for the two `admin-users` guardian pickers, drawn
    /// from every account on the nest (`allUsers`), never the paginated `users`
    /// page (`admin.md` § 2 → *Which accounts a picker offers*). Suspended users
    /// are filtered out as a UX courtesy only — the nest re-validates the
    /// guardian (exists / not suspended / not itself supervised / ≠ the admitted
    /// actor) inside the admission transaction, so this list is never the
    /// authority.
    public var guardianOptions: [FfiAdminUser] {
        allUsers.filter { !$0.suspended }
    }

    /// A guardian option's display text — the shared cross-app
    /// `adminPickerOption` (the **handle**, unique on the nest and what
    /// identifies a user in an admin picker — `admin.md` § 2's two-halves
    /// rule; the editable, non-unique `label` never is; falls back to the
    /// full actor hex for a handle-less account),
    /// mirroring `AdminDnsVM.actorLabel` /
    /// `AdminWebVM`'s already-landed shape — the other resolvers this
    /// picker must never drift from again. This is what both pickers show
    /// and what the driver's `select(id, value)` sends back.
    public func guardianLabel(_ user: FfiAdminUser) -> String {
        adminPickerOption(user: user)
    }

    /// Map a picker option's display text back to its actor id — injective by
    /// construction since [`guardianLabel`] now emits handles (unique) or the
    /// full actor hex (unique), never the editable non-unique `label` two
    /// accounts could share. `nil` for the "None" sentinel (an ordinary,
    /// unsupervised admission) and for any option no longer offered (a stale
    /// draft after a re-read dropped/suspended that user).
    public func guardianActorId(forLabel text: String) -> Data? {
        if text.isEmpty || text == L.admin.usersPage.guardianNone { return nil }
        return guardianOptions.first { guardianLabel($0) == text }?.actorId
    }

    // MARK: - Dashboard

    public func loadDashboard() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            let client = try await api.adminClient()
            // Both dashboard reads on the one admin client: `fauna.admin.stats`
            // (the four stat cards) and `fauna.admin.status` (the running
            // version) — the same pair tui's `Op::LoadDashboard` and linux's
            // `fetch_admin_server_status` make. `status` is the WS-RPC face that
            // replaced the `GET /api/v1/node-info` HTTP twin the nest deleted;
            // reading the twin threw here before `isAdmin = true` and took the
            // whole dashboard load with it.
            async let s = client.stats()
            async let n = client.status()
            stats = try await s
            version = try await n.version
            isAdmin = true
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    // MARK: - Hub (all three sections)

    /// Load every section the `admin-users` hub renders. Tiers first so the tier
    /// pickers have their option set before the rows that bind to it render.
    public func loadHub() async {
        await loadTiers()
        await loadUsers()
        await loadInviteCodes()
        await loadInviteRequests()
        await loadRegistration()
    }

    /// Retry `load` up to `attempts` times, 500ms apart, until `isReady` reports
    /// true — the shared shape behind `loadHubUntilReady`/`loadDashboardUntilReady`
    /// (both retry briefly while the authed WS-RPC connection comes up, since the
    /// `FfiNestClient` may not be ready the instant the page appears).
    private func pollUntilReady(attempts: Int, load: () async -> Void, isReady: () -> Bool) async {
        for i in 0..<attempts {
            await load()
            if isReady() { return }
            if i < attempts - 1 { try? await Task.sleep(for: .milliseconds(500)) }
        }
    }

    /// Load the hub, retrying briefly while the authed WS-RPC connection comes
    /// up. A claimed nest always has at least one user (the admin), so a
    /// non-empty `users` is the ready signal.
    public func loadHubUntilReady(attempts: Int = 26) async {
        await pollUntilReady(attempts: attempts, load: { await self.loadHub() }, isReady: { !self.users.isEmpty })
    }

    /// Dashboard counterpart of `loadHubUntilReady` — `stats` is the ready signal.
    public func loadDashboardUntilReady(attempts: Int = 26) async {
        await pollUntilReady(attempts: attempts, load: { await self.loadDashboard() }, isReady: { self.stats != nil })
    }

    // MARK: - Users section

    public func loadUsers() async {
        actionError = nil
        do {
            let reply = try await admin().usersList(limit: Int64(pageSize), offset: Int64(offset))
            users = reply.users
            totalUsers = Int(reply.total)
            isAdmin = true
        } catch {
            actionError = error.localizedDescription
        }
        await loadAllUsers()
    }

    /// Every account on the nest for the guardian pickers, read alongside
    /// every `loadUsers()` page fetch (mirrors linux `picker_users_read`,
    /// read beside every `fetch_admin_users`/`fetch_admin_users_page`). A
    /// failed read keeps the list already held rather than emptying the
    /// pickers.
    private func loadAllUsers() async {
        if let all = try? await admin().usersListAll() {
            allUsers = all
        }
    }

    public func nextPage() async {
        guard let newOffset = nextPageOffset(offset: Int64(offset), total: Int64(totalUsers), pageSize: Int64(pageSize)) else { return }
        offset = Int(newOffset)
        await loadUsers()
    }

    public func prevPage() async {
        guard let newOffset = prevPageOffset(offset: Int64(offset), pageSize: Int64(pageSize)) else { return }
        offset = Int(newOffset)
        await loadUsers()
    }

    /// Change a user's tier (= quota) via `fauna.admin.users.update`, preserving
    /// the existing label; refetch so the row reflects persisted state (proves
    /// the round-trip, not an optimistic flip).
    public func setUserTier(_ user: FfiAdminUser, tier: String) async {
        guard tier != user.tier else { return }
        actionError = nil
        do {
            try await admin().usersUpdate(actorId: user.actorId, tier: tier, label: user.label)
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Start an eviction (warn→suspend→delete timeline; the user is not deleted)
    /// with a default reason + the `other` category — the only inputs the row
    /// exposes (admin.md § Users Section 3). Refetch so the row flips to cancel.
    public func evictUser(_ user: FfiAdminUser) async {
        actionError = nil
        do {
            try await admin().usersEvict(
                actorId: user.actorId,
                reason: L.admin.usersPage.evictDefaultReason,
                category: "other"
            )
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    public func cancelEviction(_ user: FfiAdminUser) async {
        actionError = nil
        do {
            try await admin().usersCancelEviction(actorId: user.actorId)
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Cut a user off *now* (no delete timeline; `admin.md` § 2 Users → *Cutting a
    /// user off*) with the default reason + `other` category, the only inputs the
    /// row exposes. Reachable from Active or a mid-eviction `warning` row (the nest
    /// clears the pending delete). Refetch so the row updates.
    public func suspendUser(_ user: FfiAdminUser) async {
        actionError = nil
        do {
            try await admin().usersSuspend(
                actorId: user.actorId,
                reason: L.admin.usersPage.suspendDefaultReason,
                category: "other"
            )
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Grant the admin role (`admin.md` § Admin continuity and succession,
    /// instrument 1) — the roster surface's `admin-users-make-admin-button`.
    /// Schedules an `AdminAdd` pending action (24h delay); the row does not
    /// flip to an admin row right away. Refetch so the row reflects persisted
    /// state (the grant itself has no immediate rendered effect).
    public func makeAdmin(_ user: FfiAdminUser) async {
        actionError = nil
        do {
            try await admin().adminsAdd(actorId: user.actorId)
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Revoke the admin role — the roster surface's
    /// `admin-users-remove-admin-button`. Schedules an `AdminRemove` pending
    /// action; the nest refuses (`fauna.admin.conflict`, surfaced via
    /// `actionError`) when it would leave zero superadmins. Refetch so the
    /// row reflects persisted state either way.
    public func removeAdmin(_ user: FfiAdminUser) async {
        actionError = nil
        do {
            try await admin().adminsRemove(actorId: user.actorId)
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    // MARK: - Registration section (admin.md § 2 Users → Section 2)

    /// Read the persisted registration posture off `fauna.setup.status` — the
    /// raw wire string, never coerced (public-mode.md § Implementation status
    /// today: a client that can't name the posture must render read-only
    /// rather than guess, since saving a guess would overwrite the nest's
    /// real posture).
    public func loadRegistration() async {
        guard let api else { return }
        do {
            let status = try await api.setupStatus()
            registrationMode = status.registrationMode
            maxFreeUsers = status.maxFreeUsers
            ageVerificationRequired = status.ageVerificationRequired
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Save the registration posture (`admin-users-registration-save-button`)
    /// — one `fauna.admin.set_registration_mode` call carrying the mode and
    /// the orthogonal free-tier ceiling together. `maxFreeUsersInput` blank ⇒
    /// no cap; a non-numeric entry is a local user error, never dispatched
    /// (mirrors tui's `registration_mutation` / android's `saveRegistration`).
    ///
    /// `ageVerification` is the require-knob's draft
    /// (`admin-users-registration-age-verification-toggle`): the same gesture
    /// dispatches `fauna.admin.set_age_verification_required` beside the mode,
    /// and only when the draft differs from the persisted knob
    /// (family-safety.md § App surface → *Age-band surfaces*; tui's
    /// `registration_mutation` changed-only knob).
    public func saveRegistration(mode: FfiRegistrationMode, maxFreeUsersInput: String,
                                 ageVerification: Bool? = nil) async {
        let trimmed = maxFreeUsersInput.trimmingCharacters(in: .whitespacesAndNewlines)
        let maxFree: UInt64?
        if trimmed.isEmpty {
            maxFree = nil
        } else if let parsed = UInt64(trimmed) {
            maxFree = parsed
        } else {
            actionError = L.admin.usersPage.maxFreeUsersHint
            return
        }
        actionError = nil
        do {
            try await admin().setRegistrationMode(mode: mode, maxFreeUsers: maxFree)
            if let ageVerification, ageVerification != ageVerificationRequired {
                try await admin().setAgeVerificationRequired(required: ageVerification)
            }
            await loadRegistration()
        } catch {
            actionError = error.localizedDescription
        }
    }

    // MARK: - Admit section (admin.md § 2 Users → Section 3)

    /// Admit a known actor id directly via `fauna.admin.users.create` — the
    /// third account-creation path (`admin-users-admit-*`; public-mode.md §
    /// Registration & Identity). `actorHex` must be exactly 64 hex chars — a
    /// malformed id is a local user error, never dispatched (the nest would
    /// refuse it anyway; failing local keeps the message actionable, mirrors
    /// tui's `admit_mutation` / android's `admitUser`). `handle` blank ⇒
    /// `nil`, the deliberate handle-less admission (public-mode.md § A
    /// handle-less account). The form is never cleared here on success or
    /// failure — the new row in the Users-section refetch is the feedback.
    public func admitUser(actorHex: String, handle: String, tier: String) async {
        let trimmed = actorHex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count == 64, trimmed.allSatisfy({ $0.isHexDigit }) else {
            actionError = L.admin.usersPage.admitActorHint
            return
        }
        actionError = nil
        let actorId = hex_to_data(trimmed)
        let trimmedHandle = handle.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            try await admin().usersCreate(
                actorId: actorId, tier: tier, handle: trimmedHandle.isEmpty ? nil : trimmedHandle)
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    // MARK: - Tier definitions

    public func loadTiers() async {
        do { tiers = try await admin().tiersList() }
        catch { actionError = error.localizedDescription }
    }

    // MARK: - Invite section

    public func loadInviteCodes() async {
        do { inviteCodes = try await admin().inviteCodesList() }
        catch { actionError = error.localizedDescription }
    }

    /// Mint a closed-registration invite code (empty code ⇒ the nest mints,
    /// returning the token — admin.md § 3 mint-on-empty). Surfaces the minted
    /// token copyable via `mintedCode`.
    ///
    /// `guardian` (from `admin-users-invite-guardian-select`, default "None") makes
    /// the redeemed account **supervised** by that actor: the additive
    /// `guardian_actor` rides the mint exactly as `tier` does, and redemption
    /// creates the user + the guardianship + the default policy in one transaction
    /// (family-safety.md § Wire & data shape). `nil` = an ordinary admission.
    /// `ageBand` (from `admin-users-invite-age-band-select`, an `ageBandOptions()`
    /// VALUE) rides beside the guardian and only with one — a band presupposes
    /// a guardianship link (family-safety.md § App surface → *Age-band surfaces*).
    @discardableResult
    public func mintInviteCode(tier: String, uses: Int, guardian: Data? = nil,
                               ageBand: String? = nil) async -> String? {
        actionError = nil
        do {
            let code = try await admin().inviteCodesCreate(code: "", tier: tier, uses: Int64(uses),
                                                           guardianActor: guardian,
                                                           ageBand: guardian == nil ? nil : ageBand)
            mintedCode = code
            showCreateInvite = false
            await loadInviteCodes()
            return code
        } catch {
            actionError = error.localizedDescription
            return nil
        }
    }

    public func deleteInviteCode(_ code: String) async {
        actionError = nil
        do {
            try await admin().inviteCodesDelete(code: code)
            await loadInviteCodes()
        } catch {
            actionError = error.localizedDescription
        }
    }

    // MARK: - Pending requests section

    public func loadInviteRequests() async {
        do {
            let all = try await admin().inviteRequestsList()
            inviteRequests = all.filter { $0.isPending }
        } catch {
            actionError = error.localizedDescription
        }
    }

    /// Approve a pending request, admitting the requester at `tier` (creates the
    /// account + removes the request). Refetch requests + users.
    ///
    /// `guardian` (from that row's `invite-request-row-guardian-select`, default
    /// "None") admits the account **supervised** by that actor — the admin picks
    /// the guardian at approval time exactly like the tier (family-safety.md
    /// § Wire & data shape). `nil` = an ordinary admission. `ageBand` (that
    /// row's `invite-request-row-age-band-select` VALUE) rides only with a
    /// guardian, as on the mint.
    public func approveRequest(id: Int64, tier: String, guardian: Data? = nil,
                               ageBand: String? = nil) async {
        actionError = nil
        do {
            _ = try await admin().inviteRequestsApprove(id: id, tier: tier, label: nil,
                                                        guardianActor: guardian,
                                                        ageBand: guardian == nil ? nil : ageBand)
            await loadInviteRequests()
            await loadUsers()
        } catch {
            actionError = error.localizedDescription
        }
    }

    public func denyRequest(id: Int64, reason: String?) async {
        actionError = nil
        let trimmed = reason?.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            try await admin().inviteRequestsDeny(id: id, reason: (trimmed?.isEmpty ?? true) ? nil : trimmed)
            await loadInviteRequests()
        } catch {
            actionError = error.localizedDescription
        }
    }
}
