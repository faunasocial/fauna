<script lang="ts">
  // The shared report sheet and its acknowledgement line (`report-sheet`,
  // `report-status` — moderation.md § User-initiated reporting → App surface).
  // One component on every page that opens it (feed ⋯, message ⋯, an OTHER
  // profile): the page sets `target` through its own verb and mounts this once.
  //
  // Every decision here is shared Rust's (`fauna_client_moderation::report` over
  // wasm): the reason list, the submit gate, the include-text rule, the words.
  // The sheet paints `reportSheetView`; the send is `moderationAbuseReportSubmit`
  // (built from the sheet by the shared `report_request`); the follow-ups mirror
  // tui's `report.rs`: submit → `knocksBlock(author)` when ticked → the
  // reporter-side hide (`hideReported`) → the stored list into the render state.
  // A failed send keeps the sheet and reports on the PAGE's `error-message` (via
  // `onerror`); a failed block/hide lands there too, BESIDE the acknowledgement —
  // never silent.
  import { identity } from '$lib/store';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';
  import { reportSheetView, reportFailed, type ReportTarget, type ReportForm } from '$lib/wasm';
  import { moderationAbuseReportSubmit, knocksBlock, hideReported } from '$lib/rpc';
  import { setHiddenContent } from '$lib/contentPolicy.svelte';

  interface Props {
    /** The subject the opening verb chose; `null` closes the sheet. */
    target: ReportTarget | null;
    /** The page's `error-message` line (`''` clears it). */
    onerror: (message: string) => void;
    /** The author the report also blocked — the page refreshes its own block state. */
    onblocked?: (actorId: string) => void;
  }

  let { target = $bindable(), onerror, onblocked }: Props = $props();

  const emptyForm = (): ReportForm => ({
    reason: null,
    note: '',
    include_text: false,
    block_author: false,
  });

  let form = $state<ReportForm>(emptyForm());
  let sending = $state(false);
  // The acknowledgement: present only after a send landed, until the next open.
  let status = $state('');

  const view = $derived(target ? reportSheetView(target, form) : null);

  // A fresh open starts from an empty draft and no stale acknowledgement.
  let openedFor: ReportTarget | null = null;
  $effect(() => {
    if (target && target !== openedFor) {
      form = emptyForm();
      status = '';
      sending = false;
    }
    openedFor = target;
  });

  function subjectId(t: ReportTarget): string {
    const s = t.subject;
    return s.kind === 'post' ? s.cid : s.kind === 'message' ? s.record_cid : s.actor_id;
  }

  function detail(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function submit(): Promise<void> {
    const secret = $identity?.secretHex;
    const sent = target;
    if (!secret || !sent || !view?.can_submit || sending) return;
    sending = true;
    let reply;
    try {
      reply = await moderationAbuseReportSubmit(secret, sent, form);
    } catch (e) {
      // The sheet stays open for a retry.
      onerror(resolveLocalized(reportFailed(detail(e))));
      sending = false;
      return;
    }
    let followup: string | null = null;
    if (form.block_author && sent.author) {
      try {
        await knocksBlock(secret, sent.author);
        onblocked?.(sent.author);
      } catch (e) {
        followup = `block: ${detail(e)}`;
      }
    }
    try {
      setHiddenContent(await hideReported(secret, subjectId(sent)));
    } catch (e) {
      followup ??= `hide: ${detail(e)}`;
    }
    status = resolveLocalized(reply.acknowledgement);
    onerror(followup ?? '');
    sending = false;
    target = null;
  }

  function cancel(): void {
    target = null;
  }
</script>

{#if target && view}
  <div class="report-sheet" role="dialog" aria-label={resolveLocalized(view.title)} data-testid={IDS.REPORT_SHEET}>
    <h3>{resolveLocalized(view.title)}</h3>
    <label>
      {resolveLocalized(view.reason_label)}
      <select
        data-testid={IDS.REPORT_REASON_SELECT}
        value={form.reason ?? ''}
        onchange={(e) => (form.reason = e.currentTarget.value || null)}
      >
        <option value="" disabled>{resolveLocalized(view.reason_label)}</option>
        {#each view.reasons as option (option.reason)}
          <option value={option.reason}>{resolveLocalized(option.label)}</option>
        {/each}
      </select>
    </label>
    <label>
      {resolveLocalized(view.note_label)}
      <textarea data-testid={IDS.REPORT_NOTE_INPUT} bind:value={form.note} rows="3"></textarea>
    </label>
    {#if view.show_include_text}
      <label class="checkbox">
        <input type="checkbox" data-testid={IDS.REPORT_INCLUDE_TEXT_CHECKBOX} bind:checked={form.include_text} />
        {resolveLocalized(view.include_text_label)}
      </label>
    {/if}
    <label class="checkbox">
      <input type="checkbox" data-testid={IDS.REPORT_BLOCK_AUTHOR_CHECKBOX} bind:checked={form.block_author} />
      {resolveLocalized(view.block_author_label)}
    </label>
    {#if !view.can_submit && view.blocked_reason}
      <p class="muted">{resolveLocalized(view.blocked_reason)}</p>
    {/if}
    <div class="actions">
      <button
        class="btn"
        data-testid={IDS.REPORT_SUBMIT_BUTTON}
        disabled={!view.can_submit || sending}
        onclick={submit}
      >{resolveLocalized(view.submit_label)}</button>
      <button class="btn-secondary" data-testid={IDS.REPORT_CANCEL_BUTTON} onclick={cancel}>
        {resolveLocalized(view.cancel_label)}
      </button>
    </div>
  </div>
{/if}

{#if status}
  <p class="muted" role="status" data-testid={IDS.REPORT_STATUS}>{status}</p>
{/if}

<style>
  .report-sheet {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 1rem;
    border: 1px solid var(--border, #ccc);
    border-radius: 8px;
    background: var(--surface, #fff);
  }
  .report-sheet label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .report-sheet label.checkbox {
    flex-direction: row;
    align-items: center;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
</style>
