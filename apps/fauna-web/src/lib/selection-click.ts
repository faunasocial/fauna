// Selecting is not acting (`docs/goal/ui/README.md` § Text the person reads
// can be copied, rule 3). Web's read-only text is plain DOM text, selectable
// with the browser's own drag; but a row or card whose click opens or selects
// something (a post card, a search result, a notification) receives a `click`
// when a drag that selects its text ends inside it. That click is the end of a
// selection, not a press: the row's handler asks `endsTextSelection` first and
// does nothing for it. A plain click still acts: a press that lands inside a
// selection already standing does not collapse it before the click arrives
// (the browser keeps it until the release, in case the press starts a drag of
// the selected text), so the check remembers the selection each press found and
// a click whose selection is the one its press found made no new selection —
// it is a press. The keyboard activation (Enter / Space) has no press and finds
// no selection change to blame either.

/** The part of a DOM `Selection` the check reads. */
export interface SelectionLike {
  readonly isCollapsed: boolean;
  readonly anchorNode: unknown;
  readonly focusNode: unknown;
  readonly anchorOffset?: number;
  readonly focusOffset?: number;
}

/** What a press found: the selection standing when the pointer went down, or
 *  null when none stood (collapsed or absent). */
export type PressSnapshot = {
  anchorNode: unknown;
  focusNode: unknown;
  anchorOffset?: number;
  focusOffset?: number;
} | null;

/** The selection a press found, as `PressSnapshot`. */
export function snapshotSelection(selection: SelectionLike | null): PressSnapshot {
  if (!selection || selection.isCollapsed) return null;
  return {
    anchorNode: selection.anchorNode,
    focusNode: selection.focusNode,
    anchorOffset: selection.anchorOffset,
    focusOffset: selection.focusOffset,
  };
}

function sameSelection(a: PressSnapshot, b: SelectionLike): boolean {
  return (
    a !== null &&
    a.anchorNode === b.anchorNode &&
    a.focusNode === b.focusNode &&
    a.anchorOffset === b.anchorOffset &&
    a.focusOffset === b.focusOffset
  );
}

/** The part of a DOM `Node` the check reads. */
export interface ContainerLike {
  contains(other: never): boolean;
}

function currentSelection(): SelectionLike | null {
  const g = globalThis as { getSelection?: () => SelectionLike | null };
  return typeof g.getSelection === 'function' ? g.getSelection() : null;
}

// The selection the latest press found, kept from a capture-phase listener so
// every row's handler reads the same answer without wiring its own.
let lastPress: PressSnapshot = null;
{
  const d = (globalThis as { document?: { addEventListener?: (t: string, f: () => void, o: unknown) => void } }).document;
  d?.addEventListener?.('pointerdown', () => { lastPress = snapshotSelection(currentSelection()); }, { capture: true, passive: true });
}

/** Whether a click on `container` ends a text selection made within it — the
 *  selection is not collapsed, one of its ends lies inside the container, and
 *  it is not the selection the click's own press found standing (a press on a
 *  standing selection is a plain click, which acts). */
export function endsTextSelection(
  container: EventTarget | ContainerLike | null,
  selection: SelectionLike | null = currentSelection(),
  atPress: PressSnapshot = lastPress,
): boolean {
  if (!selection || selection.isCollapsed) return false;
  if (sameSelection(atPress, selection)) return false;
  const c = container as ContainerLike | null;
  if (!c || typeof c.contains !== 'function') return false;
  return c.contains(selection.anchorNode as never) || c.contains(selection.focusNode as never);
}
