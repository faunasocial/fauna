import Testing
@testable import FaunaKit

// Pin the shared `SettingsPage` enum to the cross-app settings sub-page nav
// contract the e2e state protocol drives (`{"view":"settings"},{"view":"settings","id":"<navId>"}`),
// matching the linux GTK Stack child names in
// `apps/fauna-linux/src/views/settings_shell.rs`. The macOS Settings rail
// (`SettingsNavRail`) and the iOS `SettingsView` `NavigationStack` both resolve
// sub-pages through `SettingsPage(navId:)`, so a wrong id here silently breaks
// programmatic nav on every Apple app — exactly the gap that blocked iOS
// user-mail e2e (the user-mail sub-page id is `mail`, NOT `mail-settings`).
//
// `nostr` landed as a standalone page (shared FaunaKit NostrSettingsView, nostr.md
// § Page structure); `logs` landed 2026-06-13 (shared FaunaKit LogsView);
// `member-review` landed 2026-08-25 (shared FaunaKit MemberReviewView, row 308 —
// the permanent unattested-member review backlog, succession-aftermath.md §
// Propagation item (iv)). `p2p` is
// deliberately ABSENT — p2p.md § Implementation status records macOS/iOS as having no
// p2p page surface and ui.yaml marks both "Not implemented per spec"; the unsanctioned
// WG-era PeerContactsView cluster was deleted 2026-07-13 (see
// `unknownNavIdIsNotResolved`, which pins the absence). `devices` (roster — DevicesContent) + `folders`
// (control plane — FoldersContent) replaced the former single `sync` slot at the
// 2026-06-28 sync/folder UI unification (settings.md:17); the former Apple-only
// `photo-backup` rail page is retired (its controls moved into `folders`). This
// test pins the shipped set.

@Test func navIdsMatchCrossClientContract() {
    #expect(SettingsPage.status.navId == "status")
    #expect(SettingsPage.account.navId == "account")
    // The permanent unattested-member review backlog (succession-aftermath.md § Propagation item (iv)); rail placement
    // directly after Account, reachable at all times.
    #expect(SettingsPage.memberReview.navId == "member-review")
    #expect(SettingsPage.privacy.navId == "privacy")
    // Personalization hub (Feeds/Muted-words/Community-labelers links) +
    // the Community-labelers catalog it links out to (content-moderation-
    // and-ranking.md § Tier-3 community models).
    #expect(SettingsPage.personalization.navId == "personalization")
    #expect(SettingsPage.labelerCatalog.navId == "labeler-catalog")
    #expect(SettingsPage.general.navId == "general")
    #expect(SettingsPage.encryption.navId == "encryption")
    // The device roster (formerly the top-level "Peers" page); navId stays `devices`
    // (re-homed under the Settings shell, no `settings-devices` rename — O-3).
    #expect(SettingsPage.devices.navId == "devices")
    // The folder control plane (renamed from the former `sync` sub-page); absorbs
    // the wizard/list/conflicts + desktop binding + the Apple photo-backup controls.
    #expect(SettingsPage.folders.navId == "folders")
    // The user-mail sub-page is `mail` (rail "Mail" / linux Stack child), not
    // `mail-settings` (which is the ui.yaml *page* name).
    #expect(SettingsPage.mail.navId == "mail-settings")
    #expect(SettingsPage.mailAliases.navId == "mail-aliases")
    #expect(SettingsPage.mailSpam.navId == "mail-spam")
    #expect(SettingsPage.mailExport.navId == "mail-export")
    // The foreign-mailbox migration wizard (mailbox-migration.md), the
    // export twin's counterpart — sits directly after it in the rail.
    #expect(SettingsPage.mailImport.navId == "mail-import")
    #expect(SettingsPage.mailLists.navId == "mail-lists")
    #expect(SettingsPage.mailListMembers.navId == "mail-list-members")
    #expect(SettingsPage.nostr.navId == "nostr")
    // The dedicated AT Protocol integration-depth page (ui/atproto.md); nav id
    // matches the ui.yaml page key (renamed from the interim `bluesky-settings`
    // 2026-07-22, then from `bluesky` 2026-09-28 — see the landmark `atproto-page`).
    #expect(SettingsPage.atproto.navId == "atproto")
    #expect(SettingsPage.web.navId == "web")
    // Consumer-side subscriptions page (monetization.md § Pillar 1); the nav id is
    // the ui.yaml *page* name `subscription-settings` (this page is the one case
    // where the nav id == the ui.yaml page id, unlike mail/`mail-settings`).
    #expect(SettingsPage.subscriptions.navId == "subscription-settings")
    #expect(SettingsPage.linkedNests.navId == "nests")
    #expect(SettingsPage.taskDelegation.navId == "task-delegation")
    // The one roster over every third-party principal (connected-apps.md); the nav
    // id is the ui.yaml page key, and the rail slot follows Task delegation.
    #expect(SettingsPage.connectedApps.navId == "connected-apps")
    #expect(SettingsPage.logs.navId == "logs")
}

