import { base } from '$app/paths';
import type { WsRpcClient, WasmUploadPayload } from '../../static/fauna_wasm.js';
import type { LocalizedText } from '$lib/i18n/localized';
// Type-only (erased at build, so no runtime cycle with rpc.ts, which imports this
// module): `ObligationAction` is the `fauna.moderation.actions` wire row and stays
// owned by its rpc module — the merge below takes it as the queue's server half.
import type {
  AdminUser,
  ObligationAction,
  EmailFilter,
  BackupDestination,
  MutedKeyword,
} from '$lib/rpc';
import type {
  IssuerForcedArm,
  IssuerForcedConfirmView,
  IssuerKeyRow,
  IssuerKeyView,
} from '$lib/admin-oauth-keys';
import type { MdDecoration, MdRevealRange, ComposeMarkerPlan } from '$lib/markdown-decorations';
import type { Affordance } from '$lib/offline-gate';
import type { AskRules, FeedRequestState } from '$lib/ward-asks';
import type {
  Block,
  BlockCaret,
  BlockEdit,
  BlockId,
  GestureResult,
  NoteDocument,
  StructuralGesture,
  WasmNoteLine,
} from '$lib/notes';
import type { RenderBlock, RenderDocument, QuotedPostEmbed, ResolvedLinkPreview } from '$lib/document';
// Type-only (erased at build, so no runtime cycle with task-delegation.ts,
// which imports this module) — see the `$lib/rpc` note above.
import type { RunnerStatus, PinOption } from '$lib/task-delegation';
import { classifyChallengeError } from './auth-errors';
import { wireCriticalAlertsSource, type CriticalAlertRow } from './critical-alerts';

// Re-export the typed silent-challenge errors + classifier so existing call
// sites keep importing them from `$lib/wasm`.
export { classifyChallengeError, NestOutdatedError, TransientAuthError, NestIdentityChangedError } from './auth-errors';

let wasmModule: typeof import('../../static/fauna_wasm.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the wasm module exactly once per page load. The init *promise* is
 *  memoized (not just the result): two concurrent callers `await` the SAME
 *  in-flight init, so `mod.default()` runs once. Without this, a second
 *  concurrent call would re-run `mod.default()` and reset wasm linear memory —
 *  wiping module statics like `fauna_log`'s ring (and any other shared-crate
 *  global) that a peer caller had already seeded. A hard reload re-evaluates the
 *  module → both refs reset → init runs again. */
export function ensureWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm.js');
      await mod.default(`${base}/fauna_wasm_bg.wasm`);
      wasmModule = mod;
      // This chunk hosts the session-start critical-alert sweep's registry
      // (`src/critical_alerts.rs` — `runCriticalAlertSweep` in `src/rpc.rs`
      // posts to it) — wire it into the shared shell store the moment the
      // core chunk loads, exactly as `wasm-atproto-settings.ts` wires
      // feeder #1's chunk.
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
  if (!wasmModule) throw new Error('WASM not initialized — call ensureWasm() first');
  return wasmModule;
}

/** The post-claim serving-enablement step's progress — the web read of
 *  `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`, parsed from the shared Rust
 *  derivation (`serving_enablement_json`), never re-built here. The step runs
 *  inside this core chunk (`applyServingEnablement` in `$lib/rpc`), so before
 *  the chunk loads it cannot have run: the empty run list is the honest answer. */
export type ServingEnablement = {
  started: number;
  completed: number;
  runs: {
    actor_id: string;
    decided: { email: boolean; caldav: boolean; carddav: boolean; webdav: boolean };
    completed: boolean;
  }[];
};
export function servingEnablement(): ServingEnablement {
  if (!wasmModule) return { started: 0, completed: 0, runs: [] };
  return JSON.parse(wasmModule.servingEnablementJson()) as ServingEnablement;
}

/** This tab's account runtime — the faces over `fauna-wasm`'s
 *  `account_runtime` module, the one runtime this tab hosts
 *  (`$lib/account-runtime` owns its lifecycle). Every read answers "none"
 *  while the core chunk is not loaded: no runtime can run before it. */
export async function accountRuntimeShutdown(): Promise<void> {
  if (!wasmModule) return;
  await wasmModule.accountRuntimeShutdown();
}
/** The sign-out stop: retire this browser's enrollment, then shut the runtime
 *  down (`AccountStoreHandle::shutdown_for_sign_out`). A no-op when none runs. */
export async function accountRuntimeShutdownForSignOut(): Promise<void> {
  if (!wasmModule) return;
  await wasmModule.accountRuntimeShutdownForSignOut();
}
/** The hosts' one stop budget (`ACCOUNT_RUNTIME_STOP_BUDGET`), in ms — never
 *  re-spelled here. `0` before the core chunk loads: no runtime can run then,
 *  so there is nothing to wait for. */
export function accountRuntimeStopBudgetMs(): number {
  return wasmModule?.accountRuntimeStopBudgetMs() ?? 0;
}
/** The sign-out record's faces over `fauna-wasm`'s `account_scope`
 *  (`$lib/accounts` owns their use). Record the sign-out for every account the
 *  registry names plus `signedInActor` — synchronous, so the gesture writes it
 *  before its first await; `false` when the core chunk is not loaded yet. */
export function signOutRecordBegin(signedInActor: string | undefined): boolean {
  if (!wasmModule) return false;
  wasmModule.signOutRecordBegin(signedInActor);
  return true;
}
/** Whether a confirmed sign-out's credential wipe is still owed. `false` before
 *  the core chunk loads, where the identity read answers "none" anyway. */
export function signOutWipeOwed(): boolean {
  return wasmModule?.signOutWipeOwed() ?? false;
}
/** The accounts whose store {@link signOutFinishRecorded} would erase right
 *  now — what a load puts to the other-tab probe before it sweeps. */
export async function signOutPlannedErase(): Promise<string[]> {
  await ensureWasm();
  return wasm().signOutPlannedErase();
}
/** What a finished sign-out record left: whether one was found, and the
 *  shared residue line when account stores are still in this browser. */
export interface SignOutFinish {
  found: boolean;
  residue?: LocalizedText;
}
/** Do what the sign-out record owes — the credential wipe if it has not run,
 *  then each recorded account's store erase, bounded. `eraseRefused` is the
 *  caller's other-tab probe over {@link signOutPlannedErase}: when set, no
 *  store is erased and every planned account stays recorded. */
export async function signOutFinishRecorded(eraseRefused: boolean): Promise<SignOutFinish> {
  await ensureWasm();
  return (await wasm().signOutFinish(eraseRefused)) as SignOutFinish;
}
/** The standing refusal of this browser's enrollment — the one rendered
 *  sentence (`EnrollmentRefusal::notice`) — or `null`. */
export async function accountEnrollmentNotice(): Promise<string | null> {
  if (!wasmModule) return null;
  return (await wasmModule.accountEnrollmentNotice()) ?? null;
}
/** The store-change notice (`$lib/store-change` is its one caller): resolves
 *  with the notice count once it differs from `seen`, `undefined` when this
 *  tab hosts no runtime or the runtime stopped while waiting. Never rejects. */
export async function accountStoreChangedAfter(seen: number): Promise<number | undefined> {
  if (!wasmModule) return undefined;
  return (await wasmModule.accountStoreChangedAfter(seen)) ?? undefined;
}
/** The `sync_devices` row this browser's enrollment registered on, or `null`. */
export async function accountEnrolledDeviceRow(): Promise<string | null> {
  if (!wasmModule) return null;
  return (await wasmModule.accountEnrolledDeviceRow()) ?? null;
}
/** The shared `PumpCyclesView` — `fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`'s
 *  body; `runtime: false` before the chunk loads, as while no runtime runs. */
export type AccountPumpCycles = { started: number; completed: number; runtime: boolean; holder: boolean };
export function accountPumpCycles(): AccountPumpCycles {
  if (!wasmModule) return { started: 0, completed: 0, runtime: false, holder: false };
  return JSON.parse(wasmModule.accountPumpCyclesJson()) as AccountPumpCycles;
}
/** Test-only: the `device_set_state` command's body — this browser's own
 *  account-runtime read of one device's `fauna.state.device-set` plane row, the
 *  shared reader every hosting app answers with. The export exists only in the
 *  `wasm-core-test` flavor (`libs/fauna-wasm`'s `test-helpers` feature —
 *  testing.md § convention 15), so it is reached off the module record and
 *  THROWS on a production bundle instead of failing silently (convention 11). */
export async function accountDeviceSetState(deviceIdHex: string): Promise<unknown> {
  await ensureWasm();
  const fn = (wasm() as unknown as Record<string, unknown>).accountDeviceSetStateJsonForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'accountDeviceSetStateJsonForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  return JSON.parse(await (fn as (id: string) => Promise<string>)(deviceIdHex));
}
/** One full account-pump pass now (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`). */
export async function accountPumpNow(): Promise<void> {
  if (!wasmModule) return;
  await wasmModule.accountPumpNow();
}
/** One door of a consumer seam for `actorIdHex`'s account — the core chunk's
 *  half of the account port (`accountPortCall`; `$lib/account-runtime`'s
 *  `sharedAccountPort` is the one caller). Rejects while the core chunk is not
 *  loaded, which the calling chunk reads as a port fault: no runtime can run
 *  before it. */
export async function accountPortCall(
  actorIdHex: string,
  door: string,
  payload: Uint8Array,
): Promise<Uint8Array> {
  if (!wasmModule) throw new Error('account port: the core wasm chunk is not loaded');
  return wasmModule.accountPortCall(actorIdHex, door, payload);
}

/** The initialized core wasm module, for a feature-gated glue module that must
 *  live OUTSIDE this file. The one caller today is `$lib/payments`: this module
 *  is unconditionally in the bundle, so a face wrapper defined here ships the
 *  wasm export's NAME into the store-safe artifact even with every caller folded
 *  away, and those names are the web column's criterion-2 axis
 *  (`dynamic-features.md` § Platform-family surface excision — the
 *  isolated-module pattern). Every ungated face belongs in this file; reach for
 *  this only when the face's own name must be absent from an excised flavor. */
export function wasmCoreModule() {
  return wasm();
}

/**
 * Construct the browser WS-RPC client (the Phase-4 façade over
 * `fauna_rpc_wasm::WsRpcClient`). The singleton lifecycle is owned by
 * `rpc.ts`; this just hands back a freshly-connected handle. `tokenProvider`
 * is `(forceRefresh) => Promise<bearer>` (called on connect and on a 4401).
 */
export async function createWsRpcClient(
  node: string,
  actorIdHex: string,
  tokenProvider: (forceRefresh: boolean) => Promise<string>,
): Promise<WsRpcClient> {
  await ensureWasm();
  return new (wasm().WsRpcClient)(node, actorIdHex, tokenProvider);
}

/** Which client surfaces a push has made stale — the wasm/TS twin of
 *  `fauna_protocol::StaleSurfaces` (`transport.md` § Which surfaces a push
 *  invalidates). Logical surfaces, not this app's widget tree: a page folds
 *  them onto whatever it actually re-reads, and a page with no Media view
 *  simply ignores that flag. */
export interface StaleSurfaces {
  feed: boolean;
  notifications: boolean;
  knocks: boolean;
  contacts: boolean;
  account: boolean;
  atproto: boolean;
  events: boolean;
  /** The Contacts page's Address Book segment — set by
   *  `fauna.addressbook.changed`. Page-gated like `media` (one push per card
   *  write). Web's Address Book does not consume it yet. */
  address_book: boolean;
  media: boolean;
  /** The ward's supervision read (`fauna.family.status`) — reconnect-only,
   *  like `feed`: no push feeds it. Web serves it from the root layout's own
   *  connection-status effect rather than from this flag, and that effect's
   *  successful read moves every client-enforced input — the indicator, the
   *  content floor, `content_notify`, screen time — through the reply's
   *  `supervision` fold. */
  family: boolean;
  /** The Spam page's training history — set by
   *  `fauna.bridges.push.spam_model_updated` / `…_reset` (an undo or a reset on
   *  another device). Page-gated like `address_book` (one push per lesson).
   *  Web's Spam page does not consume it yet. */
  mail_spam: boolean;
}

/** Which client surfaces a push `kind` has made stale
 *  (`fauna_protocol::StaleSurfaces::for_kind`) — the shared classifier every
 *  page's push handler should check instead of hand-matching kind strings, so
 *  a kind the seam later grows a surface for reaches every page without a
 *  per-page edit. An unrecognized kind answers all-`false`. */
export function staleSurfacesForPushKind(kind: string): StaleSurfaces {
  return wasm().WsRpcClient.staleSurfacesForPushKind(kind) as StaleSurfaces;
}

/** The full reconnect sweep (`StaleSurfaces::on_reconnect()`) — every surface
 *  a dropped push might have staled, since a reconnect resets the push `seq`
 *  to 0. Call once from the reconnect arm instead of assuming every page's own
 *  surface is covered. */
export function staleSurfacesOnReconnect(): StaleSurfaces {
  return wasm().WsRpcClient.staleSurfacesOnReconnect() as StaleSurfaces;
}

export function generateKeypair(): string {
  return wasm().generate_keypair();
}

/** Test-only: enable the wasm fake DNS provider (the wasm twin of native's
 *  `FAUNA_DNS_PROVIDER_FAKE`). After this, a credentialed `DnsManagementMachine`
 *  recognizes the `fake-dns-ok:<zone>` sentinel token offline. Installed as
 *  `window.__fauna_enableDnsFakeProviderForTest` (see `+layout.svelte`); called
 *  only from the Playwright e2e bridge, never by production code.
 *
 *  The export exists only in the `wasm-core-test` chunk flavor (gated on
 *  `libs/fauna-wasm`'s `test-helpers` feature — testing.md § convention 15), so it
 *  is reached off the module record rather than the generated type, and THROWS on
 *  a production bundle instead of failing silently (convention 11). */
export function enableDnsFakeProviderForTest(): void {
  const fn = (wasm() as unknown as Record<string, unknown>).enableDnsFakeProviderForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'enableDnsFakeProviderForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as () => void)();
}

/** Test-only: point *this core chunk's* directory-backed feeders (the
 *  session-start sweep, `runCriticalAlertSweep` below) at a fake PLC
 *  directory — this chunk's own copy of the wasm twin of native's
 *  `FAUNA_ATPROTO_PLC_DIRECTORY_URL` env var. `wasm-atproto-settings.ts`
 *  exports the identically-named hook for feeder #1's settings-page path,
 *  but each wasm chunk is a separately compiled binary with its own copy of
 *  the directory override (`src/critical_alerts.rs`'s doc comment has the
 *  full module-boundary account) — `e2e-automation.ts` calls both. Gated on
 *  `libs/fauna-wasm`'s `test-helpers` feature, so it THROWS on a production
 *  bundle instead of failing silently (convention 11), same as
 *  `enableDnsFakeProviderForTest` above. */
export async function enableFakePlcDirectoryForTestCore(url: string): Promise<void> {
  await ensureWasm();
  const fn = (wasm() as unknown as Record<string, unknown>).enableFakePlcDirectoryForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'enableFakePlcDirectoryForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as (url: string) => void)(url);
}

/** The critical-alert sweep's `[started, completed]` pass counters — convention
 *  14's causal barrier for the sweep's negative asserts
 *  (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`).
 *
 *  Reads *this core chunk's* registry, which is the one the sweep posts to;
 *  `wasm-atproto-settings.ts`'s chunk has its own registry whose counters never
 *  move, so this is deliberately not aggregated the way `criticalAlertsActive`
 *  is (`src/critical_alerts.rs`'s export carries the full account).
 *
 *  Returns `[0, 0]` before the core chunk has loaded: the sweep cannot have run
 *  yet either, so a waiter simply keeps waiting rather than reading a stale
 *  verdict — and never throws into the agent's state assembly. */
export function criticalAlertSweepPasses(): [number, number] {
  if (!wasmModule) return [0, 0];
  const fn = (wasm() as unknown as Record<string, unknown>).criticalAlertSweepPasses;
  if (typeof fn !== 'function') return [0, 0];
  const pair = (fn as () => BigUint64Array | number[])();
  return [Number(pair[0] ?? 0), Number(pair[1] ?? 0)];
}

/** A test-bundle export of the core chunk, or `null` where there is none (the
 *  chunk not loaded yet, or a production bundle). */
function coreTestExport(name: string): ((...args: never[]) => unknown) | null {
  if (!wasmModule) return null;
  const fn = (wasm() as unknown as Record<string, unknown>)[name];
  return typeof fn === 'function' ? (fn as (...args: never[]) => unknown) : null;
}

/** Test-only: end the current identity's critical-alert re-sweep wait now —
 *  the web leg of `fauna_e2e_agent::ALERT_SWEEP_WAKE`, raced against the
 *  production loop's six-hour wait inside `runCriticalAlertSweep`. `false` when
 *  no loop is live for an identity, which the command refuses loudly. Throws on
 *  a production bundle (convention 11). */
export function alertSweepWakeForTest(): boolean {
  const fn = coreTestExport('alertSweepWakeForTest');
  if (!fn) {
    throw new Error(
      'alertSweepWakeForTest is absent: the core wasm chunk is not loaded, or this ' +
        'SPA is running the PRODUCTION flavor, which compiles the e2e seams out.',
    );
  }
  return (fn as () => boolean)();
}

/** The `connection_reports` observable (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`),
 *  counted inside the transport's reconnect loop — or `null` where this bundle
 *  has no leg, which the journey refuses loudly rather than reading as zero. */
export function connectionReportsForTest(): unknown {
  const fn = coreTestExport('connectionReportsForTest');
  return fn ? JSON.parse((fn as () => string)()) : null;
}

/** Record one painted frame's candidate error surfaces (`ids[i]` showed
 *  `texts[i]`) in the shared tally — see `$lib/e2e-painted-errors`. A no-op
 *  where the bundle has no leg (the observable then reads `null`). */
export function paintedErrorsObserveForTest(ids: string[], texts: string[]): void {
  const fn = coreTestExport('paintedErrorsObserveForTest');
  if (fn) (fn as (ids: string[], texts: string[]) => void)(ids, texts);
}

/** The `painted_errors` observable (`fauna_e2e_agent::PAINTED_ERRORS_KEY`), or
 *  `null` where this bundle has no leg. */
export function paintedErrorsForTest(): unknown {
  const fn = coreTestExport('paintedErrorsForTest');
  return fn ? JSON.parse((fn as () => string)()) : null;
}

export function actorIdFromSecret(secretHex: string): string {
  return wasm().actor_id_from_secret(secretHex);
}

/**
 * One snapshot file's **decrypted** bytes, via the shared client-side walk
 * (`fauna_core::file_download`, over wasm): manifest + chunks fetched by content
 * address, opened under the owner `BackupKey` derived from `secretHex`,
 * reassembled, and verified against the manifest's whole-file address.
 *
 * This is the only mechanism that can read a **sealed** snapshot file — owner
 * chunks are sealed unconditionally and the nest holds no opening key, so the
 * nest cannot reassemble them.
 * The caller saves the bytes as a browser blob download; that save is the one
 * platform-specific leg (`ui/backups.md` § Where logic lives).
 *
 * `manifestHash` takes the `SnapshotFile.manifest_hash` wire value in either
 * shape the decoder produces (`Uint8Array` or `number[]`).
 */
export async function downloadSnapshotFileBytes(
  manifestHash: Uint8Array | number[],
  relativePath: string,
  secretHex: string,
  nestUrl: string,
): Promise<Uint8Array> {
  await ensureWasm();
  return wasm().downloadSnapshotFileBytes(
    new Uint8Array(manifestHash),
    relativePath,
    secretHex,
    nestUrl,
  );
}

// ── Logging (observability.md § Surfaces) ──────────────
//
// Ring-only on web (no on-disk file — § Persistence & privacy — Web): the wasm
// `installLogging` installs the shared `fauna_log::RingLayer` + the browser
// console at SPA boot, and the Settings → Logs page reads the ring via these
// snapshot fns. The same `LogEntry` shape crosses from the admin `adminLogs`
// RPC (rpc.ts), so one `LogsView` component renders both surfaces.

/** A captured log line — the JS shape of `fauna_log::LogEntry`. `level` is
 *  lowercase `"error" | "warn" | "info" | "debug" | "trace"`. */
export interface LogEntry {
  timestamp_ms: number;
  level: string;
  target: string;
  message: string;
}

/** One rendered `log-entry` row — the JS shape of `fauna_log::format::LogRow`.
 *  `line` is the one-line `LEVEL · HH:MM:SS · target · message` copy form,
 *  `message` the row title, `subtitle` the secondary `LEVEL · time · target`
 *  line. The shared shape every Logs list binds (linux's title-over-subtitle
 *  row, the WinUI/SwiftUI/Compose/Svelte two-line row). */
export interface LogRow {
  line: string;
  message: string;
  subtitle: string;
}

/** Install the web app's ring-only `tracing` subscriber (idempotent). Call
 *  once at SPA boot, after `ensureWasm()`. Prefer `ensureLogging()` from app
 *  code — it memoizes this and is awaitable, so a ring read never races the
 *  async install. */
export function installLogging(): void {
  wasm().installLogging();
}

let loggingReady: Promise<void> | null = null;

/** Install the ring-only logging subscriber **exactly once per page load**
 *  (memoized) and resolve when the ring is seeded. Call at SPA boot AND `await`
 *  it before reading the ring (`logSnapshot`), so the read never races the
 *  async `ensureWasm()` + install (the seed line lands first). A hard reload
 *  re-evaluates the module → the promise resets → installs again. */
export function ensureLogging(): Promise<void> {
  if (!loggingReady) {
    loggingReady = (async () => {
      await ensureWasm();
      installLogging();
    })();
  }
  return loggingReady;
}

/** Every retained log line, oldest-first — the "All" filter source. */
export function logSnapshot(): LogEntry[] {
  return wasm().logSnapshot() as LogEntry[];
}

/** Log lines at or above `minLevel` (`"error"|…|"trace"`; `"all"`/unknown ⇒
 *  all), oldest-first. The shared `LogsView` filters client-side, but the full
 *  ring API is exposed for parity with the native FFI / linux reference. */
export function logSnapshotAtLeast(minLevel: string): LogEntry[] {
  return wasm().logSnapshotAtLeast(minLevel) as LogEntry[];
}

/** Drop all retained entries (the Settings → Logs page Clear affordance). */
export function logClear(): void {
  wasm().logClear();
}

// ── Shared Logs presentation (`fauna_log::format`) over wasm ──────────
//
// `LogsView` renders both Logs surfaces through these instead of
// re-implementing the severity filter / line+row form in TypeScript (it was the
// web "twin of `logs_view.rs`"). `tzOffsetSecs` is the browser's current local
// UTC offset in seconds (via the one `$lib/utcOffset` door), so the shared formatter
// renders local `HH:MM:SS` with no timezone lib. Entries go in oldest-first (as
// the ring / admin RPC return them); `logRows` / `logRenderedText` emit
// newest-first.

/** `entries` narrowed to those at or above `minLevel` in severity, order
 *  preserved (`"all"`/unknown ⇒ everything) — the in-memory filter for both
 *  Logs surfaces (`fauna_log::format::filter_entries`). */
export function logFilterEntries(entries: LogEntry[], minLevel: string): LogEntry[] {
  return wasm().logFilterEntries(entries, minLevel) as LogEntry[];
}

/** The rendered `log-entry` rows **newest-first** — `{ line, message, subtitle }`
 *  each (`fauna_log::format::rows`). */
export function logRows(entries: LogEntry[], tzOffsetSecs: number): LogRow[] {
  return wasm().logRows(entries, tzOffsetSecs) as LogRow[];
}

/** The entries joined **newest-first** into one block — the copy payload
 *  (`fauna_log::format::rendered_text`). */
export function logRenderedText(entries: LogEntry[], tzOffsetSecs: number): string {
  return wasm().logRenderedText(entries, tzOffsetSecs);
}

/** Emit one log line into the shared `fauna_log` ring from TypeScript — the WASM
 *  `logMessage`, the web twin of the native `log_message` (observability.md
 *  § The emit API). The non-Rust shell has no `tracing`, so this is how web's
 *  three capture categories (§ What must be logged) reach the same ring the
 *  Settings → Logs page reads: the `MessageBanner` display funnel, each
 *  `console.*` print, and each meaningful swallowed error.
 *
 *  `level` is `"error" | "warn" | "info" | "debug" | "trace"` (unknown ⇒ info);
 *  `target` is the source shown in the Logs target column (e.g.
 *  `"fauna_web::conversations"`). Per the redaction rule (§ Persistence &
 *  privacy) pass levels/targets/operation names/error metadata only — never
 *  message plaintext or secrets.
 *
 *  **Non-throwing by design.** Producers call this from `catch` blocks and
 *  cleanup paths, and a banner can change before SPA boot finishes the async
 *  `ensureLogging()` install — a throw here would mask the very error being
 *  logged. If the ring isn't installed yet the line is dropped (the visible
 *  banner / console still covers the active task; the ring is the durable
 *  record, and boot capture is reliable once `ensureLogging()` resolves). */
export function logMessage(level: string, target: string, message: string): void {
  try {
    wasmModule?.logMessage(level, target, message);
  } catch {
    /* ring not installed yet (pre-boot) — drop; the banner/console still shows it */
  }
}

