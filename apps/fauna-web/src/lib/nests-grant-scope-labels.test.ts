// Deno test for web's half of the Nests trust facet naming a web-serve paywall
// grant's folder (`docs/goal/ui/nests.md` § Trust facet — grants). The folder
// is resolved in shared Rust onto the row (`TrustGrantRow.folder` /
// `TrustHistoryRow.folder`); `NestsSection.svelte`'s `scopeLine` hands the
// row's `scope` and `folder` to the `grantScopeLabels` wasm export
// (`libs/fauna-wasm/src/pairing.rs`, delegating to
// `fauna_client_pair::grant_scope_labels`) and resolves each label through
// `resolveLocalized`. tui's twin is `apps/fauna-tui/src/settings/nests.rs`'s
// `a_paywall_grant_and_its_revoke_name_the_folder`.
//
// Like `admin-picker-sentinels.test.ts`, this does NOT import `$lib/wasm.ts`
// (its `$app/paths` import resolves only under the SvelteKit/Vite build): it
// loads the static wasm-pack bundle directly — the same file `wasm.ts` wraps —
// and degrades to a skip when that gitignored build artifact is absent or
// predates the export.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`
// (the project's `web-unit-test` recipe already covers this directory).

import { assertEquals } from 'jsr:@std/assert';
import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';

const wasmModuleUrl = new URL('../../static/fauna_wasm.js', import.meta.url);
const wasmBinaryUrl = new URL('../../static/fauna_wasm_bg.wasm', import.meta.url);

type Scope = { class: string; kind: string | null; tier: string | null };
let mod:
  | { grantScopeLabels: (scope: Scope[], folder: unknown) => LocalizedText[] }
  | undefined;
try {
  mod = await import(wasmModuleUrl.href);
  await (mod as unknown as { default: (o: unknown) => Promise<unknown> }).default({
    module_or_path: await Deno.readFile(wasmBinaryUrl),
  });
  // A bundle built before the export existed is as absent as no bundle.
  if (typeof mod?.grantScopeLabels !== 'function') mod = undefined;
} catch {
  mod = undefined;
}

const premium = { Named: { name: 'premium' } };
const folderRead: Scope[] = [{ class: 'content.read', kind: 'folder', tier: null }];

// The scope line exactly as `NestsSection.svelte`'s `scopeLine` builds it.
function scopeLine(scope: Scope[], folder: unknown): string {
  return mod!.grantScopeLabels(scope, folder ?? null).map(resolveLocalized).join(', ');
}

Deno.test({
  name: "a paywall grant's scope line names its folder",
  ignore: mod === undefined,
  fn: () => {
    assertEquals(
      `${t.nests.trusted_to_read} ${scopeLine(folderRead, premium)}`,
      'Trusted to read: Your folder "premium"',
    );
  },
});

Deno.test({
  name: "a paywall grant's scopeless Revoke is named by its folder",
  ignore: mod === undefined,
  fn: () => {
    assertEquals(scopeLine([], premium), 'Your folder "premium"');
    assertEquals(scopeLine([], null), '');
  },
});

Deno.test({
  name: 'a folder read with no resolved folder keeps the bare label',
  ignore: mod === undefined,
  fn: () => {
    const bare = scopeLine(folderRead, undefined);
    if (bare.includes('premium') || bare === '') {
      throw new Error(`expected the bare folder-read label, got ${JSON.stringify(bare)}`);
    }
  },
});
