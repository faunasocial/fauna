/**
 * The Media page's write affordances, painted from the shared media machine's
 * answers (`docs/goal/ui/media.md` § Errors & edge cases → *What this person
 * may write*; `docs/goal/ui/README.md` § Offer only what is possible, rule 6).
 * `file-upload` / `upload-button` paint from `MediaMachine::upload_possibility`,
 * `media-delete-button` / `file-version-restore-button` from
 * `MediaMachine::write_possibility(<item filter_value>)` — the page derives
 * none of it, and the machine's gestures refuse on the same readings.
 */
import { paintPossibility, type Possibility } from './possibility.ts';

/** The two answers the wasm `MediaMachine` gives, as its serde JSON. */
export interface MediaPossibilitySource {
  uploadPossibilityJson(): string;
  writePossibilityJson(folder: string): string;
}

export type Paint = ReturnType<typeof paintPossibility>;

/** The upload controls' paint. Before the machine exists they are shown but
 *  dead, with nothing to say yet. */
export function uploadPaint(machine: MediaPossibilitySource | null): Paint {
  if (!machine) return { shown: true, disabled: true, reason: null };
  return paintPossibility(JSON.parse(machine.uploadPossibilityJson()) as Possibility);
}

/** The delete and restore controls' paint on an item of the set
 *  `filterValue` names. A followed item is head-only: absent. */
export function writePaint(
  machine: MediaPossibilitySource | null,
  filterValue: string,
  followed: boolean,
): Paint {
  if (!machine || followed) return paintPossibility({ state: 'absent' });
  return paintPossibility(JSON.parse(machine.writePossibilityJson(filterValue)) as Possibility);
}
