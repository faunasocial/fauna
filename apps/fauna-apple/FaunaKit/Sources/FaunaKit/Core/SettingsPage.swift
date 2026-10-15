import SwiftUI

/// The sub-pages inside the Settings shell (settings.md § Navigation model,
/// ratified 2026-06-03). Shared by **both** Apple targets (priority #2/#3 — same
/// shell concept as `AdminPage`), one source of truth for the page set + nav ids:
/// - **macOS** drives it from `Fauna-macOS/App/AppState.selectedSettingsPage`,
///   rendered by the `SettingsNavRail` sidebar-swap + `SettingsShellView`, one
///   sub-page visible at a time; the rail iterates `built` and reads
///   `label`/`systemImage`.
/// - **iOS** keeps its idiomatic settings nav (settings.md:28 — a `List` of
///   pushes, no rail) but consumes the SAME enum: `AppState.selectedSettingsPage`
///   (optional — `nil` is the settings root list) drives the `SettingsView`
///   `NavigationStack` path, so the cross-app two-element nav lands the right
///   sub-page. iOS uses its own row labels, not `label`/`systemImage`.
///
/// The *page set* and *IDs* are uniform across clients (linux is the reference);
/// only the platform widget that switches them differs (settings.md:17).
///
/// `navId` is the cross-app Settings sub-page id the e2e state protocol drives
/// (`nav.stack[1].id` — settings.md:22), matching the linux GTK Stack child names
/// (`settings_shell.rs`) and the e2e action layer's `_navigate_subpage` ids.
/// **Note:** the user-mail sub-page nav id is **`mail`** (the rail's "Mail" entry
/// / linux Stack child `mail`), *not* `mail-settings` — the ui.yaml *page* is
/// `mail-settings`, but the cross-app *nav id* is `mail` (verified against
/// `tests/e2e-unified/actions/mail_settings.py`).
///
/// `init(navId:)` is **pure**: `nil` (a plain `{"view":"settings"}` with no second
/// stack entry) and any unknown id map to `nil`. Each platform applies its own
/// default at the call site — macOS folds the live-data **Status** page in as the
/// default landing (`SettingsPage(navId: subId) ?? .status`, settings.md:20), iOS
/// treats `nil` as the root page list. (This differs from `AdminPage(navId:)`,
/// whose root `dashboard` *is* a page, so it returns `.dashboard` for `nil`.)
///
/// `built` lists the content-backed pages in canonical rail order. The canonical
/// rail defines a `nostr` sub-page (settings.md:17) — present here since the shared
/// FaunaKit `NostrSettingsView` landed (nostr.md § Page structure, standalone page
/// ratified 2026-06-13). `nests` (`LinkedNestsView`) and `logs` (`LogsView`)
/// are also present.
///
/// **`devices` + `folders` replace the former single `sync` slot** (the
/// 2026-06-28 sync/folder UI unification, settings.md:17/20-25). `devices` is the
/// device **roster** (formerly the top-level "Peers" page; `DevicesContent` roster
/// slice — devices.md); `folders` is the folder **control plane** (list +
/// wizard + per-set config + conflicts + desktop folder binding + the Apple
/// photo-backup ingress controls; `FoldersContent` — folders.md). The former
/// Apple-only `photo-backup` rail page is **retired** — the `photo-backup-*`
/// controls are now the "Photo Library" Backup-mode set's config *inside*
/// `folders` (media.md § Apple photo-backup reframe), and Media renders the
/// unified explorer instead of `PhotoBackupControlsView`. (A page with no client
/// content yet is intentionally absent from this enum until its content lands —
/// add a case + `navId`/`label`/`systemImage` arm + `built` entry then.)
public enum SettingsPage: String, CaseIterable, Identifiable {
    case status
    case account
    case memberReview
    case privacy
    case mutedWords
    #if !FAUNA_KIDS
    case personalization
    case labelerCatalog
    #endif
    case general
    case encryption
    case devices
    case folders
    #if !FAUNA_KIDS
    case nostr
    case atproto
    case mail
    case mailAliases
    case mailSpam
    case mailExport
    case mailImport
    case mailLists
    case mailListMembers
    case web
    #endif
    case subscriptions
    case linkedNests
    case taskDelegation
    #if !FAUNA_KIDS
    case connectedApps
    #endif
    case logs

    public var id: String { rawValue }

