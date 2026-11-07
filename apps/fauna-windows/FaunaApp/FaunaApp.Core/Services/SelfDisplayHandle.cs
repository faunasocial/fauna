using System;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The signed-in account's handle as the profile SELF header DISPLAYS it —
/// <c>&lt;handle&gt;@&lt;current primary&gt;</c> (<c>mail-primary-domain-rename.md</c>
/// § Display-handle interaction: the display follows the primary, read when it is shown,
/// so a rename moves it with no new alias). The resolution is the shared
/// <c>fauna_client_accounts::display_handle</c> over UniFFI
/// (<see cref="FaunaFfiMethods.AccountDisplayHandle"/>) — never the bare handle; this
/// class only picks its inputs. Tui's <c>session_display_handle</c>, linux's
/// <c>self_header_fallback</c> and FaunaKit's <c>SessionState.displayHandle</c> are the twins.
///
/// <para><b>The inputs, per field:</b> the launch machine's live identity (its latest verify
/// reply — published, not persisted, so after a mid-session rename only it carries the new
/// primary) over the registry entry's server-data cache (<see cref="ISessionAccount"/>'s
/// <c>Handle</c>/<c>Domain</c>, written at every launch). An empty live field keeps the
/// cached one, the merge tui's <c>apply_self_identity</c> makes.</para>
/// </summary>
internal static class SelfDisplayHandle
{
    /// <summary>Raised (on the launch machine's thread) when a launch machine's published
    /// identity moved — the default <see cref="SelfAddressHealObserver.IdentityMoved"/>. A
    /// SELF header repaints on it — <c>DispatcherQueue.TryEnqueue</c> first — and
    /// unsubscribes when it leaves.</summary>
    internal static event Action? Moved;

    internal static void RaiseMoved() => Moved?.Invoke();

    /// <summary>The display handle from the live identity's halves over the cached ones,
    /// or <c>null</c> while nothing resolves (the header then shows the actor id).</summary>
    internal static string? Resolve(
        string? liveHandle, string? liveDomain, string? cachedHandle, string? cachedDomain)
    {
        var handle = Present(liveHandle) ?? Present(cachedHandle);
        var domain = Present(liveDomain) ?? Present(cachedDomain);
        return FaunaFfiMethods.AccountDisplayHandle(handle ?? "", handle, domain);
    }

    private static string? Present(string? s) => string.IsNullOrWhiteSpace(s) ? null : s;
}
