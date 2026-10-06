using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → Devices page's standing enrollment notice (ui/devices.md
/// § State &amp; data shape + § Errors &amp; edge cases): the sentence the account
/// runtime's credential slot records while the nest refuses to enroll this
/// machine — today the tier device cap (behavior/devices.md § Step 4) — painted on
/// the page's <c>error-message</c> while it stands. The sentence is already
/// localized by shared Rust (<see cref="INestRpcClient.AccountEnrollmentNoticeAsync"/>
/// → <c>FfiNestClient::account_enrollment_notice</c>); nothing here parses or
/// composes it.
///
/// <para>The WinUI page (<c>DevicesPage</c>) is a thin renderer and not reachable
/// from the unit-test assembly, so the two rules that decide what it paints live
/// here, where <c>DevicesEnrollmentNoticeTests</c> pins them — the windows
/// counterpart of the shared FaunaKit <c>DevicesMachineVM.enrollmentNotice</c> on
/// apple and android's <c>DevicesVM.enrollmentNotice</c>:</para>
/// <list type="bullet">
/// <item><b>Load:</b> a successful read is a plain assignment — a <c>null</c>
/// answer CLEARS the notice (the slot no longer records the refusal, the only thing
/// that takes it down) — while an exception keeps what is already held, so a
/// transient FFI hiccup never flickers it off.</item>
/// <item><b>Render:</b> a roster gesture's own error wins while it stands, and the
/// notice is the fallback (<see cref="MessageFor"/>) — which is what keeps the
/// snapshot repaint every observer tick triggers from wiping it.</item>
/// </list>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> here: the page awaits
/// <see cref="LoadAsync"/> and then repaints, so the continuation must return to
/// the UI thread (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
internal sealed class DevicesEnrollmentNotice
{
    /// <summary>The standing notice, or <c>null</c> when none stands.</summary>
    public string? Notice { get; private set; }

    /// <summary>Re-read the slot through <paramref name="rpc"/>. Never throws.</summary>
    public async Task LoadAsync(INestRpcClient rpc)
    {
        try
        {
            // Plain assignment, deliberately NOT `if (x is not null) Notice = x`
            // (as the custody fold does for its transient-null): here null is the
            // answer that clears the notice.
            Notice = await rpc.AccountEnrollmentNoticeAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Warn(
                "DevicesEnrollmentNotice",
                $"[enrollment-notice] load failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>What the page's <c>error-message</c> shows: the roster gesture's
    /// own error when there is one, else the standing notice, else <c>null</c>
    /// (clear).</summary>
    public string? MessageFor(string? gestureError) => gestureError ?? Notice;
}
