// Deno test for web's half of the admin-picker sentinel/handle namespace
// collision: `docs/goal/behavior/admin.md:171` names
// web, alongside tui, as a string-round-tripping picker whose "option texts
// must be injective by construction, which for it *is* the binding
// mechanism." Each of web's three affected pickers (admin-users' guardian
// picker, admin-dns' catch-all and role-address pickers, admin-web's apex
// picker) mixes a localized "clear" sentinel into the same `string` option
// list as real user handles — so a user whose handle happened to collide
// with a lowercased/translated sentinel would silently resolve to "clear."
// tui's twin pin is `apps/fauna-tui/src/admin/mod.rs`'s
// `admin_picker_sentinels_can_never_be_valid_handles`.
//
// This does NOT import `$lib/wasm.ts` — its top-level `$app/paths` import
// only resolves under the SvelteKit/Vite build (see `offline-gate.test.ts`'s
// own note, and `wasm-loader-guard.test.ts`'s). Instead it loads the static
// wasm-pack bundle directly, the same file `wasm.ts` wraps, and calls the
// same `validateHandle` export the change-handle form uses for client-side
// feedback (`libs/fauna-wasm/src/lib.rs`'s `validateHandle`, delegating to
// `fauna_protocol::handle::validate_handle` — one validator for the nest and
// every app).
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`
// (the project's `web-unit-test` recipe already covers this directory).
//
// The static bundle is a gitignored build artifact (`just wasm-core`),
// absent on any machine that has never built the web SPA's wasm — including
// one whose C toolchain carries no `wasm32-unknown-unknown` target at all.
// Since `web-unit-test` is deliberately NOT wasm-gated (this file's header,
// above), a missing bundle degrades to a skip rather than an uncaught
// top-level rejection that fails every other test in the recipe.

import { t } from './i18n/strings.ts';

const wasmModuleUrl = new URL('../../static/fauna_wasm.js', import.meta.url);
const wasmBinaryUrl = new URL('../../static/fauna_wasm_bg.wasm', import.meta.url);

let mod: { validateHandle: (handle: string) => string | undefined } | undefined;
try {
  mod = await import(wasmModuleUrl.href);
  await mod!.default({ module_or_path: await Deno.readFile(wasmBinaryUrl) });
} catch {
  mod = undefined;
}

// The five sentinel strings web's pickers mix into their handle namespace —
// tui's reference pin's own list, mirrored exactly: the guardian picker's
// "None" (`t.admin.users_page.guardian_none`,
// `apps/fauna-web/src/routes/admin/users/+page.svelte:118`), the DNS
// catch-all picker's "None" (`t.admin.dns.catch_all_none`,
// `admin/dns/+page.svelte:501`), the DNS role-address picker's
// "Admin (default)" (`t.admin.dns.role_address_admin_default`,
// `admin/dns/+page.svelte:575`), the apex picker's "None (info page)"
// (`t.admin.web_page.apex_none`, `admin/web/+page.svelte:46`), and the
// `"actor {short}…"` fallback label every picker falls back to for an
// unresolvable actor id (`t.admin.actor_id_fallback_label`).
const sentinels = [
  t.admin.users_page.guardian_none,
  t.admin.dns.catch_all_none,
  t.admin.dns.role_address_admin_default,
  t.admin.web_page.apex_none,
  t.admin.actor_id_fallback_label({ short: '00112233'.repeat(8) }),
];

Deno.test({
  name: 'no admin-picker sentinel is ever a valid handle',
  ignore: mod === undefined,
  fn: () => {
    for (const sentinel of sentinels) {
      const error = mod!.validateHandle(sentinel);
      if (!error) {
        throw new Error(
          `picker sentinel ${JSON.stringify(sentinel)} is a VALID HANDLE — a user ` +
            `holding it would silently resolve to "clear" on that picker`,
        );
      }
    }
  },
});
