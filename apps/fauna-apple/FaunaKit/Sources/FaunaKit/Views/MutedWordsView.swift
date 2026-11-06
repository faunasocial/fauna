import FaunaFFISwift
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
                        .disabled(vm.busy)
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
        .onStoreChange { await vm.resync() }
    }

    private func submitAdd() {
        let word = newWord
        newWord = ""
        Task { await vm.add(word) }
    }

    /// The row's `muted-word-level-select`: options are the shared
    /// `mutedKeywordLevels()`, the selection the shared classification of the
    /// stored weight; the e2e registry reads and writes the TOKEN
    /// (`mutedKeywordLevelValue`), the human sees the label.
    private func levelPicker(index: Int) -> some View {
        let levels = mutedKeywordLevels()
        let current = vm.level(at: index) ?? .hide
        return Picker(
            "",
            selection: Binding(
                get: { vm.level(at: index) ?? .hide },
                set: { level in Task { await vm.setLevel(at: index, to: level) } })
        ) {
            ForEach(levels, id: \.self) { level in
                Text(Self.levelLabel(level)).tag(level)
            }
        }
        .labelsHidden()
        .fixedSize()
        .disabled(vm.busy)
        .accessibilityIdentifier(Ids.mutedWordLevelSelect)
        .automationSelect(
            Ids.mutedWordLevelSelect,
            value: { mutedKeywordLevelValue(level: vm.level(at: index) ?? current) },
            options: { mutedKeywordLevels().map { mutedKeywordLevelValue(level: $0) } },
            isEnabled: { !vm.busy }
        ) { token in
            if let level = mutedKeywordLevelFromValue(value: token) {
                Task { await vm.setLevel(at: index, to: level) }
            }
        }
    }

    private static func levelLabel(_ level: MutedKeywordLevel) -> String {
        switch level {
        case .hide: return L.mutedWords.levelHide
        case .showLess: return L.mutedWords.levelShowLess
        }
    }

    /// One `muted-word-item` row (indexed — rows are addressed positionally,
    /// matching `task-delegation-kind-item`'s `.automationScope` idiom).
    private func wordRow(_ word: String, index: Int) -> some View {
        HStack {
            automationText(Ids.mutedWordText, word)
            Spacer()
            levelPicker(index: index)
            Button(L.mutedWords.remove) {
                Task { await vm.remove(at: index) }
            }
            .buttonStyle(.borderless)
            .disabled(vm.busy)
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
