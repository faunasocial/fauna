using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the shared <see cref="FeatureAuthoringHost"/> — the
/// C# shell all three hosts of the feature-policy editor (admin Nest page, Status
/// own-limit control, Family guardian section) hold around <c>FfiPolicyEditor</c>
/// (<c>dynamic-features.md</c> § Authoring surfaces). The editor handle is faked
/// (<see cref="FfiPolicyEditorFakeBase"/>), so no native library is loaded; what is
/// pinned here is the host's own contract: it forwards, re-reads the view, routes a
/// refused cell to the error line and a landed write to status + re-read.
/// </summary>
public class FeatureAuthoringHostTests
{
    private static LocalizedText Text(string key) => new(key, new Dictionary<string, string>());

    private static string Resolve(LocalizedText text)
    {
        var args = text.@args;
        return args.Count == 0 ? text.@key : $"{text.@key}:{string.Join(",", args.Values)}";
    }

    private static FfiAuthoredRow Row(string feature) =>
        new(feature, Text($"name.{feature}"), Text("summary.none"), hasDocument: false, unreadable: false);

    private static FfiPolicyEditorView View(bool on, string text = "") => new(
        feature: "p2p-share",
        tier: "self",
        title: Text("title"),
        on: on,
        cells: new[] { new FfiEditorCell(Text("cell.label"), text, note: null, noteMagnitude: null) },
        offNote: null,
        canRemove: false,
        hint: Text("hint"));

    /// <summary>A scripted editor: remembers what the host forwarded and answers the
    /// verdict the test staged.</summary>
    private sealed class FakeEditor : FfiPolicyEditorFakeBase
    {
        public bool On = true;
        public string Cell = "";
        public FfiEditorVerdict Verdict = new(status: Text("editor.saved"), invalid: null);
        public Exception? Throws;
        public int Saves;
        public int Removes;

        public override FfiPolicyEditorView View() => FeatureAuthoringHostTests.View(On, Cell);
        public override void SetOn(bool on) => On = on;
        public override void SetCell(uint index, string text) => Cell = text;

        public override Task<FfiEditorVerdict> Save()
        {
            Saves++;
            return Throws is null ? Task.FromResult(Verdict) : Task.FromException<FfiEditorVerdict>(Throws);
        }

        public override Task<FfiEditorVerdict> Remove()
        {
            Removes++;
            return Task.FromResult(Verdict);
        }
    }

    private static FeatureAuthoringHost Host(
        FakeEditor editor, List<string> reads, Func<Task>? onWritten = null,
        Func<Task<IReadOnlyList<FfiAuthoredRow>>>? readRows = null) =>
        new(
            readRows ?? (() =>
            {
                reads.Add("read");
                return Task.FromResult<IReadOnlyList<FfiAuthoredRow>>(new[] { Row("payments"), Row("p2p-share") });
            }),
            _ => Task.FromResult<IFfiPolicyEditor>(editor),
            Resolve,
            onWritten);

    [Fact]
    public async Task Refresh_ReadsTheTiersAuthoredRows_AndRaisesChanged()
    {
        var reads = new List<string>();
        var host = Host(new FakeEditor(), reads);
        var changed = 0;
        host.Changed += () => changed++;

        Assert.Null(host.State.Rows); // nothing painted before the read answers

        await host.RefreshAsync();

        Assert.Equal(2, host.State.Rows!.Count);
        Assert.Equal("p2p-share", host.RowFor("p2p-share")!.@feature);
        Assert.Null(host.RowFor("zaps"));
        Assert.Equal(1, changed);
    }

    [Fact]
    public async Task Refresh_AFailedRead_LeavesNoRows_AndWordsTheReason()
    {
        var host = Host(
            new FakeEditor(), new List<string>(),
            readRows: () => throw new InvalidOperationException("offline"));

        await host.RefreshAsync();

        Assert.Null(host.State.Rows);
        Assert.Equal("features.editor_load_failed:offline", host.State.ReadError);
    }

    [Fact]
    public async Task Open_SeedsTheViewFromTheEditor_AndCancelClosesItWithoutAWrite()
    {
        var editor = new FakeEditor();
        var host = Host(editor, new List<string>());

        await host.OpenAsync("p2p-share");

        Assert.Equal("p2p-share", host.State.OpenFeature);
        Assert.True(host.State.View!.@on);

        host.Cancel();

        Assert.Null(host.State.OpenFeature);
        Assert.Null(host.State.View);
        Assert.Equal(0, editor.Saves);
    }

