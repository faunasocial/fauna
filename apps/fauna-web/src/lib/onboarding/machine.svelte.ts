// Web-side wrapper around `OnboardingMachine` from `libs/fauna-onboarding-machine`,
// exposed via wasm-bindgen as a JS class on `apps/fauna-web/static/fauna_wasm.js`.
//
// The Rust machine owns every wizard transition, network call, and validation;
// this file is a thin adapter that:
//   1. supplies the `OnboardingObserver` (Svelte 5 `$state` rune that bumps a
//      tick counter on every change),
//   2. wraps the wasm-bindgen instance in a `Proxy` that adds typed JSON helpers
//      (`dnsConfig()` / `vpsConfig()` / `visibleDnsFields()` / `loginPhase()`)
//      so views can use the canonical `m.dnsConfig()` form from the cross-app
//      substitution-rules spec without sprinkling `JSON.parse(m.dnsConfigJson())`
//      at every call site.
//
// The wizard no longer persists scratchpad state; identity confirmation
// commits the secret to the shared account registry (the long-term store the
// rest of the app reads). See the onboarding-persistence-cleanup design
// (tracked internally).
//
// `.svelte.ts` extension because module-scope rune state (`$state`) needs the
// Svelte 5 runes preprocessor.

import {
  createOnboardingMachine,
  type OnboardingMachineObserver,
} from '$lib/wasm-onboarding';
import {
  loadPendingInvite,
  savePendingInvite,
  deletePendingInvite,
} from './pending-invite-store';
import { logMessage } from '$lib/wasm';
import { setNestDialOverride } from '$lib/api';

// Svelte 5 rune-backed observer counter. Components subscribe to machine
// snapshots by referencing `machineTick.value` inside a `$derived`; any
// machine notification increments the counter, triggering re-render.
export const machineTick = $state({ value: 0 });

class TickObserver implements OnboardingMachineObserver {
  onChanged(): void {
    machineTick.value += 1;
  }
}

// Per the onboarding client target-state design (tracked internally), Rule 5:
// "Persistence happens on machine return values, not on observer ticks.
// The client persists when a method returns." So pending-invite save /
// delete is invoked from the Continue / Redeem call-sites in
// +page.svelte after they read the returned OnboardingStep + wizardOutcome().
// Callers import savePendingInvite / deletePendingInvite directly from
// './pending-invite-store'.

// ── Snapshot types ──
//
// Mirror `libs/fauna-onboarding-machine/src/state.rs` `DnsConfigState`,
// `VpsConfigState`, `FieldMetaPlain`, `LoginPhase`. Kept in sync by the
// `serde_json` round-trip — drift would surface as a runtime parse error.

// JSON field names mirror the Rust serde defaults (snake_case) — these
// types match exactly what `JSON.parse(m.dnsConfigJson())` etc. return.
// The wasm-bindgen surface uses Rust's default serde naming; UniFFI on
// native apps gets camelCase via UniFFI codegen, but the web wasm
// path goes through plain serde_json without rename_all="camelCase".

export interface TldPriceQuote {
  tld: string;
  registration_cents: number;
  renewal_cents: number;
  currency: string;
}

/** Mirror of `fauna_provisioning::registrar::ContactInfo` — WHOIS
 * contact for the buy-domain path. The 9 fields show up as the
 * `dns-contact-form` inputs when the selected registrar has
 * `registrarRequiresContact == true` (Gandi today). */
export interface ContactInfo {
  first_name: string;
  last_name: string;
  email: string;
  phone: string;
  address1: string;
  city: string;
  state: string;
  postal_code: string;
  country: string;
}

/** Mirror of `fauna_provisioning::registrar::RegistrarAvailability`. */
export type RegistrarAvailability =
  | { Buyable: { price_cents: number; currency: string | null } }
  | 'Unavailable'
  | 'TldNotSupported';

/** Mirror of `fauna_provisioning::dns::DnsZone`. Opaque to the view —
 * only the count matters for "ProviderHasDomain" gating, which the
 * machine does for us via `provider_status()`. */
export interface DnsZone {
  id: string;
  name: string;
}

export interface DnsConfigState {
  buy_domain: boolean;
  same_provider_for_vps: boolean;
  set_up_later: boolean;
  selected_provider_id: string | null;
  creds: Record<string, string>;
  verified: boolean;
  zone_id: string | null;
  current_zones: DnsZone[];
  current_availability: RegistrarAvailability | null;
  contact: ContactInfo | null;
  price_agreed: boolean;
}

/** Mirror of `fauna_onboarding_machine::state::ProviderStatus`. */
export type ProviderStatus =
  | 'NotReady'
  | 'ProviderHasDomain'
  | 'RegisteredElsewhere'
  | { UnregisteredBuyable: { price_cents: number; currency: string | null } }
  | 'UnregisteredNotBuyable';

export interface ServerTypeInfo {
  id: string;
  name: string;
  cores: number;
  memory_gb: number;
  disk_gb: number;
  monthly_price_cents: number;
  currency: string;
  regions: string[];
}

export interface VpsLocationInfo {
  id: string;
  name: string;
}

export interface VpsConfigState {
  selected_provider_id: string | null;
  creds: Record<string, string>;
  verified: boolean;
  server_types: ServerTypeInfo[];
  locations: VpsLocationInfo[];
  selected_server_type_id: string | null;
  selected_location_id: string | null;
  /** Mail-vs-social intent for the box (`vps-config-mail-mode-toggle`).
   * `null` = not chosen → use `m.provisionMailModeEnabled()` (the handle
   * real-domain default). Mirrors Rust `VpsConfigState.enable_mail`. */
  enable_mail: boolean | null;
}

/** Mirror of `fauna_onboarding_machine::state::FieldMetaPlain` returned
 * by `m.visibleDnsFields()` / similar getters. Note this is different
 * from `$lib/generated/providers.ts`'s `FieldMeta` (which is the static
 * registry shape with camelCase + a richer field set). */
export type FieldTypePlain = 'Text' | 'Secret' | 'Select' | 'HostedAuth';
export type CapabilityPlain = 'Dns' | 'Vps' | 'Registrar';

/** Which credential form a `hosted-auth` field is being driven on — mirrors
 * `fauna_onboarding_machine::state::CredentialForm`. The two forms hold
 * separate credential bags (`dnsConfig().creds` / `vpsConfig().creds`). */
export type CredentialForm = 'dns' | 'vps';

/** What `hostedAuthBegin` hands back: the URL to open through the app's
 * existing open-URL affordance and the code to show beside it. Mirrors
 * `fauna_onboarding_machine::state::HostedAuthPrompt`. */
export interface HostedAuthPrompt {
  verification_url: string;
  user_code: string;
}

/** Where a `hosted-auth` field's sign-in stands — the button's label source
 * (`onboarding.md` § 4). Mirrors
 * `fauna_onboarding_machine::state::HostedAuthState`. */
export type HostedAuthState =
  | 'Idle'
  | { Pending: { user_code: string; verification_url: string } }
  | 'Connected'
  | { Failed: { message: string } };

export interface FieldMeta {
  id: string;
  field_type: FieldTypePlain;
  label_key: string;
  required: boolean;
  kinds: CapabilityPlain[];
}

export type LoginPhase =
  | 'Detecting'
  | 'Unclaimed'
  | 'Unregistered'
  | { WelcomeBack: { old_handle: string } };

// ── Localized text (i18n key + args) ──
//
// Mirrors `fauna_onboarding_machine::state::LocalizedText`. `key` indexes
// into `i18n/strings/en.yaml`; `args` is a flat substitution map. Resolve one
// to display text with `resolveLocalized()` ($lib/i18n/localized).
export interface LocalizedText {
  key: string;
  args: Record<string, string>;
}

// ── HandleCheck snapshot ──
//
// Mirrors `libs/fauna-onboarding-machine/src/snapshots/handle_check.rs`.
// The handle field on the wizard is `alice@example.com`-shaped; the
// machine drives the snapshot through Idle → Parsing → DnsLookup →
// NestProbe → ChallengeResponse → PriceLookup → Complete as it probes.
// Outcome fans out into the variants the wizard surfaces to the UI.

export type HandleCheckPhase =
  | 'Idle' | 'Parsing' | 'DnsLookup' | 'NestProbe'
  | 'ChallengeResponse' | 'PriceLookup' | 'Complete';

export type HandleCheckOutcome =
  | 'None'
  | 'FormatInvalid'
  | 'TldInvalid'
  | 'RegisteredNoNest'
  | 'NestRunningUserUnregistered'
  | { DomainAvailable: { buyable_via_provider: boolean; price: TldPriceQuote | null } }
  | { AlreadyOnNest: { handle_matches: boolean; current_handle: string | null } }
  | { ProbeError: { phase: HandleCheckPhase; transient: boolean; cause: string } };