// ── mail-add-credential password/token helpers ──────────────
//
// Thin wrappers over `fauna_client_mail_settings::password_gen` (bound in
// `libs/fauna-wasm/src/mail_admin.rs`); the WASM twin of the native FFI exports
// the other apps use. The `mail-add-credential` dialog mints the credential
// secret + decides the manual-password warning from shared Rust (priority #2) —
// not by re-deriving the charset / token size / warning rule in TS. Callers run
// after `ensureWasm()` (MailSettingsSection hydrates the machine first).

/** Fresh OAUTHBEARER token (32 random bytes → 64 lowercase hex chars). */
export function generateBridgeToken(): string {
  return wasm().generateBridgeToken();
}

/** Whether to show the manual-password warning: auto-generate OFF + encrypted nest. */
export function warnManualBridgePassword(autoGenerate: boolean, nestEncrypted: boolean): boolean {
  return wasm().warnManualBridgePassword(autoGenerate, nestEncrypted);
}

/**
 * Mint-once sequencing (mail-credentials.md § Auto-generated bridge password):
 * the PLAIN-form password field at a settled toggle edge (kind-select → PLAIN,
 * or the auto-generate toggle) — a freshly-minted secret when `kind` is
 * `"Plain"` and `autoGenerate` is on, `null` otherwise (including
 * `"OAuthBearer"`). Call ONLY at that settled edge, never again at submit —
 * store the returned value and submit it verbatim.
 */
export function resolveAutogeneratedBridgePassword(kind: string, autoGenerate: boolean): string | null {
  return wasm().resolveAutogeneratedBridgePassword(kind, autoGenerate);
}

/**
 * Advisory `mail-add-credential-password-strength-meter` label — a `LocalizedText`
 * (`settings.mail.strength_{weak,fair,strong}`) the caller resolves via
 * `resolveLocalized`, or `null` for an empty password. Length-only, canonical
 * `<8` Weak / `<16` Fair / `≥16` Strong (the shared reference threshold); the
 * wasm twin of `fauna_client_mail_settings::password_gen::password_strength_label`,
 * so web stops hand-rolling its own raw-English meter (priority #2).
 */
export function passwordStrengthLabel(password: string): LocalizedText | null {
  return wasm().passwordStrengthLabel(password) as LocalizedText | null;
}

// The §4 provider form's two faces (`paymentsKnownKinds`, `paymentsWebhookUrl`)
// MOVED to `$lib/payments` with the rest of the payments glue — see
// `wasmCoreModule` above for why a face whose name must excise cannot be
// wrapped in this file.

/**
 * Substitute the logged-in `handle` into a credential's `mua_username`
 * (`MailCredentialSummary.mua_username` — the concrete `<handle>+<id>@<domain>`
 * username with only `{handle}` left to fill; the `+<id>` suffix / `default`→bare
 * rule / domain are already resolved in shared Rust). The wasm twin of native's
 * `resolve_mua_username` UniFFI export, so every app performs the identical
 * substitution (priority #2) rather than re-deriving the bug-prone suffix in TS.
 */
export function resolveMuaUsername(muaUsername: string, handle: string): string {
  return wasm().resolveMuaUsername(muaUsername, handle);
}

export function getRecipients(payload: Uint8Array): string {
  return wasm().get_recipients(payload);
}

/** One editable label+uri row of the profile edit form. */
export interface ProfileLinkInput {
  label: string;
  uri: string;
}

/** The three editable display fields projected from a stored profile (the
 *  populate-the-form base; non-display fields are preserved Rust-side). */
export interface ProfileDisplay {
  display_name: string | null;
  bio: string | null;
  links: ProfileLinkInput[];
  /** Hex blob hash of the stored avatar, or null when the profile has none.
   *  Fetch the bytes over the ordinary blob download path. */
  avatar_hash_hex: string | null;
  /** Hex blob hash of the stored banner, or null. */
  banner_hash_hex: string | null;
}

/** Decode a stored profile body (from `profileGet`) to its editable display
 *  fields — the read half of the read-modify-write (shared
 *  `fauna_client_profile::decode_profile_display`). */
export function decodeProfileDisplay(body: Uint8Array): ProfileDisplay {
  return JSON.parse(wasm().decode_profile_display(body)) as ProfileDisplay;
}

/** Where a knock sent from `actorIdHex`'s profile page goes — `inboxSend`'s
 *  `recipient_nest_url`, `null` = this nest. `profileBody` is the stored
 *  profile the page's open already fetched (`profileGet`); `ownNestUrl` is
 *  this box's nest base URL. The rule is the shared
 *  `fauna_client_profile::knock_recipient_nest_url` (`profile.md` § Where
 *  logic lives → *Request contact routing*) — never re-derived here. */
export function knockRecipientNestUrl(
  actorIdHex: string,
  profileBody: Uint8Array,
  ownNestUrl: string,
): string | null {
  return wasm().knockRecipientNestUrl(actorIdHex, profileBody, ownNestUrl) ?? null;
}

/** The shared ward-ask matching rules (`fauna_client_family::ward_asks`) over
 *  their wasm faces — what `$lib/ward-asks`' render helpers take as their
 *  `AskRules`, so the SPA never restates the id compare or the triple key. */
export const wardAskRules: AskRules = {
  contactAskPending: (asks, peerHex) => wasm().wardContactAskPending(asks, peerHex),
  feedRequestState: (asks, bridgeId, operation, target) =>
    (wasm().wardFeedRequestState(asks, bridgeId, operation, target) as FeedRequestState | undefined) ??
      null,
};

/** The feed-source operations' wire names, from the shared
 *  `FeedSourceOperation::as_str` over its wasm face — so the SPA never spells
 *  `"link"`/`"follow"` itself (the FFI apps read `feed_source_operation_wire`). */
export const feedSourceOp = {
  link: (): string => wasm().feedSourceOperationWire(wasm().FeedSourceOperation.Link),
  follow: (): string => wasm().feedSourceOperationWire(wasm().FeedSourceOperation.Follow),
};

/** Read-modify-write + sign for the profile edit form: overwrite only
 *  display_name/bio/links, preserve every other identity field (or minimal
 *  defaults on first publish, `baseBody == null`). Returns the signed wire for
 *  `profileSet` (shared `fauna_client_profile::build_edited_profile`). */
export function buildEditedProfile(
  secretHex: string,
  baseBody: Uint8Array | null,
  displayName: string | null,
  bio: string | null,
  links: ProfileLinkInput[],
): Uint8Array {
  return wasm().build_edited_profile(
    secretHex,
    baseBody ?? undefined,
    displayName ?? undefined,
    bio ?? undefined,
    JSON.stringify(links),
  );
}

/** `buildEditedProfile` plus the two image fields — the full edit-form write
 *  once the avatar/banner can be set or removed (shared
 *  `fauna_client_profile::build_edited_profile_with_images`). wasm-bindgen
 *  enums can't carry a payload, so each field's three-state edit arrives as
 *  two arguments (the `ProfileImageEdit::from_parts` adapter): `*Clear = true`
 *  removes the picture; otherwise a hash sets it and `undefined` leaves it
 *  untouched. `clear` wins over a supplied hash. The picture bytes are
 *  uploaded separately, through the ordinary public-post blob path
 *  (`uploadBlobMultipart`) — this only records the resulting hash on the
 *  signed profile. */
export function buildEditedProfileWithImages(
  secretHex: string,
  baseBody: Uint8Array | null,
  displayName: string | null,
  bio: string | null,
  links: ProfileLinkInput[],
  avatarClear: boolean,
  avatarHashHex: string | null,
  bannerClear: boolean,
  bannerHashHex: string | null,
): Uint8Array {
  return wasm().build_edited_profile_with_images(
    secretHex,
    baseBody ?? undefined,
    displayName ?? undefined,
    bio ?? undefined,
    JSON.stringify(links),
    avatarClear,
    avatarHashHex ?? undefined,
    bannerClear,
    bannerHashHex ?? undefined,
  );
}

export function buildRegisterRequest(secretHex: string, handle: string, domain: string): string {
  return wasm().build_register_request(secretHex, handle, domain);
}

/** Silent sign-in over the pre-identity anonymous WS-RPC challenge/verify
 *  ceremony (`fauna.auth.{challenge,verify}`) — the SPA's only bearer mint,
 *  launch and re-mint alike (`login.md` § When to use which). Resolves the
 *  verified actor (`token`/`handle`/`domain`/`tier`/`expires_at`/`expires_in`)
 *  or `null` when the actor isn't registered on `origin`
 *  (`fauna.auth.not_registered`). The anonymous WS is CORS-exempt, so a
 *  cross-origin `origin` works where the HTTP fetch was CORS-blocked. Throws
 *  `TransientAuthError` on a transport failure, `NestOutdatedError` when the nest
 *  reports it is outdated (`fauna.nest.outdated`), a plain `Error` otherwise (see
 *  `classifyChallengeError`). */
export async function challengeVerify(
  secretHex: string,
  origin: string,
): Promise<{
  token: string;
  /// The id of the session this mint created — the launch path's own-session
  /// id (`docs/goal/behavior/devices.md` § The client's own session). The wasm
  /// side has always emitted it; the type dropped it until 2026-09-20.
  token_id: string;
  handle: string;
  domain: string;
  tier: string;
  /// Unix seconds on the **nest's** clock — informational; the deadline the
  /// SPA schedules on is `expires_in` anchored on its own clock.
  expires_at: number;
  /// Seconds the token lives from the reply — anchored on this device's clock
  /// at receipt (`token-deadline.ts`; `login.md` § Token lifetime on the
  /// client's clock). Required: every nest sends it.
  expires_in: number;
} | null> {
  await ensureWasm();
  let json: unknown;
  try {
    json = await wasm().challengeVerify(secretHex, origin);
  } catch (e) {
    // `transient:` → TransientAuthError (Retry CTA); `outdated:` →
    // NestOutdatedError (non-retry "update your nest" surface); anything else →
    // a plain Error the launch screen treats as `unreachable`. The wire prefixes
    // are produced by the wasm `map_silent_err` (version-compatibility.md Dim 4).
    throw classifyChallengeError(e);
  }
  if (json == null) return null; // not registered → 404-equivalent
  return JSON.parse(json as string);
}

/** Forget the pinned nest identity for `origin` (`security.md` § Transport trust) — the explicit user-approved "trust this
 *  nest" recovery on the `nest-identity-changed-warning`. Deletes the
 *  localStorage pin so the next connect re-establishes trust on first use
 *  (re-TOFU): a changed identity re-pins to the new one, a withdrawn proof
 *  proceeds unpinned. The browser analogue of `ssh-keygen -R host`. */
export async function forgetNestIdentityPin(origin: string): Promise<void> {
  await ensureWasm();
  wasm().forgetNestIdentityPin(origin);
}

/** Pre-identity public node metadata over the anonymous WS-RPC `fauna.nest.info`
 *  kind — the replacement for the deleted HTTP `GET /api/v1/node-info`
 *  (`api-layers.md` § Public). Returns the parsed `{ domain, version,
 *  registration }`; the caller (`$lib/api.ts`) casts to `NestInfoResponse`. */
export async function nestInfo(origin: string): Promise<unknown> {
  await ensureWasm();
  return JSON.parse((await wasm().nestInfo(origin)) as string);
}

/** Whether this identity owes itself a fresh RecoveryKey from a succession it
 *  just ran — the succession's closing act, handed across the account switch
 *  (`identity-succession.md` § The RecoveryKey → *At succession*).
 *
 *  A **peek**: it answers only "does this launch belong on the surface that
 *  renders a kit?", and must not consume an obligation that surface has not yet
 *  discharged. The claim is `claimOwedSuccessionKit`, made by that surface. */
export async function successionKitOwed(actorIdHex: string): Promise<boolean> {
  await ensureWasm();
  return wasm().successionKitOwed(actorIdHex) as boolean;
}

/** Take the owed-kit obligation, clearing it in the same breath — the one-shot
 *  claim. `false` on every ordinary sign-in and on a successor's second one.
 *
 *  Take-and-clear rather than clear-on-success so two racing renders of the
 *  section cannot both mint; a mint that then fails calls
 *  `rearmOwedSuccessionKit`. */
export async function claimOwedSuccessionKit(actorIdHex: string): Promise<boolean> {
  await ensureWasm();
  return wasm().claimOwedSuccessionKit(actorIdHex) as boolean;
}

/** Put back an obligation whose mint never reached the screen.
 *
 *  ⚠ A failed mint must RE-ARM, never spend: the ceremony revokes every session
 *  of the account inside the nest's own transaction, so the successor's first
 *  mint races its own reconnect on every platform (apple recorded this as a
 *  cross-platform lesson, not an iOS one). An unshown mint would leave a kit
 *  nobody holds — strictly worse than never-created. */
export async function rearmOwedSuccessionKit(actorIdHex: string): Promise<void> {
  await ensureWasm();
  wasm().rearmOwedSuccessionKit(actorIdHex);
}

/** Adopt a **chain-verified** successor this browser already holds — the state a
 *  lost succession reply leaves behind, whose message promised that reopening
 *  the app signs in as it (`identity-succession.md` § Implementation status
 *  today). `true` → the caller switches to `verifiedSuccessor` now; the owed kit
 *  and the owed group sweep are parked for the successor's session. The decision
 *  is the shared `AccountRegistry::adopt_held_successor`.
 *
 *  ⚠ `verifiedSuccessor` must be `resolveVerifiedSuccessor`'s answer, never the
 *  successor the nest's refusal claimed. */
export async function adoptHeldSuccessor(
  predecessorActorIdHex: string,
  verifiedSuccessor: string,
): Promise<boolean> {
  await ensureWasm();
  return wasm().adoptHeldSuccessor(predecessorActorIdHex, verifiedSuccessor) as boolean;
}

/** Discharge the group sweep a relaunch adoption owes — the unbidden press of
 *  `recovery-kit-sweep-retry-button` (`succession-propagation.md` § Propagation
 *  → *Own device fleet*, the relaunch-adoption clause). Claims once; `null` when
 *  nothing was owed, else the press's answer for `error-message`. The report it
 *  parks is the one `successionSweepCopy` then reads. */
export async function dischargeOwedSuccessionSweep(
  actorIdHex: string,
): Promise<LocalizedText | null> {
  await ensureWasm();
  return wasm().dischargeOwedSuccessionSweep(actorIdHex) as LocalizedText | null;
}

/** Walk the registration chain anonymously and return the **verified** successor
 *  of a refused identity (64-hex), or `null` when the chain authorizes none.
 *
 *  The launch path's second step after a `fauna.auth.superseded` refusal: the
 *  routing shows the claim-free explanation immediately, and this upgrades it to
 *  the wording that names the successor — but only once the chain has *proven*
 *  the hop, never off the successor the nest's refusal claimed
 *  (`identity-succession.md` § Propagation → *Own device fleet*).
 *
 *  Best-effort by contract: an unreachable nest, an empty lookup or a broken
 *  chain all resolve `null`, leaving the claim-free message standing. Only an
 *  unreadable actor id rejects. */
export async function resolveVerifiedSuccessor(
  origin: string,
  oldActorIdHex: string,
): Promise<string | null> {
  await ensureWasm();
  return (await wasm().resolveVerifiedSuccessor(origin, oldActorIdHex)) as string | null;
}

/** Resolve a domain to its canonical fauna node URL over the anonymous WS-RPC
 *  `fauna.nest.resolve` kind — the replacement for the deleted HTTP
 *  `GET /api/v1/resolve-node/{domain}`. Returns the bare URL string. */
export async function nestResolve(domain: string, origin: string): Promise<string> {
  await ensureWasm();
  return (await wasm().nestResolve(domain, origin)) as string;
}

/** Resolve a handle to its actor ID over the anonymous WS-RPC
 *  `fauna.actor.by_handle` kind — the replacement for the deleted HTTP
 *  `GET /api/v1/actor/by-handle/{handle}`. The anonymous WS is CORS-exempt, so a
 *  cross-origin (remote-nest) resolve works where the HTTP fetch was blocked.
 *
 *  `domain` is the optional multi-domain qualifier: for a recipient typed as
 *  `bob@domain2` (a secondary active local domain), passing `domain2` makes the
 *  nest echo it back so we can display `bob@domain2`; omitted → the nest reports
 *  the canonical identity domain (single-domain behavior). Additive —
 *  `mail-multidomain.md` § Multi-domain handles § Resolution. */
export async function actorByHandle(
  handle: string,
  origin: string,
  domain?: string,
): Promise<{ actor_id: string; handle: string; domain: string }> {
  await ensureWasm();
  return JSON.parse((await wasm().actorByHandle(handle, origin, domain)) as string);
}

/** Classify a user-entered Linked-nests value via shared
 *  `fauna_client_pair::classify_link_input` — a 64-hex identity → single-end
 *  `Link`, any other value → both-ends `LinkBoth`. The shell routes the same on
 *  every app (priority #2/#3). Requires `ensureWasm()` first (sync). */
export function classifyLinkInput(
  raw: string,
): { NestId: { nest_id: string } } | { NestUrl: { nest_url: string } } {
  return JSON.parse(wasm().classifyLinkInput(raw));
}

/** Whether a grant row's scope marks it a **bounded** (content-sealing-epochs)
 *  mail grant, via the shared `fauna_client_pair::trust_scope_is_bounded_mail_grant`
 *  (`encryption-at-rest.md` § Capability tiering, flip-checklist line 6). The
 *  Nests page's honest-bound copy (`nest-trust-grant-bound-note`) switches on
 *  this — never re-derive the (class, kind, tier) check in TypeScript
 *  (priority #2). Requires `ensureWasm()` first (sync). */
export function isBoundedMailGrant(
  scope: { class: string; kind: string | null; tier: string | null }[],
): boolean {
  return wasm().isBoundedMailGrant(scope);
}

// ── nest-trust label maps (`fauna_client_pair::{grant_scope_labels,
// status_label, backup_status_label, mint_option_label}`) — the shared enum→i18n-key maps linux/tui (native) and
// android (UniFFI) already consume; web stops hand-rolling its own copies
// (priority #2). Each returns a `LocalizedText` resolved via
// `resolveLocalized`. Requires `ensureWasm()` first (sync).

/** The folder a folder read grant covers — `fauna_client_pair::TrustFolder`
 *  as serde carries it (externally tagged). */
export type TrustFolder = { Named: { name: string } } | 'Deleted';

/** A grant row's or History entry's whole scope line, one label per tuple —
 *  `fauna_client_pair::grant_scope_labels`, which names a folder grant's
 *  folder (the row's `folder`, resolved in shared Rust) in place of the bare
 *  folder read; a scopeless `Revoke` of such a grant yields the folder alone. */
export function grantScopeLabels(
  scope: { class: string; kind: string | null; tier: string | null }[],
  folder: TrustFolder | null | undefined,
): LocalizedText[] {
  return wasm().grantScopeLabels(scope, folder ?? null) as LocalizedText[];
}

/** A grant's `TrustLiveness` → its localized status word
 *  (`nest-trust-grant-status`). */
export function statusLabel(l: string): LocalizedText {
  return wasm().statusLabel(l) as LocalizedText;
}

/** A backup row's `TrustBackupStatus` → its localized status word
 *  (`nest-trust-backup-status`). */
export function backupStatusLabel(s: string): LocalizedText {
  return wasm().backupStatusLabel(s) as LocalizedText;
}

/** A mint-picker option's use case → its localized label
 *  (`nest-trust-mint-scope-select`); the paywalled option names WHICH tier via
 *  the `{tier}` named placeholder. `o` is the full `TrustMintOption`, not just
 *  its `use_case`/`tier`. */
export function mintOptionLabel(o: {
  use_case: string;
  tier: string | null;
  scope: { class: string; kind: string | null; tier: string | null }[];
  holder_candidates: string[];
}): LocalizedText {
  return wasm().mintOptionLabel(o) as LocalizedText;
}

/** A grant's mint duration — the shared `TrustGrantDuration` serde names. */
export type TrustGrantDuration = 'OneOff' | 'Standard';

/** `fauna_client_pair::mint_duration_options` — what the duration select offers. */
export function mintDurationOptions(): TrustGrantDuration[] {
  return wasm().mintDurationOptions() as TrustGrantDuration[];
}

/** `fauna_client_pair::duration_label` — a duration option's localized label. */
export function durationLabel(d: TrustGrantDuration): LocalizedText {
  return wasm().durationLabel(d) as LocalizedText;
}

/** `view_model::AUTO_RENEW_CHECK_SECS` — the auto-renew loop's cadence. */
export function autoRenewCheckSecs(): number {
  return wasm().autoRenewCheckSecs() as number;
}

export function buildInviteRequestSubmit(secretHex: string, handle: string, message: string): { actor_id: string; handle: string; message: string; timestamp: number; signature: string } {
  const json = wasm().build_invite_request_submit(secretHex, handle, message);
  return JSON.parse(json);
}

export function buildInviteRequestCancel(secretHex: string): { actor_id: string; timestamp: number; signature: string } {
  const json = wasm().build_invite_request_cancel(secretHex);
  return JSON.parse(json);
}

export async function buildPost(
  secretHex: string,
  body: string,
  tags: string[],
  replyToHex: string,
): Promise<Uint8Array> {
  await ensureWasm();
  return wasm().build_post(secretHex, body, JSON.stringify(tags), replyToHex);
}

export async function buildPostWithMedia(
  secretHex: string,
  body: string,
  tags: string[],
  mediaItems: { hash: string; media_type: string; size_bytes: number }[],
  replyToHex: string,
): Promise<Uint8Array> {
  await ensureWasm();
  return wasm().build_post_with_media(secretHex, body, JSON.stringify(tags), JSON.stringify(mediaItems), replyToHex);
}

export async function decodePost(data: Uint8Array): Promise<any> {
  await ensureWasm();
  return JSON.parse(wasm().decode_post(data));
}

// The resolved-post plain-text extraction the tier-1 spam-model client-write path
// trains on (`fauna_core::data::Post::decode_resolved_bytes(data).map(body_text)`, the
// `postBodyText` wasm export) — the raw bytes `fauna.posts.get` returns, decoded via the
// SAME path the nest indexer/train handler uses, so a client-path train is byte-identical
// with a nest-path train on the same post. `undefined` if the bytes decode as neither the
// embed-as-bytes signed shape nor a bare canonical `Post`. See
// `docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction.
export function postBodyText(data: Uint8Array): string | undefined {
  return (wasm().postBodyText(data) as string | undefined) ?? undefined;
}

// ── Upload sidecar (audience-keyed seal + UploadSidecar) ─────
//
// Wrappers over `fauna_media::process_and_seal`. Each yields the two
// multipart parts (`sidecar` + `bytes`) the nest's `POST /api/v1/blob`
// expects, plus the MIME to record on the referencing content. Audience is
// chosen by the call-site: the photo library seals under the owner's
// `BackupKey` (`Library`); feed attachments pass through (`PublicPost`).

/** The multipart-ready output of an audience-keyed seal. */
export interface SealedUpload {
  /** DAG-CBOR-encoded `UploadSidecar` — the `sidecar` multipart part. */
  sidecar: Uint8Array;
  /** Sealed (Library) or plaintext (PublicPost) blob bytes — the `bytes` part. */
  bytes: Uint8Array;
  /** MIME to record on the referencing content's `MediaItem`. */
  mime: string;
  /** The thumbnail blob's own `sidecar` + `bytes` parts, when the shared
   *  `process_media` derived a thumbnail (an image larger than 300×300);
   *  `undefined` otherwise.
   *
   *  POST this as a SECOND blob, **before** the primary: `sidecar` above
   *  already declares `blake3` of `thumbnail.bytes` as its `thumbnail_hash`,
   *  and the nest records that pointer when it ingests the primary. Drop it and
   *  the stored pointer names a blob that was never uploaded, which the nest's
   *  `?thumb=1` lookup degrades by serving the full-size original. */
  thumbnail?: { sidecar: Uint8Array; bytes: Uint8Array };
}

/** Copy the wasm payload's fields into a plain object, then free the handle.
 *  The getters return JS-owned copies, so the result outlives `free()`. */
function takeUploadPayload(p: WasmUploadPayload): SealedUpload {
  const out: SealedUpload = { sidecar: p.sidecar, bytes: p.bytes, mime: p.mime };
  const thumbSidecar = p.thumbnailSidecar;
  const thumbBytes = p.thumbnailBytes;
  if (thumbSidecar && thumbBytes) out.thumbnail = { sidecar: thumbSidecar, bytes: thumbBytes };
  p.free();
  return out;
}

/** Seal owner-only library media under the owner's `BackupKey` (derived from
 *  the identity seed) and build its `UploadSidecar`. */
export async function processAndSealLibrary(secretHex: string, data: Uint8Array): Promise<SealedUpload> {
  await ensureWasm();
  return takeUploadPayload(wasm().process_and_seal_library(data, secretHex));
}

/** Seal a public-post blob attachment (plaintext passthrough) and build its
 *  `UploadSidecar`. `mime` is the browser-supplied Content-Type; `hasC2pa` is
 *  the browser's own C2PA manifest detection over the raw bytes (already-shipped
 *  viewer SDK) — `media.md` § C2PA provenance, *Upload-side `has_c2pa` population
 *  on web*. */
export async function processAndSealPublicPost(
  data: Uint8Array,
  mime: string,
  hasC2pa: boolean,
): Promise<SealedUpload> {
  await ensureWasm();
  return takeUploadPayload(wasm().process_and_seal_public_post(data, mime, hasC2pa));
}

