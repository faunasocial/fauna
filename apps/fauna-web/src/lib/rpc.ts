// The web SPA's WS-RPC façade (part of the WS-RPC adoption migration; tracked internally).
//
// Owns the singleton browser `WsRpcClient` (one WebSocket per actor session,
// subprotocol-bearer handshake) and exposes typed `fauna.bridges.*` /
// `fauna.email.*` calls. The kind-composition + CBOR encoding all happen
// Rust-side (`libs/fauna-wasm/src/rpc.rs` over the shared `BridgesClient<R>` /
// `EmailClient<R>` wrappers); this module is the thin TS seam.
//
// Carries **both** directions: request/reply, and the server-initiated push
// stream (`onPushEvent` below). It fully replaced `$lib/ws.ts`, a second raw
// WebSocket that still spoke the retired `?token=` query-auth form — which the
// nest answers with 401, so it never delivered a push and simply reconnected
// every 3s forever.
//
// The TS interfaces below mirror the Rust wire types in
// `libs/fauna-protocol::{bridges_ui, email}` field-by-field; they are the
// single source of truth re-exported by `bridges.ts` / `api.ts`.

import { get } from 'svelte/store';
import {
  createWsRpcClient,
  actorIdFromSecret,
  buildKnockPayload,
  staleSurfacesForPushKind,
  staleSurfacesOnReconnect,
  type LogEntry,
  type BackupAuditAlertReason,
  type StaleSurfaces,
  type RegionPlaneHandle,
  type ReportTarget,
  type ReportForm,
  type ReportLedgerRow,
  type ReportQueueRow,
} from './wasm';
import { guardSingletonBuild } from './singleton-build';
import { resetActorScopedState } from './actorScope';
import { nodeUrl, storedNestUrl, getAuthToken } from './api';
import { mailExportSavePort } from './mail-export-save';
import { SignInRefusedError } from './auth-errors';
import { escalateIfTerminalAuthVerdict } from './post-auth-escalation';
import { hexToBytes } from './hex';
import type { SharedRpcPort } from '../../static/fauna_wasm.js';
import { identity, reconnectTick, connectionStatus } from './store';
import type { LocalizedText } from '$lib/i18n/localized';
import type { SupervisionSnapshotValue } from './supervisionRestore';
import type { BlockedPeer, ContactAsk, FeedAsk } from './ward-asks';
import type { FilterRule } from './types';
import type {
  WsRpcClient,
  WasmDnsManagementMachine,
  WasmLocalDomainMachine,
  WasmBridgeApprovalMachine,
  WasmForwarderMachine,
  WasmLinkedNestsMachine,
  WasmMailSettingsMachine,
  WasmMailAliasesMachine,
  WasmMailSpamMachine,
  WasmMailExportMachine,
  WasmMailImportMachine,
  WasmMailListsMachine,
  WasmMailListMembersMachine,
  WasmMailPolicyMachine,
  WasmCaldavPolicyMachine,
  WasmCarddavPolicyMachine,
  WasmWebdavPolicyMachine,
  WasmConversationsManager,
  WasmEventDrafts,
  WasmFeedManager,
  WasmSearchManager,
  WasmTaskDelegationView,
} from '../../static/fauna_wasm.js';
import type {
  DeviceSummary,
  FolderActorMember,
  FolderDestinationPlace,
  FolderDevice,
  FolderMember,
} from '$lib/devices-machine';
import type { IssuerForcedArm, IssuerKeyView } from '$lib/admin-oauth-keys';

// ── Wire types (mirror fauna_protocol::bridges_ui / email) ──────────

export interface BridgeIdentity {
  label: string;
  value: string;
  display: string;
}

export interface BridgeSettingOption {
  value: number | string;
  label: string;
}

export interface BridgeSetting {
  key: string;
  label: string;
  type: string;
  // A provider-defined scalar (`CborValue` on the wire) read polymorphically
  // by the page per `type` (`bool` → checkbox, `text` → input, `select` →
  // option value), so it stays `any` like the original HTTP-era interface.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  value: any;
  options: BridgeSettingOption[] | null;
}

export interface BridgeLinkField {
  key: string;
  label: string;
  type: string;
  placeholder: string | null;
}

export interface BridgeLinkMode {
  mode: string;
  label: string;
  client_action: string | null;
  platform: string | null;
  fields: BridgeLinkField[];
}

export interface BridgeStatus {
  id: string;
  name: string;
  available: boolean;
  linked: boolean;
  identity: BridgeIdentity | null;
  mode: string | null;
  settings: BridgeSetting[];
  supports_follows: boolean;
  link_modes: BridgeLinkMode[] | null;
  // Present (a provider `status(...)` error string) only on failure; the
  // typed wire always carries the slot, unlike the HTTP twin's omit-on-none.
  error: string | null;
}

export interface BridgeFollow {
  id: string;
  petname: string | null;
  created_at: number | null;
  extra: Record<string, unknown> | null;
}

export interface BridgeLinkReply {
  linked: boolean;
  identity: BridgeIdentity | null;
  redirect_url: string | null;
}

// `fauna.bridges.link_challenge` — what an external signer must sign before a
// link in that mode is accepted. `payload` is provider-shaped (Nostr `nip07`:
// the unsigned kind-22242 event `window.nostr.signEvent` takes).
export interface BridgeLinkChallengeReply {
  challenge: string;
  expires_at: number;
  payload: unknown;
}

export interface FeedSubscription {
  id: number;
  bridge: string;
  feed_uri: string;
  name: string;
  created_at: number;
}

// `action` is the externally-tagged EmailFilterAction: a string for the unit
// variants (`"Allow"`, `"Discard"`) or an object (`{ Reject: { reason } }`,
// `{ FileInto: { mailbox } }`, …). `rules` is `EmailFilterRule[]` in the same
// externally-tagged shape (`{ SenderIs: { address } }`, …).
export interface EmailFilter {
  id: number;
  name: string;
  rules: unknown[];
  combination: string;
  action: string | Record<string, unknown>;
  priority: number;
  created_at: number;
}

export interface SendEmailReply {
  local_delivered: number;
  remote_queued: number;
  remote_errors: string[];
}

// ── Wire types (mirror fauna_protocol::nostr bunker control plane) ──
// The NIP-46 bunker *Connected apps* roster (`fauna.nostr.bunker.*`,
// `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer). Consumed by
// NostrSettingsSection's Connected apps card via `$lib/nostr.ts`.

/** Reply for `fauna.nostr.bunker.create_invite` — the one-time reveal. The
 *  `connect_string` (`bunker://…&secret=…`) is composed nest-side and shown
 *  once (text + copy + QR); only its hash rests on the nest. */
export interface BunkerInvite {
  connection_id: number;
  connect_string: string;
  signer_pubkey: string;
  expires_at: number;
}

/** One connected-app row (`nostr-bunker-app-item`) — pending (invite
 *  outstanding) or active. */
export interface BunkerApp {
  id: number;
  app_pubkey?: string;
  label: string;
  status: string; // "pending" | "active"
  created_at: number;
  last_used_at?: number;
  use_count: number;
  expires_at: number;
}

// The NIP-57 zap-signer trust root (`fauna.nostr.zap_signers.*`, `docs/goal/
// behavior/monetization.md` § Zap receipts — the trust model). A kind-9735
// zap receipt is signed by the payee's LNURL/wallet server and is plain
// signed JSON anyone may mint, so its own signature proves nothing — this
// roster is what makes one believable. Consumed by NostrSettingsSection's
// Zap signers card via `$lib/nostr.ts`.

/** One designated zap signer (`nostr-zap-signer-item`). */
export interface ZapSignerEntry {
  id: number;
  signer_pubkey: string;
  label: string;
  created_at: number;
}

// ── Wire types (mirror fauna_protocol::feed / posts) ────────────────

// `fauna.feed.list` row — summary view, omits rules (rules ride only on
// `fauna.feed.get`).
export interface FeedSummary {
  feed_id: string;
  owner: string;
  name: string;
  combination: string;
  created_at: number;
  scope: string;
  contributor_seeds: string[];
}

// `fauna.feed.get` reply — the full feed including its typed rules (an array
// of externally-tagged `FilterRule` objects).
export interface FeedGetReply {
  feed_id: string;
  owner: string;
  name: string;
  rules: FilterRule[];
  combination: string;
  created_at: number;
  scope: string;
  contributor_seeds: string[];
}

export interface FeedCreateReply {
  feed_id: string;
}

// One post in a feed-query result. `score` (micro-units ×1e6) is present
// only on `order=score` queries. The post body text rides as `body`, but the
// SPA decodes the signed post separately (`fetchAndDecodePost`), so callers
// map only the index fields.
export interface FeedPostItem {
  post_id: string;
  author: string;
  body: string;
  created_at: number;
  tags: string[];
  has_media: boolean;
  is_reply: boolean;
  source: string;
  score?: number;
}

export interface FeedPostsReply {
  posts: FeedPostItem[];
  cursor?: number;
  score_cursor?: number;
}

export interface FeedLocalPostsReply {
  posts: FeedPostItem[];
  cursor?: number;
}

export interface PostCreateReply {
  post_id: string;
}

// `result` is the protocol-specific JSON body the HTTP twin returned, as a
// string (the nest round-trips heterogeneous interaction replies this way).
export interface PostInteractReply {
  action: string;
  source: string;
  result: string;
}

// ── Wire types (mirror fauna_protocol::contacts / notifications) ────
//
// The knock + contact rows are field-identical to the SPA's `Knock` / `Contact`
// (`$lib/types`), so `api.ts` returns them directly (structural assignment); the
// notification row carries `notif_type`, which `notifications.ts` renames to
// `type` for the SPA's `UnifiedNotification`.

// `fauna.knocks.list` row — one pending inbound knock. `sender` is the hex
// actor id; `created_at` is millis since epoch.
export interface KnockItem {
  id: number;
  sender: string;
  sender_node: string;
  summary: string;
  created_at: number;
}

// `fauna.contacts.list` row — one contact-roster entry. `peer_id` is the hex
// actor id; `accepted_at` (secs) is absent until the contact is accepted.
export interface ContactItem {
  peer_id: string;
  status: string;
  accepted_at?: number;
  created_at: number;
}

// `fauna.notifications.list` row. `notif_type` is the wire key (the HTTP twin's
// `type`); `sender_id` / `content_id` are hex actor ids when present.
export interface NotifItem {
  id: number;
  notif_type: string;
  source: string;
  sender_id?: string | null;
  content_id?: string | null;
  subject_uri?: string | null;
  summary: string;
  /** The row's sentence as a catalog key + data args (`notifications.md`
   *  § Localized body); absent on a row with no catalog body. Never painted
   *  directly — `notificationText` decides between it and `summary`. */
  body?: { key: string; args?: Record<string, string> } | null;
  is_read: boolean;
  created_at: number;
}

export interface NotifListReply {
  notifications: NotifItem[];
  cursor?: number;
}

// ── Wire types (mirror fauna_protocol::account) ─────────────────────
//
// The `fauna.quota.get` usage breakdown (`QuotaInfo`), the
// `fauna.profile.handle.change` pending-action reply, and the
// `fauna.account.delete` pending-action reply. `fauna.account.{get,upgrade}`
// have no web consumer (see the rpc.rs section comment), so no interfaces for
// them. i64 byte/count slots ride as plain JS numbers (`to_js`).

export interface QuotaUsageBytes {
  used_bytes: number;
  max_bytes: number;
}

export interface QuotaDeviceUsage {
  used: number;
  max: number;
}

export interface QuotaFeatures {
  versioned_backup: boolean;
  bridges: boolean;
  max_feeds: number;
}

// `fauna.quota.get` reply — the tier-aware usage breakdown the Settings page
// renders. Named `QuotaInfo` for continuity with the pre-WS-RPC SPA type
// (re-exported from `api.ts`).
export interface QuotaInfo {
  tier: string;
  inbox: QuotaUsageBytes;
  storage: QuotaUsageBytes;
  devices: QuotaDeviceUsage;
  features: QuotaFeatures;
}

// `fauna.profile.handle.change` reply — the queued (delayed + cancellable)
// pending action; `new_handle` is the requested handle the SPA shows
// optimistically.
export interface HandleChangeReply {
  pending_action_id: number;
  execute_after: number;
  status: string;
  new_handle: string;
}

// `fauna.account.delete` reply — the queued pending action with its
// cancellation window.
export interface AccountDeleteReply {
  pending_action_id: number;
  execute_after: number;
  status: string;
  message: string;
}

// ── Server pushes ───────────────────────────────────────────────────
//
// The SPA's single inbound push seam — the web twin of a native app looping
// over `fauna_client::NestClient::subscribe_pushes` (`transport.md` § Push
// events). The frames ride the same authenticated WS-RPC socket as every
// request; the shared `RpcDispatcher` decodes each `Frame::Push` into a typed
// `fauna_protocol::PushEvent`, and the wasm client hands it over as
// `(kind, payload)` — the same kind strings a native app matches on
// (`fauna.calendar.changed`, `fauna.notification`, `fauna.knock`, …), with `payload`
// the variant's payload object.
//
// A push is a *hint*, never the only path to a value: every surface that
// subscribes must also have a fetch path (pushes can be dropped under lag, and
// nothing replays them across a reconnect gap).

/** Handler for a decoded server push. `payload` is the kind's payload object. */
export type PushHandler = (kind: string, payload: unknown) => void;

const pushHandlers = new Set<PushHandler>();

/** Subscribe to server pushes for as long as the caller is mounted. Returns the
 *  unsubscribe function — call it in `onDestroy`, or the handler outlives the
 *  page and fires against torn-down state.
 *
 *  Check `staleSurfacesForPushKind(kind)`'s flags rather than matching `kind`
 *  by hand — the shared classifier (re-exported below) is what a `fauna.
 *  protocol.resync_required` push and a reconnect both answer through too, so
 *  a page wired this way self-heals a socket gap it currently has no other
 *  path to recover from. */
export function onPushEvent(handler: PushHandler): () => void {
  pushHandlers.add(handler);
  return () => {
    pushHandlers.delete(handler);
  };
}

export { staleSurfacesForPushKind, staleSurfacesOnReconnect, type StaleSurfaces };

// ── Singleton client lifecycle ──────────────────────────────────────

let client: WsRpcClient | null = null;
let clientActorId: string | null = null;
let clientNodeUrl: string | null = null;
let clientPromise: Promise<WsRpcClient> | null = null;
/** Bumped on every rebuild so a build that resolves after it was superseded
 *  closes itself instead of lingering as a live wrong-target connection. */
let clientGeneration = 0;

/** Neuter a superseded client's app-facing callbacks, then tear it down. The
 *  callback reset matters: the stale client's final `disconnected` transition
 *  must not stomp the global `connection-status` store the NEW client drives. */
function retireClient(stale: WsRpcClient): void {
  try {
    stale.setOnConnectionStateChanged(() => {});
    stale.setOnReconnected(() => {});
    stale.setOnPushEvent(() => {});
    stale.close();
  } catch (e) {
    console.warn('stale WS-RPC client teardown failed:', e);
  }
}

/** The connected singleton for the given identity **and current target nest**,
 *  building it on first use — or rebuilding (with a clean teardown of the old
 *  one) when the logged-in actor OR `nodeUrl()` changes. The URL half of the
 *  key is load-bearing: the client captures its target at construction, so a
 *  singleton built before the home nest was recorded (the same-origin dev/
 *  production fallback) would otherwise keep dialing the wrong nest forever —
 *  measured in the mid-claim crash-recovery journey, where the relaunched
 *  client's boot built the client pre-injection and every later admin check
 *  auth-failed against the SPA origin in a permanent loop. The bearer flows
 *  from `getAuthToken` — the one TS-side token cache — via the client's token
 *  provider. */
async function getClient(secretHex: string): Promise<WsRpcClient> {
  const actorId = actorIdFromSecret(secretHex);
  const url = nodeUrl();
  if (client && clientActorId === actorId && clientNodeUrl === url) return client;
  if (!clientPromise || clientActorId !== actorId || clientNodeUrl !== url) {
    if (client) {
      retireClient(client);
      client = null;
      // ⚠ RETIRING THE CLIENT KILLS EVERYTHING BUILT OVER IT, so this is a
      // teardown site and it calls the app's ONE canonical drop — the rule
      // `account-scoping.md` § The scoping taxonomy states in those words
      // ("exactly one canonical drop per app, and every teardown site calls it
      // with no list of its own"). Until 2026-09-09 this site called nothing.
      //
      // The page managers are not merely *associated* with the client, they
      // WRAP it: `c.feedManager(...)` / `c.searchManager()` construct
      // `FeedManager<WsRpcClient>`-shaped values over this connection, and
      // `WsRpcClient::close` latches a one-way `closed` flag — a closed client
      // never reconnects. So a manager left memoized across a retire is bound
      // to a permanently dead transport, and `getFeedManager()` (which keys on
      // NOTHING) would keep vending it for the rest of the page's life.
      //
      // Reaching here means the KEY changed: the `client && same actor && same
      // url` fast path above already returned, so a same-key re-entry never
      // gets this far. The actor half is usually a double-drop (`store.ts`
      // fired one on the identity change) — harmless, drops are idempotent by
      // contract — and the NEST half has no other trigger at all: `store.ts`
      // subscribes to the identity and early-returns on an unchanged
      // `secretHex`, so a nest change fires nothing anywhere else. That half is
      // this call's whole reason to exist.
      //
      // Deliberately NOT inside `retireClient` itself: that also runs on the
      // supersede arms below, where the retired instance is a freshly-built one
      // no manager was ever built over, and dropping there would throw away the
      // live managers of the client that WON.
      resetActorScopedState();
    }
    clientActorId = actorId;
    clientNodeUrl = url;
    const generation = ++clientGeneration;
    const build = (stillWanted: () => boolean): Promise<WsRpcClient> => createWsRpcClient(
      url,
      actorId,
      (forceRefresh: boolean) => getAuthToken(secretHex, undefined, forceRefresh),
    ).then((c) => {
      // Superseded while building (identity/nest changed again mid-flight), or
      // ABANDONED by the settle deadline below while still alive: retire this
      // instance instead of registering it as the live client.
      //
      // ⚠ ABANDONING A BUILD IS NOT THE SAME AS ITS FAILING, and here the
      // difference has teeth. On a rejection there is no client and nothing to
      // clean up. On a deadline expiry the task may merely be pathologically
      // slow rather than dead, so it arrives here later still alive — and
      // registering it would put its push fan-out and connection-state writes in
      // the SHARED stores beside the replacement build's, with its socket left
      // open and nobody holding it. `stillWanted()` is the guard's own answer to
      // "was this build given up on" — the trigger every memoized build in the
      // SPA consults (`singleton-build.ts`); what differs here is the remedy,
      // a retire rather than a skipped write.
      if (generation !== clientGeneration || !stillWanted()) {
        retireClient(c);
        return c;
      }
      // Re-hydrate surfaces with no poll backstop (the feed) on every reconnect
      // — the wasm twin of native `subscribe_reconnects` (transport.md § Push
      // events). Registered once per built client; bumping the shared store
      // lets whichever page is mounted re-fetch itself (the feed page does).
      c.setOnReconnected(() => reconnectTick.update((n) => n + 1));
      // Drive the global `connection-status` indicator live (Connecting/
      // Connected/Disconnected) — the wasm twin of a native app observing
      // `NestClient::connection_state()`. Seed with the current state (the
      // client starts Connecting), then track every transition so a swap gap
      // shows without surfacing as an error.
      connectionStatus.set(c.connectionState());
      c.setOnConnectionStateChanged((state: string) => connectionStatus.set(state));
      // Fan the one inbound push stream out to every page that subscribed. The
      // wasm `setOnPushEvent` is a single slot, so rpc.ts owns the registration
      // and `onPushEvent` multiplexes it — otherwise two mounted pages would
      // silently overwrite each other's handler.
      c.setOnPushEvent((kind: string, payload: unknown) => {
        for (const h of [...pushHandlers]) {
          try {
            h(kind, payload);
          } catch (e) {
            console.error('push handler threw', kind, e);
          }
        }
      });
      return c;
    });
    // ⚠ A PROMISE MEMO CACHES A REJECTION AS DURABLY AS A VALUE — and this memo
    // is the ROOT one: every RPC in the SPA reaches the nest through this
    // singleton, so one rejected build here is not one rail's outage but every
    // page's, for the rest of the page's life. `createWsRpcClient` awaits
    // `ensureWasm()` (a chunk fetch + wasm instantiation, both of which a loaded
    // box can fail transiently); `ensureWasm` clears its OWN memo so a retry
    // would succeed, but nothing ever retried, because this slot kept handing
    // out the rejection.
    //
    // The (actor, nest URL) key cannot save it: that forces a rebuild only when
    // one of them CHANGES, and the commonest re-entry by far is the SAME actor
    // calling again — which is exactly the shape `singleton-build-memo-contract.
    // test.ts` was written for, one layer above this one.
    //
    // `=== guarded` guards the same hazard `getFeedManager` names: by the time
    // this runs, a newer build (a real actor/nest switch) may already own the
    // slot, and clearing unconditionally would throw that live build away.
    // `clientActorId`/`clientNodeUrl` are deliberately left as they are — with
    // the promise cleared, the `!clientPromise` arm above rebuilds on the next
    // call regardless of what they hold.
    //
    // ⚠ AND THE OTHER HALF: a build that never SETTLES is memoized exactly as
    // durably, and `.catch` never fires for a promise with no terminal state.
    // Here that is the worst version of it in the SPA — every page's every RPC
    // awaits this one promise, so a dead build silences the whole app rather
    // than one rail, and `ensureConnected`'s 15 s throw is downstream of it and
    // never even reached. `guardSingletonBuild` adds the external settle
    // deadline that survives the wasm task's death, and hands `build` the
    // `stillWanted` its late-arrival arm above retires on.
    //
    // (Until 2026-09-10 that arm was reached by bumping `clientGeneration` in
    // this clear. It worked, but it had to sit inside the `=== guarded` check —
    // bumping while a newer build owned the slot would have retired the LIVE
    // client — and it gave this builder a private abandonment signal the other
    // four did not share. The generation now answers only what it was made for:
    // superseded by a key change.)
    const guarded = guardSingletonBuild('WS-RPC client', build, () => {
      if (clientPromise === guarded) clientPromise = null;
    });
    clientPromise = guarded;
  }
  const mine = clientPromise;
  const built = await mine;
  // Superseded while awaiting (identity/nest changed again): the retired
  // instance must not become the live singleton — defer to the current build.
  if (clientPromise !== mine) return getClient(secretHex);
  client = built;
  return client;
}

const CONNECT_TIMEOUT_MS = 15_000;

/** The error a stopped client's calls fail with — typed when the stop is a
 *  session-ending verdict. The loop stops on the nest's sign-in refusal
 *  (`sessionEndingVerdict()`, the wasm twin of native
 *  `NestClientError::session_ending_verdict`), and that verdict routes to the
 *  launch surface (`security.md` § Post-auth surfacing). `getAuthToken` has
 *  normally escalated it already, as the refusal's first witness; this is the
 *  second, and the escalation's latch makes a repeat a no-op. */
function stoppedError(c: WsRpcClient, stop: string): Error {
  if (c.sessionEndingVerdict() !== 'sign_in_refused') return new Error(stop);
  const refused = new SignInRefusedError(nodeUrl());
  escalateIfTerminalAuthVerdict(refused);
  return refused;
}

/** Wait for the reconnect loop to bring the socket up before issuing a
 *  request — or fail at once, with why, when that loop has stopped for good
 *  and no connection can come (`supervisorStop`; transport-connection.md
 *  § Connection lifecycle). */
async function ensureConnected(c: WsRpcClient): Promise<void> {
  const deadline = Date.now() + CONNECT_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (c.connectionState() === 'connected') return;
    const stop = c.supervisorStop();
    if (stop !== undefined) throw stoppedError(c, stop);
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error('WS-RPC client failed to connect within 15s');
}

async function call<T>(secretHex: string, fn: (c: WsRpcClient) => Promise<T>): Promise<T> {
  const c = await getClient(secretHex);
  await ensureConnected(c);
  return fn(c);
}

/** `call` for a seam that must live OUTSIDE this file. The one caller today is
 *  `$lib/payments`, whose functions are here-shaped but may not be *here*: this
 *  module is unconditionally in the bundle, so a `payments*` function left in it
 *  would ship its own name into the store-safe artifact even with every caller
 *  folded away (`dynamic-features.md` § Platform-family surface excision — the
 *  isolated-module pattern). Every ungated call belongs in this file; reach for
 *  this only when a face's NAME must be absent from an excised flavor. */
export function rpcCall<T>(secretHex: string, fn: (c: WsRpcClient) => Promise<T>): Promise<T> {
  return call(secretHex, fn);
}

/** Ask the session's nest (the relay, `fauna.region.artifact.get`) for every
 *  policy on the region plane's declared chain — the plane folds and verifies
 *  in shared Rust and resolves to the new device record, or `null` when
 *  nothing changed (`$lib/region.svelte`). */
export function regionRefresh(secretHex: string, plane: RegionPlaneHandle): Promise<Uint8Array | null> {
  return call(secretHex, (c) => plane.refresh(c));
}

/** Connection state for status surfaces, or `'disconnected'` if no client yet. */
export function connectionState(): string {
  return client?.connectionState() ?? 'disconnected';
}

export type { SharedRpcPort };

/**
 * The singleton's socket, lent to another wasm chunk — the ONE way a
 * page-machine chunk (folders, media, backups, labeler catalog, atproto
 * settings) reaches the nest, so the SPA holds one WebSocket per actor
 * (`docs/goal/architecture/apps/web.md` § Transport; `transport.md` § Goal).
 *
 * The port is typed by the chunks themselves: `SharedRpcPort` is declared in
 * the wasm `.d.ts` (`fauna_rpc_wasm::shared_port`), every chunk constructor
 * takes exactly it, and this is its one implementation. Only pure data
 * crosses — kind strings, canonical-CBOR `Uint8Array`s, the bearer string —
 * never a wasm-bindgen object, per the chunk discipline.
 *
 * Every method resolves the CURRENT singleton on each call rather than
 * capturing the client it was built over: `getClient` re-keys on an
 * identity or nest change and retires the old client, and a machine that
 * outlives that swap (the devices session memo, the atproto singleton) must
 * follow the socket, not hold a retired one. A request issued in a reconnect
 * gap waits it out inside the wasm client (`dispatcher_within_deadline`,
 * bounded by the kind's deadline) exactly as the core's own requests do — so
 * a chunk's gesture is exactly as connected as the `connection-status`
 * indicator says, never behind a chunk-private backoff.
 */