export interface HandleCheckSnapshot {
  phase: HandleCheckPhase;
  outcome: HandleCheckOutcome;
  message: LocalizedText;
  continue_enabled: boolean;
  control_checkbox_visible: boolean;
  control_checkbox_checked: boolean;
}

// ── ClaimCode snapshot ──
//
// Mirrors `libs/fauna-onboarding-machine/src/snapshots/claim_code.rs`.
// Reached only when handle-check returns UnregisteredUnclaimedNest:
// the wizard exits with `WizardOutcome::LoggedIn` after the one-time
// `fauna.auth.claim_admin` WS-RPC call succeeds. Submit is the terminal action;
// no separate Continue button.
export type ClaimCodeState =
  | 'Idle'
  | 'Submitting'
  | 'Claimed'
  | { Invalid: { reason: string } }
  | { Error: { transient: boolean; cause: string } };

export interface ClaimCodeSnapshot {
  state: ClaimCodeState;
  message: LocalizedText;
  submit_enabled: boolean;
}

// ── NatMode snapshot (the terminal admin-path step) ──
//
// Mirrors `libs/fauna-onboarding-machine/src/snapshots/nat_mode.rs`.
// Per `docs/goal/behavior/onboarding.md` § 3b-bis.

/** The nest's NAT axis. Note the **lowercase** wire form: `NodeMode` carries
 * `#[serde(rename_all = "snake_case")]` (`libs/fauna-core/src/nat_mode.rs`).
 * The same strings go back in via `selectNatMode`. */
export type NodeMode = 'public' | 'private';

export type NatModeState =
  | 'Choosing'
  | 'Submitting'
  | 'Done'
  | { Error: { transient: boolean; cause: string } };

export interface NatModeSnapshot {
  state: NatModeState;
  /** The mode `nat-mode-confirm-button` will commit. Pre-selected from the
   * nest's seeded `node_mode`, refined private-ward when the target is a
   * private-network address — so the common case is confirm-only. */
  selected_mode: NodeMode;
  message: LocalizedText;
  /** True iff `nat-mode-confirm-button` should be enabled: `Choosing` and
   * `Error` (the set is mutable, so resubmit is always allowed); disabled in
   * `Submitting` / `Done`. */
  submit_enabled: boolean;
}

// ── InviteRequest snapshot ──
//
// Mirrors `libs/fauna-onboarding-machine/src/snapshots/invite_request.rs`.
// The wizard does NOT exit from PendingReview (that exit retired 2026-08-12):
// it stays on `invite_request` and polls. The glue writes the request_id and
// the JSON-serialized state at the SUBMIT return, from `pendingInviteSlot()`,
// so a relaunch can reseed the wizard via `seedPendingInvite`.

export interface InviteQuota {
  storage_bytes: number;
  traffic_bytes_per_month: number;
}

export type ErrorContext = 'Submitting' | 'Rechecking' | 'Redeeming';

export type InviteRequestState =
  | 'Idle'
  | 'Submitting'
  | 'Rechecking'
  // (`Approved` retired 2026-08-12 — an admin approve deletes the request row,
  // so no live nest ever served it; approval is detected as admission.)
  | { Denied: { reason: string; request_id: string } }
  | { PendingReview: { request_id: string; last_checked_ms: number } }
  | { Error: { transient: boolean; context: ErrorContext; cause: string } };

export type OobCodeState =
  | 'Idle'
  | 'Verifying'
  /** `supervised_by` = the guardian's handle when the code carries a supervised
   *  designation (`fauna.account.invite_code.verify`'s additive reply field —
   *  family-safety.md § Wire & data shape); null/absent for an ordinary code.
   *  Rendered as `invite-code-supervised-notice` BEFORE redemption. */
  | { Valid: { invite_id: string; supervised_by?: string | null } }
  | { Invalid: { reason: string } }
  | { Error: { cause: string } };

export interface InviteRequestSnapshot {
  state: InviteRequestState;
  message: LocalizedText;
  continue_enabled: boolean;
  recheck_visible: boolean;
  out_of_band_code_state: OobCodeState;
  /** Localized text for the OOB-code row's status label, derived from
   * `out_of_band_code_state` in shared Rust per
   * `docs/goal/behavior/onboarding.md` Architectural rule 4. Refreshed on every
   * `inviteRequestSnapshot()` read. */
  oob_message: LocalizedText;
}

// ── "Almost ready" (awaiting manual DNS) snapshot ──
//
// Mirrors `fauna_onboarding_machine::snapshots::awaiting_manual_dns`. The
// deferred-DNS wait: the nest is provisioned but its DNS hasn't propagated, so
// it is neither reachable nor claimed. `message` is the machine's wording for
// the current state (including the terminal `Error { cause }`) — the client
// resolves it, never re-derives it.

export type AwaitingDnsState =
  | 'Pending'
  | 'Checking'
  | 'Claiming'
  | 'Claimed'
  | { Error: { cause: string } };

/** serde's snake_case shape — `record_type`, NOT the binding's `recordType`. */
export interface DnsRecordPlain {
  record_type: string;
  name: string;
  value: string;
  ttl: number;
  priority?: number | null;
}

export interface AwaitingManualDnsSnapshot {
  state: AwaitingDnsState;
  dns_records: DnsRecordPlain[];
  message: LocalizedText;
}

// ── Provisioning snapshot ──
//
// Mirrors `libs/fauna-provisioning/src/progress.rs`. The page rendered
// from this snapshot has fixed shape: four step rows (Domain, Server,
// Dns, Online) plus overall status. The Rust orchestrator drives every
// transition; the client renders the snapshot and forwards Cancel /
// Retry gestures.

export type ProvisionStep = 'Domain' | 'Server' | 'Dns' | 'Online';

export type StepStatus = 'Pending' | 'Running' | 'Skipped' | 'Succeeded' | 'Failed';

export type OverallStatus = 'Idle' | 'Running' | 'Succeeded' | 'Failed' | 'Cancelled';

/** Typed sub-step keys mirrored from `fauna_provisioning::progress::SubstepKey`.
 * The view maps each to the localized substep string under
 * `onboarding.provision.substep` in `i18n/strings/en.yaml`. */
export type SubstepKey =
  | 'DomainCheckingAvailability'
  | 'DomainRegistering'
  | 'DomainVerifyingZone'
  | 'ServerGeneratingDkim'
  | 'ServerCreating'
  | 'DnsAddingDomainRecords'
  | 'DnsAddingEmailRecords'
  | 'DnsSettingReverseDns'
  | 'OnlineWaiting'
  | 'OnlineClaiming'
  | 'StatusSkipped'
  | 'StatusRetrying'
  | 'StatusCancelling'
  | 'StatusCancelled';

export type SkipReason =
  | 'ZoneAlreadyVerified'
  | 'ServerAlreadyExists'
  | 'DnsRecordAlreadyExists'
  | 'PtrAlreadySet'
  | 'NestAlreadyOnline';

export interface StepSnapshot {
  kind: ProvisionStep;
  status: StepStatus;
  substep: SubstepKey | null;
  attempt: number;
  max_attempts: number;
  last_error: string | null;
  skip_reason: SkipReason | null;
  started_at_ms: number | null;
  finished_at_ms: number | null;
  /**
   * Display-projection booleans — the single source of the per-step visibility
   * rule, computed once in shared Rust (`StepSnapshot::recompute_display` in
   * `libs/fauna-provisioning/src/progress.rs`) and `enrich_display`-ed onto the
   * snapshot the machine's `provisioning_snapshot()` getter returns. Read these
   * directly; never re-derive the rule from `status`/`substep`/`attempt` (that
   * is the drift this projection exists to kill — Failed-retried-no-substep and
   * bare-Running were both wrong here before).
   */
  shows_substep: boolean;
  shows_error: boolean;
  shows_attempt_suffix: boolean;
}

export interface ProvisionResultPlain {
  server_id: string;
  ipv4: string;
  domain: string;
  claim_code: string;
}

export interface ProvisioningSnapshot {
  overall: OverallStatus;
  /** Always exactly four entries in fixed order: Domain, Server, Dns, Online. */
  steps: StepSnapshot[];
  started_at_ms: number | null;
  finished_at_ms: number | null;
  /** Set when overall == Succeeded. */
  result: ProvisionResultPlain | null;
  /** Set when overall == Failed or Cancelled. */
  final_error: string | null;
}

/**
 * One priced line item on `nest_provisioning`'s top-region price summary
 * (`provisioning-price-bom`). Mirrors
 * `fauna_onboarding_machine::state::BillOfMaterialsItem`. `label` reuses the
 * same `onboarding.provision.step.{domain,server}` keys the progress rows
 * render — see `OnboardingMachine::bill_of_materials`. `docs/goal/behavior/
 * onboarding.md` §6.
 */
