import { base } from '$app/paths';
import type { LocalizedText } from '$lib/i18n/localized';

let wasmModule: typeof import('../../static/fauna_wasm_onboarding.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the onboarding wasm chunk exactly once per page load. The init
 *  *promise* is memoized (same load-bearing shape as `wasm.ts::ensureWasm`):
 *  two concurrent callers await the SAME in-flight init, so `mod.default()`
 *  runs once — a second concurrent call would re-instantiate the chunk and
 *  reset wasm linear memory, dangling every live wasm object. A failed init
 *  drops the cached promise so a later call can retry. */
export function ensureOnboardingWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_onboarding.js');
      await mod.default(`${base}/fauna_wasm_onboarding_bg.wasm`);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) throw new Error('Onboarding WASM not initialized — call ensureOnboardingWasm() first');
  return wasmModule;
}

// ── OnboardingMachine ──────────────────────────────────────
//
// Same module-identity discipline documented in `wasm.ts:17-30`: keep the
// constructor call inside the module that holds the singleton `wasmModule`
// reference. Different Vite chunks would carry separate copies of the
// wasm-bindgen boilerplate and constructing a class from a different chunk
// would hit uninitialized memory.

export interface OnboardingMachineObserver {
  onChanged(): void;
}

// `providerBaseUrls` only ever has a value inside the `__FAUNA_E2E_AUTOMATION__`
// branch of `$lib/onboarding/machine.svelte.ts`, which a production `vite build`
// constant-folds away. The wasm chunk is flavored to match (testing.md
// convention 15): the `test-helpers` build's constructor takes the override map,
// the production build's takes `observer` alone and this second argument is
// ignored by ordinary JS arity rules. One call shape, no channel in production.
export async function createOnboardingMachine(
  observer: OnboardingMachineObserver,
  providerBaseUrls?: Record<string, string>,
): Promise<unknown> {
  await ensureOnboardingWasm();
  type Ctor = new (
    o: OnboardingMachineObserver,
    urls?: Record<string, string>,
  ) => unknown;
  return new (wasm() as unknown as { OnboardingMachine: Ctor }).OnboardingMachine(
    observer,
    providerBaseUrls,
  );
}

/**
 * The **machine-free** arms of the cross-app E2E machine-method bridge — the pin
 * seed and its reader, which touch only process-global state.
 *
 * Resolves the dispatcher's JSON result, or `undefined` when the name genuinely
 * needs a live `OnboardingMachine` (so the caller can fall back to the
 * machine-bound hook instead of guessing). See
 * `libs/fauna-wasm-onboarding/src/lib.rs::call_machine_free_method_for_test` for
 * why web needs a machine-free door at all.
 *
 * Only exists in the `test-helpers` wasm flavour (convention 15); the export is
 * absent from a production chunk, which is why the lookup is defensive.
 */
export async function callMachineFreeMethodForTest(
  name: string,
  jsonArg: string,
): Promise<string | undefined> {
  await ensureOnboardingWasm();
  const fn = (wasm() as unknown as {
    callMachineFreeMethodForTest?: (n: string, a: string) => string | undefined;
  }).callMachineFreeMethodForTest;
  if (typeof fn !== 'function') {
    throw new Error(
      'callMachineFreeMethodForTest is absent from this wasm chunk — the ' +
        'onboarding chunk was not built with `test-helpers`',
    );
  }
  return fn(name, jsonArg);
}

// ── AdminNatModeMachine (Admin → Nest NAT-mode control) ──────────────────
//
// The wasm twin of the shared `AdminNatModeMachine`
// (`fauna_onboarding_machine::admin_nat_mode`) — the post-onboarding change
// surface for the NAT axis (`admin-nest-nat-mode-*`; admin.md § Nest →
// NAT-mode control). Same commit ceremony as the wizard's nat_mode_choice
// page; dispatch-style: await an action, re-read `snapshotJson()`.

export interface AdminNatModeMachine {
  /** JSON-serialized `NatModeSnapshot` (same shape as the wizard's). */
  snapshotJson(): string;
  /** Radio wiring; `mode` is the lowercase wire form (`"public"`/`"private"`). */
  select(mode: string): void;
  /** Page load: read `fauna.setup.status`, pre-select the current mode. */
  hydrate(): Promise<void>;
  /** Sign + commit the selected mode via the mutable `fauna.setup.nat_mode`. */
  submit(): Promise<void>;
}

