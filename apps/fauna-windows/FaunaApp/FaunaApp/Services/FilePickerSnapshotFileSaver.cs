using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;

namespace FaunaApp.Services;

/// <summary>
/// Production <see cref="ISnapshotFileSaver"/>: the native
/// <c>FileSavePicker</c>, initialized with the main window's hwnd (the WinUI 3
/// desktop idiom — mirrors FoldersPage's FolderPicker). Selected by
/// <see cref="SnapshotFileSavers.ForSession"/> when NOT under
/// <c>FAUNA_E2E_BRIDGE</c>; e2e launches get
/// <see cref="DirectorySnapshotFileSaver"/> instead (a native save dialog is
/// not e2e-driveable).
/// </summary>
public sealed class FilePickerSnapshotFileSaver : ISnapshotFileSaver
{
    public async Task<string?> SaveAsync(string suggestedFileName, byte[] data, CancellationToken ct = default)
    {
        if (App.MainWindow is null) return null;

        var picker = new Windows.Storage.Pickers.FileSavePicker
        {
            SuggestedFileName = suggestedFileName,
        };
        // FileTypeChoices must be non-empty; offer the file's own extension
        // (".bin" for an extension-less snapshot path — WinRT rejects "." /
        // "*" as a save-choice extension).
        var ext = Path.GetExtension(suggestedFileName);
        if (string.IsNullOrEmpty(ext)) ext = ".bin";
        picker.FileTypeChoices.Add(
            ext.TrimStart('.').ToUpperInvariant() + " file",
            new List<string> { ext });

        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

        var file = await picker.PickSaveFileAsync();
        if (file is null) return null;
        await Windows.Storage.FileIO.WriteBytesAsync(file, data);
        return file.Path;
    }
}

/// <summary>
/// The one place a page picks its <see cref="ISnapshotFileSaver"/>: dialog-less
/// into <see cref="E2eDownloadDir"/> under e2e, the native save dialog
/// otherwise. Every download surface — backups single-file restore, Export My
/// Data, the Media detail's download — asks here, so the e2e directory and the
/// driver's <c>download_dir()</c> can never drift apart per page.
/// </summary>
public static class SnapshotFileSavers
{
    public static ISnapshotFileSaver ForSession() =>
        E2eDownloadDir.Resolve() is { } dir
            ? new DirectorySnapshotFileSaver(dir)
            : new FilePickerSnapshotFileSaver();
}