/** Seal one compose attachment for the composer's **current** audience — the
 *  SPA's door onto the shared-Rust seal-by-id helper (`media.md` § Encryption
 *  at rest). Use this, not `processAndSealPublicPost`, in the feed composer:
 *  that entry point seals only the public audience, because a tier's period key
 *  never crosses the wasm boundary, so a tier-restricted post's photo can only
 *  be sealed on the far side of it.
 *
 *  Stage the gate FIRST (`updateComposeGate`), then call this, then upload —
 *  the audience decides the seal, and a blob POSTed before the audience is
 *  known is a plaintext copy no blob DELETE can remove.
 *
 *  Returns the same `SealedUpload` shape as the public path, so the upload glue
 *  is identical; `mime` is the plaintext's real type, which is what the
 *  `MediaItem` must record. */
export async function sealComposeAttachment(
  manager: { sealComposeAttachment(raw: Uint8Array): Promise<unknown> },
  data: Uint8Array,
): Promise<SealedUpload> {
  await ensureWasm();
  return takeUploadPayload((await manager.sealComposeAttachment(data)) as never);
}

/** The DAG-CBOR `UploadSidecar` bytes for a gated post's already-sealed
 *  full-body blob (`PeriodRestrictedPost`, mime `application/octet-stream`) —
 *  the shared web twin of native `upload_gated_post_blob`'s sidecar, so every
 *  app ships a byte-identical sidecar (feed.md § Encryption at rest). The
 *  sealed bytes come from `WasmFeedManager.prepareGatedBlob`; POST both as the
 *  multipart `sidecar` + `bytes` parts of `/api/v1/blob`. */
export async function gatedPostSidecar(): Promise<Uint8Array> {
  await ensureWasm();
  return wasm().gated_post_sidecar();
}

// The legacy standalone MLS engine plane (thread-local `mls_init_engine` +
// IndexedDB persistence + the `fauna_mls_active_tab` single-tab lease +
// `beforeunload` localStorage backup) was RETIRED with slice 6 of
// devices.md § Cross-device MLS group-state sync: the conversations manager's
// engine (`$lib/conversations` / `libs/fauna-wasm/src/conversations.rs`) is the
// ONE web MLS engine — multi-tab-safe via the nest replica + CAS, and the only
// minter of key packages (a second engine's private init keys would be
// invisible to the engine that processes Welcomes).

// ── File chunking ───────────────────────────────────────────

export async function chunkFile(data: Uint8Array): Promise<{
  manifest: { file_hash: string; total_size: number; chunk_count: number };
  chunks: Array<{ hash: string; data: string }>;
}> {
  await ensureWasm();
  return JSON.parse(wasm().chunk_file(data));
}

// ── Content moderation ──────────────────────────────────────

export interface ClassifyLabel {
  category: string;
  confidence: number;
}

export function classifyText(text: string): ClassifyLabel[] {
  const json = wasm().classify_text(text);
  return JSON.parse(json);
}

// `buildScanReport` (and later the whole scan-report producer) was removed —
// its client-side producer was retired 2026-07-19 (`moderation.md` § State &
// data shape) and the `fauna.moderation.scan_report` kind itself left the wire
// 2026-09-24 with the compat-remnant sweep.

// The client-side Bayesian spam filter (bayesImport/bayesExport/
// bayesTrainSpam/bayesTrainHam/bayesScore/bayesReset, wrapping the retired
// `bayespam` crate) was removed — it had zero production callers (only
// `spam-model.ts`'s manual export/import round-trip); the real on-device
// "Fauna-app" scoring position (`docs/goal/behavior/mail-spam.md` § Scoring
// placement) is `libs/fauna-client-mail-settings/src/inbox_scorer.rs`,
// scoring against the shared `fauna_mail::spam::SpamModel`.

// ── Value formatting (shared `fauna_core::format`) ────────────
//
// Thin sync wrappers over the wasm boundary; the display helpers live in
// `$lib/value-format`. See `docs/goal/behavior/value-formatting.md`.

/** `{ localized?: {key,args}, absolute_epoch_ms?: number }` — `localized` is
 *  set for recent buckets; `absolute_epoch_ms` for `≥7d` (render a native date). */
export interface RelativeTimeDisplay {
  localized: LocalizedText | null;
  absolute_epoch_ms: number | null;
}

export function byteSizeRaw(bytes: number): LocalizedText {
  return wasm().byteSize(bytes) as LocalizedText;
}

export function relativeTimeRaw(nowMs: number, thenMs: number): RelativeTimeDisplay {
  return wasm().relativeTime(nowMs, thenMs) as RelativeTimeDisplay;
}

/** `{ clock?: string, localized?: {key,args}, absolute_epoch_ms?: number }` —
 *  exactly one set: `clock` ("HH:MM", 24h local) for today, `localized` for
 *  Yesterday / a weekday, `absolute_epoch_ms` for `≥7d` (render a native date).
 *  Bucketed in the caller's local timezone (`utcOffsetSeconds`). */
export interface ConversationTimestampDisplay {
  clock: string | null;
  localized: LocalizedText | null;
  absolute_epoch_ms: number | null;
}

export function conversationTimestampRaw(
  nowMs: number,
  thenMs: number,
  utcOffsetSeconds: number,
): ConversationTimestampDisplay {
  return wasm().conversationTimestamp(
    nowMs,
    thenMs,
    utcOffsetSeconds,
  ) as ConversationTimestampDisplay;
}

// ── Admin numeric-field validators (shared `fauna_core::format`) ──
//
// Input validation (not formatting), so they return a plain `number | undefined`
// rather than a `LocalizedText`. Consumed as `parse*(text) ?? prev` so a
// blank/unparseable edit keeps the persisted value (never silently zeroes a
// cap/knob). The wasm twin of the native `parse_cap`/`parse_count`/
// `parse_count_u64` UniFFI exports, so web stops hand-rolling the parse rules
// (priority #2). See value-formatting.md §§ Tier cap / Mail-knob validation.

/** Parse an admin tier-cap field → the non-negative cap, or `undefined`
 *  (negatives clamp to `0`). `admin/settings/+page.svelte`. */
export function parseCap(text: string): number | undefined {
  return wasm().parseCap(text) ?? undefined;
}

/** Parse an admin-mail `u32` integer knob → the value, or `undefined`. A leading
 *  `+` is accepted (canonical rule). `admin/mail/+page.svelte`. */
export function parseCount(text: string): number | undefined {
  return wasm().parseCount(text) ?? undefined;
}

/** The `u64` sibling of {@link parseCount} for the IMAP storage-bytes ceiling. */
export function parseCountU64(text: string): number | undefined {
  return wasm().parseCountU64(text) ?? undefined;
}

/** The signed-`i64` per-alias sibling of {@link parseCount} for the mail-alias
 *  `rate_limit_per_hour` override (non-negative cap; empty/invalid → `undefined`
 *  = "no override"). `MailAliasesSection.svelte`. */
export function parseCountI64(text: string): number | undefined {
  return wasm().parseCountI64(text) ?? undefined;
}

/** Parse a user-entered admin port field (CalDAV/serving-port) → a validated
 *  `1..=65535` TCP port, or `undefined` for empty/non-numeric/out-of-range
 *  input (incl. `0`). The wasm twin of the native `parse_port` UniFFI export
 *  android/apple/windows already consume. `admin-calendar`/`admin-nest`
 *  `+page.svelte`. See value-formatting.md § Port validation. */
export function parsePort(text: string): number | undefined {
  return wasm().parsePort(text) ?? undefined;
}

/** Parse a user-entered declared-region code (`admin-nest-region-input`) → a
 *  validated 2-8 character code (each an uppercase ASCII letter or digit), or
 *  `undefined` for a malformed one — never case-folded (two spellings of one
 *  region must not both be storable). The wasm twin of the native
 *  `admin_parse_region_code` UniFFI export android/apple/windows consume.
 *  `admin-nest` `+page.svelte`. See region-blocking.md § Region determination. */
export function adminParseRegionCode(text: string): string | undefined {
  return wasm().adminParseRegionCode(text) ?? undefined;
}

/** `fauna_client_moderation::takedown_form_view` → what the admin-nest
 *  legal-takedown console renders (`moderation.md` § Legal takedown →
 *  Invocation surface) — the ONE shared gating/wording every app applies (a
 *  citation-less takedown is never armable; a note-less RESTORE is; the armed
 *  confirm names verb + content + citation). Pure — no nest hop. */
export interface TakedownFormView {
  can_submit: boolean;
  blocked_reason: LocalizedText | null;
  arm_label: LocalizedText;
  confirm_summary: LocalizedText;
  confirm_label: LocalizedText;
}

export function takedownFormView(
  contentId: string,
  conversation: boolean,
  legalReference: string,
  restore: boolean,
): TakedownFormView {
  return wasm().takedownFormView(
    contentId,
    conversation,
    legalReference,
    restore,
  ) as TakedownFormView;
}

/** `fauna_client_moderation::takedown_verdict` → the console's outcome line
 *  (`admin-nest-takedown-status`). Pass the dispatch rejection's message on
 *  failure, `null` on success. */
export function takedownVerdict(restore: boolean, error: string | null): LocalizedText {
  return wasm().takedownVerdict(restore, error ?? undefined) as LocalizedText;
}

// ── User-initiated reporting (`moderation.md` § User-initiated reporting →
// *Where logic lives*) — the wasm twins of the UniFFI `abuse_report.rs` faces.
// The sheet's gating and every sentence are shared Rust's
// (`fauna_client_moderation::report`); the SPA paints what comes back.

/** The wire's tagged `AbuseReportSubject`. */
export type AbuseReportSubject =
  | { kind: 'post'; cid: string }
  | { kind: 'message'; channel: string; record_cid: string }
  | { kind: 'actor'; actor_id: string };

/** What a surface holds when it opens the sheet. */
export interface ReportTarget {
  subject: AbuseReportSubject;
  /** The nest holds no readable bytes for it (a message; a gated post). */
  sealed: boolean;
  /** The author's hex actor id — routes the report to their home nest. */
  author: string | null;
  /** The text the client already holds — the excerpt source. */
  plaintext: string | null;
}

/** The sheet's draft; `reason` is a wire token (`spam`, `harassment`, …) or null. */
export interface ReportForm {
  reason: string | null;
  note: string;
  include_text: boolean;
  block_author: boolean;
}

export interface ReportSheetView {
  title: LocalizedText;
  reason_label: LocalizedText;
  reasons: { reason: string; label: LocalizedText }[];
  note_label: LocalizedText;
  show_include_text: boolean;
  include_text_label: LocalizedText;
  block_author_label: LocalizedText;
  submit_label: LocalizedText;
  cancel_label: LocalizedText;
  can_submit: boolean;
  blocked_reason: LocalizedText | null;
}

/** `ReportTarget::post` — a feed post; `gated` is the sealed rule's post arm
 *  (the post was opened through a key). */
export function reportPostTarget(
  cid: string,
  author: string,
  plaintext: string,
  gated: boolean,
): ReportTarget {
  return wasm().reportPostTarget(cid, author, plaintext, gated) as ReportTarget;
}

/** `ReportTarget::actor` — the OTHER profile. */
export function reportActorTarget(actorId: string): ReportTarget {
  return wasm().reportActorTarget(actorId) as ReportTarget;
}

/** `ReportTarget::message` — a conversation message off its plane ref; `null`
 *  for a mail / bridged message, which paints no report verb. */
export function reportMessageTarget(
  planeScope: string,
  recordDigest: string,
  senderActor: string | null,
  plaintext: string,
): ReportTarget | null {
  return wasm().reportMessageTarget(
    planeScope,
    recordDigest,
    senderActor ?? undefined,
    plaintext,
  ) as ReportTarget | null;
}

/** `report::report_sheet_view` — the sheet's per-keystroke fold. */
export function reportSheetView(target: ReportTarget, form: ReportForm): ReportSheetView {
  return wasm().reportSheetView(target, form) as ReportSheetView;
}

/** `report::report_failed` — the line a failed send paints on `error-message`. */
export function reportFailed(error: string): LocalizedText {
  return wasm().reportFailed(error) as LocalizedText;
}

/** `report::ledger_title` / `ledger_empty`. */
export function reportLedgerWords(): { title: LocalizedText; empty: LocalizedText } {
  return wasm().reportLedgerWords() as { title: LocalizedText; empty: LocalizedText };
}

/** `report::withdraw_verdict` — `null` on success. */
export function reportWithdrawVerdict(error: string | null): LocalizedText | null {
  return wasm().reportWithdrawVerdict(error ?? undefined) as LocalizedText | null;
}

/** `report::resolve_verdict` — `acted` false is a dismissal. */
export function reportResolveVerdict(acted: boolean, error: string | null): LocalizedText {
  return wasm().reportResolveVerdict(acted, error ?? undefined) as LocalizedText;
}

/** `report::message_subject` — the subject for a conversation message from its
 *  plane ref; `null` for a mail / bridged message (no report verb). */
export function reportMessageSubject(
  planeScope: string,
  recordDigest: string,
): AbuseReportSubject | null {
  return wasm().reportMessageSubject(planeScope, recordDigest) as AbuseReportSubject | null;
}

/** `report::takedown_prefill` — what *open takedown* pre-fills the console
 *  with; `null` for an account. */
export function reportTakedownPrefill(
  subject: AbuseReportSubject,
): { content_id: string; conversation: boolean } | null {
  return wasm().reportTakedownPrefill(subject) as {
    content_id: string;
    conversation: boolean;
  } | null;
}

/** One reporter-ledger row (`moderation-report-item`), worded by the shared fold. */
export interface ReportLedgerRow {
  report_id: string;
  subject: AbuseReportSubject;
  created_at: number;
  reason: LocalizedText;
  status: LocalizedText;
  outcome: LocalizedText | null;
  routed_to: LocalizedText;
  can_withdraw: boolean;
}

/** One admin-queue row (`admin-nest-report-item`), worded by the shared fold. */
export interface ReportQueueRow {
  report_id: string;
  subject: AbuseReportSubject;
  subject_actor: string | null;
  reason: LocalizedText;
  note: string | null;
  excerpt: string | null;
  origin: LocalizedText;
  created_at: number;
  can_open_takedown: boolean;
}

// ── Outside-app sign-in keys (`admin-nest-oauth-*`) — the pure word folds of
// `fauna_client_admin` (authorization-server.md § The issuer → Two rotation
// arms), the wasm twins of the UniFFI faces the natives render. The section
// decides no sentence of its own; the calls that dispatch are `$lib/rpc`'s.

/** `fauna_client_admin::issuer_key_row_label` → one
 *  `admin-nest-oauth-key-item-{n}` line: the signer's kid, or how many more
 *  whole minutes a retired key is accepted. `nowSecs` is the clock at paint
 *  (epoch seconds) — the countdown is the point of a retired key's line. */
export function issuerKeyRowLabel(row: IssuerKeyRow, nowSecs: number): LocalizedText {
  return wasm().issuerKeyRowLabel(row, nowSecs) as LocalizedText;
}

/** `fauna_client_admin::issuer_key_rotate_cost` → the ordinary rotation's
 *  cost, painted beside `admin-nest-oauth-rotate-button` (it has no confirm). */
export function issuerKeyRotateCost(view: IssuerKeyView): LocalizedText {
  return wasm().issuerKeyRotateCost(view) as LocalizedText;
}

/** `fauna_client_admin::issuer_forced_confirm_view` → the armed confirm's
 *  summary + confirm label for `arm`, folded ONCE at arm time over the view
 *  the admin is looking at and never re-folded while armed. */
export function issuerForcedConfirmView(
  arm: IssuerForcedArm,
  view: IssuerKeyView,
): IssuerForcedConfirmView {
  return wasm().issuerForcedConfirmView(arm, view) as IssuerForcedConfirmView;
}

/** Display host of a `scheme://host[:port]/…` URL (scheme/port/path dropped),
 *  e.g. `"example.com"` for `"https://example.com/article"`; returns the
 *  original string when no host can be isolated. The wasm twin of the native
 *  `url_host` UniFFI export linux/tui/android/apple/windows already consume —
 *  `LinkPreviewCard.svelte`'s `link-preview-domain`. See value-formatting.md
 *  § URL host display. */
export function urlHost(url: string): string {
  return wasm().urlHost(url);
}

// ── Email filter create-dialog encoders (shared `fauna_protocol::email`) ──
//
// Thin sync wrappers over the shared `encode_filter_rule` / `encode_filter_action`
// — the create-filter dialog's (rule-kind, value) / (action, reject-reason) →
// typed-wire map that every app used to hand-roll (drifting on tag casing,
// rule coverage, and the reject-reason default). Now one source of truth so web
// can't drift from what the nest deserializes. Each returns the externally-tagged
// shape web already built (`{ SenderIs: { address } }`; `"Allow"` / `{ Reject:
// { reason } }`) and THROWS a string on an unknown kind (surfaced to the form's
// error line). See `docs/goal/ui/settings.md` § Email filter create-dialog
// encoding + `docs/goal/behavior/smtp-server.md` § Email filter rules.

export function encodeEmailFilterRule(kind: string, value: string): Record<string, unknown> {
  return wasm().encodeEmailFilterRule(kind, value) as Record<string, unknown>;
}

// Every input the shared filter form collects for its action — the wasm
// `FilterActionInputs` (snake_case, `fauna_protocol::email`). The form holds
// the whole object as state, so an edit writes back the Reject reason and the
// Forward copy mode unchanged.
export interface FilterActionInputs {
  kind: string;
  reject_reason: string;
  forward_address: string;
  keep_local_copy: boolean;
}

// The action tags the form's dropdown offers and the edit gate admits
// (`fauna_protocol::email::SUPPORTED_ACTION_KINDS`).
export const FILTER_ACTION_KINDS = ['Allow', 'Discard', 'Reject', 'Forward'];

// A fresh form's action inputs: the first dropdown entry, every input at its
// default (`FilterActionInputs::default()` — keep-a-local-copy checked).
export function defaultFilterActionInputs(): FilterActionInputs {
  return { kind: FILTER_ACTION_KINDS[0], reject_reason: '', forward_address: '', keep_local_copy: true };
}

export function encodeEmailFilterActionInputs(
  inputs: FilterActionInputs,
): string | Record<string, unknown> {
  return wasm().encodeEmailFilterActionInputs(inputs) as string | Record<string, unknown>;
}

// The edit dialog's reverse of the two encoders above: decode a stored
// EmailFilterRule/EmailFilterAction back into the create-dialog's (kind,
// value) pair, so the edit form pre-populates from the same shared source of
// truth instead of a hand-rolled per-variant switch. `null` for the richer
// variants no dialog collects (HeaderContains/SpamScoreAtLeast;
// FileInto/Forward/AutoReply/AddLabel) — that gap means "don't offer edit",
// not "this call failed", so it returns null rather than throwing.
// `filterIsEditableFor` is the row-level gate: true only when every rule + the
// action decode this way. See docs/goal/behavior/smtp-server.md § Email
// filter rules.

export function describeEmailFilterRule(rule: Record<string, unknown> | string): [string, string] | null {
  return wasm().describeEmailFilterRule(rule) as [string, string] | null;
}

export function describeEmailFilterActionInputs(
  action: Record<string, unknown> | string,
): FilterActionInputs | null {
  return wasm().describeEmailFilterActionInputs(action) as FilterActionInputs | null;
}

// The `filter-action` list-row badge label for a stored `EmailFilterAction`.
// `fauna_protocol::email::filter_action_label` (over wasm) owns the action →
// key decision — unlike `describeEmailFilterActionInputs` (`null` for the
// richer variants, which only gates *editability*), this always resolves.
// Returns a `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/behavior/smtp-server.md` § Email filter rules.
export function emailFilterActionLabel(action: Record<string, unknown> | string): LocalizedText {
  return wasm().emailFilterActionLabel(action) as LocalizedText;
}

export function filterIsEditableFor(filter: EmailFilter, kinds: string[]): boolean {
  return wasm().filterIsEditableFor(filter, kinds) as boolean;
}

export function durationSecsRaw(secs: number): LocalizedText {
  return wasm().durationSecs(secs) as LocalizedText;
}

export function graceCountdownRaw(deadlineMs: number, nowMs: number): LocalizedText | undefined {
  return wasm().graceCountdown(deadlineMs, nowMs) as LocalizedText | undefined;
}

// The three tip formatters MOVED to `$lib/payments` (resolved there rather than
// exposed raw, since `$lib/value-format`'s wrappers moved with them) — see
// `wasmCoreModule` above.

// Shared spam-threshold label band. `fauna_protocol::spam::spam_threshold_band`
// (over wasm) maps a 0.0–1.0 slider value to the canonical `aggressive` /
// `moderate` / `permissive` i18n key — the same buckets every app uses — so
// web stops re-deriving them inline. See `docs/goal/ui/settings.md` § Spam
// threshold slider labels.
export type SpamThresholdBandKey = 'aggressive' | 'moderate' | 'permissive';

export function spamThresholdBand(threshold: number): SpamThresholdBandKey {
  return wasm().spamThresholdBand(threshold) as SpamThresholdBandKey;
}

// Shared probability→per-mille conversion. `fauna_protocol::spam::probability_to_per_mille`
// (over wasm) is the SAME clamp+round math the nest and native apps use for a
// 0.0–1.0 spam/phishing threshold — web stops hand-rolling `Math.round(x * 1000)`.
export function probabilityToPerMille(probability: number): number {
  return wasm().probabilityToPerMille(probability);
}

// `fauna_core::data::InboxMode::to_wire` (over wasm) — the four `inbox-mode-*`
// wire tokens, in the same order tui's and linux's `INBOX_MODES` tables use.
// See `docs/goal/ui/settings.md` § Privacy sub-page item 7.
export function inboxModeValues(): string[] {
  return wasm().inboxModeValues() as string[];
}

// Shared handle-format validation. `fauna_protocol::handle::validate_handle`
// (over wasm) returns the canonical error message for a malformed handle, or
// `null` when valid — the SAME validator the nest enforces and linux/native
// apps call (the UniFFI twin is `fauna_ffi::handle::validate_handle`). The
// Settings change-handle form calls it pre-submit for instant feedback; a taken
// handle stays server-authoritative. See `docs/goal/ui/settings.md`
// § Where logic lives → Handle change.
export function validateHandle(handle: string): string | null {
  return wasm().validateHandle(handle) ?? null;
}

// `fauna_protocol::nostr_relay::relay_url_error` (over wasm) — the message
// for a relay address the user may not add (not `wss://`/`ws://` with a host,
// or a private-network address — `nest/network-exposure.md` § Rulings F7), or
// `null` when it is acceptable; the UniFFI twin is
// `fauna_ffi::nostr_relay::relay_url_error`. The Nostr settings "Add relay"
// control shows it via `resolveLocalized` instead of hand-rolling the check.
// See `docs/goal/ui/nostr.md`.
export function relayUrlError(url: string): LocalizedText | null {
  return (wasm().relayUrlError(url) as LocalizedText | undefined) ?? null;
}

// `fauna_protocol::pending_actions::describe_pending_action` (over wasm) —
// what a scheduled action will do, as one sentence (the
// `pending-action-description` contract). Shared with tui/linux
// (`PendingActionSummary::description` / `pending_description`) so an
// unrecognized `action_type` (client/nest skew) paints the same raw
// fallback everywhere. See `docs/goal/ui/settings.md` § Pending actions.
export function describePendingAction(actionType: string, target: string | null): string {
  return wasm().describePendingAction(actionType, target ?? undefined);
}

// `fauna_protocol::nostr_relay::trimmed_relay_input` (over wasm) — the
// trimmed relay input, or `null` when that's empty. Shared trim/empty rule
// every "Add relay" control applies (android/apple via the UniFFI twin
// `fauna_ffi::nostr_relay::trimmed_relay_input`). See `docs/goal/ui/nostr.md`.
export function trimmedRelayInput(input: string): string | null {
  return wasm().trimmedRelayInput(input) ?? null;
}

// `fauna_protocol::nostr_relay::relay_list_appending` (over wasm) —
// `existing` with `url` appended, or `null` when `url` is already present.
// Shared dedup-then-append rule every "Add relay" control applies once
// `relayUrlError` has returned null. See `docs/goal/ui/nostr.md`.
export function relayListAppending(existing: string[], url: string): string[] | null {
  return wasm().relayListAppending(existing, url) ?? null;
}

// Shared search-result formatters. `fauna_client_search::render` (over wasm)
// owns the canonical, prefix-aware + nest-accurate `content_type → badge` map
// and the FTS `<b>`-marker/entity snippet cleanup — the same map+cleanup every
// app uses — so web stops re-deriving its own `contentTypeLabel` and stops
// leaking raw `<b>` markers in snippets. `searchContentTypeBadge` returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/ui/search.md` § Implementation status today.
export function searchContentTypeBadge(contentType: string): LocalizedText {
  return wasm().searchContentTypeBadge(contentType) as LocalizedText;
}

export function searchCleanSnippet(raw: string): string {
  return wasm().searchCleanSnippet(raw) as string;
}

// `fauna_client_search::TYPE_FILTER_OPTIONS` — the `search-type-filter`
// option tokens, in render order. Shared so the SPA's dropdown can never
// drift from the mapping the manager applies to both search backends. See
// `docs/goal/ui/search.md` § State & data shape.
export function searchTypeFilterOptions(): string[] {
  return wasm().searchTypeFilterOptions() as string[];
}

