import type { SharedRpcPort } from '../../static/fauna_wasm_atproto_settings.js';
import { base } from '$app/paths';
import type { AtprotoSettingsMachine } from '../../static/fauna_wasm_atproto_settings.js';
import type { AtprotoSettingsSnapshot } from './atproto-settings-machine';
import type { LocalizedText } from './i18n/localized';
import { wireCriticalAlertsSource, type CriticalAlertRow } from './critical-alerts';
import { hexToBytes } from './hex';
import { registerActorScopedReset } from './actorScope';
import { sharedAccountPort } from './account-runtime';

// Re-export so route/component files consume the machine type via
// `$lib/wasm-atproto-settings` rather than a fragile relative path into
// `static/` (mirrors `wasm-labeler-catalog.ts`).
export type { AtprotoSettingsMachine };

// Loader for the page-level Bluesky/ATProto login-plane settings WASM chunk
// (`libs/fauna-wasm-atproto-settings`: the `AtprotoSettingsMachine` — app
// credentials + connected-app sessions + the external-apps kill-switch). A
// separate wasm module from `fauna_wasm` (and from `fauna_wasm_labeler_
// catalog`) — same separate-chunk discipline: keep the constructor call
// inside the module that holds the singleton `wasmModule` reference.
//
// The atproto page builds its own `AtprotoSettingsMachine` over a
// short-lived WS-RPC connection, exactly as the labeler-catalog page builds
// its own — one shared surface, all 7 apps (priority #1/#2;
// docs/goal/behavior/atproto-pds-full.md § App surface).

let wasmModule: typeof import('../../static/fauna_wasm_atproto_settings.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the atproto-settings wasm chunk exactly once per page load. The
 *  init *promise* is memoized (same load-bearing shape as
 *  `wasm.ts::ensureWasm`): two concurrent callers await the SAME in-flight
 *  init, so `mod.default()` runs once — a second concurrent call would
 *  re-instantiate the chunk and reset wasm linear memory, wiping this
 *  chunk's critical-alerts registry (feeder #1) mid-session. A failed init
 *  drops the cached promise so a later call can retry. */
export function ensureAtprotoSettingsWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_atproto_settings.js');
      await mod.default(`${base}/fauna_wasm_atproto_settings_bg.wasm`);
      wasmModule = mod;
      // This chunk hosts feeder #1's critical-alerts registry (see the Rust
      // module doc in `libs/fauna-wasm-atproto-settings/src/lib.rs`) — wire it
      // into the shared shell store the moment the chunk loads, so an alert
      // posted while this page is open stays visible on every later page too.
      wireCriticalAlertsSource({
        subscribe: (onChanged) => mod.subscribeCriticalAlerts({ onChanged }),
        active: () => JSON.parse(mod.criticalAlertsActive()) as CriticalAlertRow[],
        clearAll: () => mod.clearAllCriticalAlerts(),
      });
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) {
    throw new Error('Atproto-settings WASM not initialized — call ensureAtprotoSettingsWasm() first');
  }
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh and every gesture's own internal refresh). */
export interface AtprotoSettingsMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `AtprotoSettingsMachine` over the SPA singleton's
 * socket — `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`; `secretHex`
 * is the actor's 32-byte ed25519 seed (signs the D10 delegation). The minted
 * credential secrets and the rotation-key custody rest on the account plane,
 * reached through the account port wired here before the first refresh
 * (account-client-lifecycle.md § The account port; D3). State starts
 * empty — the caller drives `refresh()`.
 */
