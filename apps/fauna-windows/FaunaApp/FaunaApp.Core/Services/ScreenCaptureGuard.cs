using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace FaunaApp.Core.Services;

/// <summary>
/// windows' implementation of <c>docs/goal/architecture/security.md</c> § On-screen
/// secret exposure (screen capture), <b>rule 2 only</b>: while a <i>minted, revocable</i>
/// credential is actually revealed on screen, the app window carries
/// <c>WDA_EXCLUDEFROMCAPTURE</c>, so screenshots, screen recording and screen-share
/// render it as an empty black region.
///
/// <para>⚠ <b>Rule 1 outranks rule 2 and forbids the obvious "completion" of this
/// feature.</b> <c>secret-key-display</c> (the identity secret) and
/// <c>recovery-kit-secret-display</c> show <b>client-only-resident</b> key material: a user
/// who loses it loses the account outright, with no recovery path. Users screenshot a
/// recovery kit precisely because that copy is what saves them, so suppressing capture
/// there would trade a shoulder-surfing risk for an account-loss risk — the irreversible
/// one. <b>Never take a hold from those screens.</b> The mail / Bluesky / Nostr reveals are
/// safe to suppress because losing one costs a revoke-and-re-mint and nothing else.
/// On windows only <c>secret-key-display</c> is rendered at all
/// (<c>Views/Onboarding/IdentityCreatedView.xaml</c>); the recovery kit has no windows
/// surface yet, so the rule covers one screen here and will cover two when it gains one.</para>
///
/// <para><b>Why the affinity is refcounted rather than set-and-cleared per call site.</b>
/// It is a property of the <i>window</i>, not of a control, and more than one reveal can be
/// on screen at once — the mail settings panel lists several credential rows, and a
/// navigation transition can briefly hold two pages. A plain set-on-show / clear-on-hide
/// pair per row is the bug: the first row to hide clears the flag while another secret is
/// still painted. Refcounting per HWND makes both failure directions unrepresentable, and
/// the direction that would otherwise be silent is the <i>stuck-on</i> one — a leaked hold
/// leaves the whole app uncapturable, which users experience as a broken machine rather
/// than as security. apple's macOS half needed exactly this
/// (<c>FaunaKit/Sources/FaunaKit/Utilities/ScreenCapture.swift</c>), and this is its port.</para>
///
/// <para>Lives in <c>FaunaApp.Core</c> rather than the WinUI project on purpose:
/// <c>FaunaApp.Tests</c> targets plain <c>net10.0</c> and references Core only, so a guard
/// placed beside the pages it serves would be untestable. The WinUI half is one thin file
/// (<c>FaunaApp/Helpers/SuppressScreenCapture.cs</c>) that resolves <c>App.MainWindow</c>'s
/// HWND — everything with logic in it is here.</para>
/// </summary>
public sealed class ScreenCaptureGuard
{
    /// <summary>The window is captured normally — the platform default, and what
    /// <see cref="Release"/> restores to unless something else had already narrowed it.</summary>
    public const uint WdaNone = 0x00000000;

    /// <summary>Win10 2004+. The window renders as an empty black region to capturers
    /// rather than failing the capture outright, which is the behavior this posture wants:
    /// a user screen-sharing does not lose their whole screen, only the revealed secret.</summary>
    public const uint WdaExcludeFromCapture = 0x00000011;

    /// <summary>The platform seam. Production is <see cref="Win32WindowDisplayAffinity"/>;
    /// tests substitute a fake so the refcount arithmetic — including the release-on-teardown
    /// path, the half that goes wrong silently — is exercised with no window at all.</summary>
    private readonly IWindowDisplayAffinity _affinity;

    private sealed class Hold
    {
        public int Count;
        public uint Previous;
    }

    private readonly Dictionary<IntPtr, Hold> _holds = new();
    private readonly object _lock = new();

