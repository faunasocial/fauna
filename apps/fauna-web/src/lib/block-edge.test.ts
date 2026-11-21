// Behavioural cover for the profile block toggle's edge state (profile.md § User
// actions, the `profile-block-button` row): inert until the open-time read lands,
// and a late read never rewinds the viewer's own press. Mirrors tui's
// `the_toggle_is_inert_until_the_open_time_read_lands` and
// `a_late_open_hydration_does_not_rewind_the_user_toggle`.

import { assert, assertEquals } from "jsr:@std/assert";
import {
  blockEdgeBlocked,
  blockReadWord,
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

Deno.test("a failed open-time read keeps the toggle blocked with load_failed", () => {
  const failed = foldBlockRead(unknownBlockEdge(), null);
  assert(!blockToggleEnabled(failed, false), "a failed read never enables Block");
  assert(!blockEdgeBlocked(failed), "no guessed edge is painted as blocked");
  assertEquals(blockReadWord(failed), "failed");
  assertEquals(blockReadWord(unknownBlockEdge()), "pending");
  assertEquals(blockReadWord(foldBlockRead(unknownBlockEdge(), false)), "resolved");
});

Deno.test("a late failed read does not undo the viewer's press", () => {
  const pressed = foldBlockPress(true);
  const afterLateFailure = foldBlockRead(pressed, null);
  assertEquals(afterLateFailure, pressed);
  assertEquals(blockReadWord(afterLateFailure), "resolved");
  assert(blockToggleEnabled(afterLateFailure, false));
});

Deno.test("a late open-time read does not rewind the viewer's press", () => {
  const pressed = foldBlockPress(true);
  const afterLateRead = foldBlockRead(pressed, false);
  assertEquals(afterLateRead, pressed);
  assert(blockEdgeBlocked(afterLateRead), "the next press must unblock");
  assert(blockToggleEnabled(afterLateRead, false));
});
