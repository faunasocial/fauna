using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The platform half of windows' capture suppression: assert the <b>real</b>
/// <c>WDA_EXCLUDEFROMCAPTURE</c> bit, read back off a real window with
/// <c>GetWindowDisplayAffinity</c> — not that a helper was called.
///
/// <para>Shape borrowed from android's <c>ScreenCaptureWindowFlagTest</c>, which asserts the
/// real <c>FLAG_SECURE</c> bit for the same reason: a suppression feature whose only witness
/// is its own abstraction passes just as happily when the platform call silently does
/// nothing. apple's <c>.privacySensitive()</c> misfire — a modifier that tests green against
/// its own presence and suppresses no screenshot — is the documented instance of exactly that
/// failure (<c>security.md</c> § On-screen secret exposure, the platform-API correction).</para>
///
/// <para><b>Why this test builds its own window.</b> <c>SetWindowDisplayAffinity</c> accepts
/// only a top-level window <b>owned by the calling process</b> — it fails with
/// <c>ERROR_ACCESS_DENIED</c> on anything else — so there is no borrowing a window from
/// elsewhere. A hidden <c>WS_POPUP</c> on the system <c>"STATIC"</c> class is the cheapest
/// one that qualifies, and it needs no message pump: the window is created, poked, read back
/// and destroyed on this one thread.</para>
///
/// <para>The refcount arithmetic — the half where the silent, app-wide failures live — is
/// pinned separately and windowlessly in <see cref="ScreenCaptureGuardTests"/>, so this leg
/// is not left testless if a future runner cannot create a window.</para>
/// </summary>
public class ScreenCaptureAffinityTests
{
    private const uint WsPopup = 0x80000000;

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowExW(
        uint dwExStyle, string lpClassName, string lpWindowName, uint dwStyle,
        int x, int y, int nWidth, int nHeight,
        IntPtr hWndParent, IntPtr hMenu, IntPtr hInstance, IntPtr lpParam);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyWindow(IntPtr hWnd);

    private static IntPtr CreateOwnedTopLevelWindow()
    {
        var hwnd = CreateWindowExW(
            0, "STATIC", "fauna-capture-affinity-test", WsPopup,
            0, 0, 1, 1, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (hwnd == IntPtr.Zero)
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "could not create the test window this assertion needs; the platform bit "
                    + "is unverified (the refcount half is still covered by "
                    + "ScreenCaptureGuardTests)");
        }
        return hwnd;
    }

    /// <summary>The platform actually carries the bit while a hold is live, and actually
    /// stops carrying it when the last hold goes — read back from the OS, not from the
    /// guard's own bookkeeping.</summary>
    [Fact]
    public void GuardSetsAndClearsTheRealExcludeFromCaptureBit()
    {
        var affinity = new Win32WindowDisplayAffinity();
        var guard = new ScreenCaptureGuard(affinity);
        var hwnd = CreateOwnedTopLevelWindow();
        try
        {
            Assert.True(affinity.TryGet(hwnd, out var before));
            Assert.Equal(ScreenCaptureGuard.WdaNone, before);

            guard.Acquire(hwnd);

            Assert.True(affinity.TryGet(hwnd, out var during));
            Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, during);

            guard.Release(hwnd);

            Assert.True(affinity.TryGet(hwnd, out var after));
            Assert.Equal(ScreenCaptureGuard.WdaNone, after);
        }
        finally
        {
            DestroyWindow(hwnd);
        }
    }

    /// <summary>The refcount rule, asserted against the real platform bit rather than the
    /// fake: a second reveal hiding must NOT un-suppress a window whose first reveal is
    /// still painted. This is the defect the whole design exists to prevent, so it is worth
    /// pinning on both sides of the seam.</summary>
    [Fact]
    public void TheRealBitSurvivesTheFirstOfTwoHoldersLeaving()
    {
        var affinity = new Win32WindowDisplayAffinity();
        var guard = new ScreenCaptureGuard(affinity);
        var hwnd = CreateOwnedTopLevelWindow();
        try
        {
            guard.Acquire(hwnd);
            guard.Acquire(hwnd);
            guard.Release(hwnd);

            Assert.True(affinity.TryGet(hwnd, out var stillHeld));
            Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, stillHeld);

            guard.Release(hwnd);

            Assert.True(affinity.TryGet(hwnd, out var released));
            Assert.Equal(ScreenCaptureGuard.WdaNone, released);
        }
        finally
        {
            DestroyWindow(hwnd);
        }
    }

    /// <summary>The end-to-end path a page actually uses: a <see cref="ScreenCaptureHold"/>
    /// synced to a predicate, over the real platform. Without this the two halves could each
    /// be right while the composition of them is not.</summary>
    [Fact]
    public void AHoldSyncedToAPredicateDrivesTheRealBit()
    {
        var affinity = new Win32WindowDisplayAffinity();
        var guard = new ScreenCaptureGuard(affinity);
        var hwnd = CreateOwnedTopLevelWindow();
        try
        {
            var hold = new ScreenCaptureHold(guard, () => hwnd);

            hold.Sync(true);
            Assert.True(affinity.TryGet(hwnd, out var revealed));
            Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, revealed);

            hold.Sync(false);
            Assert.True(affinity.TryGet(hwnd, out var hidden));
            Assert.Equal(ScreenCaptureGuard.WdaNone, hidden);
        }
        finally
        {
            DestroyWindow(hwnd);
        }
    }
}
