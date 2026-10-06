// Audience select resolution — Public / a tier / a room / "Sell this post…"
// (feed.md § Encryption at rest, its *Room-restricted — the app half*;
// monetization.md § Pillars 2+3) — index-derived, never value-derived.
//
// `tiers.create` validates only length (1-64 chars) and the one reserved name
// `room`, so an author can name a real tier
// exactly the localized "Sell this post…" label, or exactly a room option's
// "Room: <label>". Comparing the compose-gate-tier-select's *value* against
// those labels (the prior shape) would let such a tier silently hijack the
// other answer — an author who did this could no longer gate a post to their
// own tier on web. Deriving from the select's *position* instead is
// structurally incapable of colliding, the same shape linux's GTK DropDown uses
// (`apps/fauna-linux/src/views/feed/post_list.rs`'s `GateOptions::answer` —
// never a string compare).

export interface GateSelection {
  gateTier: string;
  /** A room's hex channel id (`snapshot.own_rooms[i].room`), or ''. */
  gateRoom: string;
  sellSelected: boolean;
}

/** `selectedIndex` is the compose-gate-tier-select's DOM index: 0 = Public,
 *  1..=ownTierNames.length map onto `ownTierNames` by position, the next
 *  `ownRoomIds.length` indices map onto `ownRoomIds` by position, and the last
 *  index is "Sell this post…" — regardless of what any option's text/value
 *  happens to display. */
export function resolveGateSelection(
  selectedIndex: number,
  ownTierNames: string[],
  ownRoomIds: string[] = [],
): GateSelection {
  const none: GateSelection = { gateTier: '', gateRoom: '', sellSelected: false };
  const tiers = ownTierNames.length;
  const rooms = ownRoomIds.length;
  if (selectedIndex === tiers + rooms + 1) {
    return { ...none, sellSelected: true };
  }
  if (selectedIndex <= 0 || selectedIndex > tiers + rooms) {
    return none;
  }
  if (selectedIndex <= tiers) {
    return { ...none, gateTier: ownTierNames[selectedIndex - 1] };
  }
  return { ...none, gateRoom: ownRoomIds[selectedIndex - tiers - 1] };
}
