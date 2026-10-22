// esbuild alias target for SvelteKit's `$app/paths` virtual module, which
// `#lib/wasm` imports for `asset()` (a static file's URL under the deployment
// base path). Outside SvelteKit there is no base prefix, so the harness serves
// the wasm bundle from the root.
// This is the ONLY SvelteKit virtual the Notes-editor graph touches at runtime
// (everything else under `#lib/*` is real source the bundler resolves).
export const asset = (file: string): string => `/${file}`;
export const resolve = (path: string): string => path;
