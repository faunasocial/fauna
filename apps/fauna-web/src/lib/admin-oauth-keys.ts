// The outside-app sign-in key section's state (`admin-nest-oauth-*` on
// admin-nest; authorization-server.md § The issuer → Two rotation arms) — the
// web twin of tui's `OauthKeysRead` and `AdminState::{oauth_confirm,
// oauth_status, oauth_in_flight}`, and of the four gestures' guards
// (`apps/fauna-tui/src/admin/mod.rs`).
//
// This module decides only WHEN a control may act. Every sentence the section
// shows is a shared `fauna_client_admin` fold reached through `$lib/wasm` (the
// confirm fold is handed in as a parameter rather than imported), and every
// dispatch stays in the page. That split is what lets the guards making a
// double press harmless run under plain `deno test`: the imports are relative
// and type-only for the reason `./custody.ts` records — the `$lib` alias is a
// SvelteKit/Vite one `deno test` cannot resolve.
//
// Every transition returns a NEW section (or `null` when it refuses, leaving
// the old one untouched), so the page holds it in `$state.raw` and a refused
// press has nothing to roll back.

import type { LocalizedText } from './i18n/localized.ts';

/** One served key — mirrors shared Rust `fauna_client_admin::IssuerKeyRow`
 *  field-by-field. Instants are epoch seconds; both are `null` on the signer. */
export interface IssuerKeyRow {
  kid: string;
  signing: boolean;
  retired_at: number | null;
  /** `retired_at` + the retirement horizon, computed by the shared fold so no
   *  app re-derives the instant the nest will act on. */
  served_until: number | null;
}

/** The folded `fauna.oauth.issuer_key_status` read — mirrors
 *  `fauna_client_admin::IssuerKeyView`. `keys` arrive signer first in the
 *  nest's own order, and nothing here re-sorts them. */
export interface IssuerKeyView {
  active_kid: string;
  keys: IssuerKeyRow[];
  retirement_horizon_secs: number;
  rotation_in_flight: boolean;
}

/** The two forced arms, spelled as `fauna_client_admin::IssuerForcedArm`'s own
 *  serde names — the value the wasm faces take. */
export type IssuerForcedArm = 'IssuerKey' | 'SessionSecret';

/** An armed confirm's words — mirrors `fauna_client_admin::IssuerForcedConfirmView`. */
export interface IssuerForcedConfirmView {
  summary: LocalizedText;
  confirm_label: LocalizedText;
}

/** The key set's read. Only `ready` paints key rows: "not asked yet" and
 *  "couldn't find out" get the reason line instead, never an empty list that
 *  would read as "this nest has no keys". `reason` is already worded. */
export type OauthKeysRead =
  | { kind: 'unread' }
  | { kind: 'ready'; view: IssuerKeyView }
  | { kind: 'failed'; reason: string };

/** The armed forced confirm: which arm, and its words as folded at ARM time —
 *  never re-folded while armed, so what the admin read before pressing is what
 *  they confirm even if the set moves on underneath. */
export interface ArmedOauthForced {
  arm: IssuerForcedArm;
  confirm: IssuerForcedConfirmView;
}

export interface OauthSection {
  keys: OauthKeysRead;
  /** `null` = unarmed. One arm at a time. */
  armed: ArmedOauthForced | null;
  /** The last control's verdict (`admin-nest-oauth-status`); `null` until a
   *  control was used. Never the page's `error-message`. */
  status: string | null;
  /** Whether any of the three controls' calls is in flight. */
  inFlight: boolean;
}

export function initialOauthSection(): OauthSection {
  return { keys: { kind: 'unread' }, armed: null, status: null, inFlight: false };
}

/** The view to paint rows from — present only once the set has answered. */
export function oauthKeys(s: OauthSection): IssuerKeyView | null {
  return s.keys.kind === 'ready' ? s.keys.view : null;
}

/** All three controls are live exactly when the set has answered and no call
 *  is in flight — disabled, never hidden, otherwise. The forced confirm cannot
 *  name what it drops before the set has answered, and a press in flight must
 *  not chain a second rotation onto the first. */
export function oauthLive(s: OauthSection): boolean {
  return oauthKeys(s) !== null && !s.inFlight;
}

/** A read landed (the mount-time read; an armed confirm keeps its capture). */
export function oauthKeysLoaded(s: OauthSection, keys: OauthKeysRead): OauthSection {
  return { ...s, keys };
}

/** The ordinary rotation's press → the in-flight section, or `null` when it
 *  must dispatch nothing. It disarms a forced confirm armed beside it, whose
 *  stated key count this rotation is about to change. */
export function beginOauthRotate(s: OauthSection, working: string): OauthSection | null {
  if (!oauthLive(s)) return null;
  return { ...s, armed: null, inFlight: true, status: working };
}

/** A forced arm's press → the armed section, or `null` when refused. Arming
 *  dispatches nothing: it folds the confirm ONCE over the view the admin is
 *  looking at and clears the previous verdict. Arming the sibling replaces
 *  the confirm outright. */
export function armOauthForced(
  s: OauthSection,
  arm: IssuerForcedArm,
  fold: (arm: IssuerForcedArm, view: IssuerKeyView) => IssuerForcedConfirmView,
): OauthSection | null {
  const view = oauthKeys(s);
  if (!view || s.inFlight) return null;
  return { ...s, status: null, armed: { arm, confirm: fold(arm, view) } };
}

/** Cancel: disarm, touching nothing else. */
export function cancelOauthForced(s: OauthSection): OauthSection {
  return { ...s, armed: null };
}

/** The confirm's press, for the arm the pressed button was painted for →
 *  the in-flight section and the arm to dispatch, or `null` when it must
 *  dispatch nothing (nothing armed, the other arm armed, or a call already in
 *  flight — the armed confirm is then left exactly as the admin sees it).
 *
 *  Disarm-before-dispatch: the returned section is already unarmed, so a page
 *  that assigns it before its first `await` has no confirm left for a double
 *  press — a second forced rotation would drop the very key the first minted. */
export function takeOauthConfirm(
  s: OauthSection,
  arm: IssuerForcedArm,
  working: string,
): { state: OauthSection; arm: IssuerForcedArm } | null {
  if (!s.armed || s.armed.arm !== arm || s.inFlight) return null;
  return { state: { ...s, armed: null, inFlight: true, status: working }, arm };
}

/** A call's end: its verdict and the key set re-read after it land TOGETHER
 *  (tui's single outcome), so the verdict never names a key the rows beside
 *  it do not show yet. A failed re-read keeps the verdict and blanks the rows. */
export function finishOauthCall(
  s: OauthSection,
  status: string,
  keys: OauthKeysRead,
): OauthSection {
  return { ...s, keys, status, inFlight: false };
}
