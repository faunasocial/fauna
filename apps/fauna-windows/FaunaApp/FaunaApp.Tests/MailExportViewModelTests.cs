using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_mail;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailExportMachine"/> for view-model unit tests. Records
/// dispatched actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeMailExportMachine : MailExportMachineFakeBase
{
    public MailExportSnapshot NextSnapshot { get; set; } = AtFormat();
    /// <summary>The snapshot a successful <see cref="Dispatch"/> leaves behind, when set
    /// — lets a test model "Start accepted → session Running" or "Start rejected".</summary>
    public MailExportSnapshot? SnapshotAfterDispatch { get; set; }
    /// <summary>The snapshot <see cref="RunExport"/> leaves behind (the loop's own end state).</summary>
    public MailExportSnapshot? SnapshotAfterRun { get; set; }
    public List<MailExportAction> Dispatched { get; } = new();
    /// <summary>Every call in order: <c>handle:&lt;h&gt;</c>, <c>dispatch:&lt;Action&gt;</c>, <c>run</c>.</summary>
    public List<string> Calls { get; } = new();
    public string? NextError { get; set; }
    public string? RunError { get; set; }
    public int HydrateCalls { get; private set; }
    public int RunCalls { get; private set; }

    public override void SetActorHandle(string handle) => Calls.Add($"handle:{handle}");

    public override Task RunExport()
    {
        RunCalls++;
        Calls.Add("run");
        if (SnapshotAfterRun is not null) NextSnapshot = SnapshotAfterRun;
        if (RunError is not null) throw new InvalidOperationException(RunError);
        return Task.CompletedTask;
    }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailExportAction action)
    {
        Dispatched.Add(action);
        Calls.Add($"dispatch:{action.GetType().Name}");
        if (NextError is not null) throw new InvalidOperationException(NextError);
        if (SnapshotAfterDispatch is not null) NextSnapshot = SnapshotAfterDispatch;
        return Task.CompletedTask;
    }

    public override MailExportSnapshot Snapshot() => NextSnapshot;

    public static MailboxOption Box(string name, bool selected) => new(name, selected);

    /// <summary>A Format-step snapshot with the given mailbox options.</summary>
    public static MailExportSnapshot AtFormat(
        MailboxOption[]? mailboxes = null, ExportFormat format = ExportFormat.Mbox,
        string? error = null) =>
        new(ExportStep.Format, format, mailboxes ?? Array.Empty<MailboxOption>(),
            "", "", false, null, 0, 0, 0, 0,
            Array.Empty<MailboxProgressView>(), Array.Empty<string>(), null, "",
            "", ExportStatus.Idle, error);

    /// <summary>A Progress-step snapshot for the running/paused session projection.</summary>
    public static MailExportSnapshot AtProgress(
        ExportSessionState state, uint exported, uint total,
        MailboxProgressView[]? progress = null) =>
        new(ExportStep.Progress, ExportFormat.Mbox, Array.Empty<MailboxOption>(),
            "", "", false, state, exported, 0, 0, total,
            progress ?? Array.Empty<MailboxProgressView>(), Array.Empty<string>(), null, "",
            "", ExportStatus.Idle, null);

    /// <summary>A Done-step snapshot; <paramref name="savedPath"/> non-empty once Download ran.</summary>
    public static MailExportSnapshot AtDone(ulong blobBytes, string savedPath = "") =>
        new(ExportStep.Done, ExportFormat.Mbox, Array.Empty<MailboxOption>(),
            "", "", false, ExportSessionState.Completed, 3, 0, 0, 3,
            Array.Empty<MailboxProgressView>(), Array.Empty<string>(), blobBytes, "/api/v1/export/x",
            savedPath, ExportStatus.Idle, null);
}

