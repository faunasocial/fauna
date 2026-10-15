// The profile Block ⇄ Unblock toggle's edge state (profile.md § User actions, the
// `profile-block-button` row).
//
// The edge the viewer holds on the viewed actor is not yet known between opening
// the page and the open-time `fauna.contacts.list` read landing. A press in that
// window would run on a not-blocked default: for an actor the viewer already
// blocks it re-sends `knocks_block`, which succeeds, leaves the label on
// "Unblock" and shows no error — the e2e signature `the unblock lands: not
// reached` on a box where the read waits behind a slow `profile.get`. So the
// toggle stays disabled until the read, or the viewer's own press, sets the edge
// (mirrors tui's `is_blocked: Option<bool>`).
//
// `userAuthored` is the late-read fold: once the viewer's own press (or the
// report sheet's also-block) has decided the edge for this open, an open-time
// read issued before it must not rewind it.

export type BlockEdge = {
  /** `null` = not yet known; the open-time read has not landed. */
  blocked: boolean | null;
  userAuthored: boolean;
};

/** The edge as of a fresh open: not yet known, nothing authored. */
export function unknownBlockEdge(): BlockEdge {
  return { blocked: null, userAuthored: false };
}

/** Fold the open-time read in — unless the viewer's own press already authored the edge. */
export function foldBlockRead(edge: BlockEdge, blocked: boolean): BlockEdge {
  if (edge.userAuthored) return edge;
  return { blocked, userAuthored: false };
}

/** The viewer's own press (or the report sheet's also-block) decides the edge. */
export function foldBlockPress(blocked: boolean): BlockEdge {
  return { blocked, userAuthored: true };
}

/** Whether the toggle accepts a press: the edge is known and no call is in flight. */
export function blockToggleEnabled(edge: BlockEdge, busy: boolean): boolean {
  return edge.blocked !== null && !busy;
}

/** The edge the toggle label renders; not-yet-known paints the "Block" default (disabled). */
export function blockEdgeBlocked(edge: BlockEdge): boolean {
  return edge.blocked === true;
}
