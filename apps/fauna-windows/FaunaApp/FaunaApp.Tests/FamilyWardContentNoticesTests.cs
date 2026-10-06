using System.Collections.Generic;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>FamilyWardRow.ContentNoticesText</c>/<c>HasContentNotices</c> — the
/// guardian's per-ward Guardian Notify readout (<c>family-ward-content-notices</c>,
/// family-safety.md § Guardian Notify), mapped off <c>FfiFamilyWardInfo.contentNotices</c>
/// exactly like the existing <c>PolicySummary</c> join (<c>FamilyViewModel.LoadAsync</c>).
/// </summary>
[Collection("StringsGlobal")]
public class FamilyWardContentNoticesTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    public FamilyWardContentNoticesTests() => Strings.Initialize(new FakeLocalizer(new()
    {
        ["family/policy_content_spam_label"] = "Spam",
        ["family/policy_content_nsfw_label"] = "NSFW",
        ["family/ward_content_notice_count"] = "{count} flagged",
    }));

    private static FfiFamilyWardInfo Ward(params FfiFamilyContentNotice[] notices) =>
        FfiFamilyWardInfoFixture.Make(contentNotices: notices);

    private static FfiFamilyContentNotice Notice(string category, uint count) => new(category, count);

    [Fact]
    public void NoContentNotices_HasContentNoticesIsFalse_TextIsNull()
    {
        var row = FamilyWardRow.From(Ward());

        Assert.False(row.HasContentNotices);
        Assert.Null(row.ContentNoticesText);
    }

    [Fact]
    public void OneNotice_RendersTheResolvedLabelAndCount()
    {
        var row = FamilyWardRow.From(Ward(Notice("spam", 3)));

        Assert.True(row.HasContentNotices);
        Assert.Equal("Spam: 3 flagged", row.ContentNoticesText);
    }

    [Fact]
    public void MultipleNotices_JoinOneLinePerCategory()
    {
        var row = FamilyWardRow.From(Ward(
            Notice("spam", 3),
            Notice("nsfw", 1)));

        Assert.Equal("Spam: 3 flagged\nNSFW: 1 flagged", row.ContentNoticesText);
    }
}