// `fauna_client_search::type_filter_label` — what each `search-type-filter`
// option is *called*, the companion to `searchTypeFilterOptions`' *which tokens
// exist*. Expressed over the same badge map the result rows carry, so the
// dropdown and its own results can never read differently, and an unrecognised
// token surfaces raw rather than as a second entry reading "All" — which is
// exactly what this page's own closed `typeFilterLabel` match used to do. See
// `docs/goal/ui/search.md` § State & data shape → *Type filter*.
export function searchTypeFilterLabel(token: string): LocalizedText {
  return wasm().searchTypeFilterLabel(token) as LocalizedText;
}

/** Shared content-label badge presentation (`fauna_core::content_category`). */
export interface ContentLabelStyle {
  label: LocalizedText;
  /** Emoji icon. */
  icon: string;
  /** Background-tint base colour (hex) — apply at low alpha. */
  tint: string;
  /** Higher-contrast text/icon accent colour (hex). */
  accent: string;
}

// Shared content-label badge map. `fauna_core::content_category::content_label_style`
// (over wasm) owns the canonical 5-category vocabulary + the `category →
// label/icon/colour` map every app renders, so web stops hard-coding its
// `category-*` CSS + `categoryText`/`categoryIcon` maps (drift #157). Resolve
// `style.label` via `resolveLocalized`. See `moderation.md` § Where logic lives.
export function contentLabelStyle(category: string): ContentLabelStyle {
  return wasm().contentLabelStyle(category) as ContentLabelStyle;
}

/** One classifier verdict on a post or message (`ContentLabelEntry`). */
export interface ContentLabelEntry {
  category: string;
  confidence_per_mille: number;
}

// The `content-label-badge` label for an item's `labels`, in the badge's
// `category:confidence` prop form, or `undefined` when there are none. The PICK is
// the shared `fauna_core::content_category::primary_content_label` over wasm — one
// decision for the feed card and the DM bubble (moderation.md § Per-row badge data
// path), never a per-page reduce.
export function contentLabelBadgeFor(labels: ContentLabelEntry[] | null | undefined): string | undefined {
  if (!labels || labels.length === 0) return undefined;
  const top = wasm().primaryContentLabel(labels) as ContentLabelEntry | undefined;
  return top ? `${top.category}:${top.confidence_per_mille / 1000}` : undefined;
}

// Shared backup-destination row label. `fauna_core::format::backup_destination_label`
// (over wasm) returns the `display_name` when set (non-empty), else the destination
// URL's host (scheme/port/path stripped) — the same `destination_label` glue the
// native apps use — so web stops hand-rolling the `split('://')` host parse in
// `destLabel`. See `docs/goal/ui/backups.md` § State & data shape.
export function backupDestinationLabel(displayName: string | null, destinationNestUrl: string): string {
  return wasm().backupDestinationLabel(displayName ?? undefined, destinationNestUrl) as string;
}

/** `{ label: {key,args}, when?: RelativeTimeDisplay }` — `when` is set only when a real
 *  upload timestamp exists, and substitutes as the label's `{when}` arg. */
export interface BackupLastUploadDisplay {
  label: LocalizedText;
  when: RelativeTimeDisplay | null;
}

// Shared `backup-destination-last-upload-time` row text.
// `fauna_core::format::backup_last_upload_label` (over wasm) owns the never-vs-real key
// selection, the epoch-`0` guard, and the seconds→ms conversion web hand-rolled in
// `lastUploadText`. Pass the status's raw `last_upload_time` (unix **seconds**; absent or
// `0` ⇒ the "never" key, `when: null`). See `docs/goal/behavior/value-formatting.md`
// § Backup destination status labels.
export function backupLastUploadLabel(
  lastUploadSecs: number | null,
  nowMs: number,
): BackupLastUploadDisplay {
  return wasm().backupLastUploadLabel(lastUploadSecs ?? undefined, nowMs) as BackupLastUploadDisplay;
}

// Shared `backup-destination-backlog-count` row text.
// `fauna_core::format::backup_backlog_label` (over wasm) carries the absent-status 0
// baseline every app applied by hand, and returns a complete `LocalizedText` — no
// client-side composition. See `docs/goal/behavior/value-formatting.md` § Backup
// destination status labels.
export function backupBacklogLabel(backlogCount: number | null): LocalizedText {
  return wasm().backupBacklogLabel(backlogCount ?? undefined) as LocalizedText;
}

/** Same `{ label, when? }` shape as `BackupLastUploadDisplay`, and deliberately a
 *  distinct type: the upload row is the *source nest* reporting on its own work,
 *  this one is what the *client's own* audit independently confirmed. A client
 *  rendering one where it meant the other is the confusion the audit exists to
 *  prevent (`fauna_core::format::BackupLastAuditDisplay`). */
export interface BackupLastAuditDisplay {
  label: LocalizedText;
  when: RelativeTimeDisplay | null;
}

// Shared `backup-destination-last-audit-time` row text.
// `fauna_core::format::backup_last_audit_label` (over wasm) owns the never-vs-real key
// selection and the seconds→ms conversion. Pass the audit record's raw
// `last_passed_at` (unix **seconds**; absent or `0` ⇒ the "never" key, `when: null` —
// which is honest and deliberately NOT an alert: a destination enrolled ten minutes
// ago has never passed an audit and is perfectly healthy). See
// `docs/goal/ui/backups.md` § Audit-alert surface.
export function backupLastAuditLabel(
  lastPassedSecs: number | null,
  nowMs: number,
): BackupLastAuditDisplay {
  return wasm().backupLastAuditLabel(lastPassedSecs ?? undefined, nowMs) as BackupLastAuditDisplay;
}

// Shared `backup-destination-last-audit-time` row text for a **client-device
// custodian** row. Same `{ label, when? }` shape as `backupLastAuditLabel` and
// resolved the same way — a separate door all the way down, because the
// owner-side loop and a custodian's self-report answer the same question from
// opposite sides of the trust line (`docs/goal/ui/backups.md`
// § Audit-alert surface → *The client-device arm*). `lastPassedSecs` is the
// status row's raw `last_audit_passed_at` (unix **seconds**; absent or `0` ⇒
// "Self-checked: not yet", never a verdict).
export function backupSelfAuditLabel(
  lastPassedSecs: number | null,
  nowMs: number,
): BackupLastAuditDisplay {
  return wasm().backupSelfAuditLabel(lastPassedSecs ?? undefined, nowMs) as BackupLastAuditDisplay;
}

/** Why one `backup-audit-alert` banner is showing — **opaque to the SPA**. It comes
 *  out of a `backupAuditRunPass` row and goes straight back into
 *  `backupAuditAlertLabel`; web never inspects or constructs one, because which
 *  verdicts are loud (and what each says) is shared Rust's single answer. */
export type BackupAuditAlertReason = unknown;

// Shared `backup-audit-alert` banner text for one failing destination — a complete
// `LocalizedText` naming both the destination and the reason (the banner is indexed,
// so a bare "backup problem" would not say *which* one).
// `fauna_core::format::backup_audit_alert_label` over wasm.
export function backupAuditAlertLabel(
  reason: BackupAuditAlertReason,
  destinationLabel: string,
): LocalizedText {
  return wasm().backupAuditAlertLabel(reason, destinationLabel) as LocalizedText;
}

// Whether a client-device custodian's reported `audit_state` (the status
// row's own field) must raise a `backup-audit-alert`. Absence and an
// unrecognised value both stay quiet — the single shared answer
// (`fauna_core::format::backup_self_audit_is_alerting`), so web cannot drift
// into alerting on a custodian that has simply not audited yet.
export function backupSelfAuditIsAlerting(auditState: string | null): boolean {
  return wasm().backupSelfAuditIsAlerting(auditState ?? undefined) as boolean;
}

// The opaque `BackupAuditAlertReason::SelfReported` value for a
// `backupSelfAuditIsAlerting`-flagged row's banner. Web never constructs a
// reason by hand — every other arm arrives pre-built on an audit row — so
// this is the one door that hands it one, keeping the type opaque above.
export function backupSelfReportedAlertReason(): BackupAuditAlertReason {
  return wasm().backupSelfReportedAlertReason();
}

// Feed the audit's observation high-water from a render path: "this client has
// displayed activity stamped `lastActivityMs`". Returns whether anything was
// persisted, so a caller can keep its own in-memory high-water and skip the call on
// the common no-op (the monotonic shared `observe_local_record` writes nothing when
// the stamp is not newer).
//
// ⚠ **This is the load-bearing half of the audit on every app.** Freshness
// compares the destination against what this client saw *with its own eyes*; a shell
// that renders the audit elements but never calls this ships a permanently-passing
// audit, and nothing about it looks broken. See `docs/goal/ui/backups.md`
// § Audit-alert surface ("each shell must feed it").
export function backupAuditObserve(actorIdHex: string, lastActivityMs: number): boolean {
  return wasm().backupAuditObserve(actorIdHex, lastActivityMs) as boolean;
}

/** Test-only: shift the audit's clock by `offsetSecs`, the `backup_audit_run_now`
 *  agent command's own half (the pass itself is the production `backupAuditRunPass`).
 *  Only *time* is fakeable — the destination connection, its custody reply, and this
 *  client's own observation high-water all stay real.
 *
 *  ⚠ Process-wide, and nothing auto-resets it: an offset left behind silently
 *  subtracts from the next audit test's elapsed time. Zero it at start and end.
 *
 *  The export exists only in the `wasm-core-test` chunk flavor (gated on
 *  `libs/fauna-wasm`'s `test-helpers` feature — testing.md § convention 15), so it
 *  is reached off the module record rather than the generated type, and THROWS on
 *  a production bundle instead of failing silently (convention 11). Same shape as
 *  `enableDnsFakeProviderForTest` above. */
export function backupAuditSetClockOffsetForTest(offsetSecs: number): void {
  const fn = (wasm() as unknown as Record<string, unknown>).backupAuditSetClockOffsetForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'backupAuditSetClockOffsetForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as (o: number) => void)(offsetSecs);
}

/** Test-only: move the Nests trust facet's RENDER clock (grant liveness and the
 *  auto-renew due decision; never the mint clock) — the wasm half of the
 *  `trust_facet_advance_clock` e2e command (`$lib/trust-clock-e2e`), so a lapse
 *  journey reaches `expiring soon` / `paused` without waiting out the real
 *  ~90-day window (testing.md § convention 14).
 *
 *  ⚠ Module-wide, and nothing auto-resets it: pass `0` once the lapse
 *  assertions are done.
 *
 *  Same two-flavor discipline as `backupAuditSetClockOffsetForTest` above: the
 *  export exists only in the `wasm-core-test` chunk, so it is reached off the
 *  module record and THROWS on a production bundle (convention 11). */
export function trustSetClockOffsetForTest(offsetSecs: number): void {
  const fn = (wasm() as unknown as Record<string, unknown>).trustSetClockOffsetForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'trustSetClockOffsetForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as (o: number) => void)(offsetSecs);
}

/** Which input widget a `feed-rule-type-select` row's `(value, required)` fields need —
 *  the JS shape of `fauna_client_feed::RuleInputKind`. `Toggle` reads `required` and
 *  ignores `value`; `TextAndNumber` (the label rules) takes both a category `value` and
 *  a 0–10 threshold count. */
export type RuleInputKind = 'Text' | 'Number' | 'Toggle' | 'TextAndNumber';

/** One create-feed rule-type option — the JS shape of `fauna_client_feed::RuleTypeOption`.
 *  `value` is the wire `FilterRule` variant name (never localized — ui.yaml pins it as the
 *  select's value); `label` is the picker's localized text. */
export interface RuleTypeOption {
  value: string;
  label: LocalizedText;
  input_kind: RuleInputKind;
}

// The create-feed rule-builder's shared catalog + label fns (over `fauna_client_feed`) —
// one source for the picker options, the added-rule summary chip, and the
// required/excluded toggle label, so web stops re-deriving a local rule-type map + a
// hand-rolled `ruleLabel` switch. See `docs/goal/ui/feed.md` § Where logic lives ->
// Feed rule-builder presentation.
export function ruleTypeOptions(): RuleTypeOption[] {
  return wasm().ruleTypeOptions() as RuleTypeOption[];
}

/** One built-in create-feed factor option — the JS shape of `fauna_client_feed::FactorOption`.
 *  `value` is the factor key (never localized — the select's value); `label` the picker text. */
export interface FactorOption {
  value: string;
  label: LocalizedText;
}

/** The built-in head of `feed-factor-select` (`engagement`, `trending`) — offered before
 *  the caller's labeler and trained-topic factors (`docs/goal/ui/feed.md` § Where logic
 *  lives → Feed factor-picker built-ins). */
export function builtinFactorOptions(): FactorOption[] {
  return wasm().builtinFactorOptions() as FactorOption[];
}

export function ruleSummaryLabel(ruleType: string, value: string, required: boolean): LocalizedText {
  return wasm().ruleSummaryLabel(ruleType, value, required) as LocalizedText;
}

export function ruleRequiredLabel(required: boolean): LocalizedText {
  return wasm().ruleRequiredLabel(required) as LocalizedText;
}

/** Whether the staged `(inputKind, value, threshold)` create-feed rule inputs are
 *  complete enough to enable `feed-add-rule-button` — apple's `FeedCreateForm
 *  .canAddRule`, lifted (`docs/goal/ui/feed.md` § Add-rule gating). `inputKind` is
 *  a `ruleTypeOptions()` row's own `input_kind` field, passed straight through. */
export function canAddRule(inputKind: RuleInputKind, value: string, threshold: string): boolean {
  return wasm().canAddRule(inputKind, value, threshold) as boolean;
}

/** Short display hex for a byte id (first 4 bytes → 8 lowercase hex) — the backup
 *  restore-source label. Twin of native's `hex_short`; pass a `Uint8Array` (web's
 *  wire `source_member_id` already arrives as one). */
export function hexShort(bytes: Uint8Array): string {
  return wasm().hexShort(bytes) as string;
}

/** Full display hex for a byte id (every byte → lowercase hex) — the
 *  actor/member-id fallback label shown when no handle is available. Twin of
 *  native's `hex_full`; pass a `Uint8Array` (web's wire actor id is `number[]`
 *  → normalize with `toBytes(...)` from `$lib/hex`). */
export function hexFull(bytes: Uint8Array): string {
  return wasm().hexFull(bytes) as string;
}

/** Canonical short display form for a long hex id — 12-char prefix + `…`; ids of
 *  12 or fewer chars pass through unchanged. Twin of native's `short_id`.
 *  See `docs/goal/behavior/value-formatting.md` § Short id. */
export function shortId(hex: string): string {
  return wasm().shortId(hex) as string;
}

/** Build the canonical signed `(ContactRequest, Post)` tuple for an outbound
 *  knock (contact request), ready for the `fauna.inbox.send` kind. Twin of
 *  native's `build_knock_payload` — the `"Knock"` wire sentinel lives in shared
 *  Rust, never as a per-app literal. Both ids are hex.
 *  See `docs/goal/architecture/api-layers.md` § Contacts & Knocks. */
export function buildKnockPayload(secretHex: string, toHex: string, nodeUrl: string): Uint8Array {
  return wasm().buildKnockPayload(secretHex, toHex, nodeUrl) as Uint8Array;
}

/** Head…tail elision of a long hex `nest_actor_id` for a box-recovery row label
 *  (first 8 chars + `…` + last 8; ids of 20 or fewer chars pass through). Twin of
 *  native's `short_nest_id` — distinct from `shortId`, which is a 12-char prefix
 *  with no tail. See `docs/goal/behavior/value-formatting.md` § Short nest id. */
export function shortNestId(id: string): string {
  return wasm().shortNestId(id) as string;
}

/** The account-switcher row title: the cached handle when present and non-empty,
 *  else the canonical `shortId` of the actor id. Twin of native's
 *  `account_display_label` — owns the empty-string-handle fallback web's
 *  `handle ?? actor_id.slice(0, 12)` had drifted from. Pass `null`/`undefined`
 *  for an absent handle. See `docs/goal/behavior/value-formatting.md`
 *  § Account display label. */
export function accountDisplayLabel(handle: string | null | undefined, actorId: string): string {
  return wasm().accountDisplayLabel(handle ?? undefined, actorId) as string;
}

/** Whole-percent (half-up) display of a moderation classifier's confidence.
 *  Twin of native's `confidence_percent` — owns the `(per_mille + 5) / 10`
 *  rounding so web can't drift to `.toFixed(0)`/`Math.round(* 100)` truncation.
 *  Input is per-mille (0..=1000, the dag-cbor wire form); web's local classifier
 *  holds a float, so quantize with `Math.round(confidence * 1000)` at the call
 *  site. See `docs/goal/behavior/value-formatting.md` § Confidence percent. */
export function confidencePercent(perMille: number): number {
  return wasm().confidencePercent(perMille) as number;
}

/** A `{used_bytes, max_bytes}` usage pair reduced to a `0..=1` bar-fill fraction.
 *  Twin of native's `quota_fraction` — guards `max_bytes <= 0` (returns `0`, no
 *  divide-by-zero) and clamps to `1` when over-quota, closing a real `NaN`-width
 *  bug the hand-rolled `used / max` had on a zero-quota row. See
 *  `docs/goal/behavior/value-formatting.md` § Quota fraction. */
export function quotaFraction(usedBytes: number, maxBytes: number): number {
  return wasm().quotaFraction(usedBytes, maxBytes) as number;
}

/** Whole-percent (rounded) sibling of {@link quotaFraction}, for a bar-fill
 *  `width` percentage or a text label. Twin of native's `quota_percent`. */
export function quotaPercent(usedBytes: number, maxBytes: number): number {
  return wasm().quotaPercent(usedBytes, maxBytes) as number;
}

/** Total page count for a list paginated by (`pageSize`, `total` items),
 *  ceil-divided and floored to `1` (an empty list still shows `"1 / 1"`).
 *  Twin of native's `total_pages`. `admin/users/+page.svelte`. See
 *  `docs/goal/behavior/value-formatting.md` § Pagination. */
export function totalPages(total: number, pageSize: number): number {
  return wasm().totalPages(total, pageSize) as number;
}

/** 1-based current page number from a 0-based `offset`. Twin of native's
 *  `current_page`. See {@link totalPages}. */
export function currentPage(offset: number, pageSize: number): number {
  return wasm().currentPage(offset, pageSize) as number;
}

/** The offset one page forward, or `undefined` at the last page. Twin of
 *  native's `next_page_offset` — replaces the hand-rolled
 *  `offset + PAGE_SIZE >= total ? undefined : offset + PAGE_SIZE` guard in
 *  `admin/users/+page.svelte`. See {@link totalPages}. */
export function nextPageOffset(offset: number, total: number, pageSize: number): number | undefined {
  return wasm().nextPageOffset(offset, total, pageSize) ?? undefined;
}

/** The offset one page back, or `undefined` at page 1. Twin of native's
 *  `prev_page_offset`. See {@link nextPageOffset}. */
export function prevPageOffset(offset: number, pageSize: number): number | undefined {
  return wasm().prevPageOffset(offset, pageSize) ?? undefined;
}

/** The create-feed factor-weight editor's decimal multiplier (`"2.0"`) → the wire's
 *  signed per-mille `weight_permille`. Twin of native's `parse_weight_permille`.
 *  Replaces the hand-rolled `Math.round(Number.parseFloat(t) * 1000)`, which drifted
 *  from linux/windows on two axes: `parseFloat` is lenient (`"2abc"` → `2000`, vs the
 *  `1000` baseline fallback) and JS `Math.round` is half-**up** (`-0.0025` → `-2`, vs
 *  `-3` half-away-from-zero). A negative weight is a designed case — a strong-negative
 *  factor sinks an item. See `docs/goal/behavior/value-formatting.md` § Factor weight. */
export function parseWeightPermille(text: string): number {
  return wasm().parseWeightPermille(text) as number;
}

/** The inverse of {@link parseWeightPermille}: a wire `weight_permille` → the
 *  create-feed factor chip's display multiplier (`"{name} × {weight}"`). Rounds
 *  to 2 decimal places and strips trailing zeros, so a whole multiplier reads
 *  `"1"`, never `"1.00"`. Replaces the hand-rolled
 *  `(w / 1000).toFixed(2).replace(/\.?0+$/, '')`. Twin of native's
 *  `format_weight_permille`. See `docs/goal/behavior/value-formatting.md` §
 *  Factor weight. */
export function formatWeightPermille(weightPermille: number): string {
  return wasm().formatWeightPermille(weightPermille) as string;
}

/** Shared enforcement-action label for a moderation-queue row's `action`
 *  discriminant (`u8`). Twin of native's `obligation_action_label` — the
 *  rejected/quarantined/suppressed/rate-limited/logged/labeled/flagged →
 *  `LocalizedText` map, so no client hard-codes the action wording. Resolve with
 *  `resolveLocalized`. See `docs/goal/behavior/moderation.md` § Layout & flow (the
 *  optional enforcement-action column on a server obligation row). */
export function obligationActionLabel(action: number): LocalizedText {
  return wasm().obligationActionLabel(action) as LocalizedText;
}

/** One post-decrypt **local detection** — the client's own classification of a
 *  message body only it can read (`moderation.md` § State & data shape). The shared
 *  wire-aligned shape: per-mille confidence (`0..=1000`, the dag-cbor float ban) and
 *  a microsecond-epoch timestamp, matching `ObligationAction`, so the two queue
 *  sources merge and sort consistently. Produced by the Rust receive loop's classify
 *  hook and read back via `moderationLocalDetections()` — never hand-built in TS. */
export interface LocalDetection {
  content_id: string;
  content_type: string;
  category: string;
  confidence_per_mille: number;
  timestamp: number;
}

/** One row of the merged moderation queue. `action` is `null` on a local detection
 *  (the client classifies, it never *enforces* — the blank action column of
 *  `moderation.md` § Layout & flow) and the enforcement discriminant on a server
 *  row; `source` says which half it came from. */
export interface QueueRow {
  content_id: string;
  content_type: string;
  category: string;
  confidence_per_mille: number;
  action: number | null;
  timestamp: number;
  source: 'Server' | 'Local';
}

/** The moderation queue = the server's `fauna.moderation.actions` obligation rows
 *  **∪** the client's own post-decrypt local detections, deduped by `content_id`
 *  (server row wins — it carries the enforcement `action`) and newest-first.
 *  Twin of the native UniFFI `moderation_queue` façade over the one shared
 *  `fauna_client_moderation::merge_queue`, so the dedupe/sort rule cannot drift
 *  between web and the natives (priority #2). Pure — no nest hop.
 *  See `docs/goal/behavior/moderation.md` § Layout & flow. */
export function moderationQueue(server: ObligationAction[], local: LocalDetection[]): QueueRow[] {
  return wasm().moderationQueue(server, local) as QueueRow[];
}

/** The visible **legal-takedown tombstone** a client renders in place of a post's
 *  body once it has been taken down under a legal obligation (`moderation.md` §
 *  Categories & enforcement item 1 — "Removed under legal obligation
 *  ({reference})"). Twin of native's `legalTakedownTombstone` over the one shared
 *  `fauna_core::obligation::legal_takedown_tombstone` — no client hand-rolls the
 *  string. Resolve with `resolveLocalized`. Rendered by the quoted-post embed when
 *  `RenderBlock::QuotedPost.legal_takedown_ref` is set (feed manager surfaces it
 *  on the `fauna.posts.get` withheld-body reply). */
export function legalTakedownTombstone(reference: string): LocalizedText {
  return wasm().legalTakedownTombstone(reference) as LocalizedText;
}

/** `fauna_core::scoring::muted_keywords_collapse` — does a decrypted
 *  conversation `body` collapse behind the user's muted keywords (a
 *  case-insensitive substring match of a term muted at the full penalty; a
 *  softer weight only demotes in a ranked feed; `false` for an empty list)?
 *  Twin of native's `matches_muted_keywords`. The Conversations page calls this
 *  at render, post-decrypt, to collapse a bubble behind `dm-message-muted`
 *  (`moderation.md` § Muted keywords; `content-moderation-and-ranking.md`
 *  § Composition) — a hide/collapse, NOT a spam-queue flag. */
export function matchesMutedKeywords(body: string, mutedKeywords: MutedKeyword[]): boolean {
  return wasm().matchesMutedKeywords(body, mutedKeywords) as boolean;
}

/** The exact acknowledge phrase the immediate-delete modal requires the user to
 *  type — the protocol constant `IMMEDIATE_DELETE_ACK_TEXT` the nest checks
 *  byte-for-byte (NOT an i18n string: locale-independent). Twin of native's
 *  `immediate_delete_ack_text()`. Requires `ensureWasm()` first (sync). See
 *  `docs/goal/ui/backups.md` § User actions. */
export function immediateDeleteAckText(): string {
  return wasm().immediateDeleteAckText() as string;
}

/** The Backups immediate-delete friction-bar predicate (`backups.md`
 *  Architectural rule 4): true only when both the retyped snapshot id and the
 *  acknowledge phrase match exactly and no delete is already in flight.
 *  `targetId` `''` means no snapshot selected. Shared with windows/android/
 *  apple/linux so every app computes the identical boolean. */
