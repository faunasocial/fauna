// Conversations module (web) — the single SPA home for the shared (wasm)
// `ConversationsManager`. The browser twin of linux `src/conversations/`
// (`host.rs` singleton + `state.rs` thread-state serializer + `conv_backend.rs`
// inbound poll). Owns:
//
//   1. The process-wide manager singleton (`getConversationsManager`), built
//      once the actor secret is known — the secret seeds the FaunaMls
//      credential; the `<handle>@<domain>` SMTP From is a LIVE cell
//      (`setSelfAddress`, pushed by the identity-store subscription below)
//      read at send time, never baked at build (`conversations.md` § State &
//      data shape → *Self-address: live, never baked*). Both rails (SMTP
//      send+receive, FaunaMls E2E DMs) ride this one instance — one page,
//      every rail (`conversations.md` § Goal).
//   2. The reactive stores the page + e2e read off: `conversationsSnapshot`
//      (raw `manager.snapshot()` — list pane, compose, picker, add-participant)
//      and `conversationThreads` (the `data.conversation_threads` e2e rows,
//      mirroring linux `build_conversation_threads_state`).
//   3. The app-wide receive rail (`startReceivePoll`), both rails: the FaunaMls
//      (MLS DM) rail — `drainInbox` (durable inbox-apply, `api-layers.md` §
//      Inbox & Messaging layer 4) + `pollConversations` — and the SMTP receive
//      poll (`fauna.email.inbox.fetch` → client-side HPKE decrypt → ingest), the
//      web twin of `mail_sink.rs::start_inbound_poll`. The JS-side loop is forced
//      by the wasm `!Send` WS-RPC client (the async seams can't carry the
//      `Rc`-based client — `conversations.md` § Receiving into the conversations
//      view), which is also why web cannot run the shared
//      `ConversationsSession::start_receive_loop` (that `tokio::select!` loop is
//      `cfg(not(wasm32))`). So this module hand-mirrors the *arms* of that loop:
//      a 30s backstop ticker (native's `DEFAULT_CONV_POLL_SECS`) **plus** a push
//      arm off the one authenticated socket — `fauna.conversations.{channel.message,
//      welcome.received}` wake the conv rail, `fauna.mail.received` wakes the mail
//      rail — all funnelled through the one serialized receive pump (§ Receive pump).
//   4. The e2e command hook (`window.__fauna_callCommand`) routing the
//      `conversations_*` test commands to the manager's wasm test-helpers — the
//      browser twin of linux's `conversations_*` bridge commands. Inert in
//      production (only the Playwright bridge ever calls it).
//
// The mail receive cursor + dedup are session-local and the loop re-polls from
// uid 0 each launch: the manager's thread store is in-memory only, so a fresh
// launch rebuilds the mail threads by re-fetching (the manager's own seen-set
// guards double-ingest across overlapping pages). The fauna-native MLS rail does
// NOT re-walk its log that way — a sender cannot decrypt its own application
// messages, so `restoreMlsState()` (below) rehydrates the in-memory store from
// the sealed `__mls` replica before the first poll, which then resumes from the
// restored watermark. Build state: `docs/goal/behavior/devices.md`
// § Implementation status today.

import { get, writable, type Writable } from 'svelte/store';
import type { LocalizedText } from './i18n/localized';
import { identity, reconnectTick } from './store';
import { guardSingletonBuild } from './singleton-build';
import { registerActorScopedReset, sameActorSince } from './actorScope';
import {
  actorsServedByAnotherTab,
  engineLockName,
  removeAccountBlock,
  tryHoldEngineRole,
  type EngineRole,
  type RemoveAccountBlock,
} from './webLocks';
import { accountsList, accountsTabSessionMaterial } from './accounts';
import { ReceivePump, RECEIVE_PASS_CEILING_MS } from './receive-pump';
import {
  autosaveDebounceMs,
  ensureWasm,
  logMessage,
  postBodyText,
  setMlsResealSink,
  type LocalDetection,
} from './wasm';
import { recordAftermathLeg } from './succession-aftermath';
import { raiseNewMessageBanners, resetMessageBanners } from './message-banner';
import {
  applySpamDisposition,
  conversationsManager,
  emailInboxFetch,
  emailSentFetch,
  fetchSpamModel,
  getSpamScoringPolicy,
  mailSettingsMachine,
  moderationTrain,
  onPushEvent,
  postsGet,
  type InboxFetchReply,
} from './rpc';
import type {
  WasmConversationsManager,
  WasmFeedManager,
  WasmMailSettingsMachine,
} from '../../static/fauna_wasm.js';
// The `__fauna_callCommand` bridge below is the *single* global e2e command hook
// (one per window), so the feed-snapshot injection command rides it too — the feed
// logic stays in `./feed` (`getFeedManager` / `refreshFeed`), only its dispatch
// case lives here. Browser twin of linux's `feed_inject_posts` test-agent command.
import { feedManagerIfReady, getFeedManager, refreshFeed } from './feed';
import { registerE2eCommands } from './e2e-commands';
import { getDeviceId } from './device-id';
import {
  accountRuntimeSettled,
  accountRuntimeStarted,
  startAccountRuntimeFor,
  stopAccountRuntime,
} from './account-runtime';
import { runAtLaterEdge } from './launch-pass';

/** A row mirroring the manager snapshot's `ThreadSummary`, in the shape the e2e
 *  `data.conversation_threads` state reads. `message_count` / `channel_id_hex`
 *  come from `threadDetail`, mirroring linux `build_conversation_threads_state`. */
export interface ConversationThreadRow {
  thread_id: string;
  label: string;
  snippet: string;
  rail: string;
  flavor: string;
  unread_count: number;
  participant_count: number;
  message_count: number;
  participant_actor_ids: (string | null)[];
  channel_id_hex: string | null;
}

/** The `data.conversation_threads` e2e rows (the agent publishes this store). */
export const conversationThreads: Writable<ConversationThreadRow[]> = writable([]);

/** The `data.succession_witness` report — the member side of a succession as
 *  this seat's driver saw it (what the inbound poll did with the statements it
 *  saw, what the peer-anchor harvest managed per peer, what the witness made of
 *  each identity). Rendered whole by shared Rust
 *  (`fauna_client_recovery::witness::state_json`, the same renderer tui, linux
 *  and the four FFI apps publish); the SPA neither derives the shape nor reads
 *  it. `null` until a conversations manager with a witness exists — which is
 *  what "this seat registered none" looks like on every app. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const successionWitness: Writable<any> = writable(null);

/** Whether the real FaunaMls backend is active (the tier_3 `test_fauna_mls_real_roundtrip`
 *  opt-in). Published as `data.conv_real_backend_active`; the action layer's
 *  `enable_real_faunamls` polls it. The browser twin of linux
 *  `conv_backend.rs::is_e2e_real_active`. */
export const convRealBackendActive: Writable<boolean> = writable(false);

/** The raw `manager.snapshot()` (threads + sort + selection + new-thread compose
 *  + add-participant overlay) the conversations page renders its list pane,
 *  compose bar, and recipient picker off — observer-driven off the snapshot, no
 *  client-side state machine (`conversations.md` § Architectural rules #1). `any`
 *  because the shape is the shared `ConversationsSnapshot` serde JSON. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const conversationsSnapshot: Writable<any> = writable(null);

/** How many received mail records this page's receive loop has skipped this
 *  run because they would not open under the account's current key set — the
 *  shared `ConversationsManager::unopenable_mail_count` (wasm twin
 *  `unopenableMailCount()`), read on the same refresh the snapshot rides
 *  (`refreshConversations` below). The floor arm of the page's `error-message`
 *  precedence — `ui/conversations.md` § Errors & edge cases → *A fifth truth*
 *  — ranked below every other truth and cleared only as records retire their
 *  own entry, never by an unrelated success. */
export const unopenableMailCount: Writable<number> = writable(0);

let manager: WasmConversationsManager | null = null;
let managerPromise: Promise<WasmConversationsManager> | null = null;
/** This tab's MLS-writing role for the current account, held for as long as the
 *  manager lives (released by `resetConversationsManager`, and for free when the
 *  tab closes). `null` while no manager is built. */
let engineRole: EngineRole | null = null;
/** The account `engineRole` was taken for — `null` exactly when it is. The
 *  sign-out guard's proof that this tab, not another, holds that account's role
 *  (`signOutBlockedByAnotherTab`). */
let engineRoleActor: string | null = null;

/** Whether another tab of this browser profile serves any account a sign-out
 *  would erase — every account in the registry plus the signed-in one, since
 *  `accountsClearAll` erases them all (`account-scoping.md` § Concurrent
 *  instances → *An erase refuses while a sibling serves the account*). Asked
 *  BEFORE `identity.logout()` is entered: `logout()` records the sign-out
 *  ahead of its first `await`, and a recorded sign-out is finished by the next
 *  load whatever this tab does, so a refusal has to be decided first.
 *
 *  This tab's own role is consulted, never probed (`actorsServedByAnotherTab`).
 *  One transient false refusal is accepted: a build that has been granted the
 *  role but not yet parked it in `engineRole` probes as "held elsewhere"; the
 *  line tells the user to press sign-out again, which then succeeds. An
 *  unreadable registry degrades to the signed-in account alone, the same
 *  degrade-open posture the probe itself takes. */
export async function signOutBlockedByAnotherTab(): Promise<boolean> {
  const actorIds: string[] = [];
  const active = get(identity)?.actorId;
  if (active) actorIds.push(active);
  try {
    for (const entry of await accountsList()) actorIds.push(entry.actor_id);
  } catch (e) {
    logMessage('warn', 'fauna_web::conversations', `sign-out guard: account registry unreadable: ${e}`);
  }
  return (await actorsServedByAnotherTab(actorIds, holdsEngineRoleHere)).length > 0;
}

/** Whether a remove-account of `actorId` must refuse, and with which line —
 *  `'this_tab'` when it is the account this tab serves, `'other_tab'` when
 *  another tab holds its engine role (`$lib/webLocks::removeAccountBlock`).
 *  Asked before `accountsRemove`, which drops the account's secret slots. */
export async function removeAccountBlocked(actorId: string): Promise<RemoveAccountBlock> {
  return removeAccountBlock(actorId, get(identity)?.actorId, holdsEngineRoleHere);
}

/** Whether this tab holds `actorId`'s MLS-writing role — the erase guards'
 *  proof that no other tab does (`actorsServedByAnotherTab`'s `heldHere`). */
function holdsEngineRoleHere(actorId: string): boolean {
  return (
    engineRole != null &&
    engineRoleActor != null &&
    engineLockName(engineRoleActor) === engineLockName(actorId)
  );
}

/** True when this tab asked for the account's MLS-writing role and was refused —
 *  another tab of this browser profile holds it, or this embedding exposes no
 *  Web Locks manager and solitude cannot be proven (`$lib/webLocks`
 *  [`tryHoldEngineRole`], which fails closed).
 *
 *  The conversations page renders this through its `error-message` element: the
 *  losing tab must have a defined, *rendered* behaviour — never a crash, and
 *  never a silent second writer, which is the one outcome the device-owned-epoch
 *  invariant cannot survive (`account-scoping.md` § Concurrent instances →
 *  *Web*). Reusing the page's existing error element is deliberate: web owes no
 *  new surface here (as with the launch-collision chooser, "tabs *are* web's
 *  instance story"), so this needs no new ui.yaml id and no per-app divergence. */
export const engineRoleDenied: Writable<boolean> = writable(false);
// Built once and reused for the secret check — avoids churning a fresh
// wasm-bindgen machine every tick while waiting for mail to be enabled.
let settingsMachine: WasmMailSettingsMachine | null = null;
let pollStarted = false;
// Independent paging cursors for the two server-side mailboxes the receive poll
// drains: INBOX (received) and Sent (the actor's own outbound copies, e.g. mail
// sent from an external MUA). The UIDs are per-mailbox sequences, so a shared
// cursor would skip/double-fetch — each tracks its own (mirrors linux
// `mail_sink.rs::start_inbound_poll`'s `after_uid` / `after_uid_sent`).
let afterUid = 0;
let afterUidSent = 0;

/** The logged-in user's SMTP From, `<handle>@<domain>` — empty when either
 *  isn't populated yet (the identity-refresh race right after `identity.login`,
 *  before `refreshFromServer` lands handle/domain), matching every native
 *  app's fallback (linux `app.rs`, android `ApiClient.kt`, apple
 *  `FaunaApp.swift`/`FaunaMacApp.swift`, windows `NestRpcClient.cs`) — never a
 *  synthesized `<actor-id prefix>@fauna.local` placeholder, which reads as a
 *  real (but wrong) address and previously got baked permanently into the
 *  cached `managerPromise`. `SmtpBackend::self_domain()` degrades an empty
 *  address to `"localhost"` for Message-ID composition — a safe, already-proven
 *  degenerate case (`libs/fauna-conversations/src/backends/smtp.rs`). */
function selfAddressFor(id: { handle?: string; domain?: string }): string {
  if (!id.handle || !id.domain) return '';
  return `${id.handle}@${id.domain}`;
}

