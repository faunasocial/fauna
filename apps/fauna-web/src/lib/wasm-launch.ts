import { asset } from '$app/paths';
import { adoptStoreKey, announceStartOver, forgetStoreKey, registerIdentityStoreChunk } from './identity-store';

// Re-exported so existing call sites keep importing the snapshot type and its
// accessors from `#lib/wasm-launch`; they live in their own module only so
// `deno test` can reach them (see `launch-snapshot.ts`).
export {
  wizardEntryOf,
  phaseNameOf,
  offlineTransientOf,
  supersededSuccessorOf,
  accountIndexRefusalOf,
  identityChangedOf,
  identityForkOf,
  tokenExpiryOf,
  lockedUntilOf,
} from './launch-snapshot';
export type { LaunchSnapshot, AccountIndexRefusal } from './launch-snapshot';

let wasmModule: typeof import('../../static/fauna_wasm_launch.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the launch wasm chunk exactly once per page load. The init
 *  *promise* is memoized (same load-bearing shape as `wasm.ts::ensureWasm`):
 *  two concurrent callers await the SAME in-flight init, so `mod.default()`
 *  runs once — a second concurrent call would re-instantiate the chunk and
 *  reset wasm linear memory, dangling every live wasm object. A failed init
 *  drops the cached promise so a later call can retry (e.g. after a transient
 *  chunk-fetch failure). */
export function ensureLaunchWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_launch.js');
      await mod.default(asset('fauna_wasm_launch_bg.wasm'));
      // The identity store's key, if this page holds it, goes in before
      // anything reads the registry through this chunk (identity-store.ts).
      registerIdentityStoreChunk(mod);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) throw new Error('Launch WASM not initialized — call ensureLaunchWasm() first');
  return wasmModule;
}

// ── LaunchMachine ──────────────────────────────────────────
//
// Same module-identity discipline as `wasm-onboarding.ts:18-24`: keep the
// constructor call inside the module that holds the singleton `wasmModule`
// reference. Different Vite chunks carry separate copies of the wasm-bindgen
// boilerplate, so constructing the class from another chunk hits uninitialized
// memory.
//
// The machine drives the § App-launch routing rows off the `LaunchPersistence`
// seam (`onboarding/launch-persistence.ts`). Web mounts it for EVERY launch row,
// the silent-challenge row included: the `AuthConnector` grew the nest-identity-pin
// (TOFU) seam on 2026-07-13, so `LaunchPhase::IdentityChanged` now carries the
// `launch_identity_changed` surface that used to force web to keep its own
// classifier. On wasm the connector runs `run_pinned_silent_challenge` over the
// SAME `LocalStoragePinStore` (`fauna_nest_pins`, keyed by nest URL) the SPA's
// own `challengeVerify` path pins with, so the two can never disagree about what
// is pinned (security.md § Transport trust).

export interface LaunchObserver {
  onChanged(): void;
}

// There is deliberately NO `LaunchPersistence` JS interface anymore (CR-3):
// the machine routes on the shared registry persistence over localStorage
// (the per-actor slots), constructed Rust-side in
// `libs/fauna-wasm-launch`. Web implements no storage seam at all — the slot
// stores below (`onboarding/*-store.ts`) wrap the `registry*` accessors, so
// the gate and the machine can never disagree.

/** Wire shape of Rust's `PendingFactoryResetRecord` (serde, snake_case). */
export interface PendingFactoryResetJson {
  nest_url: string;
  handle: string;
  claim_code: string;
}

/** Wire shape of Rust's `PendingInviteRecord` (serde, snake_case). */
export interface PendingInviteJson {
  nest_url: string;
  handle: string;
  request_id: string;
  status_json: string;
}

/** Wire shape of Rust's `AwaitingDnsRecord` (serde, snake_case). */
export interface AwaitingDnsJson {
  nest_url: string;
  handle: string;
  dns_records_json: string;
  claim_code: string;
  /** The box's reach address, once `create_server` returned (optional —
   *  absent before the box exists). */
  reach_ipv4?: string | null;
  /** The identity the box was built with (64 hex), re-held as the
   *  first-contact root on relaunch (optional — absent before the box exists). */
  nest_actor_id?: string | null;
}

