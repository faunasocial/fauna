// Behavioural cover for the profile block toggle's edge state (profile.md § User
// actions, the `profile-block-button` row): inert until the open-time read lands,
// and a late read never rewinds the viewer's own press. Mirrors tui's
// `the_toggle_is_inert_until_the_open_time_read_lands` and
// `a_late_open_hydration_does_not_rewind_the_user_toggle`.

import { assert, assertEquals } from "jsr:@std/assert";
import {
  blockEdgeBlocked,
  blockToggleEnabled,
  foldBlockPress,
  foldBlockRead,
  unknownBlockEdge,
} from "./block-edge.ts";

Deno.test("the toggle is disabled until the open-time read lands", () => {
  const edge = unknownBlockEdge();
  assert(!blockToggleEnabled(edge, false), "disabled while the edge is unknown");
  assert(!blockEdgeBlocked(edge), "the unknown edge paints the Block default");

  const read = foldBlockRead(edge, true);
  assert(blockToggleEnabled(read, false), "the read enables the toggle");
  assert(blockEdgeBlocked(read), "an already-blocked actor reads Unblock");
});

Deno.test("a read that says not-blocked also enables the toggle", () => {
  const read = foldBlockRead(unknownBlockEdge(), false);
  assert(blockToggleEnabled(read, false));
  assert(!blockEdgeBlocked(read));
});

Deno.test("an in-flight call keeps a known toggle disabled", () => {
  assert(!blockToggleEnabled(foldBlockRead(unknownBlockEdge(), false), true));
});

Deno.test("a late open-time read does not rewind the viewer's press", () => {
  const pressed = foldBlockPress(true);
  const afterLateRead = foldBlockRead(pressed, false);
  assertEquals(afterLateRead, pressed);
  assert(blockEdgeBlocked(afterLateRead), "the next press must unblock");
  assert(blockToggleEnabled(afterLateRead, false));
});
