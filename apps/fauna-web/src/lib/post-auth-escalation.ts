// Post-auth escalation — the ONE thing web does with a TERMINAL auth verdict
// discovered after launch. Web's leg of `docs/goal/architecture/security.md`
// § Transport trust → § Post-auth surfacing (ratified 2026-07-23), extended
// 2026-09-01 to the succeeded-identity verdict
// (`identity-succession.md` § Propagation → *Own device fleet*) and 2026-09-28
// to the background refresh's `fauna.auth.not_registered` (`escalateSignInRefused`).
//
// ## What this is
//
// Identity verification does not end at launch. Web has two live post-auth
// re-check points (the third, an SPKI-pin mismatch on the bearer connection, is
// native-only — the browser owns TLS):
//
//   1. **Bearer re-mint** — `$lib/api`'s `getAuthToken`, whose wasm
//      `challengeVerify` possession-verifies its `cert_binding` against the
//      TOFU pin.
//   2. **Background silent challenge** — `$lib/store`'s `refreshFromServer`,
//      whose `challengeVerify` runs the same check on the verify path.
//
// Either can produce a terminal verdict. This module is the ONE thing both do
// with it, so the two channels cannot drift into different behaviours — the same
// reason the verdict itself is one shared Rust helper rather than a copy per
// channel.
//
// ## Two verdicts, one mechanism
//
// **The nest changed identity** — the SSH `known_hosts` case: possible MITM, the
// account is fine. **This identity was succeeded** — the account moved to a
// different keypair and this one can never sign in again. Different causes and
// different remedies, but structurally the same event: a verdict that no retry
// can change, whose remedy lives on a screen the current route does not have.
// So both take the identical path — drop the bearers, re-enter the launch
// route, and let the `LaunchMachine` reach the verdict through its own
// challenge and own the surface. Neither synthesises a surface in place.
//
// Web needs that re-entry for succession where the native apps do not, and the
// reason is web's route model: a signed-in reload lands wherever the user was,
// and the layout's boot guard only diverts to `/onboarding` when the stored
// identity is *missing* — which a succeeded device's is not. So on web the
// launch machine does not run at all on a relaunch, and without this escalation
// the refusal reached `store.ts`'s catch and was logged and swallowed. tui and
// linux run their launch machine on every start and reach the verdict directly.
//
// ## Why a re-entry and not a banner
//
// The goal doc is explicit that a possible-MITM signal gets the one uniform
// surface all seven apps already render — no banner, badge or toast. It is also
// explicit about the *shape*: tear the session down **without erasing
// credentials** (nothing is wrong with the identity; the *nest* changed) and
// **re-enter the real launch flow** rather than painting the surface in place.
//
// That second half is load-bearing on web for a concrete reason. The surface's
// re-trust button drives `trustNestIdentity()` on the `LaunchMachine` **that
// produced the verdict**, and the machine lives on the onboarding route. A
// synthesised in-place warning painted on, say, the feed would look identical and
// its button would silently do nothing — the dead-button trap the cross-app e2e
// exists to catch. So we navigate to the launch route and let the real machine
// re-run its pinned challenge: the pin is still poisoned, so it reaches
// `LaunchPhase::IdentityChanged` on its own and owns the recovery.
//
// A raw `location.assign` and not a `goto`: the wasm WS-RPC/MLS session must be
// rebuilt, exactly as the account switcher's teardown does it.

import { base } from '$app/paths';

import { clearTokenCache } from './api';
import {
  IdentitySupersededError,
  NestIdentityChangedError,
  SignInRefusedError,
  classifyChallengeError,
} from './auth-errors';

/**
 * True once an escalation has been started on this document.
 *
 * Both channels can fire within the same tick (a re-mint and a background
 * refresh racing on the same poisoned pin), and every in-flight request that was
 * waiting on a bearer rejects at once. Without this latch each of those would
 * fire its own `location.assign` — a burst of navigations against a document
 * that is already leaving. First verdict wins; the rest are no-ops.
 */
let escalating = false;

/** Test seam: the module-level latch survives soft state resets, so the e2e
 *  agent's per-test reset clears it. Never called from a product path. */
export function resetPostAuthEscalation(): void {
  escalating = false;
}

/**
 * The shared mechanism both verdicts take: drop every bearer, then re-enter the
 * launch route so the `LaunchMachine` reaches the verdict itself and owns the
 * surface. Returns `true` when this call owned the escalation, `false` when one
 * was already under way.
 *
 * Factored out when the succeeded-identity verdict joined the nest-identity one
 * (2026-09-01): every line here — the latch, the bearer drop, the
 * already-on-the-launch-route guard, the teardown count — is required by both
 * for the same reason, and a second copy is exactly how two channels drift.
 */
