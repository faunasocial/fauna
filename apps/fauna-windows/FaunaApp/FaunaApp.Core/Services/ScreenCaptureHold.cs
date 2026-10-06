using System;

namespace FaunaApp.Core.Services;

/// <summary>
/// One call site's hold on <see cref="ScreenCaptureGuard"/> — <b>at most one</b>, by
/// construction.
///
/// <para><b>Why this type exists at all,</b> rather than every reveal calling
/// <c>Acquire</c>/<c>Release</c> directly: the dangerous mistake in this feature is a
/// double-acquire (or a missed release), and its symptom is a whole app that has silently
/// stopped taking screenshots — not an exception, not a failed test, just a machine the user
/// thinks is broken. So no call site is asked to reason about transitions. Each one owns one
/// of these and calls <see cref="Sync"/> with a plain predicate — "is a secret painted right
/// now?" — as often as it likes; the object makes the platform state match. Idempotent by
/// construction, so a page that re-renders ten times takes one hold. This is the port of
/// apple's <c>_ScreenCaptureProbeView.sync()</c>
/// (<c>FaunaKit/Sources/FaunaKit/Utilities/ScreenCapture.swift</c>), which exists for the
/// same reason.</para>
///
/// <para><b>Not thread-safe, deliberately.</b> Every call site is a XAML page or panel
/// driving it from the UI thread; the guard underneath takes a lock, so a stray background
/// call cannot corrupt the shared refcount — only this object's own single-hold bookkeeping.
/// Keeping it lock-free makes it obvious that the call sites are UI-thread code.</para>
///
/// <para>⚠ Rule 1 (<c>security.md</c> § On-screen secret exposure): never construct one of
/// these for <c>secret-key-display</c> or <c>recovery-kit-secret-display</c>. See
/// <see cref="ScreenCaptureGuard"/> for why that direction is the irreversible harm.</para>
/// </summary>
public sealed class ScreenCaptureHold : IDisposable
{
    private readonly ScreenCaptureGuard _guard;
    private readonly Func<IntPtr> _resolveWindow;

    /// <summary>The window this call site currently holds, or <see cref="IntPtr.Zero"/>.</summary>
    private IntPtr _held = IntPtr.Zero;

    public ScreenCaptureHold(ScreenCaptureGuard guard, Func<IntPtr> resolveWindow)
    {
        _guard = guard;
        _resolveWindow = resolveWindow;
    }

    /// <summary>The window currently held — test seam, and what a leak assertion reads.</summary>
    public IntPtr HeldWindow => _held;

    /// <summary>
    /// Make the platform state match <paramref name="isActive"/>: hold while a minted
    /// credential is actually painted, no hold otherwise.
    ///
    /// <para>Re-resolving the window on every activating call is not paranoia — it is what
    /// makes the hold survive the app window being replaced (the factory-reset re-onboard
    /// hand-off does exactly that), releasing the old one instead of stranding it forever.</para>
    /// </summary>
    public void Sync(bool isActive)
    {
        var target = isActive ? _resolveWindow() : IntPtr.Zero;
        if (target == _held) return;
        if (_held != IntPtr.Zero) _guard.Release(_held);
        _held = IntPtr.Zero;
        if (target == IntPtr.Zero) return;
        _guard.Acquire(target);
        _held = target;
    }

    /// <summary>The release of last resort, for a call site torn down without its own
    /// navigate-away running. Same job as apple's <c>deinit</c> hop.</summary>
    public void Dispose() => Sync(false);
}
