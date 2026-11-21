using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using uniffi.fauna_log;
using uniffi.fauna_media_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows reading of the shared <c>Possibility</c> answer
/// (<see cref="PossibilityPaint"/>; <c>docs/goal/ui/README.md</c> § Offer only what is
/// possible) and every gate painted from it: the Logs pages' copy / clear
/// (<c>ring_action_possibility</c>), the copy confirmations and their failures, the
/// rule editor's save, the offline-share begin / expect, and the share-link button.
/// Over the REAL native <c>fauna_ffi</c> dll (the predicates are shared Rust); every
/// expected string is resolved through <see cref="Strings"/> at assertion time, so the
/// tests hold whichever localizer the process carries.
/// </summary>
[Collection("StringsGlobal")]
public class PossibilityPaintTests
{
    private static LocalizedText Key(string key) => new(key, new Dictionary<string, string>());

    // ── The one reading ──────────────────────────────────────────────────

    [Fact]
    public void Possible_IsShownAndEnabled_WithNoReason()
    {
        var paint = PossibilityPaint.Of(new Possibility.Possible());
        Assert.True(paint.Shown);
        Assert.True(paint.Enabled);
        Assert.True(paint.IsPossible);
        Assert.Null(paint.Reason);
    }

    [Fact]
    public void Blocked_IsShownDisabled_WithTheResolvedReason()
    {
        var paint = PossibilityPaint.Of(new Possibility.Blocked(Key("logs.empty")));
        Assert.True(paint.Shown);
        Assert.False(paint.Enabled);
        Assert.False(paint.IsPossible);
        Assert.Equal(Strings.Resolve(Key("logs.empty")), paint.Reason);
    }

    [Fact]
    public void Absent_IsNotShown()
    {
        var paint = PossibilityPaint.Of(new Possibility.Absent());
        Assert.False(paint.Shown);
        Assert.False(paint.IsPossible);
        Assert.Null(paint.Reason);
    }

    // ── Settings → Logs ──────────────────────────────────────────────────

    private sealed class FakeLogRing : ILogRing
    {
        public readonly List<LogEntry> Backing = new();
        public int ClearCount;

        public IReadOnlyList<LogEntry> Snapshot(LogLevel? min)
            => min is LogLevel m ? Backing.Where(e => (int)e.level <= (int)m).ToList() : Backing.ToList();

        public void Clear()
        {
            ClearCount++;
            Backing.Clear();
        }
    }

    [Fact]
    public void DeviceLogs_EmptyRing_BlocksCopyAndClear_ByThePagesEmptyLine()
    {
        var ring = new FakeLogRing();
        var vm = new LogsViewModel(ring);
        vm.Load();

        var answer = vm.RingAction();
        Assert.True(answer.Shown);
        Assert.False(answer.Enabled);
        Assert.Equal(Strings.Resolve(Key("logs.empty")), answer.Reason);
        // The actions ask the same answer and refuse.
        Assert.Null(vm.CopyPayload());
        vm.Clear();
        Assert.Equal(0, ring.ClearCount);
    }

    [Fact]
    public void DeviceLogs_FilterShowingNothing_BlocksEvenWithEntriesInTheRing()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(new LogEntry(1, LogLevel.Info, "fauna_app", "startup"));
        var vm = new LogsViewModel(ring);
        vm.Load();
        Assert.True(vm.RingAction().IsPossible);

