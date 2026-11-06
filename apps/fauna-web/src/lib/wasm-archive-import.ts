import { wasmCoreModule } from '#lib/wasm';
import type { WasmArchiveImportMachine } from '../../static/fauna_wasm.js';
import type { ComposedText, LocalizedText } from '#lib/i18n/localized';

// The archive-import page's TS face over the core chunk's
// `WasmArchiveImportMachine` (`libs/fauna-wasm/src/archive_import.rs`; built by
// `#lib/rpc`'s `archiveImportMachine`). The page is a dumb renderer: every line
// it paints comes from `pageTextOf` (the shared
// `fauna_archive_import_machine::text::page_text`, resolved with
// `resolveComposed`), every word from the label maps below — no template is
// filled here.

export type ArchiveImportMachine = WasmArchiveImportMachine;

// ── the snapshot and the page's lines (serde shapes of the shared crate) ──────

export type ArchiveImportStep = 'Source' | 'Archive' | 'Scope' | 'Confirm' | 'Progress' | 'Done';
export type ArchiveSourceKind = 'Facebook' | 'Instagram';
export type AudienceMode = 'Original' | 'OnlyMe';
export type RunState = 'Running' | 'Paused' | 'Cancelled' | 'Completed' | 'Errored';

export interface CategoryRow {
  token: string;
  count: number;
  importable: boolean;
  selected: boolean;
  imported: number;
  skipped: number;
}

export interface ArchiveImportSnapshot {
  step: ArchiveImportStep;
  source_kind: ArchiveSourceKind;
  source_export_url: string;
  archive_path: string;
  categories: CategoryRow[];
  audience_mode: AudienceMode;
  date_from: string;
  date_to: string;
  run_state: RunState | null;
  skip_log: string[];
  nest_supports_hidden_tiers: boolean | null;
  status: string;
  error: string | null;
  archive_profile: unknown | null;
  profile_applied: boolean;
}

export interface ArchiveImportProgressRowText {
  token: string;
  label: LocalizedText;
  progress: ComposedText;
}

export interface ArchiveImportPageText {
  source_link: ComposedText;
  archive_summary: ComposedText | null;
  scope_rows: ComposedText[];
  audience_summary: ComposedText;
  confirm_summary: ComposedText;
  progress_summary: ComposedText;
  progress_fraction: number;
  progress_rows: ArchiveImportProgressRowText[];
  done_summary: ComposedText;
  folder_link: ComposedText;
}

export function snapshotOf(machine: ArchiveImportMachine): ArchiveImportSnapshot {
  return machine.snapshot() as ArchiveImportSnapshot;
}

export function pageTextOf(machine: ArchiveImportMachine): ArchiveImportPageText {
  return JSON.parse(machine.pageTextJson()) as ArchiveImportPageText;
}

// ── the shared label maps ─────────────────────────────────────────────────────

export const SOURCE_KINDS: readonly ArchiveSourceKind[] = ['Facebook', 'Instagram'];
export const AUDIENCE_MODES: readonly AudienceMode[] = ['Original', 'OnlyMe'];

export function sourceKindLabel(kind: ArchiveSourceKind): string {
  return wasmCoreModule().archiveImportSourceKindLabel(kind);
}

export function sourceHelp(kind: ArchiveSourceKind): LocalizedText {
  return wasmCoreModule().archiveImportSourceHelp(kind) as LocalizedText;
}

export function audienceModeLabel(mode: AudienceMode): LocalizedText {
  return wasmCoreModule().archiveImportAudienceModeLabel(mode) as LocalizedText;
}