@Test func navIdRoundTripsForEveryPage() {
    for page in SettingsPage.allCases {
        #expect(SettingsPage(navId: page.navId) == page)
    }
}

@Test func mailNavIdResolvesToMailPage() {
    // The exact lookup the iOS user-mail e2e exercises:
    // `{"view":"settings","id":"mail-settings"}` must land on the mail-settings page.
    #expect(SettingsPage(navId: "mail-settings") == .mail)
}

@Test func nilNavIdIsNotResolved() {
    // `init(navId:)` is pure — `nil` (a plain `{"view":"settings"}` with no second
    // stack entry) → `nil`. Each platform applies its own default at the call site
    // (macOS `?? .status`; iOS treats `nil` as the root list).
    #expect(SettingsPage(navId: nil) == nil)
}

@Test func unknownNavIdIsNotResolved() {
    #expect(SettingsPage(navId: "not-a-real-page") == nil)
    // The doubled prefix is gone fleet-wide: the ui.yaml page name IS the nav id now.
    #expect(SettingsPage(navId: "mail-settings") == .mail)
    // `p2p` resolves to nil BY SPEC, not by omission: p2p.md § Implementation status
    // gives macOS/iOS no p2p page surface, and ui.yaml's p2p page marks both "Not
    // implemented per spec". Re-adding a `.p2p` case (as the WG-era PeerContactsView
    // cluster deleted 2026-07-13 did) is a spec deviation needing user approval —
    // this line is the guard.
    #expect(SettingsPage(navId: "p2p") == nil)
}

@Test func canonicalTopLevelNavViewOnlyForDevicesAndFolders() {
    // The two sub-pages migrated from a former top-level view round-trip
    // through `nav.stack[0].view` (linux `settings_subpage_canonical`'s
    // contract — a raw `{"view":"devices"|"folders"}` nav must read back the
    // same way, not just navigate there). Every other sub-page reports the
    // shell's own `"settings"` view — this pins that only these two opt in.
    #expect(SettingsPage.devices.canonicalTopLevelNavView == "devices")
    #expect(SettingsPage.folders.canonicalTopLevelNavView == "folders")
    for page in SettingsPage.allCases where page != .devices && page != .folders {
        #expect(page.canonicalTopLevelNavView == nil)
    }
}

@Test func builtIsInRailOrder() {
    // `built` is the content-backed rail order (settings.md § Navigation model),
    // consumed directly by the macOS `SettingsNavRail`.
    #expect(SettingsPage.built.map(\.navId) == [
        "status", "account", "member-review", "privacy", "muted-words", "personalization", "labeler-catalog", "general",
        "encryption", "devices", "folders",
        "nostr", "atproto", "mail-settings", "mail-aliases", "mail-spam", "mail-export", "mail-import", "mail-lists",
        "mail-list-members", "web", "subscription-settings", "nests", "task-delegation",
        "connected-apps", "logs",
    ])
}
