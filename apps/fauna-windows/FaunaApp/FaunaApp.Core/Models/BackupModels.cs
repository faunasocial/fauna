using System.Collections.Generic;

namespace FaunaApp.Core.Models;

/// <summary>
/// One configured backup destination, projected for the Backups page's
/// destination rows (<c>docs/goal/ui/backups.md</c> § Manage backup
/// destinations). <see cref="Label"/> is the display name with a URL-host
/// fallback (mirrors linux <c>destinations.rs</c> <c>destination_label</c>);
/// <see cref="Url"/> prefills the edit dialog and <see cref="Id"/> is the
/// edit/remove target. The per-row live status (<c>last-upload-time</c> /
/// <c>backlog-count</c>) renders a not-yet-backed-up baseline until the
/// coordinator's upload path lands (nest Plan 4 / Track B), uniform with linux.
///
/// <para><see cref="Kind"/>/<see cref="CustodianDeviceId"/>/<see cref="CapacityCapBytes"/>
/// landed 2026-08-03 with the client-device kind (<c>backups.md</c> § Third
/// destination kind) — mirror <c>FfiBackupDestinationView</c>'s own three
/// columns exactly. <see cref="Kind"/> feeds <c>backup_destination_kind_label</c>
/// for the badge (never string-matched directly — an unrecognised kind must
/// render as itself); the sole-client-destination predicate reads the RAW
/// <c>FfiBackupDestinationView</c> list the VM keeps alongside this projection,
/// never a reconstruction from this row (see <c>BackupsViewModel._destinationViews</c>).</para>
///
/// <para><see cref="Unattested"/> is the shared at-rest verdict
/// (<c>FfiBackupDestinationView.unattested</c>) — true only on a row an identity
/// succession carried across from a predecessor's config that the owner has not
/// yet adjudicated; it gates <c>backup-destination-unattested-mark</c> +
/// <c>backup-destination-keep-button</c> (<c>succession-aftermath.md</c>
/// § Adjudicating what the aftermath carries across). Projected verbatim, never
/// re-derived here.</para>
/// </summary>
public sealed record BackupDestinationRow(
    string Id, string Url, string Label,
    string Kind, string? CustodianDeviceId, ulong? CapacityCapBytes,
    bool Unattested = false);

/// <summary>
/// One <c>snapshot-item[i]</c> row, projected from the shared machine's
/// <c>SnapshotRow</c> (<c>backups.md</c> § Snapshot-list shape → <i>Row content
/// contract</i>). <see cref="Text"/> is the visible line — formatted
/// <c>created_at</c>, file count and formatted size, plus a non-<c>Active</c>
/// lifecycle state and, once a check has run this session, the derived integrity
/// verdict. It deliberately replaces the raw <c>SnapshotInfo</c> record
/// <c>ToString()</c> windows used to bind ("… Id = 7, TotalBytes = 4096 …"),
/// which the contract calls out by name as the shape to retire.
///
/// <para><see cref="IdText"/> is stamped on the row's immediate-delete button as
/// <c>AutomationProperties.HelpText</c> so the automation layer can read the id
/// back — the windows arm of the cross-app "every row exposes its snapshot id"
/// contract, and deliberately WEB's shape (<c>data-snapshot-id</c> on the same
/// per-row button) rather than linux/tui's row-level test-attr: a scoped FlaUI
/// read resolves by <c>FindAllDescendants</c> from the scope element, and a row
/// container is not its own descendant, so an id stamped on the container itself
/// would be unreadable at <c>scope="snapshot-item[i]"</c>. The immediate-delete
/// friction bar is its first consumer: the user must re-type the exact id, so
/// the test has to read it off the row it targets.</para>
///
/// <para><see cref="ActionsEnabled"/> is the single-flight gate — false while
/// ANY op is in flight (<c>in_progress_op</c>), so no per-row control can drift
/// off the page-level predicate.</para>
///
/// <para><see cref="Recoverable"/> gates <c>snapshot-undelete-button</c>'s
/// visibility (<c>backups.md</c> § *Soft-deleted rows*): the control renders
/// ONLY on a <c>SoftDeleted</c> row — presence is the state observable, same
/// shape as <c>snapshot-prune-execute-button</c>. This is an affordance guard
/// only; the machine refuses the gesture for any other row regardless.</para>
/// </summary>
public sealed record SnapshotDisplayRow(
    long Id, string Text, bool ActionsEnabled, bool Recoverable)
{
    /// <summary>The snapshot id as the automation layer reads it (x:Bind takes a
    /// string; a computed get-only on the row record is the windows convention).</summary>
    public string IdText => Id.ToString();
}

/// <summary>
/// One row inside <c>snapshot-detail-files</c>, projected from the machine's
/// <c>SnapshotFileRow</c>. <see cref="CanDownload"/> is <c>file_type ==
/// "regular"</c> — a directory or symlink row gets no dead download affordance
/// (tui's shape, inherited per the ledger row).
/// </summary>
public sealed record SnapshotFileDisplayRow(string Path, string SizeText, bool CanDownload);

/// <summary>
/// One <c>restore-history-item</c> row (<c>backups.md</c> § Restore history),
/// projected from <c>FfiRestoreHistoryRow</c> + its forensic divergence rows.
/// <see cref="Description"/> is the i18n-formatted "{kinds} from {source} —
/// {when}" line (source = "local snapshot" when <see cref="IsLocalSource"/>,
/// else a short hex of the destination member id — mirrors android/linux glue).
/// <see cref="HasDivergence"/> gates the per-row <c>restore-divergence-banner</c>;
/// <see cref="Divergence"/> feeds the forensic <c>restore-divergence-details-modal</c>.
/// </summary>
public sealed record RestoreHistoryRow(
    long SnapshotId,
    string Description,
    bool IsLocalSource,
    bool HasDivergence,
    string DivergenceBanner,
    IReadOnlyList<RestoreDivergenceDetailRow> Divergence);

/// <summary>
/// One <c>restore-divergence-details-item</c> row inside the forensic modal
/// (<c>backups.md</c> § Restore divergence). <see cref="Description"/> is the
/// i18n-formatted "{collection} · {mua} · client modseq {client} / server modseq
/// {server} · ~{lost} writes lost" line (mua = "(unknown)" when the protocol
/// offered none).
/// </summary>
public sealed record RestoreDivergenceDetailRow(string Description);

/// <summary>
/// One local message-kind snapshot offered by the <c>restore-snapshot-select</c>
/// picker (<c>backups.md</c> § Restore from backup destination — local path).
/// <see cref="Id"/> is the snapshot id the friction bar re-types;
/// <see cref="Label"/> is "{kind} (#{id})" (mirrors linux <c>populate_snapshot_select</c>).
/// </summary>
public sealed record RestoreSnapshotOption(long Id, string Label);