export async function sharedRpcPort(secretHex: string): Promise<SharedRpcPort> {
  // Build the singleton first so `nestUrl`/`connectionState` below have a
  // client to read on their first synchronous call.
  await getClient(secretHex);
  return {
    request: async (kind: string, idempotencyKey: Uint8Array, payload: Uint8Array) => {
      const c = await getClient(secretHex);
      return c.requestRaw(kind, idempotencyKey, payload);
    },
    bearer: (forceRefresh: boolean) => getAuthToken(secretHex, undefined, forceRefresh),
    nestUrl: () => client?.nestUrl() ?? nodeUrl(),
    actorIdHex: () => actorIdFromSecret(secretHex),
    connectionState,
  };
}

/** Test-only: pace the live client's reconnect retries, or restore them with
 *  `{}` — the web leg of `fauna_e2e_agent::RECONNECT_BACKOFF`. The payload is
 *  handed to wasm verbatim and parsed there by the natives' own parser, so every
 *  app refuses the same malformed payloads. Throws with no client (nothing to
 *  pace) and on a production bundle, whose wasm compiles the seam out
 *  (convention 11: a pace that did not land would leave the production one in
 *  force while the journey spent its budget waiting). */
export function setReconnectBackoffForTest(payload: unknown): void {
  if (!client) throw new Error('reconnect_backoff: no rpc client, so no connection to pace');
  const fn = (client as unknown as Record<string, unknown>).setReconnectBackoffForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'setReconnectBackoffForTest is absent: this SPA is running the PRODUCTION ' +
        'wasm flavor, which compiles the e2e seams out. Build with `just web-test`.',
    );
  }
  (fn as (json: string) => void).call(client, JSON.stringify(payload ?? {}));
}

// ── fauna.web.* (web-content authoring) ──────────────────────────────
// The thin web-content client — the web twin of the linux native `WebClient`
// + the native FFI `FfiWebClient`. Drives the user `web-settings` subdomain
// toggle + the `admin-web` apex picker (web-content-hosting.md § Client
// authoring UI). Direct WS-RPC calls (like `bridgesList`), not a machine.

/** `fauna.nest.info` → the host the NEST routes web content on, which is the
 *  only legitimate `domain` input to `webSubdomainView` / `webSiteLinkView`.
 *  An empty string is a real answer ("this nest serves no web content") and must
 *  be passed through, not replaced. */
export function webServingDomain(secretHex: string): Promise<string> {
  return call(secretHex, (c) => c.webServingDomain() as Promise<string>);
}

/** `fauna.web.get_subdomain_enabled` → the calling actor's subdomain opt-in. */
export function webGetSubdomainEnabled(secretHex: string): Promise<boolean> {
  return call(secretHex, (c) => c.webGetSubdomainEnabled() as Promise<boolean>);
}

/** `fauna.web.set_subdomain_enabled` → flip it; resolves the nest-confirmed state. */
export function webSetSubdomainEnabled(secretHex: string, enabled: boolean): Promise<boolean> {
  return call(secretHex, (c) => c.webSetSubdomainEnabled(enabled) as Promise<boolean>);
}

/** `fauna.web.get_apex_actor` → the apex actor id, or `null` (info page). */
export function webGetApexActor(secretHex: string): Promise<Uint8Array | null> {
  return call(secretHex, (c) => c.webGetApexActor() as Promise<Uint8Array | null>);
}

// ── fauna.web.publish.* / fauna.web.paywall.* (Published-post management;
//    web-content-hosting.md § Published-post management) — the `web-settings`
//    Published-posts section + the feed ⋯-overflow verbs. ──────────────────
//
// `postId` is whatever byte shape the caller already holds — the feed's own
// `PostSummary.post_id` is hex (decode with `$lib/hex`'s `hexToBytes` first);
// `webPublishedSite`'s rows carry `post_id` as the plain byte array the wire
// reply already deserializes to, passed straight back through unchanged.

/** `fauna.web.publish.set` → the EFFECTIVE slug (`null` slug ⇒ the nest's
 *  post-id-hex default). Build the copy-link URL from this echo, never from
 *  the requested slug. */
export function webPublishSet(
  secretHex: string,
  postId: Uint8Array | number[],
  slug: string | null,
): Promise<string> {
  return call(secretHex, (c) => c.webPublishSet(postId, slug ?? undefined) as Promise<string>);
}

/** `fauna.web.publish.unset` → take a published post down. Idempotent and
 *  reversible, so the UI offers it as a one-tap verb with no confirm step. */
export function webPublishUnset(
  secretHex: string,
  postId: Uint8Array | number[],
): Promise<boolean> {
  return call(secretHex, (c) => c.webPublishUnset(postId) as Promise<boolean>);
}

/** `fauna.web.domain.get` → `{domain, status}[]`, already narrowed to what
 *  `webSiteLinkView` takes. Only an `active` row resolves to an origin. */
export function webDomainGet(
  secretHex: string,
): Promise<Array<{ domain: string; status: string }>> {
  return call(
    secretHex,
    (c) => c.webDomainGet() as Promise<Array<{ domain: string; status: string }>>,
  );
}

/** `fauna.web.publish.list`, whole → the `web-published-posts-list` rows plus
 *  whether the nest has blanked the caller's rendered pages
 *  (`rendered_pages_down`, the `web-settings-render-status` line). A row
 *  without `gated_tier` is ungated: no paywall-link affordance. `post_id` is
 *  the plain byte array the JSON-compatible wire serializer produces (never a
 *  hex string) — hand it straight back to `webPublishUnset`/
 *  `webPaywallMintToken`. */
export interface PublishedSiteRead {
  posts: Array<{ post_id: number[]; slug: string; gated_tier?: string }>;
  rendered_pages_down: boolean;
}

export function webPublishedSite(secretHex: string): Promise<PublishedSiteRead> {
  return call(secretHex, (c) => c.webPublishedSite() as Promise<PublishedSiteRead>);
}

/** `fauna.web.paywall.mint_token` for a published+gated own post — a fresh
 *  mint per call, since the token is short-lived by ratified design and
 *  re-minting is free (`monetization.md` § Pillar 2 → Creator comp-link
 *  surface). */
export function webPaywallMintToken(
  secretHex: string,
  slug: string,
): Promise<{ token: string; expires: number; path: string }> {
  return call(
    secretHex,
    (c) =>
      c.webPaywallMintToken(slug, undefined) as Promise<{
        token: string;
        expires: number;
        path: string;
      }>,
  );
}

/** `fauna.web.set_apex_actor` → designate (`actorId`) or clear (`null`); resolves
 *  the nest-confirmed designation. */
export function webSetApexActor(
  secretHex: string,
  actorId: Uint8Array | null,
): Promise<Uint8Array | null> {
  return call(
    secretHex,
    (c) => c.webSetApexActor(actorId ? Array.from(actorId) : null) as Promise<Uint8Array | null>,
  );
}

// ── fauna.bridges.* ─────────────────────────────────────────────────

export function bridgesList(secretHex: string): Promise<BridgeStatus[]> {
  return call(secretHex, (c) => c.bridgesList());
}

export function bridgesLink(
  secretHex: string,
  bridgeId: string,
  mode: string,
  params: Record<string, unknown>,
): Promise<BridgeLinkReply> {
  return call(secretHex, (c) => c.bridgesLink(bridgeId, mode, params));
}

export function bridgesLinkChallenge(
  secretHex: string,
  bridgeId: string,
  mode: string,
): Promise<BridgeLinkChallengeReply> {
  return call(secretHex, (c) => c.bridgesLinkChallenge(bridgeId, mode));
}

export function bridgesUnlink(secretHex: string, bridgeId: string): Promise<void> {
  return call(secretHex, (c) => c.bridgesUnlink(bridgeId));
}

export function bridgesSetSettings(
  secretHex: string,
  bridgeId: string,
  settings: Record<string, unknown>,
): Promise<void> {
  return call(secretHex, (c) => c.bridgesSetSettings(bridgeId, settings));
}

export function bridgesListFollows(secretHex: string, bridgeId: string): Promise<BridgeFollow[]> {
  return call(secretHex, (c) => c.bridgesListFollows(bridgeId));
}

export function bridgesAddFollow(
  secretHex: string,
  bridgeId: string,
  id: string,
  petname?: string,
  extra?: Record<string, unknown>,
): Promise<void> {
  return call(secretHex, (c) =>
    c.bridgesAddFollow(bridgeId, id, petname ?? null, extra ?? null),
  );
}

export function bridgesRemoveFollow(
  secretHex: string,
  bridgeId: string,
  followId: string,
): Promise<void> {
  return call(secretHex, (c) => c.bridgesRemoveFollow(bridgeId, followId));
}

export function bridgesFeedsList(secretHex: string): Promise<FeedSubscription[]> {
  return call(secretHex, (c) => c.bridgesFeedsList());
}

export function bridgesFeedsCreate(
  secretHex: string,
  bridge: string,
  feedUri: string,
  name: string,
): Promise<number> {
  return call(secretHex, (c) => c.bridgesFeedsCreate(bridge, feedUri, name));
}

export function bridgesFeedsDelete(secretHex: string, id: number): Promise<void> {
  return call(secretHex, (c) => c.bridgesFeedsDelete(id));
}

// ── fauna.email.* ───────────────────────────────────────────────────

export function emailFiltersList(secretHex: string): Promise<EmailFilter[]> {
  return call(secretHex, (c) => c.emailFiltersList());
}

export function emailFiltersCreate(
  secretHex: string,
  name: string,
  rules: unknown[],
  combination: string,
  action: string | Record<string, unknown>,
  priority: number,
): Promise<number> {
  return call(secretHex, (c) => c.emailFiltersCreate(name, rules, combination, action, priority));
}

export function emailFiltersGet(secretHex: string, id: number): Promise<EmailFilter> {
  return call(secretHex, (c) => c.emailFiltersGet(id));
}

export function emailFiltersUpdate(
  secretHex: string,
  id: number,
  name: string,
  rules: unknown[],
  combination: string,
  action: string | Record<string, unknown>,
  priority: number,
): Promise<void> {
  return call(secretHex, (c) => c.emailFiltersUpdate(id, name, rules, combination, action, priority));
}

export function emailFiltersDelete(secretHex: string, id: number): Promise<void> {
  return call(secretHex, (c) => c.emailFiltersDelete(id));
}

// ── The post-succession filter-mark review (succession-aftermath.md §
// Adjudicating what the aftermath carries across, the fourth plane). `filterMarkRemoved` never deletes — this plane has
// no second removal mechanism, so the caller's own emailFiltersDelete owns
// the deletion, called FIRST; filterMarkRemoved only records the verdict
// afterward. ──

/** The ids of every filter rule still awaiting the owner's verdict. Cache
 *  what this returns and answer per-row questions against it: the filter
 *  list paints far more often than the plane changes. */
export function filterMarksList(secretHex: string): Promise<number[]> {
  return call(secretHex, (c) => c.filterMarksList() as Promise<number[]>);
}

/** Record **Keep** — the owner recognises this rule; it stays, and the mark
 *  clears. Resolves to whether anything was actually open (a concurrent
 *  device may have already answered — a success no-op, never a rejection). */
export function filterMarkKeep(secretHex: string, filterId: number): Promise<boolean> {
  return call(secretHex, (c) => c.filterMarkKeep(filterId) as Promise<boolean>);
}

/** Record **Removed** — call ONLY after `emailFiltersDelete` has already
 *  deleted the rule. Resolves to whether anything was actually open. */
export function filterMarkRemoved(secretHex: string, filterId: number): Promise<boolean> {
  return call(secretHex, (c) => c.filterMarkRemoved(filterId) as Promise<boolean>);
}

/** Thin `fauna.email.send` binding — submits an already-composed RFC 5322
 *  message (raw bytes) over WS-RPC. Awaiting its UI consumer: the general
 *  mail-compose surface (`ui.yaml` "mail-write" track) will compose via the
 *  shared WASM-safe `fauna_conversations::rfc5322::build_message` and pass the
 *  bytes here (the old hand-rolled `sendEmailSmtp` composer was deleted — see
 *  `api.ts`). Binding-precedes-consumer, the same pattern as the mail-* IDs. */
export function emailSend(
  secretHex: string,
  recipients: string[],
  rawRfc5322: Uint8Array,
): Promise<SendEmailReply> {
  return call(secretHex, (c) => c.emailSend(recipients, rawRfc5322));
}

// ── fauna.nostr.bunker.* ────────────────────────────────────────────

/** `fauna.nostr.bunker.create_invite` → mint a pending connection; the reply's
 *  `connect_string` is the one-time `bunker://…` reveal. */
export function nostrBunkerCreateInvite(secretHex: string): Promise<BunkerInvite> {
  return call(secretHex, (c) => c.nostrBunkerCreateInvite() as Promise<BunkerInvite>);
}

/** `fauna.nostr.bunker.list` → the caller's connected-app roster. */
export function nostrBunkerList(secretHex: string): Promise<BunkerApp[]> {
  return call(secretHex, (c) => c.nostrBunkerList() as Promise<BunkerApp[]>);
}

/** `fauna.nostr.bunker.revoke` → `true` if a live caller-owned row was revoked. */
export function nostrBunkerRevoke(secretHex: string, connectionId: number): Promise<boolean> {
  return call(secretHex, (c) => c.nostrBunkerRevoke(connectionId) as Promise<boolean>);
}

/** `fauna.nostr.bunker.set_label` → `true` if a live caller-owned row was relabeled. */
export function nostrBunkerSetLabel(
  secretHex: string,
  connectionId: number,
  label: string,
): Promise<boolean> {
  return call(secretHex, (c) => c.nostrBunkerSetLabel(connectionId, label) as Promise<boolean>);
}

// ── fauna.nostr.zap_signers.* ────────────────────────────────────────

/** `fauna.nostr.zap_signers.list` → the caller's designated signers, newest
 *  first. An empty array is the meaningful out-of-the-box default (a payee
 *  who has designated nobody believes nobody), not a failed load. */
export function nostrZapSignersList(secretHex: string): Promise<ZapSignerEntry[]> {
  return call(secretHex, (c) => c.nostrZapSignersList() as Promise<ZapSignerEntry[]>);
}

/** `fauna.nostr.zap_signers.add` → the stored row. Render its
 *  `signer_pubkey`, never the typed input — the nest normalizes to lowercase
 *  and only that form ever matches a receipt. */
export function nostrZapSignersAdd(
  secretHex: string,
  signerPubkey: string,
  label: string,
): Promise<ZapSignerEntry> {
  return call(secretHex, (c) => c.nostrZapSignersAdd(signerPubkey, label) as Promise<ZapSignerEntry>);
}

/** `fauna.nostr.zap_signers.remove` → `true` if a caller-owned designation
 *  was removed. Takes effect at the next receipt (the gate runs at ingest). */
export function nostrZapSignersRemove(secretHex: string, signerPubkey: string): Promise<boolean> {
  return call(secretHex, (c) => c.nostrZapSignersRemove(signerPubkey) as Promise<boolean>);
}

// ── nostr.* (protocol-native content) ───────────────────────────────
// The prefix-less protocol-native content kinds (the `bluesky.feed.thread`
// precedent) — WS-RPC successors to the deleted
// `/api/v1/nostr/{zaps,badges,publish-signed}` HTTP routes.

/** One NIP-58 badge award row (`nostr.badges.list`). */
export interface NostrBadgeItem {
  badge_id: string;
  badge_name?: string | null;
  badge_image?: string | null;
  created_at: number;
}

/** `nostr.badges.list` → badge awards for one pubkey, newest first. */
export function nostrBadges(secretHex: string, pubkey: string): Promise<NostrBadgeItem[]> {
  return call(secretHex, (c) => c.nostrBadges(pubkey) as Promise<NostrBadgeItem[]>);
}

/** `nostr.events.publish_signed` — relay-enqueue an event the NIP-07
 *  extension signed (`eventJson` = the signed event's NIP-01 wire JSON). */
export function nostrPublishSigned(secretHex: string, eventJson: string): Promise<void> {
  return call(secretHex, (c) => c.nostrPublishSigned(eventJson) as Promise<void>);
}

// ── fauna.feed.* ────────────────────────────────────────────────────

export function feedList(secretHex: string): Promise<FeedSummary[]> {
  return call(secretHex, (c) => c.feedList());
}

export function feedCreate(
  secretHex: string,
  name: string,
  rules: FilterRule[],
  combination: string,
): Promise<FeedCreateReply> {
  return call(secretHex, (c) => c.feedCreate(name, rules, combination));
}

export function feedGet(secretHex: string, feedId: string): Promise<FeedGetReply> {
  return call(secretHex, (c) => c.feedGet(feedId));
}

export function feedUpdate(
  secretHex: string,
  feedId: string,
  name: string,
  rules: FilterRule[],
  combination: string,
): Promise<void> {
  return call(secretHex, (c) => c.feedUpdate(feedId, name, rules, combination));
}

export function feedDelete(secretHex: string, feedId: string): Promise<void> {
  return call(secretHex, (c) => c.feedDelete(feedId));
}

export function feedPosts(
  secretHex: string,
  feedId: string,
  cursor?: number,
  limit?: number,
  order?: string,
  scoreCursor?: number,
  scoreCursorCreatedAt?: number,
  search?: string,
): Promise<FeedPostsReply> {
  return call(secretHex, (c) =>
    // 7 positional args — the wasm face is
    // (feed_id, cursor, limit, order, score_cursor, score_cursor_created_at, search).
    // The score cursor's two halves travel together: a nest refuses either one
    // alone (the key-only shape left the wire with the compat-remnant sweep).
    c.feedPosts(
      feedId,
      cursor ?? null,
      limit ?? null,
      order ?? null,
      scoreCursor ?? null,
      scoreCursorCreatedAt ?? null,
      search ?? null,
    ),
  );
}

export function feedLocalPosts(
  secretHex: string,
  cursor?: number,
  limit?: number,
  search?: string,
): Promise<FeedLocalPostsReply> {
  return call(secretHex, (c) => c.feedLocalPosts(cursor ?? null, limit ?? null, search ?? null));
}

// ── fauna.posts.* ───────────────────────────────────────────────────

export function postsCreate(secretHex: string, body: Uint8Array): Promise<PostCreateReply> {
  return call(secretHex, (c) => c.postsCreate(body));
}

/** Resolves the raw resolved post bytes (the SPA decodes them client-side). */
export function postsGet(secretHex: string, postId: string): Promise<Uint8Array> {
  return call(secretHex, (c) => c.postsGet(postId) as Promise<Uint8Array>);
}

export function postsInteract(
  secretHex: string,
  postId: string,
  action: string,
  body?: string,
): Promise<PostInteractReply> {
  return call(secretHex, (c) => c.postsInteract(postId, action, body ?? null));
}

// ── fauna.knocks.* / fauna.contacts.* / fauna.inbox.mode.* ──────────
//
// The connection actor is the calling actor, so no `actorId` rides (it replaced
// the deleted HTTP routes' `{actor_id}` path segment).

export function knocksList(secretHex: string): Promise<KnockItem[]> {
  return call(secretHex, (c) => c.knocksList());
}

export function knocksAccept(secretHex: string, peerId: string): Promise<void> {
  return call(secretHex, (c) => c.knocksAccept(peerId));
}

export function knocksBlock(secretHex: string, peerId: string): Promise<void> {
  return call(secretHex, (c) => c.knocksBlock(peerId));
}

export function knocksUnblock(secretHex: string, peerId: string): Promise<void> {
  return call(secretHex, (c) => c.knocksUnblock(peerId));
}

export function knocksDismiss(secretHex: string, peerId: string): Promise<void> {
  return call(secretHex, (c) => c.knocksDismiss(peerId));
}

export function contactsList(secretHex: string): Promise<ContactItem[]> {
  return call(secretHex, (c) => c.contactsList());
}

export function contactsConfirm(secretHex: string, peerId: string): Promise<void> {
  return call(secretHex, (c) => c.contactsConfirm(peerId));
}

export function inboxModeGet(secretHex: string): Promise<string> {
  return call(secretHex, (c) => c.inboxModeGet());
}

export function inboxModeSet(secretHex: string, mode: string): Promise<void> {
  return call(secretHex, (c) => c.inboxModeSet(mode));
}

/** The reply to `fauna.inbox.send`: the created inbox row id on delivery, or
 *  `null` when the peer's `allow_knock` mode stored it as a pending knock. */
export interface InboxSendReply {
  inbox_id: number | null;
}

/** `fauna.inbox.send` — hand the home nest a canonical signed payload for
 *  `peerId` (hex). `recipientNestUrl` is `null` for a same-nest recipient (the
 *  nest local-delivers) and a peer URL for a cross-nest send. */
export function inboxSend(
  secretHex: string,
  peerId: string,
  recipientNestUrl: string | null,
  payload: Uint8Array,
): Promise<InboxSendReply> {
  return call(secretHex, (c) => c.inboxSend(peerId, recipientNestUrl, payload));
}

/** Send a knock (contact request) to `peerId` (hex) — the WS-RPC successor of the
 *  deleted `POST /api/v1/contacts/{actor_id}/knock` twin. The signed
 *  `(ContactRequest, Post)` tuple is composed by shared Rust
 *  (`build_knock_payload`), then carried by `fauna.inbox.send`; the home nest
 *  local-delivers, or originates `fauna.federation.inbox.deliver` when the
 *  recipient lives on another nest. `recipientNestUrl` is `null` (this nest) for
 *  the contacts page, whose lookup is same-nest; the profile page passes the
 *  shared `knockRecipientNestUrl` answer over the profile its open already
 *  fetched (`profile.md` § Where logic lives → *Request contact routing*). See
 *  `docs/goal/architecture/api-layers.md` § Contacts & Knocks.
 *
 *  A supervised ward whose new contacts need guardian approval gets a rejection
 *  `isGuardianApprovalRequired` recognises (`$lib/guardian-refusal`). */
export function sendKnock(
  secretHex: string,
  peerId: string,
  recipientNestUrl: string | null = null,
): Promise<InboxSendReply> {
  const url = nodeUrl();
  return call(secretHex, (c) =>
    c.inboxSend(peerId, recipientNestUrl, buildKnockPayload(secretHex, peerId, url)),
  );
}

// ── fauna.notifications.* ───────────────────────────────────────────

export function notificationsList(
  secretHex: string,
  cursor?: number,
  limit?: number,
): Promise<NotifListReply> {
  return call(secretHex, (c) => c.notificationsList(cursor ?? null, limit ?? null));
}

export function notificationsMarkRead(
  secretHex: string,
  upTo?: number,
): Promise<{ marked_read: number }> {
  return call(secretHex, (c) => c.notificationsMarkRead(upTo ?? null));
}

// ── fauna.account.* / fauna.quota.get / fauna.profile.handle.change ─
//
// The connection actor is the calling actor — no bearer/path rides (it replaced
// the deleted HTTP twins' bearer). `fauna.account.{get,upgrade}` have no web
// consumer, so no wrappers (see the rpc.rs section comment).

export function quotaGet(secretHex: string): Promise<QuotaInfo> {
  return call(secretHex, (c) => c.quotaGet());
}

// ── fauna.features.* ────────────────────────────────────────────────────
//
// The gated-feature plane's transparency read (`feature-limits-section`,
// `dynamic-features.md` § Transparency & auditability). `featuresRows` is the
// one call: it joins `fauna.features.status` with `fauna.nest.info`'s
// capability set and folds both into ready-to-render rows, same as tui/linux.

/** Mirrors shared Rust `fauna_client_features::row::CellMagnitudes`. */
export interface CellMagnitudes {
  remaining: LocalizedText;
  limit: LocalizedText;
}

/** Mirrors shared Rust `fauna_client_features::row::RowCell` — one bound in
 *  force, per (dimension, window), with the tier that set it. */
export interface RowCell {
  dimension: string;
  window: string;
  limit: number;
  observed: number;
  remaining: number;
  tier: string;
  tier_label: LocalizedText;
  exhausted: boolean;
  label: LocalizedText;
  value: LocalizedText;
  magnitudes: CellMagnitudes | null;
}

/** Mirrors shared Rust `fauna_client_features::row::FeatureRow` — one
 *  registry member's row on the feature-limits surface. */
export interface FeatureRow {
  feature: string;
  name: LocalizedText;
  availability: string;
  denied_by: string | null;
  cells: RowCell[];
  per_operation_max: number | null;
  per_operation_max_tier: string | null;
  unit: string;
  affordance: string;
  restriction: LocalizedText | null;
  status: LocalizedText;
}

export function featuresRows(secretHex: string): Promise<FeatureRow[]> {
  return call(secretHex, (c) => c.featuresRows());
}

export function accountAmIAdmin(secretHex: string): Promise<boolean> {
  return call(secretHex, (c) => c.accountAmIAdmin());
}

/** `fauna.setup.status` → the deployment setup snapshot. (The storage-mode
 *  axis is retired — docs/goal/architecture/nest/storage-modes.md; the
 *  constant `mode` it once projected left the wire 2026-09-24.) */
