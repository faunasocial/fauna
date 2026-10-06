import SwiftUI

/// The shared "Muted words" Settings sub-page (macOS + iOS, one FaunaKit view)
/// — add / list / remove terms in the sealed, client-only
/// `fauna.state.moderation` muted-keyword list (moderation.md § Muted keywords;
/// content-moderation-and-ranking.md § Q3; rail placement `ui/settings.md` §
/// Navigation model — right after Privacy). A matching decrypted conversation
/// message collapses behind `dm-message-muted` (`DmMessageBubble`); this page
/// is pure CRUD over the list, no moderation-queue interaction. Structurally
/// the mail-aliases CRUD list, only simpler — the input + add button sit
/// directly on the page (no add-sheet reveal, no kind picker). Reference:
/// linux `apps/fauna-linux/src/settings/muted_words.rs`.
public struct MutedWordsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MutedWordsVM()
    @State private var newWord = ""

    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.mutedWords.title)
                    .font(.title2)
                Text(L.mutedWords.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                HStack {
                    TextField(L.mutedWords.inputPlaceholder, text: $newWord)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.mutedWordInput)
                        .automationField(Ids.mutedWordInput, text: $newWord)
                        .onSubmit { submitAdd() }
                    Button(L.mutedWords.add) { submitAdd() }
                        .accessibilityIdentifier(Ids.mutedWordAddButton)
                        .automationActivate(Ids.mutedWordAddButton) { submitAdd() }
                }

                // Two conditions, not one (README.md § List pages: loading is
                // not empty): a page still reading paints neither rows nor this.
                if vm.showsEmptyState {
                    automationText(Ids.mutedWordEmpty, L.mutedWords.empty)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(vm.words.enumerated()), id: \.offset) { index, word in
                            wordRow(word, index: index)
                        }
                    }
                    .accessibilityIdentifier(Ids.mutedWordList)
                    // A bare `.accessibilityIdentifier` is invisible to the
                    // in-process driver — only `automation*` modifiers
                    // register; presence read is the row count.
                    .automationValue(Ids.mutedWordList, text: { "\(vm.words.count)" })
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(Ids.mutedWords)
        .automationValue(Ids.mutedWords, text: { "\(vm.words.count)" })
        .pageTitle(L.mutedWords.title)
        .task {
            guard let client else { return }
            vm.configure(api: client.api)
            await vm.load()
        }
    }

    private func submitAdd() {
        let word = newWord
        newWord = ""
        Task { await vm.add(word) }
    }

    /// One `muted-word-item` row (indexed — rows are addressed positionally,
    /// matching `task-delegation-kind-item`'s `.automationScope` idiom).
    private func wordRow(_ word: String, index: Int) -> some View {
        HStack {
            automationText(Ids.mutedWordText, word)
            Spacer()
            Button(L.mutedWords.remove) {
                Task { await vm.remove(at: index) }
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.mutedWordRemoveButton)
            .automationActivate(Ids.mutedWordRemoveButton) {
                Task { await vm.remove(at: index) }
            }
        }
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mutedWordItem)
        .automationValue(Ids.mutedWordItem, text: { word })
        .automationScope(Ids.mutedWordItem, index: index)
    }
}
