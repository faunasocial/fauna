using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Recognises the first-listing gate's refusal (<c>common.needs_nest</c> /
/// <c>common.needs_other_device</c>) on a failed preference-page load, through
/// the one shared predicate <c>refused_as_not_ready</c>
/// (<c>account-client-lifecycle.md</c> § The client-side lifecycle → <i>The first
/// listing</i>, clauses (4) and (5)). A page whose load is refused shows the
/// reason on <c>error-message</c> and paints no empty state or default value as
/// the account's own — the same answer as tui, linux, web, android, macos and ios.
/// </summary>
internal static class FirstListingRefusal
{
    /// <summary>The refusal text when <paramref name="text"/> is the gate's reason, else <c>null</c>.</summary>
    internal static string? OfText(string? text) =>
        !string.IsNullOrEmpty(text) && FaunaFfiMethods.RefusedAsNotReady(text) ? text : null;

    /// <summary>The refusal text of a failed load, else <c>null</c>. Reads the
    /// shared-Rust <c>General</c> variant's <c>msg</c> — the user-facing sentence —
    /// the same unwrap <see cref="Strings.Error"/> makes, never the aggregated
    /// <c>msg=…</c> form of <see cref="Exception.Message"/>.</summary>
    internal static string? Of(Exception ex) => OfText(Strings.Error(ex));
}