export async function createAtprotoSettingsMachine(
  observer: AtprotoSettingsMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<AtprotoSettingsMachine> {
  await ensureAtprotoSettingsWasm();
  const machine = new (wasm().AtprotoSettingsMachine)(observer, port, hexToBytes(secretHex));
  // The ATProto identity custody door and the credential store, through the
  // account port (account-client-lifecycle.md § The account port): the held
  // senior rotation keys and their custody records
  // (`fauna.state.atproto-identity`) and the minted app-credential secrets
  // (`fauna.state.atproto`) are answered by this tab's account runtime in the
  // core chunk. With no runtime serving this account a custody read is
  // "cannot verify", no credential is revealable, and a write is refused;
  // `port` (`sharedRpcPort`) has loaded the core chunk.
  machine.setAccountPort(sharedAccountPort(secretHex));
  return machine;
}

let machinePromise: Promise<AtprotoSettingsMachine> | null = null;

/**
 * The single shared `AtprotoSettingsMachine` for the SPA, built once per
 * identity and reused across every mount of the AT Protocol settings page —
 * mirrors `feed.ts`'s `getFeedManager()` singleton shape exactly.
 *
 * WHY THIS MATTERS (not just an optimization): the shared machine's S4-C
 * custody check (`critical-alerts.md` feeder #1) debounces on a field that
 * lives INSIDE the machine instance (`custody_suspect` in
 * `fauna-atproto-settings-machine`'s `machine.rs`) — it alarms only on the
 * SECOND consecutive convergence that sees the same contradiction (a
 * sibling device's fresh re-mint key can look like a mismatch for one pass
 * before the `fauna.state.atproto-identity` plane syncs it; a real compromise survives into the next
 * convergence, per the doc comment on `check_custody`). Building a FRESH
 * machine on every page visit — the naive per-mount pattern — resets that
 * debounce every time, so two consecutive checks can never land on the same
 * instance and the alarm can never confirm. linux/tui don't hit this because
 * their settings page is built once and stays alive for the Settings
 * session; this singleton gives web the same shape.
 *
 * `observer` is captured only on the FIRST build for a given identity; a
 * later mount's observer is never wired in (the machine already has one).
 * Callers must therefore re-pull state explicitly after any `refresh()` /
 * gesture they trigger (`applySnapshot()` in `AtprotoSettingsSection.svelte`)
 * rather than relying solely on the observer firing into a possibly-dead
 * earlier mount — every gesture handler there already does this; only the
 * periodic refresh timer needed the same treatment.
 */
export function getAtprotoSettingsMachine(
  observer: AtprotoSettingsMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<AtprotoSettingsMachine> {
  if (machinePromise) return machinePromise;
  machinePromise = createAtprotoSettingsMachine(observer, port, secretHex);
  return machinePromise;
}

/**
 * The **pre-fetch** page state, straight from the shared Rust default
 * (`AtprotoSettingsSnapshot::default()`), for the window between mount and the
 * first `refresh()` resolving.
 *
 * Use this instead of a local literal. The default is not "all fields empty" —
 * five fields are deliberately non-zero, and `hosted_gate_reason` is a ratified
 * UI obligation (a closed gate must always say why, `ui/README.md` § Copy
 * comprehensibility rule 5). Every app that hand-rolled a stand-in for it
 * either drifted from the Rust default or shipped the inverse of the contract;
 * web's was a one-key `PENDING_GATE_REASON` const that covered exactly that one
 * field. Requires the chunk to be loaded (`ensureAtprotoSettingsWasm()` first).
 */
export function atprotoSettingsPrefetchSnapshot(): AtprotoSettingsSnapshot {
  return JSON.parse(wasm().atprotoSettingsPrefetchSnapshot()) as AtprotoSettingsSnapshot;
}

/** The D10 delegation row's granted capabilities in user voice, one per wire
 *  spelling in cert order (`atproto-delegation-scope`).
 *
 *  Goes through the wasm face rather than a local map on purpose: the snapshot
 *  carries `capabilities` as WIRE spellings and the wire→i18n-key mapping is
 *  shared Rust (`fauna_atproto_settings_machine::delegation_capability_label`),
 *  which tui and linux already call directly. A TS copy here would be the
 *  fourth, and the fourth copy is where they start disagreeing (priority #4).
 *  An unrecognized capability comes back as its own wire form rather than
 *  vanishing — dropping one would understate a grant. */
export function delegationCapabilityLabels(capabilities: string[]): LocalizedText[] {
  return JSON.parse(wasm().delegationCapabilityLabels(capabilities)) as LocalizedText[];
}

/** The D10 delegation row's liveness in user voice
 *  (`atproto-delegation-status`'s prose). Same shared-map reasoning as
 *  `delegationCapabilityLabels`. ⚠ The WIRE spelling, not this text, is what an
 *  e2e asserts — it rides the leaf's `state` attr. */
export function delegationStatusLabel(liveness: string): LocalizedText {
  return JSON.parse(wasm().delegationStatusLabel(liveness)) as LocalizedText;
}

/** The identity summary's status in user voice (`atproto-hosted-handle`'s
 *  status word) — the one shared reading tui and linux call in-process. An
 *  unrecognized status degrades to its wire word, never blanks. */
export function identityStatusLabel(status: string): LocalizedText {
  return JSON.parse(wasm().identityStatusLabel(status)) as LocalizedText;
}

/** Drop the singleton (identity change — sign-out/switch). Without it a
 *  later identity would keep reading/gesturing against the FORMER identity's
 *  machine (wrong secret, wrong actor, wrong observer). */
function resetAtprotoSettingsMachine(): void {
  machinePromise = null;
}

registerActorScopedReset(resetAtprotoSettingsMachine);

/** Test-only: point this chunk's genesis-seniority custody check (feeder #1,
 *  `critical-alerts.md`) at a fake PLC directory — the wasm twin of native's
 *  `FAUNA_ATPROTO_PLC_DIRECTORY_URL` env var (a browser has no process
 *  environment). Installed as `window.__fauna_enableFakePlcDirectoryForTest`
 *  (see `e2e-automation.ts`); called only from the Playwright e2e bridge,
 *  never by production code.
 *
 *  The export exists only in the `wasm-atproto-settings-test` chunk flavor
 *  (gated on this crate's `test-helpers` feature — testing.md § convention
 *  15, mirrors `$lib/wasm`'s `enableDnsFakeProviderForTest`), so it is
 *  reached off the module record rather than the generated type, and THROWS
 *  on a production bundle instead of failing silently (convention 11). */
export async function enableFakePlcDirectoryForTest(url: string): Promise<void> {
  await ensureAtprotoSettingsWasm();
  const fn = (wasm() as unknown as Record<string, unknown>).enableFakePlcDirectoryForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'enableFakePlcDirectoryForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as (url: string) => void)(url);
}

/** Test-only: move the D10 delegation row's RENDER clock, so `expiring_soon` /
 *  `expired` are reachable without waiting out the real ~90-day window
 *  (testing.md § convention 14 — a fake clock, never a sleep). The wasm twin of
 *  native's `atproto_delegation_advance_clock`; driven by
 *  `$lib/atproto-delegation-e2e`.
 *
 *  ⚠ Never the MINT clock: `authorizeExternalApps` always stamps a fresh cert
 *  with the real wall clock, so an offset left behind lapses the very next
 *  delegation this page mints. Pass `0` to reset — the offset is module-wide
 *  and nothing auto-clears it.
 *
 *  Same two-flavor discipline as `enableFakePlcDirectoryForTest` above: the
 *  export exists only in the `wasm-atproto-settings-test` chunk, so it is
 *  reached off the module record and THROWS on a production bundle rather than
 *  no-op'ing (convention 11). */
export async function atprotoDelegationSetClockOffsetForTest(offsetSecs: number): Promise<void> {
  await ensureAtprotoSettingsWasm();
  const fn = (wasm() as unknown as Record<string, unknown>)
    .atprotoDelegationSetClockOffsetForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'atprotoDelegationSetClockOffsetForTest is absent: this SPA is running the ' +
        'PRODUCTION wasm flavor, which compiles the e2e seams out. Build with ' +
        '`just web-test`.',
    );
  }
  (fn as (offsetSecs: number) => void)(offsetSecs);
}
