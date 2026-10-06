<script lang="ts">
  // Shared **Nostr** settings section (docs/goal/ui/nostr.md). The full account /
  // content / relays / follows management surface, rendered identically by
  // BOTH the standalone `/nostr` route (reached via `nostr-tab`) AND the settings
  // rail's "Nostr" sub-page (`{:else if current === 'nostr'}` in
  // routes/settings/[[subpage]]/+page.svelte) — one component, no per-surface
  // divergence (priority #2). Nostr keeps its OWN dedicated page, the same
  // treatment as mail (nostr.md § Page structure, ratified 2026-06-13), and the
  // settings rail entry now carries the same real content rather than a pointer
  // (settings.md § Implementation status — the web nostr-rail gap closed).
  //
  // The control plane rides the unified `fauna.bridges.*` WS-RPC seam
  // (`bridge_id:"nostr"`, served by `NostrProvider`). DMs are not listed here:
  // a Nostr DM is a bridged room on the unified Conversations page
  // (`nostr.md` § Implementation status today → DMs). NIP-07 (browser-extension
  // signing) / publish-signed go through `$lib/nostr`. The page is gated on the S8.9
  // nsec-deposit gate (`nostr.md` § The bridging gate): on a box where no
  // actor has ever deposited an nsec — or a nest built without the `nostr`
  // cargo feature, so `NostrProvider` is absent — `available` is false and
  // only the "unavailable" notice renders (cleanly degraded, never
  // half-working); linking itself stays reachable regardless (it bootstraps
  // the deposit).
  //
  // The parent surface owns the single `error-message` (no duplicate IDs); link /
  // settings / relay / follow errors flow into it via the bindable `error`
  // (mirrors MailSettingsSection / SubscriptionsSection).
  import { identity } from '$lib/store';
  import {
    listBridges,
    linkBridge,
    unlinkBridge,
    updateBridgeSettings,
    listBridgeFollows,
    addBridgeFollow,
    removeBridgeFollow,
    type BridgeSetting,
    type BridgeFollow,
  } from '$lib/bridges';
  import {
    hasNip07,
    nip07LinkFields,
    nostrBunkerCreateInvite,
    nostrZapSignersList,
    nostrZapSignersAdd,
    nostrZapSignersRemove,
    type BunkerInvite,
    type ZapSignerEntry,
  } from '$lib/nostr';
  import { npubConfirmationOwed, confirmNostrNpub } from '$lib/conversations';
  import { fetchFeatures } from '$lib/api';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import { nostrKeySourceLabel, nostrLinkModeLabel, relayUrlError, trimmedRelayInput, relayListAppending, shortNestId, shortId, qrMatrix, qrQuietZoneModules, ensureWasm, nostrContentToggleOptions, type BridgeToggleOption } from '$lib/wasm';
  import { resolveLocalized, resolveLocalizedNested } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  const BRIDGE_ID = 'nostr';

  // The parent settings/page surface owns the single error banner; Nostr
  // dispatch errors flow into it.
  let { error = $bindable('') } = $props();

  // --- State ---

  let loading = $state(true);

  // Account status. `registered` is whether the Nostr bridge is present in
  // `fauna.bridges.list` AT ALL (a nest built without the `nostr` cargo
  // feature omits it entirely) — genuinely, permanently unavailable, unlike
  // `available` (S8.9 nsec-deposit gate: false on a box with zero deposits
  // yet, but the FIRST deposit — generate/import — is always reachable
  // regardless, `nostr.md` § The bridging gate). Keep these separate: an
  // unregistered bridge shows the "unavailable" notice; a registered-but-
  // unavailable one still shows the link form when unlinked.
  let registered = $state(true);
  let available = $state(true);
  let linked = $state(false);
  let pubkey = $state('');
  let mode = $state('');
  let relayList = $state<string[]>([]);

  // Succession-aftermath npub confirm banner — checked once per `refresh()`
  // (the page's nav-enter path, tui/linux/android reference), never a
  // one-shot local flag: the successor may reach this page long after the
  // ceremony, so visibility is gated purely on the predicate, dismissible,
  // never a blocking modal.
  let npubConfirmOwed = $state(false);

  // Settings — the five content-publishing toggles. The rows themselves (id,
  // wire key, default, label, subtitle) come from the shared catalog
  // `nostrContentToggleOptions()` (`nostr.md` § Where logic lives); this
  // component previously spelled the same five keys three times over — once to
  // read them, once to write them, once to render their labels — and each
  // spelling was free to drift from the other two and from the other six apps.
  // Filled on first render (the catalog is a wasm call, so it needs the module
  // loaded); until then the section paints no toggles, exactly as it paints
  // nothing else before `refresh()` completes.
  let contentToggles: BridgeToggleOption[] = $state([]);
  // Current value per wire key. Keyed rather than five named booleans so the
  // read, the write and the render all walk the same catalog.
  let toggleValues: Record<string, boolean> = $state({});

  // Follows — `BridgeFollow` (id = npub, relay hints under extra.relay_hints).
  let follows: BridgeFollow[] = $state([]);
  let newFollowPubkey = $state('');
  let newFollowPetname = $state('');

  // Zap signers (the NIP-57 trust root, `monetization.md` § Zap receipts —
  // the trust model). Gated on LINKED alone — unlike Connected apps below,
  // NOT custodial: designating who may speak for your money is orthogonal to
  // where your key lives. `zapSignerGateReason` is the Dim-3 courtesy
  // why-line for the add button (`zaps.signer.designate`); `null` while
  // available OR un-hydrated — an un-hydrated read must leave the button
  // LIVE, since the nest, not the app, is the enforcement floor.
  let zapSigners: ZapSignerEntry[] = $state([]);
  let newZapSignerPubkey = $state('');
  let newZapSignerLabel = $state('');
  let zapSignerGateReason: string | null = $state(null);

  // Connected apps (NIP-46 bunker, `nostr.md` § The nest as the user's NIP-46
  // signer). The connections themselves are rows of Settings → Connected apps;
  // this page keeps the invite start. `bunkerInvite` holds the
  // one-time reveal after a successful mint — the connect string is shown once
  // and never re-fetched (only its hash rests on the nest), so it's cleared on
  // refresh/unlink. `bunkerQr` is the shared-Rust QR grid of the connect
  // string (rendered inline as SVG, the identity-export-qr pattern). The
  // section renders only for a linked account in a custodial mode
  // (`generated`/`imported`) — a `remote`/`nip07` account has no key on the
  // box to sign with, so the nest can't be its bunker.
  let bunkerInvite = $state<BunkerInvite | null>(null);
  let bunkerConnectCopied = $state(false);
  let bunkerQr = $state<{ side: number; darkModules: { key: number; x: number; y: number }[] } | null>(null);
  let showConnectedApps = $derived(linked && (mode === 'generated' || mode === 'imported'));

  // Link form
  let linkMode = $state('generate');
  let importNsec = $state('');
  let hasNip07Extension = $state(false);
  let pubkeyCopied = $state(false);

  // Relay management
  let newRelay = $state('');

  // --- API helpers ---
  //
  // The 5 content flags + `relay_list` arrive as a `settings[]` array keyed by
  // `key` (a polymorphic `value` per the provider's wire shape); read them by
  // key rather than positional index.

  function boolSetting(settings: BridgeSetting[], key: string, dflt: boolean): boolean {
    const s = settings.find((x) => x.key === key);
    return typeof s?.value === 'boolean' ? s.value : dflt;
  }

  function strSetting(settings: BridgeSetting[], key: string): string {
    const s = settings.find((x) => x.key === key);
    return typeof s?.value === 'string' ? s.value : '';
  }

  async function fetchStatus() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      const bridges = await listBridges(secret);
      const nostr = bridges.find((b) => b.id === BRIDGE_ID);
      registered = !!nostr;
      if (!nostr) {
        available = false;
        linked = false;
        return;
      }
      // `available` tracks whether ANY actor has deposited an nsec on this
      // box (S8.9 nsec-deposit gate, `nostr.md` § The bridging gate) — false
      // on a box with zero deposits. Linking itself (generate/import) stays
      // reachable even when `available` is false (it's what bootstraps the
      // deposit) — the template renders the link form on `!linked` regardless
      // of `available`; this gate covers everything downstream of a deposit.
      available = nostr.available;
      linked = nostr.linked;
      if (nostr.linked) {
        // npub rides identity.value (the wire omits the raw hex pubkey).
        pubkey = nostr.identity?.value ?? '';
        mode = nostr.mode ?? '';
        // Each toggle's default is the catalog's, i.e. the nest's own, so a
        // never-configured account paints what the nest would actually do.
        toggleValues = Object.fromEntries(
          contentToggles.map((o) => [o.key, boolSetting(nostr.settings, o.key, o.default_on)]),
        );
        const relayJson = strSetting(nostr.settings, 'relay_list');
        try {
          relayList = relayJson ? JSON.parse(relayJson) : [];
        } catch {
          relayList = [];
        }
      }
    } catch (e: any) {
      error = e.message;
    }
  }

  async function fetchFollows() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      follows = await listBridgeFollows(secret, BRIDGE_ID);
    } catch (e: any) {
      error = e.message;
    }
  }

  // --- Zap signers ---

  async function fetchZapSigners() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      zapSigners = await nostrZapSignersList(secret);
    } catch (e: any) {
      error = e.message;
    }
  }

  /** The `zaps` row's affordance, courtesy-read for the add button. Mirrors
   *  linux's `zap_signer_add_gate_reason`: read the shared decision, never
   *  re-derive it, and leave the button live on any failure to hydrate. */
  async function fetchZapSignerGate() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      const rows = await fetchFeatures(secret);
      const row = rows.find((r) => r.feature === 'zaps');
      if (!row || row.affordance === 'available') {
        zapSignerGateReason = null;
      } else {
        zapSignerGateReason = row.restriction
          ? resolveLocalizedNested(row.restriction)
          : resolveLocalized(row.status);
      }
    } catch {
      zapSignerGateReason = null;
    }
  }

  async function addZapSigner() {
    const secret = $identity?.secretHex;
    if (!secret || !newZapSignerPubkey.trim()) return;
    error = '';
    const pubkey = newZapSignerPubkey.trim();
    // Client-glue validation, mirroring the relay check above: the nest
    // refuses a non-64-hex key anyway, so this only spares a guaranteed
    // round trip and names the rule.
    if (!/^[0-9a-fA-F]{64}$/.test(pubkey)) {
      error = t.nostr.zap_signers.invalid_pubkey;
      return;
    }
    try {
      // Idempotent — a re-add REFRESHES the label, so re-entering a
      // designated key is the only rename path there is.
      await nostrZapSignersAdd(secret, pubkey, newZapSignerLabel.trim());
      newZapSignerPubkey = '';
      newZapSignerLabel = '';
      await fetchZapSigners();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function removeZapSigner(pubkey: string) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      await nostrZapSignersRemove(secret, pubkey);
      await fetchZapSigners();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function fetchNpubConfirm() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    // Best-effort like the shared predicate itself: any unhappy answer
    // already degrades to `false` inside the wasm call, so a bad read never
    // turns into a page error — it is just re-asked on the next load.
    try {
      npubConfirmOwed = await npubConfirmationOwed(secret);
    } catch {
      npubConfirmOwed = false;
    }
  }

  async function refresh() {
    loading = true;
    error = '';
    // The toggle catalog is a wasm call, and `fetchStatus` reads current
    // values off it — so load it first, not lazily at render. Guarded like
    // every other wasm call on this page: a module-load failure surfaces on
    // the shared error banner instead of leaving the spinner up forever.
    try {
      await ensureWasm();
      contentToggles = nostrContentToggleOptions();
      // Seed from the catalog's defaults so the first paint is the nest's own
      // posture rather than a row of undefineds; `fetchStatus` overwrites with
      // the real values a moment later.
      toggleValues = Object.fromEntries(contentToggles.map((o) => [o.key, o.default_on]));
    } catch (e: any) {
      error = e.message;
    }
    await fetchStatus();
    if (linked) {
      const tasks = [fetchFollows(), fetchNpubConfirm(), fetchZapSigners(), fetchZapSignerGate()];
      await Promise.all(tasks);
    } else {
      npubConfirmOwed = false;
    }
    loading = false;
  }

  /** "Yes, that's my npub" — writes the confirm timestamp via the shared
   *  `fauna.state.nostr-confirmation` write path, then re-checks (non-optimistic, like every other
   *  mutation on this page — the banner's disappearance is a fresh read, not
   *  an assumed outcome). */
  async function confirmNpub() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      await confirmNostrNpub(secret, Math.floor(Date.now() / 1000));
      await fetchNpubConfirm();
    } catch (e: any) {
      error = e.message;
    }
  }

  // --- Link / Unlink ---

  async function linkAccount() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      // `mode` is its own typed field; the per-mode params ride `fields`
      // (generate → {}, import → {nsec}, nip07 → {pubkey, proof_json}).
      let fields: Record<string, string> = {};
      if (linkMode === 'import') {
        if (!importNsec.trim()) {
          error = t.nostr.link_account.enter_nsec;
          return;
        }
        fields.nsec = importNsec.trim();
      }
      if (linkMode === 'nip07') {
        if (!hasNip07Extension) {
          error = t.nostr.link_account.no_nip07;
          return;
        }
        // Pubkey + the extension's signature over the nest's challenge —
        // the proof of possession the nest demands before linking.
        fields = await nip07LinkFields(secret, BRIDGE_ID);
      }
      await linkBridge(secret, BRIDGE_ID, linkMode, fields);
      importNsec = '';
      // A fresh (re-)link is itself the new-npub remedy (nostr.md:75): the
      // owner just chose this key, so best-effort record the confirmation —
      // never blocking the link on it (mirrors tui's Op::Link / linux's and
      // android's link-success arm). Harmless on an account with no
      // succession history: the predicate short-circuits before ever
      // reading it.
      try {
        await confirmNostrNpub(secret, Math.floor(Date.now() / 1000));
      } catch {
        // Best-effort — see above.
      }
      await refresh();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function unlinkAccount() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    // Plain destructive action — no confirm overlay, matching the FaunaKit
    // NostrSettingsView (`Button(role: .destructive)`) and the other apps
    // (nostr.md § Element IDs; priority #1). A native confirm() also can't be
    // driven by the e2e harness, so it would silently block the unlink flow.
    error = '';
    try {
      await unlinkBridge(secret, BRIDGE_ID);
      linked = false;
      pubkey = '';
      mode = '';
      follows = [];
      relayList = [];
      // No linked npub left to confirm — covers both a direct Unlink click
      // and the banner's own "no / nothing is linked" button (which
      // delegates here).
      npubConfirmOwed = false;
    } catch (e: any) {
      error = e.message;
    }
  }

  // --- Settings ---

  async function saveSettings() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      // Same field names the provider's settings wire expects; `relay_list` is
      // a JSON-array string. NIP-65 republish-on-change is preserved nest-side.
      await updateBridgeSettings(secret, BRIDGE_ID, {
        relay_list: JSON.stringify(relayList),
        ...Object.fromEntries(
          contentToggles.map((o) => [o.key, toggleValues[o.key] ?? o.default_on]),
        ),
      });
    } catch (e: any) {
      error = e.message;
    }
  }

  // --- Relay management ---

  function addRelay() {
    const url = trimmedRelayInput(newRelay);
    if (!url) return;
    const refusal = relayUrlError(url);
    if (refusal) {
      error = resolveLocalized(refusal);
      return;
    }
    const next = relayListAppending(relayList, url);
    if (!next) return;
    relayList = next;
    newRelay = '';
    saveSettings();
  }

  function removeRelay(url: string) {
    relayList = relayList.filter(r => r !== url);
    saveSettings();
  }

  // --- Follows ---

  async function addFollow() {
    const secret = $identity?.secretHex;
    if (!secret || !newFollowPubkey.trim()) return;
    error = '';
    try {
      // The provider accepts an npub or hex id.
      await addBridgeFollow(
        secret,
        BRIDGE_ID,
        newFollowPubkey.trim(),
        newFollowPetname.trim() || undefined,
      );
      newFollowPubkey = '';
      newFollowPetname = '';
      await fetchFollows();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function removeFollow(followId: string) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      await removeBridgeFollow(secret, BRIDGE_ID, followId);
      await fetchFollows();
    } catch (e: any) {
      error = e.message;
    }
  }

  // --- Connected apps (NIP-46 bunker) ---

  async function connectApp() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      await ensureWasm();
      const invite = await nostrBunkerCreateInvite(secret);
      bunkerInvite = invite;
      bunkerConnectCopied = false;
      // Render the connect string as a QR (shared-Rust grid, the
      // identity-export-qr pattern) — what a phone Nostr app scans off the
      // desktop screen. The string carries no quiet zone; pad all four sides.
      const m = qrMatrix(invite.connect_string);
      const quiet = qrQuietZoneModules();
      const darkModules: { key: number; x: number; y: number }[] = [];
      for (let y = 0; y < m.size; y++) {
        for (let x = 0; x < m.size; x++) {
          if (m.modules[y * m.size + x]) {
            darkModules.push({ key: y * m.size + x, x: x + quiet, y: y + quiet });
          }
        }
      }
      bunkerQr = { side: m.size + 2 * quiet, darkModules };
    } catch (e: any) {
      error = e.message;
    }
  }

  async function copyConnectString() {
    const text = bunkerInvite?.connect_string;
    if (text) {
      await navigator.clipboard.writeText(text);
      bunkerConnectCopied = true;
      setTimeout(() => { bunkerConnectCopied = false; }, 2000);
    }
  }

  // --- Lifecycle ---

  onMount(() => {
    hasNip07Extension = hasNip07();
    refresh();
  });

  async function copyPubkey() {
    const text = pubkey;
    if (text) {
      await navigator.clipboard.writeText(text);
      pubkeyCopied = true;
      setTimeout(() => { pubkeyCopied = false; }, 2000);
    }
  }