export interface LaunchMachine {
  start(): Promise<void>;
  snapshotJson(): string;
  /** `undefined` (Rust `Option::None`) unless the machine is `Online`. */
  currentBearer(): string | undefined;
  /** The id of the session `currentBearer()` names — `undefined` unless the
   *  machine is `Online` (`docs/goal/behavior/devices.md` § The client's own
   *  session). Primed into the SPA's bearer cache beside the token itself. */
  currentTokenId(): string | undefined;
  refreshToken(): Promise<void>;
  notify401(): Promise<void>;
  /** `launch-retry-button`. A no-op unless the phase is `Offline{transient:true}`
   *  or the sign-in-refused verdict,
   *  so the terminal outdated-nest and `IdentityChanged` phases can't be retried into. */
  retrySilentChallenge(): Promise<void>;
  /** `nest-identity-changed-trust-button`. Forgets the TOFU pin, then re-challenges
   *  (re-TOFU). A no-op in every phase but `IdentityChanged` — the pin is never
   *  forgotten outside this user action. */
  trustNestIdentity(): Promise<void>;
  /** Whether the locked notice painted for `shownLockedUntilSecs` still stands
   *  on the current snapshot — the shared `LaunchSnapshot::lock_notice_holds`
   *  (`devices.md` § The locked state). `false` → route the snapshot afresh. */
  lockNoticeHolds(shownLockedUntilSecs: number): boolean;
}

/**
 * The launch clock this tab signs in on — `{offset_secs, now_secs}`, the web
 * leg of the cross-app `clock` state key (`fauna_e2e_agent::CLOCK_KEY` owns
 * the shape) the wrong-clock launch witness reads. Test builds only: its sole
 * caller is `#lib/e2e-automation`, and the two getters exist only in the
 * launch chunk's `test-helpers` flavor (`libs/fauna-wasm-launch`), which also
 * seeds the offset from localStorage in the `LaunchMachine` constructor.
 *
 * `null` — never a zeroed pair — when the chunk is not initialized yet or was
 * built without the getters: "cannot answer" must not read as "the seed never
 * arrived". Called from THIS module for the module-identity reason above.
 */
export function launchClockForTest(): { offset_secs: number; now_secs: number } | null {
  if (!wasmModule) return null;
  const m = wasmModule as unknown as {
    launchClockOffsetSecsForTest?: () => number;
    launchClockNowSecsForTest?: () => number;
  };
  if (!m.launchClockOffsetSecsForTest || !m.launchClockNowSecsForTest) return null;
  return {
    offset_secs: m.launchClockOffsetSecsForTest(),
    now_secs: m.launchClockNowSecsForTest(),
  };
}

export async function createLaunchMachine(observer: LaunchObserver): Promise<LaunchMachine> {
  await ensureLaunchWasm();
  type Ctor = new (o: LaunchObserver) => LaunchMachine;
  return new (wasm() as unknown as { LaunchMachine: Ctor }).LaunchMachine(observer);
}

/**
 * Mint the post-factory-reset claim code and durably persist it into the
 * ACTIVE account's registry slot — the shared
 * `mintAndPersistPendingFactoryReset` free function
 * (`libs/fauna-wasm-launch/src/lib.rs`), which is the ONLY way to obtain the
 * code: it returns only once the row is written, so the crash-unsafe ordering
 * ("dispatch, then save whatever the reply says") is unrepresentable (gap CR-1,
 * `docs/goal/architecture/nest/common.md` § Client-state recoverability).
 *
 * Called from THIS module for the same module-identity reason as the
 * `LaunchMachine` constructor above — a wasm-bindgen export invoked from another
 * Vite chunk hits uninitialized memory.
 *
 * Returns `undefined` when the row could not be persisted (quota /
 * private-browsing): the shared rail reads the row back and refuses to hand
 * out a code it could not save. `onboarding/launch-persistence.ts` re-checks
 * and throws for its callers.
 */
export async function mintAndPersistPendingFactoryResetWasm(
  nestUrl: string,
  handle: string,
): Promise<string | undefined> {
  await ensureLaunchWasm();
  type Mint = (nestUrl: string, handle: string) => Promise<string | undefined>;
  const fn = (wasm() as unknown as { mintAndPersistPendingFactoryReset: Mint })
    .mintAndPersistPendingFactoryReset;
  return fn(nestUrl, handle);
}

// ── Registry-backed slot accessors ─────────────────────────────────────
//
// Thin typed wrappers over the `registry*` wasm exports. Reads are sync on
// the wasm side; every WRITER is a Promise, because it runs inside the
// cross-tab registry mutation lock (`fauna_client_accounts::with_web_mutation_lock`
// — tabs share one `localStorage`; `#lib/accounts` has the same shape).
// Callers must have awaited `ensureLaunchWasm()` (the async wrappers below do
// it). Records cross the boundary as serde JSON strings.

