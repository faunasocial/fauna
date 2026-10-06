import SwiftUI

/// The armed deployment-identity rotation confirm (`admin-nest-seed-rotate-*`,
/// apps row 245). The roster read is a network round trip, so arming cannot
/// paint the listing synchronously; these are the three honest renderings of
/// that gap — an empty list is never one of them, because "nobody inherits"
/// and "we haven't asked yet" would look identical (`box-recovery.md` §
/// Deployment-seed rotation → *Ordering rule*). Mirrors tui's
/// `SeedRotateConfirm` (`apps/fauna-tui/src/admin/mod.rs`).
public enum SeedRotateConfirm {
    /// The roster read is in flight; the confirm button is present but disabled.
    case loading
    /// The roster read failed — the associated string is the page's reason line.
    case failed(String)
    /// The roster answered; the shared fold decides what is painted and
    /// whether the confirm may fire at all.
    case ready(FfiSeedRotationConfirmView)
}

// MARK: - Outside-app sign-in keys (`admin-nest-oauth-*`)
//
// The nest-held OAuth issuer key set and its refresh-token secret
// (authorization-server.md § The issuer → *Two rotation arms*; admin.md § N
// Nest owns the placement). Every sentence — a key row's line, the ordinary
// arm's cost, the armed confirm, all three verdicts — is a shared
// `fauna_client_admin` fold reached through the UniFFI face
// (`libs/fauna-ffi/src/admin.rs`); nothing here words one. Mirrors tui's
// `admin/mod.rs` (the lead app) through linux's GTK-free `OauthSectionState`.

/// The key set's read — the ONE precondition all three controls share. Key
/// rows paint only from `.ready`: "not asked yet" and "couldn't find out" get
/// the `admin-nest-oauth-key-reason` line, never an empty list that would read
/// as "no keys".
public enum OauthKeysRead {
    case unread
    case ready(FfiIssuerKeyView)
    /// Already worded (`admin.nest_page.oauth_keys_error`).
    case failed(String)
}

/// The armed forced confirm: which arm, and its cost AS FOLDED WHEN IT WAS
/// ARMED — never re-folded while armed (the seed-rotate discipline), so a
/// re-read landing between arming and confirming cannot change the number of
/// keys the admin already read would stop being accepted.
public struct OauthArmed {
    public let arm: FfiIssuerForcedArm
    public let confirm: FfiIssuerForcedConfirmView
}

/// The whole section's state in ONE value, so every transition is one
/// observable write — above all a dispatch's end, where the verdict, the
/// re-read key set and the released in-flight guard must appear together
/// (tui's single `Outcome::OauthDone { status, keys }`): the cross-app journey
/// reads the rows the moment the verdict stops saying "Working…". The four
/// gestures' guards live here, pure, so the view's `.disabled` and a
/// driver-forced press answer the same test.
public struct OauthSectionState {
    /// Its own result, never the page's `error-message`: any read failure
    /// must leave the rest of admin-nest painting.
    public internal(set) var keys: OauthKeysRead = .unread
    /// `admin-nest-oauth-confirm-*` — `nil` while un-armed; one arm at a time.
    public internal(set) var armed: OauthArmed?
    /// `admin-nest-oauth-status` — `nil` until a control was used. Never
    /// `error-message`: every success here has consequences worth words, and
    /// a failure must not claim nothing changed.
    public internal(set) var status: String?
    /// All three kinds mint on the nest, so every control desensitizes while
    /// one's call is out (the ordinary arm has no confirm to disarm).
    public internal(set) var inFlight = false

    public init() {}

    /// The key set, once it has answered.
    public var answeredView: FfiIssuerKeyView? {
        guard case .ready(let view) = keys else { return nil }
        return view
    }

    /// Whether the three controls are live — disabled, never hidden, otherwise.
    public var controlsLive: Bool { answeredView != nil && !inFlight }

    /// `admin-nest-oauth-rotate-button` — `true` when the ordinary rotation
    /// should dispatch. A forced confirm armed beside it named a key count
    /// this rotation is about to change, so it is disarmed rather than left
    /// stating a stale cost.
    mutating func pressRotate() -> Bool {
        guard controlsLive else { return false }
        armed = nil
        inFlight = true
        status = L.admin.nestPage.oauthWorking
        return true
    }

    /// `admin-nest-oauth-force-rotate-button` / `-secret-force-rotate-button`
    /// — arm `arm`'s confirm, dispatching nothing. The cost is folded NOW,
    /// over the set the admin can see; refused before the set has answered
    /// (the confirm could not name what it drops) and while a call is out.
    mutating func arm(_ arm: FfiIssuerForcedArm) {
        guard !inFlight, let view = answeredView else { return }
        status = nil
        armed = OauthArmed(arm: arm, confirm: FaunaFFISwift.issuerForcedConfirmView(arm: arm, view: view))
    }