/** The single shared send+receive `ConversationsManager` for the SPA. Built once
 *  the actor secret is known; the `<handle>@<domain>` SMTP From is seeded with
 *  whatever has resolved by then (possibly `''`) and kept current through the
 *  live cell (`setSelfAddress` — the identity subscription below pushes it), so
 *  a mid-race build heals instead of refusing sends until logout. Both the
 *  inbound poll below and the conversations page drive this one instance, so a
 *  composed thread and received threads share one snapshot. Rejects until an
 *  identity exists. */
/** The already-built manager, or `null` if the async build has not finished (or no
 *  identity exists yet) — the **synchronous** accessor for callers that cannot
 *  await. Today that is the Folders page's B3 join-filter (`MlsQuery`), which
 *  the folders wasm bundle calls back synchronously: with no manager yet it
 *  answers "not joined", the designed fail-safe (member rows stay hidden until
 *  the engine can actually confirm the join). Never build here — `null` is the
 *  honest answer, and the next 15s refresh re-asks. */
export function conversationsManagerIfReady(): WasmConversationsManager | null {
  return manager;
}

/** Make sure this tab's account runtime is starting — for a page that reads
 *  an account-plane kind (`fauna.state.backup` on the Backups page) but builds
 *  no conversations manager itself. The runtime starts once the manager
 *  exists (`$lib/account-runtime`), so a direct load of such a page would
 *  otherwise never get one, and every plane read on it would answer "the
 *  account store is not ready yet". Fire-and-forget: the plane seams wait out
 *  the start themselves, and a tab that does not hold the MLS-writing role
 *  (the build refuses) simply stays without a runtime, as before. */
export function ensureAccountRuntime(): void {
  getConversationsManager().catch(() => {});
}

export function getConversationsManager(): Promise<WasmConversationsManager> {
  if (managerPromise) return managerPromise;
  const id = get(identity);
  if (!id?.secretHex) {
    return Promise.reject(new Error('conversations manager: no identity yet'));
  }
  // The identity seam ahead of the module-state write below (`actorScope.ts`):
  // this builder awaits the role lock, the wasm build and several MLS passes,
  // so a switch has plenty of room to land inside it — and
  // `resetConversationsManager`, which only nulls `manager`/`managerPromise`,
  // cannot stop a build already in flight from assigning the departing actor's
  // manager over the incoming actor's.
  const stillThisActor = sameActorSince();
  const build = async (stillWanted: () => boolean): Promise<WasmConversationsManager> => {
    await ensureWasm();
    // ⚠ THE MLS-WRITING ROLE, BEFORE ANY ENGINE EXISTS — web's leg of the
    // conversations-engine role lock (`account-scoping.md` § Concurrent
    // instances → *Web*). Placed here for the same reason the native lock sits
    // inside `SqliteStorage::open`: the role must be held before anything can
    // advance the leaf, and the shared engine-construction path is the one
    // place every writer passes through.
    //
    // It cannot be a gate on "sends" instead. This builder ALONE writes MLS
    // state four times, most of them before returning a manager to anyone —
    // `restoreMlsState` rehydrates and re-seals the replica,
    // `foldersResumePendingRemovals` publishes crash-staged rotations (in the
    // build, or at the account runtime's edge when that comes later), and the
    // leg-3 re-seal barrier runs inside the replica's own
    // `load()`. A second tab reaching any of them has already forked the
    // ratchet, so the refusal has to land before the first of them.
    if (!engineRole) {
      const role = await tryHoldEngineRole(id.actorId);
      // Both seams before the role lands in module state. The role is keyed per
      // ACCOUNT, and the drop releases only what `engineRole` already holds: a
      // switch landing inside this await would otherwise park the DEPARTING
      // account's role in the slot, where the incoming actor's build reads it as
      // "already held" and skips taking its own. An abandoned build hands it
      // back for the same reason — its successor takes the role itself.
      if (!stillThisActor() || !stillWanted()) {
        role?.release();
        throw new Error('conversations manager: superseded while taking the MLS-writing role');
      }
      engineRole = role;
      engineRoleActor = role ? id.actorId : null;
      if (!engineRole) {
        // Log the TRANSITION, not the state: `startReceivePoll` re-attempts the
        // build on every tick (which is what lets this tab take over when the
        // holder closes), so logging unconditionally would emit one warning per
        // tick for as long as both tabs stay open.
        if (!get(engineRoleDenied)) {
          logMessage(
            'warn',
            'fauna_web::conversations',
            'MLS-writing role refused for this account (another tab holds it, or this ' +
              'embedding exposes no Web Locks manager) — running no engine in this tab',
          );
        }
        engineRoleDenied.set(true);
        // Clear the memo so a later attempt (the receive poll's next tick, or a
        // reload after the holding tab closes) can win the role instead of
        // being served this same rejection forever.
        managerPromise = null;
        throw new Error('conversations manager: this account is served by another tab');
      }
      engineRoleDenied.set(false);
    }
    // ⚠ BEFORE the manager is built, not after. Leg 3 of the succession
    // aftermath — the `__mls` re-seal — is a barrier inside the replica's own
    // `load()`, which this constructor kicks off, so a sink registered
    // afterwards would miss the pass entirely and the Recovery kit section
    // would render no leg-3 line. The other
    // five legs report through `runSuccessionAftermath`'s own callback; this is
    // the same callback, reaching the one leg that cannot be handed it.
    setMlsResealSink(recordAftermathLeg);
    const built = await conversationsManager(id.secretHex, selfAddressFor(id));
    if (!stillThisActor()) {
      // Deliberately does NOT null `managerPromise`: by now it may already hold
      // the INCOMING actor's build, and clearing it here would throw that away.
      throw new Error('conversations manager: actor changed while the manager was building');
    }
    // The settle deadline's seam beside the identity one: an abandoned build
    // resolving after its replacement must not install itself over it
    // (`singleton-build.ts`).
    if (!stillWanted()) {
      throw new Error('conversations manager: build abandoned by its settle deadline');
    }
    manager = built;
    // This tab's account runtime, over the manager just built — it registers
    // the account-plane seams on it at the store's ready edge
    // (`$lib/account-runtime`). Not awaited: the store opening must never
    // hold the conversations page back, and a failed start degrades to the
    // blob rail on its own.
    startAccountRuntimeFor(id.secretHex, id.actorId, built);
    // ⚠ FROM HERE ON THE BUILD WORKS ON `built`, NEVER ON `manager`, AND
    // RE-CHECKS BOTH SEAMS BEFORE EACH PASS. Every pass below writes MLS state —
    // the restore re-seals the replica, the folder passes publish, the mint
    // grows the key-package pool — and each is an await a switch or the settle
    // deadline can land inside. After either, the slot may hold ANOTHER build
    // (the incoming actor's, or this abandoned one's replacement), and a tail
    // reading `manager.` would run its passes on that one. Nor does this build
    // hold the role for them any more: a switch released it, and an abandonment
    // put a second engine for this account in this same tab. The one call
    // already in flight cannot be stopped; the next one can.
    const stillOurs = (): boolean => stillThisActor() && stillWanted();
    const superseded = (): Error =>
      new Error('conversations manager: superseded mid-restore (actor switch or settle deadline)');
    // Draft-persistence v2 (file-sync.md § Drafts Sync): restore the owner's
    // persisted conversations drafts once, before any save can run — so the
    // composer's in-progress drafts survive an app restart and appear on the
    // user's other devices. A transient/seal failure is logged + swallowed (the
    // page must still open); the wasm side's shared `DraftsSync` keeps its save
    // gate closed until a restore *succeeds*, so a later save can't clobber an
    // unread (incl. undecryptable) blob — the next launch retries the load.
    try {
      await built.restoreDrafts();
      refreshConversations();
    } catch (e) {
      logMessage('warn', 'fauna_web::conversations', `restore drafts failed: ${e}`);
    }
    if (!stillOurs()) throw superseded();
    // Cross-device MLS group-state sync (devices.md § Cross-device MLS group-state
    // sync): restore the openMLS `provider` + per-channel `history/<ch>` replicas and
    // wire the device-owned-epoch gate/cursor BEFORE the first `pollConversations`
    // (the caller, `startReceivePoll`, awaits this whole builder before its first
    // poll). So a second browser (same secret, empty local engine) reads its OWN
    // pre-existing conversation history — which log replay can never reconstruct
    // (own-leaf messages are not sender-decryptable) — and resumes each channel from
    // its restored watermark. A load failure is logged + swallowed, leaving the tab
    // single-device; the wasm save gate stays closed until a restore *succeeds*, so a
    // later `saveMlsState` can't clobber the real nest-stored replica.
    try {
      await built.restoreMlsState();
      refreshConversations();
    } catch (e) {
      logMessage('warn', 'fauna_web::conversations', `restore mls state failed: ${e}`);
    }
    if (!stillOurs()) throw superseded();
    // Launch-time crash recovery for interrupted folder member removals
    // (`FoldersAuthor::resume_pending_removals`) — web's leg of the same call linux
    // drives from `conv_backend.rs` and apple/android/windows inherit from the
    // `fauna-ffi` session factory. It makes rotate-on-removal's forward-secrecy
    // guarantee actually complete after a crash between the staged rotation and its
    // publish (`mls-group-key-material.md` § M2 Rotate-on-removal).
    //
    // Ordering is load-bearing: it runs AFTER `restoreMlsState`, because the removal
    // gate IS the backend and the restore is what injects its `CommitGate` — resuming
    // earlier would see `NoGate` and defer every gated sentinel to the next launch. A
    // tab reload between a sentinel write and its send is precisely how web poisons one
    // (`persist_group_state` is a no-op on wasm), so web is the plane that needs this
    // most. Logged + swallowed: a stuck removal must not stop the page opening.
    //
    // ⚠ And it needs a SECOND edge: the folder-key custody it reads rests in the
    // account runtime started above, which this build never awaits. So the pass
    // runs at whichever edge is later (`$lib/launch-pass`) — in line when the
    // runtime has started, else once its start settles, beside the receive
    // poll, without holding the manager back. It used to run here regardless,
    // fail "the account runtime is not running" on every launch the runtime
    // lost the race, and leave the removed member keyed until one that won. The
    // same wasm call re-runs the foreign-home seed `restoreMlsState` ran over
    // that same unreadable custody.
    //
    // The recording device (the same id `foldersServeSet` passes) also resumes an
    // interrupted served-set walk in the same pass (`webdav-server.md` § Key model (c)).
    await runAtLaterEdge({
      started: accountRuntimeStarted,
      settled: accountRuntimeSettled,
      stillWanted: stillOurs,
      pass: async () => {
        try {
          const resumed = await built.foldersResumePendingRemovals(
            id.secretHex,
            getDeviceId(id.actorId),
          );
          // Logged every launch, zero included: the line is the witness that
          // the pass ran at all, which a swallowed failure used to hide for
          // as long as nobody read the console (`test_web_boot_effect_loop.py`).
          logMessage(
            'info',
            'fauna_web::conversations',
            `folders: launch pass ran — ${resumed} crash-staged member removal(s) resumed`,
          );
        } catch (e) {
          logMessage(
            'warn',
            'fauna_web::conversations',
            `folders: resume pending removals failed at launch: ${e}`,
          );
        }
      },
      skipped: (why) =>
        logMessage('warn', 'fauna_web::conversations', `folders: launch pass skipped — ${why}`),
    });
    if (!stillOurs()) throw superseded();
    // Login-time key-package replenish (the one-time pool + the mandatory
    // last-resort KP), so peers can add us after the pool drains — the SAME
    // manager surface linux (`conv_backend.rs` login task) and windows
    // (`mgr.EnsureKeypackages`) drive; the settings page's manual "refresh keys"
    // reuses it (devices.md § Cross-device MLS group-state sync, slice 6 — the
    // legacy standalone `mls_init_engine` plane is retired). Best-effort +
    // idempotent, off the builder's critical path. Ordering matters twice on
    // web (in-memory engine, the nest replica IS the persistence): mint AFTER
    // the restore (into the restored provider, not a to-be-replaced one), and
    // schedule the replica save right after so the fresh private init keys
    // survive a reload — an unsaved mint would leave peers holding key packages
    // whose init keys no longer exist anywhere.
    // The chain outlives the build, so it checks too — the build has settled by
    // then and stays wanted, but a switch can still land between the two mints.
    void built
      .ensureKeypackages(KEYPACKAGE_TARGET)
      .then(() => (stillOurs() ? built.ensureLastResortKeypackage() : undefined))
      .then(() => scheduleMlsSave())
      .catch((e) => {
        logMessage('warn', 'fauna_web::conversations', `login keypackage replenish failed: ${e}`);
      });
    return built;
  };
  // The engine-role refusal above already clears the memo, with the reasoning
  // that generalizes to EVERY way this build can fail: "so a later attempt can
  // win … instead of being served this same rejection forever". A promise memo
  // caches a rejection as durably as a value, and this builder has many more
  // failure paths than the role lock — `ensureWasm()`, the manager
  // construction, `restoreMlsState`, the folder passes. Without this any one of
  // them leaves conversations permanently unbuildable for the page's life, and
  // the actor-scoped drop cannot save it (it runs on an identity CHANGE, not on
  // the same actor re-entering the route). `=== guarded` is what keeps the
  // actor-changed throw from clearing a slot the drop has since refilled with
  // the INCOMING actor's build — the hazard that throw's own comment names,
  // and why it deliberately does not clear by hand. Pinned by
  // `singleton-build-memo-contract.test.ts`.
  //
  // ⚠ AND THE OTHER HALF, which none of the above reaches: a build that never
  // SETTLES is memoized exactly as durably as a rejected one, and `.catch`
  // never fires for a promise with no terminal state. This is the longest build
  // in the SPA — the MLS restore and the folder passes ride it — so it has the
  // most wasm task-deaths to lose its own deadlines to. `guardSingletonBuild`
  // adds the external 45 s settle deadline that survives the task's death and
  // routes into this same clear — and hands the build the `stillWanted` its
  // tail checks, because a deadline can abandon a build it cannot stop.
  const guarded = guardSingletonBuild('conversations manager', build, () => {
    if (managerPromise !== guarded) return;
    managerPromise = null;
    // Retract the slot with the memo (`feed.ts` says why) — and here it has
    // teeth: this build installs BEFORE its long tail, so it is the one most
    // likely abandoned after installing, and `startReceivePoll` rebuilds only
    // on an empty slot. Left filled, the receive rail would pump the abandoned
    // engine for the rest of the page's life. `engineRole` stays: it is the
    // account's, and the replacement build writes under it.
    manager = null;
  });
  managerPromise = guarded;
  return guarded;
}

