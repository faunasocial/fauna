import SwiftUI

/// The `media-item-detail` surface (opened by `media-item` tap/open) + its
/// `file-version-history` component — shared macOS + iOS FaunaKit renderer
/// (`docs/goal/ui/media.md` § User actions / § Element IDs, approved
/// 2026-07-09; semantics `docs/goal/behavior/file-sync.md` § File Versions /
/// § Restore).
///
/// An **inline `@State`-driven overlay** (not a `.sheet`/`.popover`) so it
/// stays in the in-process automation tree (`apple-e2e-automation.md`
/// registration rule 3) — mirrors the `SnapshotImmediateDeleteModal` inline-
/// reveal idiom; windows lifted the same surface as an inline-Border sheet.
///
/// Restore is reversible (an ordinary `modify` re-pointing the file at the
/// historical manifest, appending a new version — `file-sync.md` § Restore),
/// so the confirm is a single lightweight modal, not a destructive-action
/// ceremony. All semantics live in the shared `MediaMachine`; this view is a
/// pure renderer + the device-id glue already threaded by `MediaMachineVM`
/// (mirrors linux `detail.rs` / web `+page.svelte`'s per-item detail).
struct MediaItemDetailView: View {
    let vm: MediaMachineVM
    let item: MediaItemSummary
    /// The active **followed public folder** scope's opaque option value, or
    /// `nil` in the caller's own browse. A followed item's detail offers
    /// download alone — no version list, no show-pruned toggle, no restore, no
    /// delete — because the public plane is head-only by the follow's v1
    /// non-goals and the scope is read-only structurally (`ui/media.md`
    /// § Followed public folders). Same gate web, linux and tui apply. The value
    /// is handed back verbatim to the keyless `downloadFollowed`, so a scope
    /// switch mid-download fails loudly instead of reading the wrong nest.
    let followedScope: String?
    let onClose: () -> Void

    private var followed: Bool { followedScope != nil }

    @State private var versions: [FileVersionSummary] = []
    @State private var loading = true
    @State private var loadError: String?
    /// The recovery browse switch (`file-versions.md` § Retention (3), row
    /// 323) — ON re-lists with `includePruned`, so soft-pruned rows appear
    /// with their badge + undelete button.
    @State private var showPruned = false
    /// The version pending confirm — non-nil opens `file-version-restore-confirm-modal`.
    @State private var restoreTarget: FileVersionSummary?
    /// Armed by `media-delete-button` — while true the single
    /// `media-delete-confirm-modal` paints (and only then; an unarmed modal
    /// would let a driver confirm a delete the user never opened).
    @State private var deleteArmed = false
    /// In flight between the download press and the save — the button is
    /// disabled meanwhile so a double press cannot race two saves.
    @State private var downloading = false
    /// A failed download's `media.error_download` sentence, painted on this
    /// surface: the page banner sits behind the modal, where the user who asked
    /// could not read it (linux `detail.rs::download_to`'s reasoning).
    @State private var downloadError: String?

    /// The newest version row — rows are oldest→newest, so the last is the
    /// current file, and its manifest is what the download walk is keyed by.
    private var latest: FileVersionSummary? { versions.last }

