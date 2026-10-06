using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiReportShareStatus"/> (and the
/// <see cref="FfiReportShareEntry"/> rows it carries) with named overrides,
/// instead of every call site hand-listing both positionally. Widening either record now touches exactly
/// this one file. Shared between <c>SignalShareViewModelTests</c> and
/// <c>MailSpamViewModelTests</c>, which independently construct the same shape.
/// </summary>
internal static class FfiReportShareStatusFixture
{
    internal static FfiReportShareStatus Make(
        bool share = true,
        FfiReportShareEntry[]? published = null) =>
        new FfiReportShareStatus(share, published ?? System.Array.Empty<FfiReportShareEntry>());

    internal static FfiReportShareEntry Entry(
        string contentHash,
        string factor,
        uint count = 1) =>
        new FfiReportShareEntry(contentHash, factor, count);
}
