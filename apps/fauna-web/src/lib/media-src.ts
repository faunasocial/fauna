// Which URL an image element actually displays.
//
// The SPA shows post media from two different kinds of source, and they differ
// in exactly one respect that matters here:
//
//   - a nest URL (`/api/v1/blob/<hash>`), which the nest serves and which
//     therefore supports the `?thumb=1` lookup — a SEPARATE, smaller blob the
//     uploader generated and named in the parent's metadata. The nest cannot
//     render a thumbnail itself (it cannot read the bytes), so `?thumb=1` is a
//     pointer lookup, not a transform.
//
//   - a local object URL (`blob:`), which the page minted from bytes it already
//     holds decoded — an unsealed restricted-post attachment (`docs/goal/ui/media.md`
//     § Encryption at rest). A sealed item has no server-openable thumbnail by
//     construction, and a query string appended to an object URL names nothing at
//     all, so the request would simply fail.
//
// Kept here rather than inline in `C2paImage.svelte` so the rule can be tested:
// the component's own `$derived` is not reachable from `deno test`, and this is
// exactly the kind of one-expression rule that silently rots.

/**
 * The URL to put in `src` for display.
 *
 * @param src the image's canonical source — the full nest blob URL, or a local
 *   `blob:` object URL.
 * @param thumb whether the caller wants the smaller variant (a list card does; a
 *   detail view and a lightbox do not).
 */
export function displaySrcFor(src: string, thumb: boolean): string {
  if (!thumb || isObjectUrl(src)) return src;
  return `${src}${src.includes("?") ? "&" : "?"}thumb=1`;
}

/** Whether this is a locally-minted object URL rather than a nest-served one. */
export function isObjectUrl(src: string): boolean {
  return src.startsWith("blob:");
}