    var body: some View {
        ZStack {
            Color.black.opacity(0.35)
                .ignoresSafeArea()
                .onTapGesture { onClose() }

            VStack(alignment: .leading, spacing: 12) {
                automationText(Ids.mediaItemDetailName, item.name)
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.middle)

                // Everything from here to the actions row is WITHHELD for a
                // followed item — absent from the tree, not merely inert, the
                // same withdrawal rule the upload affordance follows.
                if !followed {
                    Text(L.media.versionsTitle)
                        .font(.subheadline.bold())

                    Toggle(L.media.versionsShowPruned, isOn: Binding(
                        get: { showPruned },
                        set: { newVal in
                            showPruned = newVal
                            Task { await load() }
                        }
                    ))
                    .accessibilityIdentifier(Ids.fileVersionShowPrunedToggle)
                    .automationActivate(Ids.fileVersionShowPrunedToggle, value: { showPruned ? "on" : "off" }) {
                        showPruned.toggle()
                        Task { await load() }
                    }

                    Group {
                        if loading {
                            ProgressView()
                        } else if let loadError {
                            Text(loadError)
                                .font(.caption)
                                .foregroundStyle(.red)
                        } else {
                            versionList
                        }
                    }

                    if let restoreTarget {
                        restoreConfirm(restoreTarget)
                    }

                    if deleteArmed {
                        deleteConfirm
                    }
                }

                if let downloadError {
                    Text(downloadError)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack {
                    // Delete leads (destructive, leading edge) and Close trails —
                    // the same actions-row order linux's `detail.rs` draws. A
                    // followed item has nothing to delete: the scope is read-only
                    // structurally, so the button is absent rather than disabled.
                    if !followed {
                        Button(L.media.fileDetail.deleteFile) { deleteArmed = true }
                            .accessibilityIdentifier(Ids.mediaDeleteButton)
                            .automationActivate(Ids.mediaDeleteButton) { deleteArmed = true }
                    }
                    // `share-link-button` — absent unless the machine calls the
                    // file eligible (a followed scope's items never are); it
                    // opens the page-level create surface over this detail
                    // (`share-links.md` § Flows → Create).
                    ShareLinkDetailButton(vm: vm, item: item)
                    Spacer()
                    // `media-item-detail-download-button` — painted once it is
                    // actionable (a manifest known from the version rows, or at
                    // once in a followed scope), never inert (`ui/media.md`
                    // § Element IDs). Absent, not disabled, until then.
                    if followed || latest != nil {
                        Button(L.media.download) { Task { await download() } }
                            .disabled(downloading)
                            .accessibilityIdentifier(Ids.mediaItemDetailDownloadButton)
                            .automationActivate(Ids.mediaItemDetailDownloadButton) {
                                Task { await download() }
                            }
                    }
                    Button(L.media.detailClose) { onClose() }
                        .accessibilityIdentifier(Ids.mediaItemDetailCloseButton)
                        .automationActivate(Ids.mediaItemDetailCloseButton) { onClose() }
                }
            }
            .padding()
            .frame(maxWidth: 420)
            .background(.background)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .shadow(radius: 20)
            .padding()
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mediaItemDetail)
        // Presence anchor — `automation*` modifiers are the only surface the
        // in-process driver sees; a bare `.accessibilityIdentifier` is invisible
        // to it (`apple-e2e-automation.md`).
        .automationValue(Ids.mediaItemDetail, text: { item.name })
        .task(id: item.path) { await load() }
    }

    private var versionList: some View {
        VStack(spacing: 4) {
            ForEach(Array(versions.enumerated()), id: \.offset) { offset, version in
                versionRow(version)
                    .automationScope(Ids.fileVersionItem, index: offset)
            }
        }
        .accessibilityIdentifier(Ids.fileVersionList)
        .automationValue(Ids.fileVersionList, text: { "" })
    }

