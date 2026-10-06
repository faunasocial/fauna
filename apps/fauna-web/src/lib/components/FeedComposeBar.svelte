<script lang="ts">
  import MarkdownToolbar from './MarkdownToolbar.svelte';
  import { t } from '$lib/i18n/strings';
  import { byteSize } from '$lib/value-format';
  import { resolveGateSelection } from '$lib/compose-gate';
  import { IDS } from '$lib/generated/uiIds';
  // compose-sell-asking-price moved into the gated payments id table
  // (dynamic-features.md § A gated plane's user-facing INPUTS excise with
  // it; ui.yaml's gated_features.payments.id_prefixes, added 2026-09-06).
  // The render lives in its own `payments/` component, imported only behind
  // `__FAUNA_PAYMENTS__` below, matching ProviderSection/ClaimSection's own
  // isolation (dynamic-features.md § Platform-family surface excision).
  import AskingPriceInput from '$lib/components/payments/AskingPriceInput.svelte';

  interface Props {
    composeBody: string;
    composeTags: string;
    composing: boolean;
    // The feed manager owns `submitPost`; until it has been built (the Feed page
    // sets this once `getFeedManager()` resolves, ~1s after the identity is known)
    // the submit handler silently no-ops. Gate the button on it so an enabled
    // submit can never drop the user's post — matching the create-feed /
    // bridge-subscribe "disabled until it can act" pattern, and letting the e2e
    // click auto-wait for readiness instead of racing the build.
    composeReady?: boolean;
    composeError: string;
    // The manager's own `attached_file` (name + size, never bytes) — a
    // restored draft's handle included, not only a fresh local pick
    // (feed.md § Persistence → *Attachments by content address*). Drives
    // `compose-file-ready` / `compose-file-remove` together.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    attachedFile: any;
    onsubmit: () => void;
    onbodychange: (v: string) => void;
    ontagschange: (v: string) => void;
    onfilechange: (file: File | null, data: Uint8Array | null) => void;
    onremovefile: () => void;
    ondialogopen?: () => void;
    // Gate-to-tier (feed.md § Encryption at rest; monetization.md § Pillars 2+3):
    // the author's own tiers (from the manager snapshot's own_tiers), the current
    // gate selection ('' = Public, else a tier name — never the sell sentinel,
    // see sellSelected below), and its public teaser — the parent owns the
    // state, this bar renders compose-gate-tier-select / compose-gate-preview-field.
    ownTiers?: { name: string; rank: number }[];
    gateTier?: string;
    gatePreview?: string;
    ongatetierchange?: (v: string) => void;
    ongatepreviewchange?: (v: string) => void;
    // Room-restricted (feed.md § Encryption at rest → *Room-restricted — the app
    // half*): the rooms the author can address a post to (snapshot own_rooms,
    // `{ room: hex channel id, label }`), rendered "Room: <label>" between the
    // tiers and Sell, and the selected room's id ('' = none). Resolved
    // index-wise exactly like the tier and sell answers.
    ownRooms?: { room: string; label: string }[];
    gateRoom?: string;
    ongateroomchange?: (v: string) => void;
    // "Sell this post…" (monetization.md § Per-post pay-to-unlock; IDs
    // user-approved 2026-07-29) — the select's third answer, sharing
    // gatePreview as its teaser. `sellSelected` is derived from the select's
    // *index*, not its value/text — `tiers.create` has no reserved-name check,
    // so an author-named tier can share the localized "Sell this post…" label
    // comparing by value would let that tier
    // silently hijack the sell branch. Mirrors linux's index-derived
    // `SellComposeState` (`client.rs` submit-click handler): "which entry" is
    // a position, never a string compare.
    sellSelected?: boolean;
    onsellselectedchange?: (v: boolean) => void;
    sellPrice?: string;
    // The machine-comparable price (monetization.md § The asking price) —
    // independent of sellPrice above (the free-text hint); no parsing ever
    // infers one from the other. Empty means no machine price: the minted
    // tier stays a tip target forever.
    sellAskingPrice?: string;
    sellSubscribersFree?: boolean;
    onsellpricechange?: (v: string) => void;
    onsellaskingpricechange?: (v: string) => void;
    onsellsubscribersfreechange?: (v: boolean) => void;
  }

  let {
    composeBody, composeTags, composing, composeReady = true, composeError,
    attachedFile,
    onsubmit, onbodychange, ontagschange, onfilechange, onremovefile,
    ondialogopen,
    ownTiers = [], gateTier = '', gatePreview = '',
    ongatetierchange, ongatepreviewchange,
    ownRooms = [], gateRoom = '', ongateroomchange,
    sellSelected = false, onsellselectedchange,
    sellPrice = '', sellAskingPrice = '', sellSubscribersFree = true,
    onsellpricechange, onsellaskingpricechange, onsellsubscribersfreechange,
  }: Props = $props();

  // The select's third answer, rendered with this label — used ONLY for the
  // controlled `<select>`'s display value below, never compared against to
  // decide the sell branch (see sellSelected's doc comment on Props).
  const SELL_VALUE = t.feed.post.gate_sell;
  // Public's `value` is its label too, like every other option below: the
  // cross-app driver selects and reads the picker by its painted text, and an
  // empty value made Public the one answer web could neither be read as nor
  // be switched back to.
  const PUBLIC_VALUE = t.feed.post.gate_public;

  // A room option's label — also its `value`, because the cross-app driver
  // selects by the painted option text (the web bridge's `select_option(value=…)`,
  // the same text linux and tui match). Which room was picked is still read off
  // the option's POSITION, so two rooms sharing a label stay distinct.
  function roomOption(room: { label: string }): string {
    return t.feed.post.gate_room({ room: room.label });
  }

  // The controlled `<select>`'s display value — display only, never the answer.
  function selectValue(): string {
    if (sellSelected) return SELL_VALUE;
    if (gateRoom) {
      const room = ownRooms.find((r) => r.room === gateRoom);
      return room ? roomOption(room) : PUBLIC_VALUE;
    }
    return gateTier || PUBLIC_VALUE;
  }

  function handleGateSelectChange(e: Event & { currentTarget: HTMLSelectElement }): void {
    const { gateTier: name, gateRoom: room, sellSelected: isSell } = resolveGateSelection(
      e.currentTarget.selectedIndex,
      ownTiers.map((tier) => tier.name),
      ownRooms.map((r) => r.room),
    );
    onsellselectedchange?.(isSell);
    ongateroomchange?.(room);
    if (!isSell) {
      ongatetierchange?.(name);
    }
  }

  let composeTextarea: HTMLTextAreaElement | undefined = $state();
  let showCompose = $state(true);
  let dragging = $state(false);

  async function handleDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    const file = e.dataTransfer?.files?.[0] ?? null;
    if (!file) return;
    const buf = await file.arrayBuffer();
    onfilechange(file, new Uint8Array(buf));
  }
