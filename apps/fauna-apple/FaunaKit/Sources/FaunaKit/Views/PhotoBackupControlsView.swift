import SwiftUI
import Photos

/// Shared FaunaKit photo-backup surface (priority #2 — one source of truth for
/// both Apple targets). Renders the canonical `photo-backup-*` controls
/// (ui.yaml `photo-backup-controls`) over the shared `@Observable`
/// `PhotoBackupEngine`, read from the environment so the live `isBackingUp` /
/// `completedCount` / `lastBackupDate` updates drive the UI directly (no VM
/// copy-on-action staleness).
///
/// **Surfaced as (2026-06-28 sync/folder UI unification):** the "Photo Library"
/// Backup-mode set's configuration *inside* **Settings → Folders** (the shared
/// `FoldersContent`, an Apple `platform_elements` section — media.md § Apple
/// photo-backup reframe / folders.md § Apple photo-backup). PhotoKit is the set's
/// *ingress*; the Media page renders the unified cross-set explorer, not this view.
/// The former standalone macOS `photo-backup` Settings rail page and the iOS Media
/// tab / Status link are retired. The host app injects its `PhotoBackupEngine`
/// (`FaunaClient.photoBackup`) via `.environment(...)` and configures it at sign-in.
///
/// Photo backup is an **Apple-only** feature — it reads the device Photos library
/// through PhotoKit (`PHAsset`/`PHAssetResource`), which only exists on iOS/macOS.
/// web / Linux / Windows use the generic `file-upload` paradigm (media.md). The
/// unified cross-app media paradigm (photo-backup vs folders vs generic)
/// remains the gating decision in `docs/goal/ui/media.md`; this view is the
/// current Apple-clients media-ingress, not that resolution.
public struct PhotoBackupControlsView: View {
    @Environment(PhotoBackupEngine.self) private var engine: PhotoBackupEngine?

    /// The cross-platform "backup enabled" flag the host app reads at launch to
    /// decide whether to `startObserving()` (FaunaMacApp / FaunaClient).
    public static let enabledKey = "fauna.photoBackupEnabled"

    @State private var photoBackupEnabled = UserDefaults.standard.bool(forKey: PhotoBackupControlsView.enabledKey)
    @State private var photoAuthStatus: PHAuthorizationStatus = PHPhotoLibrary.authorizationStatus(for: .readWrite)
    @State private var isSyncing = false

    public init() {}

