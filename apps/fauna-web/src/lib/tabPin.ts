// The per-tab account pin — web's leg of concurrent instances
// (`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances →
// *Web*).
//
// **What this replaces.** The browser is already a concurrent-instance host:
// tabs share one origin. Until this module existed, every tab resolved its
// identity from the *global* `active` pointer on every page load — so a switch
// in one tab silently re-pointed every other tab of the same browser profile at
// the newly-activated account. That is the convergence hazard the goal doc names ("tabs converge
// onto whichever account was activated last and cannot hold distinct
// identities"), and a pin is what removes it: a tab binds its session to the
// account it resolved at boot and keeps serving that account until it is
// explicitly re-pinned, exactly as a native bound instance keeps serving the
// account its launch named.
//
// **The native twin.** A native secondary instance receives its binding as
// `FAUNA_BOUND_ACCOUNT=<actor-id-hex>` in its environment
// (`fauna_client_accounts::requested_bound_account`). A tab has no environment,
// so the pin lives in `sessionStorage` — per-tab by construction, which is the
// whole requirement, and the same reasoning `generation-e2e.ts` already records
// for the same reason ("`sessionStorage` and not `localStorage` on purpose:
// per-tab, so two seats ... cannot see each other's"). `localStorage` is the
// shared-origin store whose sharing is the problem being solved, so it can never
// be the pin's home.
//
// **Normalization mirrors `parse_bound_account`** (that function is the rule's
// owner): trim, lowercase, and an explicitly empty value reads as *unpinned* so
// a caller can clear a pin by writing `''` as well as by removing the key.
// Validation deliberately does NOT happen here — `sessionMaterial()` is the
// single gate, and it fails closed (`undefined`) for an unknown or removed
// account, exactly as `bind_account` refuses a malformed binding as
// `UnknownActor` rather than quietly dropping it.

/** The `sessionStorage` slot. Namespaced like the SPA's other stored keys. */
const STORAGE_KEY = 'fauna_tab_account';

/**
 * This tab's pinned account id (lowercase hex), or `null` when the tab is
 * unpinned — a *primary* tab, which resolves its session from the store-active
 * account exactly as every tab did before pinning existed.
 *
 * Reads through to storage on every call rather than caching in a module
 * variable: the SPA's own switch path is a `window.location.assign` relaunch,
 * which destroys the JS heap, so a module-level cache would be reset by the very
 * navigation the pin has to survive.
 */
export function tabPin(): string | null {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw === null) return null;
    const value = raw.trim().toLowerCase();
    return value === '' ? null : value;
  } catch {
    // A tab that cannot read `sessionStorage` (private-mode quota, storage
    // blocked) is simply unpinned — the primary behaviour, which is the safe
    // degrade: it serves the store-active account rather than refusing to
    // launch. Mirrors the instance lock's "degrade open on I/O failure".
    return null;
  }
}

/**
 * Pin this tab to `actorId`. Called at boot (an unpinned tab pins itself to the
 * account it resolved, which is what makes it immune to a sibling tab's later
 * switch) and on an explicit in-tab switch (which re-points this tab and, per
 * the goal doc, still moves the global `active` pointer).
 */
export function setTabPin(actorId: string): void {
  try {
    sessionStorage.setItem(STORAGE_KEY, actorId.trim().toLowerCase());
  } catch {
    // Unpinnable tab — see `tabPin()`. It keeps working as a primary.
  }
}

/** Drop this tab's pin, returning it to primary (store-active) resolution. */
export function clearTabPin(): void {
  try {
    sessionStorage.removeItem(STORAGE_KEY);
    sessionStorage.removeItem(NEST_URL_KEY);
  } catch {
    /* nothing to undo */
  }
}

/** The pinned account's nest URL slot. See {@link tabNestUrl}. */
const NEST_URL_KEY = 'fauna_tab_nest_url';

/**
 * This tab's pinned account's nest URL, or `null` when unpinned.
 *
 * The pin covers the *identity*; this covers the **nest that identity lives
 * on**. Every re-pin (`accounts.ts`'s `pinTab`: boot, switch, the `LoggedIn`
 * terminal) writes it beside the pin, so `storedNestUrl()` dials THIS tab's
 * account's nest without a registry read per call — and a pinned tab never
 * falls back to the active account's nest, which would open its socket to the
 * account the *primary* is serving.
 *
 * Kept beside the pin, in `sessionStorage`, and read through a module that
 * imports nothing, so the per-call read stays a plain storage read.
 */
export function tabNestUrl(): string | null {
  try {
    const raw = sessionStorage.getItem(NEST_URL_KEY);
    if (raw === null) return null;
    const value = raw.trim();
    return value === '' ? null : value;
  } catch {
    return null;
  }
}

/** Record (or clear) the pinned account's nest URL for this tab. */
export function setTabNestUrl(url: string | null): void {
  try {
    if (url) sessionStorage.setItem(NEST_URL_KEY, url);
    else sessionStorage.removeItem(NEST_URL_KEY);
  } catch {
    /* an unpinnable tab keeps working as a primary — see `tabPin()` */
  }
}
