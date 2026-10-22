// The site's one navigation table. (Until 2026-08-03 this was editions.ts,
// which shipped a 13-page "full" edition and a 3-page "short" edition side by
// side; the user chose the short one, and it became the site. On 2026-08-04
// the site was cut to exactly five pages: /, /what, /who, the English bylaws,
// and the Norwegian original behind them. The long edition's copy is archived
// at docs/sites/fauna-social/archived/long/, the retired reference pages —
// news + RSS, contact — at docs/sites/fauna-social/archived/reference/. The
// third of those, /privacy, was republished on 2026-09-05 for the Microsoft
// Store submission, and is deliberately a page without a nav entry — reachable
// by URL and from the sitemap, like the Norwegian bylaws. /official-apps,
// added 2026-09-24, is the same kind of page: its nav entry went in and back
// out on 2026-09-26 — user call: no nav link until the apps can actually be
// downloaded. /code-signing-policy, added 2026-10-10 for the SignPath
// Foundation application, is the same kind of page: reached from the footer
// sentence, from /official-apps and from the sitemap. /terms, added
// 2026-10-19, is the same kind of page: reached from /privacy and from the
// sitemap.)
//
// The footer has no link list of its own (user call, 2026-08-05): its only
// entry was the bylaws, which the sentence beside it already links. The
// bylaws stay published — a governing document § 17 requires it — reached
// from that sentence and from /who. The Norwegian original is reachable only
// through the English bylaws page, by design.

export interface NavLink {
  href: string;
  label: string;
}

// public-text: site-chrome
export const nav: NavLink[] = [
  { href: '/what', label: 'What you get' },
  { href: '/who', label: 'Who we are' },
];
// public-text-end: site-chrome
