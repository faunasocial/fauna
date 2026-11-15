import type { SharedRpcPort } from '../../static/fauna_wasm_connected_apps.js';
import { asset } from '$app/paths';
import { joinChunk } from './chunk-join';
import type { ConnectedAppsMachine } from '../../static/fauna_wasm_connected_apps.js';
import { hexToBytes } from './hex';
import { sharedAccountPort } from './account-runtime';
import { nodeUrl } from './api';
import { localTimestamp } from './value-format';

// Re-export so route/component files consume the machine type via
// `#lib/wasm-connected-apps` rather than a fragile relative path into
// `static/` (mirrors `wasm-labeler-catalog.ts`).
export type { ConnectedAppsMachine };

// Loader for the page-level connected-apps WASM chunk
// (`libs/fauna-wasm-connected-apps`: the `ConnectedAppsMachine` — the roster of
// everything acting for the user from outside the apps, the Requests tray,
// Connect an app, per-client blocks). A separate wasm module from `fauna_wasm`
// — same separate-chunk discipline: keep the constructor call inside the module
// that holds the singleton `wasmModule` reference (a different Vite chunk would
// carry its own copy of the wasm-bindgen boilerplate). One shared surface, all
// 7 apps (priority #1/#2; docs/goal/ui/connected-apps.md).

let wasmModule: typeof import('../../static/fauna_wasm_connected_apps.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the connected-apps wasm chunk exactly once per page load. The
 *  init *promise* is memoized (same load-bearing shape as
 *  `wasm.ts::ensureWasm`): two concurrent callers await the SAME in-flight
 *  init, so `mod.default()` runs once — a second concurrent call would
 *  re-instantiate the chunk and reset wasm linear memory. A failed init
 *  drops the cached promise so a later call can retry. */
export function ensureConnectedAppsWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_connected_apps.js');
      await mod.default(asset('fauna_wasm_connected_apps_bg.wasm'));
      // Its tracing to the ring, its identity-store doors (chunk-join.ts).
      joinChunk(mod);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) {
    throw new Error('Connected-apps WASM not initialized — call ensureConnectedAppsWasm() first');
  }
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh, code lookup, resolve, block, revoke). */
export interface ConnectedAppsMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `ConnectedAppsMachine` over the SPA singleton's socket —
 * `port` is `sharedRpcPort(secretHex)` from `#lib/rpc`. `secretHex` is the
 * actor's 32-byte seed: the chunk builds its own mail-settings machine for the
 * mail app-password rows (the one Mail & Calendar builds, with the same
 * `nodeUrl()`), reading the account's mail custody and succession ledger
 * through this tab's account port, `sharedAccountPort(secretHex)`. State starts
 * empty — the caller drives `refresh()`, which hydrates that mail machine first.
 */