export function immediateDeleteButtonEnabled(
  deleting: boolean,
  confirmId: string,
  targetId: string,
  acknowledgeTyped: string,
): boolean {
  return wasm().immediateDeleteButtonEnabled(
    deleting,
    confirmId,
    targetId,
    acknowledgeTyped,
  ) as boolean;
}

/** A `restore-snapshot-select` option's label: `"{kind} (#{id})"`, kind first
 *  so two same-day snapshots are told apart by what they hold. `messageKind`
 *  `null`/`undefined` (a folder snapshot) reads as an empty kind. Shared with
 *  linux/tui/android/apple/windows so every app renders identical text. */
export function snapshotRestoreOptionLabel(
  messageKind: string | null | undefined,
  id: number,
): string {
  return wasm().snapshotRestoreOptionLabel(messageKind ?? undefined, id) as string;
}

// Shared event-attendee row projection. `fauna_core::ical::attendee_display`
// (over wasm) derives the three `AttendeeRow` display strings — the CN→email
// fallback display name, the generated monogram initial, and the
// email-beneath visibility — the SAME projection the native apps call via
// UniFFI `attendeeDisplay`, so web stops hand-rolling `attendeeMonogram` and
// `att.name || att.email` (which drifted: web/apple/android showed the email
// twice when the CN *was* the email; windows showed `?` for an email-only
// attendee). `secondary_email` is `null` unless the name is a real, distinct CN.
// See `docs/goal/ui/events.md` § Attendee list presentation.
export interface AttendeeDisplay {
  display_name: string;
  monogram: string;
  secondary_email: string | null;
}

export function attendeeDisplay(name: string, email: string): AttendeeDisplay {
  return wasm().attendeeDisplay(name, email) as AttendeeDisplay;
}

// Shared attendee RSVP status→label map. `fauna_core::ical::rsvp_status_label`
// (over wasm) owns the canonical `going`/`interested`/`tentative`/`declined`/
// `waitlisted`/`invited` → `events.rsvp.*` map (unknown → capitalized verbatim) the
// native apps consume via UniFFI — so web stops hand-rolling its own
// `capitalizeStatus` (which bypassed i18n). `status` is the verbatim projected
// status string the attendee roster already carries. Returns a `LocalizedText` the
// SPA resolves via `resolveLocalized`; the trailing color stays a web-local map.
// See `docs/goal/ui/events.md` § Attendee list presentation.
export function rsvpStatusLabel(status: string): LocalizedText {
  return wasm().rsvpStatusLabel(status) as LocalizedText;
}

// Shared reminder preset→label map. `fauna_core::ical::reminder_label` (over
// wasm) owns the canonical `PT15M`/`PT1H`/`P1D` → `events.reminder.*` map (a
// non-preset offset falls back to the raw value rendered verbatim) the native
// apps consume, so web stops hand-rolling its own `REMINDER_PRESETS` label
// strings + `reminderLabel`. Returns a `LocalizedText` the SPA resolves via
// `resolveLocalized`. See `docs/goal/ui/events.md` § Reminders.
export function reminderLabel(offset: string): LocalizedText {
  return wasm().reminderLabel(offset) as LocalizedText;
}

/** One reminder preset option — the JS shape of `fauna_core::ical::reminder_presets`.
 *  `value` is the ISO-8601 offset the `<select>` writes (the cross-app
 *  `select(id, "PT1H")` contract — never localized); `label` is the picker's
 *  localized text, resolved the same way as `reminderLabel`. */
export interface ReminderPresetOption {
  value: string;
  label: LocalizedText;
}

// `fauna_core::ical::reminder_presets` → the canonical `PT15M`/`PT1H`/`P1D`
// reminder preset catalog in picker order, so web stops hand-rolling its own
// `REMINDER_PRESETS` value array. See `docs/goal/ui/events.md` § Reminders.
export function reminderPresets(): ReminderPresetOption[] {
  return wasm().reminderPresets() as ReminderPresetOption[];
}

// Shared contact relationship status→label map. `fauna_core::format::contact_status_label`
// (over wasm) owns the canonical `pending`/`accepted`/`confirmed`/`blocked` →
// `common.*` map (unknown → capitalized verbatim) the native apps consume via
// UniFFI — so web stops rendering the raw lowercase `contact.status` string. `status`
// is the verbatim status the contact row already carries. Returns a `LocalizedText`
// the SPA resolves via `resolveLocalized`; the status icon/color stays a web-local
// render. See `docs/goal/ui/contacts.md` § Where logic lives → Status badge text.
export function contactStatusLabel(status: string): LocalizedText {
  return wasm().contactStatusLabel(status) as LocalizedText;
}

// Shared Nostr Connect (bunker) roster-row label decisions
// (`fauna_core::format::{bunker_app_label,bunker_last_used_label}` over wasm) —
// a `nostr-bunker-app-item` row's primary label (own label verbatim, else a
// status-derived placeholder) and last-used sub-label (never used, or the
// caller's own already-formatted time). See nostr.md § The nest as the
// user's NIP-46 signer.
export function bunkerAppLabel(label: string, status: string): LocalizedText {
  return wasm().bunkerAppLabel(label, status) as LocalizedText;
}

export function bunkerLastUsedLabel(formattedTime: string | undefined): LocalizedText {
  return wasm().bunkerLastUsedLabel(formattedTime) as LocalizedText;
}

// The §4 provider-status and §5 claim-status label decisions MOVED to
// `$lib/payments` — see `wasmCoreModule` above.

// Shared task-delegation runner/option label decision
// (`fauna_core::delegation::{runner_label,option_label}` over wasm), resolved
// through `$lib/task-delegation`'s `runnerText`/`pinOptionLabel` (participants.md
// § Task delegation). `labels` crosses as a plain object — `from_js`'s HashMap
// deserialize walks object entries, not a JS `Map`.
export function taskDelegationRunnerLabelRaw(
  runner: RunnerStatus,
  labels: Map<string, string>,
): LocalizedText {
  return wasm().taskDelegationRunnerLabel(runner, Object.fromEntries(labels)) as LocalizedText;
}

export function taskDelegationOptionLabelRaw(
  option: PinOption,
  labels: Map<string, string>,
): LocalizedText {
  return wasm().taskDelegationOptionLabel(option, Object.fromEntries(labels)) as LocalizedText;
}

// Shared conversation thread label display. `fauna_core::format::thread_label_display`
// (over wasm) owns the empty-label fallback: a blank/whitespace-only label resolves to
// the canonical `conversations.detail.no_subject` key (`(no subject)`), a non-empty
// label rides verbatim — so web stops rendering an empty `thread.label`/`detail.label`
// as blank (a latent bug) and shares the native apps' fallback via UniFFI. Returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`; the raw label stays the
// filter/sort/rename value. See `docs/goal/ui/conversations.md` § Where logic lives.
export function threadLabelDisplay(label: string): LocalizedText {
  return wasm().threadLabelDisplay(label) as LocalizedText;
}

/** One picker option for the `unknown_sender_mail` / `feed_sources`
 *  reach-policy knobs — the JS shape of
 *  `fauna_core::format::{unknown_sender_options,feed_sources_options}`.
 *  `value` is the wire value the knob is ultimately saved as; `label` is the
 *  picker's localized text. */
export interface ReachPolicyOption {
  value: string;
  label: LocalizedText;
}

// Shared `unknown_sender_mail` / `feed_sources` reach-policy catalogs
// (`fauna_core::format::{unknown_sender_options,feed_sources_options,
// unknown_sender_label,feed_sources_label,reach_policy_summary}` over wasm) —
// the single source of the picker option *set*, their fail-closed label
// resolution, and the four-line read-only policy summary, also consumed by
// windows/macos/ios/android over UniFFI. See
// `docs/goal/behavior/family-safety.md` § Where logic lives.
export function unknownSenderOptions(): ReachPolicyOption[] {
  return wasm().unknownSenderOptions() as ReachPolicyOption[];
}

// See [unknownSenderOptions].
export function feedSourcesOptions(): ReachPolicyOption[] {
  return wasm().feedSourcesOptions() as ReachPolicyOption[];
}

// One bridge-settings boolean toggle, as [nostrContentToggleOptions] hands it
// over.
export interface BridgeToggleOption {
  // The `fauna.bridges.set_settings` key, and the key the current value is read
  // back under.
  key: string;
  // The `ui.yaml` element id the toggle carries (`data-testid`).
  ui_id: string;
  // What the toggle shows when the bridge reports no value for [key] — the
  // nest's own default. Spelled `default_on`, not `default`, because `default`
  // is a keyword in C# and Swift and this field name is shared with those apps.
  default_on: boolean;
  // The row's title.
  label: LocalizedText;
  // The row's explanatory second line, where the vocabulary has one.
  subtitle: LocalizedText | null;
}

// The shared Nostr content-toggle catalog
// (`fauna_client_bridges::nostr_content_toggle_options` over wasm) — the single
// source of the five flags' element ids, wire keys, defaults and labels, in
// render order. `nostr.md` § Where logic lives. Before this, this component
// spelled the same five keys three times over (read, write, label render).
// Mirrors [roleAddressOptions].
export function nostrContentToggleOptions(): BridgeToggleOption[] {
  return wasm().nostrContentToggleOptions() as BridgeToggleOption[];
}

// One rung of the Bluesky integration-depth ladder, as
// [atprotoDepthLevelOptions] hands it over.
export interface DepthLevelOption {
  // The level's wire spelling — what the snapshot reports and what
  // `select_level` takes.
  level: string;
  // The `ui.yaml` element id this rung carries (`data-testid`).
  ui_id: string;
  title: LocalizedText;
  description: LocalizedText;
  // Whether entering this rung is subject to the hosted gate. Replaces the
  // per-app `level.startsWith('hosted')` sniff.
  hosted: boolean;
}

// The shared Bluesky depth-rung catalog
// (`fauna_atproto_settings_machine::depth_level_options` over wasm) — the
// single source of the four rungs, their order, ids and copy.
// `atproto.md` § Where logic lives already named the machine the owner of
// "level logic … all of it"; every app carried its own copy until this door.
// Mirrors [roleAddressOptions].
export function atprotoDepthLevelOptions(): DepthLevelOption[] {
  return wasm().atprotoDepthLevelOptions() as DepthLevelOption[];
}

// One overridable RFC 2142 role address, as [roleAddressOptions] hands it over.
export interface RoleAddressOption {
  // The `role_address_overrides` storage key — also the reserved local-part,
  // the `<key>@` caption, and the `admin-dns-domain-role-address-<key>-select`
  // element-id suffix.
  key: string;
  // The tag the `SetRoleAddress` action carries and
  // `role_address_overrides[].role` is matched against.
  kind: string;
}

// The shared overridable-role catalog
// (`fauna_client_mail_settings::local_domains::role_address_options` over wasm)
// — the single source of the four roles, their render order, their storage keys
// and their action tags. linux and tui reach the same table by calling
// `RoleAddressKind::as_storage_key` directly. `mail-multidomain.md`
// § Per-domain role-address routing. Mirrors [unknownSenderOptions].
export function roleAddressOptions(): RoleAddressOption[] {
  return wasm().roleAddressOptions() as RoleAddressOption[];
}

// The shared `fcrdns_mode` picker catalog
// (`fauna_client_mail_settings::admin_policy::fcrdns_mode_options` over wasm) —
// the single source of the three values and their render order. tui and linux
// reach the same table via `FcrdnsMode::ORDER`, and the Go MTA's
// `ParseFCrDNSMode` is pinned against the same tokens (the wire value is a
// policy the bridge switches on, not just a picker's value —
// `mail-policy-config.md` § Inbound hardening). Mirrors [unknownSenderOptions].
export function fcrdnsModeOptions(): ReachPolicyOption[] {
  return wasm().fcrdnsModeOptions() as ReachPolicyOption[];
}

// The shared IMAP `delete_nonempty` picker catalog — the same page's other
// raw-value picker. See [fcrdnsModeOptions].
export function imapDeleteNonemptyOptions(): ReachPolicyOption[] {
  return wasm().imapDeleteNonemptyOptions() as ReachPolicyOption[];
}

// The localized label for a stored `unknown_sender_mail` wire value, failing
// closed to `hold` for anything unrecognized — never the permissive `allow`.
export function unknownSenderLabelRaw(value: string): LocalizedText {
  return wasm().unknownSenderLabel(value) as LocalizedText;
}

// The localized label for a stored `feed_sources` wire value, failing closed
// to `block`.
export function feedSourcesLabelRaw(value: string): LocalizedText {
  return wasm().feedSourcesLabel(value) as LocalizedText;
}

// `unknown_peer_dm` (family-safety.md § The bridge-DM gate) — same shape as
// [unknownSenderOptions]/[feedSourcesOptions].
export function unknownPeerDmOptions(): ReachPolicyOption[] {
  return wasm().unknownPeerDmOptions() as ReachPolicyOption[];
}

// The localized label for a stored `unknown_peer_dm` wire value, failing
// closed to `hold` — never the permissive `allow`.
export function unknownPeerDmLabelRaw(value: string): LocalizedText {
  return wasm().unknownPeerDmLabel(value) as LocalizedText;
}

/** One rendered line of the read-only `family-policy-summary` — the JS shape
 *  of `fauna_core::format::PolicySummaryLine`. */
export interface PolicySummaryLine {
  label: LocalizedText;
  value: LocalizedText;
}

// The four-line read-only reach-policy summary the supervised side sees, in
// the ratified display order. Both string knobs fail closed via
// [unknownSenderLabelRaw] / [feedSourcesLabelRaw].
export function reachPolicySummaryRaw(
  contactApproval: boolean,
  unknownSenderMail: string,
  federationContact: boolean,
  feedSources: string,
): PolicySummaryLine[] {
  return wasm().reachPolicySummary(
    contactApproval,
    unknownSenderMail,
    federationContact,
    feedSources,
  ) as PolicySummaryLine[];
}

// The full read-only ward policy summary — the four reach-knob lines PLUS one
// line per non-inherit guardian content floor and, when on, the Notify line
// (`ReachPolicy::summary_lines()` over wasm). Pass the whole wire `ReachPolicy`
// object off `familyStatus()`. Superset of [reachPolicySummaryRaw], which showed
// only the four v1 knobs — `family-safety.md` § Content policy (ward transparency).
export function reachPolicySummaryFull(policy: unknown): PolicySummaryLine[] {
  return wasm().reachPolicySummaryFull(policy) as PolicySummaryLine[];
}

// The canonical guardian content-floor picker catalog (`inherit | collapse |
// block`, `fauna_core::format::content_floor_options` over wasm) — the content
// pillar's twin of [unknownSenderOptions]. `family-safety.md` § Content policy.
export function contentFloorOptions(): ReachPolicyOption[] {
  return wasm().contentFloorOptions() as ReachPolicyOption[];
}

// The localized label for a stored guardian content-floor wire value, failing
// closed to `block` for anything unrecognized.
export function contentFloorLabelRaw(value: string): LocalizedText {
  return wasm().contentFloorLabel(value) as LocalizedText;
}

/** A content label riding a post/message (`ContentLabelEntry`) — `category` and
 *  per-mille `confidence`, the reduced shape web holds. */
export interface ContentLabelEntry {
  category: string;
  confidence_per_mille: number;
}

/** A guardian content policy — a per-category floor (wire string values). */
export interface ContentPolicyValue {
  nsfw: string;
  spam: string;
  phishing: string;
  commercial: string;
}

/** What a render path should do with an item after composing the viewer's own
 *  thresholds with any guardian floor (`fauna_core::obligation::RenderVerdict`). */
export type ContentRenderVerdict = 'show' | 'badge' | 'collapse' | 'block';

// The client render verdict for a piece of content — the web twin of linux
// `content_policy::verdict_for` (`family-safety.md` § Content policy). Composes,
// strictest-wins, the viewer's OWN spam/phishing thresholds (→ collapse) with,
// when supervised, the guardian's per-category floor (collapse/block), entirely
// in shared Rust. `contentPolicy` is the object off `familyStatus()` (or
// null/undefined for an unsupervised viewer); the own thresholds are per-mille
// (`Math.round(prefs.threshold * 1000)`, undefined until preferences load).
export function contentRenderVerdict(
  labels: ContentLabelEntry[],
  contentPolicy: ContentPolicyValue | null | undefined,
  ownSpamPermille?: number,
  ownPhishingPermille?: number,
): ContentRenderVerdict {
  return wasm().contentRenderVerdict(
    labels,
    contentPolicy ?? null,
    ownSpamPermille,
    ownPhishingPermille,
  ) as ContentRenderVerdict;
}

/** The placeholder a region verdict paints in place of the item
 *  (`fauna_client_region::RegionPlaceholder`): the verb, the region, its
 *  authority's registry name and the authority's reason, verbatim. */
export interface RegionPlaceholder {
  verb: 'block' | 'collapse';
  region: string;
  authorityName: string;
  reason: string;
}

/** One item's render decision with the region composed in: the verdict, and
 *  the placeholder when the region drove it (`null` otherwise). */
export interface ContentRender {
  verdict: ContentRenderVerdict;
  placeholder: RegionPlaceholder | null;
  /** `block` because the viewer reported the item or its author
   *  (`moderation.md` § Corollary — block also hides): the surface paints
   *  "You reported this" in place of the body. */
  reported?: boolean;
}

/** `contentRenderForItem` — the render decision for one identified item
 *  honouring the viewer's own reports (`hiddenContent` is `loadHiddenContent`).
 *  Region policies are composed separately by the region plane; pass none. */
export function contentRenderForItem(
  hiddenContent: string[],
  itemId: string,
  authorId: string | null,
  labels: ContentLabelEntry[],
  contentPolicy: ContentPolicyValue | null | undefined,
  ownSpamPermille?: number,
  ownPhishingPermille?: number,
): { verdict: ContentRenderVerdict; reported: boolean } {
  return wasm().contentRenderForItem(
    hiddenContent,
    itemId,
    authorId ?? undefined,
    labels,
    contentPolicy ?? null,
    ownSpamPermille,
    ownPhishingPermille,
    [],
  ) as { verdict: ContentRenderVerdict; reported: boolean };
}

/** The settings region surface's read (`RegionPlane::view`); times are unix
 *  seconds. */
export interface RegionView {
  declared: { code: string; source: string; sourceLabelKey: string } | null;
  policies: {
    region: string;
    authorityName: string;
    sequence: number;
    issuedAt: number;
    state: 'applied' | 'inert' | 'malformed';
    inertVersion: number | null;
  }[];
  lastCheckedAt: number | null;
  stale: boolean;
}

/** The scorer input an item hands the region plane (its bundled scorers run
 *  over the item's own text, tags and media flag). */
export interface RegionItem {
  contentIdHex?: string | null;
  authorHex?: string | null;
  /** The key the viewer's own reports are matched on, when it is not the
   *  region's content id: a conversation message is reported by its plane
   *  record digest, not its `message_id` (`moderation.md` § App surface). */
  reportIdHex?: string | null;
  /** The author a report of this item hides, when the region's `authorHex`
   *  is deliberately null (a conversation message: the sender's actor id). */
  reportAuthorHex?: string | null;
  text: string;
  hashtags: string[];
  hasMedia: boolean;
}

/** The device's region plane — `WasmRegionPlane` (`libs/fauna-wasm/src/region.rs`,
 *  the web twin of the native `FfiRegionPlane`). */
export interface RegionPlaneHandle {
  refresh(client: WsRpcClient): Promise<Uint8Array | null>;
  refreshDue(): boolean;
  clearSession(): void;
  render(
    labels: ContentLabelEntry[],
    contentPolicy: ContentPolicyValue | null,
    ownSpamPermille: number | undefined,
    ownPhishingPermille: number | undefined,
    contentIdHex: string | null | undefined,
    authorHex: string | null | undefined,
    text: string,
    hashtags: string[],
    hasMedia: boolean,
    lang: string,
  ): ContentRender;
  view(): RegionView;
}

/** Open the region plane over the leaf (`navigator.language`, parsed in shared
 *  Rust) and the device record last persisted (`null` when none) — loaded
 *  ahead of the first fetch (`region-blocking.md` § How an app obtains its
 *  region's policy). */
export function openRegionPlane(languageTag: string | null, record: Uint8Array | null): RegionPlaneHandle {
  return new (wasm().WasmRegionPlane)(languageTag, record) as unknown as RegionPlaneHandle;
}

// The guardian-floor categories a piece of content triggers, for Guardian Notify
// counting (`family-safety.md` § Guardian Notify) — the web twin of the shared
// `guardian_enforced_categories`. Returns the subset of `nsfw|spam|phishing|
// commercial` whose guardian floor bites on this item; the ward's own-threshold
// collapses are excluded (Notify is a lens on the guardian's policy), so this takes
// only the guardian `contentPolicy`, never the viewer's thresholds. Same reduced
// `labels` shape as [contentRenderVerdict].
export function guardianEnforcedCategories(
  labels: ContentLabelEntry[],
  contentPolicy: ContentPolicyValue | null | undefined,
): string[] {
  return wasm().guardianEnforcedCategories(labels, contentPolicy ?? null) as string[];
}

// The ≤hourly Guardian Notify batch interval in seconds
// (`NOTIFY_REPORT_MIN_INTERVAL_SECS`), so web batches identically to native.
export function notifyReportMinIntervalSecs(): number {
  return wasm().notifyReportMinIntervalSecs() as number;
}

// The shared draft-autosave debounce window in milliseconds
// (`fauna_client_drafts::AUTOSAVE_DEBOUNCE`), so web's composer coalesces
// edits on the same cadence as every other app (priority #2 — one shared
// constant, no per-app drift; `docs/goal/behavior/reserved-folders.md` §
// Drafts Sync). Web's own e2e-mode override stays a deliberate test-mode
// variation, layered on top by each rail's own `draftSaveDebounceMs()`.
export function autosaveDebounceMs(): number {
  return wasm().autosaveDebounceMs() as number;
}

// The guardian's per-ward Guardian Notify readout line for one (category, count)
// notice — a `{label, value}` PolicySummaryLine (the localized category name + the
// "N flagged today" count), resolved like any other. Category + count only, never
// content (`fauna_core::format::content_notice_line` over wasm).
export function contentNoticeLine(category: string, count: number): PolicySummaryLine {
  return wasm().contentNoticeLine(category, count) as PolicySummaryLine;
}

// The ward's screen-time policy as it rides `familyStatus().policy.screen_time`
// (and, gated on a guardianship, `familyStatus().supervision.screen_time` —
// the copy the lock enforces) — bounds are minutes from the ward's local
// midnight, all fields optional
// (`family-safety.md` § Screen time).
export interface ScreenTimePolicyValue {
  window_start?: number | null;
  window_end?: number | null;
  daily_minutes?: number | null;
}

// The ward's `screen-time-lock` decision AND its `screen-time-lock-message` in
// ONE call (`fauna_core::screen_time::screen_lock_message` over wasm): `null`
// means render no lock, otherwise the `LocalizedText` to paint. Web asks this
// one question exactly as linux does, so the gating rule and the wording cannot
// drift between the two reference legs — no screen-time logic lives in JS.
// `nowLocalMinutes` is minutes since the device's local midnight;
// `usedTodayMinutes` is the day's cross-device total from the last
// `familyUsageReport` reply, `undefined` while not yet known.
export function screenLockMessage(
  policy: ScreenTimePolicyValue | null | undefined,
  nowLocalMinutes: number,
  usedTodayMinutes: number | undefined,
  guardianHandle: string,
): LocalizedText | null {
  return wasm().screenLockMessage(
    policy ?? null,
    nowLocalMinutes,
    usedTodayMinutes,
    guardianHandle,
  ) as LocalizedText | null;
}

// The screen-time usage readout line for one ward's day — a `{label, value}`
// PolicySummaryLine ("Screen time today" / "45 of 120 minutes"). The SAME call
// backs the guardian's `family-ward-usage-today` row and the ward's own summary,
// which is what makes the goal doc's transparency rule ("the ward's summary
// shows the same number") structural rather than a convention
// (`fauna_core::format::usage_today_line` over wasm).
export function usageTodayLine(
  usedMinutes: number,
  budgetMinutes: number | undefined,
): PolicySummaryLine {
  return wasm().usageTodayLine(usedMinutes, budgetMinutes) as PolicySummaryLine;
}

// The ward's screen-time heartbeat (`fauna_core::screen_time::UsageHeartbeat`
// over wasm) — a stateful handle the SPA holds for the session.
//
// Every rule that decides a child's screen time lives behind it in shared Rust:
// what counts as use, the report cadence, what a failed report owes, and how the
// nest's cross-device total combines with minutes this device has not sent yet.
// Web contributes only what a browser alone knows — the clock, the tab's
// visibility, and the UTC offset.
//
// All epoch values cross as plain JS numbers (`f64` on the Rust side): a wasm
// `i64` parameter would arrive as a `bigint` and throw on `Date.now() / 1000`.
export interface UsageHeartbeatHandle {
  setPolicy(policy: ScreenTimePolicyValue | null): void;
  isAccounting(): boolean;
  setActive(active: boolean, nowSecs: number): void;
  seedTotal(usageTodayMinutes: number | undefined): void;
  takeDue(nowSecs: number): number | undefined;
  reportSucceeded(day: number, dayTotalMinutes: number, nowSecs: number): void;
  reportFailed(): void;
  usedTodayMinutes(nowSecs: number): number | undefined;
  reset(): void;
}

