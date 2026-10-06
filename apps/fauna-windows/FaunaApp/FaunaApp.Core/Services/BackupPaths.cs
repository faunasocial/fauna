namespace FaunaApp.Core.Services;

/// <summary>
/// The single source for the canonical Fauna data dir (<c>%LocalAppData%\Fauna</c>) — used
/// by the per-destination backup status read + upload coordinator (they MUST agree on this
/// path, the data_dir canonical-path contract, <c>backups.md</c> § Per-destination status
/// read: the upload coordinator writes the manifest-mirror state the status read consumes,
/// so a divergent path would make <c>last_upload_time</c> invisible to the page), the MLS
/// conversations store (<see cref="NestRpcClient"/>), the trace log, and the nest-identity
/// pin store (both installed in <c>App.xaml.cs</c>).
/// </summary>
public static class BackupPaths
{
    /// <summary>
    /// <c>%LocalAppData%\Fauna</c> — the canonical Fauna data dir. Redirectable under e2e
    /// via <c>FAUNA_E2E_DATA_DIR</c> (read through <see cref="E2eEnv"/>, so the redirect is
    /// compiled out of release builds — convention 15; mirrors <c>FAUNA_E2E_CREDENTIAL_DIR</c>, the harness's
    /// existing per-test credential-store isolation seam — <c>drivers/windows.py</c>): the
    /// client at-rest leg of the version-skew grid
    /// (<c>version-compatibility.md</c> § Dimension 6) hands ONE persistent root to two
    /// different pinned builds launched in sequence, and the real user profile dir is not a
    /// root any test may write into. Unset in production — never a human-facing knob (no
    /// config file, no env var a user or admin would ever set).
    /// </summary>
    public static string DataDir =>
        E2eEnv.DataDir is { Length: > 0 } dir
            ? dir
            : System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
                "Fauna");
}