    public ScreenCaptureGuard(IWindowDisplayAffinity affinity) => _affinity = affinity;

    /// <summary>The app-wide guard. One per process because the affinity it manipulates is
    /// per-HWND global state: a second guard would keep a second count and the two would
    /// clear each other's suppression.</summary>
    public static ScreenCaptureGuard Shared { get; } = new(new Win32WindowDisplayAffinity());

    /// <summary>Take a hold on <paramref name="hwnd"/>. The first hold flips it to
    /// <see cref="WdaExcludeFromCapture"/>; later holds only bump the count.</summary>
    public void Acquire(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        lock (_lock)
        {
            if (_holds.TryGetValue(hwnd, out var hold))
            {
                hold.Count++;
                return;
            }
            // Remember what was there rather than assuming WdaNone: this must not silently
            // widen a window some other code had already narrowed. Reading it back can fail
            // (a destroyed HWND), in which case the platform default is the honest guess.
            var previous = _affinity.TryGet(hwnd, out var current) ? current : WdaNone;
            _holds[hwnd] = new Hold { Count = 1, Previous = previous };
            _affinity.Set(hwnd, WdaExcludeFromCapture);
        }
    }

    /// <summary>Drop a hold on <paramref name="hwnd"/>. The last one restores the affinity
    /// that was there before the first — never a hard-coded <see cref="WdaNone"/>.
    /// Releasing a window with no hold is a no-op, not a fault: a call site that releases
    /// twice must not push the count negative and strand the next acquire.</summary>
    public void Release(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        lock (_lock)
        {
            if (!_holds.TryGetValue(hwnd, out var hold)) return;
            hold.Count--;
            if (hold.Count > 0) return;
            _affinity.Set(hwnd, hold.Previous);
            _holds.Remove(hwnd);
        }
    }

    /// <summary>Live hold count for <paramref name="hwnd"/> — the test seam, and what a
    /// leak assertion reads.</summary>
    public int HoldCount(IntPtr hwnd)
    {
        lock (_lock)
        {
            return _holds.TryGetValue(hwnd, out var hold) ? hold.Count : 0;
        }
    }
}

/// <summary>The one platform call this feature makes, behind an interface so the refcount
/// above is testable without a window. Two methods, both thin.</summary>
public interface IWindowDisplayAffinity
{
    /// <summary>Read the window's current display affinity. <c>false</c> when the platform
    /// refuses (a destroyed or foreign HWND).</summary>
    bool TryGet(IntPtr hwnd, out uint affinity);

    /// <summary>Set the window's display affinity. <c>false</c> when the platform refuses.</summary>
    bool Set(IntPtr hwnd, uint affinity);
}

/// <summary>
/// The real thing: <c>user32!SetWindowDisplayAffinity</c> / <c>GetWindowDisplayAffinity</c>.
///
/// <para>These are the two declarations the track owes — neither entry point existed anywhere
/// in the tree, so "a call site rather than new interop" was true only of the HWND lookup.
/// Declaration style follows <c>FaunaApp/Services/TrayIconService.cs</c>, the app's other
/// <c>user32</c> consumer.</para>
///
/// <para>⚠ <c>SetWindowDisplayAffinity</c> only accepts a <b>top-level window owned by the
/// calling process</b> — it fails with <c>ERROR_ACCESS_DENIED</c> otherwise. That is why the
/// real-bit test creates its own window rather than borrowing one.</para>
/// </summary>
public sealed class Win32WindowDisplayAffinity : IWindowDisplayAffinity
{
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowDisplayAffinity(IntPtr hWnd, uint dwAffinity);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowDisplayAffinity(IntPtr hWnd, out uint pdwAffinity);

    public bool TryGet(IntPtr hwnd, out uint affinity) => GetWindowDisplayAffinity(hwnd, out affinity);

    public bool Set(IntPtr hwnd, uint affinity) => SetWindowDisplayAffinity(hwnd, affinity);
}
