<script lang="ts">
  // Admin Mail page (`admin-mail`) — the flat admin-tier mail *policy* form
  // (admin.md § 6 Mail / mail-policy-config.md § Policy catalog Tier 2). Where a
  // nest admin tunes the box-wide mail-policy knobs that have a LIVE write-path:
  // the deployment-wide mail-enable toggle (`set_mail_enabled`), the Spam/inbound
  // perimeter sub-struct (`put_spam_policy`), and the inbound authentication
  // enforcement sub-struct (`put_auth_policy`). Automatic concerns (DKIM/TLS/
  // MTA-STS/ACME/DMARC-publish/scanning/deliverability) have NO manual UI — they
  // auto-provision; DKIM/MX/SPF/DMARC records render read-only on `admin-dns`.
  //
  // A dumb renderer of the shared `MailPolicyMachine`
  // (libs/fauna-client-mail-settings::admin_policy, WASM twin WasmMailPolicyMachine):
  // build over the singleton WS-RPC client → hydrate() (via the Admin read twin
  // `fauna.bridges.get_mail_config`) → render snapshot() → dispatch(action) →
  // re-render. No policy logic in the SPA (priority #2). Lifts the linux lead +
  // its prior art (apps/fauna-linux/src/settings/admin_mail.rs): full-PUT saves
  // start from the persisted snapshot so a blank/unparseable integer falls back to
  // the persisted value, never silently zeroing a knob.
  //
  // Read + write are both LIVE. nest rejects an out-of-order spam-threshold write
  // (`fauna.protocol.malformed`); the machine surfaces it via `MailPolicySnapshot::
  // error` → the page's `error-message` (MessageBanner), never faked green.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-mail` page.
  import { identity } from '$lib/store';
  import { mailPolicyMachine } from '$lib/rpc';
  import {
    ensureWasm,
    parseCount,
    parseCountU64,
    fcrdnsModeOptions,
    imapDeleteNonemptyOptions,
    type ReachPolicyOption,
  } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // Shapes mirror the shared serde JSON (snake_case across the serde_wasm_bindgen
  // boundary; numbers-as-numbers via json_compatible; the status enum serializes
  // as its variant-name string).
  interface SpamPolicyView {
    max_score_before_spam_folder: number;
    max_score_before_reject: number;
    dnsbl_servers: string[];
    reject_no_rdns: boolean;
    greylist_enabled: boolean;
    greylist_delay_secs: number;
    max_conn_per_min: number;
    fcrdns_mode: string; // "off" | "score_signal" | "enforce"
    helo_identity_required: boolean;
    reject_fcrdns_fail: boolean;
    max_message_bytes: number;
    bayesian_weight_milli: number;
    bayesian_min_samples: number;
    bayesian_full_confidence_samples: number;
    training_history_retention_days: number;
    unlisted_recipient_penalty: number;
    baseline_standing_publish: boolean;
  }
  interface AuthPolicyView {
    enforce_dmarc: boolean;
    enforce_dmarc_quarantine: boolean;
    enforce_spf_hardfail: boolean;
    enforce_dkim: boolean;
    log_only: boolean;
    max_auth_failures_per_minute: number;
    max_conn_per_ip: number;
  }
  interface SubmissionPolicyView {
    max_per_day: number;
    max_recipients_per_message: number;
  }
  interface ImapPolicyView {
    idle_timeout_secs: number;
    tombstone_retention_days: number;
    delete_nonempty: string; // "forbidden" | "allowed"
    bodystructure_cache_max: number;
    storage_bytes_default: number;
    message_count_default: number;
  }
  interface OutboundPolicyView {
    retry_schedule_seconds: number[];
    permanent_failure_timeout_hours: number;
    delay_warning_at_hours: number;
    ndr_rate_limit_days: number;
    suppress_ndr_spf_hardfail: boolean;
    suppress_ndr_dmarc_reject: boolean;
    postmaster_cc_bounces: boolean; // read-only — project policy never CCs
    tlsrpt_send_reports: boolean;
    ipv6_enabled: boolean;
    treat_5xx_as_transient: string[];
  }
  interface AliasPolicyView {
    exact_aliases_max: number;
    reserved_local_parts: string[];
    subaddressing_enabled: boolean;
    wildcard_prefix_enabled: boolean;
  }
  // Outcome of the last PublishSpamBaseline action (null until the admin clicks
  // Publish). Mirrors the shared BaselinePublishView; `published` is the
  // wire flag as the nest sent it.
  interface BaselinePublishView {
    published: boolean;
    contributors: number;
    sample_count: number;
    skipped_contributors: number;
  }
  interface MailPolicySnapshot {
    mail_enabled: boolean;
    auto_enable_mail_for_new_users: boolean;
    spam: SpamPolicyView;
    auth: AuthPolicyView;
    submission: SubmissionPolicyView;
    imap: ImapPolicyView;
    outbound: OutboundPolicyView;
    alias: AliasPolicyView;
    status: string; // "Idle" | "Loading" | "Working"
    baseline_publish_result: BaselinePublishView | null;
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof mailPolicyMachine>> | null = null;
  let snap = $state<MailPolicySnapshot | null>(null);
  // The page-level `error-message` (MessageBanner): load errors + the snapshot's
  // last-action error (`fauna.protocol.malformed` on an out-of-order spam write).
  let error = $state('');

  // ── Editable form state (seeded from the snapshot on each render). Integer
  // fields are strings so partial/blank edits round-trip and parse on Save with a
  // fallback to the persisted value (linux `parse_u32`). The mail-enable toggle
  // dispatches on change, so it is rendered one-way (`checked={mailEnabled}`) — a
  // programmatic re-seed updates the checkbox without firing its change handler. ──
  let mailEnabled = $state(false);
  // Deployment-wide auto-enable-mail-for-new-users policy (default-on; read back
  // from fauna.setup.status). Like the mail-enable toggle, dispatches on change.
  let autoEnableNewUsers = $state(true);
  // Spam / inbound perimeter
  let junk = $state('');
  let reject = $state('');
  let dnsbl = $state('');
  let rejectNoRdns = $state(false);
  let greylistEnabled = $state(false);
  let greylistDelay = $state('');
  let maxConnPerMin = $state('');
  let fcrdnsMode = $state('score_signal');
  // The two raw-value picker catalogs, read from shared Rust after the wasm
  // module loads (`fauna_client_mail_settings::admin_policy`) rather than
  // spelled as <option> literals here — tui and linux read the same table, and
  // the Go MTA is pinned against the same tokens. Empty until onMount's
  // `ensureWasm()`, so the selects render no options for one frame; the bound
  // values are the wire defaults, which is what a hydrate would set anyway.
  let fcrdnsOptions = $state<ReachPolicyOption[]>([]);
  let imapDeleteOptions = $state<ReachPolicyOption[]>([]);
  let heloIdentityRequired = $state(false);
  let rejectFcrdnsFail = $state(false);
  let maxMessageBytes = $state('');
  // Per-user training (Tier-2 combined-score knobs; same put_spam_policy)
  let bayesianWeight = $state('');
  let bayesianMinSamples = $state('');
  let bayesianFullConfidenceSamples = $state('');
  let trainingHistoryRetention = $state('');
  let unlistedRecipientPenalty = $state('');
  // Auth enforcement
  let enforceDmarc = $state(false);
  let enforceDmarcQuarantine = $state(false);
  let enforceSpfHardfail = $state(false);
  let enforceDkim = $state(false);
  let logOnly = $state(false);
  let maxFailures = $state('');
  let maxConnPerIp = $state('');
  // Submission quotas
  let subMaxPerDay = $state('');
  let subMaxRecipients = $state('');
  // IMAP server policy
  let imapIdleTimeout = $state('');
  let imapTombstoneRetention = $state('');
  let imapDeleteNonempty = $state('forbidden');
  let imapBodystructureCache = $state('');
  let imapStorageBytes = $state('');
  let imapMessageCount = $state('');
  // Outbound delivery
  let outRetrySchedule = $state(''); // one value per line
  let outPermfailTimeout = $state('');
  let outDelayWarning = $state('');
  let outNdrRateLimit = $state('');
  let outSuppressNdrSpf = $state(false);
  let outSuppressNdrDmarc = $state(false);
  let outPostmasterCc = $state(false); // read-only (control disabled)
  let outTlsrptSend = $state(false);
  let outIpv6 = $state(false);
  let outTreat5xx = $state(''); // one code per line
  // Alias policy (dual-read group)
  let aliasExactMax = $state('');
  let aliasReserved = $state(''); // one local-part per line
  let aliasSubaddressing = $state(false);
  let aliasWildcardPrefix = $state(false);

  const busy = $derived(snap?.status === 'Working' || snap?.status === 'Loading');

  // Each integer knob parses through the shared `parse_count` / `parse_count_u64`
  // (wasm), falling back to the persisted `prev` on a blank/unparseable edit — the
  // no-silent-zeroing contract of the full-PUT save (value-formatting.md § Mail-knob
  // validation). web stops hand-rolling the `^\d+$` + range rule (priority #2); the
  // canonical rule now accepts a leading `+`, which web's `^\d+$` rejected (#4
  // convergence). `parseU64` is the one knob (IMAP storage-bytes) whose range can
  // exceed u32; byte ceilings stay within Number.MAX_SAFE_INTEGER.
  function parseU32(text: string, prev: number): number {
    return parseCount(text) ?? prev;
  }

  function parseU64(text: string, prev: number): number {
    return parseCountU64(text) ?? prev;
  }

  // Newline-separated text ↔ string list (the list-field convention, per the i18n
  // "one … per line" subtitles + the existing dnsbl_servers field).
  function linesToList(text: string): string[] {
    return text
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0);
  }
  // Newline-separated text → u64 list, dropping blank/non-digit lines.
  function linesToU64List(text: string): number[] {
    return text
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => /^\d+$/.test(l))
      .map((l) => Number(l))
      .filter((n) => Number.isSafeInteger(n));
  }

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as MailPolicySnapshot;
    error = snap?.error ?? '';
    if (!snap) return;
    mailEnabled = snap.mail_enabled;
    autoEnableNewUsers = snap.auto_enable_mail_for_new_users;
    const s = snap.spam;
    junk = String(s.max_score_before_spam_folder);
    reject = String(s.max_score_before_reject);
    dnsbl = s.dnsbl_servers.join('\n');
    rejectNoRdns = s.reject_no_rdns;
    greylistEnabled = s.greylist_enabled;
    greylistDelay = String(s.greylist_delay_secs);
    maxConnPerMin = String(s.max_conn_per_min);
    fcrdnsMode = s.fcrdns_mode;
    heloIdentityRequired = s.helo_identity_required;
    rejectFcrdnsFail = s.reject_fcrdns_fail;
    maxMessageBytes = String(s.max_message_bytes);
    bayesianWeight = String(s.bayesian_weight_milli);
    bayesianMinSamples = String(s.bayesian_min_samples);
    bayesianFullConfidenceSamples = String(s.bayesian_full_confidence_samples);
    trainingHistoryRetention = String(s.training_history_retention_days);
    unlistedRecipientPenalty = String(s.unlisted_recipient_penalty);
    const a = snap.auth;
    enforceDmarc = a.enforce_dmarc;
    enforceDmarcQuarantine = a.enforce_dmarc_quarantine;
    enforceSpfHardfail = a.enforce_spf_hardfail;
    enforceDkim = a.enforce_dkim;
    logOnly = a.log_only;
    maxFailures = String(a.max_auth_failures_per_minute);
    maxConnPerIp = String(a.max_conn_per_ip);
    const sub = snap.submission;
    subMaxPerDay = String(sub.max_per_day);
    subMaxRecipients = String(sub.max_recipients_per_message);
    const im = snap.imap;
    imapIdleTimeout = String(im.idle_timeout_secs);
    imapTombstoneRetention = String(im.tombstone_retention_days);
    imapDeleteNonempty = im.delete_nonempty;
    imapBodystructureCache = String(im.bodystructure_cache_max);
    imapStorageBytes = String(im.storage_bytes_default);
    imapMessageCount = String(im.message_count_default);
    const o = snap.outbound;
    outRetrySchedule = o.retry_schedule_seconds.join('\n');
    outPermfailTimeout = String(o.permanent_failure_timeout_hours);
    outDelayWarning = String(o.delay_warning_at_hours);
    outNdrRateLimit = String(o.ndr_rate_limit_days);
    outSuppressNdrSpf = o.suppress_ndr_spf_hardfail;
    outSuppressNdrDmarc = o.suppress_ndr_dmarc_reject;
    outPostmasterCc = o.postmaster_cc_bounces;
    outTlsrptSend = o.tlsrpt_send_reports;
    outIpv6 = o.ipv6_enabled;
    outTreat5xx = o.treat_5xx_as_transient.join('\n');
    const al = snap.alias;
    aliasExactMax = String(al.exact_aliases_max);
    aliasReserved = al.reserved_local_parts.join('\n');
    aliasSubaddressing = al.subaddressing_enabled;
    aliasWildcardPrefix = al.wildcard_prefix_enabled;
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      fcrdnsOptions = fcrdnsModeOptions();
      imapDeleteOptions = imapDeleteNonemptyOptions();
      machine = await mailPolicyMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // Full-PUT the whole Spam sub-struct: start from the persisted snapshot so
  // unparsed/blank integer fields fall back to the persisted value.
  function buildSpam(): SpamPolicyView {
    const base = snap!.spam;
    return {
      max_score_before_spam_folder: parseU32(junk, base.max_score_before_spam_folder),
      max_score_before_reject: parseU32(reject, base.max_score_before_reject),
      dnsbl_servers: dnsbl
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l.length > 0),
      reject_no_rdns: rejectNoRdns,
      greylist_enabled: greylistEnabled,
      greylist_delay_secs: parseU32(greylistDelay, base.greylist_delay_secs),
      max_conn_per_min: parseU32(maxConnPerMin, base.max_conn_per_min),
      fcrdns_mode: fcrdnsMode,
      helo_identity_required: heloIdentityRequired,
      reject_fcrdns_fail: rejectFcrdnsFail,
      max_message_bytes: parseU32(maxMessageBytes, base.max_message_bytes),
      bayesian_weight_milli: parseU32(bayesianWeight, base.bayesian_weight_milli),
      bayesian_min_samples: parseU32(bayesianMinSamples, base.bayesian_min_samples),
      bayesian_full_confidence_samples: parseU32(
        bayesianFullConfidenceSamples,
        base.bayesian_full_confidence_samples,
      ),
      training_history_retention_days: parseU32(
        trainingHistoryRetention,
        base.training_history_retention_days,
      ),
      unlisted_recipient_penalty: parseU32(unlistedRecipientPenalty, base.unlisted_recipient_penalty),
      // Not rendered yet: carried through so a save never turns the standing
      // baseline publish off (which withdraws the deployment's baseline).
      baseline_standing_publish: base.baseline_standing_publish,
    };
  }

  function buildAuth(): AuthPolicyView {
    const base = snap!.auth;
    return {
      enforce_dmarc: enforceDmarc,
      enforce_dmarc_quarantine: enforceDmarcQuarantine,
      enforce_spf_hardfail: enforceSpfHardfail,
      enforce_dkim: enforceDkim,
      log_only: logOnly,
      max_auth_failures_per_minute: parseU32(maxFailures, base.max_auth_failures_per_minute),
      max_conn_per_ip: parseU32(maxConnPerIp, base.max_conn_per_ip),
    };
  }

  async function handleToggleEnabled(e: Event): Promise<void> {
    if (!machine) return;
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    try {
      await machine.dispatch({ SetMailEnabled: { enabled } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleToggleAutoEnableNewUsers(e: Event): Promise<void> {
    if (!machine) return;
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    try {
      await machine.dispatch({ SetAutoEnableMailForNewUsers: { enabled } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSaveSpam(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveSpam: { policy: buildSpam() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSaveAuth(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveAuth: { policy: buildAuth() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  function buildSubmission(): SubmissionPolicyView {
    const base = snap!.submission;
    return {
      max_per_day: parseU32(subMaxPerDay, base.max_per_day),
      max_recipients_per_message: parseU32(subMaxRecipients, base.max_recipients_per_message),
    };
  }

  function buildImap(): ImapPolicyView {
    const base = snap!.imap;
    return {
      idle_timeout_secs: parseU32(imapIdleTimeout, base.idle_timeout_secs),
      tombstone_retention_days: parseU32(imapTombstoneRetention, base.tombstone_retention_days),
      delete_nonempty: imapDeleteNonempty,
      bodystructure_cache_max: parseU32(imapBodystructureCache, base.bodystructure_cache_max),
      storage_bytes_default: parseU64(imapStorageBytes, base.storage_bytes_default),
      message_count_default: parseU32(imapMessageCount, base.message_count_default),
    };
  }

  function buildOutbound(): OutboundPolicyView {
    const base = snap!.outbound;
    return {
      retry_schedule_seconds: linesToU64List(outRetrySchedule),
      permanent_failure_timeout_hours: parseU32(outPermfailTimeout, base.permanent_failure_timeout_hours),
      delay_warning_at_hours: parseU32(outDelayWarning, base.delay_warning_at_hours),
      ndr_rate_limit_days: parseU32(outNdrRateLimit, base.ndr_rate_limit_days),
      suppress_ndr_spf_hardfail: outSuppressNdrSpf,
      suppress_ndr_dmarc_reject: outSuppressNdrDmarc,
      postmaster_cc_bounces: outPostmasterCc, // read-only; echoes the persisted value
      tlsrpt_send_reports: outTlsrptSend,
      ipv6_enabled: outIpv6,
      treat_5xx_as_transient: linesToList(outTreat5xx),
    };
  }

  function buildAlias(): AliasPolicyView {
    const base = snap!.alias;
    return {
      exact_aliases_max: parseU32(aliasExactMax, base.exact_aliases_max),
      reserved_local_parts: linesToList(aliasReserved),
      subaddressing_enabled: aliasSubaddressing,
      wildcard_prefix_enabled: aliasWildcardPrefix,
    };
  }

  async function handleSaveSubmission(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveSubmission: { policy: buildSubmission() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSaveImap(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveImap: { policy: buildImap() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSaveOutbound(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveOutbound: { policy: buildOutbound() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSaveAlias(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch({ SaveAlias: { policy: buildAlias() } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  // Publish the opt-in aggregate as the deployment baseline. Unit-variant action
  // ⇒ dispatched as a bare string; the outcome lands in
  // snapshot.baseline_publish_result (rendered below the button).
  async function handlePublishBaseline(): Promise<void> {
    if (!machine || !snap) return;
    try {
      await machine.dispatch('PublishSpamBaseline');
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.admin.mail_page.title}</h1>
<p class="muted small">{t.admin.mail_page.description}</p>

<MessageBanner bind:error />

<!-- ── Mail-enable master toggle (set_mail_enabled) ── -->
<section class="section">
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.mail_page.enabled_label}</span>
      <span class="muted small">{t.admin.mail_page.enabled_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_MAIL_ENABLED_TOGGLE}
      checked={mailEnabled}
      onchange={handleToggleEnabled}
    />
  </label>
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.mail_page.auto_enable_new_users_label}</span>
      <span class="muted small">{t.admin.mail_page.auto_enable_new_users_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_MAIL_AUTO_ENABLE_NEW_USERS_TOGGLE}
      checked={autoEnableNewUsers}
      onchange={handleToggleAutoEnableNewUsers}
    />
  </label>
</section>

<!-- ── Spam / inbound perimeter (put_spam_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.spam_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.spam_group_desc}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.threshold_junk_label}</span>
    <span class="muted small">{t.admin.mail_page.threshold_junk_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_THRESHOLD_JUNK} bind:value={junk} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.threshold_reject_label}</span>
    <span class="muted small">{t.admin.mail_page.threshold_reject_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_THRESHOLD_REJECT} bind:value={reject} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.dnsbl_label}</span>
    <span class="muted small">{t.admin.mail_page.dnsbl_subtitle}</span>
    <textarea class="input" rows="3" data-testid={IDS.ADMIN_MAIL_DNSBL_SERVERS} bind:value={dnsbl}></textarea>
  </label>

  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.reject_no_rdns_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_REJECT_NO_RDNS_TOGGLE} bind:checked={rejectNoRdns} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.greylist_enabled_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_GREYLIST_ENABLED_TOGGLE} bind:checked={greylistEnabled} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.greylist_delay_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_GREYLIST_DELAY_INPUT} bind:value={greylistDelay} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.max_conn_per_min_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_MAX_CONN_PER_MIN_INPUT} bind:value={maxConnPerMin} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.fcrdns_mode_label}</span>
    <select class="input" data-testid={IDS.ADMIN_MAIL_FCRDNS_MODE_SELECT} bind:value={fcrdnsMode}>
      {#each fcrdnsOptions as o (o.value)}
        <option value={o.value}>{resolveLocalized(o.label)}</option>
      {/each}
    </select>
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.helo_identity_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_HELO_IDENTITY_REQUIRED_TOGGLE} bind:checked={heloIdentityRequired} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.reject_fcrdns_fail_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_REJECT_FCRDNS_FAIL_TOGGLE} bind:checked={rejectFcrdnsFail} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.max_message_bytes_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_MAX_MESSAGE_BYTES_INPUT} bind:value={maxMessageBytes} />
  </label>
  <!-- ── Per-user training (Tier-2 combined-score knobs; same put_spam_policy) ── -->
  <label class="field">
    <span class="label-text">{t.admin.mail_page.bayesian_weight_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_BAYESIAN_WEIGHT} bind:value={bayesianWeight} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.bayesian_min_samples_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_BAYESIAN_MIN_SAMPLES} bind:value={bayesianMinSamples} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.bayesian_full_confidence_samples_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_BAYESIAN_FULL_CONFIDENCE_SAMPLES} bind:value={bayesianFullConfidenceSamples} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.training_history_retention_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SPAM_TRAINING_HISTORY_RETENTION} bind:value={trainingHistoryRetention} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.unlisted_recipient_penalty_label}</span>
    <span class="muted small">{t.admin.mail_page.unlisted_recipient_penalty_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_UNLISTED_RECIPIENT_PENALTY} bind:value={unlistedRecipientPenalty} />
  </label>

  <button
    class="btn primary"
    data-testid={IDS.ADMIN_MAIL_SPAM_SAVE_BUTTON}
    disabled={busy || !snap}
    onclick={handleSaveSpam}
  >
    {t.admin.mail_page.spam_save}
  </button>

  <!-- ── Deployment baseline (admin opt-in aggregate; publish_spam_baseline) ── -->
  <button
    class="btn"
    data-testid={IDS.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_BUTTON}
    disabled={busy || !snap}
    onclick={handlePublishBaseline}
  >
    {t.admin.mail_page.publish_spam_baseline_button}
  </button>
  <p class="muted small">{t.admin.mail_page.publish_spam_baseline_subtitle}</p>
  <p class="muted small" data-testid={IDS.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_RESULT}>
    {#if snap?.baseline_publish_result}
      {snap.baseline_publish_result.published
        ? t.admin.mail_page.spam_baseline_published({
            contributors: String(snap.baseline_publish_result.contributors),
            samples: String(snap.baseline_publish_result.sample_count),
          })
        : t.admin.mail_page.spam_baseline_withheld({
            contributors: String(snap.baseline_publish_result.contributors),
          })}
      {#if snap.baseline_publish_result.skipped_contributors > 0}
        {' '}{t.admin.mail_page.spam_baseline_skipped_contributors({
          count: String(snap.baseline_publish_result.skipped_contributors),
        })}
      {/if}
    {/if}
  </p>
</section>

<!-- ── Inbound authentication enforcement (put_auth_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.auth_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.auth_group_desc}</p>

  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.enforce_dmarc_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_AUTH_ENFORCE_DMARC_TOGGLE} bind:checked={enforceDmarc} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.enforce_dmarc_quarantine_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_AUTH_ENFORCE_DMARC_QUARANTINE_TOGGLE} bind:checked={enforceDmarcQuarantine} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.enforce_spf_hardfail_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_AUTH_ENFORCE_SPF_HARDFAIL_TOGGLE} bind:checked={enforceSpfHardfail} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.enforce_dkim_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_AUTH_ENFORCE_DKIM_TOGGLE} bind:checked={enforceDkim} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.log_only_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_AUTH_LOG_ONLY_TOGGLE} bind:checked={logOnly} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.max_failures_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_AUTH_MAX_FAILURES_INPUT} bind:value={maxFailures} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.max_conn_per_ip_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_AUTH_MAX_CONN_PER_IP_INPUT} bind:value={maxConnPerIp} />
  </label>

  <button
    class="btn primary"
    data-testid={IDS.ADMIN_MAIL_AUTH_SAVE_BUTTON}
    disabled={busy || !snap}
    onclick={handleSaveAuth}
  >
    {t.admin.mail_page.auth_save}
  </button>
</section>

<!-- ── Submission quotas (put_submission_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.submission_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.submission_group_desc}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.submission_max_per_day_label}</span>
    <span class="muted small">{t.admin.mail_page.submission_max_per_day_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SUBMISSION_MAX_PER_DAY_INPUT} bind:value={subMaxPerDay} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.submission_max_recipients_label}</span>
    <span class="muted small">{t.admin.mail_page.submission_max_recipients_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_SUBMISSION_MAX_RECIPIENTS_INPUT} bind:value={subMaxRecipients} />
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_SUBMISSION_SAVE_BUTTON} disabled={busy || !snap} onclick={handleSaveSubmission}>
    {t.admin.mail_page.submission_save}
  </button>
