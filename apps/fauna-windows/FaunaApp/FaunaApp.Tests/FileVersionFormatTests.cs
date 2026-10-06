using System;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_media_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>file-version-item</c> row projection (media.md § Element IDs). The XAML
/// code-behind is not reachable from this test assembly, so the projection lives in
/// <see cref="FileVersionFormat"/> — the same reason <c>NestTrustFormat</c> exists.
/// <para>
/// The unit contract is the point of these tests: <c>FileVersionSummary.created_at</c>
/// is epoch <b>milliseconds</b> (<c>libs/fauna-media-machine/src/snapshots.rs</c>), while
/// the sibling <c>media-item-date</c> on the very same page is epoch <b>seconds</b>. No
/// e2e assertion reads a version timestamp, so nothing else guards this.
/// </para>
/// </summary>
public class FileVersionFormatTests
{
    // A fixed epoch so the assertions never depend on wall-clock time.
    private const long NowMs = 1_760_000_000_000L;

    // `authorDisplay` (multi-writer Phase 1 attribution) defaults to a fixed recorder:
    // most of these tests are about the created_at UNIT.
    private static FileVersionSummary Summary(
        long createdAtMs, long sizeBytes = 2048L, long versionNum = 7L, string authorDisplay = "bob",
        bool pruned = false) =>
        new(versionNum, "0badc0de", sizeBytes, createdAtMs, null, authorDisplay, pruned, null);

    [Fact]
    public void MapRow_PassesCreatedAtStraightThrough_BecauseItIsAlreadyEpochMillis()
    {
        var fiveMinutesAgo = NowMs - 300_000L;
        var row = FileVersionFormat.MapRow(Summary(fiveMinutesAgo), NowMs);

        // The shared formatter (fauna_core::format::relative_time) owns the bucket
        // decision; the row must hand it the raw millis, with no unit conversion.
        Assert.Equal(ValueFormat.RelativeTime(NowMs, fiveMinutesAgo), row.Timestamp);
    }

    [Fact]
    public void MapRow_DoesNotReadCreatedAtAsSeconds()
    {
        var fiveMinutesAgo = NowMs - 300_000L;
        var row = FileVersionFormat.MapRow(Summary(fiveMinutesAgo), NowMs);

        // Misreading created_at as seconds (the media-item-date unit) would render a
        // 1970-era absolute date rather than "5m ago". Pin the difference.
        var asIfSeconds = ValueFormat.RelativeTime(NowMs, fiveMinutesAgo / 1000L);
        Assert.NotEqual(asIfSeconds, row.Timestamp);
    }

    [Fact]
    public void MapRow_FormatsSizeThroughTheSharedByteSizeFormatter()
    {
        var row = FileVersionFormat.MapRow(Summary(NowMs, sizeBytes: 2048L), NowMs);
        Assert.Equal(ValueFormat.ByteSize(2048UL), row.Size);
    }

    [Fact]
    public void MapRow_ClampsANegativeSizeToZero_RatherThanWrappingTheUnsignedCast()
    {
        // size_bytes is i64 on the wire; a negative would wrap to ~16 EB under a bare
        // (ulong) cast and render an absurd row.
        var row = FileVersionFormat.MapRow(Summary(NowMs, sizeBytes: -1L), NowMs);
        Assert.Equal(ValueFormat.ByteSize(0UL), row.Size);
    }

    [Fact]
    public void MapRow_CarriesVersionNumVerbatim_SoARestoreClickResolvesItsSummary()
    {
        // version_num is the recording sync_changes row's `seq` — stable identity, never
        // renumbered, and the key the restore button's Tag round-trips through.
        var row = FileVersionFormat.MapRow(Summary(NowMs, versionNum: 41L), NowMs);
        Assert.Equal(41L, row.VersionNum);
    }

    /// <summary>
    /// media.md § Element IDs, <c>file-version-author</c>: <c>author_display</c> renders
    /// through the shared i18n template, not a hand-rolled "Edited by" prefix —
    /// whatever <see cref="Strings.Format"/> produces for <c>media/version_author</c>
    /// is what the row must carry.
    /// </summary>
    [Fact]
    public void MapRow_FormatsAuthorThroughTheSharedI18nTemplate()
    {
        var row = FileVersionFormat.MapRow(Summary(NowMs, authorDisplay: "Alice"), NowMs);
        Assert.Equal(Strings.Format("media/version_author", "Alice"), row.Author);
    }


    /// <summary>Recovery browse (file-versions.md § Retention (3)): the row must carry `pruned` verbatim — it gates
    /// `file-version-pruned-badge`/`file-version-undelete-button`, present only on
    /// pruned rows.</summary>
    [Fact]
    public void MapRow_CarriesPrunedThrough_WhenTrue()
    {
        var row = FileVersionFormat.MapRow(Summary(NowMs, pruned: true), NowMs);
        Assert.True(row.Pruned);
    }

    [Fact]
    public void MapRow_CarriesPrunedThrough_WhenFalse()
    {
        var row = FileVersionFormat.MapRow(Summary(NowMs, pruned: false), NowMs);
        Assert.False(row.Pruned);
    }
}
