// Pins the conversation list's empty-state gate on the shared `loaded` bit.
import { assertEquals } from "jsr:@std/assert";
import { showEmptyState } from "./conversations-empty-state.ts";

Deno.test("unloaded with zero threads paints no empty line", () => {
  assertEquals(showEmptyState(false, 0), false);
  assertEquals(showEmptyState(undefined, 0), false);
});

Deno.test("loaded with zero threads paints the empty line", () => {
  assertEquals(showEmptyState(true, 0), true);
});

Deno.test("loaded with threads paints no empty line", () => {
  assertEquals(showEmptyState(true, 2), false);
});
