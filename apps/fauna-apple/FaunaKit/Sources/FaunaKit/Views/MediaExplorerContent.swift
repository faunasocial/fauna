import SwiftUI

/// Shared (macOS + iOS) renderer for the **Media** page — the cross-set,
/// Windows-Explorer-style media browser (`docs/goal/ui/media.md`). The 2026-06-28
/// sync/folder UI unification (design tracked internally)
/// retired Apple's bespoke `PhotoBackupControlsView`: folders are the substrate,
/// and Media is the media-optimized **view** of them — a unified all-media browse
/// across every readable folder, with a per-set filter (§ Apple photo-backup
/// reframe; the photo-backup *controls* now live in Settings → Folders).
///
/// The page is the **content plane** (`media.md` rule 4 — it reads folders,
/// never configures them). It renders entirely off the shared observer-driven
/// `MediaMachine` (`libs/fauna-media-machine` via UniFFI), exactly as
/// `DevicesContent` renders off `DevicesMachine` — sort/filter run in shared Rust
/// (`MediaSnapshot::view`), so all 7 apps render one explorer off one surface.
/// The per-platform shells (`MediaSplitView`, iOS `MediaView`)
/// wrap this in their navigation chrome (`.pageTitle` → `page-heading`) and own
/// the refresh-on-appear.
///
/// Mirrors the linux UI lead (`apps/fauna-linux/src/views/media/`).
public struct MediaExplorerContent: View {
    let vm: MediaMachineVM

    /// Per-file sync state behind each row's `sync-state-badge`
    /// (`file-sync.md` § Per-file sync-status display). Apple is that section's
    /// ratified first consumer: the state comes from the in-process engine's own
    /// `SyncDb` through `FfiSyncEngineHost.fileStates`, already collapsed to the six
    /// display states. `nil` (no session / no host yet) ⇒ no badge at all, rather
    /// than a badge asserting a state we don't know.
    let syncStates: SyncStatesStore?

    /// The `media-folder-filter` value for the all-media default view (vs. a real
    /// set name). Mirrors the linux/web `FILTER_ALL_VALUE` sentinel — `__`-prefixed
    /// so it can't collide with a user set name (reserved `__*` sets are excluded
    /// from `fauna.media.list`; `media.md` § O-4). Same value the cross-app e2e
    /// driver uses (`tests/e2e-unified/actions/media.py` `MEDIA_FILTER_ALL`).
    static let filterAllValue = "__all__"

    /// The `file-upload` text input acting as a file picker (`media.md` § Layout &
    /// flow) — the local path the `upload-button` reads. The e2e driver
    /// `type_text("file-upload", path)`s into it.
    @State private var filePath = ""

    /// The `media-item-detail` surface's target — non-nil while open. Opened by
    /// `media-item` tap/open (`media.md` § User actions); an inline `@State`-driven
    /// overlay, never a `.sheet` (`apple-e2e-automation.md` registration rule 3).
    @State private var selectedItem: MediaItemSummary?

    public init(vm: MediaMachineVM, syncStates: SyncStatesStore? = nil) {
        self.vm = vm
        self.syncStates = syncStates
    }

