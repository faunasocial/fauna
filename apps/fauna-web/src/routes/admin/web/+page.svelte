<script lang="ts">
  // Admin "Web" page (`admin-web`): the deployment apex-actor designation
  // (web-content-hosting.md § Admin apex hosting). One
  // control — the **apex-actor picker** (`admin-web-apex-actor-select`): an
  // Admin-class designation of which actor's `web` content serves at
  // `https://<domain>/`, "None" clearing it to the built-in info page. The direct
  // analogue of the per-domain catch-all mail actor
  // (`admin-dns-domain-catch-all-select`) and built the same way: a `<select>`
  // whose option `value` is `adminPickerOption(u)` (handle, else full hex —
  // admin.md § 2's two-halves rule; never the raw editable label, which two
  // accounts could share), dispatching by `selectedIndex` against a parallel
  // id map (index-based so the binding half stays correct regardless). The actor
  // list is every account on the nest (`adminUsersListAll` — admin.md § 2 →
  // *Which accounts a picker offers*); the current designation + set/clear
  // ride the shared `WebClient` over the singleton WS-RPC client (the
  // `webGet/SetApexActor` calls in `#lib/rpc`, the WASM twin of the linux native
  // `WebClient` + `fauna-ffi`'s `FfiWebClient`). The apex info line is the shared
  // `webApexInfoText` (`fauna_client_web::apex_info_text`): where the apex
  // serves, or why there is no address — never `https:///` (admin.md § 7 Web).
  // The picker paints from the read's own answer (`readPossibility`): blocked
  // before the read lands and after a failed apex read. Per-user subdomain hosting is the user
  // `web-settings` page, not here. Lifts the linux lead
  // (apps/fauna-linux/src/settings/admin_web.rs). IDs: ui.yaml § admin-web.
  import { identity } from '#lib/store';
  import { webGetApexActor, webSetApexActor, webServingDomain, adminUsersListAll } from '#lib/rpc';
  import {
    ensureWasm,
    webApexInfoText,
    hexFull,
    adminPickerOption,
    readPossibility,
  } from '#lib/wasm';
  import { paintPossibility } from '#lib/possibility';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { toBytes, actorHex } from '#lib/hex';
  import { onMount } from 'svelte';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  interface ActorOption { id: Uint8Array; label: string; hex: string }

  let error = $state('');
  // Where the apex-designation read stands (rule 5): the picker is not enabled
  // before it resolves, nor after it failed.
  let readState = $state<'pending' | 'failed' | 'resolved'>('pending');
  let wasmReady = $state(false);
  // The current nest-wide apex designation (`null` ⇒ the built-in info page) +
  // the pickable deployment actors. Both set only from nest-confirmed reads.
  let current: Uint8Array | null = $state(null);
  let actors: ActorOption[] = $state([]);

  // Build the picker model: option 0 = "None" (clears), option i = an actor, plus
  // a trailing "actor xxxx…" entry if the current designation isn't among the
  // loaded actors (e.g. paginated out) so it stays visible + selected rather than
  // silently clearing. `ids[i]` is the parallel actor-id map (null at 0) the
  // onchange handler dispatches by `selectedIndex`. `selected` is the option
  // `value` (== its label) the `<select>` shows as current.
  function buildApex(): { options: string[]; ids: (Uint8Array | null)[]; selected: string } {
    const none = t.admin.web_page.apex_none;
    const options: string[] = [none];
    const ids: (Uint8Array | null)[] = [null];
    for (const a of actors) {
      options.push(a.label);
      ids.push(a.id);
    }
    let selected: string = none;
    if (current && current.length > 0) {
      const curHex = actorHex(current);
      const match = actors.find((a) => a.hex === curHex);
      if (match) {
        selected = match.label;
      } else {
        const fallback = t.admin.actor_id_fallback_label({ short: hexFull(current) });
        options.push(fallback);
        ids.push(current);
        selected = fallback;
      }
    }
    return { options, ids, selected };
  }

  let apex = $derived(buildApex());
  // The nest's own `web_serving_domain` (`fauna.nest.info`, via the shared
  // `WebClient::serving_domain`) — never the sign-in/handle domain, which can be
  // a placeholder or differ from the address the nest actually serves at
  // (admin.md § 7). Empty until read; the info line then explains the nest
  // has no web address rather than composing one.
  let servingDomain = $state('');
  let apexInfo = $derived(wasmReady ? resolveLocalized(webApexInfoText(servingDomain)) : '');
  // The picker's answer: the read alone decides (blocked while pending or
  // failed). Before wasm loads it is shown disabled, never enabled on a guess.
  let pickerAnswer = $derived(
    wasmReady
      ? paintPossibility(readPossibility(readState))
      : { shown: true, disabled: true, reason: null },
  );

  // Fetch the current apex + the actor list, then rebuild the picker. The
  // account list and the serving domain degrade rather than blank the page —
  // the designation the admin came to set still shows — but neither failure is
  // silent: each reaches `error-message` (`docs/goal/ui/README.md` § Offer only
  // what is possible, rule 7; admin.md § 7 Web).
  async function load(): Promise<void> {
    const secretHex = $identity?.secretHex ?? '';
    try {
      current = await webGetApexActor(secretHex);
    } catch (e) {
      readState = 'failed';
      throw e;
    }
    const failures: string[] = [];
    try {
      servingDomain = await webServingDomain(secretHex);
    } catch (e) {
      failures.push(`load web serving domain: ${e instanceof Error ? e.message : String(e)}`);
    }
    try {
      // Every account on the nest, never one page (admin.md § 2 → *Which
      // accounts a picker offers*). Option text is `adminPickerOption` (the
      // two-halves rule's display half): the handle, else full hex, never the
      // raw editable label.
      const users = await adminUsersListAll(secretHex);
      actors = users.map((u) => ({
        id: toBytes(u.actor_id),
        label: adminPickerOption(u),
        hex: actorHex(u.actor_id),
      }));
    } catch (e) {
      failures.push(`load accounts: ${e instanceof Error ? e.message : String(e)}`);
    }
    readState = 'resolved';
    if (failures.length > 0) error = failures.join('; ');
  }

  onMount(async () => {
    try {
      await ensureWasm();
      wasmReady = true;
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // Designate (or clear, when `actorId` is null) the apex actor, then re-read so
  // the snapshot reflects the nest-confirmed designation.
  async function setApex(actorId: Uint8Array | null): Promise<void> {
    // The pick asks the same answer the picker paints from, and a refusal
    // says why (rules 6 and 7).
    if (pickerAnswer.disabled) {
      if (pickerAnswer.reason) error = resolveLocalized(pickerAnswer.reason);
      return;
    }
    error = '';
    try {
      await webSetApexActor($identity?.secretHex ?? '', actorId);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<h1 data-testid={IDS.ADMIN_WEB_HEADING}>{t.admin.web_page.title}</h1>

<MessageBanner bind:error />

<p class="muted">{t.admin.web_page.apex_select_subtitle}</p>

<div class="apex-row">
  <span class="caption">{t.admin.web_page.apex_select_label}</span>
  <select
    class="apex-select"
    data-testid={IDS.ADMIN_WEB_APEX_ACTOR_SELECT}
    value={apex.selected}
    disabled={pickerAnswer.disabled}
    onchange={(e) => setApex(apex.ids[(e.currentTarget as HTMLSelectElement).selectedIndex])}
  >
    {#each apex.options as opt}<option value={opt}>{opt}</option>{/each}
  </select>
</div>
{#if pickerAnswer.reason}
  <!-- The picker's blocked reason, within eyeshot (rule 2). -->
  <p class="muted small">{resolveLocalized(pickerAnswer.reason)}</p>
{/if}

<p class="muted small" data-testid={IDS.ADMIN_WEB_APEX_INFO}>{apexInfo}</p>

<style>
  h1 { margin-bottom: 0.75rem; font-size: 1.25rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .apex-row {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    margin: 1rem 0;
  }
  .caption { color: var(--text-muted); font-size: 0.9rem; }
  .apex-select {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.375rem 0.5rem;
    color: var(--text);
    min-width: 14rem;
  }
</style>