export interface SetupStatus {
  domain: string;
  dns_configured: boolean;
  tls_active: boolean;
  email_enabled: boolean;
  // Deployment policy: auto-provision a new user's mailbox at first authenticated
  // setup (default-on; unset ⇒ true). The non-admin first-setup glue gates its
  // auto-mint on this together with `email_enabled` — mail-policy-config.md
  // § Tier-2 *Auto-enable mail for new users*.
  auto_enable_mail_for_new_users: boolean;
  admin_exists: boolean;
  claimed: boolean;
  version: string;
  // Deployment policy: the nest's registration posture + the orthogonal free-tier
  // ceiling (`null` = no cap). The admin-users page seeds its registration section
  // from these and writes both back through `adminSetRegistrationMode` —
  // public-mode.md § Registration Modes.
  //
  // `registration_mode` is the raw wire string on purpose. `null` (an absent field)
  // and an unrecognized value (a *newer* nest's posture)
  // are different states, and neither may be coerced to a known mode: saving
  // that guess would overwrite the nest's real posture. Narrow with
  // `asRegistrationMode`, and render read-only when it returns null.
  registration_mode: string | null;
  max_free_users: number | null;
  // The Registration section's age require-knob ("accept only signups carrying
  // app age verification"; default off, serde-default `false`) — seeds
  // `admin-users-registration-age-verification-toggle`, written back through
  // `adminSetAgeVerificationRequired` (family-safety.md § The account age band).
  age_verification_required: boolean;
  // Deployment client-facing API serving port (the admin-set `serving_port`
  // singleton, read straight from the DB; default 443 when never set). The
  // admin-nest page seeds `admin-nest-serving-port-input` from this and writes
  // via `adminSetServingPort` — nest/common.md § Serving ports.
  serving_port: number;
  // True when this nest sits behind the cloud :443 SNI router (a Docker/cloud
  // deployment), where the chosen `serving_port` is inert and a write is rejected.
  // The admin-nest page renders the port field read-only when true — additive
  // (`#[serde(default)]` = false), so an absent key reads false. nest/common.md
  // § Serving ports.
  fronted_by_router: boolean;
  // Host-OS maintenance (installers/vps.md § Host OS Maintenance § 4): pending
  // security updates / a pending reboot on the host Ubuntu box, read from the
  // `/data/maintenance` channel. The admin-nest page renders the
  // `nest-os-maintenance-status` line + `nest-os-updates-count` badge from these.
  // All additive (`#[serde(default)]`); a nest with no host channel (dev/desktop)
  // reads 0/false/null → "OS up to date", no false alarm.
  os_security_updates_pending: number;
  os_reboot_pending: boolean;
  os_reboot_deferred_since: number | null;
  os_last_patched_at: number | null;
}

export function setupStatus(secretHex: string): Promise<SetupStatus> {
  return call(secretHex, (c) => c.setupStatus() as Promise<SetupStatus>);
}

export function profileHandleChange(
  secretHex: string,
  handle: string,
): Promise<HandleChangeReply> {
  return call(secretHex, (c) => c.profileHandleChange(handle));
}

// ── fauna.profile.* (profile detail read + own-write — the edit form) ──
//
// The text-only edit form's read-modify-write + sign + decode lives in shared
// Rust (`fauna-client-profile`, surfaced via wasm `buildEditedProfile` /
// `decodeProfileDisplay` in $lib/wasm); these two only carry bytes over the
// wire. profile.md § Where logic lives → Profile publish/edit.

/** `fauna.profile.get` → the raw stored signed profile bytes for `actorIdHex`
 *  (decode client-side with `decodeProfileDisplay`). Rejects with
 *  `fauna.profile.not_found` when the actor hasn't published a profile yet. */
export function profileGet(secretHex: string, actorIdHex: string): Promise<Uint8Array> {
  return call(secretHex, (c) => c.profileGet(actorIdHex) as Promise<Uint8Array>);
}

/** The edit form's base load: the caller's OWN stored profile bytes, read
 *  through the shared read-prove-record, so a succession link the base needs
 *  is recorded in this browser's registry before the form can save
 *  (profile.md § After an identity succession → the linkless bullet). `null`
 *  is a never-published profile (first publish); rejects only when the read
 *  itself fails. */
export function loadProfileEditBase(secretHex: string): Promise<Uint8Array | null> {
  return call(secretHex, (c) => c.loadProfileEditBase(secretHex) as Promise<Uint8Array | null>);
}

/** `fauna.profile.set` — publish/replace the caller's own profile. `body` is
 *  the signed `EmbedAsBytes` wire from `buildEditedProfile`. */
export function profileSet(secretHex: string, body: Uint8Array): Promise<void> {
  return call(secretHex, (c) => c.profileSet(body) as Promise<void>);
}

export function accountDelete(secretHex: string): Promise<AccountDeleteReply> {
  return call(secretHex, (c) => c.accountDelete());
}

// ── fauna.pending_actions.* (`settings.md` § Pending actions) — the
// cancellation window the three delayed verbs above open. tui/linux twins:
// `apps/fauna-tui/src/settings/account.rs`,
// `apps/fauna-linux/src/{client,settings/pending_actions}.rs`.

/** One scheduled action (`PendingActionSummary` on the wire). */
export interface PendingActionSummary {
  id: number;
  action_type: string;
  target: string | null;
  status: string;
  created_at: number;
  execute_after: number;
  requires_quorum: number;
  approvals: string[];
}

interface PendingActionsListReply {
  actions: PendingActionSummary[];
}

/** `fauna.pending_actions.list`, filtered to still-`pending` rows (an
 *  executed / cancelled / expired action has no cancel window left, so the
 *  standing section has nothing to offer on it) — the read half of the
 *  cancellation window. Mirrors tui's / linux's own `list_pending_actions`
 *  helper (`apps/fauna-tui/src/settings/mod.rs`,
 *  `apps/fauna-linux/src/client.rs`): both the initial fetch and a landed
 *  cancel must end on the SAME projection. */
export function pendingActionsList(secretHex: string): Promise<PendingActionSummary[]> {
  return call(secretHex, async (c) => {
    const reply = (await c.pendingActionsList()) as PendingActionsListReply;
    return reply.actions.filter((a) => a.status === 'pending');
  });
}

/** Cancel a scheduled action before it executes (one click, no confirm). */
export function pendingActionCancel(secretHex: string, id: number): Promise<void> {
  return call(secretHex, (c) => c.pendingActionCancel(id) as Promise<void>);
}

// ── fauna.recovery.* (`settings.md` § Recovery kit) — the RecoveryKey's
// Settings home. Calls `fauna_client_recovery` IN-PROCESS over wasm on the
// authenticated `WsRpcClient` (no separate FFI hop); the tui/linux twins are
// `apps/fauna-tui/src/settings/mod.rs` and
// `apps/fauna-linux/src/{client,settings/recovery_kit}.rs`. ──

/** `recovery-kit-status`'s data — the status text (resolve via
 *  `resolveLocalized`) plus the three gesture-enablement flags the section's
 *  buttons read. Never re-derived client-side: this mirrors the shared
 *  projection's own `allows_*`, so the SPA cannot drift from tui/linux. */
export interface RecoveryStatus {
  status_text: LocalizedText;
  allows_create: boolean;
  allows_replace: boolean;
  allows_lost: boolean;
  /** `identity-stolen-button`'s enablement — true in EVERY state, per
   *  settings.md § Recovery kit's "stolen (any)". Read rather than assumed
   *  constant here so the enablement matrix stays the shared crate's to decide,
   *  and so the kit-in-hand phrase field gates on the same predicate tui's
   *  does — the succession is one of the ceremonies that reads it. */
  allows_stolen: boolean;
  /** `recovery-kit-escrow-reseal-button`'s render gate — the no-escrow state only. */
  allows_escrow_reseal: boolean;
  /** `recovery-pending-veto-button`'s render gate: a seed-alone replacement
   *  window is open. */
  replacement_pending: boolean;
}

/** A create/replace/lost ceremony's result — the minted secret, its
 *  `fauna://recovery` display URI, and a no-second-hop status re-read. */
export interface RecoveryMinted {
  secret_hex: string;
  uri: string;
  status: RecoveryStatus;
}

/** A fresh read off the registration chain — never a local flag, so a kit
 *  created on another device is reflected here. */
export function recoveryKitStatus(secretHex: string): Promise<RecoveryStatus> {
  return call(secretHex, (c) => c.recoveryKitStatus(secretHex) as Promise<RecoveryStatus>);
}

/** Mint the first RecoveryKey registration (`recovery-kit-create-button`,
 *  live only in `NeverCreated`). `handle` is the already-qualified
 *  `handle@domain`, or `null` pre-registration. */
export function recoveryCreateKit(
  secretHex: string,
  handle: string | null,
): Promise<RecoveryMinted> {
  return call(
    secretHex,
    (c) => c.recoveryCreateKit(secretHex, handle ?? undefined) as Promise<RecoveryMinted>,
  );
}

/** Replace the registered kit using the one the user holds
 *  (`recovery-kit-replace-button`) — `phrase` is the
 *  `recovery-entry-phrase-field` contents. */
export function recoveryReplaceKit(
  secretHex: string,
  handle: string | null,
  phrase: string,
): Promise<RecoveryMinted> {
  return call(
    secretHex,
    (c) => c.recoveryReplaceKit(secretHex, handle ?? undefined, phrase) as Promise<RecoveryMinted>,
  );
}

/** Register the kit the onboarding `recovery_kit` screen minted and the user
 *  confirmed, at the wizard's signed-in handoff (`kitHex` is the wizard's
 *  `takePendingRecoverySecret()`). Never rejects on a ceremony failure — the
 *  Settings status line tells the truth instead. */
export function recoveryRegisterDeferredKit(secretHex: string, kitHex: string): Promise<void> {
  return call(secretHex, (c) => c.recoveryRegisterDeferredKit(secretHex, kitHex) as Promise<void>);
}

/** Contest the pending seed-alone replacement with the kit the user holds
 *  (`recovery-pending-veto-button`) → the fresh status, which is the gesture's
 *  whole receipt. */
export function recoveryVeto(secretHex: string, phrase: string): Promise<RecoveryStatus> {
  return call(secretHex, (c) => c.recoveryVeto(secretHex, phrase) as Promise<RecoveryStatus>);
}

/** The no-escrow repair (`recovery-kit-escrow-reseal-button`): re-put the sealed
 *  seed under the kit already in hand, without retiring it → the fresh status. */
export function recoveryResealEscrow(secretHex: string, phrase: string): Promise<RecoveryStatus> {
  return call(
    secretHex,
    (c) => c.recoveryResealEscrow(secretHex, phrase) as Promise<RecoveryStatus>,
  );
}

/** Open a seed-alone replacement window (`recovery-kit-lost-button`). */
export function recoveryLostKit(
  secretHex: string,
  handle: string | null,
): Promise<RecoveryMinted> {
  return call(
    secretHex,
    (c) => c.recoveryLostKit(secretHex, handle ?? undefined) as Promise<RecoveryMinted>,
  );
}

/** What the aftermath resolves to, for the console ring: `'not-a-successor'`
 *  when no pass ran (the identity never succeeded), `'ran'` when it did — each
 *  leg's own result reaches the user through `onProgress`, not this value. */
export type AftermathOutcome = 'not-a-successor' | 'ran';

/** Run the whole post-succession aftermath — the ordered pass (legs 2, 4, 7, 6)
 *  that repairs what a succession moves ownership of but not the seal on: the
 *  `NestBackupKey` grant the nest's backup sweep enumerates owners by, the
 *  capability-grant ledger, the `__drafts` rails, and the mail plane the
 *  retired seed's holder otherwise keeps reading (`succession-aftermath.md`
 *  § Re-key scope → the `BackupKey` corpus row).
 *
 *  ⚠ **One pass, not four calls to sequence here.** Which leg waits on which
 *  is the safety property and it lives in `fauna_client_recovery::aftermath`,
 *  shared with tui. `onProgress` fires twice per leg (start, settle) with the
 *  store field the line belongs to; `null` means that leg finished owing the
 *  user nothing to read.
 *
 *  Safe and free on every actor settle: an identity that never succeeded has no
 *  predecessors and returns before any round trip, so the caller does not try to
 *  detect a succession first. Idempotent — a completed pass writes nothing. */
export function runSuccessionAftermath(
  secretHex: string,
  onProgress: (leg: string, line: LocalizedText | null) => void,
): Promise<AftermathOutcome> {
  return call(
    secretHex,
    (c) =>
      c.runSuccessionAftermath(secretHex, nodeUrl(), onProgress) as Promise<AftermathOutcome>,
  );
}

/** Whether the ephemeral kit-side member-review pass should render this
 *  session (`succession-aftermath.md` § Propagation, item (ii)) —
 *  `runSuccessionAftermath` already witnessed the sweep the instant it ran,
 *  so this is a synchronous local read with no round trip; it skips `call`'s
 *  connection wait rather than block Settings render on a socket this read
 *  never touches. `false` on every ordinary sign-in and on a successor's
 *  *second* one. The roster itself is the caller's own `$lib/member-reviews`
 *  read — this answers only "was a sweep the reason it's non-empty now". */
export async function ephemeralReviewPassActive(secretHex: string): Promise<boolean> {
  const c = await getClient(secretHex);
  return c.ephemeralReviewPassActive(secretHex) as boolean;
}

/** *Review The Rest Later* — hides the pass and decides nothing: the open
 *  items stay exactly where they are on the succession ledger, inherited by the
 *  permanent review page. Clears the witness above; never touches the
 *  roster. */
export async function deferEphemeralReviewPass(secretHex: string): Promise<void> {
  const c = await getClient(secretHex);
  c.deferEphemeralReviewPass(secretHex);
}

/** The post-succession sweep's own lines, as the shared projection selected
 *  them (`settings.md` § Recovery kit → *The sweep's own lines*). Each is
 *  rendered through `resolveLocalized`, never composed here; `null` is a line
 *  the page must not paint (a sweep over an account with no groups says
 *  nothing — the projection's silence, not this page's). */
export interface SweepCopy {
  /** What the sweep did — one of its four outcome arms. */
  outcome: LocalizedText | null;
  /** The roster it cannot vouch for, as its OWN line — two facts, never one
   *  "you are safe" verdict. */
  unattested: LocalizedText | null;
  /** Whether a retry could still finish the sweep — what
   *  `recovery-kit-sweep-retry-button` renders on. */
  owesWork: boolean;
}

/** The sweep's lines for this identity, or `null` when no succession ran in
 *  this tab. Same synchronous local read as `ephemeralReviewPassActive`, and
 *  for the same reason: the ceremony parked its view across the document
 *  swap that ends it. */
export async function successionSweepCopy(secretHex: string): Promise<SweepCopy | null> {
  const c = await getClient(secretHex);
  return c.successionSweepCopy(secretHex) as SweepCopy | null;
}

/** `recovery-kit-sweep-retry-button`'s press — the sentence to paint, never
 *  null: the button must answer in words on every press, because its render
 *  gate is unfinished work rather than "this device can retry".
 *
 *  On web the answer is always the member-side remedy (the retired identity's
 *  MLS state rests in the nest replica behind revoked bearers, so no browser
 *  ever holds it), and it is the SHARED sentence — a native device without that
 *  history says the same words. */
export async function successionSweepRetry(secretHex: string): Promise<LocalizedText> {
  const c = await getClient(secretHex);
  return c.successionSweepRetry() as LocalizedText;
}

/** What the succession ceremony landed (`identity-stolen-button`). */
export interface LandedSuccession {
  /** The successor identity's 64-hex secret — the account, from here on. At the
   *  instant this arrives it exists nowhere else in the world. */
  secretHex: string;
  /** The actor id the account now belongs to, lowercase hex. */
  newActorId: string;
  /** Whether the seed was verified present in this browser's account store by a
   *  read-back. `false` ⇒ the secret MUST stay on screen: it is the only way
   *  back into the account. */
  persisted: boolean;
  /** The propagation half's outcome, never the ceremony's. */
  sweep: 'no-engine' | 'failed' | 'ran';
  /** On `failed`, why; on `ran`, how many groups still owe work. */
  sweepDetail?: string;
  /** Unix seconds the nest applied it — absent on the reconcile arm. */
  succeededAt?: number;
}

/** How `identity-stolen-button`'s ceremony ended — the shared Rust
 *  `StolenOutcome`, typed end to end (`identity-succession.md`
 *  § Implementation status today, the *typed outcome* ruling). Only
 *  `not-landed` is a failure. Paint `message` verbatim on `error-message` and
 *  wrap nothing: each sentence carries its own headline. */
export interface StolenOutcome {
  kind: 'landed' | 'not-landed' | 'landed-for-another' | 'undecided';
  /** The sentence to resolve and paint — absent on `landed` alone. */
  message?: LocalizedText | null;
  /** On `landed`, the succession — absent on every other arm. */
  landed?: LandedSuccession | null;
  /** The undecided arm whose save was not verified: `message` carries the only
   *  copy of the successor seed, so it is PARKED exactly as
   *  `stolen_persist_failed` is, and the session is not torn down. */
  carriesTheOnlySeed: boolean;
}

/** Take the account back from a stolen secret using the kit in hand
 *  (`identity-stolen-button`) — the irreversible ceremony.
 *
 *  `conversations` is the LIVE manager when the tab has one, so the
 *  post-succession group sweep can run from the retired identity's own engine —
 *  the only moment in the page's life where it and the successor's exist at
 *  once. It is a parameter rather than a `conversationsManagerIfReady()` call
 *  here because `$lib/conversations` imports this module, and the caller (the
 *  settings page) already has both. ⚠ Pass only an ALREADY-BUILT manager: a
 *  freshly built one has no restored state, and sweeping it would report a sweep
 *  that moved nothing. Omitting it is the honest `no-engine` outcome — the
 *  shared contract's own arm for "conversations were not up".
 *
 *  The two nest URLs are resolved here, not passed in: the successor's account
 *  row is bound to `storedNestUrl()` (the truth) while the ceremony's sockets go
 *  to `nodeUrl()` (the dial resolution). They are the same string in production
 *  and differ under e2e automation — binding the row to a dial override would
 *  leave the successor pointing at a URL that stops existing when the test ends.
 *
 *  ⚠ A `landed` outcome ENDS the session it is called from: the nest revokes
 *  the old identity's bearers inside the succession transaction. The caller's
 *  next move is to sign in as `landed.newActorId`, never another call on this
 *  client. Rejects only when the ceremony could not start at all. */
export function succeedIdentityWithHeldKit(
  secretHex: string,
  phrase: string,
  conversations?: WasmConversationsManager | null,
): Promise<StolenOutcome> {
  return call(
    secretHex,
    (c) =>
      c.succeedIdentityWithHeldKit(
        secretHex,
        phrase,
        storedNestUrl(),
        nodeUrl(),
        conversations ?? undefined,
      ) as Promise<StolenOutcome>,
  );
}

/** Reply from `fauna.admin.factory_reset`. The `claim_code` is the freshly
 * minted code for the wiped nest — the human never sees it, so the client
 * re-seeds onboarding at claim-code with it pre-filled. */
export interface FactoryResetReply {
  claim_code: string;
}

/** `fauna.admin.factory_reset` — return the nest to fresh/unclaimed (restart
 * wipe). The handler replies with the new claim code then exits + restarts;
 * the client tolerates the ~1-2s WS drop and re-seeds onboarding at claim-code
 * with `reply.claim_code` pre-filled (mail-bridge-lifecycle.md § Factory reset).
 * `newClaimCode` omitted ⇒ the nest regenerates a random code. */
export function factoryReset(
  secretHex: string,
  newClaimCode?: string,
): Promise<FactoryResetReply> {
  return call(
    secretHex,
    (c) => c.adminFactoryReset(newClaimCode) as Promise<FactoryResetReply>,
  );
}

// ── fauna.admin.* — the consolidated admin-users hub ────────────────
//
// The Pending requests / Invite / Users sections of the `admin-users` page
// (admin.md § Users) all drive the `fauna.admin.*` WS-RPC kinds through the
// shared Rust `AdminClient` (libs/fauna-wasm/src/rpc.rs over
// libs/fauna-client-admin) — no `/admin/api/*` HTTP twins. These mirror the
// linux hub's per-kind calls. `actor_id` rides as raw 32 bytes (Uint8Array);
// list calls return the inner arrays; `adminInviteCodesCreate("")` mints
// (empty `code` ⇒ the nest generates + returns the token, admin.md § 3).

/** A user row (`fauna.admin.users.list`). `actor_id` is the raw 32-byte id. */
export interface AdminUser {
  actor_id: Uint8Array | number[];
  tier: string;
  label: string;
  // The account's handle — unique on the nest, and what identifies a user in an
  // admin picker (admin.md § 2; the editable, non-unique label never is). Absent
  // for a handle-less admission (the admit form's blank handle)
  // — pickers fall back to the full actor hex.
  handle?: string;
  suspended: boolean;
  created_at: number;
  inbox_bytes_used: number;
  storage_bytes_used: number;
  eviction?: { status: string } | null;
  // Read-only IMAP/CalDAV-serving audit flag (default on; absent ⇒ serving).
  // The user alone controls it from their own mail-settings serve-here toggle —
  // the admin only sees it (mail-settings.md § Local IMAP/CalDAV-serving toggle;
  // admin.md § Users). Rides the shared users-list projection, no admin write.
  mail_serving_enabled: boolean;
  // Read-only: does this actor hold the admin role (`admin_actor_ids`)? Resolved
  // nest-side; granted/revoked via `fauna.admin.admins.{add,remove}`, never from a
  // Users row. An admin can be neither suspended nor evicted (the nest answers
  // `fauna.admin.conflict`), so the row withholds both entry controls — see
  // `adminUserRowControls`. An omitted key ⇒ decodes `false` ⇒ the
  // controls render and the refusal surfaces in `admin-users-action-error`.
  is_admin?: boolean;
}

/** A storage/feature tier definition (`fauna.admin.tiers.list`). */
export interface AdminTier {
  name: string;
  max_inbox_bytes: number;
  max_storage_bytes: number;
  max_devices: number;
  max_blob_size: number;
  max_feeds: number;
}

/** One closed-registration invite code (`fauna.admin.invite_codes.list`). */
export interface AdminInviteCode {
  code: string;
  tier: string;
  uses_left: number;
  created_at: number;
  /** The band the redeeming account is admitted under (wire token), echoed on
   *  the `invite-code-item` row; absent/null for an ordinary code. Additive. */
  age_band?: string | null;
}

/** One in-band invite request (`fauna.admin.invite_requests.list`). */
export interface AdminInviteRequest {
  id: number;
  actor_id: Uint8Array | number[];
  handle: string;
  message: string;
  status: string;
  created_at: number;
  // Shared `fauna_protocol::admin::AdminInviteRequest::is_pending` predicate,
  // projected onto the reply by the wasm seam. Read this instead of re-coding
  // `status === 'pending'` (admin.md — adopting clients call `is_pending`).
  is_pending: boolean;
  /** The applicant's age-claim band + how it was established, when the submit
   *  carried one — `invite-request-row-age-claim` renders it (absence is the
   *  signal), and it seeds the row's band select. Additive. */
  age_band?: string | null;
  age_band_provenance?: string | null;
}

// Users (Section 3 — the user list + per-row change-tier).

export function adminUsersList(
  secretHex: string,
  limit = 50,
  offset = 0,
): Promise<{ users: AdminUser[]; total: number }> {
  return call(
    secretHex,
    (c) => c.adminUsersList(limit, offset) as Promise<{ users: AdminUser[]; total: number }>,
  );
}

// Every account on the nest, newest first — the shared
// `fauna_client_admin::users_list_all` over the wasm face. This, never one
// `adminUsersList` page, is what an admin actor picker offers (admin.md § 2 →
// *Which accounts a picker offers*): a single page loses the oldest accounts,
// the box claimer first.
export function adminUsersListAll(secretHex: string): Promise<AdminUser[]> {
  return call(secretHex, (c) => c.adminUsersListAll() as Promise<AdminUser[]>);
}

export function adminUsersUpdate(
  secretHex: string,
  actorId: Uint8Array,
  tier: string,
  label: string,
): Promise<void> {
  return call(secretHex, (c) => c.adminUsersUpdate(actorId, tier, label) as Promise<void>);
}

// Admin Logs (observability.md § Surfaces) — the nest's in-memory `fauna-log`
// ring, admin-scoped. Same `{ timestamp_ms, level, target, message }` shape as
// the client ring (`logSnapshot`), so the admin Logs page reuses `LogsView`.
// No clear: there is no admin RPC to wipe the nest ring.

export function adminLogs(secretHex: string): Promise<LogEntry[]> {
  return call(secretHex, (c) => c.adminLogs() as Promise<LogEntry[]>);
}

export function adminUsersEvict(
  secretHex: string,
  actorId: Uint8Array,
  reason: string,
  category: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.adminUsersEvict(actorId, reason, category) as Promise<void>,
  );
}

// Cut a user off now, no delete timeline (`admin-users-suspend-button`). Pass
// empty `reason`/`category` to take the nest's canonical defaults ("suspended by
// admin" / "other") — unlike evict, whose handler *requires* a reason.
export function adminUsersSuspend(
  secretHex: string,
  actorId: Uint8Array,
  reason: string,
  category: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.adminUsersSuspend(actorId, reason, category) as Promise<void>,
  );
}

export function adminUsersCancelEviction(
  secretHex: string,
  actorId: Uint8Array,
): Promise<void> {
  return call(secretHex, (c) => c.adminUsersCancelEviction(actorId) as Promise<void>);
}

// Direct admission (`admin-users-admit-*`, `public-mode.md` § Registration &
// Identity) — the third account-creation path: the admin admits a known
// actor id under a tier and (usually) a handle, one `fauna.admin.users.create`
// call. `handle` blank/`undefined` admits the deliberate handle-less state
// (§ A handle-less account) — there is no free-text label on this path.

export function adminUsersCreate(
  secretHex: string,
  actorId: Uint8Array,
  tier: string,
  handle?: string,
): Promise<void> {
  return call(secretHex, (c) => c.adminUsersCreate(actorId, tier, handle) as Promise<void>);
}

// Grant/revoke the admin role (`admin-users-make-admin-button` /
// `admin-users-remove-admin-button`, admin.md § Admin continuity and
// succession, instrument 1). Both schedule a 24h-delayed pending action — a
// scheduled reply (no error) is success; the row does not flip right away.

export function adminAdminsAdd(secretHex: string, actorId: Uint8Array): Promise<void> {
  return call(secretHex, (c) => c.adminAdminsAdd(actorId) as Promise<void>);
}

export function adminAdminsRemove(secretHex: string, actorId: Uint8Array): Promise<void> {
  return call(secretHex, (c) => c.adminAdminsRemove(actorId) as Promise<void>);
}