</script>

{#if loading}
  <p class="muted">{t.common.loading}</p>
{:else if !registered}
  <section class="card">
    <p class="muted">{t.nostr.unavailable}</p>
  </section>
{:else if !linked}
  <!-- Link Account -->
  <section class="card">
    <h2>{t.nostr.link_account.title}</h2>
    <p class="muted">{t.nostr.link_account.description}</p>

    <div class="form-group">
      <label for="link-mode">{t.common.mode}</label>
      <select id="link-mode" data-testid={IDS.NOSTR_LINK_MODE} bind:value={linkMode}>
        <option value="generate">{resolveLocalized(nostrLinkModeLabel('generate'))}</option>
        <option value="import">{resolveLocalized(nostrLinkModeLabel('import'))}</option>
        {#if hasNip07Extension}
          <option value="nip07">{resolveLocalized(nostrLinkModeLabel('nip07'))}</option>
        {/if}
      </select>
    </div>

    {#if linkMode === 'import'}
      <div class="form-group">
        <label for="nsec-input">{t.nostr.link_account.nsec_label}</label>
        <input
          id="nsec-input"
          type="password"
          data-testid={IDS.NOSTR_NSEC_INPUT}
          bind:value={importNsec}
          placeholder={t.nostr.link_account.nsec_placeholder}
        />
      </div>
    {/if}

    {#if linkMode === 'nip07'}
      <p class="muted">{t.nostr.link_account.nip07_prompt}</p>
    {/if}

    <button class="btn primary" data-testid={IDS.NOSTR_LINK_BUTTON} onclick={linkAccount}>{t.nostr.link_account.link_button}</button>
  </section>
{:else}
  <!-- Account Status -->
  <section class="card">
    <h2>{t.common.account}</h2>
    <div class="info-row">
      <span class="label">{t.nostr.account.public_key}</span>
      <span class="value mono">{pubkey}</span>
      <button class="btn-copy" data-testid={IDS.NOSTR_PUBKEY_COPY_BTN} onclick={copyPubkey}>{pubkeyCopied ? t.common.copied : t.common.copy}</button>
    </div>
    <div class="info-row">
      <span class="label">{t.common.mode}</span>
      <span class="value">{resolveLocalized(nostrKeySourceLabel(mode))}</span>
    </div>
    <button class="btn danger" data-testid={IDS.NOSTR_UNLINK_BUTTON} onclick={unlinkAccount}>{t.nostr.account.unlink}</button>
  </section>

  {#if npubConfirmOwed}
    <!-- Succession-aftermath npub confirm banner (leg 3 — nostr.md § Key
         succession and rotation; tui/linux/android reference). Dismissible,
         never a blocking modal. -->
    <section class="card">
      <p data-testid={IDS.NOSTR_NPUB_CONFIRM_BANNER}>{t.nostr.npub_confirm.banner({ npub: pubkey })}</p>
      <div class="reveal-actions">
        <button class="btn primary" data-testid={IDS.NOSTR_NPUB_CONFIRM_YES_BUTTON} onclick={confirmNpub}>{t.nostr.npub_confirm.yes_button}</button>
        <!-- "No / nothing is linked" reuses the EXISTING unlink gesture
             (nostr.md:75: the remedy is "the existing page machinery") —
             never a bespoke flow. -->
        <button class="btn" data-testid={IDS.NOSTR_NPUB_CONFIRM_NO_BUTTON} onclick={unlinkAccount}>{t.nostr.npub_confirm.no_button}</button>
      </div>
    </section>
  {/if}

  <!-- Settings -->
  <section class="card">
    <h2>{t.common.settings}</h2>
    <!-- One row per catalog entry, in the catalog's render order — the ids and
         labels are the shared table's, not this file's. -->
    {#each contentToggles as toggle (toggle.key)}
      <div class="toggle-row">
        <label>
          <input
            type="checkbox"
            data-testid={toggle.ui_id}
            data-state={toggleValues[toggle.key] ? 'on' : 'off'}
            bind:checked={toggleValues[toggle.key]}
            onchange={saveSettings}
          />
          {resolveLocalized(toggle.label)}
        </label>
        {#if toggle.subtitle}
          <p class="toggle-subtitle">{resolveLocalized(toggle.subtitle)}</p>
        {/if}
      </div>
    {/each}
  </section>

  <!-- Relays -->
  <section class="card">
    <h2>{t.nostr.relays.title}</h2>
    {#if relayList.length === 0}
      <p class="muted">{t.nostr.relays.none}</p>
    {:else}
      <ul class="relay-list">
        {#each relayList as relay}
          <li data-testid={IDS.NOSTR_RELAY_ITEM}>
            <span class="mono">{relay}</span>
            <button class="btn-sm danger" data-testid={IDS.NOSTR_REMOVE_RELAY} onclick={() => removeRelay(relay)}>{t.common.remove}</button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="inline-form">
      <input
        type="text"
        data-testid={IDS.NOSTR_RELAY_INPUT}
        bind:value={newRelay}
        placeholder={t.nostr.relays.placeholder}
        onkeydown={(e) => { if (e.key === 'Enter') addRelay(); }}
      />
      <button class="btn" data-testid={IDS.NOSTR_ADD_RELAY} onclick={addRelay}>{t.nostr.relays.add}</button>
    </div>
  </section>

  <!-- Follows -->
  <section class="card">
    <h2>{t.bridges.follows} ({follows.length})</h2>
    {#if follows.length === 0}
      <p class="muted">{t.nostr.follows.none}</p>
    {:else}
      <ul class="follow-list">
        {#each follows as f}
          <li data-testid={IDS.NOSTR_FOLLOW_ITEM}>
            <div class="follow-info">
              <span class="mono">{shortNestId(f.id)}</span>
              {#if f.petname}
                <span class="petname">({f.petname})</span>
              {/if}
            </div>
            <button class="btn-sm danger" data-testid={IDS.NOSTR_REMOVE_FOLLOW} onclick={() => removeFollow(f.id)}>{t.common.remove}</button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="inline-form">
      <input
        type="text"
        data-testid={IDS.NOSTR_FOLLOW_PUBKEY_INPUT}
        bind:value={newFollowPubkey}
        placeholder={t.nostr.follows.pubkey_placeholder}
      />
      <input
        type="text"
        data-testid={IDS.NOSTR_FOLLOW_PETNAME_INPUT}
        bind:value={newFollowPetname}
        placeholder={t.nostr.follows.petname_placeholder}
      />
      <button class="btn" data-testid={IDS.NOSTR_ADD_FOLLOW} onclick={addFollow}>{t.common.follow}</button>
    </div>
  </section>

  {#if showConnectedApps}
    <!-- Connected apps (Nostr Connect — the nest as the user's NIP-46 signer) -->
    <section class="card">
      <h2>{t.nostr.connected_apps.title}</h2>
      <p class="muted">{t.nostr.connected_apps.description}</p>

      <button class="btn primary" data-testid={IDS.NOSTR_BUNKER_CONNECT_BTN} onclick={connectApp}>
        {t.nostr.connected_apps.connect_button}
      </button>

      {#if bunkerInvite}
        <div class="connect-reveal">
          <p class="reveal-title">{t.nostr.connected_apps.reveal_title}</p>
          {#if bunkerQr}
            <svg
              data-testid={IDS.NOSTR_BUNKER_CONNECT_QR}
              class="bunker-qr"
              viewBox="0 0 {bunkerQr.side} {bunkerQr.side}"
              shape-rendering="crispEdges"
              role="img"
              aria-label={t.nostr.connected_apps.qr_alt}
            >
              <rect x="0" y="0" width={bunkerQr.side} height={bunkerQr.side} fill="#fff" />
              {#each bunkerQr.darkModules as m (m.key)}
                <rect x={m.x} y={m.y} width="1" height="1" fill="#000" />
              {/each}
            </svg>
          {/if}
          <code class="connect-string mono" data-testid={IDS.NOSTR_BUNKER_CONNECT_STRING}>{bunkerInvite.connect_string}</code>
          <div class="reveal-actions">
            <button class="btn" data-testid={IDS.NOSTR_BUNKER_CONNECT_COPY_BTN} onclick={copyConnectString}>
              {bunkerConnectCopied ? t.common.copied : t.common.copy}
            </button>
          </div>
          <p class="reveal-hint">{t.nostr.connected_apps.reveal_hint}</p>
        </div>
      {/if}

      <!-- The connections themselves moved to Settings → Connected apps
           (connected-apps.md — a lift, never a duplication); this page keeps
           the invite start. -->
    </section>
  {/if}

  <!-- Zap signers (the NIP-57 trust root — monetization.md § Zap receipts —
       the trust model). Unlike Connected apps above, rendered for ANY linked
       account, not just custodial ones. -->
  <section class="card">
    <h2>{t.nostr.zap_signers.title}</h2>
    <p class="muted">{t.nostr.zap_signers.description}</p>

    {#if zapSigners.length === 0}
      <p class="muted" data-testid={IDS.NOSTR_ZAP_SIGNER_EMPTY}>{t.nostr.zap_signers.none}</p>
    {:else}
      <ul class="follow-list">
        {#each zapSigners as signer (signer.id)}
          <li data-testid={IDS.NOSTR_ZAP_SIGNER_ITEM}>
            <span class="mono">
              {signer.label.trim() ? signer.label : t.nostr.zap_signers.unnamed} — {shortId(signer.signer_pubkey)}
            </span>
            <button class="btn-sm danger" data-testid={IDS.NOSTR_ZAP_SIGNER_REMOVE} onclick={() => removeZapSigner(signer.signer_pubkey)}>
              {t.nostr.zap_signers.remove}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="inline-form">
      <input
        type="text"
        data-testid={IDS.NOSTR_ZAP_SIGNER_PUBKEY_INPUT}
        bind:value={newZapSignerPubkey}
        placeholder={t.nostr.zap_signers.pubkey_placeholder}
      />
      <input
        type="text"
        data-testid={IDS.NOSTR_ZAP_SIGNER_LABEL_INPUT}
        bind:value={newZapSignerLabel}
        placeholder={t.nostr.zap_signers.label_placeholder}
      />
      <!-- The Dim-3 courtesy gate: disabled with its why-line only once a
           restricted verdict actually resolves — never eagerly, which is
           what leaves an un-hydrated read live. -->
      <button class="btn" data-testid={IDS.NOSTR_ZAP_SIGNER_ADD_BTN} disabled={!!zapSignerGateReason} onclick={addZapSigner}>
        {t.nostr.zap_signers.add}
      </button>
    </div>
    {#if zapSignerGateReason}
      <p class="muted">{zapSignerGateReason}</p>
    {/if}
  </section>
{/if}

<style>
  h2 { margin: 0 0 0.75rem; font-size: 1.1rem; }

  .card {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
    margin-bottom: 1rem;
  }


  .muted { color: var(--text-muted); }
  .mono { font-family: monospace; font-size: 0.875rem; }

  .info-row {
    display: flex;
    gap: 1rem;
    padding: 0.375rem 0;
  }
  .btn-copy {
    padding: 0.125rem 0.5rem; font-size: 0.75rem; margin-left: 0.5rem;
    border: 1px solid var(--border); border-radius: 4px;
    background: var(--bg-surface); color: var(--text-muted); cursor: pointer;
  }
  .btn-copy:hover { background: var(--bg-hover); }
  .info-row .label {
    font-weight: 600;
    min-width: 100px;
    color: var(--text-muted);
  }

  .form-group {
    margin-bottom: 0.75rem;
  }
  .form-group label {
    display: block;
    font-weight: 600;
    margin-bottom: 0.25rem;
    color: var(--text-muted);
  }
  .form-group input,
  .form-group select {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
  }

  .toggle-row {
    padding: 0.375rem 0;
  }
  .toggle-row label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
  }
  /* The catalog's second line, where a row has one — indented past the
     checkbox so it reads as that row's explanation, matching the linux
     SwitchRow subtitle it now shares a source with. */
  .toggle-subtitle {
    margin: 0.125rem 0 0 1.5rem;
    font-size: 0.85em;
    opacity: 0.75;
  }

  .btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn.primary {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }
  .btn.danger {
    background: var(--bg-error, #2d1b1b);
    color: var(--text-error, #f87171);
    border-color: var(--text-error, #f87171);
    margin-top: 0.75rem;
  }
  .btn-sm {
    padding: 0.25rem 0.5rem;
    font-size: 0.8rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
  }
  .btn-sm.danger {
    color: var(--text-error, #f87171);
    border-color: var(--text-error, #f87171);
  }

  .inline-form {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
  .inline-form input {
    flex: 1;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
  }

  .relay-list, .follow-list {
    list-style: none;
    padding: 0;
    margin: 0 0 0.5rem;
  }
  .relay-list li, .follow-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .follow-info {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .petname { color: var(--text-muted); font-size: 0.875rem; }

  /* Connected apps (NIP-46 bunker) — the invite reveal */
  .connect-reveal {
    margin-top: 0.75rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .reveal-title { font-weight: 600; margin: 0 0 0.5rem; }
  .bunker-qr {
    width: 180px;
    height: 180px;
    display: block;
    margin: 0 0 0.5rem;
    background: #fff;
    border-radius: 4px;
  }
  .connect-string {
    display: block;
    word-break: break-all;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
  }
  .reveal-actions { margin-top: 0.5rem; }
  .reveal-hint { color: var(--text-muted); font-size: 0.8rem; margin: 0.5rem 0 0; }
</style>
