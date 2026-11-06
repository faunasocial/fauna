// The web-settings subdomain toggle is painted disabled until the nest-confirmed
// view is in (parity lift of the tui and Apple rows: an early flip would be
// dropped without a word). Run via:
//
//     deno test --allow-read apps/fauna-web/src/lib/web-settings-toggle-disabled-until-hydrated-contract.test.ts
//
// A source-contract test (the component is not mountable under plain Deno):
// it pins that the toggle's `disabled` is derived from the `view` state, which
// is null until `hydrate()` runs `applyView` — disabled pre-hydrate, enabled
// after — and that `view` starts null.

const src = await Deno.readTextFile(
  new URL('./components/WebSettingsSection.svelte', import.meta.url),
);

function check(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

Deno.test('the view starts null, so the toggle starts disabled', () => {
  check(/let view = \$state<SubdomainView \| null>\(null\);/.test(src), 'view must start null');
});

Deno.test('the toggle button is disabled while the view is null', () => {
  const m = src.match(/<button[^>]*data-testid=\{IDS\.WEB_SETTINGS_SUBDOMAIN_TOGGLE\}[\s\S]*?>/);
  check(m !== null, 'toggle button not found');
  check(/disabled=\{view === null\}/.test(m![0]), 'toggle must carry disabled={view === null}');
});

Deno.test('applyView (run by hydrate) is what sets the view, enabling the toggle', () => {
  check(/view = webSubdomainView\(/.test(src), 'applyView must set view');
  check(/applyView\(subEnabled\);/.test(src), 'hydrate must call applyView');
});