// Deployment-seed rotation (`admin-nest-seed-rotate-*`, box-recovery.md §
// Deployment-seed rotation) — give this nest a brand-new identity; every
// enrolled app re-trusts it automatically, and anyone still holding the old
// identity (a removed admin, a lost device) stops being able to use it.

/** One admin who inherits the rotation's successor seed — mirrors shared Rust
 *  `fauna_client_admin::SeedRotationInheritor`. */
export interface SeedRotationInheritor {
  actor_id: Uint8Array | number[];
  /** `AdminUser.label`, or the canonical short id when unresolved/blank —
   *  never dropped: an inheritor the fold cannot name still inherits. */
  label: string;
}

/** What the rotation confirm surface renders — mirrors shared Rust
 *  `fauna_client_admin::SeedRotationConfirmView`. `blocked_reason` is
 *  non-null exactly when `can_confirm` is false (an empty roster withholds
 *  the destructive confirm). */
export interface SeedRotationConfirmView {
  inheritors: SeedRotationInheritor[];
  can_confirm: boolean;
  blocked_reason: LocalizedText | null;
}

/** `fauna.admin.admins.list` + a per-admin label join, folded by the shared
 *  `seed_rotation_confirm_view` — read-only, arms nothing rotated. Fired on
 *  `admin-nest-seed-rotate-button`. */
export function adminSeedRotateRoster(secretHex: string): Promise<SeedRotationConfirmView> {
  return call(secretHex, (c) => c.adminSeedRotateRoster() as Promise<SeedRotationConfirmView>);
}

/** Drive the deployment-seed rotation ceremony (mint → custody → dispatch →
 *  mark, then the account-plane fan-out on success) and resolve the shared
 *  verdict sentence — render via `resolveLocalized`. Fired on
 *  `admin-nest-seed-rotate-confirm-button`, once the caller has disarmed the
 *  roster (disarm-before-dispatch: a double click must not chain a second
 *  rotation onto the first). */
export function rotateDeploymentSeed(secretHex: string): Promise<LocalizedText> {
  return call(secretHex, (c) => c.rotateDeploymentSeed(secretHex) as Promise<LocalizedText>);
}

// Outside-app sign-in keys (`admin-nest-oauth-*`, authorization-server.md §
// The issuer → Two rotation arms) — the nest-held OAuth issuer key set and its
// second signer, the refresh-token secret. The view types mirror shared Rust
// `fauna_client_admin` and live in `$lib/admin-oauth-keys` beside the
// section's pure guards; the pure word folds are `$lib/wasm`'s.

/** `fauna.oauth.issuer_key_status` folded through the shared `issuer_key_view`
 *  — the one read `admin-nest-oauth-section` paints its key rows from. REJECTS
 *  on failure: word it onto `admin-nest-oauth-key-reason` (never the page's
 *  error line — any read error, and the rest of the
 *  page must still paint). */
export function adminIssuerKeyStatus(secretHex: string): Promise<IssuerKeyView> {
  return call(secretHex, (c) => c.adminIssuerKeyStatus() as Promise<IssuerKeyView>);
}

/** `admin-nest-oauth-rotate-button` — the ordinary rotation
 *  (`fauna.oauth.rotate_issuer_key`), dispatched and worded: resolves the
 *  shared verdict that IS `admin-nest-oauth-status`, failure included (the face
 *  never rejects — a reply lost to a timeout can follow a committed rotation,
 *  and only the shared fold may say so). Re-read the key set after. */
export function adminRotateIssuerKey(secretHex: string): Promise<LocalizedText> {
  return call(secretHex, (c) => c.adminRotateIssuerKey() as Promise<LocalizedText>);
}

/** `admin-nest-oauth-confirm-button` — exactly the armed arm's kind
 *  (`fauna.oauth.force_rotate_issuer_key` / `…force_rotate_session_secret`),
 *  dispatched and worded like `adminRotateIssuerKey`. `formatInstant(secs)` is
 *  the SPA's clock face for the session-secret verdict's instant (wasm has no
 *  OS timezone database); the sentence around it stays the shared fold's.
 *  Disarm before calling. */
export function adminForceRotateIssuer(
  secretHex: string,
  arm: IssuerForcedArm,
  formatInstant: (secs: number) => string,
): Promise<LocalizedText> {
  return call(
    secretHex,
    (c) => c.adminForceRotateIssuer(arm, formatInstant) as Promise<LocalizedText>,
  );
}

export interface ModerationLegalTakedownReply {
  status: string;
  content_id: string;
}

/** `fauna.moderation.legal_takedown` — the Admin-only legal-compulsion
 *  takedown/overturn (`moderation.md` § Legal takedown → Invocation surface).
 *  `conversation` selects the MLS relay-withhold kind, else post; `restore`
 *  overturns (the reference becomes the optional note). Gate the dispatch
 *  through `takedownFormView` (`$lib/wasm`) so a citation-less takedown is
 *  refused before it bounces off the nest's `invalid_params`. Fired on
 *  `admin-nest-takedown-confirm-button`, once the caller has disarmed the
 *  form (disarm-before-dispatch: a double click must not dispatch a second
 *  compulsory act). */
export function moderationLegalTakedown(
  secretHex: string,
  contentId: string,
  conversation: boolean,
  legalReference: string,
  restore: boolean,
): Promise<ModerationLegalTakedownReply> {
  return call(
    secretHex,
    (c) =>
      c.moderationLegalTakedown(
        contentId,
        conversation,
        legalReference,
        restore,
      ) as Promise<ModerationLegalTakedownReply>,
  );
}

// ── User-initiated reporting (`moderation.md` § User-initiated reporting) —
// the five client calls and the reporter-side hide. The sheet's decisions are
// `$lib/wasm`'s `reportSheetView` & co.; these only dispatch.

/** `fauna.moderation.abuse_report.submit`, built from the sheet by the shared
 *  `report_request` (a sheet the view would not let send rejects before
 *  anything leaves). `block_author` is recorded only — the caller chains
 *  `knocksBlock` and `hideReported` itself. */
export function moderationAbuseReportSubmit(
  secretHex: string,
  target: ReportTarget,
  form: ReportForm,
): Promise<{
  report_id: string;
  routed_to: string[];
  acknowledgement: import('$lib/i18n/localized').LocalizedText;
}> {
  return call(secretHex, (c) => c.moderationAbuseReportSubmit(target, form) as Promise<never>);
}

/** `fauna.moderation.abuse_report.mine` — the ledger, newest first, worded. */
export function moderationAbuseReportMine(secretHex: string): Promise<ReportLedgerRow[]> {
  return call(secretHex, (c) => c.moderationAbuseReportMine() as Promise<ReportLedgerRow[]>);
}

/** `fauna.moderation.abuse_report.withdraw` — errors reject for
 *  `reportWithdrawVerdict` to word. */
export function moderationAbuseReportWithdraw(secretHex: string, reportId: string): Promise<void> {
  return call(secretHex, (c) => c.moderationAbuseReportWithdraw(reportId) as Promise<void>);
}

/** `fauna.moderation.abuse_report.queue` — the open reports, oldest first
 *  (Admin-class). */
export function adminAbuseReportQueue(secretHex: string): Promise<ReportQueueRow[]> {
  return call(secretHex, (c) => c.adminAbuseReportQueue() as Promise<ReportQueueRow[]>);
}

/** `fauna.moderation.abuse_report.resolve` — a record, not an action; `acted`
 *  false dismisses (Admin-class). */
export function adminAbuseReportResolve(
  secretHex: string,
  reportId: string,
  acted: boolean,
): Promise<void> {
  return call(secretHex, (c) => c.adminAbuseReportResolve(reportId, acted) as Promise<void>);
}

/** The ids the owner hid by reporting them (`moderation.md` § Corollary). */
export function loadHiddenContent(secretHex: string): Promise<string[]> {
  return call(secretHex, (c) => c.loadHiddenContent() as Promise<string[]>);
}

/** Hide a reported subject for the owner; resolves to the stored list. */
export function hideReported(secretHex: string, id: string): Promise<string[]> {
  return call(secretHex, (c) => c.hideReported(id) as Promise<string[]>);
}

// Tiers (definitions — shared with admin-settings; the tier pickers' options).

export function adminTiersList(secretHex: string): Promise<AdminTier[]> {
  return call(secretHex, (c) => c.adminTiersList() as Promise<AdminTier[]>);
}

/** `fauna.admin.tiers.update` — overwrite a tier's caps in place (the
 *  admin-settings per-row editable caps + save button; admin.md § 3). Raw-i64
 *  caps, mirroring linux's in-place tier-cap editing over the shared
 *  `AdminClient`. The caller refetches `adminTiersList` after to re-render from
 *  persisted state. */
export function adminTiersUpdate(
  secretHex: string,
  name: string,
  maxInboxBytes: number,
  maxStorageBytes: number,
  maxDevices: number,
  maxBlobSize: number,
  maxFeeds: number,
): Promise<void> {
  return call(
    secretHex,
    (c) =>
      c.adminTiersUpdate(
        name,
        maxInboxBytes,
        maxStorageBytes,
        maxDevices,
        maxBlobSize,
        maxFeeds,
      ) as Promise<void>,
  );
}

// Membership designations (monetization.md § Pillar 4) — a link editor over the
// admin's own subscription tiers, never a third tier list. `tier_name` names a
// row in `subscriptionsTiersList` (the admin's own tiers); `admin_tier` /
// `lapse_tier` name `adminTiersList` rows (quota tiers).

/** One membership designation (`fauna.admin.membership_tiers.list`). */
export interface AdminMembershipTier {
  tier_name: string;
  admin_tier: string;
  lapse_tier: string;
  created_at: number;
}

export function adminMembershipTiersList(secretHex: string): Promise<AdminMembershipTier[]> {
  return call(
    secretHex,
    (c) => c.adminMembershipTiersList() as Promise<AdminMembershipTier[]>,
  );
}

/** `fauna.admin.membership_tiers.set` — designate/re-point (an upsert). Pass an
 *  empty `lapseTier` for the documented default (`free`). The caller refetches
 *  `adminMembershipTiersList` after to re-render from persisted state (the
 *  `adminTiersUpdate` shape). */
export function adminMembershipTiersSet(
  secretHex: string,
  tierName: string,
  adminTier: string,
  lapseTier: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.adminMembershipTiersSet(tierName, adminTier, lapseTier) as Promise<void>,
  );
}

/** `fauna.admin.membership_tiers.clear` — drop a designation; the subscription
 *  tier itself is untouched, it just reverts to undesignated. */
export function adminMembershipTiersClear(secretHex: string, tierName: string): Promise<void> {
  return call(secretHex, (c) => c.adminMembershipTiersClear(tierName) as Promise<void>);
}

// Services (admin-nest — the admin `pairing` toggle, the one service flag
// the UI still exposes after the per-page-services redesign dropped the
// vestigial bridge toggle; admin.md § N Nest). Over the WS-RPC
// `fauna.admin.services.{list,update}` kinds — replaces the deprecated
// /admin/api/services HTTP twin (removed 2026-06-04). The last `/admin/api/*`
// web twin, /admin/api/stats, migrated here as `adminStats` 2026-06-06 and the
// `admin-api.ts` module was deleted (no HTTP `/admin/api/*` calls remain).

/** The nest service-enable flags (`fauna.admin.services.list` → AdminServiceFlags). */
export interface AdminServiceFlags {
  bridge: boolean;
  pairing: boolean;
}

export function adminServicesList(secretHex: string): Promise<AdminServiceFlags> {
  return call(secretHex, (c) => c.adminServicesList() as Promise<AdminServiceFlags>);
}

/** `fauna.admin.stats` → the deployment counters for the admin dashboard.
 *  Mirrors `fauna_protocol::admin::AdminStatsReply` field-by-field. Replaces the
 *  deleted HTTP `/admin/api/stats` twin; email/TLS status come from
 *  `setupStatus`, the running version from `fetchNestInfo`. */
export interface AdminStats {
  total_users: number;
  users_by_tier: [string, number][];
  suspended_users: number;
  total_inbox_bytes: number;
  total_storage_bytes: number;
  ws_connections: number;
}

export function adminStats(secretHex: string): Promise<AdminStats> {
  return call(secretHex, (c) => c.adminStats() as Promise<AdminStats>);
}

/** `fauna.admin.services.update` — flip one service flag (`pairing` is the only
 *  one the UI drives). The caller refetches `adminServicesList` to reflect the
 *  applied state. */
export function adminServicesUpdate(
  secretHex: string,
  name: string,
  enabled: boolean,
): Promise<void> {
  return call(secretHex, (c) => c.adminServicesUpdate(name, enabled) as Promise<void>);
}

/** `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
 *  serving port (the admin-nest serving-port field; nest/common.md § Serving
 *  ports). `port` is a u16 in [1, 65535] (the caller validates the range first).
 *  The caller re-reads `setupStatus().serving_port` to reflect the applied value;
 *  the new port binds on the next nest restart. Mirrors `adminServicesUpdate` —
 *  the raw kind over the wasm `NestClient`, no per-feature wrapper. */
export function adminSetServingPort(secretHex: string, port: number): Promise<void> {
  return call(secretHex, (c) => c.adminSetServingPort(port) as Promise<void>);
}

/** The three registration postures (public-mode.md § Registration Modes) — the
 *  values `admin-users-registration-mode-select` carries. */
export type RegistrationMode = 'open' | 'invite_required' | 'closed';

const REGISTRATION_MODES: readonly string[] = ['open', 'invite_required', 'closed'];

/** Narrow the posture `setupStatus()` reports to one this client can render.
 *
 *  Returns `null` when the nest reports no posture (a non-conforming reply)
 *  **or** one this client predates (a newer nest's mode) — two states that must
 *  not be coerced to a known mode, because saving that guess would overwrite the
 *  nest's real posture. A `null` here means "render read-only; do not offer a
 *  save", never "closed". The nest always reports a concrete mode, so `null` is
 *  never "the admin has not chosen yet". */
export function asRegistrationMode(mode: string | null): RegistrationMode | null {
  return mode !== null && REGISTRATION_MODES.includes(mode) ? (mode as RegistrationMode) : null;
}

/** `fauna.admin.set_registration_mode` — set the deployment's registration posture
 *  and, orthogonally, the free-tier ceiling: the single Save on the admin-users
 *  registration section (admin.md § 2 Users → Section 2 — Registration). One call
 *  carries both; the nest swaps the live posture with no restart. The caller
 *  re-reads `setupStatus().registration_mode` / `.max_free_users` to reflect the
 *  applied values.
 *
 *  `maxFreeUsers` `undefined` **clears** the cap (the blank input = no cap) rather
 *  than leaving it unchanged — mode + ceiling are one decision, saved together.
 *  The cap counts every free-tier account *including the admin's own*, so "room
 *  for one more" is a cap of 2, not 1.
 *
 *  Takes a plain `number` and widens to `BigInt` here: the wasm-bindgen ABI types
 *  the `u64` argument as `bigint`, while `setupStatus()` reads the same value back
 *  as a `number` (the reply rides the `json_compatible` serializer). This wrapper
 *  is where that asymmetry is absorbed — callers stay in `number`. */
export function adminSetRegistrationMode(
  secretHex: string,
  mode: RegistrationMode,
  maxFreeUsers: number | undefined,
): Promise<void> {
  const cap = maxFreeUsers === undefined ? undefined : BigInt(maxFreeUsers);
  return call(secretHex, (c) => c.adminSetRegistrationMode(mode, cap) as Promise<void>);
}

/** `fauna.admin.set_age_verification_required` — the Registration section's age
 *  require-knob. The section's one save dispatches it beside
 *  `adminSetRegistrationMode`, only when the toggle's value changed. */
export function adminSetAgeVerificationRequired(secretHex: string, required: boolean): Promise<void> {
  return call(secretHex, (c) => c.adminSetAgeVerificationRequired(required) as Promise<void>);
}

/** `fauna.admin.request_host_restart` — the admin "restart now" affordance for an
 *  onboarded VPS host (installers/vps.md § Host OS Maintenance § 4). Writes a flag
 *  the host reboot-coordinator picks up and reboots gracefully. Rejected as
 *  `fauna.host_maintenance.no_host` on a nest with no maintenance mount. */
export function adminRequestHostRestart(secretHex: string): Promise<void> {
  return call(secretHex, (c) => c.adminRequestHostRestart() as Promise<void>);
}

/** Every rendering decision `admin-nest-region-*` needs, already made by the
 *  shared `fauna_client_admin::admin_region_view` fold (region-blocking.md §
 *  Region determination) — this app decides nothing about the plane. Mirrors
 *  `fauna_client_admin::AdminRegionView` field-by-field. */
export interface AdminRegionView {
  declared: string | null;
  status: LocalizedText;
  authority: LocalizedText | null;
  staleness: LocalizedText | null;
  can_withdraw: boolean;
}

/** `fauna.admin.region.get` folded through the shared `admin_region_view` —
 *  the one call `admin-nest-region-section` needs to paint all five fields. */
export function adminRegionStatus(secretHex: string): Promise<AdminRegionView> {
  return call(secretHex, (c) => c.adminRegionStatus() as Promise<AdminRegionView>);
}

/** `fauna.admin.region.set` — `region` is `undefined` for withdraw. Declare
 *  (`admin-nest-region-save-button`) sends the code already validated by
 *  `adminParseRegionCode`; withdraw (`admin-nest-region-withdraw-button`)
 *  sends the region absent, which also retires the previous region's
 *  feature-policy document nest-side. */
export function adminSetRegion(secretHex: string, region: string | undefined): Promise<void> {
  return call(secretHex, (c) => c.adminSetRegion(region) as Promise<void>);
}

// Invite codes (Section 2 — mint/list/delete).

export function adminInviteCodesList(secretHex: string): Promise<AdminInviteCode[]> {
  return call(secretHex, (c) => c.adminInviteCodesList() as Promise<AdminInviteCode[]>);
}

/** Mint an invite code. Empty `code` ⇒ the nest mints + returns one.
 * `guardianActor` links the redeemed account to a guardian for supervised
 * admission (family-safety.md § Wire & data shape); omit for an ordinary code. */
export function adminInviteCodesCreate(
  secretHex: string,
  tier: string,
  uses: number,
  guardianActor?: Uint8Array,
  ageBand?: string,
): Promise<string> {
  return call(
    secretHex,
    (c) => c.adminInviteCodesCreate('', tier, uses, guardianActor, ageBand) as Promise<string>,
  );
}

export function adminInviteCodesDelete(secretHex: string, code: string): Promise<void> {
  return call(secretHex, (c) => c.adminInviteCodesDelete(code) as Promise<void>);
}

// Pending requests (Section 1 — list/approve-at-tier/deny).

export function adminInviteRequestsList(secretHex: string): Promise<AdminInviteRequest[]> {
  return call(
    secretHex,
    (c) => c.adminInviteRequestsList() as Promise<AdminInviteRequest[]>,
  );
}

/** Approve a pending request, admitting the requester at `tier`.
 * `guardianActor` links the admitted account to a guardian for supervised
 * admission (family-safety.md § Wire & data shape); omit for an ordinary
 * account. */
export function adminInviteRequestsApprove(
  secretHex: string,
  id: number,
  tier: string,
  guardianActor?: Uint8Array,
  ageBand?: string,
): Promise<unknown> {
  return call(
    secretHex,
    (c) => c.adminInviteRequestsApprove(id, tier, null, guardianActor, ageBand) as Promise<unknown>,
  );
}

export function adminInviteRequestsDeny(
  secretHex: string,
  id: number,
  reason: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.adminInviteRequestsDeny(id, reason || null) as Promise<void>,
  );
}

// ── fauna.admin.custody_hosting.* — the `admin-custody-hosting` registry ──
//
// The nest-wide custody-hosting registry (account-data-plane.md § Two-sided
// bounds): an admin can see every
// host's hosting rows and remove one, over the shared `AdminHostingClient` +
// `admin_hosting_rows` fold (libs/fauna-wasm/src/rpc.rs), the wasm twin of
// `libs/fauna-ffi/src/admin.rs`'s `FfiAdminClient::custody_hosting_{list,remove}`.
// Reference: `apps/fauna-tui/src/admin/custody_hosting.rs` +
// `apps/fauna-linux/src/client.rs`'s `fetch_custody_hosting`/
// `remove_custody_hosting`, both calling the same shared client directly.

/** One row of the registry (`fauna.admin.custody_hosting.list`), already
 * folded (heaviest hold first, tie-broken on `(host, owner, grant)`) — do NOT
 * re-sort or re-derive `receipt_state` in TS. `grant_id` rides as raw bytes;
 * `(host_actor_id, grant_id)` is the remove door's key, carried here so a row
 * is removed from what the list rendered, never a painted index (a re-read
 * can reorder rows). `retained_bytes_cap: 0` means the row carries no cap —
 * the pump substitutes the hard-coded default, so the page must render
 * *Default*, never `0 B`. `receipt_state` is one of `"fresh"` / `"stale"` /
 * `"no_receipt_yet"` — the three-state honesty word every custody-facing
 * surface renders, never collapsed. */
export interface AdminHostingRow {
  host_actor_id: string;
  owner_actor_id: string;
  owner_nest_url: string;
  grant_id: Uint8Array | number[];
  retained_bytes_cap: number;
  held_bytes: number;
  stopped: boolean;
  receipt_state: 'fresh' | 'stale' | 'no_receipt_yet';
}

/** `fauna.admin.custody_hosting.remove` reply — `removed: false` is an
 * honest no-op (the row was already gone, e.g. a sibling admin or the host's
 * own reclaim), not a failure. `store_dropped` is true only when the removed
 * row was its `(host, owner)` pair's last — the custodied store falls with
 * it (no credit-back is owed anywhere: the held-byte figure is derived). */
export interface AdminHostingRemoveReply {
  removed: boolean;
  store_dropped: boolean;
}

/** `fauna.admin.custody_hosting.list` → every hosting row on this nest. */
export function adminHostingList(secretHex: string): Promise<AdminHostingRow[]> {
  return call(secretHex, (c) => c.adminHostingList() as Promise<AdminHostingRow[]>);
}

/** `fauna.admin.custody_hosting.remove` — drop one row, keyed by the
 * `(host, grant)` pair an `adminHostingList` row carries. */
export function adminHostingRemove(
  secretHex: string,
  hostActorId: string,
  grantId: Uint8Array,
): Promise<AdminHostingRemoveReply> {
  return call(
    secretHex,
    (c) => c.adminHostingRemove(hostActorId, grantId) as Promise<AdminHostingRemoveReply>,
  );
}

// ── admin mail / DNS machines ───────────────────────────────────────
//
// Stateful machine handles — the WASM twin of the UniFFI mail-admin machines
// (`libs/fauna-client-dns`, `libs/fauna-client-mail-settings`). Each factory
// returns a handle bound to the singleton client; the admin page holds it and
// drives `await m.hydrate()` (initial load) → `m.snapshot()` (render) →
// `await m.dispatch(action)` → `m.snapshot()`. Snapshot/action shapes are the
// shared machines' serde JSON (typed `any` from the generated bindings); the
// admin pages — `admin-dns` / `admin-bridges-pending` — layer typed interfaces
// over them (TIER-2 web work).

export function dnsManagementMachine(secretHex: string): Promise<WasmDnsManagementMachine> {
  return call(secretHex, (c) => Promise.resolve(c.dnsManagementMachine()));
}

// Managed-mode `admin-dns`: the read/verify surface plus the client-held
// DNS-provider credential store + publish/verify provider seam. Passes the
// actor's 32-byte secret (seals/loads `fauna.state.dns` under the derived
// BackupKey; the nest never sees the provider key — dns-management.md § Where the
// credential lives). Browser provider-API calls that can't be reached
// cross-origin are routed through `fauna_provisioning::proxy`'s CORS proxy, the
// same path the onboarding wizard uses.
export function dnsManagementMachineWithCredentials(
  secretHex: string,
): Promise<WasmDnsManagementMachine> {
  return call(secretHex, (c) =>
    Promise.resolve(c.dnsManagementMachineWithCredentials(hexToBytes(secretHex))),
  );
}

export function localDomainMachine(secretHex: string): Promise<WasmLocalDomainMachine> {
  return call(secretHex, (c) => Promise.resolve(c.localDomainMachine()));
}

// User-facing `mail-settings` page — the mail-credential lifecycle (enable mail,
// add/revoke credentials, rotate keys), WASM twin of the UniFFI
// `MailSettingsMachine` (`libs/fauna-client-mail-settings`). Passes the actor's
// 32-byte secret (signs submission tokens + keys the account plane) and the
// deployment node URL (MUA connection details). No mail logic in the SPA (#2).
export function mailSettingsMachine(secretHex: string): Promise<WasmMailSettingsMachine> {
  return call(secretHex, (c) =>
    Promise.resolve(c.mailSettingsMachine(hexToBytes(secretHex), nodeUrl())),
  );
}

// User-facing `mail-aliases` page — a person's own per-account alias surface
// (exact / wildcard / disposable + per-alias controls), WASM twin of the shared
// `MailAliasesMachine` (`libs/fauna-client-mail-settings`). User-class,
// owner-scoped, so it needs only the connection (no secret). No alias logic in
// the SPA (#2) — the page renders `snapshot()` and dispatches actions.
export function mailAliasesMachine(secretHex: string): Promise<WasmMailAliasesMachine> {
  return call(secretHex, (c) => Promise.resolve(c.mailAliasesMachine()));
}

// User-facing `mail-spam` page — a person's own per-account spam classifier
// (reset model, deployment-baseline opt-in, per-event undo), WASM twin of the
// shared `MailSpamMachine`. Undo of a client-written (sealed) training row runs
// the reseal loop, which needs the actor's MSEK — so like `mailSettingsMachine`
// it passes the 32-byte secret + the deployment node URL (to build the
// sealed-model writer). No spam logic in the SPA (#2) — the page renders
// `snapshot()` and dispatches actions.
export function mailSpamMachine(secretHex: string): Promise<WasmMailSpamMachine> {
  return call(secretHex, (c) =>
    Promise.resolve(c.mailSpamMachine(hexToBytes(secretHex), nodeUrl())),
  );
}