function registryFns() {
  return wasm() as unknown as {
    registryHasIdentity(): boolean;
    registryIndexBlob(): string | undefined;
    registryHasSecretRow(actorId: string): boolean;
    identityStoreHasHeader(): boolean;
    identityStoreIsLocked(): boolean;
    registryLoadPendingInvite(): string | undefined;
    registrySavePendingInvite(json: string): Promise<boolean>;
    registryDeletePendingInvite(): Promise<void>;
    registryLoadAwaitingDns(): string | undefined;
    registryPersistAwaitingDns(secretHex: string, json: string): Promise<string | undefined>;
    registryClearAwaitingDns(): Promise<void>;
    registryLoadPendingFactoryReset(): string | undefined;
    registryDeletePendingFactoryReset(): Promise<void>;
  };
}

function parseOrNull<T>(json: string | undefined): T | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as T;
  } catch {
    return null;
  }
}

/** Identity present for the active account (the gates' identity half). */
export async function registryHasIdentity(): Promise<boolean> {
  await ensureLaunchWasm();
  return registryFns().registryHasIdentity();
}

/**
 * A FACT REPORT of what `LaunchMachine::start()` is about to read — logged by
 * the onboarding route immediately before it starts the machine.
 *
 * `WizardAt IdentityChoice` is the `(None, _, _)` arm of `machine.rs::start`,
 * so it means `RegistryLaunchPersistence::load_identity()` answered `None`
 * (`launch_persistence.rs` — `session_account()` then `registry.secrets(...)`).
 * That single `None` has three different causes with three different owners,
 * and from outside the app they are indistinguishable:
 *
 *   1. no active account at all — the index is absent;
 *   2. an UNREADABLE index — present but malformed JSON, or a `min_reader_version`
 *      past this build, which `index()` deliberately reports as an EMPTY index
 *      rather than healing it (`IndexState::Unreadable`);
 *   3. an active account that carries NO usable secret — the index names an
 *      actor whose `fauna/{actor}/secret` row is gone.
 *
 * (2) and (3) are different bugs — a corrupt write versus a registry sweep that
 * left the index behind — so a session that cannot tell them apart is guessing.
 * `registryHasIdentity()` is `persistence().load_identity().is_some()` over the
 * SAME adapter the machine routes on, so it is the authoritative verdict; the
 * rest is the identity store's raw rows (`registryIndexBlob` /
 * `registryHasSecretRow`, opened by the store when it rests sealed —
 * `web.md` § Identity store at rest), read with none of the registry's
 * parsing, which is what makes the report useful when the two disagree. `seal`
 * names the store's arm: `plain`, `unlocked`, or `LOCKED` — a locked store
 * reads nothing, a fourth cause of `None` that unlock-before-routing rules out.
 *
 * Values are never logged — only presence, counts, versions, and 8-char actor
 * prefixes for correlation.
 */
interface IndexBlobShape {
  active?: string | null;
  accounts?: unknown[];
  schema_version?: number;
  min_reader_version?: number;
}

export async function launchIdentityInputs(): Promise<Record<string, unknown>> {
  await ensureLaunchWasm();
  const fns = registryFns();
  const raw = fns.registryIndexBlob() ?? null;
  const report: Record<string, unknown> = {};
  report.seal = !fns.identityStoreHasHeader()
    ? 'plain'
    : fns.identityStoreIsLocked()
      ? 'LOCKED'
      : 'unlocked';

  if (raw === null) {
    report.index = 'absent';
  } else {
    let parsed: IndexBlobShape | null = null;
    try {
      parsed = JSON.parse(raw) as IndexBlobShape;
    } catch {
      parsed = null;
    }
    if (!parsed) {
      // Cause (2), the malformed half — the blob is present and this build
      // cannot read it, so `index()` answers with an empty index and `active`
      // is None.
      report.index = 'UNPARSEABLE';
      report.index_bytes = raw.length;
    } else {
      const active = parsed.active ?? null;
      report.index = 'parsed';
      report.schema_version = parsed.schema_version ?? 'absent(baseline)';
      report.min_reader_version = parsed.min_reader_version ?? 'absent(baseline)';
      report.accounts = Array.isArray(parsed.accounts) ? parsed.accounts.length : 'malformed';
      report.active = active ? active.slice(0, 8) : 'NONE';
      if (active) {
        // Cause (3) reads off this line: `active_secret_row: 'absent'` is the
        // index naming an account the store has no secret for.
        report.active_secret_row = fns.registryHasSecretRow(active) ? 'present' : 'absent';
      }
    }
  }

  try {
    report.load_identity = (await registryHasIdentity()) ? 'Some' : 'NONE';
  } catch (e) {
    report.load_identity = `threw: ${e}`;
  }
  return report;
}