/** Tear down the singleton (`identity.logout()` calls this). Without it a
 *  soft-nav sign-out → sign-in-as-a-different-identity within one page load
 *  (module state survives; sign-out never reloads) keeps rendering the
 *  PREVIOUS actor's `ConversationsManager` — its threads, snippets, and MLS
 *  state — to the NEW actor, since `getConversationsManager` returns the built
 *  `managerPromise` unconditionally regardless of the identity that built it.
 *  `mocksInstalled` resets alongside since it tracks whether the e2e mock
 *  backends were installed on THIS singleton instance, not on identity. The
 *  session-lifetime `startReceivePoll` loop already re-checks `!manager` each
 *  tick and rebuilds against whatever identity is current, so no separate
 *  restart is needed. The next `getConversationsManager()` call builds fresh. */
export function resetConversationsManager(): void {
  manager = null;
  managerPromise = null;
  mocksInstalled = false;
  conversationsSnapshot.set(null);
  conversationThreads.set([]);
  // The banner log is this actor's, like the threads above: the rebuilt manager
  // carries a fresh `MessageNotificationTracker`, so the incoming identity's
  // restored threads seed silently rather than raising a banner apiece, and the
  // outgoing identity's fired list must not be read as the new one's.
  resetMessageBanners();
  // Hand the MLS-writing role back. Load-bearing on the actor-switch path this
  // function exists for: the role is keyed per account, so a tab that kept the
  // PREVIOUS actor's role would both hold a lock nothing in this tab still
  // writes under, and lock a second tab out of an account this one has left.
  // The browser also releases it for free if the tab simply closes.
  engineRole?.release();
  engineRole = null;
  engineRoleActor = null;
  engineRoleDenied.set(false);
  // The account runtime is the departing account's too: its store and its
  // Web Lock go with the manager it registered its seams on. The plain stop —
  // this reset fires on every identity change and cannot know why; a sign-out
  // has already run its own stop (`identity.logout()`), so this one finds
  // nothing running.
  void stopAccountRuntime('account-switch');
}

// Self-registered, like the feed manager's — see `actorScope.ts` for why the
// registration lives beside the state instead of in the switch handlers.
registerActorScopedReset(resetConversationsManager);

// Self-address self-heal — the web leg of `conversations.md` § State & data
// shape → *Self-address: live, never baked*. The manager is built as soon as
// `secretHex` exists (MLS/DM delivery must not wait on identity resolution), so
// its SMTP From may still be `''` in the login race window — and a server-side
// handle rename changes the address mid-session. This one subscription pushes
// every identity-state landing (login `refreshFromServer`, background refresh,
// rename) into the manager's live cell, which both rails read at use time; a
// manager built mid-race therefore starts sending the moment the address lands,
// instead of refusing `no_handle` until the next full logout. While unresolved,
// `selfAddressFor` stays `''` and the § Errors & edge cases local refusal is
// the floor. A null identity is logout teardown — `resetConversationsManager`
// (above) owns that; nothing to push. Registered once, module-lifetime, same
// idiom as the reset registration.
identity.subscribe((id) => {
  if (manager && id?.secretHex) {
    manager.setSelfAddress(selfAddressFor(id));
  }
});

const PROD_POLL_MS = 30_000;
const E2E_POLL_MS = 2_000;
const PAGE_LIMIT = 50;
// Login-time key-package pool target — the value linux (`KEYPACKAGE_TARGET`)
// and windows (`EnsureKeypackages(20)`) replenish to. Exported for the settings
// page's manual "refresh keys", which drives the same manager surface.
export const KEYPACKAGE_TARGET = 20n;

/** An explicit e2e cadence override, in milliseconds — `null` when unset. The web
 *  twin of native's `FAUNA_CONV_POLL_SECS` env var (`transport.md` § Push events →
 *  the conv-rail probe note), and the reason it exists: the agent cadence below
 *  makes the backstop ticker deliver within any generous budget, so a *push*-arm
 *  test on this rail passes whether or not the push arm is alive. Muting the
 *  ticker (a huge interval) leaves the push arm as the only trigger, which is what
 *  makes such a test able to fail. Written only by `setConvPollSecs`, which the e2e
 *  automation surface owns; production never sets it. */
let pollOverrideMs: number | null = null;

/** The backstop sleep currently in flight, so `setConvPollSecs` can **re-arm** it
 *  rather than let an already-scheduled wake fire on the old cadence. Without the
 *  re-arm a mute would leave a sleep of up to the previous interval pending, and a
 *  ticker sweep could still land after the event the test is attributing to the
 *  push arm — a wall-clock race of exactly the shape convention 14 forbids. */
let pollWake: { timer: ReturnType<typeof setTimeout>; resolve: () => void } | null = null;

/** The Playwright e2e bridge injects `window.__faunaTestAgent` before page
 *  scripts; a faster cadence keeps the receive round-trip test off the
 *  production interval (mirrors native's `FAUNA_CONV_POLL_SECS=2`). Re-read
 *  each tick so a slightly-late agent injection still speeds the loop up.
 *  Exported so the subscriptions author pump (`subscriptionsAuthor.ts`) shares
 *  the one web receive cadence — the NEXT's "mirror the conv poll cadence". */
export function pollIntervalMs(): number {
  if (pollOverrideMs !== null) return pollOverrideMs;
  const hasAgent =
    __FAUNA_E2E_AUTOMATION__ &&
    typeof window !== 'undefined' &&
    (window as unknown as { __faunaTestAgent?: unknown }).__faunaTestAgent != null;
  return hasAgent ? E2E_POLL_MS : PROD_POLL_MS;
}

/** Override the receive rail's backstop cadence — `secs = null` restores the
 *  default. Governs the whole web receive cadence exactly as native's
 *  `FAUNA_CONV_POLL_SECS` governs the whole `start_receive_loop` ticker (both
 *  rails, plus the subscriptions-author pump that shares this interval).
 *
 *  **Takes effect on the sleep already in flight**, not merely on the next one:
 *  the pending wake is cancelled and re-armed at the new interval, so once this
 *  returns the next ticker sweep is provably a full new interval away. That is
 *  what lets a caller mute the ticker and then treat any later delivery as the
 *  push arm's, with no wall-clock assumption.
 *
 *  Imported only by `$lib/e2e-automation` (testing.md § convention 15), so a
 *  production bundle tree-shakes it out with the rest of the surface. */
export function setConvPollSecs(secs: number | null): void {
  pollOverrideMs = secs === null ? null : Math.max(0, secs) * 1000;
  if (pollWake) {
    clearTimeout(pollWake.timer);
    const { resolve } = pollWake;
    pollWake = {
      timer: setTimeout(() => {
        pollWake = null;
        resolve();
      }, pollIntervalMs()),
      resolve,
    };
  }
}

// ── Draft persistence (v2) — the compose-change SAVE trigger ─────────────────
//
// The conversations page calls `scheduleDraftSave()` from its mutator chokepoint
// (`run()`); we coalesce a burst of keystrokes into one debounced
// `manager.saveDrafts()` (snapshot → seal → `fauna.drafts.put`, all in wasm —
// reserved-folders.md § Drafts Sync). The put is fire-and-forget: a transient failure is
// logged, never surfaced on the page (a not-yet-synced draft is not a user-facing
// error). The restore-on-launch half lives in `getConversationsManager` above.

// Under the Playwright e2e agent, debounce far shorter so a restart round-trip
// test (`test_conversations_draft_persistence`) needn't wait the production
// window — mirrors `pollIntervalMs()`'s `__faunaTestAgent` test cadence.
const DRAFT_SAVE_DEBOUNCE_E2E_MS = 150;
let draftSaveTimer: ReturnType<typeof setTimeout> | null = null;

function draftSaveDebounceMs(): number {
  const hasAgent =
    __FAUNA_E2E_AUTOMATION__ &&
    typeof window !== 'undefined' &&
    (window as unknown as { __faunaTestAgent?: unknown }).__faunaTestAgent != null;
  return hasAgent ? DRAFT_SAVE_DEBOUNCE_E2E_MS : autosaveDebounceMs();
}

/** Debounced persist of the conversations drafts after a compose change
 *  (reserved-folders.md § Drafts Sync). Coalesces a burst of edits into one
 *  `fauna.drafts.put`; fire-and-forget (errors logged, never surfaced). */
export function scheduleDraftSave(): void {
  if (draftSaveTimer) clearTimeout(draftSaveTimer);
  draftSaveTimer = setTimeout(() => {
    draftSaveTimer = null;
    if (!manager) return;
    void manager.saveDrafts().catch((e) => {
      logMessage('debug', 'fauna_web::conversations', `save drafts failed (transient): ${e}`);
    });
  }, draftSaveDebounceMs());
}

/** Force an immediate save, bypassing the debounce — the leave-door flush
 *  (`reserved-folders.md` § The leave-flush promise, row 481), called from the
 *  root layout's `visibilitychange`/`pagehide` handlers. Best-effort like every
 *  such handler on the web platform (the goal doc's own wording: a browser
 *  grants no reliable async work after either event) — `saveDrafts()` is
 *  fire-and-forget here exactly as the debounced call above is; there is
 *  nothing to block on that the browser would honor. */
export function flushDraftsNow(): void {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  if (!manager) return;
  void manager.saveDrafts().catch((e) => {
    logMessage('debug', 'fauna_web::conversations', `leave-flush drafts failed: ${e}`);
  });
}

// ── Cross-device MLS state sync — the mutation SAVE trigger ───────────────────
//
// The MLS-state twin of `scheduleDraftSave` above (devices.md § Cross-device MLS
// group-state sync). Every state-mutating event routes through
// `refreshConversations()` — the web's "after every mutator + each receive-poll
// page" chokepoint, which is the observer-driven autosave the linux leg rides
// (`attach_replica_autosave`) — and that schedules this. We coalesce a burst into
// one debounced `manager.saveMlsState()` (snapshot provider + per-channel history →
// seal → `fauna.mls.put`, all in wasm). Fire-and-forget: a transient failure is
// logged, never surfaced.
//
// This debounce is the steady-state coalescer, NOT the durability guarantee: the
// thread store is in-memory and is *seeded from* the replica at launch, so anything
// missing durable storage before a reload is gone — and a sender cannot MLS-decrypt
// its own application messages. What closes that window is `devices.md` § Durability
// rules **Rule 3 (durable-before-done)**: `manager.send` (shared Rust, wasm included)
// awaits a `history/<ch>` CAS-save after appending the own message through the
// `HistoryPersist` seam `restore_and_wire` injects — so a reload at any moment after
// the send action returns loses nothing, and this debounce only coalesces the
// remaining (log-reconstructible or retryable) churn.
//
// The wasm launch gate keeps it a no-op until `restoreMlsState` succeeds, so it can
// never clobber the real replica. The restore-on-launch half lives in
// `getConversationsManager`.

let mlsSaveTimer: ReturnType<typeof setTimeout> | null = null;

/** Debounced persist of the cross-device MLS state replica after an engine/store
 *  mutation (send, inbound-fold, membership). Coalesces a burst into one
 *  `fauna.mls.put` pair; fire-and-forget (errors logged, never surfaced). Shares
 *  the draft-save debounce cadence (`draftSaveDebounceMs`). */
export function scheduleMlsSave(): void {
  if (mlsSaveTimer) clearTimeout(mlsSaveTimer);
  mlsSaveTimer = setTimeout(() => {
    mlsSaveTimer = null;
    if (!manager) return;
    void manager.saveMlsState().catch((e) => {
      logMessage('debug', 'fauna_web::conversations', `save mls state failed (transient): ${e}`);
    });
  }, draftSaveDebounceMs());
}

/** Re-read the manager snapshot into both reactive stores. Called after every
 *  mutator (compose, send, inject, membership, …) and each receive-poll page —
 *  the snapshot-after-call reactivity contract (no observer→JS callback, same as
 *  the wasm admin machines). `message_count` / `channel_id_hex` need a
 *  `threadDetail` read per thread (the `ThreadSummary` carries neither), mirroring
 *  linux `build_conversation_threads_state`. */
