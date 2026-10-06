// Snapshot shapes for the `BackupsMachine` (`libs/fauna-backups-machine`, WASM
// twin `libs/fauna-wasm-backups`), consumed by the Backups page's **snapshot
// half** (`routes/backups/+page.svelte`). The page's *destination* half is
// unaffected and keeps its own seams.
//
// Mirrors `fauna_backups_machine::BackupsSnapshot` serde JSON (snake_case across
// the boundary). The three enums ride as their Rust serde variant names —
// PascalCase, and the two lifecycle ones are *externally tagged* struct variants
// (`{ "DeletionPending": { "execute_after": 123 } }`), which is why
// `SnapshotState` is a union of an object and the bare `"Active"` string rather
// than a flat discriminant. `snapshotState()` below is the single reader; nothing
// else in the SPA should string-match these.
//
// Shape owner: docs/goal/ui/backups.md § Snapshot-list shape.
import type { LocalizedText } from '$lib/i18n/localized';

/** One row of the folder selector (`backup-folder-selector`). */
export interface BackupFolderRow {
  name: string;
  snapshot_count: number;
  last_snapshot_at: number | null;
}

/** `fauna_backups_machine::SnapshotState`, externally tagged. */
export type SnapshotState =
  | 'Active'
  | { DeletionPending: { execute_after: number | null } }
  | { SoftDeleted: { purge_after: number | null } };

/** The flattened reading of a row's lifecycle state — the ONE place the wire's
 *  external tagging is decoded, so no component string-matches the variants. */
export type SnapshotLifecycle =
  | { kind: 'active' }
  | { kind: 'deletion_pending'; deadline: number | null }
  | { kind: 'soft_deleted'; deadline: number | null };

export function snapshotLifecycle(state: SnapshotState): SnapshotLifecycle {
  if (typeof state === 'string') return { kind: 'active' };
  if ('DeletionPending' in state) {
    return { kind: 'deletion_pending', deadline: state.DeletionPending.execute_after };
  }
  return { kind: 'soft_deleted', deadline: state.SoftDeleted.purge_after };
}

/** What this session's integrity check said about one row. Derived by the
 *  machine from the check reply's `structured_errors` — never nest state. */
export type RowIntegrity = 'Unknown' | 'CheckedOk' | 'Implicated';

/** One row of the snapshot list (`snapshot-item[i]`), wire order (newest-first);
 *  the page does not re-sort. */
export interface SnapshotRow {
  id: number;
  created_at: number;
  file_count: number;
  total_bytes: number;
  device_id: string | null;
  tags: string[];
  state: SnapshotState;
  integrity: RowIntegrity;
}

/** The mutating operation in flight. While this is non-null EVERY mutating
 *  control is disabled — the single-flight rule that retires the page's old
 *  ad-hoc `creatingSnapshot` / `immediateDeleting` flags. */
export type BackupOp =
  | 'Create'
  | 'Delete'
  | 'ImmediateDelete'
  | 'Undelete'
  | 'Prune'
  | 'Check'
  | 'Refresh'
  | 'Detail';

export interface SnapshotFileRow {
  path: string;
  size_bytes: number;
  /** `"regular"` | `"dir"` | `"symlink"` — the download affordance is a
   *  regular-file gesture; the rest render as rows only (tui's ruling). */
  file_type: string;
  manifest_hash: string;
}

export interface SnapshotDetail {
  snapshot_id: number;
  files: SnapshotFileRow[];
}

/** The last check's verdict. A completed check with errors is a **result, not
 *  an error** — it renders on its own surface, never in `error-message`. */
export interface CheckOutcome {
  /** The shared `SnapshotCheckReply::is_ok()` predicate, called — never
   *  re-derived from counts. */
  is_ok: boolean;
  snapshots_checked: number;
  files_checked: number;
  manifests_checked: number;
  chunks_checked: number;
  missing_manifests: number;
  missing_chunks: number;
  corrupt_manifests: number;
  implicated: number[];
}

/** What the nest could make of the set's resting `retention_policy` column.
 *  `NotSet` and `Unparseable` both prune nothing and say *why*. */
export type PolicyState = 'Applied' | 'NotSet' | 'Unparseable';

export interface PruneCandidate {
  id: number;
  created_at: number;
  tags: string[];
}

/** A prune dry-run awaiting execute-or-cancel. Execute is offered ONLY from
 *  here (§ Snapshot-list shape, *Prune* ruling). */
export interface PrunePreview {
  would_prune: number;
  remaining: number;
  candidates: PruneCandidate[];
  policy_state: PolicyState;
}

/** The whole renderable snapshot half. Architectural rule 1: the page renders
 *  this and dispatches gestures; it holds no page logic. */
export interface BackupsSnapshot {
  folders: BackupFolderRow[];
  selected_folder: string | null;
  snapshots: SnapshotRow[];
  last_backed_up: number | null;
  in_progress_op: BackupOp | null;
  check_result: CheckOutcome | null;
  prune_preview: PrunePreview | null;
  detail: SnapshotDetail | null;
  error: LocalizedText | null;
}
