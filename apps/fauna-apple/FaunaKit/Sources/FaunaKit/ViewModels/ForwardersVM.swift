import SwiftUI

/// Shared view-model for the admin `admin-aliases` external-forwarders page
/// (macOS + iOS, one FaunaKit VM). A thin proxy over the
/// shared `ForwarderMachine` (UniFFI): the machine owns the forwarder list +
/// hosted-domain feed + create/delete orchestration (hex-decoding the alias id,
/// re-reading after each mutation); this VM holds the latest `ForwardersSnapshot`
/// as `@Observable` state and re-reads it after each `hydrate` / `dispatch`. The
/// machine is **pull-based** (no observer callback, like `MailSpamMachine` /
/// `BridgeApprovalMachine`), so re-assigning `snapshot` drives the SwiftUI
/// re-render.
///
/// Target behavior: `docs/goal/behavior/admin.md` § 4 Aliases (external
/// forwarders, Kind 7) + `mail-aliases.md` § Kind 7. Reference renderers: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_admin_aliases_page`), windows
/// (`AdminAliasesViewModel`), web (`routes/admin/aliases/+page.svelte`).
/// This page owns ONLY the external-forwarder slice; the per-domain catch-all
/// (Kind 4) lives on `admin-dns`.
@MainActor @Observable
public final class ForwardersVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `forwarders`
    /// (the rows) + `localDomains` (the add-form domain picker) + `status`.
    public private(set) var snapshot: ForwardersSnapshot?
    /// Page-level connect/build error (`error-message`).
    public var errorMessage: String?
    /// Alias-action error (`admin-aliases-action-error`) — the machine's
    /// `snapshot.error` from a failed create/list/delete (mirrors AdminVM's
    /// `actionError`).
    public var actionError: String?
    public private(set) var isLoading = false

    private var machine: ForwarderMachine?

    public init() {}

    /// The forwarder rows to render (each row's `address` is pre-composed
    /// `<pattern>@<localDomain>`).
    public var forwarders: [ForwarderView] { snapshot?.forwarders ?? [] }
    /// Hosted local domains for the add-form picker.
    public var localDomains: [String] { snapshot?.localDomains ?? [] }
    /// True while a list/create/delete round-trip is in flight — gates the form.
    public var isBusy: Bool { isLoading || snapshot?.status == .working }

    /// Vend the machine from APIClient and load the first feed. Idempotent.
    public func configure(api: APIClient) async {
        guard machine == nil else { return }
        do {
            machine = try await api.forwardersMachine()
            snapshot = machine?.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Re-read the forwarder list + hosted domains (page mount / refresh).
    public func hydrate() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        let snap = machine.snapshot()
        snapshot = snap
        actionError = snap.error
    }

    /// Create an external forwarder `<pattern>@<localDomain>` → `forwardTarget`.
    /// The machine re-reads the list (so the new row appears) on success.
    public func create(localDomain: String, pattern: String, forwardTarget: String) async {
        await dispatch(.create(localDomain: localDomain, pattern: pattern, forwardTarget: forwardTarget))
    }

    /// Delete the forwarder identified by its hex alias id. The machine re-reads.
    public func delete(aliasIdHex: String) async {
        await dispatch(.delete(aliasIdHex: aliasIdHex))
    }

    private func dispatch(_ action: ForwarderAction) async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        try? await machine.dispatch(action: action)
        let snap = machine.snapshot()
        snapshot = snap
        actionError = snap.error
    }
}
