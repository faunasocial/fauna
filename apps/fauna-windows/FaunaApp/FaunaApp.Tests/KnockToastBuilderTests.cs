using FaunaApp.Services;
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
}
