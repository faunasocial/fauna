<script lang="ts">
  // The profile Tiers tab's §4 payment-provider section (`monetization.md`
  // § Pillar 3), lifted out of `routes/profile/[[actorId]]/+page.svelte` for the
  // web `payments` excision leg.
  //
  // ⚠ THE LOGIC HAD TO COME WITH THE MARKUP, and that is the whole point. Moving
  // only the template would have left `paymentsProvidersList` / `…Set` /
  // `…Remove` / `paymentsKnownKinds` / `paymentsWebhookUrl` named in the profile
  // route's own chunk, which ships in every flavor — so the store-safe bundle
  // would still carry those face names even with the render folded away. web's
  // criterion-2 axis IS those names (`just web-store-safe-check`), so the seam
  // has to move to where the compile condition can remove it
  // (`dynamic-features.md` § Platform-family surface excision — the
  // isolated-module pattern).
  //
  // The section therefore owns its own read: the page bumps `reloadTick`
  // wherever it used to call `loadSelf()`, which is what preserves the existing
  // refresh triggers (mount, actor change, and the Tiers-tab re-click).
  import { t } from '$lib/i18n/strings';
  import { storedNestUrl } from '$lib/api';
  import {
    paymentsProvidersSet,
    paymentsProvidersList,
    paymentsProvidersRemove,
    paymentsKnownKinds,
    paymentsWebhookUrl,
    providerStatusLabel,
    type PaymentProvider,
  } from '$lib/payments';
  import { PAYMENTS_IDS } from '$lib/components/payments/generatedIds';

  interface Props {
    /** The logged-in actor's secret, or null before identity resolves. */
    secretHex: string | null;
    /** The author's own §1 tiers — a provider maps to exactly one of them. */
    tiers: { name: string }[];
    /** The author's hex actor id, for the webhook-URL preview. */
    actorIdHex: string;
    /** Bumped by the page wherever it reloads the SELF surfaces; every change
     *  re-reads the provider list. */
    reloadTick: number;
    /** Surface a failure on the page's shared `error-message` element
     *  (convention 2 — the page owns the one error surface). */
    onerror: (message: string) => void;
  }
  let { secretHex, tiers, actorIdHex, reloadTick, onerror }: Props = $props();

  // The webhook secret is never pre-filled — the nest deliberately omits it from
  // the list reply, so a re-save re-enters it.
  let providers = $state<PaymentProvider[]>([]);
  let formVisible = $state(false);
  let providerKind = $state('');
  let providerSecret = $state('');
  let providerTier = $state('');

  $effect(() => {
    const s = secretHex;
    void reloadTick; // re-read on every page-level reload
    if (!s) return;
    void load(s);
  });

  async function load(s: string): Promise<void> {
    try {
      providers = await paymentsProvidersList(s);
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }

  // The exact URL to register at the provider's dashboard for the currently
  // selected kind — derived client-side from the shared `fauna-payments`
  // constant the nest registers its ingress route from, so it previews live as
  // the kind select changes, before the provider is even saved (no nest
  // round-trip). Guarded on the form being open: it reads wasm, which is loaded
  // by then, and an unread `$derived` is never evaluated.
  const webhookUrlPreview = $derived(
    // `storedNestUrl()`, not `nodeUrl()`: this string is copied into a third
    // party's dashboard, so it must be the nest's real URL — the dial seam
    // redirects the socket, never the truth (`fauna_launch_machine::dial`).
    formVisible && providerKind ? paymentsWebhookUrl(storedNestUrl(), actorIdHex, providerKind) : '',
  );

  function openForm(): void {
    // The secret is never pre-filled (the nest doesn't echo it back).
    providerSecret = '';
    providerKind = paymentsKnownKinds()[0] ?? '';
    if (!tiers.some((tr) => tr.name === providerTier)) providerTier = tiers[0]?.name ?? '';
    onerror('');
    formVisible = true;
  }

  // No client-side validation — the nest rejects unknown kinds / dangling tiers
  // / empty secrets with its own typed errors.
  async function save(): Promise<void> {
    const s = secretHex;
    if (!s || !providerTier) return;
    try {
      await paymentsProvidersSet(s, providerKind, providerSecret, providerTier);
      // The secret is a credential — don't leave it in the field post-save.
      providerSecret = '';
      formVisible = false;
      onerror('');
      await load(s);
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }

  async function remove(kind: string): Promise<void> {
    const s = secretHex;
    if (!s) return;
    try {
      await paymentsProvidersRemove(s, kind);
      await load(s);
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }

  // Best-effort: the clipboard API is permission-gated and absent on insecure
  // origins. A copy failure must not surface as a page error — the URL is
  // selectable in the field regardless.
  async function copyWebhookUrl(): Promise<void> {
    try {
      await navigator.clipboard?.writeText(webhookUrlPreview);
    } catch {
      /* ignore — the field still shows the URL */
    }
  }
</script>

<!-- Status renders the shared evidence-based provider_status_label
     (configured/verified/error). -->
<section class="section" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_SECTION}>
  <h2>{t.subscriptions.payment_providers}</h2>
  <button
    class="btn primary" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_ADD_BUTTON}
    onclick={openForm}
  >{t.subscriptions.add_provider}</button>

  {#if formVisible}
    <div class="form" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM}>
      <div class="select-row">
        <label for="provider-kind-select">{t.subscriptions.provider_kind_label}</label>
        <select
          id="provider-kind-select" class="input"
          data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_KIND} bind:value={providerKind}
        >
          {#each paymentsKnownKinds() as kind (kind)}
            <option value={kind}>{kind}</option>
          {/each}
        </select>
      </div>
      <input
        class="input" type="password" placeholder={t.subscriptions.webhook_secret}
        data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_SECRET} bind:value={providerSecret}
      />
      <div class="select-row">
        <label for="provider-tier-select">{t.subscriptions.provider_tier_label}</label>
        <select
          id="provider-tier-select" class="input"
          data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_TIER_MAP} bind:value={providerTier}
        >
          {#each tiers as tier (tier.name)}
            <option value={tier.name}>{tier.name}</option>
          {/each}
        </select>
      </div>
      <!-- The exact URL to register at the provider's dashboard, live as soon
           as the form opens and recomputed as the kind select changes — so the
           creator never hand-assembles it. Read-only: the nest owns the route. -->
      <div class="select-row">
        <label for="provider-webhook-url">{t.subscriptions.webhook_url_label}</label>
        <input
          id="provider-webhook-url" class="input mono" type="text" readonly
          data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL}
          value={webhookUrlPreview}
        />
        <button
          class="btn" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL_COPY_BUTTON}
          onclick={copyWebhookUrl}
        >{t.common.copy}</button>
      </div>
      <div class="form-buttons">
        <button
          class="btn" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_CANCEL}
          onclick={() => (formVisible = false)}
        >{t.subscriptions.cancel}</button>
        <button
          class="btn primary" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_FORM_SAVE}
          onclick={save}
        >{t.subscriptions.save}</button>
      </div>
    </div>
  {/if}

  <div class="list" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_LIST}>
    {#each providers as provider (provider.kind)}
      <div class="row" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_ROW}>
        <span class="grow" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_KIND}>{provider.kind}</span>
        <span>{provider.tier}</span>
        <span class="badge" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_STATUS}
          >{providerStatusLabel(provider.last_verified_at, provider.last_rejected_at)}</span
        >
        <button
          class="btn danger" data-testid={PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_REMOVE_BUTTON}
          onclick={() => remove(provider.kind)}
        >{t.subscriptions.remove}</button>
      </div>
    {/each}
    {#if providers.length === 0}
      <p class="muted">{t.subscriptions.no_providers}</p>
    {/if}
  </div>
</section>

<style>
  /* Scoped copies of the profile page's own section chrome — the page can no
     longer style this markup once it lives here, and a global stylesheet would
     leak these generic names app-wide. Same duplication every page in this SPA
     already carries. */
  .section {
    margin: 1.25rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .section h2 { font-size: 1rem; margin: 0 0 0.75rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .mono {
    font-family: monospace;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grow { flex: 1; }
  .badge {
    font-size: 0.75rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-hover, #21262d);
    color: var(--text-muted, #8b949e);
  }
  .list { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0.75rem 0;
    padding: 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .form-buttons { display: flex; justify-content: flex-end; gap: 0.5rem; }
  .select-row { display: flex; align-items: center; gap: 0.5rem; }
  .input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }
  .btn {
    padding: 0.35rem 0.7rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn:hover { background: var(--bg-hover, #21262d); }
  .btn:disabled { opacity: 0.6; cursor: default; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
</style>