function escalateToLaunchSurface(log: string): boolean {
  if (escalating) return false;

  console.warn(log);

  // Drop every cached bearer FIRST. The session is de-facto dead — its
  // connections can no longer graduate — and a bearer minted before the verdict
  // must not outlive it or be reused by anything still running while the
  // navigation settles.
  clearTokenCache();

  // ⚠ Already on the launch route? Then STOP here — the `LaunchMachine` owns
  // this surface and is reaching the same verdict through its own pinned
  // challenge. Navigating would reload the very page that produced the verdict,
  // which re-runs `identity.init()`, which re-fires this refresh, which
  // escalates again: a self-sustaining reload loop, since the latch above is
  // module state and a document swap resets it. That is the same trap the
  // layout's route guard carries (an unconditional same-URL `goto` measured at
  // ~47 navigations/s, 2026-08-02) — one door, one owner.
  //
  // And do NOT latch here. Nothing navigates on this arm, so there is no burst
  // to suppress — while a latch set now would outlive the launch route: the
  // wizard leaves `/onboarding` by an in-document `goto`, so a verdict met
  // before the account exists (a bearer mint refused `not_registered`
  // mid-wizard) would silence every later escalation for the rest of the
  // signed-in session.
  if (window.location.pathname.startsWith(`${base}/onboarding`)) {
    console.warn('[identity] already on the launch route — the LaunchMachine owns the surface');
    return true;
  }
  escalating = true;

  // Count the teardown at its initiation point, synchronously, in the same job
  // as the navigation below (convention 14's negative-assert observable). Same
  // hook-not-import shape as the account switcher's bump, so a production build
  // never bundles the module: see `$lib/generation-e2e`.
  if (__FAUNA_E2E_AUTOMATION__) {
    (window as unknown as { __fauna_recordSessionTeardown?: () => void })
      .__fauna_recordSessionTeardown?.();
  }

  // Credentials stay put. The nest-identity exits (re-trust, or pick a different
  // nest) both need the identity in hand — and a succeeded device's seed is
  // still the user's own, the thing a successor ceremony and any later re-import
  // reason about. The account moved; the person did not.
  window.location.assign(`${base}/onboarding`);
  return true;
}

/**
 * Route a nest-identity verdict to the blocking launch surface.
 *
 * Returns `true` when this call owned the escalation (so a caller can log or
 * short-circuit), `false` when one was already under way.
 */
export function escalateNestIdentityChanged(e: NestIdentityChangedError): boolean {
  return escalateToLaunchSurface(
    `[identity] nest identity changed for ${e.origin || 'this nest'} ` +
      `(pinned ${e.pinned || '?'}, seen ${e.seen ?? 'none — proof withdrawn'}) — ` +
      'blocking the session and re-entering the launch flow',
  );
}

/**
 * Route a succeeded-identity verdict to the blocking launch surface, where the
 * launch machine's `superseded_successor` arm routes it on to the import screen.
 *
 * The claimed successor is logged and goes no further: an admin debugging a
 * fleet wants it, and the user must not be shown it as fact before the
 * registration chain proves it (`identity-succession.md` § Propagation → *Own
 * device fleet* — the nest is enforcer and distributor, never authorizer).
 */
export function escalateIdentitySuperseded(e: IdentitySupersededError): boolean {
  return escalateToLaunchSurface(
    '[identity] this identity was succeeded ' +
      `(claimed successor ${e.claimedSuccessor || 'unnamed'}) — ` +
      'blocking the session and re-entering the launch flow',
  );
}

/**
 * Route the home nest's `fauna.auth.not_registered` — met by the background
 * silent challenge, which reports it as a `null` result rather than a throw — to
 * the launch surface, where the `LaunchMachine` meets the same refusal on its
 * own challenge and parks on the previously-signed-in row (`onboarding.md`
 * § App-launch routing; the shared rule is `fauna_launch_machine::auth`'s
 * `SilentSignInVerdict::NotRegistered`, the third verdict that escalates).
 *
 * The nest no longer signs this identity in — suspended or removed, opaque by
 * design — so the session is de-facto dead, and until this escalation web was
 * the one app whose signed-in reload sat on its route telling the user nothing.
 * Credentials stay put (`settings.md` § Where logic lives → Account deletion: a
 * suspension may be lifted; only the user's own sign-out erases).
 */
export function escalateSignInRefused(): boolean {
  return escalateToLaunchSurface(
    '[identity] this nest no longer signs this identity in (fauna.auth.not_registered) — ' +
      'blocking the session and re-entering the launch flow',
  );
}

/**
 * The one-line adapter for a background path's catch: escalate when `e` is a
 * TERMINAL post-auth verdict, and report whether it was.
 *
 * A caller that gets `false` back must keep its existing behaviour for that
 * error — every *other* failure class on these paths stays logged-and-swallowed
 * (transient/network faults are not verdicts, and a session that tore itself
 * down on a blip would be strictly worse than one that degrades). Only a
 * terminal verdict escalates.
 *
 * **Takes the error in EITHER form, and that is the point.** A caller's catch
 * may hold `challengeVerify`'s internally classified typed error, or a raw
 * prefixed string straight from wasm (the shape the retired handshake re-mint
 * handed over, and any future raw channel's). Normalising here — rather than
 * asking each call site to know which it has — is what keeps a caller from
 * double-classifying. Running an
 * already-typed error back through `classifyChallengeError` silently DOWNGRADES
 * it: the classifier reads `.message`, which by then is the human sentence and
 * no longer carries the wire prefix, so the verdict comes back a plain `Error`
 * and is swallowed. That is not hypothetical — it is exactly how this shipped
 * red on its first e2e run.
 */
export function escalateIfTerminalAuthVerdict(e: unknown): boolean {
  if (e instanceof NestIdentityChangedError) return escalateNestIdentityChanged(e);
  if (e instanceof IdentitySupersededError) return escalateIdentitySuperseded(e);
  if (e instanceof SignInRefusedError) return escalateSignInRefused();
  const verdict = classifyChallengeError(e);
  if (verdict instanceof NestIdentityChangedError) return escalateNestIdentityChanged(verdict);
  if (verdict instanceof IdentitySupersededError) return escalateIdentitySuperseded(verdict);
  return false;
}
