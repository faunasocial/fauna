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

// Since apps 6161 the toggle paints from the shared Possibility answer:
// `subdomain_toggle_possibility` (libs/fauna-client-web) answers Pending —
// disabled — for a resolved read with no view, and the pre-wasm fallback is
// disabled too. So the contract is: `disabled` comes from that answer, and
// the answer is fed the `view` state.
Deno.test('the toggle button is disabled while the view is null', () => {
  const m = src.match(/<button[^>]*data-testid=\{IDS\.WEB_SETTINGS_SUBDOMAIN_TOGGLE\}[\s\S]*?>/);
  check(m !== null, 'toggle button not found');
  check(/disabled=\{toggleAnswer\.disabled\}/.test(m![0]), 'toggle must carry disabled={toggleAnswer.disabled}');
  check(
    /webSubdomainTogglePossibility\(readState, view\)/.test(src),
    'the toggle answer must be asked over the view state',
  );
  check(
    /:\s*\{ shown: true, disabled: true, reason: null \}/.test(src),
    'before wasm is ready the toggle must paint disabled',
  );
});

Deno.test('applyView (run by hydrate) is what sets the view, enabling the toggle', () => {
  check(/view = webSubdomainView\(/.test(src), 'applyView must set view');
  check(/applyView\(subEnabled\);/.test(src), 'hydrate must call applyView');
});