export function newUsageHeartbeat(): UsageHeartbeatHandle {
  return new (wasm().UsageHeartbeat)() as UsageHeartbeatHandle;
}

// ── Engagement-cue capture: the shared tracker (engagement-cues.md § Cue
// vocabulary & derivation) — the wasm `WasmCueTracker`. The web shell that
// drives it is `$lib/feed-cues`; all bookkeeping is shared Rust.

/** One probe row — `fauna_feed::CueRow`. `height <= 0` means "not yet
 *  arranged": held, never read as 0-visibility. */
export interface CueRow {
  post_id: string;
  top: number;
  height: number;
  is_media: boolean;
  media_played_pm: number | null;
}

/** One finished exposure — `fauna_feed::CueObservation`, exactly
 *  `WasmFeedManager.recordObservation`'s arguments. */
export interface CueExposure {
  content_id: string;
  is_media: boolean;
  media_played_pm: number | null;
  dwell_ms_at_skip_visibility: number;
  dwell_ms_at_long_visibility: number;
  observed_at_ms: number;
}

export interface CueTrackerHandle {
  sample(
    rows: CueRow[],
    windowPostIds: string[],
    viewportStart: number,
    viewportEnd: number,
    monoNowMs: number,
    wallNowMs: number,
  ): CueExposure[];
  drainAll(wallNowMs: number): CueExposure[];
  free(): void;
}

/** A tracker for a container with the given leave model — `"hold-unmeasured"`
 *  for an eager/retained DOM list, `"absence-is-leave"` for a virtualizing one. */
export function newCueTracker(leaveModel: 'hold-unmeasured' | 'absence-is-leave'): CueTrackerHandle {
  return new (wasm().WasmCueTracker)(leaveModel) as unknown as CueTrackerHandle;
}

/** The shared sampling cadence (`CUE_SAMPLE_INTERVAL_MS`), never re-declared. */
export function cueSampleIntervalMs(): number {
  return wasm().cueSampleIntervalMs();
}

// A guardian-typed `"HH:MM"` window bound as minutes from local midnight, for
// the two `family-policy-screen-window-*-input`s. `null` for an empty input
// ("this bound is unset" — how a guardian clears the window); THROWS the reason
// string for anything unparseable, which the editor surfaces on `error-message`.
export function parseTimeOfDay(input: string): number | null {
  return (wasm().parseTimeOfDay(input) ?? null) as number | null;
}

// Minutes from local midnight rendered back as `"HH:MM"` — the inverse of
// [parseTimeOfDay], used to fill the editor from the stored policy.
export function formatTimeOfDay(minutesFromMidnight: number): string {
  return wasm().formatTimeOfDay(minutesFromMidnight) as string;
}

// A guardian-typed daily budget in whole minutes for
// `family-policy-screen-daily-minutes-input`. `null` for empty (no budget);
// THROWS outside `0..=1440`, the same bound the nest enforces at `policy.update`.
export function parseDailyMinutes(input: string): number | null {
  return (wasm().parseDailyMinutes(input) ?? null) as number | null;
}

// What a `family-approval-item` row should display verbatim, or `null` when
// the caller should render its own localized no-sender placeholder
// (`fauna_core::format::approval_display_text` over wasm —
// `docs/goal/behavior/family-safety.md` § Where logic lives).
export function approvalDisplayTextRaw(
  kind: string,
  peerAddress: string,
  peerHandle: string,
  summary: string,
  bridgeId: string,
  operation: string,
  target: string,
): string | null {
  return wasm().approvalDisplayText(
    kind,
    peerAddress,
    peerHandle,
    summary,
    bridgeId,
    operation,
    target,
  ) as string | null;
}

// Shared admin-nest host-OS-maintenance status label.
// `fauna_core::format::os_maintenance_status_label` (over wasm) owns the state→key
// decision (reboot-pending / updates-pending / up-to-date, from the `os_*` fields
// on `fauna.setup.status`) the native apps consume via UniFFI — so web shares it
// instead of its own ternary. Returns a `LocalizedText` the SPA resolves via
// `resolveLocalized`. See `installers/vps.md` § Host OS Maintenance § 4.
export function osMaintenanceStatusLabel(
  securityUpdatesPending: number,
  rebootPending: boolean,
): LocalizedText {
  return wasm().osMaintenanceStatusLabel(securityUpdatesPending, rebootPending) as LocalizedText;
}

// Shared contact-roster filter predicate. `fauna_core::format::contact_matches_filter`
// (over wasm) owns the `contacts-search-field` match: a case-insensitive substring of
// the trimmed query over handle / domain / hex actor-id (empty query matches all), so
// web filters the roster through the same predicate the native apps use instead of
// its own actor-id-only `.includes`. `handle`/`domain` are `undefined` for a federated
// peer. Local-only — never a nest query. See `docs/goal/ui/contacts.md` § Where logic lives.
export function contactMatchesFilter(
  query: string,
  handle: string | undefined,
  domain: string | undefined,
  actorId: string,
): boolean {
  return wasm().contactMatchesFilter(query, handle, domain, actorId) as boolean;
}

// Shared `profile-block-button` toggle label. `fauna_core::format::contact_toggle_block_label`
// (over wasm) owns the `is_blocked ? unblock : block` decision (`profile.unblock` /
// `profile.block`), so web shares the wording instead of its own `$derived` ternary —
// the same map all five apps that surface the button hand-roll. Returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`. The button *style* stays
// web-local. See `docs/goal/ui/profile.md` § Element table.
export function contactToggleBlockLabel(isBlocked: boolean): LocalizedText {
  return wasm().contactToggleBlockLabel(isBlocked) as LocalizedText;
}

// Shared block-state predicate. `fauna_core::format::contact_row_blocks_actor` (over
// wasm) owns the `profile-block-button` state derivation: a `blocked` roster row whose
// `peer_id` matches the target (case-insensitive hex) → blocked. Web folds it over
// `fauna.contacts.list` with `.some(..)` instead of its own
// `c.peer_id === id && c.status === 'blocked'` match. Local-only. See
// `docs/goal/ui/contacts.md` § Where logic lives → Unblock.
export function contactRowBlocksActor(
  rowPeerId: string,
  rowStatus: string,
  targetActorId: string,
): boolean {
  return wasm().contactRowBlocksActor(rowPeerId, rowStatus, targetActorId) as boolean;
}

// Shared `profile-follow-button` label. `fauna_core::format::follow_toggle_label` (over
// wasm) owns the toggle wording (`profile.following` when the viewer already follows,
// else `profile.follow`), so web shares it instead of its own ternary. The `isFollowing`
// derivation (subscription status / optimistic flip) stays web-local by ratified
// decision. See `docs/goal/ui/profile.md` § Where logic lives → Follow / unfollow.
export function followToggleLabel(isFollowing: boolean): LocalizedText {
  return wasm().followToggleLabel(isFollowing) as LocalizedText;
}

// Shared Devices `device-status` label. `fauna_core::format::device_status_label` (over
// wasm) owns the online→label map (`devices.online` when the device's `online` flag is
// set, else `devices.offline`), so web shares it instead of its own ternary — the map
// windows hand-rolled with hardcoded English. Returns a `LocalizedText` the SPA resolves
// via `resolveLocalized`. The status dot *color* stays web-local. See
// `docs/goal/ui/devices.md` § Where logic lives.
export function deviceStatusLabel(online: boolean): LocalizedText {
  return wasm().deviceStatusLabel(online) as LocalizedText;
}

// Shared Devices `device-folder-role-badge` chip label for one device place's
// three flags. `fauna_core::format::device_place_label` (over wasm) composes it
// from the SAME `devices.wizard.place_*` words the wizard's checkboxes use (one
// flag → that word; two/three → the `devices.place_two`/`place_three` template;
// none → `devices.place_none`). The templates' ARGUMENTS are i18n keys, so render
// it with `resolveLocalizedNested` — plain `resolveLocalized` paints raw keys.
// See `docs/goal/ui/devices.md` § Element table (`device-folder-role-badge`).
export function devicePlaceLabel(
  originates: boolean,
  accepts: boolean,
  applies_deletes: boolean,
): LocalizedText {
  return wasm().devicePlaceLabel(originates, accepts, applies_deletes) as LocalizedText;
}

/** Which of the three lifecycle controls an admin Users row offers. */
export interface AdminUserRowControls {
  /** `admin-users-suspend-button` — cut off now, no delete timeline. */
  suspend: boolean;
  /** `admin-users-evict-button` — the timed warn → suspend → delete ladder. */
  evict: boolean;
  /** `admin-users-cancel-eviction-button` — restore, from *either* entry point. */
  restore: boolean;
  /** `admin-users-make-admin-button` — grant the admin role. */
  make_admin: boolean;
  /** `admin-users-remove-admin-button` — revoke the admin role. */
  remove_admin: boolean;
}

// Shared admin Users row lifecycle controls. `fauna_client_admin::admin_user_row_controls`
// (over wasm) owns the rule — three eviction states crossed with the admin-role guard —
// so no client re-derives it. Two arms are load-bearing and a naive `eviction ? restore :
// evict` branch gets both wrong: Suspend STAYS offered on a mid-eviction `warning` row
// (it promotes the user and clears the pending delete — the no-user-data-loss direction),
// and an admin row offers NO entry control (the nest answers `fauna.admin.conflict`) yet
// still offers restore, so granting the role to a suspended user can't strand them.
// Pass a `fauna.admin.users.list` row. See `docs/goal/behavior/admin.md` § 2 Users →
// *Cutting a user off*.
// Pass the WHOLE row: the wasm side deserializes the full `AdminUser` projection, so a
// partial object is a runtime error, not a type error.
export function adminUserRowControls(user: AdminUser): AdminUserRowControls {
  return wasm().adminUserRowControls(user) as AdminUserRowControls;
}

// The option text an admin picker (guardian, invite-request) offers for `user`.
// `fauna_client_admin::admin_picker_option` (over wasm) owns the rule — the
// handle, falling back to the full actor hex for a handle-less account — so no
// client re-derives it (admin.md § 2 → *What identifies a user in an admin
// picker*). Pass the WHOLE row, same contract as `adminUserRowControls`.
export function adminPickerOption(user: AdminUser): string {
  return wasm().adminPickerOption(user) as string;
}

// Shared admin Users `admin-users-mail-serving-status` label.
// `fauna_core::format::mail_serving_status_label` (over wasm) owns the enabled→label map
// (`admin.users_page.serving_here` when the user's `mail_serving_enabled` flag is set, else
// `serving_disabled`), so web shares it instead of its own ternary — the same map
// windows/native consume. Returns a `LocalizedText` the SPA resolves via `resolveLocalized`.
// See `docs/goal/behavior/admin.md` § Where logic lives.
export function mailServingStatusLabel(enabled: boolean): LocalizedText {
  return wasm().mailServingStatusLabel(enabled) as LocalizedText;
}

/** One `admin-users-registration-mode-select` option — the JS shape of
 *  `fauna_client_admin::RegistrationModeOption`. `value` is the wire
 *  `RegistrationMode` string (never localized — what the select writes and
 *  what the cross-app `select(id, value)` e2e contract drives); `label`
 *  is the picker's localized text. */
export interface RegistrationModeOption {
  value: string;
  label: LocalizedText;
}

// The admin-users registration-mode picker's shared option catalog (over
// `fauna_client_admin`) — the single source of the `open`/`invite_required`/
// `closed` wire vocabulary + display order + label, so web stops re-listing
// the three `<option>`s by hand. See `docs/goal/behavior/admin.md` § Where
// logic lives.
export function registrationModeOptions(): RegistrationModeOption[] {
  return wasm().registrationModeOptions() as RegistrationModeOption[];
}

/** One age-band picker option — the JS shape of
 *  `fauna_client_admin::AgeBandOption`. `value` is the wire token (`not-set`
 *  or a band), what the select writes and what the cross-app
 *  `select(id, value)` e2e contract drives; `label` is the localized text. */
export interface AgeBandOption {
  value: string;
  label: LocalizedText;
}

// The two admission band pickers' shared catalog (`admin-users-invite-age-band-select`
// / `invite-request-row-age-band-select`: *not set* + the four bands, in the
// ratified order) plus the request-row seed and the band/claim/readout labels —
// all shared Rust, so web never spells the band vocabulary itself
// (`family-safety.md` § App surface → *Age-band surfaces*). Every label args
// key is a nested i18n key: resolve with `resolveLocalizedNested`.
export function ageBandOptions(): AgeBandOption[] {
  return wasm().ageBandOptions() as AgeBandOption[];
}

/** The request row's band-select seed: the applicant's claimed band when
 *  nameable, else the *not set* value. */
export function claimedAgeBandOption(claimed: string | null | undefined): string {
  return wasm().claimedAgeBandOption(claimed ?? undefined) as string;
}

/** `invite-request-row-age-claim` — total ("No app age verification" when none). */
export function ageClaimLabel(
  band: string | null | undefined,
  provenance: string | null | undefined,
): LocalizedText {
  return wasm().ageClaimLabel(band ?? undefined, provenance ?? undefined) as LocalizedText;
}

/** The two family readouts' line (`own` = the ward's own summary), or null
 *  when the band is unnamed — absent, never placeholdered. */
export function ageBandLine(band: string, provenance: string, own: boolean): LocalizedText | null {
  return (wasm().ageBandLine(band, provenance, own) as LocalizedText | null) ?? null;
}

/** A band's label (the `invite-code-item` echo), or null when unnamed. */
export function ageBandLabel(band: string): LocalizedText | null {
  return (wasm().ageBandLabel(band) as LocalizedText | null) ?? null;
}

// Shared `admin-dns` record-matrix verdict label. `fauna_core::format::dns_verdict_label`
// (over wasm) owns the verdict→key decision (`Ok`/`Missing`/`Mismatch` →
// `admin.dns.status_*`, else `status_checking`) the native apps consume via UniFFI —
// so web shares it instead of its own `statusLabel` switch. Pass `verdict?.status ?? ''`
// and `verdict?.observed ?? []` (an absent verdict reads as checking); `observed` is what
// public DNS actually served, which a `Mismatch` renders into the label. Returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`; the verdict CSS class stays
// web-local. See `docs/goal/behavior/value-formatting.md` § DNS verdict label.
export function dnsVerdictLabel(status: string, observed: string[]): LocalizedText {
  return wasm().dnsVerdictLabel(status, observed) as LocalizedText;
}

// Shared `admin-dns` served-cert health-state label. `fauna_core::format::cert_status_label`
// (over wasm) owns the state→key decision (`ValidTrusted` → `status_valid`, `Expiring` →
// `status_expiring`, else `status_on_floor`) the native apps consume via UniFFI — so
// web shares it instead of its own `certStatusText` switch. Only the state word — a badge
// wants `certStatusView`, which composes this with the sub-label that follows it. Returns
// a `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/behavior/value-formatting.md` § Cert status badge.
export function certStatusLabel(state: string): LocalizedText {
  return wasm().certStatusLabel(state) as LocalizedText;
}

// The global `connection-status` indicator's label for a `connectionState()` word.
// `fauna_core::format::connection_state_label` (over wasm) owns the state→key
// decision every app shares, so the SPA no longer re-decides it in a ternary —
// which is what let the fourth state (`unreachable` → "Cannot connect", a settled
// failure rather than a passing blip) fall into web's old "anything not
// connected/connecting is Disconnected" default arm. Returns a `LocalizedText` the
// SPA resolves via `resolveLocalized`. See transport.md § Connection-status indicator.
export function connectionStateLabel(state: string): LocalizedText {
  return wasm().connectionStateLabel(state) as LocalizedText;
}

// The `media-sort-select` option label. `fauna_core::format::media_sort_label`
// (over wasm) owns the value (`"name"`/`"size"`/`"date"`) → key decision the
// native apps share, so the SPA no longer re-decides it in a `switch`. Returns
// a `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/ui/media.md` § Layout & flow.
export function mediaSortLabel(value: string): LocalizedText {
  return wasm().mediaSortLabel(value) as LocalizedText;
}

// The `media-sort-direction` option label for a `descending` flag.
// `fauna_core::format::media_sort_direction_label` (over wasm) owns the
// bool → key decision.
export function mediaSortDirectionLabel(descending: boolean): LocalizedText {
  return wasm().mediaSortDirectionLabel(descending) as LocalizedText;
}

// `fauna_protocol::offline_class::affordance` — W4 (account-data-plane.md § Workstreams) phase 4's UI-desensitizing
// verdict for one wire kind at one connection state (`account-data-plane.md`
// § The offline-mutation contract → *How a surface asks*). `connection_state` is
// the same lowercase word `connectionStateLabel` takes, so the `connection-status`
// indicator and the gate cannot disagree about what "connected" means.
//
// The SPA must NEVER test a class itself: the rule makes three rulings that are
// easy to get backwards (only class 3 desensitizes; an *unregistered* kind stays
// available; only the *known* offline words count as offline), and re-deriving
// them here is the per-app copy priority #2 exists to prevent. `reason` is
// `undefined` exactly when `available` is true.
//
// Consumers do not call this directly — they bind it into the `use:offlineGate`
// action (`$lib/offline-gate`), which owns how a reactive tree obeys the verdict.
export function offlineAffordance(kind: string, connectionState: string): Affordance {
  return wasm().offlineAffordance(kind, connectionState) as Affordance;
}

// `fauna_protocol::offline_class::is_online` — the same rule as
// `offlineAffordance` above, for a caller holding a connection state but no
// kind: is this transport word one the gate treats as ONLINE?
//
// ⚠ Never replace a call to this with `state === 'connected'`. The gate's
// polarity is deliberately asymmetric — online unless the word is a *known*
// offline word, so a future state word leaves controls live rather than greying
// them on a guess — and an equality test inverts that ruling in the direction
// that HANGS a waiter forever on exactly the case the rule was built to
// tolerate. The word list has one owner, in Rust, and this is how TS reaches
// its verdict.
//
// The consumer is the e2e automation surface's `connection` observable
// (`$lib/e2e-automation`), which publishes the boolean beside the word so the
// harness never re-derives the list either.
export function connectionIsOnline(connectionState: string): boolean {
  return wasm().connectionIsOnline(connectionState) as boolean;
}

// The whole `admin-dns` served-cert badge. `fauna_core::format::cert_status_view` (over
// wasm) owns the state word AND which of the two mutually-exclusive sub-labels follows
// it: `(self-signed)` for the floor, or the expiry date for a trusted cert — never both,
// because a floor cert's own far-future expiry would read as reassurance on an untrusted
// cert. Web renders the text assembly and the CSS class; the decision is shared with the
// native apps over UniFFI. See `docs/goal/behavior/value-formatting.md`
// § Cert status badge.
export interface CertStatusView {
  state: LocalizedText;
  show_self_signed: boolean;
  expires_at_unix: number | null;
}
export function certStatusView(
  state: string,
  isFloor: boolean,
  notAfterUnix: number,
): CertStatusView {
  return wasm().certStatusView(state, isFloor, notAfterUnix) as CertStatusView;
}

// Shared `subscription-offer-status` badge label. `fauna_core::format::offer_status` +
// `offer_status_label` (over wasm) own the per-tier status derivation
// (`subscriptions.offer_status_*`, precedence Active>Pending>None) the native apps
// consume as an `OfferStatus` enum via UniFFI — so web shares the precedence + label map
// instead of its own inline `offerStatusLabel`. `statusTier` is the viewer's confirmed
// held tier (`status.get`, `null` if none); `pending` a transient post-click flag
// (`status.get` carries no pending discriminant). Web's Subscribe button is
// status-independent, so it needs only the label. Returns a `LocalizedText` the SPA
// resolves via `resolveLocalized`. See `docs/goal/ui/profile.md` § Layout & flow /
// `docs/goal/behavior/monetization.md` § Pillar 1.
export function offerStatusLabel(
  tierName: string,
  statusTier: string | null,
  pending: boolean,
): LocalizedText {
  return wasm().offerStatusLabel(tierName, statusTier ?? undefined, pending) as LocalizedText;
}

// Shared pending-bridge display name. `fauna_client_mail_settings::bridge_display_name`
// (over wasm) owns the canonical role→name map (MDA = "Mail & calendar bridge",
// MTA = "Mail bridge", unknown = "Bridge") that linux/windows already consume and
// android/apple are being migrated onto — so web stops hand-rolling its own
// `bridgeDisplayName`. Returns a `LocalizedText` the SPA resolves via
// `resolveLocalized`. See `docs/goal/architecture/apps/bridges.md` § Active bridges.
export function bridgeDisplayName(role: string): LocalizedText {
  return wasm().bridgeDisplayName(role) as LocalizedText;
}

// Shared Nostr signing-mode label. `fauna_client_bridges::nostr_key_source_label`
// (over wasm) owns the canonical `generated`/`imported`/`remote`/`nip07` →
// `nostr.account.mode_*` map (unknown/placeholder → verbatim) that linux already
// consumes — so the Nostr settings page stops rendering the raw stored `mode`
// enum. `mode` is the verbatim `fauna.bridges.list` `mode` field the snapshot
// already carries. Returns a `LocalizedText` the SPA resolves via
// `resolveLocalized`. See `docs/goal/ui/nostr.md` § User actions.
export function nostrKeySourceLabel(mode: string): LocalizedText {
  return wasm().nostrKeySourceLabel(mode) as LocalizedText;
}

// Shared Nostr link-*request*-mode label — the request-mode twin of
// `nostrKeySourceLabel`. `fauna_client_bridges::nostr_link_mode_label` (over
// wasm) owns the canonical `generate`/`import`/`remote`/`nip07` → label map
// (`generate`/`import_nsec`/`nip07` keys, `remote` reusing the stored-mode
// `nostr.account.mode_remote` key) that linux/tui/android/apple/windows
// already consume — so the `nostr-link-mode` picker stops hand-writing its
// own `t.nostr.link_account.*` lookups. Returns a `LocalizedText` the SPA
// resolves via `resolveLocalized`. See `docs/goal/ui/nostr.md` § Account
// linking → *Link-mode picker labels*.
export function nostrLinkModeLabel(mode: string): LocalizedText {
  return wasm().nostrLinkModeLabel(mode) as LocalizedText;
}

// Shared unified-Bridges-page membership predicate.
// `fauna_client_bridges::is_unified_bridges_page_bridge` (over wasm) owns the
// rule — Nostr has its own dedicated page, so it's excluded from the generic
// Bridges surface even though it rides the same `fauna.bridges.list` wire.
// See `docs/goal/behavior/bridges.md` § Scope.
export function isUnifiedBridgesPageBridge(id: string): boolean {
  return wasm().isUnifiedBridgesPageBridge(id) as boolean;
}

// Why a bridge's Link control is not actionable — `null` means it IS.
// `fauna_client_bridges::link_block` (over wasm) owns the rule, so web cannot
// drift from the other six apps on what a mode-less bridge means.
// `provider_error` carries the nest's own sentence: render it verbatim, never
// swapped for the localized generic. See `docs/goal/behavior/bridges.md`
// § Errors & edge cases.
export type BridgeLinkBlock =
  | { kind: 'provider_error'; message: string }
  | { kind: 'no_applicable_mode' };

export function bridgeLinkBlock(
  linked: boolean,
  error: string | null,
  applicableModes: number,
): BridgeLinkBlock | null {
  return wasm().bridgeLinkBlock(linked, error ?? undefined, applicableModes) as
    | BridgeLinkBlock
    | null;
}

// Does one declared link mode apply on `platform`? The shared platform
// string-match (`fauna_client_bridges::mode_applies`) — lifted 2026-08-15 from
// the seven per-app copies, so web cannot drift on which scoped modes it
// renders. Web passes 'web'; its nip07 extension probe stays local (a runtime
// browser capability), ANDed after this.
export function bridgeModeApplies(modePlatform: string | undefined, platform: string): boolean {
  return wasm().bridgeModeApplies(modePlatform, platform) as boolean;
}

// Shared mail-list member status label. `fauna_client_mail_settings::member_status_label`
// (over wasm) owns the canonical Subscribed/Unsubscribed label map that the native
// apps consume — so web stops hand-rolling its own `memberStatusBadge`. `status`
// is the serde variant-name string the members snapshot already carries. Returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/behavior/mail-mass-mailing.md`.
export function memberStatusLabel(status: string): LocalizedText {
  return wasm().memberStatusLabel(status) as LocalizedText;
}

// Shared mail-settings enum→badge label maps. `fauna_client_mail_settings`
// (over wasm) owns the canonical alias-kind / spam-training-label /
// spam-training-source / export-format maps the native apps consume — so web
// stops hand-rolling `kindBadge` / `labelBadge` / `sourceBadge` / `formatLabel`.
// Each arg is the serde variant-name string the matching snapshot already carries;
// each returns a `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/behavior/{mail-aliases,mail-spam,mail-export}.md`.
/** `mail-aliases-list-item-hits` count text (`mail_aliases.hits` /
 *  `mail_aliases.hits_with_last`) as a `LocalizedText` the caller resolves via
 *  `resolveLocalized`. The last-hit `date` is passed in pre-formatted (web keeps
 *  its JS locale date — a local-tz concern); only the template is shared. Twin of
 *  native's `alias_hits_label`. */
export function aliasHitsLabel(hitCount: number, lastHitDate: string | null): LocalizedText {
  return wasm().aliasHitsLabel(hitCount, lastHitDate ?? undefined) as LocalizedText;
}

