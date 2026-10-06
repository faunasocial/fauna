namespace FaunaApp.Core.Models;

/// <summary>
/// One address book in the Contacts page's Address Book picker (mirror
/// <see cref="CalendarInfo"/>). The <see cref="Id"/> is the hex-encoded 32-byte
/// <c>addressbook_id</c> — the <c>query_cards</c> read key (contacts.md
/// § Address Book segment; carddav-server.md § Independent enablement).
/// </summary>
public record AddressbookInfo(string Id, string Name, uint CardCount)
{
    /// <summary>Map a decoded <c>FfiAddressbookRow</c> → the picker display record.
    /// <c>internal</c> (not <c>public</c>) because the parameter type is a
    /// UniFFI-<c>internal</c> reply record — matches the accessibility of the
    /// FFI surface it wraps.</summary>
    internal static AddressbookInfo FromFfi(uniffi.fauna_ffi.FfiAddressbookRow row) =>
        new(row.id, row.name, row.cardCount);
}

/// <summary>
/// One decoded vCard flattened for display (mirror <see cref="EventInfo"/> +
/// linux <c>views::contacts::carddav_backend::VCardRow</c>). All sub-values are
/// already unsealed + parsed by the shared <c>fauna-client-carddav</c> crate;
/// this record only carries the fields the Address Book card list + detail pane
/// render. Read-only (slice 4b).
/// </summary>
public record CardInfo(
    string Id,
    /// <summary>`FN` — the card-list label + detail header.</summary>
    string FormattedName,
    IReadOnlyList<string> Emails,
    IReadOnlyList<string> Tels,
    /// <summary>Pre-formatted one-line addresses (<c>FfiPostalAddress.Formatted</c> —
    /// already joined by the shared crate, uniform across clients).</summary>
    IReadOnlyList<string> Addresses,
    /// <summary>`ORG` components joined with " · " (matches linux/web render),
    /// empty ⇒ hidden.</summary>
    string Org,
    /// <summary>`TITLE`, empty ⇒ hidden.</summary>
    string Title,
    /// <summary>`NOTE`, empty ⇒ hidden.</summary>
    string Note)
{
    /// <summary>
    /// Map a decoded <c>FfiCardRow</c> → the flat display record (card list +
    /// detail pane). Joins non-empty ORG components with " · " — byte-identical
    /// to the linux/web render (<c>carddav_backend::vcard_row</c>). <c>internal</c>
    /// (not <c>public</c>) because the parameter type is a UniFFI-<c>internal</c>
    /// reply record.
    /// </summary>
    internal static CardInfo FromFfi(uniffi.fauna_ffi.FfiCardRow row) => new(
        row.id,
        row.formattedName,
        row.emails.Select(v => v.value).ToList(),
        row.tels.Select(v => v.value).ToList(),
        row.addresses.Select(a => a.formatted).ToList(),
        string.Join(" · ", row.org.Where(s => !string.IsNullOrEmpty(s))),
        row.title,
        row.note);
}
