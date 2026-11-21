<!--
  The `provisioning-step-checkbox` of one step row (`onboarding-provisioning.md`
  § 6 → *A running step shows activity, never an attempt count*): a `Running`
  row paints an indeterminate CSS spinner, every other status the shared glyph.
  The span's text stays the shared glyph (`provisioningStatusGlyph`) — visually
  hidden while the spinner shows — so an automation read is unchanged; under
  `prefers-reduced-motion` the static glyph shows instead of the spinner. Used
  by the onboarding page's progress rows and the retire page's step rows.
-->
<script lang="ts">
  import { IDS } from '#lib/generated/uiIds';
  import { provisioningStatusGlyph } from '#lib/wasm-onboarding';

  let { status }: { status: string } = $props();
  const running = $derived(status === 'Running');
</script>

<span
  data-testid={IDS.PROVISIONING_STEP_CHECKBOX}
  class="prov-checkbox"
  class:prov-checkbox--running={running}
  aria-hidden="true"
>
  {#if running}<span class="prov-spinner"></span>{/if}<span class="prov-glyph">{provisioningStatusGlyph(status)}</span>
</span>

<style>
  .prov-checkbox {
    grid-area: checkbox;
    font-size: 1.125rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.25em;
  }
  .prov-spinner {
    display: inline-block;
    width: 0.9em;
    height: 0.9em;
    border: 2px solid var(--text-muted);
    border-top-color: transparent;
    border-radius: 50%;
    animation: prov-spin 0.9s linear infinite;
  }
  /* The glyph is the read; while the spinner paints it is clipped from view,
     never removed (display:none would drop it from innerText). */
  .prov-checkbox--running .prov-glyph {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    clip-path: inset(50%);
    white-space: nowrap;
  }
  @keyframes prov-spin {
    to { transform: rotate(360deg); }
  }
  @media (prefers-reduced-motion: reduce) {
    .prov-spinner { display: none; }
    .prov-checkbox--running .prov-glyph {
      position: static;
      width: auto;
      height: auto;
      overflow: visible;
      clip: auto;
      clip-path: none;
    }
  }
</style>