export function aliasKindBadge(kind: string): LocalizedText {
  return wasm().aliasKindBadge(kind) as LocalizedText;
}
export function trainingLabelBadge(label: string): LocalizedText {
  return wasm().trainingLabelBadge(label) as LocalizedText;
}
export function trainingSourceBadge(source: string): LocalizedText {
  return wasm().trainingSourceBadge(source) as LocalizedText;
}
export function exportFormatLabel(format: string): LocalizedText {
  return wasm().exportFormatLabel(format) as LocalizedText;
}
// The `mail-import` wizard's two picker vocabularies, from the same shared map
// every other app reads (`import_source_kind_label` / `import_tls_mode_label`)
// — the export twin's precedent, so a seventh app cannot hand-roll a sixth copy.
export function importSourceKindLabel(kind: string): LocalizedText {
  return wasm().importSourceKindLabel(kind) as LocalizedText;
}
export function importTlsModeLabel(mode: string): LocalizedText {
  return wasm().importTlsModeLabel(mode) as LocalizedText;
}

// The Source step's ordered commit-then-Connect action list
// (`fauna_client_mail_settings::connect_actions`) — tui's and linux's shared
// builder, which web's own hand-rolled TS sequence named as its model without
// ever calling. Each returned action is exactly the shape `machine.dispatch`
// already accepts (`{ SetHost: { value } }` / the `"Connect"` unit string).
export function mailImportConnectActions(
  kind: string,
  host: string,
  port: string,
  username: string,
  password: string,
): unknown[] {
  return wasm().mailImportConnectActions(kind, host, port, username, password) as unknown[];
}

// Shared mail-credential kind label. `fauna_client_mail_settings::credential_kind_badge`
// (over wasm) owns the canonical Password/Bearer label map that the native apps
// consume — so web stops hard-coding the English `'Password'`/`'Bearer token'`
// strings (it had bypassed i18n). `kind` is the serde variant-name string
// (`'Plain'` / `'OAuthBearer'`) the credential row already carries. Returns a
// `LocalizedText` the SPA resolves via `resolveLocalized`. See
// `docs/goal/behavior/mail-credentials.md`.
export function credentialKindBadge(kind: string): LocalizedText {
  return wasm().credentialKindBadge(kind) as LocalizedText;
}

// Shared mail-settings status-indicator label. `fauna_client_mail_settings::
// settings_status_label` (over wasm) owns the `SettingsStatus`(+enabled) → key
// decision the native apps consume via UniFFI — so web shares it instead of
// its own `statusLabel` ternary. `status` is the snapshot's serde `SettingsStatus`
// (`'Idle'`/`'Syncing'`/`{ RotationInProgress: { credentials_remaining } }`),
// `undefined` before hydrate (→ `Idle`, reading `status_disabled` while
// `enabled` is false). Returns a `LocalizedText` the SPA resolves via
// `resolveLocalized`. See `docs/goal/ui/mail-settings.md` § the status indicator.
export function settingsStatusLabel(status: unknown, enabled: boolean): LocalizedText {
  return wasm().settingsStatusLabel(status, enabled) as LocalizedText;
}

// Web-content authoring view projections — `fauna_client_web::subdomain_view` +
// `fauna_core::web::apex_url` over wasm. The reserved-label rule + the
// `<handle>.<domain>` / apex URLs live in shared Rust (one source of truth with
// the nest's routing), so the web-settings + admin-web pages never re-derive them
// (priority #2). The native twin is `fauna-ffi`'s `webSubdomainView`/`webApexView`.
// See `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting /
// § Published-post management.
export type SubdomainDisabledReason = 'NoHandle' | 'ReservedLabel' | 'NoServingDomain';
export interface SubdomainView {
  enabled: boolean;
  /** The live `https://<handle>.<domain>/` URL, or `null` when it can't serve
   *  (see `disabled_reason`). */
  url: string | null;
  disabled_reason: SubdomainDisabledReason | null;
}

/** The `web-settings` subdomain-toggle render state for `(enabled, handle, domain)`.
 *
 *  ⚠ `domain` MUST be the nest's own serving domain (`webServingDomain` on the
 *  RPC client), never the identity store's cached sign-in `domain`: that value
 *  is the `"localhost"` placeholder on a domainless nest, and `<handle>.localhost`
 *  is exactly the host the nest's resolver never strips. */
export function webSubdomainView(
  enabled: boolean,
  handle: string | null,
  domain: string,
): SubdomainView {
  return wasm().webSubdomainView(enabled, handle ?? undefined, domain) as SubdomainView;
}

/** The `https://<domain>/` URL the apex serves at, for the `admin-web` info line. */
export function webApexUrl(domain: string): string {
  return wasm().webApexUrl(domain) as string;
}

// ── Published-post management: the link surface (web-content-hosting.md
//    § Published-post management) — shared by the `web-settings`
//    Published-posts section and the feed ⋯-overflow verbs. ────────────────

export type SiteLinkDisabledReason =
  | 'SubdomainDisabled'
  | 'NoHandle'
  | 'ReservedLabel'
  | 'NoServingDomain';
export interface SiteLinkView {
  /** The serving origin with a trailing slash, or `null` — see `disabled_reason`. */
  origin: string | null;
  disabled_reason: SiteLinkDisabledReason | null;
}

/** Resolve the origin a creator's copy-link affordances should build on —
 *  active custom domain beats an enabled subdomain
 *  (`fauna_client_web::site_link_view`). `domains` is `webDomainGet`'s reply,
 *  passed straight through. */
export function webSiteLinkView(
  domains: Array<{ domain: string; status: string }>,
  subdomainEnabled: boolean,
  handle: string | null,
  domain: string,
): SiteLinkView {
  return wasm().webSiteLinkView(domains, subdomainEnabled, handle ?? undefined, domain) as SiteLinkView;
}

/** The public page URL for a published post — the *Copy web link* value. */
export function webPostPageUrl(origin: string, slug: string): string {
  return wasm().webPostPageUrl(origin, slug) as string;
}

/** The full-access URL for a minted paywall link — the *Copy paywall link*
 *  value. `path` must be the mint reply's own `path`, never a client-rebuilt one. */
export function webTokenedUrl(origin: string, path: string, token: string): string {
  return wasm().webTokenedUrl(origin, path, token) as string;
}

/** The "your posts have no public address, and here's why" line for a
 *  `SiteLinkView` with no origin — resolve via `resolveLocalized`. */
export function webDisabledReasonText(reason: SiteLinkDisabledReason | null): LocalizedText {
  return wasm().webDisabledReasonText(reason ?? undefined) as LocalizedText;
}

// Shared recipient-input classification. `fauna_core::resolve::classify_recipient`
// (over wasm) returns a flat `[kind, actorId, user, domain]` array; this wraps it in a
// named, discriminated object so `resolve.ts` shares the 64-hex actor-id check + the
// `user@domain` handle split with android/linux instead of re-implementing them.
export type RecipientClassification =
  | { kind: 'actor_id'; actorId: string }
  | { kind: 'handle'; user: string; domain: string }
  | { kind: 'invalid' };

export function classifyRecipient(input: string): RecipientClassification {
  const [kind, actorId, user, domain] = wasm().classifyRecipient(input) as string[];
  switch (kind) {
    case 'actor_id':
      return { kind: 'actor_id', actorId };
    case 'handle':
      return { kind: 'handle', user, domain };
    default:
      return { kind: 'invalid' };
  }
}

// NIP-23 article Markdown → HTML, via the shared `fauna_core::markdown` parser (over
// wasm), so web renders the same Markdown subset the native apps parse from the same
// crate instead of maintaining its own converter. See `docs/goal/ui/feed.md` § Article.
export function markdownToHtml(md: string): string {
  return wasm().markdownToHtml(md) as string;
}

// Markdown body → the shared semantic `RenderDocument` (`fauna_core::render::markdown_to_document`
// over wasm). The Feed page paints a post body through the SAME `documentToHtml` walker the
// Conversations page uses (render-model.md § D6); the snapshot's `PostSummary.document` is the
// preview document, and this produces a document from the longer decoded body web augments with.
export function markdownToDocument(md: string): RenderDocument {
  return wasm().markdownToDocument(md) as RenderDocument;
}

// Whether a `RenderDocument` carries ≥1 un-revealed remote image — the `load-remote-content-button`
// gate (`fauna_core::render::RenderDocument::has_blocked_remote_images` over wasm; render-model.md
// § D3/§ D4). The browser twin of the native UniFFI `render_document_has_blocked_remote_images`
// face (linux native / windows UniFFI), so the reveal-gate walk (a body `RemoteImage`, or a
// `Resolved` link-preview og:image at any nesting depth) is single-sourced across all 7 apps
// instead of re-walked in TS. Callers guard `undefined` before calling (see `documentHasBlockedRemoteImages`).
export function renderDocumentHasBlockedRemoteImages(doc: RenderDocument): boolean {
  return wasm().renderDocumentHasBlockedRemoteImages(doc) as boolean;
}

// The folded feed quote-post embed, or null (`fauna_core::render::RenderDocument::quoted_post`
// over wasm; render-model.md § D6). The browser twin of the native UniFFI
// `render_document_quoted_post` face. Also the fire-once guard for `resolveQuotedPost`: fire only
// while the post has a `quoted_post_id` and this is null, so the manager's idempotent re-emit
// settles instead of driving a render loop.
// `None` crosses `serde_wasm_bindgen` as `undefined`, not `null` (same as
// `renderDocumentFirstImageHash` below, whose generated `.d.ts` says so outright) — normalize so
// the declared type is honest and callers can compare against one absent value.
export function renderDocumentQuotedPost(doc: RenderDocument): QuotedPostEmbed | null {
  return (wasm().renderDocumentQuotedPost(doc) as QuotedPostEmbed | null | undefined) ?? null;
}

// The content hash of the first trusted `Image` block, or null
// (`fauna_core::render::RenderDocument::first_image_hash` over wasm; render-model.md § D6) — the
// feed's lazily-resolved media, folded in by `resolve_media`. Only the hash crosses: the shared
// model deliberately carries no byte loader (the async-byte-load-stays-client idiom), so the page
// paints `post-image` through its own blob loader. The browser twin of the native UniFFI
// `render_document_first_image_hash` face.
export function renderDocumentFirstImageHash(doc: RenderDocument): string | null {
  return (wasm().renderDocumentFirstImageHash(doc) as string | null | undefined) ?? null;
}

// The content hash of the first trusted `Video` block, or null
// (`fauna_core::render::RenderDocument::first_video_hash` over wasm) — the twin of
// `renderDocumentFirstImageHash`, painted as `video-thumbnail` through the page's own blob
// loader. The browser twin of the native UniFFI `render_document_first_video_hash` face.
export function renderDocumentFirstVideoHash(doc: RenderDocument): string | null {
  return (wasm().renderDocumentFirstVideoHash(doc) as string | null | undefined) ?? null;
}

// Every `ProxiedImage` block in body order as `{path, alt}` records
// (`fauna_core::render::RenderDocument::proxied_images` over wasm; render-model.md § D6c) — a
// bridged post's own pictures, each a nest-relative path the page fetches with its authenticated
// `fetch`. The browser twin of the native UniFFI `render_document_proxied_images` face.
export function renderDocumentProxiedImages(doc: RenderDocument): { path: string; alt: string }[] {
  return (wasm().renderDocumentProxiedImages(doc) as { path: string; alt: string }[] | undefined) ?? [];
}

// Every trusted media block (`Image` / `Video` / `ProxiedImage` / `ProxiedVideo`) in body order
// (`fauna_core::render::RenderDocument::media_blocks` over wasm) — what web paints its media
// row from, since web renders EVERY attachment (the richest existing pattern, priority #4).
export function renderDocumentMediaBlocks(doc: RenderDocument): RenderBlock[] {
  return (wasm().renderDocumentMediaBlocks(doc) as RenderBlock[] | undefined) ?? [];
}

// The urls of link previews still `Resolving`, in body order
// (`fauna_core::render::RenderDocument::resolving_link_preview_urls` over wasm;
// render-model.md § D4) — fire `resolveLinkPreview` for each. Fire-once by construction: a
// resolved block no longer yields its url. The browser twin of the native UniFFI
// `render_document_resolving_link_preview_urls` face.
export function renderDocumentResolvingLinkPreviewUrls(doc: RenderDocument): string[] {
  return wasm().renderDocumentResolvingLinkPreviewUrls(doc) as string[];
}

// The `Resolved` link previews, in body order (`fauna_core::render::RenderDocument::resolved_link_previews`
// over wasm; render-model.md § D4) — one `link-preview-card` per entry. The og:image is
// reveal-gated: paint `image_hash` only when `revealed` (the D3 twin). The browser twin of the
// native UniFFI `render_document_resolved_link_previews` face.
export function renderDocumentResolvedLinkPreviews(doc: RenderDocument): ResolvedLinkPreview[] {
  return wasm().renderDocumentResolvedLinkPreviews(doc) as ResolvedLinkPreview[];
}

// Every link preview in body order with its state name — `resolving` / `resolved` /
// `failed`, whatever the state (`fauna_core::render::RenderDocument::link_previews` over
// wasm; render-model.md § D4). The e2e state dump publishes it as
// `data.feed.posts[].link_previews`, so a test can tell a FAILED preview (no card, for
// good) from one still resolving (no card, yet). The browser twin of the native UniFFI
// `render_document_link_previews` face.
export interface LinkPreviewState {
  url: string;
  state: string;
}

export function renderDocumentLinkPreviews(doc: RenderDocument): LinkPreviewState[] {
  return (wasm().renderDocumentLinkPreviews(doc) as LinkPreviewState[]) ?? [];
}

// Compose-toolbar inline-style wrap, shared with the native toolbars
// (`fauna_core::markdown::wrap_selection` over wasm): keeps a selection's edge
// whitespace OUTSIDE the markers so a double-click word-selection's trailing space
// doesn't produce `*italic *` and collide into `*italic ***bold**`. Returns the
// replacement for the selected region plus the pieces to re-select the wrapped core
// (`beforeCore` = leading whitespace + opening marker). Empty selection wraps the
// placeholder.
export function wrapMarkdownSelection(
  selected: string,
  prefix: string,
  suffix: string,
  placeholder: string,
): { replacement: string; beforeCore: string; core: string } {
  const [replacement, beforeCore, core] = wasm().wrapMarkdownSelection(
    selected,
    prefix,
    suffix,
    placeholder,
  ) as string[];
  return { replacement, beforeCore, core };
}

// Compose-field inline-styling decoration map — the styled-content + marker byte
// ranges over the RAW compose source, from the shared `fauna_core::markdown::decoration_map`
// (over wasm). The same inline scanner feeds `markdownToHtml`, so the compose preview and
// the sent message never disagree on what is styled. `$lib/markdown-decorations` converts
// these byte ranges to UTF-16 CodeMirror positions for the `MarkdownEditor` applier (the
// web twin of linux's `gtk::TextTag` applier / the native FFI `FfiMdDecoration`).
// See `docs/goal/ui/conversations.md` § Compose-field inline markdown styling.
export function decorationMap(src: string): MdDecoration[] {
  return wasm().decorationMap(src) as MdDecoration[];
}

// Notes editor caret-edge reveal set — the inline-emphasis marker byte ranges to UN-HIDE
// for a caret at byte offset `caret`, from the shared `fauna_core::markdown::inline_reveal_ranges`
// (over wasm). A markers-never-shown (Notes) editor conceals every marker `decorationMap`
// returns and reveals only these (the run the caret is in) so its raw markdown can be edited;
// structural block markers are never returned. One shared policy so web + native can't drift
// on which markers reveal. `$lib/markdown-decorations` maps the byte ranges to UTF-16 positions.
export function inlineRevealRanges(src: string, caret: number): MdRevealRange[] {
  return wasm().inlineRevealRanges(src, caret) as MdRevealRange[];
}

// Compose hide-by-default marker treatment (over `fauna_core::markdown::compose_decoration_plan`)
// — which inline markers to conceal vs which to dim, for a caret at byte offset `caret`. The
// inline/structural split + the caret-edge reveal decision are shared Rust (one policy for web +
// native — priority #2; design tracked internally); the web only
// converts the byte ranges to UTF-16 + styles content from `decorationMap` (see `$lib/notes-editor`
// `composeMarkPlan`).
export function composeDecorationPlan(src: string, caret: number): ComposeMarkerPlan {
  return wasm().composeDecorationPlan(src, caret) as ComposeMarkerPlan;
}

// Compose "show markers" (dimmed live-preview) mode reveal policy (over
// `fauna_core::markdown::compose_show_markers_dim_ranges`) — the marker byte ranges to DIM for
// a caret at byte offset `caret`: every inline-emphasis/structural marker whose source line
// differs from the caret's line (markers on the caret's line are revealed, i.e. absent here).
// Replaces `$lib/markdown-decorations`' hand-rolled `caretLine`/`lineOfUtf16` marker filter —
// one shared caret-line rule for web + native (windows/android/linux already consume it). See
// `docs/goal/ui/conversations.md` § the compose "show markers" (dim) mode.
export function composeShowMarkersDimRanges(src: string, caret: number): MdRevealRange[] {
  return wasm().composeShowMarkersDimRanges(src, caret) as MdRevealRange[];
}

/** `{ token, label }` for the `recipient-resolve-status` element — `token` drives
 *  `data-state`; `label` is `null` for idle (renders empty). */
export interface ResolveStatusView {
  token: string;
  label: LocalizedText | null;
}

// Shared recipient-picker resolve status.
// `fauna_conversations::compose::recipient_resolve_status_from_variant` (over wasm) owns
// the state→(token, label) map that all five apps hand-rolled and drifted on (apple's
// share-sheet copy covered only 3 arms, blanking a resolved/not-found status). Pass the
// compose snapshot's `resolve_state` serde variant name verbatim (`"Resolved"`,
// `"NotFound"`, …); an unknown/absent value degrades to idle, never a false state. The
// status line's styling stays web-local. See `docs/goal/ui/conversations.md`
// § Errors & edge cases.
export function recipientResolveStatus(state: string | undefined): ResolveStatusView {
  return wasm().recipientResolveStatus(state) as ResolveStatusView;
}

// Shared thread-list sort cycle. `fauna_conversations::snapshot::next_sort_order_from_variant`
// (over wasm) owns the canonical latest-activity → oldest-first → unread → latest-activity
// advance, so the SPA never enumerates the orders itself (a 2-way ternary previously left
// `Unread` unreachable). Pass the snapshot's `sort` serde variant name verbatim; an
// unknown/absent value restarts the cycle at the default. See `docs/goal/ui/conversations.md`
// § Where logic lives → the Thread-list sort cycle.
export function nextSortOrder(current: string | undefined): string {
  return wasm().nextSortOrder(current) as string;
}

// The reaction quick-set, in the one shared order — `fauna_conversations::QUICKSET_EMOJIS`
// over wasm, the twin of the UniFFI face the native apps read. Web used to re-type the six
// emoji in `conversations/+page.svelte`; the ORDER is a cross-app contract
// (`dm-reaction-option` is indexed, so an e2e tapping index 0 asserts 👍 everywhere), and a
// hand-kept copy had nothing to catch it drifting. See `docs/goal/ui/conversations.md`
// § Reactions & message delete. The fuller "more" grid stays web-local — that picker widget
// is the one sanctioned per-platform divergence (§ Rendering / picker glue).
export function quicksetEmojis(): string[] {
  return wasm().quicksetEmojis() as string[];
}

// ── The room model's render mappings (`ui/conversations.md` § Element IDs) ──
//
// Over `fauna_conversations::room`'s twins — the SAME mappings tui and linux
// call directly and apple/android/windows reach through UniFFI, so the SPA
// hand-types no token, label or choice list (priority #2). Each takes the
// serde spelling the thread state already carries (`"EndToEnd"`, `"Owner"`,
// …), so a caller feeds back what it just read off `detail.room`.
export interface RoomEditorChoice {
  token: string;
  label: string;
}

// `thread-room-class` / `recipient-picker-class`'s sentence.
export function roomClassLabel(klass: string): string {
  return wasm().roomClassLabel(klass) as string;
}

// …and its driver-facing `class` attribute (`end-to-end` / `community` /
// `transport-only`).
export function roomClassAttrToken(klass: string): string {
  return wasm().roomClassAttrToken(klass) as string;
}

// `conversation-guardian-state`'s text on a bridged room…
export function guardianStateLabel(state: string): string {
  return wasm().guardianStateLabel(state) as string;
}

// …and its driver-facing `state` attribute (`held` / `blocked`).
export function guardianStateAttrToken(state: string): string {
  return wasm().guardianStateAttrToken(state) as string;
}

// `thread-member-chip[i]`'s `role` attribute (`owner` / `admin` / `member`).
export function roomRoleAttrToken(role: string): string {
  return wasm().roomRoleAttrToken(role) as string;
}

// `thread-member-chip[i]`'s text: the display name plus the localized
// owner/admin mark on a governed room. `null` role → the bare display name.
export function roomMemberChipText(display: string, role: string | null): string {
  return wasm().roomMemberChipText(display, role) as string;
}

// `room-join-rule-select`'s options, in the one order all seven editors offer.
export function roomJoinRuleEditorChoices(): RoomEditorChoice[] {
  return wasm().roomJoinRuleEditorChoices() as RoomEditorChoice[];
}

// `room-history-policy-select`'s options, likewise.
export function roomHistoryPolicyEditorChoices(): RoomEditorChoice[] {
  return wasm().roomHistoryPolicyEditorChoices() as RoomEditorChoice[];
}

// The draft's staged rule/policy as the token its picker round-trips. A native
// app reads the draft's field as a typed enum and maps it with its own twin;
// the SPA reads the same field off the serialized draft and maps it here, so
// neither hand-types a token.
export function roomJoinRuleToken(rule: string): string {
  return wasm().roomJoinRuleToken(rule) as string;
}

export function roomHistoryPolicyToken(policy: string): string {
  return wasm().roomHistoryPolicyToken(policy) as string;
}

// The class of the room the new-thread picker is about to create, or `null`
// before the first chip is committed (`recipient-picker-class`).
export function roomProspectiveClass(
  chips: unknown[],
  includeHomeNest: boolean,
): string | null {
  return wasm().roomProspectiveClass(chips, includeHomeNest) as string | null;
}

// Notes editor block model (over `fauna_core::notes`) — the substrate-agnostic editor layer
// the Notes WYSIWYG surface binds to. `parseNote` (markdown buffer → blocks) and `serializeNote`
// (blocks → markdown buffer) are the Fork-1 lowering's ends; `applyStructuralGesture` is the
// pure bullets-first gesture engine (Enter/Tab/Shift-Tab/Backspace/checkbox); `applyEdits` is
// the Fork-1 reference lowering of the edits it returns. One shared policy for all 7 apps
// (priorities #1/#2). See `$lib/notes` for the model types + the design spec.
export function parseNote(md: string): NoteDocument {
  return wasm().parseNote(md) as NoteDocument;
}

/** `fauna_core::notes::note_line_map` — the per-buffer-line structural projection (byte offsets;
 *  `mapNoteLines` in `$lib/notes-editor` remaps to CodeMirror UTF-16). `blocks` is the array
 *  `parseNote` returns. The shared structural derivation (code folding, checkbox indexing,
 *  ordered-list numbering, prefix range) every app's Notes view turns into chrome + per-line
 *  styling — computed once, never per client (priority #2). */
export function noteLineMap(value: string, blocks: Block[]): WasmNoteLine[] {
  return wasm().noteLineMap(value, blocks) as WasmNoteLine[];
}

/** `fauna_core::notes::caret_to_block_caret` — map a whole-buffer UTF-8 **byte** caret to a shared
 *  `{block, offset}` `BlockCaret` (or `null` for an empty buffer). The block lookup / prefix skip /
 *  code-body accumulation is shared Rust; web converts CodeMirror's UTF-16 caret to a byte offset
 *  first (`utf16ToByte` — the only seam web keeps). `blocks` is the `parseNote` array. */
export function caretToBlockCaret(value: string, blocks: Block[], caretByte: number): BlockCaret | null {
  return wasm().caretToBlockCaret(value, blocks, caretByte) as BlockCaret | null;
}

/** `fauna_core::notes::block_caret_to_byte` — the inverse: map a shared `BlockCaret` (e.g. a
 *  gesture's returned caret) back to a whole-buffer UTF-8 **byte** offset against the post-gesture
 *  `(value, blocks)`. Web converts the byte offset back to a CodeMirror UTF-16 caret at the seam. */
export function blockCaretToByte(value: string, blocks: Block[], caret: BlockCaret): number {
  return wasm().blockCaretToByte(value, blocks, caret) as number;
}

export function serializeNote(doc: NoteDocument): string {
  return wasm().serializeNote(doc) as string;
}

export function applyStructuralGesture(
  doc: NoteDocument,
  caret: BlockCaret,
  gesture: StructuralGesture,
  newId: BlockId,
): GestureResult {
  return wasm().applyStructuralGesture(doc, caret, gesture, newId) as GestureResult;
}

export function applyEdits(doc: NoteDocument, edits: BlockEdit[]): NoteDocument {
  return wasm().applyEdits(doc, edits) as NoteDocument;
}