    /// `admin-nest-oauth-cancel-button` — disarm, touching nothing.
    mutating func cancel() {
        armed = nil
    }

    /// `admin-nest-oauth-confirm-button` — the arm to dispatch, if any. `arm`
    /// is the one the pressed confirm was RENDERED for: a mismatch, or a call
    /// already out, dispatches nothing and keeps what the admin can see.
    /// Otherwise disarms FIRST — a double press must not fire a second forced
    /// rotation, which would drop the very key the first minted.
    mutating func pressConfirm(_ arm: FfiIssuerForcedArm) -> FfiIssuerForcedArm? {
        guard let current = armed, current.arm == arm, !inFlight else { return nil }
        armed = nil
        inFlight = true
        status = L.admin.nestPage.oauthWorking
        return current.arm
    }

    /// A key-set read landed on its own (page load). Replaces only the read:
    /// an armed confirm keeps the cost it captured.
    mutating func keysLoaded(_ keys: OauthKeysRead) {
        self.keys = keys
    }

    /// A control's call finished: its verdict and the key set re-read after
    /// it land in one write.
    mutating func done(status: String, keys: OauthKeysRead) {
        self.keys = keys
        self.status = status
        inFlight = false
    }
}

/// The armed legal-takedown confirm: the form the dispatch will send, plus the
/// decision surface AS FOLDED WHEN IT WAS ARMED — never re-folded while armed
/// (the seed-rotate / oauth discipline), so a field the admin keeps typing
/// after arming cannot silently change what the confirm already named. A
/// compulsory act is never confirmed blind (`moderation.md` § Legal takedown →
/// *Invocation surface*).
public struct TakedownArmed {
    public let contentId: String
    public let conversation: Bool
    public let reference: String
    public let restore: Bool
    /// `admin-nest-takedown-confirm-summary` — names the verb, the content and
    /// (for a takedown) the citation the dispatch will record.
    public let summary: String
    /// `admin-nest-takedown-confirm-button`'s label (the verb flips with restore).
    public let confirmLabel: String
}

/// The legal-takedown console's state in ONE value, the `OauthSectionState`
/// shape: every transition is one observable write, and both gestures' guards
/// live here, pure, so the view's `.disabled` and a driver-forced press answer
/// the same test. The form buffers live here too (tui's `AdminState::takedown_*`),
/// so the fold the arm control reads is never a stale copy captured by a
/// closure — `takedownFormView` is pure, so it is recomputed per read rather
/// than cached (linux's `refresh_takedown_arm`).
public struct TakedownSectionState {
    /// `admin-nest-takedown-content-id-input` — the content under the legal
    /// obligation. Presence-checked by the shared fold; the id's shape is the
    /// nest's to judge.
    public var contentId = ""
    /// `admin-nest-takedown-reference-input` — the legal-obligation reference
    /// (REQUIRED for a takedown, the optional overturn note on restore).
    public var reference = ""
    /// `admin-nest-takedown-type-{post,conversation}-radio` — the MLS
    /// relay-withhold kind when set, the serve-withhold half when not.
    public var conversation = false
    /// `admin-nest-takedown-restore-checkbox` — overturn mode.
    public var restore = false
    /// `admin-nest-takedown-confirm-*` — `nil` while un-armed.
    public internal(set) var armed: TakedownArmed?
    /// `admin-nest-takedown-status` — `nil` until an attempt was made.
    /// Deliberately NOT `error-message`: success is the common outcome and its
    /// consequences deserve words (a tombstone is served and the author can
    /// appeal; a restore leaves the record standing).
    public internal(set) var status: String?
    /// One compulsory act at a time — the controls desensitize while a
    /// dispatch is out.
    public internal(set) var inFlight = false

    public init() {}

    /// The live fold of the form — the shared `fauna_client_moderation::
    /// takedown_form_view` over UniFFI, which decides the arm control's label,
    /// its enablement and the reason it is withheld. Pure, so it is recomputed
    /// per read: no app grows its own gating or wording (priority #2).
    public var fold: FfiTakedownFormView {
        FaunaFFISwift.takedownFormView(
            contentId: contentId,
            conversation: conversation,
            legalReference: reference,
            restore: restore)
    }

    /// `admin-nest-takedown-button` — live only when the shared guard allows
    /// the submission (content id present; reference present unless restoring)
    /// and no dispatch is out.
    public var canArm: Bool { fold.canSubmit && !inFlight }

