using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Logs;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// The ONE path by which the windows app shows a <see cref="ContentDialog"/>:
/// every <c>ShowAsync()</c> in the app is this class's, and nothing else
/// assigns the open-dialog registry (the grep ratchet
/// <c>tests/e2e-unified/tests/test_windows_dialog_gate_ratchet.py</c> holds
/// both).
///
/// <para><b>Why a gate.</b> WinUI allows ONE ContentDialog per XamlRoot, and a
/// second <c>ShowAsync()</c> throws <c>InvalidOperationException</c>. Every
/// show handler is <c>async void</c>, so that throw reaches
/// <c>Application.UnhandledException</c> and kills the process — reached by a
/// user whenever a code path opens a dialog while another is up (a keyboard
/// accelerator, a background prompt), and by the e2e harness whenever a test
/// ends with a dialog open.</para>
///
/// <para><b>The policy: refuse the second open legibly.</b> The open dialog
/// stays up, untouched, and the refusal is stated on <c>error-message</c>
/// (<c>common.dialog_already_open</c>) — never a crash, never a silent drop
/// (e2e convention 11). The alternative, bringing the open dialog forward, has
/// nothing to do: a ContentDialog is already modal and on top of its XamlRoot,
/// so "forward" would read exactly like a silent drop. A caller whose
/// preparation mutates page state (a pending id, the dialog's own fields) passes
/// it as <c>prepare</c>, which runs only once the gate has let the open through,
/// so a refused open leaves the open dialog's state exactly as the user left
/// it.</para>
///
/// <para><b>Reset owns the force-close.</b> A ContentDialog belongs to the
/// XamlRoot, not the frame, so a page navigation does not close it. The e2e
/// <c>reset</c> and actor-swap arms call <see cref="CloseAll"/> so the next
/// test starts on a clean XamlRoot; since every dialog is shown here, the
/// registry is exhaustive and no stale dialog can survive a reset.</para>
///
/// <para>UI-thread only, like every call that shows or hides a dialog, so the
/// registry takes no lock.</para>
/// </summary>
internal static class Dialogs
{
    private static readonly Dictionary<XamlRoot, ContentDialog> _open = new();

    // RO_E_ILLEGAL_METHOD_CALL — "An async operation was not properly started".
    private const int E_ILLEGAL_METHOD_CALL = unchecked((int)0x80000019);

    /// <summary>
    /// Show <paramref name="dialog"/> on its <see cref="ContentDialog.XamlRoot"/>
    /// (the caller sets it first). Returns the dialog's result, or <c>null</c>
    /// when the open was refused because another dialog is already up there.
    /// </summary>
    internal static async Task<ContentDialogResult?> ShowAsync(ContentDialog dialog, Action? prepare = null)
    {
        var root = dialog.XamlRoot;
        if (root is null)
        {
            // The owning page is not in the visual tree: there is no page to
            // show the dialog on, nor one to state a refusal on. Traced only.
            E2eTrace.Write($"[dialogs] dropped '{Describe(dialog)}': no XamlRoot");
            return null;
        }
        if (_open.TryGetValue(root, out var shown))
        {
            Refuse($"'{Describe(shown)}' is open; refused '{Describe(dialog)}'");
            return null;
        }
        prepare?.Invoke();
        _open[root] = dialog;
        try
        {
            return await dialog.ShowAsync();
        }
        catch (COMException ex) when (ex.HResult == E_ILLEGAL_METHOD_CALL)
        {
            // WinUI's own "Only a single ContentDialog can be open at any time"
            // (measured 2026-09-27: a COMException, not the InvalidOperationException
            // the docs suggest). Unreachable while every dialog is shown through this
            // gate — kept so a dialog the registry lost track of is still a refusal,
            // not a process death.
            Refuse($"WinUI refused '{Describe(dialog)}': {ex.Message}");
            return null;
        }
        finally
        {
            if (_open.TryGetValue(root, out var d) && ReferenceEquals(d, dialog))
                _open.Remove(root);
        }
    }

    /// <summary>
    /// Hide every open dialog and clear the registry. Called by the e2e
    /// <c>reset</c> and actor-swap arms, on the UI thread, before they navigate.
    /// </summary>
    internal static void CloseAll()
    {
        var stale = _open.Values.ToList();
        _open.Clear();
        foreach (var d in stale)
        {
            try { d.Hide(); } catch { /* already closing */ }
        }
    }

    private static void Refuse(string why)
    {
        E2eTrace.Write($"[dialogs] refused: {why}");
        var message = S.Get("common/dialog_already_open");
        App.CurrentErrorMessage = message;
        Views.MainPage.Current?.ShowError(message);
    }

    private static string Describe(ContentDialog d) =>
        Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(d) is { Length: > 0 } id
            ? id
            : d.Title?.ToString() ?? d.GetType().Name;
}
