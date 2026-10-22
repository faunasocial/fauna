import { defineConfig } from 'astro/config';
import { fileURLToPath } from 'node:url';

// The pages import their words from outside this directory — the copy in
// docs/sites/fauna-social/ and the governing documents in docs/governance/.
// Vite's dev watcher only reaches inside the project root, so an edit to
// either one lands in the next request but never reaches the open browser
// tab. Watch both and ask the browser to reload. Dev only: the hook does not
// run during `astro build`.
const watched = ['../../docs/sites/fauna-social', '../../docs/governance']
  .map((dir) => fileURLToPath(new URL(dir, import.meta.url)));

function watchCopySources() {
  return {
    name: 'fauna-watch-copy-sources',
    hooks: {
      'astro:server:setup': ({ server }) => {
        for (const dir of watched) server.watcher.add(dir);

        server.watcher.on('change', (file) => {
          if (!watched.some((dir) => file.startsWith(dir))) return;

          // Drop the cached module so the next render re-reads the file...
          const mod = server.moduleGraph.getModuleById(file);
          if (mod) server.moduleGraph.invalidateModule(mod);

          // ...then tell the open tab to go and fetch it.
          const channel = server.hot ?? server.ws;
          channel?.send({ type: 'full-reload', path: '*' });
        });
      },
    },
  };
}

export default defineConfig({
  site: 'https://fauna.social',
  output: 'static',
  integrations: [watchCopySources()],
  // Out of node_modules, which the build sandbox never lets a build write
  // (build-system.md § The Deno build sandbox).
  cacheDir: './.cache/astro',
  vite: {
    cacheDir: './.cache/vite',
    // The browsers the site's CSS is written down to: Vite 6's `modules` default,
    // which the site was built for under astro 5. Vite 8's newer default lets its
    // minifier emit media-query range syntax and drop the `100vh` fallback beside
    // `100dvh`, which silently loses every mobile breakpoint on Safari < 16.4.
    build: { cssTarget: ['chrome87', 'edge88', 'firefox78', 'safari14'] },
  },
});
