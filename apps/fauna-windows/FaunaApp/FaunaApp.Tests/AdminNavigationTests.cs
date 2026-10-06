using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the Windows admin-shell navigation map
/// (admin.md § Navigation model, ratified 2026-06-01). FlaUI e2e
/// (test_admin_nav.py) flakes on win-arm64, so this is the gate that the shared
/// state-protocol admin sub-page ids route to the right shell page — and that an
/// unknown / bare-admin nav lands on the dashboard, never nowhere.
/// </summary>
public class AdminNavigationTests
{
    [Theory]
    [InlineData("users", "users")]
    [InlineData("settings", "settings")]
    [InlineData("admin-nest", "admin-nest")]
    [InlineData("admin-aliases", "admin-aliases")]
    [InlineData("admin-mail", "admin-mail")]
    [InlineData("admin-calendar", "admin-calendar")]
    [InlineData("admin-contacts", "admin-contacts")]
    [InlineData("admin-files", "admin-files")]
    [InlineData("admin-web", "admin-web")]
    [InlineData("admin-dns", "admin-dns")]
    [InlineData("admin-bridges-pending", "admin-bridges-pending")]
    [InlineData("admin-custody-hosting", "admin-custody-hosting")]
    [InlineData("admin-logs", "admin-logs")]
    public void SubPageTag_MapsKnownIds(string subId, string expected)
        => Assert.Equal(expected, AdminNavigation.SubPageTag(subId));

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("dashboard")]
    [InlineData("private-nest")]   // retired page id — must not 404 into nowhere
    [InlineData("admin-services")] // removed 2026-06-04 (per-page-services redesign) — must not 404 into nowhere
    [InlineData("nonexistent")]
    public void SubPageTag_DefaultsToDashboard(string? subId)
        => Assert.Equal(AdminNavigation.Dashboard, AdminNavigation.SubPageTag(subId));

    [Fact]
    public void SubPages_DashboardIsFirst_AndIsIdentityOverItsOwnTags()
    {
        // The dashboard is the shell's default landing page (entered via admin-tab).
        Assert.Equal(AdminNavigation.Dashboard, AdminNavigation.SubPages[0]);
        // Every shell tag round-trips through SubPageTag (it's its own id), so
        // selecting a sub-page and re-deriving its tag is stable.
        foreach (var tag in AdminNavigation.SubPages)
        {
            Assert.Equal(tag, AdminNavigation.SubPageTag(tag));
        }
    }

    [Fact]
    public void SubPages_CoversTheCanonicalAdminPageSet()
    {
        // The admin.md page set (Dashboard / Users / Settings / Nest / Mail /
        // Calendar / Contacts / Files / Aliases — per the per-page-services
        // redesign, admin-services replaced by admin-nest 2026-06-04;
        // admin-calendar added 2026-06-17 (admin.md § 8 Calendar); admin-contacts
        // / admin-files added 2026-07-10, admin.md § Contacts / § Files) + the
        // contextual detail pages (admin-web apex designation / admin-dns /
        // admin-bridges-pending / admin-custody-hosting / admin-logs) the shell
        // switches between.
        Assert.Equal(14, AdminNavigation.SubPages.Count);
        Assert.Contains("admin-logs", AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Users, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Settings, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Nest, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Aliases, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Mail, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Calendar, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Contacts, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Files, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Web, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.Dns, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.BridgesPending, AdminNavigation.SubPages);
        Assert.Contains(AdminNavigation.CustodyHosting, AdminNavigation.SubPages);
        // The removed admin-services page is no longer a shell sub-page.
        Assert.DoesNotContain("admin-services", AdminNavigation.SubPages);
    }
}
