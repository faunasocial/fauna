using System;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_media_machine;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Projects a shared <c>FileVersionSummary</c> (the <c>MediaMachine::file_versions</c>
/// reply) into the <see cref="FileVersionRow"/> the <c>file-version-item</c> DataTemplate
/// binds (media.md § Element IDs).
/// <para>
/// It lives here, not in <c>MediaPage.xaml.cs</c>, for the same reason
/// <see cref="NestTrustFormat"/> does: the XAML project's code-behind is not reachable
/// from <c>FaunaApp.Tests</c>, and the one thing worth pinning is a **unit contract** no
/// e2e assertion covers — the version list's timestamps are never read by
/// <c>test_file_version_history_and_restore</c>, only its sizes.
/// </para>
/// <para>
/// <b>The contract:</b> <c>FileVersionSummary.created_at</c> is epoch <b>milliseconds</b>
/// (<c>libs/fauna-media-machine/src/snapshots.rs</c>), whereas <c>MediaItemSummary.updated_at</c>
/// — rendered as <c>media-item-date</c> on the very same page — is epoch <b>seconds</b>.
/// Both units therefore appear in one DataTemplate pair. Pass <c>created_at</c> to
/// <see cref="ValueFormat.RelativeTime"/> <em>unconverted</em>. (linux multiplies by 1000
/// only because its own helper takes <em>micros</em>; web passes it raw, as we do.)
/// </para>
/// The formatting <em>decisions</em> (relative-time buckets, 1024-unit byte sizes) live
/// once in shared Rust — never hand-roll them per client (priority #2).
/// </summary>
internal static class FileVersionFormat
{
    /// <param name="version">One entry of the oldest→newest history.</param>
    /// <param name="nowMs">Current time, epoch milliseconds — injected rather than read
    /// from the clock so the projection is deterministic under test.</param>
    internal static FileVersionRow MapRow(FileVersionSummary version, long nowMs) => new(
        VersionNum: version.@versionNum,
        Timestamp: ValueFormat.RelativeTime(nowMs, version.@createdAt),
        // size_bytes is i64 on the wire; a bare (ulong) cast of a negative would wrap to
        // ~16 EB. Clamp instead of trusting the projection to never emit one.
        Size: ValueFormat.ByteSize((ulong)Math.Max(0L, version.@sizeBytes)),
        Author: Strings.Format("media/version_author", version.@authorDisplay),
        Pruned: version.@pruned);
}
