import SwiftUI
import FaunaKit

/// The macOS Settings shell content pane (settings.md § Navigation model,
/// ratified 2026-06-03). Renders the sub-page selected in the Settings nav rail
/// (`SettingsNavRail`, the sidebar-swap) — one sub-page visible at a time,
/// replacing the former single-scroll `PreferencesView` stack. Mirrors
/// `AdminShellView` (priority #3 — Settings uses the same shell concept as
/// admin). The page set is `SettingsPage` and the active page is
/// `MacAppState.selectedSettingsPage`, driven by the e2e two-element nav
/// `{"view":"settings"},{"view":"settings","id":"<page>"}` (settings.md:22).
///
/// **Status is the default + folded-in sub-page** — it carries all live nest-data
/// (`account-actor-id`, `quota-*`, `status-*-copy-btn`; settings.md:20), so a
/// plain `navigate()` lands here and the cross-app settings tests
/// (`test_actor_id_visible`, `test_quota_section`, `test_copy_buttons_visible`)
/// stay green. The Account sub-page is pure actions (settings.md:20).
///
/// `.id(selectedSettingsPage)` gives each sub-page a clean identity so switching
/// remounts the sub-view (its `.task` re-fetches) — the same recreation
/// `ContentView` applies per sidebar item.
struct SettingsShellView: View {
    @Environment(MacAppState.self) private var appState

    var body: some View {
        Group {
            switch appState.selectedSettingsPage {
            case .status:
                MacStatusView()
            case .account:
                AccountSettingsView(
                    session: appState.session,
                    ownLock: appState.instanceLock,
                    onAccountReset: {
                        appState.onSignOut?()
                        appState.isOnboarded = false
                    },
                    onSwitchAccount: { try await appState.onSwitchAccount?($0, $1) },
                    onAddAccount: { appState.onAddAccount?() }
                )
            case .memberReview:
                // The permanent unattested-member review backlog (shared
                // FaunaKit; row 308, succession-aftermath.md § Propagation
                // item (iv)) — reachable at all times, not just after a
                // succession.
                MemberReviewView()
            case .privacy:
                PrivacySettingsView(onInboxModeChanged: { appState.inboxMode = $0 })
            case .mutedWords:
                // Per-keyword conversation-collapse filter (shared FaunaKit;
                // content-moderation-and-ranking.md § Q3), sibling of Privacy.
                MutedWordsView()
            case .personalization:
                // Feeds / Muted-words / Community-labelers hub (shared FaunaKit;
                // content-moderation-and-ranking.md § Tier-3 community models).
                PersonalizationView(
                    onNavigateFeed: { appState.selectedSidebar = .feed },
                    onNavigateMutedWords: { appState.selectedSettingsPage = .mutedWords },
                    onNavigateCatalog: { appState.selectedSettingsPage = .labelerCatalog }
                )
            case .labelerCatalog:
                // Browse + inspect-before-subscribe (shared FaunaKit), reached
                // from the personalization home's browse-catalog link.
                LabelerCatalogView()
            case .general:
                GeneralSettingsView()
            case .encryption:
                EncryptionSettingsView()
            case .devices:
                // The device roster (formerly the top-level "Peers" page; shared
                // FaunaKit `DevicesContent` roster slice — devices.md).
                DevicesView(reloadToken: appState.navGeneration)
            case .folders:
                // The folder control plane (list + wizard + per-set config +
                // conflicts + desktop folder binding + the Apple photo-backup
                // controls) — shared FaunaKit `FoldersContent`, folders.md. The
                // former `Settings → Sync` page + standalone `photo-backup` page
                // fold in here (2026-06-28 unification).
                MacFoldersView(reloadToken: appState.navGeneration)
            case .nostr:
                // The standalone Nostr page (shared FaunaKit) — account/content/
                // follows over the unified `fauna.bridges.*` control plane; its own
                // dedicated page, not folded into Bridges (nostr.md § Page structure).
                NostrSettingsView()
            case .atproto:
                // The dedicated Bluesky integration-depth page (shared FaunaKit) —
                // ui/atproto.md; follows Nostr in the rail per its ratified slot.
                AtprotoSettingsView()
            case .mail:
                MailSettingsView()
            case .mailAliases:
                MailAliasesView()
            case .mailSpam:
                MailSpamView()
            case .mailExport:
                MailExportView()
            case .mailImport:
                // The foreign-mailbox migration wizard (shared FaunaKit;
                // mailbox-migration.md). Its two Done deep-links go to
                // Conversations, where mail lives — the tui lead app's own
                // resolution, since no "view inbox" / "skip log" RPC exists.
                MailImportView(onNavigateToConversations: {
                    appState.selectedSidebar = .conversations
                })
            case .mailLists:
                MailListsView(onNavigateToMembers: { listIdHex, friendlyName in
                    appState.selectedMailListId = listIdHex
                    appState.selectedMailListName = friendlyName
                    appState.selectedSettingsPage = .mailListMembers
                })
            case .mailListMembers:
                if let listIdHex = appState.selectedMailListId, let name = appState.selectedMailListName {
                    MailListMembersView(listIdHex: listIdHex, listName: name)
                } else {
                    MailListMembersView()
                }
            case .web:
                WebSettingsView(webPublish: appState.webPublish, reloadToken: appState.navGeneration)
            case .subscriptions:
                // The consumer-side "my subscriptions across all creators" page
                // (shared FaunaKit; monetization.md § Pillar 1 consumer path).
                SubscriptionSettingsView()
            case .linkedNests:
                LinkedNestsView(reloadToken: appState.navGeneration)
            case .taskDelegation:
                // The cross-participant runner + assignment surface
                // (participants.md § Task delegation) — shared FaunaKit,
                // reload-on-every-visit since the runner column is live
                // advisory-lease state.
                TaskDelegationView(reloadToken: appState.navGeneration)
            case .connectedApps:
                // The one roster over every third-party principal (shared
                // FaunaKit; connected-apps.md) — reload-on-every-visit since
                // rows are nest state and a quiet push raises no event; the
                // same token restarts the visit when a `fauna://consent` route
                // lands while the page is already on screen.
                ConnectedAppsView(reloadToken: appState.navGeneration)
            case .logs:
                // The client's durable log record — the process-global `fauna_log`
                // ring (observability.md § Surfaces). Self-wires (no client handle).
                LogsView(source: .clientRing)
            }
        }
        .id(appState.selectedSettingsPage)
        .pageTitle(appState.selectedSettingsPage.label)
    }
}
