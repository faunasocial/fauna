<script lang="ts">
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The "via connected app" badge (`delegated-origin-badge`), rendered **iff** an
  // EXTERNAL APP authored this post as the account, through the D10 delegated
  // authoring sub-key — the shared `fauna_core::render::AuthoringOriginStatus` is
  // `Delegated` (`docs/goal/behavior/atproto-pds-full.md` § Problem 1 → D10 →
  // *Audit*, ratified 2026-07-29). This is what makes the grant *audited* rather
  // than merely revocable: the signed bytes **are** the log, read client-side, so
  // a user scrolling their own feed can tell which posts they did not write.
  //
  // The structural twin of `UnverifiedSourceBadge`, one field over. The enum
  // serializes over wasm-bindgen as the plain variant string, so the snapshot
  // post's `authoring_origin` is `'Delegated'` / `'Direct'` / `'Unknown'`.
  //
  // ⚠ The gate is `=== 'Delegated'`, deliberately narrower than "not Direct".
  // `'Unknown'` covers **both** the undecoded nest-index list card *and* the
  // verification-FAILED case: an unverified wire's `signer_auth` cert is exactly
  // the part nothing authenticated, so badging it would let a forgery paint
  // itself as "merely delegated" — the inversion of an audit surface.
  //
  // The badge names the FACT, never an app: one authoring sub-key is minted per
  // account, so nothing in the signed bytes says WHICH app wrote the post.
  let { authoringOrigin }: { authoringOrigin: string } = $props();
</script>

{#if authoringOrigin === 'Delegated'}
  <span
    class="delegated-origin-badge"
    data-testid={IDS.DELEGATED_ORIGIN_BADGE}
    title={t.feed.delegated_origin_tooltip}
  >🔗 {t.feed.delegated_origin}</span>
{/if}

<style>
  .delegated-origin-badge {
    display: inline-flex;
    align-items: center;
    font-size: 0.7rem;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    font-weight: 500;
    white-space: nowrap;
    border: 1px solid var(--border);
    background: var(--bg-surface);
    color: var(--text-muted);
  }
</style>
