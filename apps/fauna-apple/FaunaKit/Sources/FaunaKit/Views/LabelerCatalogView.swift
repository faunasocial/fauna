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
                    inspectPanel(inspecting)
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

    /// The inspect-before-subscribe detail (`labeler-inspect-panel`), rendered
    /// only while `snapshot.inspecting` is `Some` — transcribes the decoded,
    /// re-verified signed `AlgorithmLabeler` metadata (never the raw WASM
    /// bytes). Mirrors linux `render_inspect_panel`'s field order. For a
    /// `list` artifact, additionally renders the decoded publisher-chosen
    /// name + the EXACT id→score map (content-moderation-and-ranking.md
    /// § Tier-3 artifact kinds: "the client renders the exact id→score map
    /// before subscribing" — a capped preview is not the exact map).
    private func inspectPanel(_ view: LabelerInspectView) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(
                Ids.labelerInspectMetadata,
                "labeler_id: \(view.labelerId)\nversion: \(view.version)\n"
                    + "artifact_kind: \(view.artifactKind)\n"
                    + "wasm_hash: \(view.wasmHash)\nwasm_size: \(view.wasmSize)\n"
                    + "needs_text: \(view.needsText)\nneeds_hashtags: \(view.needsHashtags)\n"
                    + "needs_media_metadata: \(view.needsMediaMetadata)\nneeds_author: \(view.needsAuthor)\n"
                    + "needs_attachment_bytes: \(view.needsAttachmentBytes)\n"
                    + "verified: \(view.verified)"
            )
            .font(.caption)

            if view.artifactKind == "list" {
                // An unnamed list is valid (pre-name artifacts) — say so
                // rather than rendering an empty label.
                automationText(Ids.labelerInspectListName,
                                view.listName.map { L.labelerCatalog.listName(name: $0) }
                                    ?? L.labelerCatalog.unnamedList)
                    .font(.caption)
                automationText(Ids.labelerInspectListEntryCount,
                                L.labelerCatalog.listEntryCount(count: "\(view.listEntries.count)"))
                    .font(.caption)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(view.listEntries.enumerated()), id: \.element.contentId) { offset, entry in
                        HStack(spacing: 8) {
                            automationText(Ids.labelerInspectListEntryId, entry.contentId)
                                .font(.caption.monospaced())
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            // The publisher's per-mille verbatim — the same
                            // number the publish sheet showed; no rescale
                            // anywhere in the chain.
                            automationText(Ids.labelerInspectListEntryScore, "\(entry.score)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.labelerInspectListEntry)
                        .automationValue(Ids.labelerInspectListEntry, text: { entry.contentId })
                        .automationScope(Ids.labelerInspectListEntry, index: offset)
                    }
                }
                .accessibilityIdentifier(Ids.labelerInspectListEntries)
                .automationValue(Ids.labelerInspectListEntries, text: { "\(view.listEntries.count)" })
            }

            if view.artifactKind == "text-model" {
                // An unnamed model is valid, same rule as the List's unnamed
                // case. The name rides inside the artifact, so inspect is
                // where it first becomes visible.
                automationText(Ids.labelerInspectModelName,
                                view.modelName.map { L.labelerCatalog.modelName(name: $0) }
                                    ?? L.labelerCatalog.unnamedModel)
                    .font(.caption)
                automationText(Ids.labelerInspectModelNgramCount,
                                L.labelerCatalog.modelNgramCount(count: "\(view.modelNgrams.count)"))
                    .font(.caption)
                // The FULL vocabulary, never a top-N — this is what a
                // subscriber is being asked to trust (content-moderation-
                // and-ranking.md § Tier-3 artifact kinds). Direction and
                // count are the SAME shared faces the publisher's own review
                // rows use, so the two ends cannot disagree about what they mean.
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(view.modelNgrams.enumerated()), id: \.element.ngram) { offset, ngram in
                        HStack(spacing: 8) {
                            automationText(Ids.labelerInspectModelEntryText, ngram.ngram)
                                .font(.caption.monospaced())
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Spacer()
                            automationText(
                                Ids.labelerInspectModelEntryDirection,
                                renderLocalizedText(FaunaFFISwift.ngramDirectionLabel(more: ngram.more, less: ngram.less))
                            )
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            automationText(
                                Ids.labelerInspectModelEntryCount,
                                renderLocalizedText(FaunaFFISwift.ngramDocCountLabel(more: ngram.more, less: ngram.less))
                            )
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.labelerInspectModelEntry)
                        .automationValue(Ids.labelerInspectModelEntry, text: { ngram.ngram })
                        .automationScope(Ids.labelerInspectModelEntry, index: offset)
                    }
                }
                .accessibilityIdentifier(Ids.labelerInspectModelEntries)
                .automationValue(Ids.labelerInspectModelEntries, text: { "\(view.modelNgrams.count)" })
            }

            Button(L.labelerCatalog.closeInspect) { vm.closeInspect() }
                .accessibilityIdentifier(Ids.labelerInspectCloseButton)
                .automationActivate(Ids.labelerInspectCloseButton) { vm.closeInspect() }
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.labelerInspectPanel)
        .automationValue(Ids.labelerInspectPanel, text: { "shown" })
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
                    .accessibilityIdentifier(Ids.labelerCatalogItemSubscribeButton)
                    .automationActivate(Ids.labelerCatalogItemSubscribeButton) {
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
