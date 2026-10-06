using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailSpamMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="FakeMailAliasesMachine"/>. Records dispatched
/// actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeMailSpamMachine : MailSpamMachineFakeBase
{
    public MailSpamSnapshot NextSnapshot { get; set; } = Empty();
    public List<MailSpamAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailSpamAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override MailSpamSnapshot Snapshot() => NextSnapshot;

    public static MailSpamSnapshot Empty() =>
        new(Array.Empty<SpamTrainingView>(), false, SpamStatus.Idle, null);

    public static MailSpamSnapshot Snap(
        SpamTrainingView[] events, bool contribute = false, string? error = null) =>
        new(events, contribute, SpamStatus.Idle, error);

    public static SpamTrainingView Row(
        string idHex, string message, TrainingLabel label, TrainingSource source,
        long createdAtMs = 1_700_000_000_000) =>
        // Field order matches the UniFFI `SpamTrainingView` record: …, modelDeltaApplied,
        // sealedSubject, mailbox. A plaintext (server-written) row has an empty
        // sealedSubject + empty mailbox (the machine leaves `message` as formatted).
        new(idHex, message, label, source, createdAtMs, Array.Empty<byte>(), Array.Empty<byte>(), "");
}

/// <summary>
/// Deterministic unit tests for the <c>mail-spam</c> page VM, over the
/// <see cref="FakeMailSpamMachine"/> (the UniFFI <c>IMailSpamMachine</c> seam — the
/// e2e flow made deterministic; no live nest / FlaUI, which flakes on windows).
/// Covers the snapshot projection (rows, label/source badges, the contribute flag)
/// and that each user action dispatches the right <c>MailSpamAction</c>.
/// </summary>
public class MailSpamViewModelTests
{
    private static SpamTrainingView[] SeededRows() => new[]
    {
        FakeMailSpamMachine.Row("01", "Cheap pills — INBOX", TrainingLabel.Spam, TrainingSource.ExplicitButton),
        FakeMailSpamMachine.Row("02", "Lunch? — INBOX", TrainingLabel.Ham, TrainingSource.ImapJunkMove),
    };

    [Fact]
    public async Task Load_ProjectsRowsAndFlag()
    {
        var fake = new FakeMailSpamMachine
        {
            NextSnapshot = FakeMailSpamMachine.Snap(SeededRows(), contribute: true),
        };
        var vm = new MailSpamViewModel(fake, new MockNestRpcClient());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Equal(2, vm.Events.Count);
        Assert.Equal("01", vm.Events[0].HistoryIdHex);
        Assert.Equal("Cheap pills — INBOX", vm.Events[0].Message);
        // Label / source → badge via the shared training_{label,source}_badge formatters
        // (no localizer in the test host → Strings.Resolve falls back to the canonical dotted key).
        Assert.Equal("mail_spam.label_spam", vm.Events[0].LabelBadge);
        Assert.Equal("mail_spam.source_explicit_button", vm.Events[0].SourceBadge);
        Assert.Equal("mail_spam.label_ham", vm.Events[1].LabelBadge);
        Assert.True(vm.ContributeBaseline);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task ResetModel_DispatchesResetModel()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(SeededRows()) };
        var vm = new MailSpamViewModel(fake, new MockNestRpcClient());
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ResetModelAsync();

