import SwiftUI

/// Shared view-model for the **backup-destination management** section of the
/// Backups page (`docs/goal/ui/backups.md` § Manage backup destinations), macOS +
/// iOS (one FaunaKit VM). A thin proxy over the four thin
/// FFI free fns (`backupDestinationsList` / `backupDestinationAdd` /
/// `backupDestinationEdit` / `backupDestinationRemove`, vended by `APIClient`) —
/// **no machine** (the native seam the linux/web/android thin glue uses, not a
/// `BackupDestinationsMachine`). It holds the latest destination list as
/// `@Observable` state and the inline add/edit + remove-confirm dialog state, and
/// re-reads the freshly-persisted list each FFI call returns.
///
/// Mirrors the linux lead `apps/fauna-linux/src/views/backups/destinations.rs`:
/// each mutation resolves identity (add / URL-change edit) → mutates
/// the `fauna.state.backup` rows → the plane write (the single atomic decision point,
/// so a client crash mid-enroll/mid-removal is recoverable — the coordinator
/// reconciles on its next pass). The per-row LIVE status
/// (`backup-destination-last-upload-time` / `-backlog-count`) is read from the
/// **nest's** `fauna.backup.status` projection over the FFI fn
/// `backupDestinationStatus(nest, ownerSecret)` and rebuilt on page mount + after
/// every add/edit/remove, uniform with every other app since the 2026-07-24
/// leg-(d) repoint (`backups.md` § Per-destination status read) — there is no
/// local source-side coordinator behind this page any more, so the numbers stay
/// live even with the app closed.
@MainActor @Observable
public final class BackupDestinationsVM {
    /// The configured destinations (the indexed `backup-destination-status-row`s).
    public private(set) var destinations: [FfiBackupDestinationView] = []
    /// The LIVE per-destination status (last-upload time + backlog), keyed by
    /// `destination_id` — read from the nest's `fauna.backup.status` projection
    /// (`backups.md` § Per-destination status read). A destination with no entry
    /// (read not yet done, or it degraded on a transient socket hiccup) renders
    /// the not-yet-backed-up baseline ("never" / "0 queued"), uniform with linux.
    public private(set) var statuses: [String: FfiBackupDestinationStatus] = [:]
    /// The client-side backup audit's per-destination verdicts (`ui/backups.md`
    /// § Audit-alert surface — trusts neither source nest nor destination
    /// self-report), keyed by `destination_id`. Re-run on page mount + after
    /// every add/edit/remove, mirroring the status read's cadence; NOT a
    /// polling loop (android's model — `BackupDestinationsVM.kt`'s
    /// `triggerAudit`/`refreshAudit`).
    public private(set) var auditRows: [String: FfiDestinationAuditRow] = [:]
    /// Page-level error surface (`error-message`) — carries connect/resolve/save
    /// failures and the interim "Resolving…" status, mirroring linux.
    public var errorMessage: String?
    public private(set) var isLoading = false

    /// Inline add/edit dialog reveal (`backup-destination-add-modal`).
    public private(set) var formVisible = false
    /// `Some(id)` while editing an existing destination; `nil` while adding.
    public private(set) var editingId: String?
    /// Bound by the dialog's `backup-destination-url-input` / `-name-input`.
    public var urlText = ""
    public var nameText = ""
    /// Bound by `backup-destination-kind-select` — the wire value (`"nest"` /
    /// `"client-device"`), never a per-app literal. Add-only: the row's own
    /// kind is prefilled but the control paints disabled in edit mode
    /// (mirrors linux/tui — the kind is not an editable property).
    public var kindInput = ""
    /// Bound by `backup-destination-capacity-input` (client-device kind only).
    /// A blank value is a real choice (uncapped), never a placeholder.
    public var capacityText = ""

    /// `Some(id)` of the destination the remove-confirm dialog
    /// (`backup-destination-remove-confirm-modal`) is armed for.
    public private(set) var removingId: String?

    /// How many bytes this device's **orphaned** sealed custodian store holds —
    /// `nil` whenever `backup-orphaned-store-row` must not paint at all
    /// (`backups.md` § Manage backup destinations → *Reclaim this device's copy*).
    ///
    /// A cached **verdict**, reached in `refreshOrphanedStore` on mount and after
    /// every mutation, and deliberately **never recomputed on a render path** —
    /// it costs an agent round trip (macOS) or a disk walk (iOS) on top of the
    /// destination-row join, and a view that recomputed it would pay both on
    /// every repaint. Uniform with linux's `Ctx::orphaned_store`.
    public private(set) var orphanedStoreBytes: UInt64?

    /// Whether `backup-reclaim-confirm-modal` is open — the plain confirm in
    /// front of the destructive gesture (no re-type: reclaiming ends this
    /// device's standalone-restore property, which is the reason for the modal,
    /// but the copy is re-buildable from a fresh pull on re-enrollment).
    public private(set) var reclaiming = false