    /// `admin-nest-takedown-button` — capture the form AND its fold now and
    /// arm the confirm, dispatching nothing; clear any earlier verdict so a
    /// stale one cannot be read as this attempt's. Refused on exactly the test
    /// `canArm` renders, so a driver-forced press cannot arm what the shared
    /// guard refuses.
    mutating func arm() {
        let view = fold
        guard view.canSubmit, !inFlight else { return }
        status = nil
        armed = TakedownArmed(
            contentId: contentId,
            conversation: conversation,
            reference: reference,
            restore: restore,
            summary: renderLocalizedText(view.confirmSummary),
            confirmLabel: renderLocalizedText(view.confirmLabel))
    }

    /// `admin-nest-takedown-cancel-button` — disarm, touching nothing.
    mutating func cancel() {
        armed = nil
    }

    /// `admin-nest-takedown-confirm-button` — the form to dispatch, if any.
    /// Disarms FIRST: a double press must not dispatch a second compulsory
    /// act. The status goes to "Submitting…" in the same write, so the verdict
    /// a caller polls for can never be the previous attempt's.
    mutating func pressConfirm() -> TakedownArmed? {
        guard let current = armed, !inFlight else { return nil }
        armed = nil
        inFlight = true
        status = L.admin.nestPage.takedownWorking
        return current
    }

    /// The dispatch finished: its verdict and the released guard in one write.
    mutating func done(status: String) {
        self.status = status
        inFlight = false
    }
}

/// Shared view-model for the admin **`admin-nest`** page (admin.md § N Nest,
/// ratified 2026-06-04 per-page-services redesign), shared by macOS + iOS (one
/// FaunaKit VM). The Nest page is the home for nest-wide
/// settings that aren't a feature page:
///
///   - the admin **pairing** policy toggle (`admin-service-pairing-toggle` /
///     `-pairing-status`) — the one live service flag (gates `fauna.pair.add`),
///     over `fauna.admin.services.{list,update}` name `pairing` (moved off the
///     removed Services page), and
///   - the **Factory Reset** danger zone (`fauna.admin.factory_reset`, moved off
///     Settings) — the button + call live here; the re-onboard re-seed is the
///     platform shell's job (the view's `onFactoryReset` callback).
///
/// Reference renderer: linux (`views/admin.rs::build_nest_page` / `update_services`).
/// The `bridge`/`algorithm`/`dns` toggles were dropped (vestigial / residue / on
/// `admin-dns`); only `pairing` surfaces here. The storage-mode indicator was
/// retired with the no-modes cutover (Phase-4 S8.7) — every nest is sealed at
/// rest, so there is nothing left to display.
@MainActor @Observable
public final class AdminNestVM: BusyAdminCommandVM {
    /// The admin-set client-facing API serving port (`admin-nest-serving-port-input`),
    /// hydrated from `fauna.setup.status` `serving_port` (default 443). Applies on the
    /// next nest restart; the chosen value persists + reads back immediately.
    public private(set) var servingPort: UInt16 = 443
    /// Whether a Docker/cloud router fronts this nest (`fauna.setup.status`
    /// `fronted_by_router`). When `true` the serving port is fixed by the deployment
    /// (served on 443 by the router), so the serving-port field renders read-only —
    /// editing it is a genuine admin choice only on a direct-listener nest
    /// (nest/common.md § Serving ports). Default `false` = direct listener = editable.
    public private(set) var frontedByRouter = false
    /// The admin pairing flag (reflective — set only from a confirmed `services.list`).
    public private(set) var pairingEnabled = false
    /// The NAT-mode control's pending/selected mode (`admin-nest-nat-mode-{public,private}-radio`),
    /// pre-selected from `fauna.setup.status` `node_mode` by the shared `AdminNatModeMachine`
    /// (admin.md § Nest → NAT-mode control — the same seam + commit ceremony as the onboarding
    /// wizard's `nat_mode_choice` page).
    public private(set) var natMode: NodeMode = .public
    /// The `admin-nest-nat-mode-status` line — the machine's rendered snapshot message
    /// (idle/saved texts carry the live-vs-restart-applied caveat; submit/error states
    /// render their own `admin.nest_page.nat_mode_*` strings).
    public private(set) var natStatusText = ""
    /// Whether `admin-nest-nat-mode-save-button` should be enabled: true while choosing/
    /// erroring (resubmit always allowed), false only mid-submit. Stays true after a
    /// successful save too — the set is mutable, an immediate re-flip is allowed — so this
    /// is NOT folded into the page-level `isBusy` disable.
    public private(set) var natSaveEnabled = false
    /// Whether the NAT-mode radios should be disabled — true only mid-submit
    /// (mirrors the onboarding wizard's own `inFlight` gate on its sibling radios).
    public private(set) var natInFlight = false
    /// Host-OS pending-security-update count (`fauna.setup.status` `os_security_updates_pending`),
    /// backing the `nest-os-updates-count` badge (rendered only when `> 0` — the raw integer).
    /// A nest with no host channel reports 0 → no badge (installers/vps.md § Host OS Maintenance § 4).
    public private(set) var osSecurityUpdates: UInt32 = 0
    /// Whether the host has a pending reboot (`fauna.setup.status` `os_reboot_pending`),
    /// backing the `nest-os-restart-now-button` (rendered only when `true`).
    public private(set) var osRebootPending = false
    /// The always-present `nest-os-maintenance-status` line. The state→key decision is the
    /// shared `fauna_core::format::os_maintenance_status_label` (one source of truth for all
    /// six apps; do NOT hand-roll the map), resolved to the localized headline
    /// ("OS up to date" / "Security updates pending" / "Restart pending …").
    public var osMaintenanceStatusText: String {
        Self.osMaintenanceLabel(securityUpdates: osSecurityUpdates, rebootPending: osRebootPending)
    }
    /// The `admin-service-pairing-status` badge text (Enabled / Disabled).
    public var pairingStatus: String {
        pairingEnabled ? L.admin.servicesPage.enabled : L.admin.servicesPage.disabled
    }
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public internal(set) var isBusy = false

