using FaunaApp.Core.Services;

namespace FaunaApp.Core.Models;

/// <summary>
/// A single media item rendered in the MediaPage cross-set explorer — a flattened
/// projection of the shared <c>MediaItemSummary</c> (UniFFI
/// <c>uniffi.fauna_media_machine</c>) the <c>fauna.media.list</c>-backed
/// <c>MediaMachine</c> aggregates across every readable folder (media.md
/// § State &amp; data shape). The page maps each snapshot item to one of these for
/// the ListView / GridView <c>media-item</c> DataTemplate; all sort/filter/derivation
/// already ran in shared Rust.
/// </summary>
public record MediaItem(
    /// <summary>Display name for <c>media-item-name</c> — the file's basename,
    /// derived in shared Rust (<c>MediaItemSummary.name</c>), so every app shows
    /// the same name.</summary>
    string Name,
    /// <summary>Folder-relative path. Not shown as a column; used as the row's
    /// <c>AutomationProperties.Name</c> so FlaUI can count <c>media-item</c> rows.</summary>
    string Path,
    /// <summary>The name of the readable folder this item belongs to — a label two
    /// sets can share. Not a column; the <c>(folder, path)</c> pair the gestures that
    /// still take a name use (<c>restore_version</c>, <c>download_file</c>,
    /// <c>open_share_create</c>).</summary>
    string Folder,
    /// <summary>The <c>media-folder-filter</c> value that scopes to this item's own
    /// set — an own set's name, a shared set's <c>local:&lt;id&gt;</c>
    /// (<c>MediaItemSummary.filterValue</c>, minted in shared Rust). What the detail
    /// surface hands <c>MediaMachine::{file_versions, delete}</c>, so a shared set's
    /// item lists and tombstones in ITS row, never in an own set of the same name
    /// (media.md § Encryption at rest → <i>The filter and the upload target address
    /// the row</i>, (b); file-sync.md § File Versions — a bare path_hash is ambiguous
    /// across sets).</summary>
    string FilterValue,
    ulong SizeBytes,
    ulong UpdatedAt,
    /// <summary>
    /// Thumbnail blob hash for <c>media-thumbnail</c>, or <c>null</c> when
    /// unavailable. NOTE: folder files carry no thumbnail association yet, so
    /// <c>fauna.media.list</c> returns <c>null</c> here today; the field is in the
    /// contract and renders once the uploader↔manifest thumbnail association lands
    /// (media.md § Implementation status). <c>ImageHashBind.ManageVisibility</c>
    /// collapses the image when the hash is null.
    /// </summary>
    string? ThumbnailHash,
    /// <summary>
    /// Localized source-liveness status string for <c>media-source-status</c>
    /// ("online" / "offline"), mapped from the shared
    /// <c>MediaItemSummary.sourceOnline</c> boolean (the backing folder's source
    /// device reachability — media.md § Source status vs. sync state).
    /// </summary>
    string SourceStatus,
    /// <summary>
    /// Whether <c>share-link-button</c> is offered on this item's detail — the shared
    /// <c>MediaItemSummary.share_link_eligible</c> verdict (the folder's owner-attested
    /// public audience; share-links.md § Which files can be linked), already
    /// <c>false</c> in a followed browse scope. The control is absent, never inert,
    /// when this is <c>false</c>.
    /// </summary>
    bool ShareLinkEligible = false
)
{
    /// <summary>
    /// Localized <c>sync-state-badge</c> text (file-sync.md § Per-file sync-status
    /// display). This page is a CONTROL-PLANE surface — <c>fauna.media.list</c>
    /// carries no per-file status, so every listed item is treated as present and
    /// renders the single shared "Synced" label, matching the web + linux media-page
    /// precedent (priority #1/#3); real per-file state lives in the out-of-app
    /// fauna-sync-agent's OS shell-overlay carve-out, not this WS-RPC list.
    /// Single-sourced via the shared <c>fauna_core::format::sync_display_state_label</c>
    /// over the value-format FFI wrapper — mirrors <c>FolderFileInfo.SyncStateLabel</c>.
    /// Computed get-only (no backing field) so the record's synthesized equality is
    /// unaffected. The badge color/icon stays a per-app render (none on windows v1,
    /// matching linux/web's text-only rendering of this single state).
    /// </summary>
    public string SyncStateLabel =>
        Strings.Resolve(
            uniffi.fauna_ffi.FaunaFfiMethods.SyncDisplayStateLabel(
                uniffi.fauna_core.SyncDisplayState.Synced));
}
