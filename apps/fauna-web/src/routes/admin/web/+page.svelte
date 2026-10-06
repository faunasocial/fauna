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
  // `webGet/SetApexActor` calls in `$lib/rpc`, the WASM twin of the linux native
  // `WebClient` + `fauna-ffi`'s `FfiWebClient`). The apex URL hint is the shared
  // `webApexUrl` projection (`fauna_core::web::apex_url`), one source of truth with
  // the nest's routing (priority #2). Per-user subdomain hosting is the user
  // `web-settings` page, not here. Lifts the linux lead
  // (apps/fauna-linux/src/settings/admin_web.rs). IDs: ui.yaml § admin-web.
  import { identity } from '$lib/store';
  import { webGetApexActor, webSetApexActor, adminUsersListAll } from '$lib/rpc';
  import { ensureWasm, webApexUrl, hexFull, adminPickerOption } from '$lib/wasm';
  import { toBytes, actorHex } from '$lib/hex';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface ActorOption { id: Uint8Array; label: string; hex: string }

  let error = $state('');
  let loading = $state(true);
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
  let apexUrl = $derived(webApexUrl($identity?.domain ?? ''));

  // Fetch the current apex + the actor list, then rebuild the picker.
  async function load(): Promise<void> {
    const secretHex = $identity?.secretHex ?? '';
    current = await webGetApexActor(secretHex);
    // A failed actor-list read must not break the page — the picker degrades to
    // "None" + any current designation kept visible.
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
    } catch {
      // Non-fatal: the picker just can't offer new actors yet.
    }
  }

  onMount(async () => {
    try {
      await ensureWasm();
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  // Designate (or clear, when `actorId` is null) the apex actor, then re-read so
  // the snapshot reflects the nest-confirmed designation.
  async function setApex(actorId: Uint8Array | null): Promise<void> {
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
    disabled={loading}
    onchange={(e) => setApex(apex.ids[(e.currentTarget as HTMLSelectElement).selectedIndex])}
  >
    {#each apex.options as opt}<option value={opt}>{opt}</option>{/each}
  </select>
</div>

<p class="muted small" data-testid={IDS.ADMIN_WEB_APEX_INFO}>{t.admin.web_page.apex_info({ url: apexUrl })}</p>

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
