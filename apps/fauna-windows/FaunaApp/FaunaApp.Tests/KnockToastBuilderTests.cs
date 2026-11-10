using FaunaApp.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The knock toast's payload (`notifications.md` § Localized body → The knock
/// toast): <c>AddText</c> escapes toast XML itself, and every knock shares one tag.
/// </summary>
public class KnockToastBuilderTests
{
    [Fact]
    public void Build_EscapesMarkupInTheKnockersText()
    {
        var toast = KnockToastBuilder.Build("Knock", "a <b> & \"q\"");

        Assert.Contains("<text>a &lt;b&gt; &amp; &quot;q&quot;</text>", toast.Payload);
    }

    [Fact]
    public void Build_SharesOneTagAcrossKnocks()
    {
        var first = KnockToastBuilder.Build("Knock", "abcd1234 wants to connect");
        var second = KnockToastBuilder.Build("Knock", "ffff0000 wants to connect");

        Assert.Equal(KnockToastBuilder.Tag, first.Tag);
        Assert.Equal(first.Tag, second.Tag);
    }

    /// <summary>The one tag is spelled from the shared replace id
    /// (<c>knock_toast_replace_id</c>), never a value of windows' own — the id the
    /// knock-toast log records as <c>replace_id</c>, as web and apple spell theirs
    /// (<c>notifications.md</c> § Localized body, the knock toast).</summary>
    [Fact]
    public void Tag_IsSpelledFromTheSharedReplaceId()
    {
        Assert.Equal($"fauna-knock-{FaunaFfiMethods.KnockToastReplaceId()}", KnockToastBuilder.Tag);
        Assert.Equal("fauna-knock-1003", KnockToastBuilder.Tag);
    }
}