    /// The `backup-destination-remove-reclaim-checkbox` tick inside an open
    /// remove-confirm dialog: *also* free this device's copy once the removal
    /// lands. Reset whenever the dialog opens or closes — an opt-in must never
    /// survive from one removal into the next.
    public var removeReclaim = false

    /// Whether `backup-destination-reseed-confirm-modal` is open — the plain
    /// confirm in front of the restore (`backups.md` § Restore after losing the
    /// nest). No re-type: the restore deletes nothing, here or on the nest.
    public private(set) var reseedConfirming = false

    /// A restore is running: every `backup-destination-reseed-button` is
    /// disarmed until it ends, so one tap cannot start two ceremonies.
    public private(set) var reseedRunning = false

    /// `backup-destination-reseed-result`'s text — the running line while the
    /// ceremony runs, then the shared `result_lines`; `nil` until a first run
    /// and after a run that stopped (a stop renders on `error-message`).
    public private(set) var reseedResultText: String?

    private var api: APIClient?
    /// Where this device's sealed custodian store lives on this platform — the
    /// agent on macOS, this process's own disk on iOS. One seam so neither this
    /// VM nor the view carries an `#if os(...)` (`CustodianStoreAccess`).
    /// `nil` in a unit test that doesn't exercise the reclaim surface, and on any
    /// shell that never configured one: the row then simply never paints.
    private var custodianStore: CustodianStoreAccess?
    /// This device's stable sync device id — the enroll verb's
    /// `custodian_device_id` (never the identity actor id). `nil` before
    /// `configure` runs.
    private var deviceId: String?
    // No upload-driver handle: apple's in-app segment-backup upload driver went
    // at the slice-5 flip 2026-08-15, so there is nothing to rebuild when the
    // destination set changes — the **source nest** picks the new set up on its
    // own sweep. Status already came from the nest's `fauna.backup.status`
    // projection (the leg-(d) repoint), so this page's reads are unaffected.

    /// Stable machine token `backup_destination_edit` returns when the new URL
    /// resolves to a *different* nest identity (locale-agnostic by design — mapped
    /// to the i18n string here, keeping i18n client-side).
    private static let editDifferentNestToken = "backup-destination-edit-different-nest"

    #if DEBUG
    /// The currently-displayed page's VM, so the `backup_audit_run_now` TestAgent
    /// command (`BackupAuditTestCommand`) can poke a re-run on the SAME instance
    /// the Backups page renders — this VM is page-local `@State`, unlike
    /// `FeedVM`/`ConversationsVM`. Same shape as `AtprotoSettingsVM
    /// .liveInstanceForTest`. **Weak**: must never keep a dismissed page's VM
    /// alive, and a command arriving with no page open must fail loudly rather
    /// than mutate a dead one (convention 11). `#if DEBUG` per convention 15.
    @MainActor
    public static weak var liveInstanceForTest: BackupDestinationsVM?
    #endif

    public init() {}

    #if DEBUG
    /// Seed the destination list without a nest round trip — **unit tests
    /// only** (convention 15: the automation/test surface is compiled out of
    /// release artifacts). It exists because the reclaim rules this page turns
    /// on — which rows claim this device's store, whether the remove dialog
    /// offers the opt-in — are decidable from the list alone, and a rule that
    /// arms a destructive gesture must be provable without a live nest.
    func seedDestinationsForTest(_ rows: [FfiBackupDestinationView]) {
        destinations = rows
    }
    #endif

    public var isEmpty: Bool { destinations.isEmpty }

    /// `backup-sole-client-destination-warning` — standing warning while
    /// EVERY configured destination is a client device (devices get lost,
    /// wiped and replaced; the user has no off-device copy). Wraps the shared
    /// `everyDestinationIsAClientDevice`, which counts an `Inert` row as *not*
    /// a client device.
    public var showSoleClientDestinationWarning: Bool {
        !destinations.isEmpty && everyDestinationIsAClientDevice(destinations: destinations)
    }

    public var formTitle: String {
        editingId == nil
            ? L.backups.backupDestinationFormAddTitle
            : L.backups.backupDestinationFormEditTitle
    }

    public func configure(
        api: APIClient, deviceId: String? = nil, custodianStore: CustodianStoreAccess? = nil
    ) {
        self.api = api
        self.deviceId = deviceId
        self.custodianStore = custodianStore
        #if DEBUG
        // Late-bind on every configure (not just the first): re-entering the
        // page builds a fresh VM, and the agent must reach THAT one.
        Self.liveInstanceForTest = self
        #endif
    }

    /// Load the destination list with the same WS-up retry linux uses (the page
    /// can mount before the WS-RPC socket is ready). A failed first hydrate leaves
    /// the empty placeholder rather than a scary error — the user hasn't acted yet;
    /// a real error surfaces on the first mutation.
    ///
    /// Kept: `listBackupDestinations` composes a config-store fetch with a
    /// local unseal, not a single NestClient RPC (transport.md § Request
    /// lifecycle step 3's note; mirrors linux's `load_destinations`).
    public func hydrate() async {
        guard let api else { return }
        isLoading = true
        defer { isLoading = false }
        for _ in 0..<10 {
            do {
                destinations = try await api.listBackupDestinations()
                await refreshStatuses()
                await runAudit()
                await refreshOrphanedStore()
                return
            } catch {
                try? await Task.sleep(nanoseconds: 500_000_000)
            }
        }
    }