    public var body: some View {
        ZStack {
            VStack(spacing: 0) {
                controlsBar
                Divider()
                itemsArea
            }
            .safeAreaInset(edge: .bottom) {
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                        .padding()
                }
            }

            if let selectedItem {
                MediaItemDetailView(
                    vm: vm,
                    item: selectedItem,
                    // A followed item's detail offers download alone: no version
                    // list, no show-pruned toggle, no restore, no delete — the
                    // public plane is head-only by the follow's v1 non-goals
                    // (`ui/media.md` § Followed public folders). Same gate web,
                    // linux and tui apply at their own detail-open.
                    followedScope: vm.snapshot?.followedScope?.value,
                    onClose: { self.selectedItem = nil })
            }

            // The share-link create / list / revoke surfaces, render-driven off
            // the machine snapshot and stacked above the detail whose
            // `share-link-button` opens the create one (`share-links.md` § Flows).
            ShareLinkSurfaces(vm: vm)
        }
        // Read the engine's per-file states for every set this page can show, once
        // the set list is known. Later transfers re-read through the host's observer
        // (`SyncStatesStore`), so the badges stay live without polling.
        .task(id: vm.snapshot?.folderOptions ?? []) {
            await syncStates?.refresh(folders: vm.snapshot?.folderOptions ?? [])
        }
        // A staged `fauna://…?action=versions` deep link (the FP context
        // action's Version history leaf) opens the target's `media-item-detail`
        // once the snapshot holds it — consume-on-match, so a still-loading
        // snapshot just retries on the next refresh (`MediaDeepOpen`).
        .task(id: vm.snapshot?.items ?? []) {
            if let match = MediaDeepOpen.shared.consume(matching: vm.snapshot?.items ?? []) {
                selectedItem = match
            }
        }
        // A `search-result-item` File-arm deep link (`ui/search.md` § Where
        // logic lives → Result navigation): actively resolve via `locate_file`
        // once the machine is configured, and again for every link staged
        // after that. The id is `-1` until the machine exists, then the
        // staging generation: a fresh mount fires on the machine arriving
        // (macOS remounts this page on every sidebar navigation,
        // `MediaSplitView`'s own doc comment), and a page already on screen —
        // iOS's search overlay leaves the Media tab mounted beneath it — fires
        // on the new generation.
        .task(id: vm.machine == nil ? -1 : MediaFileLocate.shared.generation) {
            if let match = MediaFileLocate.shared.consume(machine: vm.machine) {
                selectedItem = match
            }
        }
        // Re-read the cross-set aggregate on a fauna.sync.changed push while this
        // page is on screen — a file recorded by this device's own sync engine, a
        // second device, or a collaborator otherwise stays invisible until the
        // user navigates away and back (media.md § Staying live while the page is
        // open). One shared attachment covers both apple targets (the shells
        // `.pageTitle`-wrap this content but own no refresh triggers of their
        // own — `MediaSplitView`/`MediaView`'s own doc comments).
        .onMediaChanged { await vm.refresh() }
        // A dropped push (offline, or the socket flapped) is recovered on the
        // next reconnect — the apple twin of linux/tui's `StaleSurfaces::media`
        // reconnect sweep, which apple does not consume directly (`StaleSurfaces`
        // carries no UniFFI face yet, transport.md § Which surfaces a push
        // invalidates; only this page's own reconnect-refresh, not a fleet-wide
        // sweep adoption).
        .onReconnect { await vm.refresh() }
    }

    // MARK: - Explorer chrome

    @ViewBuilder
    private var controlsBar: some View {
        let snap = vm.snapshot
        // Two stacked rows, NOT one — a single `HStack` of every control (view /
        // sort / filter + the 140pt file field + upload) has a ~467pt minimum
        // width that overflows the 402pt iPhone width; SwiftUI then centers the
        // whole page off-screen, parking even the media rows below it at negative
        // x (the `media-item` count-0 the `/tree` debugDump pinned to this header,
        // not the row). Mirrors android `MediaScreen.kt`'s stacked control rows
        // (priority #3): browse controls on top, the file picker + upload below.
        // Each row fills the width and left-aligns via a trailing `Spacer`.
        VStack(spacing: 8) {
            HStack(spacing: 12) {
                viewToggle(snap)
                sortPicker
                sortDirectionPicker
                filterPicker(snap)
                Spacer(minLength: 0)
            }
            // A followed browse scope is READ-ONLY, structurally: the upload
            // affordance is ABSENT entirely rather than painted-but-inert,
            // because a follow never enters `known_folders` and so can never be
            // an upload target (`ui/media.md` § Followed public folders; tui
            // gates its own upload gesture on the same `followed_scope.is_none()`
            // and linux hides the whole row). ⚠ Withdrawing it means REMOVING
            // it from the tree, not disabling it — the e2e counts
            // `upload-button == 0`, and a disabled control still counts.
            HStack(spacing: 12) {
                if snap?.followedScope == nil {
                    TextField(L.media.chooseFile, text: $filePath)
                        .textFieldStyle(.roundedBorder)
                        .frame(minWidth: 140)
                        .accessibilityIdentifier(Ids.fileUpload)
                        .automationField(Ids.fileUpload, text: $filePath)
                    Button(L.media.upload) { submitUpload() }
                        .accessibilityIdentifier(Ids.uploadButton)
                        // In-process e2e actuation: fires the same `submitUpload()` the
                        // Button does, exercising the real upload path. No-op in production.
                        .automationActivate(Ids.uploadButton) { submitUpload() }
                }
                // "Shared links" — the page-level entry to the caller's share
                // links (`share-links.md` § Flows → List), after the upload
                // affordance in the ui.yaml page order. A page control, not an
                // upload one, so a followed scope keeps it.
                ShareLinkListButton(vm: vm)
                Spacer(minLength: 0)
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }

    /// `media-view-toggle` — a Button flipping list ↔ thumbnail grid; its label
    /// reflects the active mode (so a test can assert the flip). One `Entry`
    /// (actuate + read) via `automationActivate(value:)`.
    private func viewToggle(_ snap: MediaPageSnapshot?) -> some View {
        let grid = snap?.viewGrid ?? false
        return Button { toggleView() } label: {
            Label(grid ? L.media.viewGrid : L.media.viewList,
                  systemImage: grid ? "square.grid.2x2" : "list.bullet")
        }
        .accessibilityIdentifier(Ids.mediaViewToggle)
        .automationActivate(Ids.mediaViewToggle, value: { viewToggleLabel() }) { toggleView() }
    }

    /// `media-sort-select` — name / size / date, keyed on the stable wire value
    /// (not the localized label), so the cross-app `select(id, "name")` contract
    /// holds. Sort itself runs in shared Rust. The label text comes from the
    /// shared `fauna_core::format::media_sort_label` (UniFFI `mediaSortLabel(value:)`)
    /// so the value→label vocabulary can't drift per-app (linux/tui/web/android
    /// already delegate the same way).
    private var sortPicker: some View {
        Picker("", selection: sortBinding) {
            ForEach(["name", "size", "date"], id: \.self) { value in
                Text(renderLocalizedText(mediaSortLabel(value: value))).tag(value)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.mediaSortSelect)
        .automationSelect(Ids.mediaSortSelect,
                          value: { vm.snapshot?.sort ?? "name" },
                          set: { vm.setSort($0) })
    }

    /// `media-sort-direction` — ascending (default) / descending, applied to the
    /// active `media-sort-select` key. The ordering runs in shared Rust
    /// (`MediaSnapshot::view`'s `descending`), which already supported it before
    /// any client offered the control.
    ///
    /// This is load-bearing beyond user preference on THIS client: the macOS
    /// grid is a `LazyVGrid`, so an off-screen row registers neither its cell nor
    /// its name — making a newest-first order the difference between reading the
    /// latest files and silently missing them (media.md § Layout & flow).
    private var sortDirectionPicker: some View {
        Picker("", selection: sortDirectionBinding) {
            Text(renderLocalizedText(mediaSortDirectionLabel(descending: false))).tag(Self.directionAscending)
            Text(renderLocalizedText(mediaSortDirectionLabel(descending: true))).tag(Self.directionDescending)
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.mediaSortDirection)
        // String-backed selection: read/write the SAME `sortDirectionBinding`.
        .automationSelect(Ids.mediaSortDirection,
                          value: { sortDirectionBinding.wrappedValue },
                          set: { sortDirectionBinding.wrappedValue = $0 })
    }

    /// `media-folder-filter` — the all-media sentinel (default) + the readable
    /// sets that have media (rebuilt from the snapshot), then the **followed
    /// public folders** as browse scopes. A real set shows its own name (the
    /// stable e2e key); a followed scope shows the machine-minted label.
    private func filterPicker(_ snap: MediaPageSnapshot?) -> some View {
        Picker("", selection: filterBinding) {
            Text(L.media.filterAll).tag(Self.filterAllValue)
            ForEach(snap?.folders ?? [], id: \.self) { name in
                Text(name).tag(name)
            }
            // The followed browse scopes, AFTER the own-set options
            // (`ui/media.md` § Followed public folders). Both halves are
            // shared-Rust-minted: `value` is an opaque stable string guaranteed
            // disjoint from every set name, and `label` already carries the
            // owner disambiguator that tells a follow apart from a same-named
            // set of the user's own. Render them; never compose or parse
            // either. SwiftUI's `Text(label).tag(value)` carries both halves on
            // the option itself, so apple needs no value→label side map — the
            // shape GTK/Compose/WinUI legs do need, because their pickers' model
            // holds the values alone.
            ForEach(snap?.followed ?? [], id: \.value) { scope in
                Text(scope.label).tag(scope.value)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.mediaFolderFilter)
        .automationSelect(Ids.mediaFolderFilter,
                          value: { vm.snapshot?.filter ?? Self.filterAllValue },
                          options: { [Self.filterAllValue] + (snap?.folders ?? [])
                                     + (snap?.followed ?? []).map(\.value) },
                          set: { applyFilter($0) })
    }

    // MARK: - Items

    @ViewBuilder
    private var itemsArea: some View {
        let items = vm.snapshot?.items ?? []
        let grid = vm.snapshot?.viewGrid ?? false
        // `loaded` gates the empty state (shared `MediaPageSnapshot.loaded`): an
        // absent snapshot and a first read still in flight both have no items, and
        // "No media yet" over media that is about to arrive is wrong for the user
        // and the ambiguity the harness cannot see past. While unloaded the area
        // paints nothing, so the ABSENCE of `media-empty-state` beside zero
        // `media-item` rows is the loading state (`media.md` § Default view).
        if items.isEmpty {
            if vm.snapshot?.loaded == true {
                ContentUnavailableView(L.media.noMediaYet, systemImage: "photo.on.rectangle.angled")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier(Ids.mediaEmptyState)
                    .automationValue(Ids.mediaEmptyState, text: { L.media.noMediaYet })
            } else {
                Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else if grid {
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                    ForEach(Array(items.enumerated()), id: \.offset) { offset, item in
                        MediaItemCard(item: item, grid: true, fetchThumbnail: fetchThumbnail,
                                      syncState: syncState(for: item),
                                      onOpen: { selectedItem = $0 })
                            .automationScope(Ids.mediaItem, index: offset)
                    }
                }
                .padding()
            }
        } else {
            // Eager `ScrollView { VStack }`, not `List` (`apple-e2e-automation.md`
            // registration rule 6): `List` pools its rows to the visible window, so
            // an off-screen `media-item[N]` never registers and scoped reads 404.
            // The eager container registers every row — safe now that the row above
            // is width-bounded and no longer overflows/centers off-screen.
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(Array(items.enumerated()), id: \.offset) { offset, item in
                        MediaItemCard(item: item, grid: false, fetchThumbnail: fetchThumbnail,
                                      syncState: syncState(for: item),
                                      onOpen: { selectedItem = $0 })
                            .automationScope(Ids.mediaItem, index: offset)
                    }
                }
            }
        }
    }

    /// This item's display state. A file the local engine tracks reports its real
    /// state; one it doesn't (a set this device never bound — every set on iOS, and
    /// any unbound set on macOS) is on the nest only, which is exactly `.remoteOnly`
    /// in the six-state vocabulary (`file-sync.md` § Per-file sync-status display).
    /// With no session/host at all we render no badge rather than assert a state.
    private func syncState(for item: MediaItemSummary) -> SyncDisplayState? {
        guard let syncStates else { return nil }
        return syncStates.state(folder: item.folder, path: item.path) ?? .remoteOnly
    }

    // MARK: - Gestures (read `vm` live so the registry closures never go stale)

    private func viewToggleLabel() -> String {
        (vm.snapshot?.viewGrid ?? false) ? L.media.viewGrid : L.media.viewList
    }

    private func toggleView() {
        vm.setViewGrid(!(vm.snapshot?.viewGrid ?? false))
    }

    private var sortBinding: Binding<String> {
        Binding(get: { vm.snapshot?.sort ?? "name" }, set: { vm.setSort($0) })
    }

    /// The stable `media-sort-direction` wire values, mapped to the shared
    /// machine's boolean. Strings (not a bool) because the cross-app contract
    /// is `select(id, value)` and every other Media control keys on a stable
    /// option value.
    private static let directionAscending = "ascending"
    private static let directionDescending = "descending"

    private var sortDirectionBinding: Binding<String> {
        Binding(
            get: { (vm.snapshot?.descending ?? false) ? Self.directionDescending : Self.directionAscending },
            set: { vm.setDescending($0 == Self.directionDescending) }
        )
    }

    private var filterBinding: Binding<String> {
        Binding(get: { vm.snapshot?.filter ?? Self.filterAllValue }, set: { applyFilter($0) })
    }

    /// Map the `media-folder-filter` wire value to the machine filter: the
    /// all-media sentinel → `nil`, a real set name → that set.
    /// A followed scope's minted value routes to the async on-demand listing
    /// fetch (`ui/media.md` § Followed public folders); a set name routes to the
    /// ordinary filter.
    ///
    /// **The MACHINE says which values are followed** — this asks the snapshot's
    /// own `followed` list rather than parsing the value, because the value is
    /// opaque by contract and its shape is the machine's business (the same
    /// lookup tui's media page and web's `changeFilter` both make).
    private func applyFilter(_ value: String) {
        if let scope = (vm.snapshot?.followed ?? []).first(where: { $0.value == value }) {
            // The machine notifies on settle; the page repaints from the observer.
            Task { await vm.selectFollowedScope(scope.value) }
            return
        }
        vm.setFilter(value == Self.filterAllValue ? nil : value)
    }

    private func submitUpload() {
        let path = filePath
        Task { await vm.uploadFromPath(path) }
    }

    /// Lazy per-item `media-thumbnail` fetch, threaded into each `MediaItemCard`'s
    /// on-appear task (mirrors linux threading `machine`/`backup_key`/`runtime`
    /// into `build_media_item`). Reads `vm` live so the fetch hits the configured
    /// machine; returns `nil` on any error so the card keeps its placeholder.
    private func fetchThumbnail(_ hash: String) async -> Data? {
        await vm.fetchThumbnail(hash: hash)
    }
}

// MARK: - Media item (indexed `media-item` component)

/// One item in the cross-set Media explorer — the indexed `media-item` component
/// (`media-item-name` / `-size` / `-date` / `media-thumbnail` /
/// `media-source-status`). Mirrors the `DeviceCard` / `MacPostCardView` recipe
/// (`.accessibilityElement(children: .contain)` + a presence anchor; the
/// `.automationScope("media-item", index:)` lives on the `ForEach` row so scoped
/// child reads `media-item[i]/media-item-name` resolve). Cross-set aggregation +
/// sort + filter already ran in shared Rust — this is a pure renderer (`media.md`
/// rule 2). `grid` picks the tile (vertical, thumbnail-led) vs. list (a thumbnail
/// beside a vertically-stacked metadata column — mirrors android `MediaItemTile`,
/// so the row fits phone width) layout; both expose the same child ids.
private struct MediaItemCard: View {
    let item: MediaItemSummary
    let grid: Bool
    /// Lazy `media-thumbnail` fetch (owner-key derive → shared
    /// `MediaMachine::fetch_thumbnail`), threaded from the parent. Returns `nil`
    /// on any error → the card keeps its placeholder.
    let fetchThumbnail: (String) async -> Data?
    /// This file's presence state for the `sync-state-badge`; `nil` ⇒ render none.
    let syncState: SyncDisplayState?
    /// Tap/open → the `media-item-detail` surface (`media.md` § User actions).
    let onOpen: (MediaItemSummary) -> Void

    /// The decoded thumbnail once the lazy fetch resolves; `nil` = show the
    /// placeholder icon (no hash, still loading, or a fetch/decode failure).
    @State private var loadedThumbnail: FaunaPlatformImage?

    var body: some View {
        Button {
            onOpen(item)
        } label: {
            Group {
                if grid {
                    VStack(alignment: .leading, spacing: 4) {
                        thumbnail
                        nameLabel
                        sizeLabel
                        dateLabel
                        sourceStatus
                        syncStateBadge
                    }
                } else {
                    // List-row: thumbnail beside a vertically-stacked metadata
                    // column — mirrors android's `Row { thumbnail + Column(weight
                    // 1f){ name; size; date; source } }` (`MediaScreen.kt`
                    // `MediaItemTile`). A single horizontal line cannot fit all
                    // seven elements at iPhone width (402pt): only `nameLabel`
                    // truncates, so the non-truncating metadata (size + the long
                    // `.abbreviated`+`.shortened` date + source-status + sync-badge)
                    // plus the six 12pt gaps sum to ~455pt, and in an eager
                    // container SwiftUI centers that overflow off-screen (a `List`
                    // merely clips the trailing badges — the defect macOS users
                    // see). Stacking the metadata into a `maxWidth: .infinity`
                    // column bounds the row to the proposed width, so it renders at
                    // every width. `alignment: .top` keeps the thumbnail aligned to
                    // the name, not centered on the taller column.
                    HStack(alignment: .top, spacing: 12) {
                        thumbnail
                        VStack(alignment: .leading, spacing: 2) {
                            nameLabel
                            sizeLabel
                            dateLabel
                            sourceStatus
                            syncStateBadge
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mediaItem)
        // Row tap/open — ONE Entry per item (read + activate combined; a
        // separate `.automationValue` for the same id would split them across
        // two index slots, `apple-e2e-automation.md`).
        .automationActivate(Ids.mediaItem, value: { item.name }) { onOpen(item) }
    }

    /// `media-thumbnail` — the real thumbnail when the item carries a
    /// `thumbnail_hash` and it fetches+decodes, else a placeholder icon. The fetch
    /// is lazy (on appear, keyed on the hash) through the shared
    /// `MediaMachine::fetch_thumbnail` (GET direct-by-hash → content-address verify
    /// → owner-`BackupKey` decrypt — all shared Rust); a `None` hash or any
    /// fetch/decode error keeps the placeholder so one unreadable thumbnail never
    /// blanks the card (`media.md` § Thumbnails). Mirrors linux `build_media_item`.
    ///
    /// `/element/attr?attr=state` answers `painted` while the view holds decoded
    /// thumbnail bytes and `placeholder` while it shows the icon — read live off
    /// `loadedThumbnail`, the same state the branch below paints from, never a
    /// separately-kept flag. The strings are linux's `gtk::Image` read and
    /// `post-image`'s (`tests/e2e-unified/actions/media.py::thumbnail_kind`).
    private var thumbnail: some View {
        let side: CGFloat = grid ? 96 : 32
        return Group {
            if let loadedThumbnail {
                Image(platformImage: loadedThumbnail)
                    .resizable()
                    .scaledToFill()
                    .frame(width: side, height: side)
                    .clipped()
            } else {
                Image(systemName: "photo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: side, height: side)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityIdentifier(Ids.mediaThumbnail)
        .automationValue(
            Ids.mediaThumbnail,
            value: { item.thumbnailHash },
            attributes: { ["state": loadedThumbnail != nil ? "painted" : "placeholder"] }
        )
        .task(id: item.thumbnailHash) {
            loadedThumbnail = nil
            guard let hash = item.thumbnailHash else { return }
            if let data = await fetchThumbnail(hash) {
                loadedThumbnail = FaunaImage.decode(data)
            }
        }
    }

    private var nameLabel: some View {
        automationText(Ids.mediaItemName, item.name)
            .font(.headline)
            .lineLimit(1)
            .truncationMode(.middle)
    }

    private var sizeLabel: some View {
        // Shared 1024-unit, locale-invariant formatter (`fauna_core::format::byte_size`)
        // — the cross-app canonical linux/web/windows/android render `media-item-size`
        // with; never the native `ByteCountFormatter` (1000-based, locale-localized
        // separator) it was written to replace (`value-formatting.md`).
        automationText(Ids.mediaItemSize,
                       ValueFormat.byteSize(UInt64(max(item.sizeBytes, 0))))
            .font(.caption)
            .foregroundStyle(.secondary)
    }

    private var dateLabel: some View {
        automationText(Ids.mediaItemDate, Self.formatDate(item.updatedAt))
            .font(.caption)
            .foregroundStyle(.secondary)
    }

    /// `sync-state-badge` — where this file's bytes currently live, in the shared
    /// six-state vocabulary (`file-sync.md` § Per-file sync-status display). The
    /// **text is shared** (`syncDisplayStateLabel`, the `media.status_label.*` keys),
    /// only the icon + color are an idiomatic per-app render — so this cannot
    /// drift from the label linux/web/windows/android show.
    ///
    /// Distinct from `media-source-status` above, which is the *source folder's*
    /// liveness, not the file's presence.
    @ViewBuilder
    private var syncStateBadge: some View {
        if let syncState {
            HStack(spacing: 4) {
                Image(systemName: Self.badgeIcon(syncState))
                    .foregroundStyle(Self.badgeColor(syncState))
                    .font(.caption2)
                automationText(Ids.syncStateBadge,
                               renderLocalizedText(syncDisplayStateLabel(state: syncState)))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private static func badgeIcon(_ state: SyncDisplayState) -> String {
        switch state {
        case .synced: "checkmark.circle.fill"
        case .uploading: "arrow.up.circle.fill"
        case .downloading: "arrow.down.circle.fill"
        case .localOnly: "internaldrive.fill"
        case .remoteOnly: "cloud.fill"
        case .conflict: "exclamationmark.triangle.fill"
        }
    }

    private static func badgeColor(_ state: SyncDisplayState) -> Color {
        switch state {
        case .synced: .green
        case .uploading: .orange
        case .downloading: .blue
        case .localOnly: .yellow
        case .remoteOnly: .secondary
        case .conflict: .red
        }
    }

    /// Source folder online/offline liveness (distinct from a file's sync-state
    /// badge — `media.md` § Source status vs. sync state).
    private var sourceStatus: some View {
        HStack(spacing: 4) {
            Circle()
                .fill(item.sourceOnline ? .green : .red)
                .frame(width: 8, height: 8)
            automationText(Ids.mediaSourceStatus,
                           item.sourceOnline ? L.media.sourceOnline : L.media.sourceOffline)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    /// `updated_at` is unix **seconds**.
    private static func formatDate(_ epochSeconds: Int64) -> String {
        ValueFormat.absoluteDate(epochMs: epochSeconds * 1000, withTime: true)
    }
}