    private func versionRow(_ version: FileVersionSummary) -> some View {
        HStack {
            automationText(Ids.fileVersionTimestamp, Self.formatMillis(version.createdAt))
                .font(.caption)
            Spacer(minLength: 8)
            automationText(Ids.fileVersionSize,
                           ValueFormat.byteSize(UInt64(max(version.sizeBytes, 0))))
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            automationText(Ids.fileVersionAuthor, L.media.versionAuthor(author: version.authorDisplay))
                .font(.caption)
                .foregroundStyle(.secondary)
            // A soft-pruned row (only an includePruned listing carries one)
            // says so and offers its recovery verb — badge + undelete,
            // present ONLY on pruned rows (a live row never renders
            // either, the `snapshot-undelete-button` present-only-on-
            // recoverable shape).
            if version.pruned {
                Spacer(minLength: 8)
                automationText(Ids.fileVersionPrunedBadge, L.media.versionPrunedBadge)
                    .font(.caption)
                    .foregroundStyle(.orange)
                Button(L.media.versionUndelete) {
                    Task { await confirmUndelete(version) }
                }
                .accessibilityIdentifier(Ids.fileVersionUndeleteButton)
                .automationActivate(Ids.fileVersionUndeleteButton) {
                    Task { await confirmUndelete(version) }
                }
            }
            Button(L.media.versionRestore) { restoreTarget = version }
                .accessibilityIdentifier(Ids.fileVersionRestoreButton)
                .automationActivate(Ids.fileVersionRestoreButton) { restoreTarget = version }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.fileVersionItem)
        // Presence anchor for the indexed row (one Entry per version, so
        // `count("file-version-item")` is the version count).
        .automationValue(Ids.fileVersionItem, text: { "" })
    }