        Assert.Contains(fake.Dispatched, a => a is MailSpamAction.ResetModel);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SetContributeBaseline_DispatchesWithFlag()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var vm = new MailSpamViewModel(fake, new MockNestRpcClient());
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.ContributeBaseline);

        // The toggle's success re-reads with the flag persisted.
        fake.NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>(), contribute: true);
        await vm.SetContributeBaselineAsync(true);

        Assert.Contains(fake.Dispatched, a => a is MailSpamAction.SetContributeBaseline { contribute: true });
        Assert.True(vm.ContributeBaseline);
    }

    [Fact]
    public async Task UndoTraining_DispatchesWithHexId()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(SeededRows()) };
        var vm = new MailSpamViewModel(fake, new MockNestRpcClient());
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.UndoTrainingAsync("01");

        Assert.Contains(fake.Dispatched, a => a is MailSpamAction.UndoTraining { historyIdHex: "01" });
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(SeededRows()) };
        var vm = new MailSpamViewModel(fake, new MockNestRpcClient());
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The VM
        // swallows the throw and surfaces the error rather than letting it escape.
        fake.NextError = "boom";
        await vm.ResetModelAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    // ── Report-share transparency pane (report-sharing.md § Client wire) ──
    // A small dedicated flow over MockNestRpcClient, not the machine — the
    // fauna.moderation.report_share.{set,status} peer of the machine dispatch tests
    // above.

    [Fact]
    public async Task Load_ProjectsReportShareToggleAndPublishedList()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient
        {
            NextReportShareStatus = FfiReportShareStatusFixture.Make(
                published: new[] { FfiReportShareStatusFixture.Entry("ab12", "report:spam", 3u) }),
        };
        var vm = new MailSpamViewModel(fake, rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("ModerationReportShareStatus", rpc.Calls);
        Assert.True(vm.ShareReports);
        Assert.Single(vm.PublishedReports);
        Assert.Equal("ab12", vm.PublishedReports[0].ContentHash);
        Assert.Equal("report:spam", vm.PublishedReports[0].Factor);
        Assert.Equal("3", vm.PublishedReports[0].Count);
    }

    [Fact]
    public async Task SetShareReports_RoundTripsAndRefreshesPublishedList()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient();
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.ShareReports);
        Assert.Empty(vm.PublishedReports);

        // The nest's status re-read reflects the persisted opt-in + a newly-published
        // aggregate (mirrors the toggle crossing the k-anonymity floor).
        rpc.NextReportShareStatus = FfiReportShareStatusFixture.Make(
            published: new[] { FfiReportShareStatusFixture.Entry("cd34", "report:spam", 5u) });
        await vm.SetShareReportsAsync(true);

        Assert.Equal(true, rpc.LastReportShareSet);
        Assert.True(vm.ShareReports);
        Assert.Single(vm.PublishedReports);
        Assert.Equal("cd34", vm.PublishedReports[0].ContentHash);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SetShareReports_Failure_RoutesToError()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient();
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.SetShareReportsAsync(true);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    // ── Per-account spam-threshold override (mail-policy-config.md § Tier 3) ──
    // A plain read/write over MockNestRpcClient, not the machine — same shape as
    // the report-share pane tests above.

    [Fact]
    public async Task Load_ProjectsThresholdOverride()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient { NextSpamThresholdOverride = 5 };
        var vm = new MailSpamViewModel(fake, rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("SpamThresholdOverrideGet", rpc.Calls);
        Assert.Equal("5", vm.ThresholdOverrideText);
    }

    [Fact]
    public async Task Load_NoOverride_RendersEmptyNotZero()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient { NextSpamThresholdOverride = null };
        var vm = new MailSpamViewModel(fake, rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(string.Empty, vm.ThresholdOverrideText);
    }

    [Fact]
    public async Task CommitThresholdOverride_ZeroIsARealSettingNotUnset()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient();
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.ThresholdOverrideText = "0";
        await vm.CommitThresholdOverrideAsync();

        Assert.Equal((uint?)0, rpc.LastSpamThresholdOverrideSet);
        Assert.Equal("0", vm.ThresholdOverrideText);
    }

    [Fact]
    public async Task CommitThresholdOverride_EmptyClearsTheOverride()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient { NextSpamThresholdOverride = 5 };
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Equal("5", vm.ThresholdOverrideText);

        vm.ThresholdOverrideText = "";
        await vm.CommitThresholdOverrideAsync();

        Assert.Null(rpc.LastSpamThresholdOverrideSet);
        Assert.Equal(string.Empty, vm.ThresholdOverrideText);
    }

    [Fact]
    public async Task CommitThresholdOverride_ReflectsThePersistedValueNotTheKeystroke()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient();
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        // An unparseable edit falls back to whatever the nest confirms, never the
        // local keystroke — the same "never trust the echo" rule as ShareReports.
        vm.ThresholdOverrideText = "not a number";
        await vm.CommitThresholdOverrideAsync();

        Assert.Null(rpc.LastSpamThresholdOverrideSet);
        Assert.Equal(string.Empty, vm.ThresholdOverrideText);
    }

    [Fact]
    public async Task CommitThresholdOverride_Failure_RoutesToError()
    {
        var fake = new FakeMailSpamMachine { NextSnapshot = FakeMailSpamMachine.Snap(Array.Empty<SpamTrainingView>()) };
        var rpc = new MockNestRpcClient();
        var vm = new MailSpamViewModel(fake, rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        vm.ThresholdOverrideText = "7";
        await vm.CommitThresholdOverrideAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
