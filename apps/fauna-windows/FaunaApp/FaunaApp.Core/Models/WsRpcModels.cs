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

    /// <summary>
    /// Localized status badge text (pending/accepted/confirmed/blocked) off the
    /// shared <c>fauna_core::format::contact_status_label</c> via the value-format
    /// FFI wrapper — the single source of truth across clients (contacts.md
    /// § Where logic lives → Status badge text; priority #1/#2/#4). Replaces the
    /// per-app <c>StatusToLabel</c> switch, which lacked a <c>Confirmed</c> arm
    /// and rendered "Unknown". The enum name lowercases to the canonical wire
    /// status string the shared fn expects. Computed get-only (no backing field) so
    /// the record's synthesized equality is unaffected. The status <b>color</b>
    /// stays the per-app <c>StatusToBrush</c> map (not lifted, per the goal doc).
    /// </summary>
    public string StatusLabel =>
        Strings.Resolve(
            uniffi.fauna_ffi.FaunaFfiMethods.ContactStatusLabel(Status.ToString().ToLowerInvariant()));

    /// <summary>
    /// The row's name: the handle, or the shared short id when the row carries none
    /// (a federated peer — contacts.md § State & data shape). Never empty, because
    /// <c>ContactsPage.xaml</c> binds it as the <c>contact-row</c> root's
    /// <c>AutomationProperties.Name</c> too, and a <c>Grid</c> root with no Name is
    /// pruned from UI Automation. A binding to the nullable <see cref="Handle"/> could
    /// not guarantee that: <c>FallbackValue</c> does not cover a null value.
    /// </summary>
    public string DisplayLabel =>
        string.IsNullOrEmpty(Handle) ? uniffi.fauna_ffi.FaunaFfiMethods.ShortId(ActorId) : Handle;
}

/// <summary>A pending incoming knock. <c>fauna.knocks.list</c> carries the sender's
/// actor id but no handle, so the row shows the id.</summary>
public record KnockInfo(string ActorId, string? Summary, ulong Timestamp)
{
    /// <summary>
    /// The <c>knock-sender</c> text, "Sender ID display" (ui.yaml): the shared short id
    /// of the sender, web's shape (<c>shortId(knock.sender)</c>). Also the
    /// <c>knock-card</c> root's <c>AutomationProperties.Name</c>, so it must never be
    /// empty; see <see cref="ContactInfo.DisplayLabel"/> for why.
    /// </summary>
    public string SenderLabel => uniffi.fauna_ffi.FaunaFfiMethods.ShortId(ActorId);
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
