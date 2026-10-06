<script lang="ts">
  // Consumer-side "Subscriptions" settings section (subscriptions Slice B) —
  // monetization.md § Pillar 1 / ui.yaml `subscription-settings` page. Shows
  // THIS user's own subscriptions across every creator with per-row unsubscribe.
  // Distinct from the profile Tiers-tab SELF author management (what the user
  // *offers*): this is what the user *consumes*.
  //
  // A dumb renderer over the shared `fauna-client-subscriptions` via the wasm
  // `WsRpcClient.subscriptionsMineList` / `subscriptionsUnsubscribe` (rpc.ts
  // delegates → `SubscriptionsClient::mine_list` / `unsubscribe`; priority #2).
  // Observer-free (manual re-read): reloads on mount and after unsubscribe.
  // Unsubscribe in encrypted mode returns `Queued` (the row stays until the
  // author commits the removal), so the row does NOT vanish on click — the
  // re-read reflects the nest's state. Lifts the linux lead
  // (apps/fauna-linux/src/settings/subscriptions.rs). Reuses the settings page's
  // single `error-message` surface via the bindable `error` (no duplicate IDs).
  import { identity } from '$lib/store';
  import {
    subscriptionsMineList,
    subscriptionsUnsubscribe,
    type SubscriptionMine,
  } from '$lib/rpc';
  import { ensureWasm } from '$lib/wasm';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import ClaimRedeem from './payments/ClaimRedeem.svelte';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; our errors flow in.
  let { error = $bindable('') } = $props();

  let subscriptions = $state<SubscriptionMine[]>([]);

  // (Claim redemption moved to `payments/ClaimRedeem.svelte` — the money gesture
  // excises with the web `payments` flavor while this Pillar-1 list stays in
  // every build.)

  function secret(): string | null {
    return $identity?.secretHex ?? null;
  }

  async function loadMine(): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      subscriptions = await subscriptionsMineList(s);
      error = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function unsubscribe(authorIdHex: string): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      await subscriptionsUnsubscribe(s, authorIdHex);
      await loadMine();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(async () => {
    try {
      await ensureWasm();
      await loadMine();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });
</script>

<section class="section" data-testid={IDS.SUBSCRIPTION_MINE_SECTION}>
  <h2>{t.subscriptions.my_subscriptions}</h2>
  <div class="list">
    {#each subscriptions as sub (sub.author_id + ':' + sub.tier)}
      <div class="row" data-testid={IDS.SUBSCRIPTION_MINE_ROW}>
        <span class="grow author" data-testid={IDS.SUBSCRIPTION_MINE_AUTHOR}>{sub.author_display}</span>
        <span data-testid={IDS.SUBSCRIPTION_MINE_TIER}>{sub.tier}</span>
        <span class="status" data-testid={IDS.SUBSCRIPTION_MINE_STATUS}>{sub.status}</span>
        <button
          class="btn danger"
          data-testid={IDS.SUBSCRIPTION_MINE_UNSUBSCRIBE_BUTTON}
          onclick={() => unsubscribe(sub.author_id)}
        >{t.subscriptions.unsubscribe}</button>
      </div>
    {/each}
    {#if subscriptions.length === 0}
      <p class="muted">{t.subscriptions.no_subscriptions}</p>
    {/if}
  </div>

  <!-- The claim-redeem input, behind the web family's `payments` compile
       condition (`dynamic-features.md` § Platform-family surface excision — a
       gated plane's user-facing INPUTS excise with it). The list above is
       Pillar 1 and stays. -->
  {#if __FAUNA_PAYMENTS__}
    <ClaimRedeem
      secretHex={$identity?.secretHex ?? null}
      onredeemed={loadMine}
      onerror={(m) => (error = m)}
    />
  {/if}
</section>

<style>
  .section {
    margin: 1.25rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .section h2 { font-size: 1rem; margin: 0 0 0.75rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .grow { flex: 1; }
  /* The author column carries the full hex actor id as its text (the e2e reads it
     exactly); truncate it visually only, never in the text content. */
  .author {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: monospace;
    font-size: 0.85rem;
  }
  .status {
    font-size: 0.75rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-hover, #21262d);
    color: var(--text-muted, #8b949e);
  }
  .list { display: flex; flex-direction: column; gap: 0.5rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
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
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
  /* `.claim-title`, `.input` and `.btn.primary` moved with the redeem row into
     `payments/ClaimRedeem.svelte` — the redeem button was this section's only
     primary one, so leaving the rule here is a `css_unused_selector` warning. */
</style>
