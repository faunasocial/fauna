/**
 * The Backups page's gated controls, painted from the shared backups answers
 * (`docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*;
 * `docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4, 6, 7).
 * The answers are `fauna_backups_machine::possibility`'s and
 * `fauna_protocol::offline_class::carve_out_affordance`'s, read over the
 * backups wasm chunk as `Possibility` serde JSON: the page derives none of
 * them. Each control paints from its `*Paint` (disabled with the reason as its
 * `title`, web's disabled-with-a-reason idiom) and each gesture asks the
 * matching `*Refusal` before it runs, showing a non-null reason on
 * `error-message` (rule 7: never a silent no-op).
 *
 * The wasm faces are injected, so this module stays testable under plain
 * `deno test` (`backups-possibility.test.ts`) — `#lib/wasm-backups` pulls the
 * SvelteKit `$app/paths` alias, which `deno test` cannot resolve.
 */
import type { LocalizedText } from './i18n/localized.ts';
import { paintPossibility, type Possibility } from './possibility.ts';

export type Paint = ReturnType<typeof paintPossibility>;

/** The destination dialog's ceremony. Web is never a custodian, so it never
 *  runs `add_client_device`; it does edit a client-device row (a rename). */
export type DestinationFormTag = 'add_nest' | 'add_client_device' | 'edit_nest' | 'edit_client_device';

/** The backups chunk's free faces (`libs/fauna-wasm-backups`). */
export interface BackupsFaces {
  destinationFormPossibilityJson(form: DestinationFormTag, url: string, capacity: string): string;
  restoreInputsPossibilityJson(snapshotId: string | null | undefined): string;
  restorePossibilityJson(
    running: boolean,
    snapshotId: string | null | undefined,
    kindTicked: boolean,
    typed: string,
  ): string;
  carveOutPossibilityJson(connectionState: string): string;
}

/** The wasm `BackupsMachine`'s snapshot-half answers, over its current state. */
export interface BackupsMachineFaces {
  setActionPossibilityJson(): string;
  deletePossibilityJson(index: number): string;
  immediateDeletePossibilityJson(index: number): string;
}

/** Shown but dead, with nothing to say yet: the chunk or machine is not up. */
const NOT_READY: Paint = { shown: true, disabled: true, reason: null };

function read(json: string): Possibility {
  return JSON.parse(json) as Possibility;
}

/** A gesture's refusal: the blocked reason, or `null` to run. Absent and
 *  not-ready refuse with no reason (the control is not offered at all). */
function refusal(answer: Possibility | null): LocalizedText | null {
  return answer?.state === 'blocked' ? answer.reason : null;
}

/** A gesture's guard: `true` (stop) unless the answer is `possible`, handing
 *  a blocked answer's reason to `show` (the page's `error-message`) so the
 *  refusal is never silent. Absent or not-ready stops with nothing to say —
 *  the control was not offered. */
export function refuse(answer: Possibility | null, show: (reason: LocalizedText) => void): boolean {
  if (answer?.state === 'possible') return false;
  const reason = refusal(answer);
  if (reason) show(reason);
  return true;
}

// ── The destination dialog ──────────────────────────────────────────

/** The form the dialog is running: an add (web only enrolls nests), or an
 *  edit of a nest or a client-device row — the latter a rename with no URL. */
export function destinationForm(editing: { isClientDevice: boolean } | null): DestinationFormTag {
  if (!editing) return 'add_nest';
  return editing.isClientDevice ? 'edit_client_device' : 'edit_nest';
}

export function destinationFormAnswer(
  faces: BackupsFaces | null,
  form: DestinationFormTag,
  url: string,
  capacity: string,
): Possibility | null {
  return faces ? read(faces.destinationFormPossibilityJson(form, url, capacity)) : null;
}