    /// Run one client-side audit pass and rebuild `auditRows`, keyed by
    /// `destination_id`. A transient failure leaves the prior map (degrade to
    /// the last-known verdict rather than a scary error) — mirrors
    /// `refreshStatuses`. `state_path` is actor-scoped
    /// (`FaunaClient.backupAuditStatePath`), never shared across accounts; the
    /// sync state dir anchors the covered-folder mirror plane on this device's
    /// own replica (`FaunaClient.syncStateDir`, the sync engine host's dir).
    public func runAudit() async {
        guard let api, !destinations.isEmpty else {
            auditRows = [:]
            return
        }
        let ownCustodian = await ownCustodianStore()
        guard
            let rows = try? await api.backupAuditRunPass(
                statePath: FaunaClient.backupAuditStatePath,
                syncStateDir: FaunaClient.syncStateDir,
                ownCustodian: ownCustodian)
        else { return }
        auditRows = Dictionary(uniqueKeysWithValues: rows.map { ($0.destinationId, $0) })
    }

    /// This device's own custodian store read, handed to the audit pass so the
    /// shared fold can keep the fifth alert reason (*source regressed*) up until
    /// recovered. The same `CustodianStoreAccess` read and device id the
    /// orphaned-store verdict uses; nothing is decided here — the shared pass
    /// picks the row. A missing seam, a missing device id or a failed read is
    /// `nil` ("no store was read"), which leaves the row's record standing.
    func ownCustodianStore() async -> FfiOwnCustodianStore? {
        guard let custodianStore, let deviceId, !deviceId.isEmpty else { return nil }
        guard let info = try? await custodianStore.custodianStoreFootprint() else { return nil }
        return FfiOwnCustodianStore(deviceId: deviceId, sourceRegressions: info.sourceRegressions)
    }

    /// Read the LIVE per-destination status and rebuild the `statuses` map, keyed
    /// by `destination_id`. Zero destinations ⇒ an empty map (the rows render only
    /// when ≥1 destination is configured; the FFI fn also skips its build +
    /// `fauna.segments.list` round-trip). A transient failure leaves the prior map
    /// (degrade to baseline before the first read), matching linux's
    /// `run_status_read` — no scary error before the user acts. Re-run on page
    /// mount (via `hydrate`) and after every add/edit/remove (`backups.md`
    /// § Per-destination status read).
    private func refreshStatuses() async {
        guard let api, !destinations.isEmpty else {
            statuses = [:]
            return
        }
        // One read for every app since the leg-(d) repoint: the nest's
        // `fauna.backup.status` projection. The old macOS fold onto the
        // always-on upload driver's open `segment-backup.sqlite` handle is gone
        // — there is no local coordinator state behind this page any more, and
        // the nest's numbers are live with the app closed
        // (`backups.md` § Per-destination status read).
        let rows: [FfiBackupDestinationStatus]? = try? await api.loadBackupDestinationStatus()
        guard let rows else { return }
        statuses = Dictionary(uniqueKeysWithValues: rows.map { ($0.destinationId, $0) })
    }

    // MARK: - Add/edit dialog

    public func openAddForm() {
        editingId = nil
        urlText = ""
        nameText = ""
        capacityText = ""
        kindInput = kindOptions.first?.tag ?? ""
        errorMessage = nil
        removingId = nil
        formVisible = true
    }

    public func openEditForm(_ dest: FfiBackupDestinationView) {
        editingId = dest.destinationId
        urlText = dest.destinationNestUrl
        nameText = dest.displayName ?? ""
        // Prefilled from the row so the disabled select paints THIS row's own
        // kind rather than the add-dialog default. An unrecognized kind (a
        // newer client wrote it) falls back to the first catalog option
        // (`nest`) — mirrors tui/linux; the control is disabled here and never
        // read back, so it cannot rewrite the row.
        kindInput = kindOptions.first(where: { $0.tag == dest.kind })?.tag
            ?? kindOptions.first?.tag ?? ""
        capacityText = dest.capacityCapBytes.map { ValueFormat.byteSize($0) } ?? ""
        errorMessage = nil
        removingId = nil
        formVisible = true
    }

    public func cancelForm() {
        formVisible = false
        editingId = nil
    }

    /// The `backup-destination-kind-select` catalog — the wire value + its
    /// localized label, straight off the shared FFI catalog (`kind_options()`)
    /// so the paint order and the label text can never drift from the row
    /// badge's own `kindBadgeText(for:)`.
    public var kindOptions: [(tag: String, label: String)] {
        backupDestinationKindOptions().map { (tag: $0.value, label: renderLocalizedText($0.label)) }
    }

