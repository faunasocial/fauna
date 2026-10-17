// The client-computed media hash rule's web half (`docs/goal/architecture/security.md`
// § Signed-envelope shape): a blob-upload reply names the hash every caller then
// signs over, so the SPA compares it with the hash it computed itself from the
// bytes it sent. Kept free of `$lib/wasm` so a deno test can drive it; the hasher
// is the wasm `blobContentHashHex` export (shared Rust `blake3`), injected.

/** Return the lowercase blake3 hex of `bytes` when the nest's `echoed` hash
 *  names it (case-insensitive); otherwise throw — the upload is refused, as
 *  native's `upload_prepared_blob` does. */
export function assertEchoedBlobHash(
  echoed: unknown,
  bytes: Uint8Array,
  hashHex: (bytes: Uint8Array) => string,
): string {
  const local = hashHex(bytes).toLowerCase();
  if (typeof echoed !== 'string' || echoed.toLowerCase() !== local) {
    throw new Error('blob upload hash mismatch: the nest named a different blob than was sent');
  }
  return local;
}
