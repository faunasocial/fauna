/**
 * Re-type byte arrays as `ArrayBuffer`-backed for the DOM transport APIs.
 *
 * TypeScript 5.7+ types `Uint8Array` as `Uint8Array<ArrayBufferLike>`, and
 * `ArrayBufferLike` admits `SharedArrayBuffer`. The DOM `BodyInit` (fetch body),
 * `BlobPart` (`new Blob([...])`) and `BufferSource` types only accept
 * `ArrayBuffer`-backed views, so bytes coming out of WASM linear memory or
 * `Response.arrayBuffer()` no longer type-check at those boundaries.
 *
 * The cast is sound here: the web app sets no COOP/COEP cross-origin-isolation
 * headers, so none of our `Uint8Array`s are ever backed by a `SharedArrayBuffer`.
 * Zero-copy — prefer this over `.slice()` for large blobs (uploads/attachments).
 */
export function toArrayBufferView(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes as Uint8Array<ArrayBuffer>;
}