</script>

<button class="compose-toggle" data-testid={IDS.COMPOSE_BUTTON} onclick={() => { showCompose = !showCompose; }}>{t.feed.post.compose}</button>
{#if showCompose}
<div
  class="compose-box"
  class:drag-over={dragging}
  role="region"
  aria-label={t.feed.post.compose_drop_hint}
  ondragover={(e) => { e.preventDefault(); dragging = true; }}
  ondragleave={() => { dragging = false; }}
  ondrop={handleDrop}
>
  <MarkdownToolbar textarea={composeTextarea} value={composeBody} onchange={onbodychange} />
  <textarea
    class="compose-textarea"
    data-testid={IDS.COMPOSE_TEXT_FIELD}
    placeholder={t.feed.post.whats_on_your_mind}
    rows="3"
    value={composeBody}
    oninput={(e) => onbodychange(e.currentTarget.value)}
    bind:this={composeTextarea}
  ></textarea>
  <div class="compose-meta">
    <input
      class="text-input small"
      data-testid={IDS.COMPOSE_TAGS_FIELD}
      type="text"
      placeholder={t.feed.post.tags_placeholder}
      value={composeTags}
      oninput={(e) => ontagschange(e.currentTarget.value)}
    />
  </div>
  <!-- Gate-to-tier controls (compose-gate-tier-select / compose-gate-preview-field,
       feed.md § Encryption at rest; monetization.md § Pillars 2+3). Options are the
       author's own tiers (snapshot own_tiers, refreshed on feed load), then one
       per room they can address a post to (snapshot own_rooms, "Room: <label>"),
       then "Sell this post…" always last. The public-teaser field appears once
       any restricted answer is picked — its plaintext becomes the gated post's
       body, the full body seals under the tier's period key, the room's key, or
       the auto-minted unlock tier's, at submit. -->
  <div class="compose-meta">
    <select
      class="text-input small"
      data-testid={IDS.COMPOSE_GATE_TIER_SELECT}
      aria-label={t.feed.post.gate_audience}
      value={selectValue()}
      onchange={handleGateSelectChange}
    >
      <option value={PUBLIC_VALUE}>{PUBLIC_VALUE}</option>
      {#each ownTiers as tier}
        <option value={tier.name}>{tier.name}</option>
      {/each}
      {#each ownRooms as room (room.room)}
        <option value={roomOption(room)}>{roomOption(room)}</option>
      {/each}
      <option value={SELL_VALUE}>{SELL_VALUE}</option>
    </select>
    {#if gateTier || gateRoom || sellSelected}
      <input
        class="text-input small"
        data-testid={IDS.COMPOSE_GATE_PREVIEW_FIELD}
        type="text"
        placeholder={t.feed.post.gate_preview_placeholder}
        value={gatePreview}
        oninput={(e) => ongatepreviewchange?.(e.currentTarget.value)}
      />
    {/if}
  </div>
  {#if sellSelected}
    <!-- "Sell this post…" controls (monetization.md § Per-post pay-to-unlock;
         IDs user-approved 2026-07-29). The rank knob defaults CHECKED — an
         existing paying subscriber isn't charged twice for a post their
         subscription would cover; pay-per-view is the deliberate opt-in. -->
    <div class="compose-meta">
      <input
        class="text-input small"
        data-testid={IDS.COMPOSE_SELL_PRICE}
        type="text"
        placeholder={t.feed.post.sell_price_placeholder}
        value={sellPrice}
        oninput={(e) => onsellpricechange?.(e.currentTarget.value)}
      />
      {#if __FAUNA_PAYMENTS__}
        <AskingPriceInput
          variant="compose-sell"
          placeholder={t.feed.post.sell_asking_price_placeholder}
          value={sellAskingPrice}
          onvaluechange={(v) => onsellaskingpricechange?.(v)}
        />
      {/if}
      <label class="sell-subscribers-free-label">
        <input
          type="checkbox"
          data-testid={IDS.COMPOSE_SELL_SUBSCRIBERS_FREE}
          checked={sellSubscribersFree}
          data-checked={sellSubscribersFree ? 'true' : 'false'}
          onchange={(e) => onsellsubscribersfreechange?.(e.currentTarget.checked)}
        />
        {t.feed.post.sell_subscribers_free}
      </label>
    </div>
  {/if}
  <div class="compose-file">
    <input
      data-testid={IDS.COMPOSE_FILE}
      type="file"
      accept="image/*,video/*"
      onchange={async (e) => {
        const file = (e.target as HTMLInputElement).files?.[0] ?? null;
        if (file) {
          const buf = await new Promise<ArrayBuffer>((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result as ArrayBuffer);
            reader.onerror = () => reject(reader.error);
            reader.readAsArrayBuffer(file);
          });
          onfilechange(file, new Uint8Array(buf));
        } else {
          onfilechange(null, null);
        }
      }}
    />
    {#if attachedFile}
      <span data-testid={IDS.COMPOSE_FILE_READY} class="file-ready">{attachedFile.name} ({byteSize(attachedFile.size)})</span>
      <button
        type="button"
        class="btn-secondary"
        data-testid={IDS.COMPOSE_FILE_REMOVE}
        title={t.common.remove}
        onclick={onremovefile}
      >✕</button>
    {/if}
  </div>
  {#if composeError}
    <p class="error-text" data-testid={IDS.COMPOSE_ERROR}>{composeError}</p>
  {/if}
  <div class="compose-actions">
    <button
      class="btn-secondary"
      data-testid={IDS.COMPOSE_DIALOG_BUTTON}
      type="button"
      title={t.feed.post.open_rich_compose}
      onclick={() => ondialogopen?.()}
    >
      ⛶
    </button>
    <button
      class="btn-primary"
      data-testid={IDS.POST_SUBMIT_BUTTON}
      disabled={composing || !composeBody.trim() || !composeReady}
      onclick={onsubmit}
    >
      {composing ? t.feed.post.posting : t.common.post}
    </button>
  </div>
</div>
{/if}

<style>
  .compose-toggle {
    background: var(--bg-surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.4rem 0.75rem;
    cursor: pointer;
    font-size: 0.85rem;
    margin-bottom: 0.5rem;
  }
  .compose-toggle:hover { background: var(--bg-hover); }
  .compose-box {
    border: 2px dashed transparent;
    border-color: var(--border);
    border-style: solid;
    border-radius: 8px;
    padding: 0.75rem;
    margin-bottom: 1rem;
    background: var(--bg-card, var(--bg));
    transition: border-color 0.15s, background 0.15s;
  }
  .compose-box.drag-over {
    border-style: dashed;
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-card, var(--bg)));
  }
  .compose-textarea {
    width: 100%;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.5rem;
    font-size: 0.85rem;
    resize: vertical;
    color: var(--text);
    font-family: inherit;
  }
  .compose-meta {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  .sell-subscribers-free-label {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.8rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .text-input.small {
    flex: 1;
    font-size: 0.8rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
  }
  .compose-file { margin-top: 0.5rem; font-size: 0.8rem; }
  .file-ready { color: var(--accent); font-weight: 600; margin-left: 0.25rem; }
  .error-text { color: var(--error, #e74c3c); font-size: 0.8rem; margin: 0.25rem 0; }
  .compose-actions { margin-top: 0.5rem; display: flex; justify-content: flex-end; }
  .btn-primary {
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 4px;
    padding: 0.4rem 1rem;
    font-weight: 600;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-secondary {
    background: var(--bg-surface);
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.4rem 0.6rem;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .btn-secondary:hover { background: var(--bg-hover); }
</style>
