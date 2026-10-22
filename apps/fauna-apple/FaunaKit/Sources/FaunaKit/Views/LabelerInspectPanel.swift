// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The inspect-before-subscribe detail (`labeler-inspect-panel`), shared by the
/// Community-labelers catalog page and the room settings editor (which opens it
/// IN PLACE so the staged draft survives an inspect) — one view, not two.
/// Rendered only while the catalog snapshot's `inspecting` is `Some`; transcribes
/// the decoded, re-verified signed `AlgorithmLabeler` metadata (never the raw
/// WASM bytes). Mirrors linux `render_inspect_panel`'s field order. For a `list`
/// artifact, additionally renders the decoded publisher-chosen name + the EXACT
/// id→score map (content-moderation-and-ranking.md § Tier-3 artifact kinds: "the
/// client renders the exact id→score map before subscribing" — a capped preview
/// is not the exact map).
struct LabelerInspectPanel: View {
    let view: LabelerInspectView
    /// `labeler-inspect-close-button` — the host closes its own machine's inspect.
    let onClose: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(
                Ids.labelerInspectMetadata,
                "labeler_id: \(view.labelerId)\nversion: \(view.version)\n"
                    + "artifact_kind: \(view.artifactKind)\n"
                    + "wasm_hash: \(view.wasmHash)\nwasm_size: \(view.wasmSize)\n"
                    + "needs_text: \(view.needsText)\nneeds_hashtags: \(view.needsHashtags)\n"
                    + "needs_media_metadata: \(view.needsMediaMetadata)\nneeds_author: \(view.needsAuthor)\n"
                    + "needs_attachment_bytes: \(view.needsAttachmentBytes)\n"
                    + "label_abi: \(view.labelAbi)\n"
                    + "verified: \(view.verified)"
                    // A revision newer than this app's own reads inert and says
                    // so — the same key the machine's refused subscribe names.
                    + (view.newerThanThisApp ? "\n" + L.labelerCatalog.inspectNeedsNewerApp : "")
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

            Button(L.labelerCatalog.closeInspect) { onClose() }
                .accessibilityIdentifier(Ids.labelerInspectCloseButton)
                .automationActivate(Ids.labelerInspectCloseButton) { onClose() }
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.labelerInspectPanel)
        .automationValue(Ids.labelerInspectPanel, text: { "shown" })
    }
}
#endif // !FAUNA_KIDS