export async function createConnectedAppsMachine(
  observer: ConnectedAppsMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<ConnectedAppsMachine> {
  await ensureConnectedAppsWasm();
  return new (wasm().ConnectedAppsMachine)(
    observer,
    port,
    hexToBytes(secretHex),
    sharedAccountPort(secretHex),
    nodeUrl(),
  );
}

/** A roster row's text as composed once in shared Rust (`compose::roster_row_text`):
 *  `lines` top to bottom, then the `facts` the page joins with `factsSeparator()`
 *  as the last line. Resolve each with `resolveComposed`. Instants are formatted
 *  in JS (`localTimestamp`) — wasm has no OS timezone database. */
export interface RosterRowText {
  lines: ComposedText[];
  facts: ComposedText[];
  /** Whether the row links to its plugin's settings card
   *  (`connected-apps-item-settings-link`) — a hosted plugin's row, decided in
   *  shared Rust (`opens_settings_card`), never by a class test here. */
  opens_settings_card: boolean;
  /** Whether the row opens a per-operation record
   *  (`connected-apps-oracle-open`) — a principal the oracle served, decided in
   *  shared Rust (`opens_record`); never an app-password session's row. */
  opens_record: boolean;
}

/** One operation of an open record, composed once in shared Rust
 *  (`compose::oracle_op_text`). Resolve it with `resolveComposed`. */
export function composeOracleOp(op: OracleOpRow): ComposedText {
  return JSON.parse(wasm().oracleOpTextJson(JSON.stringify(op), localTimestamp)) as ComposedText;
}

/** Compose one snapshot row's text. Needs the chunk initialized (it is: a row
 *  only exists after the machine was built). */
export function composeRosterRow(row: ConnectedAppRow): RosterRowText {
  return JSON.parse(wasm().rosterRowTextJson(JSON.stringify(row), localTimestamp)) as RosterRowText;
}

/** Compose one request card's lines (`compose::request_card_lines`). */
export function composeRequestCard(card: ConsentCardRow): ComposedText[] {
  return JSON.parse(wasm().requestCardLinesJson(JSON.stringify(card))) as ComposedText[];
}

/** An admin's hosted-plugin install card's install section, composed once in
 *  shared Rust (`compose::request_install_text`): one `ComposedText` (or a list
 *  of them, joined with `lineSeparator()`) per `connected-apps-request-install-*`
 *  leaf. */
export interface InstallSectionText {
  heading: ComposedText;
  /** The admin framing line under the heading (untagged); null on a bind card. */
  framing: ComposedText | null;
  form: ComposedText;
  publisher: ComposedText;
  digest: ComposedText;
  kinds: ComposedText[];
  hosts: ComposedText[];
  settings: ComposedText[];
}

/** Compose one request card's install section; `null` on every card but a
 *  hosted-plugin install card. */
export function composeRequestInstall(card: ConsentCardRow): InstallSectionText | null {
  return JSON.parse(wasm().requestInstallTextJson(JSON.stringify(card))) as InstallSectionText | null;
}

/** What the install section's multi-line leaves put between their lines. */
export function lineSeparator(): string {
  return wasm().lineSeparator();
}

/** What a roster row's facts line puts between its facts. */
export function factsSeparator(): string {
  return wasm().factsSeparator();
}

// ── Snapshot shapes — `ConnectedAppsSnapshot` (libs/fauna-client-connected-apps/
// src/snapshots.rs) as `snapshotJson()` serializes it. The web page renders off
// these verbatim: roster composition, scope words, the class key and which verb
// revokes a row are all the shared machine's.
import type { ComposedText, LocalizedText } from '#lib/i18n/localized';
import type { ConsentCardRow, ConsentSetRow } from '#lib/atproto-settings-machine';

export type { ConsentCardRow };

/** One roster row. `key` is opaque — the machine picks the revoke verb from it. */
export interface ConnectedAppRow {
  key: string;
  /** A `class::*` value (`remote`, `device`, `wasm`, `container`, `app_password`,
   *  `signer`, `oauth`) or an unknown execution form — no badge then. */
  class: string;
  name: LocalizedText;
  client_id: string | null;
  publisher: string | null;
  scope_descriptions: LocalizedText[];
  /** The permission sets the row's permissions came from, in the consent
   *  card's own composition; empty for every non-principal row. */
  sets: ConsentSetRow[];
  created_at_millis: number;
  last_used_at_millis: number | null;
  lasts_until_millis: number | null;
  connected: boolean;
  /** The row is on the external-app plane and held back reversibly: the
   *  account's external-apps switch is OFF (a principal or an ATProto
   *  app-password session), or a step-down suspended every OAuth grant a
   *  principal holds (`suspended`). Listed and revocable, not connected. */
  paused: boolean;
  /** A principal whose every OAuth grant a step-down suspended: the cause of
   *  `paused` when the switch is ON; no lasts-until; a fresh sign-in clears
   *  it. The shared composition words it. */
  suspended: boolean;
  /** Set on a mail app-password row only: the MUA login (only `{handle}` left
   *  for the renderer — `resolveMuaUsername`), the kind label, and whether the
   *  identity-succession burn killed it. */
  mail: { mua_username: string; kind: LocalizedText; revoked: boolean } | null;
  /** A NIP-46 signer row the identity-succession burn killed and kept: listed
   *  not connected, the mail row's `revoked` for the signer class. */
  signer_burned: boolean;
  /** The oracle summary — present exactly on a principal the nest's custodian
   *  served, or an ATProto app-password session that has written, never
   *  zero-filled on one it did not. */
  oracle?: OracleSummaryRow | null;
  /** The trust facet — present exactly on a principal that attested a holder
   *  key on a machine that reads the owner's grant log; absent on every other
   *  row (never an empty state). */
  trust?: PrincipalTrust | null;
}

export type TrustScopeTuple = { class: string; kind: string | null; tier: string | null };
/** The folder a folder read grant covers (`fauna_client_pair::TrustFolder`). */
export type TrustFolderRef = { Named: { name: string } } | 'Deleted';

/** One live key (`fauna_client_pair::TrustGrantRow`); times are epoch seconds. */
export interface PrincipalTrustGrant {
  grant_id: number[];
  scope: TrustScopeTuple[];
  lasts_until: number;
  liveness: 'Active' | 'ExpiringSoon' | 'Expired' | 'AutoRenewing';
  folder?: TrustFolderRef | null;
}

/** One grant event (`fauna_client_pair::TrustHistoryRow`); `at` is epoch seconds. */
export interface PrincipalTrustHistory {
  grant_id: number[];
  kind: 'Mint' | 'Renew' | 'Revoke';
  scope: TrustScopeTuple[];
  at: number;
  folder?: TrustFolderRef | null;
}

/** A principal row's Now / History lens over its keys (`snapshots.rs::PrincipalTrust`). */
export interface PrincipalTrust {
  lens: 'Now' | 'History';
  grants: PrincipalTrustGrant[];
  history: PrincipalTrustHistory[];
}

/** What the oracle did for one principal: operations in the current window,
 *  every request answered, and the newest operation (`snapshots.rs`). */
export interface OracleSummaryRow {
  ops_in_window: number;
  requests_served: number;
  last_op: OracleOpRow | null;
}

export interface OracleOpRow {
  at_millis: number;
  class: string;
  words: LocalizedText;
  detail: LocalizedText;
}

export interface BlockedAppRow {
  client_id: string;
  blocked_at_millis: number;
}

export interface ConnectedAppsSnapshot {
  loaded: boolean;
  requests: ConsentCardRow[];
  principals: ConnectedAppRow[];
  blocked: BlockedAppRow[];
  /** The account's external-apps switch is OFF, as the last read that answered
   *  said — what the toggle (`atproto-external-apps-enable`) paints. ON is the
   *  zero value. */
  external_apps_off: boolean;
  /** The open per-operation record, if any — the machine's on-demand read,
   *  at most one open (`openRecord` / `closeRecord`). */
  record?: OracleRecord | null;
  error: LocalizedText | null;
}

/** One principal's per-operation record, open on the page (`snapshots.rs`). */
export interface OracleRecord {
  /** The row key it belongs to. */
  key: string;
  /** Whether the read has returned: before it, neither operations nor the
   *  empty state paint. */
  loaded: boolean;
  /** Newest first. */
  ops: OracleOpRow[];
}