/** The moderation queue's **local half** — this session's post-decrypt local
 *  detections, read straight off the shared `LocalDetectionStore` the manager owns.
 *  The Rust receive loop classifies each just-decrypted incoming message and writes
 *  them there; the decrypted body itself never crosses into JS (`moderation.md`
 *  § State & data shape — the detections are held client-side, never round-tripped
 *  through the nest). Session-scoped, exactly like the natives'.
 *
 *  Empty when no manager has been built yet — nothing has been received, so nothing
 *  can have been classified. It deliberately does **not** build one: opening Settings
 *  must not spin up an MLS engine. */
export function moderationLocalDetections(): LocalDetection[] {
  if (!manager) return [];
  return manager.moderationLocalDetections() as LocalDetection[];
}

/** Per-channel counts of inbound MLS commits this tab has folded in, as a
 *  `{channel_hex: count}` object — the web leg of the twin-device barrier
 *  (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY` owns the cross-app contract).
 *
 *  `{}` when no manager has been built yet: nothing has been received, so nothing
 *  can have been folded in. Like `moderationLocalDetections` it deliberately does
 *  NOT build one — reading a barrier must not spin up an MLS engine. That empty
 *  object is a legitimate zero; an app with no leg at all publishes `null`
 *  instead, which the consumer refuses loudly rather than reading as zero. */
export function mlsFoldedCommits(): Record<string, number> {
  if (!manager) return {};
  return manager.mlsFoldedCommits() as Record<string, number>;
}

/** Drop the local-detection row for `contentId` once the user trains a correction on
 *  it — the row is corrected, so it leaves the queue. `false` for a server row (never
 *  in this store) or when no manager exists. */
export function moderationRemoveLocalDetection(contentId: string): boolean {
  if (!manager) return false;
  return manager.moderationRemoveLocalDetection(contentId);
}

/** The retained decrypted plaintext body of one message, by the message-id string a
 *  local-detection `QueueRow.content_id` carries — wasm twin of native's
 *  `ConversationsManager::message_body`. `undefined` once the message has aged out of
 *  the thread store, or when no manager exists. */
function messageBody(messageId: string): string | undefined {
  if (!manager) return undefined;
  return manager.messageBody(messageId) ?? undefined;
}

/** Feed one train event to the **client-side** sealed tier-1 model write
 *  (`trainSpamModelClient`) for text the client already holds. Fire-and-forget in the
 *  sense that a `sealed: false` (older nest / mail not enabled) outcome is silently
 *  swallowed — for client-only content there is no server row to train against, so the
 *  caller's flag removal alone is the correction, exactly as before this write path
 *  existed. Mirrors linux `FaunaClient::train_spam_model_client_side`. */
async function trainSpamModelClientSide(secretHex: string, text: string, isSpam: boolean): Promise<void> {
  if (!settingsMachine) settingsMachine = await mailSettingsMachine(secretHex);
  await settingsMachine.trainSpamModelClient(text, isSpam);
}

/** Apply a `train-correction-button` click for one merged moderation queue row — the
 *  tier-1 spam-model client-write switch (`mail-spam.md` § Encrypted-mode interaction).
 *  A **server** row (`source: "Server"`, carries an enforcement `action`) submits a ham
 *  training correction: a sealed pre-check, then fetch the post body (`fauna.posts.get`
 *  + the shared `postBodyText` wasm decode) and write via `trainSpamModelClient`;
 *  `sealed == false` (older nest / mail not enabled) or an empty/fetch-miss body falls
 *  back to the existing server-side `fauna.moderation.train`. A **local** row
 *  (`source: "Local"`, a client-only post-decrypt detection) has no nest obligation to
 *  train against — the content is client-only (MLS-sealed at rest) — so the correction
 *  removes the false-positive flag from the session store, and when the sealed write
 *  path is available it *additionally* feeds the ham correction to the tier-1 model
 *  with the SAME retained decrypted text the classifier saw. Mirrors linux
 *  `FaunaClient::{correct_moderation_row,train_moderation_flow}`. */
export async function trainModerationCorrection(
  secretHex: string,
  contentId: string,
  source: 'Server' | 'Local',
): Promise<void> {
  if (source === 'Local') {
    const text = messageBody(contentId);
    moderationRemoveLocalDetection(contentId);
    if (text) await trainSpamModelClientSide(secretHex, text, false);
    return;
  }
  if (!settingsMachine) settingsMachine = await mailSettingsMachine(secretHex);
  if (await settingsMachine.sealedSpamWriteAvailable()) {
    const bytes = await postsGet(secretHex, contentId);
    const text = postBodyText(bytes);
    if (text) {
      const outcome = (await settingsMachine.trainSpamModelClient(text, false)) as { sealed: boolean };
      if (outcome.sealed) return;
    }
  }
  await moderationTrain(secretHex, contentId, 'ham');
}

export function refreshConversations(): void {
  if (!manager) return;
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const snap = manager.snapshot() as any;
    conversationsSnapshot.set(snap);
    unopenableMailCount.set(manager.unopenableMailCount());
    // One call over `fauna_conversations::state_json::conversation_threads_json`
    // — the same function tui and linux publish — rather than hand-rebuilding
    // the row shape (per-thread `threadDetail`/`channelHex` joins,
    // `participant_actor_ids`' Fauna-rail-vs-null mapping) here in TS; that
    // assembly lives ONCE in shared Rust, tested by
    // `libs/fauna-conversations/tests/state_json_tests.rs`, and this is its
    // wasm twin (mirrors `mlsFoldedCommits`'s exact shape).
    const rows = manager.conversationThreadsJson() as ConversationThreadRow[];
    conversationThreads.set(rows);
    // The member-side succession report, off the same shared-Rust-renders-it
    // rule as the rows above. Refreshed here rather than on its own timer
    // because this function is already the post-receive chokepoint, and the
    // report is three field reads (convention 11's second corollary — the
    // state provider is the ack path, so it may never round-trip).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    successionWitness.set(manager.successionWitnessStateJson() as any);
    // A thread read owes the nest `\Seen` writes: run the mail pass now, which
    // ends in sending them, so another device hears of the read promptly — the
    // web twin of the native session's mail-read poke. This is the chokepoint
    // every page action and every receive ends in.
    if (manager.hasOwedMailSeen()) wakeMailRail();
  } catch (e) {
    console.warn('conversations snapshot failed:', e);
    logMessage('warn', 'fauna_web::conversations', `conversations snapshot failed: ${e}`);
  }
  // The running tab's new-message OS banner (`conversations` outcome 11) — one
  // tracker tick per snapshot change, which is what this chokepoint is. The
  // when/for-whom decision is the shared `MessageNotificationTracker` behind
  // `newMessageBanners()`; `$lib/message-banner` only fires. Outside the try
  // above deliberately: a snapshot read that threw must not also cost the tick,
  // since the tracker is stateful and a skipped tick mis-seeds every later one.
  raiseNewMessageBanners(manager);
  // Persist the cross-device MLS state replica after any state change — this is the
  // web's post-mutation chokepoint (called after every mutator + each receive-poll
  // page), so it is the natural autosave trigger (devices.md § Cross-device MLS
  // group-state sync). Debounced + `_if_changed`-deduped + launch-gated, so pure
  // read-refreshes (select/sort/search) and pre-restore ticks are cheap no-ops.
  scheduleMlsSave();
  // The feed's room options are a projection of this plane, so its change tick
  // is their trigger too (feed.md § Encryption at rest → *Room-restricted — the
  // app half*).
  syncFeedRoomPosts();
}

// ── Room-restricted posts: the feed's room-post seam ─────────────────────────
//
// The feed opens and seals room-restricted posts through THIS manager's FaunaMls
// rail and offers the user's rooms from its threads, so the two singletons meet
// here — and here rather than in `feed.ts`, which this module already imports
// (the reverse import would be a cycle). Called from the post-receive chokepoint
// above and by the Feed page once its own manager is built, so whichever
// singleton lands second installs it. The native twins install the
// conversations session where the feed manager and the session meet (tui's
// `session.rs`, linux's `conv_backend.rs`).

/** The (feed, conversations) pair the seam was last installed on — a rebuilt
 *  manager on either side (an actor switch) installs afresh. */
let roomSeamPair: { feed: WasmFeedManager; conv: WasmConversationsManager } | null = null;

/** Install the room-post seam on the feed manager when both singletons exist,
 *  then re-read the composer's rooms, re-reading the feed snapshot only when
 *  the list changed. A no-op until both are built; never builds either. */
export function syncFeedRoomPosts(): void {
  const feed = feedManagerIfReady();
  const conv = manager;
  if (!feed || !conv) return;
  try {
    if (roomSeamPair?.feed !== feed || roomSeamPair?.conv !== conv) {
      // `false` = a manager built without the FaunaMls rail: no room keys to
      // offer, so every room post stays locked — the honest state.
      if (!feed.setRoomPostKeys(conv)) return;
      roomSeamPair = { feed, conv };
    }
  } catch (e) {
    logMessage('warn', 'fauna_web::conversations', `room-post seam install failed: ${e}`);
    return;
  }
  void feed
    .refreshOwnRooms()
    .then((changed: boolean) => {
      if (changed) refreshFeed();
    })
    .catch((e: unknown) => {
      logMessage('debug', 'fauna_web::conversations', `own rooms refresh failed: ${e}`);
    });
}

/** Returns true once the manager holds the account's standing recipient key set
 *  (mail enabled). The MSEK never crosses into JS — the mail-settings machine
 *  derives the generation-scoped secrets in wasm and returns just those. */
async function ensureRecipientSecret(secretHex: string): Promise<boolean> {
  if (!manager) return false;
  if (manager.hasRecipientSecret()) return true;
  if (!settingsMachine) settingsMachine = await mailSettingsMachine(secretHex);
  // The complete standing key set — current + grace generations (X25519 +
  // ML-KEM halves each), the same set the MDA opens with — so pre-rotation
  // mail opens here exactly as on every native app.
  const keypairs = (await settingsMachine.recipientStandingKeypairs()) as Uint8Array;
  if (keypairs.length > 0) {
    manager.setRecipientKeypairs(keypairs);
    // Content-sealing epochs: thread the client's mail-epoch roots (current +
    // grace generations, derived from the MSEK in wasm) so the epoch opener can
    // read mail sealed under a mail epoch key once the write flip is thrown.
    // Empty ⇒ standing-only open, unchanged. The MSEK never crosses into JS.
    const epochRoots = (await settingsMachine.mailEpochRoots()) as Uint8Array;
    manager.setMailEpochRoots(epochRoots);
    // The account's own recipient PUBLIC key — what the bridged rail seals the
    // user's own copy of a sent message to (`pollBridgedOnce`).
    manager.setRecipientPublicKey((await settingsMachine.recipientPublicKey()) as Uint8Array);
    return true;
  }
  return false;
}

/** Mark a received conversation message as spam — the live `Insert` consumer
 *  (`dm-message-mark-as-spam-button`; `mail-spam.md` § Wire shapes `put_spam_model`
 *  `history_op`). Trains the per-user sealed spam model over the retained decrypted
 *  `body` AND writes a sealed training-history row atomically
 *  (`train_spam_model_client_mail` → `apply_spam_model_write(Train, Some(Insert))`),
 *  mirroring linux `FaunaClient::mark_message_spam`. `messageId` is the opaque
 *  `MessageSnapshot.message_id` string — encoded to bytes, never decoded nest-side
 *  (matches linux's `message_id.into_bytes()`). `subject` falls back to a body
 *  snippet inside the shared façade when the message carries no subject line. The
 *  mailbox is `INBOX` — a received conversation message has no IMAP mailbox; it is
 *  display metadata (`{subject} · INBOX`) on the sealed row only. Resolves `true`
 *  when a sealed row was written; `false` (silent, no fallback) when the nest lacks
 *  `spam-model-sealed-at-rest` or mail isn't enabled — a conversation message is
 *  client-only encrypted content the nest cannot read, so there is no server-train
 *  fallback (unlike the moderation-queue server-row correction). */
export async function markMessageSpam(
  messageId: string,
  body: string,
  subject: string,
): Promise<boolean> {
  if (!body.trim()) return false;
  const id = get(identity);
  if (!id?.secretHex) return false;
  if (!settingsMachine) settingsMachine = await mailSettingsMachine(id.secretHex);
  const outcome = (await settingsMachine.trainSpamModelClientMail(
    body,
    true,
    new TextEncoder().encode(messageId),
    'INBOX',
    subject,
  )) as { sealed: boolean; sample_count: number };
  return outcome.sealed;
}

/** Flatten a `body_ref`'s ordered chunk hashes into the 32-bytes-each buffer
 *  `resolveMailBodyRef` takes. Each hash arrives as a `Uint8Array` or a plain
 *  `number[]` depending on the decoder path, so coerce each; the shared Rust
 *  resolver re-splits on 32 and refuses anything that isn't a whole number of
 *  hashes, so the width rule stays in one place. */
