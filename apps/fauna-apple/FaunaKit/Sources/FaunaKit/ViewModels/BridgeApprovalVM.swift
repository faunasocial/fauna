import SwiftUI

/// Shared view-model for the admin `admin-bridges-pending` page (macOS + iOS, one
/// FaunaKit VM). A thin proxy over the shared
/// `BridgeApprovalMachine` (UniFFI): the machine owns the pending-bridge feed +
/// approve/reject orchestration (hex-decoding the card's pubkey, re-reading after
/// each mutation); this VM holds the latest `BridgeApprovalSnapshot` as
/// `@Observable` state and re-reads it after each `hydrate` / `dispatch`. The
/// machine is **pull-based** (no observer callback, like `MailSpamMachine` /
/// `MailAliasesMachine`), so re-assigning `snapshot` is what drives the SwiftUI
/// re-render.
///
/// Target behavior: `docs/goal/behavior/mail-bridge-lifecycle.md` § Pending
/// approval + § Service-user re-keying; `docs/goal/behavior/admin.md`
/// § Approved-bridges roster. Reference renderers: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_pending_bridge_card` +
/// `build_approved_bridge_card`) + web (`routes/admin/bridges-pending/+page.svelte`)
/// + windows (`AdminBridgesPendingPage`). The page reads `pending` / `approved` /
/// `status` / `error`; the `mailEnabled` / `caldavEnabled` toggles on the
/// snapshot belong to other surfaces (admin-mail / launch glue).
@MainActor @Observable
public final class BridgeApprovalVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `pending`
    /// (the approval cards) + `status` (busy gating) + `error`.
    public internal(set) var snapshot: BridgeApprovalSnapshot?
    /// Page-level error surface (`error-message`) — carries both connect/build
    /// failures and the machine's own `snapshot.error`.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: BridgeApprovalMachine?

    public init() {}

    /// The pending-bridge cards to render (empty until hydrated / when none pend).
    public var pending: [PendingBridgeView] { snapshot?.pending ?? [] }

    /// The approved-bridge roster cards to render below the pending cards, each
    /// carrying the rotate-service-user-key affordance (`admin.md` § Approved-
    /// bridges roster).
    public var approved: [ApprovedBridgeView] { snapshot?.approved ?? [] }

    /// True while a list/approve/reject round-trip is in flight — gates the
    /// approve/reject buttons (mirrors web's `status === 'Working'`).
    public var isBusy: Bool { isLoading || snapshot?.status == .working }

    /// Vend the machine from APIClient and load the first feed. The machine is
    /// built once; every call (including a re-navigation to this already-
    /// mounted page, `reloadToken`-driven) re-hydrates — mirrors
    /// `DevicesMachineVM.configure`'s own re-entry branch. **Found missing
    /// here**: a blind `guard machine == nil else { return }` meant
    /// a second navigation bumped `reloadToken`, re-firing the `.task`, but
    /// silently skipped `hydrate()` — the exact macOS twin of the gap web's
    /// own `onMount`-only fetch had for this same page, caught by this row's own two-approval e2e test.
    public func configure(api: APIClient) async {
        guard machine == nil else {
            await hydrate()
            return
        }
        do {
            machine = try await api.bridgeApprovalMachine()
            snapshot = machine?.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Re-read the pending-bridge feed from the nest (page mount / refresh).
    /// Shared plumbing — `MachineBackedVM.hydrateFromMachine()`, whose same
    /// `if let e = snap.error` guard (mirror `AdminDnsVM.dnsDispatch`) never
    /// wipes a thrown rejection's `errorMessage` back to nil (the bug that
    /// hid `admin-bridges-pending`'s rejection).
    public func hydrate() async { await hydrateFromMachine() }

    /// Approve the pending bridge identified by `pubkeyHex`, confirming its
    /// enrolled `role`. The machine flips the row to approved and re-reads the
    /// feed (so the card drops out on success).
    public func approve(pubkeyHex: String, role: String) async {
        await dispatch(.approve(pubkeyHex: pubkeyHex, role: role))
    }

    /// Reject the pending bridge identified by `pubkeyHex` (→ revoked). The
    /// machine re-reads the feed, dropping the card. Matches linux/web/windows —
    /// a direct dispatch, no confirmation dialog.
    public func reject(pubkeyHex: String) async {
        await dispatch(.reject(pubkeyHex: pubkeyHex))
    }

    /// Rotate an **approved** bridge's service-user key (`mail-bridge-lifecycle.md`
    /// § Service-user re-keying, step 3): dispatches `revoke_service_user`, which
    /// the running bridge detects and exits; the supervisor restarts it, and on a
    /// mail-enabled box the fresh key auto-approves. The machine re-reads the
    /// feed, dropping the card from the Approved roster. The UI gates this behind
    /// the `admin-bridges-rotate-confirm` reveal (no DKIM warning for any role).
    public func rotate(pubkeyHex: String) async {
        await dispatch(.rotate(pubkeyHex: pubkeyHex))
    }

    /// Dispatch an action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    private func dispatch(_ action: BridgeApprovalAction) async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.dispatch(action: action) }
        catch { errorMessage = DisplayError.message(error) }
        let snap = machine.snapshot()
        snapshot = snap
        if let e = snap.error { errorMessage = e }
    }
}
