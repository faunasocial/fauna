// Fills the org-identity tokens in copy strings — the mechanism that keeps
// docs/sites/fauna-social/*.md free of literal org facts (org.ts is the single
// source; the token list is documented in that directory's README § Tokens).
// Applied to every rendered copy string, frontmatter and body alike, so a
// token works anywhere words do. `{email}` renders as a working mailto link;
// the two nullable registry fields fall back per field (see org.ts on why
// there is no site-wide "registration pending" flag).
import { org } from './org.ts';

export function fill(s: string): string {
  return s
    .replaceAll('{email}', `<a href="mailto:${org.email}">${org.email}</a>`)
    .replaceAll('{legalName}', org.legalName)
    .replaceAll('{orgForm}', org.orgForm)
    .replaceAll('{registry}', org.registry)
    .replaceAll('{orgNumber}', org.orgNumber ?? '<em>Registration in progress.</em>')
    .replaceAll('{founded}', org.founded ?? '<em>Founding meeting being formalized.</em>')
    .replaceAll('{domain}', org.domain)
    .replaceAll('{repo}', `<a href="${org.repo}">${org.repo.replace(/^https:\/\//, '')}</a>`)
    .replaceAll(
      '{maintainer}',
      org.maintainers.map((m) => `<a href="${m.url}">${m.handle}</a>`).join(', '),
    );
}
