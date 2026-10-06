import {
  challengeVerify,
  nestInfo as wasmNestInfo,
  hexFull,
} from './wasm';
import type { FeedDefinition, FilterRule, FeedPost, FeedQueryResult, Knock, Contact } from './types';
// Cyclic with this module by construction (the escalation drops the bearer cache
// that lives here). Inert: neither side touches the other's bindings during
// module evaluation — both are called only from inside functions.
import { escalateIfTerminalAuthVerdict } from './post-auth-escalation';
import { SignInRefusedError } from './auth-errors';
import { toArrayBufferView } from './bytes';
import { isSafeNodeUrl } from './safe-url';
import { tabNestUrl, tabPin } from './tabPin';
import { accountsActiveSessionMaterial } from './accounts';
import * as rpc from './rpc';
import { OwnSessionIds } from './own-session-ids';
import { deadlineOnOwnClock } from './token-deadline';
import { launchClockForTest } from './wasm-launch';

/**
 * The **stored** nest URL — web's `nest_url`, the *truth*, or `null` when none
 * is stored (the caller then has no nest to dial but the SPA origin). Reads this
 * tab's nest URL (kept beside its pin), else — for an UNPINNED tab only — the
 * active account's per-actor `fauna/{actor}/nest_url` through the registry. A
 * pinned tab never falls back to the active account's nest: that would serve
 * another account's nest, the one thing the pin exists to prevent. A value is
 * honoured only when it passes `isSafeNodeUrl` (https, or http to a loopback
 * host for dev/e2e) — a spoofed/malformed value is ignored so the client can't
 * be silently steered to an attacker origin (the web SPA's
 * channel-binding-exemption residual). Also the gate for reads that must only
 * run against a real stored nest (never the origin fallback). */
export function storedNestUrlOrNull(): string | null {
  if (typeof window === 'undefined') return null;
  const tab = tabNestUrl();
  if (tab && isSafeNodeUrl(tab)) return tab;
  if (tabPin()) return null;
  const active = accountsActiveSessionMaterial()?.nest_url;
  return active && isSafeNodeUrl(active) ? active : null;
}

/**
 * {@link storedNestUrlOrNull}, defaulting to the SPA's own origin.
 *
 * Read this — never `nodeUrl()` — wherever the URL is *shown to the user* or
 * handed to a third party: the dial seam below "redirects the socket, never the
 * truth" (`libs/fauna-launch-machine/src/dial.rs`), so a rendering that resolved
 * would show a harness URL under e2e. Current callers: the settings nest-URL
 * field, its copy-to-clipboard action, and the payments webhook URL. */
export function storedNestUrl(): string {
  return storedNestUrlOrNull() ?? window.location.origin;
}

/**
 * The URL to open a socket to for the nest `storedNestUrl()` names — **web's
 * twin of `fauna_launch_machine::dial::resolved_dial_url`**, and the default
 * every connection path already calls.
 *
 * The concept map is 1:1 with the shared seam (priority #3): Rust's stored
 * `nest_url` is `storedNestUrl()`; Rust's `resolved_dial_url(nest_url)` is this.
 * In production the two are the same string — `__FAUNA_E2E_AUTOMATION__` folds
 * to false and this function *is* `storedNestUrl()`, exactly as `dial.rs`'s
 * production twin is the identity function, so a release bundle carries neither
 * the override state nor its setter (e2e-conventions.md point 15).
 *
 * **Why the resolver keeps the well-trodden name.** A connection site that
 * forgets to resolve is the failure `dial.rs` warns about — "a split brain no
 * test could see". Resolving in the function ~25 nest-facing call sites already
 * call makes every present *and future* connection path correct by default, and
 * leaves the rarer, lower-stakes duty (name the literal where truth is shown) to
 * the three sites that render it.
 *
 * **Why web needs its own twin at all.** Every other app reaches the shared Rust
 * seam through `fauna_launch_machine`'s `WsAuthConnector`; web builds its nest
 * clients in TypeScript from this string, so the Rust override — which lives in
 * a `static` inside whichever wasm chunk wrote it, and wasm chunks share no
 * linear memory — can never reach web's socket. This is that consumption
 * (`long-term-store.md` § Implementation status today). */
export function nodeUrl(): string {
  if (__FAUNA_E2E_AUTOMATION__) {
    const dial = nestDialOverride();
    if (dial) return dial;
  }
  return storedNestUrl();
}

// ── The nest dial override (test-only) ───────────────────────────────────────
//
// The whole block folds away in a production bundle: `__FAUNA_E2E_AUTOMATION__`
// is the compile-time gate, never a runtime switch (e2e-conventions.md point 15).
//
// Carried in `sessionStorage`, which is web's analogue of the Rust seam's
// process-global `static`: the launch paths that consume it — a reload, an "Add
// account" switch — have no object to hang it off, and the SPA's own append exit
// (`goto('/app/feed')`) drops the query param the harness seeded it with. Per-tab
// and per-run, so it cannot outlive the browser context the fixture owns.
const NEST_DIAL_OVERRIDE_KEY = 'fauna_e2e_nest_dial_override';

