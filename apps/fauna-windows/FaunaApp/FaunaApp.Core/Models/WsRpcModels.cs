using FaunaApp.Core.Services;

namespace FaunaApp.Core.Models;

// Data records carried by the WS-RPC path (fauna.contacts.*, fauna.sync.*,
// fauna.snapshots.*, …) and rendered by the app's view models.
//
// These are plain data shapes: the app decodes the wire via the shared UniFFI
// surface (uniffi.fauna_ffi) and projects into these records, so nothing here
// speaks a codec of its own. They carried hand-written dag-cbor Encode/Decode
// while the app talked to fauna-sync-service over the named pipe
// (\\.\pipe\fauna-service); that transport was retired onto the shared FFI
// provisioner and the codec deleted with it, leaving the data + the two
// shared-FFI display-label members below.

// ── Enums ──

public enum ConnectionState : uint
{
    Connected = 0,
    Connecting = 1,
    Disconnected = 2,
    /// Connecting has failed enough times running that the gap is no longer
    /// transient — the indicator says "Cannot connect" rather than an indefinite
    /// "Connecting…". Additive (a new value, never a renumbering).
    /// `fauna_ws_substrate::supervisor::ConnectionState::Unreachable`.
    Unreachable = 3,
}

public enum ContactStatus : uint
{
    Accepted = 0,
    Pending = 1,
    Blocked = 2,
    // A mutually-confirmed edge (`fauna.contacts.confirm` promotes `accepted` →
    // `confirmed`; contacts.md § Persistence). Previously collapsed into Accepted at
    // the RPC boundary, which dropped the distinct "Confirmed" label — kept distinct
    // now so the shared contact_status_label renders it correctly (contacts.md
    // § Where logic lives → Status badge text).
    Confirmed = 3,
    // A relationship status this build does not name (a newer nest's). It is no
    // longer filed under Accepted: that painted a confirm button and the "Accepted"
    // label on it, and put it in the accepted section. Its wire string rides
    // ContactInfo.WireStatus, which the label and the roster grouping read
    // (contacts.md § Where logic lives → Roster status grouping).
    Other = 4,
}

/// <summary>The <c>FfiContactItem.status</c> wire string → <see cref="ContactStatus"/>.
/// The four statuses this build names map 1:1; any other is
/// <see cref="ContactStatus.Other"/>, its string kept on
/// <see cref="ContactInfo.WireStatus"/>.</summary>
public static class ContactStatusWire
{
    public static ContactStatus Parse(string wire) => wire switch
    {
        "pending" => ContactStatus.Pending,
        "accepted" => ContactStatus.Accepted,
        "confirmed" => ContactStatus.Confirmed,
        "blocked" => ContactStatus.Blocked,
        _ => ContactStatus.Other,
    };
}

// ── Data records ──

public record IdentityInfo(string ActorId, string? Handle, string? NestUrl);

public record SyncStatusInfo(
    bool Connected,
    bool Syncing,
    ulong FilesPending,
    ulong BytesPending,
    ulong? LastSync);

public record ServiceStatusInfo(
    string Version,
    ulong UptimeSecs,
    ConnectionState Connection,
    SyncStatusInfo Sync,
    IdentityInfo? Identity);

public record ContactInfo(string ActorId, string? Handle, ContactStatus Status, ulong? UpdatedAt)
{
    /// <summary>The contact's nest domain (host part of <c>handle@domain</c>), a LIVE
    /// WS-RPC enrichment from <c>FfiContactItem.domain</c> — fed to the shared
    /// roster-filter predicate so windows matches on domain too (<c>contacts.md</c>
    /// § Where logic lives → Contact roster filter). Init-only.</summary>
    public string? Domain { get; init; }

    /// <summary>The peer nest's URL a Find User lookup answered for a contact on
    /// ANOTHER nest (<c>find_user</c>'s <c>nest_url</c>, empty/null for a home-nest
    /// actor) — carried into the knock send as <c>recipient_nest_url</c>, never
    /// re-derived (federation.md § Client-side peer discovery for a cross-nest knock).
    /// Init-only; only the Find User result sets it.</summary>
    public string? NestUrl { get; init; }

    /// <summary>The peer's home nest (<c>FfiContactItem.home_nest_url</c>, a roster
    /// row) — the contact-row door carries it into the profile opener so a follow
    /// from that page reaches the contact's own nest. Init-only.</summary>
    public string? HomeNestUrl { get; init; }

