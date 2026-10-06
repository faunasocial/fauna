// Bundles `harness-entry.ts` (+ the real $lib Notes-editor graph + the real wasm
// glue) into a single ESM file the harness page loads, and stages the wasm binary
// next to it. Run with deno: `deno run -A build_harness.ts` (the `just
// notes-browser-harness` recipe wraps this). Uses the app's already-installed
// @codemirror/* + svelte from apps/fauna-web/node_modules — no new dependency.
//
// Two import rewrites make the SvelteKit-resolved source bundle outside Kit:
//   $app/paths → app-paths-stub.ts (just `base = ''`)
//   $lib/*     → apps/fauna-web/src/lib/*
// The dynamic `import('../../static/fauna_wasm.js')` inside $lib/wasm is bundled
// inline (no code-splitting), so the page loads exactly one JS file; the wasm
// *binary* is fetched at runtime from the served root, so we copy it there.

import * as esbuild from 'npm:esbuild@0.25.0';
import { fromFileUrl, dirname, join } from 'jsr:@std/path@1';

const here = dirname(fromFileUrl(import.meta.url));
const webRoot = join(here, '..', '..', '..', 'apps', 'fauna-web');
const srcLib = join(webRoot, 'src', 'lib');
const staticDir = join(webRoot, 'static');
const outDir = join(here, 'build');

await Deno.mkdir(outDir, { recursive: true });

await esbuild.build({
  entryPoints: [join(here, 'harness-entry.ts')],
  outfile: join(outDir, 'harness.bundle.js'),
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: 'es2022',
  // Resolve the app's node_modules (@codemirror/*, etc.) from the web root.
  nodePaths: [join(webRoot, 'node_modules')],
  alias: {
    '$app/paths': join(here, 'app-paths-stub.ts'),
    $lib: srcLib,
  },
  logLevel: 'info',
});

// Stage the wasm binary at the served root so `ensureWasm()`'s
// `init('/fauna_wasm_bg.wasm')` (base='') resolves. The glue JS itself is bundled
// into harness.bundle.js; only the binary is fetched at runtime.
await Deno.copyFile(
  join(staticDir, 'fauna_wasm_bg.wasm'),
  join(outDir, 'fauna_wasm_bg.wasm'),
);
// Stage the page next to the bundle so the whole served dir is `build/`.
await Deno.copyFile(join(here, 'harness.html'), join(outDir, 'harness.html'));

console.log('[build_harness] wrote', outDir);
await esbuild.stop();
