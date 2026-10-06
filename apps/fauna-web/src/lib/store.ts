import { get, writable } from 'svelte/store';
import type { Identity, DecodedEmail, Knock, Contact } from './types';
import {
  actorIdFromSecret,
  ensureWasm,
  logMessage,
  signOutRecordBegin,
  signOutWipeOwed,
} from './wasm';
import {
  accountsBoot,
  accountsClearAll,
  signOutFinish,
  accountsUpdateCache,
  accountsSessionMaterial,
  accountsActiveSessionMaterial,
  type SessionMaterial,
} from './accounts';
import { tabPin, clearTabPin } from './tabPin';
import { clearTokenCache, silentSignIn } from './api';
import { escalateIfTerminalAuthVerdict, escalateSignInRefused } from './post-auth-escalation';
import { silentSignInRetryDelay } from './silent-sign-in-retry';
import { resetActorScopedState } from './actorScope';

// --- Identity ---
//
// The identity lives in the shared account registry (`$lib/accounts`, over
// `fauna_client_accounts`) and NOWHERE else — this store is its in-memory view.
// Two concerns share the registry's per-actor rows:
//
//   1. Long-term identity store: the account's secret (and its nest URL and
//      device id). Required to authenticate. Written only at the wizard's
//      hand-off moments (confirm, `LoggedIn` terminal); wiped only on explicit
//      logout. Mirrors Apple's KeychainStore, Linux's libsecret items, and
//      Windows's Credential Manager entries.
//
//   2. Server-data cache: handle, domain, tier (`accountsUpdateCache`).
//      Populated from the registration response and the silentSignIn refresh
//      so the settings UI shows the user's identity instantly on relaunch
//      without a network round-trip. Server is the source of truth; the cache
//      can drift if the user's record changes out-of-band.
//
// `registered` is no longer stored — it's derivable from `handle != null`
// and the one consumer (settings page) checks for the handle directly.
// `fauna_registered` was redundant; dropped here. Cross-app convergence
// (giving Apple/Linux/Windows the same cache shape and refresh-on-launch
// semantics) is tracked internally as a long-term-store unification follow-up.

function identityFromMaterial(m: SessionMaterial): Identity {
  return {
    secretHex: m.secret_hex,
    actorId: m.actor_id,
    handle: m.handle ?? undefined,
    domain: m.domain ?? undefined,
    tier: m.tier ?? undefined,
  };
}

/** This tab's identity, read from the registry: the PINNED account's for a
 *  pinned tab, the ACTIVE account's otherwise (`account-scoping.md` § Concurrent
 *  instances → *Session identity resolves through the session's account*).
 *
 *  Fail closed on both branches. `undefined` material means the account has no
 *  resolvable secret (removed out from under this tab, no account at all, or
 *  wasm not initialized yet at this sync call — every app route calls `init()`
 *  before its own `await ensureWasm()`, deliberately, so the actor-scope seam is
 *  live before the module instantiates). A pinned session must never fall back
 *  to another account's material, so it shows no identity rather than the
 *  active one. Recovery is `init()`'s async path, which runs with wasm
 *  guaranteed and can tell "account genuinely gone" from "too early".
 *
 *  A third fail-closed case: a sign-out was confirmed and its credential wipe
 *  has not run (the tab that confirmed it closed first). The registry still
 *  holds the account, and nobody is signed in — `accountsBoot()` finishes the
 *  sign-out before it resolves anything (`account-scoping.md` § Erasure follows
 *  scope, the web paragraph, decision 2). */
function loadIdentity(): Identity | null {
  if (typeof window === 'undefined') return null;
  if (signOutWipeOwed()) return null;
  const pinned = tabPin();
  const m = pinned ? accountsSessionMaterial(pinned) : accountsActiveSessionMaterial();
  return m ? identityFromMaterial(m) : null;
}

