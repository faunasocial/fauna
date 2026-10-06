using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Models;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Platform-side model for the metadata-driven bridge link form
/// (<c>bridges.md</c> § Link modes + § Element IDs). The XAML
/// <c>BridgesPage</c> renders the active mode's fields generically, tagging each
/// input <c>bridge-link-field-{key}</c> — there is no per-bridge hard-coded
/// switch. Mirrors linux <c>views/bridges/detail.rs</c> and web
/// <c>bridges/+page.svelte</c> (priority #1/#2/#4: one shared shape across all 6
/// apps).
/// </summary>
public static class BridgeLinkForm
{
    /// <summary>This client family, used to scope <c>platform</c>-restricted modes
    /// (e.g. Nostr's NIP-07, <c>platform == "web"</c>).</summary>
    public const string Platform = "windows";

    /// <summary>The link modes renderable on this client. The platform
    /// string-match is the SHARED rule (lifted 2026-08-15 from the seven
    /// per-app copies — <c>fauna_client_bridges::mode_applies</c>, over the
    /// UniFFI <c>BridgeModeApplies</c>); windows supplies only its canonical
    /// name.</summary>
    public static IReadOnlyList<BridgeLinkMode> ModesForPlatform(BridgeInfo bridge) =>
        bridge.LinkModes
            .Where(m => uniffi.fauna_ffi.FaunaFfiMethods.BridgeModeApplies(m.Platform, Platform))
            .ToList();

    /// <summary>The e2e/UIA automation id for a field input — the raw
    /// provider-declared key (bridges.md § Implementation status today: clients tag
    /// <c>bridge-link-field-{field.key}</c>, e.g. <c>bridge-link-field-handle</c>).</summary>
    public static string FieldTestId(BridgeLinkField field) => $"bridge-link-field-{field.Key}";
}
