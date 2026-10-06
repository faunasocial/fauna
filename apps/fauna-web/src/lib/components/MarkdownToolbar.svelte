<script lang="ts">
  import { wrapMarkdownSelection } from '$lib/wasm';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    textarea?: HTMLTextAreaElement | null;
    value: string;
    onchange: (value: string) => void;
    // Capability gating (`conversations.md` § Architectural rules #5): the
    // markdown toolbar is greyed out + disabled on rails without markdown
    // support (Smtp/Bluesky/Nostr). Rendered as the string "true" so the e2e
    // `get_attr("markdown-toolbar"/"markdown-bold-button", "disabled")` reads it.
    disabled?: boolean;
    // Optional splice hook for a non-textarea editor (the conversations compose
    // `MarkdownEditor` / CodeMirror): when present it OWNS the wrap + re-select on
    // its own buffer, so the textarea path below is bypassed. The feed/article
    // compose textareas leave this unset and keep the `textarea` splice unchanged.
    wrap?: (before: string, after: string) => void;
    // Per-editor marker-visibility toggle (tracked internally).
    // Rendered only when `onToggleMarkers` is set — i.e. a CM-based compose editor that can conceal
    // markers (the web feed/article textarea path leaves it unset, since a plain textarea can't).
    // `markersShown` reflects the current state (default false = hidden, the new compose default).
    markersShown?: boolean;
    onToggleMarkers?: () => void;
  }

  let {
    textarea = null,
    value,
    onchange,
    disabled = false,
    wrap,
    markersShown = false,
    onToggleMarkers,
  }: Props = $props();
  // The e2e bridge's `get_attr(..., "disabled")` reads `Locator.is_disabled()`,
  // a native-attribute check — it does NOT fall back to `data-disabled` (unlike
  // other attribute names), so the real `disabled` attribute below carries the
  // gating signal; `data-disabled`/`.disabled` stay for CSS + non-"disabled"
  // readers.
  let dis = $derived(String(disabled));

  function insert(before: string, after: string = '') {
    if (disabled) return;
    if (wrap) {
      // Editor-owned splice (CodeMirror): it applies the shared wrap rule on its
      // own buffer + re-selects the core atomically; nothing more to do here.
      wrap(before, after);
      return;
    }
    if (!textarea) {
      // Fallback: wrap entire value
      onchange(before + value + after);
      return;
    }
    const start = textarea.selectionStart;
    const end = textarea.selectionEnd;
    const selected = value.slice(start, end);
    // Shared wrap (fauna_core::markdown::wrap_selection over wasm): keeps edge
    // whitespace outside the markers so a word-selection's trailing space doesn't
    // produce `*italic *` → `*italic ***bold**`.
    const { replacement, beforeCore, core } = wrapMarkdownSelection(selected, before, after, 'text');
    onchange(value.slice(0, start) + replacement + value.slice(end));
    setTimeout(() => {
      textarea!.focus();
      textarea!.selectionStart = start + beforeCore.length;
      textarea!.selectionEnd = start + beforeCore.length + core.length;
    }, 0);
  }
</script>

<div class="md-toolbar" data-testid={IDS.MARKDOWN_TOOLBAR} data-disabled={dis} aria-disabled={disabled} class:disabled>
  <button {disabled} data-testid={IDS.MARKDOWN_BOLD_BUTTON} type="button" class="md-btn" title={t.markdown.bold} data-disabled={dis} onclick={() => insert('**', '**')}>B</button>
  <button {disabled} data-testid={IDS.MARKDOWN_ITALIC_BUTTON} type="button" class="md-btn md-italic" title={t.markdown.italic} data-disabled={dis} onclick={() => insert('*', '*')}>I</button>
  <button {disabled} data-testid={IDS.MARKDOWN_CODE_BUTTON} type="button" class="md-btn md-code" title={t.markdown.code} data-disabled={dis} onclick={() => insert('`', '`')}>{'<>'}</button>
  <button {disabled} data-testid={IDS.MARKDOWN_LINK_BUTTON} type="button" class="md-btn" title={t.markdown.link} data-disabled={dis} onclick={() => insert('[', '](url)')}>{t.markdown.link}</button>
  <button {disabled} data-testid={IDS.MARKDOWN_HEADING_BUTTON} type="button" class="md-btn" title={t.markdown.heading} data-disabled={dis} onclick={() => insert('## ')}>H</button>
  <button {disabled} data-testid={IDS.MARKDOWN_LIST_BUTTON} type="button" class="md-btn" title={t.markdown.list_item} data-disabled={dis} onclick={() => insert('- ')}>{t.markdown.list}</button>
  {#if onToggleMarkers}
    <button
      disabled={disabled}
      data-testid={IDS.MARKDOWN_MARKER_TOGGLE_BUTTON}
      type="button"
      class="md-btn md-marker-toggle"
      class:active={markersShown}
      title={t.markdown.toggle_markers}
      aria-pressed={markersShown}
      data-disabled={dis}
      onclick={() => { if (!disabled) onToggleMarkers?.(); }}
    >∗</button>
  {/if}
</div>

<style>
  .md-toolbar { display: flex; gap: 0.25rem; margin-bottom: 0.25rem; }
  .md-toolbar.disabled { opacity: 0.4; pointer-events: none; }
  .md-btn {
    background: var(--bg-hover, #f0f0f0);
    border: 1px solid var(--border, #ddd);
    border-radius: 4px;
    padding: 0.15rem 0.4rem;
    cursor: pointer;
    font-size: 0.75rem;
    font-weight: 700;
    color: var(--text, #333);
  }
  .md-btn:hover { background: var(--bg-active, #e0e0e0); }
  .md-italic { font-style: italic; }
  .md-code { font-family: monospace; }
  /* Pressed = markers shown (the non-default state); default (hidden) is un-pressed. */
  .md-marker-toggle.active {
    background: var(--accent, #2563eb);
    color: #fff;
    border-color: var(--accent, #2563eb);
  }
</style>