function createIdentityStore() {
  const { subscribe, set, update } = writable<Identity | null>(null);
  // Single-flight refresh guard, keyed on the IDENTITY it is refreshing.
  // `init()` is called from every page mount (10+ sites — feed, groups,
  // settings, etc.). Without dedup, a returning user with a cached identity
  // would fire one silentSignIn per mount. The first call captures the Promise;
  // subsequent calls for the SAME secret are no-ops while it's pending, and once
  // it resolves further mounts skip the refresh entirely (the in-memory store
  // already has handle).
  //
  // ⚠ The key is load-bearing, and its absence was a real bug: this module
  // outlives a client-side navigation, and an append-mode "Add account" exits
  // with `goto('/app/feed')` — no reload. An unkeyed guard therefore handed the
  // NEW identity the OUTGOING one's settled promise, so the appended account was
  // never verified against its nest on the load it was activated in (and its
  // `registered` signal never became true). The same staleness let the outgoing
  // identity's `verified` payload land on whatever the store held by the time it
  // resolved — which after a switch is the incoming account, i.e. one account's
  // handle/domain/tier smudged onto another (account-scoping.md class 4).
  let refreshInFlight: Promise<void> | null = null;
  let refreshingSecret: string | null = null;
  // Bumped by every refresh that gets past the guard. A refresh waiting out a
  // retry backoff compares it on waking, so a later refresh — a switch to
  // another identity, a switch back, or the forced re-check below — ends the
  // older one's loop rather than leaving two loops dialing for one tab.
  let refreshGeneration = 0;
  function refreshFromServer(secretHex: string, force = false): Promise<void> {
    // The guard is deliberately *sticky* (see above): once a refresh for this
    // identity has settled, later mounts reuse it rather than re-running the
    // ceremony. `force` is the one door through it — the post-auth re-check
    // trigger (`security.md` § Post-auth surfacing), which must reach the nest
    // again rather than replay a settled promise. Same arrangement as apple's
    // `performPostAuthSilentSignIn()`: one production call at authenticated
    // launch, plus a trigger that re-runs that same production body.
    if (!force && refreshInFlight && refreshingSecret === secretHex) return refreshInFlight;
    refreshingSecret = secretHex;
    const generation = ++refreshGeneration;
    // Whether this refresh may still act for its identity: no later refresh has
    // superseded it (the key the sticky guard above is keyed on), and the store
    // still holds that identity — a sign-out clears the store without starting
    // a refresh of its own, so the generation alone would not notice it.
    const stillCurrent = () =>
      generation === refreshGeneration &&
      refreshingSecret === secretHex &&
      get({ subscribe })?.secretHex === secretHex;
    refreshInFlight = (async () => {
      // One attempt per turn; only a TRANSIENT failure comes round again (see
      // the catch). The whole loop is the single-flight promise, so a mount
      // that lands mid-backoff joins it rather than dialing a second time.
      for (let attempt = 0; ; attempt++) {
        try {
          const verified = await silentSignIn(secretHex);
          if (!verified) {
            // `fauna.auth.not_registered`: the nest this identity was signed in
            // to no longer signs it in. A signed-in reload never runs the launch
            // machine (the layout diverts only on a MISSING identity), so this
            // refresh is the one place web can see it — escalate, as the shared
            // classifier's `NotRegistered` arm does on every other app.
            if (stillCurrent()) escalateSignInRefused();
            return;
          }
          // Persist the freshened cache to the account's registry row so the
          // next cold launch (and the switcher) shows the current
          // handle/domain/tier instantly. Server is the source of truth —
          // overwriting cached values on drift is intentional. Best-effort: an
          // identity not in the index yet (an append-mode identity before its
          // terminal registers it) surfaces as an UnknownActor — swallow it.
          accountsUpdateCache(
            actorIdFromSecret(secretHex),
            verified.handle,
            verified.domain,
            verified.tier,
          ).catch(() => { /* not in the index yet */ });
          // Only apply to the identity this refresh was FOR. A refresh that
          // settles after an identity change (sign-out, or the append-mode switch
          // above) would otherwise write the outgoing account's server payload
          // onto the incoming one.
          update(id => id && id.secretHex === secretHex ? {
            ...id,
            handle: verified.handle,
            domain: verified.domain,
            tier: verified.tier,
            // The live-session signal. `verified` means the silent challenge
            // completed against the nest, so this is the one point in the SPA
            // that knows the identity reached a WORKING session rather than
            // merely being present in localStorage. Before this existed, nothing
            // ever set `registered`, so the e2e state provider's
            // `session.authenticated` (`web-bridge/agent.js`) was `undefined` for
            // every real session and could only be made true by the harness's own
            // `set_state` injection — a field that structurally could not be true
            // wearing the costume of a product bug (`long-term-store.md`
            // § Implementation status today; e2e-conventions.md point 11).
            registered: true,
          } : id);
          return;
        } catch (e) {
          // TWO failure classes escape this catch, both TERMINAL verdicts.
          // `challengeVerify` possession-verifies the nest's `cert_binding`
          // against the TOFU pin, so this background refresh is one of web's
          // post-auth re-check points (`security.md` § Transport trust →
          // § Post-auth surfacing, channel 3) — and a verdict swallowed here
          // reads as a generic warning while the session keeps running against a
          // nest that just failed to prove it is the one we pinned. That is
          // exactly the shape tui carried until 2026-07-30, and the reason the
          // goal doc states the rule as "background refreshes stay silent for
          // every *other* failure class".
          //
          // The second is the SUCCEEDED-identity refusal, added 2026-09-01. On
          // web this catch is the only place it can be seen at all: a signed-in
          // reload keeps the user's route, and the layout diverts to the launch
          // flow only when the stored identity is MISSING — which a succeeded
          // device's is not. So web's launch machine never runs on that relaunch,
          // and until this escalation the refusal landed in the generic warning
          // below while the app sat on the feed telling the user nothing
          // (`identity-succession.md` § Propagation → *Own device fleet*).
          // `e` verbatim: `challengeVerify` has already classified it into the
          // typed error, and re-classifying would downgrade it back to a plain
          // `Error` (see `escalateIfTerminalAuthVerdict`).
          if (escalateIfTerminalAuthVerdict(e)) return;
          // A TRANSIENT failure (the anonymous connect refused or dropped, a
          // timeout) is retried, silently, for as long as the identity is still
          // this tab's. It has to be: `registered` is set by this refresh ALONE,
          // and the succession closing act gates on it (`identity-succession.md`
          // § The RecoveryKey → *At succession*). Native reaches the same session
          // through its launch machine, whose transient state is a visible Retry
          // CTA and whose next launch re-offers the kit; a signed-in web reload
          // never shows a launch surface at all, so a refresh that gave up here
          // left the tab with no working session, no kit and no error until the
          // user happened to reload. Silent, not abandoned (`security.md`
          // § Transport trust → § Post-auth surfacing).
          const delay = silentSignInRetryDelay(e, attempt);
          if (delay !== null) {
            console.warn(
              `[identity] silentSignIn refresh failed (transient; retry ${attempt + 1} in ${delay} ms):`,
              e,
            );
            logMessage(
              'warn',
              'fauna_web::store',
              `silentSignIn refresh failed (transient; retry ${attempt + 1} in ${delay} ms): ${e}`,
            );
            await new Promise((resolve) => setTimeout(resolve, delay));
            if (!stillCurrent()) return;
            continue;
          }
          // Any other failure ends the refresh, as it always did. The opaque
          // bucket is deliberately NOT retried: a verdict that ever lost its
          // typed prefix lands there, and a retry loop is exactly where
          // `security.md` § Post-auth surfacing says a flattened verdict must
          // never end up. Cached values (if any) remain visible — the UI
          // degrades gracefully.
          console.warn('[identity] silentSignIn refresh failed:', e);
          logMessage('warn', 'fauna_web::store', `silentSignIn refresh failed: ${e}`);
          return;
        }
      }
    })();
    return refreshInFlight;
  }
  return {
    subscribe,
    /**
     * Re-run the background silent challenge against the nest — web's post-auth
     * identity re-check point (`security.md` § Transport trust → § Post-auth
     * surfacing, channel 3).
     *
     * The body is the same `refreshFromServer` the launch path runs, catch
     * included, so the identity verdict escalates from here exactly as it does
     * on its own: the trigger is the only thing that differs, never the handler
     * (e2e-conventions.md point 8 — drive the production path, never a shortcut
     * that fakes the verdict).
     */
    refreshFromNest(secretHex: string): Promise<void> {
      return refreshFromServer(secretHex, true);
    },
    /** Direct store set — for test agent use (bypasses WASM). */
    set,
    init() {
      const id = loadIdentity();
      set(id); // instant identity from the registry (null if wasm is not up yet)
      // ALWAYS boot, whatever the sync read found: it fails closed when wasm is
      // not initialized (see `loadIdentity`), and the async path below is the
      // only place wasm is guaranteed — so it is also the only place that can
      // tell "there is no account" / "the pinned account was really removed"
      // from "the read simply ran too early". Gating the boot on anything the
      // sync read produced strands exactly the tab that most needs it — a
      // second tab of a signed-in profile (fresh `sessionStorage`, so no pin),
      // which would then show no identity at all and have no way back on that
      // load.
      const pinnedAtBoot = tabPin();
      // Boot the shared multi-account registry, THEN re-read the identity
      // through it and run the identity-derived side effects against that.
      // No-op on a normal load: the re-read identity equals `id`, so the store
      // is left as set above.
      accountsBoot()
        .then(() => {
          const healed = loadIdentity() ?? id;
          if (!healed && !pinnedAtBoot) {
            // No pin, and nothing resolvable after the boot: this browser has
            // no active account (or it was cleared under us mid-boot). There
            // is no pin to drop and nothing a reload would change, so leave
            // the tab to the layout guard's own routing rather than reloading
            // — the reload below is the PINNED tab's escape and would loop
            // here, arriving at this same state every time.
            return;
          }
          if (!healed) {
            // Pinned, and the account genuinely no longer resolves — wasm is
            // up by now and `accountsBoot()` has already walked the pin
            // through its succession chain, so this is not a too-early read.
            // Drop the pin: leaving it would fail this tab closed on every
            // future load, which is unrecoverable-by-a-client state.
            clearTabPin();
            // Then reload once, and the reload is load-bearing rather than
            // cosmetic. The layout guard runs in `onMount` and deliberately
            // DEFERS (rather than redirecting) when a pin cannot be resolved,
            // because "unreadable" must not read as "unauthenticated" — so by
            // the time we get here it has already declined to route, and
            // nothing else will re-run it for this mount. Without a reload the
            // tab would sit blank: signed out, un-pinned, and never sent
            // anywhere. One reload takes the now-unpinned path and reaches a
            // terminal state — the app as the store-active account, or the
            // launch flow if there is none. It cannot loop: the pin that
            // brought us here is gone before the navigation starts.
            window.location.reload();
            return;
          }
          if (healed.secretHex !== id?.secretHex) set(healed);
          // Background refresh: pull current handle/domain/tier from the nest so
          // out-of-band changes (server-side handle rename, tier upgrade) replace
          // the cache. Non-blocking — the store already holds the registry's
          // cached identity.
          refreshFromServer(healed.secretHex);
        })
        .catch((e) => {
          logMessage('warn', 'fauna_web::store', `account boot failed: ${e}`);
          // Registry boot failed — fall back to the synchronously-loaded
          // identity. A tab that never resolved one has nothing to fall back
          // TO (a pinned tab must never fall back to another account), so it
          // stops here and shows the launch flow.
          if (!id) return;
          refreshFromServer(id.secretHex);
        });
    },
    /** Also called mid-wizard by the append-mode "Add account" flow
     *  (`onboarding/+page.svelte`'s `identityContinue`/`importIdentity`) — a
     *  SECOND soft-nav door onto the same identity-change surface `logout()`
     *  guards (see its comment above). Signed in as A, add account B with NO
     *  sign-out: `login(secretB)` is the only identity-change hook that fires
     *  before the append lands on `/app/feed`, so B must not render or compose
     *  through A's still-live managers.
     *
     *  That drop is no longer this function's job to remember: `set()` below runs
     *  the actor-scope pass (see the subscription under `identity`), which is what
     *  makes this door — and every future one — safe by construction rather than by
     *  each call site hand-listing the state of the day.
     *
     *  In-memory ONLY: nothing is persisted here. Entering a screen never changes
     *  which identity is canonical — the registry is written at the wizard's
     *  hand-off moments (`accountsPersistConfirmedIdentity` at confirm, the
     *  `LoggedIn` terminal), never by this call. */
    login(secretHex: string) {
      const actorId = actorIdFromSecret(secretHex);
      set({ secretHex, actorId });
    },
    /** The registration reply's server data onto the in-memory identity, and
     *  into the account's registry cache row (best-effort: an identity not in
     *  the index yet rejects, and the silent sign-in refresh re-writes it). */
    completeRegistration(handle: string, domain: string, tier: string) {
      const current = get({ subscribe }) ?? loadIdentity();
      if (!current) return;
      set({ ...current, handle, domain, tier });
      accountsUpdateCache(current.actorId, handle, domain, tier).catch(() => {
        /* not in the index yet — the silent sign-in refresh re-writes it */
      });
    },
    /** Sign out: retire this browser's enrollment, erase the whole credential
     *  namespace, erase every reached account's store — in the native order,
     *  behind one durable decision (`account-scoping.md` § The scoping
     *  taxonomy → *Erasure follows scope*, the paragraph "Web's account store
     *  is in the erase too"): **record, stop, wipe, erase.** The other-tab
     *  refusal is the caller's, asked before this is entered.
     *
     *  1. **Record.** The sign-out record names every account this reaches,
     *     written before the first `await`, so a tab closed anywhere below is
     *     finished by the next load (`accountsBoot()`'s reconcile) and a
     *     caller that cannot await (the e2e agent's `reset`, which navigates
     *     on the next line) still lands signed out.
     *  2. **Stop.** The runtime's sign-out stop, awaited under the hosts' one
     *     stop budget: it retires the enrollment over the session this tab
     *     still holds and closes the store. Until it returns the runtime can
     *     still write a credential slot the wipe is about to delete, and holds
     *     the database a delete would wait on.
     *  3. **Wipe.** `clearTokenCache()` drops the in-memory bearers so none
     *     outlives the identity that minted it — sign-out `goto`s rather than
     *     reloading, so module state would otherwise survive into the next
     *     identity — and `set(null)` runs the actor-scope pass for the same
     *     reason: the feed/conversations managers return an already-built
     *     instance unconditionally, so a soft-nav sign-out then
     *     sign-in-as-a-different-identity would otherwise render the previous
     *     actor's posts/threads to the new one. `accountsClearAll()` is the
     *     shared-Rust credential erase: leaving `fauna/index` pointing at this
     *     account and its `fauna/{actor}/secret` on disk would make the next
     *     load read it back.
     *  4. **Erase.** `signOutFinish()` erases each recorded account's store,
     *     bounded, and each leaves the record as its store goes. A store that
     *     would not go stays recorded, and the onboarding page this lands on
     *     tells the user (`$lib/accounts`'s `signOutResidue`). */
    async logout() {
      // The leave-gesture push drop (`common.md` § Registration — "the
      // subscription follows the signed-in identity"): capture the secret
      // before the erase strands it, then fire-and-forget — sign-out must
      // complete offline, so the erase never waits on the network, and the
      // helper itself swallows failures (stranded rows go to the ruling's
      // reapers). Dynamic import because `push.ts` → `rpc.ts` → this module
      // is a static cycle. Sign-out's `goto` is a soft nav, so the in-flight
      // promise survives the navigation.
      const secret = get({ subscribe })?.secretHex ?? accountsActiveSessionMaterial()?.secret_hex;
      if (secret) {
        void import('./push').then((m) => m.dropActorPushRow(secret));
      }
      const signedIn = get({ subscribe })?.actorId;
      // The core chunk is loaded wherever a runtime runs, so the record is
      // written synchronously on every path that has something to stop; a
      // page that signs out before the chunk loaded has no runtime, and
      // records as soon as it has.
      if (!signOutRecordBegin(signedIn)) {
        try {
          await ensureWasm();
          signOutRecordBegin(signedIn);
        } catch (e) {
          logMessage('warn', 'fauna_web::store', `sign-out not recorded: ${e}`);
        }
      }
      // Dynamic import for the reason `push.ts` is one above:
      // `account-runtime.ts` → `rpc.ts` → this module is a static cycle.
      try {
        await (await import('./account-runtime')).stopAccountRuntime('sign-out');
      } catch (e) {
        logMessage('warn', 'fauna_web::store', `sign-out: runtime stop not run: ${e}`);
      }
      clearTokenCache();
      set(null);
      // Always this tab's own wipe, whatever the record says: a sibling tab
      // loading inside the stop above may have run the record's wipe early,
      // and a slot the stopping runtime wrote after it must still go.
      try {
        await accountsClearAll();
      } catch (e) {
        logMessage('warn', 'fauna_web::store', `account registry clear failed: ${e}`);
      }
      try {
        await signOutFinish('gesture');
      } catch (e) {
        logMessage('warn', 'fauna_web::store', `account store erase failed: ${e}`);
      }
    },
  };
}

