using System;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The single place an <see cref="FfiAdminUser"/> becomes an admin picker's
/// OPTION TEXT (admin.md § 2 Users → <i>What identifies a user in an admin
/// picker</i>: the handle, falling back to the full actor hex for a
/// handle-less account — the editable, non-unique <c>label</c> is never a
/// picker identity). A thin pass-through to the shared
/// <c>fauna_client_admin::admin_picker_option</c> FFI face
/// (<see cref="FaunaFfiMethods.AdminPickerOption"/>), but the ONE C# entry
/// point both <c>AdminDnsPage</c>'s per-domain catch-all + four role-address
/// pickers and <c>AdminWebPage</c>'s apex picker call.
///
/// <c>AdminDnsPage</c> is WinUI code-behind with no VM/DI seam of its own, so
/// this static method in <c>FaunaApp.Core</c> — reachable from
/// <c>FaunaApp.Tests</c> via <c>InternalsVisibleTo</c> — is what makes the
/// two-same-label injectivity property unit-testable at all (mirrors
/// android's <c>actorOptions</c>). Never hand-roll
/// a second copy of the handle-else-hex match at a new build site — call this
/// instead.
/// </summary>
internal static class AdminActorOptions
{
    internal static string Label(FfiAdminUser user) => FaunaFfiMethods.AdminPickerOption(user);

    /// <summary>
    /// The picker's trailing "not loaded" option for a current designation that isn't
    /// among the fetched actors — the FULL un-truncated actor hex, not a short prefix
    /// (admin.md § 2's two-halves rule;: widened from a 4-byte prefix to match
    /// apple's/linux's/tui's/web's full hex).
    /// Mirrors linux's extracted <c>actor_not_loaded_fallback_label</c>; the ONE C# entry
    /// point both <c>AdminDnsPage.BuildActorPicker</c> and
    /// <c>AdminWebViewModel.BuildPicker</c> call — never hand-roll a second copy.
    /// </summary>
    internal static string NotLoadedFallbackLabel(byte[] id) =>
        Strings.Format("admin/actor_id_fallback_label", Convert.ToHexString(id).ToLowerInvariant());
}
