// The `?thumb=1` rule (media.md § Encryption at rest — the thumbnail is a separate
// blob sealed under the same audience as its parent, and the nest serves it by a
// metadata lookup because it cannot read the bytes to render one).

import { assertEquals } from "jsr:@std/assert";
import { displaySrcFor, isObjectUrl } from "./media-src.ts";

const NEST = "https://nest.example/api/v1/blob/abc123";
const OBJECT = "blob:https://app.example/9d1c-4f2a";

Deno.test("a nest blob URL takes the thumbnail variant when one is wanted", () => {
  assertEquals(displaySrcFor(NEST, true), `${NEST}?thumb=1`);
  assertEquals(displaySrcFor(NEST, false), NEST);
});

Deno.test("a nest blob URL that already has a query gains the thumb param, not a second ?", () => {
  assertEquals(displaySrcFor(`${NEST}?token=x`, true), `${NEST}?token=x&thumb=1`);
});

Deno.test("an object URL is NEVER given a thumb query", () => {
  // This is the load-bearing one. An object URL is bytes the page already holds
  // — an unsealed restricted-post attachment — so there is no server to ask for a
  // smaller variant, and `blob:...?thumb=1` names nothing: the image would simply
  // fail to load, which on this path looks exactly like "the unseal didn't work".
  assertEquals(displaySrcFor(OBJECT, true), OBJECT);
  assertEquals(displaySrcFor(OBJECT, false), OBJECT);
});

Deno.test("isObjectUrl separates the two source kinds", () => {
  assertEquals(isObjectUrl(OBJECT), true);
  assertEquals(isObjectUrl(NEST), false);
  // Not fooled by a nest URL that merely mentions the word.
  assertEquals(isObjectUrl("https://nest.example/api/v1/blob/deadbeef"), false);
});