    /// The cross-app Settings sub-page id (e2e `nav.stack[1].id`, settings.md:22).
    public var navId: String {
        switch self {
        case .status: "status"
        case .account: "account"
        case .memberReview: "member-review"
        case .privacy: "privacy"
        case .mutedWords: "muted-words"
        #if !FAUNA_KIDS
        case .personalization: "personalization"
        case .labelerCatalog: "labeler-catalog"
        #endif
        case .general: "general"
        case .encryption: "encryption"
        case .devices: "devices"
        case .folders: "folders"
        #if !FAUNA_KIDS
        case .nostr: "nostr"
        case .atproto: "atproto"
        case .mail: "mail-settings"
        case .mailAliases: "mail-aliases"
        case .mailSpam: "mail-spam"
        case .mailExport: "mail-export"
        case .mailImport: "mail-import"
        case .mailLists: "mail-lists"
        case .mailListMembers: "mail-list-members"
        case .web: "web"
        #endif
        case .subscriptions: "subscription-settings"
        case .linkedNests: "nests"
        case .taskDelegation: "task-delegation"
        #if !FAUNA_KIDS
        case .connectedApps: "connected-apps"
        #endif
        case .logs: "logs"
        }
    }

    /// The canonical top-level nav `view` name this sub-page round-trips as, for
    /// the two sub-pages migrated from a former top-level view (`devices` /
    /// `folders` — the 2026-06-28 sync/folder UI unification). `nil` for every
    /// other sub-page, which reports the shell's own `"settings"` view. Mirrors
    /// linux's `settings_subpage_canonical` (`main.rs`): the nav contract is
    /// that `{"view":"devices"}`/`{"view":"folders"}` round-trips through
    /// `nav.stack[0].view`, not just navigates there.
    public var canonicalTopLevelNavView: String? {
        switch self {
        case .devices: "devices"
        case .folders: "folders"
        default: nil
        }
    }

    /// Resolve a nav id to a page. Pure: `nil` and unknown ids map to `nil` (the
    /// settings root). Consumers apply their own default (macOS `?? .status`).
    public init?(navId: String?) {
        switch navId {
        case "status": self = .status
        case "account": self = .account
        case "member-review": self = .memberReview
        case "privacy": self = .privacy
        case "muted-words": self = .mutedWords
        #if !FAUNA_KIDS
        case "personalization": self = .personalization
        case "labeler-catalog": self = .labelerCatalog
        #endif
        case "general": self = .general
        case "encryption": self = .encryption
        case "devices": self = .devices
        case "folders": self = .folders
        #if !FAUNA_KIDS
        case "nostr": self = .nostr
        case "atproto": self = .atproto
        case "mail-settings": self = .mail
        case "mail-aliases": self = .mailAliases
        case "mail-spam": self = .mailSpam
        case "mail-export": self = .mailExport
        case "mail-import": self = .mailImport
        case "mail-lists": self = .mailLists
        case "mail-list-members": self = .mailListMembers
        case "web": self = .web
        #endif
        case "subscription-settings": self = .subscriptions
        case "nests": self = .linkedNests
        case "task-delegation": self = .taskDelegation
        #if !FAUNA_KIDS
        case "connected-apps": self = .connectedApps
        #endif
        case "logs": self = .logs
        default: return nil
        }
    }

    public var label: String {
        switch self {
        case .status: L.common.status
        case .account: L.common.account
        case .memberReview: L.settings.memberReviewPage.title
        case .privacy: L.settings.privacy
        case .mutedWords: L.mutedWords.title
        #if !FAUNA_KIDS
        case .personalization: L.personalization.title
        case .labelerCatalog: L.labelerCatalog.title
        #endif
        case .general: L.settings.general
        case .encryption: L.settings.encryptionPage.title
        case .devices: L.devices.title
        case .folders: L.folders.title
        #if !FAUNA_KIDS
        case .nostr: L.nostr.title
        case .atproto: L.atprotoSettings.title
        case .mail: L.mailSettings.title
        case .mailAliases: L.mailAliases.title
        case .mailSpam: L.mailSpam.title
        case .mailExport: L.mailExport.title
        case .mailImport: L.mailImport.title
        case .mailLists: L.mailLists.title
        case .mailListMembers: L.mailLists.membersTitle
        case .web: L.webSettings.title
        #endif
        case .subscriptions: L.subscriptions.title
        case .linkedNests: L.nests.title
        case .taskDelegation: L.taskDelegation.title
        #if !FAUNA_KIDS
        case .connectedApps: L.connectedApps.title
        #endif
        case .logs: L.logs.title
        }
    }

