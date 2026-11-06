using FaunaApp.Core.ViewModels;
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
        Assert.Equal(new[] { "device-rename-button" }, DevicesRenameEditor.CardElementIds(false));
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
            DevicesRenameEditor.CardElementIds(true));
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
