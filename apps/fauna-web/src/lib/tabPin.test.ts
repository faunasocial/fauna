import { tabPin, setTabPin, clearTabPin, tabNestUrl, setTabNestUrl } from './tabPin.ts';
import {
  actorsServedByAnotherTab,
  engineLockName,
  removeAccountBlock,
  tryHoldEngineRole,
} from './webLocks.ts';

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

Deno.test('engineLockName — one derivation, normalized like the pin', () => {
  eq(
    engineLockName('  AbC123 '),
    'fauna.mls.conversations-engine/abc123',
    'per-account lock name, in the fauna.<owner>.<role>/<key> scheme',
  );
});

// ── The engine role's posture is the OPPOSITE of the registry mutation lock's ─
//
// The registry mutation lock lives in shared Rust
// (`fauna_client_accounts::with_web_mutation_lock`) and degrades OPEN where the
// Web Locks API is absent — a re-run of the pre-lock behaviour, never an app
// that refuses to start; the engine role fails CLOSED (a forked ratchet on
// class-5 user-irrecoverable state) — the same split the native role lock
// states in `libs/fauna-mls/src/storage.rs`. The closed half is pinned here
// against the absent lock manager (Deno exposes `navigator` but no
// `navigator.locks`, so this is the real degrade path, not a mock of it) to
// keep the asymmetry from being "simplified" away later.
Deno.test('tryHoldEngineRole — fails CLOSED where Web Locks is absent', async () => {
  eq((navigator as { locks?: unknown }).locks ?? null, null, 'precondition: no lock manager here');
  const role = await tryHoldEngineRole('abc123');
  eq(role, null, 'a tab that cannot prove solitude does not get the writing role');
});

/** A minimal `navigator.locks` honouring the one option this module uses:
 *  `ifAvailable` answers the callback with `null` instead of queueing. */
function installMockLockManager(): { held: Set<string>; uninstall: () => void } {
  const held = new Set<string>();
  const nav = navigator as unknown as { locks?: unknown };
  const prior = nav.locks;
  nav.locks = {
    // deno-lint-ignore no-explicit-any
    request(name: string, opts: any, cb: any) {
      if (opts?.ifAvailable && held.has(name)) return Promise.resolve(cb(null));
      held.add(name);
      return Promise.resolve(cb({ name })).finally(() => held.delete(name));
    },
  };
  return { held, uninstall: () => { nav.locks = prior; } };
}

Deno.test('tryHoldEngineRole — one holder at a time, and release hands it on', async () => {
  const mock = installMockLockManager();
  try {
    const first = await tryHoldEngineRole('abc123');
    if (!first) throw new Error('the first tab must win the role');

    // A second tab of the same origin asking for the SAME account is refused —
    // not queued. Queueing would hang it until the holder's tab closed.
    eq(await tryHoldEngineRole('abc123'), null, 'the second tab is refused, not queued');

    // A different account is a different lock name, so it is unaffected: the
    // election is per-account, exactly like the native per-`mls_state.db` lock.
    const other = await tryHoldEngineRole('def456');
    if (!other) throw new Error('a different account must not be blocked by this one');
    other.release();

    // Releasing (an actor switch, or the tab closing) hands the role on.
    first.release();
    await Promise.resolve();
    const second = await tryHoldEngineRole('abc123');
    if (!second) throw new Error('the role must be takeable once released');
    second.release();
  } finally {
    mock.uninstall();
  }
});

// ── The sign-out guard's question (`actorsServedByAnotherTab`) ──────────────
//
// `account-scoping.md` § Concurrent instances → *An erase refuses while a
// sibling serves the account*: a sign-out refuses while another tab holds any
// erased account's engine role, and proceeds when none does — INCLUDING in the
// tab that holds the role itself, which a naive probe would refuse forever.

Deno.test('actorsServedByAnotherTab — a role held by another tab refuses; a free account does not', async () => {
  const mock = installMockLockManager();
  try {
    const otherTab = await tryHoldEngineRole('abc123');
    if (!otherTab) throw new Error('precondition: the other tab holds the role');
    const served = await actorsServedByAnotherTab(['abc123', 'def456'], () => false);
    eq(JSON.stringify(served), JSON.stringify(['abc123']), 'only the held account is served elsewhere');
    // The probe of the free account must not linger as a hold.
    eq(mock.held.has(engineLockName('def456')), false, 'a granted probe is dropped at once');
    otherTab.release();
  } finally {
    mock.uninstall();
  }
});

Deno.test('actorsServedByAnotherTab — this tab\'s own role is consulted, never probed (the elected tab can sign out)', async () => {
  const mock = installMockLockManager();
  try {
    const ownRole = await tryHoldEngineRole('abc123');
    if (!ownRole) throw new Error('precondition: this tab holds the role');
    // Case-insensitive like the lock name: the registry and the identity may
    // spell one actor differently.
    const heldHere = (id: string) => engineLockName(id) === engineLockName('ABC123');
    eq((await actorsServedByAnotherTab(['abc123', 'def456'], heldHere)).length, 0, 'lone tab proceeds');
    // The pin: without consulting, the same probe sees its own reflection.
    eq((await actorsServedByAnotherTab(['abc123'], () => false)).length, 1, 'naive probe refuses');
    // Consulting never released the role this tab still serves.
    eq(mock.held.has(engineLockName('abc123')), true, 'own role still held');
    ownRole.release();
  } finally {
    mock.uninstall();
  }
});

// ── Remove-account's question (`removeAccountBlock`) ─────────────────────────
//
// `account-scoping.md` § Concurrent instances → *Remove-account also refuses
// the account THIS instance serves*: the served-here check comes FIRST, because
// the sibling probe skips this tab's own role and would answer "free".

Deno.test('removeAccountBlock — the account this tab serves refuses as this_tab, even while it holds the role', async () => {
  const mock = installMockLockManager();
  try {
    const ownRole = await tryHoldEngineRole('abc123');
    if (!ownRole) throw new Error('precondition: this tab holds the role');
    const heldHere = (id: string) => engineLockName(id) === engineLockName('abc123');
    eq(await removeAccountBlock('ABC123', 'abc123', heldHere), 'this_tab', 'served here');
    ownRole.release();
  } finally {
    mock.uninstall();
  }
});

Deno.test('removeAccountBlock — an account another tab holds refuses as other_tab; an idle one proceeds', async () => {
  const mock = installMockLockManager();
  try {
    const otherTab = await tryHoldEngineRole('def456');
    if (!otherTab) throw new Error('precondition: the other tab holds the role');
    eq(await removeAccountBlock('def456', 'abc123', () => false), 'other_tab', 'held elsewhere');
    eq(await removeAccountBlock('0a0b0c', 'abc123', () => false), null, 'idle account proceeds');
    otherTab.release();
  } finally {
    mock.uninstall();
  }
});

Deno.test('actorsServedByAnotherTab — degrades OPEN where Web Locks is absent', async () => {
  eq((navigator as { locks?: unknown }).locks ?? null, null, 'precondition: no lock manager here');
  eq((await actorsServedByAnotherTab(['abc123'], () => false)).length, 0, 'nobody can hold a role, so nobody serves');
});