// User-facing `mail-export` page — a person exports their own mailbox over a
// resumable wizard (format → scope → confirm → progress → done), WASM twin of
// the shared `MailExportMachine`. Owner-scoped, and the machine has key custody
// (it mints the per-session key and opens every record under the account's
// MSEK) — so like `mailSpamMachine` it passes the 32-byte secret + the
// deployment node URL, plus the user's handle (the archive's root directory)
// and the save port, the browser half of the download
// (`$lib/mail-export-save`). No export logic in the SPA (#2): the page renders
// `snapshot()`, dispatches actions and spawns `runExport()` after a
// Start/Resume that lands a running session.
export function mailExportMachine(secretHex: string, handle: string): Promise<WasmMailExportMachine> {
  return call(secretHex, (c) =>
    Promise.resolve(
      c.mailExportMachine(hexToBytes(secretHex), nodeUrl(), handle, mailExportSavePort(nodeUrl)),
    ),
  );
}

// User-facing `mail-import` page — the export twin's mirror image: a person
// pulls their existing mail off a foreign IMAP server into their Fauna mailbox
// over a five-screen wizard (source → scope → confirm → progress → done). WASM
// twin of the shared `MailImportMachine`. User-class, owner-scoped.
//
// ⚠ The stub here is the SOURCE side, not the nest side (the opposite of the
// export twin). The nest half is real, so hydrate / pause / resume / cancel on
// an already-open session all work; `Connect` returns the honest rejection
// because the web IMAP transport over the relay is unbuilt
// (`mailbox-migration.md` § Implementation status today).
export function mailImportMachine(secretHex: string): Promise<WasmMailImportMachine> {
  return call(secretHex, (c) => Promise.resolve(c.mailImportMachine()));
}

// User-facing `task-delegation` page — a person sees which participant runs each
// heavy background task kind, and may pin one instead of letting the automatic
// policy order choose. WASM twin of the native `FfiTaskDelegationView`, over the
// same shared `TaskDelegationView` (`libs/fauna-client-delegation`): it composes
// the user's pins (`fauna.state.delegation`) with the live
// advisory lease (`fauna.delegation.observe`). The capability is NOT a parameter —
// a browser tab is always `ViewerOnly` and the wasm binding hard-codes it, so the
// SPA cannot strand a task kind by pinning it to a participant that never runs
// (participants.md § The assignment picker). No delegation logic in the SPA (#2).
//
// `deviceIdHex` is this browser's device identity for the account
// (`$lib/device-id`'s `getDeviceId(actorId)`), passed in by the caller — the same
// id the push registration keys this browser's row under.
export function taskDelegationView(
  secretHex: string,
  deviceIdHex: string,
): Promise<WasmTaskDelegationView> {
  return call(secretHex, (c) =>
    Promise.resolve(c.taskDelegationViewForDevice(deviceIdHex)),
  );
}

// `fauna.sync.devices.list` — the actor's device roster. The Task-delegation page
// joins it by hex `device_id` to name the participant running a kind; the shared
// row carries a `ParticipantRef`, not a name, because device names are client-side
// state (`fauna_core::delegation::RunnerStatus`). Rows are `DeviceSummary` — the
// same `fauna.sync.devices.list` shape the Devices page's roster machine exposes.
export function syncDevicesList(secretHex: string): Promise<DeviceSummary[]> {
  return call(secretHex, (c) => c.syncDevicesList() as Promise<DeviceSummary[]>);
}

// User-facing `mail-lists` page — a person's own per-account mailing lists (a
// list is a sixth alias kind): create / edit / delete a list, WASM twin of the
// shared `MailListsMachine`. User-class, owner-scoped (connection only). The
// list backend is deferred on the nest, so the page surfaces the honest
// rejection until it lands (UI precedes backend — not faked green).
export function mailListsMachine(secretHex: string): Promise<WasmMailListsMachine> {
  return call(secretHex, (c) => Promise.resolve(c.mailListsMachine()));
}

// User-facing `mail-list-members` drill-down off a `mail-lists` row — add /
// batch-import / unsubscribe / resubscribe one list's members, WASM twin of the
// shared `MailListMembersMachine`. Scoped to one list, so it takes the list's
// hex id + friendly name (the page heading); the factory rejects on a malformed
// hex id. Built lazily when the user opens a list's members view.
export function mailListMembersMachine(
  secretHex: string,
  listIdHex: string,
  listName: string,
): Promise<WasmMailListMembersMachine> {
  return call(secretHex, (c) => Promise.resolve(c.mailListMembersMachine(listIdHex, listName)));
}

// Unified conversations manager — wires BOTH rails over this connection: the
// `Rail::Smtp` backend (send + receive over `fauna.email.send` / `inbox.fetch`)
// and the `Rail::FaunaMls` backend (E2E MLS DMs over `fauna.conversations.*`,
// driving an in-memory `MlsEngine`). `selfAddress` is the logged-in
// `<handle>@<domain>`; the actor's 32-byte secret seeds the MLS credential. The
// receive poll + compose share this one instance — one page, every rail
// (`docs/goal/ui/conversations.md` § Goal).
export function conversationsManager(
  secretHex: string,
  selfAddress: string,
): Promise<WasmConversationsManager> {
  return call(secretHex, (c) =>
    Promise.resolve(c.conversationsManager(selfAddress, hexToBytes(secretHex))),
  );
}

// Start this tab's account runtime over this connection and register the
// account-plane conversations seams on `manager` at its store-ready edge
// (`$lib/account-runtime` owns the lifecycle). The nest URL is the session's
// own — the key its identity pin rests under, which the runtime's escrow trust
// reads. The peer-token factory is the Nests page's own (`linkedNestsMachine`
// below): the runtime connects to each linked replica nest with it
// (`account-sync-plane.md` § The bind leg, ruling 4).
export function startAccountRuntime(
  secretHex: string,
  manager: WasmConversationsManager,
): Promise<void> {
  const peerTokenProviderFactory = (peerUrl: string) => (forceRefresh: boolean) =>
    getAuthToken(secretHex, peerUrl, forceRefresh);
  return call(secretHex, (c) =>
    c.startAccountRuntime(manager, hexToBytes(secretHex), nodeUrl(), peerTokenProviderFactory),
  );
}

// The shared Feed-page manager — the web twin of the native `FfiFeedManager`,
// wrapping `fauna_feed::FeedManager<WsRpcClient>` over this connection. The
// 32-byte secret seeds post building/signing on `submitPost`. The Feed page
// renders entirely from its `snapshot()` and forwards gestures to its async
// methods — one page, one shared post-list/compose/feed-rule state
// (`docs/goal/ui/feed.md` § State & data shape). Built once the identity is
// known (`$lib/feed.ts`).
export function feedManager(secretHex: string): Promise<WasmFeedManager> {
  return call(secretHex, (c) => Promise.resolve(c.feedManager(hexToBytes(secretHex))));
}

// The owner's events-rail draft persistence — the third and last constant of
// `fauna_protocol::drafts::DRAFT_RAILS` (`reserved-folders.md` § Drafts Sync).
// Not a page manager: the Events page has no shared manager on any app, so this
// face carries the five `event-form` inputs while the canonical encoding, the
// seal, the WS calls and the launch gate stay in Rust. The 32-byte secret seeds
// the at-rest `BackupKey` only — drafts are owner-only and sign nothing. Built
// once the identity is known (`$lib/event-drafts.ts`).
export function eventDrafts(secretHex: string): Promise<WasmEventDrafts> {
  return call(secretHex, (c) => Promise.resolve(c.eventDrafts(hexToBytes(secretHex))));
}

// The shared Search-page manager — the web twin of the native
// `FfiSearchManager`, wrapping `fauna_client_search::SearchManager<WsRpcClient>`
// over this connection. Unlike `feedManager` this needs no actor secret —
// searching signs nothing. The Search page renders entirely from its
// `snapshot()` and forwards gestures to its async methods
// (`docs/goal/ui/search.md` § State & data shape). Built once the identity is
// known (`$lib/search.ts`).
export function searchManager(secretHex: string): Promise<WasmSearchManager> {
  return call(secretHex, (c) => Promise.resolve(c.searchManager()));
}

// ── fauna.conversations.keypackage.* ────────────────────────────────

/** `fauna.conversations.keypackage.count` — non-destructive count of the
 *  actor's remaining non-expired MLS key packages (self, for the settings
 *  top-up gauge). WS-RPC twin of the deleted `GET /api/v1/keypackage/{actor}/count`. */
export function keypackageCount(secretHex: string, actorIdHex: string): Promise<number> {
  return call(secretHex, (c) => c.keypackageCount(actorIdHex));
}

/** `fauna.conversations.keypackage.upload` — publish the wasm-MLS-generated key
 *  packages (hex) for the calling actor; resolves the stored count.
 *  `lastResort = false` for the consumable login top-up pool. WS-RPC twin of the
 *  deleted `POST /api/v1/keypackage/{actor}`. */
export function keypackageUpload(
  secretHex: string,
  packagesHex: string[],
  lastResort = false,
): Promise<number> {
  return call(secretHex, (c) => c.keypackageUpload(packagesHex, lastResort));
}

// ── fauna.email.inbox.fetch (conversations SMTP receive rail) ────────

/** One sealed inbound record as shipped by `fauna.email.inbox.fetch` — the
 *  body stays HPKE-sealed until `WasmConversationsManager.ingestSealedInbound`
 *  opens it client-side. `message_id` / `sealed_envelope` are raw bytes. */
export interface InboxMessage {
  uid: number;
  message_id: Uint8Array | number[];
  internal_date: number;
  /** The record's **seal instant** (`segment_records.stored_at`, epoch seconds)
   *  — the content-sealing-epochs classification basis for the client's epoch
   *  opener. Distinct from `internal_date` (imported mail diverges by design), so the opener keys off this, not the sender-supplied
   *  `Date:`. Always present; `0` = unknown (the nest's append-time clock read
   *  failed), a standing-sealed record the opener's standing arm opens. */
  stored_at: number;
  /** The sealed outer envelope, inline. **Empty** when `body_ref` is set — the
   *  bytes are on the byte plane instead. */
  sealed_envelope: Uint8Array | number[];
  /** IMAP flag/keyword set (`\Seen`, `\Junk`, `$FaunaSpamScored`, …). The
   *  on-device spam scorer reads it to skip a message already carrying the
   *  `$FaunaSpamScored` watermark (mail-spam.md § Re-file timing). Additive —
   *  absent → `undefined`; treat as empty. */
  flags?: string[];
  /** Present only when this message's stored outer envelope is too large to
   *  ride the 2 MiB WS-RPC frame inline: the sealed bytes wait on the byte
   *  plane and this names them (smtp-server.md § Message size limits). Resolve
   *  it via `WasmConversationsManager.resolveMailBodyRef` to get the exact
   *  envelope, then ingest that as if it had arrived inline. Additive —
   *  `undefined` from any nest that doesn't serve references (and from every
   *  nest for a message under the frame). */
  body_ref?: MailBodyRef;
}
/** The ordered chunk hashes a large sealed body was staged under, plus the
 *  length they must rejoin to. `total_bytes` arrives as a plain number (a mail
 *  body is bounded far below 2^53). */
export interface MailBodyRef {
  chunk_hashes: (Uint8Array | number[])[];
  total_bytes: number;
}
export interface InboxFetchReply {
  messages: InboxMessage[];
  more: boolean;
  /** The mailbox's highest modseq when the page was built — the mail
   *  read-state sync's flag-change baseline (`mail-app-surface.md` § Read
   *  state), handed to `WasmConversationsManager.noteInboxPage` for INBOX
   *  pages. Additive — optional. */
  highest_modseq?: number;
}

/** Fetch one page of sealed INBOX records with `uid > afterUid` (`limit` 0 =
 *  source default). The conversations receive poll drains pages, decrypting +
 *  ingesting each (`$lib/conversations.ts`). */
export function emailInboxFetch(
  secretHex: string,
  afterUid: number,
  limit: number,
): Promise<InboxFetchReply> {
  return call(secretHex, (c) => c.emailInboxFetch(afterUid, limit) as Promise<InboxFetchReply>);
}

/** Fetch one page of sealed Sent records (`uid > afterUid`) — the Sent sibling of
 *  `emailInboxFetch` (same `InboxFetchReply` wire shape; the mailbox is chosen
 *  server-side). Surfaces mail the user sent from an external MUA; the
 *  conversations receive poll drains a separate Sent cursor, decrypting +
 *  ingesting each as an outbound message (`$lib/conversations.ts`). */
export function emailSentFetch(
  secretHex: string,
  afterUid: number,
  limit: number,
): Promise<InboxFetchReply> {
  return call(secretHex, (c) => c.emailSentFetch(afterUid, limit) as Promise<InboxFetchReply>);
}

// ── On-device mail spam scorer (mail-spam.md § Scoring placement) ─────
//
// The three RPCs the conversations receive loop drives around the INBOX drain
// to score mail on-device (the Fauna-app scoring position): fetch the sealed
// per-user model + the effective policy, then apply the scorer's watermark+move
// outcome. Scoring itself runs in `WasmConversationsManager` — the decrypted
// body + unwrapped model never cross into JS.

/** The admin-effective spam-scoring policy (`fauna.bridges.get_spam_scoring_policy`)
 *  the on-device scorer scores against, so its INBOX→Junk line matches the
 *  MDA/nest (mail-spam.md § Architectural rules). Snake_case = the wire reply. */
export interface SpamScoringPolicy {
  spam_folder_threshold: number;
  bayesian_weight_milli: number;
  bayesian_min_samples: number;
  bayesian_full_confidence_samples: number;
}

/** `fauna.bridges.fetch_spam_model` → `{ blob, baseline }` — the caller's
 *  per-user model **sealed to them** (a bare inner `wrapped_blob`) plus, for a
 *  client-sealed stored model only, the published deployment baseline the
 *  agent folds locally (else `baseline: null` — the nest already folded it
 *  server-side; mail-spam.md § Encrypted-mode interaction). `null` overall
 *  when untrained (cold start). `actorIdHex` is the caller's OWN 32-byte id
 *  hex (the RPC is caller-scoped). Both are handed to
 *  `WasmConversationsManager.enableSpamScoring`, which unwraps the model under
 *  the held recipient secret (the model never lands unwrapped in JS). */
export interface FetchedSpamModel {
  blob: Uint8Array;
  baseline: Uint8Array | null;
}

export function fetchSpamModel(
  secretHex: string,
  actorIdHex: string,
): Promise<FetchedSpamModel | null> {
  return call(
    secretHex,
    (c) => c.fetchSpamModel(hexToBytes(actorIdHex)) as Promise<FetchedSpamModel | null>,
  );
}

/** `fauna.bridges.get_spam_scoring_policy` → the effective `spam_folder` tier +
 *  Bayesian knobs. Server-wide, non-secret; readable by any `User`. */
export function getSpamScoringPolicy(secretHex: string): Promise<SpamScoringPolicy> {
  return call(secretHex, (c) => c.getSpamScoringPolicy() as Promise<SpamScoringPolicy>);
}

// ── Per-account spam-threshold override (mail-policy-config.md § Tier 3) — the mail-spam page's threshold input. ──

/** `fauna.bridges.get_spam_threshold_override` → the caller's per-account
 *  spam-folder threshold override, or `null` when the account follows the
 *  admin default. */
export function spamThresholdOverrideGet(secretHex: string): Promise<number | null> {
  return call(secretHex, (c) => c.spamThresholdOverrideGet() as Promise<number | null>);
}

/** `fauna.bridges.set_spam_threshold_override` — set (or clear, with `null`)
 *  the override; resolves to the persisted value the nest confirmed, never
 *  the local edit. `0` is a real setting — it turns automatic Junk filing
 *  off for this account, distinct from `null` (follow the admin default). */
export function spamThresholdOverrideSet(
  secretHex: string,
  value: number | null,
): Promise<number | null> {
  return call(secretHex, (c) => c.spamThresholdOverrideSet(value) as Promise<number | null>);
}

/** `fauna.email.apply_spam_disposition` → `{ watermarked, moved_to_junk }`.
 *  Watermark every `scoredUids` message `$FaunaSpamScored`, then move the
 *  `junkUids` subset (⊆ scoredUids) INBOX→Junk. */
export function applySpamDisposition(
  secretHex: string,
  scoredUids: number[],
  junkUids: number[],
): Promise<{ watermarked: number; moved_to_junk: number }> {
  return call(
    secretHex,
    (c) =>
      c.applySpamDisposition(Uint32Array.from(scoredUids), Uint32Array.from(junkUids)) as Promise<{
        watermarked: number;
        moved_to_junk: number;
      }>,
  );
}

// ── fauna.bridges.* (encrypted CalDAV store) ─────────────────────────
//
// The Events page's WS-RPC surface over the *encrypted* `bridge_caldav_*` store
// via the shared `fauna_client_caldav::CalDavClient` (events.md Decision-B store
// flip), retiring the legacy plaintext `fauna.events.*` + `/api/calendars` path.
// `secretHex` is passed into the wasm method too: it derives the actor id and
// loads `cfg.mail.msek` *inside wasm* (the msek never crosses into JS) to
// seal/unseal the store. `nowSecs` is the current epoch-seconds (the wasm-time
// discipline — wasm never calls `Date.now()`); `selfEmail` is the caller's
// `<handle>@<handle_domain>`. Event `id` is the hex `uid_hash` (the mutate key).

/** The actor's calendars `{ calendars: [{ id, name, color, visibility, event_count }] }`.
 *  Lazy-provisions Personal on first load; empty when mail/CalDAV is off. */
export function caldavListCalendars(secretHex: string): Promise<{ calendars: any[] }> {
  return call(secretHex, (c) => c.caldavListCalendars(secretHex) as Promise<{ calendars: any[] }>);
}

/** Provision a new calendar (MKCOL). `calendarId` is a client-generated 32-byte
 *  hex id (`crypto.getRandomValues`). Resolves `{ id }`. */
export function caldavProvisionCalendar(
  secretHex: string,
  calendarId: string,
  name: string,
  color: string,
): Promise<{ id: string }> {
  return call(
    secretHex,
    (c) => c.caldavProvisionCalendar(secretHex, calendarId, name, color) as Promise<{ id: string }>,
  );
}

/** A calendar's events `{ events: [{ id, uid, summary, dtstart, dtend, location,
 *  description, status, is_all_day, rrule, alarm, organized_by_me, attendees }] }`.
 *  `organized_by_me` is computed in wasm from the VEVENT ORGANIZER vs `selfEmail`
 *  (the canonical author-gate predicate). Empty when mail/CalDAV is off. */
export function caldavQueryEvents(
  secretHex: string,
  calendarId: string,
  selfEmail: string,
): Promise<{ events: any[] }> {
  return call(
    secretHex,
    (c) => c.caldavQueryEvents(secretHex, calendarId, selfEmail) as Promise<{ events: any[] }>,
  );
}

// ── CardDAV Address Book (read-only, slice 4b) ────────────────────────────────
//
// The Contacts page's "Address Book" segment over the encrypted `bridge_carddav_*`
// store via the shared `fauna_client_carddav::CardDavClient` — the read analogue of
// the caldav Events wrappers above. Unseal + parse happen in wasm (`secretHex`
// derives the actor id + loads `cfg.mail.msek` inside wasm — the msek never crosses
// into JS). No write path in 4b. carddav-server.md § Independent enablement.

/** The actor's address books `{ addressbooks: [{ id, name, description, card_count }] }`.
 *  Read-only; empty when mail/CardDAV is off or no book exists yet (no lazy
 *  provisioning — a book is minted by a CardDAV MUA or the future write slice). */
export function carddavListAddressbooks(
  secretHex: string,
): Promise<{ addressbooks: any[] }> {
  return call(
    secretHex,
    (c) => c.carddavListAddressbooks(secretHex) as Promise<{ addressbooks: any[] }>,
  );
}

/** Locate a card by its `uid_hash` (hex-encoded `blake3(uid)`) across every
 *  address book the actor holds — the `SearchNav::Contact` deep-link door a
 *  search hit needs (`ui/search.md` § Where logic lives → *Result navigation
 *  (deep link)*), since the holding book need not be the one currently open,
 *  or loaded at all. Resolves `{ addressbooks: [...], addressbook_id: hex |
 *  null, card_id: hex | null, cards: [...] }` — every address book (so the
 *  Contacts page's picker renders off one round of reads) plus the found
 *  card's location; `cards` is the found book's own decoded rows (same shape
 *  `carddavQueryCardsDecoded` returns), `addressbook_id`/`card_id` are both
 *  `null` when no book holds the uid_hash (deleted since it was indexed). ⚠
 *  `uid_hash` and `card_id` are different id spaces of the same width — never
 *  cast one for the other; this is the only sanctioned lookup. */
export function carddavLocateCardByUidHash(
  secretHex: string,
  uidHashHex: string,
): Promise<{ addressbooks: any[]; addressbook_id: string | null; card_id: string | null; cards: any[] }> {
  return call(
    secretHex,
    (c) =>
      c.carddavLocateCardByUidHash(secretHex, uidHashHex) as Promise<{
        addressbooks: any[];
        addressbook_id: string | null;
        card_id: string | null;
        cards: any[];
      }>,
  );
}

/** An address book's vCards `{ cards: [{ id, uid, formatted_name, emails, tels,
 *  addresses, urls, org, title, note, bday, has_fauna_ext }] }`, where each
 *  `emails`/`tels`/`urls` entry is `{ value, types, pref }` and each `addresses`
 *  entry carries the structured components + a shared one-line `formatted`. Unsealed
 *  + parsed in wasm. Empty when mail/CardDAV is off or the book has no row. */
export function carddavQueryCardsDecoded(
  secretHex: string,
  addressbookId: string,
): Promise<{ cards: any[] }> {
  return call(
    secretHex,
    (c) => c.carddavQueryCardsDecoded(secretHex, addressbookId) as Promise<{ cards: any[] }>,
  );
}

/** Create an event in `calendarId`. Resolves `{ id: hex(uid_hash), uid }`. */
export function caldavCreateEvent(
  secretHex: string,
  params: {
    calendar_id: string;
    uid: string;
    summary: string;
    dtstart: string;
    dtend: string;
    description?: string;
    location?: string;
  },
  selfEmail: string,
  nowSecs: number,
): Promise<{ id: string; uid: string }> {
  return call(
    secretHex,
    (c) =>
      c.caldavCreateEvent(
        secretHex,
        params.calendar_id,
        params.uid,
        params.summary,
        params.dtstart,
        params.dtend,
        params.description,
        params.location,
        selfEmail,
        nowSecs,
      ) as Promise<{ id: string; uid: string }>,
  );
}

/** Delete an event by `uidHash` (hex). */
export function caldavDeleteEvent(
  secretHex: string,
  calendarId: string,
  uidHash: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.caldavDeleteEvent(secretHex, calendarId, uidHash) as Promise<void>,
  );
}

/** RSVP (`going` / `interested` / `declined` / `tentative`) for `selfEmail`. */
export function caldavRsvpEvent(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  response: string,
  selfEmail: string,
  nowSecs: number,
): Promise<{ status: string }> {
  return call(
    secretHex,
    (c) =>
      c.caldavRsvpEvent(secretHex, calendarId, uidHash, response, selfEmail, nowSecs) as Promise<{
        status: string;
      }>,
  );
}

/** The event's reminder offset (ICS duration, e.g. `-PT15M`), or `null`. */
export function caldavReminderGet(
  secretHex: string,
  calendarId: string,
  uidHash: string,
): Promise<string | null> {
  return call(
    secretHex,
    (c) => c.caldavReminderGet(secretHex, calendarId, uidHash) as Promise<string | null>,
  );
}

/** Set the event's reminder `offset`. Resolves the offset. */
export function caldavReminderSet(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  offset: string,
  nowSecs: number,
): Promise<string> {
  return call(
    secretHex,
    (c) => c.caldavReminderSet(secretHex, calendarId, uidHash, offset, nowSecs) as Promise<string>,
  );
}

/** Clear the event's reminder. */
export function caldavReminderRemove(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  nowSecs: number,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.caldavReminderRemove(secretHex, calendarId, uidHash, nowSecs) as Promise<void>,
  );
}

/** Invite an attendee by email (adds a `mailto:` ATTENDEE + re-PUTs). */
export function caldavInviteAttendee(
  secretHex: string,
  calendarId: string,
  uidHash: string,
  attendeeEmail: string,
  selfEmail: string,
  nowSecs: number,
): Promise<void> {
  return call(
    secretHex,
    (c) =>
      c.caldavInviteAttendee(
        secretHex,
        calendarId,
        uidHash,
        attendeeEmail,
        selfEmail,
        nowSecs,
      ) as Promise<void>,
  );
}

/** Export the calendar as a single RFC 5545 `.ics` string (`generate_ical_multi`
 *  over every stored event). */
export function caldavExportCalendar(
  secretHex: string,
  calendarId: string,
): Promise<string> {
  return call(
    secretHex,
    (c) => c.caldavExportCalendar(secretHex, calendarId) as Promise<string>,
  );
}

/** Import `.ics` text into the calendar (`parse_ical_multi` → seal + PUT each
 *  VEVENT). Resolves `{ imported, skipped, total }`. */
export function caldavImportCalendar(
  secretHex: string,
  calendarId: string,
  icsText: string,
  selfEmail: string,
  nowSecs: number,
): Promise<{ imported: number; skipped: number; total: number }> {
  return call(
    secretHex,
    (c) =>
      c.caldavImportCalendar(secretHex, calendarId, icsText, selfEmail, nowSecs) as Promise<{
        imported: number;
        skipped: number;
        total: number;
      }>,
  );
}

// Settings → Privacy spam-classifier preferences — the WS-RPC seam over the
// shared `fauna_client_spam::SpamClient` (`fauna.spam.{get,set}_preferences`),
// retiring `GET|PUT /api/v1/spam/preferences`. Thresholds are probability
// `[0.0, 1.0]` (the wasm twin converts to/from the per-mille wire), so the shape
// is identical to the retired HTTP twin. No spam logic in the SPA (priority #2).
export interface SpamPreferences {
  spam_threshold: number;
  phishing_threshold: number;
}