export const identity = createIdentityStore();

// ── The actor-scope pass ─────────────────────────────────────────────────────
//
// One subscription, owned here because this module owns the identity, driving
// every drop of in-memory actor-scoped state in the SPA (`actorScope.ts`, and
// account-scoping.md § The scoping taxonomy's in-memory corollary).
//
// Keyed on `secretHex` rather than hooked into `login()`/`logout()`, because
// those are not the only doors: the e2e agent's session patch deliberately calls
// `identity.set()` directly (it would otherwise need WASM's `actorIdFromSecret`),
// and `init()` itself can legitimately re-`set` a DIFFERENT identity mid-boot when
// the post-boot registry re-read resolves another account than the sync read. Keying on the
// value catches all three; hooking the functions caught one.
//
// Ordering is the contract: EVERY reset runs before ANY actor-change handler, so a
// route's caches are already clear when its own rebuild runs. That is why the two
// lists are separate — resets drop, handlers rebuild — and why a handler must never
// be registered as a reset.
//
// This subscription is established at module init, before any component can
// subscribe, so its pass always precedes component reactions to the same change.
type ActorChangeHandler = (id: Identity) => void | Promise<void>;
const actorChangeHandlers = new Set<ActorChangeHandler>();
let lastActorSecretHex: string | null = null;

