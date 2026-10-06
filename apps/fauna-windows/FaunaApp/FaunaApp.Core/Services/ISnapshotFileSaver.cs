namespace FaunaApp.Core.Services;

/// <summary>
/// Where downloaded bytes land — single-file restore (backups.md § Layout &amp;
/// flow — <c>snapshot-file-download-button</c>), Export My Data
/// (settings.md § Data export — <c>settings-export-data-button</c>) and the
/// Media detail's download (media.md § Element IDs —
/// <c>media-item-detail-download-button</c>) all save through this seam, picked
/// by the app project's <c>SnapshotFileSavers.ForSession()</c>. Production is the platform save dialog
/// (<c>FilePickerSnapshotFileSaver</c> in the WinUI app project — it needs a
/// window handle, so it cannot live in this class lib); e2e launches use
/// <see cref="DirectorySnapshotFileSaver"/> (a native save dialog is not
/// e2e-driveable); unit tests fake the interface.
/// </summary>
public interface ISnapshotFileSaver
{
    /// <summary>Persist <paramref name="data"/> under a user-chosen (or
    /// e2e-fixed) location. <paramref name="suggestedFileName"/> is the
    /// file's basename (no directories). Returns the saved path, or null
    /// when the user cancelled the dialog.</summary>
    Task<string?> SaveAsync(string suggestedFileName, byte[] data, CancellationToken ct = default);
}

/// <summary>
/// The one e2e download directory every dialog-less save path writes into —
/// <c>FAUNA_E2E_DOWNLOAD_DIR</c> when the launch carried one, else
/// <c>&lt;BackupPaths.DataDir&gt;\e2e-downloads</c> (under the canonical,
/// redirectable data dir — a second hand-built <c>%LocalAppData%\Fauna</c>
/// would drop e2e downloads into the real user profile even when the harness
/// isolated the launch, e2e rule 10). Null outside e2e mode. Both reads go
/// through <see cref="E2eEnv"/>, compiled out of release builds (convention
/// 15), so a shipped app always gets null. The windows e2e driver's
/// <c>download_dir()</c> mirrors this choice.
/// </summary>
internal static class E2eDownloadDir
{
    internal static string? Resolve() =>
        E2eEnv.Bridge is not null
            ? E2eEnv.DownloadDir ?? Path.Combine(BackupPaths.DataDir, "e2e-downloads")
            : null;
}

/// <summary>
/// Dialog-free saver writing into a fixed directory — the e2e-mode
/// implementation (selected under <c>FAUNA_E2E_BRIDGE</c>; the windows e2e
/// driver's <c>download_dir()</c> mirrors the directory choice).
/// </summary>
public sealed class DirectorySnapshotFileSaver : ISnapshotFileSaver
{
    private readonly string _dir;

    public DirectorySnapshotFileSaver(string dir) => _dir = dir;

    public async Task<string?> SaveAsync(string suggestedFileName, byte[] data, CancellationToken ct = default)
    {
        Directory.CreateDirectory(_dir);
        var path = Path.Combine(_dir, suggestedFileName);
        await File.WriteAllBytesAsync(path, data, ct);
        return path;
    }
}
