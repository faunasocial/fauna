<script lang="ts">
  // /app/devices moved into the Settings shell (sync-file-set-ui-unification,
  // 2026-06-28): the device roster is now Settings → Devices and the folder
  // wizard/list/conflicts are Settings → Folders. `devices` is no longer a
  // top-level nav item (ui.yaml drops it from navigation.pages). This route is a
  // redirect kept for old links/bookmarks and the e2e {"view":"devices"} nav —
  // the live UI lives in src/lib/components/{DevicesSection,FoldersSection}.svelte.
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';

  onMount(() => {
    void goto('/app/settings/devices', { replaceState: true });
  });
</script>
