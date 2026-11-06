using System;
using FaunaApp.Core.Logs;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Views;

/// <summary>
/// The nested-frame half of <c>apps/windows.md</c> § Client lifetime's third rule
/// (a page is per-login, so every page-scoped resource is released in
/// <c>OnNavigatedFrom</c>).
///
/// <para>WinUI calls <c>OnNavigatedFrom</c> only on the page whose OWN frame navigates.
/// Three shells host an inner frame — <see cref="MainPage"/>'s <c>ContentFrame</c>,
/// <see cref="SettingsShellPage"/>'s and <see cref="AdminShellPage"/>'s — and when the
/// shell is the page that leaves (sign-out, an account switch and the e2e reset navigate
/// the ROOT frame; leaving Settings or Admin navigates <c>ContentFrame</c>), the page
/// inside it sees only <c>Unloaded</c>. Its <c>OnNavigatedFrom</c> cleanup never ran: a
/// Feed page stayed subscribed to the session's feed manager and kept its cue-capture
/// timer, the Calendar page kept polling, and every Settings sub-page kept what it had
/// wired.</para>
///
/// <para>So each shell ends its inner page from its own <c>OnNavigatedFrom</c>, by
/// navigating the inner frame to <see cref="EndedPage"/> — the inner page then gets the
/// same <c>OnNavigatedFrom</c> any navigation gives it, and the release lives in one
/// place per page, as the rule says. The ending recurses: <see cref="MainPage"/> ending
/// a Settings shell runs that shell's <c>OnNavigatedFrom</c>, which ends ITS sub-page.</para>
///
/// <para>On the root-frame paths this runs AFTER the actor change, so each page's
/// <c>OnNavigatedFrom</c> must already tolerate a destroyed session object (the same
/// rule's teardown corollary); the catch below is a last guard, never the design.</para>
/// </summary>
internal static class NestedFrame
{
    /// <summary>Navigate <paramref name="frame"/> off whatever page it shows so that
    /// page's own <c>OnNavigatedFrom</c> runs. <paramref name="owner"/> names the shell
    /// for the log. Idempotent: an empty frame or one already ended is left alone.</summary>
    internal static void EndPage(Frame frame, string owner)
    {
        if (frame.Content is null or EndedPage) return;
        var ending = frame.Content.GetType().Name;
        try
        {
            frame.Navigate(typeof(EndedPage));
            E2eTrace.Write($"[page-lifetime] {owner} ended its inner {ending}");
        }
        catch (Exception ex)
        {
            ShellLog.Warn(owner, $"[page-lifetime] ending inner {ending} failed: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