export async function registryLoadPendingInvite(): Promise<PendingInviteJson | null> {
  await ensureLaunchWasm();
  return parseOrNull<PendingInviteJson>(registryFns().registryLoadPendingInvite());
}

/** `false` means the write was refused (no active identity / unparseable). */
export async function registrySavePendingInvite(rec: PendingInviteJson): Promise<boolean> {
  await ensureLaunchWasm();
  return registryFns().registrySavePendingInvite(JSON.stringify(rec));
}

export async function registryDeletePendingInvite(): Promise<void> {
  await ensureLaunchWasm();
  await registryFns().registryDeletePendingInvite();
}

export async function registryLoadAwaitingDns(): Promise<AwaitingDnsJson | null> {
  await ensureLaunchWasm();
  return parseOrNull<AwaitingDnsJson>(registryFns().registryLoadAwaitingDns());
}

/** The slot VERBATIM — the string the registry holds — for
 *  `seedAwaitingManualDnsRecordJson`, which takes the whole record and never
 *  wants it re-shaped in JS. `null` when there is no slot. */
export async function registryLoadAwaitingDnsJson(): Promise<string | null> {
  await ensureLaunchWasm();
  return registryFns().registryLoadAwaitingDns() ?? null;
}

/** The deferred-DNS exit through the shared `persist_awaiting_dns`: registers
 *  and activates the identity `secretHex` names and completes its slot.
 *  Resolves to its actor id, `null` when the write was refused. */
export async function registryPersistAwaitingDns(
  secretHex: string,
  rec: AwaitingDnsJson,
): Promise<string | null> {
  await ensureLaunchWasm();
  return (await registryFns().registryPersistAwaitingDns(secretHex, JSON.stringify(rec))) ?? null;
}

export async function registryClearAwaitingDns(): Promise<void> {
  await ensureLaunchWasm();
  await registryFns().registryClearAwaitingDns();
}

export async function registryLoadPendingFactoryReset(): Promise<PendingFactoryResetJson | null> {
  await ensureLaunchWasm();
  return parseOrNull<PendingFactoryResetJson>(registryFns().registryLoadPendingFactoryReset());
}

export async function registryDeletePendingFactoryReset(): Promise<void> {
  await ensureLaunchWasm();
  await registryFns().registryDeletePendingFactoryReset();
}

// ── The identity store's unlock gate ───────────────────────────────────
//
// `web.md` § Identity store at rest → *Unlock comes before launch routing*:
// nothing reads the registry until the store serves reads. The launch chunk is
// the one that creates or unlocks (it is small and loads first); the key it
// then holds is adopted by the page (`identity-store.ts`) and installed into
// every other chunk. Errors reject with the stable code strings of
// `web_store.rs` (`wrong_passphrase_or_corrupt`, `newer_header`, …).

/** The localStorage key a test-capable build reads to opt a journey into the
 *  sealed arm when there is no header yet — the web twin of a native launch
 *  variable, under the variable's own name. Only `__FAUNA_E2E_AUTOMATION__`
 *  builds read it; a shipped build has no plain arm and always creates. */
export const SEALED_IDENTITY_STORE_OPT_IN_KEY = 'FAUNA_E2E_SEALED_IDENTITY_STORE';

/** `open`: the store serves reads now (unlocked, or the test build's plain
 *  arm). `create`: no header — choose a passphrase. `unlock`: a header and no
 *  key in this page. */
export type IdentityStoreGate = 'open' | 'create' | 'unlock';

function identityStoreFns() {
  return wasm() as unknown as {
    identityStoreHasHeader(): boolean;
    identityStoreIsLocked(): boolean;
    identityStoreCreate(passphrase: string): void;
    identityStoreUnlock(passphrase: string): void;
    identityStoreChangePassphrase(current: string, next: string): void;
    identityStoreKey(): string | undefined;
    identityStoreInstallKey(keyHex: string): void;
    identityStoreStartOver(): void;
    identityStorePasskeys(): PasskeyWrapInfo[];
    identityStoreNewPasskeySalt(): Uint8Array<ArrayBuffer>;
    identityStoreAddPasskey(credentialId: Uint8Array, prfSalt: Uint8Array, prfOutput: Uint8Array): void;
    identityStoreUnlockWithPasskey(credentialId: Uint8Array, prfOutput: Uint8Array): void;
    identityStoreRemovePasskey(credentialId: Uint8Array): boolean;
  };
}

