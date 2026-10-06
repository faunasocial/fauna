using System.Collections.Generic;

namespace FaunaApp.Core.Services;

/// <summary>
/// Pure navigation map for the Windows settings shell (settings.md § Navigation
/// model, ratified 2026-06-03). Windows uses a sidebar-swap shell whose sub-pages
/// are switched by a shell-local NavigationView, mirroring the admin shell's Model
/// B shape (a single gated <c>settings-tab</c> entry opens the shell; sub-pages
/// are selected within it).
///
/// This class is the platform-agnostic half (no WinUI types) so it is unit-
/// testable in FaunaApp.Tests — FlaUI e2e flakes on win-arm64, so this map is the
/// deterministic gate. It maps the shared e2e state-protocol settings sub-page id
/// (the linux GTK Stack child name the cross-app action layer sends as
/// <c>nav.stack[last].id</c> — see tests/e2e-unified/actions/settings.py and
/// actions/mail_settings.py) to a stable shell tag; the WinUI
/// <see cref="object"/> SettingsShellPage maps the tag to the concrete Page type.
/// </summary>
public static class SettingsNavigation
{
    // Shell sub-page tags (also the inner NavigationView item Tags). Kept equal
    // to the state-protocol sub-page ids so the map is identity for the explicit
    // ids; a bare {view:"settings"} nav (no id — the status page) falls through
    // to Status, the shell's default landing page.
    public const string Status = "status";
    public const string Account = "account";
    // The permanent post-succession unattested-member review
    // (succession-aftermath.md § Propagation -> Removing a flagged member). Placed directly after Account, tui's
    // own rail position (its Recovery Kit section holds the ephemeral half).
    // State-protocol id "member-review".
    public const string MemberReview = "member-review";
    public const string Privacy = "privacy";
    // The tier-1 user keyword-mute list (moderation.md § Muted keywords;
    // content-moderation-and-ranking.md § Q3) — placed right after Privacy as
    // the sibling personal content-filtering surface to the spam preferences
    // there (settings.md § Navigation model).
    public const string MutedWords = "muted-words";
    // The unified tier-1 personalization home (content-moderation-and-ranking.md
    // § Composition + § Tier-3) — hubs the Feeds facet (exits to the feed page),
    // the Muted words link, and the caller's SUBSCRIBED community labelers.
    // Placed right after Muted words as its sibling tier-1 filtering surface.
    public const string Personalization = "personalization";
    // Community-labeler catalog — browse + inspect-before-subscribe, reached
    // from the personalization home's browse-catalog button.
    public const string LabelerCatalog = "labeler-catalog";
    public const string General = "general";
    public const string Encryption = "encryption";
    // Per-user web hosting opt-in (web-content-hosting.md § Published-post
    // management; ui.yaml web-settings). The state-protocol id is "web"
    // (actions/settings.py / test_web_authoring.py navigate {view:"settings",id:"web"}).
    public const string Web = "web";
    // Device roster (Settings → Devices; devices.md, ui.yaml § devices). The
    // device/sync-administration cluster (Devices · Folders) — formerly the
    // top-level "Peers" page, re-homed under Settings by the 2026-06-28
    // sync/folder UI unification. State-protocol id "devices".
    public const string Devices = "devices";
    // Folder control plane (Settings → Folders; folders.md, ui.yaml § folders) —
    // the create wizard, folder list + per-set config (selective sync, conflict policy),
    // candidate-model conflicts, and the desktop local-folder binding nested per set.
    // Renamed from the former "sync" (Sync-folders) slot by the 2026-06-28 sync/folder
    // UI unification; pairs with Devices as the device/sync-administration cluster.
    // State-protocol id "folders".
    public const string Folders = "folders";
    // The standalone Nostr settings page (nostr.md § Page structure, ratified
    // 2026-06-13: Nostr keeps its OWN dedicated page like mail, NOT folded into the
    // unified Bridges page). State-protocol id "nostr" (actions/nostr.py navigates
    // {view:"settings",id:"nostr"}). Placed before Mail — the sibling
    // dedicated-integration page, matching the linux/web IA.
    public const string Nostr = "nostr";
    // The standalone AT Protocol settings page (ui/atproto.md § Layout & flow — one
    // ordered integration-depth choice, not a pile of toggles). Its rail slot is
    // "after Nostr" (§ Navigation), the sibling deep-integration page. State-protocol
    // id "atproto" (actions/atproto_settings.py navigates {view:"settings",id:"atproto"}).
    public const string Atproto = "atproto";
    public const string Mail = "mail-settings";
    // Consumer subscriptions page — the tiers this user subscribes to, across all
    // creators (monetization.md § Pillar 1 consumer path; ui.yaml subscription-settings).
    // Sibling of mail-settings / web-settings, mirroring the linux IA.
    public const string Subscriptions = "subscription-settings";
    public const string MailAliases = "mail-aliases";
    public const string MailSpam = "mail-spam";
    public const string MailExport = "mail-export";
    // The import-from-foreign-IMAP wizard (mailbox-migration.md § UX shape). Its rail
    // slot is directly after mail-export — the sibling mailbox-portability page —
    // mirroring the linux/web IA (apps/fauna-linux/src/views/settings_shell.rs).
    public const string MailImport = "mail-import";
    public const string MailLists = "mail-lists";
    public const string MailListMembers = "mail-list-members";
    public const string LinkedNests = "nests";
    // The per-user Task delegation surface — which participant runs each heavy
    // background task kind, with a pin override (participants.md § Task
    // delegation; placement ratified 2026-07-08 — right after Nests). State-
    // protocol id "task-delegation" (no "-tab", like muted-words/nests/
    // personalization).
    public const string TaskDelegation = "task-delegation";
    // Connected apps — the roster of things acting for the user from outside the
    // seven apps, the Requests tray, the typed-code start and the Blocked apps
    // (connected-apps.md; settings.md § Navigation model — directly after Task
    // delegation, tui's own rail position). State-protocol id "connected-apps".
    public const string ConnectedApps = "connected-apps";
    // The client's own durable log record (the fauna_log ring) — a flat rail
    // sub-page after the existing pages (observability.md § Surfaces; the
    // state-protocol id is "logs", actions/logs.py navigate).
    public const string Logs = "logs";

