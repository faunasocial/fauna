import SwiftUI

/// The shared "Personalization" Settings sub-page (macOS + iOS, one FaunaKit
/// view) — hubs the **Feeds** facet (a link exiting Settings to the Feed tab
/// / create-feed dialog), the **Muted words** facet (a link re-homing the
/// already-shipped `muted-words` sub-page — reused, not rebuilt), and the
/// caller's SUBSCRIBED **Community labelers** (rendered inline, each
/// unsubscribable, with a link out to the full labeler-catalog
/// browse/inspect/subscribe page).
/// `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3
/// community models. Reference: linux
/// `apps/fauna-linux/src/views/personalization/mod.rs`.
///
/// **Trained topics** (the sealed personal-topic-factor CRUD facet,
/// `topic-factors.md` § Authoring surface & picker) — the S8 apple lift,
/// mirroring windows' `TrainedTopicsViewModel`/`PersonalizationPage` (the
/// richest reference: one name-input, two flows — create vs. a row's
/// rename-button retargeting the same input) plus linux's engagement toggle
/// + "Clear activity data" affordance (windows landed before Layer A).
///
/// The navigation actions reach outside FaunaKit (top-level tab switch /
/// other Settings sub-pages), so they arrive as closures — mirrors
/// `PrivacySettingsView(onInboxModeChanged:)`.
public struct PersonalizationView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The app-root `FeedVM` instance (injected at `WindowGroup` on both
    /// platforms) — "Clear activity data" must reset the SAME live
    /// `FfiFeedManager` the Feed page observes, not a second instance
    /// (mirrors linux's `crate::feed::host::manager()` singleton).
    @Environment(FeedVM.self) private var feedVM: FeedVM
    @State private var vm = LabelerCatalogVM()
    @State private var trainedTopicsVM = TrainedTopicsVM()
    @State private var trainedTopicName = ""
    /// `nil` = the input + create-button add a new factor; non-nil = the
    /// input is retargeted at that row's rename (windows' one-input-two-flows
    /// shape). Reset on any successful commit.
    @State private var renamingTrainedFactorId: Data?

    /// Publish review-prune sheet (topic-factors.md § Publishing a trained
    /// factor; frame D8) — single instance, pre-targeted at the row whose
    /// publish-button opened it (the admin-dns-rename-sheet shape: an inline
    /// reveal, not a system `.sheet`). `publishGeneration` guards the async
    /// corpus-score read against landing after the sheet was closed or
    /// re-opened against a different factor (mirrors linux `Ctx.generation`).
    @State private var publishSheetOpen = false
    @State private var publishTargetId: Data?
    @State private var publishTargetFactorKey: String?
    /// The kind the open sheet reviews — the raw wire discriminator
    /// (`"list"` | `"text-model"`), never a translated label; List is the
    /// default (the weaker disclosure, `fauna_core::format::publish_kind_options`).
    @State private var publishKind = "list"
    @State private var publishExemplars: [ScoredExemplar] = []
    /// Keyed by `ScoredExemplar.postId` — every scored exemplar defaults to
    /// included (the restore-kind-checkbox prune shape: the user unchecks
    /// what they'd rather not endorse).
    @State private var publishIncluded: [String: Bool] = [:]
    /// Model kind: every surviving n-gram, ascending — the whole disclosure,
    /// not a top-N (`TrainedModelReview.ngrams`).
    @State private var publishNgrams: [ReviewNgram] = []
    /// Keyed by `ReviewNgram.ngram` — default included, same prune shape as
    /// `publishIncluded`.
    @State private var publishNgramIncluded: [String: Bool] = [:]
    /// The Model corpus-size facts the mandated copy states — carried
    /// UNSHRUNK through to publish regardless of what the review prunes.
    @State private var publishMoreDocs: UInt32 = 0
    @State private var publishLessDocs: UInt32 = 0
    @State private var publishIncludedExamples: UInt32 = 0
    @State private var publishMarkedExamples: UInt32 = 0
    @State private var publishName = ""
    @State private var publishGeneration = 0

    /// Layer-B opt-in state (engagement-cues.md § Layer B) — read/written
    /// directly over the live `FfiFeedManager` (the "Clear activity data"
    /// precedent), not a dedicated VM: `signalShareStatus`/`setSignalSharing`
    /// are manager methods, not `APIClient` wrappers. Non-optimistic:
    /// `setSignalShare` re-reads status after the set, so `signalShare` only
    /// ever reflects a nest-confirmed value.
    @State private var signalShare = false
    /// The nest-wide ≥k transparency export (signal:* + report:* aggregates) —
    /// exactly what a peer nest sees. Empty until hydrated or before any
    /// content clears the k floor.
    @State private var signalSharePublished: [FfiReportShareEntry] = []

    let onNavigateFeed: () -> Void
    let onNavigateMutedWords: () -> Void
    let onNavigateCatalog: () -> Void

    public init(
        onNavigateFeed: @escaping () -> Void,
        onNavigateMutedWords: @escaping () -> Void,
        onNavigateCatalog: @escaping () -> Void
    ) {
        self.onNavigateFeed = onNavigateFeed
        self.onNavigateMutedWords = onNavigateMutedWords
        self.onNavigateCatalog = onNavigateCatalog
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.personalization.title)
                    .font(.title2)

                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                Button(L.personalization.feedsLink) { onNavigateFeed() }
                    .accessibilityIdentifier(Ids.personalizationFeedsLink)
                    .automationActivate(Ids.personalizationFeedsLink) { onNavigateFeed() }

                Button(L.personalization.mutedWordsLink) { onNavigateMutedWords() }
                    .accessibilityIdentifier(Ids.personalizationMutedWordsLink)
                    .automationActivate(Ids.personalizationMutedWordsLink) { onNavigateMutedWords() }

                Text(L.labelerCatalog.title)
                    .font(.headline)

                // Same three-state gate as the catalog page's own empty state:
                // an in-flight read is not an empty subscription list
                // (`docs/goal/ui/README.md` § List pages: loading is not empty).
                if labelersLoaded, subscribedEntries.isEmpty {
                    automationText(Ids.personalizationLabelersEmpty, L.personalization.labelersEmpty)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(subscribedEntries) { pair in
                            LabelerCatalogRow(
                                entry: pair.entry, index: pair.index,
                                showInspectSubscribe: false, vm: vm)
                        }
                    }
                    .accessibilityIdentifier(Ids.personalizationLabelersList)
                    // A bare `.accessibilityIdentifier` is invisible to the
                    // in-process driver — only `automation*` modifiers
                    // register; presence read is the row count.
                    .automationValue(Ids.personalizationLabelersList, text: { "\(subscribedEntries.count)" })
                }

                Button(L.personalization.browseCatalog) { onNavigateCatalog() }
                    .accessibilityIdentifier(Ids.personalizationBrowseCatalogButton)
                    .automationActivate(Ids.personalizationBrowseCatalogButton) { onNavigateCatalog() }

                Text(L.personalization.trainedTopicsTitle)
                    .font(.headline)

                if let error = trainedTopicsVM.errorMessage {
                    ErrorBanner(message: error)
                }

                trainedTopicsList

                publishSheetSection

                HStack {
                    TextField(L.personalization.trainedFactorPlaceholder, text: $trainedTopicName)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.personalizationTrainedFactorNameInput)
                        .automationField(Ids.personalizationTrainedFactorNameInput, text: $trainedTopicName)

                    Button(renamingTrainedFactorId == nil
                           ? L.personalization.trainedFactorCreate
                           : L.personalization.trainedFactorSave) {
                        Task { await submitTrainedTopic() }
                    }
                    .accessibilityIdentifier(Ids.personalizationTrainedFactorCreateButton)
                    .automationActivate(Ids.personalizationTrainedFactorCreateButton) {
                        Task { await submitTrainedTopic() }
                    }
                }

                Button(L.personalization.clearEngagementData) {
                    Task { await clearEngagementData() }
                }
                .accessibilityIdentifier(Ids.personalizationClearEngagementDataButton)
                .automationActivate(Ids.personalizationClearEngagementDataButton) {
                    Task { await clearEngagementData() }
                }

                Text(L.personalization.shareSignalsTitle)
                    .font(.headline)

                Toggle(L.personalization.shareSignalsLabel, isOn: Binding(
                    get: { signalShare },
                    set: { on in Task { await setSignalShare(on) } }
                ))
                .accessibilityIdentifier(Ids.personalizationShareSignalsToggle)
                // Wire contract: "on"/"off" — the mail-spam report-share
                // toggle convention (NOT the bare-Switch "true"/"false" the
                // engagement toggle above uses); actions/personalization.py's
                // `signal_sharing_on` reads `get_attr(id, "state") == "on"`.
                .automationActivate(
                    Ids.personalizationShareSignalsToggle,
                    value: { signalShare ? "on" : "off" }
                ) {
                    Task { await setSignalShare(!signalShare) }
                }
                Text(L.personalization.shareSignalsSubtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                signalSharePublishedSection
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("personalization")
        .automationValue("personalization", text: { "" })
        .pageTitle(L.personalization.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
            trainedTopicsVM.configure(api: client.api)
            await trainedTopicsVM.load()
            await hydrateSignalShare()
        }
    }

    /// The Trained-topics facet's row container (topic-factors.md § Authoring
    /// surface & picker) — empty state or the row list, keyed by the factor's
    /// stable 16-byte id so a rename/reorder never rebinds the wrong row.
    @ViewBuilder private var trainedTopicsList: some View {
        Group {
            if trainedTopicsVM.rows.isEmpty {
                Text(L.personalization.trainedTopicsEmpty)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(trainedTopicsVM.rows.enumerated()), id: \.element.id) { index, row in
                        trainedTopicRow(row, index: index)
                    }
                }
            }
        }
        .accessibilityIdentifier(Ids.personalizationTrainedFactorList)
        .automationValue(Ids.personalizationTrainedFactorList, text: { "\(trainedTopicsVM.rows.count)" })
    }

    /// One trained-factor row: name + example count + the engagement toggle +
    /// rename/delete. `.contain` keeps the container id AND its child ids
    /// queryable together (the apple container-a11y rule — a bare container id
    /// alone would clobber the children, and the in-process driver only sees
    /// what `automation*` modifiers register).
    private func trainedTopicRow(_ row: FfiTrainedTopicRow, index: Int) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                automationText(Ids.personalizationTrainedFactorName, row.name)
                automationText(
                    Ids.personalizationTrainedFactorExampleCount,
                    L.personalization.trainedFactorExamples(count: "\(row.exampleCount)")
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()

            Toggle("", isOn: Binding(
                get: { row.learnFromEngagement },
                set: { newValue in Task { await trainedTopicsVM.setLearnFromEngagement(id: row.id, on: newValue) } }
            ))
            .labelsHidden()
            .help(L.personalization.trainedFactorEngagementToggle)
            .accessibilityIdentifier(Ids.personalizationTrainedFactorEngagementToggle)
            // The wire contract for this toggle is "true"/"false" (matching
            // linux's gtk::Switch, whose `state` attr is a generic bool
            // property read) — NOT the "on"/"off" idiom other apple toggles
            // use; `actions/personalization.py::engagement_toggle_on` already
            // checks `== "true"`.
            .automationActivate(
                Ids.personalizationTrainedFactorEngagementToggle,
                value: { row.learnFromEngagement ? "true" : "false" }
            ) {
                Task { await trainedTopicsVM.setLearnFromEngagement(id: row.id, on: !row.learnFromEngagement) }
            }

            Button(L.personalization.trainedFactorRename) {
                renamingTrainedFactorId = row.id
                trainedTopicName = row.name
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.personalizationTrainedFactorRenameButton)
            .automationActivate(Ids.personalizationTrainedFactorRenameButton) {
                renamingTrainedFactorId = row.id
                trainedTopicName = row.name
            }

            Button(L.personalization.trainedFactorPublish) {
                openPublishSheet(row)
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishButton)
            .automationActivate(Ids.personalizationTrainedFactorPublishButton) {
                openPublishSheet(row)
            }

            Button(role: .destructive) {
                Task { await trainedTopicsVM.delete(id: row.id) }
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.personalizationTrainedFactorDeleteButton)
            .automationActivate(Ids.personalizationTrainedFactorDeleteButton) {
                Task { await trainedTopicsVM.delete(id: row.id) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.personalizationTrainedFactorItem)
        // The row's `factor` test-attr — the minted `topic:<hex>` key's hex
        // suffix (`actions/personalization.py::topic_factor_key`). The
        // registry is sealed under the BackupKey, so no wire read can answer
        // this; it must come from the rendered row.
        .automationValue(Ids.personalizationTrainedFactorItem, value: { hexSuffix(row.factorKey) })
        .automationScope(Ids.personalizationTrainedFactorItem, index: index)
    }

    /// The Layer-B transparency pane ("What this nest publishes") — the
    /// nest-wide ≥k export view, byte-identical to the federation export.
    /// Read-only, pure transparency; mirrors `MailSpamView.reportSharePublishedSection`
    /// (the report-share sibling on the mail-spam page).
    @ViewBuilder private var signalSharePublishedSection: some View {
        Text(L.personalization.signalPublishedTitle)
            .font(.subheadline.weight(.semibold))
        Text(L.personalization.signalPublishedDescription)
            .font(.caption)
            .foregroundStyle(.secondary)
        if signalSharePublished.isEmpty {
            automationText(Ids.signalSharePublishedList, L.personalization.signalPublishedEmpty)
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(signalSharePublished.enumerated()), id: \.offset) { _, entry in
                    signalSharePublishedRow(entry)
                }
            }
            .accessibilityIdentifier(Ids.signalSharePublishedList)
            .automationValue(Ids.signalSharePublishedList, text: { "" })
        }
    }

    @ViewBuilder
    private func signalSharePublishedRow(_ entry: FfiReportShareEntry) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            // Display title combines count + label ("N contributors") — but the
            // -count test id carries the RAW number only, mirroring
            // report-share-published-list-item-count exactly (the cross-app
            // action layer's signal_published_contributor_count() asserts the
            // bare digit string).
            Text("\(entry.count) \(L.personalization.signalPublishedContributors)")
                .font(.headline)
            automationText(Ids.signalSharePublishedListItemHash, entry.contentHash)
                .font(.caption.monospaced())
                .textSelection(.enabled)
            HStack(spacing: 4) {
                automationText(Ids.signalSharePublishedListItemFactor, entry.factor)
                Text("·")
                automationText(Ids.signalSharePublishedListItemCount, "\(entry.count)")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.signalSharePublishedListItem)
        // Per-row presence entry so the flat in-process registry can `count`
        // published rows (mirrors report-share-published-list-item).
        .automationValue(Ids.signalSharePublishedListItem, text: { entry.contentHash })
    }

    /// The publish review-prune sheet (`personalization-trained-factor-publish-sheet`)
    /// — inline reveal, single instance, pre-targeted at the row whose
    /// publish-button opened it (the admin-dns-rename-sheet shape). Scores
    /// the loaded feed window through the SAME live `FfiFeedManager` the Feed
    /// page observes (mirrors linux's `crate::feed::host::manager()`
    /// singleton — never a fresh instance), lists each exemplar
    /// default-included, states both mandated disclosures (corpus limit +
    /// anonymity), and publishes what survived.
    @ViewBuilder private var publishSheetSection: some View {
        if publishSheetOpen {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.personalization.publishSheetTitle).font(.headline)

                Picker(L.personalization.publishKindLabel, selection: Binding(
                    get: { publishKind },
                    set: { setPublishKind($0) }
                )) {
                    ForEach(publishKindChoices, id: \.tag) {
                        Text($0.label).tag($0.tag).accessibilityIdentifier($0.tag)
                    }
                }
                .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishKindSelect)
                .automationSelect(
                    Ids.personalizationTrainedFactorPublishKindSelect,
                    value: { publishKind },
                    options: { publishKindChoices.map(\.tag) }
                ) { setPublishKind($0) }

                TextField(L.personalization.publishNamePlaceholder, text: $publishName)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishNameInput)
                    .automationField(Ids.personalizationTrainedFactorPublishNameInput, text: $publishName)

                automationText(
                    Ids.personalizationTrainedFactorPublishLimitationNote,
                    isModelKind
                        ? "\(L.personalization.publishLimitationNoteModel)\n\(L.personalization.publishCorpusSize(included: "\(publishIncludedExamples)", marked: "\(publishMarkedExamples)"))"
                        : L.personalization.publishLimitationNote
                )
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if isModelKind {
                    publishNgramReview
                } else {
                    publishExemplarReview
                }

                HStack {
                    Button(L.personalization.publishSubmit) {
                        Task { await submitPublish() }
                    }
                    .disabled(!publishCanSubmit)
                    .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishSubmitButton)
                    .automationActivate(
                        Ids.personalizationTrainedFactorPublishSubmitButton,
                        isEnabled: { publishCanSubmit }
                    ) { Task { await submitPublish() } }

                    Button(L.common.cancel) { closePublishSheet() }
                        .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishCancelButton)
                        .automationActivate(Ids.personalizationTrainedFactorPublishCancelButton) {
                            closePublishSheet()
                        }
                }
            }
            .padding(12)
            .background(Color.secondary.opacity(0.06))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishSheet)
            .automationValue(Ids.personalizationTrainedFactorPublishSheet, text: { "" })
        }
    }

    /// Whether the open sheet currently reviews the Model kind — the raw wire
    /// discriminator, never inferred from any per-app state.
    private var isModelKind: Bool { publishKind == "text-model" }

    /// The kind picker's options — the shared `fauna_core::format::publish_kind_options`
    /// catalog, so the option a user picks and the words they read back can
    /// never drift (mirrors `BackupDestinationsVM.kindOptions`).
    private var publishKindChoices: [(tag: String, label: String)] {
        FaunaFFISwift.publishKindOptions().map { (tag: $0.value, label: renderLocalizedText($0.label)) }
    }

    /// The List kind's review body — the scored top-N exemplars.
    @ViewBuilder private var publishExemplarReview: some View {
        if publishExemplars.isEmpty {
            automationText(Ids.personalizationTrainedFactorPublishExemplarEmpty,
                            L.personalization.publishExemplarsEmpty)
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(publishExemplars.enumerated()), id: \.element.postId) { offset, exemplar in
                    publishExemplarRow(exemplar, index: offset)
                }
            }
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishExemplarList)
            .automationValue(Ids.personalizationTrainedFactorPublishExemplarList,
                             text: { "\(publishExemplars.count)" })
        }
    }

    /// The Model kind's review body — **every** surviving n-gram: where the
    /// List's rows bound endorsement of already-public ids, these rows **are**
    /// the disclosure (`topic-factors.md` § Publishing), so there is no top-N.
    @ViewBuilder private var publishNgramReview: some View {
        if publishNgrams.isEmpty {
            automationText(Ids.personalizationTrainedFactorPublishNgramEmpty,
                            L.personalization.publishNgramsEmpty)
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(publishNgrams.enumerated()), id: \.element.ngram) { offset, ngram in
                    publishNgramRow(ngram, index: offset)
                }
            }
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishNgramList)
            .automationValue(Ids.personalizationTrainedFactorPublishNgramList,
                              text: { "\(publishNgrams.count)" })
        }
    }

    /// One surviving n-gram row (indexed) — text + class direction + distinct-
    /// doc count + include checkbox, checked by default. Direction and count
    /// are BOTH shared faces (`ngramDirectionLabel`/`ngramDocCountLabel`) —
    /// the publisher's review is a promise about what a subscriber reads back
    /// at `labeler-inspect-model-entry-*`, so the two ends must not disagree.
    private func publishNgramRow(_ ngram: ReviewNgram, index: Int) -> some View {
        HStack(spacing: 8) {
            automationText(Ids.personalizationTrainedFactorPublishNgramText, ngram.ngram)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(
                Ids.personalizationTrainedFactorPublishNgramDirection,
                renderLocalizedText(FaunaFFISwift.ngramDirectionLabel(more: ngram.more, less: ngram.less))
            )
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(
                Ids.personalizationTrainedFactorPublishNgramCount,
                renderLocalizedText(FaunaFFISwift.ngramDocCountLabel(more: ngram.more, less: ngram.less))
            )
                .font(.caption)
                .foregroundStyle(.secondary)
            Toggle("", isOn: Binding(
                get: { publishNgramIncluded[ngram.ngram] ?? true },
                set: { publishNgramIncluded[ngram.ngram] = $0 }
            ))
            .labelsHidden()
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishNgramCheckbox)
            // Same wire contract as the exemplar checkbox: "true"/"false".
            .automationActivate(
                Ids.personalizationTrainedFactorPublishNgramCheckbox,
                value: { (publishNgramIncluded[ngram.ngram] ?? true) ? "true" : "false" }
            ) {
                publishNgramIncluded[ngram.ngram] = !(publishNgramIncluded[ngram.ngram] ?? true)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishNgramItem)
        .automationValue(Ids.personalizationTrainedFactorPublishNgramItem, text: { ngram.ngram })
        .automationScope(Ids.personalizationTrainedFactorPublishNgramItem, index: index)
    }

    /// One scored exemplar row (indexed) — preview text + per-mille score +
    /// include checkbox, checked by default (the restore-kind-checkbox prune
    /// shape). `.automationScope` so the checkbox's scoped `state` read
    /// resolves; the flat occurrence-index `-text`/`-score`/`-checkbox` reads
    /// work unscoped too since every row carries exactly one of each, in the
    /// same best-score-first order `scoreCorpusForFactor` already returns.
    private func publishExemplarRow(_ exemplar: ScoredExemplar, index: Int) -> some View {
        HStack(spacing: 8) {
            automationText(Ids.personalizationTrainedFactorPublishExemplarText, exemplar.preview)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.personalizationTrainedFactorPublishExemplarScore,
                            L.personalization.publishScore(score: "\(exemplar.score)"))
                .font(.caption)
                .foregroundStyle(.secondary)
            Toggle("", isOn: Binding(
                get: { publishIncluded[exemplar.postId] ?? true },
                set: { publishIncluded[exemplar.postId] = $0 }
            ))
            .labelsHidden()
            .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishExemplarCheckbox)
            // Same wire contract as the engagement toggle above: "true"/"false",
            // not "on"/"off" (actions/personalization.py::publish_exemplar_included
            // checks == "true").
            .automationActivate(
                Ids.personalizationTrainedFactorPublishExemplarCheckbox,
                value: { (publishIncluded[exemplar.postId] ?? true) ? "true" : "false" }
            ) {
                publishIncluded[exemplar.postId] = !(publishIncluded[exemplar.postId] ?? true)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.personalizationTrainedFactorPublishExemplarItem)
        .automationValue(Ids.personalizationTrainedFactorPublishExemplarItem, text: { exemplar.postId })
        .automationScope(Ids.personalizationTrainedFactorPublishExemplarItem, index: index)
    }

    /// Publish is armed once at least one row of the ACTIVE kind is kept —
    /// independent of the name field (the disarm/re-arm e2e leg flips
    /// checkboxes before ever typing a name). Mirrors linux
    /// `refresh_submit_sensitivity`.
    private var publishCanSubmit: Bool {
        if isModelKind {
            return publishNgrams.contains { publishNgramIncluded[$0.ngram] ?? false }
        }
        return publishExemplars.contains { publishIncluded[$0.postId] ?? false }
    }

    /// Reveal the sheet pre-targeted at `row`, on the default List kind, then
    /// asynchronously score the loaded corpus. The `generation` snapshot
    /// discards a stale reply that lands after the sheet closed or
    /// re-targeted a different row.
    private func openPublishSheet(_ row: FfiTrainedTopicRow) {
        publishTargetId = row.id
        publishTargetFactorKey = row.factorKey
        publishKind = "list"
        publishExemplars = []
        publishIncluded = [:]
        publishNgrams = []
        publishNgramIncluded = [:]
        publishMoreDocs = 0
        publishLessDocs = 0
        publishIncludedExamples = 0
        publishMarkedExamples = 0
        publishName = ""
        publishGeneration += 1
        let generation = publishGeneration
        publishSheetOpen = true
        trainedTopicsVM.publishSheetOpen = true
        // A fresh, user-initiated open retires a stale refusal immediately.
        trainedTopicsVM.errorMessage = nil
        guard let factorKey = row.factorKey else { return }
        Task {
            do {
                let scored = try await feedVM.manager?.scoreCorpusForFactor(factor: factorKey) ?? []
                guard generation == publishGeneration else { return }
                publishExemplars = scored
                publishIncluded = Dictionary(uniqueKeysWithValues: scored.map { ($0.postId, true) })
            } catch {
                guard generation == publishGeneration else { return }
                trainedTopicsVM.errorMessage = DisplayError.message(error)
            }
        }
    }

    /// `-publish-kind-select`: swap which artifact kind the open sheet
    /// reviews. **Re-reads from scratch** — the two kinds review different
    /// objects through different shared faces, so a carried-over prune or
    /// resolved-corpus state would paint one kind's refusal over the other's
    /// un-read corpus (mirrors tui `set_publish_kind_op`). Picking the kind
    /// already selected is a no-op — a redundant select must not discard a
    /// review in progress. The public name survives the swap.
    private func setPublishKind(_ kind: String) {
        guard kind != publishKind, let factorKey = publishTargetFactorKey else { return }
        publishKind = kind
        publishExemplars = []
        publishIncluded = [:]
        publishNgrams = []
        publishNgramIncluded = [:]
        publishMoreDocs = 0
        publishLessDocs = 0
        publishIncludedExamples = 0
        publishMarkedExamples = 0
        publishGeneration += 1
        let generation = publishGeneration
        Task {
            do {
                if kind == "text-model" {
                    guard let review = try await feedVM.manager?.scrubCorpusForFactor(factor: factorKey) else { return }
                    guard generation == publishGeneration else { return }
                    publishNgrams = review.ngrams
                    publishNgramIncluded = Dictionary(uniqueKeysWithValues: review.ngrams.map { ($0.ngram, true) })
                    publishMoreDocs = review.moreDocs
                    publishLessDocs = review.lessDocs
                    publishIncludedExamples = review.includedExamples
                    publishMarkedExamples = review.markedExamples
                } else {
                    let scored = try await feedVM.manager?.scoreCorpusForFactor(factor: factorKey) ?? []
                    guard generation == publishGeneration else { return }
                    publishExemplars = scored
                    publishIncluded = Dictionary(uniqueKeysWithValues: scored.map { ($0.postId, true) })
                }
            } catch {
                guard generation == publishGeneration else { return }
                trainedTopicsVM.errorMessage = DisplayError.message(error)
            }
        }
    }

    private func closePublishSheet() {
        publishSheetOpen = false
        trainedTopicsVM.publishSheetOpen = false
        publishTargetId = nil
        publishTargetFactorKey = nil
        publishKind = "list"
        publishExemplars = []
        publishIncluded = [:]
        publishNgrams = []
        publishNgramIncluded = [:]
        publishMoreDocs = 0
        publishLessDocs = 0
        publishIncludedExamples = 0
        publishMarkedExamples = 0
        publishName = ""
        publishGeneration += 1
    }

    /// Publish the checked rows of the ACTIVE kind under `publishName`
    /// (trimmed; a blank name is a client-side no-op, same guard
    /// `TrainedTopicsVM.create` uses — the boundary's own `BlankName` error is
    /// a defensive fallback that should never actually fire from this guard).
    /// A refusal surfaces via the page's existing `error-message` (shared with
    /// the rest of the trained-topics facet); success closes the sheet.
    ///
    /// Model kind: `moreDocs`/`lessDocs` are passed through UNSHRUNK — they
    /// are the corpus's own counters, not the pruned entry count, and stay
    /// true however much of the vocabulary the user withheld.
    private func submitPublish() async {
        guard let targetId = publishTargetId, let client else { return }
        let trimmedName = publishName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else { return }
        if isModelKind {
            let ngrams = publishNgrams
                .filter { publishNgramIncluded[$0.ngram] ?? false }
                .map { FfiPublishNgram(ngram: $0.ngram, more: $0.more, less: $0.less) }
            guard !ngrams.isEmpty else { return }
            do {
                _ = try await client.api.trainedTopicPublishModel(
                    factorId: targetId, name: trimmedName,
                    moreDocs: publishMoreDocs, lessDocs: publishLessDocs, ngrams: ngrams)
                trainedTopicsVM.errorMessage = nil
                closePublishSheet()
            } catch {
                trainedTopicsVM.errorMessage = mapPublishModelError(error)
            }
            return
        }
        let entries = publishExemplars
            .filter { publishIncluded[$0.postId] ?? false }
            .map { FfiPublishEntry(postId: $0.postId, score: $0.score) }
        guard !entries.isEmpty else { return }
        do {
            _ = try await client.api.trainedTopicPublishList(
                factorId: targetId, name: trimmedName, entries: entries)
            trainedTopicsVM.errorMessage = nil
            closePublishSheet()
        } catch {
            trainedTopicsVM.errorMessage = mapPublishError(error)
        }
    }

    /// Mirrors `TrainedTopicsVM.mapError`'s shape: a boundary error this VM
    /// doesn't own (including a cancellation) falls to `DisplayError.message`;
    /// `.General` surfaces its boundary-supplied message directly.
    private func mapPublishError(_ error: Error) -> String? {
        guard let ffi = error as? FfiPublishListError else { return DisplayError.message(error) }
        switch ffi {
        case .BlankName: return L.personalization.publishNameBlank
        case .NameTooLong(let max): return L.personalization.publishNameTooLong(max: "\(max)")
        case .General(let msg): return msg
        }
    }

    /// `mapPublishError`'s shape, over the Model kind's error variants —
    /// `.EmptyVocabulary` is the one refusal a user fixes by marking more
    /// public posts rather than by editing the sheet (§ Publishing).
    private func mapPublishModelError(_ error: Error) -> String? {
        guard let ffi = error as? FfiPublishModelError else { return DisplayError.message(error) }
        switch ffi {
        case .EmptyVocabulary: return L.personalization.publishVocabularyEmpty
        case .BlankName: return L.personalization.publishNameBlank
        case .NameTooLong(let max): return L.personalization.publishNameTooLong(max: "\(max)")
        case .General(let msg): return msg
        }
    }

    /// Delete the sealed engagement-cue rollup + reset the live engine
    /// (engagement-cues.md § At rest; the user-revocable delete affordance).
    /// Surfaces a failure via the page's existing error surface — silently
    /// swallowing it here would look exactly like a no-op success, a
    /// recurring silent-feature-death bug class on this client.
    private func clearEngagementData() async {
        do { try await feedVM.manager?.deleteCueRollup() }
        catch { trainedTopicsVM.errorMessage = DisplayError.message(error) }
    }

    /// Read the signal opt-in + published list (`fauna.moderation.signal_share.status`,
    /// page mount). Mirrors linux `hydrate_signal_share`.
    private func hydrateSignalShare() async {
        guard let manager = feedVM.manager else { return }
        do {
            let status = try await manager.signalShareStatus()
            signalShare = status.share
            signalSharePublished = status.published
        } catch {
            trainedTopicsVM.errorMessage = DisplayError.message(error)
        }
    }

    /// Set the opt-in (`fauna.moderation.signal_share.set`), then re-read
    /// status so the toggle + list reflect the persisted value — opting out
    /// withdraws this actor's signals, which may shrink the list. Mirrors
    /// linux `set_signal_share`.
    private func setSignalShare(_ share: Bool) async {
        guard let manager = feedVM.manager else { return }
        do {
            _ = try await manager.setSignalSharing(share: share)
            let status = try await manager.signalShareStatus()
            signalShare = status.share
            signalSharePublished = status.published
        } catch {
            trainedTopicsVM.errorMessage = DisplayError.message(error)
        }
    }

    private func hexSuffix(_ factorKey: String?) -> String {
        guard let factorKey, factorKey.hasPrefix("topic:") else { return "" }
        return String(factorKey.dropFirst("topic:".count))
    }

    /// Create (no row targeted) or rename (a row targeted via its
    /// rename-button) the name currently staged in `trainedTopicName`, then
    /// reset to create mode — mirrors windows' `SubmitTrainedTopicAsync`.
    private func submitTrainedTopic() async {
        if let id = renamingTrainedFactorId {
            await trainedTopicsVM.rename(id: id, name: trainedTopicName)
        } else {
            await trainedTopicsVM.create(name: trainedTopicName)
        }
        renamingTrainedFactorId = nil
        trainedTopicName = ""
    }

    /// The caller's SUBSCRIBED labelers, indices preserved into the full
    /// `snapshot.entries` so `unsubscribe(index:)` targets the right row
    /// (mirrors linux `rebuild_rows`'s index-preserving filter).
    private var subscribedEntries: [IndexedLabelerEntry] {
        guard let entries = vm.snapshot?.entries else { return [] }
        return entries.enumerated()
            .filter { $0.element.subscribed }
            .map { IndexedLabelerEntry(index: $0.offset, entry: $0.element) }
    }

    /// Whether the shared catalog read has RESOLVED — `personalization-labelers-empty`'s
    /// second painting condition. Named for the facet because this page hosts
    /// three of them; the other two carry their own load signals
    /// (`trainedTopics`' own read, and the engagement-cue controls').
    private var labelersLoaded: Bool { vm.snapshot?.loaded ?? false }
}
