namespace FaunaApp.Core.Models;

/// <summary>
/// One <c>share-link-item</c> row of the Media page's <c>share-link-list</c>
/// (share-links.md § Flows → List) — a display projection of the shared
/// <c>ShareLinkSummary</c> (UniFFI <c>uniffi.fauna_media_machine</c>) that
/// <c>MediaPageSnapshot.share_links.rows</c> carries, newest first.
/// <para>
/// The summary record is <c>internal</c> to the generated bindings, so the page maps
/// each one to this public record for the <c>x:Bind</c> DataTemplate — the
/// <see cref="FileVersionRow"/> precedent. Every decision (the state, the verified
/// re-derived URL, the labels) was already made in shared Rust; this only carries it.
/// </para>
/// </summary>
/// <param name="TokenId">The registry id (hex) — the revoke key, carried on the row's
/// revoke <c>Button.Tag</c>.</param>
/// <param name="Name">The file's name, opened seal-first (<c>share-link-item-name</c>).</param>
/// <param name="Expires">"Expires ‹date›" for <c>share-link-item-expires</c>.</param>
/// <param name="State">The stable state value — <c>active</c> / <c>expired</c> /
/// <c>revoked</c> — the <c>share-link-item-state</c> <c>state</c> attribute a test
/// asserts on.</param>
/// <param name="StateLabel">The painted state label (shared <c>share_link_state_label</c>).</param>
/// <param name="Url">The verified re-derived URL, or <c>null</c> — the copy control is
/// then ABSENT, never a wrong link (share-links.md § Flows → List).</param>
public record ShareLinkRow(
    string TokenId,
    string Name,
    string Expires,
    string State,
    string StateLabel,
    string? Url
)
{
    /// <summary><c>share-link-item-copy-button</c> is present only where the URL
    /// re-derived and verified.</summary>
    public bool CanCopy => Url is not null;

    /// <summary><c>share-link-revoke-button</c> is present only on an Active row
    /// (there is no un-revoke; share-links.md § Flows → Revoke).</summary>
    public bool CanRevoke => State == "active";
}