export interface BillOfMaterialsItem {
  label: LocalizedText;
  price_cents: number;
  currency: string;
  /** `true` for the VPS's monthly charge, `false` for the domain's one-time
   * registration charge. */
  recurring: boolean;
  /** The registrar's quoted renewal price, when it quoted one — the
   * recurring cost disclosed alongside the domain's one-time first-year
   * price (`onboarding.md` § 6: said before the charge, not after). `null`
   * for the VPS's recurring item (every adapter but this one is
   * `renewal_cents: None`). */
  renewal_price_cents: number | null;
}

// ── Wizard outcome ──
//
// Mirrors `libs/fauna-onboarding-machine/src/outcome.rs`. `wizardOutcome()`
// returns `undefined` while the wizard is still running; once a terminal
// transition fires the machine populates the outcome and stays at
// OnboardingStep::Done.
export type WizardOutcome =
  | { LoggedIn: { nest_url: string; handle: string } }
  // (`InviteSubmitted` retired 2026-08-12 — the pending-review journey never
  // exits the wizard; see `pendingInviteSlot`.)
  | { AwaitingManualDns: { nest_url: string; dns_records: DnsRecordPlain[]; claim_code: string } };

/** The resume slot to write at the `wizardSubmitInviteRequest()` return —
 *  `onboarding.md` § 3 Persistence callouts' "only write moment". Assembled by
 *  shared Rust so the two silent-when-wrong rules (state nest_url, opaque
 *  status_json) are not re-derived per app. */
export type PendingInviteSlot = {
  nest_url: string;
  handle: string;
  request_id: string;
  status_json: string;
};

// ── IdentityOrigin ──
//
// Mirrors `fauna_onboarding_machine::state::IdentityOrigin`. `null` until
// the user picks Create or Import in IdentityChoice.
export type IdentityOrigin = 'Created' | 'Imported' | null;

