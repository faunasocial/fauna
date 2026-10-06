using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>What the rotate-keys form shows and allows right now: the
/// <c>mail-rotate-keys-progress-indicator</c> text (empty when nothing runs) and
/// whether confirm + cancel take input.</summary>
internal readonly record struct MailRotateFormPaint(string ProgressText, bool ControlsEnabled);

/// <summary>
/// The rotate-keys form's hold-open decision, lifted out of
/// <c>MailSettingsPanel</c>'s code-behind so its ORDER is unit-testable
/// (<c>mail-settings.md</c> § Element visibility — the progress indicator shows
/// "during multi-step rotation"; § Architectural rules 1 and 5 — same shape on all 7
/// apps, no second rotation racing the first). Mirrors linux's
/// <c>paint_rotate_form</c> / <c>rotate_progress_text</c> split
/// (<c>apps/fauna-linux/src/settings/mail.rs</c>).
///
/// <para>On confirm the form stays open: confirm and cancel are disabled and the
/// progress line is painted from the count the rotation re-wraps (shared Rust
/// <c>rotation_rewrap_count</c>) while the dispatch runs; only once it returns does
/// the form collapse and re-enable. The collapse-first shape it replaces left
/// nothing on screen showing the rotation running, so the e2e barrier
/// <c>wait_for_rotation_to_finish</c> passed at once and a relaunch could cut the
/// rotation short.</para>
/// </summary>
internal sealed class MailRotateForm
{
    // The credentials-remaining count of THIS form's own rotation, from confirm until
    // its dispatch returns; null when none of this form's rotations is running.
    private ulong? _inFlight;

    /// <summary>True from confirm until its dispatch returns.</summary>
    public bool InFlight => _inFlight.HasValue;

    /// <summary>The form's paint for <paramref name="status"/>: the snapshot's own
    /// <c>RotationInProgress</c> when it reports one, else — while this form's
    /// rotation is in flight — the same label from the confirm-time count (the page
    /// holds the pre-rotation snapshot until the dispatch returns), else empty.
    /// Confirm and cancel are disabled only while THIS form's rotation is in flight,
    /// never for one the snapshot reports (a resumed rotation).</summary>
    public MailRotateFormPaint Paint(SettingsStatus status)
    {
        var shown = (status, _inFlight) switch
        {
            (SettingsStatus.RotationInProgress, _) => status,
            (_, ulong remaining) => new SettingsStatus.RotationInProgress(remaining),
            _ => null,
        };
        var text = shown is null
            ? string.Empty
            : Strings.Resolve(FaunaClientMailSettingsMethods.SettingsStatusLabel(shown, true));
        return new MailRotateFormPaint(text, ControlsEnabled: !InFlight);
    }

    /// <summary>Run one confirmed rotation: disable + paint progress, await
    /// <paramref name="dispatch"/>, collapse the form, re-enable. A second confirm
    /// while one runs is ignored (returns false). <paramref name="status"/> reads the
    /// panel's current snapshot status at each paint; a dispatch that throws still
    /// re-enables the controls and leaves the form open.</summary>
    public async Task<bool> ConfirmAsync(
        ulong rewrapCount,
        Func<SettingsStatus> status,
        Action<MailRotateFormPaint> paint,
        Func<Task> dispatch,
        Action collapse)
    {
        if (InFlight) return false;
        _inFlight = rewrapCount;
        paint(Paint(status()));
        try
        {
            await dispatch();
            collapse();
        }
        finally
        {
            _inFlight = null;
            paint(Paint(status()));
        }
        return true;
    }
}