export async function createAdminNatModeMachine(
  nestUrl: string,
  secretHex: string,
): Promise<AdminNatModeMachine> {
  await ensureOnboardingWasm();
  type Ctor = new (nestUrl: string, secretHex: string) => AdminNatModeMachine;
  return new (wasm() as unknown as { AdminNatModeMachine: Ctor }).AdminNatModeMachine(
    nestUrl,
    secretHex,
  );
}

// ── Sync helpers (caller must have awaited ensureOnboardingWasm) ──

export function formatPrice(cents: bigint, currency: string): string {
  return (wasm() as unknown as { formatPrice: (c: bigint, cur: string) => string }).formatPrice(cents, currency);
}

/// The TLD of the onboarding handle's domain (the substring after the domain's
/// last dot), or `undefined` when there is no real TLD — the dns_config
/// no-provider message hides. Single-sourced in shared Rust
/// (`fauna_onboarding_machine::handle_tld`; the native apps call the
/// same-named UniFFI twin). Sync — only read mid-onboarding, after the
/// OnboardingMachine (and thus the wasm) is initialized, like `formatPrice`.
export function handleTld(handle: string): string | undefined {
  return (wasm() as unknown as { handleTld: (h: string) => string | undefined }).handleTld(handle);
}

export function serverTypeLabel(serverTypeJson: string): string {
  return (wasm() as unknown as { serverTypeLabel: (s: string) => string }).serverTypeLabel(serverTypeJson);
}

/// RAM gate for the `vps-config-mail-mode-toggle`: whether a server type may be
/// selected given the chosen mail mode (mail ON requires `mem_gb >= 2`; mail OFF
/// allows every plan). Single-sourced in shared Rust
/// (`fauna_onboarding_machine::server_type_allowed_for_mail`; the native apps
/// call the same-named UniFFI twin). Sync — only read mid-onboarding, like
/// `serverTypeLabel`.
export function serverTypeAllowedForMail(serverTypeJson: string, enableMail: boolean): boolean {
  return (
    wasm() as unknown as { serverTypeAllowedForMail: (s: string, e: boolean) => boolean }
  ).serverTypeAllowedForMail(serverTypeJson, enableMail);
}

/// Re-qualify a bare-localpart admin handle with its mail domain for a
/// factory-reset re-claim, single-sourced in shared Rust
/// (`fauna_onboarding_machine::qualify_reclaim_handle`; the native apps call
/// the same-named UniFFI twin). An empty/already-`@` handle is returned
/// unchanged; otherwise the cached domain (or, absent it, the nest-URL host) is
/// appended. Async — the admin-settings page that calls this for factory reset
/// has not loaded the onboarding wasm chunk, so it ensures init first (unlike the
/// sync `formatPrice`/`serverTypeLabel`, which only run mid-onboarding). See
/// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
export async function qualifyReclaimHandle(
  handle: string,
  domain: string | undefined,
  nestUrl: string,
): Promise<string> {
  await ensureOnboardingWasm();
  return (
    wasm() as unknown as {
      qualifyReclaimHandle: (h: string, d: string | undefined, n: string) => string;
    }
  ).qualifyReclaimHandle(handle, domain, nestUrl);
}

/// The `provisioning-elapsed` ticker decision as a `LocalizedText` `{ key, args }`
/// (or `null` before the run starts), single-sourced in shared Rust and resolved
/// SPA-side via `resolveLocalized`. Mirrors the UniFFI `provisioning_elapsed` the
/// native apps call. See `docs/goal/behavior/value-formatting.md`.
export function provisioningElapsedRaw(
  startedAtMs: number | undefined,
  finishedAtMs: number | undefined,
  nowMs: number,
): LocalizedText | null {
  return (
    wasm() as unknown as {
      provisioningElapsed: (s?: number, f?: number, n?: number) => LocalizedText | null;
    }
  ).provisioningElapsed(startedAtMs, finishedAtMs, nowMs) ?? null;
}