/** The installed override, if any. */
function nestDialOverride(): string | undefined {
  if (!__FAUNA_E2E_AUTOMATION__ || typeof window === 'undefined') return undefined;
  try {
    return sessionStorage.getItem(NEST_DIAL_OVERRIDE_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

/** Install (a URL) or clear (`undefined`) the nest dial override — the twin of
 *  `fauna_launch_machine::set_nest_dial_override`.
 *
 *  Clearing is as load-bearing as installing: the override outlives the wizard
 *  that installed it, and a stale one would point a later test's launch at a
 *  torn-down nest. */
export function setNestDialOverride(url: string | undefined): void {
  if (!__FAUNA_E2E_AUTOMATION__ || typeof window === 'undefined') return;
  try {
    if (url === undefined) {
      sessionStorage.removeItem(NEST_DIAL_OVERRIDE_KEY);
    } else if (isSafeNodeUrl(url)) {
      sessionStorage.setItem(NEST_DIAL_OVERRIDE_KEY, url);
    } else {
      // Name the rejection rather than falling back silently: a dial that
      // quietly ignored its override presents as a launch that cannot connect
      // (testing.md point 6 — failures diagnose themselves).
      console.warn('[dial] refusing an unsafe nest dial override:', url);
    }
  } catch (e) {
    console.warn('[dial] nest dial override write failed:', e);
  }
}

if (typeof window !== 'undefined' && __FAUNA_E2E_AUTOMATION__) {
  // Seed from the one gesture the harness already makes:
  // `driver.set_provider_base_urls({"nest": ...})`, which on web is a
  // `?fauna_e2e_provider_base_urls` query param + reload (drivers/web.py). This
  // mirrors the `"nest"` entry into the dial exactly where the Rust seam mirrors
  // it — `OnboardingMachine::set_provider_base_urls` (machine.rs) — so no driver,
  // agent command or test has to learn a second gesture.
  //
  // Read at MODULE LOAD of `$lib/api`, which every nest-facing page imports, so a
  // launch that never mounts the onboarding page (a plain relaunch straight into
  // `/app/feed`) installs it too.
  const raw = new URLSearchParams(location.search).get('fauna_e2e_provider_base_urls');
  if (raw) {
    try {
      const nest = (JSON.parse(raw) as Record<string, string>).nest;
      if (nest) setNestDialOverride(nest);
    } catch (e) {
      console.warn('[dial] invalid fauna_e2e_provider_base_urls param:', e);
    }
  }
}

/** Per-(node, identity) token cache: maps `tokenKey(node, secret)` →
 *  { token, expiresAt }.
 *
 *  The identity is part of the key because two actors on the same nest must
 *  never share a bearer. Keyed on the node alone, a sign-out followed by
 *  signing in as a *different* identity within one page load — `signOut()`
 *  `goto`s to onboarding and onboarding `goto`s to the feed, both client-side,
 *  so module state survives — handed the new identity the signed-out actor's
 *  cached bearer, and the SPA then talked to the nest as that account. (The
 *  account switcher escaped this only because it re-inits via a full reload.) */
const tokenCache = new Map<string, { token: string; expiresAt: number; tokenId: string }>();

/** **The session ids this browser minted**, per cache key — the current one
 *  plus every earlier own id not yet expired
 *  (`docs/goal/behavior/devices.md` § The client's own session). Web's leg of
 *  the set every bearer holder in the fleet keeps; the rule itself lives in
 *  `./own-session-ids` so it is reachable by `deno test`, which cannot import
 *  this module (it loads wasm).
 *
 *  Separate from `tokenCache` deliberately, and for the same reason
 *  `TokenCache`'s set is a separate lock from its slot: it outlives the token.
 *  A `forceRefresh` (the 4401 path) replaces the cached token while the id it
 *  named stays listed nest-side until it expires — so the app's own row must
 *  keep folding into "this app" rather than turning into a stranger. */
const ownSessionIds = new Map<string, OwnSessionIds>();

/** The set for `key`, created on first use. */
function ownIdsFor(key: string): OwnSessionIds {
  let own = ownSessionIds.get(key);
  if (!own) {
    own = new OwnSessionIds();
    ownSessionIds.set(key, own);
  }
  return own;
}

/** **The client clock every bearer deadline in this module is anchored and
 *  compared on** (`login.md` § Token lifetime on the client's clock) — the
 *  mint anchor (`storeMinted`), the spend rule (`getAuthToken`) and the own-id
 *  pruning. It must be the SAME clock the launch machine anchors the bearer it
 *  hands `primeTokenCache` on, or the two disagree by whatever the clocks do.
 *
 *  In production that is simply `Date.now()`. In a build made for testing
 *  (`__FAUNA_E2E_AUTOMATION__`, constant-folded away in a production bundle —
 *  e2e convention 15) it is the launch chunk's clock itself
 *  (`fauna_protocol::client_clock` inside `fauna-wasm-launch`, seeded from the
 *  `FAUNA_E2E_CLOCK_OFFSET_SECS` localStorage key in the `LaunchMachine`
 *  constructor), so the wrong-clock witness's offset reaches this cache too —
 *  one offset, read where it lives, never a second seed. Before the launch
 *  chunk is up the real clock answers; nothing here mints before launch. */
function clientNowSecs(): number {
  if (__FAUNA_E2E_AUTOMATION__) {
    const clock = launchClockForTest();
    if (clock) return clock.now_secs;
  }
  return Math.floor(Date.now() / 1000);
}

/** Record a freshly-minted session id against its cache key, pruning what has
 *  lapsed. Called from every path that puts a token in `tokenCache`. */
function recordOwnSessionId(key: string, tokenId: string, expiresAt: number): void {
  const own = ownIdsFor(key);
  own.record(tokenId, expiresAt);
  own.prune(clientNowSecs());
}

/** **Every session id this browser minted for (node, identity) and still
 *  holds** — what a sessions surface folds into its one "this app" row. Read
 *  at call time; never persisted. */
export function ownTokenIds(secretHex: string, targetNode?: string): string[] {
  const key = tokenKey(targetNode || nodeUrl(), secretHex);
  return ownSessionIds.get(key)?.idsAt(clientNowSecs()) ?? [];
}

/** **The current session id** for (node, identity) — what `revoke_all`'s
 *  `keep_token_id` is filled from, read at call time and never from a
 *  previously painted list: a renewal between paint and press must not name a
 *  dead token (`devices.md` § The client's own session). */
export function currentTokenId(secretHex: string, targetNode?: string): string | null {
  const key = tokenKey(targetNode || nodeUrl(), secretHex);
  return ownSessionIds.get(key)?.currentAt(clientNowSecs()) ?? null;
}

/** The home-nest bearer's schedule for `secretHex` on the client clock —
 *  `{expires_in_secs, own_session_ids}`, the web leg of the cross-app
 *  `launch_token` state key (`fauna_e2e_agent::LAUNCH_TOKEN_KEY` owns the shape;
 *  web's held bearer is this cache). Test builds only: its sole caller is
 *  `$lib/e2e-automation`. A plain read — never mints. */
export function homeBearerScheduleForTest(secretHex: string): {
  expires_in_secs: number | null;
  own_session_ids: string[];
} {
  const key = tokenKey(nodeUrl(), secretHex);
  const now = clientNowSecs();
  const cached = tokenCache.get(key);
  return {
    expires_in_secs: cached ? cached.expiresAt - now : null,
    own_session_ids: ownSessionIds.get(key)?.idsAt(now) ?? [],
  };
}

/** Cache key. NUL joins the parts: it occurs in neither a URL nor a hex secret,
 *  so no (node, secret) pair can collide with another. */
function tokenKey(base: string, secretHex: string): string {
  return `${base}\u0000${secretHex}`;
}

/** Drop every cached bearer, so none outlives the identity that minted it.
 *  `identity.logout()` calls this on sign-out. */
export function clearTokenCache(): void {
  tokenCache.clear();
  // The own-id set goes too: sign-out ends the identity, so none of its
  // sessions are "ours" any more. (A 401/`forceRefresh` is the opposite case
  // and deliberately keeps the set — see `ownSessionIds`.)
  ownSessionIds.clear();
}

/**
 * Seed the bearer cache with a token minted elsewhere — specifically the shared
 * `LaunchMachine`'s silent challenge, which authenticates the launch row exactly
 * as `silentSignIn` used to (and, like it, caches under `tokenKey(base, secretHex)`
 * so a second identity can never overwrite the first's bearer).
 *
 * Without this the machine and the SPA would be two live bearer owners: the
 * machine holds the token it just minted (`currentBearer()`), while the app's
 * first request would find an empty cache and re-mint a *second* bearer through
 * `getAuthToken`. One extra round trip and two tokens; handing the machine's
 * already pin-verified token to the cache keeps the launch bearer the *only*
 * bearer.
 *
 * `expiresAt` must already be on this device's clock — the machine's
 * `TokenStatus::Valid` deadline is (`login.md` § Token lifetime on the client's
 * clock), and the cache's spend rule compares it against `clientNowSecs()`,
 * which is the machine's clock.
 */
export function primeTokenCache(
  base: string,
  secretHex: string,
  token: string,
  expiresAt: number,
  tokenId = '',
): void {
  const key = tokenKey(base, secretHex);
  tokenCache.set(key, { token, expiresAt, tokenId });
  // The launch token is minted by the machine, not by `getAuthToken`, so this
  // is the only place it can be folded into the own-id set. Defaulted for a
  // caller that cannot name it (a machine with no token id to name): `record` ignores an empty
  // id rather than minting a phantom row.
  recordOwnSessionId(key, tokenId, expiresAt);
}

/**
 * Get a bearer token from a nest. When targetNode is omitted, authenticates
 * against the home node. For cross-nest requests, pass the target node URL
 * to get a token that the target nest will accept.
 *
 * Mints over the pre-identity anonymous WS-RPC silent challenge
 * `fauna.auth.{challenge,verify}` (wasm `challengeVerify`) — the ceremony
 * `login.md` § When to use which assigns to every bearer an app holds, the TTL
 * and 4401 re-mints included: it signs the nest's nonce, not a client
 * timestamp, so a device whose clock is hours wrong stays signed in on it. The
 * cached deadline is anchored on this device's clock at receipt
 * (`deadlineOnOwnClock` over `clientNowSecs()`, § Token lifetime on the
 * client's clock), which is the clock the spend rule below reads. The anonymous WS is CORS-exempt, so a
 * cross-origin `targetNode` mint works — the seam the both-ends pairing peer
 * connection depends on.
 */
export async function getAuthToken(secretHex: string, targetNode?: string, forceRefresh = false): Promise<string> {
  const base = targetNode || nodeUrl();
  const key = tokenKey(base, secretHex);
  const cached = tokenCache.get(key);
  // `forceRefresh` is wired to the WS-RPC client's token provider so a 4401
  // close busts the cache and re-mints the bearer (mirrors native
  // `clear_token` + `ensure_auth`).
  if (!forceRefresh && cached && cached.expiresAt > clientNowSecs() + 60) {
    return cached.token;
  }

  let verified;
  try {
    verified = await challengeVerify(secretHex, base);
  } catch (e) {
    // The re-mint is a full anonymous connect, so it runs the possession-verify
    // + pin compare too — making it web's post-auth re-check point #1
    // (`security.md` § Transport trust → § Post-auth surfacing). Route the
    // identity verdict to the blocking launch surface rather than letting it
    // reach the caller as a token failure: the WS-RPC client's token provider
    // calls this, and a reconnect supervisor treats a generic token failure as
    // **retryable** — a transient-retry loop on a MITM signal, which is the
    // precise failure mode the goal doc's plumbing rule exists to prevent.
    //
    // ⚠ ONLY for the home nest. `getAuthToken` also mints against *other*
    // origins (a pairing peer, a backup destination), and those mints run the
    // same possession check — correctly, since a bearer must never be minted
    // against a nest that failed to prove its identity. But the blocking surface
    // says "the nest you signed in to changed", and re-entering the launch flow
    // re-challenges the HOME nest, which is fine and would let the user straight
    // back in. Blanking a healthy session over a *peer's* pin verdict would
    // therefore be both a lie and a loop; the peer's rejection is an ordinary
    // failure for its own caller to surface.
    //
    // Still rethrow either way: the caller's request genuinely cannot proceed,
    // and the escalation is a navigation, not a resolution.
    // `e` verbatim — `challengeVerify` has already classified it into a typed
    // error, and the escalation takes either form (running a typed error back
    // through the classifier would downgrade it; `post-auth-escalation.ts`).
    if (base === nodeUrl()) {
      escalateIfTerminalAuthVerdict(e);
    }
    throw e;
  }
  if (!verified) {
    // `fauna.auth.not_registered` — the silent challenge reports it as `null`
    // (the launch flow's drop-into-onboarding signal). For a bearer request it
    // is simply a refusal: the actor holds no account on `base` (or is
    // suspended there — the code is deliberately opaque, `login.md` § Errors).
    //
    // Typed, because on the HOME nest it is a session-ending verdict: the WS
    // client's re-mint after the nest's revocation teardown (4401) lands here,
    // and a signed-in session the nest stopped accepting must reach the launch
    // surface rather than retry into a generic failure (`onboarding.md`
    // § App-launch routing, the previously-signed-in row). The same home-only
    // rule as the identity verdict above: a peer's refusal is its caller's.
    const refused = new SignInRefusedError(base);
    if (base === nodeUrl()) {
      escalateIfTerminalAuthVerdict(refused);
    }
    throw refused;
  }
  storeMinted(key, verified);
  return verified.token;
}

/** Cache a freshly minted bearer under `key`, its deadline anchored on this
 *  device's clock at receipt — the one write both mint paths (`getAuthToken`,
 *  `silentSignIn`) make, so neither can put the nest's clock in the cache. */
function storeMinted(
  key: string,
  minted: { token: string; token_id: string; expires_at: number; expires_in: number },
): void {
  const expiresAt = deadlineOnOwnClock(clientNowSecs(), minted.expires_in, minted.expires_at);
  tokenCache.set(key, { token: minted.token, expiresAt, tokenId: minted.token_id });
  recordOwnSessionId(key, minted.token_id, expiresAt);
}

export async function authHeaders(secretHex: string, targetNode?: string): Promise<Record<string, string>> {
  const token = await getAuthToken(secretHex, targetNode);
  return { authorization: `Bearer ${token}` };
}

// --- Registration & account management ---

export interface NodeRegistrationInfo {
  tiers: string[];
  handle_domain: string;
}

export interface NestInfoResponse {
  domain: string;
  version: string;
  registration: NodeRegistrationInfo | null;
}

/** Public node metadata over the pre-identity anonymous WS-RPC `fauna.nest.info`
 *  kind (replacing the deleted HTTP `GET /api/v1/node-info`; `api-layers.md` §
 *  Public). The wasm face resolves the same `discovery_core` the twin did. */
export async function fetchNestInfo(): Promise<NestInfoResponse> {
  return (await wasmNestInfo(nodeUrl())) as NestInfoResponse;
}

// (`checkHandleAvailable` — the old HTTP `GET /api/v1/handle-available` helper —
// was removed: it had no caller. Web onboarding validates handles inside the
// shared wasm `OnboardingMachine` (`fauna.account.register` rejects a taken
// handle), so no standalone availability probe is wired on web. The
// `fauna.handle.available` WS-RPC kind exists for clients that need it.)

// (`registerAccount` — the old HTTP `POST /api/v1/register` helper — was
// removed: web onboarding rides the shared wasm `OnboardingMachine`, whose
// register step uses the `fauna.account.register` WS-RPC kind.)

// --- Ed25519 challenge-response auth (onboarding silent-check path) ---

export interface ChallengeVerifiedActor {
  token: string;
  handle: string;
  domain: string;
  /**
   * The user's billing/feature tier on this nest (e.g. "personal", "pro").
   * Server-derived from the per-actor user record. Clients use this to
   * populate their handle/domain/tier cache on launch without a second
   * round-trip — see docs/goal/architecture/long-term-store.md.
   */
  tier: string;
  expires_at: number;
}

/**
 * Silent sign-in: run the challenge/verify ceremony over the pre-identity
 * anonymous WS-RPC kinds `fauna.auth.{challenge,verify}` (via the wasm
 * `challengeVerify`), and cache the returned token so subsequent authenticated
 * API calls reuse it. Returns `null` if the actor isn't registered on the
 * target nest (caller falls through to register / invite-request).
 *
 * This is web's WS-RPC replacement for the deleted HTTP
 * `POST /api/v1/auth/{challenge,verify}` — the last cross-origin HTTP auth
 * call web made (transport.md § Pre-identity (anonymous) connection; the
 * anonymous WS is CORS-exempt). A `TransientAuthError` (anonymous-WS connect /
 * timeout) propagates so the onboarding launch screen can show its Retry CTA.
 */
export async function silentSignIn(
  secretHex: string,
  targetNode?: string,
): Promise<ChallengeVerifiedActor | null> {
  const base = targetNode || nodeUrl();
  const verified = await challengeVerify(secretHex, base);
  if (verified) {
    storeMinted(tokenKey(base, secretHex), verified);
  }
  return verified;
}

// Invite-request management — both halves are off HTTP. The user-side helpers
// (`submitInviteRequest`/`getInviteRequestStatus`/`cancelInviteRequest`) ride
// the wasm `OnboardingMachine` (`fauna.account.invite_request.*`); the admin
// side (list/approve/deny) rides `rpc.ts` `adminInviteRequests{List,Approve,Deny}`
// (`fauna.admin.invite_requests.*`), consumed by `routes/admin/users/+page.svelte`.
// The dead `/admin/api/invite-requests*` HTTP helpers + `InviteRequestRow` type
// were removed here (superseded by the WS-RPC path; no consumers remained).

// `QuotaInfo` is the `fauna.quota.get` wire type — defined in rpc.ts (the
// WS-RPC wire-type source of truth), re-exported here for the SPA (mirrors the
// `EmailFilter` re-export below).
export type { QuotaInfo } from './rpc';

/** Tier-aware usage breakdown via `fauna.quota.get` (WS-RPC). */
export async function fetchQuota(secretHex: string): Promise<rpc.QuotaInfo> {
  return rpc.quotaGet(secretHex);
}

// `FeatureRow` is the `fauna.features.status` wire type — defined in rpc.ts,
// re-exported here for the SPA.
export type { FeatureRow } from './rpc';

/** The gated-feature plane's transparency read (`feature-limits-section`,
 *  `dynamic-features.md` § Transparency & auditability) via `fauna.features.status`
 *  joined with `fauna.nest.info`'s capability set (WS-RPC). */
export async function fetchFeatures(secretHex: string): Promise<rpc.FeatureRow[]> {
  return rpc.featuresRows(secretHex);
}

/** Queue a handle change via `fauna.profile.handle.change` (WS-RPC). The change
 *  is a delayed + cancellable pending action; `handle` echoes the requested
 *  handle (the reply's `new_handle`), which the caller shows optimistically. */
export async function changeHandle(
  secretHex: string,
  newHandle: string,
): Promise<{ handle: string }> {
  const reply = await rpc.profileHandleChange(secretHex, newHandle);
  return { handle: reply.new_handle };
}

/** Queue account deletion via `fauna.account.delete` (WS-RPC). Sits in the
 *  pending-action queue for the cancellation window before executing. No
 *  sign-out, no navigation — the pending-actions section below is the
 *  receipt and its cancel button the way back. */
export async function deleteAccount(secretHex: string): Promise<void> {
  await rpc.accountDelete(secretHex);
}

// ── Pending actions (`settings.md` § Pending actions) ──

export type { PendingActionSummary } from './rpc';

/** This actor's still-`pending` scheduled actions, newest first — the
 *  cancellation window the three delayed verbs above open. */
export async function pendingActionsList(secretHex: string): Promise<rpc.PendingActionSummary[]> {
  return rpc.pendingActionsList(secretHex);
}

/** Cancel a scheduled action before it executes (`pending-action-cancel-button`
 *  — one click, no confirm: cancelling is the safe direction). */
export async function pendingActionCancel(secretHex: string, id: number): Promise<void> {
  await rpc.pendingActionCancel(secretHex, id);
}

// ── Recovery kit (`settings.md` § Recovery kit) ──

export type { RecoveryStatus, RecoveryMinted } from './rpc';

/** A fresh read of `recovery-kit-status` off the registration chain. */
export async function recoveryKitStatus(secretHex: string): Promise<rpc.RecoveryStatus> {
  return rpc.recoveryKitStatus(secretHex);
}

/** Mint the first RecoveryKey registration (`recovery-kit-create-button`). */
export async function createRecoveryKit(
  secretHex: string,
  handle: string | null,
): Promise<rpc.RecoveryMinted> {
  return rpc.recoveryCreateKit(secretHex, handle);
}

/** Replace the registered kit using the one the user holds
 *  (`recovery-kit-replace-button`). */
export async function replaceRecoveryKit(
  secretHex: string,
  handle: string | null,
  phrase: string,
): Promise<rpc.RecoveryMinted> {
  return rpc.recoveryReplaceKit(secretHex, handle, phrase);
}

/** Register the onboarding-minted kit at the signed-in handoff. */
export async function registerDeferredRecoveryKit(secretHex: string, kitHex: string): Promise<void> {
  return rpc.recoveryRegisterDeferredKit(secretHex, kitHex);
}

/** Contest the pending replacement with the kit in hand (`recovery-pending-veto-button`). */
export async function vetoRecoveryReplacement(
  secretHex: string,
  phrase: string,
): Promise<rpc.RecoveryStatus> {
  return rpc.recoveryVeto(secretHex, phrase);
}

/** The no-escrow repair (`recovery-kit-escrow-reseal-button`). */
export async function resealRecoveryEscrow(
  secretHex: string,
  phrase: string,
): Promise<rpc.RecoveryStatus> {
  return rpc.recoveryResealEscrow(secretHex, phrase);
}

/** Open a seed-alone replacement window (`recovery-kit-lost-button`). */
export async function requestRecoveryKitLost(
  secretHex: string,
  handle: string | null,
): Promise<rpc.RecoveryMinted> {
  return rpc.recoveryLostKit(secretHex, handle);
}

export type { LandedSuccession, StolenOutcome } from './rpc';

/** Run the succession ceremony (`identity-stolen-button`) — take the account
 *  back from a stolen secret with the kit in hand. `conversations` is the live
 *  manager when the tab has one, for the post-succession group sweep.
 *
 *  ⚠ A `landed` outcome ends the session it is called from; the caller signs
 *  in as the returned identity. Every other arm is a sentence to paint. */
export async function succeedIdentityWithHeldKit(
  secretHex: string,
  phrase: string,
  conversations?: Parameters<typeof rpc.succeedIdentityWithHeldKit>[2],
): Promise<rpc.StolenOutcome> {
  return rpc.succeedIdentityWithHeldKit(secretHex, phrase, conversations);
}

/**
 * Upload a blob via the sidecar multipart wire and return its hash.
 *
 * `sidecar` is the DAG-CBOR-encoded `UploadSidecar`, `bytes` the sealed (or, for
 * `PublicPost`, plaintext) blob — both produced by `process_and_seal*` in
 * `wasm.ts`. The nest expects exactly two parts named `sidecar` and `bytes`
 * (`bins/fauna-nest/src/blob_routes.rs::parse_multipart_upload`). We must NOT
 * set `Content-Type` ourselves — the browser fills in the `multipart/form-data`
 * boundary when the body is a `FormData`.
 *
 * (Wire design tracked internally.)
 */
export async function uploadBlobMultipart(
  secretHex: string,
  sidecar: Uint8Array,
  bytes: Uint8Array,
): Promise<string> {
  const headers = await authHeaders(secretHex);
  const form = new FormData();
  form.append('sidecar', new Blob([toArrayBufferView(sidecar)], { type: 'application/cbor' }), 'sidecar');
  form.append('bytes', new Blob([toArrayBufferView(bytes)], { type: 'application/octet-stream' }), 'bytes');
  const res = await fetch(`${nodeUrl()}/api/v1/blob`, {
    method: 'POST',
    headers,
    body: form,
  });
  if (!res.ok) throw new Error(`blob upload failed: ${res.status}`);
  const json = await res.json();
  return json.hash;
}

/**
 * Fetch a blob's raw bytes on the bulk plane (`GET /api/v1/blob/{hash}`) — the
 * web twin of native `NestContentApi::get(paths::blob::by_hash(...))`. Used to
 * pull a gated post's already-sealed full-body blob for
 * `WasmFeedManager.unlockGatedPost` (the seal, not the transport, is what keeps
 * the body private, so the sealed bytes travel like any other blob).
 */
export async function fetchBlob(secretHex: string, hash: string): Promise<Uint8Array> {
  return fetchNestPath(secretHex, `/api/v1/blob/${hash}`);
}

// The bytes at a nest-relative `path` on the reader's own nest, with the session bearer — how a
// bridged post's `ProxiedImage` is fetched, exactly as a blob is (render-model.md § D6c: an
// `<img src>` cannot carry the bearer, so the page paints an object URL of these bytes).
export async function fetchNestPath(secretHex: string, path: string): Promise<Uint8Array> {
  const headers = await authHeaders(secretHex);
  const res = await fetch(`${nodeUrl()}${path}`, { headers });
  if (!res.ok) throw new Error(`nest fetch ${path}: ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

// A hand-rolled `sendEmailSmtp` RFC 5322 composer lived here but was dead (no UI
// caller, no `ui.yaml` element, no e2e) and diverged from linux's twin while both
// bypassed the canonical WASM-safe composer `fauna_conversations::rfc5322::build_message`.
// Removed (drift, #2/#4). The general mail-compose surface (`ui.yaml` "mail-write"
// track) composes via `build_message` and submits over the retained `rpc.emailSend`
// binding below — no hand-rolled composer is resurrected.

// The fauna-native inbox drain (`GET /api/v1/inbox/{actor}` → `fauna.inbox.fetch`)
// and the cross-nest delivery POST (`POST /api/v1/inbox/{actor}`) had web app
// functions here (`fetchInbox`/`sendToInbox`), but both were dead code — zero call
// sites since the old conversations infra. Web receives inbound
// social payloads via other already-WS-RPC paths (knocks/contacts on-demand;
// MLS Welcomes via the mail poll, `conversations.ts`). No client drains the
// fauna-native inbox today; that durable-delivery feature is dormant fleet-wide
// (owned by the WS-RPC-everywhere lead).

// The community-groups BARE-signed HTTP plane (`GET /api/v1/groups/{actor}`,
// `POST /api/v1/group[/{id}]`, `/members`, `/messages`) was deleted at T8 of the
// conversations WS-RPC slice, and its `fauna.conversations.group.*` WS-RPC
// successor was retired in turn (`conversation-rooms.md` § The group plane's
// fate); community conversations are the room family
// (`fauna.conversations.room.*`) on the shared wasm `ConversationsManager`. The
// web `listGroups`/`createGroup`/`sendGroupAction`/`fetchGroupMembers`/
// `fetchGroupMessages` HTTP twins were removed once the `/groups` page +
// `$lib/channels` were deleted in the page convergence.

// `fetchBackupStatus` (was `GET /api/v1/sync/backup-status`) moved to
// `$lib/rpc` (`fauna.sync.backup_status`).

// Media-page sync state rides `fauna.sync.{status,files}` over the shared
// `fauna_client_sync::SyncClient` (wasm twin), retiring `GET /api/v1/sync/*`.
// Signatures preserved so `routes/media/+page.svelte` is untouched.
// `fetchSyncStatus` throws on error (matching the twin's
// non-OK throw — the page catches it); `fetchSyncFiles` returns `[]` on error
// (the twin returned `[]` on any non-OK, e.g. an unowned/empty folder).
export async function fetchSyncStatus(
  secretHex: string,
  folder: string,
): Promise<{ source_online: boolean }> {
  return rpc.syncStatus(secretHex, folder);
}

export async function fetchSyncFiles(
  secretHex: string,
  folder: string,
): Promise<{ path: string; size_bytes: number; manifest_hash: string; updated_at: number }[]> {
  try {
    return await rpc.syncFiles(secretHex, folder);
  } catch {
    return [];
  }
}

// Snapshot CRUD + list + detail + prune + check (was `/api/v1/snapshots*`
// + `/api/v1/sync/backup-status`) moved to `$lib/rpc` as the
// `fauna.filesync.snapshot.*` / `fauna.sync.backup_status` WS-RPC twins
// (Track B15 / B13). No snapshot route is HTTP: a file's bytes come from the
// shared client-side walk (`downloadSnapshotFileBytes` →
// `fauna_core::file_download` over wasm; `ui/backups.md` § Where logic lives →
// *Single-file byte download*) — owner chunks are sealed and the nest holds no
// opening key, so the nest cannot reassemble a file.

// --- Feed + Posts API (fauna.feed.* / fauna.posts.* WS-RPC) ---
//
// The HTTP twins (`POST|GET|PUT|DELETE /api/v1/feeds*`, `POST /api/v1/posts`,
// `POST /api/v1/posts/{id}/interact`) were removed in
// T2/T4; these route through the `rpc.ts`
// singleton WS-RPC client (backed by the shared `FeedClient`/`PostsClient`
// wrappers). Feed `rules` ride typed — an array of externally-tagged
// `FilterRule` objects, carried natively by the dag-cbor wire (`ui/feed.md`
// § Where logic lives). `GET /api/v1/posts/{id}`
// is a federation-only surface; local clients use `fauna.posts.get`
// (`transport.md:482`).

export async function createFeed(
  secretHex: string,
  name: string,
  rules: FilterRule[],
  combination: string,
): Promise<{ feed_id: string }> {
  return rpc.feedCreate(secretHex, name, rules, combination);
}

export async function listFeeds(secretHex: string): Promise<{ feeds: FeedDefinition[] }> {
  const feeds = await rpc.feedList(secretHex);
  // The list view omits rules (they ride only on `fauna.feed.get`), matching
  // the legacy HTTP `/api/v1/feeds` shape.
  return {
    feeds: feeds.map((f) => ({
      feed_id: f.feed_id,
      name: f.name,
      rules: [],
      combination: f.combination,
      created_at: f.created_at,
    })),
  };
}

export async function getFeed(secretHex: string, feedId: string): Promise<FeedDefinition> {
  const f = await rpc.feedGet(secretHex, feedId);
  return {
    feed_id: f.feed_id,
    name: f.name,
    rules: f.rules,
    combination: f.combination,
    created_at: f.created_at,
  };
}

export async function updateFeed(
  secretHex: string,
  feedId: string,
  name: string,
  rules: FilterRule[],
  combination: string,
): Promise<void> {
  await rpc.feedUpdate(secretHex, feedId, name, rules, combination);
}

export async function deleteFeed(secretHex: string, feedId: string): Promise<void> {
  await rpc.feedDelete(secretHex, feedId);
}

// Map a `fauna.feed.{posts,local.posts}` item to the SPA's `FeedPost`. The
// signed post body is decoded separately client-side (`fetchAndDecodePost`),
// so only the index fields are mapped here.
function mapFeedPost(p: rpc.FeedPostItem): FeedPost {
  return {
    post_id: p.post_id,
    author: p.author,
    created_at: p.created_at,
    tags: p.tags,
    has_media: p.has_media,
    is_reply: p.is_reply,
    source: p.source,
    // Carry the nest's index body so the feed renders the text immediately and
    // still shows posts whose signed-body decode is pending or fails — matching
    // linux/windows (`post_list.rs` renders `item.body`). Previously dropped, so
    // web showed nothing until the per-post decode landed.
    body: p.body,
  };
}

export async function queryFeed(
  secretHex: string,
  feedId: string,
  cursor?: number,
  limit?: number,
): Promise<FeedQueryResult> {
  const reply = await rpc.feedPosts(secretHex, feedId, cursor, limit);
  return { posts: reply.posts.map(mapFeedPost), cursor: reply.cursor ?? null };
}

export async function queryLocalFeed(
  secretHex: string,
  cursor?: number,
  limit?: number,
): Promise<FeedQueryResult> {
  const reply = await rpc.feedLocalPosts(secretHex, cursor, limit);
  return { posts: reply.posts.map(mapFeedPost), cursor: reply.cursor ?? null };
}

export async function createPost(secretHex: string, payload: Uint8Array): Promise<{ post_id: string }> {
  return rpc.postsCreate(secretHex, payload);
}

export async function getPost(secretHex: string, postId: string): Promise<Uint8Array> {
  return rpc.postsGet(secretHex, postId);
}

// `interactWithPost` is deliberately GONE: it fired `fauna.posts.interact` and
// discarded the reply, so the interaction counts on screen — which render from
// the FeedManager snapshot — never moved. The one interact path is
// `WasmFeedManager.interact` (feed.md § User actions), which folds the nest's
// post-act counters back into that snapshot. `rpc.postsInteract` remains the raw
// transport underneath it; call the manager, not the transport.

// --- Knock & Contact API ---

// Routed through the `fauna.{knocks,contacts,inbox.mode}.*` WS-RPC kinds via
// the `rpc.ts` singleton façade (the HTTP twins were deleted nest-side). The
// connection actor is the calling actor, so the legacy `{actor_id}` path param
// is dropped. The knock/contact wire rows are field-identical to `Knock` /
// `Contact`, so they flow straight through.

export async function fetchKnocks(secretHex: string): Promise<Knock[]> {
  return rpc.knocksList(secretHex);
}

export async function acceptKnock(secretHex: string, peerId: string): Promise<void> {
  await rpc.knocksAccept(secretHex, peerId);
}

export async function blockKnock(secretHex: string, peerId: string): Promise<void> {
  await rpc.knocksBlock(secretHex, peerId);
}

export async function dismissKnock(secretHex: string, peerId: string): Promise<void> {
  await rpc.knocksDismiss(secretHex, peerId);
}

export async function fetchContacts(secretHex: string): Promise<Contact[]> {
  return rpc.contactsList(secretHex);
}

export async function confirmContact(secretHex: string, peerId: string): Promise<void> {
  await rpc.contactsConfirm(secretHex, peerId);
}

/** Send a knock (contact request) to `peerId`. Rides `fauna.inbox.send` with a
 *  shared-Rust-composed payload — the successor of the deleted
 *  `POST /api/v1/contacts/{actor_id}/knock` twin
 *  (`api-layers.md` § Contacts & Knocks). */
export async function sendKnock(secretHex: string, peerId: string): Promise<void> {
  await rpc.sendKnock(secretHex, peerId);
}

export async function getInboxMode(secretHex: string): Promise<string> {
  return rpc.inboxModeGet(secretHex);
}

export async function setInboxMode(secretHex: string, mode: string): Promise<void> {
  await rpc.inboxModeSet(secretHex, mode);
}

// --- MLS cross-nest transport (retired — now nest-side over the federation channel) ---
//
// Web's same-nest MLS surface (channel send/fetch, key-package upload/fetch/count,
// welcome deliver) rides the wasm `ConversationsManager` over the
// `fauna.conversations.*` WS-RPC kinds; the same-nest HTTP twins were deleted at the
// conversations slice T8. The two cross-nest *direct-client-to-foreign-nest* HTTP
// wrappers `postWelcomeToNest` (`POST /api/v1/welcome/{actor}?nest_url=…`) and
// `fetchKeyPackage` (`GET /api/v1/keypackage/{actor}`) were **removed here**: nest
// Spec Y2 slice 5 retired the HTTP federation interim, so a client no
// longer reaches a foreign nest directly — its *home* nest originates the cross-nest
// leg over the nest↔nest `fauna.federation.{welcome.deliver,keypackage.fetch}` WS-RPC
// channel (`docs/goal/architecture/federation.md` § Federation residue surface). Both
// web wrappers had zero callers (the wasm path already carried cross-nest), so this is
// a pure dead-code prune, not a behavior change.

// --- Bridge Feed Subscriptions (fauna.bridges.feeds.* WS-RPC) ---

export async function subscribeBridgeFeed(secretHex: string, bridge: string, feedUri: string, name: string): Promise<{ id: number }> {
  return { id: await rpc.bridgesFeedsCreate(secretHex, bridge, feedUri, name) };
}

export async function listBridgeFeeds(secretHex: string): Promise<{ subscriptions: rpc.FeedSubscription[] }> {
  return { subscriptions: await rpc.bridgesFeedsList(secretHex) };
}

export function unsubscribeBridgeFeed(secretHex: string, id: number): Promise<void> {
  return rpc.bridgesFeedsDelete(secretHex, id);
}

// --- Email Filters (fauna.email.filters.* WS-RPC) ---

// Re-exported from rpc.ts (the single source of truth, mirroring
// fauna_protocol::email::EmailFilter field-by-field).
export type { EmailFilter } from './rpc';

export function listEmailFilters(secretHex: string): Promise<rpc.EmailFilter[]> {
  return rpc.emailFiltersList(secretHex);
}

export async function createEmailFilter(
  secretHex: string,
  name: string,
  rules: unknown[],
  combination: string,
  action: string | Record<string, unknown>,
  priority: number,
): Promise<{ id: number }> {
  return { id: await rpc.emailFiltersCreate(secretHex, name, rules, combination, action, priority) };
}

export async function deleteEmailFilter(secretHex: string, id: number): Promise<void> {
  return rpc.emailFiltersDelete(secretHex, id);
}

export function getEmailFilter(secretHex: string, id: number): Promise<rpc.EmailFilter> {
  return rpc.emailFiltersGet(secretHex, id);
}

export async function updateEmailFilter(
  secretHex: string,
  id: number,
  name: string,
  rules: unknown[],
  combination: string,
  action: string | Record<string, unknown>,
  priority: number,
): Promise<void> {
  return rpc.emailFiltersUpdate(secretHex, id, name, rules, combination, action, priority);
}

// The post-succession filter-mark review (succession-aftermath.md §
// Adjudicating what the aftermath carries across, the fourth plane).

export function filterMarksList(secretHex: string): Promise<number[]> {
  return rpc.filterMarksList(secretHex);
}

export function filterMarkKeep(secretHex: string, filterId: number): Promise<boolean> {
  return rpc.filterMarkKeep(secretHex, filterId);
}

export function filterMarkRemoved(secretHex: string, filterId: number): Promise<boolean> {
  return rpc.filterMarkRemoved(secretHex, filterId);
}

// --- Calendar & Events (encrypted CalDAV store, events.md Decision B) ---
//
// The Events page reads/writes the *encrypted* `bridge_caldav_*` store via the
// `caldav*` WS-RPC seam (`rpc.ts` → wasm `WsRpcClient` → shared
// `fauna_client_caldav::CalDavClient`) — the same store + RPCs the mail-bridge
// MDA serves to Apple Calendar, so a Fauna-created appointment and an
// Apple-Calendar one are the same data. This retired the legacy plaintext path
// (the `fauna.events.*` WS-RPC + `/api/{events,calendars}` REST twins → the
// `content` table), which on an encrypted nest was a disjoint store invisible to
// CalDAV clients. (Slice 2).
//
// Two facts shape the flow:
//   • the encrypted path is **msek-gated** — `cfg.mail.msek` is `None` on a
//     localhost/IP/mail-off nest, so the lists come back empty (graceful, not an
//     error; the page renders an honest "enable mail" state).
//   • every event row is the *full* flat VEVENT (attendees embedded), keyed by
//     the hex `uid_hash` (`EventSummary.id`) — the mutate key for
//     delete/rsvp/reminder/invite. There is no separate event-detail or
//     attendee-list fetch (`getEvent`/`listEventAttendees` are gone), and the
//     legacy cross-calendar "my events" / cross-nest inbox-invitation queries
//     (`queryMyEvents`/`fetchInboxInvitations`/`remoteRsvp`) have no CalDAV
//     analogue — invitations are iMIP (caldav-server.md § Scheduling).
//
// `selfEmail` is the caller's `<handle>@<domain>` (the VEVENT ORGANIZER / self
// RSVP CAL-ADDRESS); `nowSecs` is epoch-seconds stamped JS-side (the wasm-time
// discipline — wasm never calls `Date.now()`).

export interface Calendar {
  id: string;
  name: string;
  color: string;
  event_count?: number;
}

export interface EventAttendee {
  name: string;
  email: string;
  /** Projected RSVP: going | interested | tentative | declined | invited. */
  rsvp: string;
}

export interface EventSummary {
  /** Hex `uid_hash` — the encrypted-store row key + the mutate target. */
  id: string;
  uid: string;
  summary: string;
  dtstart: string;
  dtend: string;
  location: string;
  description: string;
  status: string;
  is_all_day: boolean;
  rrule: string;
  alarm: string;
  /** Canonical author-gate predicate (computed in shared Rust from the VEVENT
   *  ORGANIZER email vs the actor's email) — gates delete/invite. Same predicate
   *  native's `FfiCalEvent.organized_by_me` exposes. */
  organized_by_me: boolean;
  attendees: EventAttendee[];
}

const DEFAULT_CALENDAR_COLOR = '#3273dc';

/** A fresh client-assigned 32-byte calendar id, hex-encoded (the MKCOL id;
 *  mirrors the linux client-side id). */
function randomCalendarId(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return hexFull(bytes);
}

function nowSecs(): number {
  return Math.floor(Date.now() / 1000);
}

export async function listCalendars(secretHex: string): Promise<Calendar[]> {
  const { calendars } = await rpc.caldavListCalendars(secretHex);
  return calendars as Calendar[];
}

export async function createCalendar(secretHex: string, name: string): Promise<{ id: string }> {
  return rpc.caldavProvisionCalendar(secretHex, randomCalendarId(), name, DEFAULT_CALENDAR_COLOR);
}

export async function queryEvents(
  secretHex: string,
  calendarId: string,
  selfEmail: string,
): Promise<EventSummary[]> {
  const { events } = await rpc.caldavQueryEvents(secretHex, calendarId, selfEmail);
  return events as EventSummary[];
}

export async function createEvent(secretHex: string, params: {
  calendar_id: string;
  uid: string;
  summary: string;
  dtstart: string;
  dtend: string;
  description?: string;
  location?: string;
}, selfEmail: string): Promise<{ id: string; uid: string }> {
  return rpc.caldavCreateEvent(secretHex, params, selfEmail, nowSecs());
}

export async function deleteEvent(secretHex: string, calendarId: string, uidHash: string): Promise<void> {
  return rpc.caldavDeleteEvent(secretHex, calendarId, uidHash);
}

export async function rsvpEvent(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  response: string,
  selfEmail: string,
): Promise<{ status: string }> {
  return rpc.caldavRsvpEvent(secretHex, calendarId, uidHash, response, selfEmail, nowSecs());
}

/** Invite an attendee by email (adds a `mailto:` ATTENDEE to the VEVENT roster
 *  + re-PUTs; the iMIP `REQUEST` fan-out rides the same path — caldav-server.md
 *  § Scheduling & invitations). */
export async function inviteToEvent(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  attendeeEmail: string,
  selfEmail: string,
): Promise<void> {
  return rpc.caldavInviteAttendee(secretHex, calendarId, uidHash, attendeeEmail, selfEmail, nowSecs());
}

// --- Event Reminders (VALARM on the encrypted VEVENT, read-mutate-rewrite) ---

export async function getReminder(secretHex: string, calendarId: string, uidHash: string): Promise<string | null> {
  return rpc.caldavReminderGet(secretHex, calendarId, uidHash);
}

export async function setReminder(secretHex: string, calendarId: string, uidHash: string, offset: string): Promise<void> {
  await rpc.caldavReminderSet(secretHex, calendarId, uidHash, offset, nowSecs());
}

export async function removeReminder(secretHex: string, calendarId: string, uidHash: string): Promise<void> {
  return rpc.caldavReminderRemove(secretHex, calendarId, uidHash, nowSecs());
}

// (Setup-status was an HTTP helper hitting `GET /api/v1/setup-status`. Web
// now reaches the same data via the anonymous WS-RPC `fauna.setup.status`
// kind through `OnboardingMachine.probeSetupStatus`.)

// --- Admin ---

export async function checkIsAdmin(secretHex: string): Promise<boolean> {
  // `fauna.account.am_i_admin` (WS-RPC). Any transport/auth failure → treat as
  // not-admin so the admin UI stays hidden (the gate is fail-closed).
  try {
    return await rpc.accountAmIAdmin(secretHex);
  } catch {
    return false;
  }
}

// (`claimAdmin` — the old HTTP `POST /api/v1/claim-admin` helper — was removed:
// web onboarding rides the shared wasm `OnboardingMachine`, whose claim step
// uses the `fauna.auth.claim_admin` WS-RPC kind.)

// ── Spam / moderation API ───────────────────────────────────

// `trainSpam` rides `fauna.moderation.train` over the shared
// `fauna_client_moderation::ModerationClient` (wasm twin) — the nest half of a
// training correction (read gate + report capture; it trains nothing, the
// model rests sealed). It throws on a server error — the consumer surfaces
// "Training failed".
export async function trainSpam(secretHex: string, contentId: string, verdict: 'spam' | 'ham'): Promise<void> {
  await rpc.moderationTrain(secretHex, contentId, verdict);
}

// Spam-classifier preferences ride the shared `fauna.spam.{get,set}_preferences`
// WS-RPC kinds (`rpc.ts` → wasm `SpamClient`), replacing the deleted
// `GET|PUT /api/v1/spam/preferences` twins. Thresholds stay probability
// `[0.0, 1.0]` (the wasm twin converts to/from the per-mille wire), so the shape
// is unchanged. (S3b.)
export async function getSpamPreferences(secretHex: string): Promise<{
  spam_threshold: number;
  phishing_threshold: number;
}> {
  return rpc.spamGetPreferences(secretHex);
}

export async function updateSpamPreferences(
  secretHex: string,
  prefs: Partial<{ spam_threshold: number; phishing_threshold: number }>,
): Promise<void> {
  await rpc.spamSetPreferences(secretHex, prefs);
}

// Calendar `.ics` import/export on the encrypted CalDAV store (events.md
// § Import / Export): `generate_ical_multi` / `parse_ical_multi` run inside wasm
// over the actor's msek, retiring the `/api/calendars/{id}/{import,export}` REST
// twins. Import seals + PUTs each VEVENT; re-PUTting a duplicate `UID` updates
// its row (the `uid_hash` dedup key), so there is no separate skip/overwrite
// mode (mirrors the linux lead).
export async function importCalendar(
  secretHex: string,
  calendarId: string,
  icsText: string,
  selfEmail: string,
): Promise<{ imported: number; skipped: number; total: number }> {
  return rpc.caldavImportCalendar(secretHex, calendarId, icsText, selfEmail, nowSecs());
}

export async function exportCalendar(secretHex: string, calendarId: string): Promise<string> {
  return rpc.caldavExportCalendar(secretHex, calendarId);
}

// Full-text search rides `fauna.search.query` over the shared
// `fauna_client_search::SearchClient` (wasm twin), retiring `GET /api/v1/search`.
// Signature-preserving so the search page (`routes/search/+page.svelte`) is
// untouched.
export type { SearchResult } from './rpc';

export async function searchContent(
  secretHex: string,
  query: string,
  contentType?: string,
  limit?: number,
): Promise<rpc.SearchResult[]> {
  return rpc.searchQuery(secretHex, query, contentType, limit);
}
