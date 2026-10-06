using System;
using System.IO;
using System.Runtime.InteropServices;

namespace FaunaApp.Core.Services;

/// <summary>
/// Where the mailbox export's Download writes the recovered <c>.zip.zst</c>
/// (mail-export.md § Download flow step 5). The shared sink writes
/// <c>&lt;name&gt;.part</c> and renames it into place only once the archive is complete
/// and terminated, so a refused download leaves nothing here that reads as a mailbox;
/// it also creates the directory if it is missing.
///
/// <para>Production: the user's Downloads folder — the desktop destination tui, linux
/// and macOS use, and the Done summary names the saved path, so no save dialog is
/// needed for the press to be visible. Under e2e: <c>FAUNA_E2E_DOWNLOAD_DIR</c>, the
/// directory the driver reads back, behind the same gate as the account export's
/// <c>DirectorySnapshotFileSaver</c> — the one <see cref="E2eDownloadDir"/>
/// resolution (convention 15).</para>
/// </summary>
internal static class MailExportSaveDir
{
    internal static string Resolve()
    {
        if (E2eDownloadDir.Resolve() is { } e2eDir) return e2eDir;
        var profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        return KnownDownloadsFolder() ?? Path.Combine(profile, "Downloads");
    }

    /// <summary>FOLDERID_Downloads — .NET's <see cref="Environment.SpecialFolder"/> has no
    /// Downloads member, and the user may have moved the folder off the profile.</summary>
    private static readonly Guid FolderIdDownloads = new("374DE290-123F-4565-9164-39C4925E467B");

    private static string? KnownDownloadsFolder()
    {
        try
        {
            if (SHGetKnownFolderPath(FolderIdDownloads, 0, IntPtr.Zero, out var ptr) != 0) return null;
            try
            {
                var path = Marshal.PtrToStringUni(ptr);
                return string.IsNullOrEmpty(path) ? null : path;
            }
            finally
            {
                Marshal.FreeCoTaskMem(ptr);
            }
        }
        catch (Exception)
        {
            return null;
        }
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int SHGetKnownFolderPath(
        [MarshalAs(UnmanagedType.LPStruct)] Guid rfid, uint dwFlags, IntPtr hToken, out IntPtr ppszPath);
}