    /// <summary>The relationship status as the wire spelled it
    /// (<c>FfiContactItem.status</c>, lowercased) — the string the shared status
    /// functions take. Defaults to the enum name lowercased, which IS the wire string
    /// for the four statuses this build names; <c>NestRpcClient.ContactsListAsync</c>
    /// sets it from the wire, so a status this build does not name
    /// (<see cref="ContactStatus.Other"/>) keeps its own string, label and roster
    /// section. Init-only.</summary>
    public string WireStatus { get; init; } = Status.ToString().ToLowerInvariant();

    /// <summary>
    /// Localized status badge text (pending/accepted/confirmed/blocked, or an unnamed
    /// status's own text) off the shared <c>fauna_core::format::contact_status_label</c>
    /// via the value-format FFI wrapper — the single source of truth across clients
    /// (contacts.md § Where logic lives → Status badge text; priority #1/#2/#4).
    /// Replaces the per-app <c>StatusToLabel</c> switch, which lacked a
    /// <c>Confirmed</c> arm and rendered "Unknown". Computed get-only (no backing
    /// field) so the record's synthesized equality is unaffected. The status
    /// <b>color</b> stays the per-app <c>StatusToBrush</c> map (not lifted, per the
    /// goal doc).
    /// </summary>
    public string StatusLabel => ContactStatusHeading(WireStatus);

    /// <summary>The shared label for a wire status string, resolved — a row's badge
    /// and a roster section's heading read the same text.</summary>
    public static string ContactStatusHeading(string wireStatus) =>
        Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ContactStatusLabel(wireStatus));
}

/// <summary>One painted roster row: the wire row plus the names the shared overlay
/// face answered for it (<c>ContactOverlayNames</c>; contacts.md § The private
/// overlay → <i>Where the nickname paints</i>). <see cref="Name"/> is the
/// <c>contact-name</c> line — the viewer's nickname, else the handle, else the
/// canonical short id — and is NEVER empty, because <c>ContactsPage.xaml</c> binds it as
/// the <c>contact-row</c> root's <c>AutomationProperties.Name</c> too, and a
/// <c>Grid</c> root with no Name is pruned from UI Automation.
/// <see cref="PublicName"/> (<c>contact-public-name</c>) is the public name a
/// nickname replaced and <see cref="Labels"/> (<c>contact-labels</c>) the viewer's
/// label line; each is null when absent, and the element is then collapsed. Built
/// only by <c>ContactsViewModel</c>; no field here is windows-derived.</summary>
public sealed record ContactRow(ContactInfo Contact, string Name, string? PublicName, string? Labels)
{
    public string ActorId => Contact.ActorId;
    public ContactStatus Status => Contact.Status;
    public string StatusLabel => Contact.StatusLabel;
    public string? HomeNestUrl => Contact.HomeNestUrl;

    /// <summary>Phase two's affordances on this row (<c>archive-import.md</c> § Linking
    /// and merging, ruling 8) — one paint per asked or linked pair, empty for every
    /// other row. Built by <c>ArchiveLinksHost.RowPaints</c> from the shared views.</summary>
    public IReadOnlyList<ArchiveLinkRowPaint> ArchiveLinks { get; init; } = Array.Empty<ArchiveLinkRowPaint>();
}

/// <summary>One section of the grouped contacts roster (contacts.md § Where logic
/// lives → Roster status grouping): the rows holding one relationship
/// <see cref="Status"/> (the wire string), in the roster's own order, under
/// <see cref="Heading"/> — the shared <c>contact_status_label</c>, the text each row's
/// badge reads too. The roster's sections come in the shared
/// <c>contact_roster_group_order</c>; the page paints them as a grouped
/// <c>ListView</c>, so the indexed <c>contact-row</c> ids count across sections in
/// paint order. The heading carries no element id (ui.yaml scopes none). Built only
/// by <c>ContactsViewModel</c>. Public with public members because a WinUI
/// <c>x:Bind</c> to an internal type reads null.</summary>
public sealed class ContactRosterSection : List<ContactRow>
{
    public ContactRosterSection(string status, IEnumerable<ContactRow> rows) : base(rows)
    {
        Status = status;
        Heading = ContactInfo.ContactStatusHeading(status);
    }

