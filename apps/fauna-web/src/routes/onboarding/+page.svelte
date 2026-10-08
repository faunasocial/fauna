<script lang="ts">
  import { goto } from '$app/navigation';
  import { onDestroy, onMount } from 'svelte';
  import { identity } from '$lib/store';
  // The launch-path error classes (`TransientAuthError` / `NestOutdatedError` /
  // `NestIdentityChangedError`) and `forgetNestIdentityPin` are no longer imported
  // here: the shared `LaunchMachine` classifies the launch row and owns the
  // pin-forget seam. They stay exported from `$lib/wasm` for the SPA's own
  // `challengeVerify` path (`lib/store.ts`'s background identity refresh).
  import { ensureWasm, parseIdentityImport, logMessage, recoveryBoxesLocal, recoverSelfhostedCommandLocal, shortNestId, actorIdFromSecret, resolveVerifiedSuccessor, adoptHeldSuccessor } from '$lib/wasm';
  import { base } from '$app/paths';
  import {
    dnsManagementMachineWithCredentials,
    applyServingEnablement,
    recoverSelfhostedCommand,
    deploymentSeeds,
    linkedNestsMachineWithTrust,
    recoveryRegisterDeferredKit,
  } from '$lib/rpc';
  import { qrDisplayFromUri } from '$lib/qr-display';
  import { primeTokenCache, storedNestUrlOrNull } from '$lib/api';
  import { setTabNestUrl } from '$lib/tabPin';
  import {
    initOnboardingMachine,
    resetMachine,
    setAppendMode,
    tryRestorePendingInvite,
    machineTick,
    type Machine,
    type DnsConfigState,
    type VpsConfigState,
    type FieldMeta as MachineFieldMeta,
    type WizardOutcome,
    type ProviderStatus,
    type ContactInfo,
    type ProvisioningSnapshot,
    type StepSnapshot,
    type NodeMode,
    type AwaitingManualDnsSnapshot,
    type BillOfMaterialsItem,
    type CredentialForm,
    type HostedAuthState,
  } from '$lib/onboarding/machine.svelte';
  import {
    savePendingInvite,
    deletePendingInvite,
  } from '$lib/onboarding/pending-invite-store';
  import {
    loadPendingFactoryReset,
    deletePendingFactoryReset,
  } from '$lib/onboarding/pending-factory-reset-store';
  import {
    loadAwaitingDnsJson,
    saveAwaitingDns,
    deleteAwaitingDns,
  } from '$lib/onboarding/awaiting-dns-store';
  import { runLoggedInTerminal } from '$lib/onboarding/logged-in-terminal';
  import {
    createLaunchMachine,
    launchIdentityInputs,
    wizardEntryOf,
    phaseNameOf,
    offlineTransientOf,
    identityChangedOf,
    supersededSuccessorOf,
    accountIndexRefusalOf,
    tokenExpiryOf,
    type LaunchMachine,
    type LaunchSnapshot,
    type AccountIndexRefusal,
  } from '$lib/wasm-launch';
  import {
    accountsActiveSessionMaterial,
    signOutReconciled,
    signOutResidue,
    signOutFinish,
    accountsAdd,
    accountsPersistLoggedIn,
    accountsPersistRestoredPredecessors,
    accountsSwitch,
  } from '$lib/accounts';
  import { PROVIDERS, type ProviderMeta } from '$lib/generated/providers';
  import {
    formatPrice,
    serverTypeLabel,
    serverTypeAllowedForMail,
    provisioningElapsedRaw,
    provisioningStatusGlyph,
    provisioningStepLabelRaw,
    provisioningSubstepLabelRaw,
    inviteRecheckPollMs,
    awaitingDnsPollMs,
    handleTld as handleTldShared,
  } from '$lib/wasm-onboarding';
  import { t } from '$lib/i18n/strings';
  import { resolveLocalized, resolveKey } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  // Step values produced by the OnboardingMachine. Snake-case translated
  // by `machine.svelte.ts:camelToSnake`. The forbidden stages
  // (`nest_select` / `nest_connect` / `nest_login` / `invite_request_pending`)
  // are intentionally absent from this union — the redesigned flow has no such
  // pages; the machine still has those variants but the new flow never produces
  // them — if it ever does the template falls through to nothing rendered
  // (a clear loud-failure signal).
  type Step =
    | 'loading'
    | 'identity_choice'
    | 'identity_created'
    | 'identity_import'
    | 'recovery_kit'
    | 'recovery_entry'
    | 'handle_entry'
    | 'dns_config'
    | 'vps_config'
    | 'dns_post_instructions'
    | 'invite_request'
    | 'claim_code'
    | 'nat_mode_choice'
    | 'trust_prompt'
    | 'nest_provisioning'
    | 'nest_recovery'
    | 'recover_selfhosted_instructions'
    | 'done';

  type IdentityStep = 'identity_choice' | 'identity_created' | 'identity_import';

  let m: Machine | null = $state(null);

  // Step derives from the machine. Until `initOnboardingMachine()`
  // resolves, `m` is null and we render `'loading'`.
  const step: Step = $derived.by(() => {
    void machineTick.value;
    if (m) return m.step() as Step;
    return 'loading';
  });

  // The "Almost ready" (awaiting-manual-DNS) surface renders whenever
  // `wizard_outcome() == AwaitingManualDns` — it is NOT an `OnboardingStep`
  // (onboarding.md § "Almost ready" surface). Deriving it off the outcome (not
  // off a flag we set) is what makes the same-session exit and the relaunch
  // hydration ONE code path, and what makes the surface disappear on its own the
  // moment the claim lands and the machine clears the outcome.
  const awaitingDns: boolean = $derived.by(() => {
    void machineTick.value;
    const outcome = m?.wizardOutcome();
    return !!outcome && 'AwaitingManualDns' in outcome;
  });
  // The pending-invite twin of `awaitingDns`, and keyed the same way — off the
  // derived surface state, so the same-session wait and the relaunch hydration
  // are one code path. Keyed on the STATE (not an outcome) because this journey
  // has no wizard exit: `onboarding.md` § The pending-invite surface.
  const pendingInviteReview: boolean = $derived.by(() => {
    void machineTick.value;
    const state = m?.inviteRequestSnapshot()?.state;
    return typeof state === 'object' && state !== null && 'PendingReview' in state;
  });
  const awaitingDnsSnapshot: AwaitingManualDnsSnapshot | null = $derived.by(() => {
    void machineTick.value;
    return awaitingDns ? (m?.awaitingManualDnsSnapshot() ?? null) : null;
  });
  // The one formatter every app renders AND copies — never re-derived here.
  const awaitingDnsText: string = $derived.by(() => {
    void machineTick.value;
    return awaitingDns ? (m?.awaitingDnsRecordsText() ?? '') : '';
  });
  // A probe or claim is already in flight; a second would race it for no gain.
  const awaitingDnsBusy: boolean = $derived.by(() => {
    const s = awaitingDnsSnapshot?.state;
    return s === 'Checking' || s === 'Claiming';
  });
  // Records-less resumed runs have nothing to copy; disabled, never hidden —
  // ui.yaml scopes this ID to the page's required elements.
  const awaitingDnsCopyEnabled: boolean = $derived.by(() => {
    void machineTick.value;
    return awaitingDns ? (m?.awaitingDnsCopyEnabled() ?? false) : false;
  });

  // Snapshot reads tracked via machineTick. Templates use these `$derived`
  // values instead of inlining `m.currentHandle()` etc., which would bypass
  // the tick dependency and stop re-rendering after machine notifications.
  const handleValue: string = $derived.by(() => {
    void machineTick.value;
    return m?.currentHandle() ?? '';
  });
  // The sign-out residue line (`$lib/accounts`'s `signOutResidue`), painted as
  // the `sign-out-residue` view on the step a sign-out and a signed-out load
  // land on, and only while nobody is signed in: "Add account" opens this same
  // step over a live session, where "Signed out, but …" would be false. It is
  // `$lib/accounts` state, not the wizard's, re-read on every derivation, so a
  // wizard tick cannot wipe it (`account-scoping.md` § Erasure follows scope,
  // the ⚠ *pin the line, not paint it once*).
  const signOutResidueLine: string = $derived(
    step === 'identity_choice' && !$identity ? resolveLocalized($signOutResidue) : '',
  );
  let residueRetrying = $state(false);
  /** Remove Again: the one sweep the sign-out's tail and a load run, over what
   *  the record still names, behind the other-tab probe. Whatever it answers
   *  replaces the line; a clean sweep removes the view. */
  async function retrySignOutResidue() {
    if (residueRetrying) return;
    residueRetrying = true;
    try {
      await signOutFinish('retry');
    } catch (e) {
      console.warn('sign-out residue retry failed; the record is kept:', e);
    } finally {
      residueRetrying = false;
    }
  }
  const errorMessageValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    // Page-local `error` (parse failures caught before they ever reach the
    // machine, e.g. `importIdentity`'s invalid_secret; thrown exceptions from
    // a wasm/RPC call) wins first; the machine's own tracked error covers
    // domain failures that complete without throwing; the defensive
    // launch-failure surface (`surfaceLaunchFailure` / the null-`rec` rows)
    // fills in when neither has one — with `m` null NOTHING else can reach
    // `error-message`, which was the entire silence this closes. The sign-out
    // residue is not here: it has its own `sign-out-residue` view.
    return error || m?.errorMessage() || launchFailureMessage || undefined;
  });
  const domainStatusValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.domainStatus();
  });
  const isLoadingValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.isLoading() ?? false;
  });
  const dnsConfigValue: DnsConfigState | null = $derived.by(() => {
    void machineTick.value;
    return m?.dnsConfig() ?? null;
  });
  const visibleDnsFieldsValue: MachineFieldMeta[] = $derived.by(() => {
    void machineTick.value;
    return m?.visibleDnsFields() ?? [];
  });
  // A `hosted-auth` field's button label source (`onboarding.md` § 4) —
  // per-field id -> { label, enabled }, re-derived every observer tick.
  // Mirrors tui's `hosted_auth_button`; nothing here is re-derived beyond
  // resolving the machine's own state/can-begin getters to display strings.
  const hostedAuthDnsValue: Record<string, { label: string; enabled: boolean }> = $derived.by(() => {
    void machineTick.value;
    const out: Record<string, { label: string; enabled: boolean }> = {};
    for (const field of visibleDnsFieldsValue) {
      if (field.field_type !== 'HostedAuth') continue;
      out[field.id] = {
        label: hostedAuthButtonLabel(m?.hostedAuthState('dns', field.id) ?? 'Idle'),
        enabled: m?.hostedAuthCanBegin('dns', field.id) ?? false,
      };
    }
    return out;
  });
  const dnsStatusTextValue: string = $derived.by(() => {
    void machineTick.value;
    // The machine exposes the status as a LocalizedText (`dnsStatusTextKey`);
    // there is NO plain `dnsStatusText()` method on the wasm surface (calling
    // it throws "not a function" and tears down the whole creds-form render).
    // Resolve the LocalizedText through the shared i18n resolver instead.
    return resolveLocalized(m?.dnsStatusTextKey());
  });
  const canVerifyDnsValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.canVerifyDns() ?? false;
  });
  const canContinueDnsValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.canContinueDns() ?? false;
  });
  const providerStatusValue: ProviderStatus | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.providerStatus();
  });
  /** Narrows providerStatusValue when the registrar will sell — gives
   * the price-display + WHOIS form their gating signal in one read. */
  const buyableProviderStatus: { price_cents: number; currency: string | null } | null = $derived.by(() => {
    const ps = providerStatusValue;
    if (ps && typeof ps === 'object' && 'UnregisteredBuyable' in ps) {
      return ps.UnregisteredBuyable;
    }
    return null;
  });
  /** Contact-form / registrar-notes visibility — the business rules live in
   * the shared machine (`shouldShowContactForm` / `shouldShowRegistrarNotes`,
   * onboarding.md § 4); these tracked reads just re-render on machine ticks.
   * The key→i18n text render stays here as platform shell. */
  const shouldShowContactFormValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.shouldShowContactForm() ?? false;
  });
  const shouldShowRegistrarNotesValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.shouldShowRegistrarNotes() ?? false;
  });
  /** dns-no-provider-message visibility — the shared machine
   * (`shouldShowNoProviderMessage`, onboarding.md § 4) gates it on
   * `buy_domain && handle_check.outcome == DomainAvailable { buyable_via_provider:
   * false }`, computed at the (early) handle-check step. This replaces the old
   * web-only `provider_status() === 'UnregisteredNotBuyable'` predicate, which
   * was both LATE (provider_status is NotReady until a provider is selected) and
   * provider-specific (the message is about the TLD, not the selected provider). */
  const shouldShowNoProviderMessageValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.shouldShowNoProviderMessage() ?? false;
  });
  const contactValue: ContactInfo = $derived.by(() => {
    void machineTick.value;
    return (m?.dnsConfig().contact) ?? {
      first_name: '', last_name: '', email: '', phone: '',
      address1: '', city: '', state: '', postal_code: '', country: '',
    };
  });
  /** TLD derived from currentHandle's domain. Used by the no-provider-
   * carries-tld message ("None of our supported registrars carry .xyz").
   * Empty string when the handle has no real TLD (input still being typed).
   * Single-sourced in shared Rust via `handleTldShared` (priority #2) — the
   * same `fauna_onboarding_machine::handle_tld` the native apps call. */
  const handleTld: string = $derived.by(() => {
    void machineTick.value;
    return handleTldShared(m?.currentHandle() ?? '') ?? '';
  });
  const vpsConfigValue: VpsConfigState | null = $derived.by(() => {
    void machineTick.value;
    return m?.vpsConfig() ?? null;
  });
  const canVerifyVpsValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.canVerifyVps() ?? false;
  });
  // Twin of hostedAuthDnsValue above, keyed the same way, for the VPS
  // credentials form (whose fields come from the generated providers.ts
  // list, not visibleVpsFields() — see the {#each} below).
  const hostedAuthVpsValue: Record<string, { label: string; enabled: boolean }> = $derived.by(() => {
    void machineTick.value;
    const out: Record<string, { label: string; enabled: boolean }> = {};
    for (const p of PROVIDERS) {
      for (const field of p.fields) {
        if (field.type !== 'hosted-auth' || !field.kinds.includes('vps')) continue;
        out[field.id] = {
          label: hostedAuthButtonLabel(m?.hostedAuthState('vps', field.id) ?? 'Idle'),
          enabled: m?.hostedAuthCanBegin('vps', field.id) ?? false,
        };
      }
    }
    return out;
  });
  // vps-config-mail-mode-toggle: resolved bool (user choice, else handle
  // real-domain default). Drives the toggle's checked state and the
  // server-type RAM gate just below it.
  const provisionMailModeEnabledValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.provisionMailModeEnabled() ?? true;
  });
  const canContinueVpsValue: boolean = $derived.by(() => {
    void machineTick.value;
    return m?.canContinueVps() ?? false;
  });
  // The reason `vps-config-continue-button` is disabled, or `''` when it's
  // live (`ui/README.md` § Copy comprehensibility rule 5).
  const vpsContinueBlockedReasonValue: string = $derived.by(() => {
    void machineTick.value;
    return resolveLocalized(m?.vpsContinueBlockedReason());
  });
  const dnsPostInstructionsValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.dnsPostInstructions();
  });
  // Snapshots driving the new handle_entry and invite_request stages.
  // Mirror `fauna_onboarding_machine` snapshot types; see
  // apps/fauna-web/src/lib/onboarding/machine.svelte.ts for the TS shapes.
  const handleCheckSnapshotValue = $derived.by(() => {
    void machineTick.value;
    return m?.handleCheckSnapshot();
  });
  const inviteRequestSnapshotValue = $derived.by(() => {
    void machineTick.value;
    return m?.inviteRequestSnapshot();
  });
  // The guardian a verified out-of-band invite code designates, if any — the
  // `supervised_by` field on `fauna.account.invite_code.verify`'s reply
  // (family-safety.md § Wire & data shape). Surfaced as
  // `invite-code-supervised-notice` BEFORE redemption: supervision is declared at
  // account creation, never sprung on the account afterwards (§ The trust shape,
  // invariant 4). Null for an ordinary code.
  const oobSupervisedBy = $derived.by(() => {
    const oob = inviteRequestSnapshotValue?.out_of_band_code_state;
    return oob && typeof oob === 'object' && 'Valid' in oob ? (oob.Valid.supervised_by ?? null) : null;
  });
  // Snapshot driving the claim_code page (target §3a). Reached only when
  // handle-check returns UnregisteredUnclaimedNest. Mirrors
  // `fauna_onboarding_machine::ClaimCodeSnapshot`.
  const claimCodeSnapshotValue = $derived.by(() => {
    void machineTick.value;
    return m?.claimCodeSnapshot();
  });
  // Snapshot driving the nat_mode_choice page (the terminal admin-path step —
  // target § 3b-bis). Mirrors `fauna_onboarding_machine::NatModeSnapshot`.
  const natModeSnapshotValue = $derived.by(() => {
    void machineTick.value;
    return m?.natModeSnapshot();
  });
  // Snapshot for the four-step nest_provisioning page. Re-read on every
  // observer tick (orchestrator notifies on every step / substep / retry
  // transition). Pre-run / no-run state is `overall: 'Idle'` with all
  // four steps Pending — the snapshot type's `idle()` constructor.
  const provisioningSnapshotValue: ProvisioningSnapshot | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.provisioningSnapshot();
  });
  // The reason `provisioning-continue-button` is disabled, or `''` when it's
  // live — the four `○` step glyphs are a symbol, not a reason
  // (`ui/README.md` § Copy comprehensibility rule 5).
  const provisioningContinueBlockedReasonValue: string = $derived.by(() => {
    void machineTick.value;
    return resolveLocalized(m?.provisioningContinueBlockedReason());
  });
  // Top-region price summary ("Bill of Materials", onboarding.md §6) — up to
  // two items: domain (one-time, only when buying a new domain) then VPS
  // (recurring, always present). Pure recap of prices already shown/agreed
  // earlier in the wizard; no new price source.
  const billOfMaterialsValue: BillOfMaterialsItem[] = $derived.by(() => {
    void machineTick.value;
    return m?.billOfMaterials() ?? [];
  });
  // Box-recovery branch reactive reads (box-recovery.md § Recovery UI (step 4)).
  // The box list holds public `nest_actor_id` hex only — the custodied seed
  // itself never crosses into JS; the re-provision drive resolves it inside
  // Rust (Task C2).
  const recoveryBoxesValue: string[] = $derived.by(() => {
    void machineTick.value;
    return m?.recoveryBoxes() ?? [];
  });
  const recoverySelectedNestIdValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.recoverySelectedNestId();
  });
  const recoveryCameFromValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.recoveryCameFrom();
  });
  // Wall-clock tick for the `provisioning-elapsed` display. The
  // orchestrator only notifies on step boundaries, so without this the
  // elapsed text would freeze between substep transitions. Driven by a
  // 1s setInterval started/stopped from $effect below based on overall
  // status.
  let nowMs = $state<number>(Date.now());

  function vpsProviders(): ProviderMeta[] {
    return PROVIDERS.filter(
      (p) => p.capabilities.includes('vps') && p.curatedOffers.length > 0,
    );
  }

  /** A `hosted-auth` field's button label, from the machine's own
   * `HostedAuthState` — mirrors tui's `hosted_auth_button` match arm.
   * Nothing here is re-derived beyond string lookup. */
  function hostedAuthButtonLabel(state: HostedAuthState): string {
    if (state === 'Idle') return t.provisioning.hosted_auth.connect;
    if (state === 'Connected') return t.provisioning.hosted_auth.connected;
    if ('Pending' in state) {
      return t.provisioning.hosted_auth.pending({ code: state.Pending.user_code });
    }
    return t.provisioning.hosted_auth.failed({ message: state.Failed.message });
  }

  /** Press handler for a `hosted-auth` field's button: begin the
   * device-authorization request, open the verification URL, then await
   * approval. `window.open` is called SYNCHRONOUSLY here (a blank tab,
   * before the await) so the user-gesture chain survives popup blockers in
   * browsers that would otherwise block a post-await `window.open` —
   * navigated to the real URL once `hostedAuthBegin` resolves. If the tab
   * was blocked, fall back to the same best-effort `window.open` the
   * provider signup/open-in-browser buttons already use. */
  async function beginHostedAuth(form: CredentialForm, fieldId: string) {
    error = '';
    const tab = window.open('about:blank', '_blank', 'noopener,noreferrer');
    try {
      const prompt = await m?.hostedAuthBegin(form, fieldId);
      if (prompt) {
        if (tab && !tab.closed) {
          tab.location.href = prompt.verification_url;
        } else {
          window.open(prompt.verification_url, '_blank', 'noopener,noreferrer');
        }
      }
      await m?.hostedAuthWait(form, fieldId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Resolve a dot-notation i18n key / a `LocalizedText` against the generated
  // `t` table — the shared `$lib/i18n/localized` resolver (one helper for every
  // consumer; was duplicated here + admin-dns + value formatting).
  const L = resolveKey;
  const Lookup = resolveLocalized;

  function dnsProviders(): ProviderMeta[] {
    return PROVIDERS.filter((p) => p.capabilities.includes('dns'));
  }

  // The recover-box-item row label — the box's own handle domain when known
  // (box-recovery.md § Trust & audience: DeploymentSeedEntry.domain, resolved
  // from the box's fauna.nest.info at claim), else the short nest_actor_id (a
  // domainless home-relay box, or a list injected without domains in the e2e).
  // Every path that fills `recoveryBoxDomains` awaits `ensureWasm()` first, so
  // the sync `shortNestId` thunk is safe on this render path.
  function recoveryBoxLabel(boxId: string): string {
    return recoveryBoxDomains[boxId] || shortNestId(boxId);
  }

  // WHOIS contact form definition. Field order + IDs match
  // tests/e2e-unified/ui.yaml dns_config.optional_elements and the
  // ContactInfo struct in libs/fauna-provisioning/src/registrar/mod.rs.
  // Each `field` is the snake_case ContactInfo field; `id` is the
  // kebab-case dns-contact-{id}-input testid suffix.
  type ContactField = keyof ContactInfo;
  const contactFieldDefs: Array<{ id: string; field: ContactField; label: string }> = [
    { id: 'first-name',  field: 'first_name',  label: t.onboarding.dns_config.contact_fields.first_name },
    { id: 'last-name',   field: 'last_name',   label: t.onboarding.dns_config.contact_fields.last_name },
    { id: 'email',       field: 'email',       label: t.onboarding.dns_config.contact_fields.email },
    { id: 'phone',       field: 'phone',       label: t.onboarding.dns_config.contact_fields.phone },
    { id: 'address1',    field: 'address1',    label: t.onboarding.dns_config.contact_fields.address1 },
    { id: 'city',        field: 'city',        label: t.onboarding.dns_config.contact_fields.city },
    { id: 'state',       field: 'state',       label: t.onboarding.dns_config.contact_fields.state },
    { id: 'postal-code', field: 'postal_code', label: t.onboarding.dns_config.contact_fields.postal_code },
    { id: 'country',     field: 'country',     label: t.onboarding.dns_config.contact_fields.country },
  ];

  function onContactFieldInput(field: ContactField, value: string): void {
    if (!m) return;
    const next: ContactInfo = { ...contactValue, [field]: value };
    m.setContact(next);
  }

  // DNS-provider button eligibility — the buy_domain→registrar /
  // same_provider_for_vps→vps capability rule lives in the shared machine
  // (`dnsProviderEligible`, onboarding.md § 4) instead of being re-derived per
  // client. `void machineTick.value` makes the per-button `disabled` re-render
  // when the toggles flip; the `capabilities` fallback covers the brief
  // pre-init window where `m` is still null (matches the old behaviour).
  function dnsProviderEnabled(p: ProviderMeta): boolean {
    void machineTick.value;
    return m?.dnsProviderEligible(p.id) ?? p.capabilities.includes('dns');
  }

  // The reason `dns-provider-row[p.id]` is disabled, or `''` when it's
  // selectable / not yet known — a disabled control owes the user a reason
  // (`ui/README.md` § Copy comprehensibility rule 5).
  function dnsProviderIneligibleReasonText(p: ProviderMeta): string {
    void machineTick.value;
    return Lookup(m?.dnsProviderIneligibleReason(p.id));
  }

  let error = $state('');
  // Identity state
  let importHex = $state('');
  let secretKeyCopied = $state(false);
  // recovery_kit / recovery_entry (onboarding.md § 1 Identity). The kit root
  // itself is never held here — it is read off the machine per render.
  let recoveryKitCopied = $state(false);
  let recoveryPhrase = $state('');
  let recoveryAccount = $state('');
  // OOB invite-code input on the invite_request stage. Local-only;
  // the wizard owns the verification result via OobCodeState.
  let oobCode = $state('');
  // Claim-code input on the claim_code stage. Local-only; the wizard
  // owns the submit-result via ClaimCodeState.
  let claimCodeValue = $state('');

  // Pre-fill the claim-code input from the factory-reset re-onboard slot
  // (`navigateToClaimCodeForKnownNestWithCode` stashed the returned code —
  // the human never saw it, so without this they'd be stranded). Only fill
  // an empty input so we never fight a user edit. Mirrors
  // apps/fauna-linux/src/views/onboarding/claim_code.rs. Per onboarding.md §3a.
  $effect(() => {
    void machineTick.value;
    if (step !== 'claim_code' || claimCodeValue !== '') return;
    const prefill = m?.claimCodePrefill();
    if (prefill) claimCodeValue = prefill;
  });

  // Launch-screen silent-challenge state. `'idle'` is the steady-state
  // post-launch (either authenticated and redirected, or wizard ready).
  // `'in_flight'` shows the loading screen while we run the silent
  // challenge handshake. `'transient_error'` shows a retry CTA after a
  // network failure (per target-state §"App-launch routing"); the user
  // can hit Retry or fall through to the wizard at handle_entry.
  // `'needs_update'` is the NON-retry terminal surface shown when the nest
  // authoritatively reports it is outdated (`fauna.nest.outdated`): retrying
  // the same outdated nest is futile, so we show the localized update message
  // (no Retry CTA) and offer only "Use a different nest" — the web sibling of
  // the launch machine's `Offline { transient: false }` (version-compatibility.md
  // Dim 4 / onboarding.md § App-launch routing — version-mismatch row).
  // `'account_index_refusal'`: the saved account index is present and this
  // build cannot use it (`version-compatibility.md` § 5 item 9) — checked
  // BEFORE every other row in `applyLaunchPhase`. Never paired with a retry
  // or fallthrough CTA; the malformed verdict alone reveals a start-over
  // confirm via `accountIndexResetConfirming` (a purely local UI toggle, no
  // machine round trip — mirrors tui's `LaunchSurface::AccountIndexUnreadable`).
  let launchState:
    | 'idle'
    | 'in_flight'
    | 'transient_error'
    | 'needs_update'
    | 'sign_in_refused'
    | 'identity_changed'
    | 'account_index_refusal' = $state('idle');
  let needsUpdateMessage = $state('');
  let accountIndexRefusal: AccountIndexRefusal | null = $state(null);
  let accountIndexResetConfirming = $state(false);
  // The DEFENSIVE launch-failure surface (common.md § Client-state
  // recoverability; testing.md § points 2 + 6). Set when the onMount launch
  // sequence throws, rejects, or never settles (watchdog) — every one of which
  // used to leave `m` null and the bare loader up forever, with the failure
  // visible nowhere (three sessions of phantom root causes, 2026-07-16).
  // Rendered through the page's EXISTING `error-message` element (via the
  // `errorMessageValue` fallback), never a new testid.
  let launchFailureMessage: string | null = $state(null);
  // Fires if the launch sequence neither settles nor throws within the bound —
  // the never-settling class a try/catch cannot see (measured: a dep-level
  // `unwrap_throw` escaping a wasm future's poll kills the task WITHOUT
  // rejecting its JS promise, so `await start()` hangs with no exception).
  // 45s > the machine's own 30s dispatch bound, so it can never fire while the
  // machine is still legitimately probing.
  let launchWatchdog: ReturnType<typeof setTimeout> | null = null;
  const LAUNCH_WATCHDOG_MS = 45_000;

  function disarmLaunchWatchdog() {
    if (launchWatchdog !== null) {
      clearTimeout(launchWatchdog);
      launchWatchdog = null;
    }
  }

  function armLaunchWatchdog() {
    disarmLaunchWatchdog();
    launchWatchdog = setTimeout(() => {
      launchWatchdog = null;
      if (m !== null || appendMode) return; // something rendered — not stuck
      if (launchState !== 'idle' && launchState !== 'in_flight') return; // a launch surface is up
      console.error('[launch] watchdog: launch sequence never settled');
      void surfaceLaunchFailure(new Error(`launch did not settle within ${LAUNCH_WATCHDOG_MS / 1000}s`));
    }, LAUNCH_WATCHDOG_MS);
  }

  /** Convert any launch-sequence death into a visible error + a usable wizard —
   *  no exit may leave the eternal testid-less loader (the InviteRequest
   *  defensive precedent, generalized). */
  async function surfaceLaunchFailure(err: unknown) {
    disarmLaunchWatchdog();
    launchState = 'idle';
    const detail = err instanceof Error ? err.message : String(err);
    launchFailureMessage = `${t.onboarding.launch.launch_failed} (${detail})`;
    if (!m) {
      try {
        m = await initOnboardingMachine();
        const secret = accountsActiveSessionMaterial()?.secret_hex;
        if (secret) m.seedIdentity(secret);
      } catch (e2) {
        // Even the fallback wizard couldn't come up (e.g. the onboarding wasm
        // chunk is unreachable). `step` stays 'loading' and its template branch
        // renders `error-message` off `launchFailureMessage` — still loud.
        console.error('[launch] fallback wizard construction failed too:', e2);
      }
    }
  }
  // The `LaunchMachine` that classified this launch, held so the launch-surface
  // CTAs act on the SAME machine: Retry → `retrySilentChallenge()`, "trust this
  // nest" → `trustNestIdentity()`. Both are no-ops outside the phase they belong
  // to, so a CTA can never drive the machine somewhere the phase doesn't allow
  // (in particular: `IdentityChanged` can never be *retried* into trusting).
  let launchMachine: LaunchMachine | null = null;
  // The launch row's (secret, nest_url) — the wizard fallthrough re-seeds the
  // identity from it, and `Online` primes the bearer cache keyed on it.
  let pendingSilentChallenge: { secret: string; nestUrl: string } | null = $state(null);
  // The custodied boxes readable at launch, gating the surviving-device
  // `launch-recover-button` on launch_retry (box-recovery.md § Recovery UI
  // (step 4)). Populated by a best-effort reachable-nest `deploymentSeeds()`
  // read (`loadRecoverableBoxes`); stays empty when the saved nest is the dead
  // box (a dead saved-nest cannot be read — the goal doc's reachable-nest
  // scoping), so the button hides and the fresh-client `recover-lost-box-button`
  // on identity_choice is the fallback entry.
  let recoverableBoxes = $state<string[]>([]);
  // Public box-domain labels, keyed by `nest_actor_id` — the box's own handle
  // domain (box-recovery.md § Trust & audience), the human `recover-box-item`
  // label; absent/`null` for a domainless home-relay box → the row falls back to
  // the short id. Non-secret (unlike the seed), so it lives in JS alongside the
  // id; filled by the same reachable-nest `deploymentSeeds()` read that populates
  // the box list.
  let recoveryBoxDomains = $state<Record<string, string | null>>({});

  // Append-mode "Add account" (long-term-store.md § Multi-account evolution): the
  // switcher navigates here with `?add=1` to add a SECOND+ identity. We run a
  // fresh wizard at IdentityChoice (never resuming the current identity) and, on
  // success, register + activate the new account rather than overwriting the one
  // session (handleWizardExit append branch). Mirrors linux's `append` window.
  let appendMode = $state(false);

  // The pending-invite journey's own append adoption (`persistInviteSlotIfActionable`)
  // is one-shot per request — re-persisting the slot on every recheck poll must
  // not re-register/re-switch. `null` until this append run has registered +
  // activated its pending identity; then the adopted request's id, so a NEW
  // submit (a different request_id) re-adopts.
  let appendAdoptedRequestId: string | null = null;

  onMount(async () => {
    // Launch routing (App-launch routing) — ONE machine, ONE start(), one
    // phase switch. `LaunchMachine::start()` (machine.rs) is the complete
    // router: it reads every slot through the registry persistence seam and
    // routes to the authoritative phase itself — factory-reset boot-reconcile,
    // then awaiting-manual-dns, then the silent challenge (identity + nest_url),
    // then pending-invite → InviteRequest, then identity-only → HandleEntry,
    // then nothing-stored → IdentityChoice. The page no longer re-derives that
    // order in per-row gates; it seeds the wizard from the slot the machine
    // routed on (`applyLaunchPhase`). Append-mode (`?add=1`) is the ONE case
    // that is not machine-carried — a fresh wizard for a NEW identity — so it
    // stays ahead of `start()`.

    // Everything from here to `applyLaunchPhase()` is one guarded sequence:
    // any throw/rejection surfaces through `surfaceLaunchFailure` (the
    // `error-message` element + a seeded fallback wizard), and the watchdog
    // catches the never-settling class no catch can see. The breadcrumbs are
    // for the e2e bridge's console capture — a wedged launch names its last
    // completed step in the failure text (testing.md § point 6).
    armLaunchWatchdog();
    try {
      console.debug('[launch] ensureWasm');
      await ensureWasm();
      // A sign-out a closed tab left unfinished is finished before the launch
      // machine reads the registry: the account it would otherwise route on is
      // one the user already signed out of (`account-scoping.md` § Erasure
      // follows scope, the web paragraph, decision 2).
      console.debug('[launch] signOutReconciled');
      await signOutReconciled();
      identity.init();

      // Append-mode ("Add account" from the switcher): a fresh wizard for a NEW
      // identity. Bypass every resume case below — those would silent-challenge
      // back into the current identity — and start clean at IdentityChoice. The
      // current identity stays safe in the registry; the new one is registered +
      // activated on success. `resetMachine()` guarantees a clean IdentityChoice
      // (no pending slots exist in the fully-onboarded state add-account launches from).
      // Declare the entry to the machine module BEFORE any wizard is built: an
      // append wizard's identity-confirm step must skip the moment-1 registry
      // commit, which would register a half-account and move `active` to it
      // mid-session (long-term-store.md § Multi-account evolution). Declared on
      // EVERY mount, both branches, so the module-scoped flag cannot outlive an
      // append and exempt the next wizard. The append registers exactly once, at
      // the `LoggedIn` outcome below (accountsAdd + accountsSwitch).
      const isAppendEntry =
        new URLSearchParams(window.location.search).get('add') === '1';
      setAppendMode(isAppendEntry);

      if (isAppendEntry) {
        appendMode = true;
        resetMachine();
        m = await initOnboardingMachine();
        return;
      }

      // The registry's active account — the identity the launch machine routes
      // on (`RegistryLaunchPersistence`), read through the same registry.
      const active = accountsActiveSessionMaterial();
      const secret = active?.secret_hex ?? null;
      const nodeUrlStored = active?.nest_url ?? null;

      // ONE machine, ONE start(). The machine constructs its own persistence over
      // the shared account registry (`fauna-wasm-launch` — `RegistryLaunchPersistence`
      // over `LocalStorageSecretStore`; a bespoke web store is unrepresentable) and
      // boot-reconciles + routes on its own (`machine.rs::start` — factory-reset
      // reconcile → awaiting-manual-dns → silent-challenge → pending-invite →
      // handle-entry → identity-choice), so there is no per-row gate here and no
      // second machine to disagree with it. `applyLaunchPhase` then seeds the
      // wizard from the slot the machine routed on. Per common.md § Client-state
      // recoverability (the factory-reset row is the machine's boot-reconcile,
      // CR-2) + onboarding.md § App-launch routing.
      console.debug('[launch] createLaunchMachine');
      launchMachine = await createLaunchMachine(
        { onChanged() { /* the surfaces render off `launchState` / the wizard, set by applyLaunchPhase */ } },
      );
      // Show the loader while the machine classifies. Stash the launch row so the
      // launch-surface CTAs (Retry / trust-this-nest / Online bearer-prime) act on
      // the SAME machine and the SAME (secret, nest_url) — only the Online/Offline
      // phases read `pendingSilentChallenge`; the WizardAt phases ignore it.
      if (secret) launchState = 'in_flight';
      if (secret && nodeUrlStored) {
        pendingSilentChallenge = { secret, nestUrl: nodeUrlStored };
      }
      // The launch machine's decision INPUTS, reported before it decides. A
      // `WizardAt IdentityChoice` on a session the harness has already signed
      // in is the machine's `(None, _, _)` arm, and three unrelated registry
      // states produce that same `None` — see `launchIdentityInputs`. Without
      // this line a red can only say the machine chose the wizard, never why,
      // which is what cost this seam three wrong causes in a
      // day. Fact-reporting, not cause-assuming: it
      // logs what is in the store, and the authoritative `load_identity()`
      // verdict beside it.
      try {
        console.debug('[launch] identity inputs:', JSON.stringify(await launchIdentityInputs()));
      } catch (e) {
        console.debug('[launch] identity inputs unavailable:', e);
      }
      console.debug('[launch] machine.start()');
      await launchMachine.start();
      console.debug('[launch] applyLaunchPhase');
      await applyLaunchPhase();
      console.debug('[launch] settled');
      disarmLaunchWatchdog();
    } catch (err) {
      console.error('[launch] launch sequence failed:', err);
      await surfaceLaunchFailure(err);
    }
  });

  // An ASYNC onMount cannot register cleanup (Svelte only honors a
  // synchronously-returned function), so the watchdog disarm on unmount —
  // e.g. the Online path's goto('/app/feed') — needs its own onDestroy.
  onDestroy(disarmLaunchWatchdog);

  /**
   * Render the machine's settled phase onto the right surface. Called after the
   * initial `start()` and by both recovery CTAs (`retrySilentChallenge()` /
   * `trustNestIdentity()`), so a re-challenge lands on exactly the surfaces the
   * first attempt did — one mapping, not three.
   *
   * Handles EVERY phase `start()` can settle on: the launch surfaces
   * (Online / IdentityChanged / Offline) that keep `launchMachine` for their
   * CTAs, and the `WizardAt` rows that hand off to the onboarding machine `m`,
   * seeding it from the slot the machine routed on. The silent-challenge
   * classification is shared Rust now (priority #2): the machine's wasm connector
   * runs `run_pinned_silent_challenge` over the SAME `LocalStoragePinStore`
   * (`fauna_nest_pins`) web's own path pinned with, so migrating cannot change
   * what is pinned or what verdict a pin produces. Web keeps only the *rendering*
   * decision — which surface a phase maps to.
   */
  async function applyLaunchPhase() {
    const machine = launchMachine;
    if (!machine) return;
    // The machine settled — a late settle after the watchdog fired supersedes
    // the defensive surface (the machine's routing beats the fallback wizard).
    launchFailureMessage = null;
    const snap = JSON.parse(machine.snapshotJson()) as LaunchSnapshot;
    const phase = phaseNameOf(snap);
    console.debug('[launch] applyLaunchPhase:', phase, wizardEntryOf(snap) ?? '');
    // Read AFTER `start()`: the machine may have just persisted the verify
    // reply (`save_authenticated`) onto the active account's rows.
    const active = accountsActiveSessionMaterial();
    const secret = active?.secret_hex ?? '';
    const nodeUrlStored = active?.nest_url ?? null;
    const handleStored = active?.handle ?? '';

    // The saved account index is present and this build cannot use it
    // (`version-compatibility.md` § 5 item 9). Checked BEFORE every other
    // row — the machine reads `LaunchPersistence::account_index_refusal`
    // before it even attempts `load_identity`, so this outranks
    // `IdentityChanged`, `supersededSuccessorOf`, and the generic `Offline`
    // rows below (`onboarding.md` § App-launch routing). Mirrors tui's
    // `route()` and linux's `main.rs` guard ordering. Never a retry or
    // fallthrough — the nest was never contacted.
    const refusal = accountIndexRefusalOf(snap);
    if (refusal !== null) {
      accountIndexRefusal = refusal;
      accountIndexResetConfirming = false;
      launchState = 'account_index_refusal';
      return;
    }

    // ── Launch surfaces — keep `launchMachine`; the CTAs act on this machine ──
    if (phase === 'Online') {
      // Online is only reachable through a silent challenge, so the launch row
      // (`pendingSilentChallenge`) is set here; it names the (secret, nest_url)
      // the bearer cache is keyed on.
      const pending = pendingSilentChallenge;
      const bearer = machine.currentBearer();
      const expiresAt = tokenExpiryOf(snap);
      if (pending && bearer && expiresAt !== null) {
        // One bearer owner: hand the machine's already-pin-verified token to the
        // SPA's cache instead of letting the app re-mint an unpinned one.
        // The session id goes across with the token: this is the one bearer
        // web does not mint through `getAuthToken`, so it is the one the SPA
        // could otherwise not name (`devices.md` § The client's own session).
        primeTokenCache(
          pending.nestUrl,
          pending.secret,
          bearer,
          expiresAt,
          machine.currentTokenId() ?? '',
        );
      }
      // The machine persisted (nest_url, handle, domain, tier) through the
      // registry adapter's `save_authenticated` on its way to Online (per-actor
      // slots), so the identity is read back from the registry it just wrote —
      // one writer, no second copy of the verify reply to drift from it.
      // (`completeRegistration` re-writes the same cache row and, crucially,
      // hydrates the in-memory store the app renders from — which the
      // persistence seam alone does not touch.)
      identity.completeRegistration(
        handleStored,
        active?.domain ?? '',
        active?.tier ?? 'free',
      );
      goto('/app/feed', { replaceState: true });
      return;
    }

    if (phase === 'IdentityChanged') {
      // (security.md § Transport trust): the nest's pinned
      // deployment identity changed, or it can no longer prove the identity we
      // pinned (the withdrawn/downgrade case, `seenHex === null`). BLOCK
      // auto-entry and warn loudly — never silently proceed to a possibly
      // impersonated nest, and never offer a Retry (a retry cannot change the
      // verdict, and must never silently re-pin). The bearer is already dropped
      // machine-side. Recovery is explicit: trust-this-nest, or a different nest.
      launchState = 'identity_changed';
      return;
    }

    // The identity was SUCCEEDED — the account belongs to someone else's
    // keypair now (`identity-succession.md` § Propagation → *Own device
    // fleet*). Checked BEFORE the generic `Offline` arms below, exactly as
    // tui's `launch.rs` and linux's `main.rs` check it, because the machine
    // deliberately projects this to `Offline { transient: false }`: the
    // successor rides the snapshot side channel, not the phase, so apps which
    // cannot yet render it still stop retrying. Without this arm web fell to
    // the `transient === false` row below and told a succeeded user to update
    // their nest — the wrong surface offering the wrong remedy.
    const claimedSuccessor = supersededSuccessorOf(snap);
    if (claimedSuccessor !== null) {
      await routeSupersededToImport(claimedSuccessor, nodeUrlStored, secret);
      return;
    }

    const transient = offlineTransientOf(snap);
    if (transient === true) {
      launchState = 'transient_error';
      // Light up the surviving-device `launch-recover-button` if this device's
      // own account store or the saved nest custodies ≥1 box — box-recovery.md
      // § Recovery UI (step 4). Fire-and-forget so the retry surface paints now.
      void loadRecoverableBoxes(secret);
      return;
    }
    if (transient === false && snap.sign_in_refused === true) {
      // The saved nest no longer signs this identity in (suspended or removed —
      // deliberately unsaid). Same `Offline { transient: false }` phase as the
      // outdated-nest row below, told apart by the snapshot side channel and
      // checked first; unlike it, the page offers Retry (the admin's restore
      // happens off this device). onboarding.md § App-launch routing.
      launchState = 'sign_in_refused';
      return;
    }
    if (transient === false) {
      // Terminal, NOT retryable — today that is the nest authoritatively
      // reporting it is outdated (`fauna.nest.outdated`). Show the machine's
      // actionable message (no Retry CTA) and offer only "use a different nest";
      // dropping into the wizard would lose it. version-compatibility.md Dim 4.
      needsUpdateMessage = snap.last_error ?? '';
      launchState = 'needs_update';
      return;
    }

    // ── WizardAt rows — hand off to the onboarding machine `m`, seeding it from
    //    the slot the machine routed on. The machine already ran every probe
    //    (factory-reset boot-reconcile, silent-challenge `fauna.setup.status`);
    //    the page just mounts the wizard where it landed and hydrates the record.
    launchState = 'idle';
    const entry = wizardEntryOf(snap);

    if (entry === 'PendingFactoryReset') {
      // The crash-recovery re-claim row (common.md § Client-state recoverability,
      // CR-1/CR-2). The machine boot-reconciled the slot (probe → honor on
      // Unclaimed/Unreachable, clear on Claimed); landing here means it is live.
      // Pre-fill the claim with the code the wiped box minted before the reset.
      const rec = await loadPendingFactoryReset();
      m = await initOnboardingMachine();
      if (secret) m.seedIdentity(secret);
      if (rec) {
        m.navigateToClaimCodeForKnownNestWithCode(rec.nestUrl, rec.handle, rec.claimCode);
      } else {
        // The machine just routed on this slot, so a null read here is itself
        // a defect (a torn/unparseable row, or a reader/machine divergence) —
        // never the eternal loader. Land at the wizard with the failure named
        // (the InviteRequest defensive precedent at the row below).
        console.error('[launch] PendingFactoryReset routed but the slot read returned null');
        launchFailureMessage = `${t.onboarding.launch.launch_failed} (factory-reset slot unreadable)`;
      }
      return;
    }

    if (entry === 'AwaitingManualDns') {
      // Deferred-DNS resume (onboarding.md § "Almost ready" surface). Seed
      // identity FIRST (so the eventual claim can sign), then the slot record —
      // the WHOLE record, VERBATIM, as one JSON string: never rebuild it (serde
      // emits `record_type` while the WASM binding exposes `recordType`, so a
      // hand-rolled round-trip yields an EMPTY list, an "Almost ready" page with
      // nothing to add at the registrar, invisible until the next relaunch),
      // and the machine re-holds the box's built-with identity from it before
      // the first poll. Seeding flips `wizard_outcome()` to AwaitingManualDns,
      // which the poll `$effect` below starts off — one code path with the
      // same-session exit.
      const recJson = await loadAwaitingDnsJson();
      m = await initOnboardingMachine();
      if (secret) m.seedIdentity(secret);
      if (recJson && m.seedAwaitingManualDnsRecordJson(recJson)) {
        // seeded
      } else {
        // Same defensive shape as the PendingFactoryReset row above.
        console.error('[launch] AwaitingManualDns routed but the slot read returned null');
        launchFailureMessage = `${t.onboarding.launch.launch_failed} (awaiting-DNS slot unreadable)`;
      }
      return;
    }

    if (entry === 'InviteRequest') {
      // TWO paths reach an identical `WizardAt{InviteRequest}` snapshot and need
      // DIFFERENT seeding — disambiguate by `nodeUrlStored`, exactly as
      // machine.rs branches (`(identity, Some(url), _)` → the silent-challenge
      // probe; `(identity, None, Some(pending))` → the pending-invite slot):
      if (nodeUrlStored) {
        // Silent challenge found a CLAIMED nest — request a NEW invite for it.
        m = await initOnboardingMachine();
        m.seedIdentity(secret);
        m.navigateToInviteRequestForKnownNest(nodeUrlStored, handleStored);
      } else {
        // A submitted invite is pending. Restore the FULL record (requestId +
        // statusJson) via `seedPendingInvite` so the recheck/continue affordances
        // work — `navigateTo...ForKnownNest` would drop them and the orphan slot
        // could never self-heal (machine.svelte.ts § tryRestorePendingInvite).
        // It does the seedIdentity + seedPendingInvite; then bind the page's `m`.
        const restored = await tryRestorePendingInvite();
        m = await initOnboardingMachine();
        // Defensive: the machine routed here off the same slot the JS store
        // reads, so `restored` is true in practice. If it somehow was not,
        // seed the identity so the wizard lands at handle_entry, not
        // identity_choice (matches the old identity-only fallthrough).
        if (!restored && secret) m.seedIdentity(secret);
      }
      return;
    }

    if (entry === 'ClaimCode') {
      // Silent challenge found an UNCLAIMED nest (no admin to ask for an invite).
      m = await initOnboardingMachine();
      m.seedIdentity(secret);
      m.navigateToClaimCodeForKnownNest(nodeUrlStored ?? '', handleStored);
      return;
    }

    // HandleEntry / IdentityChoice: seeding the identity (or not) already lands
    // the wizard there — no navigation.
    m = await initOnboardingMachine();
    if (secret) m.seedIdentity(secret);
    // The nest hint (onboarding.md § 2 Handle entry → *Nest hint*): a nest's
    // central-origin redirect carries `nest=<its domain>`. Read on this
    // route only, and only here — app-launch routing above has already sent a
    // signed-in session elsewhere, and the add-account entry returns before
    // reaching this row. The raw value goes to the shared machine, which
    // classifies it and pre-fills the handle's domain part or drops it
    // silently; nothing here parses it or connects anywhere.
    const nestHint = new URLSearchParams(window.location.search).get('nest');
    if (nestHint !== null) m.setNestHint(nestHint);
  }

  /** The succeeded-identity landing: the import screen, explained.
   *
   *  The affordance IS the existing import flow (`paste-secret-field`) — no
   *  launch surface of its own was minted for this state, on any app. What
   *  distinguishes it from a user who *chose* to import is the explanation on
   *  that page's own `error-message`, which is why step and reason are set
   *  together through the shared `beginImportIdentityWithReason` transition
   *  rather than by writing to a page-local slot: `errorMessageValue` re-derives
   *  from `errorMessage()` on every observer tick, so a slot write would be
   *  erased by the tick this very transition fires.
   *
   *  `claimed` is deliberately NOT shown. It is whatever the refusal said, and
   *  the nest is enforcer and distributor, never authorizer — it reaches the
   *  console (an admin debugging a fleet wants it) and nothing else until the
   *  chain proves a successor of its own accord. */
  async function routeSupersededToImport(
    claimed: string,
    nestUrl: string | null,
    secret: string,
  ) {
    console.debug('[launch] superseded refusal; claimed successor', claimed);
    launchState = 'idle';
    m = await initOnboardingMachine();
    // NOT `seedIdentity(secret)`: the stored secret is the identity that was
    // just refused, and seeding it walks the wizard past the very screen the
    // user was sent to. The remedy is a DIFFERENT seed, typed in below.
    m.beginImportIdentityWithReason(t.onboarding.launch.identity_superseded);
    void upgradeToVerifiedSuccessor(nestUrl, secret);
  }

  /** Best-effort: verify the succession against the registration chain, so the
   *  screen can name the successor as *fact* rather than as the nest's claim.
   *
   *  Anonymous, and not a shortcut: the refused identity cannot authenticate —
   *  that is what the refusal means — and it works only because
   *  `succession.lookup` and `registration.chain` are pre-identity kinds. The
   *  claimed successor is never passed into the walk; the chain's verdict is
   *  the only one that counts (`identity-succession.md` § Propagation → *Own
   *  device fleet*).
   *
   *  Guarded on still being on the import step when it lands: this is a round
   *  trip that can resolve after the user typed their seed and moved on, and
   *  re-asserting a supersession over whatever they are now doing would be a
   *  mystery banner from a flow they already dealt with (tui's own guard, same
   *  reason). Every failure — unreachable nest, empty lookup, broken chain —
   *  leaves the claim-free message standing, which is the correct fallback. */
  async function upgradeToVerifiedSuccessor(nestUrl: string | null, secret: string) {
    if (!nestUrl || !secret) return; // No stored nest/identity ⇒ nothing to walk.
    try {
      await ensureWasm();
      const predecessor = actorIdFromSecret(secret);
      const successor = await resolveVerifiedSuccessor(nestUrl, predecessor);
      if (!successor) return;
      if (m && m.step() === 'identity_import') {
        // Save in the one case with nothing left to import: this browser
        // already holds the PROVEN successor's key — the state a lost
        // succession reply leaves behind, whose message promised that reopening
        // the app signs in as it. The screen is then not upgraded but
        // replaced: activate the successor and relaunch into it, the switch
        // `performSwitch` makes (a hard reload — a soft `goto` would not
        // rebuild the wasm session). tui's `App::adopt_held_successor` twin.
        if (await adoptHeldSuccessor(predecessor, successor)) {
          console.info('[launch] adopting the held verified successor', successor);
          await accountsSwitch(successor);
          window.location.assign(`${base}/feed`);
          return;
        }
        m.beginImportIdentityWithReason(
          t.onboarding.launch.identity_superseded_verified({ successor }),
        );
      }
    } catch (e) {
      console.warn('[launch] could not verify the succession', e);
    }
  }

  // --- "Almost ready" (awaiting-manual-DNS) polling ---
  //
  // The machine's `recheck_manual_dns()` is deliberately SINGLE-SHOT (mirroring
  // `recheck_invite_status`): the client owns the cadence so native and wasm
  // behave identically and no interval runs inside the machine.
  //
  // The interval itself comes from shared Rust — this was a hand-copied 10_000
  // until 2026-08-12, one of the copies `onboarding.md` § The pending-invite
  // surface names ("never seven hand-copied numbers").
  //
  // Read INSIDE the effect, not at component setup: `awaitingDnsPollMs()`
  // calls the onboarding wasm module directly (no `initOnboardingMachine()`
  // gate), and this effect only ever runs once `awaitingDns` is true — which
  // requires the machine to already be live. Reading it eagerly at setup
  // races `ensureOnboardingWasm()` and throws "Onboarding WASM not
  // initialized" on every cold load, before onMount's `await
  // initOnboardingMachine()` ever gets a chance to run (found 2026-08-14
  // chasing an unrelated e2e failure — this crashed the WHOLE page, every
  // load, since the 2026-08-12 commit that introduced it).
  let awaitingDnsTimer: ReturnType<typeof setInterval> | null = null;

  // Start/stop the timer purely from the derived surface state, so the
  // same-session exit and the relaunch hydration need no separate wiring, and
  // the timer can't outlive the surface.
  $effect(() => {
    if (awaitingDns && awaitingDnsTimer === null) {
      awaitingDnsTimer = setInterval(() => { void recheckAwaitingDns(); }, awaitingDnsPollMs());
    } else if (!awaitingDns && awaitingDnsTimer !== null) {
      clearInterval(awaitingDnsTimer);
      awaitingDnsTimer = null;
    }
    return () => {
      if (awaitingDnsTimer !== null) {
        clearInterval(awaitingDnsTimer);
        awaitingDnsTimer = null;
      }
    };
  });

  // --- pending-invite polling ---
  //
  // The structural twin of the awaiting-DNS timer above. Approval reaches an
  // *unregistered* actor through no push channel — every notification plane is
  // keyed on a bearer-proven actor_id the requester does not have yet — so the
  // client asks (`onboarding.md` § The pending-invite surface: "Poll is the
  // channel — structurally, not provisionally").
  // Same deferred-read reasoning as `awaitingDnsPollMs()` above — read inside
  // the effect, not at component setup.
  let pendingInviteTimer: ReturnType<typeof setInterval> | null = null;

  $effect(() => {
    if (pendingInviteReview && pendingInviteTimer === null) {
      // First poll fires IMMEDIATELY, which is what makes the relaunch case
      // (a hydrated PendingReview) resolve without a 30s stare at a stale page.
      void onPollInvite();
      pendingInviteTimer = setInterval(() => { void onPollInvite(); }, inviteRecheckPollMs());
    } else if (!pendingInviteReview && pendingInviteTimer !== null) {
      clearInterval(pendingInviteTimer);
      pendingInviteTimer = null;
    }
    return () => {
      if (pendingInviteTimer !== null) {
        clearInterval(pendingInviteTimer);
        pendingInviteTimer = null;
      }
    };
  });

  /** One single-shot poll. Unlike the recheck BUTTON this must also route the
   *  approval: `recheck_invite_status` resolves an admission into `LoggedIn` by
   *  itself, and nothing else would carry the user into the app. */
  async function onPollInvite() {
    if (!m) return;
    try {
      const nextStep = (await m.recheckInviteStatus()) as string;
      await persistInviteSlotIfActionable();
      // Approved: the probe confirmed admission and the machine already routed
      // to LoggedIn. `handleWizardExit` is the one exit path — it spends the
      // pending-invite slot and handles append mode — so route through it
      // rather than duplicating either. A no-op on any non-Done step, which is
      // every still-waiting poll.
      handleWizardExit(nextStep);
    } catch (e) {
      // A poll that cannot reach the nest must NOT wedge the loop or paint an
      // error over a page that is simply waiting; the next tick retries.
      logMessage('warn', 'fauna_web::onboarding', `pending-invite poll failed: ${e}`);
    }
  }

  /** One single-shot probe of the provisioned nest. Also the
   *  `awaiting-dns-recheck-button` handler — the button just skips the wait.
   *
   *  `recheck_manual_dns()` swallows an unreachable nest to a still-waiting
   *  `Pending` snapshot inside the machine (never rejects on that path) — but
   *  the try/catch mirrors `onPollInvite`'s twin anyway, as defense-in-depth
   *  against a genuinely abnormal wasm/JS throw: without it, such a throw
   *  would surface as an unhandled promise rejection every poll interval for
   *  as long as the user sits on this page. */
  async function recheckAwaitingDns() {
    if (!m || awaitingDnsBusy) return;
    try {
      const nextStep = (await m.recheckManualDns()) as string;

      // `Done` is ALSO the still-waiting state — `recheck_manual_dns()` returns
      // Done while the nest is still unreachable, exactly as the deferred-DNS exit
      // does. A client that treats every Done as "wizard finished" drops the user
      // into an app with NO NEST (android shipped precisely this bug). So route on
      // the OUTCOME, never on the step alone.
      const outcome = m.wizardOutcome();
      if (outcome && 'AwaitingManualDns' in outcome) return; // still waiting

      // The claim landed — the machine now sits on `NatModeChoice`, or on the
      // trust offer (the already-claimed edge). The slot is deliberately NOT
      // cleared here: its one clearing moment is `LoggedIn`, inside the shared
      // `persist_logged_in` (onboarding.md § Long-term store contract, ratified
      // 2026-09-21). Clearing at the claim — what this line did until then —
      // would send a relaunch after a force-quit on the NAT page or on the offer
      // straight into the app with the offer lost; leaving the slot lets it come
      // back into "Almost ready", whose first poll takes the already-claimed
      // resume and asks once more (§ 3b-ter).
      handleWizardExit(nextStep);
    } catch (e) {
      // A poll that cannot reach the nest must NOT wedge the loop or paint an
      // error over a page that is simply waiting; the next tick retries.
      logMessage('warn', 'fauna_web::onboarding', `awaiting-DNS poll failed: ${e}`);
    }
  }

  /** Retry CTA on the transient-error launch screen (`launch-retry-button`).
   *  Re-runs the silent challenge on the SAME machine — which no-ops unless the
   *  phase really is `Offline{transient:true}`, so the terminal outdated-nest and
   *  identity-changed surfaces cannot be retried into even if a CTA leaked. */
  async function retrySilentChallenge() {
    if (!launchMachine) return;
    launchState = 'in_flight';
    await launchMachine.retrySilentChallenge();
    await applyLaunchPhase();
  }

  /** "Trust this nest" CTA on the nest-identity-changed warning
   *  (`nest-identity-changed-trust-button`): forget the pin (the browser analogue
   *  of `ssh-keygen -R host`) and re-challenge, which re-establishes trust on
   *  first use — re-pinning whatever identity the nest now proves, or proceeding
   *  unpinned if it presents no binding — and lands the user in the app.
   *
   *  Both halves are the machine's `trust_nest_identity()` now, so the forget hits
   *  the same `LocalStoragePinStore` the check consulted; the pin can never be
   *  forgotten by any other path (no silent re-pin, ever). */
  async function trustChangedNestIdentity() {
    if (!launchMachine) return;
    launchState = 'in_flight';
    await launchMachine.trustNestIdentity();
    await applyLaunchPhase();
  }

  /** `account-index-reset-button` — "Start over on this device", shown ONLY
   *  for the malformed verdict. Reveals `account-index-reset-confirm-button`;
   *  performs nothing itself — a purely local reveal, no machine round trip
   *  (mirrors tui's `LaunchAction::StartOver`/`reveal_start_over`). Guarded
   *  on the verdict so a stray call is inert. */
  function revealAccountIndexReset() {
    if (accountIndexRefusal?.kind !== 'Malformed') return;
    accountIndexResetConfirming = true;
  }

  /** `account-index-reset-confirm-button` — the documented floor
   *  (`long-term-store.md` § Cleanup contract): erase the whole credential
   *  namespace and land on fresh onboarding. `identity.logout()` is the same
   *  erase "Sign Out" runs (`accountsClearAll()` + the local-storage
   *  identity keys + the token cache) — the one shared implementation, not a
   *  second "clear everything" button. Guarded on the confirm actually being
   *  shown, so a stray call can't skip the residual-stating step. */
  async function confirmAccountIndexReset() {
    if (accountIndexRefusal?.kind !== 'Malformed' || !accountIndexResetConfirming) return;
    launchMachine = null;
    launchState = 'idle';
    await identity.logout();
    m = await initOnboardingMachine();
  }

  /** "Use a different nest" CTA (`launch-fallthrough-button`) — on every launch
   *  surface: transient retry, the terminal outdated nest, and the identity
   *  warning. Seeds the identity and drops to the wizard at handle_entry. */
  async function fallThroughToHandleEntry() {
    const pending = pendingSilentChallenge;
    pendingSilentChallenge = null;
    launchMachine = null;
    launchState = 'idle';
    m = await initOnboardingMachine();
    if (pending) m.seedIdentity(pending.secret);
  }

  // ── Box-recovery entries + branch handlers (box-recovery.md § Recovery UI
  //    (step 4)). The wizard branch is shared Rust (fauna-onboarding-machine);
  //    these handlers are the thin web glue. ──

  /** The pre-login custody read every recovery surface here goes through
   * (box-recovery.md § The plane-era recovery floor, (b) The reads): this
   * device's own account store JOINED with a cold read from the stored nest
   * whenever a nest client exists (`deploymentSeeds()`), the local read alone
   * otherwise (`recoveryBoxesLocal()`). Never either-or on whether a nest URL
   * is stored — in the case recovery exists for, the stored nest is the dead
   * box, so a nest that does not answer falls back to the local read. Gated on
   * a stored nest URL (`storedNestUrlOrNull`) so a fresh client never hangs
   * the connect against the SPA-origin fallback. Rejects only when every
   * source failed. */
  async function resolveRecoveryBoxes(secretHex: string) {
    await ensureWasm();
    if (storedNestUrlOrNull()) {
      try {
        return await deploymentSeeds(secretHex);
      } catch (e) {
        logMessage(
          'info',
          'fauna_web::onboarding',
          `stored nest did not answer the custody read — local read only: ${e}`,
        );
      }
    }
    return await recoveryBoxesLocal(secretHex);
  }

  /** Best-effort read of the custodied box list at launch, to gate the
   * surviving-device `launch-recover-button` (box-recovery.md § Recovery UI
   * (step 4)): the joined read of `resolveRecoveryBoxes`, so a dead saved box
   * still leaves this device's own custody visible. An empty read leaves
   * `recoverableBoxes` empty and the button hides (the fresh-client
   * `recover-lost-box-button` is then the entry). Never throws — the launch
   * surface must paint regardless of the read. */
  async function loadRecoverableBoxes(secret: string) {
    try {
      const boxes = await resolveRecoveryBoxes(secret);
      recoverableBoxes = boxes.map((b) => b.nestActorId);
      recoveryBoxDomains = Object.fromEntries(boxes.map((b) => [b.nestActorId, b.domain]));
    } catch (e) {
      // Every custody source failed — leave `recoverableBoxes` empty so the
      // surviving-device button stays hidden.
      logMessage('warn', 'fauna_web::onboarding', `recoverable box list not resolved: ${e}`);
    }
  }

  /** `recover-lost-box-button` on identity_choice (fresh-client recovery
   * entry). Routes through identity_import with recovery intent so the
   * identity seed the custody reads need is in hand; the existing `importIdentity` handler's
   * `confirmImportedIdentity` then lands the wizard on nest_recovery. */
  function recoverLostBox() {
    error = '';
    importHex = '';
    m?.beginRecoverLostBox();
  }

  /** `launch-recover-button` on the launch-retry surface (surviving-device
   * recovery entry). Seeds the identity for recovery and drops straight into
   * nest_recovery, pushing the boxes the launch-time `deploymentSeeds()` read
   * found (Task C2 populates `recoverableBoxes`). Mirrors
   * `fallThroughToHandleEntry` but for the recovery branch. */
  async function recoverFromLaunch() {
    const pending = pendingSilentChallenge;
    pendingSilentChallenge = null;
    launchState = 'idle';
    m = await initOnboardingMachine();
    if (pending) m.seedIdentityForRecovery(pending.secret);
    if (recoverableBoxes.length) m?.setRecoveryBoxes(recoverableBoxes);
  }

  /** `recover-back-button` on nest_recovery. Came-from-launch is a wizard EXIT
   * owned by the client glue (the machine stays put), so tear back down to the
   * launch surface; came-from-identity → the machine's `back()`
   * (NestRecovery → IdentityImport). */
  function recoveryBack() {
    error = '';
    if (recoveryCameFromValue === 'Launch') {
      // Came-from-launch is a wizard EXIT (the machine stays put on
      // NestRecovery — C1). Re-enter the onboarding route so the launch flow
      // re-runs; the identity stays in the registry. (This path only
      // becomes reachable once Task C2 populates `recoverableBoxes` at launch;
      // C2 refines the launch re-entry.)
      m?.reset();
      m = null;
      launchState = 'idle';
      goto('/app/onboarding', { replaceState: true });
      return;
    }
    m?.back();
  }

  /** A box row on nest_recovery — selects it, enabling the method buttons. */
  function selectRecoveryBox(nestActorId: string) {
    error = '';
    m?.selectRecoveryBox(nestActorId);
  }

  /** `recover-method-cloud-button` — advance to vps_config in recovery mode
   * (the orchestrator re-provisions with the custodied seed — Task C2). Throws
   * if no box is selected; the button is disabled until one is, so this is
   * defensive. */
  function recoverViaCloud() {
    error = '';
    try {
      m?.recoverViaCloud();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  /** `recover-method-selfhosted-button` — advance to the self-hosted installer
   * instructions. */
  function recoverViaSelfhosted() {
    error = '';
    try {
      m?.recoverViaSelfhosted();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Populate the box list on entering nest_recovery (box-recovery.md § Recovery
  // UI (step 4)) through the one joined read (`resolveRecoveryBoxes`): this
  // device's own account store joined with a cold read from the stored nest
  // whenever a nest client exists, the local read alone otherwise (box-recovery.md
  // § The plane-era recovery floor, (b)). The ids are pushed into the shared
  // machine so `recover-box-item` rows render every box the admin custodies (with
  // the box's domain as the row label). Only overwrite the machine list when the
  // read returns ≥1 box, so an empty/failed read never clobbers an already-shown
  // list (the injected e2e list via `setRecoveryBoxes`, or the instant
  // launch-time push).
  $effect(() => {
    if (step !== 'nest_recovery') return;
    // The identity this recovery runs as is the wizard's own (seeded from the
    // launch row, or just imported) — read from the MACHINE, never the store.
    const secretHex = m?.effectiveSecret();
    if (!secretHex) return;
    let cancelled = false;
    void (async () => {
      try {
        const boxes = await resolveRecoveryBoxes(secretHex);
        if (cancelled || boxes.length === 0) return;
        recoveryBoxDomains = Object.fromEntries(boxes.map((b) => [b.nestActorId, b.domain]));
        m?.setRecoveryBoxes(boxes.map((b) => b.nestActorId));
      } catch (e) {
        // Every custody source failed — leave the current list (injected,
        // launch-pushed, or empty → recover-box-empty-message).
        console.warn('[onboarding] recover box list fetch failed:', e);
        logMessage('warn', 'fauna_web::onboarding', `recover box list not resolved: ${e}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // The real `recover-selfhosted-command` — the `FAUNA_DEPLOYMENT_SEED=<64-hex>`
  // line for the selected box, rendered by the shared getter (box-recovery.md
  // § Recovery UI (step 4)). `null` until the read resolves; the display falls
  // back to the pending placeholder (also what shows when no custody source
  // holds that box).
  let recoverSelfhostedCommandValue = $state<string | null>(null);
  const recoverSelfhostedCommandDisplay: string = $derived(
    recoverSelfhostedCommandValue ?? t.onboarding.recovery.selfhosted_command_pending,
  );

  // Fetch the self-hosted installer command on entering the instructions page
  // with a box selected. The getter resolves the box's custodied seed IN RUST
  // (the seed IS surfaced here by design — the installer input) through the
  // same joined read as the box list (box-recovery.md § The plane-era recovery
  // floor, (b)): this device's own account store joined with a cold read from
  // the stored nest when a nest client exists (`recoverSelfhostedCommand`),
  // the local read alone otherwise or when that nest does not answer
  // (`recoverSelfhostedCommandLocal`). Without a stored nest URL, `nodeUrl()`
  // falls back to the SPA origin (not a nest), so the nest leg is gated on
  // one. Fire-and-forget so the placeholder paints immediately and swaps to
  // the real command when the read resolves; a reject leaves the placeholder
  // (never a blank command).
  $effect(() => {
    if (step !== 'recover_selfhosted_instructions') {
      recoverSelfhostedCommandValue = null;
      return;
    }
    const nestActorId = recoverySelectedNestIdValue;
    if (!nestActorId) return;
    const secretHex = m?.effectiveSecret();
    if (!secretHex) return;
    const nodeUrlStored = !!storedNestUrlOrNull();
    let cancelled = false;
    void (async () => {
      try {
        await ensureWasm();
        let cmd: string | null = null;
        if (nodeUrlStored) {
          try {
            cmd = await recoverSelfhostedCommand(secretHex, nestActorId);
          } catch (e) {
            logMessage(
              'info',
              'fauna_web::onboarding',
              `stored nest did not answer the custody read — local read only: ${e}`,
            );
          }
        }
        cmd ??= await recoverSelfhostedCommandLocal(secretHex, nestActorId);
        if (!cancelled) recoverSelfhostedCommandValue = cmd;
      } catch (e) {
        // No custody source holds that box — keep the placeholder rather
        // than blanking the command.
        console.warn('[onboarding] recover self-hosted command fetch failed:', e);
        logMessage(
          'warn',
          'fauna_web::onboarding',
          `recover self-hosted command not resolved: ${e}`,
        );
      }
    })();
    return () => { cancelled = true; };
  });

  /** `recover-selfhosted-copy-button` — copy the resolved installer command
   * (the real `FAUNA_DEPLOYMENT_SEED=…` line once the getter resolves, else the
   * pending placeholder). */
  let recoverSelfhostedCopied = $state(false);
  async function copyRecoverSelfhostedCommand() {
    await navigator.clipboard.writeText(recoverSelfhostedCommandDisplay);
    recoverSelfhostedCopied = true;
    setTimeout(() => { recoverSelfhostedCopied = false; }, 2000);
  }

  /** `recover-selfhosted-continue-button` — exits the wizard; the box
   * reconnects once the admin has run the installer and it is reachable. */
  function recoverySelfhostedContinue() {
    m?.reset();
    m = null;
    // Exit to the app; the normal launch flow reconnects once the rebuilt box
    // is reachable (same landing as a LoggedIn wizard exit).
    goto('/app/feed', { replaceState: true });
  }

  /** `recover-restore-cta` — deep-link to the Backups page's restore section
   * (the existing backups `restore-*` flow; box-recovery.md § Recovery UI
   * reuses it — do not rebuild). */
  function recoveryRestore() {
    goto('/app/backups');
  }

  // --- Identity Choice / Created / Import ---
  //
  // Generation, persistence-of-origin, and step transitions live in the
  // OnboardingMachine. The view's handlers are thin: set the in-memory
  // `identity` store (no persistence — `identity.login` writes nothing), then
  // invoke the machine method, whose wrapper commits moment 1 to the registry.

  function createIdentity() {
    error = '';
    m?.beginCreateIdentity();
  }

  function goToImport() {
    error = '';
    importHex = '';
    m?.beginImportIdentity();
  }

  async function copySecretKey() {
    const secret = m?.generatedSecret() ?? '';
    if (!secret) return;
    await navigator.clipboard.writeText(secret);
    secretKeyCopied = true;
    setTimeout(() => { secretKeyCopied = false; }, 2000);
  }

  function identityContinue() {
    if (!m) return;
    const secret = m.generatedSecret();
    if (!secret) {
      error = t.onboarding.identity_created.not_generated;
      return;
    }
    identity.login(secret);
    try {
      // The wrapper commits moment 1 to the account registry (writes nothing
      // in append mode — the rule lives in shared Rust) and advances the wizard.
      m.confirmGeneratedIdentity();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function backToIdentityChoice() {
    error = '';
    m?.back();
  }

  // --- recovery_kit (onboarding.md § 1 Identity) ---
  //
  // The screen mints and displays only; registration + escrow run at the
  // signed-in handoff (`captureLoggedInHandoffs`). The display is the bare
  // hex, the QR and the copy button carry the machine's one `fauna://recovery`
  // URI (identity-succession.md § The RecoveryKey).

  const recoveryKitSecretValue: string | undefined = $derived.by(() => {
    void machineTick.value;
    return m?.recoveryKitSecretHex();
  });
  const recoveryKitQr = $derived.by(() => {
    void machineTick.value;
    const uri = m?.recoveryKitUri();
    return uri ? qrDisplayFromUri(uri) : null;
  });

  async function copyRecoveryKit() {
    const uri = m?.recoveryKitUri();
    if (!uri) return;
    await navigator.clipboard.writeText(uri);
    recoveryKitCopied = true;
    setTimeout(() => { recoveryKitCopied = false; }, 2000);
  }

  function confirmRecoveryKit() {
    error = '';
    m?.confirmRecoveryKit();
  }

  function skipRecoveryKit() {
    error = '';
    m?.skipRecoveryKit();
  }

  // --- recovery_entry (onboarding.md § 1 Identity) ---

  function restoreFromRecoveryKit() {
    error = '';
    recoveryPhrase = '';
    recoveryAccount = '';
    m?.beginRecoveryEntry();
  }

  async function submitRecoveryEntry() {
    if (!m) return;
    error = '';
    // The typed account rides on the machine's one account field — the one
    // `handle_entry` asks for next. ALWAYS forwarded, empty included: what the
    // field shows is what is sent, never a handle an earlier flow left behind.
    m.setCurrentHandle(recoveryAccount.trim());
    try {
      const result = await m.submitRecoveryEntry(recoveryPhrase);
      if (result.superseded) {
        // Uniform with the launch flow's refusal: the import screen, carrying why.
        m.beginImportIdentityWithReason(t.onboarding.recovery_entry.superseded);
        return;
      }
      if (result.restored) {
        // The proxy committed the seed (moment 1); the in-memory identity is
        // what the rest of the wizard signs in with, exactly as an import.
        const secret = m.effectiveSecret();
        if (secret) identity.login(secret);
      }
      // The shared outcome text — it can ride a restore that succeeded (a
      // predecessor section that did not open), which is why it is written
      // after the step already moved on.
      if (result.message) error = resolveLocalized(result.message) ?? '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Import-field parsing (bare 64-hex secret | `fauna://identity?secret=&handle=` query
  // form | iOS colon form) lives in shared Rust (`fauna_core::identity_qr`, over wasm) so
  // web, iOS, and android parse identical input instead of each hand-rolling it. See
  // `$lib/wasm` `parseIdentityImport`.

  async function importIdentity() {
    error = '';
    await ensureWasm();
    const parsed = parseIdentityImport(importHex);
    if (!parsed) {
      error = t.onboarding.identity_import.invalid_secret;
      return;
    }
    identity.login(parsed.secret);
    // Per target-state §1: when the QR/URI payload includes a handle,
    // pre-fill the input on step 2 via setCurrentHandle.
    if (parsed.handle && m) {
      m.setCurrentHandle(parsed.handle);
    }
    try {
      m?.confirmImportedIdentity(parsed.secret);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // --- Continue / Redeem call sites ---
  //
  // Wizard exit handling: after every Continue/Redeem returns an
  // OnboardingStep, check it. If "Done", read wizardOutcome() and route.
  // Persistence happens here (Rule 5: "Persistence happens on machine
  // return values, not on observer ticks").

  async function onHandleEntryContinue() {
    if (!m) return;
    error = '';
    try {
      const stepName = await m.submitHandleCheckContinue();
      if (stepName === 'NestRecovery' && m.recoveryIntent()) {
        // Q2-A (box-recovery.md § Recovery UI (step 4)): the admin just
        // connected to a SURVIVING nest they own (AlreadyOnNest) to read its
        // deployment-seed custody. Point THIS TAB at the resolved URL so
        // the nest_recovery $effect's joined read (this device's store + that
        // nest's cold read) fires immediately, instead of the local read
        // alone with no stored URL. Tab-scoped, never the
        // registry: entering a screen never changes which identity (or home
        // nest) is canonical — only the `LoggedIn` terminal records one, and
        // the next boot/re-pin rewrites this beside the tab's pin.
        // `handleWizardExit` only acts on `Done`, so this isn't its concern.
        setTabNestUrl(m.nestUrl());
      }
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function onInviteSubmit() {
    if (!m) return;
    error = '';
    try {
      // wizardSubmitInviteRequest doesn't terminate — the wizard stays on
      // InviteRequest. After it returns, the snapshot transitions to
      // PendingReview (or Error). Per target-state §3 "Persistence callouts":
      // on PendingReview, write the long-term-store slot so a relaunch can
      // re-seed the wizard via seedPendingInvite.
      await m.wizardSubmitInviteRequest();
      await persistInviteSlotIfActionable();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function onRecheckInvite() {
    if (!m) return;
    error = '';
    try {
      await m.recheckInviteStatus();
      // Per target-state §3: actionable states (PendingReview / Approved)
      // refresh the slot's status_json; the not-found Error variant clears
      // the slot.
      await persistInviteSlotIfActionable();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function persistInviteSlotIfActionable() {
    if (!m) return;
    const snap = m.inviteRequestSnapshot();
    const state = snap.state;
    // 404 from recheck parks the snapshot at
    // Error{transient:false, context:Rechecking, cause:'invite.error.not_found'}.
    // Per target-state §3 + machine.rs:1766-1772, drop the slot when this
    // shape is observed.
    if (
      typeof state === 'object'
      && 'Error' in state
      && state.Error.context === 'Rechecking'
      && state.Error.cause === 'invite.error.not_found'
    ) {
      await deletePendingInvite();
      return;
    }
    // The slot is assembled by shared Rust (`pendingInviteSlot()`), not
    // re-derived here: the nest_url rule (machine state, never the
    // provider-override URL) and the opaque status_json are both
    // silent-when-wrong, so they live in one place for all 7 apps
    // (`onboarding.md` § 3 Persistence callouts). It returns undefined unless
    // the wizard is actually in PendingReview.
    const slot = m.pendingInviteSlot();
    if (!slot) return;
    let readyToPersist = true;
    if (appendMode && appendAdoptedRequestId !== slot.request_id) {
      // Append ("Add account") mode: moment 1 (`commitConfirmedIdentity`)
      // deliberately skipped the registry commit (see its doc) — nothing has
      // registered this identity yet. Adopt it now, BEFORE writing the slot:
      // tui's `adopt_appended_pending_invite` shape;
      // onboarding.md § Multi-account — "the append glue adopts on the submit
      // return". Without this, `registrySavePendingInvite` below writes onto
      // whichever account `reg.active()` still names — the OUTGOING one, since
      // nothing moved it yet — silently corrupting that account's own slot
      // instead of the new identity's.
      const secret = m.effectiveSecret();
      if (secret) {
        try {
          const newActor = await accountsAdd(secret, null, null);
          await accountsSwitch(newActor);
          appendAdoptedRequestId = slot.request_id;
        } catch (e) {
          logMessage('warn', 'fauna_web::onboarding', `append pending-invite adopt failed: ${e}`);
          readyToPersist = false;
        }
      } else {
        readyToPersist = false;
      }
    }
    if (readyToPersist) {
      await savePendingInvite({
        nestUrl: slot.nest_url,
        handle: slot.handle,
        requestId: slot.request_id,
        statusJson: slot.status_json,
      });
    }
  }

  async function onInviteContinue() {
    if (!m) return;
    error = '';
    const snap = m.inviteRequestSnapshot();
    const oob = snap.out_of_band_code_state;
    const oobValid = typeof oob === 'object' && 'Valid' in oob;
    // Continue is the out-of-band code's redeem and nothing else
    // (`onboarding.md` § 3 — the button's row). The Approved and PendingReview
    // branches retired 2026-08-12 with the continue-exit: no live nest serves
    // Approved, and the pending-review journey advances by polling.
    if (!oobValid) return;
    try {
      handleWizardExit(await m.redeemInvite());
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // --- claim_code handlers (target §3a) ---

  async function onClaimCodeSubmit() {
    if (!m) return;
    error = '';
    try {
      const stepName = await m.wizardSubmitClaimCode(claimCodeValue);
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onClaimCodeBack() {
    error = '';
    m?.clearError();
    m?.back();
  }

  // --- nat_mode_choice handlers (target § 3b-bis) ---

  function onSelectNatMode(mode: NodeMode) {
    error = '';
    m?.clearError();
    m?.selectNatMode(mode);
  }

  async function onNatModeConfirm() {
    if (!m) return;
    error = '';
    try {
      // Terminal on success — commits fauna.setup.nat_mode and exits to
      // LoggedIn (admin). On 4xx/5xx the wizard stays on NatModeChoice with
      // the snapshot at Error; the set is mutable, so resubmit is allowed.
      const stepName = await m.submitNatModeChoice();
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onNatModeDefer() {
    if (!m) return;
    error = '';
    try {
      // Sends nothing — the seeded mode is already a working default, so
      // there is no unresolved state and no resume slot. Returns "Done"
      // with wizardOutcome() == LoggedIn.
      const stepName = m.deferNatModeChoice();
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // onboarding.md § 3b-ter — `trust_prompt`. Both exits are pure local
  // latches (the screen asks only; it holds no authenticated session), so
  // both handlers latch-and-conclude inline exactly like `onNatModeDefer` —
  // unlike a native GTK/UI-thread app, nothing here blocks the JS event
  // loop, so there is no reason to defer the conclusion to a later tick.
  function onGrantDefaultTrust() {
    if (!m) return;
    error = '';
    try {
      const stepName = m.grantDefaultTrust();
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onSkipTrustPrompt() {
    if (!m) return;
    error = '';
    try {
      const stepName = m.skipTrustPrompt();
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Onboarding→launch hand-off (dns-management.md § Where the credential lives /
  // onboarding.md §4): if the wizard's DNS step verified a provider credential,
  // seal it into the admin's `fauna.state.dns` now that we have an authenticated
  // identity. The onboarding machine has no `fauna.account.state.put` capability and (in
  // the fresh-provision path) no live nest at the DNS step, so it only *captures*
  // the credential (`capturedDnsCredentialJson()`); the launched client seals it
  // through the post-onboarding `DnsManagementMachine::PutCredentials` path —
  // one store, one writer (mirrors linux `launch_main_app_after_signin`). The
  // machine re-runs `verify()` to (re)derive zones, so what onboarding captured
  // cannot go stale. Returns `null`/no-op for the manual / set-up-later /
  // returning-user paths. Fire-and-forget: the provider verify is a network
  // round-trip we must not block the user's landing on the feed; the WS client +
  // machine are module-level singletons in `$lib/rpc`, so the dispatch outlives
  // this page's navigation.
  function sealCapturedDnsCredential(secretHex: string, credJson: string | undefined): void {
    if (!credJson) return;
    void (async () => {
      try {
        const cred = JSON.parse(credJson) as {
          provider_id: string;
          fields: Record<string, string>;
          label: string;
        };
        await ensureWasm();
        const machine = await dnsManagementMachineWithCredentials(secretHex);
        const fields = Object.entries(cred.fields).map(([id, value]) => ({ id, value }));
        await machine.dispatch({
          PutCredentials: { provider_id: cred.provider_id, fields, label: cred.label },
        });
      } catch (e) {
        // Non-fatal: the admin can re-add the credential from admin-dns. Onboarding
        // already succeeded; don't strand the user.
        console.warn('[onboarding] DNS credential seal failed:', e);
        logMessage('warn', 'fauna_web::onboarding', `DNS credential seal failed: ${e}`);
      }
    })();
  }

  // onboarding.md § 3b-ter: the one-tap trust answer latched on the
  // `trust_prompt` page. The page only asks — minting needs this
  // authenticated session and the nest's own content-processor roster — so
  // the mint runs here, the same deferral the DNS credential above uses.
  // Best-effort and log-only by design (mirrors tui/linux's
  // `mint_default_trust_set`): a failure must not paint an error over a
  // completed onboarding, and the same trust is grantable any time from
  // Settings → Nests. WHICH grants is not decided here either:
  // `MintDefaultSet` mints exactly what the shared mint catalog derives, so
  // this glue holds no policy that could drift from the Nests page's own
  // picker (and an empty set on a box with nothing enrolled yet is an
  // honest no-op, not an error).
  /** Register the kit the user confirmed on `recovery_kit` — the deferred half
   *  of that screen's ceremony (identity-succession.md § The RecoveryKey →
   *  *Creation UX*). Best-effort: a failure leaves Settings' status line saying
   *  never-created, which is the truth. */
  function registerDeferredRecoveryKit(secretHex: string, kitHex: string | undefined): void {
    if (!kitHex) return;
    void recoveryRegisterDeferredKit(secretHex, kitHex).catch((e: unknown) => {
      logMessage('warn', 'fauna_web::onboarding', `deferred recovery kit: ${e}`);
    });
  }

  /** Land the predecessor seeds a phrase restore recovered, linked to the
   *  restored identity — they are the only copies left anywhere
   *  (identity-succession.md § Seed escrow). The terminal awaits it before
   *  entering the app (`runLoggedInTerminal` owns why). */
  function persistRestoredPredecessors(secretHex: string, json: string): Promise<void> {
    if (json === '[]') return Promise.resolve();
    return accountsPersistRestoredPredecessors(actorIdFromSecret(secretHex), json);
  }

  function mintDefaultTrustSet(secretHex: string, trustGranted: boolean): void {
    // The intent was taken synchronously at the terminal
    // (`captureLoggedInHandoffs`) — `m` may be torn down once we navigate.
    if (!trustGranted) return;
    void (async () => {
      try {
        await ensureWasm();
        const machine = await linkedNestsMachineWithTrust(secretHex);
        await machine.dispatch('MintDefaultSet');
      } catch (e) {
        console.warn('[onboarding] one-tap trust: minting the default set failed:', e);
        logMessage(
          'warn',
          'fauna_web::onboarding',
          `one-tap trust: minting the default set failed (${e}); \
           the same trust can be granted from Settings → Nests`,
        );
      }
    })();
  }

  // The post-claim serving enablement (onboarding.md § 3b *Mechanism*) — ONE
  // shared Rust step every app's `LoggedIn` handoff calls
  // (`fauna_client_mail_settings::serving_enablement`, via the wasm
  // `applyPostClaimServingEnablement`). It owns the whole firing: the
  // first-setup mail provision (`am_i_admin`-discriminated — the admin's
  // mailbox + deployment mail on iff the email intent, or the policy-gated
  // new-user auto-mint), the three DAV deployment toggles, and the
  // one-MSEK-mint-path companion mints — and publishes the
  // `fauna_e2e_agent::SERVING_ENABLEMENT_KEY` completion anchor. The four
  // intents are machine-derived and claim-gated (§ 3b *Gating rule*), so a
  // sign-in or invite redemption hands over all four OFF. Captured
  // synchronously, before any await — the onboarding machine `m` may be torn
  // down once we navigate to the feed. Fire-and-forget: every step logs its own
  // failure and each is redoable from the settings pages.
  function applyPostClaimServingEnablement(
    secretHex: string,
    intents: { email: boolean; caldav: boolean; carddav: boolean; webdav: boolean },
  ): void {
    void (async () => {
      try {
        await ensureWasm();
        await applyServingEnablement(secretHex, intents);
      } catch (e) {
        console.warn('[onboarding] serving enablement failed:', e);
        logMessage('warn', 'fauna_web::onboarding', `serving enablement failed: ${e}`);
      }
    })();
  }

  /** The post-`LoggedIn` hand-offs, with every machine read they need taken
   *  NOW (synchronously, at the terminal — `m` may be torn down once we
   *  navigate). Returns the two runners the terminal calls with the onboarded
   *  identity's secret once its registry write has landed: the awaited
   *  predecessor persist and the fire-and-forget rest. */
  function captureLoggedInHandoffs(): {
    persistRestoredPredecessors: (secretHex: string) => Promise<void>;
    handoffs: (secretHex: string) => void;
  } {
    const credJson = m?.capturedDnsCredentialJson();
    const trustGranted = m?.takeTrustPromptGranted() ?? false;
    // The kit confirmed on `recovery_kit` (consume-once; undefined if skipped)
    // and any predecessor seeds a phrase restore recovered — both read NOW,
    // before the machine is torn down.
    const pendingKitHex = m?.takePendingRecoverySecret();
    const restoredPredecessors = m?.restoredPredecessorsJson() ?? '[]';
    const intents = {
      email: m?.emailEnableRequested() ?? false,
      caldav: m?.caldavEnableRequested() ?? false,
      carddav: m?.carddavEnableRequested() ?? false,
      webdav: m?.webdavEnableRequested() ?? false,
    };
    return {
      persistRestoredPredecessors: (secretHex: string) =>
        persistRestoredPredecessors(secretHex, restoredPredecessors),
      handoffs: (secretHex: string) => {
        registerDeferredRecoveryKit(secretHex, pendingKitHex);
        sealCapturedDnsCredential(secretHex, credJson);
        mintDefaultTrustSet(secretHex, trustGranted);
        applyPostClaimServingEnablement(secretHex, intents);
      },
    };
  }

  function handleWizardExit(stepName: string) {
    if (stepName !== 'Done') return;
    const outcome: WizardOutcome | undefined = m?.wizardOutcome();
    if (!outcome) return;
    if ('LoggedIn' in outcome) {
      void deletePendingInvite();
      void deletePendingFactoryReset();
      // The deferred-DNS flow's ONE claim terminal for the awaiting slot
      // (onboarding.md § Long-term store contract, ratified 2026-09-21): the
      // launch row outranks every other row, so a slot surviving into the
      // logged-in state would pin the next launch on "Almost ready" forever.
      // `accountsPersistLoggedIn` below spends it on the cold-boot path; this
      // explicit call is what covers the append arm, which registers through
      // its own add + switch and never reaches the helper.
      void deleteAwaitingDns();
      // Read from the MACHINE, not the store (onboarding.md § Long-term store
      // contract → *The terminal reads the secret from the MACHINE*): at this
      // terminal the wizard just authenticated with the secret, so the machine
      // always has it, while the registry has it only if moment 1's write
      // already landed — and in append mode moment 1 wrote nothing at all.
      // tui/android/linux read the machine here too.
      // The same identity feeds the post-`LoggedIn` hand-offs below: it is the
      // identity that just onboarded, which in append mode is NOT the active
      // account until the switch lands.
      const secret = m?.effectiveSecret();
      if (!secret) {
        // Structurally impossible (the wizard cannot reach `LoggedIn` without
        // a secret) — a loud bug, never a fallback to the store.
        logMessage('error', 'fauna_web::onboarding', 'LoggedIn terminal: machine has no secret');
        goto('/app/feed', { replaceState: true });
        return;
      }
      const nestUrl = outcome.LoggedIn.nest_url;
      // Captured NOW, before any await: `mintDefaultTrustSet` *takes* the
      // latch and the hand-offs read the machine, which may be torn down once
      // we navigate. They run once the registry write below has landed and
      // re-pinned this tab, so their sockets dial the NEW home nest
      // (`storedNestUrl()` reads the tab's nest beside its pin).
      const reachIpv4 = m?.provisionReachIpv4() ?? null;
      const handoffs = captureLoggedInHandoffs();
      let addedActor: string | null = null;
      void runLoggedInTerminal({
        register: async () => {
          if (appendMode) {
            // Register the new identity as another account + activate it
            // (rather than silently overwriting the one session): accountsAdd
            // records it with its home nest, and `activate` below —
            // accountsSwitch — makes it active and re-pins this tab (append
            // mode is exempt from `persist_logged_in` — activating there would
            // move `active` before this runs).
            addedActor = await accountsAdd(secret, nestUrl, null);
          } else {
            // The `LoggedIn` terminal records the home nest PER-ACTOR — the ONLY
            // place it is recorded (`onboarding.md` § Long-term store contract →
            // *The `LoggedIn` terminal records the home nest PER-ACTOR*). Moment
            // 1 materialized the account with NO `nest_url` row, so a terminal
            // that failed to land this would degrade the next launch's routing
            // tuple to `(Some(secret), None, None)` → `WizardAt(HandleEntry)`: a
            // completed onboarding silently re-rendering the handle-entry page.
            // AWAITED, not fire-and-forget: `goto` mounts the feed, whose
            // `identity.init()` reads the registry back.
            await accountsPersistLoggedIn(secret, nestUrl, null, reachIpv4);
          }
        },
        persistRestoredPredecessors: () => handoffs.persistRestoredPredecessors(secret),
        activate: appendMode
          ? async () => {
              if (addedActor) await accountsSwitch(addedActor);
            }
          : undefined,
        handoffs: () => handoffs.handoffs(secret),
        enterApp: () => void goto('/app/feed', { replaceState: true }),
        onFailure: (step, e) => {
          if (step === 'register') {
            logMessage(
              'error',
              'fauna_web::onboarding',
              appendMode ? `append add-account failed: ${e}` : `persist_logged_in failed: ${e}`,
            );
          } else if (step === 'activate') {
            logMessage('error', 'fauna_web::onboarding', `append switch-account failed: ${e}`);
          } else {
            logMessage('warn', 'fauna_web::onboarding', `restored predecessors: ${e}`);
          }
        },
      });
      return;
      // ⚠ There is deliberately no `InviteSubmitted` arm (retired 2026-08-12).
      // It never worked as its own comment claimed: it said "leave the user on
      // the InviteRequest page", but the exit had already set `step: 'done'`,
      // which has no template arm — so web rendered a BLANK page. The journey
      // now has no exit at all; the wizard stays on `invite_request` and polls,
      // and the slot is written at the submit return (`onSubmitInviteRequest`).
    } else if ('AwaitingManualDns' in outcome) {
      // Deferred DNS: the nest is provisioned but DNS hasn't propagated, so it is
      // not claimed and not reachable. Persist the awaiting-manual-dns slot — all
      // FOUR fields, `handle` included — per onboarding.md § Long-term store
      // contract. `handle` comes from the wizard's `current_handle()` at the exit,
      // not from the outcome payload; it is the field the legacy 3-key slot
      // omitted, which is why that slot could never satisfy `seed_awaiting_manual_dns`
      // and was dead storage.
      //
      // No home nest is recorded: that is the *authenticated* nest, and
      // recording an un-DNS'd, unclaimed nest there is exactly what
      // made the next launch silent-challenge a dead host, fall through to
      // handle_entry, and DISCARD the nest the user just provisioned. The record
      // below carries its own nest_url; the launch row reads it from there.
      //
      // The records go in as `awaitingDnsRecordsJson()` — the shared getter —
      // never a hand-built stringify. serde emits `record_type` while the WASM
      // binding exposes `recordType`, so a hand-rolled round-trip silently
      // deserializes to an EMPTY list: an "Almost ready" page with nothing to add
      // at the registrar, and the failure is invisible until a relaunch.
      void saveAwaitingDns({
        nestUrl: outcome.AwaitingManualDns.nest_url,
        handle: m?.currentHandle() ?? '',
        recordsJson: m?.awaitingDnsRecordsJson() ?? '[]',
        claimCode: outcome.AwaitingManualDns.claim_code,
      });
      // No navigation: the `awaitingDns` derived is already true (the exit set
      // the outcome), so the "Almost ready" surface renders and starts polling.
    }
  }

  // --- Provisioning page helpers ---
  //
  // The status glyph, step name, and sub-step text are single-sourced in shared
  // Rust (`fauna_provisioning::progress::{status_glyph,step_label,substep_label}`,
  // value-formatting.md § Provisioning step display) and consumed over the
  // `fauna-wasm-onboarding` `provisioning{StatusGlyph,StepLabel,SubstepLabel}`
  // exports — the canonical glyph set + the variant→i18n-key mapping (the old
  // `pascalToSnake` re-derivation) now live there, so the template just resolves
  // the returned `LocalizedText`. Only the two-i18n-string concatenations stay
  // SPA-side: the `step_attempt_template` suffix below, and the Skipped-row
  // "already configured" placeholder (one `LocalizedText` can't carry either).

  function attemptSuffix(step: StepSnapshot): string {
    // Gate on the shared `shows_attempt_suffix` projection, not a re-derived
    // `attempt > 1 && max_attempts > 1` (see StepSnapshot doc comment).
    if (step.shows_attempt_suffix) {
      return t.onboarding.provision.step_attempt_template({
        attempt: String(step.attempt),
        max_attempts: String(step.max_attempts),
      });
    }
    return '';
  }

  // The `provisioning-elapsed` ticker compute + format is single-sourced in
  // shared Rust (`fauna_provisioning::progress::elapsed_display`, value-formatting.md):
  // returns the canonical `onboarding.nest_provisioning.elapsed_template` i18n
  // key, resolved here like every other app (no hand-rolled English).
  function formatElapsed(snap: ProvisioningSnapshot, currentMs: number): string {
    return resolveLocalized(
      provisioningElapsedRaw(snap.started_at_ms ?? undefined, snap.finished_at_ms ?? undefined, currentMs),
    );
  }

  // 1s ticker: only runs while `overall === 'Running'`, so the elapsed
  // display advances even when no orchestrator notification fires
  // between substep transitions.
  $effect(() => {
    if (provisioningSnapshotValue?.overall !== 'Running') return;
    const id = setInterval(() => { nowMs = Date.now(); }, 1000);
    return () => clearInterval(id);
  });

  // Top-region `provisioning-start-button` — "Buy and set up" CTA.
  // Spawns the orchestrator on the WASM runtime and returns
  // immediately; observer ticks drive snapshot re-render. Idempotent —
  // safe to re-press after a partial prior run (the orchestrator's
  // pre-flight short-circuits done steps).
  function onProvisioningStart() {
    if (!m) return;
    error = '';
    try { m.startProvisioning(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
  }

  function onProvisioningCancel() {
    if (!m) return;
    error = '';
    try { m.cancelProvisioning(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
  }

  function onProvisioningRetry() {
    if (!m) return;
    error = '';
    try { m.retryProvisioning(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
  }

  // Bottom-row Continue. Enabled only when overall == Succeeded.
  // continue_from_provisioning() advances the wizard to NatModeChoice (the
  // standard path — `Succeeded` there means built AND claimed, so the § 3b-bis
  // tail owns the exit) or DnsPostInstructions (deferred-DNS path); we route on
  // the returned step. It returns the CURRENT step unchanged when the box is not
  // claimed yet, which `handleWizardExit` treats as a no-op like any other
  // non-terminal step.
  function onProvisioningContinue() {
    if (!m) return;
    error = '';
    try {
      const stepName = m.continueFromProvisioning();
      handleWizardExit(stepName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

</script>

<div class="onboarding">
  <!-- The "Almost ready" surface (onboarding.md § "Almost ready" surface).
       NOT an OnboardingStep, so it is matched BEFORE the step branches: it is
       rendered whenever `wizard_outcome() == AwaitingManualDns`, which is true on
       BOTH paths that reach it — the same-session exit from dns_post_instructions
       (where `step` is already 'done') and the relaunch hydration (where the
       machine's step is whatever seed_identity left it on). Structural twin of
       launch_retry, likewise a non-step surface with a ui.yaml page. -->
  {#if awaitingDns}
    <div class="center">
      <h1>{t.onboarding.awaiting_dns.title}</h1>

      <!-- The machine owns the status wording (including the terminal
           `Error { cause }`), handed over as a LocalizedText — never re-derived
           here, so every app says the same thing in the same state. -->
      <p data-testid={IDS.AWAITING_DNS_STATUS} class="muted">
        {resolveLocalized(awaitingDnsSnapshot?.message)}
      </p>

      <pre data-testid={IDS.AWAITING_DNS_RECORDS} class="dns-records">{awaitingDnsText}</pre>

      <div class="button-group">
        <button
          class="btn"
          data-testid={IDS.AWAITING_DNS_COPY_BUTTON}
          disabled={!awaitingDnsCopyEnabled}
          onclick={() => { void navigator.clipboard?.writeText(awaitingDnsText); }}
        >{t.onboarding.awaiting_dns.copy_button}</button>
        <button
          class="btn primary"
          data-testid={IDS.AWAITING_DNS_RECHECK_BUTTON}
          disabled={awaitingDnsBusy}
          onclick={() => { void recheckAwaitingDns(); }}
        >{t.onboarding.awaiting_dns.recheck_button}</button>
      </div>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'loading'}
    <div class="center">
      {#if launchState === 'transient_error'}
        <!-- Silent-challenge transient retry per target-state §"App-launch routing":
             Network failure → show retry indicator on launch screen;
             user can wait+retry, or fall back to wizard at handle_entry. -->
        <p data-testid={IDS.LAUNCH_TRANSIENT_ERROR} class="muted">
          {t.onboarding.launch.transient_error}
        </p>
        <div class="button-group">
          <button
            class="btn primary"
            data-testid={IDS.LAUNCH_RETRY_BUTTON}
            onclick={() => { void retrySilentChallenge(); }}
          >{t.common.retry}</button>
          <button
            class="btn link"
            data-testid={IDS.LAUNCH_FALLTHROUGH_BUTTON}
            onclick={() => { void fallThroughToHandleEntry(); }}
          >{t.onboarding.launch.use_different_nest}</button>
          {#if recoverableBoxes.length > 0}
            <!-- Surviving-device recovery entry (box-recovery.md § Recovery UI
                 (step 4)). Shown only when the admin custodies ≥1 deployment
                 seed — `recoverableBoxes` is filled by the launch-time
                 joined custody read (`resolveRecoveryBoxes`). -->
            <button
              class="btn link"
              data-testid={IDS.LAUNCH_RECOVER_BUTTON}
              onclick={() => { void recoverFromLaunch(); }}
            >{t.onboarding.launch.recover_lost_box}</button>
          {/if}
        </div>
      {:else if launchState === 'needs_update'}
        <!-- Nest authoritatively reported it is outdated (`fauna.nest.outdated`):
             a NON-retry terminal surface (no Retry CTA — retrying the same
             outdated nest is futile). Render the localized update message in the
             canonical `error-message` element and offer only "Use a different
             nest". version-compatibility.md Dim 4 / onboarding.md § App-launch
             routing (version-mismatch row). -->
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{needsUpdateMessage}</div>
        <div class="button-group">
          <button
            class="btn link"
            data-testid={IDS.LAUNCH_FALLTHROUGH_BUTTON}
            onclick={() => { void fallThroughToHandleEntry(); }}
          >{t.onboarding.launch.use_different_nest}</button>
        </div>
      {:else if launchState === 'sign_in_refused'}
        <!-- The saved nest no longer signs this identity in (page
             `launch_sign_in_refused`): the honest sentence in its own element,
             WITH Retry — the way back in once the admin restores — and "Use a
             different nest". Never the invite wizard: the nest already holds the
             account. onboarding.md § App-launch routing. -->
        <h1>{t.onboarding.launch.sign_in_refused_title}</h1>
        <p data-testid={IDS.LAUNCH_SIGN_IN_REFUSED_NOTICE} class="muted">
          {t.onboarding.launch.sign_in_refused}
        </p>
        <div class="button-group">
          <button
            class="btn primary"
            data-testid={IDS.LAUNCH_RETRY_BUTTON}
            onclick={() => { void retrySilentChallenge(); }}
          >{t.common.retry}</button>
          <button
            class="btn link"
            data-testid={IDS.LAUNCH_FALLTHROUGH_BUTTON}
            onclick={() => { void fallThroughToHandleEntry(); }}
          >{t.onboarding.launch.use_different_nest}</button>
        </div>
      {:else if launchState === 'identity_changed'}
        <!-- (security.md § Transport trust, the web-exempt note):
             the nest's pinned deployment identity changed, or it can no longer
             prove the identity we previously trusted. BLOCK auto-entry and warn
             loudly (the SSH known_hosts model). The user either explicitly
             re-trusts (forget the pin → re-TOFU on the next connect) or points at
             a different nest. -->
        <div data-testid={IDS.NEST_IDENTITY_CHANGED_WARNING} class="error" role="alert">
          {t.onboarding.launch.identity_changed_warning}
        </div>
        <div class="button-group">
          <button
            class="btn primary"
            data-testid={IDS.NEST_IDENTITY_CHANGED_TRUST_BUTTON}
            onclick={() => { void trustChangedNestIdentity(); }}
          >{t.onboarding.launch.identity_changed_trust}</button>
          <button
            class="btn link"
            data-testid={IDS.LAUNCH_FALLTHROUGH_BUTTON}
            onclick={() => { void fallThroughToHandleEntry(); }}
          >{t.onboarding.launch.use_different_nest}</button>
        </div>
      {:else if launchState === 'account_index_refusal'}
        <!-- The saved account index is present and this build cannot use it
             (`version-compatibility.md` § 5 item 9). Never a retry or
             fallthrough — the nest was never contacted. The `NewerBuild`
             verdict offers nothing else; the `Malformed` verdict reveals a
             start-over confirm (`account-index-reset-confirm-button`) that
             states its residual before running the documented floor
             (`long-term-store.md` § Cleanup contract). -->
        <div data-testid={IDS.ACCOUNT_INDEX_REFUSAL_WARNING} class="error" role="alert">
          {accountIndexRefusal?.kind === 'Malformed'
            ? (accountIndexResetConfirming
                ? t.onboarding.launch.index_malformed_reset_residual
                : t.onboarding.launch.index_malformed)
            : t.onboarding.launch.index_newer_build}
        </div>
        {#if accountIndexRefusal?.kind === 'Malformed'}
          <div class="button-group">
            {#if accountIndexResetConfirming}
              <button
                class="btn danger"
                data-testid={IDS.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON}
                onclick={() => { void confirmAccountIndexReset(); }}
              >{t.onboarding.launch.index_malformed_reset_confirm}</button>
            {:else}
              <button
                class="btn danger"
                data-testid={IDS.ACCOUNT_INDEX_RESET_BUTTON}
                onclick={revealAccountIndexReset}
              >{t.onboarding.launch.index_malformed_reset}</button>
            {/if}
          </div>
        {/if}
      {:else if launchFailureMessage}
        <!-- The defensive launch-failure surface (common.md § Client-state
             recoverability): the launch sequence died or never settled AND the
             fallback wizard could not come up either (`m` still null — else the
             wizard page renders and carries the banner via `errorMessageValue`).
             Reuses the page's canonical `error-message` + the existing
             fallthrough CTA — no new testids. -->
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{launchFailureMessage}</div>
        <div class="button-group">
          <button
            class="btn link"
            data-testid={IDS.LAUNCH_FALLTHROUGH_BUTTON}
            onclick={() => { void fallThroughToHandleEntry(); }}
          >{t.onboarding.launch.use_different_nest}</button>
        </div>
      {:else}
        <p class="muted">{t.common.loading}</p>
      {/if}
    </div>

  {:else if step === 'identity_choice'}
    <div class="center">
      <h1>{t.onboarding.identity_choice.title}</h1>
      <p class="subtitle">{t.onboarding.identity_choice.subtitle}</p>

      <div class="button-group">
        <button
          class="btn primary large"
          data-testid={IDS.CREATE_IDENTITY_BUTTON}
          onclick={createIdentity}
        >{t.onboarding.identity_choice.create_new}</button>

        <button
          class="btn large"
          data-testid={IDS.IMPORT_IDENTITY_BUTTON}
          onclick={goToImport}
        >{t.onboarding.identity_choice.import_existing}</button>

        <!-- The phrase-only IDENTITY restore (onboarding.md § 1 Identity) —
             distinct from the lost-box NEST recovery below. -->
        <button
          class="btn large"
          data-testid={IDS.RESTORE_FROM_RECOVERY_KIT_BUTTON}
          onclick={restoreFromRecoveryKit}
        >{t.onboarding.identity_choice.restore_from_recovery_kit}</button>

        <!-- Fresh-client recovery entry (box-recovery.md § Recovery UI
             (step 4)). Routes through identity_import with recovery intent,
             then lands on nest_recovery. -->
        <button
          class="btn link"
          data-testid={IDS.RECOVER_LOST_BOX_BUTTON}
          onclick={recoverLostBox}
        >{t.onboarding.identity_choice.recover_lost_box}</button>
      </div>

      <!-- What a sign-out's erase could not remove from this browser
           (account-scoping.md § Erasure follows scope → the residue surface):
           present exactly while the sign-out record owes a store. -->
      {#if signOutResidueLine}
        <div data-testid={IDS.SIGN_OUT_RESIDUE} class="sign-out-residue">
          <p data-testid={IDS.SIGN_OUT_RESIDUE_MESSAGE} class="error">{signOutResidueLine}</p>
          <button
            class="btn"
            data-testid={IDS.SIGN_OUT_RESIDUE_RETRY_BUTTON}
            disabled={residueRetrying}
            onclick={retrySignOutResidue}
          >{t.settings.sign_out_residue_retry}</button>
        </div>
      {/if}

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'identity_created'}
    <div class="center">
      <h1>{t.onboarding.identity_created.title}</h1>
      <p class="subtitle">{t.onboarding.identity_created.desc}</p>

      <div class="secret-display">
        <span class="field-label">{t.onboarding.identity_created.secret_key_label}</span>
        <code class="secret-key" data-testid={IDS.SECRET_KEY_DISPLAY}>{(machineTick.value, m?.generatedSecret() ?? '')}</code>
        <button
          class="btn-copy"
          data-testid={IDS.SECRET_KEY_COPY_BTN}
          onclick={copySecretKey}
        >{secretKeyCopied ? t.common.copied : t.common.copy}</button>
      </div>

      <p class="warning">{t.onboarding.identity_created.warning}</p>

      <button
        class="btn primary large"
        data-testid={IDS.IDENTITY_CONTINUE_BUTTON}
        onclick={identityContinue}
      >{t.onboarding.identity_created.continue}</button>

      <button data-testid={IDS.IDENTITY_CREATED_BACK_BUTTON} class="btn link" onclick={backToIdentityChoice}>{t.common.back}</button>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'identity_import'}
    <div class="center">
      <h1>{t.onboarding.identity_import.title}</h1>
      <p class="subtitle">{t.onboarding.identity_import.paste_subtitle}</p>

      <div class="form-group">
        <label class="field-label" for="paste-secret">{t.onboarding.identity_import.paste_label}</label>
        <input
          id="paste-secret"
          type="text"
          class="input"
          data-testid={IDS.PASTE_SECRET_FIELD}
          placeholder={t.onboarding.identity_import.paste_placeholder}
          bind:value={importHex}
        />
      </div>

      <button
        class="btn primary large"
        data-testid={IDS.IMPORT_SUBMIT_BUTTON}
        onclick={importIdentity}
      >{t.onboarding.identity_import.import}</button>

      <button data-testid={IDS.IDENTITY_IMPORT_BACK_BUTTON} class="btn link" onclick={() => { error = ''; m?.back(); }}>{t.common.back}</button>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'recovery_kit'}
    <!-- onboarding.md § 1 Identity — the recovery-kit offer, right after the
         identity secret. Mints and displays only; `recovery-kit-escrow-status`
         therefore renders exactly one state here, the deferred line. -->
    <div class="center">
      <h1>{t.onboarding.recovery_kit.title}</h1>
      <p class="subtitle" data-testid={IDS.RECOVERY_KIT_DESCRIPTION}>{t.onboarding.recovery_kit.desc}</p>

      <div class="secret-display">
        <code class="secret-key" data-testid={IDS.RECOVERY_KIT_SECRET_DISPLAY}>{recoveryKitSecretValue ?? t.onboarding.recovery_kit.not_minted}</code>
        <button
          class="btn-copy"
          data-testid={IDS.RECOVERY_KIT_SECRET_COPY_BTN}
          disabled={!recoveryKitSecretValue}
          onclick={copyRecoveryKit}
        >{recoveryKitCopied ? t.common.copied : t.common.copy}</button>
      </div>

      {#if recoveryKitQr}
        <!-- Dark-on-light, one <rect> per dark module (the Settings kit
             display's shape) — a theme-inverted QR does not scan. -->
        <svg
          data-testid={IDS.RECOVERY_KIT_QR}
          class="identity-qr"
          viewBox="0 0 {recoveryKitQr.side} {recoveryKitQr.side}"
          shape-rendering="crispEdges"
          role="img"
          aria-label={t.onboarding.recovery_kit.title}
        >
          <rect x="0" y="0" width={recoveryKitQr.side} height={recoveryKitQr.side} fill="#fff" />
          {#each recoveryKitQr.darkModules as mod (mod.key)}
            <rect x={mod.x} y={mod.y} width="1" height="1" fill="#000" />
          {/each}
        </svg>
      {/if}

      <p class="muted" data-testid={IDS.RECOVERY_KIT_ESCROW_STATUS}>{t.onboarding.recovery_kit.escrow_deferred}</p>

      <button
        class="btn primary large"
        data-testid={IDS.RECOVERY_KIT_CONFIRM_BUTTON}
        disabled={!recoveryKitSecretValue}
        onclick={confirmRecoveryKit}
      >{t.onboarding.recovery_kit.confirm}</button>

      <button data-testid={IDS.RECOVERY_KIT_SKIP_BUTTON} class="btn link" onclick={skipRecoveryKit}>{t.onboarding.recovery_kit.skip}</button>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'recovery_entry'}
    <!-- onboarding.md § 1 Identity — the phrase-only identity restore. Every
         refusal's words are the shared `RecoveryEntryOutcome::message`; the
         submit is always enabled, because every way the input can be wrong
         is an answer the ceremony gives on `error-message`. -->
    <div class="center">
      <h1>{t.onboarding.recovery_entry.title}</h1>
      <p class="subtitle">{t.onboarding.recovery_entry.desc}</p>
      <p class="muted">{t.onboarding.recovery_entry.account_hint}</p>

      <div class="form-group">
        <label class="field-label" for="recovery-phrase">{t.onboarding.recovery_entry.phrase_label}</label>
        <input
          id="recovery-phrase"
          type="text"
          class="input"
          data-testid={IDS.RECOVERY_ENTRY_PHRASE_FIELD}
          bind:value={recoveryPhrase}
        />
      </div>
      <div class="form-group">
        <label class="field-label" for="recovery-account">{t.common.handle}</label>
        <input
          id="recovery-account"
          type="text"
          class="input"
          data-testid={IDS.RECOVERY_ENTRY_ACCOUNT_FIELD}
          bind:value={recoveryAccount}
        />
      </div>

      <button
        class="btn primary large"
        data-testid={IDS.RECOVERY_ENTRY_SUBMIT_BUTTON}
        onclick={submitRecoveryEntry}
      >{t.onboarding.recovery_entry.submit}</button>

      <button data-testid={IDS.RECOVERY_ENTRY_BACK_BUTTON} class="btn link" onclick={backToIdentityChoice}>{t.common.back}</button>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'handle_entry'}
    <!-- Handle-entry stage.
         Driven entirely by handleCheckSnapshot(); the Continue button reads
         submitHandleCheckContinue() and routes on the returned step (Done →
         wizardOutcome). -->
    <div class="center">
      <h1>{t.onboarding.handle.prompt}</h1>
      <p class="subtitle">{t.onboarding.handle.examples_help}</p>
      <p class="help">{t.onboarding.handle.localhost_hint}</p>

      <div class="form-group handle-row">
        <input
          type="text"
          class="input"
          data-testid={IDS.HANDLE_INPUT}
          value={handleValue}
          oninput={(e) => m?.setCurrentHandle((e.currentTarget as HTMLInputElement).value)}
          placeholder="alice@example.com"
        />
        <button
          class="btn"
          data-testid={IDS.HANDLE_CHECK_BUTTON}
          disabled={!handleValue.trim() || !m}
          onclick={() => { error = ''; m?.clearError(); void m?.startHandleCheck(handleValue); }}
        >{t.common.check}</button>
      </div>

      {#if handleCheckSnapshotValue}
        <div data-testid={IDS.HANDLE_MESSAGE_AREA} class="handle-message" class:phase-loading={handleCheckSnapshotValue.phase !== 'Idle' && handleCheckSnapshotValue.phase !== 'Complete'}>
          {Lookup(handleCheckSnapshotValue.message)}
        </div>
        {#if handleCheckSnapshotValue.control_checkbox_visible}
          <label class="checkbox-row">
            <input
              type="checkbox"
              data-testid={IDS.HANDLE_CONTROL_CHECKBOX}
              checked={handleCheckSnapshotValue.control_checkbox_checked}
              onchange={(e) => m?.setControlCheckbox((e.currentTarget as HTMLInputElement).checked)}
            />
            {t.onboarding.handle.control_checkbox}
          </label>
        {/if}
      {/if}

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <div class="button-group">
        <button
          class="btn link"
          data-testid={IDS.HANDLE_ENTRY_BACK_BUTTON}
          onclick={() => { error = ''; m?.clearError(); m?.back(); }}
        >{t.common.back}</button>

        <button
          class="btn primary large"
          data-testid={IDS.HANDLE_ENTRY_CONTINUE_BUTTON}
          disabled={!handleCheckSnapshotValue?.continue_enabled}
          onclick={onHandleEntryContinue}
        >{t.common.continue}</button>
      </div>
    </div>

  {:else if step === 'dns_config'}
    <div class="center">
      <h1>{t.onboarding.dns_config.title}</h1>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <label class="checkbox-row">
        <input
          type="checkbox"
          data-testid={IDS.DNS_BUY_DOMAIN_CHECKBOX}
          checked={dnsConfigValue?.buy_domain ?? false}
          onchange={(e) => m?.toggleBuyDomain((e.currentTarget as HTMLInputElement).checked)}
          disabled={domainStatusValue !== 'Unregistered'}
        />
        {t.onboarding.dns_config.buy_domain_checkbox}
      </label>

      <label class="checkbox-row">
        <input
          type="checkbox"
          data-testid={IDS.DNS_SAME_PROVIDER_CHECKBOX}
          checked={dnsConfigValue?.same_provider_for_vps ?? false}
          onchange={(e) => m?.toggleSameProviderForVps((e.currentTarget as HTMLInputElement).checked)}
        />
        {t.onboarding.dns_config.same_provider_checkbox}
      </label>

      <!--
        Click is delegated at the container, not per-button. The provider
        buttons combine a dynamic template-literal `data-testid` with a dynamic
        `disabled` attribute, which made Svelte drop the per-button `onclick`
        (no event fired). The container <div> has a static testid + a single
        handler, so its `onclick` works; clicks from the enabled buttons bubble
        to it. The native `disabled` attribute is kept (the uniform cross-app
        `is_disabled` contract — disabled buttons don't dispatch click, so they
        self-guard); the provider is identified via `data-provider-id`.
      -->
      <div
        data-testid={IDS.DNS_PROVIDER_ROW}
        class="provider-row"
        onclick={(e) => {
          const btn = (e.target as HTMLElement).closest('button[data-provider-id]') as HTMLButtonElement | null;
          if (btn && !btn.disabled && btn.dataset.providerId) m?.selectDnsProvider(btn.dataset.providerId);
        }}
      >
        {#each dnsProviders() as p (p.id)}
          <button
            data-testid={`dns-provider-row[${p.id}]`}
            data-provider-id={p.id}
            class="btn"
            class:selected={dnsConfigValue?.selected_provider_id === p.id}
            disabled={!dnsProviderEnabled(p)}
          >{L(p.displayNameKey)}</button>
          {#if !dnsProviderEnabled(p) && dnsProviderIneligibleReasonText(p)}
            <p class="muted">{dnsProviderIneligibleReasonText(p)}</p>
          {/if}
          <!-- The selected provider's link + credentials sub-form render HERE,
               as a direct sibling of the class:selected button INSIDE this
               {#each}, NOT a top-level `{#if selectedDnsProvider}` block after
               the row. Svelte 5 (5.53.12) does not re-schedule a step-block-level
               {#if}/`$effect` whose condition stayed falsy across the two
               dns_config-checkbox machineTick bumps and then flips truthy on the
               provider-click tick — the form silently never appeared after the
               natural uncheck-a-box-then-select flow. A {#each}-item effect on
               the same `dnsConfigValue?.selected_provider_id === p.id` expression
               the button's `class:selected` uses IS re-scheduled, so we render
               the form in this item. Verified by driver instrumentation; see
               memory svelte5-step-block-if-not-scheduled-use-each-scope. The
               `provider-detail` wrapper is full-width (flex-basis:100%) so it
               breaks below the button row. -->
          {#if dnsConfigValue?.selected_provider_id === p.id}
            <div class="provider-detail">
            <a
              data-testid={IDS.DNS_PROVIDER_LINK}
              href={p.signupUrl}
              target="_blank"
              rel="noopener noreferrer"
            >{p.signupUrl}</a>
            <button
              class="btn"
              data-testid={IDS.DNS_PROVIDER_OPEN_BROWSER_BUTTON}
              onclick={() => window.open(p.signupUrl, '_blank', 'noopener,noreferrer')}
            >{t.onboarding.dns_config.open_in_browser}</button>

            <p data-testid={IDS.DNS_PROVIDER_HELP_TEXT}>{L(p.helpKey)}</p>

            <div data-testid={IDS.DNS_CREDENTIALS_FORM}>
              {#each visibleDnsFieldsValue as field (field.id)}
                {#if field.field_type === 'HostedAuth'}
                  <!-- A `hosted-auth` field is a button, not an input — the
                       bundled provider's hosted sign-in (onboarding.md § 4).
                       Same derived id as every field; the button is a SIBLING
                       of the caption span, not nested inside a wrapping
                       <label> (a <button> is itself labelable, so a wrapping
                       label double-fires its click). -->
                  <div class="form-group">
                    <span class="field-label">{L(field.label_key)}</span>
                    <button
                      class="btn"
                      data-testid={`dns-credentials-form-${field.id}`}
                      disabled={!(hostedAuthDnsValue[field.id]?.enabled ?? false)}
                      onclick={() => beginHostedAuth('dns', field.id)}
                    >{hostedAuthDnsValue[field.id]?.label ?? t.provisioning.hosted_auth.connect}</button>
                  </div>
                {:else}
                  <label class="form-group">
                    <span class="field-label">{L(field.label_key)}</span>
                    <input
                      type={field.field_type === 'Secret' ? 'password' : 'text'}
                      class="input"
                      data-testid={`dns-credentials-form-${field.id}`}
                      value={dnsConfigValue?.creds[field.id] ?? ''}
                      oninput={(e) => m?.setDnsCred(field.id, (e.currentTarget as HTMLInputElement).value)}
                    />
                  </label>
                {/if}
              {/each}
            </div>

            <button
              class="btn"
              data-testid={IDS.DNS_VERIFY_BUTTON}
              disabled={!canVerifyDnsValue || isLoadingValue}
              onclick={async () => {
                error = '';
                try {
                  await m?.verifyDns();
                } catch (e) {
                  error = e instanceof Error ? e.message : String(e);
                }
              }}
            >{isLoadingValue ? t.common.loading : t.common.verify}</button>

            <!-- Per target-state §4: price display reads from
                 provider_status() (the canonical computed view), not raw
                 dns_config().current_availability. -->
            {#if buyableProviderStatus}
              <div data-testid={IDS.DNS_TLD_PRICE_DISPLAY}>
                {formatPrice(
                  BigInt(buyableProviderStatus.price_cents),
                  buyableProviderStatus.currency ?? 'USD',
                )}
              </div>
              <label class="checkbox-row">
                <input
                  type="checkbox"
                  data-testid={IDS.DNS_PRICE_CONFIRM_CHECKBOX}
                  checked={dnsConfigValue?.price_agreed ?? false}
                  value={t.registrar.price_confirm}
                  onchange={(e) => {
                    if ((e.currentTarget as HTMLInputElement).checked) m?.confirmPrice();
                  }}
                />
                {t.registrar.price_confirm}
              </label>
            {/if}

            <!-- Per target-state §4: dns-registrar-notes-text when
                 buy_domain && selected provider has registrarNotesKey. -->
            {#if shouldShowRegistrarNotesValue && p.registrarNotesKey}
              <p data-testid={IDS.DNS_REGISTRAR_NOTES_TEXT} class="help">
                {L(p.registrarNotesKey)}
              </p>
            {/if}

            <p data-testid={IDS.DNS_STATUS_TEXT}>{dnsStatusTextValue}</p>

            <!-- Per target-state §4: dns-contact-form when buy_domain &&
                 provider_status == UnregisteredBuyable && registrarRequiresContact. -->
            {#if shouldShowContactFormValue}
              <fieldset data-testid={IDS.DNS_CONTACT_FORM} class="contact-form">
                <legend>{t.onboarding.dns_config.contact_form_heading}</legend>
                {#each contactFieldDefs as f (f.id)}
                  <label class="form-group">
                    <span class="field-label">{f.label}</span>
                    <input
                      type={f.id === 'email' ? 'email' : (f.id === 'phone' ? 'tel' : 'text')}
                      class="input"
                      data-testid={`dns-contact-${f.id}-input`}
                      value={contactValue[f.field] ?? ''}
                      oninput={(e) => onContactFieldInput(f.field, (e.currentTarget as HTMLInputElement).value)}
                    />
                  </label>
                {/each}
              </fieldset>
            {/if}
            </div>
          {/if}
        {/each}
      </div>

      <button
        class="btn link"
        data-testid={IDS.DNS_SET_UP_LATER_BUTTON}
        onclick={() => m?.dnsSetUpLater()}
      >{t.onboarding.dns_config.set_up_later}</button>
      <p class="help">{t.onboarding.dns_config.set_up_later_warning}</p>

      <!-- Per target-state §4: dns-no-provider-message visible when
           buy_domain && the handle-check outcome is DomainAvailable with
           buyable_via_provider == false (shared shouldShowNoProviderMessage).
           Render outside the per-provider block so the user sees it
           before drilling into a specific (non-supporting) provider. -->
      {#if shouldShowNoProviderMessageValue}
        <p data-testid={IDS.DNS_NO_PROVIDER_MESSAGE} class="warning">
          {t.onboarding.dns_config.no_provider_carries_tld({ tld: handleTld })}
        </p>
      {/if}

      <div class="button-group">
        <button
          class="btn link"
          data-testid={IDS.DNS_CONFIG_BACK_BUTTON}
          onclick={() => { error = ''; m?.clearError(); m?.back(); }}
        >{t.common.back}</button>

        <button
          class="btn primary large"
          data-testid={IDS.DNS_CONFIG_CONTINUE_BUTTON}
          disabled={!canContinueDnsValue}
          onclick={() => {
            error = '';
            try {
              m?.continueFromDns();
            } catch (e) {
              error = e instanceof Error ? e.message : String(e);
            }
          }}
        >{t.common.continue}</button>
      </div>
    </div>

  {:else if step === 'vps_config'}
    <div class="center">
      <h1>{t.onboarding.vps_config.title}</h1>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <div data-testid={IDS.VPS_PROVIDER_ROW} class="provider-row">
        {#each vpsProviders() as p (p.id)}
          <button
            data-testid={`vps-provider-row[${p.id}]`}
            class="btn"
            class:selected={vpsConfigValue?.selected_provider_id === p.id}
            onclick={() => m?.selectVpsProvider(p.id)}
          >{L(p.displayNameKey)}</button>
          <!-- Same Svelte 5 reactivity fix as the dns_config credentials form:
               render the selected provider's link + credentials sub-form INSIDE
               this {#each} (a sibling of the class:selected button), not a
               top-level `{#if selectedVpsProvider}` block. A step-block-level
               {#if} off the selection is not re-scheduled when the value flips
               after intermediate same-value machineTick bumps; a {#each}-item
               effect on the same expression the button's class:selected uses IS.
               See the dns_config comment + memory
               svelte5-step-block-if-not-scheduled-use-each-scope. Kept uniform
               with DNS (priority #1). The `provider-detail` wrapper is full-width
               (order:1) so it breaks below the button row. -->
          {#if vpsConfigValue?.selected_provider_id === p.id}
            <div class="provider-detail">
            <a
              data-testid={IDS.VPS_PROVIDER_LINK}
              href={p.signupUrl}
              target="_blank"
              rel="noopener noreferrer"
            >{p.signupUrl}</a>
            <button
              class="btn"
              data-testid={IDS.VPS_PROVIDER_OPEN_BROWSER_BUTTON}
              onclick={() => window.open(p.signupUrl, '_blank', 'noopener,noreferrer')}
            >{t.onboarding.dns_config.open_in_browser}</button>

            <p data-testid={IDS.VPS_PROVIDER_HELP_TEXT}>{L(p.helpKey)}</p>

            <div data-testid={IDS.VPS_CREDENTIALS_FORM}>
              {#each p.fields.filter((f) => f.kinds.includes('vps')) as field (field.id)}
                {#if field.type === 'hosted-auth'}
                  <!-- Twin of the dns_config hosted-auth arm above — same
                       derived id, same button-as-sibling-of-caption shape. -->
                  <div class="form-group">
                    <span class="field-label">{L(field.labelKey)}</span>
                    <button
                      class="btn"
                      data-testid={`vps-credentials-form-${field.id}`}
                      disabled={!(hostedAuthVpsValue[field.id]?.enabled ?? false)}
                      onclick={() => beginHostedAuth('vps', field.id)}
                    >{hostedAuthVpsValue[field.id]?.label ?? t.provisioning.hosted_auth.connect}</button>
                  </div>
                {:else}
                  <label class="form-group">
                    <span class="field-label">{L(field.labelKey)}</span>
                    <input
                      type={field.type === 'secret' ? 'password' : 'text'}
                      class="input"
                      data-testid={`vps-credentials-form-${field.id}`}
                      value={vpsConfigValue?.creds[field.id] ?? ''}
                      oninput={(e) => m?.setVpsCred(field.id, (e.currentTarget as HTMLInputElement).value)}
                    />
                  </label>
                {/if}
              {/each}
            </div>

            <button
              class="btn"
              data-testid={IDS.VPS_VERIFY_BUTTON}
              disabled={!canVerifyVpsValue || isLoadingValue}
              onclick={async () => {
                error = '';
                try {
                  await m?.verifyVps();
                } catch (e) {
                  error = e instanceof Error ? e.message : String(e);
                }
              }}
            >{isLoadingValue ? t.common.loading : t.common.verify}</button>

            <label class="checkbox-row">
              <input
                type="checkbox"
                data-testid={IDS.VPS_CONFIG_MAIL_MODE_TOGGLE}
                checked={provisionMailModeEnabledValue}
                onchange={(e) =>
                  m?.setProvisionMailMode((e.currentTarget as HTMLInputElement).checked)}
              />
              {t.onboarding.vps_config.mail_mode_label}
            </label>
            <p class="help">{t.onboarding.vps_config.mail_mode_desc}</p>

            {#if vpsConfigValue && vpsConfigValue.server_types.length > 0}
              <fieldset class="server-type-picker">
                <legend>{t.onboarding.vps_config.server_type_radio_legend}</legend>
                {#each vpsConfigValue.server_types
                  .filter((st) => serverTypeAllowedForMail(JSON.stringify(st), provisionMailModeEnabledValue))
                  .slice(0, 5) as st, idx (st.id)}
                  <label>
                    <input
                      type="radio"
                      name="vps-server-type"
                      data-testid={`vps-server-type-radio[${idx}]`}
                      checked={vpsConfigValue.selected_server_type_id === st.id}
                      onchange={() => m?.selectVpsServerType(st.id)}
                    />
                    {serverTypeLabel(JSON.stringify(st))}
                  </label>
                {/each}
              </fieldset>
            {/if}
            </div>
          {/if}
        {/each}
      </div>

      <div class="button-group">
        <button
          class="btn link"
          data-testid={IDS.VPS_CONFIG_BACK_BUTTON}
          onclick={() => { error = ''; m?.clearError(); m?.back(); }}
        >{t.common.back}</button>

        <button
          class="btn primary large"
          data-testid={IDS.VPS_CONFIG_CONTINUE_BUTTON}
          disabled={!canContinueVpsValue || isLoadingValue}
          onclick={async () => {
            error = '';
            try {
              // Step transition only — the orchestrator spawns from the
              // nest_provisioning page's `provisioning-start-button`,
              // not here (target docs/goal/behavior/onboarding.md §6). Async
              // to mirror the UniFFI surface so native and web call
              // sites are identical.
              await m?.continueFromVps();
            } catch (e) {
              error = e instanceof Error ? e.message : String(e);
            }
          }}
        >{t.common.continue}</button>
        {#if vpsContinueBlockedReasonValue}
          <p class="muted">{vpsContinueBlockedReasonValue}</p>
        {/if}
      </div>
    </div>

  {:else if step === 'dns_post_instructions'}
    <div class="center">
      <h1>{t.onboarding.dns_post_instructions.title}</h1>
      <p>{t.onboarding.dns_post_instructions.description}</p>

      <pre data-testid={IDS.DNS_POST_INSTRUCTIONS_TEXT} class="dns-records">{dnsPostInstructionsValue ?? ''}</pre>

      <div class="button-group">
        <button
          class="btn"
          data-testid={IDS.DNS_POST_INSTRUCTIONS_COPY_BUTTON}
          onclick={() => navigator.clipboard.writeText(dnsPostInstructionsValue ?? '')}
        >{t.onboarding.dns_post_instructions.copy_button}</button>

        <button
          class="btn primary large"
          data-testid={IDS.DNS_POST_INSTRUCTIONS_CONTINUE_BUTTON}
          onclick={() => {
            if (!m) return;
            error = '';
            try {
              const stepName = m.continueFromDnsPostInstructions();
              handleWizardExit(stepName);
            } catch (e) {
              error = e instanceof Error ? e.message : String(e);
            }
          }}
        >{t.common.continue}</button>
      </div>
    </div>

  {:else if step === 'invite_request'}
    <!-- Invite-request stage.
         Two independent rows on one page:
           Top: Submit + status + Recheck (PendingReview only)
           Bottom: OOB code input + Check + status
         Single Continue is the OOB code's redeem (redeemInvite) and nothing
         else — disabled during PendingReview, which advances by polling
         (onboarding.md § 3, the button's row). -->
    <div class="center">
      <h1>{t.onboarding.invite.idle}</h1>

      {#if inviteRequestSnapshotValue}
        <!-- Top row: admin-flow -->
        <div class="invite-row">
          <button
            class="btn primary"
            data-testid={IDS.INVITE_REQUEST_SUBMIT_BUTTON}
            disabled={isLoadingValue
              || (typeof inviteRequestSnapshotValue.state === 'object'
                  && ('PendingReview' in inviteRequestSnapshotValue.state
                      || 'Approved' in inviteRequestSnapshotValue.state))}
            onclick={onInviteSubmit}
          >{t.onboarding.invite_request.submit}</button>

          <p data-testid={IDS.INVITE_REQUEST_STATUS} class="status-line">
            {Lookup(inviteRequestSnapshotValue.message)}
          </p>

          {#if inviteRequestSnapshotValue.recheck_visible}
            <button
              class="btn"
              data-testid={IDS.INVITE_REQUEST_RECHECK_BUTTON}
              onclick={onRecheckInvite}
            >{t.common.refresh}</button>
          {/if}
        </div>

        <!-- Bottom row: out-of-band code -->
        <div class="invite-row">
          <input
            type="text"
            class="input"
            data-testid={IDS.INVITE_CODE_INPUT}
            placeholder={t.onboarding.oob_code.idle}
            oninput={(e) => { oobCode = (e.currentTarget as HTMLInputElement).value; }}
            value={oobCode}
          />
          <button
            class="btn"
            data-testid={IDS.INVITE_CODE_CHECK_BUTTON}
            disabled={!oobCode.trim()}
            onclick={() => { error = ''; void m?.verifyOobInviteCode(oobCode); }}
          >{t.common.check}</button>
          <p data-testid={IDS.INVITE_CODE_STATUS} class="status-line">
            {Lookup(inviteRequestSnapshotValue.oob_message)}
          </p>
          {#if oobSupervisedBy}
            <p data-testid={IDS.INVITE_CODE_SUPERVISED_NOTICE} class="status-line">
              {t.family.supervised_notice_onboarding({ guardian: oobSupervisedBy })}
            </p>
          {/if}
        </div>
      {/if}

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <div class="button-group">
        <button
          data-testid={IDS.INVITE_REQUEST_BACK_BUTTON}
          class="btn link"
          onclick={() => { error = ''; m?.clearError(); m?.cancelInviteOp(); m?.back(); }}
        >{t.common.back}</button>

        <button
          class="btn primary large"
          data-testid={IDS.INVITE_REQUEST_CONTINUE_BUTTON}
          disabled={!inviteRequestSnapshotValue?.continue_enabled}
          onclick={onInviteContinue}
        >{t.common.continue}</button>
      </div>
    </div>

  {:else if step === 'claim_code'}
    <!-- Claim-code stage per docs/goal/behavior/onboarding.md §3a.
         Reached only when handle-check returned UnregisteredUnclaimedNest:
         the nest exists but no admin has claimed it yet. The user pastes
         the one-time code printed by the nest server's bootstrap and
         atomically becomes the admin via the `fauna.auth.claim_admin` WS-RPC kind.
         Submit is terminal — on success the wizard exits to LoggedIn. -->
    <div class="center">
      <h2>{t.onboarding.claim_code.title}</h2>
      <p class="subtitle">{t.onboarding.claim_code.description}</p>

      <div class="form-group">
        <input
          type="text"
          class="input"
          data-testid={IDS.CLAIM_CODE_INPUT}
          placeholder={t.onboarding.claim_code.placeholder}
          bind:value={claimCodeValue}
        />
      </div>

      <button
        class="btn primary large"
        data-testid={IDS.CLAIM_CODE_SUBMIT_BUTTON}
        disabled={!(claimCodeSnapshotValue?.submit_enabled ?? false)}
        onclick={onClaimCodeSubmit}
      >{t.onboarding.claim_code.submit_button}</button>

      <div data-testid={IDS.CLAIM_CODE_STATUS} class="status-line">
        {Lookup(claimCodeSnapshotValue?.message)}
      </div>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <button
        class="btn link"
        data-testid={IDS.CLAIM_CODE_BACK_BUTTON}
        onclick={onClaimCodeBack}
      >{t.common.back}</button>
    </div>

  {:else if step === 'nat_mode_choice'}
    <!-- NAT-mode choice for a freshly-claimed nest — the single, terminal
         admin-path setup step, per docs/goal/behavior/onboarding.md § 3b-bis.
         Rendered from natModeSnapshot(). selected_mode pre-selects the nest's
         seeded node_mode (refined private-ward when the target is a
         private-network address), so the common case is confirm-only — one
         click on nat-mode-confirm-button. "Decide later" is visible on every
         state and exits with the seed still in effect: it is a working
         default, so unlike the encryption defer there is no resume slot and
         no unresolved state. Both exits are LoggedIn, so handleWizardExit's
         existing LoggedIn arm carries the launch glue. No Back button (the
         admin is server-committed). -->
    <div class="center">
      <h1>{t.onboarding.nat_mode.title}</h1>
      <p class="subtitle">{t.onboarding.nat_mode.description}</p>

      <div class="enc-mode-picker">
        <label class="enc-mode-option">
          <input
            type="radio"
            name="nat-mode"
            data-testid={IDS.PUBLIC_NAT_MODE_RADIO}
            checked={natModeSnapshotValue?.selected_mode === 'public'}
            disabled={natModeSnapshotValue?.state === 'Submitting'}
            onchange={() => onSelectNatMode('public')}
          />
          <span class="enc-mode-text">
            <strong>{t.onboarding.nat_mode.public_label}</strong>
            <span class="enc-mode-desc">{t.onboarding.nat_mode.public_desc}</span>
          </span>
        </label>
        <label class="enc-mode-option">
          <input
            type="radio"
            name="nat-mode"
            data-testid={IDS.PRIVATE_NAT_MODE_RADIO}
            checked={natModeSnapshotValue?.selected_mode === 'private'}
            disabled={natModeSnapshotValue?.state === 'Submitting'}
            onchange={() => onSelectNatMode('private')}
          />
          <span class="enc-mode-text">
            <strong>{t.onboarding.nat_mode.private_label}</strong>
            <span class="enc-mode-desc">{t.onboarding.nat_mode.private_desc}</span>
          </span>
        </label>
      </div>

      <p data-testid={IDS.NAT_MODE_STATUS} class="status-line">
        {Lookup(natModeSnapshotValue?.message)}
      </p>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <div class="button-group">
        <button
          class="btn link"
          data-testid={IDS.NAT_MODE_DEFER_BUTTON}
          onclick={onNatModeDefer}
        >{t.onboarding.nat_mode.defer_button}</button>

        <button
          class="btn primary large"
          data-testid={IDS.NAT_MODE_CONFIRM_BUTTON}
          disabled={!(natModeSnapshotValue?.submit_enabled ?? false)}
          onclick={onNatModeConfirm}
        >{t.onboarding.nat_mode.confirm_button}</button>
      </div>
    </div>

  {:else if step === 'trust_prompt'}
    <!-- The one-tap "trust this box" offer, onboarding.md § 3b-ter — the one
         ratified survivor of the retired claim-time trust question
         (storage-modes.md § What replaced each piece of the axis).
         ui.yaml `onboarding.trust_prompt` elements: `trust-box-summary`,
         `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`).
         Deliberately no `page-heading` id — the approved set is exactly
         those three, same as tui/linux. No Back button: the admin is
         already server-committed by the time this shows; grant and skip are
         its only exits and both conclude the wizard identically.
         The screen asks only — minting needs an authenticated session and
         the nest's content-processor roster, neither of which the wizard
         holds, so the answer is latched (`grantDefaultTrust` →
         `takeTrustPromptGranted`) and the mint runs at the signed-in
         handoff (`mintDefaultTrustSet` above). -->
    <div class="center">
      <h1>{t.onboarding.trust_prompt.title}</h1>
      <p data-testid={IDS.TRUST_BOX_SUMMARY} class="subtitle">{t.onboarding.trust_prompt.summary}</p>

      {#if errorMessageValue}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}

      <div class="button-group">
        <button
          class="btn link"
          data-testid={IDS.TRUST_BOX_SKIP_BUTTON}
          onclick={onSkipTrustPrompt}
        >{t.onboarding.trust_prompt.skip_button}</button>

        <button
          class="btn primary large"
          data-testid={IDS.TRUST_BOX_GRANT_BUTTON}
          onclick={onGrantDefaultTrust}
        >{t.onboarding.trust_prompt.grant_button}</button>
      </div>
    </div>

  {:else if step === 'nest_provisioning'}
    <!-- Snapshot-driven nest_provisioning page per
         docs/goal/behavior/onboarding.md §6.
         The four step rows render straight from `provisioningSnapshot()` —
         the orchestrator owns every transition. Buy / Cancel / Retry /
         Continue visibility is gated on `overall`. -->
    <div class="center provisioning">
      <h1>{t.onboarding.nest_provisioning.title}</h1>
      <p class="muted">{t.onboarding.provision.time}</p>

      {#if billOfMaterialsValue.length > 0}
        <!-- Top-region price summary ("Bill of Materials", onboarding.md §6):
             a pre-commit recap of up to two priced line items, sourced from
             bill_of_materials(). Both prices were already shown/agreed
             earlier in the wizard (dns-tld-price-display, the vps_config
             server-type options) — this is a recap, not a new price source. -->
        <div data-testid={IDS.PROVISIONING_PRICE_BOM} class="price-bom">
          {#each billOfMaterialsValue as item (item.recurring ? 'vps' : 'domain')}
            <div
              data-testid={item.recurring ? 'provisioning-bom-vps-line' : 'provisioning-bom-domain-line'}
              class="price-bom-line"
            >
              {#if item.recurring}
                {t.onboarding.nest_provisioning.bom_line_recurring({
                  label: resolveLocalized(item.label),
                  price: formatPrice(BigInt(item.price_cents), item.currency),
                })}
              {:else if item.renewal_price_cents != null}
                <!-- The renewal price + term are disclosed here when the
                     registrar quoted one (onboarding.md § 6) — the recurring
                     cost the user is signing up for, said before the charge,
                     not after. Mirrors tui's wizard::nest_provisioning. -->
                {t.onboarding.nest_provisioning.bom_line_domain({
                  label: resolveLocalized(item.label),
                  price: formatPrice(BigInt(item.price_cents), item.currency),
                  renewal: formatPrice(BigInt(item.renewal_price_cents), item.currency),
                })}
              {:else}
                {t.onboarding.nest_provisioning.bom_line({
                  label: resolveLocalized(item.label),
                  price: formatPrice(BigInt(item.price_cents), item.currency),
                })}
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if provisioningSnapshotValue?.overall === 'Idle'}
        <!-- Top-region "Buy and set up" CTA (handle-check + progress design).
             Visible only at Idle; once startProvisioning() spawns, the
             observer flips overall to Running and the Cancel button
             takes over below. -->
        <div class="button-group">
          <button
            class="btn primary large"
            data-testid={IDS.PROVISIONING_START_BUTTON}
            onclick={onProvisioningStart}
          >{t.onboarding.nest_provisioning.start_button}</button>
        </div>
      {/if}

      {#if provisioningSnapshotValue}
        <div data-testid={IDS.PROVISIONING_PROGRESS} class="prov-rows">
          {#each provisioningSnapshotValue.steps as step (step.kind)}
            <div
              data-testid={IDS.PROVISIONING_STEP_ROW}
              class={`prov-row prov-row--${step.status.toLowerCase()}`}
            >
              <span data-testid={IDS.PROVISIONING_STEP_CHECKBOX} class="prov-checkbox" aria-hidden="true">
                {provisioningStatusGlyph(step.status)}
              </span>
              <span data-testid={IDS.PROVISIONING_STEP_LABEL} class="prov-label">
                {resolveLocalized(provisioningStepLabelRaw(step.kind))}
              </span>
              {#if step.shows_substep}
                <span data-testid={IDS.PROVISIONING_SUBSTEP} class="prov-substep">
                  {resolveLocalized(provisioningSubstepLabelRaw(step.substep, step.last_error ?? undefined))}{attemptSuffix(step)}
                </span>
              {/if}
              {#if step.shows_error}
                <span data-testid={IDS.PROVISIONING_STEP_ERROR} class="prov-error">
                  {step.last_error}
                </span>
              {/if}
            </div>
          {/each}
        </div>

        {#if provisioningSnapshotValue.started_at_ms != null}
          <p data-testid={IDS.PROVISIONING_ELAPSED} class="prov-elapsed muted">
            {formatElapsed(provisioningSnapshotValue, nowMs)}
          </p>
        {/if}

        <div class="button-group">
          <button
            class="btn link"
            data-testid={IDS.PROVISIONING_BACK_BUTTON}
            onclick={() => { error = ''; m?.clearError(); m?.back(); }}
          >{t.common.back}</button>
          {#if provisioningSnapshotValue.overall === 'Running'}
            <button
              class="btn link"
              data-testid={IDS.PROVISIONING_CANCEL_BUTTON}
              onclick={onProvisioningCancel}
            >{t.common.cancel}</button>
          {/if}
          {#if provisioningSnapshotValue.overall === 'Failed' || provisioningSnapshotValue.overall === 'Cancelled'}
            <!-- Retry resumes a stopped run. Visible for both Failed and
                 Cancelled: soft-cancel leaves overall == Cancelled with
                 resources intact, and retry_provisioning() resets to idle
                 and re-runs (idempotency skips completed steps), so it is
                 the user's resume path after cancelling. Without this a
                 cancelled run would strand the user with only Back. Per
                 docs/goal/behavior/onboarding.md §6. -->
            <button
              class="btn primary"
              data-testid={IDS.PROVISIONING_RETRY_BUTTON}
              onclick={onProvisioningRetry}
            >{t.common.retry}</button>
          {/if}
          <button
            class="btn primary large"
            data-testid={IDS.PROVISIONING_CONTINUE_BUTTON}
            disabled={provisioningSnapshotValue.overall !== 'Succeeded'}
            onclick={onProvisioningContinue}
          >{t.common.continue}</button>
          {#if provisioningContinueBlockedReasonValue}
            <p class="muted">{provisioningContinueBlockedReasonValue}</p>
          {/if}
        </div>

        {#if provisioningSnapshotValue.final_error}
          <div data-testid={IDS.ERROR_MESSAGE} class="error">{provisioningSnapshotValue.final_error}</div>
        {/if}
      {/if}

      {#if errorMessageValue && !provisioningSnapshotValue?.final_error}
        <div data-testid={IDS.ERROR_MESSAGE} class="error">{errorMessageValue}</div>
      {/if}
    </div>

  {:else if step === 'nest_recovery'}
    <!-- Box-recovery step 4: box-selection hub (box-recovery.md § Recovery UI
         (step 4)). One selectable row per custodied box; picking one enables
         the two re-provision methods (cloud / self-hosted). The box list is
         populated by the joined custody read (the $effect above,
         `resolveRecoveryBoxes`); each row shows the box's domain
         (or the short nest_actor_id when domainless) — the custodied seed
         itself never crosses into JS. -->
    <div class="center">
      <h1>{t.onboarding.recovery.title}</h1>
      <p class="subtitle">{t.onboarding.recovery.subtitle}</p>

      {#if recoveryBoxesValue.length === 0}
        <p data-testid={IDS.RECOVER_BOX_EMPTY_MESSAGE} class="muted">
          {t.onboarding.recovery.empty_message}
        </p>
      {:else}
        <div data-testid={IDS.RECOVER_BOX_LIST} class="recover-box-list">
          <span class="field-label">{t.onboarding.recovery.box_list_label}</span>
          {#each recoveryBoxesValue as boxId, i (boxId)}
            <button
              data-testid={`recover-box-item-${i}`}
              class="recover-box-item"
              class:selected={recoverySelectedNestIdValue === boxId}
              aria-pressed={recoverySelectedNestIdValue === boxId}
              onclick={() => selectRecoveryBox(boxId)}
            >
              <code class="recover-box-id">{recoveryBoxLabel(boxId)}</code>
              <span class="recover-box-hint muted">{t.onboarding.recovery.box_item_hint}</span>
            </button>
          {/each}
        </div>
      {/if}

      <div class="button-group">
        <button
          class="btn primary"
          data-testid={IDS.RECOVER_METHOD_CLOUD_BUTTON}
          disabled={!recoverySelectedNestIdValue}
          onclick={recoverViaCloud}
        >{t.onboarding.recovery.method_cloud}</button>
        <button
          class="btn"
          data-testid={IDS.RECOVER_METHOD_SELFHOSTED_BUTTON}
          disabled={!recoverySelectedNestIdValue}
          onclick={recoverViaSelfhosted}
        >{t.onboarding.recovery.method_selfhosted}</button>
      </div>

      <button
        class="btn link"
        data-testid={IDS.RECOVER_BACK_BUTTON}
        onclick={recoveryBack}
      >{t.common.back}</button>
    </div>

  {:else if step === 'recover_selfhosted_instructions'}
    <!-- Box-recovery step 4: self-hosted seed install (box-recovery.md §
         Recovery UI (step 4)). The installer command carries
         FAUNA_DEPLOYMENT_SEED so the rebuilt box re-presents the same
         nest_actor_id. The real command (carrying the selected box's custodied
         seed) is rendered by the shared recoverSelfhostedCommand() getter,
         resolved via a reachable-nest read; the pending placeholder shows until
         it resolves (and stays for a fresh client with no resolved nest URL —
         Task C2 leg 2). -->
    <div class="center">
      <h1>{t.onboarding.recovery.selfhosted_title}</h1>
      <p class="subtitle">{t.onboarding.recovery.selfhosted_desc}</p>

      <div class="recover-command-row">
        <code data-testid={IDS.RECOVER_SELFHOSTED_COMMAND} class="recover-command">{recoverSelfhostedCommandDisplay}</code>
        <button
          class="btn-copy"
          data-testid={IDS.RECOVER_SELFHOSTED_COPY_BUTTON}
          onclick={() => { void copyRecoverSelfhostedCommand(); }}
        >{recoverSelfhostedCopied ? t.common.copied : t.common.copy}</button>
      </div>

      <div class="button-group">
        <button
          class="btn"
          data-testid={IDS.RECOVER_RESTORE_CTA}
          onclick={recoveryRestore}
        >{t.onboarding.recovery.restore_cta}</button>
        <button
          class="btn primary large"
          data-testid={IDS.RECOVER_SELFHOSTED_CONTINUE_BUTTON}
          onclick={recoverySelfhostedContinue}
        >{t.onboarding.recovery.selfhosted_continue}</button>
      </div>
    </div>
  {/if}
</div>

<style>
  /* The provider buttons are a wrapping flex row (mirrors admin-dns's
     `.provider-row`). The selected provider's credentials sub-form renders
     INSIDE this same {#each} — a sibling of the class:selected button — because
     Svelte 5 won't re-schedule a step-block-level {#if} off the selection here
     (see the dns_config comment). Since the form is interleaved after the
     selected button in the DOM, `order: 1` + `flex-basis: 100%` float it onto
     its own full-width line AFTER all the buttons regardless of which provider
     is selected. */
  .provider-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .provider-detail {
    order: 1;
    flex-basis: 100%;
    width: 100%;
  }

  .onboarding {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 2rem;
  }

  .center {
    max-width: 480px;
    width: 100%;
    text-align: center;
  }

  h1 {
    font-size: 1.5rem;
    margin-bottom: 0.5rem;
  }

  .subtitle {
    color: var(--text-muted);
    margin-bottom: 2rem;
    line-height: 1.5;
  }

  .muted { color: var(--text-muted); }
  .sign-out-residue {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    margin-top: 1rem;
  }

  .button-group {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    margin-top: 1rem;
  }

  .btn {
    padding: 0.625rem 1.25rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    font-size: 0.875rem;
    transition: background 0.15s;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }
  .btn.primary:hover { background: var(--accent-hover); }
  .btn.large {
    padding: 0.875rem 1.5rem;
    font-size: 1rem;
  }
  .btn.link {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 0.875rem;
    margin-top: 1rem;
  }
  .btn.link:hover { color: var(--text); }

  .form-group {
    margin-bottom: 1rem;
    text-align: left;
  }

  .field-label {
    display: block;
    color: var(--text-muted);
    font-size: 0.875rem;
    margin-bottom: 0.375rem;
  }

  .input {
    width: 100%;
    padding: 0.625rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--text);
    font-family: monospace;
    font-size: 0.875rem;
  }

  .secret-display {
    margin: 1.5rem 0;
    text-align: left;
  }

  /* The recovery-kit QR — the same box the Settings kit display draws. */
  .identity-qr { display: block; width: 220px; height: 220px; margin: 0.5rem auto; border-radius: 4px; }

  .btn-copy {
    display: block;
    margin-top: 0.5rem;
    padding: 0.375rem 0.75rem;
    font-size: 0.8rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text-muted);
    cursor: pointer;
  }
  .btn-copy:hover { background: var(--bg-hover); }

  .secret-key {
    display: block;
    padding: 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: monospace;
    font-size: 0.875rem;
    word-break: break-all;
    line-height: 1.6;
    user-select: all;
  }

  /* Box-recovery step 4 (box-recovery.md § Recovery UI (step 4)) */
  .recover-box-list {
    margin: 1.5rem 0;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .recover-box-item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
    width: 100%;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    text-align: left;
  }
  .recover-box-item:hover { background: var(--bg-hover); }
  .recover-box-item.selected { border-color: var(--accent, var(--text)); }
  .recover-box-id {
    font-family: monospace;
    font-size: 0.875rem;
    word-break: break-all;
  }
  .recover-box-hint { font-size: 0.8rem; }
  .recover-command-row {
    margin: 1.5rem 0;
    text-align: left;
  }
  .recover-command {
    display: block;
    padding: 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: monospace;
    font-size: 0.875rem;
    word-break: break-all;
    line-height: 1.6;
    user-select: all;
  }

  .warning {
    color: var(--danger);
    font-size: 0.875rem;
    margin-bottom: 1.5rem;
    padding: 0.75rem;
    border: 1px solid var(--danger);
    border-radius: 8px;
    background: color-mix(in srgb, var(--danger) 10%, transparent);
  }

  /* nest_provisioning page: top-region price summary ("Bill of Materials") */
  .price-bom {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin: 1rem 0;
    text-align: left;
  }
  .price-bom-line {
    color: var(--text-muted);
    font-size: 0.9375rem;
  }

  /* nest_provisioning page (snapshot-driven 4-step rendering) */
  .provisioning .prov-rows {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    margin: 1.5rem 0;
    text-align: left;
  }
  .prov-row {
    display: grid;
    grid-template-columns: 2rem 6rem 1fr;
    grid-template-areas:
      "checkbox label substep"
      ".        .     error";
    column-gap: 0.75rem;
    row-gap: 0.25rem;
    align-items: baseline;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-surface);
  }
  .prov-row--running { border-color: var(--accent); }
  .prov-row--succeeded { color: var(--text-muted); }
  .prov-row--failed { border-color: var(--danger); }
  .prov-checkbox { grid-area: checkbox; font-size: 1.125rem; }
  .prov-label    { grid-area: label; font-weight: 500; }
  .prov-substep  { grid-area: substep; color: var(--text-muted); font-size: 0.875rem; }
  .prov-error    { grid-area: error; color: var(--danger); font-size: 0.8125rem; word-break: break-word; }
  .prov-elapsed  { font-variant-numeric: tabular-nums; margin-top: 0.5rem; }

  /* nat_mode_choice page (NAT-mode picker, target § 3b-bis) */
  .enc-mode-picker {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    margin: 1.5rem 0;
    text-align: left;
  }
  .enc-mode-option {
    display: flex;
    gap: 0.625rem;
    align-items: flex-start;
    cursor: pointer;
  }
  .enc-mode-text { display: flex; flex-direction: column; gap: 0.25rem; }
  .enc-mode-desc { color: var(--text-muted); font-size: 0.875rem; }

</style>
