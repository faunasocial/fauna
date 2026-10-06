<script lang="ts">
  import { contentLabelStyle, confidencePercent } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  let { label }: { label: string } = $props();

  function parseCategory(l: string): string {
    return l.split(':')[0] || l;
  }

  function parseConfidence(l: string): number {
    const parts = l.split(':');
    if (parts.length < 2) return 0;
    const val = parseFloat(parts[1]);
    return isNaN(val) ? 0 : val;
  }

  const category = $derived(parseCategory(label));
  const confidence = $derived(parseConfidence(label));
  // Canonical category → label/icon/colour map (shared Rust — no per-app
  // hard-coding; moderation.md § Where logic lives, drift #157).
  const style = $derived(contentLabelStyle(category));
  const text = $derived(resolveLocalized(style.label));
  // Shared half-up rounding (per-mille → percent) — the local float is quantized to
  // the dag-cbor wire form first, matching the 5 native apps (no `Math.round(* 100)`).
  const confidencePct = $derived(confidencePercent(Math.round(confidence * 1000)));
</script>

<span
  class="content-label-badge"
  data-testid={IDS.CONTENT_LABEL_BADGE}
  title="{category}: {confidencePct}% confidence"
  style="background: {style.tint}26; color: {style.accent};"
>{style.icon} {text}</span>

<style>
  .content-label-badge {
    display: inline-flex;
    align-items: center;
    font-size: 0.7rem;
    padding: 0.125rem 0.5rem;
    border-radius: 999px;
    font-weight: 500;
    white-space: nowrap;
  }
</style>