    public string Status { get; }
    public string Heading { get; }
}

/// <summary>Which of the two archive-link shapes a Contacts row paints.</summary>
public enum ArchiveLinkRowKind { Asked, Linked }

/// <summary>What one Contacts row paints for phase two of archive import
/// (<c>archive-import.md</c> § Linking and merging, ruling 8; <c>contacts.md</c> § User
/// actions): <see cref="ArchiveLinkRowKind.Asked"/> — <c>contact-archive-link-prompt</c>
/// (<see cref="Text"/>) with <c>contact-archive-link-yes</c> / <c>-no</c>;
/// <see cref="ArchiveLinkRowKind.Linked"/> — <c>contact-archive-linked</c>
/// (<see cref="Text"/>) with <c>contact-archive-unlink</c>. <see cref="Text"/> is the
/// machine crate's composer resolved through the localizer, never worded here;
/// <see cref="Contact"/> and <see cref="Platform"/> are the view's own, carried into the
/// shared answer and unlink calls. Public with public members because a WinUI
/// <c>x:Bind</c> to an internal type reads null.</summary>
public sealed record ArchiveLinkRowPaint(ArchiveLinkRowKind Kind, string Text, string Contact, string Platform)
{
    public bool IsAsked => Kind == ArchiveLinkRowKind.Asked;
    public bool IsLinked => Kind == ArchiveLinkRowKind.Linked;
}

/// <summary>A pending incoming knock. <c>fauna.knocks.list</c> carries the sender's
/// actor id but no handle.</summary>
public record KnockInfo(string ActorId, string? Summary, ulong Timestamp);

/// <summary>One painted knock card: the <c>knock-sender</c> text is the shared
/// resolver's answer for the sender (the viewer's nickname, else the canonical short
/// id — <c>peer_label(None, None, id)</c>), never empty because it is also the
/// <c>knock-card</c> root's <c>AutomationProperties.Name</c>.
/// <paramref name="AwaitsGuardian"/> paints the card's <c>knock-guardian-pending</c>
/// (<c>WardAsksCache.KnocksAwaitGuardian</c> when the row was built).</summary>
public sealed record KnockRow(KnockInfo Knock, string SenderLabel, bool AwaitsGuardian = false)
{
    public string ActorId => Knock.ActorId;
    public string? Summary => Knock.Summary;
}

public record SnapshotInfo(
    ulong Id,
    ulong FileCount,
    ulong TotalBytes,
    ulong CreatedAt,
    List<string> Tags,
    string? DeviceId);

public record SnapshotDetailInfo(
    ulong Id,
    string Folder,
    ulong FileCount,
    ulong TotalBytes,
    ulong CreatedAt,
    List<string> Tags,
    string? DeviceId,
    List<SnapshotFileInfo> Files);

public record SnapshotFileInfo(string Path, ulong SizeBytes, string FileType);

public record FolderStatus(string Name, ulong? LastChangeAt);

public record FolderFileInfo(string Path, ulong SizeBytes, ulong UpdatedAt)
{
    /// <summary>
    /// Localized in-app sync-state badge text. The windows media page is a
    /// CONTROL-PLANE surface (file-sync.md § Per-file sync-status display): it
    /// lists fauna.sync.files (which carries no per-file status — the row is
    /// path/size/updated_at only) and treats every listed file as present and
    /// available, so the badge renders the shared "Synced" label, matching the
    /// web + linux media-page precedent (priority #1/#3). Real per-file engine
    /// state lives in the out-of-app fauna-sync-agent (the OS shell-overlay
    /// carve-out), NOT this WS-RPC list, so no fauna_sync_engine::SyncState::
    /// to_display() is needed here. Single-sourced via the shared
    /// fauna_core::format::sync_display_state_label over the value-format FFI
    /// wrapper. Computed get-only (no backing field) so the record's synthesized
    /// equality is unaffected (the ContactInfo.StatusLabel pattern). The badge
    /// color/icon stays a per-app render.
    /// </summary>
    public string SyncStateLabel =>
        Strings.Resolve(
            uniffi.fauna_ffi.FaunaFfiMethods.SyncDisplayStateLabel(
                uniffi.fauna_core.SyncDisplayState.Synced));
}

public record ConflictInfo(long Id, string Path, string ConflictType, string? Details, ulong CreatedAt);
