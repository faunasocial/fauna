using System;
using System.Linq;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_media_machine;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The share-link surfaces' display projections (share-links.md § Flows, § Expiry):
/// a <c>ShareLinkSummary</c> → <see cref="ShareLinkRow"/>, the expiry and state labels,
/// and which page error a share surface repeats on its own status line.
/// <para>
/// It lives here, not in <c>MediaPage.xaml.cs</c>, for <see cref="FileVersionFormat"/>'s
/// reason: the XAML project's code-behind is not reachable from <c>FaunaApp.Tests</c>.
/// Every label comes from shared Rust (<c>fauna_core::format::share_link_expiry_label</c> /
/// <c>share_link_state_label</c> / <c>format_unix_local_date</c>); an unknown value paints
/// raw, exactly as linux's <c>expiry_label</c> / <c>state_label</c> do.
/// </para>
/// </summary>
internal static class ShareLinkFormat
{
    /// <summary>The page-error keys the create surface repeats on its own status line.</summary>
    internal static readonly string[] CreateErrorKeys = { "share_link.error_create" };

    /// <summary>The page-error keys the list surface repeats on its own status line.</summary>
    internal static readonly string[] ListErrorKeys = { "share_link.error_list", "share_link.error_revoke" };

    internal static ShareLinkRow MapRow(ShareLinkSummary row) => new(
        TokenId: row.@tokenId,
        Name: row.@name,
        Expires: Strings.Format("share_link/expires", FaunaFfiMethods.FormatUnixLocalDate(row.@expiresAt)),
        State: row.@state,
        StateLabel: StateLabel(row.@state),
        Url: row.@url);

    /// <summary>The <c>share-link-expiry-select</c> label for an option value; the value
    /// itself stays the option's key so the cross-app <c>select(id, "7d")</c> holds.</summary>
    internal static string ExpiryLabel(string value) =>
        FaunaFfiMethods.ShareLinkExpiryLabel(value) is { } text ? Strings.Resolve(text) : value;

    /// <summary>The painted <c>share-link-item-state</c> label for a stable state value.</summary>
    internal static string StateLabel(string state) =>
        FaunaFfiMethods.ShareLinkStateLabel(state) is { } text ? Strings.Resolve(text) : state;

    /// <summary>
    /// The page error, resolved — but only when its key is one of <paramref name="keys"/>,
    /// so a surface never repeats an unrelated page error it did not cause. The share
    /// errors land in the page's <c>error-message</c> (the machine's contract), which sits
    /// behind these sheets where it cannot be read — the delete confirm's reasoning —
    /// so each surface repeats ITS errors, untagged (linux's <c>own_error</c>).
    /// </summary>
    internal static string? OwnError(uniffi.fauna_core.LocalizedText? error, string[] keys) =>
        error is { } e && keys.Contains(e.@key) ? Strings.Resolve(e) : null;
}
