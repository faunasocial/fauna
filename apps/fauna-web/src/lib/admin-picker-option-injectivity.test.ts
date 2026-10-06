// Deno test for the call-site pin, mirroring
// apple's `actorLabelStaysInjectiveWhenLabelsCollide` (`AdminDnsVMTests.swift`)
// and linux's `guardian_select_options_stay_injective_when_labels_collide`.
//
// `adminPickerOption` (`fauna_client_admin::admin_picker_option` over wasm) is
// the shared option-text rule every admin picker on web builds its options
// from (`admin/users/+page.svelte:122`, `admin/dns/+page.svelte:323`,
// `admin/web/+page.svelte:85` — `docs/goal/behavior/admin.md` § 2 → *What
// identifies a user in an admin picker*): the handle, never the freely
// editable, non-unique `label`. This pins the property itself — two
// non-suspended users sharing a display label but holding distinct handles
// must still produce two DISTINCT option strings — the property that broke on
// windows/apple before the fix.
//
// This does NOT exercise the three `.svelte` build sites directly: they carry
// a top-level `$app/paths` import that only resolves under the
// SvelteKit/Vite build (`admin-picker-sentinels.test.ts`'s own note), so — as
// that file does for the sentinel/handle collision — this loads the static
// wasm-pack bundle directly and pins the shared rule those three sites all
// delegate to, rather than the wiring at each call site.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`
// (the project's `web-unit-test` recipe already covers this directory).
//
// The static bundle is a gitignored build artifact (`just wasm-core`),
// absent on any machine that has never built the web SPA's wasm. Since
// `web-unit-test` is deliberately NOT wasm-gated, a missing bundle degrades
// to a skip rather than an uncaught top-level rejection.

interface AdminUser {
  actor_id: number[];
  tier: string;
  label: string;
  handle?: string;
  suspended: boolean;
  created_at: number;
  inbox_bytes_used: number;
  storage_bytes_used: number;
  mail_serving_enabled: boolean;
}

const wasmModuleUrl = new URL('../../static/fauna_wasm.js', import.meta.url);
const wasmBinaryUrl = new URL('../../static/fauna_wasm_bg.wasm', import.meta.url);

let mod: { adminPickerOption: (user: AdminUser) => string } | undefined;
try {
  mod = await import(wasmModuleUrl.href);
  await mod!.default({ module_or_path: await Deno.readFile(wasmBinaryUrl) });
} catch {
  mod = undefined;
}

function user(actorByte: number, handle: string | undefined): AdminUser {
  return {
    actor_id: new Array(32).fill(actorByte),
    tier: 'free',
    // Both users share this label on purpose — the collision the fix exists
    // to survive.
    label: 'e2e-test',
    handle,
    suspended: false,
    created_at: 0,
    inbox_bytes_used: 0,
    storage_bytes_used: 0,
    mail_serving_enabled: true,
  };
}

Deno.test({
  name: 'adminPickerOption stays injective when two users share a label',
  ignore: mod === undefined,
  fn: () => {
    const alex = mod!.adminPickerOption(user(0x11, 'alex99'));
    const bao = mod!.adminPickerOption(user(0x22, 'bao77'));
    if (alex === bao) {
      throw new Error(
        `two same-labelled users with distinct handles produced the same option ` +
          `(${JSON.stringify(alex)}) — an admin could no longer designate the second`,
      );
    }
    if (alex !== 'alex99' || bao !== 'bao77') {
      throw new Error(`expected the handles verbatim, got ${JSON.stringify({ alex, bao })}`);
    }
  },
});

Deno.test({
  name: 'adminPickerOption falls back to the full actor hex for a handle-less account',
  ignore: mod === undefined,
  fn: () => {
    const handleless = mod!.adminPickerOption(user(0x33, undefined));
    const hex = '33'.repeat(32);
    if (handleless !== hex) {
      throw new Error(`expected the full actor hex ${JSON.stringify(hex)}, got ${JSON.stringify(handleless)}`);
    }
  },
});
