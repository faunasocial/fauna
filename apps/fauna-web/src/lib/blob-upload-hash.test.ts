// Deno test for web's half of the client-computed media hash rule
// (`docs/goal/architecture/security.md` § Signed-envelope shape): the nest's
// blob-upload reply names the blob's hash, and every caller then signs a post or
// profile image over it, so `uploadBlobMultipart` must refuse a reply whose hash
// is not the blake3 of the bytes it just sent. Native's twin is
// `fauna_client::media_upload::upload_prepared_blob`.
//
// Like `nests-grant-scope-labels.test.ts`, this does NOT import `$lib/api.ts`
// or `$lib/wasm.ts` (their `$app/paths` import resolves only under the
// SvelteKit/Vite build). The comparison lives in the dependency-free
// `blob-upload-hash.ts`, which `uploadBlobMultipart` calls with the wasm
// `blobContentHashHex` export; a source assertion pins that wiring.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`

import { assertEquals, assertStringIncludes, assertThrows } from 'jsr:@std/assert';
import { assertEchoedBlobHash } from './blob-upload-hash.ts';
import { stripComments } from './source-contract.ts';

const wasmModuleUrl = new URL('../../static/fauna_wasm.js', import.meta.url);
const wasmBinaryUrl = new URL('../../static/fauna_wasm_bg.wasm', import.meta.url);

let mod: { blobContentHashHex: (b: Uint8Array) => string } | undefined;
try {
  mod = await import(wasmModuleUrl.href);
  await (mod as unknown as { default: (o: unknown) => Promise<unknown> }).default({
    module_or_path: await Deno.readFile(wasmBinaryUrl),
  });
  // A bundle built before the export existed is as absent as no bundle.
  if (typeof mod?.blobContentHashHex !== 'function') mod = undefined;
} catch {
  mod = undefined;
}

const bytes = new TextEncoder().encode('sealed blob bytes');

Deno.test('a reply naming a different hash is refused', () => {
  const fakeHash = () => 'a'.repeat(64);
  assertThrows(
    () => assertEchoedBlobHash('b'.repeat(64), bytes, fakeHash),
    Error,
    'blob upload hash mismatch',
  );
});

Deno.test('a reply with no hash is refused', () => {
  const fakeHash = () => 'a'.repeat(64);
  assertThrows(() => assertEchoedBlobHash(undefined, bytes, fakeHash), Error, 'hash mismatch');
});

Deno.test('a matching reply is returned, compared case-insensitively', () => {
  const fakeHash = () => 'abcdef' + '0'.repeat(58);
  assertEquals(assertEchoedBlobHash('ABCDEF' + '0'.repeat(58), bytes, fakeHash), 'abcdef' + '0'.repeat(58));
});

Deno.test({
  name: 'the wasm export returns a deterministic 64-char lowercase hex digest',
  ignore: mod === undefined,
  fn: () => {
    const a = mod!.blobContentHashHex(bytes);
    assertEquals(/^[0-9a-f]{64}$/.test(a), true);
    assertEquals(mod!.blobContentHashHex(bytes.slice()), a);
    assertEquals(mod!.blobContentHashHex(new Uint8Array([...bytes, 0])) === a, false);
  },
});

Deno.test('uploadBlobMultipart checks the reply through the shared hash', async () => {
  const src = stripComments(
    await Deno.readTextFile(new URL('./api.ts', import.meta.url)),
  );
  const start = src.indexOf('export async function uploadBlobMultipart');
  const body = src.slice(start, src.indexOf('\n}\n', start));
  assertStringIncludes(body, 'assertEchoedBlobHash(json.hash, bytes, blobContentHashHex)');
  assertEquals(body.includes('return json.hash'), false);
});
