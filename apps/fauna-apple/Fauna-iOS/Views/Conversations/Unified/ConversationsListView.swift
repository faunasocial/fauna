import SwiftUI
import FaunaKit

/// Unified conversations page (iOS). The mobile-collapsed counterpart of macOS's
/// two-pane `MacConversationsView`: a single `NavigationStack` whose list pushes
/// a thread-detail screen (`ConversationDetailView`) and whose
/// `new-conversation-button` pushes a new-thread compose screen
/// (`NewThreadComposeView`) — no two-pane split, no modal.
///
/// Renders entirely off `ConversationsVM` (a thin `@Observable` observer over
/// `fauna-conversations`'s `ConversationsManager`); no client state machine, no
/// `rail` branches — capability gating lives in the leaf views and keys off
/// `capabilities.*`. There is no Groups page: group threads live here with
/// `flavor: mlsGroup`.
struct ConversationsListView: View {
    /// Bumped on every nav patch (`appState.navGeneration` — the same token
    /// `CalendarListView` takes, and for the same reason). A patch targeting
    /// conversations must show the conversations PAGE, i.e. the thread list:
    /// the change pops a pushed thread detail, the one navigation this page's
    /// own `@State` `path` otherwise makes unreachable from outside.
    ///
    /// Every OTHER app lands on the list by construction — their list pane is
    /// always on screen (macOS/windows/linux/tui/web two-pane, web-mobile
    /// route swap), so `navigate_to("conversations")` can never leave a detail
    /// covering it. iOS's single-pane `NavigationStack` is the one shape that
    /// can, and did.
    ///
    /// What that looked like, measured on the failing run — a `/tree` dump taken at the assertion, worth keeping because the
    /// symptom names the wrong layer. The row slots stay REGISTERED, so this is
    /// not a lost registration; they are voted *hidden*:
    ///
    ///     conversation-item (0/2 visible)
    ///       [0] HIDDEN(votes) geo=nil votes=disappear+detach
    ///       [1] HIDDEN(votes) geo=nil votes=disappear+detach
    ///     dm-message-text   (1/1 visible)   ← the OTHER thread's detail, on top
    ///
    /// `AutomationRegistry.hideSignal` is built to tolerate exactly this cover
    /// (a push fires the root's `.onDisappear` with no balancing `.onAppear` on
    /// the pop, so an attached sentinel votes once and the slot stays visible) —
    /// but here the push detached the rows' sentinels too, so BOTH votes landed
    /// and the slots hid definitively. Meanwhile the root's geometry-less
    /// toolbar items (`conversation-sort`, `new-conversation-button`) stayed
    /// visible, which is why `_ensure_on_conversations_page`'s own
    /// `wait_for("new-conversation-button")` sails through a covered list.
    ///
    /// Net effect on the harness: state already lists the new thread while
    /// `count("conversation-item")` reads 0 — `open_thread_by_id`'s "never
    /// rendered … lagged the state push", a *render* message for a *navigation*
    /// bug. Its `selected_thread_id` fallback cannot save it either: the
    /// selection is the thread the composer just sent to, not the one being
    /// opened.
    var reloadToken: Int = 0
    @Environment(AppState.self) private var appState
    @Environment(ConversationsVM.self) private var vm
    // Read only to key the account-scope drop of `path` below on the session — the
    // page's data all comes through the environment-held `vm`.
    @Environment(FaunaClient.self) private var client: FaunaClient?

    @State private var path: [Route] = []

    /// `newThread` carries a per-push `UUID` rather than being a bare
    /// no-payload case, so two `.newThread` pushes are never the same `Route`
    /// value and each push/pop gets its own destination identity.
    private enum Route: Hashable {
        case thread(ThreadId)
        case newThread(UUID)

