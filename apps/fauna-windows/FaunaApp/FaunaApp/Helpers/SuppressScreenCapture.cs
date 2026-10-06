using System;
using FaunaApp.Core.Services;

namespace FaunaApp.Helpers;

/// <summary>
/// The WinUI half of windows' capture suppression — <c>docs/goal/architecture/security.md</c>
/// § On-screen secret exposure (screen capture), rule 2.
///
/// <para>All it does is resolve the app window's HWND, because SwiftUI-style, XAML has no
/// window handle of its own and <c>WinRT.Interop.WindowNative.GetWindowHandle</c> is a WinUI
/// type <c>FaunaApp.Core</c> cannot reference. Every piece of this feature with logic in it —
/// the per-HWND refcount, the restore-the-previous-value rule, the one-hold-per-call-site
/// invariant — lives in <see cref="ScreenCaptureGuard"/> and <see cref="ScreenCaptureHold"/>,
/// where <c>FaunaApp.Tests</c> (plain <c>net10.0</c>, Core-only) can reach it.</para>
///
/// <para>⚠ <b>Rule 1 — the one that wins on collision.</b> Never call this from
/// <c>secret-key-display</c> (<c>Views/Onboarding/IdentityCreatedView.xaml</c>) or from a
/// <c>recovery-kit-secret-display</c> surface should windows ever grow one. Those show
/// client-only-resident key material: a user who loses it loses the account, and users
/// screenshot a recovery kit precisely because that copy is what saves them. Suppressing
/// there trades a shoulder-surfing risk for an account-loss risk — the irreversible one.
/// The in-scope surfaces are the <b>minted, revocable</b> three, and only those:
/// <c>mail-settings-credential-item-secret</c>, <c>atproto-app-credential-reveal</c>,
/// <c>nostr-bunker-connect-string</c>. That list is pinned from outside this app by tier_1
/// <c>tests/e2e-unified/tests/test_screen_capture_posture.py</c>, in both directions.</para>
/// </summary>
internal static class SuppressScreenCapture
{
    /// <summary>A fresh hold for one call site, bound to the app window.
    ///
    /// <para>Hand the result a plain "is a secret painted right now?" predicate via
    /// <see cref="ScreenCaptureHold.Sync"/> on every render, and <c>Sync(false)</c> (or
    /// <c>Dispose</c>) on navigate-away. Do not call <see cref="ScreenCaptureGuard.Acquire"/>
    /// directly from a page: the hold object is what makes a double-acquire — a permanently
    /// uncapturable app — unrepresentable.</para></summary>
    internal static ScreenCaptureHold ForMainWindow() =>
        new(ScreenCaptureGuard.Shared, MainWindowHandle);

    /// <summary>The app window's HWND, or <see cref="IntPtr.Zero"/> before it exists.
    ///
    /// <para>Zero is a legitimate answer, not a failure: the guard treats it as "no window to
    /// suppress" and skips. A reveal cannot be painted before the window exists anyway, so
    /// the only way to get here early is a render pass racing startup.</para></summary>
    private static IntPtr MainWindowHandle()
    {
        var window = App.MainWindow;
        if (window is null) return IntPtr.Zero;
        try
        {
            return WinRT.Interop.WindowNative.GetWindowHandle(window);
        }
        catch (Exception)
        {
            // A window torn down between the null check and the interop call. Nothing to
            // suppress, and a security control must never be the thing that crashes a page.
            return IntPtr.Zero;
        }
    }
}