    /// <summary>
    /// The settings sub-pages in shell display order. The first (<see cref="Status"/>)
    /// is the default landing page when the shell is entered via <c>settings-tab</c>
    /// or a bare <c>{view:"settings"}</c> nav.
    /// </summary>
    public static readonly IReadOnlyList<string> SubPages = new[]
    {
        Status, Account, MemberReview, Privacy, MutedWords, Personalization, LabelerCatalog, General, Encryption, Web,
        Subscriptions, Devices, Folders,
        Nostr,
        Atproto,
        Mail, MailAliases, MailSpam, MailExport, MailImport, MailLists, MailListMembers,
        LinkedNests, TaskDelegation, ConnectedApps, Logs,
    };

    /// <summary>
    /// Map a state-protocol settings sub-page id to its shell tag. A null / empty /
    /// unrecognized id (incl. the bare <c>{view:"settings"}</c> nav and an explicit
    /// "status") lands on Status — the shell's default page, so a settings nav never
    /// resolves to "nowhere".
    /// </summary>
    public static string SubPageTag(string? subId) => subId switch
    {
        Account => Account,
        MemberReview => MemberReview,
        Privacy => Privacy,
        MutedWords => MutedWords,
        Personalization => Personalization,
        LabelerCatalog => LabelerCatalog,
        General => General,
        Encryption => Encryption,
        Web => Web,
        Subscriptions => Subscriptions,
        Devices => Devices,
        Folders => Folders,
        Nostr => Nostr,
        Atproto => Atproto,
        Mail => Mail,
        MailAliases => MailAliases,
        MailSpam => MailSpam,
        MailExport => MailExport,
        MailImport => MailImport,
        MailLists => MailLists,
        MailListMembers => MailListMembers,
        LinkedNests => LinkedNests,
        TaskDelegation => TaskDelegation,
        ConnectedApps => ConnectedApps,
        Logs => Logs,
        _ => Status,
    };

    // No external-redirect hook: its one entry was "p2p", routing to windows'
    // separate top-level P2P page — a documented divergence that ENDED
    // 2026-08-23 when the WireGuard registration page it fronted was deleted.
    // windows now has no P2P surface at all (like macOS/iOS), so the shared id
    // {"view":"settings","id":"p2p"} takes SubPageTag's Status fallback.
}