        var isNewThread: Bool {
            if case .newThread = self { return true }
            return false
        }
    }

    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                ScrollView {
                    // Eager `ScrollView { VStack }`, NOT a lazy `List { Section }` (rule 6 —
                    // apple-e2e-automation.md § Registration rules): an iOS `List` lazily
                    // realizes AND POOLS its rows, so a thread filtered out by
                    // `conversation-search-box` (`test_search_filters_thread_list`, which
                    // narrows the `conversation-item` count as the query narrows) can
                    // linger on-screen past its real removal — the same delete-zombie class
                    // rule 6 fixed for iOS Events. Cost: rows lose `.listRowSeparator`/List
                    // inset styling (accepted rule-6 production-UI tradeoff).
                    VStack(spacing: 0) {
                        // `conversation-search-box` lives at the top of the scroll content
                        // so XCUITest can hit it (ui.yaml conversations `notes.ios`); it's a
                        // child of the `conversation-list-item` component.
                        TextField(L.conversations.list.searchPlaceholder, text: vm.searchBinding)
                            .textFieldStyle(.roundedBorder)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                            .accessibilityIdentifier(Ids.conversationSearchBox)
                            // Drives the manager's set_search_query (mirrors macOS). Env-gated no-op.
                            .automationField(Ids.conversationSearchBox, text: vm.searchBinding)
                            .padding(EdgeInsets(top: 4, leading: 16, bottom: 4, trailing: 16))

                        ForEach(Array(vm.rows.enumerated()), id: \.element.id) { index, row in
                            NavigationLink(value: Route.thread(row.id)) {
                                ConversationListRow(model: row)
                            }
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier(Ids.conversationItem)
                            // Indexed row actuation — drives the same NavigationStack
                            // push the NavigationLink tap performs (the iOS analogue of
                            // macOS's `selectRow`); reads the row label. Env-gated no-op.
                            .automationActivate(Ids.conversationItem, value: { row.label }) {
                                path.append(.thread(row.id))
                            }
                            // Scoped container (rule 5): `dm-unread-indicator` is 0-or-1
                            // per row and read by `scope="conversation-item[N]"`. Without
                            // a real subtree path the flat heuristic answers a scoped
                            // count with the GLOBAL one, so every row reads unread while
                            // any is (feed-item / device-card precedent).
                            .automationScope(Ids.conversationItem, index: index)
                            Divider()
                        }
                    }
                }