    /// The armed deployment-identity rotation confirm
    /// (`admin-nest-seed-rotate-confirm-button` + the roster rows), `nil`
    /// while un-armed. Mirrors tui's `SeedRotateConfirm` (`admin/mod.rs`) —
    /// three states, deliberately distinguished (`box-recovery.md` §
    /// Deployment-seed rotation → *Ordering rule*): the confirm's whole job
    /// is to say *who inherits* before dispatch, so "we don't know yet" and
    /// "we couldn't find out" must not both render as an empty list beside a
    /// live confirm button.
    public private(set) var seedRotateConfirm: SeedRotateConfirm?
    /// The rotation ceremony's own outcome line
    /// (`admin-nest-seed-rotate-status`), `nil` until one has been
    /// attempted. Held separately from `errorMessage` because the
    /// interesting outcomes are *successes with a caveat* (an unmarked
    /// predecessor), which the error surface would misreport.
    public var seedRotateStatus: String?

    /// Every rendering decision `admin-nest-region-*` needs
    /// (`admin-nest-region-status/-authority/-staleness/-withdraw-button`),
    /// already made by the shared `admin_region_view` fold (admin.md § N Nest
    /// → Declared region; region-blocking.md § Region determination). `nil`
    /// only before the first successful `hydrate()` — the View falls back to
    /// a local "no region declared" string for that gap, matching web/android.
    public private(set) var regionView: FfiAdminRegionView?

    /// The outside-app sign-in key section (`admin-nest-oauth-*`), one value
    /// so each transition is one write (`OauthSectionState`).
    public private(set) var oauth = OauthSectionState()

    /// The legal-takedown console (`admin-nest-takedown-*`), one value so each
    /// transition is one write (`TakedownSectionState`).
    public private(set) var takedown = TakedownSectionState()

    private var admin: FfiAdminClient?
    private var api: APIClient?
    private var natMachine: AdminNatModeMachine?

    public init() {}

    /// Vend the admin client + retain the APIClient (for the `fauna.setup.status`
    /// read) and load the page. Idempotent (client built once); always re-hydrates
    /// so a re-navigation refetches.
    public func configure(api: APIClient) async {
        self.api = api
        if admin == nil {
            do { admin = try await api.adminClient() }
            catch { errorMessage = DisplayError.message(error); return }
        }
        if natMachine == nil {
            // Non-fatal: a construction failure (no primed secret) leaves the
            // NAT-mode control at its default idle render rather than blocking
            // the rest of the page (pairing/serving-port/OS-maintenance).
            natMachine = try? api.adminNatModeMachine()
        }
        await hydrate()
        // Its own read, after and outside `hydrate()`'s one `do`: a throw
        // there blanks the whole page's error surface, and this one must not.
        oauth.keysLoaded(await readOauthKeys())
    }

