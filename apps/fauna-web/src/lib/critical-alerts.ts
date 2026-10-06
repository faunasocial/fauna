// The cross-page critical-alerts store — web's rendering seam for the
// "something is very wrong" surface (`docs/goal/behavior/critical-alerts.md`;
// ui.yaml `global:` `critical-alerts`/`critical-alert[N]`, user-approved
// 2026-07-23). The web twin of `apps/fauna-linux/src/critical_alerts.rs` /
// `apps/fauna-tui/src/critical_alerts.rs`.
//
// A native app shares ONE process-wide `CriticalAlerts` registry across
// every page for free (one process, one address space). Web cannot: each
// lazy-loaded wasm chunk is a *separately compiled binary* with its own
// linear memory, so a feeder's registry (a Rust `Arc`) can only live inside
// the one chunk that hosts it — it cannot cross into another chunk or into
// this module. This file is the aggregation layer that gives web the same
// "one shell, every page" rendering behavior anyway: each chunk that hosts a
// feeder registers itself as a *source* once it loads (today: both
// `fauna-wasm-atproto-settings`, feeder #1's home, and the `fauna-wasm` core
// chunk, the session-start sweep's home since TRACK 4 — `critical-alerts.md`
// § Implementation status today), and this module merges every registered
// source's active alerts into one shared store the shell renders — no
// matter which page is currently mounted, and even after the page that
// loaded the source has been left (the source's wasm module stays imported
// for the rest of the app session, matching a native process's registry
// lifetime).

import { writable } from 'svelte/store';
import { registerActorScopedReset } from './actorScope';
import type { LocalizedText } from './i18n/localized';

/** One active alert, as `criticalAlertsActive()` returns it — mirrors
 *  `fauna_client_alerts::CriticalAlertRow`. */
export interface CriticalAlertRow {
  key: string;
  lines: LocalizedText[];
}

interface CriticalAlertsSource {
  subscribe(onChanged: () => void): void;
  active(): CriticalAlertRow[];
  clearAll(): void;
}

const sources: CriticalAlertsSource[] = [];

/** Merged active alerts across every wired source, deterministic order (by
 *  key — mirrors the shared registry's own `BTreeMap` order within a single
 *  source; stable across sources since there is only ever one today). Empty
 *  ⇒ the `critical-alerts` container is absent (ui.yaml presence rule). */
export const criticalAlerts = writable<CriticalAlertRow[]>([]);

function recompute(): void {
  const merged = sources.flatMap((s) => s.active());
  merged.sort((a, b) => a.key.localeCompare(b.key));
  criticalAlerts.set(merged);
}

/**
 * Wire one chunk's critical-alerts registry into the shared cross-page store.
 * Call once, from the module that loads that chunk (mirrors
 * `wasm-atproto-settings.ts`'s `ensureAtprotoSettingsWasm`) — a future
 * feeder living in a different chunk registers its own source the same way.
 */
export function wireCriticalAlertsSource(source: CriticalAlertsSource): void {
  sources.push(source);
  source.subscribe(recompute);
  recompute();
}

/**
 * Drop every active alert across every wired source — the identity-teardown
 * boundary (`critical-alerts.md` § Mechanism → *Lifetime*: sign-out, account
 * switch, nest-untrust, factory reset). Safe to call with no source wired yet
 * (nothing can have been posted).
 *
 * Registered as an actor-scoped reset (`actorScope.ts`) so it fires
 * automatically on sign-out, account switch, and the e2e test agent's session
 * patches — every identity change that goes through the `identity` store.
 * Factory reset is the one teardown that does NOT change `identity` (the
 * local credentials are deliberately kept for re-claim), so its own call site
 * (`admin/nest/+page.svelte`) calls this directly too.
 */
export function clearAllCriticalAlerts(): void {
  for (const s of sources) s.clearAll();
}

registerActorScopedReset(clearAllCriticalAlerts);
