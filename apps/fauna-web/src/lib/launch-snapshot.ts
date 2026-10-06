// The launch snapshot's shape and its pure accessors — split out of
// `wasm-launch.ts` so they can be unit-tested (`launch-snapshot.test.ts`). That
// module initializes the launch wasm chunk and therefore imports `$app/paths`,
// which `deno test` cannot resolve outside the SvelteKit build; these functions
// touch no wasm and no SvelteKit at all. Same split, for the same reason, as
// `auth-errors.ts` out of `wasm.ts`.
//
// `wasm-launch.ts` re-exports everything here, so call sites keep importing
// from `$lib/wasm-launch` and nothing outside this file changed.

/** `{phase, token, last_error, superseded_successor, …}` per
 *  `fauna-launch-machine/src/snapshots.rs` — `snapshotJson()` serializes the
 *  WHOLE snapshot, so a field is readable here the moment it is declared.
 *  `phase` is the serde-tagged `LaunchPhase` — `"Boot"` / `"Hydrating"` /
 *  `"Online"` / `{ WizardAt: { entry: "AwaitingManualDns" } }` / … */
export interface LaunchSnapshot {
  phase: unknown;
  /** `TokenStatus` — not a bare string. Unused by the awaiting-DNS row. */
  token?: unknown;
  last_error?: string | null;
  /** The successor the `fauna.auth.superseded` refusal CLAIMED, 64-hex — an
   *  additive side channel rather than a `LaunchPhase` variant, so an app that
   *  never reads it still stops retrying (the phase is
   *  `Offline { transient: false }`). Read it through `supersededSuccessorOf`. */
  superseded_successor?: string | null;
  /** The saved account index is present and this build cannot use it
   *  (`version-compatibility.md` § 5 item 9) — an additive side channel on
   *  the same `superseded_successor` pattern above, checked before every
   *  other row (`onboarding.md` § App-launch routing). Serde-tagged: the
   *  `NewerBuild` verdict arrives as `{ NewerBuild: { index_v, index_min,
   *  bin_v } }`, the unit `Malformed` verdict as the bare string
   *  `"Malformed"`. Read it through `accountIndexRefusalOf`. */
  account_index_refusal?: unknown;
  /** A nest this app had signed in to before no longer signs the identity in
   *  (suspended or removed — deliberately unsaid) — an additive side channel on
   *  the `superseded_successor` pattern: the phase is still
   *  `Offline { transient: false }`, so the routing checks this BEFORE the
   *  generic `transient === false` "update needed" row (`onboarding.md`
   *  § App-launch routing → the previously-signed-in row). Unlike every other
   *  terminal surface, its page offers Retry. */
  sign_in_refused?: boolean;
}

/** `LaunchSnapshot.account_index_refusal`, normalized to one shape regardless
 *  of serde's struct-vs-unit-variant tagging difference. */
export type AccountIndexRefusal =
  | { kind: 'NewerBuild'; index_v: number; index_min: number; bin_v: number }
  | { kind: 'Malformed' };

/** The `WizardAt` entry the machine routed to, or `null` for any other phase.
 *  `phase` is serde-tagged, so `WizardAt` arrives as `{ WizardAt: { entry } }`. */
export function wizardEntryOf(snap: LaunchSnapshot): string | null {
  const phase = snap.phase as { WizardAt?: { entry?: string } } | string | undefined;
  if (!phase || typeof phase === 'string') return null;
  return phase.WizardAt?.entry ?? null;
}

/** The phase's variant NAME — `"Online"`, `"Offline"`, `"WizardAt"`,
 *  `"IdentityChanged"`, … Serde's external tagging makes a unit variant a bare
 *  string and a struct variant a single-key object, so one helper covers both.
 *
 *  NOTE the shape, not a `JSON.stringify` compare: a wasm binding that hands JS
 *  an enum "as JSON" yields a *quoted* string (`'"Online"'`), which no
 *  compare-by-name call site expects. Reading the parsed snapshot's key is the
 *  shape that can't rot. */
export function phaseNameOf(snap: LaunchSnapshot): string | null {
  const phase = snap.phase;
  if (typeof phase === 'string') return phase;
  if (phase && typeof phase === 'object') {
    const keys = Object.keys(phase as object);
    return keys[0] ?? null;
  }
  return null;
}

