// Central place for the organization's public identity. Every page reads from
// here, so filling in the real registry details before deploy is a one-file
// edit — see the deploy checklist in sites/fauna-social/README.md.
//
// Unfilled values are `null`, never a placeholder string, and there is no
// site-wide "registration pending" flag. That is deliberate: a placeholder like
// 'registration in progress' reads as real content the moment any page forgets
// to branch on it, and one shared flag flips to false as soon as the FIRST
// field is filled — publishing the remaining placeholders as if they were
// facts. With `null` per field, every page falls back per field, so filling one
// field publishes exactly that field and nothing else.

// The registered address is deliberately NOT here and NOT published (user
// decision, 2026-07-29). The organization number below is the public
// identifier; anyone who needs the address can look it up in the register or
// write to us. Don't add it back without asking.

interface Org {
  legalName: string;
  orgForm: string;
  registry: string;
  /** Norwegian organisasjonsnummer, grouped for display. */
  orgNumber: string | null;
  /** Founding-meeting date, e.g. '12 August 2026'. */
  founded: string | null;
  email: string;
  domain: string;
  /** The public source repository — the tree every release is built from. */
  repo: string;
  /**
   * The code-signing team (the association's maintainer accounts on the
   * hosting service), rendered by `{maintainer}` on /code-signing-policy.
   * Project accounts, not personal details: the handle is the one on every
   * commit of the public tree.
   */
  maintainers: { handle: string; url: string }[];
}

export const org: Org = {
  legalName: 'Fauna Social',
  orgForm: 'Non-profit association (forening)',
  registry: 'Brønnøysund Register Centre, Norway',

  orgNumber: '938 137 722',
  founded: '30 June 2026',

  email: 'hello@fauna.social',
  domain: 'fauna.social',
  repo: 'https://github.com/faunasocial/fauna',
  maintainers: [{ handle: 'faunagud', url: 'https://github.com/faunagud' }],
};
