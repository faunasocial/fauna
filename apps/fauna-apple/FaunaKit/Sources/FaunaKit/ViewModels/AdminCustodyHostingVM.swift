import SwiftUI

/// Shared view-model for the admin `admin-custody-hosting` page (macOS + iOS,
/// one FaunaKit VM): the nest-wide custody-hosting registry
/// (`docs/goal/architecture/account-data-plane.md` § Two-sided bounds), fetched over
/// `fauna.admin.custody_hosting.list` (`FfiAdminClient.custodyHostingList`,
/// already folded by the shared `admin_hosting_rows` projection so every lift
/// app renders the same rows in the same order) and re-fetched after a remove.
/// Mirrors the linux reference (`apps/fauna-linux/src/client.rs`'s
/// `fetch_custody_hosting`/`remove_custody_hosting`), tui's lead
/// (`apps/fauna-tui/src/admin/mod.rs`'s `load_custody_hosting_snapshot`), and
/// android's `AdminCustodyHostingVM.kt`.
///
/// `rows == nil` is the pre-hydrate state — distinct from an answered empty
/// list. "Nobody has asked this nest to hold anything" and "the read has not
/// answered yet" are different facts, and the page must never render the
/// reassuring one for the unknown one.
@MainActor @Observable
public final class AdminCustodyHostingVM {
    public private(set) var rows: [FfiAdminHostingRow]?
    public var errorMessage: String?
    /// The remove's own verdict (Removed / Removed-and-store-freed /
    /// already-gone) — chrome text, not an error (`removed == false` is an
    /// honest no-op). No ui.yaml id of its own, mirroring tui's
    /// `Element::chrome(status)` / android's `status`.
    public private(set) var status: String?
    public private(set) var isBusy = false

    private var admin: FfiAdminClient?

    public init() {}

    /// Vend the admin client from APIClient and load the registry. Idempotent
    /// (client built once).
    public func configure(api: APIClient) async {
        if admin == nil {
            do {
                admin = try await api.adminClient()
            } catch { errorMessage = DisplayError.message(error); return }
        }
        await load()
    }

    public func load() async {
        guard let admin else { return }
        do {
            rows = try await admin.custodyHostingList()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Drop one row, keyed by the `(host, grant)` pair a `rows` entry carries
    /// (never a painted index — a re-read can reorder rows), then re-fetch.
    public func remove(hostActorId: String, grantId: Data) async {
        guard let admin else { return }
        isBusy = true
        status = nil
        defer { isBusy = false }
        do {
            let reply = try await admin.custodyHostingRemove(hostActorId: hostActorId, grantId: grantId)
            status = !reply.removed ? L.admin.custodyHosting.removeMissing
                : reply.storeDropped ? L.admin.custodyHosting.removedWithStore
                : L.admin.custodyHosting.removed
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }
}