export function spamGetPreferences(secretHex: string): Promise<SpamPreferences> {
  return call(secretHex, (c) => c.spamGetPreferences() as Promise<SpamPreferences>);
}

export function spamSetPreferences(
  secretHex: string,
  prefs: Partial<SpamPreferences>,
): Promise<SpamPreferences> {
  return call(
    secretHex,
    (c) =>
      c.spamSetPreferences(
        prefs.spam_threshold,
        prefs.phishing_threshold,
      ) as Promise<SpamPreferences>,
  );
}

// Full-text search — the WS-RPC seam over the shared
// `fauna_client_search::SearchClient` (`fauna.search.query`), retiring
// `GET /api/v1/search`. The wasm twin restores `rank` to the float the HTTP
// twin returned, so the shape is identical. No search logic in the SPA
// (priority #2).
export interface SearchResult {
  content_type: string;
  content_id: string;
  created_at: number;
  rank: number;
  snippet: string;
}

export function searchQuery(
  secretHex: string,
  query: string,
  contentType?: string,
  limit?: number,
): Promise<SearchResult[]> {
  return call(secretHex, async (c) => {
    const reply = (await c.searchQuery(query, contentType, limit)) as { results: SearchResult[] };
    return reply.results;
  });
}

// `fauna.moderation.train` — the WS-RPC seam over the shared
// `fauna_client_moderation::ModerationClient`: the nest half of a training
// correction (read gate + report capture). No moderation logic in the SPA
// (priority #2).
export function moderationTrain(
  secretHex: string,
  contentId: string,
  verdict: 'spam' | 'ham',
): Promise<void> {
  return call(secretHex, async (c) => {
    await c.moderationTrain(contentId, verdict);
  });
}

// `moderationScanReport` / `ScanLabelInput` were removed 2026-07-19 (the
// client-side producer was retired — `moderation.md` § State & data shape owns
// the verdict) and the `fauna.moderation.scan_report` kind itself left the wire
// 2026-09-24 with the compat-remnant sweep.

/** One server-issued obligation-action row from `fauna.moderation.actions` — why a
 * piece of the caller's **own** content was labeled / quarantined / rejected.
 * `confidence_per_mille` is `u16` (0–1000, the dag-cbor float ban); `action` is the
 * raw obligation-action discriminant (rendered via the shared `obligationActionLabel`).
 * See `docs/goal/behavior/moderation.md` § State & data shape. */
export interface ObligationAction {
  id: number;
  content_type: string;
  content_id: string;
  category: string;
  confidence_per_mille: number;
  action: number;
  timestamp: number;
}

/** `fauna.moderation.actions` — the caller's own server-issued moderation queue
 * (obligation-action records: mail-ingest/admin quarantine·reject·label + appeals,
 * plus plaintext-mode social labels). These are the queue's **server half**: the
 * `moderationQueue` wasm face unions them with the session's post-decrypt local
 * detections (`$lib/conversations` `moderationLocalDetections`) through the shared
 * `merge_queue`, so the superset — and its dedupe rule — is one shared impl, not a
 * TS hand-roll (`moderation.md` § State & data shape). No moderation logic in the
 * SPA (priority #2). */
export function moderationActions(secretHex: string): Promise<ObligationAction[]> {
  return call(secretHex, async (c) => {
    const reply = (await c.moderationActions()) as { actions: ObligationAction[] };
    return reply.actions;
  });
}

// Push registration — the WS-RPC seam over the shared
// `fauna_client_push::registration` machine (the install opt-in bit, the
// which-actor record, the leave-shape drops), keyed under this browser's device
// id for the session's actor wasm-side. The browser `serviceWorker`/
// `PushManager` work that produces a subscription stays in `$lib/push.ts`.
export function pushVapidKey(secretHex: string): Promise<string> {
  return call(secretHex, async (c) => {
    const reply = (await c.pushVapidKey()) as { public_key: string };
    return reply.public_key;
  });
}

/** A browser push subscription, as `PushManager` hands it back. */
export interface BrowserPushSubscription {
  endpoint: string;
  key_p256dh: string;
  key_auth: string;
}

export function pushEnable(secretHex: string, sub: BrowserPushSubscription): Promise<void> {
  return call(secretHex, (c) => c.pushEnable(sub.endpoint, sub.key_p256dh, sub.key_auth));
}

export function pushDisable(secretHex: string): Promise<void> {
  return call(secretHex, (c) => c.pushDisable());
}

export function pushRearm(secretHex: string, sub: BrowserPushSubscription): Promise<boolean> {
  return call(secretHex, (c) => c.pushRearm(sub.endpoint, sub.key_p256dh, sub.key_auth));
}

export function pushDropActorRow(secretHex: string): Promise<void> {
  return call(secretHex, (c) => c.pushDropActorRow());
}

// User-settings `linked-nests` page — per-user nest pairing (list/link/unlink),
// the WASM twin of the UniFFI `LinkedNestsMachine` (`libs/fauna-client-pair`).
// Renders `fauna.pair.list`; dispatches Link → `fauna.pair.add`, Unlink →
// `fauna.pair.revoke`. No pairing logic in the SPA (priority #2).
export function linkedNestsMachine(secretHex: string): Promise<WasmLinkedNestsMachine> {
  // Both-ends linking (`LinkBoth`) opens a *second* authenticated WS-RPC client
  // to a peer nest. The factory yields a token provider bound to the peer origin
  // — `getAuthToken(secret, peerUrl, …)` mints that bearer over the CORS-exempt
  // anonymous WS (`challengeVerify`), the path the cross-origin HTTP `/auth/token`
  // fetch could not take. The single-end `Link` path never invokes it.
  const peerTokenProviderFactory = (peerUrl: string) => (forceRefresh: boolean) =>
    getAuthToken(secretHex, peerUrl, forceRefresh);
  return call(secretHex, (c) =>
    Promise.resolve(c.linkedNestsMachine(peerTokenProviderFactory)),
  );
}

// The Nests-page machine (`linkedNestsMachine` + the trust-facet seams). Same
// linking surface, plus: the snapshot's `home` row carries this nest's current
// content-processing grants (Now) / grant-event timeline (History), and the
// machine drives Renew / Revoke / SetLens. `secret` (the 32-byte ed25519 actor
// key) is handed to the machine so it can sign the client-authoritative grant
// log behind its seam — the raw key never crosses back to JS. No trust logic in
// the SPA (priority #2). Both-ends linking still rides the peer-token factory.
export function linkedNestsMachineWithTrust(secretHex: string): Promise<WasmLinkedNestsMachine> {
  const peerTokenProviderFactory = (peerUrl: string) => (forceRefresh: boolean) =>
    getAuthToken(secretHex, peerUrl, forceRefresh);
  return call(secretHex, (c) =>
    Promise.resolve(c.linkedNestsMachineWithTrust(peerTokenProviderFactory, hexToBytes(secretHex))),
  );
}

// The post-claim serving enablement (onboarding.md § 3b *Mechanism*): the ONE
// shared Rust step every app's `LoggedIn` handoff runs
// (`fauna_client_mail_settings::serving_enablement`). The four intents are the
// onboarding machine's `*EnableRequested()`; resolves once every step has
// answered. No enablement logic in the SPA (priority #2).
export function applyServingEnablement(
  secretHex: string,
  intents: { email: boolean; caldav: boolean; carddav: boolean; webdav: boolean },
): Promise<void> {
  return call(secretHex, (c) =>
    c.applyPostClaimServingEnablement(
      hexToBytes(secretHex),
      nodeUrl(),
      intents.email,
      intents.caldav,
      intents.carddav,
      intents.webdav,
    ),
  );
}

export function bridgeApprovalMachine(secretHex: string): Promise<WasmBridgeApprovalMachine> {
  return call(secretHex, (c) => Promise.resolve(c.bridgeApprovalMachine()));
}

// Admin `admin-aliases` page — external forwarders (admin.md § 4 / mail-aliases.md
// § Kind 7), the WASM twin of the shared `ForwarderMachine`
// (`libs/fauna-client-mail-settings::forwarders`). Renders
// `fauna.bridges.list_forwarders` + `list_local_domains`; dispatches Create →
// `create_forwarder`, Delete → `delete_forwarder`. No forwarder logic in the SPA
// (priority #2).
export function forwarderMachine(secretHex: string): Promise<WasmForwarderMachine> {
  return call(secretHex, (c) => Promise.resolve(c.forwarderMachine()));
}

// Admin `admin-mail` flat mail-policy page (admin.md § 6 Mail /
// mail-policy-config.md § Policy catalog Tier 2), the WASM twin of the shared
// `MailPolicyMachine` (`libs/fauna-client-mail-settings::admin_policy`). Hydrates
// via the Admin read twin `fauna.bridges.get_mail_config`; dispatches
// SetMailEnabled → `set_mail_enabled`, SaveSpam → `put_spam_policy`, SaveAuth →
// `put_auth_policy` (full-PUT sub-structs). No policy logic in the SPA (priority
// #2). Lifts the linux lead-client page (apps/fauna-linux/src/settings/admin_mail.rs).
export function mailPolicyMachine(secretHex: string): Promise<WasmMailPolicyMachine> {
  return call(secretHex, (c) => Promise.resolve(c.mailPolicyMachine()));
}

// Admin `admin-calendar` flat CalDAV-enable page (admin.md § 8 Calendar /
// caldav-server.md § Independent enablement), the WASM twin of the shared
// `CaldavPolicyMachine` (`libs/fauna-client-mail-settings::caldav_policy`) — the
// single-toggle sibling of `mailPolicyMachine`. Hydrates `caldav_enabled` via the
// Admin read twin `fauna.bridges.get_mail_config`; dispatches SetCaldavEnabled →
// `fauna.bridges.set_caldav_enabled`. No policy logic in the SPA (priority #2);
// lifts the linux lead page (apps/fauna-linux/src/settings/admin_calendar.rs).
export function caldavPolicyMachine(secretHex: string): Promise<WasmCaldavPolicyMachine> {
  return call(secretHex, (c) => Promise.resolve(c.caldavPolicyMachine()));
}

// Admin `admin-contacts` flat CardDAV-enable page (admin.md § Contacts /
// carddav-server.md § Independent enablement), the WASM twin of the shared
// `CarddavPolicyMachine` (`libs/fauna-client-mail-settings::carddav_policy`) —
// the contacts sibling of `caldavPolicyMachine`. Hydrates `carddav_enabled` via
// the Admin read twin `fauna.bridges.get_mail_config`; dispatches
// SetCarddavEnabled → `fauna.bridges.set_carddav_enabled`. No policy logic in
// the SPA (priority #2).
export function carddavPolicyMachine(secretHex: string): Promise<WasmCarddavPolicyMachine> {
  return call(secretHex, (c) => Promise.resolve(c.carddavPolicyMachine()));
}

// Admin `admin-files` flat WebDAV-enable page (admin.md § Files /
// webdav-server.md § Independent enablement), the WASM twin of the shared
// `WebdavPolicyMachine` (`libs/fauna-client-mail-settings::webdav_policy`) —
// the files sibling of `carddavPolicyMachine`. Hydrates `webdav_enabled` via
// the Admin read twin `fauna.bridges.get_mail_config`; dispatches
// SetWebdavEnabled → `fauna.bridges.set_webdav_enabled`. No policy logic in
// the SPA (priority #2).
export function webdavPolicyMachine(secretHex: string): Promise<WasmWebdavPolicyMachine> {
  return call(secretHex, (c) => Promise.resolve(c.webdavPolicyMachine()));
}

// ── The Backups page's snapshot half is NOT here ────────────────────
//
// It moved onto the shared `BackupsMachine` (`libs/fauna-backups-machine`, WASM
// twin `libs/fauna-wasm-backups`, loaded via `$lib/wasm-backups`) on 2026-08-05,
// which is what retired this seam's eight `fauna.filesync.snapshot.*` /
// `fauna.sync.backup_status` wrappers: the machine composes those kinds in Rust
// over `SnapshotsClient<R>` and hands the page one renderable snapshot. Nothing
// else consumed them — `fauna.sync.backup_status` in particular was read ONLY to
// derive `last-backed-up`, from `last_change_at` (the last *file change*, not the
// last snapshot), which is the live wrong-value bug the ratified derivation
// replaces. The message-kind RESTORE reads below are a different surface and stay.
//
// The Folders page's "Sync defaults" section — the user-global default
// conflict policy stamped onto NEW folders (file-sync.md § Conflicts,
// policy; user-approved 2026-07-11). Sealed under the owner's BackupKey in
// `fauna.state.sync-prefs`; existing sets are untouched — each set's own
// `folder-conflict-policy-select` stays authoritative.

/** The default conflict policy for new sets (`"auto"` | `"latest_wins_always"`),
 *  or `null` = no preference (nest column default, "auto"). */
export function defaultConflictPolicyGet(secretHex: string): Promise<string | null> {
  return call(secretHex, (c) => c.loadSyncPrefs() as Promise<string | null>);
}

/** Set (or clear, with `null`) the default conflict policy for new sets;
 *  returns the stored value. */
export function defaultConflictPolicySet(
  secretHex: string,
  policy: string | null,
): Promise<string | null> {
  return call(
    secretHex,
    (c) => c.saveSyncPrefs(policy ?? undefined) as Promise<string | null>,
  );
}

// Owner-side cross-user sharing roster (the "Shared with" section on an expanded
// folder-row — folders.md § Sharing). These two faces live on the WsRpcClient
// (unlike foldersShareSet/RemoveMember, which run the MLS author orchestration on
// the conversations manager). The roster read powers the `folder-shared-badge`
// ("Shared · N") + the `folder-member-item` list; set_access edits a member's
// Reader/Writer access + byte cap in place.

/** `fauna.folders.members.list_actors` — the cross-user actor roster of a
 *  shared set ({actor_id, handle, role, access, byte_cap, …}); "member" rows feed
 *  the owner-side list, "owner" the recipient badge. The nest cannot observe an
 *  MLS join, so "Pending"/"Active" is client-derived (folders.md § Sharing). */
export function foldersActorMembers(
  secretHex: string,
  name: string,
): Promise<FolderActorMember[]> {
  return call(secretHex, (c) => c.foldersActorMembers(name) as Promise<FolderActorMember[]>);
}

/** `fauna.folders.members.list` — the enrolled DEVICE roster of a folder
 *  ({device_id, label, role, flags}), the rows the device-place editor paints.
 *  Distinct from `foldersActorMembers` (the cross-user actor roster). The SPA
 *  never reads `role`/`flags` off these itself — it puts the reply through
 *  `placeRows`, the one shared flags-only rule all 7 apps
 *  resolve a seat with. */
export function foldersMembers(secretHex: string, name: string): Promise<FolderMember[]> {
  return call(secretHex, (c) => c.foldersMembers(name) as Promise<FolderMember[]>);
}

/** `fauna.folders.members.set_access` — the owner sets a member's access
 *  ("reader" | "writer") and byte cap in place. `byteCap` rides as `f64`
 *  (wasm no-i64-param rule); `undefined` = uncapped. Access + cap are written as
 *  ONE pair — sending one half would clear the other (multi-writer Phase 1). */
export function foldersMemberSetAccess(
  secretHex: string,
  name: string,
  actorId: string,
  access: string,
  byteCap?: number,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.foldersMemberSetAccess(name, actorId, access, byteCap) as Promise<void>,
  );
}

/** `fauna.folders.devices` — per-device recorded sync activity for one file
 *  set ({device_id, label, last_change_at, change_count}), read on demand when
 *  the row's device-activity detail expands (the ordinary sync activity
 *  signal — file-sync.md § Implementation status today). */
export function foldersDevices(secretHex: string, name: string): Promise<FolderDevice[]> {
  return call(secretHex, (c) => c.foldersDevices(name) as Promise<FolderDevice[]>);
}

// Destination places (docs/goal/behavior/backup-destinations.md § Ordinary-folder
// coverage) — the folders page's per-folder attach/detach section, over the
// shared `fauna_client_config::{list_folder_destinations,
// attach_folder_to_destination, detach_folder_from_destination}` sequences.
// Each mutation returns the folder's re-read places (never an optimistic
// flip), same non-optimistic contract as linux's `attach_folder_destination`.

/** `fauna.backup.destination.list` joined with the config's display names,
 *  for ONE folder — lazy-loaded on row expand, same shape as `foldersDevices`. */
export function foldersDestinationsList(
  secretHex: string,
  folderId: number,
): Promise<FolderDestinationPlace[]> {
  return call(
    secretHex,
    (c) => c.folderDestinationsList(secretHex, folderId) as Promise<FolderDestinationPlace[]>,
  );
}

/** Attach `folderId` to `destinationId`; resolves to the folder's re-read places. */
export function foldersDestinationAttach(
  secretHex: string,
  folderId: number,
  destinationId: string,
): Promise<FolderDestinationPlace[]> {
  return call(
    secretHex,
    (c) =>
      c.folderDestinationAttach(secretHex, folderId, destinationId) as Promise<
        FolderDestinationPlace[]
      >,
  );
}

/** Detach `folderId` from `destinationId`; `folderSet` is the attached row's own
 *  `folder_set` — never re-derived here. Resolves to the folder's re-read places. */
export function foldersDestinationDetach(
  secretHex: string,
  folderId: number,
  destinationId: string,
  folderSet: string,
): Promise<FolderDestinationPlace[]> {
  return call(
    secretHex,
    (c) =>
      c.folderDestinationDetach(secretHex, folderId, destinationId, folderSet) as Promise<
        FolderDestinationPlace[]
      >,
  );
}

// Media page sync state — the WS-RPC seam over the shared
// `fauna_client_sync::SyncClient` (`fauna.sync.{status,files}`), retiring
// `GET /api/v1/sync/{status,files}`. No sync logic in the SPA (priority #2).
export interface SyncStatus {
  folder: string;
  source_online: boolean;
}

export interface SyncFile {
  path: string;
  manifest_hash: string;
  size_bytes: number;
  updated_at: number;
}

export function syncStatus(secretHex: string, folder: string): Promise<SyncStatus> {
  return call(secretHex, (c) => c.syncStatus(folder) as Promise<SyncStatus>);
}

export function syncFiles(secretHex: string, folder: string): Promise<SyncFile[]> {
  return call(secretHex, (c) => c.syncFiles(folder) as Promise<SyncFile[]>);
}

// ── Backups restore surfaces (message-kind path) ────────────────────
//
// Restore-snapshot picker + restore history / divergence reads + the
// local restore action (`docs/goal/ui/backups.md` §§ Restore history /
// Restore divergence / Restore from backup destination). Mirror of the
// Linux `views/backups/restore.rs` glue over the shared `SnapshotsClient`.

// One message-kind snapshot in the local-restore picker.
export interface MessageKindSnapshot {
  id: number;
  created_at: number;
  // `"mail"` / `"calendar"` for message-kind snapshots.
  message_kind?: string | null;
  file_count: number;
  total_bytes: number;
}

export interface RestoreHistoryRow {
  id: number;
  completed_at: number;
  snapshot_id: number;
  // `"mail"` / `"calendar"` / `"mail+calendar"`.
  kinds_restored: string;
  // Backup-destination provenance (a CBOR byte string → `Uint8Array`);
  // `null` → "local snapshot".
  source_member_id?: Uint8Array | null;
}

export interface RestoreDivergenceRow {
  id: number;
  snapshot_id: number;
  observed_at: number;
  protocol: string;
  // Mailbox name (IMAP) or calendar id hex (CalDAV).
  collection: string;
  // Advisory MUA identity; `null` → "(unknown)".
  mua_id?: string | null;
  client_modseq: number;
  server_modseq: number;
  lost_event_count: number;
}

export interface RestoreMessageKindResult {
  snapshot_id: number;
  kind: string;
  config_present: boolean;
  note: string;
}

// Local-snapshot picker: every message-kind snapshot the owner holds.
export function fetchMessageKindSnapshots(secretHex: string): Promise<MessageKindSnapshot[]> {
  return call(secretHex, (c) => c.snapshotListMessageKind());
}

export function fetchRestoreHistory(secretHex: string): Promise<RestoreHistoryRow[]> {
  return call(secretHex, (c) => c.snapshotListRestoreHistory());
}

export function fetchRestoreDivergence(
  secretHex: string,
  snapshotId: number,
): Promise<RestoreDivergenceRow[]> {
  return call(secretHex, (c) => c.snapshotListRestoreDivergence(snapshotId));
}

export function restoreMessageKind(
  secretHex: string,
  snapshotId: number,
  confirmId: string,
): Promise<RestoreMessageKindResult> {
  return call(secretHex, (c) => c.snapshotRestoreMessageKind(snapshotId, confirmId));
}

// ── Backups: manage backup destinations (backups.md § Manage backup
// destinations). The cross-location destination CRUD — the web twin of the
// Linux views/backups/destinations.rs glue over the shared resolve + the
// add/edit/remove mutate helpers + the plane write (fauna.account.state.put). No
// new config WS-RPC kind — the destination set rests as `fauna.state.backup` rows.

export interface BackupDestination {
  destination_id: string;
  // Empty on a client-device row — a custodian has no address at all
  // (backups.md § Custodian contract, question 1).
  destination_nest_url: string;
  // null → the row label falls back to the destination domain.
  display_name?: string | null;
  // Which kind of destination holds these bytes (backups.md § Third destination
  // kind). "nest" on every pre-existing row (the serde default), "client-device"
  // for one of the owner's own devices, anything else for a kind a newer client
  // wrote. Feed it to `backupDestinationKindLabel` rather than string-matching
  // it — an unrecognised kind renders as itself.
  kind: string;
  // client-device rows only: the custodian device's stable sync device_id.
  // A "client-device" row without one is a row nothing can drive.
  custodian_device_id?: string | null;
  // client-device rows only: the user-set capacity cap in bytes. null =
  // uncapped, which is a real configuration ("fill the disk"), not a zero.
  capacity_cap_bytes?: number | null;
  // Whether the aftermath carried this row across a succession and the owner
  // has not yet kept or removed it — the `backup-destination-unattested-mark`
  // render gate (succession-aftermath.md § Adjudicating what the aftermath
  // carries across). Computed in shared Rust,
  // never re-derived here; deliberately
  // the same field name the trust plane's row uses.
  //
  // ⚠ Optional because only `backupDestinationList` resolves it — the mutation
  // faces return the stored rows without it, which is why every mutation below
  // re-reads through `loadDestinations()` rather than assigning their result.
  unattested?: boolean;
  // (destination_actor_pubkey / folder_name / added_at also ride
  // the wire, but the UI doesn't read them.)
}

// Every method resolves the new destination list, so the page re-renders rows.
export function backupDestinationList(secretHex: string): Promise<BackupDestination[]> {
  return call(secretHex, (c) => c.backupDestinationList(secretHex));
}

/** **Keep** one raised destination: "I recognise this". Clears that row's
 *  review mark and leaves the destination enrolled — Keep is not Remove, and the
 *  row stays removable forever after. Resolves whether anything changed; `false`
 *  (already answered, or already gone) is a legitimate outcome, not an error. */
export function backupDestinationKeep(
  secretHex: string,
  destinationId: string,
): Promise<boolean> {
  return call(secretHex, (c) => c.backupDestinationKeep(secretHex, destinationId));
}

export function backupDestinationAdd(
  secretHex: string,
  url: string,
  name: string,
): Promise<BackupDestination[]> {
  return call(secretHex, (c) => c.backupDestinationAdd(secretHex, url, name));
}

// ── Muted keywords (moderation.md § Muted keywords; content-moderation-and-
// ranking.md § Q3). The `muted-words` Settings sub-page CRUD over the sealed,
// nest-opaque `fauna.state.moderation` muted-keyword list — no new WS-RPC
// kind, the wasm twin of the FFI `muted_keywords_{list,set}`. Both calls return
// the page record the shared seam mints, so the page re-renders exactly what was
// saved (trim/drop-blank/case-insensitive-dedupe, first-seen spelling kept).

/** One muted keyword — mirrors `fauna_core::data::MutedKeyword`: the term
 *  and the per-mille weight a match contributes, in `[-1000, 0]` (−1000, the
 *  default, sinks and collapses; a softer weight only demotes in a ranked feed —
 *  `content-moderation-and-ranking.md` § Composition). */
export interface MutedKeyword {
  keyword: string;
  weight: number;
}

/** The `muted-words` page in one record — mirrors
 *  `fauna_client_config::MutedWordsSnapshot` (serde JSON, snake_case). */
export interface MutedWordsSnapshot {
  /** The normalized, persisted entries — each `keyword` is a `muted-word-item`
   *  row; the whole list is what `matchesMutedKeywords` reads. */
  keywords: MutedKeyword[];
  /** Whether a read has **returned successfully**, i.e. whether `keywords` is
   *  an answer rather than an absence. The second painting condition of
   *  `muted-word-empty` (`docs/goal/ui/README.md` § *List pages: loading is not
   *  empty*): paint it only on `loaded && keywords.length === 0`, never on the
   *  length alone — a list is empty both before the first read returns and after
   *  one that found nothing. */
  loaded: boolean;
}

export function mutedKeywordsList(secretHex: string): Promise<MutedWordsSnapshot> {
  return call(secretHex, (c) => c.loadMutedWords());
}

/** Whole-list intents only — the page's add/remove buttons go through
 *  `mutedKeywordsAdd`/`mutedKeywordsRemove` below; sending the page's list
 *  wholesale clobbers a term another device stored since the page loaded
 * . */
export function mutedKeywordsSet(
  secretHex: string,
  keywords: MutedKeyword[],
): Promise<MutedWordsSnapshot> {
  return call(secretHex, (c) => c.saveMutedWords(keywords));
}

/** Add one term as a DELTA against the stored list — the shared seam re-reads
 *  it inside its own CAS update, so a concurrent device's term survives.
 *  Re-adding an existing term is a no-op; the stored normalization returns. */
export function mutedKeywordsAdd(secretHex: string, word: string): Promise<MutedWordsSnapshot> {
  return call(secretHex, (c) => c.addMutedWord(word));
}

/** Remove one term — `mutedKeywordsAdd`'s inverse; removing a term already
 *  gone is a success no-op (convergence, not an error). */