</section>

<!-- ── IMAP server policy (put_imap_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.imap_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.imap_group_desc}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_idle_timeout_label}</span>
    <span class="muted small">{t.admin.mail_page.imap_idle_timeout_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_IMAP_IDLE_TIMEOUT_INPUT} bind:value={imapIdleTimeout} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_tombstone_retention_label}</span>
    <span class="muted small">{t.admin.mail_page.imap_tombstone_retention_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_IMAP_TOMBSTONE_RETENTION_INPUT} bind:value={imapTombstoneRetention} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_delete_nonempty_label}</span>
    <select class="input" data-testid={IDS.ADMIN_MAIL_IMAP_DELETE_NONEMPTY_SELECT} bind:value={imapDeleteNonempty}>
      {#each imapDeleteOptions as o (o.value)}
        <option value={o.value}>{resolveLocalized(o.label)}</option>
      {/each}
    </select>
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_bodystructure_cache_label}</span>
    <span class="muted small">{t.admin.mail_page.imap_bodystructure_cache_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_IMAP_BODYSTRUCTURE_CACHE_INPUT} bind:value={imapBodystructureCache} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_storage_bytes_label}</span>
    <span class="muted small">{t.admin.mail_page.imap_storage_bytes_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_IMAP_STORAGE_BYTES_INPUT} bind:value={imapStorageBytes} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.imap_message_count_label}</span>
    <span class="muted small">{t.admin.mail_page.imap_message_count_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_IMAP_MESSAGE_COUNT_INPUT} bind:value={imapMessageCount} />
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_IMAP_SAVE_BUTTON} disabled={busy || !snap} onclick={handleSaveImap}>
    {t.admin.mail_page.imap_save}
  </button>