// Type expressing the Proxy-augmented machine: every method on the inner
// wasm-bindgen `OnboardingMachine` plus the typed JSON helpers below. The
// view layer treats this as a single object.
export interface Machine {
  // Sync getters
  step(): string;
  currentHandle(): string;
  errorMessage(): string | undefined;
  isLoading(): boolean;
  generatedSecret(): string | undefined;
  nestUrl(): string;
  claimCode(): string;
  inviteCode(): string;
  localNestReachable(): boolean;
  domainStatus(): string | undefined;
  // NOTE: there is deliberately NO `dnsStatusText(): string` here. The wasm
  // surface only exposes `dnsStatusTextKey()` (a LocalizedText); a plain
  // `dnsStatusText()` was never implemented and calling it throws. Resolve the
  // key via `resolveLocalized(dnsStatusTextKey())` at the call site instead.
  dnsPostInstructions(): string | undefined;
  canVerifyDns(): boolean;
  canContinueDns(): boolean;
  canVerifyVps(): boolean;
  canContinueVps(): boolean;
  /** The reason `vps-config-continue-button` is disabled, or `undefined`
   * when it's live. A disabled control owes the user a reason
   * (`ui/README.md` § Copy comprehensibility rule 5). */
  vpsContinueBlockedReason(): LocalizedText | undefined;
  /** The reason `provisioning-continue-button` is disabled, or `undefined`
   * when it's live — the four `○` step glyphs are a symbol, not a reason
   * (`ui/README.md` rule 5). */
  provisioningContinueBlockedReason(): LocalizedText | undefined;
  // Typed JSON helpers (Proxy-supplied)
  dnsConfig(): DnsConfigState;
  vpsConfig(): VpsConfigState;
  visibleDnsFields(): FieldMeta[];
  visibleVpsFields(): FieldMeta[];
  loginPhase(): LoginPhase;
  handleCheckSnapshot(): HandleCheckSnapshot;
  inviteRequestSnapshot(): InviteRequestSnapshot;
  /** Snapshot of the claim_code page. Reached only when handle-check
   * returns UnregisteredUnclaimedNest. Pre-submit state is
   * `state == 'Idle'`. */
  claimCodeSnapshot(): ClaimCodeSnapshot;
  /** Snapshot of the four-step provisioning orchestrator run. The page
   * re-reads this on every observer tick. Pre-run / no-run state is
   * `overall == 'Idle'` with all four steps `Pending`. */
  provisioningSnapshot(): ProvisioningSnapshot;
  /** Up to two priced line items for `nest_provisioning`'s top-region price
   * summary — domain (one-time, only when buying a new domain) then VPS
   * (recurring, always present). Pure computation over already-in-state
   * DNS/VPS data. See `BillOfMaterialsItem`. */
  billOfMaterials(): BillOfMaterialsItem[];
  // ── hosted-auth (a bundled provider's hosted sign-in, onboarding.md § 4) ──
  // `form` is which credential form the field lives on.
  /** Step 1: POSTs the device-authorization request. Returns the URL to
   * open (through `window.open`, synchronously inside the click handler —
   * awaiting this first breaks the user-gesture chain some browsers need to
   * avoid a popup block) and the code to show beside it. Follow with
   * `hostedAuthWait`. */
  hostedAuthBegin(form: CredentialForm, fieldId: string): Promise<HostedAuthPrompt>;
  /** Step 2: resolves once the token has landed in the form's creds
   * (`hostedAuthState` reads `Connected`) or rejects when the attempt
   * ended. */
  hostedAuthWait(form: CredentialForm, fieldId: string): Promise<void>;
  /** The field's button label source. `Idle` for a field never started. */
  hostedAuthState(form: CredentialForm, fieldId: string): HostedAuthState;
  /** Whether the sign-in button is pressable: the form's `base-url` is
   * filled in and no attempt on this field is mid-flight. */
  hostedAuthCanBegin(form: CredentialForm, fieldId: string): boolean;
  /** `undefined` while the wizard is still running. */
  wizardOutcome(): WizardOutcome | undefined;
  /** The identity secret this run authenticated with, straight from the
   * machine — never `localStorage`. `undefined` is structurally impossible
   * at the `LoggedIn` terminal (the wizard cannot reach it without a
   * secret); callers there treat that as a loud bug, not a fallback path.
   * Passthrough — same name on the wasm binding (onboarding.md § Long-term
   * store contract → *The terminal reads the secret from the MACHINE,
   * never from the store*). */
  effectiveSecret(): string | undefined;
  /** The reach address the provisioning run captured — the box's public IPv4 —
   * or `undefined` on every path that did not provision a box. Read at the
   * `LoggedIn` terminal (before the wizard is torn down, like the secret above)
   * and persisted as the account's **reach hint**, so the first main-app session
   * opens connected while the domain is still propagating. Passthrough — same
   * name on the wasm binding (onboarding.md § Reach hint). */
  provisionReachIpv4(): string | undefined;
  /** JSON `{provider_id, fields, label}` of the DNS-provider credential the
   * wizard verified at the DNS step, or `undefined` on the manual / set-up-later
   * / returning-user paths. The launched client seals it into `fauna.state.dns`
   * at `LoggedIn` (onboarding→launch hand-off — dns-management.md § Where the
   * credential lives). */
  capturedDnsCredentialJson(): string | undefined;
  identityOrigin(): IdentityOrigin;
  dnsStatusTextKey(): LocalizedText;
  /** Per-provider DNS status driven by current snapshot inputs. */
  providerStatus(): ProviderStatus;
  /** Whether the DNS-provider button for `providerId` should be selectable
   * given the current buy_domain / same_provider_for_vps choices — the shared
   * `buy_domain → Registrar` / `same_provider_for_vps → Vps` capability rule
   * (onboarding.md § 4). Replaces re-deriving the rule per client. */
  dnsProviderEligible(providerId: string): boolean;
  /** The reason `dns-provider-row[providerId]` is disabled, or `undefined`
   * when it's selectable. A disabled control owes the user a reason
   * (`ui/README.md` § Copy comprehensibility rule 5). Wrapper-supplied
   * (parametrized — doesn't fit the generic zero-arg
   * `TYPED_JSON_HELPERS` path); see `wrapMachine`. */
  dnsProviderIneligibleReason(providerId: string): LocalizedText | undefined;
  /** Whether the dns_config page should render the WHOIS contact form:
   * selected registrar requires a contact AND `providerStatus()` is
   * `UnregisteredBuyable` (the buy-domain path actually runs). */
  shouldShowContactForm(): boolean;
  /** Whether the registrar-specific notes blurb should be shown: buy-domain
   * path AND the selected provider declares a registrar-notes key. */
  shouldShowRegistrarNotes(): boolean;
  /** Whether the `dns-no-provider-message` warning should be shown: buy-domain
   * path AND the (early) handle-check outcome is `DomainAvailable` with
   * `buyable_via_provider == false` — i.e. no supported registrar carries the
   * TLD. Shared with native apps; replaces the web-only LATE,
   * provider-specific `providerStatus() === 'UnregisteredNotBuyable'` predicate
   * (onboarding.md § 4 dns_config). */
  shouldShowNoProviderMessage(): boolean;
  /** WHOIS contact setter for the buy-domain path. The wasm binding
   * marshals via JSON; the wrapper does the JSON.stringify so callers
   * can pass a typed object. */
  setContact(contact: ContactInfo): void;
  // Sync mutators
  setCurrentHandle(h: string): void;
  /** The nest hint (onboarding.md § 2 Handle entry → *Nest hint*): hand the
   * raw `nest` query value over untouched. The shared machine classifies it,
   * pre-fills the handle's domain part, and drops an unclassifiable one
   * silently (`false`) — the page never parses it itself. */
  setNestHint(raw: string): boolean;
  toggleBuyDomain(on: boolean): void;
  toggleSameProviderForVps(on: boolean): void;
  selectDnsProvider(id: string): void;
  setDnsCred(fieldId: string, value: string): void;
  dnsSetUpLater(): void;
  selectVpsProvider(id: string): void;
  setVpsCred(fieldId: string, value: string): void;
  selectVpsServerType(id: string): void;
  /** `vps-config-mail-mode-toggle`: whether the box provisions the mail
   * subsystem (mail box vs social-only). Drives `CloudInitParams::enable_mail`
   * + the server-type RAM gate. See onboarding.md §5. */
  setProvisionMailMode(enabled: boolean): void;
  /** Resolved mail-mode for the toggle's checked state — the user's choice,
   * else the handle real-domain default. See onboarding.md §5. */
  provisionMailModeEnabled(): boolean;
  selectVpsLocation(id: string): void;
  confirmPrice(): void;
  back(): void;
  setNestUrl(url: string): void;
  clearError(): void;
  reset(): void;
  beginCreateIdentity(): void;
  beginImportIdentity(): void;
  /**
   * `beginImportIdentity`, carrying the reason the user was *sent* there —
   * today only the launch flow's `superseded` refusal
   * (`identity-succession.md` § Propagation → *Own device fleet*). Step and
   * reason land under one machine mutation, so no observer tick can render the
   * import screen without the explanation that justifies it; a reason written
   * to a page-local slot instead would be erased by the very tick this fires
   * (`errorMessageValue` re-reads `errorMessage()` on every tick). Shared with
   * tui and linux — the same transition, not a per-app one each.
   */
  beginImportIdentityWithReason(reason: string): void;
  /**
   * Confirms the freshly-generated identity, commits it to the account
   * registry (moment 1 — a no-op write in append mode, see
   * `commitConfirmedIdentity`), and advances the wizard to `HandleEntry`.
   * Returns the 64-hex secret. Wrapper-supplied — see `wrapMachine`.
   */
  confirmGeneratedIdentity(): string;
  /**
   * Validates and imports a user-supplied 64-hex secret, commits it to the
   * account registry (moment 1 — a no-op write in append mode), and advances
   * the wizard to `HandleEntry`. Returns the validated secret.
   * Wrapper-supplied — see `wrapMachine`.
   */
  confirmImportedIdentity(secret: string): string;
  /**
   * Pre-seeds an existing secret (the registry's active account, read on app
   * launch) and lands the wizard at `HandleEntry`. Used when a user has an
   * identity but no nest URL yet — avoids re-walking IdentityChoice.
   */
  seedIdentity(secret: string): void;
  // Async actions
  verifyDns(): Promise<void>;
  continueFromDns(): void;
  verifyVps(): Promise<void>;
  /** Transition `vps_config` → `nest_provisioning`. Async to mirror the
   * UniFFI surface; the body is a pure step transition with no IO, but
   * keeping native and web in lockstep makes per-app code identical.
   * The orchestrator does NOT spawn here — the user lands on
   * `nest_provisioning` with an idle snapshot and explicitly clicks
   * the page's `provisioning-start-button` which calls
   * `startProvisioning()`. */
  continueFromVps(): Promise<void>;
  /** Spawns the four-step orchestrator on the WASM runtime and returns
   * immediately. Idempotent — pre-flight checks short-circuit completed
   * work, so safe to call after a partial prior run. Observer ticks
   * drive snapshot re-render. */
  startProvisioning(): void;
  /** Soft-cancel the in-flight provisioning run. The orchestrator
   * observes the flag at the next step boundary or retry iteration;
   * already-created VPS/DNS resources stay. */
  cancelProvisioning(): void;
  /** Re-run provisioning from the top. Idempotency makes already-done
   * steps short-circuit. Re-uses the same wizard inputs (handle, DNS,
   * VPS) collected in earlier stages. Sync (fire-and-forget spawn). */
  retryProvisioning(): void;
  /** Bottom-row Continue on nest_provisioning. Refuses unless overall ==
   * Succeeded. Returns the next OnboardingStep name (Debug-formatted —
   * `"DnsPostInstructions"` for deferred-DNS, `"Done"` otherwise). On
   * `"Done"` query `wizardOutcome()` to route. */
  continueFromProvisioning(): string;
  /** Continue on dns_post_instructions. Sets wizardOutcome to
   * AwaitingManualDns and returns `"Done"`. */
  continueFromDnsPostInstructions(): string;
  // Handle-check (handle-first wizard)
  startHandleCheck(handle: string): Promise<void>;
  cancelHandleCheck(): void;
  setControlCheckbox(checked: boolean): void;
  /** handle-entry-continue-button wiring. Returns the next OnboardingStep
   * name (Debug-formatted, e.g. `"DnsConfig"` / `"InviteRequest"` /
   * `"Done"`). On `"Done"` query `wizardOutcome()` to route. */
  submitHandleCheckContinue(): Promise<string>;
  // Invite-request (snapshot-driven flow)
  /** invite-request-submit-button wiring. No message arg — the wizard
   * owns the request body. Returns the next OnboardingStep name. */
  wizardSubmitInviteRequest(): Promise<string>;
  /** Returns the OnboardingStep name (Debug-formatted) the wizard moved to. */
  recheckInviteStatus(): Promise<string>;
  verifyOobInviteCode(code: string): Promise<void>;
  /** Returns the OnboardingStep name (Debug-formatted) the wizard moved to. */
  redeemInvite(): Promise<string>;
  /** The pending-invite resume slot, or `undefined` when the wizard is not in
   *  `PendingReview`. Replaced `submitInviteRequestContinue` (retired
   *  2026-08-12): that journey no longer exits the wizard, so the slot is read
   *  and written at the submit return instead. */
  pendingInviteSlot(): PendingInviteSlot | undefined;
  cancelInviteOp(): void;
  /** claim-code-submit-button wiring. Calls the `fauna.auth.claim_admin`
   * WS-RPC kind with the user-supplied code; on success exits the wizard to
   * LoggedIn (admin), on a server-side rejection the wizard stays on
   * `claim_code` with the snapshot moved to `Invalid` so the user can retry.
   * Returns the next OnboardingStep
   * name (Debug-formatted — `"Done"` on success, `"ClaimCode"` while
   * the user is still on the page). */
  wizardSubmitClaimCode(code: string): Promise<string>;
  // ── "Almost ready" (awaiting manual DNS) ──
  //
  // Not an OnboardingStep: the surface renders whenever `wizardOutcome()` is
  // `AwaitingManualDns` (onboarding.md § "Almost ready" surface).
  /** The records + state + localized message the surface renders. */
  awaitingManualDnsSnapshot(): AwaitingManualDnsSnapshot | undefined;
  /** The records as the slot stores them — serde's snake_case (`record_type`),
   * NOT the binding's camelCase (`recordType`). Write this into the long-term
   * store verbatim and hand it back to `seedAwaitingManualDnsJson` verbatim: a
   * hand-built `JSON.stringify` of the *bound* type round-trips to an EMPTY
   * list, leaving an "Almost ready" page with nothing to add at the registrar.
   * Pinned by `libs/fauna-onboarding-machine/tests/continue_from_dns_post_instructions.rs`. */
  awaitingDnsRecordsJson(): string;
  /** The one formatter every app both RENDERS and COPIES. */
  awaitingDnsRecordsText(): string;
  /** Whether the "Copy all" button has anything to copy — false for a
   * resumed standard-path run's records-less slot. Disabled, never hidden:
   * ui.yaml scopes `awaiting-dns-copy-button` to this page's required
   * elements. */
  awaitingDnsCopyEnabled(): boolean;
  /** Relaunch hydration from the long-term store's awaiting-manual-dns slot.
   * Call `seedIdentity(secret)` FIRST (so the eventual claim can sign). Sets
   * `wizardOutcome()` to `AwaitingManualDns` — exactly what the same-session
   * `continue_from_dns_post_instructions` exit produces — so the client renders
   * the surface identically on both paths. */
  seedAwaitingManualDnsJson(
    nestUrl: string, handle: string, dnsRecordsJson: string, claimCode: string,
  ): void;
  /** Relaunch hydration from the slot as ONE opaque JSON record (the string
   * `loadAwaitingDnsJson()` returns) — the door to use: the machine re-holds the
   * box's built-with identity as the first-contact root before the surface's
   * first poll, and nothing is re-shaped in JS. `false` = not a record at all. */
  seedAwaitingManualDnsRecordJson(recordJson: string): boolean;
  /** ONE single-shot probe of the provisioned nest (the client owns the cadence,
   * mirroring `recheckInviteStatus`). Returns the next OnboardingStep name —
   * `"Done"` is ALSO the still-waiting state, so check `wizardOutcome()` before
   * treating a `Done` as an exit. */
  recheckManualDns(): Promise<string>;

