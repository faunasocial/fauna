<script lang="ts">
  // The `/app` root-index redirect is owned entirely by the layout guard
  // (`+layout.svelte` onMount): unauthenticated / pending-factory-reset → the
  // onboarding route, otherwise → the default landing page (Conversations). This
  // page used to `goto('/app/conversations')` from its own onMount, which raced
  // the layout guard — a "one owner per claim" violation whose winner depended on
  // Svelte's onMount ordering. Keeping the redirect in exactly one place makes
  // the landing deterministic.
</script>

<!-- Intentionally empty: the layout guard redirects away from the bare index. -->
