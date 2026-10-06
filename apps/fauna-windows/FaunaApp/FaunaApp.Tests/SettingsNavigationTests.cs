using System.Linq;
using System.Reflection;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the Windows settings-shell navigation map
/// (settings.md § Navigation model, ratified 2026-06-03). FlaUI e2e
/// (test_settings_nav.py) flakes on win-arm64, so this is the gate that the
/// shared state-protocol settings sub-page ids route to the right shell page —
/// and that an unknown / bare-settings nav lands on status, never nowhere.
/// </summary>
public class SettingsNavigationTests
{
    [Theory]
    [InlineData("account", "account")]
    [InlineData("member-review", "member-review")]
    [InlineData("privacy", "privacy")]
    [InlineData("muted-words", "muted-words")]
    [InlineData("personalization", "personalization")]
    [InlineData("labeler-catalog", "labeler-catalog")]
    [InlineData("general", "general")]
    [InlineData("encryption", "encryption")]
    [InlineData("web", "web")]
    [InlineData("subscription-settings", "subscription-settings")]
    [InlineData("devices", "devices")]
    [InlineData("folders", "folders")]
    [InlineData("nostr", "nostr")]
    [InlineData("mail-settings", "mail-settings")]
    [InlineData("mail-aliases", "mail-aliases")]
    [InlineData("mail-spam", "mail-spam")]
    [InlineData("mail-export", "mail-export")]
    [InlineData("mail-lists", "mail-lists")]
    [InlineData("mail-list-members", "mail-list-members")]
    [InlineData("nests", "nests")]
    [InlineData("task-delegation", "task-delegation")]
    [InlineData("logs", "logs")]
    public void SubPageTag_MapsKnownIds(string subId, string expected)
        => Assert.Equal(expected, SettingsNavigation.SubPageTag(subId));

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("status")]
    [InlineData("bogus")] // unknown id — must not resolve to nowhere
    [InlineData("nonexistent")]
    public void SubPageTag_DefaultsToStatus(string? subId)
        => Assert.Equal(SettingsNavigation.Status, SettingsNavigation.SubPageTag(subId));

    [Fact]
    public void SubPages_StatusIsFirst_AndIsIdentityOverItsOwnTags()
    {
        // Status is the shell's default landing page (entered via settings-tab
        // or a bare {view:"settings"} nav).
        Assert.Equal(SettingsNavigation.Status, SettingsNavigation.SubPages[0]);
        // Every shell tag round-trips through SubPageTag (it's its own id), so
        // selecting a sub-page and re-deriving its tag is stable.
        foreach (var tag in SettingsNavigation.SubPages)
        {
            Assert.Equal(tag, SettingsNavigation.SubPageTag(tag));
        }
    }

    [Fact]
    public void SubPages_CoversTheCanonicalSettingsPageSet()
    {
        // The settings.md page set. Asserted STRUCTURALLY, never as a hard-coded
        // total: a magic count silently rots every time the rail gains a page, and
        // rewriting the number is the reflex that hides a genuinely dropped entry.
        // What actually matters is that the ordered list and the declared tags are
        // the same set, with no duplicate and Status first (the default landing page).
        var declared = typeof(SettingsNavigation)
            .GetFields(BindingFlags.Public | BindingFlags.Static)
            .Where(f => f.IsLiteral && f.FieldType == typeof(string))
            .Select(f => (string)f.GetRawConstantValue()!)
            .ToList();

        Assert.Equal(declared.OrderBy(x => x), SettingsNavigation.SubPages.OrderBy(x => x));
        Assert.Equal(SettingsNavigation.SubPages.Count, SettingsNavigation.SubPages.Distinct().Count());
        Assert.Equal(SettingsNavigation.Status, SettingsNavigation.SubPages[0]);
        Assert.Contains("logs", SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Nostr, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Status, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Account, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MemberReview, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Privacy, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MutedWords, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Personalization, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.LabelerCatalog, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.General, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Encryption, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Web, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Subscriptions, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Devices, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Folders, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.Mail, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MailAliases, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MailSpam, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MailExport, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MailLists, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.MailListMembers, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.LinkedNests, SettingsNavigation.SubPages);
        Assert.Contains(SettingsNavigation.TaskDelegation, SettingsNavigation.SubPages);
    }

    [Fact]
    public void SubPages_ConnectedAppsImmediatelyFollowsTaskDelegation()
    {
        // settings.md § Navigation model: Connected apps placed directly after
        // Task delegation (connected-apps.md; tui's own rail position).
        var tags = SettingsNavigation.SubPages;
        var taskDelegationIndex = tags.ToList().IndexOf(SettingsNavigation.TaskDelegation);
        Assert.Equal(SettingsNavigation.ConnectedApps, tags[taskDelegationIndex + 1]);
        Assert.Equal("connected-apps", SettingsNavigation.ConnectedApps);
    }

    [Fact]
    public void SubPageTag_ConnectedAppsIdResolvesToItsOwnPage()
    {
        // The state-protocol id the e2e navigates ({view:"settings", id:"connected-apps"}).
        Assert.Equal(SettingsNavigation.ConnectedApps, SettingsNavigation.SubPageTag("connected-apps"));
    }

    [Fact]
    public void SubPages_TaskDelegationImmediatelyFollowsLinkedNests()
    {
        // settings.md § Navigation model: Task delegation placed right after
        // Nests (participants.md § Task delegation, placement ratified 2026-07-08).
        var tags = SettingsNavigation.SubPages;
        var linkedNestsIndex = tags.ToList().IndexOf(SettingsNavigation.LinkedNests);
        Assert.Equal(SettingsNavigation.TaskDelegation, tags[linkedNestsIndex + 1]);
    }

    [Fact]
    public void SubPages_MemberReviewImmediatelyFollowsAccount()
    {
        // Tui's own rail position (its Recovery Kit section holds the
        // ephemeral half of the same review family).
        var tags = SettingsNavigation.SubPages;
        var accountIndex = tags.ToList().IndexOf(SettingsNavigation.Account);
        Assert.Equal(SettingsNavigation.MemberReview, tags[accountIndex + 1]);
    }

    [Fact]
    public void SubPages_MutedWordsImmediatelyFollowsPrivacy()
    {
        // settings.md § Navigation model: "Muted words ... placed after Privacy
        // as the sibling personal content-filtering surface to the spam
        // preferences on Privacy."
        var tags = SettingsNavigation.SubPages;
        var privacyIndex = tags.ToList().IndexOf(SettingsNavigation.Privacy);
        Assert.Equal(SettingsNavigation.MutedWords, tags[privacyIndex + 1]);
    }

    [Fact]
    public void SubPages_PersonalizationAndLabelerCatalogImmediatelyFollowMutedWords()
    {
        // content-moderation-and-ranking.md § Composition + § Tier-3: the
        // Personalization home + Community-labelers catalog are placed right
        // after Muted words (its sibling tier-1 personalization surface).
        var tags = SettingsNavigation.SubPages.ToList();
        var mutedWordsIndex = tags.IndexOf(SettingsNavigation.MutedWords);
        Assert.Equal(SettingsNavigation.Personalization, tags[mutedWordsIndex + 1]);
        Assert.Equal(SettingsNavigation.LabelerCatalog, tags[mutedWordsIndex + 2]);
    }

    [Fact]
    public void SubPages_HasNoDuplicates()
    {
        var tags = SettingsNavigation.SubPages;
        var distinct = new System.Collections.Generic.HashSet<string>(tags);
        Assert.Equal(distinct.Count, tags.Count);
    }

    // windows' P2P page was WireGuard registration only and went with the stack
    // 2026-08-23, taking the ExternalRedirectView hook (whose one entry was
    // "p2p") with it. What survives is the fallback contract: an id windows has
    // no sub-page for resolves to Status rather than to nothing.
    [Theory]
    [InlineData("p2p")]
    [InlineData("bogus")]
    public void SubPageTag_FallsBackToStatusForIdsWindowsHasNoSubPageFor(string subId)
        => Assert.Equal(SettingsNavigation.SubPageTag("status"), SettingsNavigation.SubPageTag(subId));

    [Fact]
    public void SubPages_DoesNotContainP2P()
        => Assert.DoesNotContain("p2p", SettingsNavigation.SubPages);
}
