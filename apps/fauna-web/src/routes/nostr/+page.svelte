<script lang="ts">
  // The standalone Nostr page (nostr-tab → /app/nostr). Nostr keeps its OWN
  // dedicated page — the same treatment as mail (nostr.md § Page structure,
  // ratified 2026-06-13). A thin wrapper: page chrome (`page-heading` +
  // MessageBanner) around the shared `NostrSettingsSection`, which the settings
  // rail's "Nostr" sub-page renders identically (one component, priority #2).
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import NostrSettingsSection from '$lib/components/NostrSettingsSection.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  let error = $state('');
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.nostr.title}</h1>

<MessageBanner bind:error />

<NostrSettingsSection bind:error />

<style>
  h1 { margin-bottom: 1.5rem; }
</style>