/// The canonical `provisioning-step-checkbox` glyph (`○`/`…`/`—`/`✓`/`✗`) for a
/// step's status, single-sourced in shared Rust
/// (`fauna_provisioning::progress::status_glyph`; the native apps call the
/// same UniFFI twin). `status` is the serde variant name off the parsed snapshot
/// (`'Pending'`/`'Running'`/…); an unrecognised status → empty string. A
/// locale-invariant symbol, so it returns the final string directly (no
/// `resolveLocalized`). See `docs/goal/behavior/value-formatting.md`.
export function provisioningStatusGlyph(status: string): string {
  return (wasm() as unknown as { provisioningStatusGlyph: (s: string) => string }).provisioningStatusGlyph(status);
}

/// The `invite_request` page's poll cadence in ms, single-sourced in shared Rust
/// (`fauna_onboarding_machine::INVITE_RECHECK_POLL_MS`; the native apps call the
/// UniFFI twin `invite_recheck_poll_ms()`). `onboarding.md` § The pending-invite
/// surface: "read by all 7 apps — never seven hand-copied numbers."
export function inviteRecheckPollMs(): number {
  return (wasm() as unknown as { inviteRecheckPollMs: () => number }).inviteRecheckPollMs();
}

/// The `awaiting_manual_dns` page's poll cadence in ms — see
/// [`inviteRecheckPollMs`]. Web restated this as a literal `10_000` until
/// 2026-08-12.
export function awaitingDnsPollMs(): number {
  return (wasm() as unknown as { awaitingDnsPollMs: () => number }).awaitingDnsPollMs();
}

/// The provisioning step name as a `LocalizedText` `{ key, args }` (canonical
/// `onboarding.provision.step.*` key family) the SPA resolves via
/// `resolveLocalized`, single-sourced in shared Rust
/// (`fauna_provisioning::progress::step_label`; the native apps call the same
/// UniFFI twin). `kind` is the serde variant name (`'Domain'`/…). Mirrors
/// `provisioningElapsedRaw`. See `docs/goal/behavior/value-formatting.md`.
export function provisioningStepLabelRaw(kind: string): LocalizedText | null {
  return (
    wasm() as unknown as { provisioningStepLabel: (k: string) => LocalizedText | null }
  ).provisioningStepLabel(kind) ?? null;
}

/// The provisioning sub-step text as a `LocalizedText` the SPA resolves via
/// `resolveLocalized`, single-sourced in shared Rust
/// (`fauna_provisioning::progress::substep_label`; the native apps call the
/// same UniFFI twin). `key` is the serde variant name (`'DomainRegistering'`/…)
/// or `null` (no sub-step → returns `null`, which `resolveLocalized` renders as
/// `''`); `cause` fills the `status_retrying` `{cause}` arg (pass the step's
/// `last_error`). The shared fn owns the variant→key mapping that web's old
/// `pascalToSnake` re-derived. See `docs/goal/behavior/value-formatting.md`.
export function provisioningSubstepLabelRaw(
  key: string | null,
  cause: string | undefined,
): LocalizedText | null {
  if (!key) return null;
  return (
    wasm() as unknown as {
      provisioningSubstepLabel: (k: string, c?: string) => LocalizedText | null;
    }
  ).provisioningSubstepLabel(key, cause) ?? null;
}

// ── Provider verification + provisioning + registrar ──

export async function verifyVpsProvider(providerJson: string): Promise<string> {
  await ensureOnboardingWasm();
  return wasm().verify_vps_provider(providerJson);
}

export async function verifyDnsProvider(providerJson: string): Promise<string> {
  await ensureOnboardingWasm();
  return wasm().verify_dns_provider(providerJson);
}

export async function registrarCheck(providerId: string, credsJson: string, domain: string): Promise<string> {
  await ensureOnboardingWasm();
  return wasm().registrar_check(providerId, credsJson, domain);
}

export async function registrarRegister(
  providerId: string, credsJson: string, domain: string, years: number,
  agreedPriceCents: bigint, contactJson: string,
): Promise<string> {
  await ensureOnboardingWasm();
  return wasm().registrar_register(providerId, credsJson, domain, years, agreedPriceCents, contactJson);
}