    /// Whether the (add-dialog) `kindInput` is currently the client-device
    /// kind — never a hard-coded `"client-device"` literal.
    public var isCustodianKind: Bool { kindInput == destinationKindClientDevice() }

    /// Read the dialog inputs and dispatch add / enroll-custodian / edit (per
    /// `editingId` + `kindInput`). A blank URL is a no-op on the nest path
    /// (mirrors linux `submit_form`); a blank name defaults to the
    /// destination's handle domain inside the FFI fn. Add + client-device kind
    /// takes a wholly separate branch — no URL at all — handled by
    /// `submitCustodianEnroll()`.
    public func submitForm() async {
        guard let api else { return }
        if editingId == nil, isCustodianKind {
            await submitCustodianEnroll()
            return
        }
        let url = urlText.trimmingCharacters(in: .whitespacesAndNewlines)
        let name = nameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else { return }
        // Resolving a destination is a network round-trip; show interim status in
        // the same surface linux does.
        errorMessage = L.backups.backupDestinationResolving
        isLoading = true
        defer { isLoading = false }
        do {
            let updated: [FfiBackupDestinationView]
            if let id = editingId {
                updated = try await api.editBackupDestination(id: id, url: url, name: name)
            } else {
                updated = try await api.addBackupDestination(url: url, name: name)
            }
            destinations = updated
            await refreshStatuses()
            await runAudit()
            await refreshOrphanedStore()
            errorMessage = nil
            formVisible = false
            editingId = nil
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// Add + client-device kind: no URL, no resolve round-trip, no
    /// `NestBackupKey` grant — mints its own `destination_id`. A blank
    /// capacity is a real choice (uncapped, `nil`); an unparseable one is a
    /// refusal the user sees, never a substituted default (guessing a cap is
    /// how a device's disk fills).
    private func submitCustodianEnroll() async {
        guard let api, let deviceId else { return }
        let name = nameText.trimmingCharacters(in: .whitespacesAndNewlines)
        let typed = capacityText.trimmingCharacters(in: .whitespacesAndNewlines)
        let cap: UInt64?
        if typed.isEmpty {
            cap = nil
        } else if let bytes = parseByteSize(input: typed) {
            cap = bytes
        } else {
            errorMessage = L.backups.backupDestinationCapacityInvalid
            return
        }
        isLoading = true
        defer { isLoading = false }
        do {
            destinations = try await api.enrollCustodianBackupDestination(
                custodianDeviceId: deviceId, name: name, capacityCapBytes: cap)
            await refreshStatuses()
            await runAudit()
            await refreshOrphanedStore()
            errorMessage = nil
            formVisible = false
            editingId = nil
        } catch {
            errorMessage = mapError(error)
        }
    }

    // MARK: - Remove-confirm dialog

    public func armRemove(_ id: String) {
        removingId = id
        formVisible = false
        // A fresh dialog, a fresh opt-in: an earlier removal's tick must never
        // carry into this one and free a copy the user did not offer up.
        removeReclaim = false
    }

    public func cancelRemove() {
        removingId = nil
        removeReclaim = false
    }

    /// Remove the armed destination, and — only when
    /// `backup-destination-remove-reclaim-checkbox` is ticked — free this
    /// device's copy straight after.
    ///
    /// **Ordering is load-bearing and mirrors linux's
    /// `remove_destination_and_maybe_reclaim` (and android's `remove`):** the
    /// reclaim runs strictly AFTER a successful deregister, never before and
    /// never concurrently. Reclaiming first would destroy the owner's only
    /// offline copy while the destination row still stood — and had the removal
    /// then failed, the user would be left with a row claiming a copy that no
    /// longer exists.
    public func confirmRemove() async {
        guard let api, let id = removingId else { return }
        let alsoReclaim = removeReclaim
        removingId = nil
        removeReclaim = false
        isLoading = true
        defer { isLoading = false }
        do {
            // Committed the moment it lands. Nothing below may retract it: a
            // failed reclaim afterwards is reported ALONGSIDE a removal that
            // really did happen, never instead of it.
            destinations = try await api.removeBackupDestination(id: id)
            errorMessage = nil
            if alsoReclaim {
                await reclaimAfterRemoval()
            }
            await refreshStatuses()
            await runAudit()
            await refreshOrphanedStore()
        } catch {
            errorMessage = mapError(error)
        }
    }

    // MARK: - Keep a row a succession carried across

    /// **Keep** — `backup-destination-keep-button` on a raised row. No confirm,
    /// unlike Remove: Keep is non-destructive and re-decidable (the row stays
    /// removable forever after). The returned list is the at-rest re-read, so
    /// the mark stops rendering because the row now reads `unattested: false`,
    /// never because this VM flipped it optimistically.
    public func keep(_ id: String) async {
        guard let api else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            destinations = try await api.keepBackupDestination(id: id)
            errorMessage = nil
        } catch {
            errorMessage = mapError(error)
        }
    }

    // MARK: - Reclaim this device's copy

    /// Arm `backup-reclaim-confirm-modal`. Guarded on the cached verdict — the
    /// backstop for an actuation that reaches the button without the row being
    /// painted (a stale in-process registry entry, a keyboard path): the gesture
    /// deletes the owner's only offline copy, so "the row is not showing" must
    /// mean "the gesture does nothing", not "the gesture runs anyway".
    public func armReclaim() {
        guard orphanedStoreBytes != nil else { return }
        reclaiming = true
    }

    /// Close the confirm dialog without freeing anything.
    public func cancelReclaim() {
        reclaiming = false
    }

    /// Free this device's whole sealed store — the confirmed
    /// `backup-destination-reclaim-button` action, not tied to a removal.
    ///
    /// A `stillHosting` refusal is surfaced as an error even though the call
    /// itself succeeded: nothing was deleted, and reporting it as success would
    /// retire the row in the user's mind while the bytes remain. The refresh
    /// afterwards repaints the row from what is actually on disk, so a refusal
    /// leaves the offer standing — which is the retry.
    public func confirmReclaim() async {
        guard let custodianStore else { return }
        reclaiming = false
        isLoading = true
        defer { isLoading = false }
        do {
            let outcome = try await custodianStore.reclaimCustodianStore()
            errorMessage = outcome.stillHosting ? L.backups.backupReclaimStillHosting : nil
        } catch {
            errorMessage = mapError(error)
        }
        await refreshOrphanedStore()
    }

    /// The opt-in reclaim that follows a landed removal.
    ///
    /// Surfaces a refusal or failure without ever claiming the removal failed —
    /// and deliberately surfaces the shared `stillHosting` sentence itself rather
    /// than composing an English "removed, but could not free" line around it:
    /// that composed string has no approved i18n key, the removal's success is
    /// already visible in the list it just left, and what the user needs is the
    /// reason the bytes are still there plus a way to retry — which is the
    /// `backup-orphaned-store-row` the refresh after this is about to paint.
    private func reclaimAfterRemoval() async {
        guard let custodianStore else { return }
        do {
            let outcome = try await custodianStore.reclaimCustodianStore()
            if outcome.stillHosting { errorMessage = L.backups.backupReclaimStillHosting }
        } catch {
            errorMessage = mapError(error)
        }
    }

    // MARK: - Restore after losing the nest (re-seed)

    /// Where `backup-destination-reseed-button` paints: `nil` for the orphaned
    /// store's row, a `destination_id` for a destination row. The shared
    /// `backupReseedRows` decides (the rule linux's and tui's `reseed_rows`
    /// apply) over state this VM already caches — the list and the orphaned
    /// verdict — so reading it on a render path costs no round trip.
    public var reseedRows: [String?] {
        guard custodianStore != nil else { return [] }
        return backupReseedRows(
            destinations: destinations, thisDeviceId: deviceId ?? "",
            orphanedStore: orphanedStoreBytes != nil)
    }

    /// Does the orphaned-store row carry the restore button?
    public var reseedOnOrphanedStore: Bool { reseedRows.contains(nil) }

    /// Does this destination row carry the restore button?
    public func reseedOnRow(_ dest: FfiBackupDestinationView) -> Bool {
        reseedRows.contains(dest.destinationId)
    }

    /// Open the restore confirm. A no-op while a restore runs or when no row
    /// offers the gesture — the keyboard/registry backstop behind the paint.
    public func armReseed() {
        guard !reseedRunning, !reseedRows.isEmpty else { return }
        reclaiming = false
        reseedConfirming = true
    }

    public func cancelReseed() {
        reseedConfirming = false
    }

    /// The confirmed restore. Nothing about the ceremony's order is decided
    /// here: the side that hosts the store runs the shared ceremony and its
    /// post-ceremony re-enrollment (`CustodianStoreAccess.reseedCustodianStore`),
    /// and this only paints how it ended, as linux's `render_reseed` does.
    public func confirmReseed() async {
        guard let custodianStore, !reseedRunning else { return }
        reseedConfirming = false
        reseedRunning = true
        errorMessage = nil
        reseedResultText = L.backups.backupReseedRunning
        defer { reseedRunning = false }
        let result: FfiReseedResult
        do {
            result = try await custodianStore.reseedCustodianStore()
        } catch {
            reseedResultText = nil
            errorMessage = L.backups.backupReseedFailed(reason: mapError(error) ?? "\(error)")
            return
        }
        applyReseed(result)
        // Re-list only when nothing is left to say: the re-enrolled row appears
        // and the orphaned verdict is re-measured against it. A re-enrollment
        // failure keeps its message, which a successful reload would clear.
        if result.stopped == nil, result.reenrollError == nil {
            await hydrate()
        }
    }

    /// Paint a finished or stopped restore. Internal so the unit tests can pin
    /// the three endings without a store.
    func applyReseed(_ result: FfiReseedResult) {
        if let reason = result.stopped {
            reseedResultText = nil
            errorMessage = L.backups.backupReseedFailed(reason: reason)
            return
        }
        reseedResultText = result.resultLines.map(renderLocalizedTextNested).joined(separator: "\n")
        // The data IS back; say so on the result, and say what did not happen.
        errorMessage = result.reenrollError.map { L.backups.backupReseedReenrollFailed(reason: $0) }
    }

    /// Re-derive the orphaned-store verdict — on mount and after every mutation,
    /// **never on a render path** (see `orphanedStoreBytes`).
    ///
    /// Both halves come from shared Rust and neither is re-implemented here: the
    /// footprint is this device's own store (the agent's on macOS, this
    /// process's on iOS — `CustodianStoreAccess`), and the join is
    /// `custodianStoreIsOrphaned` over the destination list already loaded.
    /// Deliberately **not** `custodianAssignmentFor(..) == nil`, which answers
    /// `nil` for two rows naming this device and would offer to delete live
    /// custody.
    ///
    /// Every failure path lands on `nil` — no seam configured, no device id, a
    /// refusing agent, a read that threw — because the row it paints arms a
    /// destructive gesture and *cannot tell* must never offer to delete the
    /// owner's only offline copy.
    ///
    /// Internal rather than private so the unit tests can drive the verdict
    /// directly: every production caller of it sits behind a live nest call, and
    /// a rule this destructive must be provable without one.
    func refreshOrphanedStore() async {
        guard let custodianStore, let deviceId, !deviceId.isEmpty else {
            orphanedStoreBytes = nil
            return
        }
        guard let info = try? await custodianStore.custodianStoreFootprint() else {
            orphanedStoreBytes = nil
            return
        }
        let orphaned = custodianStoreIsOrphaned(
            destinations: destinations, thisDeviceId: deviceId, storeHoldsBytes: info.bytes > 0)
        orphanedStoreBytes = orphaned ? info.bytes : nil
    }

    /// The `backup-orphaned-store-row` sentence, two-level like `usageText`: the
    /// `{held}` slot takes the ALREADY-resolved byte size, so the unit localizes
    /// before it lands inside the sentence. The shared
    /// `fauna_core::format::orphaned_store_label` is the one source of that copy
    /// across the apps — this composes nothing of its own.
    public func orphanedStoreText(held: UInt64) -> String {
        let display = orphanedStoreLabel(heldBytes: held)
        return renderLocalizedText(display.label)
            .replacingOccurrences(of: "{held}", with: renderLocalizedText(display.held))
    }

    // MARK: - Helpers

    /// The row's human label: `display_name`, else the destination URL's host
    /// (a destination saved without a name carries `nil`). Wraps the shared
    /// `backupDestinationLabel` (`fauna_core::format::backup_destination_label`),
    /// the one source of truth across the six apps (priorities #1/#2;
    /// `backups.md` § Where logic lives) — the native twin of linux
    /// `destination_label`. Locale-agnostic (a host is not localized), so it stays
    /// FFI-side rather than going through i18n.
    public func label(for dest: FfiBackupDestinationView) -> String {
        backupDestinationLabel(
            displayName: dest.displayName, destinationNestUrl: dest.destinationNestUrl)
    }

    /// The current label for the destination with `id`, re-read from the LIVE
    /// `destinations` list. The in-process automation row registers its read
    /// closure once (`.onAppear`); a *rename* keeps the same `ForEach` identity
    /// (`destinationId` unchanged), so the row updates in place WITHOUT re-firing
    /// `.onAppear` — a closure that captured the value-type `dest` would then read
    /// the stale pre-rename `display_name`. Capturing only the stable `id` and
    /// re-reading here keeps the registry read fresh (the same by-id pattern the
    /// per-row status reads use via `statuses[dest.destinationId]`). Empty string
    /// if the id is gone (the row is unregistering). `backups.md` § destination
    /// management.
    public func label(forId id: String) -> String {
        guard let dest = destinations.first(where: { $0.destinationId == id })
        else { return "" }
        return label(for: dest)
    }

    /// `backup-destination-kind-badge` text for a row — every row carries one,
    /// regardless of kind. Wraps the shared `backupDestinationKindLabel`, the
    /// SAME `LocalizedText` the add dialog's `kindOptions` catalog uses, so the
    /// two can never drift apart.
    public func kindBadgeText(for dest: FfiBackupDestinationView) -> String {
        renderLocalizedText(backupDestinationKindLabel(kind: dest.kind))
    }

    /// Whether `dest` is the client-device kind — never a hard-coded
    /// `"client-device"` literal.
    public func isClientDeviceKind(_ dest: FfiBackupDestinationView) -> Bool {
        dest.kind == destinationKindClientDevice()
    }

    /// The row the remove-confirm dialog is armed for, if it is still in the
    /// list — what decides whether that dialog offers the reclaim opt-in.
    public var removingDestination: FfiBackupDestinationView? {
        removingId.flatMap { id in destinations.first { $0.destinationId == id } }
    }

    /// `backup-destination-remove-reclaim-checkbox`'s render rule: is THIS row
    /// one of the owner's own devices? The shared
    /// `fauna_core::data::row_is_a_client_device`, uniform with linux's
    /// `set_visible(is_client_device)` — **not** `isClientDeviceKind`, which
    /// reads the kind string alone: a `client-device` row with no
    /// `custodian_device_id` is not a client device, because nothing can drive
    /// it, and offering to free a copy no device is keeping is offering nothing.
    public func rowIsAClientDevice(_ dest: FfiBackupDestinationView) -> Bool {
        destinationRowIsAClientDevice(destination: dest)
    }

    /// `backup-destination-usage` text for a row — client-device rows only
    /// (the caller gates on `isClientDeviceKind(_:)`). Held bytes vs. the
    /// user-set cap; **cap-reached is read from the status's `cap_state`,
    /// never inferred from `held >= cap`** — a pull pass that stops at its cap
    /// ends *below* it, so inference would render a stalled backup as
    /// healthy-with-room.
    public func usageText(for dest: FfiBackupDestinationView) -> String {
        Self.usageText(statuses[dest.destinationId], capacityCapBytes: dest.capacityCapBytes)
    }

    static func usageText(_ status: FfiBackupDestinationStatus?, capacityCapBytes: UInt64?) -> String {
        let display = backupUsageLabel(
            heldBytes: status?.heldBytes, capacityCapBytes: capacityCapBytes,
            capState: status?.capState)
        var label = renderLocalizedText(display.label)
        if let held = display.held {
            label = label.replacingOccurrences(of: "{held}", with: renderLocalizedText(held))
        }
        if let cap = display.cap {
            label = label.replacingOccurrences(of: "{cap}", with: renderLocalizedText(cap))
        }
        return label
    }

    /// `backup-destination-last-upload-time` text for a row: the live status's
    /// `last_upload_time` rendered through the shared relative-time formatter, or
    /// the "never" baseline when there is no upload yet (or no read yet).
    public func lastUploadText(for dest: FfiBackupDestinationView) -> String {
        Self.lastUploadText(statuses[dest.destinationId])
    }

    /// `backup-destination-backlog-count` text for a row: the live backlog count
    /// ("N queued"), defaulting to 0 when there is no read yet.
    public func backlogText(for dest: FfiBackupDestinationView) -> String {
        Self.backlogText(statuses[dest.destinationId])
    }

    /// Per-row status → i18n text mapping (the client-glue half of
    /// `backups.md` § Per-destination status read), via the shared
    /// `backupLastUploadLabel` FFI fn (`value-formatting.md` § Backup
    /// destination status labels) — the never-vs-real decision, the epoch-0
    /// guard (a `0` timestamp now correctly renders "never", not the 1970
    /// epoch), and the seconds→ms conversion all live once in shared Rust.
    /// The label's `{when}` placeholder is filled by resolving `when` (an
    /// already-computed `RelativeTimeDisplay`) through `ValueFormat.render`
    /// — no second FFI round-trip. Static + pure so the non-e2e unit tests
    /// pin the mapping without a live FFI read.
    static func lastUploadText(_ status: FfiBackupDestinationStatus?) -> String {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let display = backupLastUploadLabel(lastUploadSecs: status?.lastUploadTime, nowMs: nowMs)
        var label = renderLocalizedText(display.label)
        if let when = display.when, let t = status?.lastUploadTime {
            label = label.replacingOccurrences(
                of: "{when}", with: ValueFormat.render(when, fallbackMs: Int64(t) * 1000))
        }
        return label
    }

    /// Backlog-count → i18n text mapping via the shared `backupBacklogLabel`
    /// FFI fn. `nil` ⇒ "0 queued".
    static func backlogText(_ status: FfiBackupDestinationStatus?) -> String {
        renderLocalizedText(backupBacklogLabel(backlogCount: status?.backlogCount))
    }

    /// `backup-destination-last-audit-time` text for a row: when this
    /// destination last **passed** the client-side audit, or the "never"
    /// baseline before the first pass (or before the first read).
    public func lastAuditText(for dest: FfiBackupDestinationView) -> String {
        Self.lastAuditText(auditRows[dest.destinationId])
    }

    static func lastAuditText(_ row: FfiDestinationAuditRow?) -> String {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let display = backupLastAuditLabel(lastPassedSecs: row?.lastPassedAt, nowMs: nowMs)
        var label = renderLocalizedText(display.label)
        if let when = display.when, let t = row?.lastPassedAt {
            label = label.replacingOccurrences(
                of: "{when}", with: ValueFormat.render(when, fallbackMs: Int64(t) * 1000))
        }
        return label
    }

    /// `backup-audit-alert` banner texts for a row — one per reason the shared
    /// pass yields (`alertReasons`: the standing verdict AND the fifth reason's
    /// open recovery window can both hold at once), empty for a healthy
    /// destination (the banners render only then — `ui/backups.md` §
    /// Audit-alert surface).
    public func alertTexts(for dest: FfiBackupDestinationView) -> [String] {
        guard let row = auditRows[dest.destinationId] else { return [] }
        return Self.alertTexts(row, destinationLabel: label(for: dest))
    }

    static func alertTexts(_ row: FfiDestinationAuditRow, destinationLabel: String) -> [String] {
        row.alertReasons.map {
            renderLocalizedText(backupAuditAlertLabel(reason: $0, destinationLabel: destinationLabel))
        }
    }

    /// `backup-destination-last-audit-time` text for a **client-device
    /// custodian** row — the custodian's own last **passed** self-audit,
    /// via the shared `backupSelfAuditLabel`, a separate door from
    /// `lastAuditText` all the way down (`ui/backups.md` § Audit-alert
    /// surface → *The client-device arm*): the owner-side loop and a
    /// custodian's self-report answer the same question from opposite sides
    /// of the trust line. Read from the **status row** (`lastAuditPassedAt`),
    /// never `auditRows` — a custodian has no address for this client's own
    /// audit pass to reach, so it carries no entry there.
    public func selfAuditText(for dest: FfiBackupDestinationView) -> String {
        Self.selfAuditText(statuses[dest.destinationId])
    }

    static func selfAuditText(_ status: FfiBackupDestinationStatus?) -> String {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let display = backupSelfAuditLabel(lastPassedSecs: status?.lastAuditPassedAt, nowMs: nowMs)
        var label = renderLocalizedText(display.label)
        if let when = display.when, let t = status?.lastAuditPassedAt {
            label = label.replacingOccurrences(
                of: "{when}", with: ValueFormat.render(when, fallbackMs: Int64(t) * 1000))
        }
        return label
    }

    /// `backup-destination-last-audit-time` cell text for a row, dispatched
    /// on the row's kind (mirrors linux's `audit_cell_text`): a
    /// client-device row carries its own self-audit; every other kind keeps
    /// the owner-side independent check this client itself performed.
    public func auditCellText(for dest: FfiBackupDestinationView) -> String {
        Self.auditCellText(dest, status: statuses[dest.destinationId], auditRow: auditRows[dest.destinationId])
    }

    /// Free-function-shaped twin of the instance wrapper above (mirrors
    /// linux's `audit_cell_text`) — pure data, directly unit-testable
    /// without seeding a live VM's `private(set)` maps.
    static func auditCellText(
        _ dest: FfiBackupDestinationView,
        status: FfiBackupDestinationStatus?,
        auditRow: FfiDestinationAuditRow?
    ) -> String {
        dest.kind == destinationKindClientDevice() ? selfAuditText(status) : lastAuditText(auditRow)
    }

    /// `backup-audit-alert` banner texts for client-device destinations whose
    /// own last self-audit is loud, via the shared `backupSelfAuditIsAlerting`
    /// predicate — the *only* failure signal that exists for a kind the
    /// owner-side loop can never sample (`ui/backups.md` § Audit-alert
    /// surface → *The client-device arm*). Absence and an unrecognised
    /// reported value both stay quiet, decided once in shared Rust. A
    /// **separate text list**, not a second `ForEach` keyed on
    /// `destinationId` alongside `alertText`'s — a destination could in
    /// principle carry both an owner-side and a self-reported reason, and
    /// two `ForEach`s over the same id would collide.
    public func selfReportedAlertTexts() -> [String] {
        Self.selfReportedAlertTexts(destinations: destinations, statuses: statuses) { label(for: $0) }
    }

    /// Free-function-shaped twin of the instance wrapper above (mirrors
    /// linux's `self_reported_alert_destinations`) — pure data, no
    /// `@Observable` state, so it is directly unit-testable without seeding
    /// a live VM's `private(set)` maps.
    static func selfReportedAlertTexts(
        destinations: [FfiBackupDestinationView],
        statuses: [String: FfiBackupDestinationStatus],
        label: (FfiBackupDestinationView) -> String
    ) -> [String] {
        destinations.compactMap { dest in
            guard dest.kind == destinationKindClientDevice(),
                  backupSelfAuditIsAlerting(auditState: statuses[dest.destinationId]?.auditState)
            else { return nil }
            return renderLocalizedText(
                backupAuditAlertLabel(reason: .selfReported, destinationLabel: label(dest)))
        }
    }

    /// Map an FFI error to a user string; the edit-different-nest sentinel becomes
    /// the localized guidance, everything else routes through the shared
    /// `DisplayError` mapping (never this boundary's own debug shape).
    /// `internal` (not `private`) so the non-e2e unit tests can pin the
    /// sentinel→i18n mapping.
    func mapError(_ error: Error) -> String? {
        if let ffi = error as? FfiError, case let .General(msg) = ffi,
           msg == Self.editDifferentNestToken {
            return L.backups.backupDestinationEditDifferentNest
        }
        // Defensive: tolerate the token arriving wrapped in any other error shape.
        if "\(error)".contains(Self.editDifferentNestToken) {
            return L.backups.backupDestinationEditDifferentNest
        }
        return DisplayError.message(error)
    }
}