/// <summary>
/// Deterministic unit tests for the <c>mail-export</c> wizard VM, over the
/// <see cref="FakeMailExportMachine"/> (the UniFFI <c>IMailExportMachine</c> seam — the
/// e2e flow made deterministic; no live nest / FlaUI). Covers the snapshot projection
/// (step mirror, format index, mailbox rows, progress summary/fraction, pause/resume
/// gating) and that each user action dispatches the right <c>MailExportAction</c>. The
/// wizard FSM itself is unit-tested in the Rust export::tests.
/// </summary>
public class MailExportViewModelTests
{
    [Fact]
    public async Task Load_ProjectsFormatStepAndMailboxes()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtFormat(new[]
            {
                FakeMailExportMachine.Box("INBOX", true),
                FakeMailExportMachine.Box("Trash", false),
            }),
        };
        var vm = new MailExportViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Equal(MailExportStep.Format, vm.Step);
        Assert.Equal(0, vm.FormatIndex);
        Assert.Equal(2, vm.Mailboxes.Count);
        Assert.Equal("INBOX", vm.Mailboxes[0].Name);
        Assert.True(vm.Mailboxes[0].Selected);
        Assert.False(vm.Mailboxes[1].Selected);
        // The mail-export-scope-mailbox-item's `state` attribute (AutomationProperties.HelpText),
        // mirroring MailImportMailboxRow.State — the cross-app mailbox_selected action reads
        // this, never the checkbox glyph.
        Assert.Equal("on", vm.Mailboxes[0].State);
        Assert.Equal("off", vm.Mailboxes[1].State);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SelectFormat_DispatchesEmlZip()
    {
        var fake = new FakeMailExportMachine();
        var vm = new MailExportViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SelectFormatAsync(2);

        Assert.Contains(fake.Dispatched, a => a is MailExportAction.SelectFormat { format: ExportFormat.EmlZip });
    }

    [Fact]
    public async Task ToggleMailbox_And_Navigation_Dispatch()
    {
        var fake = new FakeMailExportMachine();
        var vm = new MailExportViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ToggleMailboxAsync("Work");
        await vm.NextAsync();
        await vm.StartAsync();

        Assert.Contains(fake.Dispatched, a => a is MailExportAction.ToggleMailbox { mailbox: "Work" });
        Assert.Contains(fake.Dispatched, a => a is MailExportAction.Next);
        Assert.Contains(fake.Dispatched, a => a is MailExportAction.Start);
    }

    [Fact]
    public async Task Progress_ProjectsSummaryFractionAndPauseGate()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtProgress(
                ExportSessionState.Running, exported: 25, total: 100,
                progress: new[] { new MailboxProgressView("INBOX", 25, 100) }),
        };
        var vm = new MailExportViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(MailExportStep.Progress, vm.Step);
        Assert.Equal(0.25, vm.ProgressFraction, 3);
        Assert.True(vm.CanPause);
        Assert.False(vm.CanResume);
        Assert.Single(vm.MailboxProgress);
        Assert.Equal("25/100", vm.MailboxProgress[0].Progress);
    }

    [Fact]
    public async Task Paused_GatesResumeNotPause()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtProgress(ExportSessionState.Paused, 25, 100),
        };
        var vm = new MailExportViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.CanPause);
        Assert.True(vm.CanResume);
    }

    // ── The drive half (mail-export.md § Implementation status today: custody and the
    //    run_export spawn land together; the VM spawns, the machine never self-spawns) ──

    [Fact]
    public async Task Start_ThatLandsRunning_RefreshesHandleThenArmsTheDriveLoop()
    {
        var fake = new FakeMailExportMachine
        {
            SnapshotAfterDispatch = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 0, 3),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.StartAsync();

        // The handle names the archive; it is read at the gesture, BEFORE the dispatch.
        Assert.Equal(new[] { "handle:alice", "dispatch:Start" }, fake.Calls);
        Assert.True(vm.ShouldDriveExport);
    }

    [Fact]
    public async Task Start_ThatIsRejected_NeverArmsTheDriveLoop()
    {
        // A rejected Start leaves no Running session: spawning run_export on it would
        // drive nothing while hiding the rejection behind a Progress screen.
        var fake = new FakeMailExportMachine
        {
            SnapshotAfterDispatch = FakeMailExportMachine.AtFormat(error: "not yet available"),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.StartAsync();

        Assert.False(vm.ShouldDriveExport);
        Assert.Equal("not yet available", vm.Error);
    }

    [Fact]
    public async Task Resume_ThatLandsRunning_ArmsTheDriveLoop()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtProgress(ExportSessionState.Paused, 1, 3),
            SnapshotAfterDispatch = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 1, 3),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ResumeAsync();

        Assert.Equal(new[] { "handle:alice", "dispatch:Resume" }, fake.Calls);
        Assert.True(vm.ShouldDriveExport);
    }

    [Fact]
    public async Task Pause_NeverArmsTheDriveLoop()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 1, 3),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.PauseAsync();

        Assert.False(vm.ShouldDriveExport);
        Assert.DoesNotContain("handle:alice", fake.Calls);
    }

    [Fact]
    public async Task DriveExport_RunsTheLoop_ThenDisarmsAndReprojects()
    {
        var fake = new FakeMailExportMachine
        {
            SnapshotAfterDispatch = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 0, 3),
            SnapshotAfterRun = FakeMailExportMachine.AtDone(blobBytes: 4096),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);
        await vm.StartAsync();

        await vm.DriveExportAsync();

        Assert.Equal(1, fake.RunCalls);
        Assert.False(vm.ShouldDriveExport);
        Assert.Equal(MailExportStep.Done, vm.Step);
    }

    [Fact]
    public async Task DriveExport_Failure_RoutesToError()
    {
        var fake = new FakeMailExportMachine { RunError = "loop died" };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.DriveExportAsync();

        Assert.False(vm.ShouldDriveExport);
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task Repaint_ReprojectsWithoutDispatching()
    {
        var fake = new FakeMailExportMachine
        {
            NextSnapshot = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 0, 4),
        };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextSnapshot = FakeMailExportMachine.AtProgress(ExportSessionState.Running, 2, 4);
        vm.Repaint();

        Assert.Equal(0.5, vm.ProgressFraction, 3);
        Assert.Empty(fake.Dispatched);
    }

    [Fact]
    public async Task Download_RefreshesHandleThenDispatchesDownload()
    {
        var fake = new FakeMailExportMachine { NextSnapshot = FakeMailExportMachine.AtDone(4096) };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.DownloadAsync();

        Assert.Equal(new[] { "handle:alice", "dispatch:Download" }, fake.Calls);
        Assert.False(vm.ShouldDriveExport);
    }

    [Fact]
    public async Task EmptyHandle_IsNeverPushed()
    {
        // A fresh sign-in may not know its handle yet; pushing "" would name the
        // archive after nobody. The machine keeps whatever it had.
        var fake = new FakeMailExportMachine { NextSnapshot = FakeMailExportMachine.AtDone(4096) };
        var vm = new MailExportViewModel(fake, () => "");
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.DownloadAsync();

        Assert.Equal(new[] { "dispatch:Download" }, fake.Calls);
    }

    [Fact]
    public async Task DoneSummary_NamesWhereTheArchiveWasSaved()
    {
        const string saved = @"C:\Users\u\Downloads\fauna-export-alice-mbox-2026-09-26.zip.zst";
        var fake = new FakeMailExportMachine { NextSnapshot = FakeMailExportMachine.AtDone(4096) };
        var vm = new MailExportViewModel(fake, () => "alice");
        await vm.LoadCommand.ExecuteAsync(null);
        // No string table under unit test: Strings.Format answers the key it resolved,
        // which is exactly the choice under test.
        Assert.Equal("mail_export/done_summary_fmt", vm.DoneSummary);

        fake.NextSnapshot = FakeMailExportMachine.AtDone(4096, saved);
        vm.Repaint();

        // The visible answer to the Download press (mail_export.saved_summary_fmt).
        Assert.Equal("mail_export/saved_summary_fmt", vm.DoneSummary);
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailExportMachine();
        var vm = new MailExportViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextError = "boom";
        await vm.StartAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