function concatChunkHashes(hashes: (Uint8Array | number[])[]): Uint8Array {
  const parts = hashes.map((h) => new Uint8Array(h));
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** Drain one server-side mailbox feed to its end, ingesting each sealed record
 *  into the shared manager (which dedups by RFC-5322 Message-ID across feeds, so
 *  INBOX and Sent never double-ingest). `fetchPage` returns one page given the
 *  current cursor; the advanced cursor is returned for the caller to persist.
 *  The web twin of linux `mail_sink.rs`'s per-feed `poll_inbound_mail` drive. */
async function drainFeed(
  secretHex: string,
  fetchPage: (afterUid: number) => Promise<InboxFetchReply>,
  afterUidStart: number,
  score: boolean,
  mailbox: MailFeed,
): Promise<number> {
  let cursor = afterUidStart;
  for (;;) {
    const reply = await fetchPage(cursor);
    // The flag-change baseline rides every INBOX page; the shared manager keeps
    // the first one (mail-app-surface.md § Read state). Sent's modseq numbers
    // another mailbox and is never offered.
    if (mailbox === 'inbox') manager!.noteInboxPage(reply?.highest_modseq ?? 0);
    const messages = reply?.messages ?? [];
    for (const m of messages) {
      try {
        // A message whose stored outer envelope overflows the 2 MiB frame
        // arrives with an empty `sealed_envelope` and a `body_ref` naming the
        // sealed bytes on the byte plane; fetch them back into the exact
        // envelope first, and from there ingest is identical to the inline case
        // (smtp-server.md § Message size limits). Under the frame there is no
        // `body_ref` and this is a no-op.
        const sealedEnvelope = m.body_ref
          ? await manager!.resolveMailBodyRef(
              concatChunkHashes(m.body_ref.chunk_hashes),
              m.body_ref.total_bytes,
            )
          : new Uint8Array(m.sealed_envelope);
        // Coerce to Uint8Array — serde-wasm-bindgen may hand back bytes as a
        // plain array under the json-compatible serializer.
        manager!.ingestSealedInbound(
          m.uid,
          new Uint8Array(m.message_id),
          // `internal_date_secs` is an i64 param → wasm-bindgen wants a bigint.
          BigInt(m.internal_date),
          // Content-sealing-epochs seal instant (`stored_at`, epoch seconds) —
          // the epoch-classification basis. `f64` param → plain number.
          m.stored_at,
          sealedEnvelope,
          // On-device spam scoring: pass the per-message flags (for the
          // `$FaunaSpamScored` watermark skip) and score only the INBOX feed
          // (never Sent). The manager scores in-place when scoring is enabled.
          m.flags ?? [],
          score,
          mailbox,
          // The mailed-REPLY merge an INBOX record also takes (inside wasm — the
          // bytes and the MSEK never cross into JS): the seed for its key
          // context, and the merge's clock (the wasm-time discipline).
          secretHex,
          Math.floor(Date.now() / 1000),
        );
        // The cursor moves only once the record is behind us. A record that
        // does not open under the account's key set is NOT a throw: the wasm
        // ingest skips it, records it on the shared manager (the page shows
        // the count), and returns normally, so the drain continues past it —
        // the same rule every native app applies (`mail-app-surface.md`
        // § Inbound client receive → *Unopenable records*).
        if (m.uid > cursor) cursor = m.uid;
      } catch (e) {
        // What CAN throw is transient — the body-ref resolve over the byte
        // plane, a decode fault in the reply — so this page is abandoned with
        // the cursor still BEFORE this record, and the next tick re-fetches
        // from here. Advancing past it would silently drop the message.
        console.warn('ingest inbound failed:', e);
        logMessage('warn', 'fauna_web::conversations', `ingest inbound failed: ${e}`);
        refreshConversations();
        return cursor;
      }
    }
    refreshConversations();
    if (!reply?.more) break;
  }
  return cursor;
}

async function pollOnce(secretHex: string): Promise<void> {
  if (!manager) return;
  if (!(await ensureRecipientSecret(secretHex))) return;
  // On-device spam scoring: enable/refresh the scorer BEFORE the INBOX drain so
  // each message is scored at ingest (best-effort — a failure here must never
  // degrade mail delivery, so it just disables scoring this pass).
  await prepareSpamScoring(secretHex);
  // INBOX — the PRIMARY feed (received mail). `score = true`: the manager scores
  // each un-watermarked message at ingest.
  afterUid = await drainFeed(
    secretHex,
    (cur) => emailInboxFetch(secretHex, cur, PAGE_LIMIT),
    afterUid,
    true,
    'inbox',
  );
  // Apply the scored-this-pass disposition (watermark + move spam to Junk) in
  // one RPC, after the INBOX drain. Best-effort — isolated from delivery.
  await flushSpamScoring(secretHex);
  // Sent — SECONDARY, best-effort: mail the user sent from an external MUA leaves
  // a server-side Sent copy sealed to the sender's own MSEK key, so it threads
  // into the unified view as a sent bubble. A Sent-side failure (e.g. a transport
  // error on `fauna.email.sent.fetch`) must NOT
  // degrade INBOX delivery, so it's isolated + logged at debug. The manager
  // dedups by Message-ID across both feeds; the cursors are independent. `score
  // = false`: Sent mail is never per-user-scored.
  try {
    afterUidSent = await drainFeed(
      secretHex,
      (cur) => emailSentFetch(secretHex, cur, PAGE_LIMIT),
      afterUidSent,
      false,
      'sent',
    );
  } catch (e) {
    logMessage('debug', 'fauna_web::conversations', `sent feed drain failed (best-effort): ${e}`);
  }
  // Mail attachments the store's budget evicted and a render has since asked
  // for — re-read from their records after both drains (the web twin of the
  // native mail sweep's refill step; conversations.md § Attachments →
  // Retention). Web has no in-process poke, so a miss waits for this tick.
  await refillMailAttachments(secretHex);
  // Mail read state (conversation-read-state.md § Mail: `\Seen` is the
  // marker): the `\Seen` writes reads have owed go out in one batch, and flag
  // changes made elsewhere come in — the same shared driver the native receive
  // loop's mail sweep runs, after the drain so the first INBOX page has named
  // the change baseline. Never rejects.
  await manager!.syncMailReadState();
}

/** The server-side mailbox a mail record's UID counts in — the shared
 *  `MailFeed`'s serialized spelling. INBOX and Sent number UIDs independently. */
type MailFeed = 'inbox' | 'sent';

/** One mail record whose evicted attachments a render has missed, as
 *  `takeWantedMailRecords` hands it out — passed back whole to
 *  `refillMailRecord` / `mailRecordGone`. */
interface WantedMailRecord {
  record: { mailbox: MailFeed; uid: number };
  blobHashes: string[];
}

/** Re-read each wanted mail record by `(mailbox, uid)` and hand it to the shared
 *  refill (`backends::smtp::refill_mail_record_attachments`), which re-parses its
 *  MIME and caches the wanted parts through the store's one door. One record
 *  after `uid - 1` on the record's own feed is that record while it exists;
 *  anything else means the mailbox no longer holds it (moved to Junk, expunged),
 *  and its handles are forgotten so they render declared. A referenced body
 *  (`body_ref`) is resolved exactly as `drainFeed` resolves it — an evicted
 *  attachment is usually a large one. A failure leaves the handles remembered,
 *  so the next render's miss asks again. Never rejects. */
async function refillMailAttachments(secretHex: string): Promise<void> {
  const mgr = manager;
  if (!mgr) return;
  let wanted: WantedMailRecord[];
  try {
    wanted = mgr.takeWantedMailRecords() as WantedMailRecord[];
  } catch (e) {
    logMessage('debug', 'fauna_web::conversations', `take wanted mail records failed: ${e}`);
    return;
  }
  let refilled = 0;
  for (const want of wanted) {
    const { mailbox, uid } = want.record;
    try {
      const fetchPage = mailbox === 'sent' ? emailSentFetch : emailInboxFetch;
      const reply = await fetchPage(secretHex, Math.max(0, uid - 1), 1);
      const m = (reply?.messages ?? []).find((msg) => msg.uid === uid);
      if (!m) {
        mgr.mailRecordGone(want);
        continue;
      }
      const sealedEnvelope = m.body_ref
        ? await mgr.resolveMailBodyRef(
            concatChunkHashes(m.body_ref.chunk_hashes),
            m.body_ref.total_bytes,
          )
        : new Uint8Array(m.sealed_envelope);
      refilled += mgr.refillMailRecord(
        want,
        m.stored_at,
        sealedEnvelope,
      );
    } catch (e) {
      logMessage(
        'debug',
        'fauna_web::conversations',
        `mail attachment refill (${mailbox} uid ${uid}) failed; asked again on the next miss: ${e}`,
      );
    }
  }
  if (refilled > 0) refreshConversations();
}

/** Enable/refresh the manager's on-device INBOX spam scorer for this poll, or
 *  disable it (cold start / mail off). Fetches the caller's sealed per-user
 *  model + the admin-effective scoring policy; the manager unwraps the model in
 *  wasm (never in JS) and scores each INBOX message at ingest against the SAME
 *  effective threshold + knobs the MDA/nest use (mail-spam.md § Scoring
 *  placement). Best-effort: any failure disables scoring this pass rather than
 *  breaking the receive loop. An untrained actor (`fetchSpamModel` → null) is a
 *  cold start — no scorer, so no un-based watermarking. */
async function prepareSpamScoring(secretHex: string): Promise<void> {
  if (!manager) return;
  const id = get(identity);
  if (!id?.actorId) {
    manager.disableSpamScoring();
    return;
  }
  try {
    const fetched = await fetchSpamModel(secretHex, id.actorId);
    if (!fetched) {
      manager.disableSpamScoring();
      return;
    }
    const p = await getSpamScoringPolicy(secretHex);
    manager.enableSpamScoring(
      fetched.blob,
      p.spam_folder_threshold,
      p.bayesian_weight_milli,
      p.bayesian_min_samples,
      p.bayesian_full_confidence_samples,
      // Present only for a client-sealed stored model — the manager folds it
      // locally (the nest folds plaintext-stored models itself).
      fetched.baseline ?? undefined,
    );
  } catch (e) {
    manager.disableSpamScoring();
    logMessage(
      'debug',
      'fauna_web::conversations',
      `spam scoring prepare failed (best-effort, disabled this pass): ${e}`,
    );
  }
}

/** Apply the scored-this-pass disposition in one `apply_spam_disposition` RPC:
 *  watermark every scored message + move the spam subset INBOX→Junk. No-op when
 *  scoring was disabled or nothing scored. Best-effort — never break the loop. */
async function flushSpamScoring(secretHex: string): Promise<void> {
  if (!manager) return;
  try {
    const disp = manager.takeSpamDisposition() as { scoredUids: number[]; junkUids: number[] };
    if (disp.scoredUids.length > 0) {
      await applySpamDisposition(secretHex, disp.scoredUids, disp.junkUids);
      // Re-render to reflect the freshly-applied watermark state. The spam the
      // scorer moved to Junk was already kept out of the thread view at ingest
      // (`ingestSealedInbound` suppresses a junk verdict) — the client-side twin
      // of the MDA moving spam out of INBOX before the SELECT snapshot.
      refreshConversations();
    }
  } catch (e) {
    logMessage(
      'debug',
      'fauna_web::conversations',
      `spam disposition flush failed (best-effort): ${e}`,
    );
  }
}

/** Drive the FaunaMls (MLS DM) receive rail once: drain the durable inbox —
 *  joining + binding any group whose Welcome was queued while we were offline
 *  (`api-layers.md` § Inbox & Messaging layer 4) — then poll every bound channel
 *  for new ciphertext. Runs on both arms: the backstop ticker AND a
 *  `fauna.conversations.*` push wake (§ Receive pump). The native twin is
 *  `session.rs`'s ticker arm (`drain_once` → `poll_bound`) — same order, and for
 *  the same reason: the drain must bind a Welcome's channel *before* the same
 *  pass polls it, so pre-join history lands in one go. Ungated by the recipient
 *  secret: the MLS DM rail is independent of mail-enable. Self-contained error
 *  handling — a transient failure (disconnect / backend not yet active) is
 *  logged, never thrown, so it can't block the SMTP rail in the same pass. */
async function pollFaunaMlsOnce(): Promise<void> {
  if (!manager) return;
  try {
    // drainInbox first (queued Welcomes → group join, plus the folder contact
    // gate's auto-join arm), then pull channel ciphertext on the newly-bound +
    // existing channels; refresh after.
    await manager.drainInbox();
    await manager.pollConversations();
    // Folder channels are commit-only and bind no thread, so they sit outside
    // `pollConversations`'s bound-channel sweep and need their own drive — this is
    // how a member applies the owner's rotate-on-removal commit and advances to the
    // epoch the re-published content-key envelope is sealed under
    // (`mls-group-key-material.md` § Rotate-on-removal, liveness half). The web twin
    // of the native ticker's `poll_folder_feed` arm. Never rejects.
    await manager.pollFolders();
    // Scheduling channels are one-off iMIP deliveries that bind no thread, so they
    // sit outside the bound-channel sweep too: this is the web leg of the
    // mailbox-less CalDAV rail, where an invitation sent from another calendar is
    // MLS-sealed onto its own channel instead of arriving as mail
    // (`caldav-server.md` § Server-side auto-schedule, Half-1 — a mailbox-less user
    // runs no mail poll, so the drain has to be here). The web twin of the native
    // ticker's `poll_scheduling_feed` arm; without it the invite decrypts nowhere
    // and never reaches the Events page. `secretHex` rides in per call (the manager
    // holds no identity seed) and the timestamp from here (the wasm-time
    // discipline). Never rejects.
    const schedulingId = get(identity);
    if (schedulingId?.secretHex) {
      await manager.pollScheduling(schedulingId.secretHex, Math.floor(Date.now() / 1000));
    }
    refreshConversations();
  } catch (e) {
    logMessage(
      'debug',
      'fauna_web::conversations',
      `faunamls receive poll failed (transient): ${e}`,
    );
  }
}

// ── Receive pump — the web twin of the shared loop's `tokio::select!` ────────
//
// Every receive pass — ticker, push, reconnect or poke — funnels through ONE
// `ReceivePump` (`$lib/receive-pump`, which owns why: serialization over the
// single-threaded MLS engine, the cycle counters, and the pass ceiling that turns
// a never-settling pass into a reported stall instead of a silently dead rail).
// This module owns only the two rails it pumps and the arms that wake it.

/** True while the receive pump is stalled — a pass outran its ceiling, web's
 *  shape of a receive loop that died (`$lib/receive-pump`). The conversations
 *  page renders it on `error-message` directly under the engine-role refusal
 *  (`ui/conversations.md` § Errors & edge cases). */
export const receiveRailStalled: Writable<boolean> = writable(false);

/** The SMTP mail rail's pass — a no-op until mail is enabled (the recipient
 *  secret becomes derivable), so enabling mail after load is picked up without a
 *  reload. Self-contained error handling, as the pump requires. */
async function pollMailOnce(): Promise<void> {
  const id = get(identity);
  if (!id?.secretHex) return;
  try {
    await pollOnce(id.secretHex);
  } catch {
    // Transient (disconnect / mail not enabled) — the ticker retries.
  }
  await pollBridgedOnce(id.secretHex);
}

/** The bridged rail's pass — the rooms and inbox of every bridge serving the
 *  account, through the shared driver (`manager.pollBridged`; the web twin of
 *  the native loop's bridged sweep, `conversations.md` § Where logic lives →
 *  *The `Bridged` adapter*). It rides the mail rail's slot in the pump because
 *  it opens under the same recipient keys — but it is NOT gated on them: with
 *  mail off the rooms still list and paint, and only the inbox read waits.
 *  Never rejects. */
async function pollBridgedOnce(secretHex: string): Promise<void> {
  if (!manager) return;
  try {
    await ensureRecipientSecret(secretHex);
  } catch {
    // No keys this pass — the rooms still load below.
  }
  try {
    await manager.pollBridged();
    refreshConversations();
  } catch (e) {
    logMessage('debug', 'fauna_web::conversations', `bridged receive poll failed (transient): ${e}`);
  }
}

const receivePump = new ReceivePump({
  conv: pollFaunaMlsOnce,
  mail: pollMailOnce,
  ceilingMs: RECEIVE_PASS_CEILING_MS,
  onStalledChange: (stalled) => receiveRailStalled.set(stalled),
});

/** `[started, completed]` — read by the e2e bridge's state assembly. */
export function convReceiveCycles(): [number, number] {
  return receivePump.cycles();
}

/** `conv_receive_cycles.exit` (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`):
 *  `'stalled'` while the pump is stalled, else `null` — web's loop has no
 *  designed exit, it runs for the life of the tab. Read by the e2e bridge beside
 *  the counters. */
export function convReceiveExit(): 'stalled' | null {
  return receivePump.exit();
}

/** Run one receive cycle now — convention 14's `run_now` poke, the web twin of
 *  native `ConversationsSession::poke_receive_cycle`. Drives the *same* pump the
 *  backstop ticker drives, so a poked delivery is the real path.
 *
 *  Fire-and-forget by design: the barrier is `convReceiveCycles`, not this call.
 *  A poke that lands mid-pass is not lost — the cycle request survives the running
 *  pass and the pump runs a fresh counted cycle before it returns. */
export function convReceiveNow(): void {
  void receivePump.cycle();
}

/** Wake the FaunaMls (MLS DM) rail — the `ConvPushEvent::{Welcome,ChannelMessage}`
 *  arms of the native loop. Both kinds resolve to the same "poll now" nudge: the
 *  `ChannelMessage` payload is discarded natively too (`push_event_to_conv` maps it
 *  to a bare `ConvPushEvent::ChannelMessage`), and a Welcome's bytes are recovered
 *  from the durable inbox by `drainInbox` — so neither arm needs the payload, and
 *  the per-channel cursor dedups whatever a coalesced burst pulls. */
function wakeConvRail(): void {
  receivePump.wakeConv();
}

/** Wake the SMTP mail rail — the `ConvPushEvent::MailReceived` arm
 *  (`fauna.mail.received`; `smtp-server.md` § Inbound client receive → Arrival
 *  push). The per-mailbox `after_uid` cursor dedups. */
function wakeMailRail(): void {
  receivePump.wakeMail();
}

/** e2e-only: when true, this module's push AND reconnect arms are inert, leaving
 *  the backstop ticker + durable drain as the rail's only delivery path. The web
 *  twin of native's `FAUNA_E2E_SUPPRESS_CONV_PUSH`, which makes `conv_push_source`
 *  return `None` — and note *both* arms, because that `None` kills native's
 *  reconnect arm too (`subscribe_reconnects()` lives inside the same push source).
 *
 *  Written only by `setConvPushSuppressed`, which the e2e automation surface owns;
 *  production never sets it, so both arms are always live in a real deployment. */
let convPushSuppressed = false;

/** Suppress (or restore) this module's push + reconnect arms — the exact pair
 *  native's `FAUNA_E2E_SUPPRESS_CONV_PUSH` switches off.
 *
 *  Its purpose is the mirror of `setConvPollSecs`: with the push arm inert, a
 *  delivery can only have come from the durable inbox-apply drain the ticker
 *  drives, which is what lets the layer-5 web receive proof
 *  (`api-layers.md` § Inbox & Messaging) actually fail if that drain breaks.
 *  Registration happens once at rail start, so this is a *flag* the registered
 *  handler reads rather than an unsubscribe — a test can flip it mid-session and
 *  restore it, which an unsubscribe could not.
 *
 *  Imported only by `$lib/e2e-automation` (testing.md § convention 15). */
export function setConvPushSuppressed(on: boolean): void {
  convPushSuppressed = on;
}

/** Registered once, app-wide, when the receive rail starts — NOT per page. A DM
 *  must be ingested while the user sits on the feed (the MLS state would otherwise
 *  fall behind the channel log), exactly as native's loop runs for the whole
 *  session, not per screen. Mirrors the three push arms of the shared loop. */
function subscribeReceivePushes(): void {
  onPushEvent((kind) => {
    if (convPushSuppressed) return;
    switch (kind) {
      case 'fauna.conversations.channel.message':
      case 'fauna.conversations.welcome.received':
        wakeConvRail();
        break;
      case 'fauna.mail.received':
      // A flag on an INBOX message changed elsewhere — the mail pass ends in
      // the flag-change drain (`ConvPushEvent::MailFlagsChanged`'s arm).
      case 'fauna.mail.flags_changed':
      // A bridged room changed — a deposit, a room upsert, the user's own
      // send from another device (`ConvPushEvent::BridgedChanged`'s arm). The
      // bridged pass rides the mail rail's slot.
      case 'fauna.bridges.push.conversation_changed':
        wakeMailRail();
        break;
    }
  });
}

/** The **reconnect arm** — the wasm twin of native's `ConvPushEvent::Reconnected`
 *  wake (`libs/fauna-client-conversations` selecting `subscribe_reconnects()`
 *  beside its push kinds; `transport.md` § Push events). A push is a transient
 *  broadcast: anything the nest tried to deliver while the socket was down is
 *  never re-broadcast, and only a *pull* recovers it — so on every reconnect we
 *  run the same full sweep the ticker does (both rails), rather than waiting up to
 *  a full backstop tick after every other surface has already re-pulled. Both
 *  rails funnel through the one receive pump, so this can never overlap a ticker or
 *  push pass on the single MLS engine, and each rail's cursor dedups a reconnect
 *  that races a tick (idempotent, exactly as native).
 *
 *  Registered once, app-wide, from `startReceivePoll` — not per page — because the
 *  receive rail runs for the whole session. `reconnectTick` is a `writable(0)`
 *  that fires once on subscribe, so the seed value is skipped (the feed/
 *  notifications/contacts idiom); the callback outlives the session, so it is
 *  never unsubscribed. */
function subscribeReconnectSweep(): void {
  let firstTick = true;
  reconnectTick.subscribe(() => {
    if (firstTick) {
      firstTick = false;
      return;
    }
    // Suppressed with the push arm, not separately: native's
    // `FAUNA_E2E_SUPPRESS_CONV_PUSH` returns `None` for the whole push source,
    // and `subscribe_reconnects()` lives inside it — so a suppressed native
    // session has no reconnect sweep either. Mirroring that keeps the two knobs
    // describing the same two configurations on every app.
    if (convPushSuppressed) return;
    // A reconnect sweeps every rail, so it is a full cycle for the counters —
    // native's `Reconnected` arm expands the same `full_sweep!` its ticker does.
    void receivePump.cycle();
  });
}

/** Start the app-wide receive rail — both rails, both arms. Idempotent (at most
 *  one loop runs). Started from the root layout once an identity is present.
 *
 *  This is the **ticker arm**: every `pollIntervalMs()` it wakes both rails
 *  unconditionally. It is the *backstop*, not the delivery path — a push is a
 *  hint that is dropped under broadcast lag and never replayed across a reconnect
 *  gap (`transport.md` § Push events), so the sweep must keep running even though
 *  the push and reconnect arms now make delivery prompt. Native holds the
 *  identical stance: its `DEFAULT_CONV_POLL_SECS = 30` ticker was *not* slowed when
 *  pushes landed, and our `PROD_POLL_MS` is the same 30s.
 *
 *  The **push arm** (`subscribeReceivePushes`) and the **reconnect arm**
 *  (`subscribeReconnectSweep`) are registered once, here, so they live as long as
 *  the session rather than as long as a page. The reconnect arm is what closes the
 *  gap the backstop ticker only *masks*: without it, after a socket flap MLS
 *  delivery waited up to a full tick while every other surface re-pulled at once
 *  (the latency an over-load-attributed real-MLS e2e red was tracing). */
export async function startReceivePoll(): Promise<void> {
  if (pollStarted) return;
  pollStarted = true;
  await ensureWasm();
  subscribeReceivePushes();
  subscribeReconnectSweep();
  for (;;) {
    const id = get(identity);
    if (id?.secretHex) {
      try {
        // Built lazily here on the first tick with an actor secret (not at
        // poll start, where the identity may still be absent). The
        // `<handle>@<domain>` need not have resolved yet — the live
        // self-address cell picks it up when it lands (the identity
        // subscription above), so DM delivery is never delayed by identity
        // resolution.
        if (!manager) {
          await getConversationsManager();
          refreshConversations();
        }
      } catch {
        // Transient (disconnect / not-yet-connected) — retry next tick.
      }
      // Both rails, through the one pump — so a ticker sweep can never overlap a
      // push wake on the single MLS engine. FaunaMls is ungated; the SMTP rail
      // no-ops until mail is enabled (the recipient secret becomes derivable), so
      // enabling mail after load is picked up without a reload.
      //
      // `cycle()` marks this as a FULL cycle, so it is counted for
      // `convReceiveCycles` — the ticker, reconnect and poke arms are the cycle
      // triggers, exactly as native counts its own and not its single-rail push
      // wakes. Awaited: a stalled pass therefore parks this ticker too, which is
      // what stops a dead pump being re-armed (`$lib/receive-pump`).
      await receivePump.cycle();
    }
    // Published as `pollWake` so `setConvPollSecs` can re-arm this exact sleep;
    // a plain `setTimeout` would strand a wake on the previous cadence.
    await new Promise<void>((resolve) => {
      pollWake = {
        timer: setTimeout(() => {
          pollWake = null;
          resolve();
        }, pollIntervalMs()),
        resolve,
      };
    });
  }
}

// ── e2e command hook ────────────────────────────────────────────────────────
//
// The browser twin of linux's `conversations_*` bridge commands
// (`apps/fauna-linux/src/main.rs`). The Playwright driver calls
// `window.__fauna_callCommand(action, payloadJson)` (see
// `tests/e2e-unified/drivers/web.py::call_command`); we route both the snapshot
// test-helper commands (`inject_inbound` / `create_mls_group` / `accept_recipient`,
// mock-backed) and the `real_*` / `{enable,disable}_real_faunamls` commands
// (tier_3 `test_fauna_mls_real_roundtrip`, real `FaunaMlsBackend` over the WS-RPC
// seam) — the browser twin of linux `conv_backend.rs`'s `e2e_*` drivers.

/** Whether the mock rail backends have been installed on the singleton. The web
 *  equivalent of linux's e2e-mode `host::manager()` install — done lazily on the
 *  first `conversations_*` command (which only ever fires under the e2e bridge),
 *  so production keeps its real SMTP + FaunaMls backends. */
let mocksInstalled = false;

// ── flavor-agnostic test-seam access ────────────────────────────────────────
//
// The wasm test seams exist ONLY in the `wasm-core-test` chunk flavor — they are
// gated on `libs/fauna-wasm`'s off-by-default `test-helpers` feature so a
// production bundle exports none of them (testing.md § convention 15). That means
// the generated `static/fauna_wasm.d.ts` does not declare them in the production
// flavor, while this module must type-check against WHICHEVER flavor was built
// last (`just web-check` builds the production one).
//
// So the seams are declared here by hand and reached through one guarded cast,
// rather than named on the generated type. Same problem and same answer as the
// onboarding chunk, which has needed it since it got its own test flavor
// (`$lib/onboarding/machine.svelte.ts`'s string-name dispatch). Declaring them as
// an intersection is safe against signature drift: under the test flavor the
// generated type declares them too, and TS merges the two into an overload set.
interface ConversationsTestSeams {
  installMockBackendsForTest(): void;
  foldersServeEnableForTest(ownerSecretHex: string, name: string, create: boolean): Promise<number>;
  reinstallRealFaunaMlsForTest(): boolean;
  createMlsGroupForTest(participants: string[]): string;
  injectSendFailure(threadId: string, reason: string): void;
  injectPageError(key: string, message: string): void;
  pageErrorDiagnosticForTest(): string | undefined;
  clearForTest(): void;
  seedResolvedLinkPreviewForTest(
    url: string, title: string, description: string, imageHash: string | undefined,
  ): void;
  injectInboundFromTestJson(payloadJson: string): void;
  evictThreadAttachmentsForTest(threadId: string, filename: string): number;
}

interface FeedTestSeams {
  injectPostsForTest(specs: unknown): void;
  injectErrorForTest(key: string, message: string): void;
  setCueRollupForTest(contentIds: string[]): Promise<void>;
  holdNextReloadForTest(): void;
  releaseHeldReloadForTest(): void;
}

/** Assert `m` came from a test-flavored wasm chunk and expose its seams.
 *
 *  THROWS when the seams are absent — i.e. the SPA is running the production wasm
 *  flavor. A test command must be honoured or fail loudly, never silently dropped
 *  (testing.md § convention 11); a `return` here would surface downstream as a
 *  product bug. The message names the recipe that builds the right flavor. */
// ── Unattested-member review ──
//
// The permanent Settings sub-page's data seam — thin wrappers over the wasm
// `WasmConversationsManager` methods (zero logic owed, priority #2).
// `memberReviewRemove` never takes a verdict: it always evicts first and
// persists only what the eviction earned, so this app cannot record
// `Removed` from its own reasoning — the one seam that can write it calls
// the one function that can earn it.

/** One open review item — mirrors the wasm `MemberReviewJs` (`person` as hex,
 *  `reasons` as wire strings via `MemberUnattestedReason::as_wire`). */
export interface MemberReview {
  person: string;
  reasons: string[];
}

/** The shared row-text parts for one review item — mirrors
 *  `fauna_core::data::MemberReviewRowText`. `who` is the handle verbatim
 *  (as a `LocalizedText` whose `key` IS the handle — `resolveLocalized`
 *  falls back to the raw key on no i18n match, which is exactly right here)
 *  when one resolves, otherwise the shared "no longer in any of your
 *  groups" key. */
export interface MemberReviewRowText {
  who: LocalizedText;
  reasons: LocalizedText[];
}

/** One group a `CrossGroupEviction` could not clear, with the backend's reason. */
export interface EvictionFailure {
  thread: string;
  reason: string;
}

/** Why a raised seat is not clearable from the review surface — § Propagation
 *  rule (5)'s two blocking classes; mirrors `UnreachableSeatClass`. */
export type UnreachableSeatClass = 'ChatGroupNoThreadHere' | 'FolderChannel';

/** One raised seat the cross-group eviction cannot clear from here. */
export interface UnreachableSeat {
  channelHex: string;
  class: UnreachableSeatClass;
}

/** What a cross-group eviction actually achieved, per group — mirrors
 *  `fauna_conversations::eviction::CrossGroupEviction`. Deliberately not a
 *  boolean: a partial eviction is the ordinary outcome under a flaky link,
 *  and the whole honesty of the review surface turns on not rounding that
 *  to success. */
export interface CrossGroupEviction {
  evicted: string[];
  failed: EvictionFailure[];
  unreachable: UnreachableSeat[];
}

/** Read the open review roster (`fauna_client_config::load_member_reviews`
 *  via the shared wasm face). */
export async function memberReviewList(): Promise<MemberReview[]> {
  const m = await getConversationsManager();
  return (await m.memberReviewList()) as MemberReview[];
}

/** The shared row-text parts for one review item — consumed, never
 *  re-derived (`fauna_core::data::review_row_text`'s reason-selection and
 *  unnameable-person rules live once, shared). `handle` must be resolved
 *  BEFORE a `memberReviewRemove` call for the same person:
 *  `memberReviewHandleForPerson` reads live membership, so there is no seat
 *  left to read one off afterward. */
export async function memberReviewRowText(
  personHex: string,
  reasons: string[],
  handle: string | null,
): Promise<MemberReviewRowText> {
  const m = await getConversationsManager();
  return m.memberReviewRowText(personHex, reasons, handle ?? undefined) as MemberReviewRowText;
}

/** The handle `person` is seated under, in the owner's own conversations —
 *  `null` when conversations are not up yet or they hold no seat this
 *  manager can name. */
export async function memberReviewHandleForPerson(personHex: string): Promise<string | null> {
  const m = await getConversationsManager();
  return m.handleForPerson(personHex) ?? null;
}

/** Record **Keep** — closes every open item for `person` with no group
 *  changes. Resolves to whether anything was actually open (a concurrent
 *  device may have already answered — a success no-op, never a rejection). */
export async function memberReviewKeep(personHex: string): Promise<boolean> {
  const m = await getConversationsManager();
  return m.memberReviewKeep(personHex);
}

/** Record **Remove** — evicts `person` from every group of the owner's they
 *  are in NOW (re-derived, never from the stored item), then persists only
 *  whatever verdict the eviction earned. A partial eviction earns none: the
 *  review item stays open, and the resolved `evicted`/`failed`/`unreachable`
 *  fields are what the caller composes its own message from — the
 *  derivation is shared, the wording per-app. */
export async function memberReviewRemove(personHex: string): Promise<CrossGroupEviction> {
  const m = await getConversationsManager();
  return (await m.memberReviewRemove(personHex)) as CrossGroupEviction;
}

// ── Nostr succession-aftermath npub confirm ──
//
// The Nostr page's data seam — thin wrappers over the wasm
// `WasmConversationsManager` methods (zero logic owed, priority #2), the same
// shape as the member-review wrappers above.

/** Is the caller owed an npub confirmation right now — the Nostr page's
 *  nav-enter read (tui `nostr.rs::refresh_and_check_npub` is the reference).
 *  Best-effort: any unhappy answer already degrades to `false` inside the
 *  wasm call, so no separate try/catch is owed here. The stamp is on the
 *  account plane, so the read first waits out a runtime start still in
 *  flight (a page entered right after sign-in would otherwise read none). */
export async function npubConfirmationOwed(secretHex: string): Promise<boolean> {
  const m = await getConversationsManager();
  await accountRuntimeSettled();
  return m.npubConfirmationOwed(secretHex);
}

/** Record the owner's "yes, that's my npub" confirmation. `nowSecs` is the
 *  caller's own clock, epoch seconds. */
export async function confirmNostrNpub(secretHex: string, nowSecs: number): Promise<void> {
  const m = await getConversationsManager();
  await m.confirmNostrNpub(secretHex, nowSecs);
}

function testSeams<T extends object, S>(m: T, probe: keyof S & string, typeName: string): T & S {
  if (typeof (m as unknown as Record<string, unknown>)[probe] !== 'function') {
    throw new Error(
      `${typeName} has no test seam '${probe}': this SPA is running the PRODUCTION ` +
        `wasm flavor, which compiles the e2e injection seams out ` +
        `(testing.md § convention 15). Build the test flavor with \`just web-test\`.`,
    );
  }
  return m as T & S;
}

type WebdavServeReply = { ok: true; served_sets: number } | { ok: false; error: string };

/** The `serve_enable_folder` outcome slot the test agent reads
 *  (`tests/e2e-unified/web-bridge/agent.js`). */
function setWebdavServeReply(reply: WebdavServeReply | undefined): void {
  // `conversations.ts` is a production module (`NostrSettingsSection.svelte`
  // imports it), so the write itself carries the guard: convention 15 wants the
  // hook absent from a release bundle, and a production `vite build` folds this
  // constant to `false` and strips the block.
  if (__FAUNA_E2E_AUTOMATION__) {
    (window as unknown as { __fauna_webdav_serve_reply?: WebdavServeReply })
      .__fauna_webdav_serve_reply = reply;
  }
}

/** The conversations manager with its test seams, asserted present. */
async function conversationsSeams(): Promise<WasmConversationsManager & ConversationsTestSeams> {
  return testSeams<WasmConversationsManager, ConversationsTestSeams>(
    await getConversationsManager(),
    'installMockBackendsForTest',
    'WasmConversationsManager',
  );
}

/** Build the singleton + install mock backends once, for the snapshot commands. */
async function ensureManagerForTest(): Promise<WasmConversationsManager & ConversationsTestSeams> {
  const m = await conversationsSeams();
  if (!mocksInstalled) {
    m.installMockBackendsForTest();
    mocksInstalled = true;
  }
  return m;
}

/** The e2e commands this module owns — the explicit list the registry keys on, so a
 *  second domain claiming one of these fails at install rather than shadowing it
 *  (testing.md § convention 11). `feed_inject_posts` is here because the feed
 *  manager's inject seam is driven from this module's imports, not because it is a
 *  conversations command; it moves the day the feed grows its own command module. */
const CONVERSATIONS_COMMANDS = [
  'conversations_inject_inbound',
  'conversations_evict_attachment',
  'conversations_seed_resolved_link_preview',
  'conversations_create_mls_group',
  'conversations_inject_send_failure',
  'conversations_inject_page_error',
  'conversations_accept_recipient',
  'conversations_enable_real_faunamls',
  'conversations_disable_real_faunamls',
  'conversations_real_resolve_send_new',
  'conversations_real_send',
  'conversations_real_add',
  'conversations_real_remove',
  'conversations_real_rename',
  'feed_inject_posts',
  'feed_inject_error',
  'feed_seed_cue_rollup_for_test',
  'feed_hold_next_reload',
  'feed_release_held_reload',
  // WebDAV served-set fixture — here because it runs over this module's
  // conversations manager (the folders author's MLS engine), not because it is
  // a conversations command.
  'serve_enable_folder',
  // The receive loop's run-one-cycle-now poke — same action name on every app
  // (`fauna_e2e_agent::CONV_RECEIVE_NOW`), so a test spells one call.
  'conv_receive_now',
] as const;

/** Register this module's e2e commands. Called only from `$lib/e2e-automation`
 *  (test builds — testing.md § Test-agent build exclusion), so production bundles
 *  tree-shake it out entirely. The `window.__fauna_callCommand` hook itself is
 *  owned by `$lib/e2e-commands`. */
export function registerConversationsCommands(): void {
  registerE2eCommands(CONVERSATIONS_COMMANDS, handleConversationsCommand);
}

/** Throw the page error a membership/label gesture just stamped, if any.
 *  `confirmAddParticipant`, `removeParticipant` and `renameThread` resolve
 *  normally even when their wire op failed: they report ONLY through the
 *  manager's page error. So an agent arm awaiting one must read that back, or
 *  it acks green for an op the nest refused (`e2e-conventions.md` § convention
 *  11). The throw is web's loud channel: it propagates into the driver call.
 *  Twin of tui's and linux's `conv_backend.rs::page_error` and windows'
 *  `ConversationsCommands.ThrowOnPageError`. */
function throwOnPageError(
  m: { pageErrorDiagnosticForTest(): string | undefined },
  action: string,
): void {
  const diagnostic = m.pageErrorDiagnosticForTest();
  if (diagnostic) {
    throw new Error(`${action}: ${diagnostic}`);
  }
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function handleConversationsCommand(action: string, p: any): Promise<unknown> {
  {
    switch (action) {
      case 'conversations_inject_inbound': {
        const m = await ensureManagerForTest();
        // The payload goes to the SHARED parser whole
        // (`ConversationsManager::inject_inbound_from_test_payload`, the one tui
        // and linux call in process): `recipients` and the mail rail's real-self
        // resolution, attachments (cached via `make_attachment_for_test`), labels,
        // `is_own` and `force_subject_change` all behave as they do on every other
        // app, and a key added to the payload reaches web with no edit here.
        m.injectInboundFromTestJson(JSON.stringify(p ?? {}));
        refreshConversations();
        return null;
      }
      case 'conversations_evict_attachment': {
        // Drop a thread's cached attachment bytes the way the store's budget
        // eviction does (the shared `evict_thread_attachments_for_test`). Nothing
        // evicted is a FAILED command, never an ack (convention 11): a render
        // asserted after a no-op evict witnesses nothing.
        const m = await ensureManagerForTest();
        const threadId = String(p?.thread_id ?? '');
        const filename = String(p?.filename ?? '');
        const evicted = m.evictThreadAttachmentsForTest(threadId, filename);
        if (evicted === 0) {
          throw new Error(
            `conversations_evict_attachment: no resident attachment named ${JSON.stringify(filename)} ` +
              `in thread ${JSON.stringify(threadId)}`,
          );
        }
        refreshConversations();
        return null;
      }
      case 'conversations_seed_resolved_link_preview': {
        // Stamp a pre-resolved link-preview (`PreviewState::Resolved`) for a URL so an
        // injected bubble's `LinkPreview` block folds `Resolved` and the bubble paints the
        // `link-preview-card` (render-model.md § D4) — the conversations twin of the feed's
        // `link_preview` inject spec. The resolve is mocked here (tier_2), exactly as
        // `test_feed_link_preview.py` mocks it: a real resolve needs a live nest OpenGraph
        // fetch. `image_hash` absent → `undefined` → `None` (og:image omitted from the card).
        const m = await ensureManagerForTest();
        m.seedResolvedLinkPreviewForTest(
          p.url, p.title ?? '', p.description ?? '', p.image_hash ?? undefined,
        );
        refreshConversations();
        return null;
      }
      case 'conversations_create_mls_group': {
        const m = await ensureManagerForTest();
        m.createMlsGroupForTest(p.participants ?? []);
        refreshConversations();
        return null;
      }
      case 'conversations_inject_send_failure': {
        // Stamp the shared ComposeState.send_state = Failed { reason } on a
        // thread + select it, so the page surfaces it on `error-message`
        // (conversations.md § Errors & edge cases). Browser twin of linux
        // `handle_conversations_inject_send_failure`.
        const m = await ensureManagerForTest();
        m.injectSendFailure(p.thread_id, p.reason);
        refreshConversations();
        return null;
      }
      case 'conversations_inject_page_error': {
        // Stamp ConversationsSnapshot.error — the membership/label twin of
        // conversations_inject_send_failure, same reason: no product path
        // fails one of those ops on demand (conversations.md § Errors & edge
        // cases).
        const m = await ensureManagerForTest();
        m.injectPageError(p.key, p.message);
        refreshConversations();
        return null;
      }
      case 'conversations_accept_recipient': {
        // Commit the typed recipient chip on the REAL manager. This is a genuine
        // UI action — the bridge's keyboard-Enter substitute (Windows SendInput
        // can't reach a non-foreground UI; web uses it for parity) — NOT a
        // snapshot injection, so it must use `getConversationsManager()` (like the
        // `conversations_real_*` commands), NOT `ensureManagerForTest()`. The
        // latter calls `installMockBackendsForTest()`, which replaces the real
        // `SmtpBackend`/`FaunaMls` rails with no-op `MockRailBackend`s — so a
        // subsequent compose-send resolved the MOCK and returned a synthetic Ok
        // (echo rendered) WITHOUT issuing `fauna.email.send`, silently swallowing
        // every web first-party send to an external recipient.
        const m = await getConversationsManager();
        // **Probe first, then commit** — the same order this command's own door
        // drives (`+page.svelte::onRecipientKeydown`: `resolveRecipient()` then
        // `acceptCurrentRecipientChip()`), and the same order tui
        // (`conversations/mod.rs::accept_recipient`) and linux
        // (`conv_backend::e2e_accept_recipient`) settled on. Committing without
        // the probe can only ever use the format-only parse, and
        // `try_parse_typed_address` cannot produce `TypedAddress::Fauna` by
        // design, so a typed Fauna handle or 64-hex actor id would commit no
        // chip at all over the agent while working for a real user. The picker's
        // 30 ms debounce usually beats us to it, which is why web has not felt
        // this — but the agent must not depend on a race it does not control.
        await m.resolveRecipient();
        // The manager picks the active picker (add-participant overlay takes
        // priority over new-thread compose) — mirrors linux's
        // `accept_current_recipient_chip`.
        if (!m.acceptCurrentRecipientChip()) {
          // Convention 11: a recognised arm that declines must say so. This
          // returned `null` regardless, so a chip that failed to commit acked
          // green and surfaced ~5 s later as the action layer's generic "chip
          // not added" — a downstream read that names neither the command nor
          // the reason. Same shape as `conversations_real_resolve_send_new`'s
          // own guard, which has always thrown here.
          throw new Error(
            'conversations_accept_recipient: nothing committed — the active ' +
              'picker had no resolvable recipient (empty input, or an address ' +
              'that resolved to no chip)',
          );
        }
        refreshConversations();
        return null;
      }

      // ── real-wire FaunaMls (tier_3 test_fauna_mls_real_roundtrip) ─────────
      //
      // The real path uses `getConversationsManager()` (NOT
      // `ensureManagerForTest`, which would install mocks): the manager singleton
      // already wired the real `FaunaMlsBackend` over `WsConversationsRpc` at
      // construction. `reinstallRealFaunaMlsForTest` re-registers it in case an
      // earlier snapshot test (same session-cached page) installed mocks. Mirrors
      // linux `conv_backend.rs` `request_e2e_activation` + the `e2e_*` drivers.

      case 'conversations_enable_real_faunamls': {
        const m = await conversationsSeams();
        m.reinstallRealFaunaMlsForTest();
        mocksInstalled = false;
        // Login-time replenish so peers can fetch a package to add us to a group.
        // Fire-and-forget + best-effort + idempotent, mirroring linux `activate`
        // (which spawns the publish rather than blocking activation) — keeps
        // `enable` off the key-package round-trip's critical path.
        void m
          .ensureKeypackages(KEYPACKAGE_TARGET)
          .then(() => m.ensureLastResortKeypackage())
          .catch((e) => {
            console.warn('ensure keypackages (enable real faunamls):', e);
            logMessage('warn', 'fauna_web::conversations', `ensure keypackages (enable real faunamls): ${e}`);
          });
        convRealBackendActive.set(true);
        return null;
      }
      case 'conversations_disable_real_faunamls': {
        // Restore the FaunaMls mock backend (test-ordering safety net): activation
        // replaced the mock for the rest of the session-cached page, so a snapshot
        // test running after the real-wire test would otherwise hit the real
        // backend. Mirrors linux `disable_e2e_real_backend`.
        const m = await conversationsSeams();
        m.installMockBackendsForTest();
        mocksInstalled = true;
        convRealBackendActive.set(false);
        return null;
      }
      case 'conversations_real_resolve_send_new': {
        // Resolve the typed recipient through the REAL backend probe (no actor_id
        // injection): the recipient picker's `resolveRecipient` probes
        // `fauna.conversations.keypackage.count` to promote a 64-hex actor id to a
        // `Fauna` chip, then `sendNewThread` bootstraps the group (fetch key
        // package → create MLS group → deliver Welcome → post Application envelope).
        const m = await getConversationsManager();
        m.startNewConversation();
        m.setNewThreadRecipientInput(p.recipient);
        await m.resolveRecipient();
        if (!m.acceptCurrentRecipientChip()) {
          throw new Error(
            `recipient '${p.recipient}' did not resolve to a chip (not a reachable Fauna actor?)`,
          );
        }
        m.setNewThreadBody(p.body ?? '');
        await m.sendNewThread();
        refreshConversations();
        return null;
      }
      case 'conversations_real_send': {
        // Send on an existing thread (a forked-but-unbound group bootstraps lazily).
        const m = await getConversationsManager();
        m.setComposeBody(p.thread_id, p.body ?? '');
        await m.send(p.thread_id);
        refreshConversations();
        return null;
      }
      case 'conversations_real_add': {
        // Add the peer (actor_id injected) to the thread: bound group → MLS Commit
        // + Welcome; 1:1 → forks a fresh group (snapshot-only until its first send).
        const m = await conversationsSeams();
        m.openAddParticipant(p.thread_id);
        m.setAddParticipantRecipientInput(p.peer_handle);
        m.acceptAddParticipantFaunaChip(p.peer_handle, p.peer_actor_id_hex);
        await m.confirmAddParticipant();
        refreshConversations();
        throwOnPageError(m, action);
        return null;
      }
      case 'conversations_real_remove': {
        // Remove the peer (actor_id injected) from a bound group (MLS Commit, no
        // Welcome). `peer_handle` must match the one used at add — the snapshot
        // removal keys on the handle; the wire op finds the leaf by actor_id.
        const m = await conversationsSeams();
        await m.removeParticipant(p.thread_id, p.peer_handle, p.peer_actor_id_hex);
        refreshConversations();
        throwOnPageError(m, action);
        return null;
      }
      case 'conversations_real_rename': {
        // Rename a bound group (posts the encrypted GroupMeta::NameChanged envelope).
        const m = await conversationsSeams();
        await m.renameThread(p.thread_id, p.label);
        refreshConversations();
        throwOnPageError(m, action);
        return null;
      }
      // ── feed snapshot injection (tier_2 unverified-source-badge) ─────────
      case 'feed_inject_posts': {
        // Inject a `Loaded` post list straight onto the shared feed snapshot so
        // the page paints posts with an arbitrary `verification` — the only way
        // to exercise the `Failed` unverified-source-badge render (a real signed
        // post is only ever `Unchecked`/`Verified`). The wasm side builds each
        // post's `document` from its `body` in Rust, so JS sends only plain
        // specs. Browser twin of linux `handle_feed_inject_posts`.
        const m = testSeams<WasmFeedManager, FeedTestSeams>(
          await getFeedManager(),
          'injectPostsForTest',
          'WasmFeedManager',
        );
        m.injectPostsForTest(p.posts ?? []);
        refreshFeed();
        return null;
      }
      case 'feed_inject_error': {
        // Stamp the feed snapshot's `error` — the state a failed background
        // fetch leaves — so the page surfaces it on `error-message`. There is
        // no *product* path that fails a feed fetch on demand. Browser twin of
        // tui's `feed_inject_error`. `{key: string, message: string}`.
        const m = testSeams<WasmFeedManager, FeedTestSeams>(
          await getFeedManager(),
          'injectErrorForTest',
          'WasmFeedManager',
        );
        const key = typeof p.key === 'string' && p.key !== '' ? p.key : 'feed.error_load';
        const message = typeof p.message === 'string' ? p.message : 'feed load failed';
        m.injectErrorForTest(key, message);
        refreshFeed();
        return null;
      }
      case 'feed_hold_next_reload': {
        // Arm the feed manager's one-shot reload hold: the NEXT reload publishes
        // the list it kept or cleared, then parks before its fetch until
        // `feed_release_held_reload` (`feed.md` § The read model). A web gesture
        // is a DOM event the driver never awaits, so unlike tui's agent nothing
        // here has to start-rather-than-await a feed op while it is armed.
        testSeams<WasmFeedManager, FeedTestSeams>(
          await getFeedManager(),
          'holdNextReloadForTest',
          'WasmFeedManager',
        ).holdNextReloadForTest();
        return null;
      }
      case 'feed_release_held_reload': {
        // Release the parked reload (or disarm a hold none reached yet).
        testSeams<WasmFeedManager, FeedTestSeams>(
          await getFeedManager(),
          'releaseHeldReloadForTest',
          'WasmFeedManager',
        ).releaseHeldReloadForTest();
        return null;
      }
      case 'serve_enable_folder': {
        // Arrange a WebDAV-served, content-keyed folder — the fixture step
        // `helpers/webdav_roundtrip.py::serve_enable_folder` runs before a real
        // WebDAV client PUTs/GETs. Web twin of tui's arm of the same name: create
        // the set when asked, then the production `FoldersAuthor::serve_set`. The
        // outcome is published as `webdav_serve_reply` in tui's exact wire shape
        // (`{ok: true, served_sets}` / `{ok: false, error}`), cleared at the start
        // and set before this resolves, so the helper's first poll never reads a
        // stale value. `{folder: string, create?: boolean}`.
        setWebdavServeReply(undefined);
        try {
          const folder = typeof p.folder === 'string' ? p.folder : '';
          if (!folder) throw new Error('payload needs a non-empty `folder`');
          const secret = get(identity)?.secretHex ?? accountsTabSessionMaterial()?.secret_hex;
          if (!secret) throw new Error('no identity on this seat (not logged in?)');
          const m = await conversationsSeams();
          const served = await m.foldersServeEnableForTest(secret, folder, p.create === true);
          setWebdavServeReply({ ok: true, served_sets: served });
        } catch (e) {
          setWebdavServeReply({ ok: false, error: e instanceof Error ? e.message : String(e) });
        }
        return null;
      }
      case 'feed_seed_cue_rollup_for_test': {
        // Seed the live engagement-cue engine with a real `cues:v1` nest row (a
        // real network round trip, unlike `feed_inject_posts` above), so web's
        // capture-less client can reach "Clear activity data" with something to
        // actually delete. Browser twin of tui/windows' same-named seam.
        const m = testSeams<WasmFeedManager, FeedTestSeams>(
          await getFeedManager(),
          'setCueRollupForTest',
          'WasmFeedManager',
        );
        await m.setCueRollupForTest((p.content_ids as string[] | undefined) ?? []);
        refreshFeed();
        return null;
      }
      case 'conv_receive_now': {
        // Convention 14's `run_now` for the receive rail — the web twin of
        // native's `ConversationsSession::poke_receive_cycle`. Drives the one
        // pump every other arm drives (`convReceiveNow`), so the poked cycle is
        // the real delivery path. Fire-and-forget: the barrier is
        // `conv_receive_cycles`, not this ack.
        convReceiveNow();
        return null;
      }
      default:
        // Unreachable: the registry only routes CONVERSATIONS_COMMANDS here, and an
        // action outside the whole table throws in `installCommandHook`. Kept so
        // adding a name to the list above without a case fails loudly rather than
        // resolving `undefined` (convention 11).
        throw new Error(
          `${action} is declared in CONVERSATIONS_COMMANDS but has no case — ` +
            `implement it or remove it from the list`,
        );
    }
  }
}