export function mutedKeywordsRemove(secretHex: string, word: string): Promise<MutedWordsSnapshot> {
  return call(secretHex, (c) => c.removeMutedWord(word));
}

// ── Trained topic factors (topic-factors.md § Authoring surface & picker).
// The Trained-topics facet's four gestures, each a wrapper over the SHARED
// `fauna_client_personalization::TrainedTopics` lifecycle — the registry (sealed
// in `fauna.state.personalization`) and the model plane
// (`fauna.personalization.model.*`) are driven together there, not here: a
// delete pairs the registry removal with a `model.delete`, and the create cap is
// enforced client-side. Every mutator returns the fresh row list, so the page
// re-renders exactly what was persisted.
//
// `id` is hex (the SPA writes it into the row's `data-factor` attribute — the
// e2e's only way to learn a freshly-minted key, since the registry is sealed).

/** One Trained-topics row: registry entry + its advisory nest-side example count. */
export interface TrainedTopicRow {
  /** The factor's 16-byte registry id, hex. Stable across renames. */
  id: string;
  /** The user's chosen display name. Sealed — the nest never sees it. */
  name: string;
  /** The composition key (`topic:<hex>`) the picker offers; null for a corrupt id. */
  factor_key: string | null;
  /** Explicit examples trained into this factor (0 = never trained). */
  example_count: number;
  /** The Layer-A opt-in ("Learn from my activity"): when on, the feed's
   *  watch/skip engagement cues weak-train this factor (engagement-cues.md
   *  § Layer A). Default off, per-factor. */
  learn_from_engagement: boolean;
}

/**
 * A rejected trained-topic gesture. `code` is machine-readable precisely so the
 * SPA localizes — the wasm layer must not ship pre-formatted English into a
 * translated UI. `cap` carries the limit in `max`.
 */
export interface TrainedTopicsError {
  code: 'cap' | 'blank_name' | 'config' | 'model';
  max: number | null;
  message: string;
}

export function trainedTopicsList(secretHex: string): Promise<TrainedTopicRow[]> {
  return call(secretHex, (c) => c.listTrainedTopics());
}

export function trainedTopicsCreate(secretHex: string, name: string): Promise<TrainedTopicRow[]> {
  return call(secretHex, (c) => c.createTrainedTopic(name));
}

export function trainedTopicsRename(
  secretHex: string,
  id: string,
  name: string,
): Promise<TrainedTopicRow[]> {
  return call(secretHex, (c) => c.renameTrainedTopic(id, name));
}

export function trainedTopicsDelete(secretHex: string, id: string): Promise<TrainedTopicRow[]> {
  return call(secretHex, (c) => c.deleteTrainedTopic(id));
}

/** Flip a trained topic's Layer-A opt-in (`learn_from_engagement`). Registry-only
 *  — turning it off stops future weak training without rewriting what
 *  engagement already taught. Returns the fresh row list. */
export function trainedTopicsSetLearnFromEngagement(
  secretHex: string,
  id: string,
  on: boolean,
): Promise<TrainedTopicRow[]> {
  return call(secretHex, (c) => c.setTrainedTopicEngagement(id, on));
}

/** One kept exemplar handed back from the publish review-prune sheet. Never
 *  the preview text — a published List is `content_id → score` and nothing
 *  else. */
export interface PublishListEntry {
  post_id: string;
  score: number;
}

/** A landed publish (`topic-factors.md` § Publishing a trained factor). */
export interface PublishedList {
  labeler_id: string;
  version: number;
  entry_count: number;
}

/**
 * A rejected publish. `code` is machine-readable for the two cases a user can
 * cause by typing (`blank_name`, `name_too_long` — `max` carries the name
 * bound); every other code falls back to the pre-formatted `message`, mirroring
 * linux's `localize()` (`PublishListError::to_string()`).
 */
export interface PublishListError {
  code: 'blank_name' | 'name_too_long' | 'transport' | 'artifact';
  max: number | null;
  message: string;
}

/**
 * Publish a trained factor's kept exemplars as a tier-3 List labeler. A
 * wrapper over the SHARED `fauna_client_personalization::publish::
 * publish_trained_factor_list` — the whole lifecycle (derive the per-factor
 * keypair, resolve the next version, build + sign, `fauna.labelers.publish`)
 * lives there, not here. `id` is the factor's 16-byte registry id, hex.
 */
export function publishTrainedFactorList(
  secretHex: string,
  id: string,
  name: string,
  entries: PublishListEntry[],
): Promise<PublishedList> {
  return call(secretHex, (c) => c.publishTrainedFactorList(secretHex, id, name, entries));
}

/** One n-gram the Model review sheet hands back — the rows off
 *  `scrubCorpusForFactor`, in whatever order the sheet rendered them. */
export interface PublishNgram {
  ngram: string;
  more: number;
  less: number;
}

/** A landed Model publish (`topic-factors.md` § Publishing a trained factor,
 *  v2) — the `PublishedList` twin. */
export interface PublishedModel {
  labeler_id: string;
  version: number;
  ngram_count: number;
  document_count: number;
}

/** The Model lifecycle's error twin — same shape and passthrough rule as
 *  {@link PublishListError}, plus the `empty_vocabulary` code the scrub's
 *  3-post privacy floor can produce (no List equivalent — a List's corpus is
 *  never itself pruned to empty by the shared lifecycle). */
export interface PublishModelError {
  code: 'empty_vocabulary' | 'blank_name' | 'name_too_long' | 'transport' | 'artifact';
  max: number | null;
  message: string;
}

/**
 * Publish a trained factor's scrubbed vocabulary as a tier-3 `text-model`
 * labeler. A wrapper over the SHARED `fauna_client_personalization::publish::
 * publish_trained_factor_model` — the whole lifecycle lives there, not here.
 * `moreDocs`/`lessDocs` are `scrubCorpusForFactor`'s corpus counters, passed
 * through UNSHRUNK by the prune (the shared lifecycle's documented rule — an
 * SPA must not recount from the kept rows).
 */
export function publishTrainedFactorModel(
  secretHex: string,
  id: string,
  name: string,
  moreDocs: number,
  lessDocs: number,
  ngrams: PublishNgram[],
): Promise<PublishedModel> {
  return call(secretHex, (c) =>
    c.publishTrainedFactorModel(secretHex, id, name, moreDocs, lessDocs, ngrams),
  );
}

export function backupDestinationEdit(
  secretHex: string,
  id: string,
  url: string,
  name: string,
): Promise<BackupDestination[]> {
  return call(secretHex, (c) => c.backupDestinationEdit(secretHex, id, url, name));
}

export function backupDestinationRemove(
  secretHex: string,
  id: string,
): Promise<BackupDestination[]> {
  return call(secretHex, (c) => c.backupDestinationRemove(secretHex, id));
}

// Per-destination backup status (backups.md § Per-destination status read) —
// one row of the NEST's fauna.backup.status projection, the same read all 7
// apps make since the slice-4 leg (d) repoint (2026-07-24). The wire shape is
// unchanged from the retired source-side computation, so this interface is too.
// `last_upload_time` is a real timestamp on web now: the nest's own in-process
// coordinator uploads for web-only users, which the old degenerate wasm read
// (always null, whole-source backlog) could never report. Keyed by
// destination_id.
export interface BackupDestinationStatus {
  destination_id: string;
  last_upload_time: number | null;
  backlog_count: number;
  /** Client-device rows only (`backup-destination-usage`): bytes the custodian
   *  reported holding at its last check-in. `null` on a nest row **and** on a
   *  custodian that has never checked in — which the shared label renders as
   *  *nothing held yet*, not *0 bytes held*. */
  held_bytes?: number | null;
  /** Client-device rows only: the nest's `CAP_STATE_OK` / `CAP_STATE_REACHED`.
   *
   *  ⚠ Pass it through to `backupUsageText` untouched. Cap-reached is **read,
   *  never inferred**: a pull pass that stops at its cap ends *below* the cap,
   *  so re-deriving the verdict from `held >= cap` renders a silently-stopped
   *  backup as healthy-with-room. That is the whole reason this field is on the
   *  wire at all instead of being computed from the other two. */
  cap_state?: string | null;
  /** Client-device rows only: the custodian's own last self-audit verdict
   *  (`AUDIT_STATE_OK` / `AUDIT_STATE_FAILED`), read through the shared
   *  `backupSelfAuditIsAlerting`. `null` on a nest row **and** on a custodian
   *  that has not audited yet — that absence is deliberately not a failure. */
  audit_state?: string | null;
  /** Client-device rows only: unix seconds of the custodian's last **passed**
   *  self-audit. Read **with** `audit_state`, never instead of it — a failure
   *  leaves this stamp at the previous pass. */
  last_audit_passed_at?: number | null;
}

export function backupDestinationStatus(
  secretHex: string,
): Promise<BackupDestinationStatus[]> {
  return call(secretHex, (c) => c.backupDestinationStatus(secretHex));
}

// ── the client-side backup audit (backups.md § Audit-alert surface) ──────────
// One row per configured destination, from the shared
// `fauna_client_backup::audit::run_audit_pass`. Web implements no audit logic: no
// merge, no debounce, no verdict — it supplies three seams (destination connector,
// inclusion source, localStorage state store) and renders what comes back.

/** One destination's audit picture: the row timestamp and the standing banner. */
export interface DestinationAuditRow {
  destination_id: string;
  /** Unix seconds of the last **passed** audit; `null` = never passed, which
   *  renders "never" and is deliberately not an alert (a destination enrolled ten
   *  minutes ago has never passed one and is perfectly healthy). */
  last_passed_at: number | null;
  /** Present ⇒ render a `backup-audit-alert` banner for this destination; absent ⇒
   *  don't. **Opaque** — it goes straight into `backupAuditAlertText` for its text.
   *  The SPA never inspects it: which verdicts are loud is
   *  `DestinationAuditRecord::alert_reason`'s single shared answer, and
   *  `AuditVerdict::is_alerting` is itself defined through it, so a fourth alerting
   *  verdict cannot appear without also gaining a banner. */
  alert_reason: BackupAuditAlertReason | null;
  /** Every banner reason standing at the pass's `now` — the page's single
   *  `backup-audit-alert` answer (`DestinationAuditRecord::alert_reasons`): the
   *  standing verdict's reason, then at most one `SourceRegressed`. Paint each
   *  entry; each is **opaque**, straight into `backupAuditAlertText`. Read it
   *  through `auditAlertReasons` (`$lib/backup-audit-alerts`). */
  alert_reasons: BackupAuditAlertReason[];
}

/**
 * Run one audit pass and return the full per-destination picture.
 *
 * The pass opens this client's **own** authenticated session to each destination —
 * never a read routed through the source nest — samples records from the
 * destination's public blob routes, and opens them under the owner's derived
 * `NestBackupKey`. It is debounced to `AUDIT_MIN_INTERVAL` (24 h) per destination
 * inside shared Rust, so calling it on every page mount is cheap: a repeat visit
 * costs one `localStorage` read and no round trip.
 *
 * The returned vector always covers **every** configured destination, including
 * ones the debounce skipped (`merge_outcomes` folds the pass over the persisted
 * records) — so rendering it directly cannot blank a standing alert.
 */
export function backupAuditRunPass(secretHex: string): Promise<DestinationAuditRow[]> {
  return call(secretHex, (c) => c.backupAuditRunPass(secretHex));
}

// ── deployment-seed custody leg (box-recovery.md § The plane-era recovery
//    floor, (c) The writes) ───────────────────────────────────────────────
// The web twin of the native hosts' `run_custody_leg`. The ONLY capture: the
// claim reply's seed is no longer consumed, and nothing is fanned out — the
// account plane carries the custody map. Fired at the universal post-auth
// point (`+layout.svelte`) on every connect; the leg runs at whichever of that
// edge and the account runtime's store-ready edge (`startAccountRuntime`)
// lands second, and at every later post-auth edge. The seed is fetched and
// merged inside Rust — it never crosses into JS.

/** What a run of the custody leg's post-auth edge settled as (for the
 *  console ring). `store_not_ready`: the account store is not up yet — the
 *  store-ready edge runs the leg instead. */
export type DeploymentSeedCustodyOutcome =
  | 'already_custodied'
  | 'captured'
  | 'not_admin'
  | 'handoff_unavailable'
  | 'nest_holds_no_seed'
  | 'refused_mismatch'
  | 'store_refused'
  | 'bound_unresolved'
  | 'store_not_ready';

/** The warning a run owes the admin when custody ends unconfirmed:
 *  `mismatch` (the box handed off a seed that does not derive to its bound
 *  identity) or `failed` (anything else unconfirmed — retried at the next
 *  edge). */
export type DeploymentSeedCustodyWarning = 'mismatch' | 'failed';

export function selfHealDeploymentSeedCustody(
  secretHex: string,
  onWarning: (warning: DeploymentSeedCustodyWarning) => void,
): Promise<DeploymentSeedCustodyOutcome> {
  return call(secretHex, (c) =>
    c.selfHealDeploymentSeedCustody(secretHex, onWarning),
  ) as Promise<DeploymentSeedCustodyOutcome>;
}

// ── mail content-sealing epoch schedule refresh ────────────────────────
// The web twin of linux `FaunaClient::refresh_mail_epoch_schedule`
// (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
// *Content-sealing epochs*). Fired at the same universal post-auth point as
// `selfHealDeploymentSeedCustody`: slides the published epoch-seal-key horizon forward over
// the shared, idempotent `MailSettingsMachine::refresh_epoch_schedule` — a
// no-op when mail isn't enabled (no MSEK). Best-effort — the caller should
// catch and log, never surface to the user.
export function refreshMailEpochSchedule(secretHex: string): Promise<void> {
  return mailSettingsMachine(secretHex).then((m) => m.refreshEpochSchedule());
}

// ── fauna.dns.set_host_address (client host-address reporting) ────────
// The admin client reports the nest's PUBLIC IP so the nest gates ACME HTTP-01
// on the strong resolve-check + assembles the apex/`mail.` records. The web twin
// of the native `report_host_address` FFI / linux's direct drive. Called at the
// universal post-auth hook (admin-gated) — fire-and-forget, idempotent nest-side.
// `domains-and-tls-bootstrap.md` § Host-address acquisition.

/** The `{ kind, nest_ipv4?, error? }` the wasm `reportHostAddress` resolves.
 * `reported` = the nest now knows its public IPv4; `skipped_no_public_ip` = a LAN
 * box with no reflector, or a name that would not resolve to a global address (the
 * spec'd safe floor — normal, no alarm); `failed` = the RPC was refused/dropped
 * (retried on the next connect). Never publishes a private/LAN address (the safety
 * invariant lives in the shared Rust decision fn). */
export type HostAddressReport = {
  kind: 'reported' | 'skipped_no_public_ip' | 'failed';
  nest_ipv4?: string;
  error?: string;
};

export function reportHostAddress(secretHex: string): Promise<HostAddressReport> {
  return call(secretHex, (c) =>
    c.reportHostAddress(),
  ) as Promise<HostAddressReport>;
}

// ── critical-alert re-sweep loop ─────────────────────────────────────────
// The web twin of tui `critical_alerts::spawn_session_start_sweep` /
// linux `FaunaClient::run_critical_alert_sweep`. Fired at the same universal
// post-auth point as `selfHealDeploymentSeedCustody`: runs the feeders that have no page of
// their own (`docs/goal/behavior/critical-alerts.md` § Mechanism → *Who runs
// the detector*), immediately and then every `RE_SWEEP_INTERVAL_SECS` for as
// long as the identity lives. The returned promise does not resolve under
// normal operation (only on sign-out) — callers MUST stay fire-and-forget,
// never `await` this.
export function runCriticalAlertSweep(secretHex: string): Promise<void> {
  return call(secretHex, (c) => c.runCriticalAlertSweep(secretHex)) as Promise<void>;
}

// ── S8 seal-backfill sweep ───────────────────────────────────────────────
// The web twin of the sync daemon's / windows' / apple's / android's
// session-start sweep, fired at the same universal post-auth point as
// `runCriticalAlertSweep`: D1 stamps any folder row whose sealed siblings a
// prior session left missing, then D3 stamps each owned set's snapshot tags
// (`docs/goal/behavior/path-sealing.md` § Implementation status today). The
// sequencing, the owner-only skip and the fail-closed custody are all shared
// Rust — there is deliberately no loop on this side, which is what kept web
// from becoming a fifth hand-written copy of the policy.
//
// Single pass (unlike the alert sweep's loop): the promise resolves when the
// pass is done. Best-effort — it rejects only on a malformed secret.
export function runSealBackfillSweep(secretHex: string): Promise<void> {
  return call(secretHex, (c) => c.runSealBackfillSweep(secretHex)) as Promise<void>;
}

/** The `recover-selfhosted-command` line for the box-recovery step-4 self-hosted
 * install page (`box-recovery.md` § Recovery UI (step 4)) — renders
 * `FAUNA_DEPLOYMENT_SEED=<64-hex>` from the selected box's custodied seed. The
 * seed is resolved **in Rust** from this device's own account store joined with
 * a cold read from the reachable nest (box-recovery.md § The plane-era recovery
 * floor, (b); keyed by `nestActorIdHex`, one of the ids `deploymentSeeds()`
 * listed) and rendered via the single shared render, so web + native emit a
 * byte-identical command. Unlike the box list, the seed IS surfaced here by
 * design — it is the installer input the admin pastes (box-recovery.md § Trust &
 * audience). Rejects if the nest cannot be reached (the caller then falls back to
 * `$lib/wasm`'s `recoverSelfhostedCommandLocal`) or no source custodies that
 * box. */
export function recoverSelfhostedCommand(
  secretHex: string,
  nestActorIdHex: string,
): Promise<string> {
  return call(secretHex, (c) =>
    c.recoverSelfhostedCommand(secretHex, nestActorIdHex),
  ) as Promise<string>;
}

/** One custodied box the recovery UI can enumerate (box-recovery.md § Recovery UI
 * (step 4)): the public `nestActorId` (64-hex) + the box's own handle domain for
 * the `recover-box-item` label (§ Trust & audience — `null` for a domainless
 * home-relay box, so the row falls back to the short id). The custodied **seed**
 * never crosses into JS — only these non-secret fields do; the re-provision drive
 * resolves the seed back in Rust by `nestActorId`. */
export type DeploymentSeedBox = {
  nestActorId: string;
  domain: string | null;
};

/** The box list for box-recovery step 4 when a nest client exists — this
 * device's own account store JOINED with a cold read from the connected nest
 * (box-recovery.md § The plane-era recovery floor, (b) The reads: never
 * either-or, because a surviving device's stored nest may be the dead box).
 * Superseded boxes excluded. Empty when no box is custodied; **rejects** when no
 * nest is reachable at all — callers gate on a stored nest URL
 * (`storedNestUrlOrNull`) and fall back to `$lib/wasm`'s local-only
 * `recoveryBoxesLocal`. The list is public ids; the seed never crosses into JS. */
export async function deploymentSeeds(secretHex: string): Promise<DeploymentSeedBox[]> {
  const raw = (await call(secretHex, (c) => c.deploymentSeeds(secretHex))) as Array<{
    nest_actor_id: string;
    domain: string | null;
  }>;
  return raw.map((e) => ({ nestActorId: e.nest_actor_id, domain: e.domain ?? null }));
}

// ── fauna.subscriptions.* (profile Tiers-tab SELF author management) ──
//
// The thin TS seam over the wasm `WsRpcClient.subscriptions*` methods
// (libs/fauna-wasm/src/rpc.rs), themselves over the shared
// `fauna-client-subscriptions` `SubscriptionsClient` (thin kinds) +
// `SubscriptionsAuthor` (encrypted-mode mint+upload orchestration). The
// crypto-bearing actions (createTier / approve / removeSubscriber) compose the
// `EncryptedKeyBlobUpload` envelope Rust-side, so this seam stays a pass-through
// (priority #2; monetization.md § Pillar 1). 32-byte ids ride as hex strings.

/** One of the author's own subscription tiers (§1 My tiers). */
export interface SubscriptionTier {
  name: string;
  rank: number;
  description: string | null;
  price_hint: string | null;
  /** The machine-comparable price in sats, the reverse of the create/update
   *  sats conversion (monetization.md § The asking price) — independent of
   *  `price_hint`. `null` when the tier is not for sale to an inferring
   *  mechanism, or a unit this build cannot interpret (fail-closed). */
  asking_price_sats: number | null;
  payment_url: string | null;
  auto_approve: boolean;
  /** Set when this is a per-post pay-to-unlock tier auto-minted by "Sell this
   *  post…" (monetization.md § Per-post pay-to-unlock) — the designated post's
   *  id. The §1 My-tiers management list excludes rows where this is non-null
   *  (`tier_item_to_web_json` carries it for exactly that purpose); §3/§4/§5
   *  tier pickers do NOT filter it — a designated tier still needs a claim
   *  minted / subscribers viewed against it. */
  unlocks_post: string | null;
  /** Hidden from every offer surface (monetization.md § The unifying model —
   *  A tier may be hidden): the reserved owner-only tier the archive import
   *  mints. Present on the §1 own read only; false on every ordinary tier. */
  hidden: boolean;
}

/** A pending subscribe/unsubscribe request (§2 Pending requests). */
/** One pending §2 request. Hand the whole row back to {@link subscriptionsApprove}
 *  — it is the wasm `WebPendingRequest`, a total map of the wire `PendingRequest`,
 *  and the approve mint reads fields the UI never renders (`kind`,
 *  `mlkem_encaps_key`). Don't rebuild it from parts. */
export interface SubscriptionRequest {
  request_id: number;
  subscriber_id: string; // hex
  tier_name: string;
  kind: string; // "subscribe" | "unsubscribe"
  /** Microseconds since the Unix epoch. */
  created_at: number;
  /** The subscriber's ML-KEM-768 encapsulation key (hex), or null if they
   *  published none — the author then seals classical for them. */
  mlkem_encaps_key: string | null;
  /** The payment engine entitled this request (a verified provider webhook or
   *  a redeemed claim code) rather than it being a plain manual subscribe —
   *  drives the §2 paid badge (monetization.md § Pillar 3). */
  payment_entitled: boolean;
}

/** One confirmed subscriber of the selected tier (§3 Subscribers). */
export interface SubscriptionSubscriber {
  subscriber_id: string; // hex
}

/** §1 — the calling author's own tier definitions (`tiers.list`). */
export function subscriptionsTiersList(secretHex: string): Promise<SubscriptionTier[]> {
  return call(secretHex, (c) => c.subscriptionsTiersList() as Promise<SubscriptionTier[]>);
}

/** §1 — create a tier (`SubscriptionsAuthor::create_tier`: custody + create). */
export function subscriptionsCreateTier(
  secretHex: string,
  name: string,
  rank: number,
  description: string | null,
  priceHint: string | null,
  paymentUrl: string | null,
  autoApprove: boolean,
  askingPriceSats: number | null = null,
): Promise<void> {
  return call(secretHex, (c) =>
    c.subscriptionsCreateTier(
      secretHex,
      name,
      rank,
      description ?? undefined,
      priceHint ?? undefined,
      paymentUrl ?? undefined,
      autoApprove,
      askingPriceSats ?? undefined,
    ) as Promise<void>,
  );
}

/** §1 — overwrite a tier's mutable fields (`tiers.update`; thin, name is the key).
 *  `askingPriceSats` is `null`/omitted to KEEP the tier's current price — this
 *  kind has no clear verb, like every other optional field on it. */
export function subscriptionsUpdateTier(
  secretHex: string,
  name: string,
  rank: number,
  description: string | null,
  priceHint: string | null,
  paymentUrl: string | null,
  autoApprove: boolean,
  askingPriceSats: number | null = null,
): Promise<void> {
  return call(secretHex, (c) =>
    c.subscriptionsUpdateTier(
      name,
      rank,
      description ?? undefined,
      priceHint ?? undefined,
      paymentUrl ?? undefined,
      autoApprove,
      askingPriceSats ?? undefined,
    ) as Promise<void>,
  );
}

/** §1 — delete a tier by name (`tiers.delete`; thin, idempotent). */
export function subscriptionsDeleteTier(secretHex: string, name: string): Promise<void> {
  return call(secretHex, (c) => c.subscriptionsDeleteTier(name) as Promise<void>);
}

/** §2 — the author's pending requests (`requests.list`). */
export function subscriptionsRequestsList(secretHex: string): Promise<SubscriptionRequest[]> {
  return call(secretHex, (c) => c.subscriptionsRequestsList() as Promise<SubscriptionRequest[]>);
}

/** §2 — approve a request (`SubscriptionsAuthor::approve_subscriber`: mint+upload).
 *  Takes the whole row from {@link subscriptionsRequestsList}, verbatim — the same
 *  contract as the native `subscriptions_approve_subscriber(…, FfiPendingRequest)`. */
export function subscriptionsApprove(
  secretHex: string,
  request: SubscriptionRequest,
): Promise<void> {
  return call(secretHex, (c) => c.subscriptionsApprove(secretHex, request) as Promise<void>);
}

/** §2 — reject a request by id (`requests.reject`; thin, idempotent). */
export function subscriptionsReject(secretHex: string, requestId: number): Promise<void> {
  return call(secretHex, (c) => c.subscriptionsReject(requestId) as Promise<void>);
}

/** §3 — the confirmed roster of `tierName` (`subscribers.list`). */
export function subscriptionsSubscribersList(
  secretHex: string,
  tierName: string,
): Promise<SubscriptionSubscriber[]> {
  return call(
    secretHex,
    (c) => c.subscriptionsSubscribersList(tierName) as Promise<SubscriptionSubscriber[]>,
  );
}

/** §3 — remove a subscriber (`SubscriptionsAuthor::remove_subscriber`: rotate+mint+upload). */
export function subscriptionsRemoveSubscriber(
  secretHex: string,
  tierName: string,
  subscriberIdHex: string,
): Promise<void> {
  return call(secretHex, (c) =>
    c.subscriptionsRemoveSubscriber(secretHex, tierName, subscriberIdHex) as Promise<void>,
  );
}