</section>

<!-- ── Outbound delivery (put_outbound_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.outbound_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.outbound_group_desc}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.outbound_retry_schedule_label}</span>
    <span class="muted small">{t.admin.mail_page.outbound_retry_schedule_subtitle}</span>
    <textarea class="input" rows="3" data-testid={IDS.ADMIN_MAIL_OUTBOUND_RETRY_SCHEDULE} bind:value={outRetrySchedule}></textarea>
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.outbound_permfail_timeout_label}</span>
    <span class="muted small">{t.admin.mail_page.outbound_permfail_timeout_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_OUTBOUND_PERMFAIL_TIMEOUT_INPUT} bind:value={outPermfailTimeout} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.outbound_delay_warning_label}</span>
    <span class="muted small">{t.admin.mail_page.outbound_delay_warning_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_OUTBOUND_DELAY_WARNING_INPUT} bind:value={outDelayWarning} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.outbound_ndr_rate_limit_label}</span>
    <span class="muted small">{t.admin.mail_page.outbound_ndr_rate_limit_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_OUTBOUND_NDR_RATE_LIMIT_INPUT} bind:value={outNdrRateLimit} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.outbound_suppress_ndr_spf_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_OUTBOUND_SUPPRESS_NDR_SPF_TOGGLE} bind:checked={outSuppressNdrSpf} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.outbound_suppress_ndr_dmarc_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_OUTBOUND_SUPPRESS_NDR_DMARC_TOGGLE} bind:checked={outSuppressNdrDmarc} />
  </label>
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.mail_page.outbound_postmaster_cc_label}</span>
      <span class="muted small">{t.admin.mail_page.outbound_postmaster_cc_subtitle}</span>
    </span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_OUTBOUND_POSTMASTER_CC_TOGGLE} checked={outPostmasterCc} disabled />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.outbound_tlsrpt_send_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_OUTBOUND_TLSRPT_SEND_TOGGLE} bind:checked={outTlsrptSend} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.outbound_ipv6_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_OUTBOUND_IPV6_TOGGLE} bind:checked={outIpv6} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.outbound_treat_5xx_label}</span>
    <span class="muted small">{t.admin.mail_page.outbound_treat_5xx_subtitle}</span>
    <textarea class="input" rows="3" data-testid={IDS.ADMIN_MAIL_OUTBOUND_TREAT_5XX_TRANSIENT} bind:value={outTreat5xx}></textarea>
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_OUTBOUND_SAVE_BUTTON} disabled={busy || !snap} onclick={handleSaveOutbound}>
    {t.admin.mail_page.outbound_save}
  </button>
