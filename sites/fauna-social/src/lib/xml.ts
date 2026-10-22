// Minimal XML text escaping for the hand-built feed and sitemap endpoints.
// Deliberately dependency-free: this site ships zero JavaScript and keeps its
// build dependencies to Astro + the self-hosted webfont, so two endpoints do
// not justify pulling in `@astrojs/rss` / `@astrojs/sitemap`.
export function escapeXml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;');
}
