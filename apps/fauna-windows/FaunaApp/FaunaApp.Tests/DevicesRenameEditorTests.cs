using System.Collections.Generic;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_devices_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Devices page's device-name editor (ui/devices.md § Device names): which
/// device's editor is open, keyed by <c>device_id</c>, and the element sequence a
/// card paints with the editor closed and open. tui pins the same sequence in
/// <c>apps/fauna-tui/src/settings/devices.rs</c>; the page is WinUI and not
/// reachable from this assembly, so the state lives in
/// <see cref="DevicesRenameEditor"/>.
/// </summary>
public class DevicesRenameEditorTests
{
    [Fact]
    public void ClosedCard_PaintsOnlyTheRenameButton()
    {
        Assert.Equal(
            new[] { "device-rename-button" },
            DevicesRenameEditor.CardElementIds(renameOffered: true, editorOpenOnThisCard: false));
    }

    [Fact]
    public void CardOfferingNoRename_PaintsNoRenameElement()
    {
        Assert.Empty(DevicesRenameEditor.CardElementIds(renameOffered: false, editorOpenOnThisCard: false));
        Assert.Empty(DevicesRenameEditor.CardElementIds(renameOffered: false, editorOpenOnThisCard: true));
    }

    [Fact]
    public void OpenCard_PaintsButtonThenInputConfirmCancel()
    {
        Assert.Equal(
            new[]
            {
                "device-rename-button",
                "device-rename-input",
                "device-rename-confirm-button",
                "device-rename-cancel-button",
            },
            DevicesRenameEditor.CardElementIds(renameOffered: true, editorOpenOnThisCard: true));
    }

    // ── Which cards offer a rename (ui/devices.md § Device names) ─────────

    /// <summary>A devices machine that answers the shared <c>rename_possibility</c>
    /// as shared Rust does for the nest's WebDAV pseudo-device row (Absent) and
    /// for an ordinary listed device (possible over its unchanged name), and
    /// records the draft each ask carried.</summary>
    private sealed class FakeRenameMachine : DevicesMachineFakeBase
    {
        public const string WebdavRow = "webdav-pseudo";
        public readonly Dictionary<string, string?> Names = new()
        {
            ["dev-a"] = "Kitchen laptop",
            [WebdavRow] = "WebDAV",
            ["dev-unnamed"] = null,
        };
        public readonly List<(string DeviceId, string Draft)> Asks = new();

        public override string? DeviceName(string deviceId)
            => Names.TryGetValue(deviceId, out var name) ? name : null;

        public override Possibility RenamePossibility(string deviceId, string draft)
        {
            Asks.Add((deviceId, draft));
            return deviceId == WebdavRow || !Names.ContainsKey(deviceId)
                ? new Possibility.Absent()
                : new Possibility.Possible();
        }
    }

    [Fact]
    public void TheWebdavPseudoDeviceCard_OffersNoRename_WhileEveryOtherCardDoes()
    {
        var machine = new FakeRenameMachine();

        Assert.False(DevicesRenameEditor.RenameOffered(machine, FakeRenameMachine.WebdavRow));
        Assert.True(DevicesRenameEditor.RenameOffered(machine, "dev-a"));
        Assert.True(DevicesRenameEditor.RenameOffered(machine, "dev-unnamed"));
    }

    [Fact]
    public void RenameOffered_AsksOverTheRowsUnchangedName_OrEmptyWhenUnknown()
    {
        var machine = new FakeRenameMachine();

        DevicesRenameEditor.RenameOffered(machine, "dev-a");
        DevicesRenameEditor.RenameOffered(machine, "dev-unnamed");

        Assert.Equal(
            new[] { ("dev-a", "Kitchen laptop"), ("dev-unnamed", "") },
            machine.Asks);
    }

    [Fact]
    public void ARowTheRosterNoLongerLists_OffersNoRename()
    {
        Assert.False(DevicesRenameEditor.RenameOffered(new FakeRenameMachine(), "dev-gone"));
    }

    [Fact]
    public void NoMachineYet_PaintsTheRenameButton()
    {
        Assert.True(DevicesRenameEditor.RenameOffered(null, "dev-a"));
    }

    [Fact]
    public void Open_PrefillsAndIsKeyedByDeviceId()
    {
        var editor = new DevicesRenameEditor();
        Assert.False(editor.IsOpen);

        editor.Open("dev-a", "Kitchen laptop");

        Assert.True(editor.IsOpen);
        Assert.True(editor.IsOpenFor("dev-a"));
        Assert.False(editor.IsOpenFor("dev-b"));
        Assert.Equal("Kitchen laptop", editor.Draft);
    }

    [Fact]
    public void Open_WithNoKnownName_StartsEmpty()
    {
        var editor = new DevicesRenameEditor();
        editor.Open("dev-a", null);
        Assert.Equal("", editor.Draft);
    }

    [Fact]
    public void OpeningAnotherCard_ReplacesTheOpenEditor()
    {
        var editor = new DevicesRenameEditor();
        editor.Open("dev-a", "A");
        editor.SetDraft("A typed");

        editor.Open("dev-b", "B");

        Assert.False(editor.IsOpenFor("dev-a"));
        Assert.True(editor.IsOpenFor("dev-b"));
        Assert.Equal("B", editor.Draft);
    }

    [Fact]
    public void SetDraft_KeepsTheTypedText_AndIsIgnoredWhenClosed()
    {
        var editor = new DevicesRenameEditor();
        editor.SetDraft("ignored");
        Assert.Equal("", editor.Draft);

        editor.Open("dev-a", "A");
        editor.SetDraft("Renamed");
        Assert.Equal("Renamed", editor.Draft);
    }

    [Fact]
    public void Close_DropsTheEditorAndTheDraft()
    {
        var editor = new DevicesRenameEditor();
        editor.Open("dev-a", "A");
        editor.SetDraft("Renamed");

        editor.Close();

        Assert.False(editor.IsOpen);
        Assert.False(editor.IsOpenFor("dev-a"));
        Assert.Equal("", editor.Draft);
    }
}
