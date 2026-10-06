using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Serializes the test classes that mutate the process-global <see cref="Strings"/>
/// localizer (this one + <see cref="ValueFormatTests"/>) so their
/// <c>Strings.Initialize</c> calls don't clobber each other under xUnit's
/// default parallel-by-class runner.
/// </summary>
[CollectionDefinition("StringsGlobal", DisableParallelization = true)]
public class StringsGlobalCollection { }

[Collection("StringsGlobal")]
public class StringsResolutionTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    [Fact]
    public void Get_ReturnsMappedValue_AndFallsBackToKey()
    {
        // This is the exact contract LocalizeExtension.ProvideValue relies on.
        Strings.Initialize(new FakeLocalizer(new() { ["onboarding/claim_code/title"] = "Enter claim code" }));

        Assert.Equal("Enter claim code", Strings.Get("onboarding/claim_code/title"));
        Assert.Equal("onboarding/missing/key", Strings.Get("onboarding/missing/key")); // fallback = raw key
    }

    [Fact]
    public void Error_ShowsASharedRefusalAsItsOwnSentence_NotBehindTheHttpErrorWrapper()
    {
        // A shared-Rust refusal's `msg` IS the user-facing sentence. The exception's own
        // Message aggregates it as "@msg=<sentence>", which painted as
        // "HTTP error: @msg=<sentence>" on every VM that reached ShowError.
        Strings.Initialize(new FakeLocalizer(new() { ["errors/http_error"] = "HTTP error: {0}" }));
        const string sentence = "That handle already belongs to someone else on your nest. Choose a different one.";

        Assert.Equal(sentence, Strings.Error(new uniffi.fauna_ffi.FfiException.General(sentence)));
    }

    [Fact]
    public void Error_KeepsTheHttpErrorWrapperForAnyOtherException()
    {
        Strings.Initialize(new FakeLocalizer(new() { ["errors/http_error"] = "HTTP error: {0}" }));

        Assert.Equal("HTTP error: boom", Strings.Error(new InvalidOperationException("boom")));
    }
}