/** `Offline { transient }` → the flag; `null` for any other phase. Web routes
 *  `transient: true` to the retry surface and `transient: false` to the NON-retry
 *  "update required" surface (onboarding.md § App-launch routing, the nest-outdated
 *  row: clients route `transient: false` there, never to the retry CTA). */
export function offlineTransientOf(snap: LaunchSnapshot): boolean | null {
  const phase = snap.phase as { Offline?: { transient?: boolean } } | string | undefined;
  if (!phase || typeof phase === 'string') return null;
  return phase.Offline?.transient ?? null;
}

/** The successor a `fauna.auth.superseded` refusal named, or `null`. Present
 *  **only** in the succeeded-identity terminal state, so it is the discriminator
 *  the routing keys on — the phase alone cannot distinguish it from the
 *  nest-outdated row, both being `Offline { transient: false }` (deliberately:
 *  `snapshots.rs` explains why the successor rides a side channel instead of a
 *  new phase variant). Checked AHEAD of the generic `Offline` arms, exactly as
 *  tui's `launch.rs` and linux's `main.rs` check it.
 *
 *  ⚠ CLAIMED, not proven. What comes back is whatever the nest's refusal said;
 *  naming it to the user requires `resolveVerifiedSuccessor` first
 *  (`identity-succession.md` § Propagation → *Own device fleet*: the nest is
 *  enforcer and distributor, never authorizer). */
export function supersededSuccessorOf(snap: LaunchSnapshot): string | null {
  const claimed = snap.superseded_successor;
  return typeof claimed === 'string' && claimed !== '' ? claimed : null;
}

/** Which account-index verdict is active, or `null` (`version-compatibility.md`
 *  § 5 item 9). Checked AHEAD of `supersededSuccessorOf` and the generic
 *  `Offline` arms — the machine reads `LaunchPersistence::account_index_refusal`
 *  before it even attempts `load_identity`, so this outranks every other row
 *  (`onboarding.md` § App-launch routing). Mirrors tui's `route()` and linux's
 *  `main.rs` guard ordering. */
export function accountIndexRefusalOf(snap: LaunchSnapshot): AccountIndexRefusal | null {
  const raw = snap.account_index_refusal;
  if (raw === 'Malformed') return { kind: 'Malformed' };
  if (raw && typeof raw === 'object' && 'NewerBuild' in raw) {
    const nb = (raw as { NewerBuild?: { index_v?: number; index_min?: number; bin_v?: number } })
      .NewerBuild;
    return {
      kind: 'NewerBuild',
      index_v: nb?.index_v ?? 0,
      index_min: nb?.index_min ?? 0,
      bin_v: nb?.bin_v ?? 0,
    };
  }
  return null;
}

/** `IdentityChanged { pinned_hex, seen_hex }` → the fingerprints, else `null`.
 *  `seenHex === null` is the **withdrawn** case (the nest can no longer prove any
 *  identity for an origin we pinned — downgrade protection), as opposed to a
 *  changed one. Both block auto-entry (security.md § Transport trust). */
export function identityChangedOf(
  snap: LaunchSnapshot,
): { pinnedHex: string; seenHex: string | null } | null {
  const phase = snap.phase as
    | { IdentityChanged?: { pinned_hex?: string; seen_hex?: string | null } }
    | string
    | undefined;
  if (!phase || typeof phase === 'string') return null;
  const ic = phase.IdentityChanged;
  if (!ic) return null;
  return { pinnedHex: ic.pinned_hex ?? '', seenHex: ic.seen_hex ?? null };
}

/** `TokenStatus::Valid { expires_at_secs }` → unix seconds, else `null`. Paired
 *  with `currentBearer()` to prime the SPA's bearer cache from the machine's
 *  token, so the app doesn't re-mint one on its first request. */
export function tokenExpiryOf(snap: LaunchSnapshot): number | null {
  const token = snap.token as { Valid?: { expires_at_secs?: number } } | string | undefined;
  if (!token || typeof token === 'string') return null;
  return token.Valid?.expires_at_secs ?? null;
}