    /// The lightweight `file-version-restore-confirm-modal` — restore propagates
    /// to every device but is reversible (the pre-restore head stays restorable
    /// as its own version), so a single confirm, not a friction-bar ceremony.
    /// Inline reveal, appended below the version list (mirrors
    /// `SnapshotImmediateDeleteModal`'s placement idiom).
    private func restoreConfirm(_ version: FileVersionSummary) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.media.restoreConfirmTitle)
                .font(.headline)
            Text(L.media.restoreConfirmBody)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button(L.media.restoreCancel) { restoreTarget = nil }
                    .accessibilityIdentifier(Ids.fileVersionRestoreCancelButton)
                    .automationActivate(Ids.fileVersionRestoreCancelButton) { restoreTarget = nil }
                Button(L.media.restoreConfirm) {
                    Task { await confirmRestore(version) }
                }
                .accessibilityIdentifier(Ids.fileVersionRestoreConfirmButton)
                .automationActivate(Ids.fileVersionRestoreConfirmButton) {
                    Task { await confirmRestore(version) }
                }
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.fileVersionRestoreConfirmModal)
        .automationValue(Ids.fileVersionRestoreConfirmModal, text: { L.media.restoreConfirmTitle })
    }

    /// The `media-delete-confirm-modal` — a **single** confirm, deliberately not
    /// the backups typed-id immediate-delete ceremony: a media delete records a
    /// tombstone and leaves the historical version rows standing
    /// (`file-sync.md` § File Versions), and backup-mode destinations do not
    /// forward deletes at all. Same inline-reveal placement as
    /// `restoreConfirm` above (`media.md` § Element IDs, user-approved
    /// 2026-07-16).
    private var deleteConfirm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.media.fileDetail.deleteConfirmTitle)
                .font(.headline)
            // The body names the file through the shared `{name}`-arg string —
            // never a hand-assembled sentence (i18n placeholders are named).
            Text(L.media.fileDetail.deleteConfirm(name: item.name))
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                // Cancel is a PURE no-op — disarm, mutate nothing, surface no
                // error (the contract its sibling confirms hold; pinned by
                // `test_media_delete_cancel_is_a_no_op`).
                Button(L.common.cancel) { deleteArmed = false }
                    .accessibilityIdentifier(Ids.mediaDeleteCancelButton)
                    .automationActivate(Ids.mediaDeleteCancelButton) { deleteArmed = false }
                Button(L.media.fileDetail.deleteConfirmButton) {
                    Task { await confirmDelete() }
                }
                .accessibilityIdentifier(Ids.mediaDeleteConfirmButton)
                .automationActivate(Ids.mediaDeleteConfirmButton) {
                    Task { await confirmDelete() }
                }
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mediaDeleteConfirmModal)
        .automationValue(Ids.mediaDeleteConfirmModal,
                         text: { L.media.fileDetail.deleteConfirmTitle })
    }

    private func load() async {
        // A followed item has NO version history to load — the public plane is
        // head-only by the follow's v1 non-goals, so the read would be a
        // pointless round trip whose failure would paint an error on a detail
        // that is working exactly as designed. Web guards its own
        // `loadVersions` on the same condition, and tui skips the call at its
        // detail-open (`followed_scope_value.is_some()`).
        guard !followed else {
            loading = false
            return
        }
        loading = true
        loadError = nil
        do {
            versions = try await vm.fileVersions(
                folder: item.folder, path: item.path, includePruned: showPruned)
        } catch {
            loadError = DisplayError.message(error).map { L.media.versionsError(message: $0) }
        }
        loading = false
    }

    /// `media-item-detail-download-button`: run the shared download query, then
    /// hand the plaintext to the platform's save path, named after the file's
    /// basename (`SnapshotFileSaver.save` — the backups single-file download's
    /// seam, dialog-less under e2e). A followed scope reads keyless through
    /// `downloadFollowed`; anything else walks the LATEST version row's
    /// manifest through `downloadFile`. Mirrors linux `detail.rs::start_download`
    /// / `download_to`; a failure lands on this surface as `media.error_download`.
    private func download() async {
        guard !downloading else { return }
        let latest = self.latest
        guard followedScope != nil || latest != nil else { return }
        downloading = true
        downloadError = nil
        defer { downloading = false }
        do {
            let data: Data
            if let followedScope {
                data = try await vm.downloadFollowed(value: followedScope, relativePath: item.path)
            } else if let latest {
                data = try await vm.downloadFile(
                    manifestHash: latest.manifestHash,
                    contentKeyVersion: latest.contentKeyVersion,
                    folder: item.folder, relativePath: item.path)
            } else {
                return
            }
            try SnapshotFileSaver.save(suggestedFileName: Self.saveFileName(item.name), data: data)
        } catch {
            downloadError = DisplayError.message(error).map { L.media.errorDownload(message: $0) }
        }
    }

    /// The save file name for `name`: its last path component, so a name that
    /// carries a separator can never steer the write outside the chosen
    /// directory (linux `detail.rs::save_file_name`).
    static func saveFileName(_ name: String) -> String {
        let last = (name as NSString).lastPathComponent
        return last.isEmpty || last == "/" ? "download" : last
    }

    /// Recover a soft-pruned version — re-loads the (still
    /// `includePruned`) list afterward so the row's badge clears once the
    /// version is live again.
    private func confirmUndelete(_ version: FileVersionSummary) async {
        do {
            try await vm.undeleteVersion(path: item.path, versionNum: version.versionNum)
            await load()
        } catch {
            loadError = DisplayError.message(error).map { L.media.versionsError(message: $0) }
        }
    }

    /// Record the tombstone through the shared `MediaMachine::delete` (which
    /// repaints the outer page snapshot itself), then close this surface — the
    /// item it describes no longer exists, so leaving it open would render a
    /// detail for a row the explorer has already dropped (linux's
    /// `open_delete_confirm` closes its detail window on the same edge).
    private func confirmDelete() async {
        deleteArmed = false
        await vm.deleteItem(folder: item.folder, path: item.path)
        onClose()
    }

    private func confirmRestore(_ version: FileVersionSummary) async {
        restoreTarget = nil
        await vm.restoreVersion(folder: item.folder, path: item.path, version: version)
        // The restore lands via the shared machine (which repaints the outer
        // page snapshot on its own); re-load this surface's own version list so
        // the new head appears as the newest row.
        await load()
    }

    /// `FileVersionSummary.createdAt` is epoch **millis** (unlike
    /// `MediaItemSummary.updatedAt`, which is seconds — units trap).
    private static func formatMillis(_ epochMillis: Int64) -> String {
        ValueFormat.absoluteDate(epochMs: epochMillis, withTime: true)
    }
}
