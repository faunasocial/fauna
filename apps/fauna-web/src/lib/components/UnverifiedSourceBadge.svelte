<script lang="ts">
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The muted "unverified source" badge (`unverified-source-badge`), rendered
  // **iff** THIS client's signature verification of the post's signed envelope
  // FAILED — the shared `fauna_core::render::VerificationStatus` is `Failed`
  // (`docs/goal/architecture/security.md` § App display of unverified content;
  // review F-CL2/F-CL3). The enum serializes over wasm-bindgen as the plain
  // variant string, so the snapshot post's `verification` is `'Failed'` /
  // `'Verified'` / `'Unchecked'`. No badge for `'Unchecked'` (the default — a
  // trusted nest-index projection with no envelope to verify) or `'Verified'`.
  // The post body still renders in full; this badge is the visible caveat (the
  // DKIM-fail analogue), so a transient key-rotation-lag false-negative never
  // makes a legitimate post silently vanish.
  let { verification }: { verification: string } = $props();
</script>

{#if verification === 'Failed'}
  <span
    class="unverified-source-badge"
    data-testid={IDS.UNVERIFIED_SOURCE_BADGE}
    title={t.feed.unverified_source_tooltip}
  >⚠ {t.feed.unverified_source}</span>
{/if}

<style>
  .unverified-source-badge {
    display: inline-flex;
    align-items: center;
    font-size: 0.7rem;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    font-weight: 500;
    white-space: nowrap;
    border: 1px solid #f59e0b;
    background: rgba(245, 158, 11, 0.12);
    color: #d97706;
  }
</style>