/** One passkey wrap in the seal header — readable while locked (neither field
 *  is secret): the credential to ask for and its PRF eval input. */
export interface PasskeyWrapInfo {
  credentialId: Uint8Array<ArrayBuffer>;
  prfSalt: Uint8Array<ArrayBuffer>;
}

function sealedArmOptedOut(): boolean {
  if (!__FAUNA_E2E_AUTOMATION__) return false;
  try {
    return localStorage.getItem(SEALED_IDENTITY_STORE_OPT_IN_KEY) !== '1';
  } catch {
    return true;
  }
}

export async function identityStoreGate(): Promise<IdentityStoreGate> {
  await ensureLaunchWasm();
  const fns = identityStoreFns();
  if (fns.identityStoreHasHeader()) return fns.identityStoreIsLocked() ? 'unlock' : 'open';
  return sealedArmOptedOut() ? 'open' : 'create';
}

/** Whether Settings offers the credential-store section: the sealed arm is
 *  active (a header exists) and this page holds its key. */
export async function identityStoreSealedAndOpen(): Promise<boolean> {
  await ensureLaunchWasm();
  const fns = identityStoreFns();
  return fns.identityStoreHasHeader() && !fns.identityStoreIsLocked();
}

function adoptLaunchKey(): void {
  const key = identityStoreFns().identityStoreKey();
  if (key) adoptStoreKey(key);
}

/** Write the header under `passphrase` and seal every row this browser holds. */
export async function identityStoreCreateWith(passphrase: string): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreCreate(passphrase);
  adoptLaunchKey();
}

/** Open the header under `passphrase`. */
export async function identityStoreUnlockWith(passphrase: string): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreUnlock(passphrase);
  adoptLaunchKey();
}

/** Install a key another tab handed over; rejects `wrong_store_key` when it
 *  does not open the header's check (a stale key), installing nothing. */
export async function identityStoreInstallHandedKey(keyHex: string): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreInstallKey(keyHex);
  adoptLaunchKey();
}

/** Re-wrap the store key under `next` — one header write; other tabs keep
 *  working on the unchanged key. */
export async function identityStoreChangePassphraseWith(current: string, next: string): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreChangePassphrase(current, next);
}

/** The header's passkey wraps (`web.md` § Identity store at rest → *A passkey
 *  wrap*); empty with no header. */
export async function identityStorePasskeyWraps(): Promise<PasskeyWrapInfo[]> {
  await ensureLaunchWasm();
  return identityStoreFns().identityStorePasskeys();
}

/** A fresh 32-byte PRF salt for a passkey wrap about to be added. */
export async function identityStoreNewPasskeySalt(): Promise<Uint8Array<ArrayBuffer>> {
  await ensureLaunchWasm();
  return identityStoreFns().identityStoreNewPasskeySalt();
}

/** Wrap the held store key under a passkey's PRF output — one header write. */
export async function identityStoreAddPasskeyWrap(
  credentialId: Uint8Array,
  prfSalt: Uint8Array,
  prfOutput: Uint8Array,
): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreAddPasskey(credentialId, prfSalt, prfOutput);
}

/** Open the header with a passkey's PRF output; rejects `passkey_did_not_open`
 *  and leaves the store locked when it does not. */
export async function identityStoreUnlockWithPasskeyOutput(
  credentialId: Uint8Array,
  prfOutput: Uint8Array,
): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreUnlockWithPasskey(credentialId, prfOutput);
  adoptLaunchKey();
}

/** Remove the passkey wrap for `credentialId` — one header write; the
 *  passphrase still opens the store. */
export async function identityStoreRemovePasskeyWrap(credentialId: Uint8Array): Promise<boolean> {
  await ensureLaunchWasm();
  return identityStoreFns().identityStoreRemovePasskey(credentialId);
}

/** Start over: erase this browser's identity store and lock every chunk. The
 *  caller has the user's explicit confirmation and returns to create mode. */
export async function identityStoreStartOverNow(): Promise<void> {
  await ensureLaunchWasm();
  identityStoreFns().identityStoreStartOver();
  forgetStoreKey();
  announceStartOver();
}