  /** Reseeds the wizard at `InviteRequest` from a long-term-store record.
   * Caller is responsible for navigating to `/app/onboarding`. */
  seedPendingInvite(nestUrl: string, handle: string, requestId: string, statusJson: string): void;
  /** Lands the wizard at `InviteRequest` with nest_url + handle pre-set
   * and the snapshot reset to Idle. Used by the silent-challenge
   * "secret unregistered" fallback per target §"App-launch routing"
   * — saves the user one extra click compared to dropping them at
   * handle_entry. Distinct from seedPendingInvite (which restores a
   * previously submitted request). */
  navigateToInviteRequestForKnownNest(nestUrl: string, handle: string): void;
  /** Pre-identity probe over the anonymous WS-RPC connection (`fauna.setup.status`).
   * Used by app-launch routing to discriminate claimed vs unclaimed nests
   * when silent-challenge reports the secret isn't registered. Resolves
   * to a JSON string `{claimed: boolean, mode?: "Encrypted" | "Plaintext"}`;
   * rejects on transport/decode failure. Replaces the legacy
   * `GET /api/v1/setup-status` HTTP probe. */
  probeSetupStatus(nestUrl: string): Promise<string>;
  /** Lands the wizard at `ClaimCode` with nest_url + handle pre-set
   * and the snapshot reset to Idle. Used by the silent-challenge
   * "secret unregistered + setup-status.claimed=false" fallback per
   * target § App-launch routing — silent-challenge fallback table
   * (unclaimed-nest row). */
  navigateToClaimCodeForKnownNest(nestUrl: string, handle: string): void;
  /** Factory-reset re-onboard: lands the wizard at `ClaimCode` with nest_url +
   * handle pre-set AND the returned claim `code` stashed for pre-fill
   * (`claimCodePrefill()`). The human never sees the code, so the claim-code
   * page must pre-fill the input from it. See mail-bridge-lifecycle.md
   * § Factory reset (Client affordance) + onboarding.md §3a. */
  navigateToClaimCodeForKnownNestWithCode(nestUrl: string, handle: string, code: string): void;
  /** The claim code stashed by `navigateToClaimCodeForKnownNestWithCode`, or
   * `undefined`. The claim-code page pre-fills `claim-code-input` from this. */
  claimCodePrefill(): string | undefined;
  // ── Machine-derived service-enable intents (onboarding.md §3b) ──
  // No user toggle: each is ON iff the handle targets a real registerable
  // domain (OFF for `user@localhost` / `user@<ip>`), derived from the handle
  // at claim. Read post-onboarding by the launch glue (the `LoggedIn`
  // handler) to fire the matching Admin-class `set_*_enabled` call.
  /** Read post-onboarding by the launch glue: if true, auto-mint the admin
   * mailbox (generated password) via the shared `MailSettingsMachine`
   * helper, which also enables deployment mail. */
  emailEnableRequested(): boolean;
  /** Read post-onboarding by the launch glue: if true, enable deployment
   * CalDAV via the bridge-approval machine, independently of email. See
   * caldav-server.md § Independent enablement. */
  caldavEnableRequested(): boolean;
  /** Read post-onboarding by the launch glue: if true, enable deployment
   * CardDAV via the bridge-approval machine, independently of email and
   * calendar (and mint the CardDAV-only mailbox when neither sibling did).
   * See carddav-server.md § Independent enablement. */
  carddavEnableRequested(): boolean;
  /** Read post-onboarding by the launch glue: if true, enable deployment
   * WebDAV via the bridge-approval machine, independently of email,
   * calendar, and contacts. WebDAV has no per-actor mailbox, so there is no
   * companion mint path. See webdav-server.md § Independent enablement. */
  webdavEnableRequested(): boolean;
  /** Snapshot driving the `nat_mode_choice` page (onboarding.md § 3b-bis). */
  natModeSnapshot(): NatModeSnapshot | undefined;
  /** `public-nat-mode-radio` / `private-nat-mode-radio` wiring. `mode` is the
   * lowercase wire form (`"public"` / `"private"`) — the same repr the
   * snapshot carries. */
  selectNatMode(mode: NodeMode): void;
  /** `nat-mode-confirm-button` wiring. Commits `fauna.setup.nat_mode`.
   * Returns the next OnboardingStep name — `"Done"` on success (query
   * `wizardOutcome()` → `LoggedIn`), `"NatModeChoice"` while the user stays
   * on the page after an error. */
  submitNatModeChoice(): Promise<string>;
  /** `nat-mode-defer-button` wiring. Sends nothing (the seeded mode is
   * already a working default) and returns `"Done"` with a `LoggedIn`
   * outcome — there is no resume slot to persist and no unresolved state. */
  deferNatModeChoice(): string;
  // ── One-tap "trust this box" offer (onboarding.md § 3b-ter) ──
  //
  // App capability declaration, called once at construction (below) — see
  // `initOnboardingMachine`. Routes both `nat_mode_choice` exits through the
  // `trust_prompt` interstitial instead of straight to `Done`.
  setRendersTrustPrompt(renders: boolean): void;
  /** `trust-box-grant-button` wiring. Latches the answer for the signed-in
   * handoff (`takeTrustPromptGranted`) and concludes the wizard. Returns the
   * next `OnboardingStep` name — always `"Done"`. */
  grantDefaultTrust(): string;
  /** `trust-box-skip-button` wiring. Latches nothing and concludes the
   * wizard identically to a grant. Returns the next `OnboardingStep` name —
   * always `"Done"`. */
  skipTrustPrompt(): string;
  /** Consume-once read of the trust_prompt answer, at the signed-in handoff
   * only — the one point web holds an authenticated session and can
   * dispatch `LinkedNestsAction::MintDefaultSet`. */
  takeTrustPromptGranted(): boolean;
  // ── Recovery kit + phrase restore (onboarding.md § 1 Identity) ──
  //
  // App capability declaration, called once at construction (below) — routes a
  // created identity through the `recovery_kit` offer.
  setRendersRecoveryKit(renders: boolean): void;
  /** The minted-but-unregistered kit root `recovery-kit-secret-display` shows
   *  (bare 64-hex); `undefined` outside the screen's lifetime. */
  recoveryKitSecretHex(): string | undefined;
  /** The one `fauna://recovery` URI behind `recovery-kit-qr` AND the copy
   *  button — never the bare hex (identity-succession.md § The RecoveryKey). */
  recoveryKitUri(): string | undefined;
  /** `recovery-kit-confirm-button`: to `handle_entry`, root kept for handoff. */
  confirmRecoveryKit(): void;
  /** `recovery-kit-skip-button`: the root is dropped, nothing registers. */
  skipRecoveryKit(): void;
  /** Consume-once read of the confirmed root at the signed-in handoff. */
  takePendingRecoverySecret(): string | undefined;
  /** `restore-from-recovery-kit-button` on identity_choice. */
  beginRecoveryEntry(): void;
  /** `recovery-entry-submit-button`. `restored` → the seed is back (the proxy
   *  commits it like an import); `superseded` → route to
   *  `beginImportIdentityWithReason`; `message` → the shared outcome text to
   *  render on `error-message` (it can ride a restore that succeeded). */
  submitRecoveryEntry(
    phrase: string,
  ): Promise<{ restored: boolean; superseded: boolean; message: LocalizedText | null }>;
  /** JSON `[{actor_id_hex, seed_hex}]` of recovered predecessor seeds. */
  restoredPredecessorsJson(): string;
  // ── Box-recovery branch (box-recovery.md § Recovery UI (step 4)) ──
  //
  // The step-4 total-box-loss recovery wizard branch. All pure state
  // transitions in the shared `fauna-onboarding-machine` (wasm-exported); the
  // proxy passes them straight through. The box list is injected by the client
  // glue from the reachable-nest `deploymentSeeds()` read (Task C2); the
  // machine only holds the list + the selected id (public ids — the seed
  // itself never crosses into JS).
  /** `recover-lost-box-button` on identity_choice → IdentityImport with
   * recovery intent; after the identity is imported the wizard lands on
   * NestRecovery (so the `fauna.state.deployment-seeds` map is decryptable). */
  beginRecoverLostBox(): void;
  /** `launch-recover-button` on launch_retry → seed the surviving device's
   * identity and drop straight into NestRecovery. */
  seedIdentityForRecovery(secret: string): void;
  /** Inject the custodied box list (`nest_actor_id` hex) for `nest_recovery`
   * to render as `recover-box-item` rows. */
  setRecoveryBoxes(boxes: string[]): void;
  /** The custodied box list rendered as `recover-box-item` rows. */
  recoveryBoxes(): string[];
  /** Select a custodied box on `nest_recovery` (enables the method buttons). */
  selectRecoveryBox(nestActorId: string): void;
  /** `recover-method-cloud-button` → VpsConfig (recovery mode). Throws if no
   * box is selected (`require_selected_recovery_box`). */
  recoverViaCloud(): void;
  /** `recover-method-selfhosted-button` → RecoverSelfhostedInstructions.
   * Throws if no box is selected. */
  recoverViaSelfhosted(): void;
  /** Whether the wizard is in the recovery branch (gates the entry CTAs). */
  recoveryIntent(): boolean;
  /** Which entry the recovery branch was reached from — `"Launch"` /
   * `"Identity"` / `undefined`. The glue routes `recover-back-button` on this
   * (came-from-launch tears down to launch; came-from-identity → machine
   * `back()`). */
  recoveryCameFrom(): string | undefined;
  /** The selected box's `nest_actor_id` (hex) on `nest_recovery`, or
   * `undefined`. Gates the re-provision method buttons. */
  recoverySelectedNestId(): string | undefined;
}

