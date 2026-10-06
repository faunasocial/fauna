using System;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Services;

/// <summary>
/// <b>The platform leaf</b> of the region content plane on windows
/// (<c>region-blocking.md</c> § How an app obtains its region's policy) — the one
/// genuinely platform-divergent function: it names the declared region and its
/// source, and hands both to the shared plane (<see cref="RegionPlaneHost"/> over
/// <c>FfiRegionPlane.open</c>). Everything downstream is shared Rust.
///
/// <para>The answer is the user's <b>Windows "Country or region" setting</b>
/// (Settings → Time &amp; language → Language &amp; region), read through
/// <c>GlobalizationPreferences.HomeGeographicRegion</c> — a user-set, visible,
/// network-free declaration, source <c>SystemRegion</c>, never detected from the
/// network. It is <i>not</i> <c>RegionInfo.CurrentRegion</c>, which follows the
/// display-format culture rather than the region the user declared. The code is
/// handed over verbatim and shared Rust decides whether it is a region code at all;
/// one the registry enrols nobody for (the setting can hold a UN M.49 area such as
/// <c>419</c>) simply binds no policy.</para>
///
/// <para><b>A Store build reads the same setting.</b> The design has a
/// store-distributed build read its storefront; the Microsoft Store has no
/// storefront apart from this setting — its market IS the Windows region — so there
/// is no second, asynchronous leaf to wait on (apple's pending open) and no store
/// API call that could reach the network. No Store build of the app exists today.</para>
///
/// <para>No in-app override: a knob would make the declaration a choice rather than
/// a fact. In a test-capable build the shared e2e override
/// (<c>FAUNA_E2E_REGION_DECLARED</c>) replaces the code inside
/// <c>FfiRegionPlane.open</c>, keeping this source — no windows seam of its own.</para>
/// </summary>
internal static class RegionLeaf
{
    /// Open the device's plane with the leaf's answer. Called once at launch, before
    /// any view renders; a failure leaves the plane unopened, which renders every
    /// surface exactly as before the plane existed (the family arms alone).
    public static void OpenPlane()
    {
        try
        {
            RegionPlaneHost.Open(DeclaredCode(), FfiRegionSource.SystemRegion);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RegionLeaf", $"region plane not opened: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// The user's Windows region setting, or <c>null</c> when none can be read.
    private static string? DeclaredCode()
    {
        try
        {
            var code = Windows.System.UserProfile.GlobalizationPreferences.HomeGeographicRegion;
            return string.IsNullOrWhiteSpace(code) ? null : code;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RegionLeaf", $"region setting unreadable: {ex.GetType().Name}: {ex.Message}");
            return null;
        }
    }
}
