using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The post-succession aftermath's per-leg progress store
/// (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The post-succession
/// aftermath's progress lines</i>): the sink records each leg's already-resolved
/// line, <c>null</c> clears it (a leg owing nothing must not leave an earlier
/// pass's line painted), and a new pass starts from nothing.
/// </summary>
[Collection(AftermathProgressCollection.Name)]
public sealed class AftermathProgressTests
{
    private static LocalizedText Line(string key) => new(key, new Dictionary<string, string>());

    [Fact]
    public void SinkRecordsEachLegsLine()
    {
        AftermathProgress.Reset();
        var sink = new LoggingAftermathSink();

        sink.Progress(FfiAftermathLeg.BackupRegrant, Line("settings.recovery_kit.backup_regrant_done"));

        Assert.Equal("settings.recovery_kit.backup_regrant_done",
            AftermathProgress.Line(FfiAftermathLeg.BackupRegrant)?.key);
        Assert.Null(AftermathProgress.Line(FfiAftermathLeg.DraftsReseal));
    }

    [Fact]
    public void NullLineClearsAnEarlierOne()
    {
        AftermathProgress.Reset();
        var sink = new LoggingAftermathSink();
        sink.Progress(FfiAftermathLeg.DraftsReseal, Line("settings.recovery_kit.drafts_reseal_running"));

        sink.Progress(FfiAftermathLeg.DraftsReseal, null);

        Assert.Null(AftermathProgress.Line(FfiAftermathLeg.DraftsReseal));
    }

    [Fact]
    public void EveryRecordRaisesChanged()
    {
        AftermathProgress.Reset();
        var raised = 0;
        void OnChanged() => raised++;
        AftermathProgress.Changed += OnChanged;
        try
        {
            var sink = new LoggingAftermathSink();
            sink.Progress(FfiAftermathLeg.MailBurn, Line("k"));
            sink.Progress(FfiAftermathLeg.MailBurn, null);
        }
        finally
        {
            AftermathProgress.Changed -= OnChanged;
        }

        Assert.Equal(2, raised);
    }

    [Fact]
    public void ResetForgetsEveryLeg()
    {
        var sink = new LoggingAftermathSink();
        sink.Progress(FfiAftermathLeg.GrantRemint, Line("k"));

        AftermathProgress.Reset();

        Assert.Null(AftermathProgress.Line(FfiAftermathLeg.GrantRemint));
    }
}

/// <summary>The store is process-wide, so its tests never run in parallel.</summary>
[CollectionDefinition(Name, DisableParallelization = true)]
public sealed class AftermathProgressCollection
{
    public const string Name = "AftermathProgress";
}
