// Deno tests for the by-id body binding on web's rich-render decode. Run via:
//
//     deno test --allow-read apps/fauna-web/src/lib/feed-decode-binding.test.ts
//
// Web fetches each fauna/nostr card's body a SECOND time (`posts.get` + decode)
// beside the feed manager's own fetch, and paints that body under the manager's
// badge. `security.md` § App display of unverified content: a body fetched by id
// verifies only if it is validly signed AND its wire id equals the requested id.
// The binding itself is shared Rust (`decode_post_shaped_fetched_as`, pinned by
// its native test in `fauna-client-core`); this file pins web's two halves:
// the paint gate's BEHAVIOUR, and its ADOPTION on the one call site — read as
// source text, like the sibling contract tests, because importing the feed page
// pulls the whole SPA graph into `deno test`.

import { paintableDecodedPost } from "./feed-utils.ts";
import { stripComments } from "./source-contract.ts";

function assert(cond: unknown, msg: string): void {
  if (!cond) throw new Error(msg);
}

Deno.test("a bound, validly signed decode is painted", () => {
  const decoded = { body: "post x", valid: true };
  assert(paintableDecodedPost(decoded) === decoded, "valid decode must paint");
});

Deno.test("a substituted or unsigned decode is never painted", () => {
  // `valid: false` is what the shared decode returns for another post's bytes
  // served under this id — the substitution this gate exists for.
  assert(paintableDecodedPost({ body: "post y", valid: false }) === null, "valid:false painted");
  assert(paintableDecodedPost({ body: "post y" }) === null, "missing valid painted");
  assert(paintableDecodedPost({ body: "post y", valid: "true" }) === null, "non-boolean valid painted");
  assert(paintableDecodedPost(null) === null, "null painted");
  assert(paintableDecodedPost(undefined) === null, "undefined painted");
});

const here = new URL(".", import.meta.url);
const read = (rel: string) => stripComments(Deno.readTextFileSync(new URL(rel, here)));

// The decode moved with the card's augmentation into the card host the feed
// page and the profile's Posts tab share (`feed-post-host.svelte.ts`).
Deno.test("the feed card host decodes by id through the binding and the paint gate", () => {
  const page = read("./feed-post-host.svelte.ts");
  const start = page.indexOf("async function fetchAndDecodePost(postId");
  assert(start >= 0, "fetchAndDecodePost(postId…) not found in the feed card host");
  const body = page.slice(start, page.indexOf("\n  }\n", start));
  assert(
    /paintableDecodedPost\(\s*await decodePostFetchedAs\(\s*postId\s*,/.test(body),
    "fetchAndDecodePost must decode with the requested postId and gate the result through paintableDecodedPost",
  );
});

Deno.test("web exposes no unbound post decode", () => {
  const wasmTs = read("./wasm.ts");
  assert(!/\.decode_post\(/.test(wasmTs), "wasm.ts calls the retired unbound decode_post");
  assert(!/export async function decodePost\(/.test(wasmTs), "wasm.ts exports an unbound decodePost");
});
