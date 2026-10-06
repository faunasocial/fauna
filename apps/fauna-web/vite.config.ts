import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import { gitSha } from './build-id.js';

export default defineConfig({
  plugins: [sveltekit()],
  // Out of node_modules, which the build sandbox never lets a build write
  // (build-system.md § The Deno build sandbox).
  cacheDir: '.cache/vite',
  define: {
    // The same derivation `svelte.config.js` uses for `kit.version.name`
    // (build-id.js): the env when the build passes it, else the checkout's
    // HEAD, else the 'dev' sentinel the settings page hides the row on.
    'import.meta.env.VITE_GIT_SHA': JSON.stringify(gitSha() ?? 'dev'),
    // Compile-time flag for the e2e automation surface (testing.md § Test-agent
    // build exclusion): `just web-test` builds with FAUNA_WEB_E2E_AUTOMATION=1
    // so the `window.__fauna_*` hooks exist in the e2e-served bundle; every
    // other build (production included) folds the flag to false and
    // dead-code-eliminates the whole surface out of the artifact.
    __FAUNA_E2E_AUTOMATION__: JSON.stringify(process.env.FAUNA_WEB_E2E_AUTOMATION === '1'),
    // The web family's `payments` excision switch (dynamic-features.md
    // § Platform-family surface excision). Opposite default to the flag above:
    // this one is ON unless a flavor turns it off, because the app is a flavor
    // root and a plain build must ship the plane — `just web-store-safe` sets
    // FAUNA_WEB_PAYMENTS=0 and the fold removes the renders plus, via the
    // isolated-module pattern, the payments glue chunk itself.
    __FAUNA_PAYMENTS__: JSON.stringify(process.env.FAUNA_WEB_PAYMENTS !== '0'),
  },
});
