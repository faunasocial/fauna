// sitemap.xml, derived from the page files themselves.
//
// The route list is NOT hand-maintained: `import.meta.glob` resolves at build
// time to every `src/pages/**/*.astro`, so adding a page automatically adds it
// to the sitemap. A hardcoded list would silently go stale the first time
// someone adds a page and forgets this file — the whole class of bug this
// derivation removes. (That is also why no `@astrojs/sitemap` dependency is
// needed: its only real value over this is the same derivation.)
import type { APIRoute } from 'astro';
import { escapeXml } from '../lib/xml.ts';

// Eager: false — only the keys (file paths) are used, never the modules.
const PAGE_FILES = Object.keys(import.meta.glob('../pages/**/*.astro'));

/** `organization/bylaws.astro` → `/organization/bylaws/`; index → `/`. */
function fileToRoute(file: string): string | null {
  // Vite does not promise one spelling for glob keys: it normalizes the
  // same-directory case to './404.astro' and may also hand back
  // '../pages/404.astro' or '/src/pages/404.astro'. Strip all three shapes —
  // getting this wrong once already put a bogus /404/ in the sitemap.
  const rel = file
    .replace(/^.*\/pages\//, '')
    .replace(/^\.\//, '')
    .replace(/\.astro$/, '');
  // 404 is not a crawlable destination, and neither is a talk slide: every
  // page under talks/ is noindex (a screen-sized page behind a talk, not a
  // page to browse), and a sitemap must not advertise what robots are told
  // not to index. The `[` guard is defensive: the site has no dynamic routes
  // today (news, the last one, was retired 2026-08-04), and a future one
  // would need its own expansion here rather than a bogus literal-bracket
  // <loc>.
  if (rel === '404' || rel.startsWith('talks/') || rel.includes('[')) return null;
  const path = rel.replace(/(^|\/)index$/, '');
  // Fail the build rather than emit a wrong <loc>: any leftover '.' means the
  // key shape changed again and the strips above no longer cover it.
  if (path.includes('.')) {
    throw new Error(`sitemap: could not derive a route from page file "${file}"`);
  }
  return path ? `/${path}/` : '/';
}

export const GET: APIRoute = async ({ site }) => {
  const base = site ?? new URL('https://fauna.social');

  const entries: { path: string; lastmod?: string }[] = PAGE_FILES.map(fileToRoute)
    .filter((route): route is string => route !== null)
    .sort()
    .map((path) => ({ path }));

  const urls = entries
    .map(({ path, lastmod }) => {
      const loc = escapeXml(new URL(path, base).href);
      return lastmod
        ? `  <url><loc>${loc}</loc><lastmod>${lastmod}</lastmod></url>`
        : `  <url><loc>${loc}</loc></url>`;
    })
    .join('\n');

  const body = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls}
</urlset>
`;

  return new Response(body, {
    headers: { 'Content-Type': 'application/xml; charset=utf-8' },
  });
};