    /// Re-read the serving port / host-OS maintenance state (`fauna.setup.status`)
    /// + the admin pairing flag (`fauna.admin.services.list`) + the NAT-mode
    /// control (`fauna.setup.status` `node_mode`, via the shared `AdminNatModeMachine`).
    public func hydrate() async {
        guard let admin, let api else { return }
        isBusy = true
        defer { isBusy = false }
        if let natMachine {
            await natMachine.hydrate()
            applyNatSnapshot(natMachine.snapshot())
        }
        do {
            let status = try await api.setupStatus()
            // Admin-set client-facing serving port (the chosen value, read straight
            // from the nest's `serving_port` singleton — immediate, even though the
            // live listener only rebinds on the next restart). A router-fronted nest
            // pins the external port, so the field renders read-only (see below).
            servingPort = status.servingPort
            frontedByRouter = status.frontedByRouter

            // Host-OS maintenance state (passive read off the same setup.status fetch).
            // Defaults (0/false) on a nest with no host channel → "OS up to date", no badge,
            // no button (installers/vps.md § Host OS Maintenance § 4).
            osSecurityUpdates = status.osSecurityUpdatesPending
            osRebootPending = status.osRebootPending

            // Admin pairing flag (reflective).
            let flags = try await admin.servicesList()
            pairingEnabled = flags.pairing

            // Declared region (`fauna.admin.region.get`) — folded onto the
            // same nav-edge read as serving-port/pairing above (admin.md § N
            // Nest → Declared region).
            regionView = try await api.adminRegionStatus()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Flip the admin pairing flag via `fauna.admin.services.update` name
    /// "pairing", then refetch so the toggle + status badge reflect persisted
    /// state (non-optimistic, mirroring linux `set_service` + `update_services`).
    public func setPairing(_ enabled: Bool) async {
        guard let admin else { return }
        await runAdminCommand { try await admin.servicesUpdate(name: "pairing", enabled: enabled) }
    }

    /// Radio click (`admin-nest-nat-mode-{public,private}-radio`) — a pure local
    /// selection, no network round trip (mirrors the onboarding wizard's own
    /// `selectNatMode`). Recovers from `Error`/`Done` back to `Choosing`; save
    /// stays enabled.
    public func selectNatMode(_ mode: NodeMode) {
        guard let natMachine else { return }
        natMachine.select(mode: mode)
        applyNatSnapshot(natMachine.snapshot())
    }

    /// Save (`admin-nest-nat-mode-save-button`): sign + commit the selected mode
    /// via the mutable `fauna.setup.nat_mode`. Kept independent of the shared
    /// `isBusy`/`hydrate()` cycle (unlike pairing/serving-port) — a re-hydrate
    /// would just re-fetch the same machine's own snapshot, and the machine
    /// already tracks its own in-flight state via `natInFlight`/`natSaveEnabled`.
    public func saveNatMode() async {
        guard let natMachine else { return }
        await natMachine.submit()
        applyNatSnapshot(natMachine.snapshot())
    }

    /// Mirror an `AdminNatModeMachine` snapshot onto the published NAT-mode
    /// fields (`admin-nest-nat-mode-*`).
    private func applyNatSnapshot(_ snap: NatModeSnapshot) {
        natMode = snap.selectedMode
        natStatusText = renderLocalizedText(snap.message)
        natSaveEnabled = snap.submitEnabled
        natInFlight = snap.state == .submitting
    }

    /// Set the client-facing API serving port via `fauna.admin.set_serving_port`
    /// (Admin-class), then refetch so the field reflects the persisted value
    /// (non-optimistic, mirroring `setPairing`). Applies on the next nest restart,
    /// but `fauna.setup.status` reports the chosen value immediately. The View
    /// validates the u16 range before calling this (invalid → `error-message`).
    public func setServingPort(_ port: UInt16) async {
        guard let admin else { return }
        await runAdminCommand { try await admin.setServingPort(port: port) }
    }

    /// Declare/re-declare (`region` already validated by
    /// `adminParseRegionCode`, the View's job) or withdraw (`region == nil`)
    /// via `fauna.admin.region.set`, then refetch so the section re-seeds
    /// from the persisted declaration (non-optimistic, mirroring
    /// `setServingPort`/`setPairing` — a re-declaration also retires the
    /// previous region's feature-policy document nest-side, so this is never
    /// a mere toggle). Mirrors web `+page.svelte`'s `setRegion` / android
    /// `AdminNestVM.setRegion`.
    public func setRegion(_ region: String?) async {
        guard let api else { return }
        await runAdminCommand { try await api.adminSetRegion(region) }
    }

    /// Expedite the host's idle-gated reboot via `fauna.admin.request_host_restart`
    /// (Admin-class), then refetch so the indicator reflects any change. The nest
    /// writes a `restart-requested` flag the host `fauna-reboot-coordinator` consumes
    /// on its next run; rejected `fauna.host_maintenance.no_host` on a nest with no
    /// maintenance mount → surfaced on the page `error-message` (mirrors web/android
    /// `restartNow` / linux `client.rs::request_host_restart`).
    public func restartNow() async {
        guard let admin else { return }
        await runAdminCommand { try await admin.requestHostRestart() }
    }

    /// `fauna.admin.factory_reset` — wipe deployment state, return the nest to
    /// unclaimed, and get the post-reset claim code (the human never sees it).
    /// Returns the code so the shell re-seeds onboarding at claim-code with it
    /// pre-filled (mail-bridge-lifecycle.md § Factory reset). On error, surfaces
    /// `factory_reset_failed` and returns nil (the nest is unchanged).
    ///
    /// **Ordering is the whole of gap CR-1** (`architecture/nest/common.md`
    /// § Client-state recoverability): mint + durably persist the claim code, *then*
    /// dispatch with it pinned. The code used to exist only in the synchronous reply,
    /// so a client killed between dispatch and reply-render lost it — the box landed
    /// fresh/unclaimed but nobody could claim it. `mintAndPersistPendingFactoryReset`
    /// writes the `(nest_url, handle, claim_code)` row, reads it back, and hands the
    /// code over only if it landed, so the crash-unsafe ordering is unrepresentable
    /// here. If it could not persist, we **refuse to dispatch**: wiping the box
    /// against a code nobody holds is unrecoverable, whereas refusing to start always
    /// is. A relaunch after a crash resumes the pre-filled claim from the slot — via the
    /// shared `LaunchMachine`'s `PendingFactoryReset` row, which `FaunaMacApp` /
    /// `FaunaApp` `runLaunch()` now route on (and whose CR-2 boot reconcile clears the
    /// slot instead if the box turns out to be healthy and still claimed).
    public func factoryReset() async -> String? {
        guard let admin else { return nil }
        isBusy = true
        defer { isBusy = false }

        // Source the resume target from the registry — the durable store the relaunch
        // will itself read, so the row we persist is the row the resume routes on.
        let keychain = KeychainStore()
        let material = FaunaAccounts.sessionMaterial(keychain: keychain)
        let nestUrl = material?.nestUrl ?? ""

        // The handle, however, must NOT come from the cache alone. The cached handle is a
        // *display* cache (the "Welcome back, @handle" line) whose write is best-effort —
        // every writer swallows the error, on the stated grounds that a failed cache write
        // "costs at most one stale line". That is true for a greeting and false here: the
        // mint's read-back guard rejects a record with an empty handle, so we would refuse
        // to dispatch, and an admin whose cache write once failed could then NEVER factory-
        // reset their box. A missing display cache must not cost an admin a recovery path.
        //
        // So ask the still-live session for the authoritative handle first, before the wipe,
        // and keep the cache only as the fallback. This is exactly what linux does
        // (`client.rs` — `AccountClient::get()`, then the cached handle); apple was the
        // client reading the cache alone (priority #4 — adopt the richer pattern).
        var handle = ""
        if let api, let reply = try? await api.getAccount(), let h = reply.handle, !h.isEmpty {
            handle = h
        }
        if handle.isEmpty {
            handle = material?.handle ?? ""
        }

        // Re-qualify the bare localpart to localpart@domain before the re-claim: the
        // nest stores bare localparts and auto-registers the primary mail domain from
        // the handle's @domain at claim, so a custom-domain box needs the qualified
        // handle (mail-bridge-lifecycle.md § Factory reset → re-claim handle
        // sourcing). The qualification rule + nest-URL-host parse live once in shared
        // Rust (linux/windows/android already adopt it) — never re-derive inline.
        handle = qualifyReclaimHandle(handle: handle, domain: material?.domain, nestUrl: nestUrl)

        guard let pinnedCode = mintAndPersistPendingFactoryReset(
            store: FaunaAccounts.launchPersistence(keychain: keychain),
            nestUrl: nestUrl,
            handle: handle
        ) else {
            // The Keychain silently dropped the row (a denial or a full disk — the
            // store cannot report either). Do NOT dispatch; the nest is untouched.
            errorMessage = L.admin.settingsPage.factoryResetPersistFailed
            return nil
        }

        do {
            // The nest honors a pinned code verbatim, so this is the code already in
            // the slot; return the reply's copy anyway so the nest stays the authority
            // on what the box booted with.
            return try await admin.factoryReset(newClaimCode: pinnedCode)
        } catch {
            errorMessage = L.admin.settingsPage.factoryResetFailed
            return nil
        }
    }

    /// Whether the serving-port field (`admin-nest-serving-port-*`) is editable:
    /// only on a direct-listener nest that isn't mid-request. A router-fronted nest
    /// pins the external port (served on 443 by the deployment), so the field is
    /// read-only there (nest/common.md § Serving ports); `isBusy` additionally
    /// locks it during a hydrate/save. The View gates `.disabled()` + the automation
    /// `isEnabled` on this. Pure rule (no FFI) so the gate is unit-testable.
    public var servingPortEditable: Bool {
        Self.servingPortEditable(isBusy: isBusy, frontedByRouter: frontedByRouter)
    }

    static func servingPortEditable(isBusy: Bool, frontedByRouter: Bool) -> Bool {
        !isBusy && !frontedByRouter
    }

    // MARK: - Deployment-seed rotation (`admin-nest-seed-rotate-*`, apps row 245)

    /// Open the rotation confirm (`admin-nest-seed-rotate-button`): arm at
    /// `.loading` immediately (the confirm renders disabled with zero rows,
    /// never absent), then read the roster.
    public func openSeedRotateConfirm() async {
        seedRotateConfirm = .loading
        seedRotateStatus = nil
        guard let api else { return }
        do {
            let view = try await api.seedRotateRoster()
            // Still-armed guard: a cancel that lands before this read
            // returns must not silently re-arm the confirm (mirrors linux's
            // guard on the same race / tui's `Outcome::SeedRotateRoster`
            // `is_some()` check).
            if seedRotateConfirm != nil {
                seedRotateConfirm = .ready(view)
            }
        } catch {
            if seedRotateConfirm != nil, let text = DisplayError.message(error) {
                seedRotateConfirm = .failed(L.admin.nestPage.rotateSeedRosterError(cause: text))
            }
        }
    }

    /// Cancel the armed confirm (`admin-nest-seed-rotate-cancel-button`).
    public func cancelSeedRotateConfirm() {
        seedRotateConfirm = nil
    }

    /// Confirm (`admin-nest-seed-rotate-confirm-button`): drive the
    /// ceremony. Honours `view.canConfirm`, not merely "the roster
    /// answered" — a resolved-but-empty roster is refused too. Disarms
    /// BEFORE dispatch (a double click must not chain a second rotation
    /// onto the first) and outlives its own click by design: the committed
    /// rotation tears down the box's serving generation, so the FFI call
    /// itself blocks through the reconnect (mirrors tui's
    /// `PageOp::outlives_click`) — this `Task` must not be cancelled by a
    /// view teardown or an automation reply budget.
    public func confirmSeedRotate() async {
        guard case .ready(let view) = seedRotateConfirm, view.canConfirm else { return }
        seedRotateConfirm = nil
        seedRotateStatus = L.admin.nestPage.rotateSeedWorking
        guard let api else { return }
        do {
            let result = try await api.rotateDeploymentSeed()
            seedRotateStatus = renderLocalizedText(result.verdict)
        } catch {
            if let text = DisplayError.message(error) {
                seedRotateStatus = L.admin.nestPage.rotateSeedFailed(cause: text)
            }
        }
    }

    // MARK: - Outside-app sign-in keys (`admin-nest-oauth-*`)
    //
    // Each gesture checks its guard and writes its state SYNCHRONOUSLY, before
    // any suspension — the view calls these directly, not inside a `Task` — so
    // the automation click's reply already shows the disarm (the journey
    // asserts the confirm gone right after the press) and a second press can
    // never slip between a check and its write.

    /// `fauna.oauth.issuer_key_status`, read and folded — or the worded reason
    /// it could not be. Never throws: a failure is the section's own line.
    private func readOauthKeys() async -> OauthKeysRead {
        guard let api else { return .unread }
        do {
            return .ready(try await api.adminIssuerKeyStatus())
        } catch {
            guard let text = DisplayError.message(error) else { return .unread }
            return .failed(L.admin.nestPage.oauthKeysError(cause: text))
        }
    }

    /// A dispatch's shared tail: re-read the key set FIRST, then publish the
    /// verdict, the re-read set and the released guard as one write, so the
    /// status keeps saying "Working…" until the rows beside it are current.
    private func finishOauthCall(_ verdict: String) async {
        let keys = await readOauthKeys()
        oauth.done(status: verdict, keys: keys)
    }

    /// `admin-nest-oauth-rotate-button` — the ordinary rotation, no confirm
    /// (nothing breaks; its cost is stated beside it).
    public func rotateIssuerKey() {
        guard let api, oauth.pressRotate() else { return }
        Task {
            let verdict: String
            do {
                verdict = renderLocalizedText(try await api.adminRotateIssuerKey())
            } catch {
                guard let text = DisplayError.message(error) else { return }
                verdict = L.admin.nestPage.oauthRotateFailed(cause: text)
            }
            await finishOauthCall(verdict)
        }
    }

    /// `admin-nest-oauth-force-rotate-button` / `-secret-force-rotate-button`.
    public func armOauthForced(_ arm: FfiIssuerForcedArm) {
        oauth.arm(arm)
    }

    /// `admin-nest-oauth-cancel-button`.
    public func cancelOauthForced() {
        oauth.cancel()
    }

    /// `admin-nest-oauth-confirm-button` — dispatch exactly `arm`, the arm the
    /// pressed confirm was rendered for (`OauthSectionState.pressConfirm`).
    public func confirmOauthForced(_ arm: FfiIssuerForcedArm) {
        guard let api, let arm = oauth.pressConfirm(arm) else { return }
        Task {
            let verdict: String
            do {
                verdict = renderLocalizedText(try await api.adminForceRotateIssuer(arm: arm))
            } catch {
                guard let text = DisplayError.message(error) else { return }
                verdict = L.admin.nestPage.oauthRotateFailed(cause: text)
            }
            await finishOauthCall(verdict)
        }
    }

    // ── Legal takedown (`admin-nest-takedown-*`; moderation.md § Legal
    //    takedown → *Invocation surface*) ─────────────────────────────────
    //
    // The legal-compulsion carve-out's admin trigger, never a policy lever.
    // Every sentence and every guard is the shared
    // `fauna_client_moderation::takedown` fold reached through UniFFI
    // (`takedownFormView` / `takedownVerdict`); this VM captures, dispatches
    // and publishes — it decides nothing (priority #2).

    /// `admin-nest-takedown-button` — arm the confirm with the fold captured
    /// NOW (a citation-less takedown is never armable; a note-less restore
    /// is), so a later dispatch sends exactly what the admin was shown.
    public func armTakedown() {
        takedown.arm()
    }

    /// The console's four form buffers. Written through `Binding`s the view
    /// hands its field/radio/checkbox controls, so a driver keystroke reaches
    /// the same storage a human's does.
    public func setTakedownContentId(_ value: String) { takedown.contentId = value }
    public func setTakedownReference(_ value: String) { takedown.reference = value }
    public func setTakedownConversation(_ value: Bool) { takedown.conversation = value }
    public func setTakedownRestore(_ value: Bool) { takedown.restore = value }

    /// `admin-nest-takedown-cancel-button` — disarm, touching nothing.
    public func cancelTakedown() {
        takedown.cancel()
    }

    /// `admin-nest-takedown-confirm-button` — dispatch the captured form over
    /// `fauna.moderation.legal_takedown`, then publish the shared verdict.
    /// The failure arm is the SAME fold with the error string, so a refusal
    /// never reads as "nothing changed" (`takedownVerdict`).
    public func confirmTakedown() {
        guard let api, let armed = takedown.pressConfirm() else { return }
        Task {
            var failure: String?
            do {
                _ = try await api.moderationClient().legalTakedown(
                    contentId: armed.contentId,
                    conversation: armed.conversation,
                    legalReference: armed.reference,
                    restore: armed.restore)
            } catch {
                // Never let a cancellation collapse `failure` back to nil here —
                // nil means the takedown SUCCEEDED (`takedownVerdict`'s contract),
                // and a legal takedown must never read as done when it wasn't.
                failure = DisplayError.message(error) ?? "\(error)"
            }
            takedown.done(status: renderLocalizedText(
                FaunaFFISwift.takedownVerdict(restore: armed.restore, error: failure)))
        }
    }

    /// Resolve the `nest-os-maintenance-status` headline via the shared
    /// `os_maintenance_status_label` (UniFFI free fn → `LocalizedText`), so the
    /// state→key map (reboot-pending > updates-pending > up-to-date) is single-sourced
    /// in shared Rust and can't drift per-app (priority #2). Module-qualified to
    /// stay consistent with other FFI-backed label resolvers on this VM. The raw
    /// count renders separately in `nest-os-updates-count`.
    static func osMaintenanceLabel(securityUpdates: UInt32, rebootPending: Bool) -> String {
        renderLocalizedText(FaunaFFISwift.osMaintenanceStatusLabel(
            securityUpdatesPending: securityUpdates, rebootPending: rebootPending))
    }
}