let _machine: Machine | null = null;
// In-flight `initOnboardingMachine()` promise, cached alongside `_machine`.
// The page's `onMount` and the e2e bridge's `__fauna_callMachineMethod`
// can both call `initOnboardingMachine()` before either resolves — a test
// that fixtures wizard state via `call_machine_method(...)` runs while the
// page is still doing `await ensureWasm()` in onMount, so neither sees a
// cached `_machine` yet. Without the in-flight cache each constructs its
// own wasm `OnboardingMachine`: the bridge mutates one, the page renders
// the other, and the fixtured step never shows. Caching the promise makes
// concurrent callers share the single instance.
let _machineInit: Promise<Machine> | null = null;

const TYPED_JSON_HELPERS = new Set([
  'dnsConfig', 'vpsConfig',
  'visibleDnsFields', 'visibleVpsFields',
  'loginPhase', 'handleCheckSnapshot', 'inviteRequestSnapshot',
  'claimCodeSnapshot', 'natModeSnapshot', 'awaitingManualDnsSnapshot',
  'wizardOutcome', 'identityOrigin', 'dnsStatusTextKey',
  'providerStatus', 'provisioningSnapshot', 'billOfMaterials',
  'vpsContinueBlockedReason', 'provisioningContinueBlockedReason',
]);
const JSON_GETTER_FOR: Record<string, string> = {
  dnsConfig: 'dnsConfigJson',
  vpsConfig: 'vpsConfigJson',
  visibleDnsFields: 'visibleDnsFieldsJson',
  visibleVpsFields: 'visibleVpsFieldsJson',
  loginPhase: 'loginPhaseJson',
  handleCheckSnapshot: 'handleCheckSnapshotJson',
  inviteRequestSnapshot: 'inviteRequestSnapshotJson',
  claimCodeSnapshot: 'claimCodeSnapshotJson',
  natModeSnapshot: 'natModeSnapshotJson',
  awaitingManualDnsSnapshot: 'awaitingManualDnsSnapshotJson',
  wizardOutcome: 'wizardOutcomeJson',
  identityOrigin: 'identityOriginJson',
  dnsStatusTextKey: 'dnsStatusTextKeyJson',
  providerStatus: 'providerStatus',
  provisioningSnapshot: 'provisioningSnapshot',
  billOfMaterials: 'billOfMaterials',
  vpsContinueBlockedReason: 'vpsContinueBlockedReasonJson',
  provisioningContinueBlockedReason: 'provisioningContinueBlockedReasonJson',
};

// `m.step()` on the wasm-bindgen surface returns the Rust `Debug` name
// (`"HandleEntry"`, `"DnsConfig"`, …). Per-app plans for the web app
// compare against snake_case forms (`'handle_entry'`, `'dns_config'`) — the
// idiom Svelte templates are most readable in. Other apps (Swift / Kotlin
// / C#) compare against their generated enum types directly. Translate once
// here so every view matches the plan verbatim.
function camelToSnake(s: string): string {
  return s.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
}

/**
 * Moment 1's commit — the shared `persist_confirmed_identity` over the wasm
 * account registry: per-actor account + secret READ BACK (a store that silently
 * kept nothing is an error, not a success) + activation.
 *
 * Called in BOTH modes. Append ("Add account") mode is outside moment 1 and must
 * stay that way (`long-term-store.md` § Multi-account evolution) — registering
 * and activating a half-account mid-session would move `active` off the live
 * account — but that rule lives in shared Rust now: with `append` the call
 * writes nothing and only derives the actor id. The appended identity stays in
 * the wizard machine until the `LoggedIn` terminal reads it back
 * (`effectiveSecret()`) and registers + switches via accountsAdd +
 * accountsSwitch. tui exempts the same moment via `session::confirm_identity_sink`.
 *
 * Fire-and-forget: the wrapper's surface is synchronous by cross-app contract
 * (the wasm machine has already advanced to HandleEntry), while the registry
 * lives in the core wasm chunk behind an async import. Nothing later in the
 * same run reads the identity back from the store — the terminal and every
 * post-`LoggedIn` hand-off read the machine. Failure surfaces log-only — the
 * cross-app convention for a failed store write (Apple `try?`, Linux
 * `eprintln!`).
 */
function commitConfirmedIdentity(secret: string): void {
  const append = _appendMode;
  void import('$lib/accounts')
    .then((m) => m.accountsPersistConfirmedIdentity(secret, append))
    .catch((e: unknown) => {
      console.warn('[onboarding] persist_confirmed_identity failed:', e);
      logMessage(
        'warn',
        'fauna_web::onboarding',
        `persist_confirmed_identity (registry commit + read-back) failed: ${e}`,
      );
    });
}

/** Append ("Add account") mode. Read only by `commitConfirmedIdentity`, which
 *  hands it to the shared registry call that carries the rule. */
let _appendMode = false;

/** Declare whether the wizard about to be constructed is an append-mode ("Add
 *  account") wizard, exempting its identity-confirm step from the moment-1
 *  registry commit (`long-term-store.md` § Multi-account evolution — "Append
 *  mode is outside moment 1 and must stay that way").
 *
 *  The onboarding page calls this on EVERY mount, with the entry it actually
 *  took — not only on the append branch. This module outlives a client-side
 *  navigation, so a one-sided setter would leave the flag stuck `true` after an
 *  append and silently exempt the NEXT wizard (sign-out → create identity in the
 *  same page load), which needs the commit. One declaration per mount keeps the
 *  flag equal to the entry by construction. */
export function setAppendMode(on: boolean): void {
  _appendMode = on;
}

let _pendingBaseUrls: Record<string, string> | undefined = undefined;

export function setPendingProviderBaseUrls(urls: Record<string, string>): void {
  _pendingBaseUrls = urls;
  // Mirror the `"nest"` entry into web's dial seam — the same mirror
  // `OnboardingMachine::set_provider_base_urls` performs into
  // `fauna_launch_machine::set_nest_dial_override` (machine.rs). Both entry
  // points reach this function (the query param read at module load below, and
  // the `__fauna_setProviderBaseUrls` window hook), so the wizard's provider HTTP
  // and the post-`LoggedIn` store-read dial can never disagree about which nest
  // this run talks to — the "split brain no test could see" `dial.rs` warns of.
  //
  // `$lib/api` seeds the same override from the query param on its own module
  // load, which covers the launch that never mounts this page at all; this call
  // is what covers the window-hook path. Both are idempotent.
  setNestDialOverride(urls.nest);
}

/**
 * Initialize the onboarding machine. Idempotent — repeat calls return the
 * cached singleton. Must be awaited from `onMount` (or a `+page.ts` `load`)
 * before any synchronous `getMachine()` call.
 */
export async function initOnboardingMachine(): Promise<Machine> {
  if (_machine) return _machine;
  if (!_machineInit) {
    _machineInit = createOnboardingMachine(new TickObserver(), _pendingBaseUrls)
      .then((inner) => {
        _machine = wrapMachine(inner as object);
        // web renders the `trust_prompt` interstitial (`onboarding.md` §
        // 3b-ter, built 2026-08-14 after tui led it), so the machine routes
        // the NAT step's exits through the one-tap trust offer. This
        // capability flag is the ONLY thing that makes the step reachable —
        // leaving it undeclared exits straight to `Done`, unchanged.
        _machine.setRendersTrustPrompt(true);
        // web renders the `recovery_kit` offer and the `recovery_entry`
        // restore (`onboarding.md` § 1 Identity; tui led), so a created
        // identity routes through the kit screen. The declaration and the
        // page's two steps land together.
        _machine.setRendersRecoveryKit(true);
        return _machine;
      })
      .catch((e) => {
        // Failed construction: drop the cached promise so a later call
        // (e.g. after the wasm asset becomes reachable) can retry.
        _machineInit = null;
        throw e;
      });
  }
  return _machineInit;
}