</section>

<!-- ── Alias policy (put_alias_policy; dual-read via get_alias_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.alias_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.alias_group_desc}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.alias_exact_max_label}</span>
    <span class="muted small">{t.admin.mail_page.alias_exact_max_subtitle}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_ALIAS_EXACT_MAX_INPUT} bind:value={aliasExactMax} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.alias_reserved_label}</span>
    <span class="muted small">{t.admin.mail_page.alias_reserved_subtitle}</span>
    <textarea class="input" rows="3" data-testid={IDS.ADMIN_MAIL_ALIAS_RESERVED_LOCAL_PARTS} bind:value={aliasReserved}></textarea>
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.alias_subaddressing_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_ALIAS_SUBADDRESSING_TOGGLE} bind:checked={aliasSubaddressing} />
  </label>
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.alias_wildcard_prefix_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_ALIAS_WILDCARD_PREFIX_TOGGLE} bind:checked={aliasWildcardPrefix} />
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_ALIAS_SAVE_BUTTON} disabled={busy || !snap} onclick={handleSaveAlias}>
    {t.admin.mail_page.alias_save}
  </button>
</section>

<style>
  h1 { margin-bottom: 0.25rem; font-size: 1.5rem; }
  h2 { font-size: 1.125rem; margin-bottom: 0.25rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .section {
    margin: 1.5rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin: 0.75rem 0;
  }
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin: 0.75rem 0;
  }
  .switch-row span { display: flex; flex-direction: column; gap: 0.125rem; }
  .label-text {
    font-size: 0.8rem;
    color: var(--text, #e6edf3);
    font-weight: 500;
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
  textarea.input { resize: vertical; }
  .btn {
    margin-top: 1rem;
    padding: 0.5rem 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover, #1c2128); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
</style>
