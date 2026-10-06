using System;
using System.Collections.Generic;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The refcount half of windows' capture suppression — <c>docs/goal/architecture/security.md</c>
/// § On-screen secret exposure (screen capture), rule 2.
///
/// <para>Split from <see cref="ScreenCaptureAffinityTests"/> the way android splits its own
/// leg: the arithmetic that decides <em>when</em> the platform bit is set runs against a fake
/// seam with no window at all, and one separate test asserts the real
/// <c>WDA_EXCLUDEFROMCAPTURE</c> bit. The reason for the split is that the silent failure in
/// this feature is a refcount mistake, not a P/Invoke mistake — a stuck-on hold leaves the
/// whole app uncapturable, which reaches the user as a broken machine rather than as a test
/// failure — so that half must be pinned even where a real HWND is unavailable.</para>
/// </summary>
public class ScreenCaptureGuardTests
{
    /// <summary>Records what the guard did to the platform, and lets a test pretend the
    /// window already carried some other affinity.</summary>
    private sealed class FakeAffinity : IWindowDisplayAffinity
    {
        public readonly Dictionary<IntPtr, uint> Current = new();
        public readonly List<(IntPtr Hwnd, uint Affinity)> Sets = new();
        public bool GetFails;

        public bool TryGet(IntPtr hwnd, out uint affinity)
        {
            affinity = 0;
            if (GetFails) return false;
            affinity = Current.TryGetValue(hwnd, out var v) ? v : ScreenCaptureGuard.WdaNone;
            return true;
        }

        public bool Set(IntPtr hwnd, uint affinity)
        {
            Current[hwnd] = affinity;
            Sets.Add((hwnd, affinity));
            return true;
        }
    }

    private static readonly IntPtr WindowA = new(0x1111);
    private static readonly IntPtr WindowB = new(0x2222);

    [Fact]
    public void FirstAcquireSuppresses_AndLastReleaseRestores()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(WindowA);

        Assert.Equal(1, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowA]);

        guard.Release(WindowA);

        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
    }

    /// <summary>The bug the row and the goal doc both name: a per-row set/clear pair lets the
    /// FIRST row to hide un-suppress while another secret is still painted. Refcounting is
    /// what makes that unrepresentable, so this is the test the feature exists for.</summary>
    [Fact]
    public void SecondHolderKeepsSuppressionAfterTheFirstReleases()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(WindowA);
        guard.Acquire(WindowA);
        guard.Release(WindowA);

        Assert.Equal(1, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowA]);

        guard.Release(WindowA);

        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
    }

    /// <summary>Restore the REMEMBERED value, never a hard-coded <c>WDA_NONE</c>: this must
    /// not silently widen a window something else had already narrowed.</summary>
    [Fact]
    public void ReleaseRestoresThePreviousAffinity_NotTheDefault()
    {
        const uint WdaMonitor = 0x00000001; // the other documented affinity
        var fake = new FakeAffinity();
        fake.Current[WindowA] = WdaMonitor;
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(WindowA);
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowA]);

        guard.Release(WindowA);
        Assert.Equal(WdaMonitor, fake.Current[WindowA]);
    }

    /// <summary>Two windows are independent — the navigation-transition case, where two
    /// pages briefly coexist and each holds its own.</summary>
    [Fact]
    public void HoldsAreScopedPerWindow()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(WindowA);
        guard.Acquire(WindowB);
        guard.Release(WindowA);

        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(1, guard.HoldCount(WindowB));
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowB]);
    }

    /// <summary>An over-release must not push the count negative and strand the NEXT acquire
    /// — that failure mode would show up as a reveal that quietly stopped being protected,
    /// which is the direction with no visible symptom at all.</summary>
    [Fact]
    public void ReleasingAnUnheldWindowIsANoOp()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);

        guard.Release(WindowA);
        Assert.Empty(fake.Sets);

        guard.Acquire(WindowA);
        Assert.Equal(1, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowA]);
    }

    [Fact]
    public void ZeroHandleIsIgnored()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(IntPtr.Zero);
        guard.Release(IntPtr.Zero);

        Assert.Empty(fake.Sets);
        Assert.Equal(0, guard.HoldCount(IntPtr.Zero));
    }

    /// <summary>A read-back failure (a destroyed HWND) must not stop the suppression from
    /// being applied — the platform default is the honest fallback for what to restore.</summary>
    [Fact]
    public void SuppressesEvenWhenThePreviousAffinityCannotBeRead()
    {
        var fake = new FakeAffinity { GetFails = true };
        var guard = new ScreenCaptureGuard(fake);

        guard.Acquire(WindowA);
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowA]);

        guard.Release(WindowA);
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
    }

    // ── ScreenCaptureHold: one call site, at most one hold ────────────────────────

    /// <summary>The invariant that makes a double-acquire unreachable from a page. A render
    /// pass runs on every gesture, so <c>Sync(true)</c> is called many times over one
    /// reveal; if each took a hold, the app would be permanently uncapturable.</summary>
    [Fact]
    public void HoldSyncIsIdempotent()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);
        var hold = new ScreenCaptureHold(guard, () => WindowA);

        hold.Sync(true);
        hold.Sync(true);
        hold.Sync(true);

        Assert.Equal(1, guard.HoldCount(WindowA));

        hold.Sync(false);
        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(IntPtr.Zero, hold.HeldWindow);

        // And repeated release is equally harmless.
        hold.Sync(false);
        Assert.Equal(0, guard.HoldCount(WindowA));
    }

    /// <summary>Disposal is the release of last resort — a call site torn down without its
    /// own navigate-away running must not strand a hold.</summary>
    [Fact]
    public void DisposingAHoldReleasesIt()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);
        var hold = new ScreenCaptureHold(guard, () => WindowA);

        hold.Sync(true);
        Assert.Equal(1, guard.HoldCount(WindowA));

        hold.Dispose();
        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
    }

    /// <summary>If the app window is replaced under a live hold (the factory-reset re-onboard
    /// hand-off does exactly that), the hold moves rather than stranding the old window
    /// uncapturable forever.</summary>
    [Fact]
    public void HoldFollowsAReplacedWindow()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);
        var current = WindowA;
        var hold = new ScreenCaptureHold(guard, () => current);

        hold.Sync(true);
        Assert.Equal(1, guard.HoldCount(WindowA));

        current = WindowB;
        hold.Sync(true);

        Assert.Equal(0, guard.HoldCount(WindowA));
        Assert.Equal(1, guard.HoldCount(WindowB));
        Assert.Equal(ScreenCaptureGuard.WdaNone, fake.Current[WindowA]);
        Assert.Equal(ScreenCaptureGuard.WdaExcludeFromCapture, fake.Current[WindowB]);
    }

    /// <summary>No window yet (a render pass racing startup) is "nothing to suppress", not a
    /// crash and not a stranded hold.</summary>
    [Fact]
    public void HoldWithNoWindowIsInert()
    {
        var fake = new FakeAffinity();
        var guard = new ScreenCaptureGuard(fake);
        var hold = new ScreenCaptureHold(guard, () => IntPtr.Zero);

        hold.Sync(true);

        Assert.Empty(fake.Sets);
        Assert.Equal(IntPtr.Zero, hold.HeldWindow);
    }
}
