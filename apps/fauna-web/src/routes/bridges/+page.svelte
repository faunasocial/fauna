<script lang="ts">
  import {
    listBridges,
    linkBridge,
    unlinkBridge,
    updateBridgeSettings,
    listBridgeFollows,
    addBridgeFollow,
    removeBridgeFollow,
    type BridgeInfo,
    type BridgeFollow,
  } from '$lib/bridges';
  import { identity } from '$lib/store';
  import { isSafeNavUrl } from '$lib/safe-url';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import BridgeCard from '$lib/components/BridgeCard.svelte';
  import { t } from '$lib/i18n/strings';
  import { feedSourceOp, isUnifiedBridgesPageBridge, wardAskRules } from '$lib/wasm';
  import { IDS } from '$lib/generated/uiIds';
  import { familyFeedSourceRequest } from '$lib/rpc';
  import { isGuardianApprovalRequired, refusalText } from '$lib/guardian-refusal';
  import {
    addRefusedTriple,
    sourceAskRows,
    type FeedTriple,
  } from '$lib/ward-asks';
  import { wardAsks, refreshWardAsks, rereadAfterAsk } from '$lib/wardAsks.svelte';

  let bridges: BridgeInfo[] = $state([]);
  let loading = $state(true);
  let error = $state('');
  let follows: Record<string, BridgeFollow[]> = $state({});
  // The `(bridge_id, operation, target)` triples this session saw refused by
  // the guardian gate (family-safety.md § Feed-source approvals) — the LOCAL
  // half of the ask surface, what turns a refusal the ward just hit into a
  // visible `bridge-source-request-button`. The DURABLE half is the ward's own
  // `status.feed_requests` (`$lib/wardAsks.svelte`), read first. Keyed on the
  // ask data, never the add-follow buffers (the card clears them at dispatch).
  let refused: FeedTriple[] = $state([]);

  /** Split the guardian gate off every other failure (the typed refusal only —
   *  a transport failure must never imply supervision). The refusal still lands
   *  on `error-message`: the operation did not happen, it just is no longer a
   *  dead end. */
  function failedOrRefused(e: unknown, triple: FeedTriple) {
    if (isGuardianApprovalRequired(e)) {
      error = t.bridges.source_blocked;
      refused = addRefusedTriple(refused, triple);
    } else {
      error = refusalText(e) || t.common.error;
    }
  }

  /** `bridge-source-request-button` — ask the guardian for exactly the refused
   *  triple, then re-read so the pending state is the nest's own. The ask's own
   *  typed refusals (cap reached, knob off) are the ward's to read verbatim.
   *  Display label: the bridge name — the follow's petname buffer is gone by now. */
  async function handleRequestSource(bridge: BridgeInfo, triple: FeedTriple) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await familyFeedSourceRequest(secret, triple.bridge_id, triple.operation, triple.target, bridge.name);
      error = '';
      await rereadAfterAsk(secret);
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }

  async function refresh() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      // Nostr is NOT a Bridges-page bridge — it has its own dedicated page
      // (the shared NostrSettingsSection; nostr.md § Page structure / bridges.md
      // § Scope, 2026-06-13). Exclude it via the shared predicate — every
      // app with this page applies the same rule.
      bridges = (await listBridges(secret)).filter((b) => isUnifiedBridgesPageBridge(b.id));
      const newFollows: Record<string, BridgeFollow[]> = {};
      for (const bridge of bridges) {
        if (bridge.linked && bridge.supports_follows) {
          newFollows[bridge.id] = await listBridgeFollows(secret, bridge.id);
        }
      }
      follows = newFollows;
    } catch (e: any) {
      error = e.message;
    }
    loading = false;
  }

  onMount(() => {
    // Check for OAuth callback params
    const params = new URLSearchParams(window.location.search);
    const bridgeParam = params.get('bridge');
    const resultParam = params.get('result');
    if (bridgeParam && resultParam === 'error') {
      error = `Linking ${bridgeParam} failed. Please try again.`;
    }
    // Clear query params after reading
    if (bridgeParam && resultParam) {
      window.history.replaceState({}, '', window.location.pathname);
    }

    // The ward's own feed-source asks, so a pending / approved ask paints on an
    // open that never saw the refusal (best-effort).
    const secret = $identity?.secretHex;
    if (secret) void refreshWardAsks(secret);
    refresh();
  });

  async function handleLink(bridge: BridgeInfo, mode: string, fields: Record<string, string>) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    error = '';
    try {
      const resp = await linkBridge(secret, bridge.id, mode, fields);
      if (resp.redirect_url) {
        // The redirect target is nest-supplied; refuse to navigate to a
        // non-https scheme (security.md
        // § Transport trust). A legitimate OAuth redirect is always https.
        if (!isSafeNavUrl(resp.redirect_url)) {
          error = t.bridges.unsafe_redirect;
          return;
        }
        window.location.href = resp.redirect_url;
        return;
      }
      await refresh();
    } catch (e) {
      // A link ask carries an EMPTY target by construction — approving a link
      // approves connecting that bridge; the mode is mechanism, not scope.
      failedOrRefused(e, { bridge_id: bridge.id, operation: feedSourceOp.link(), target: '' });
    }
  }

  async function handleUnlink(bridge: BridgeInfo) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await unlinkBridge(secret, bridge.id);
      await refresh();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleSettingChange(bridge: BridgeInfo, key: string, value: any) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await updateBridgeSettings(secret, bridge.id, { [key]: value });
      await refresh();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleAddFollow(bridgeId: string, id: string, petname?: string) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await addBridgeFollow(secret, bridgeId, id, petname);
      await refresh();
    } catch (e) {
      failedOrRefused(e, { bridge_id: bridgeId, operation: feedSourceOp.follow(), target: id });
    }
  }

  async function handleRemoveFollow(bridgeId: string, fId: string) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await removeBridgeFollow(secret, bridgeId, fId);
      await refresh();
    } catch (e: any) {
      error = e.message;
    }
  }
</script>

<h2 data-testid={IDS.PAGE_HEADING}>{t.common.bridges}</h2>

<MessageBanner bind:error />

{#if loading}
  <p>{t.common.loading}</p>
{:else}
  {#each bridges as bridge}
    <BridgeCard
      {bridge}
      follows={follows[bridge.id] ?? []}
      sourceAsks={sourceAskRows(wardAskRules, wardAsks().feed, refused, bridge.id)}
      onRequestSource={(triple) => handleRequestSource(bridge, triple)}
      onLink={(mode, fields) => handleLink(bridge, mode, fields)}
      onUnlink={() => handleUnlink(bridge)}
      onSettingChange={(key, value) => handleSettingChange(bridge, key, value)}
      onAddFollow={(id, petname) => handleAddFollow(bridge.id, id, petname)}
      onRemoveFollow={(fId) => handleRemoveFollow(bridge.id, fId)}
    />
  {/each}
{/if}
