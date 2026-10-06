<script lang="ts">
  import { identity } from '$lib/store';
  import {
    adminTiersList, adminTiersUpdate, type AdminTier,
    adminMembershipTiersList, adminMembershipTiersSet, adminMembershipTiersClear,
    type AdminMembershipTier,
    subscriptionsTiersList,
  } from '$lib/rpc';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { byteSize } from '$lib/value-format';
  import { parseCap } from '$lib/wasm';
  import { IDS } from '$lib/generated/uiIds';

  // admin-settings — renamed "Tiers" (admin.md § Admin IA redesign, 2026-06-04).
  // After the per-page-services redesign this page holds only tier *definitions*
  // (policy); the read-only storage-mode indicator and the Factory Reset danger
  // zone moved to the new admin-nest page (admin/nest/+page.svelte). Invite-code
  // minting moved to the admin-users hub's Invite section (admin.md § 3,
  // 2026-05-29); domain management moved to admin-dns (2026-05-25). The element
  // IDs keep their historical `admin-settings-` prefix. All over `fauna.admin.*`
  // WS-RPC.

  // Tiers state
  let tiers = $state<AdminTier[]>([]);
  let tiersLoading = $state(true);
  let error = $state('');

  // Per-row editable cap drafts, keyed by tier name and seeded from the
  // persisted caps (raw-i64 text — matching ui.yaml's admin-settings-tier-cap-*
  // text inputs + linux's in-place tier-cap editing). Saving parses them back
  // to numbers for `fauna.admin.tiers.update`, then refetches so the row
  // re-renders from persisted state.
  type TierDraft = { inbox: string; storage: string; devices: string; blobSize: string; feeds: string };
  let drafts = $state<Record<string, TierDraft>>({});
  let savingTier = $state<string | null>(null);

  // The caught value as the `{message}` every admin page-error string carries
  // (ui/README.md § Copy comprehensibility — the banner must name the gesture AND
  // the reason; rendering the bare exception instead of the string dropped the first).
  function detail(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function seedDrafts() {
    const next: Record<string, TierDraft> = {};
    for (const tier of tiers) {
      next[tier.name] = {
        inbox: String(tier.max_inbox_bytes),
        storage: String(tier.max_storage_bytes),
        devices: String(tier.max_devices),
        blobSize: String(tier.max_blob_size),
        feeds: String(tier.max_feeds),
      };
    }
    drafts = next;
  }

  // Membership designations (monetization.md § Pillar 4) — a link editor over
  // the admin's own subscription tiers, never a third tier list. The row set
  // is the admin's own subscription tier names (`subscriptionsTiersList`); each
  // row's persisted designation (if any) comes from `adminMembershipTiersList`.
  // Designating creates neither kind of tier; an empty row set is the normal
  // out-of-the-box state (no subscription tiers minted yet), not an error.
  let ownMembershipTierNames = $state<string[]>([]);
  let membershipTiers = $state<AdminMembershipTier[]>([]);
  let membershipLoading = $state(true);
  // Whether the first load ever landed — once true, a post-save/-clear
  // refetch keeps the stale rows visible until the fresh data replaces them
  // (linux's atomic-fold shape), rather than blanking to the loading state on
  // every reload (which would flash the section empty mid-save).
  let membershipLoaded = $state(false);
  type MembershipDraft = { tierName: string; adminTier: string; lapseTier: string };
  let membershipDrafts = $state<Record<string, MembershipDraft>>({});
  let savingMembership = $state<string | null>(null);
  let clearingMembership = $state<string | null>(null);

  // Re-seed every row's drafts from the two cached sources — called from both
  // `loadTiers` (the admit/lapse option catalog) and `loadMembership` (the row
  // set + designations), whichever lands second producing the final correct
  // render (linux's `update_membership_tiers` race-tolerance shape).
  function seedMembershipDrafts() {
    const next: Record<string, MembershipDraft> = {};
    for (const name of ownMembershipTierNames) {
      const existing = membershipTiers.find((m) => m.tier_name === name);
      next[name] = {
        tierName: name,
        adminTier: existing?.admin_tier ?? (tiers[0]?.name ?? ''),
        lapseTier: existing?.lapse_tier ?? 'free',
      };
    }
    membershipDrafts = next;
  }

  onMount(() => {
    loadTiers();
    loadMembership();
  });

  async function loadTiers() {
    const id = $identity;
    if (!id?.secretHex) return;
    tiersLoading = true;
    try {
      tiers = await adminTiersList(id.secretHex);
      seedDrafts();
      seedMembershipDrafts();
    } catch (e) {
      error = t.admin.settings_page.load_tiers_error({ message: detail(e) });
    } finally {
      tiersLoading = false;
    }
  }

  async function loadMembership() {
    const id = $identity;
    if (!id?.secretHex) return;
    if (!membershipLoaded) membershipLoading = true;
    try {
      const [names, designations] = await Promise.all([
        subscriptionsTiersList(id.secretHex).then((list) => list.map((tier) => tier.name)),
        adminMembershipTiersList(id.secretHex),
      ]);
      ownMembershipTierNames = names;
      membershipTiers = designations;
      seedMembershipDrafts();
      membershipLoaded = true;
    } catch (e) {
      error = t.admin.settings_page.load_membership_tiers_error({ message: detail(e) });
    } finally {
      membershipLoading = false;
    }
  }

  // Designate/re-point a row via `fauna.admin.membership_tiers.set` (an
  // upsert), then refetch so the row re-renders from persisted state (the
  // `saveTier` shape). The lapse-tier draft is always a definite selection
  // (seeded to the shared default), so it always rides explicit — never
  // relying on the wire's omit-means-default.
  async function saveMembership(name: string) {
    const id = $identity;
    const d = membershipDrafts[name];
    if (!id?.secretHex || !d || !d.adminTier) return;
    savingMembership = name;
    error = '';
    try {
      await adminMembershipTiersSet(id.secretHex, d.tierName, d.adminTier, d.lapseTier);
      await loadMembership();
    } catch (e) {
      error = t.admin.settings_page.save_membership_tier_error({ message: detail(e) });
    } finally {
      savingMembership = null;
    }
  }

  // Drop a row's designation via `fauna.admin.membership_tiers.clear`; the
  // subscription tier itself survives, reverting to undesignated.
  async function clearMembership(name: string) {
    const id = $identity;
    if (!id?.secretHex) return;
    clearingMembership = name;
    error = '';
    try {
      await adminMembershipTiersClear(id.secretHex, name);
      await loadMembership();
    } catch (e) {
      error = t.admin.settings_page.clear_membership_tier_error({ message: detail(e) });
    } finally {
      clearingMembership = null;
    }
  }

  // Persist one tier's edited caps via `fauna.admin.tiers.update`, then refetch
  // so the row re-renders (and re-seeds its draft) from the persisted values —
  // proving the write landed in the nest, not merely echoed in the widget.
  async function saveTier(name: string) {
    const id = $identity;
    const d = drafts[name];
    const tier = tiers.find((tier) => tier.name === name);
    if (!id?.secretHex || !d || !tier) return;
    savingTier = name;
    error = '';
    try {
      // Each cap parses through the shared `parse_cap` (wasm), falling back to
      // the persisted value on a blank/unparseable edit — the no-silent-zeroing
      // contract (value-formatting.md § Tier cap validation), unlike the prior
      // `Number('')`→0 / `Number('abc')`→NaN.
      await adminTiersUpdate(
        id.secretHex,
        name,
        parseCap(d.inbox) ?? tier.max_inbox_bytes,
        parseCap(d.storage) ?? tier.max_storage_bytes,
        parseCap(d.devices) ?? tier.max_devices,
        parseCap(d.blobSize) ?? tier.max_blob_size,
        parseCap(d.feeds) ?? tier.max_feeds,
      );
      await loadTiers();
    } catch (e) {
      error = t.admin.settings_page.save_tier_error({ message: detail(e) });
    } finally {
      savingTier = null;
    }
  }
</script>

<h1 data-testid={IDS.ADMIN_SETTINGS_HEADING}>{t.admin.settings_page.title}</h1>

<MessageBanner bind:error />

<!-- Tiers (definitions — policy, distinct from admission on admin-users) -->
<section class="section">
  <h2>{t.admin.settings_page.tiers}</h2>
  {#if tiersLoading}
    <p class="muted">{t.admin.settings_page.loading_tiers}</p>
  {:else if tiers.length === 0}
    <p class="muted">{t.admin.settings_page.no_tiers}</p>
  {:else}
    <div class="tier-list" data-testid={IDS.ADMIN_SETTINGS_TIERS_SECTION}>
      <div class="tier-grid">
        {#each tiers as tier (tier.name)}
          <div class="tier-item" data-testid={IDS.ADMIN_SETTINGS_TIER_ITEM}>
            <span class="tier-name">{tier.name}</span>
            {#if drafts[tier.name]}
              <label class="cap">
                <span class="cap-label">{t.admin.settings_page.cap_inbox_bytes} ({byteSize(tier.max_inbox_bytes)})</span>
                <input
                  class="cap-input" type="text" inputmode="numeric"
                  data-testid={IDS.ADMIN_SETTINGS_TIER_CAP_INBOX}
                  bind:value={drafts[tier.name].inbox}
                />
              </label>
              <label class="cap">
                <span class="cap-label">{t.admin.settings_page.cap_storage_bytes} ({byteSize(tier.max_storage_bytes)})</span>
                <input
                  class="cap-input" type="text" inputmode="numeric"
                  data-testid={IDS.ADMIN_SETTINGS_TIER_CAP_STORAGE}
                  bind:value={drafts[tier.name].storage}
                />
              </label>
              <label class="cap">
                <span class="cap-label">{t.admin.settings_page.cap_devices}</span>
                <input
                  class="cap-input" type="text" inputmode="numeric"
                  data-testid={IDS.ADMIN_SETTINGS_TIER_CAP_DEVICES}
                  bind:value={drafts[tier.name].devices}
                />
              </label>
              <label class="cap">
                <span class="cap-label">{t.admin.settings_page.cap_blob_size} ({byteSize(tier.max_blob_size)})</span>
                <input
                  class="cap-input" type="text" inputmode="numeric"
                  data-testid={IDS.ADMIN_SETTINGS_TIER_CAP_BLOB_SIZE}
                  bind:value={drafts[tier.name].blobSize}
                />
              </label>
              <label class="cap">
                <span class="cap-label">{t.admin.settings_page.cap_feeds}</span>
                <input
                  class="cap-input" type="text" inputmode="numeric"
                  data-testid={IDS.ADMIN_SETTINGS_TIER_CAP_FEEDS}
                  bind:value={drafts[tier.name].feeds}
                />
              </label>
              <button
                class="tier-save-btn"
                data-testid={IDS.ADMIN_SETTINGS_TIER_SAVE_BUTTON}
                onclick={() => saveTier(tier.name)}
                disabled={savingTier === tier.name}
              >{savingTier === tier.name ? t.common.loading : t.common.save}</button>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
</section>

<!-- Membership designations (monetization.md § Pillar 4): a link editor over
     the admin's own subscription tiers, never a third tier list. -->
<section class="section" data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_SECTION}>
  <h2>{t.admin.settings_page.membership_section}</h2>
  {#if membershipLoading}
    <p class="muted">{t.admin.settings_page.loading_membership}</p>
  {:else if ownMembershipTierNames.length === 0}
    <p class="muted">{t.admin.settings_page.no_membership_tiers}</p>
  {:else}
    <div class="tier-grid">
      {#each ownMembershipTierNames as name (name)}
        {@const draft = membershipDrafts[name]}
        {@const hasExisting = membershipTiers.some((m) => m.tier_name === name)}
        <div class="tier-item" data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_ITEM}>
          {#if draft}
            <select
              class="cap-input"
              data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_TIER_SELECT}
              bind:value={draft.tierName}
            >
              {#each ownMembershipTierNames as opt (opt)}
                <option value={opt}>{opt}</option>
              {/each}
            </select>
            <label class="cap">
              <span class="cap-label">{t.admin.settings_page.membership_admits_at}</span>
              <select
                class="cap-input"
                data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_ADMIN_TIER_SELECT}
                bind:value={draft.adminTier}
              >
                {#each tiers as tier (tier.name)}
                  <option value={tier.name}>{tier.name}</option>
                {/each}
              </select>
            </label>
            <label class="cap">
              <span class="cap-label">{t.admin.settings_page.membership_lapses_to}</span>
              <select
                class="cap-input"
                data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_LAPSE_TIER_SELECT}
                bind:value={draft.lapseTier}
              >
                {#each tiers as tier (tier.name)}
                  <option value={tier.name}>{tier.name}</option>
                {/each}
              </select>
            </label>
            <button
              class="tier-save-btn"
              data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_SAVE_BUTTON}
              onclick={() => saveMembership(name)}
              disabled={savingMembership === name}
            >{savingMembership === name ? t.common.loading : t.admin.settings_page.membership_save}</button>
            <button
              class="tier-save-btn"
              data-testid={IDS.ADMIN_SETTINGS_MEMBERSHIP_CLEAR_BUTTON}
              onclick={() => clearMembership(name)}
              disabled={!hasExisting || clearingMembership === name}
            >{clearingMembership === name ? t.common.loading : t.admin.settings_page.membership_clear}</button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  h1 { margin-bottom: 1rem; font-size: 1.5rem; }
  .section {
    margin-bottom: 2rem;
  }
  .section:last-child {
    margin-bottom: 0;
  }
  .muted { color: var(--text-muted, #8b949e); }
  .tier-item {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .tier-name {
    font-weight: 600;
  }
  .tier-grid {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .cap {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .cap-label {
    color: var(--text-muted);
    font-size: 0.8rem;
  }
  .cap-input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    font-family: monospace;
    font-size: 0.875rem;
  }
  .tier-save-btn {
    align-self: flex-start;
    padding: 0.375rem 0.75rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
  }
  .tier-save-btn:hover { background: var(--bg-hover); }
  .tier-save-btn:disabled { opacity: 0.6; cursor: default; }
</style>
