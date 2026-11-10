// Every `post-image` on the SPA resolves through `mediaUrl`, never `blobUrl`.
//
// Asserted over the source rather than as one more hand-picked outcome (testing.md
// convention 17), because the failure it guards is invisible at runtime for the
// case anyone would test by hand: a PUBLIC post's image renders identically either
// way. Only a tier-restricted post's attachment tells the two apart, and that needs
// a subscriber, a tier, a sealed upload and an unlock to reach — which is exactly
// why the bug this contract exists for survived shipping.
//
// Web is one of the two apps that CANNOT route every hash through the shared
// `openMediaBytes` unconditionally (apple is the other): its post image is an
// `<img src>`, so the browser does the GET and the decode natively and no JS ever
// holds the bytes. So web asks the shared manager `isSealedMedia(hash)` first and
// takes one of two paths — the plain nest URL, or fetch-and-open into an object URL
// (`docs/goal/ui/media.md` § Encryption at rest owns the two shapes). `mediaUrl` IS
// that decision. An `<img>` wired straight to `blobUrl` bypasses it and paints AEAD
// ciphertext, which the browser renders as a broken image — indistinguishable from
// "this post has no photo", the precise symptom that made the original gap so long-lived.
//
// `blobUrl` itself stays legitimate for everything that is NOT post media: the
// video thumbnail, the link-preview og:image, quoted-post embeds. This contract is
// deliberately scoped to elements carrying the `post-image` id.

import { stripComments } from "./source-contract.ts";

// The list card and the post detail both paint one shared `PostMedia` component, so
// its blob-image tag is the one render site this contract reads; the two surfaces are
// held to it by the "both surfaces paint PostMedia" test below.
const SOURCES = [new URL("./components/PostMedia.svelte", import.meta.url)];
const SURFACES = [
  new URL("../routes/feed/+page.svelte", import.meta.url),
  new URL("./components/PostCard.svelte", import.meta.url),
];

/** Every element tag in `src` that carries the POST_IMAGE test id, comments removed. */
function postImageTags(src: string): string[] {
  const code = stripComments(src);
  // Tags are single-line in both files; match the whole tag around the id.
  return [...code.matchAll(/<[A-Za-z][^\n>]*IDS\.POST_IMAGE[^\n>]*>/g)].map((m) => m[0]);
}

Deno.test("no post-image element resolves its src through blobUrl", () => {
  const offenders: string[] = [];
  for (const url of SOURCES) {
    for (const tag of postImageTags(Deno.readTextFileSync(url))) {
      if (/src=\{[^}]*blobUrl\s*\(/.test(tag)) {
        offenders.push(`${url.pathname.split("/").pop()}: ${tag.slice(0, 160)}`);
      }
    }
  }
  if (offenders.length > 0) {
    throw new Error(
      `these post-image elements take their src from blobUrl, which bypasses the ` +
        `sealed-media decision and paints ciphertext for a tier-restricted post's ` +
        `attachment:\n  ${offenders.join("\n  ")}\n` +
        `Resolve through mediaUrl(hash) instead — it returns the plain blob URL for ` +
        `ordinary media and a local object URL for an item that had to be unsealed.`,
    );
  }
});

// A guard on the guard: if either file is restructured so the matcher stops finding
// the elements, the test above passes vacuously — green while checking nothing,
// which is the failure mode conventions 7 and 17 both warn about.
Deno.test("the post-image contract matcher still sees the render site", () => {
  const counts = SOURCES.map((u) => postImageTags(Deno.readTextFileSync(u)).length);
  if (counts.some((c) => c === 0)) {
    throw new Error(
      `expected a post-image element in PostMedia (the list card's and the post ` +
        `detail's one media painter), found ${JSON.stringify(counts)}. The matcher ` +
        `looks for a single-line tag containing IDS.POST_IMAGE; if the markup moved ` +
        `or was split across lines, fix the matcher — do not let it read zero.`,
    );
  }
});

Deno.test("both surfaces paint post media through PostMedia, never a tag of their own", () => {
  for (const url of SURFACES) {
    const code = stripComments(Deno.readTextFileSync(url));
    const name = url.pathname.split("/").pop();
    if (!/<PostMedia\b/.test(code)) {
      throw new Error(`${name} no longer paints its media through PostMedia`);
    }
    if (postImageTags(code).length > 0) {
      throw new Error(
        `${name} paints a post-image tag of its own beside PostMedia — the list card and ` +
          `the detail must share the one painter (render-model.md § D6b)`,
      );
    }
  }
});

// And the positive half: the resolver both sites depend on must actually exist and
// consult the shared manager. A `mediaUrl` that quietly became an alias for
// `blobUrl` would satisfy the contract above while restoring the bug.
// `mediaUrl` lives in the card host the feed page and the profile's Posts tab
// share (`feed-post-host.svelte.ts`); the feed page's detail takes it from there.
const CARD_HOST = new URL("./feed-post-host.svelte.ts", import.meta.url);

Deno.test("the feed card host's mediaUrl asks the shared manager whether the item is sealed", () => {
  const code = stripComments(Deno.readTextFileSync(CARD_HOST));
  if (!/function\s+mediaUrl\s*\(/.test(code)) {
    throw new Error("the feed card host no longer defines mediaUrl — the two post-image sites depend on it");
  }
  if (!/isSealedMedia\s*\(/.test(code)) {
    throw new Error(
      `mediaUrl must decide via the shared manager's isSealedMedia(hash): the keys ` +
        `live there, and a client-side guess about which posts are sealed is exactly ` +
        `the per-app divergence the shared seam exists to prevent.`,
    );
  }
  if (!/openMediaBytes\s*\(/.test(code)) {
    throw new Error(
      `mediaUrl must open a sealed item's bytes through the shared manager's ` +
        `openMediaBytes before handing them to an <img>.`,
    );
  }
});