/**
 * Return the cached singleton. Throws if `initOnboardingMachine()` hasn't
 * resolved yet — gate UI rendering on a `ready` flag set inside `onMount`.
 */
export function getMachine(): Machine {
  if (!_machine) {
    throw new Error('OnboardingMachine not initialized — await initOnboardingMachine() in onMount first');
  }
  return _machine;
}

/**
 * Reset the wizard to its initial state and drop the cached singleton.
 * Useful in tests and when the user explicitly wants to start over.
 *
 * Also exposed on `window.__fauna_resetOnboardingMachine` (see init below)
 * so the e2e test agent (`tests/e2e-unified/web-bridge/agent.js`) can
 * drop the stale machine reference between tests — SvelteKit's
 * `goto('/app/onboarding')` doesn't reload the module, so the
 * module-level `_machine` cache would otherwise carry a previous test's
 * (now-orphaned) wasm instance into the next test, manifesting as
 * "memory access out of bounds" the next time a method is called.
 */
export function resetMachine(): void {
  // Drop the JS-side reference. We deliberately do NOT call `.free()` on
  // the wasm-bindgen instance — that frees the Rust-side allocation but
  // leaves the wasm linear memory in a state where the next
  // `new OnboardingMachine()` allocation can hit corrupted memory
  // (observed empirically: free + re-construct → "memory access out of
  // bounds"). Letting the wasm-bindgen FinalizationRegistry handle
  // cleanup on JS GC is safer; the leak is bounded (one instance per
  // page load) and doesn't accumulate within a session because
  // SvelteKit's `goto` keeps the module-cached `_machine` reference live
  // as the only reference until we null it here.
  //
  // No sessionStorage cleanup needed — the wizard no longer persists
  // scratchpad state. Identity confirmation writes the secret directly
  // to the account registry.
  //
  // The pending-invite slot IS dropped here. resetMachine is the
  // user-initiated "start over" path (e.g. e2e test reset, or a Reset
  // button); a stale record from a previous attempt would otherwise
  // re-seed the wizard back into InviteRequest on next launch.
  void deletePendingInvite();
  _machine = null;
  _machineInit = null;
  // `_pendingBaseUrls` is deliberately NOT cleared here. It is test-only state
  // (set at module load from `?fauna_e2e_provider_base_urls`, or by the window
  // hook — both compiled out of release builds), and the append branch calls
  // this function on its way to building the append wizard. Clearing it here
  // discarded the override on EVERY append entry, so the reload the web driver's
  // `set_provider_base_urls` performs seeded a map that was thrown away a tick
  // later and the wizard was built with no override at all — which surfaces as a
  // post-switch session that cannot connect, i.e. wearing the costume of a
  // product bug in the switch glue (`long-term-store.md` § Multi-account
  // evolution). In production this value is always undefined, so keeping it is a
  // no-op there.
  // `_appendMode` is likewise NOT cleared here: it is declared once per page
  // mount by the onboarding page (see `setAppendMode`), and the append branch
  // calls this function on its way to building the append wizard — clearing it
  // here would undo the declaration it exists to carry.
}

/**
 * If localStorage holds a pending-invite record, hydrate the wizard via
 * `seedPendingInvite()` so the UI can land on the InviteRequest page on
 * relaunch. Returns true when a record was found and seeded; false
 * otherwise. Caller is responsible for navigating to `/app/onboarding`
 * (typically from the app-launch path in `+layout.svelte`).
 *
 * Idempotent. Initializes the machine if it isn't already — the wizard's
 * `seedPendingInvite()` entry point requires an existing OnboardingMachine.
 */
export async function tryRestorePendingInvite(): Promise<boolean> {
  const rec = await loadPendingInvite();
  if (!rec) return false;
  const m = await initOnboardingMachine();
  // Seed the identity secret BEFORE the pending-invite snapshot. Recheck (and
  // Continue) derive the actor id from the machine's secret — a restored slot
  // is useless without it: recheck would return Error{"no identity"} and the
  // not-found cleanup (persistInviteSlotIfActionable) would never fire, so an
  // orphan slot could never self-heal. seedIdentity sets step=HandleEntry;
  // seedPendingInvite then advances to InviteRequest (order matters). Mirrors
  // the launch-routing Case 3 contract ("seed_identity + seed_pending_invite",
  // see +page.svelte onMount) and apps/fauna-linux Case 3
  // (build_onboarding_window_with_seed + seed_pending_invite). The secret is
  // the registry's active account — the one the pending-invite slot belongs to.
  const { accountsActiveSessionMaterial } = await import('$lib/accounts');
  const secret = accountsActiveSessionMaterial()?.secret_hex;
  if (secret) m.seedIdentity(secret);
  m.seedPendingInvite(rec.nestUrl, rec.handle, rec.requestId, rec.statusJson);
  return true;
}

// The whole block below is the onboarding machine's e2e automation surface —
// compiled only into test builds (testing.md § Test-agent build exclusion; a
// production build folds `__FAUNA_E2E_AUTOMATION__` to false and strips it).
// It stays in this module (rather than `$lib/e2e-automation`) because it must
// run at MODULE LOAD, before `+page.svelte`'s onMount constructs the machine —
// see the base-URL comment below.
if (typeof window !== 'undefined' && __FAUNA_E2E_AUTOMATION__) {
  // Test-only: the e2e harness redirects provider HTTP at the fake_cloud
  // fixture by putting the base-URL map in a `?fauna_e2e_provider_base_urls`
  // query param (see drivers/web.py `set_provider_base_urls`). We read it
  // here, at module load — BEFORE `+page.svelte`'s onMount calls the first
  // `initOnboardingMachine()` — so the machine is constructed WITH the
  // override from the start. That's what makes the page's machine ref and
  // the e2e bridge share one override instance: there is no drop-and-rebuild
  // that would strand the page's `m` on a no-override singleton. Because the
  // param lives in the URL, `location.reload()` (the web driver's
  // `hard_reload`) preserves it across re-mounts. A production load with no
  // param is a no-op.
  const e2eProviderBaseUrls = new URLSearchParams(location.search).get(
    'fauna_e2e_provider_base_urls',
  );
  if (e2eProviderBaseUrls) {
    try {
      setPendingProviderBaseUrls(JSON.parse(e2eProviderBaseUrls) as Record<string, string>);
    } catch (e) {
      console.warn('[onboarding] invalid fauna_e2e_provider_base_urls param:', e);
      logMessage('warn', 'fauna_web::onboarding', `invalid fauna_e2e_provider_base_urls param: ${e}`);
    }
  }

  // Wire a window-global reset hook so the e2e test agent can clear the
  // module-cached machine between tests (see resetMachine doc).
  (window as unknown as { __fauna_resetOnboardingMachine?: () => void }).__fauna_resetOnboardingMachine = resetMachine;

  // Test-only hook: redirect provider HTTP base URLs (handle-check,
  // invite-request, status polling, etc.) at the fake_cloud fixture so
  // e2e tests don't escape to the real internet. The Svelte module
  // caches the map and feeds it to `createOnboardingMachine` on next
  // construction. Tests call this from the Playwright web bridge before
  // driving the wizard.
  (window as unknown as { __fauna_setProviderBaseUrls?: (urls: Record<string, string>) => void })
    .__fauna_setProviderBaseUrls = (urls: Record<string, string>) => {
      setPendingProviderBaseUrls(urls);
      // Drop the cached machine (and any in-flight init) so the next
      // initOnboardingMachine() picks up the URLs. Mirrors the
      // resetMachine() pattern but without the pending-invite deletion
      // (URL changes are test-orchestration, not user "start over").
      _machine = null;
      _machineInit = null;
    };

  // Per the onboarding client target-state design (tracked internally),
  // §"E2E bridge contract": `driver.call_machine_method(name, json_arg)`
  // serializes a wizard method call across the bridge. The Web bridge
  // reaches this hook from the test agent; the hook initializes the
  // machine if it isn't already and dispatches the named method with
  // the JSON-decoded argument. Returns the method's result (sync or
  // Promise) — the test agent serializes whatever it gets back.
  // Cross-app dispatcher names whose camelCased spelling collides with a
  // DIFFERENT production wasm binding — each mapped to the thin `…ForTest`
  // export that routes to the shared dispatcher instead. See the note inside
  // the hook below for why reflecting on the bare name is unsafe for these.
  const DISPATCHER_EXPORTS: Record<string, string> = {
    seed_awaiting_manual_dns: 'seedAwaitingManualDnsForTest',
  };

  (window as unknown as { __fauna_callMachineMethod?: (n: string, a: string) => unknown })
    .__fauna_callMachineMethod = async (name: string, jsonArg: string) => {
      const m = await initOnboardingMachine();
      // The cross-app bridge passes Rust source names (snake_case);
      // the wasm-bindgen surface uses camelCase. Translate so the test
      // helpers' single name spelling reaches the right method.
      //
      // ⚠ FIRST consult the redirect table: reflecting by camelCased name is
      // only safe while no PRODUCTION binding happens to share a dispatcher
      // name with a different signature. Where one does, the reflection finds
      // the production binding and mis-calls it — silently, because
      // wasm-bindgen coerces missing/!string arguments instead of throwing.
      // `seed_awaiting_manual_dns` is exactly that collision: the
      // cross-app contract is one JSON object, the same-named production
      // binding takes four positional strings. Every entry here points at a
      // thin `…ForTest` export that hands the name to the shared dispatcher,
      // the same one native routes through — one name table, not two.
      const camel = DISPATCHER_EXPORTS[name]
        ?? name.replace(/_([a-z])/g, (_m, c: string) => c.toUpperCase());
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const fn = (m as any)[camel] ?? (m as any)[name];
      if (typeof fn !== 'function') {
        // No hand-written export by that name — hand the name to the SHARED
        // dispatcher instead of throwing. Reflection can only see names someone
        // wrote a `#[wasm_bindgen]` export for, so a dispatcher-only name
        // (`set_provision_labels`, `set_provision_image_tag`, …) was unreachable
        // from web while every native app served it off the one shared name
        // table. This is that table's door, not a second implementation of it.
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const dispatch = (m as any).callMachineMethodForTest;
        if (typeof dispatch !== 'function') {
          throw new Error(`OnboardingMachine has no method '${name}' (tried '${camel}')`);
        }
        return await dispatch.call(m, name, jsonArg ?? '');
      }
      // Calling-convention rules:
      //   1. Empty / undefined / null jsonArg → call with no args.
      //   2. Methods whose Rust signature takes a JSON string verbatim
      //      (snapshot setters that internally serde_json::from_str) →
      //      pass the raw string without re-parsing. They live in the
      //      `passRawJson` set below.
      //   3. JSON-parsed array → spread as N positional args (for
      //      multi-arg methods like seedPendingInvite).
      //   4. Otherwise → JSON.parse(arg) and call with the single value.
      if (jsonArg === undefined || jsonArg === null || jsonArg === '') {
        return fn.call(m);
      }
      const passRawJson = new Set([
        'setHandleCheckSnapshotForTest',
        'setInviteRequestSnapshotForTest',
        'setProvisioningSnapshotForTest',
        'setDnsRecordsForTest',
        'setClaimCodeSnapshotForTest',
        'setNatModeSnapshotForTest',
        'setCapturedDnsCredentialForTest',
        'setVpsStateForTest',
        'setDnsAvailabilityForTest',
        // These hand their raw JSON straight to the machine's shared
        // `call_machine_method*` dispatcher, which does the `serde_json::from_str`.
        'setNestIdentityPinForTest',
        'nestIdentityPinForTest',
        'seedAwaitingManualDnsForTest',
      ]);
      if (passRawJson.has(camel)) {
        return fn.call(m, jsonArg);
      }
      let parsed: unknown;
      try {
        parsed = JSON.parse(jsonArg);
      } catch {
        // Caller passed a raw string that isn't JSON. Treat as single
        // string arg.
        return fn.call(m, jsonArg);
      }
      if (Array.isArray(parsed)) {
        return fn.call(m, ...parsed);
      }
      return fn.call(m, parsed);
    };
}

