import { tabPin, setTabPin, clearTabPin, tabNestUrl, setTabNestUrl } from './tabPin.ts';

// Deno 2.x ships native `localStorage` / `sessionStorage`, so the production
// code runs unmodified here — which is the point: the pin's whole contract is
// *which* storage it uses, and a shim that blurred the two would test nothing.

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)}`);
  }
}

function fresh() {
  sessionStorage.clear();
  localStorage.clear();
}

Deno.test('tabPin — an unpinned tab reads null (the primary case)', () => {
  fresh();
  eq(tabPin(), null, 'no pin');
});

Deno.test('tabPin — set then read round-trips', () => {
  fresh();
  setTabPin('abc123');
  eq(tabPin(), 'abc123', 'pinned');
});

Deno.test('tabPin — clear returns the tab to primary', () => {
  fresh();
  setTabPin('abc123');
  clearTabPin();
  eq(tabPin(), null, 'cleared');
});

// Normalization mirrors `fauna_client_accounts::parse_bound_account`, which owns
// the rule for the native twin `FAUNA_BOUND_ACCOUNT`. Actor ids are lowercase
// hex everywhere, so a pin written in another case must still match the ids the
// registry hands back — otherwise `sessionMaterial()` fails closed on a pin that
// names a perfectly live account.
Deno.test('tabPin — normalizes case and surrounding whitespace, like parse_bound_account', () => {
  fresh();
  setTabPin('  AbC123  ');
  eq(tabPin(), 'abc123', 'trimmed and lowercased');
});

// The other half of the same rule: "An explicitly EMPTY value reads as 'not
// bound', so a spawner can clear an inherited binding without having to unset
// the variable."
Deno.test('tabPin — an explicitly empty pin reads as unpinned, not as an empty account', () => {
  fresh();
  sessionStorage.setItem('fauna_tab_account', '   ');
  eq(tabPin(), null, 'blank pin is no pin');
});

// The pin MUST be per-tab, which on the web platform means `sessionStorage` and
// nothing else: `localStorage` is the shared-origin store whose sharing is the
// convergence hazard this mechanism exists to remove. This test is the
// structural witness — it fails if the slot ever migrates to `localStorage`,
// which would silently make every tab agree again.
Deno.test('tabPin — lives in sessionStorage, never in the origin-shared localStorage', () => {
  fresh();
  setTabPin('abc123');
  eq(sessionStorage.getItem('fauna_tab_account'), 'abc123', 'written per-tab');
  eq(localStorage.getItem('fauna_tab_account'), null, 'never written origin-wide');
});

// The pinned account's nest travels with the pin, and dies with it: a stale
// nest URL outliving its pin would point `storedNestUrl()` at a nest this tab
// has no account on — worse than the shared-slot fallback it replaced.
Deno.test('tabNestUrl — set, read back, and cleared together with the pin', () => {
  fresh();
  setTabPin('abc123');
  setTabNestUrl('https://nest.example');
  eq(tabNestUrl(), 'https://nest.example', 'nest url stored beside the pin');
  clearTabPin();
  eq(tabNestUrl(), null, 'clearing the pin clears its nest url too');
});

Deno.test('tabNestUrl — an unset or blank slot reads as null, not as an empty URL', () => {
  fresh();
  eq(tabNestUrl(), null, 'unset');
  setTabNestUrl('   ');
  eq(tabNestUrl(), null, 'blank');
  setTabNestUrl('https://nest.example');
  setTabNestUrl(null);
  eq(tabNestUrl(), null, 'explicitly cleared');
});