/**
 * Run `fn` whenever the signed-in actor changes — including the first identity of
 * the page load, and a sign-in following a sign-out.
 *
 * This is the seam for state a *route* has to rebuild for the incoming actor (its
 * manager, its post-auth reads). Call it in `onMount` and call the returned
 * unsubscribe in `onDestroy`.
 *
 * Do NOT rely on the component remounting instead: an in-app switch is a same-route
 * `goto`, which does not remount, and that is exactly how the feed page
 * and the conversations page each shipped a wrong-actor render.
 *
 * `fn` runs after every registered reset, so anything it rebuilds is safe from
 * being dropped by this same pass. Its promise is not awaited — guard each step
 * that can throw, or the rejection vanishes (feed page's `loadStep`).
 */
export function onActorChange(fn: ActorChangeHandler): () => void {
  actorChangeHandlers.add(fn);
  // Fire immediately if an actor is already present: a component mounting after
  // the identity settled must still get its build, and this is the common case
  // (`identity.init()` runs in the same `onMount`, often resolving first).
  const current = get(identity);
  if (current) void fn(current);
  return () => {
    actorChangeHandlers.delete(fn);
  };
}

identity.subscribe((id) => {
  const secretHex = id?.secretHex ?? null;
  // The store seeds to `null`, so a signed-out load's first notification lands
  // here as null === null and costs nothing — no drop, and no handler fired
  // against an absent identity.
  if (secretHex === lastActorSecretHex) return;
  lastActorSecretHex = secretHex;
  resetActorScopedState();
  if (!id) return;
  for (const handler of actorChangeHandlers) void handler(id);
});

