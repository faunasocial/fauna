// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The shared "Community labelers" Settings sub-page (macOS + iOS, one
/// FaunaKit view) — browse every published tier-3 labeler with
/// inspect-before-subscribe + subscribe/unsubscribe
/// (`docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3
/// community models). Reached from the Personalization home's
/// `personalization-browse-catalog-button`. Reference: linux
/// `apps/fauna-linux/src/views/personalization/mod.rs`.
public struct LabelerCatalogView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = LabelerCatalogVM()

    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.labelerCatalog.title)
                    .font(.title2)

                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                if let inspecting = vm.snapshot?.inspecting {
                    LabelerInspectPanel(view: inspecting, onClose: { vm.closeInspect() })
                }

                // `loaded &&`, not `entries.isEmpty` alone: "nobody has published
                // a labeler" is a claim only a read that RETURNED can make, and
                // an in-flight first read has an empty `entries` too
                // (`docs/goal/ui/README.md` § List pages: loading is not empty).
                if loaded, entries.isEmpty {
                    automationText(Ids.labelerCatalogEmpty, L.labelerCatalog.empty)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(indexedEntries) { pair in
                            LabelerCatalogRow(
                                entry: pair.entry, index: pair.index,
                                showInspectSubscribe: true, vm: vm)
                        }
                    }
                    .accessibilityIdentifier(Ids.labelerCatalogList)
                    // A bare `.accessibilityIdentifier` is invisible to the
                    // in-process driver — only `automation*` modifiers
                    // register; presence read is the row count.
                    .automationValue(Ids.labelerCatalogList, text: { "\(entries.count)" })
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(Ids.labelerCatalog)
        .automationValue(Ids.labelerCatalog, text: { "" })
        .pageTitle(L.labelerCatalog.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    private var entries: [LabelerCatalogEntry] { vm.snapshot?.entries ?? [] }

    /// Whether the catalog read has RESOLVED — the second painting condition of
    /// `labeler-catalog-empty`. Two ways to be unloaded and both must suppress
    /// the empty state: no snapshot yet (the `.task` refresh is still in
    /// flight), and a snapshot whose only read FAILED — which is why this reads
    /// the machine's own `loaded` rather than `vm.snapshot != nil`.
    private var loaded: Bool { vm.snapshot?.loaded ?? false }
    private var indexedEntries: [IndexedLabelerEntry] {
        entries.enumerated().map { IndexedLabelerEntry(index: $0.offset, entry: $0.element) }
    }
}

/// Pairs a `LabelerCatalogEntry` with its index in the FULL, unfiltered
/// `snapshot.entries` — the machine's `inspect`/`subscribe`/`unsubscribe`
/// gestures are index-addressed into that full list, so a filtered view
/// (Personalization's subscribed-only facet) must preserve original indices.
struct IndexedLabelerEntry: Identifiable {
    let index: Int
    let entry: LabelerCatalogEntry
    var id: Int { index }
}

/// One `labeler-catalog-item` row (indexed), shared by both the
/// Personalization home and the Community-labelers catalog page.
/// `showInspectSubscribe` gates the inspect + subscribe affordances
/// (labeler-catalog page only — ui.yaml `labeler-catalog-item`); unsubscribe
/// is always shown so a subscribed row can be dropped from either page.
/// Mirrors linux `build_labeler_row`.
struct LabelerCatalogRow: View {
    let entry: LabelerCatalogEntry
    let index: Int
    let showInspectSubscribe: Bool
    let vm: LabelerCatalogVM

    var body: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                automationText(Ids.labelerCatalogItemPublisher, entry.publisherActor)
                automationText(Ids.labelerCatalogItemContentKind, entry.contentKind)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                automationText(Ids.labelerCatalogItemVersion, "\(entry.version)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                automationText(Ids.labelerCatalogItemFactor, entry.factor)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // Distinguishes a curated List from an executable wasm module
                // BEFORE inspect (content-moderation-and-ranking.md § Tier-3
                // artifact kinds; ID user-approved 2026-07-16). Mirrors linux
                // `build_labeler_row`. The "needs a newer app" override (the
                // unknown-version contract's says-so half, ID 2026-08-13) is
                // the shared face's call, never re-derived here: `nil` means
                // paint the raw discriminator unchanged.
                automationText(
                    Ids.labelerCatalogItemKind,
                    FaunaFFISwift.textModelNeedsNewerApp(
                        artifactKind: entry.artifactKind, artifactVersion: entry.artifactVersion
                    ).map(renderLocalizedText) ?? entry.artifactKind
                )
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if showInspectSubscribe {
                Button(L.labelerCatalog.inspect) {
                    Task { await vm.inspect(index: index) }
                }
                .accessibilityIdentifier(Ids.labelerCatalogItemInspectButton)
                .automationActivate(Ids.labelerCatalogItemInspectButton) {
                    Task { await vm.inspect(index: index) }
                }
                if !entry.subscribed {
                    Button(L.labelerCatalog.subscribe) {
                        Task { await vm.subscribe(index: index) }
                    }
                    .disabled(!vm.subscribeEnabled(index: index))
                    .accessibilityIdentifier(Ids.labelerCatalogItemSubscribeButton)
                    .automationActivate(
                        Ids.labelerCatalogItemSubscribeButton,
                        isEnabled: { vm.subscribeEnabled(index: index) }
                    ) {
                        Task { await vm.subscribe(index: index) }
                    }
                }
            }
            if entry.subscribed {
                Button(L.labelerCatalog.unsubscribe) {
                    Task { await vm.unsubscribe(index: index) }
                }
                .accessibilityIdentifier(Ids.labelerCatalogItemUnsubscribeButton)
                .automationActivate(Ids.labelerCatalogItemUnsubscribeButton) {
                    Task { await vm.unsubscribe(index: index) }
                }
            }
        }
        .padding(.vertical, 4)
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.labelerCatalogItem)
        .automationValue(Ids.labelerCatalogItem, text: { entry.labelerId })
        .automationScope(Ids.labelerCatalogItem, index: index)
    }
}
#endif // !FAUNA_KIDS
