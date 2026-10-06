// The SPA build's second entry: the private share link viewer page
// (share-links.md § The private-file extension). It is built OUTSIDE SvelteKit
// on purpose — every SvelteKit page runs under the app shell's root layout
// (identity store, wasm runtime, receive polls), which the viewer must never
// load (rule 3) — and it lands in the same `build/` the nest serves `/app/`
// from, as `share-viewer.html` plus its assets under `_share/`. The nest's
// `GET /share/<token>` navigation arm answers that file. Runs after the
// SvelteKit build (`deno task build`), so it must not empty `build/`.

import { resolve } from 'node:path';
import { defineConfig } from 'vite';
import { t } from './src/lib/i18n/strings.ts';

const here = import.meta.dirname!;

/** The static text an unfurler sees, from the shared strings (en.yaml). */
function escapeHtml(s: string): string {
  return s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
}

export default defineConfig({
  root: resolve(here, 'share-viewer'),
  base: '/app/',
  publicDir: false,
  cacheDir: resolve(here, '.cache/vite-share-viewer'),
  plugins: [
    {
      name: 'share-viewer-text',
      transformIndexHtml(html: string) {
        return html
          .replaceAll('%SHARE_VIEWER_TITLE%', escapeHtml(t.share_viewer.title))
          .replaceAll('%SHARE_VIEWER_GENERIC_BODY%', escapeHtml(t.share_viewer.generic_body));
      },
    },
  ],
  build: {
    outDir: resolve(here, 'build'),
    emptyOutDir: false,
    assetsDir: '_share',
    // The wasm is fetched by URL, never inlined as a data: URI (the CSP allows
    // no data:).
    assetsInlineLimit: 0,
    rollupOptions: {
      input: resolve(here, 'share-viewer/share-viewer.html'),
    },
  },
});
