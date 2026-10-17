using System;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.Services;

/// <summary>
/// The elapsed time and time left both mail wizards (<c>mail-export</c>,
/// <c>mail-import</c>) append to their progress summary, and the time the Done
/// card reports (<c>mail-export.md</c> § Wizard steps steps 4–5,
/// <c>mailbox-migration.md</c> § Wizard steps steps 5–6). The computation is
/// the shared <c>fauna_client_mail_settings::transfer_timing</c> (priority #2);
/// this only renders it through the i18n templates and
/// <see cref="ValueFormat.DurationSecs"/>.
/// </summary>
internal static class TransferTimingText
{
    /// <summary>The shared timing of one session, at the client's clock.</summary>
    public static TransferTiming Of(
        long startedAt, long lastProgressAt, bool running, ulong processed, ulong total) =>
        FaunaClientMailSettingsMethods.TransferTiming(
            startedAt, lastProgressAt, running, processed, total,
            DateTimeOffset.UtcNow.ToUnixTimeSeconds());

    /// <summary>
    /// <paramref name="summary"/> with the elapsed time and, while the run is moving, the
    /// time left. <paramref name="area"/> is the string-table group
    /// (<c>mail_export</c> / <c>mail_import</c>); no clock yet leaves the summary as is.
    /// </summary>
    public static string WithTiming(string area, string summary, TransferTiming timing)
    {
        if (timing.elapsedSecs is not ulong elapsed) return summary;
        return timing.remainingSecs is ulong remaining
            ? Strings.Format(
                area + "/progress_remaining_fmt",
                summary, ValueFormat.DurationSecs(elapsed), ValueFormat.DurationSecs(remaining))
            : Strings.Format(area + "/progress_elapsed_fmt", summary, ValueFormat.DurationSecs(elapsed));
    }
}