// ── author auto-approve loop (encrypted-mode frictionless follow) ──
//
// The two on-connect author passes the web pump drives (`subscriptionsAuthor.ts`),
// the browser twin of linux `subscriptions_author.rs` + windows `SubscriptionsAuthorPump`.
// In encrypted mode the nest can't mint the KeyBlob, so a follow *enqueues* (`Queued`)
// and the author's own client must drain it (monetization.md § grant path 2 + § Pillar 1).

/** Auto-approve every queued `auto_approve` subscribe request (mint+upload the
 *  covering KeyBlob) — `SubscriptionsAuthor::drain_auto_approvals`. Resolves to
 *  the number approved this pass. What makes an encrypted-mode follow frictionless. */
export function subscriptionsDrainAutoApprovals(secretHex: string): Promise<number> {
  return call(secretHex, (c) => c.subscriptionsDrainAutoApprovals(secretHex) as Promise<number>);
}

/** Re-drive any crash-staged subscriber-removal — `SubscriptionsAuthor::resume_pending_removals`.
 *  Run in the same on-connect pass as the drain so the rotate-on-removal forward-secrecy
 *  guarantee completes once the author is online. Resolves to the count resumed. */
export function subscriptionsResumePendingRemovals(secretHex: string): Promise<number> {
  return call(secretHex, (c) => c.subscriptionsResumePendingRemovals(secretHex) as Promise<number>);
}

/** One author-pump tick — `SubscriptionsAuthor::reconcile_once`, the shared
 *  resume-then-drain sequencing tui/linux/android/windows already call. The
 *  web pump (`subscriptionsAuthor.ts`) schedules this single call per tick
 *  instead of re-deriving the two-step order itself (monetization.md § Pillar 1
 *  → *Where the logic lives*: "An app MUST NOT re-derive either"). Best-effort
 *  by construction — never rejects; either half's failure comes back as a
 *  string for the pump to log. */
export interface ReconcilePass {
  resumed: number;
  approved: number;
  resume_error: string | null;
  drain_error: string | null;
}

export function subscriptionsReconcileOnce(secretHex: string): Promise<ReconcilePass> {
  return call(secretHex, (c) => c.subscriptionsReconcileOnce(secretHex) as Promise<ReconcilePass>);
}

// ── consumer side (subscription-settings page, Slice B) ──

/** One of this user's own subscriptions across creators (the consumer page). */
export interface SubscriptionMine {
  author_id: string; // hex
  tier: string;
  status: string; // "active" | "pending"
  handle: string | null; // creator handle if the nest resolved one, else null
  /**
   * The `subscription-mine-author` label: `handle` when non-blank, else the full
   * hex `author_id`. Pre-computed by the shared chooser in the wasm transcribe —
   * render verbatim, never re-derive the fallback here.
   * See `docs/goal/behavior/value-formatting.md` § Subscription author label.
   */
  author_display: string;
}

/** The calling actor's subscriptions across every creator (`mine.list`). */
export function subscriptionsMineList(secretHex: string): Promise<SubscriptionMine[]> {
  return call(secretHex, (c) => c.subscriptionsMineList() as Promise<SubscriptionMine[]>);
}

/** Drop this user's subscription to `authorIdHex` (`unsubscribe`; encrypted mode queues). */
export function subscriptionsUnsubscribe(secretHex: string, authorIdHex: string): Promise<void> {
  return call(secretHex, (c) => c.subscriptionsUnsubscribe(authorIdHex) as Promise<void>);
}

// ── OTHER profile (subscriber browse): offers + subscribe + status ──
//
// The Tiers tab when viewing another actor's profile (profile.md § Layout & flow
// → Another's profile; monetization.md § Pillar 1 — surface 2). The CALLER is the
// viewer (`secretHex` = the viewer's secret → their authed WS-RPC connection);
// `authorIdHex` is the creator being browsed. All thin reads/writes — no
// author-side mint orchestration (that is the SELF Tiers tab's job).

/** Outcome of a subscribe/follow request — drives the offer-row status flip. */
export type SubscriptionSubscribeOutcome =
  | { outcome: 'approved'; tier: string }
  | { outcome: 'queued'; request_id: number };

/** The viewer's current subscription status for a creator (`status.get`). */
export interface SubscriptionStatus {
  tier: string | null; // the active tier, or null when not subscribed
  auto_approve: boolean;
}

/** Browse another creator's offered tiers (`offers.list`); same shape as a SELF tier. */
export function subscriptionsOffersList(
  secretHex: string,
  authorIdHex: string,
): Promise<SubscriptionTier[]> {
  return call(
    secretHex,
    (c) => c.subscriptionsOffersList(authorIdHex) as Promise<SubscriptionTier[]>,
  );
}

/**
 * Subscribe the viewer to `tier` from `authorIdHex` (also follow via
 * tier="followers"), **publishing** the viewer's identity-seed ML-KEM ek
 * (surface B, S4b) unconditionally (no capability token), so the author can
 * later wrap hybrid `KeyBlob`s to this subscriber. The
 * web twin of `fauna-ffi`'s `subscriptions_subscribe_publishing_ek`; mirrors linux
 * `offers.rs::subscribe_to` / `mod.rs::follow`.
 */
export function subscriptionsSubscribePublishingEk(
  secretHex: string,
  authorIdHex: string,
  tier: string,
): Promise<SubscriptionSubscribeOutcome> {
  return call(
    secretHex,
    (c) =>
      c.subscriptionsSubscribePublishingEk(
        secretHex,
        authorIdHex,
        tier,
      ) as Promise<SubscriptionSubscribeOutcome>,
  );
}

/** The viewer's current status for `authorIdHex` (`status.get`; tier=null when none). */
export function subscriptionsStatusGet(
  secretHex: string,
  authorIdHex: string,
): Promise<SubscriptionStatus> {
  return call(
    secretHex,
    (c) => c.subscriptionsStatusGet(authorIdHex) as Promise<SubscriptionStatus>,
  );
}

// ── fauna.payments.* — MOVED to `$lib/payments` (2026-08-16) ────────────────
//
// The Pillar-3 client seam lived here until the web `payments` excision leg.
// It is not here any more because THIS module is unconditionally in the
// bundle: a `payments*` function defined here ships its own name into the
// store-safe artifact even when every caller is folded away by
// `__FAUNA_PAYMENTS__` (`dynamic-features.md` § Platform-family surface
// excision — the isolated-module pattern; `just web-store-safe-check` greps
// exactly those names). It reaches the client through `rpcCall` above.
// Add a new payments call to `$lib/payments`, never back here.

// ── fauna.family.* (family-safety.md § App surface) ──────────────
//
// The Family surface + the global supervised indicator, over the wasm
// `WsRpcClient.family*` passthroughs (libs/fauna-wasm/src/rpc.rs), themselves
// over the shared `fauna-client-family` `FamilyClient` — the browser twin of
// the native `FfiFamilyClient`. Thin seam: no composition here.
//
// ONE `familyStatus()` read answers both roles (guardian + supervised), and the
// approvals queue is NOT ward-scoped — `familyApprovalsList()` returns every
// ward's entries. Actor ids ride as raw bytes (`$lib/hex` normalizes the two
// serde_wasm_bindgen wire shapes); policies ride as the wire `ReachPolicy`.

/** The ward's reach-policy document (`fauna_protocol::family::ReachPolicy`).
 *  Every default is the unsupervised-equivalent value, so a fresh link changes
 *  nothing until the guardian tightens it. `unknown_sender_mail` is
 *  `allow | hold | reject`; `feed_sources` is `allow | block` (wire values — the
 *  page maps them to/from the localized `t.family.value_*` labels it renders). */
export interface ReachPolicy {
  contact_approval: boolean;
  unknown_sender_mail: string;
  federation_contact: boolean;
  feed_sources: string;
  /** v1.x content pillar — a per-category guardian floor (`inherit | collapse |
   *  block` wire values); absent means the all-inherit unsupervised-equivalent
   *  default (`family-safety.md` § Content policy). */
  content_policy?: {
    nsfw: string;
    spam: string;
    phishing: string;
    commercial: string;
  };
  /** v1.x Guardian Notify — category+count doorbell; absent/false is off. */
  content_notify?: boolean;
  /** v1.x screen-time pillar (`family-safety.md` § Screen time) — the usage
   *  window's two bounds as minutes from the ward's local midnight (wrapping
   *  allowed, so `window_start > window_end` is legal) plus a cross-device
   *  daily budget in minutes. Every field absent/null is the
   *  unsupervised-equivalent default: that control is unset and never locks.
   *  Absent on `policy.update` means "leave the pillar unchanged". */
  screen_time?: {
    window_start?: number | null;
    window_end?: number | null;
    daily_minutes?: number | null;
  };
  /** v1.x bridge-DM gate — `allow | hold` wire value (§ The bridge-DM gate:
   *  deliberately no `reject` arm); absent means "leave unchanged" on
   *  `policy.update`, and renders the `allow` default (never the fail-closed
   *  `hold`) — the one reach knob where absence is not "unparseable". Rendered
   *  by tui (2026-08-14) and web (2026-08-22); the remaining apps lift it. */
  unknown_peer_dm?: string;
}

/** Who supervises this account (`supervised_by` — absent for a full account). */
export interface FamilyGuardianInfo {
  actor_id: Uint8Array | number[];
  handle: string;
}

/** The outstanding transfer proposal for a ward, as the INITIATING side sees
 *  it (family-safety.md § Graduation & transfer) — pending until the proposed
 *  guardian accepts. Additive: a reply without it reads as no pending proposal. */
export interface FamilyPendingTransferInfo {
  proposed_guardian_actor_id: Uint8Array | number[];
  proposed_guardian_handle: string;
  created_at: number;
}

/** A transfer proposal awaiting THIS caller's consent as proposed guardian
 *  (`family-incoming-transfer-item`). `guardian_handle` is the ward's CURRENT
 *  guardian (the initiator may have been the admin). */
export interface FamilyIncomingTransferInfo {
  supervised_actor_id: Uint8Array | number[];
  supervised_handle: string;
  guardian_handle: string;
  created_at: number;
}

/** One coarse per-category Guardian Notify count on a ward
 *  (`family-ward-content-notices`, family-safety.md § Guardian Notify).
 *  `category` is `nsfw | spam | phishing | commercial`; `count` carries **no
 *  content identifier** — the whole point of Notify. */
export interface FamilyContentNotice {
  category: string;
  count: number;
}

/** A supervised account this caller guards (`family-ward-item`). */
export interface FamilyWardInfo {
  actor_id: Uint8Array | number[];
  handle: string;
  policy: ReachPolicy;
  /** Pending transfer proposal, if any (additive — a reply without it reads as none). */
  pending_transfer?: FamilyPendingTransferInfo | null;
  /** v1.x Guardian Notify — the ward's coarse per-category enforcement counts for
   *  the current day (category + count, never content). Additive; empty/absent
   *  when Notify is off or nothing was reported today. */
  content_notices?: FamilyContentNotice[];
  /** v1.x device marker (family-safety.md § Full visibility for young children,
   *  Slice F) — the ward's devices in the slim guardian-facing projection, so
   *  the Family page renders the per-device mark toggle from this one status
   *  read. Guardian-populated and guardianship-guarded nest-side; additive, so
   *  absent/empty for a ward with no devices. */
  devices?: FamilyWardDeviceInfo[];
  /** v1.x screen time (family-safety.md § Screen time) — this ward's
   *  cross-device foreground total for THEIR current local day, which the nest
   *  derives from the link's last-reported UTC offset. Absent/null when no
   *  daily budget is set: no accounting without a declared policy, so the
   *  `family-ward-usage-today` readout renders nothing. Additive. */
  usage_today_minutes?: number | null;
  /** v1.x bridge-DM gate — the peers this ward's guardian DENIED
   *  (`family-safety.md` § The bridge-DM gate → *The un-deny surface*), block
   *  verdicts only; each carries the `(bridge_id, peer_id)` pair the un-deny
   *  (`familyAllowBlockedDmPeer`) takes.
   *  Additive — absent/empty when there are no denials. */
  blocked_dm_peers?: BlockedPeer[];
  /** The ward's age band + how it was established (`family-ward-age-band`);
   *  absent for a band-less admission. Additive. */
  age_band?: FamilyAgeBandInfo | null;
}

/** An account's age band (`fauna_protocol::family::FamilyAgeBandInfo`): the
 *  wire token + its provenance. Render via `ageBandLine`, never by hand. */
export interface FamilyAgeBandInfo {
  band: string;
  provenance: string;
}

/** One of a ward's devices in the guardian-facing projection on
 *  {@link FamilyWardInfo} — the id `fauna.family.device.mark` takes, a label to
 *  show, and the mark state the toggle renders. Deliberately NOT the full
 *  `SyncDevice`: a guardian marks a ward's devices, it does not manage the
 *  ward's sync. */
export interface FamilyWardDeviceInfo {
  /** Hex-encoded 32-byte device id — the `device.mark` key. */
  device_id: string;
  label: string;
  /** This device carries the guardian-enrolled marker (additive). */
  guardian_marked?: boolean;
}

/** `fauna.family.status` — both roles in one read. */
export interface FamilyStatus {
  supervised_by?: FamilyGuardianInfo | null;
  /** MY active policy (supervised side); null when this account isn't supervised. */
  policy?: ReachPolicy | null;
  wards: FamilyWardInfo[];
  /** Proposals awaiting the caller's consent as proposed guardian (additive).
   *  The family-tab gate widens on it — a target not otherwise in a family
   *  relationship must still reach the prompt. */
  incoming_transfers?: FamilyIncomingTransferInfo[];
  /** v1.x screen time — the CALLER'S OWN cross-device total for their current
   *  local day, when they are supervised under a daily budget. The same number
   *  their guardian sees on `family-ward-usage-today` — the pillar's
   *  transparency rule. Absent/null when unsupervised or no budget. Additive. */
  usage_today_minutes?: number | null;
  /** The supervised caller's OWN age band (`family-age-band-summary`); absent
   *  when there is none. Additive. */
  age_band?: FamilyAgeBandInfo | null;
  /** v1.x — the supervised caller's OWN outstanding contact asks
   *  (`family-safety.md` § Child-initiated contact requests → *Ward
   *  transparency*), what `contact-request-pending` renders from. Additive. */
  contact_requests?: ContactAsk[];
  /** v1.x — the supervised caller's OWN feed-source asks, pending and
   *  approved-but-unredeemed (`family-safety.md` § Feed-source approvals),
   *  what `bridge-source-request-state` renders from. Additive. */
  feed_requests?: FeedAsk[];
  /** NOT a wire field — the shared `SupervisionSnapshot::from_status` fold of
   *  this reply, attached by the wasm `familyStatus` choke point (the same
   *  value it persists). Every client-enforced input — the content floor,
   *  `content_notify`, the screen-time policy and the guardian the lock names —
   *  moves from HERE, never from `policy`, so the graduation gate (nothing is
   *  enforceable without `supervised_by`) lives once, in shared Rust. */
  supervision: SupervisionSnapshotValue;
  /** NOT a wire field — the shared `kids_app_eligible` verdict over this reply
   *  (`family-safety.md` § The account age band → the kids-app bullet), attached
   *  by the same choke point. Web carries no kids flavor (a declared platform
   *  absence), so nothing here reads it yet; optional so fixtures need not. */
  kidsAppEligible?: boolean;
}

/** One pending reach approval (`fauna.family.approvals.list`), across every ward.
 *  A `contact` entry is named by `peer_actor_id`, a `mail_hold` by `message_id`
 *  (never by the address — approving one held message must not sweep the sender's
 *  other mail). A `mail_hold`'s `summary` is deliberately ALWAYS empty: a subject
 *  line is content and the message is sealed to the ward, so the entry carries
 *  envelope metadata only (`peer_address`) — family-safety.md § Reach approvals. */
export interface FamilyApprovalEntry {
  supervised_actor_id: Uint8Array | number[];
  supervised_handle: string;
  /** `contact` | `mail_hold` | `contact_request` | `feed_source` | `dm_hold`
   *  (v1.x adds kinds additively). */
  kind: string;
  peer_actor_id: Uint8Array | number[];
  /** The `mail_hold` sender's envelope address, or a `dm_hold`'s external peer
   *  id (which is not an actor on this nest, so it cannot ride
   *  `peer_actor_id`); empty for a `contact`. */
  peer_address: string;
  /** The held message's id, naming a `mail_hold` on decide; empty for a `contact`. */
  message_id: Uint8Array | number[];
  /** For a `feed_source`, the ward's own display label for the thing they asked
   *  for. Always empty for a `mail_hold`: a subject line is content. */
  summary: string;
  /** v1.x — the peer's handle, so a `contact_request` renders without a second
   *  lookup. Optional: empty on other kinds. */
  peer_handle?: string;
  /** v1.x — with `operation` and `target`, the `feed_source` item's key on
   *  decide. Optional: empty on other kinds. */
  bridge_id?: string;
  /** v1.x — see `bridge_id`. `link` | `follow` | `feed`. */
  operation?: string;
  /** v1.x — see `bridge_id`. Empty for a `link`. */
  target?: string;
  created_at: number;
}

/** `fauna.family.status` — the supervised indicator + the whole Family page. */
export function familyStatus(secretHex: string): Promise<FamilyStatus> {
  return call(secretHex, (c) => c.familyStatus() as Promise<FamilyStatus>);
}

/** `fauna.family.policy.update` — replace a ward's reach policy (guardian-only). */
export function familyPolicyUpdate(
  secretHex: string,
  supervisedActorId: Uint8Array,
  policy: ReachPolicy,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyPolicyUpdate(supervisedActorId, policy) as Promise<void>,
  );
}

/** `fauna.family.approvals.list` — the queue across ALL wards (not ward-scoped). */
export function familyApprovalsList(secretHex: string): Promise<FamilyApprovalEntry[]> {
  return call(secretHex, (c) => c.familyApprovalsList() as Promise<FamilyApprovalEntry[]>);
}

/** `fauna.family.approvals.decide` — approve/deny one queue item. Pass the key the
 *  `kind` names and leave the others empty; a `familyApprovalsList` entry carries
 *  all of them. `contact`/`contact_request` → `peer_actor_id`; `mail_hold` →
 *  `message_id`; `feed_source` → the whole `(bridge_id, operation, target)` triple
 *  (`target` is empty for a `link`; the label is never part of the key);
 *  `dm_hold` → `(bridge_id, peer_address)`. */
export function familyApprovalsDecide(
  secretHex: string,
  supervisedActorId: Uint8Array,
  kind: string,
  peerActorId: Uint8Array,
  messageId: Uint8Array,
  bridgeId: string,
  operation: string,
  target: string,
  peerAddress: string,
  approve: boolean,
): Promise<void> {
  return call(
    secretHex,
    (c) =>
      c.familyApprovalsDecide(
        supervisedActorId,
        kind,
        peerActorId,
        messageId,
        bridgeId,
        operation,
        target,
        peerAddress,
        approve,
      ) as Promise<void>,
  );
}

/** The guardian's un-deny of one denied bridge-DM peer (`family-safety.md`
 *  § The bridge-DM gate → *The un-deny surface*) — the shared
 *  `FamilyClient::allow_blocked_dm_peer`, which owns the approving `dm_hold`
 *  decide's wire shape. Pass the denied row's own `BlockedPeer`. */
export function familyAllowBlockedDmPeer(
  secretHex: string,
  supervisedActorId: Uint8Array,
  peer: BlockedPeer,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyAllowBlockedDmPeer(supervisedActorId, peer.bridge_id, peer.peer_id) as Promise<void>,
  );
}

/** `fauna.family.contact.add` — pre-approve a contact on the ward's behalf (the
 *  guardian-side complement of contact-approval mode). */
export function familyContactAdd(
  secretHex: string,
  supervisedActorId: Uint8Array,
  peerActorId: Uint8Array,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyContactAdd(supervisedActorId, peerActorId) as Promise<void>,
  );
}

/** `fauna.family.contact.request` — the SUPERVISED caller's in-app ask
 *  (`contact-request-guardian-button`), offered only after a knock came back
 *  with the typed guardian refusal (`family-safety.md` § Child-initiated
 *  contact requests). The reply is a bare ack; re-read `familyStatus` for the
 *  durable `contact_requests`. */
export function familyContactRequest(secretHex: string, peerActorId: Uint8Array): Promise<void> {
  return call(secretHex, (c) => c.familyContactRequest(peerActorId) as Promise<void>);
}

/** `fauna.family.feed_source.request` — the SUPERVISED caller's ask
 *  (`bridge-source-request-button`) for one `(bridge_id, operation, target)`
 *  the `feed_sources = "block"` knob just refused (`family-safety.md`
 *  § Feed-source approvals). `label` is display-only. The reply is a bare ack;
 *  re-read `familyStatus` for the durable `feed_requests`. */
export function familyFeedSourceRequest(
  secretHex: string,
  bridgeId: string,
  operation: string,
  target: string,
  label: string,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyFeedSourceRequest(bridgeId, operation, target, label) as Promise<void>,
  );
}

/** `fauna.family.graduate` — supervised → full account, in place (same actor, keys,
 *  handle, data; the link + policy rows go away and pending items are released). */
export function familyGraduate(
  secretHex: string,
  supervisedActorId: Uint8Array,
): Promise<void> {
  return call(secretHex, (c) => c.familyGraduate(supervisedActorId) as Promise<void>);
}

/** `fauna.family.notify_report` — the SUPERVISED caller reports coarse per-category
 *  enforcement counts (family-safety.md § Guardian Notify). Each `count` is a delta
 *  the nest accumulates into the ward's day bucket; carries **no content id**.
 *  `utcOffsetMinutes` is the device's UTC offset (nest-clamped to `-720..=840` — the
 *  § Screen time day-bucket rule). A no-op nest-side unless the caller is supervised
 *  with the guardian's `content_notify` on. */
export function familyNotifyReport(
  secretHex: string,
  entries: FamilyContentNotice[],
  utcOffsetMinutes: number,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyNotifyReport(entries, utcOffsetMinutes) as Promise<void>,
  );
}

/** `fauna.family.transfer` — PROPOSE a new guardian for a ward; pending until
 *  the target accepts (family-safety.md § Graduation & transfer). A
 *  self-proposal completes immediately (consent by construction). */
export function familyTransfer(
  secretHex: string,
  supervisedActorId: Uint8Array,
  newGuardianActorId: Uint8Array,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyTransfer(supervisedActorId, newGuardianActorId) as Promise<void>,
  );
}

/** `fauna.family.transfer.accept` — consent to a proposal naming the caller;
 *  completes the re-point (admissibility re-validated nest-side). */
export function familyTransferAccept(
  secretHex: string,
  supervisedActorId: Uint8Array,
): Promise<void> {
  return call(secretHex, (c) => c.familyTransferAccept(supervisedActorId) as Promise<void>);
}

/** `fauna.family.transfer.decline` — refuse a proposal naming the caller; the
 *  existing link stands. */
export function familyTransferDecline(
  secretHex: string,
  supervisedActorId: Uint8Array,
): Promise<void> {
  return call(secretHex, (c) => c.familyTransferDecline(supervisedActorId) as Promise<void>);
}

/** `fauna.family.transfer.cancel` — withdraw the ward's pending proposal
 *  (current guardian or admin). */
export function familyTransferCancel(
  secretHex: string,
  supervisedActorId: Uint8Array,
): Promise<void> {
  return call(secretHex, (c) => c.familyTransferCancel(supervisedActorId) as Promise<void>);
}

/** `fauna.family.device.mark` — set/clear the guardian-enrolled-device marker on
 *  one of the ward's devices (family-safety.md § Full visibility for young
 *  children). Guardian-only nest-side and re-checked per target; a marked device
 *  is un-removable by the ward and auto-revoked at graduation. `deviceId` is the
 *  hex spelling `fauna.family.status` hands back. */
export function familyDeviceMark(
  secretHex: string,
  supervisedActorId: Uint8Array,
  deviceId: string,
  marked: boolean,
): Promise<void> {
  return call(
    secretHex,
    (c) => c.familyDeviceMark(supervisedActorId, deviceId, marked) as Promise<void>,
  );
}

/** Reply to `fauna.family.usage_report` — the nest-stamped local-day bucket and
 *  that day's cross-device total after this report. */
export interface FamilyUsageReportReply {
  day: number;
  day_total_minutes: number;
}

/** `fauna.family.usage_report` — the supervised caller heartbeats coarse
 *  foreground minutes for the daily screen-time budget (family-safety.md
 *  § Screen time). `minutes` is the foreground delta since the last SUCCESSFUL
 *  report; `0` is a pure read, which is how a locked-out ward learns that local
 *  midnight has rolled the bucket or that their guardian raised the budget.
 *  Best-effort telemetry: a silent zero-reply no-op unless the caller is
 *  supervised with a budget set. */
export function familyUsageReport(
  secretHex: string,
  minutes: number,
  utcOffsetMinutes: number,
): Promise<FamilyUsageReportReply> {
  return call(
    secretHex,
    (c) => c.familyUsageReport(minutes, utcOffsetMinutes) as Promise<FamilyUsageReportReply>,
  );
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** Install `window.__fauna_rpcEcho(dataHex)` — the `fauna.protocol.echo`
 *  round-trip probe that `tests/e2e-unified/tests/test_web_ws_rpc_echo.py`
 *  drives. Resolves `{ok, data_hex}` or `{ok:false, error}`. Called only from
 *  `$lib/e2e-automation` (test builds — testing.md § Test-agent build
 *  exclusion), so production bundles tree-shake it out entirely. */
export function installRpcTestHooks(): void {
  if (typeof window === 'undefined') return;
  (window as unknown as { __fauna_rpcEcho?: (h: string) => Promise<unknown> }).__fauna_rpcEcho =
    async (dataHex: string) => {
      try {
        const id = get(identity);
        if (!id) return { ok: false, error: 'not logged in' };
        const c = await getClient(id.secretHex);
        await ensureConnected(c);
        const reply: Uint8Array = await c.rpcEcho(hexToBytes(dataHex));
        return { ok: true, data_hex: bytesToHex(reply) };
      } catch (e: unknown) {
        return { ok: false, error: String(e instanceof Error ? e.message : e) };
      }
    };
}
