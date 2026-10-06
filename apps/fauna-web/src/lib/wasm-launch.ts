import { base } from '$app/paths';

// Re-exported so existing call sites keep importing the snapshot type and its
// accessors from `$lib/wasm-launch`; they live in their own module only so
// `deno test` can reach them (see `launch-snapshot.ts`).
export {
  wizardEntryOf,
  phaseNameOf,
  offlineTransientOf,
  supersededSuccessorOf,
  accountIndexRefusalOf,
  identityChangedOf,
  tokenExpiryOf,
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
      await mod.default(`${base}/fauna_wasm_launch_bg.wasm`);
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
}

/**
 * The launch clock this tab signs in on — `{offset_secs, now_secs}`, the web
 * leg of the cross-app `clock` state key (`fauna_e2e_agent::CLOCK_KEY` owns
 * the shape) the wrong-clock launch witness reads. Test builds only: its sole
 * caller is `$lib/e2e-automation`, and the two getters exist only in the
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
// — tabs share one `localStorage`; `$lib/accounts` has the same shape).
// Callers must have awaited `ensureLaunchWasm()` (the async wrappers below do
// it). Records cross the boundary as serde JSON strings.

function registryFns() {
  return wasm() as unknown as {
    registryHasIdentity(): boolean;
    registryLoadPendingInvite(): string | undefined;
    registrySavePendingInvite(json: string): Promise<boolean>;
    registryDeletePendingInvite(): Promise<void>;
    registryLoadAwaitingDns(): string | undefined;
    registrySaveAwaitingDns(json: string): Promise<boolean>;
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
 * rest is read straight out of localStorage with no wasm, which is what makes
 * the report available even when the two disagree.
 *
 * Values are never logged — only presence, counts, versions, and 8-char actor
 * prefixes for correlation. Costs one localStorage scan on the launch path.
 */
interface IndexBlobShape {
  active?: string | null;
  accounts?: unknown[];
  schema_version?: number;
  min_reader_version?: number;
}

export async function launchIdentityInputs(): Promise<Record<string, unknown>> {
  const raw = typeof localStorage === 'undefined' ? null : localStorage.getItem('fauna/index');
  const report: Record<string, unknown> = {};

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
        const secretRow =
          typeof localStorage === 'undefined'
            ? null
            : localStorage.getItem(`fauna/${active}/secret`);
        // Cause (3) reads off this line: `active_secret_row: 'absent'` is the
        // index naming an account the store has no secret for.
        report.active_secret_row = secretRow ? 'present' : 'absent';
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

/** `false` means the write was refused (no active identity / unparseable). */
export async function registrySaveAwaitingDns(rec: AwaitingDnsJson): Promise<boolean> {
  await ensureLaunchWasm();
  return registryFns().registrySaveAwaitingDns(JSON.stringify(rec));
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