    [Fact]
    public async Task SetOn_ForwardsToTheEditor_AndReReadsTheView()
    {
        var editor = new FakeEditor();
        var host = Host(editor, new List<string>());
        await host.OpenAsync("p2p-share");

        host.SetOn(false);

        Assert.False(editor.On);
        Assert.False(host.State.View!.@on);
    }

    [Fact]
    public async Task SetCell_ForwardsTheKeystroke_AndRaisesNothing()
    {
        // Typing must not rebuild the host's controls (the entry would lose focus):
        // the caller repaints the notes from the returned view instead.
        var editor = new FakeEditor();
        var host = Host(editor, new List<string>());
        await host.OpenAsync("p2p-share");
        var changed = 0;
        host.Changed += () => changed++;

        var view = host.SetCell(0, "5");

        Assert.Equal("5", editor.Cell);
        var cells = view!.@cells;
        Assert.Equal("5", cells[0].@text);
        Assert.Equal(0, changed);
    }

    [Fact]
    public async Task Save_ALandedWrite_ShowsTheStatus_ReReadsTheRows_AndNotifiesTheOwner()
    {
        var editor = new FakeEditor();
        var reads = new List<string>();
        var owner = 0;
        var host = Host(editor, reads, onWritten: () => { owner++; return Task.CompletedTask; });
        await host.OpenAsync("p2p-share");

        await host.SaveAsync();

        Assert.Equal(1, editor.Saves);
        Assert.Equal("editor.saved", host.State.Status);
        Assert.Null(host.State.Error);
        Assert.Single(reads);      // the authored rows were re-read after the write
        Assert.Equal(1, owner);    // and the owner re-reads the plane it also shows
        Assert.Equal("p2p-share", host.State.OpenFeature); // the editor stays open, re-seeded
    }

    [Fact]
    public async Task Save_ARefusedCell_LandsOnTheErrorLine_AndDispatchesNothingMore()
    {
        var editor = new FakeEditor
        {
            Verdict = new FfiEditorVerdict(status: null, invalid: Text("editor.invalid_count")),
        };
        var reads = new List<string>();
        var owner = 0;
        var host = Host(editor, reads, onWritten: () => { owner++; return Task.CompletedTask; });
        await host.OpenAsync("p2p-share");

        await host.SaveAsync();

        Assert.Equal("editor.invalid_count", host.State.Error);
        Assert.Null(host.State.Status);
        Assert.Empty(reads);
        Assert.Equal(0, owner);
    }

    [Fact]
    public async Task Save_ATransportFailure_IsWordedOnTheErrorLine()
    {
        var editor = new FakeEditor { Throws = new InvalidOperationException("no nest") };
        var host = Host(editor, new List<string>());
        await host.OpenAsync("p2p-share");

        await host.SaveAsync();

        Assert.Equal("features.editor_save_failed:no nest", host.State.Error);
    }

    [Fact]
    public async Task Remove_SendsTheAbsentPolicy_NotASave()
    {
        var editor = new FakeEditor { Verdict = new FfiEditorVerdict(Text("editor.removed"), null) };
        var host = Host(editor, new List<string>());
        await host.OpenAsync("p2p-share");

        await host.RemoveAsync();

        Assert.Equal(1, editor.Removes);
        Assert.Equal(0, editor.Saves);
        Assert.Equal("editor.removed", host.State.Status);
    }

    [Fact]
    public async Task Reset_ForgetsRowsAndEditor_SoAnotherWardNeverSeesTheLastOnes()
    {
        var host = Host(new FakeEditor(), new List<string>());
        await host.RefreshAsync();
        await host.OpenAsync("p2p-share");

        host.Reset();

        Assert.Null(host.State.Rows);
        Assert.Null(host.State.OpenFeature);
        Assert.Null(host.State.View);
        // And a write with no editor open is a no-op, not a crash.
        await host.SaveAsync();
        Assert.Null(host.State.Error);
    }

    [Fact]
    public async Task Open_AMemberTheNestDoesNotCarry_IsWordedNotThrown()
    {
        var host = new FeatureAuthoringHost(
            () => Task.FromResult<IReadOnlyList<FfiAuthoredRow>>(Array.Empty<FfiAuthoredRow>()),
            _ => throw new InvalidOperationException("this nest does not carry the feature"),
            Resolve);

        await host.OpenAsync("zaps");

        Assert.Null(host.State.OpenFeature);
        Assert.Equal(
            "features.editor_load_failed:this nest does not carry the feature", host.State.Error);
    }
}