function wrapMachine(inner: object): Machine {
  return new Proxy(inner, {
    get(target, prop, _receiver) {
      // Typed JSON helpers — call the *Json getter, parse, return.
      // `wizardOutcomeJson` returns `string | undefined` (Rust
      // `Option<String>`); other helpers always return a non-empty
      // string. Treat `undefined` / `null` / `""` uniformly as "no value"
      // so callers can `if (!m.wizardOutcome())` to gate.
      if (typeof prop === 'string' && TYPED_JSON_HELPERS.has(prop)) {
        const jsonKey = JSON_GETTER_FOR[prop];
        return () => {
          const raw = (target as Record<string, () => string | null | undefined>)[jsonKey]();
          if (raw === undefined || raw === null || raw === '') return undefined;
          return JSON.parse(raw);
        };
      }
      // setContact: the wasm binding expects a JSON-encoded string;
      // the typed Machine API accepts a ContactInfo object. Wrap once
      // here so callers don't sprinkle JSON.stringify at every call site.
      if (prop === 'setContact') {
        return (contact: ContactInfo) => {
          const fn = (target as Record<string, (s: string) => void>).setContact.bind(target);
          fn(JSON.stringify(contact));
        };
      }
      // dnsProviderIneligibleReason: takes a `providerId` argument, so it
      // doesn't fit the generic zero-arg `TYPED_JSON_HELPERS` path above —
      // call the *Json getter with the argument, then apply the same
      // empty-string-means-"no value" convention.
      if (prop === 'dnsProviderIneligibleReason') {
        return (providerId: string) => {
          const fn = (target as Record<string, (id: string) => string>).dnsProviderIneligibleReasonJson.bind(target);
          const raw = fn(providerId);
          if (raw === undefined || raw === null || raw === '') return undefined;
          return JSON.parse(raw);
        };
      }
      // hostedAuthBegin: async, takes (form, fieldId) — the wasm surface
      // resolves a JSON HostedAuthPrompt string; parse before handing it
      // back so callers get the typed object, same convention as every
      // other *Json getter here.
      if (prop === 'hostedAuthBegin') {
        return async (form: string, fieldId: string) => {
          const fn = (target as Record<string, (f: string, id: string) => Promise<string>>).hostedAuthBegin.bind(target);
          const raw = await fn(form, fieldId);
          return JSON.parse(raw);
        };
      }
      // hostedAuthState: sync, takes (form, fieldId) — same
      // takes-an-argument shape as dnsProviderIneligibleReason above, but
      // always has a value (no undefined case: Idle is the machine's own
      // default).
      if (prop === 'hostedAuthState') {
        return (form: string, fieldId: string) => {
          const fn = (target as Record<string, (f: string, id: string) => string>).hostedAuthStateJson.bind(target);
          return JSON.parse(fn(form, fieldId));
        };
      }
      // hostedAuthWait / hostedAuthCanBegin: same method name on both the
      // typed Machine surface and the wasm binding (no *Json suffix — the
      // former resolves void, the latter returns a plain bool), so they
      // fall through to the generic passthrough below.
      // step() — translate Rust Debug CamelCase to snake_case so views
      // can compare against the substitution-rules spec idiom.
      if (prop === 'step') {
        return () => camelToSnake((target as Record<string, () => string>).step());
      }
      // Identity-confirmation wrappers: call into the wasm machine, then
      // commit the returned secret to the account registry (moment 1). The
      // Rust side returns the 64-hex secret synchronously (the wasm-bindgen
      // surface declares `confirmGeneratedIdentity(): string`). The wrapper
      // preserves that synchronous shape so the surface matches
      // Apple/Windows/Linux; the commit's failure is logged and swallowed
      // (see `commitConfirmedIdentity`) because the wasm machine has already
      // advanced to HandleEntry.
      if (prop === 'confirmGeneratedIdentity') {
        return () => {
          const fn = (target as Record<string, () => string>).confirmGeneratedIdentity.bind(target);
          const secret = fn();
          commitConfirmedIdentity(secret);
          return secret;
        };
      }
      if (prop === 'confirmImportedIdentity') {
        return (secret: string) => {
          const fn = (target as Record<string, (s: string) => string>).confirmImportedIdentity.bind(target);
          const validated = fn(secret);
          commitConfirmedIdentity(validated);
          return validated;
        };
      }
      // submitRecoveryEntry: parse the outcome JSON, and commit a restored
      // seed to the registry exactly as `confirmImportedIdentity` commits an
      // import (moment 1) — a restore IS an import once the seed is back.
      if (prop === 'submitRecoveryEntry') {
        return async (phrase: string) => {
          const fn = (target as Record<string, (p: string) => Promise<string>>).submitRecoveryEntry.bind(target);
          const result = JSON.parse(await fn(phrase));
          if (result.restored) {
            const secret = (target as Record<string, () => string | undefined>).effectiveSecret.call(target);
            if (secret) commitConfirmedIdentity(secret);
          }
          return result;
        };
      }
      // seedIdentity: passthrough — the secret is already in the
      // registry (read by the launch path); the wizard just needs
      // to skip past IdentityChoice straight to HandleEntry.
      if (prop === 'seedIdentity') {
        const fn = (target as Record<string, (s: string) => void>).seedIdentity;
        return fn.bind(target);
      }
      // Everything else passes through to the wasm-bindgen instance. Bind
      // methods to the inner target so wasm-bindgen's `this` (the underlying
      // pointer) resolves correctly.
      const v = Reflect.get(target, prop, target);
      return typeof v === 'function' ? (v as (...args: unknown[]) => unknown).bind(target) : v;
    },
  }) as Machine;
}
