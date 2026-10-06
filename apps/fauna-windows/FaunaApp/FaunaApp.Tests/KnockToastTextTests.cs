using FaunaApp.Core.Services;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="KnockToastText.For"/> — the shared <c>knock_text_for</c>
/// decision resolved through windows' RESW pipeline
/// (`notifications.md` § Localized body → *The knock toast*).
/// </summary>
[Collection("StringsGlobal")]
public class KnockToastTextTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static void UseRealKnockStrings() => Strings.Initialize(new FakeLocalizer(new()
    {
        ["notifications/knock_title"] = "New Contact Request",
        ["notifications/knock_body"] = "{name} wants to connect",
        ["notifications/row_knock"] = "{sender} wants to connect: {message}",
    }));

    [Fact]
    public void KnownKeyKnock_PaintsTheRowsSentence()
    {
        UseRealKnockStrings();
        var body = new LocalizedText("notifications.row_knock",
            new Dictionary<string, string> { ["sender"] = "abcd1234", ["message"] = "hi there" });
        var knock = MockNestRpcClient.MakeKnock("abcd1234deadbeef", "hi there", body);

        var (title, text) = KnockToastText.For(knock);

        Assert.Equal("New Contact Request", title);
        Assert.Equal("abcd1234 wants to connect: hi there", text);
    }

    [Fact]
    public void BodylessKnock_PaintsTheKnockBodySentence_NeverTheRawSummary()
    {
        UseRealKnockStrings();
        // A knock this build's catalog can't localize carries
        // no body — the toast falls back to its own sentence naming the sender, never
        // `summary` (the knocker's raw message) painted alone.
        var knock = MockNestRpcClient.MakeKnock("abcd1234deadbeef", "please add me!!!", body: null);

        var (title, text) = KnockToastText.For(knock);

        Assert.Equal("New Contact Request", title);
        Assert.Equal("abcd1234 wants to connect", text);
        Assert.DoesNotContain("please add me", text);
    }

    [Fact]
    public void MessageWithControlCharacters_IsStrippedBeforePainting()
    {
        // The shared decision (`knock_text_for`) reduces `notifications.row_knock`'s
        // raw `{message}` arg to plain single-line text — a newline becomes a
        // space, other control characters drop — so a knocker can't forge
        // multi-line structure into a single-line toast surface.
        UseRealKnockStrings();
        var body = new LocalizedText("notifications.row_knock",
            new Dictionary<string, string> { ["sender"] = "abcd1234", ["message"] = "hi\nthere\u0007" });
        var knock = MockNestRpcClient.MakeKnock("abcd1234deadbeef", "hi\nthere\u0007", body);

        var (_, text) = KnockToastText.For(knock);

        Assert.Equal("abcd1234 wants to connect: hi there", text);
        Assert.DoesNotContain('\n', text);
        Assert.DoesNotContain('\u0007', text);
    }
}