    public var body: some View {
        // Eager `ScrollView { VStack { GroupBox } }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): an iOS `Form` lazily
        // realizes its rows, so the status/actions sections (only shown once
        // backup is enabled) risk never `.onAppear`-registering below the fold —
        // the same class of gap fixed for `MailSpamView`. Mirrors the
        // `GroupBox { VStack }` idiom every `Admin*View`/`MailSettingsView` uses.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                enableSection
                if photoBackupEnabled {
                    statusSection
                    actionsSection
                }
                errorSection
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Enable / disable

    @ViewBuilder
    private var enableSection: some View {
        groupedSection(title: L.photoBackup.title) {
            Toggle(L.photoBackup.backUpPhotos, isOn: $photoBackupEnabled)
                .accessibilityIdentifier(Ids.photoBackupEnableToggle)
                // The activate flips the SAME `@State` the Toggle binds, so the
                // `.onChange` below — which is what actually requests Photos
                // access — runs identically for a driver tap and a human one.
                .automationActivate(
                    Ids.photoBackupEnableToggle,
                    value: { photoBackupEnabled ? "on" : "off" },
                    perform: { photoBackupEnabled.toggle() })
                .onChange(of: photoBackupEnabled) { _, enabled in
                    if enabled {
                        requestPhotoAccess()
                    } else {
                        UserDefaults.standard.set(false, forKey: Self.enabledKey)
                    }
                }

            switch photoAuthStatus {
            case .authorized, .limited:
                if photoBackupEnabled {
                    Label(L.photoBackup.photosAccessGranted, systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .font(.caption)
                }
            case .denied, .restricted:
                Label(L.errors.photosAccessDenied, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .font(.caption)
            default:
                EmptyView()
            }

            if photoBackupEnabled {
                Toggle(L.photoBackup.wifiOnly, isOn: .constant(true))
                    .disabled(true)
                    .accessibilityIdentifier(Ids.photoBackupWifiOnlyToggle)
                    // Pinned on today (`.constant(true)`) — so it publishes a
                    // value and its real disabled-ness, and no activate: a
                    // driver must read the same inertness a user sees.
                    .automationValue(
                        Ids.photoBackupWifiOnlyToggle,
                        value: { "on" },
                        isEnabled: { false })

                Text(L.photoBackup.autoUploadDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Status

    @ViewBuilder
    private var statusSection: some View {
        groupedSection(title: L.common.status) {
            if let engine {
                LabeledContent(L.photoBackup.backedUp) {
                    Text(L.photoBackup.photosCount(count: String(engine.completedCount)))
                }

                if engine.pendingCount > 0 {
                    LabeledContent(L.photoBackup.pendingCount(count: String(engine.pendingCount))) {
                        Text(L.photoBackup.remainingCount(count: String(engine.pendingCount)))
                    }
                }

                if engine.isBackingUp {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(progressText(engine))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityIdentifier(Ids.photoBackupSyncProgress)
                    // The published value is the SAME string the user reads, from
                    // the same function — so a driver that sees a position proves
                    // a user would have seen one (convention 1: never a shim that
                    // publishes what the UI does not show).
                    .automationValue(Ids.photoBackupSyncProgress, text: {
                        progressText(engine)
                    })
                }

                if let lastBackup = engine.lastBackupDate {
                    LabeledContent(L.photoBackup.lastBackup) {
                        Text(lastBackup, style: .relative)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityIdentifier(Ids.photoBackupLastSyncText)
                    // The one latency-independent post-condition of a completed
                    // pass: `lastBackupDate` is set only after `syncNewPhotos`
                    // returns, so this element's mere presence is the barrier a
                    // walk waits on (convention 14 — no settle sleep).
                    .automationValue(Ids.photoBackupLastSyncText, text: {
                        ValueFormat.absoluteDate(epochMs: Int64(lastBackup.timeIntervalSince1970 * 1000))
                    })
                }
            } else {
                Text(L.errors.photoBackupNotConfigured)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Actions

    @ViewBuilder
    private var actionsSection: some View {
        groupedSection {
            HStack {
                Button(L.photoBackup.syncNow) { syncNow() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(isSyncing || engine?.isBackingUp == true)
                .accessibilityIdentifier(Ids.photoBackupSyncNowButton)
                .automationActivate(
                    Ids.photoBackupSyncNowButton,
                    isEnabled: { !(isSyncing || engine?.isBackingUp == true) },
                    perform: { syncNow() })

                if isSyncing {
                    ProgressView().controlSize(.small)
                }
            }
        }
    }

    // MARK: - Error

    @ViewBuilder
    private var errorSection: some View {
        if let error = engine?.errorMessage {
            ErrorBanner(message: error)
        }
    }

    // MARK: - Helpers

    /// The in-flight pass's position, as the user reads it — "Syncing 3 of 5...",
    /// the catalog's own `photo_backup.syncing` (which had no caller on any app
    /// until 2026-09-20, while both rendered a bare indeterminate spinner).
    ///
    /// `ui/folders.md` § Photo backup promises that "while a backup pass runs, the
    /// phone shows how far it has got"; a spinner says only THAT one runs. The
    /// indeterminate wording survives for the one moment it is honest: before
    /// `PHAsset.fetchAssets` has answered there is genuinely no position yet.
    private func progressText(_ engine: PhotoBackupEngine) -> String {
        let progress = engine.passProgress
        guard progress.total > 0 else { return L.photoBackup.backupInProgress }
        return L.photoBackup.syncing(
            uploaded: String(progress.processed), total: String(progress.total))
    }

    /// Run one backup pass. Named rather than inline so the `Button` and its
    /// `.automationActivate` invoke the identical symbol and cannot drift.
    private func syncNow() {
        guard let engine else { return }
        isSyncing = true
        Task {
            _ = try? await engine.syncNewPhotos()
            isSyncing = false
        }
    }

    /// Prompt for Photos access; on grant, persist the enabled flag and start the
    /// engine immediately (observe new additions + an initial pass) so toggling on
    /// takes effect without waiting for the next launch.
    private func requestPhotoAccess() {
        PHPhotoLibrary.requestAuthorization(for: .readWrite) { status in
            Task { @MainActor in
                photoAuthStatus = status
                if status == .authorized || status == .limited {
                    UserDefaults.standard.set(true, forKey: Self.enabledKey)
                    engine?.startObserving()
                    if let engine { _ = try? await engine.syncNewPhotos() }
                } else {
                    photoBackupEnabled = false
                    UserDefaults.standard.set(false, forKey: Self.enabledKey)
                }
            }
        }
    }
}

// `groupedSection` (`GroupedSection.swift`) is the shared eager-container
// replacement for `Form`'s `Section` this file uses.
