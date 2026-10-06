// The SPA's build identity — one derivation, consumed by both configs
// (`svelte.config.js` for `kit.version.name`, `vite.config.ts` for the
// `import.meta.env.VITE_GIT_SHA` the settings page shows).
//
// SvelteKit's `kit.version.name` defaults to `Date.now()`, which lands in
// `_app/version.json` AND inside a content-hashed chunk, so the hashed
// filenames of the whole chunk graph cascade from it and two builds of one
// commit never match (measured 2026-09-24: 122 of 170 output files differed
// between two consecutive builds of an unchanged tree). A reproducible SPA
// build is the first of release-integrity.md § Release signing → Web-app
// verifiability's three pieces, so the identity is the commit instead:
// `VITE_GIT_SHA` when the build passes it (the release path — the nest-image
// build receives it as a build argument), else the checkout's own HEAD.
//
// A build that knows no commit at all (no env, no git — a source tarball)
// returns null, and `svelte.config.js` then keeps SvelteKit's timestamp
// default rather than a constant: `version.name` is also what lets a running
// client notice a redeploy and reload instead of failing on a chunk that no
// longer exists, and a constant would switch that off. A dirty checkout still
// names HEAD; the dev loop's redeploy detection is `just web-dev`'s HMR, not
// this.
import { execFileSync } from 'node:child_process';

/** @returns {string | null} the full commit sha this build is of, if known */
export function gitSha() {
  const fromEnv = process.env.VITE_GIT_SHA;
  if (fromEnv && fromEnv !== 'dev') return fromEnv;
  try {
    const out = execFileSync('git', ['rev-parse', 'HEAD'], {
      stdio: ['ignore', 'pipe', 'ignore'],
    })
      .toString()
      .trim();
    return out || null;
  } catch {
    return null;
  }
}
