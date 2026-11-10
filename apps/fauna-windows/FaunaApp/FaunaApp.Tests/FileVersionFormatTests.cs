using System;
using System.Collections.Generic;
using System.Threading.Tasks;
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
        bool pruned = false, string? depositedBy = null) =>
        new(versionNum, "0badc0de", sizeBytes, createdAtMs, null, authorDisplay, pruned, null,
            depositedBy: depositedBy);

    // A connected app's hex writer key, as `FileVersionSummary.deposited_by` carries it.
    private const string Depositor = "ab0000000000000000000000000000000000000000000000000000000000cd";

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

    // ── The deposited version's author line (folder-deposit.md ruling 3) ──────────
    // The wording is composed once in shared Rust (`ConnectedAppsMachine::
    // version_author`); the row only resolves what the machine returns, and with no
    // machine falls back to the recorder alone or the generic depositor wording, as
    // tui's `version_author` and linux's do.

    [Fact]
    public void MapRow_WithNoMachine_ADepositedVersionReadsTheGenericDepositorWording()
    {
        var row = FileVersionFormat.MapRow(
            Summary(NowMs, authorDisplay: "Alice", depositedBy: Depositor), NowMs);
        Assert.Equal(Strings.Format("media/version_author_deposited_unknown", "Alice"), row.Author);
        Assert.NotEqual(Strings.Format("media/version_author", "Alice"), row.Author);
    }

    [Fact]
    public void MapRow_WithNoMachine_AnOrdinaryVersionReadsTheRecorderAlone()
    {
        var row = FileVersionFormat.MapRow(Summary(NowMs, authorDisplay: "Alice"), NowMs, apps: null);
        Assert.Equal(Strings.Format("media/version_author", "Alice"), row.Author);
    }

    [Fact]
    public void MapRow_WithAMachine_PaintsTheLineTheSharedMachineComposes_NamingTheApp()
    {
        var apps = new FakeDepositorApps
        {
            Line = new uniffi.fauna_core.LocalizedText(
                "media.version_author_deposited",
                new Dictionary<string, string> { ["app"] = "Photo Sync", ["author"] = "Alice" }),
        };
        var row = FileVersionFormat.MapRow(
            Summary(NowMs, authorDisplay: "Alice", depositedBy: Depositor), NowMs, apps);

        Assert.Equal(Strings.Resolve(apps.Line), row.Author);
        // The machine is handed the recorder and the writer key verbatim: no app
        // matches a writer key itself.
        Assert.Equal(("Alice", (string?)Depositor), apps.Asked);
    }

    [Fact]
    public async Task ReadDepositorRoster_ReadsTheRosterOnlyWhenTheMachineWantsIt()
    {
        var versions = new[]
        {
            Summary(NowMs, versionNum: 1L),
            Summary(NowMs, versionNum: 2L, depositedBy: Depositor),
        };
        var keys = FileVersionFormat.DepositorKeys(versions);
        Assert.Equal(new[] { Depositor }, keys);

        var wanting = new FakeDepositorApps { Wanted = true };
        Assert.True(await FileVersionFormat.ReadDepositorRosterAsync(wanting, keys));
        Assert.Equal(new[] { Depositor }, wanting.WantedAsked);
        Assert.Equal(1, wanting.Refreshes);

        var satisfied = new FakeDepositorApps { Wanted = false };
        Assert.False(await FileVersionFormat.ReadDepositorRosterAsync(satisfied, keys));
        Assert.Equal(0, satisfied.Refreshes);
    }

    [Fact]
    public async Task ReadDepositorRoster_AsksNothingWhenNoVersionNamesADepositorOrThereIsNoMachine()
    {
        var keys = FileVersionFormat.DepositorKeys(new[] { Summary(NowMs) });
        Assert.Empty(keys);

        var apps = new FakeDepositorApps { Wanted = true };
        Assert.False(await FileVersionFormat.ReadDepositorRosterAsync(apps, keys));
        Assert.Null(apps.WantedAsked);
        Assert.Equal(0, apps.Refreshes);

        Assert.False(await FileVersionFormat.ReadDepositorRosterAsync(null, new[] { Depositor }));
    }

    private sealed class FakeDepositorApps : uniffi.fauna_client_connected_apps.ConnectedAppsMachineFakeBase
    {
        public uniffi.fauna_core.LocalizedText Line { get; init; } =
            new("media.version_author", new Dictionary<string, string> { ["author"] = "?" });
        public bool Wanted { get; init; }
        public (string, string?)? Asked { get; private set; }
        public string[]? WantedAsked { get; private set; }
        public int Refreshes { get; private set; }

        public override uniffi.fauna_core.LocalizedText VersionAuthor(string authorDisplay, string? depositedBy)
        {
            Asked = (authorDisplay, depositedBy);
            return Line;
        }

        public override bool DepositorReadWanted(string[] depositedBy)
        {
            WantedAsked = depositedBy;
            return Wanted;
        }

        public override Task Refresh()
        {
            Refreshes++;
            return Task.CompletedTask;
        }
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