                // Always-present page-level error element (Rule 2).
                ErrorBanner(message: vm.pageError)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 6)
            }
            .pageTitle(L.conversations.list.title)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        vm.startNewConversation()
                    } label: {
                        Image(systemName: "square.and.pencil")
                    }
                    .accessibilityIdentifier(Ids.newConversationButton)
                    // Only activates the shared compose state — `presentComposerIfNeeded`
                    // (below) is the SOLE pusher of `.newThread`, reacting to that state
                    // exactly as the `startDirectMessage` seed-path already does (the iOS
                    // analogue of macOS's `vm.startNewConversation()`).
                    .automationActivate(Ids.newConversationButton) {
                        vm.startNewConversation()
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        vm.cycleSort()
                    } label: {
                        Image(systemName: "arrow.up.arrow.down")
                    }
                    .accessibilityIdentifier(Ids.conversationSort)
                    // Cycle-button pattern (AutomationRegistry.swift): one entry
                    // actuates AND reads the current order (mirrors macOS).
                    // Env-gated no-op in production.
                    .automationActivate(Ids.conversationSort, value: { ConversationsUI.sortLabel(vm.snapshot.sort) }) {
                        vm.cycleSort()
                    }
                }
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case let .thread(id):
                    ConversationDetailView(threadId: id)
                case .newThread:
                    NewThreadComposeView(onCancel: {
                        var txn = Transaction()
                        txn.disablesAnimations = true
                        withTransaction(txn) {
                            path.removeAll { $0.isNewThread }
                        }
                    })
                }
            }
        }
        // Mirror macOS's reactive detail-pane mount (`if vm.newThreadCompose != nil
        // { MacNewThreadComposeView() }`): whenever the shared compose state goes
        // active, ensure the new-thread screen is on the stack. This IS the `+`
        // button's push too (it only calls `vm.startNewConversation()`, same as
        // `startDirectMessage` from a profile) — one call site pushes `.newThread`
        // for every route into the composer, so there is no separate imperative
        // push to race against Cancel's pop. `onAppear`
        // catches the seed-then-tab-switch path (the list mounts with state already
        // active); `onChange` catches a live transition while the tab is already
        // visible; the `path.contains` guard prevents a double-push.
        // Plain-back / cancel / send / select-thread all flip `new_thread_active`
        // false (the manager), so a dismissed composer is never reopened.
        .onChange(of: vm.newThreadCompose != nil) { _, isComposing in
            if isComposing { presentComposerIfNeeded() }
        }
        .onAppear { presentComposerIfNeeded() }
        // `send_new_thread` materializes the thread and selects it BEFORE the send
        // even attempts (`manager.rs::send_new_thread`) — success or failure alike,
        // so `newThreadCompose` going nil is not on its own proof the user chose to
        // leave. macOS's `detailPane` is a `@ViewBuilder` that just re-renders off
        // the same two fields (`newThreadCompose`/`selectedDetail`) every frame, so
        // it falls through to `ThreadDetailView` for free. This `NavigationStack`'s
        // `path` is a discrete push stack with no such fallthrough: left alone, the
        // `.newThread` route stays on top showing nothing (`NewThreadComposeForm`
        // renders empty once its compose state is gone) — neither the list's
        // `new-conversation-button` nor any compose field is reachable, and a send
        // failure's `error-message` (which now lives on the new thread, per
        // `ThreadDetailView`) has nowhere to surface. Swap the stranded `.newThread`
        // entry for the real thread so the send's outcome has somewhere to render.
        // Cancel / plain-back never reach this branch: Cancel pops via its own
        // `onCancel` closure and plain-back via the system gesture, and neither
        // touches `selectedThreadId` — `ConversationDetailView.onDisappear`
        // already cleared it, since the `+` button is only reachable from the list.
        .onChange(of: vm.selectedThreadId) { _, newId in
            guard let newId, vm.newThreadCompose == nil,
                  let idx = path.firstIndex(where: { $0.isNewThread }) else { return }
            // No-animation, like the push/pop above — this swap races the send's
            // own outcome (the error banner rendering moments later on the very
            // screen this mounts), and an animated NavigationStack transition is a
            // real ~0.3-0.5s of wall-clock the outcome shouldn't have to outlast.
            // (The e2e timeout first blamed on this animation was something else:
            // the list root's page banner, covered by this very push, retracting
            // the thread screen's identical refusal from `AppMessages.error` —
            // fixed in `AppMessages.bannerDisappeared`.)
            var txn = Transaction()
            txn.disablesAnimations = true
            withTransaction(txn) {
                path[idx] = .thread(newId)
            }
        }
        // A nav patch that targets THIS tab shows the conversations page: pop a
        // pushed thread detail so the list root is on screen (see `reloadToken`).
        // Scoped to the patch's own target because this view, unlike
        // `CalendarListView`'s More-hub `case`, stays alive in the `TabView` and
        // therefore sees every patch's bump — including ones aimed elsewhere,
        // which must leave an open thread alone.
        //
        // The WHOLE stack goes, a live `.newThread` composer included
        // (`ui/README.md` § Navigation model: the page's own push stack returns to
        // its root). Popping the composer is the back gesture: its `.onDisappear`
        // calls `deactivateNewConversation`, which keeps the half-written draft,
        // and `new-conversation-button` resumes it (`conversations.md`
        // § Persistence). An earlier carve-out kept the composer on the premise
        // that its compose state would stay active with nothing to re-push it;
        // the deactivate refutes that, and the carve-out left the list root
        // covered for a switch to another thread.
        .onChange(of: reloadToken) {
            guard appState.selectedTab == "conversations" else { return }
            var txn = Transaction()
            txn.disablesAnimations = true
            withTransaction(txn) {
                path.removeAll()
            }
        }
        // The test agent's `nav_back` (the back gesture): pop the top screen, as the
        // nav-bar back does — for a `.newThread` composer that fires its
        // `.onDisappear` → `deactivateNewConversation`, keeping the draft.
        // No-animation, like every other push/pop on this `path`.
        .onChange(of: appState.navBackGeneration) {
            guard appState.selectedTab == "conversations", !path.isEmpty else { return }
            var txn = Transaction()
            txn.disablesAnimations = true
            withTransaction(txn) {
                _ = path.removeLast()
            }
        }
        // The account-scope seam for page-owned `@State` that is NOT a view model
        // (`ActorScope` § *State a view owns*; `account-scoping.md` § The scoping
        // taxonomy, the in-memory corollary ). This page's
        // `ConversationsVM` is environment-held and already on the canonical drop, but
        // `path` is the view's own, and a `.thread(id)` entry NAMES the outgoing
        // account's thread. iOS never unmounts a tab, so without this a switch made
        // while a thread was open left the incoming account looking at a thread detail
        // pushed for the previous account's conversation.
        //
        // Keyed on the client INSTANCE, so it fires across the nil phase and on a
        // swap that keeps `client != nil` true. Unlike the nav-token pop above, this
        // drops `.newThread` entries too: that reasoning ("its shared
        // `new_thread_compose` state would still be active") is about a *nav* pop
        // within one session, and at an identity change the compose state is the
        // outgoing account's and is itself dropped by the shell's teardown.
        .onChange(of: SessionKey(client)) {
            path.removeAll()
        }
    }

    private func presentComposerIfNeeded() {
        if vm.newThreadCompose != nil && !path.contains(where: { $0.isNewThread }) {
            // No-animation push, paired with the no-animation pop in `onCancel`
            // above (same reasoning there): an ANIMATED `NavigationStack` push/pop
            // is a ~0.3-0.5s transition, and a quick Cancel-then-`+` re-push
            // landing inside that window let the pop's completion tear down the
            // just-pushed fresh screen instead of the one it was popping — a real
            // double-tap hazard. Disabling the
            // transition animation removes the window the race lived in.
            var txn = Transaction()
            txn.disablesAnimations = true
            withTransaction(txn) {
                path.append(.newThread(UUID()))
            }
        }
    }
}