        vm.SetFilter(1); // Error only — the Info entry is not shown
        Assert.False(vm.RingAction().IsPossible);
    }

    [Fact]
    public void DeviceLogs_ConfirmedCopy_PaintsCopied_AndAFailedOneSaysSoOnTheError()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(new LogEntry(1, LogLevel.Info, "fauna_app", "startup"));
        var vm = new LogsViewModel(ring);
        vm.Load();
        var text = vm.CopyPayload();
        Assert.NotNull(text);

        vm.RecordCopyOutcome(text!, failure: null);
        Assert.Equal(text, vm.Copied);
        Assert.Null(vm.Error);

        vm.RecordCopyOutcome(text!, new InvalidOperationException("clipboard busy"));
        Assert.Null(vm.Copied);
        Assert.Equal(Strings.Format("common/copy_failed", "clipboard busy"), vm.Error);
    }

    // ── Admin → Logs ─────────────────────────────────────────────────────

    [Fact]
    public async Task AdminLogs_EmptyRing_BlocksCopy_ByThePagesEmptyLine()
    {
        var vm = new AdminLogsViewModel(new MockNestRpcClient { NextAdminLogs = Array.Empty<LogEntry>() });
        await vm.LoadAsync();

        var answer = vm.RingAction();
        Assert.False(answer.Enabled);
        Assert.Equal(Strings.Resolve(Key("admin.logs_page.empty")), answer.Reason);
        Assert.Null(vm.CopyPayload());
    }

    [Fact]
    public async Task AdminLogs_Copy_ConfirmsOrPutsCopyFailedOnTheError()
    {
        var vm = new AdminLogsViewModel(new MockNestRpcClient
        {
            NextAdminLogs = new[] { MockNestRpcClient.MakeLogEntry(1, LogLevel.Info, message: "nest up") },
        });
        await vm.LoadAsync();
        Assert.True(vm.RingAction().IsPossible);
        var text = vm.CopyPayload()!;

        vm.RecordCopyOutcome(text, failure: null);
        Assert.Equal(text, vm.Copied);

        vm.RecordCopyOutcome(text, new InvalidOperationException("denied"));
        Assert.Null(vm.Copied);
        Assert.Equal(Strings.Format("common/copy_failed", "denied"), vm.Error);
    }

    // ── admin-users-invite-code-copy-btn ─────────────────────────────────

    [Fact]
    public async Task InviteCopy_ConfirmsTheCode_AndAFailureLandsOnTheActionError()
    {
        var vm = new AdminUsersViewModel(new MockNestRpcClient { NextMintedInviteCode = "ABC-123" });
        await vm.CreateCodeCommand.ExecuteAsync(null);
        Assert.Equal("ABC-123", vm.MintedCode);

        vm.RecordInviteCopy("ABC-123", failure: null);
        Assert.Equal("ABC-123", vm.MintedCodeCopied);
        Assert.Null(vm.ActionError);

        vm.RecordInviteCopy("ABC-123", new InvalidOperationException("clipboard busy"));
        Assert.Null(vm.MintedCodeCopied);
        Assert.Equal(Strings.Format("common/copy_failed", "clipboard busy"), vm.ActionError);
    }

    [Fact]
    public async Task InviteCopy_ConfirmationClears_WhenAnotherCodeIsMinted()
    {
        var rpc = new MockNestRpcClient { NextMintedInviteCode = "FIRST" };
        var vm = new AdminUsersViewModel(rpc);
        await vm.CreateCodeCommand.ExecuteAsync(null);
        vm.RecordInviteCopy("FIRST", failure: null);

        rpc.NextMintedInviteCode = "SECOND";
        await vm.CreateCodeCommand.ExecuteAsync(null);

        Assert.Null(vm.MintedCodeCopied);
    }

    // ── rule-editor-save-button ──────────────────────────────────────────

    [Fact]
    public void RuleEditorSave_IsBlockedByTheStatusSentence_ThenPossible()
    {
        var model = new RuleEditorModel();
        model.Load(FfiRuleEditor.ForNest("example.test", null));
        model.AddRule(); // a category rule with no reason yet: unsaveable

        var blocked = model.SaveAnswer();
        Assert.True(blocked.Shown);
        Assert.False(blocked.Enabled);
        Assert.False(string.IsNullOrEmpty(blocked.Reason));
        Assert.False(model.Paint().SaveEnabled);

        model.SetReason(0, "No spam here");
        var possible = model.SaveAnswer();
        Assert.True(possible.IsPossible);
        Assert.True(model.Paint().SaveEnabled);
    }

#if P2P_SHARE
    // ── offline-share-begin-button / offline-receive-expect-button ───────
    // The ceremony faces exist only in the P2P_SHARE flavor (store-safe has none).

    private static byte[] Secret(byte fill) => Enumerable.Repeat(fill, 32).ToArray();

    [Fact]
    public void OfflineShare_EmptyCode_BlocksBegin_AndExpectIsAbsentOffItsPanel()
    {
        var view = FaunaFfiMethods.OfflineShareView(
            OfflineSharePanel.Initiate, Secret(1), null, "", CeremonyStatus.Idle);

        var begin = PossibilityPaint.Of(FaunaFfiMethods.OfflineShareBeginPossibility(view));
        Assert.True(begin.Shown);
        Assert.False(begin.Enabled);
        Assert.False(string.IsNullOrEmpty(begin.Reason));

        var expect = PossibilityPaint.Of(FaunaFfiMethods.OfflineShareExpectPossibility(view));
        Assert.False(expect.Shown);
    }

    [Fact]
    public void OfflineShare_AValidPeerCode_MakesBeginAndExpectPossible()
    {
        var peerCode = FaunaFfiMethods.OfflineShareView(
            OfflineSharePanel.Initiate, Secret(2), null, "", CeremonyStatus.Idle).ownCode;

        var initiate = FaunaFfiMethods.OfflineShareView(
            OfflineSharePanel.Initiate, Secret(1), null, peerCode, CeremonyStatus.Idle);
        Assert.True(PossibilityPaint.Of(FaunaFfiMethods.OfflineShareBeginPossibility(initiate)).IsPossible);

        var receive = FaunaFfiMethods.OfflineShareView(
            OfflineSharePanel.Receive, Secret(1), null, peerCode, CeremonyStatus.Idle);
        Assert.True(PossibilityPaint.Of(FaunaFfiMethods.OfflineShareExpectPossibility(receive)).IsPossible);
    }
#endif

    // ── share-link-button ────────────────────────────────────────────────

    [Fact]
    public void ShareLink_EligibleIsPossible_IneligibleIsAbsent()
    {
        Assert.True(PossibilityPaint.Of(FaunaMediaMachineMethods.ShareLinkPossibility(true)).IsPossible);
        var absent = PossibilityPaint.Of(FaunaMediaMachineMethods.ShareLinkPossibility(false));
        Assert.False(absent.Shown);
    }
}