// --- Inbox ---

export const inbox = writable<DecodedEmail[]>([]);

// --- Sent ---

export const sent = writable<DecodedEmail[]>([]);

// --- Knocks & Contacts ---

export const knocks = writable<Knock[]>([]);
export const contacts = writable<Contact[]>([]);

// --- Feed ---
//
// The Feed page's post-list / selector / cursor / loading state moved onto the
// shared `fauna_feed::FeedManager` snapshot (`$lib/feed.ts` `feedSnapshot`;
// feed.md § State & data shape, ratified 2026-06-14) — no client-side post-list
// stores here anymore (the priority-#1 divergence the lift retired).

// --- Reconnect signal ---
//
// Bumped by the WS-RPC client's `setOnReconnected` callback (wired in `rpc.ts`)
// on every reconnect — a `Connected` after the first connect, the wasm twin of
// native `subscribe_reconnects` (`transport.md` § Push events). The mounted page
// subscribes and re-pulls its surface; the feed is the one with no poll backstop
// (a post that arrived while disconnected stays invisible until a manual refresh
// otherwise), so the feed page re-fetches here. The conversations/MLS + mail rails
// also subscribe (app-wide, in `conversations.ts` `subscribeReconnectSweep`) and
// run their full receive sweep — parity with native's `ConvPushEvent::Reconnected`,
// so a DM/mail delivered across the gap arrives on reconnect rather than up to a
// full backstop tick later. Notifications/contacts reload on navigation.
export const reconnectTick = writable(0);

// --- Connection status ---
//
// Live nest WS-RPC connection state for the global `connection-status`
// indicator (top of the sidebar shell): `'connecting' | 'connected' |
// 'disconnected'`. Seeded + updated by the WS-RPC client's
// `setOnConnectionStateChanged` callback (wired in `rpc.ts`) on every transition
// — the wasm twin of a native app observing `NestClient::connection_state()`.
// Distinct from `reconnectTick` (which fires only on a *reconnect*): this also
// reflects Connecting/Disconnected, so a Watchtower-swap gap shows live without
// ever surfacing as an error banner. `'disconnected'` until a client is built.
export const connectionStatus = writable<string>('disconnected');

// --- PostId deduplication ---

const seenPostIds = new Set<string>();

export function isNewPost(postId: string): boolean {
  if (seenPostIds.has(postId)) return false;
  seenPostIds.add(postId);
  // Cap at 10,000 entries
  if (seenPostIds.size > 10_000) {
    const first = seenPostIds.values().next().value;
    if (first) seenPostIds.delete(first);
  }
  return true;
}
