using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the shared thread-label display fallback
/// (<c>fauna_core::format::thread_label_display</c> via the value-format FFI
/// wrapper; conversations.md § Where logic lives → Thread label display). These
/// call the REAL UniFFI export (the native <c>fauna_ffi</c> dll loads in the test
/// host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>), locking
/// windows to the single source of truth: a blank/whitespace label → the canonical
/// <c>conversations.detail.no_subject</c> key, a non-empty label rides verbatim.
/// Replaces the prior hardcoded untranslated literal <c>"(no label)"</c> the
/// conversation list-row + thread-header both used (priority #1).
/// </summary>
public class ThreadLabelDisplayTests
{
    // FFI contract — localizer-independent (asserts the LocalizedText key directly).
    [Theory]
    [InlineData("", "conversations.detail.no_subject")]
    [InlineData("   ", "conversations.detail.no_subject")]
    [InlineData("Project X", "Project X")]
    [InlineData("v1.2.3", "v1.2.3")]
    public void ThreadLabelDisplay_BlankFallsBackToNoSubject_NonEmptyVerbatim(string label, string expectedKey)
    {
        Assert.Equal(expectedKey, FaunaFfiMethods.ThreadLabelDisplay(label).@key);
    }

    // Windows render path — the exact expression the list-row (ThreadRow.Label) and
    // the thread-header (RefreshDetailView → SetLabel) paint. The test host has no
    // localizer mapping these keys, so a blank label resolves to the dotted key and
    // a non-empty label rides verbatim — including one with dots, which Resolve's
    // slash-fallback restores to the original (the subtle passthrough guard).
    [Theory]
    [InlineData("", "conversations.detail.no_subject")]
    [InlineData("Project X", "Project X")]
    [InlineData("v1.2.3", "v1.2.3")]
    public void ThreadLabelDisplay_ResolvesThroughWindowsPipeline(string label, string expected)
    {
        Assert.Equal(expected, Strings.Resolve(FaunaFfiMethods.ThreadLabelDisplay(label)));
    }
}
