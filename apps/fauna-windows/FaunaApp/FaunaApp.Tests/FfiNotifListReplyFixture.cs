using System;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiNotifListReply"/> with named overrides
/// for just the field(s) a test cares about, instead of every call site
/// hand-listing both positionally. Widening <see cref="FfiNotifListReply"/> now touches exactly this one file.
/// </summary>
internal static class FfiNotifListReplyFixture
{
    internal static FfiNotifListReply Make(
        FfiNotifItem[]? notifications = null,
        long? cursor = null) =>
        new FfiNotifListReply(notifications ?? Array.Empty<FfiNotifItem>(), cursor);
}