// Markdown → HTML for an **untrusted inbound** body (mail / DM): remote `![]()` images
// render BLOCKED — emitted as `<img data-remote-src=… class="blocked-remote-image">` with
// no `src`, so the browser issues no request (the privacy perimeter; html-mail.md
// § Security & privacy). Use this — not `markdownToHtml` (fetch mode) — for the
// conversations bubble; a per-message reveal re-renders that one message in fetch mode.
export function markdownToHtmlBlocked(md: string): string {
  return wasm().markdownToHtmlBlocked(md) as string;
}

// How many blocked remote images a body carries — gates the per-message
// `load-remote-content-button` (shown iff `> 0` and not yet revealed). Same shared count
// the blocked renderer emits, so the button and the placeholders agree by construction.
export function countRemoteImages(md: string): number {
  return wasm().countRemoteImages(md) as number;
}

// A raw `TypedAddress` (the externally-tagged snapshot object) → its canonical
// per-rail display string, via the shared `fauna_conversations::TypedAddress::display`
// (over wasm). The wasm twin of the native FFI `typedAddressDisplay`
// (windows/android/apple) + linux's direct `.display()` — so web's conversations
// page stops carrying its own variant→string switch (priority #2/#4). See
// `docs/goal/ui/conversations.md` § Where logic lives.
export function typedAddressDisplay(addr: unknown): string {
  return wasm().typedAddressDisplay(addr) as string;
}

// Identity-import field parsing (a bare 64-hex secret, the `fauna://identity?secret=&handle=`
// query form, or the iOS colon form) via the shared `fauna_core::identity_qr` parser (over
// wasm), shared with iOS/android instead of hand-rolled per client. Returns `null` on a
// parse failure (empty array from the wasm face).
export function parseIdentityImport(input: string): { secret: string; handle: string | null } | null {
  const parts = wasm().parseIdentityImport(input) as string[];
  if (parts.length === 0) return null;
  const [secret, handle] = parts;
  return { secret, handle: handle && handle.length > 0 ? handle : null };
}

// Register the SPA's aftermath progress callback for LEG 3, the `__mls` re-seal
// (succession-aftermath.md § Re-key scope, the BackupKey corpus row).
//
// Separate from `runSuccessionAftermath`'s callback parameter, which carries the other
// five legs, purely because of WHEN leg 3 reports: it is a barrier inside the
// conversations replica's own load, so the sink must be in place before that plane is
// built — earlier than the post-auth pass runs. Same `(leg, line | null) => void` shape,
// and it files under `mlsReseal`, so the SPA keeps ONE progress channel for all six.
//
// Pass `undefined` to clear it.
export function setMlsResealSink(
  onProgress: ((leg: string, line: LocalizedText | null) => void) | undefined,
): void {
  wasm().setMlsResealSink(onProgress);
}

// The export half of the same loop: build exactly the URI `parseIdentityImport` accepts, so
// export → scan → import is closed end-to-end. A handle-less client writes the bare secret
// form (settings.md § Identity export). Note the JS name is snake_case — the Rust export
// carries no `js_name`, unlike the two QR faces below.
export function identityQrEncode(secretHex: string, handle?: string | null): string {
  return wasm().identity_qr_encode(secretHex, handle ?? undefined) as string;
}

// A QR code as a square, row-major grid of dark/light modules — `modules[y * size + x]`.
// The shared `fauna_core::qr_matrix` encoder (over wasm), the SAME one the five native
// apps call over UniFFI: the SPA paints the grid itself rather than pulling a JS QR
// library, so there is one encoder for six apps (priorities #1/#2).
//
// The grid carries NO quiet zone — pad by `qrQuietZoneModules()` on all four sides when
// drawing, or scanners refuse the code.
export interface QrMatrix {
  size: number;
  modules: boolean[];
}

export function qrMatrix(data: string): QrMatrix {
  return wasm().qrMatrix(data) as QrMatrix;
}

// The mandatory quiet-zone margin, in modules — the shared
// `fauna_core::qr_matrix::QUIET_ZONE_MODULES`, so the SPA doesn't hard-code its own `4`.
export function qrQuietZoneModules(): number {
  return wasm().qrQuietZoneModules() as number;
}

// Structured-post field projection. `fauna_core::structured::structured_view`
// (over wasm) projects a decoded `PostBody::Structured` into the typed feed-card
// view its schema declares — so web stops re-deriving the per-schema field keys
// in TS and shares one contract with the nostr bridge writer (priority #2/#4).
// Returns `null` for any non-structured body (plain / media / video posts and
// the `nostr/kind-N` unknown-kind fallback). Sync: `decoded` only exists after
// `decodePost`, which has already awaited `ensureWasm()`. See
// `docs/goal/ui/feed.md` § Post content types + § Where logic lives.
export type StructuredView =
  | { kind: 'article'; title: string; summary: string; image: string; content: string }
  | { kind: 'community'; name: string; description: string; identifier: string; rules: string }
  | { kind: 'classified'; title: string; price: string; location: string; condition: string; content: string }
  | { kind: 'live-activity'; title: string; status: string; streaming_url: string; participants: string; summary: string };

export function structuredView(decoded: unknown): StructuredView | null {
  if (decoded == null) return null;
  return (wasm().structuredView(decoded) as StructuredView | null) ?? null;
}

// Post source classification (badges). `fauna_feed::classify_sources` (over
// wasm) parses the comma-separated wire `source` field (e.g. `"fauna, bluesky"`)
// into an ordered, deduplicated list of `{ id, label, glyph }` badges — the single
// classification contract shared with linux/native (`source_label`) and the
// per-app `protocolIcon`/`protocolLabel`/`ProtocolBadge`/`build_protocol_badge`
// switches it replaced (priority #2/#4). Web keeps only its `SourceGlyph → emoji`
// map (`$lib/source-glyph` `sourceGlyphEmoji`) keyed off the precomputed `glyph`
// concept — the SAME map the conversations rail uses, so the badge and the rail
// can't drift (render-model.md § Deltas → D5); the `label` is the canonical shared
// `SourceKind::label`. Sync: every caller renders a post that only exists after
// `decodePost`, which has already awaited `ensureWasm()`. See `docs/goal/ui/feed.md`
// § Where logic lives.
export interface SourceBadge {
  /** Stable icon-map key: `fauna | bluesky | nostr | activitypub | email | other`. */
  id: string;
  /** Canonical user-facing label (e.g. "Fediverse" for activitypub). */
  label: string;
  /** Shared source-icon concept (`SourceKind::glyph()`), serialized to its
   *  lowercase id (`fox | envelope | butterfly | bolt | globe | unknown | archive | bridge`); the
   *  key for `$lib/source-glyph` `sourceGlyphEmoji`. */
  glyph: string;
}

export function classifySources(sourceField: string): SourceBadge[] {
  return (wasm().classifySources(sourceField) as SourceBadge[]) ?? [];
}

// The i18n key for a feed-manager error that is a STATED REFUSAL (today only
// `feed.reference_restricted` — words under a restricted post, `ui/feed.md` §
// Encryption at rest, ruling 6), `undefined` for every other error text. The
// verbs reject with the refusal's stable text, and `fauna_feed::refusal_i18n_key`
// is the one place that recognizes it. Sync for the same reason as
// `classifySources`: a verb that rejected has already awaited `ensureWasm()`.
export function feedRefusalI18nKey(err: string): string | undefined {
  return wasm().feedRefusalI18nKey(err) ?? undefined;
}

// ── Calendar / date math (shared `fauna_core::caltime`) ──────────────────────
//
// Raw thunks over the wasm boundary; the JS-`Date`-friendly adapters (0↔1-indexed
// month conversion, `Date` construction) live in `$lib/caltime`. Months are
// **1-indexed** (1 = January) and weekday is **0 = Mon … 6 = Sun** here, matching
// `fauna_core::caltime` — the canonical Gregorian date math the native apps
// consume (linux re-exports the same module), so the web Events month-grid +
// week-range layout stops re-deriving it in JS `Date` arithmetic (priority #2/#4).
// See `docs/goal/ui/events.md` § Where logic lives — "web's events read-path is
// the first expected consumer". Sync: callers run after `ensureWasm()` (the
// Events page awaits it before gating the calendar UI on `ready`).

/** A `(year, month, day)` date — month 1-indexed. */
export interface CalDate {
  year: number;
  month: number;
  day: number;
}

/** A `(year, month)` pair — month 1-indexed. */
export interface YearMonth {
  year: number;
  month: number;
}

/** `caltime::month_grid` — the 42-cell (6×7) month grid, padded with prev/next-
 *  month days. `weekStart` is `0 = Mon … 6 = Sun`. */
export function monthGrid(year: number, month: number, weekStart: number): CalDate[] {
  return wasm().monthGrid(year, month, weekStart) as CalDate[];
}

/** `caltime::prev_month` — the `{ year, month }` before, year-wrapped at January. */
export function prevMonth(year: number, month: number): YearMonth {
  return wasm().prevMonth(year, month) as YearMonth;
}

/** `caltime::next_month` — the `{ year, month }` after, year-wrapped at December. */
export function nextMonth(year: number, month: number): YearMonth {
  return wasm().nextMonth(year, month) as YearMonth;
}

/** `caltime::week_start_date` — the `{ year, month, day }` of the `weekStart` day
 *  (`0 = Mon … 6 = Sun`) of the week containing the given date. */
export function weekStartDate(year: number, month: number, day: number, weekStart: number): CalDate {
  return wasm().weekStartDate(year, month, day, weekStart) as CalDate;
}

/** `caltime::add_days` — the `{ year, month, day }` `n` days away (`n` may be
 *  negative), crossing month/year boundaries. */
export function addDays(year: number, month: number, day: number, n: number): CalDate {
  return wasm().addDays(year, month, day, n) as CalDate;
}

/** The shared `calendar-view-*` vocabulary — `caltime::CalendarViewMode::as_wire`.
 *  One spelling for the SPA's view state, its toggle ids and the wasm boundary;
 *  the SPA's historical `'list'` for the agenda is deliberately not part of it
 *  (`caltime::CalendarViewMode::from_wire` rejects it, and `pan`/`visibleDays`
 *  throw rather than silently selecting nothing). */
export type CalendarViewMode = 'agenda' | 'month' | 'week' | 'day';

/** `caltime::pan` — one `events-prev/next-month` click in `mode`, moving **one
 *  visible range**: a month in month view, a week in week, a day in day, and
 *  nothing in the date-unfiltered agenda. `forward` picks the direction
 *  (`false` = prev). Throws on an unknown `mode`. */
export function pan(
  mode: CalendarViewMode,
  year: number,
  month: number,
  day: number,
  forward: boolean,
): CalDate {
  return wasm().pan(mode, year, month, day, forward) as CalDate;
}

/** `caltime::visible_days` — the dates `mode` shows for the anchor: the month
 *  grid's 42 cells, the week's 7 columns, the single day, or `[]` for the
 *  date-unfiltered agenda. `weekStart` is `0 = Mon … 6 = Sun`. */
export function visibleDays(
  mode: CalendarViewMode,
  year: number,
  month: number,
  day: number,
  weekStart: number,
): CalDate[] {
  return wasm().visibleDays(mode, year, month, day, weekStart) as CalDate[];
}

/** A 24-hour time of day. */
export interface TimeOfDay {
  hour: number;
  minute: number;
}

/** `caltime::WORKING_DAY_START` — the day cell's double-click compose prefill
 *  hour (events.md § User actions), shared with tui/linux. */
export function workingDayStart(): TimeOfDay {
  return wasm().workingDayStart() as TimeOfDay;
}

/** `fauna_client_caldav::resolve_calendar_selection` (events.md § Where logic
 *  lives → "Which calendars the page is scoped to"): resolve the Events
 *  page's calendar selection against the calendars that actually exist right
 *  now. Returns the id back unchanged when still live, or `undefined` (the
 *  no-selection union) when it has vanished — deleted here, or by a CalDAV
 *  MUA against the same `bridge_caldav_*` store — rather than stranding the
 *  page on a permanently blank list. Resolve at READ time on every query,
 *  never by clearing the stored selection on a calendar-list refresh (the
 *  poll would otherwise drop a live selection every cadence interval). */
export function resolveCalendarSelection(
  selected: string | null | undefined,
  existingIds: string[],
): string | undefined {
  return wasm().resolveCalendarSelection(selected ?? undefined, existingIds);
}

/** `fauna_client_caldav::calendar_is_displayed` (events.md § Where logic lives
 *  → *Which calendars display*): does an event on `calendarId` belong on the
 *  page right now? The whole scope composition in one call — a live
 *  `selected` calendar wins outright (visibility never applies to a
 *  selection, and the staleness rule runs inside so pass the *raw* stored
 *  selection); with none, `visibleCalendars` filters the union, an **empty**
 *  set meaning "no filter" (the full union), never "hide everything". */
export function calendarIsDisplayed(
  selected: string | null | undefined,
  existingIds: string[],
  visibleCalendars: string[],
  calendarId: string,
): boolean {
  return wasm().calendarIsDisplayed(selected ?? undefined, existingIds, visibleCalendars, calendarId);
}

/** One timed event's sub-column assignment in a week/day time grid. Block width
 *  is `1 / totalColumns` of the day column; block x-offset is `columnIndex`. */
export interface OverlapColumn {
  eventIndex: number;
  columnIndex: number;
  totalColumns: number;
}

/** `caltime::find_overlaps` — side-by-side column packing for overlapping timed
 *  event blocks. `intervals` are `[startMinutes, endMinutes]` pairs (minutes
 *  from midnight); the i-th result corresponds to the i-th interval. */
export function findOverlaps(intervals: [number, number][]): OverlapColumn[] {
  return wasm().findOverlaps(intervals) as OverlapColumn[];
}

/** One event's placement in a week/day time-grid day column — the JS shape of
 *  `fauna_core::caltime::EventPlacement` (flattened). `allDay: true` → an
 *  all-day-band chip (the minute/column fields are 0 and meaningless);
 *  otherwise a positioned timed block. */
export interface DayEventPlacement {
  allDay: boolean;
  startMin: number;
  endMin: number;
  columnIndex: number;
  totalColumns: number;
}

/** `caltime::day_column_layout` — the complete all-day/timed layout for one day
 *  column: `isAllDay` classification, timed minute geometry (an event crossing
 *  midnight renders start → midnight everywhere, end clamped to 1440 — never
 *  the collapsed 30-minute block a per-event `minutesOfDay(dtend)` derivation
 *  produces), and `findOverlaps` column packing, in one call. `events` are
 *  `[dtstart, dtend]` ISO pairs; the date filter (which events fall on this
 *  day) stays client-side. The i-th result corresponds to the i-th event. */
export function dayColumnLayout(events: [string, string | null][]): DayEventPlacement[] {
  return wasm().dayColumnLayout(events) as DayEventPlacement[];
}

/** `caltime::normalize_event_datetime_input` — the one shared rule turning the
 *  `event-dtstart` / `event-dtend` `YYYY-MM-DDTHH:MM` input into the
 *  seconds-bearing wall-clock datetime the API takes (events.md § Where logic
 *  lives). Wall-clock on purpose: it is what every app's time grid reads back,
 *  so a midnight-to-midnight event stays all-day wherever the browser is. */
export function normalizeEventDatetimeInput(input: string): string {
  return wasm().normalizeEventDatetimeInput(input);
}

// ── box-recovery: the local read (nest-less) ────────────────────────────
//
// box-recovery.md § The plane-era recovery floor, (b) The reads: a pre-login
// recovery surface reads this device's own account store JOINED with a cold
// read from a reachable nest (`rpc.ts`'s `deploymentSeeds()` /
// `recoverSelfhostedCommand()` do both). When no nest client exists — no nest
// URL is stored, or the stored one (in the case recovery exists for, the dead
// box) does not answer — these free functions (no `WsRpcClient`) are the local
// read alone, over the same IndexedDB store the account runtime opens.

/** One custodied box read from this device's own account store — same shape
 *  as `rpc.ts`'s `DeploymentSeedBox`. The custodied seed never crosses into
 *  JS, identical to the reachable-nest getter. */
export interface RecoveryBoxLocal {
  nestActorId: string;
  domain: string | null;
}

/** The custodied deployment-seed box list read from this device's own account
 *  store — no nest round-trip, no `WsRpcClient`, no account runtime. A store
 *  that does not exist yields `[]` (it is never created); rejects on a store
 *  that fails to open or a row that does not decode. */
export async function recoveryBoxesLocal(secretHex: string): Promise<RecoveryBoxLocal[]> {
  const raw = (await wasm().recoveryBoxesLocal(secretHex)) as Array<{
    nest_actor_id: string;
    domain: string | null;
  }>;
  return raw.map((e) => ({ nestActorId: e.nest_actor_id, domain: e.domain ?? null }));
}

/** The `recover-selfhosted-command` line read from this device's own account
 *  store — the nest-less twin of `rpc.ts`'s `recoverSelfhostedCommand()`.
 *  Rejects when the store custodies no seed for that box. */
export function recoverSelfhostedCommandLocal(
  secretHex: string,
  nestActorIdHex: string,
): Promise<string> {
  return wasm().recoverSelfhostedCommandLocal(secretHex, nestActorIdHex) as Promise<string>;
}


// ── Client-device destination kind (backups.md § Third destination kind) ──

/** One arm of `backup-destination-kind-select`: the `kind` a chosen option writes
 *  to the row, and the text to paint for it — deliberately the *same*
 *  `LocalizedText` that row's badge will carry. */
export interface BackupDestinationKindOption {
  value: string;
  label: LocalizedText;
}

// Shared `backup-destination-kind-badge` text for one destination row.
// `fauna_core::format::backup_destination_kind_label` over wasm. Pass the row's raw
// `kind`; an unrecognised kind renders *as itself* (the raw string interpolated)
// rather than collapsing into a generic word — a row a newer client wrote is exactly
// when the user needs to see what this build cannot drive.
export function backupDestinationKindLabel(kind: string): LocalizedText {
  return wasm().backupDestinationKindLabel(kind) as LocalizedText;
}

// Shared `backup-destination-kind-select` option catalog, in paint order (nest first
// — it is the kind that actually satisfies "off-site").
// `fauna_core::format::backup_destination_kind_options` over wasm. Do NOT pair a
// hand-written option list against `backupDestinationKindLabel`: the property this
// catalog exists for is that the option a user picks and the badge they get back are
// the same text. The ratified-but-deferred S3 kind is absent rather than
// present-and-disabled.
export function backupDestinationKindOptions(): BackupDestinationKindOption[] {
  return wasm().backupDestinationKindOptions() as BackupDestinationKindOption[];
}

// ── Trained-factor publish kind (topic-factors.md § Publishing a trained
// factor, v2) — the `personalization-trained-factor-publish-kind-select`
// catalog and the shared Model-review faces both halves (publisher review +
// subscriber inspect) read.

/** One arm of `personalization-trained-factor-publish-kind-select`: the
 *  `artifact_kind` a chosen option writes, and the text to paint for it — the
 *  `BackupDestinationKindOption` shape verbatim. RAW-VALUE: the `<option
 *  value>` an SPA renders is `value` itself, never the resolved label. */
export interface PublishKindOption {
  value: string;
  label: LocalizedText;
}

// `fauna_core::format::publish_kind_options` over wasm — the
// `personalization-trained-factor-publish-kind-select` catalog, in paint
// order (List first — the weaker disclosure, so it is what an unattended
// default picks). Do NOT pair a hand-written option list against
// `publishKindLabel`: the option a user picks and the words they read back
// must be the same text.
export function publishKindOptions(): PublishKindOption[] {
  return wasm().publishKindOptions() as PublishKindOption[];
}

// `fauna_core::format::publish_kind_label` over wasm — the kind-select
// option's text for one wire value. Paint-only: the select round-trips the
// wire discriminator, so this never becomes a driver contract.
export function publishKindLabel(kind: string): LocalizedText {
  return wasm().publishKindLabel(kind) as LocalizedText;
}

// `fauna_core::format::ngram_direction_label` over wasm — the Model review
// row's class-direction text, shared by the publisher's
// `…-publish-ngram-direction` and the subscriber's
// `labeler-inspect-model-entry-direction`, so the two cannot disagree about
// what "more"/"less" means.
export function ngramDirectionLabel(more: number, less: number): LocalizedText {
  return wasm().ngramDirectionLabel(more, less) as LocalizedText;
}

// `fauna_core::format::ngram_doc_count_label` over wasm — the Model review
// row's class-blind distinct-document count (`more + less`), the quantity
// the 3-post privacy floor bounds.
export function ngramDocCountLabel(more: number, less: number): LocalizedText {
  return wasm().ngramDocCountLabel(more, less) as LocalizedText;
}

// `fauna_core::format::text_model_needs_newer_app` over wasm — the
// `labeler-catalog-item-kind` badge's override text, or `null` to paint the
// raw `artifact_kind` verbatim (every ordinary row). The one non-passthrough
// field: a subscribed `text-model` this build's tokenizer contract does not
// implement says so here, over the SAME predicate the compose seam's inert
// branch reads, so the badge cannot disagree with the scorer.
export function textModelNeedsNewerApp(
  artifactKind: string,
  artifactVersion: number,
): LocalizedText | null {
  return wasm().textModelNeedsNewerApp(artifactKind, artifactVersion) as LocalizedText | null;
}

/** `{ label, held?, cap? }` — resolve `held`/`cap` first (they are themselves
 *  byte-size `LocalizedText`s) and substitute them as `label`'s `{held}`/`{cap}`
 *  args, the same two-level shape as `BackupLastUploadDisplay`. */
export interface BackupUsageDisplay {
  label: LocalizedText;
  held: LocalizedText | null;
  cap: LocalizedText | null;
}

// Shared `backup-destination-usage` row text (client-device rows only): held bytes
// against the user-set cap. `fauna_core::format::backup_usage_label` over wasm.
//
// ⚠ `capState` is READ, never inferred. Pass the status row's `cap_state` through; do
// not re-derive cap-reached from `held >= cap`. A pull pass that stopped at its cap
// ends *below* the cap (a segment larger than the remaining headroom stops the pass
// without filling it), so inferring the verdict renders "healthy, with room to spare"
// for a backup that has silently stopped advancing.
//
// `heldBytes: null` is "this custodian has never checked in", which reads as *nothing
// held yet* rather than *0 bytes held*.
export function backupUsageLabel(
  heldBytes: number | null,
  capacityCapBytes: number | null,
  capState: string | null,
): BackupUsageDisplay {
  return wasm().backupUsageLabel(
    heldBytes ?? undefined,
    capacityCapBytes ?? undefined,
    capState ?? undefined,
  ) as BackupUsageDisplay;
}

// Shared `backup-destination-capacity-input` parse — the inverse of `byteSize`.
// `fauna_core::format::parse_byte_size` over wasm. Liberal about what a person types
// ("50 GB", "50GB", "1,5 TB", a bare "1024") and strict about what counts as a number;
// units are 1024-based, matching `byteSize`'s own scaling, so a cap round-trips through
// the two unchanged instead of drifting every repaint.
//
// `null` is a REFUSAL to surface on `error-message`, never a substituted default:
// silently recording a cap the user did not choose is the class of guess that fills a
// device's disk.
export function parseByteSize(input: string): number | null {
  return (wasm().parseByteSize(input) as number | undefined) ?? null;
}

// Shared `backup-sole-client-destination-warning` predicate: does every configured
// destination hold its copy on one of the owner's own devices?
// `fauna_core::data::every_row_is_a_client_device` over wasm — pass the array
// `backupDestinationList` returned, unchanged.
//
// Shared because it is a POLICY answer, not a rendering one, and both arms are the
// conservative direction: an empty list is NOT sole-client (painting a durability
// warning on an account with no backup at all is simply false), and a row whose kind
// this build does not implement counts as NOT a client device (it may well BE the
// off-site copy the warning would otherwise deny the user has).
export function everyDestinationIsAClientDevice(destinations: readonly BackupDestination[]): boolean {
  return wasm().everyDestinationIsAClientDevice(destinations) as boolean;
}

// The Notifications page's per-row icon: `notif_type` wire string → display
// emoji. `fauna_core::notification_glyph::notification_type_emoji` over wasm
// — the shared mapping lifted out of this file's own hand-written
// `notificationIcon()` switch (linux had the identical duplicate; both now
// delegate). See `docs/goal/behavior/notifications.md` § Where logic lives.
export function notificationTypeGlyph(notifType: string): string {
  return wasm().notificationTypeGlyph(notifType) as string;
}

/** What a notification row says — see `notificationText`. */
export type NotificationText =
  | ({ kind: 'localized' } & LocalizedText)
  | { kind: 'verbatim'; text: string };

// What a notification row says: the localized body, the English `summary`, or
// the default — `fauna_client_notifications::notification_text` over wasm
// (`docs/goal/behavior/notifications.md` § Localized body). Pass the row AS IT
// ARRIVES from `notificationsList`. Never paint `body` or `summary` yourself: a
// key this build's catalog lacks must lose to `summary`, and "is the key known"
// is the shared decision's to answer.
export function notificationText(row: unknown): NotificationText {
  return wasm().notificationText(row) as NotificationText;
}

// The conversations rail / feed badge source icon: glyph id → display emoji.
// `fauna_core::source_glyph::source_glyph_emoji` over wasm — the shared map
// lifted out of this app's own hand-written `sourceGlyphEmoji()` switch, which
// all seven apps had a copy of. See `render-model.md` § Deltas → D5.
export function sourceGlyphEmoji(glyphId: string): string {
  return wasm().sourceGlyphEmoji(glyphId) as string;
}