export function destinationFormPaint(
  faces: BackupsFaces | null,
  form: DestinationFormTag,
  url: string,
  capacity: string,
): Paint {
  const answer = destinationFormAnswer(faces, form, url, capacity);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function destinationFormRefusal(
  faces: BackupsFaces | null,
  form: DestinationFormTag,
  url: string,
  capacity: string,
): LocalizedText | null {
  return refusal(destinationFormAnswer(faces, form, url, capacity));
}

// ── The snapshot half ───────────────────────────────────────────────

function machineAnswer(
  machine: BackupsMachineFaces | null,
  ask: (m: BackupsMachineFaces) => string,
): Possibility | null {
  return machine ? read(ask(machine)) : null;
}

export function setActionAnswer(machine: BackupsMachineFaces | null): Possibility | null {
  return machineAnswer(machine, (m) => m.setActionPossibilityJson());
}

/** `snapshot-create-button`, `snapshot-prune-button`, `snapshot-check-button`. */
export function setActionPaint(machine: BackupsMachineFaces | null): Paint {
  const answer = setActionAnswer(machine);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function setActionRefusal(machine: BackupsMachineFaces | null): LocalizedText | null {
  return refusal(setActionAnswer(machine));
}

export function deleteAnswer(machine: BackupsMachineFaces | null, index: number): Possibility | null {
  return machineAnswer(machine, (m) => m.deletePossibilityJson(index));
}

/** `snapshot-delete` on the row at `index` (the 48-hour soft delete). */
export function deletePaint(machine: BackupsMachineFaces | null, index: number): Paint {
  const answer = deleteAnswer(machine, index);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function deleteRefusal(machine: BackupsMachineFaces | null, index: number): LocalizedText | null {
  return refusal(deleteAnswer(machine, index));
}

export function immediateDeleteAnswer(
  machine: BackupsMachineFaces | null,
  index: number,
): Possibility | null {
  return machineAnswer(machine, (m) => m.immediateDeletePossibilityJson(index));
}

/** `snapshot-immediate-delete-button` on the row at `index`. */
export function immediateDeletePaint(machine: BackupsMachineFaces | null, index: number): Paint {
  const answer = immediateDeleteAnswer(machine, index);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function immediateDeleteRefusal(
  machine: BackupsMachineFaces | null,
  index: number,
): LocalizedText | null {
  return refusal(immediateDeleteAnswer(machine, index));
}

// ── Restore ─────────────────────────────────────────────────────────

/** `restore-kind-checkbox` and `restore-confirm-input`: `snapshotId` is the
 *  picked local snapshot's id, `null` when there is none. */
export function restoreInputsPaint(faces: BackupsFaces | null, snapshotId: string | null): Paint {
  return faces ? paintPossibility(read(faces.restoreInputsPossibilityJson(snapshotId))) : NOT_READY;
}

export function restoreAnswer(
  faces: BackupsFaces | null,
  running: boolean,
  snapshotId: string | null,
  kindTicked: boolean,
  typed: string,
): Possibility | null {
  return faces ? read(faces.restorePossibilityJson(running, snapshotId, kindTicked, typed)) : null;
}

/** `restore-confirm-button`. */
export function restorePaint(
  faces: BackupsFaces | null,
  running: boolean,
  snapshotId: string | null,
  kindTicked: boolean,
  typed: string,
): Paint {
  const answer = restoreAnswer(faces, running, snapshotId, kindTicked, typed);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function restoreRefusal(
  faces: BackupsFaces | null,
  running: boolean,
  snapshotId: string | null,
  kindTicked: boolean,
  typed: string,
): LocalizedText | null {
  return refusal(restoreAnswer(faces, running, snapshotId, kindTicked, typed));
}

// ── The carve-out fetches ───────────────────────────────────────────

export function carveOutAnswer(faces: BackupsFaces | null, connectionState: string): Possibility | null {
  return faces ? read(faces.carveOutPossibilityJson(connectionState)) : null;
}

/** A snapshot file's download (`snapshot-file-download-button`): it needs a
 *  live nest, so it is blocked while the connection word is offline. */
export function carveOutPaint(faces: BackupsFaces | null, connectionState: string): Paint {
  const answer = carveOutAnswer(faces, connectionState);
  return answer ? paintPossibility(answer) : NOT_READY;
}

export function carveOutRefusal(faces: BackupsFaces | null, connectionState: string): LocalizedText | null {
  return refusal(carveOutAnswer(faces, connectionState));
}