    public var systemImage: String {
        switch self {
        case .status: "chart.bar"
        case .account: "person.crop.circle"
        case .memberReview: "person.crop.circle.badge.questionmark"
        case .privacy: "hand.raised"
        case .mutedWords: "speaker.slash"
        #if !FAUNA_KIDS
        case .personalization: "sparkles"
        case .labelerCatalog: "shield.checkerboard"
        #endif
        case .general: "gearshape"
        case .encryption: "lock.shield"
        case .devices: "desktopcomputer"
        case .folders: "folder"
        #if !FAUNA_KIDS
        case .nostr: "antenna.radiowaves.left.and.right"
        case .atproto: "at"
        case .mail: "envelope"
        case .mailAliases: "arrowshape.turn.up.right"
        case .mailSpam: "exclamationmark.shield"
        case .mailExport: "square.and.arrow.up"
        case .mailImport: "square.and.arrow.down"
        case .mailLists: "list.bullet.rectangle"
        case .mailListMembers: "person.2"
        case .web: "globe"
        #endif
        case .subscriptions: "star"
        case .linkedNests: "link"
        case .taskDelegation: "gearshape.2"
        #if !FAUNA_KIDS
        case .connectedApps: "puzzlepiece.extension"
        #endif
        case .logs: "doc.text.magnifyingglass"
        }
    }

    /// The content-backed pages, in canonical rail order (settings.md:17).
    /// `memberReview` (`MemberReviewView` — the permanent unattested-member
    /// review backlog, row 308/`succession-aftermath.md` § Propagation item
    /// (iv)) follows `account` directly, per tui's ratified rail placement;
    /// reachable at all times, not just after a succession.
    /// `mutedWords` (`MutedWordsView` — the per-keyword conversation-collapse
    /// filter, content-moderation-and-ranking.md § Q3) follows `privacy` as its
    /// sibling personal content-filtering surface (settings.md ratified rail
    /// order). `personalization` (`PersonalizationView` — the Feeds/Muted-words/
    /// Community-labelers hub) follows `mutedWords` per its ratified placement;
    /// `labelerCatalog` (`LabelerCatalogView` — browse + inspect-before-subscribe)
    /// follows it, reached from the personalization home's browse-catalog link
    /// (content-moderation-and-ranking.md § Tier-3 community models). `devices`
    /// (the roster — `DevicesContent` roster slice) and `folders` (the
    /// folder control plane — `FoldersContent`) occupy the former single `sync`
    /// slot, placed after `encryption` (the device/sync administration cluster).
    /// **There is deliberately no `p2p` case:** `p2p.md` § Implementation status
    /// records macOS/iOS as having *no* p2p page surface, and `ui.yaml`'s p2p page
    /// marks both "Not implemented per spec". The unsanctioned WG-era
    /// `PeerContactsView`/`WireGuardView` cluster that used to occupy this slot was
    /// deleted 2026-07-13 (it also leaked a throwaway keypair's *secret* bytes into
    /// the invite URI's public-key field); the WireGuard stack it fronted was
    /// deleted outright 2026-08-23, so there is nothing left for such a case to
    /// show. `nostr` follows `folders` (shared
    /// FaunaKit `NostrSettingsView` — account/content/
    /// follows over `fauna.bridges.*`; nostr.md § Page structure); the Mail sub-pages
    /// follow `mail`, as on linux — with `mail-import` (`MailImportView`, the
    /// foreign-mailbox migration wizard, mailbox-migration.md) directly after
    /// `mail-export`, the same slot the linux/web/windows IA gives it;
    /// `subscription-settings` (`SubscriptionSettingsView`
    /// — the consumer "my subscriptions across all creators" page, monetization.md
    /// § Pillar 1, "sibling of mail-settings/web-settings") follows `web`;
    /// `nests` (`LinkedNestsView`) follows it; `task-delegation`
    /// (`TaskDelegationView` — the cross-participant runner + assignment surface,
    /// participants.md § Task delegation) follows Nests per its ratified rail
    /// placement; `connected-apps` (`ConnectedAppsView` — the one roster over every
    /// third-party principal, connected-apps.md) follows Task delegation per
    /// settings.md § Navigation model; `logs` (`LogsView`, observability.md
    /// § Surfaces) is last.
    ///
    /// The former Apple-only `photo-backup` rail page is **retired**: the
    /// `photo-backup-*` controls are now the "Photo Library" Backup-mode set's
    /// config *inside* `folders` (the shared `FoldersContent`, Apple
    /// `platform_elements`; media.md § Apple photo-backup reframe), so they no longer
    /// need their own rail entry.
    public static var built: [SettingsPage] {
        #if FAUNA_KIDS
        // The Fauna Kids build's reduced settings (family-safety.md § The account
        // age band, the kids-app bullet, item (4)): the excised planes' pages do
        // not exist in this build at all.
        [.status, .account, .memberReview, .privacy, .mutedWords, .general, .encryption,
         .devices, .folders, .subscriptions, .linkedNests, .taskDelegation, .logs]
        #else
        [.status, .account, .memberReview, .privacy, .mutedWords, .personalization, .labelerCatalog, .general, .encryption,
         .devices, .folders, .nostr, .atproto,
         .mail, .mailAliases, .mailSpam, .mailExport, .mailImport, .mailLists, .mailListMembers, .web,
         .subscriptions, .linkedNests, .taskDelegation, .connectedApps, .logs]
        #endif
    }
}
