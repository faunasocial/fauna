<script lang="ts">
  // Admin Mail reports page (`admin-mail-reports`) — the DMARC aggregate reports
  // peer mail servers send this nest about its own domains (dmarc-reporting.md
  // § Admin pane summary surface). A dumb renderer of the shared
  // `DmarcReportsMachine` (libs/fauna-client-mail-settings::dmarc_reports, WASM
  // twin WasmDmarcReportsMachine): the totals, shares, merged top lists and the
  // raw view's gunzip are all the machine's. Lifts the tui lead
  // (apps/fauna-tui/src/admin/mail_reports.rs): before the first read answers
  // only the heading and filters paint; "no reports" and "not read yet" are
  // different facts. Re-reads on open, on a filter change and on refresh; no push.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-mail-reports` page.
  import { identity } from '#lib/store';
  import { dmarcReportsMachine } from '#lib/rpc';
  import { ensureWasm, dmarcReportWindowOptions } from '#lib/wasm';
  import { onMount } from 'svelte';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  // Shapes mirror the shared serde JSON (snake_case; numbers-as-numbers).
  interface Totals {
    message_count: number;
    report_count: number;
    submitter_count: number;
    dmarc_pass: number;
    dkim_pass: number;
    spf_pass: number;
    dmarc_pass_percent: number;
    dkim_pass_percent: number;
    spf_pass_percent: number;
    disposition_none: number;
    disposition_quarantine: number;
    disposition_reject: number;
  }
  interface CountRow {
    value: string;
    count: number;
  }
  interface ReportRow {
    our_domain: string;
    submitter: string;
    report_id: string;
    begin_at: number;
    end_at: number;
    message_count: number;
    dmarc_pass: number;
  }
  interface Snapshot {
    window_days: number;
    our_domain: string | null;
    local_domains: string[];
    loaded: boolean;
    totals: Totals;
    sources: CountRow[];
    senders: CountRow[];
    reports: ReportRow[];
    truncated: boolean;
    opened: { xml: string } | null;
    error: string | null;
  }

  const T = t.admin.mail_reports_page;

  let machine: Awaited<ReturnType<typeof dmarcReportsMachine>> | null = null;
  let snap = $state<Snapshot | null>(null);
  let error = $state('');
  let windows = $state<number[]>([]);

  // Raw select values: the day count, and the domain ('' = every domain).
  const windowValue = $derived(String(snap?.window_days ?? windows[0] ?? ''));
  const domainValue = $derived(snap?.our_domain ?? '');
  const domains = $derived(['', ...(snap?.local_domains ?? [])]);

  // The local `YYYY-MM-DD` date of an epoch-seconds instant (the web twin of
  // `fauna_core::format::format_unix_local_date`, which needs the OS tz
  // database wasm lacks — same as the admin-mail page's `localDate`).
  function localDate(secs: number): string {
    const d = new Date(secs * 1000);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as Snapshot;
    error = snap?.error ?? '';
  }

  async function run(action: unknown): Promise<void> {
    if (!machine) return;
    try {
      await machine.dispatch(action);
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      windows = dmarcReportWindowOptions();
      machine = await dmarcReportsMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  function onWindow(e: Event): Promise<void> {
    const days = Number((e.currentTarget as HTMLSelectElement).value);
    return run({ SetWindow: { days } });
  }

  function onDomain(e: Event): Promise<void> {
    const v = (e.currentTarget as HTMLSelectElement).value;
    return run({ SetDomain: { domain: v === '' ? null : v } });
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{T.title}</h1>
<p class="muted small">{T.description}</p>

<MessageBanner bind:error />

<section class="section">
  <label class="field">
    <span class="label-text">{T.window_label}</span>
    <select
      class="input"
      data-testid={IDS.ADMIN_MAIL_REPORTS_WINDOW_SELECT}
      value={windowValue}
      onchange={onWindow}
    >
      {#each windows as d (d)}
        <option value={String(d)}>{T.window_days({ days: String(d) })}</option>
      {/each}
    </select>
  </label>
  <label class="field">
    <span class="label-text">{T.domain_label}</span>
    <select
      class="input"
      data-testid={IDS.ADMIN_MAIL_REPORTS_DOMAIN_SELECT}
      value={domainValue}
      onchange={onDomain}
    >
      {#each domains as d (d)}
        <option value={d}>{d === '' ? T.every_domain : d}</option>
      {/each}
    </select>
  </label>
  <button class="btn" data-testid={IDS.ADMIN_MAIL_REPORTS_REFRESH_BUTTON} onclick={() => run('Refresh')}>
    {T.refresh}
  </button>
</section>

{#if snap?.loaded}
  {@const totals = snap.totals}
  <section class="section" data-testid={IDS.ADMIN_MAIL_REPORTS_SUMMARY}>
    <h2>{T.summary_title}</h2>
    <p class="small" data-testid={IDS.ADMIN_MAIL_REPORTS_MESSAGES}>
      {T.messages({
        messages: String(totals.message_count),
        reports: String(totals.report_count),
        peers: String(totals.submitter_count),
      })}
    </p>
    <p class="small" data-testid={IDS.ADMIN_MAIL_REPORTS_ALIGNMENT}>
      {T.alignment({
        dmarc: String(totals.dmarc_pass),
        dmarc_share: String(totals.dmarc_pass_percent),
        dkim: String(totals.dkim_pass),
        dkim_share: String(totals.dkim_pass_percent),
        spf: String(totals.spf_pass),
        spf_share: String(totals.spf_pass_percent),
      })}
    </p>
    <p class="small" data-testid={IDS.ADMIN_MAIL_REPORTS_DISPOSITIONS}>
      {T.dispositions({
        none: String(totals.disposition_none),
        quarantine: String(totals.disposition_quarantine),
        reject: String(totals.disposition_reject),
      })}
    </p>
    {#if snap.truncated}
      <p class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_TRUNCATED}>{T.truncated}</p>
    {/if}
    {#if snap.reports.length === 0}
      <p class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_EMPTY}>{T.empty}</p>
    {/if}
  </section>

  {#if snap.sources.length > 0}
    <section class="section">
      <h2>{T.sources_title}</h2>
      {#each snap.sources as row, i (i)}
        <div class="row" data-testid={IDS.ADMIN_MAIL_REPORTS_SOURCE}>
          <span class="mono" data-testid={IDS.ADMIN_MAIL_REPORTS_SOURCE_IP}>{row.value}</span>
          <span class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_SOURCE_COUNT}>
            {T.row_messages({ count: String(row.count) })}
          </span>
        </div>
      {/each}
    </section>
  {/if}

  {#if snap.senders.length > 0}
    <section class="section">
      <h2>{T.senders_title}</h2>
      {#each snap.senders as row, i (i)}
        <div class="row" data-testid={IDS.ADMIN_MAIL_REPORTS_SENDER}>
          <span class="mono" data-testid={IDS.ADMIN_MAIL_REPORTS_SENDER_DOMAIN}>{row.value}</span>
          <span class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_SENDER_COUNT}>
            {T.row_messages({ count: String(row.count) })}
          </span>
        </div>
      {/each}
    </section>
  {/if}

  {#if snap.reports.length > 0}
    <section class="section">
      <h2>{T.reports_title}</h2>
      {#each snap.reports as r (`${r.our_domain}\u0000${r.submitter}\u0000${r.report_id}`)}
        <div class="row" data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT}>
          <span data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_SUBMITTER}>{r.submitter}</span>
          <span class="mono" data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_DOMAIN}>{r.our_domain}</span>
          <span class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_PERIOD}>
            {T.report_period({ begin: localDate(r.begin_at), end: localDate(r.end_at) })}
          </span>
          <span class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_MESSAGES}>
            {T.row_messages({ count: String(r.message_count) })}
          </span>
          <span class="muted small" data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_PASS}>
            {T.report_pass({ passed: String(r.dmarc_pass), count: String(r.message_count) })}
          </span>
          <button
            class="btn"
            data-testid={IDS.ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON}
            onclick={() =>
              run({
                OpenReport: {
                  our_domain: r.our_domain,
                  submitter: r.submitter,
                  report_id: r.report_id,
                },
              })}
          >
            {T.view}
          </button>
        </div>
      {/each}
    </section>
  {/if}

  {#if snap.opened}
    <section class="section">
      <h2>{T.raw_title}</h2>
      <pre class="raw" data-testid={IDS.ADMIN_MAIL_REPORTS_RAW}>{snap.opened.xml}</pre>
    </section>
  {/if}
{/if}

<style>
  h1 { margin-bottom: 0.25rem; font-size: 1.5rem; }
  h2 { font-size: 1.125rem; margin-bottom: 0.25rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .mono { font-family: var(--font-mono, monospace); }
  .section {
    margin: 1.5rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .field { display: flex; flex-direction: column; gap: 0.25rem; margin: 0.75rem 0; }
  .label-text { font-size: 0.8rem; color: var(--text, #e6edf3); font-weight: 500; }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
    margin: 0.5rem 0;
  }
  .raw {
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
    font-size: 0.75rem;
  }
  .input {
    padding: 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
    font-family: inherit;
  }
  .btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover, #1c2128); }
</style>
